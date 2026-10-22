//! The WASM host, as EXTENSIONS.md lays it out: native plugins built against
//! `wit/vsn.wit`, run by wasmtime on a thread of their own, each in its own
//! store with fuel and memory limits. They have no WASI, so no file, clock or
//! network access. What they ask of the editor comes back as the JS host's
//! messages, so both hosts share one path into the workbench.

use std::{collections::HashMap, path::PathBuf, sync::mpsc};

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
    },
    Run {
        id: String,
        command: String,
    },
}

struct State {
    id: String,
    tx: UnboundedSender<Value>,
    limits: StoreLimits,
}

impl State {
    fn send(&self, msg: Value) {
        self.tx.unbounded_send(msg).ok();
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
}

/// Starts the host thread; it ends when the returned sender is dropped.
pub(super) fn start(tx: UnboundedSender<Value>) -> mpsc::Sender<Req> {
    let (req, rx) = mpsc::channel();
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
        for req in rx {
            let (id, result) = match req {
                Req::Activate { id, wasm } => {
                    if plugins.contains_key(&id) {
                        continue;
                    }
                    let result = load(&engine, &linker, &id, &wasm, &tx).and_then(|mut p| {
                        p.0.set_fuel(FUEL)?;
                        p.1.call_activate(&mut p.0)?;
                        plugins.insert(id.clone(), p);
                        Ok(())
                    });
                    (id, result)
                }
                Req::Run { id, command } => {
                    let result = match plugins.get_mut(&id) {
                        Some((store, plugin)) => store
                            .set_fuel(FUEL)
                            .and_then(|()| plugin.call_run_command(&mut *store, &command))
                            .map_err(Into::into),
                        None => Err(anyhow::anyhow!("command '{command}' not found")),
                    };
                    (id, result)
                }
            };
            // A trap leaves the plugin's state unknown, so it doesn't run again.
            if let Err(err) = result {
                plugins.remove(&id);
                error(&tx, format!("Extension '{id}' failed: {err:#}"));
            }
        }
    });
    req
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
    use futures::StreamExt as _;

    /// Runs examples/hello-wasm, when built, through the host: its activation,
    /// a command, and one that never returns until the fuel runs out.
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
        };
        host.send(Req::Activate {
            id: id.clone(),
            wasm,
        })
        .unwrap();
        host.send(run("helloWasm.sayHello")).unwrap();
        host.send(run("helloWasm.loop")).unwrap();
        // Trapped, so it's gone.
        host.send(run("helloWasm.sayHello")).unwrap();
        drop(host);
        let msgs: Vec<Value> = futures::executor::block_on(rx.collect());
        let texts: Vec<&str> = msgs.iter().map(|m| m["text"].as_str().unwrap()).collect();
        assert_eq!(
            texts[..4],
            [
                "$(rocket) Hello WASM",
                "activated",
                "Hello from WebAssembly! (1)",
                "$(rocket) Hello WASM: 1"
            ]
        );
        assert!(texts[4].contains("all fuel consumed"), "{}", texts[4]);
        assert!(texts[5].contains("not found"), "{}", texts[5]);
        assert_eq!(msgs.len(), 6);
    }
}
