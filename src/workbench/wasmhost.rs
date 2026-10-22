//! The WASM host, as EXTENSIONS.md lays it out: native plugins built against
//! `wit/vsn.wit`, run by wasmtime on a thread of their own, each in its own
//! store with fuel and memory limits. They have no WASI, so no file, clock or
//! network access. What they ask of the editor comes back as the JS host's
//! messages, so both hosts share one path into the workbench.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};

use anyhow::Context as _;
use futures::channel::mpsc::UnboundedSender;
use serde_json::{Value, json};
use wasmtime::{
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder,
    component::{Component, HasSelf, Linker},
};

wasmtime::component::bindgen!({ path: "wit", world: "plugin" });

use vsn::plugin::editor;

/// Instructions a call may run before it's stopped, about a second's worth.
const FUEL: u64 = 1_000_000_000;
const MEMORY: usize = 64 << 20;

pub(super) enum Req {
    /// Loads and activates the plugin, if it isn't already.
    Activate {
        id: String,
        wasm: PathBuf,
        /// Its package.json's configuration defaults, by key.
        defaults: Value,
        world: World,
        /// Its package.json's `vsn`: the languages it serves and the
        /// permissions it asks for.
        vsn: Value,
    },
    Run {
        id: String,
        command: String,
        /// A JSON array.
        args: Value,
        world: World,
    },
    /// A language server message for the plugins serving its document's
    /// language: a request (answered, null when none does) or a notification.
    Lsp(Value),
}

/// What a plugin can read of the editor during a call.
#[derive(Default)]
pub(super) struct World {
    /// settings.json.
    pub settings: Value,
    /// The active text editor's `path`, `languageId` and `text`, or null.
    pub editor: Value,
}

/// The ids of the plugins' questions, above the JS host's.
pub(super) const ASKS: u64 = 1 << 40;
static NEXT_ASK: AtomicU64 = AtomicU64::new(ASKS);

/// The WASM host's thread: its requests, and the answers to its plugins'
/// questions.
#[derive(Clone)]
pub(super) struct WasmHost {
    req: mpsc::Sender<Req>,
    answers: mpsc::Sender<(u64, Value)>,
}

impl WasmHost {
    /// Sends `req`; a stopped host drops it.
    pub fn send(&self, req: Req) {
        self.req.send(req).ok();
    }

    /// Answers question `id`; null is dismissed.
    pub fn answer(&self, id: u64, value: Value) {
        self.answers.send((id, value)).ok();
    }
}

struct State {
    id: String,
    tx: UnboundedSender<Value>,
    limits: StoreLimits,
    defaults: Value,
    world: World,
    answers: Arc<Mutex<mpsc::Receiver<(u64, Value)>>>,
    /// package.json's `vsn.permissions`, and the user's answers so far.
    permissions: Vec<String>,
    granted: HashMap<String, bool>,
}

impl State {
    fn send(&self, msg: Value) {
        self.tx.unbounded_send(msg).ok();
    }

    /// Asks the workbench `msg` and waits for its answer.
    // ponytail: the host thread waits too, so other plugins' calls queue
    // behind an unanswered question.
    fn ask(&self, mut msg: Value) -> Value {
        let id = NEXT_ASK.fetch_add(1, Ordering::Relaxed);
        msg["id"] = id.into();
        self.send(msg);
        let answers = self.answers.lock().unwrap();
        // A closed workbench ends the wait as a dismissal.
        while let Ok((answered, value)) = answers.recv() {
            if answered == id {
                return value;
            }
        }
        Value::Null
    }

    /// Whether the plugin may use `permission`: it asks for it in
    /// package.json, and the user allows it, asked the first time.
    // ponytail: the answer lasts until vsn closes; remember it in settings
    // if asking every session gets old.
    fn allowed(&mut self, permission: &str, what: &str) -> Result<(), String> {
        if !self.permissions.iter().any(|p| p == permission) {
            return Err(format!(
                "\"{permission}\" isn't in package.json's vsn.permissions"
            ));
        }
        let granted = match self.granted.get(permission) {
            Some(&granted) => granted,
            None => {
                let placeholder = format!("Allow extension '{}' to {what}?", self.id);
                let rows = [["Allow", ""], ["Deny", ""]];
                let msg = json!({ "m": "quickPick", "placeholder": placeholder, "rows": rows });
                let answer = self.ask(msg);
                // Dismissed asks again next time.
                if !answer.is_null() {
                    self.granted.insert(permission.to_string(), answer == 0);
                }
                answer == 0
            }
        };
        granted
            .then_some(())
            .ok_or_else(|| format!("the user denied \"{permission}\""))
    }
}

impl editor::Host for State {
    fn show_message(&mut self, severity: editor::Severity, text: String) {
        let severity = match severity {
            editor::Severity::Info => "info",
            editor::Severity::Warning => "warning",
            editor::Severity::Error => "error",
        };
        self.send(json!({ "m": "message", "severity": severity, "text": text }));
    }

    fn set_status(&mut self, text: String) {
        let visible = !text.is_empty();
        let id = format!("wasm:{}", self.id);
        self.send(
            json!({ "m": "status", "id": id, "text": text, "visible": visible, "wasm": true }),
        );
    }

    fn log(&mut self, text: String) {
        self.send(json!({ "m": "log", "channel": self.id, "text": text }));
    }

    fn get_setting(&mut self, key: String) -> Option<String> {
        let value = (self.world.settings.get(&key)).or(self.defaults.get(&key))?;
        Some(value.to_string())
    }

    fn active_document(&mut self) -> Option<editor::Document> {
        let e = &self.world.editor;
        let text = |k: &str| e[k].as_str().map(str::to_string);
        Some(editor::Document {
            uri: text("uri")?,
            path: text("path")?,
            language: text("languageId")?,
            text: text("text")?,
        })
    }

    fn show_quick_pick(&mut self, placeholder: String, items: Vec<String>) -> Option<u32> {
        let rows: Vec<_> = items.iter().map(|i| [i.as_str(), ""]).collect();
        let msg = json!({ "m": "quickPick", "placeholder": placeholder, "rows": rows });
        let ix = self.ask(msg).as_u64()?;
        ix.try_into().ok()
    }

    fn show_input_box(&mut self, placeholder: String, value: String) -> Option<String> {
        let msg = json!({ "m": "inputBox", "placeholder": placeholder, "value": value });
        self.ask(msg).as_str().map(str::to_string)
    }

    fn lsp_notify(&mut self, method: String, params: String) {
        let params: Value = serde_json::from_str(&params).unwrap_or_default();
        let msg = json!({ "method": method, "params": params });
        self.send(json!({ "m": "lsp", "wasm": true, "msg": msg }));
    }

    fn apply_edit(&mut self, edit: String) {
        let edit: Value = serde_json::from_str(&edit).unwrap_or_default();
        self.send(json!({ "m": "applyEdit", "edit": edit }));
    }

    // ponytail: file and network calls block the host thread (fuel doesn't
    // run during them); fetch gives up after 30 seconds.
    fn read_file(&mut self, path: String) -> Result<String, String> {
        self.allowed("files", "read and write files")?;
        std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))
    }

    fn write_file(&mut self, path: String, text: String) -> Result<(), String> {
        self.allowed("files", "read and write files")?;
        std::fs::write(&path, text).map_err(|e| format!("{path}: {e}"))
    }

    fn fetch(&mut self, url: String) -> Result<String, String> {
        self.allowed("network", "access the network")?;
        ureq::get(&url)
            .config()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .call()
            .and_then(|mut r| r.body_mut().read_to_string())
            .map_err(|e| format!("{url}: {e}"))
    }
}

/// Starts the host thread; it ends when the returned host is dropped.
pub(super) fn start(tx: UnboundedSender<Value>) -> WasmHost {
    let (req, rx) = mpsc::channel();
    let (answers, answers_rx) = mpsc::channel();
    let answers_rx = Arc::new(Mutex::new(answers_rx));
    std::thread::spawn(move || {
        let engine = match Engine::new(Config::new().consume_fuel(true)) {
            Ok(engine) => engine,
            Err(err) => return error(&tx, format!("Unable to start the WASM host: {err}")),
        };
        let mut linker = Linker::<State>::new(&engine);
        if let Err(err) = Plugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s) {
            return error(&tx, format!("Unable to start the WASM host: {err}"));
        }
        let mut plugins: HashMap<String, (Store<State>, Plugin)> = HashMap::new();
        // Each plugin's languages, and each open document's.
        let mut serves: Vec<(String, Vec<String>)> = Vec::new();
        let mut docs: HashMap<String, String> = HashMap::new();
        for req in rx {
            match req {
                Req::Activate {
                    id,
                    wasm,
                    defaults,
                    world,
                    vsn,
                } => {
                    if plugins.contains_key(&id) {
                        continue;
                    }
                    let answers = answers_rx.clone();
                    match load(&engine, &linker, &id, &wasm, &tx, answers) {
                        Ok(mut p) => {
                            let state = p.0.data_mut();
                            (state.defaults, state.world) = (defaults, world);
                            state.permissions = strings(&vsn["permissions"]);
                            plugins.insert(id.clone(), p);
                            serves.push((id.clone(), strings(&vsn["languages"])));
                            call(&mut plugins, &id, &tx, |p, s| p.call_activate(s));
                        }
                        Err(err) => error(&tx, format!("Extension '{id}' failed: {err:#}")),
                    }
                }
                Req::Run {
                    id,
                    command,
                    args,
                    world,
                } => {
                    let Some((store, _)) = plugins.get_mut(&id) else {
                        error(
                            &tx,
                            format!("Extension '{id}' failed: command '{command}' not found"),
                        );
                        continue;
                    };
                    store.data_mut().world = world;
                    let args = args.to_string();
                    call(&mut plugins, &id, &tx, |p, s| {
                        p.call_run_command(s, &command, &args)
                    });
                }
                Req::Lsp(msg) => {
                    let method = msg["method"].as_str().unwrap_or_default();
                    let doc = &msg["params"]["textDocument"];
                    let uri = doc["uri"].as_str().unwrap_or_default().to_string();
                    if let Some(lang) = doc["languageId"].as_str() {
                        docs.insert(uri.clone(), lang.to_string());
                    }
                    let lang = docs.get(&uri).cloned().unwrap_or_default();
                    if method == "textDocument/didClose" {
                        docs.remove(&uri);
                    }
                    let ids: Vec<String> = (serves.iter())
                        .filter(|(id, langs)| langs.contains(&lang) && plugins.contains_key(id))
                        .map(|(id, _)| id.clone())
                        .collect();
                    let params = msg["params"].to_string();
                    let Some(req) = msg.get("id") else {
                        for id in ids {
                            call(&mut plugins, &id, &tx, |p, s| {
                                p.call_lsp(s, method, &params)
                            });
                        }
                        continue;
                    };
                    // ponytail: the first plugin for the language answers;
                    // VS Code merges every provider's results.
                    let result = (ids.first())
                        .and_then(|id| {
                            call(&mut plugins, id, &tx, |p, s| p.call_lsp(s, method, &params))
                        })
                        .flatten()
                        .and_then(|r| serde_json::from_str(&r).ok())
                        .unwrap_or(Value::Null);
                    let msg = json!({ "jsonrpc": "2.0", "id": req, "result": result });
                    tx.unbounded_send(json!({ "m": "lsp", "wasm": true, "msg": msg }))
                        .ok();
                }
            }
        }
    });
    WasmHost { req, answers }
}

/// Calls plugin `id` with a full tank of fuel. A trap leaves its state
/// unknown, so it doesn't run again.
fn call<T>(
    plugins: &mut HashMap<String, (Store<State>, Plugin)>,
    id: &str,
    tx: &UnboundedSender<Value>,
    f: impl FnOnce(&Plugin, &mut Store<State>) -> wasmtime::Result<T>,
) -> Option<T> {
    let (store, plugin) = plugins.get_mut(id)?;
    match store.set_fuel(FUEL).and_then(|()| f(plugin, store)) {
        Ok(value) => Some(value),
        Err(err) => {
            plugins.remove(id);
            error(tx, format!("Extension '{id}' failed: {err:#}"));
            None
        }
    }
}

/// A JSON array's strings.
pub(super) fn strings(list: &Value) -> Vec<String> {
    (list.as_array().into_iter().flatten())
        .filter_map(|s| Some(s.as_str()?.to_string()))
        .collect()
}

fn error(tx: &UnboundedSender<Value>, text: String) {
    tx.unbounded_send(json!({ "m": "error", "text": text }))
        .ok();
}

/// A plugin's component; a core module (what `cargo build --target
/// wasm32-unknown-unknown` makes, with wit-bindgen's type section) is
/// wrapped into one first.
fn load(
    engine: &Engine,
    linker: &Linker<State>,
    id: &str,
    wasm: &PathBuf,
    tx: &UnboundedSender<Value>,
    answers: Arc<Mutex<mpsc::Receiver<(u64, Value)>>>,
) -> anyhow::Result<(Store<State>, Plugin)> {
    let mut bytes = std::fs::read(wasm).with_context(|| format!("{}", wasm.display()))?;
    if bytes.get(4..8) == Some(&[1, 0, 0, 0]) {
        bytes = wit_component::ComponentEncoder::default()
            .module(&bytes)?
            .validate(true)
            .encode()?;
    }
    let component = Component::new(engine, &bytes)?;
    let state = State {
        id: id.to_string(),
        tx: tx.clone(),
        limits: StoreLimitsBuilder::new().memory_size(MEMORY).build(),
        defaults: Value::Null,
        world: World::default(),
        answers,
        permissions: Vec::new(),
        granted: HashMap::new(),
    };
    let mut store = Store::new(engine, state);
    store.limiter(|s| &mut s.limits);
    store.set_fuel(FUEL)?;
    let plugin = Plugin::instantiate(&mut store, &component, linker)?;
    Ok((store, plugin))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs examples/hello-wasm, when built, through the host: its activation,
    /// a command, one reading the editor and asking a question, and one that
    /// never returns until the fuel runs out.
    #[test]
    fn hello_wasm() {
        let wasm = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("examples/hello-wasm/target/wasm32-unknown-unknown/release/hello_wasm.wasm");
        if !wasm.exists() {
            return eprintln!("skipped: build examples/hello-wasm first");
        }
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let host = start(tx);
        let id = "vsn.hello-wasm".to_string();
        let run = |command: &str| Req::Run {
            id: id.clone(),
            command: command.into(),
            args: json!([{ "$uri": "file:///c%3A/x/a.rs" }]),
            world: World {
                settings: json!({ "hello.greeting": "Hi" }),
                editor: json!({
                    "uri": "file:///c%3A/x/a.rs",
                    "path": "c:\\x\\a.rs",
                    "languageId": "rust",
                    "text": "fn",
                }),
            },
        };
        host.send(Req::Activate {
            id: id.clone(),
            wasm,
            defaults: json!({ "hello.greeting": "Hello", "hello.count": 3 }),
            world: World::default(),
            vsn: Value::Null,
        });
        host.send(run("helloWasm.sayHello"));
        host.send(run("helloWasm.info"));
        let mut rx = futures::executor::block_on_stream(rx);
        let mut texts = Vec::new();
        for msg in rx.by_ref() {
            if msg["m"] == "quickPick" {
                assert_eq!(msg["rows"], json!([["a", ""], ["b", ""]]));
                host.answer(msg["id"].as_u64().unwrap(), json!(1));
                break;
            }
            texts.push(msg["text"].as_str().unwrap().to_string());
        }
        host.send(run("helloWasm.loop"));
        // Trapped, so it's gone.
        host.send(run("helloWasm.sayHello"));
        drop(host);
        texts.extend(rx.map(|m| m["text"].as_str().unwrap().to_string()));
        assert_eq!(
            texts[..6],
            [
                "$(rocket) Hello WASM",
                "activated",
                "Hello from WebAssembly! (1)",
                "$(rocket) Hello WASM: 1",
                r#"[{"$uri":"file:///c%3A/x/a.rs"}] "Hi" 3 c:\x\a.rs rust 2"#,
                "picked 1",
            ]
        );
        assert!(texts[6].contains("all fuel consumed"), "{}", texts[6]);
        assert!(texts[7].contains("not found"), "{}", texts[7]);
        assert_eq!(texts.len(), 8);
    }

    /// examples/hello-wasm, when built, as plain text's language server: a
    /// notification it answers with diagnostics, a request it serves, and one
    /// for a document of another language.
    #[test]
    fn hello_wasm_lsp() {
        let wasm = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("examples/hello-wasm/target/wasm32-unknown-unknown/release/hello_wasm.wasm");
        if !wasm.exists() {
            return eprintln!("skipped: build examples/hello-wasm first");
        }
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let host = start(tx);
        host.send(Req::Activate {
            id: "vsn.hello-wasm".into(),
            wasm,
            defaults: Value::Null,
            world: World::default(),
            vsn: json!({ "languages": ["plaintext"] }),
        });
        let open = |uri: &str, lang: &str| {
            Req::Lsp(
                json!({ "method": "textDocument/didOpen", "params": { "textDocument": {
                    "uri": uri, "languageId": lang, "version": 1, "text": "hi",
                }}}),
            )
        };
        let hover = |id: i64, uri: &str| {
            Req::Lsp(
                json!({ "id": id, "method": "textDocument/hover", "params": {
                    "textDocument": { "uri": uri }, "position": { "line": 0, "character": 0 },
                }}),
            )
        };
        host.send(open("file:///a.txt", "plaintext"));
        host.send(open("file:///b.rs", "rust"));
        host.send(hover(1, "file:///a.txt"));
        host.send(hover(2, "file:///b.rs"));
        drop(host);
        let msgs: Vec<Value> = futures::executor::block_on_stream(rx)
            .filter(|m| m["m"] == "lsp")
            .map(|m| m["msg"].clone())
            .collect();
        assert_eq!(msgs.len(), 3, "{msgs:?}");
        assert_eq!(msgs[0]["method"], "textDocument/publishDiagnostics");
        assert_eq!(msgs[0]["params"]["uri"], "file:///a.txt");
        assert_eq!(msgs[0]["params"]["diagnostics"][0]["severity"], 4);
        assert_eq!(msgs[1]["id"], 1);
        assert_eq!(
            msgs[1]["result"]["contents"]["value"],
            "**Hello** from WebAssembly"
        );
        assert_eq!(
            msgs[2],
            json!({ "jsonrpc": "2.0", "id": 2, "result": null })
        );
    }

    /// examples/hello-wasm, when built, reading the active document's file:
    /// without the permission, then with it once the user allows it, which
    /// isn't asked again.
    #[test]
    fn hello_wasm_files() {
        let wasm = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("examples/hello-wasm/target/wasm32-unknown-unknown/release/hello_wasm.wasm");
        if !wasm.exists() {
            return eprintln!("skipped: build examples/hello-wasm first");
        }
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let len = std::fs::read_to_string(&file).unwrap().len();
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let host = start(tx);
        let activate = |id: &str, vsn| Req::Activate {
            id: id.into(),
            wasm: wasm.clone(),
            defaults: Value::Null,
            world: World::default(),
            vsn,
        };
        let read = |id: &str| Req::Run {
            id: id.into(),
            command: "helloWasm.read".into(),
            args: json!([]),
            world: World {
                settings: Value::Null,
                editor: json!({ "uri": "", "path": file, "languageId": "toml", "text": "" }),
            },
        };
        host.send(activate("a", Value::Null));
        host.send(activate("b", json!({ "permissions": ["files"] })));
        host.send(read("a"));
        host.send(read("b"));
        let mut rx = futures::executor::block_on_stream(rx);
        let mut texts = Vec::new();
        let mut asked = 0;
        for msg in rx.by_ref() {
            if msg["m"] == "quickPick" {
                asked += 1;
                assert_eq!(
                    msg["placeholder"],
                    "Allow extension 'b' to read and write files?"
                );
                host.answer(msg["id"].as_u64().unwrap(), json!(0));
                host.send(read("b"));
                drop(host);
                break;
            }
            texts.push(msg["text"].as_str().unwrap().to_string());
        }
        texts.extend(rx.map(|m| m["text"].as_str().unwrap().to_string()));
        assert_eq!(asked, 1);
        let read = format!("read {len} bytes");
        assert_eq!(
            texts[4..],
            [
                r#"error: "files" isn't in package.json's vsn.permissions"#.to_string(),
                read.clone(),
                read
            ]
        );
    }
}
