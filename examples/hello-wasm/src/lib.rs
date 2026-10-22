//! A sample vsn plugin against `wit/vsn.wit`: commands, a status bar entry,
//! and hovers, completions and a hint for plain text files. Build it for wasm32-unknown-unknown and copy the .wasm next
//! to package.json; vsn wraps the core module into a component when loading.

use std::sync::atomic::{AtomicU32, Ordering};

use vsn::plugin::editor::{self, Severity};

wit_bindgen::generate!({ path: "../../wit", world: "plugin" });

static HELLOS: AtomicU32 = AtomicU32::new(0);

struct Hello;

impl Guest for Hello {
    fn activate() {
        editor::set_status("$(rocket) Hello WASM");
        editor::log("activated");
    }

    fn run_command(command: String, args: String) {
        match command.as_str() {
            "helloWasm.sayHello" => {
                let n = HELLOS.fetch_add(1, Ordering::Relaxed) + 1;
                editor::show_message(Severity::Info, &format!("Hello from WebAssembly! ({n})"));
                editor::set_status(&format!("$(rocket) Hello WASM: {n}"));
            }
            // What a plugin can read of the editor, and a question.
            "helloWasm.info" => {
                let setting = |key| editor::get_setting(key).unwrap_or_default();
                let (greeting, count) = (setting("hello.greeting"), setting("hello.count"));
                let doc = editor::active_document();
                let (path, language, len) =
                    doc.map_or_else(Default::default, |d| (d.path, d.language, d.text.len()));
                editor::log(&format!(
                    "{args} {greeting} {count} {path} {language} {len}"
                ));
                let items = ["a".to_string(), "b".to_string()];
                let picked = editor::show_quick_pick("Pick one", &items);
                editor::log(&format!("picked {}", picked.map_or(-1, i64::from)));
            }
            // An edit at the start of the active document.
            "helloWasm.insert" => {
                if let Some(doc) = editor::active_document() {
                    let at = r#"{"line":0,"character":0}"#;
                    let edit = format!(
                        r#"{{"changes":{{"{}":[{{"range":{{"start":{at},"end":{at}}},"newText":"Hello "}}]}}}}"#,
                        doc.uri
                    );
                    editor::apply_edit(&edit);
                }
            }
            // Never returns: vsn's fuel limit stops it, and the editor keeps going.
            "helloWasm.loop" => loop {
                std::hint::black_box(HELLOS.load(Ordering::Relaxed));
            },
            _ => editor::log(&format!("unknown command {command}")),
        }
    }

    // ponytail: string slicing for the one field it reads; a real plugin
    // would parse with serde_json.
    fn lsp(method: String, params: String) -> Option<String> {
        match method.as_str() {
            "textDocument/hover" => Some(
                r#"{"contents":{"kind":"markdown","value":"**Hello** from WebAssembly"}}"#.into(),
            ),
            "textDocument/completion" => Some(r#"[{"label":"hello"},{"label":"wasm"}]"#.into()),
            // A hint on the first character of each file opened.
            "textDocument/didOpen" => {
                let uri = params.split(r#""uri":""#).nth(1)?.split('"').next()?;
                let range = r#"{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}"#;
                editor::lsp_notify(
                    "textDocument/publishDiagnostics",
                    &format!(
                        r#"{{"uri":"{uri}","diagnostics":[{{"range":{range},"severity":4,"message":"Served by Hello WASM","source":"hello-wasm"}}]}}"#
                    ),
                );
                None
            }
            _ => None,
        }
    }
}

export!(Hello);
