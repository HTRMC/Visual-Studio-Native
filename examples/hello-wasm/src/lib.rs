//! A sample vsn plugin: two commands and a status bar entry, against
//! `wit/vsn.wit`. Build it for wasm32-unknown-unknown and copy the .wasm next
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

    fn run_command(command: String) {
        match command.as_str() {
            "helloWasm.sayHello" => {
                let n = HELLOS.fetch_add(1, Ordering::Relaxed) + 1;
                editor::show_message(Severity::Info, &format!("Hello from WebAssembly! ({n})"));
                editor::set_status(&format!("$(rocket) Hello WASM: {n}"));
            }
            // Never returns: vsn's fuel limit stops it, and the editor keeps going.
            "helloWasm.loop" => loop {
                std::hint::black_box(HELLOS.load(Ordering::Relaxed));
            },
            _ => editor::log(&format!("unknown command {command}")),
        }
    }
}

export!(Hello);
