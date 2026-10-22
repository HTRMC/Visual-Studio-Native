//! The JS extension host, as EXTENSIONS.md lays it out: one Node or Bun
//! process running every JS extension against exthost.js's `vscode` shim,
//! started when the first extension activates and talking newline-delimited
//! JSON on stdio. It exits when vsn closes its stdin.

use std::{
    collections::HashSet,
    io::{BufRead as _, BufReader, Write as _},
    process::{Command, Stdio},
    sync::mpsc,
};

use serde_json::{Value, json};

use super::extensions::{Extension, installed};
use super::*;
use crate::theme::codicons;

const HOST: &str = include_str!("exthost.js");
/// VS Code gives up on a host that crashes this many times.
const RESTARTS: usize = 3;

/// Runs an extension's command, from the palette or a keybinding.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = vsn, no_json)]
pub(crate) struct RunExtensionCommand {
    pub id: String,
}

/// A status bar entry an extension shows.
pub(super) struct StatusItem {
    id: String,
    text: String,
    tooltip: String,
    command: Option<String>,
    args: Vec<Value>,
    right: bool,
    priority: f64,
}

#[derive(Default)]
pub(super) struct Host {
    out: Option<mpsc::Sender<String>>,
    /// The extensions activated in this host, to activate again in the next.
    activated: HashSet<String>,
    status: Vec<StatusItem>,
    /// Counts the hosts started, so a closed one's last messages are ignored.
    generation: usize,
    crashes: usize,
}

impl Host {
    fn send(&self, msg: Value) {
        if let Some(out) = &self.out {
            out.send(msg.to_string() + "\n").ok();
        }
    }

    pub fn send_settings(&self) {
        self.send(json!({ "m": "settings", "settings": terminal::settings() }));
    }
}

/// The runtimes to try: `VSN_JS_RUNTIME`, one bundled in `runtime` next to
/// vsn, then Node and Bun on PATH. Node first: on Windows it starts in
/// ~25ms where Bun takes ~630ms (EXTENSIONS.md, Bun or Node).
fn runtimes() -> Vec<std::ffi::OsString> {
    let mut list: Vec<std::ffi::OsString> =
        std::env::var_os("VSN_JS_RUNTIME").into_iter().collect();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| Some(e.parent()?.join("runtime")))
    {
        list.extend(["node", "bun"].map(|n| {
            dir.join(n)
                .with_extension(std::env::consts::EXE_EXTENSION)
                .into()
        }));
    }
    list.extend(["node", "bun"].map(Into::into));
    list
}

/// Whether `ext` activates on `event`. Startup ("*") also covers
/// `onStartupFinished` and `workspaceContains`, and a command activates the
/// extension that contributes it (as VS Code does since 1.74).
fn wants(ext: &Extension, event: &str, root: Option<&Path>) -> bool {
    let listed = (ext.manifest["activationEvents"]
        .as_array()
        .into_iter()
        .flatten())
    .filter_map(Value::as_str)
    .any(|e| {
        e == event
            || (event == "*" && e == "onStartupFinished")
            || (event == "*")
                .then(|| e.strip_prefix("workspaceContains:"))
                .flatten()
                // ponytail: plain paths only; glob patterns need a walk of the folder.
                .is_some_and(|p| {
                    !p.contains(['*', '?']) && root.is_some_and(|r| r.join(p).exists())
                })
    });
    listed
        || event.strip_prefix("onCommand:").is_some_and(|cmd| {
            ext.contributes("commands")
                .any(|c| c["command"].as_str() == Some(cmd))
        })
}

/// The palette's extension commands, "Category: Title" and the command id,
/// less those the manifest hides from it (`when: false`).
pub(super) fn palette_commands() -> Vec<(String, String)> {
    let mut list = Vec::new();
    for e in installed().iter().filter(|e| e.enabled) {
        let hidden: HashSet<&str> = (e.manifest["contributes"]["menus"]["commandPalette"]
            .as_array())
        .into_iter()
        .flatten()
        .filter(|m| m["when"] == "false")
        .filter_map(|m| m["command"].as_str())
        .collect();
        for c in e.contributes("commands") {
            let Some(id) = c["command"].as_str().filter(|id| !hidden.contains(id)) else {
                continue;
            };
            let (title, category) = (e.localize(&c["title"]), e.localize(&c["category"]));
            let label = match category.is_empty() {
                true => title,
                false => format!("{category}: {title}"),
            };
            list.push((label, id.to_string()));
        }
    }
    list
}

/// Extensions' keybindings, VS Code's "ctrl+shift+h" as gpui's "ctrl-shift-h".
// ponytail: a binding with a `when` clause is skipped, as vsn doesn't
// evaluate those yet; and bindings are read at startup, so a newly
// installed extension's keys work after a restart.
pub(super) fn key_bindings(mapper: &dyn PlatformKeyboardMapper) -> Vec<KeyBinding> {
    let platform = match std::env::consts::OS {
        "macos" => "mac",
        "windows" => "win",
        _ => "linux",
    };
    let mut list = Vec::new();
    for e in installed().iter().filter(|e| e.enabled) {
        for k in e.contributes("keybindings").filter(|k| k["when"].is_null()) {
            let (Some(key), Some(id)) = (
                k[platform].as_str().or(k["key"].as_str()),
                k["command"].as_str(),
            ) else {
                continue;
            };
            let keys = (key.to_lowercase().split_whitespace())
                .map(|chord| chord.replace('+', "-"))
                .collect::<Vec<_>>()
                .join(" ");
            let action = Box::new(RunExtensionCommand { id: id.into() });
            list.extend(KeyBinding::load(&keys, action, None, false, None, mapper).ok());
        }
    }
    list
}

/// A status bar label, its `$(name)` codicons drawn.
fn status_label(text: &str) -> Vec<AnyElement> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("$(") {
        let Some(len) = rest[start..].find(')') else {
            break;
        };
        let text = rest[..start].trim();
        if !text.is_empty() {
            parts.push(text.to_string().into_any_element());
        }
        // `$(sync~spin)` names its animation after the `~`.
        let name = rest[start + 2..start + len]
            .split('~')
            .next()
            .unwrap_or_default();
        if let Some((_, ch)) = codicons().find(|(n, _)| *n == name) {
            parts.push(codicon(ch).text_size(px(14.)).into_any_element());
        }
        rest = &rest[start + len + 1..];
    }
    if !rest.trim().is_empty() {
        parts.push(rest.trim().to_string().into_any_element());
    }
    parts
}

impl Workbench {
    /// Activates the extensions waiting for `event`, starting the host for
    /// the first.
    pub(super) fn activate_extensions(&mut self, event: &str, cx: &mut Context<Self>) {
        let ids: Vec<String> = (installed().iter())
            .filter(|e| e.enabled && e.manifest["main"].is_string())
            .filter(|e| !self.ext.activated.contains(&e.id))
            .filter(|e| wants(e, event, self.root.as_deref()))
            .map(|e| e.id.clone())
            .collect();
        if ids.is_empty() || (self.ext.out.is_none() && !self.start_host(cx)) {
            return;
        }
        for id in ids {
            self.ext.send(json!({ "m": "activate", "id": id }));
            self.ext.activated.insert(id);
        }
    }

    /// Runs extension command `id`, activating its extension first.
    pub(super) fn run_extension_command(
        &mut self,
        id: String,
        args: Vec<Value>,
        cx: &mut Context<Self>,
    ) {
        self.activate_extensions(&format!("onCommand:{id}"), cx);
        match self.ext.out {
            Some(_) => self
                .ext
                .send(json!({ "m": "run", "command": id, "args": args })),
            None => self.error(format!("command '{id}' not found"), cx),
        }
    }

    /// Closes the host so a disabled or removed extension stops; the others
    /// activate again in a new one.
    pub(super) fn restart_extension_host(&mut self, cx: &mut Context<Self>) {
        if self.ext.out.take().is_some() {
            self.reactivate(cx);
        }
    }

    /// Starts a new host for the extensions the last one had, still enabled.
    fn reactivate(&mut self, cx: &mut Context<Self>) {
        self.ext.status.clear();
        let ids = std::mem::take(&mut self.ext.activated);
        let ids: Vec<String> = (installed().iter())
            .filter(|e| e.enabled && ids.contains(&e.id))
            .map(|e| e.id.clone())
            .collect();
        if !ids.is_empty() && self.start_host(cx) {
            for id in ids {
                self.ext.send(json!({ "m": "activate", "id": id }));
                self.ext.activated.insert(id);
            }
        }
        cx.notify();
    }

    fn start_host(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(dir) = dirs::home_dir().map(|h| h.join(".vsn")) else {
            return false;
        };
        let script = dir.join("exthost.js");
        if let Err(err) = fs::create_dir_all(&dir).and_then(|()| fs::write(&script, HOST)) {
            self.error(format!("Unable to start the extension host: {err}"), cx);
            return false;
        }
        let spawned = runtimes().into_iter().find_map(|exe| {
            let mut command = Command::new(exe);
            command
                .arg(&script)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            #[cfg(windows)]
            std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000); // CREATE_NO_WINDOW
            command.spawn().ok()
        });
        let Some(mut child) = spawned else {
            self.error("Extensions need Node.js or Bun, and neither was found.", cx);
            return false;
        };
        let (mut stdin, stdout, stderr) = (
            child.stdin.take().unwrap(),
            child.stdout.take().unwrap(),
            child.stderr.take().unwrap(),
        );
        let (out, out_rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for msg in out_rx {
                if stdin
                    .write_all(msg.as_bytes())
                    .and_then(|()| stdin.flush())
                    .is_err()
                {
                    return;
                }
            }
        });
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Value>();
        let log = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log.unbounded_send(
                    json!({ "m": "log", "channel": "Extension Host", "text": line }),
                )
                .ok();
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(msg) = serde_json::from_str(&line) {
                    tx.unbounded_send(msg).ok();
                }
            }
            child.wait().ok();
        });
        self.ext.generation += 1;
        let generation = self.ext.generation;
        self.lsp_host_gone(cx);
        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let current = |this: &mut Self| this.ext.generation == generation;
                if (this.update(cx, |this, cx| current(this).then(|| this.on_host(msg, cx))))
                    .is_err()
                {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                if this.ext.generation == generation {
                    this.host_exited(cx)
                }
            })
            .ok();
        })
        .detach();
        self.ext.out = Some(out);
        // Defaults from every extension's configuration, for getConfiguration.
        let mut defaults = serde_json::Map::new();
        let mut extensions = Vec::new();
        for e in installed().iter().filter(|e| e.enabled) {
            for c in e.contributes("configuration") {
                for (key, prop) in c["properties"].as_object().into_iter().flatten() {
                    if !prop["default"].is_null() {
                        defaults.insert(key.clone(), prop["default"].clone());
                    }
                }
            }
            extensions.push(json!({ "id": e.id, "dir": e.dir, "manifest": e.manifest }));
        }
        self.ext.send(json!({
            "m": "init",
            "settings": terminal::settings(),
            "defaults": defaults,
            "root": self.root,
            "extensions": extensions,
        }));
        true
    }

    fn on_host(&mut self, msg: Value, cx: &mut Context<Self>) {
        let text = |k: &str| msg[k].as_str().unwrap_or_default().to_string();
        match msg["m"].as_str().unwrap_or_default() {
            "message" => {
                let severity = match msg["severity"].as_str() {
                    Some("error") => Severity::Error,
                    Some("warning") => Severity::Warning,
                    _ => Severity::Info,
                };
                self.note(text("text").into(), severity, cx);
            }
            "error" => {
                // The stack goes to Output; the toast gets its first line.
                let text = text("text");
                self.log("Extension Host", &text, cx);
                self.error(text.lines().next().unwrap_or_default().to_string(), cx);
            }
            "log" => self.log(&text("channel"), &text("text"), cx),
            "languages" => {
                let ids = (msg["ids"].as_array().into_iter().flatten())
                    .filter_map(|id| Some(id.as_str()?.to_string()))
                    .collect();
                if let Some(out) = self.ext.out.clone() {
                    self.lsp_languages(ids, out, cx);
                }
            }
            "lsp" => self.lsp_from_host(msg["msg"].clone(), cx),
            "status" => {
                let id = text("id");
                self.ext.status.retain(|i| i.id != id);
                if msg["visible"] == true {
                    self.ext.status.push(StatusItem {
                        id,
                        text: text("text"),
                        tooltip: text("tooltip"),
                        command: msg["command"].as_str().map(str::to_string),
                        args: msg["args"].as_array().cloned().unwrap_or_default(),
                        right: msg["alignment"] == 2,
                        priority: msg["priority"].as_f64().unwrap_or_default(),
                    });
                }
                cx.notify();
            }
            "setting" => {
                if let Err(err) = terminal::set_setting(&text("key"), msg["value"].clone()) {
                    self.settings_error(err, cx);
                }
            }
            // ponytail: VS Code's own commands (vscode.open, workbench.*) aren't
            // mapped onto vsn's actions yet.
            "execute" => {
                let text = format!("command '{}' not found", text("command"));
                self.log("Extension Host", &text, cx);
            }
            _ => {}
        }
    }

    /// The current host ended: closed on purpose, or a crash, which VS Code
    /// restarts from up to 3 times.
    fn host_exited(&mut self, cx: &mut Context<Self>) {
        if self.ext.out.take().is_none() {
            return;
        }
        if self.ext.crashes == RESTARTS {
            // ponytail: counts every crash, not VS Code's last 5 minutes.
            self.error(
                "Extension host terminated unexpectedly 3 times within the last 5 minutes.",
                cx,
            );
            self.ext.status.clear();
            self.lsp_host_gone(cx);
            return cx.notify();
        }
        self.ext.crashes += 1;
        self.error("Extension host terminated unexpectedly. Restarting...", cx);
        self.reactivate(cx);
    }

    /// Extensions' status bar entries on one side, highest priority leftmost.
    pub(super) fn extension_status(&self, right: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut items: Vec<&StatusItem> = (self.ext.status.iter())
            .filter(|i| i.right == right)
            .collect();
        items.sort_by(|a, b| b.priority.total_cmp(&a.priority));
        (items.into_iter().enumerate())
            .map(|(ix, item)| {
                let tip = SharedString::from(item.tooltip.clone());
                let (command, args) = (item.command.clone(), item.args.clone());
                h_flex()
                    .id(("sb-ext", ix + if right { 1000 } else { 0 }))
                    .h_full()
                    .px(px(5.))
                    .gap(px(4.))
                    .children(status_label(&item.text))
                    .when(!tip.is_empty(), |d| {
                        d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    })
                    .when_some(command, |d, command| {
                        d.cursor_pointer()
                            .hover(|s| s.bg(col::status_hover()).text_color(col::fg()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.run_extension_command(command.clone(), args.clone(), cx)
                            }))
                    })
                    .into_any_element()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::status_label;

    #[test]
    fn splits_status_icons() {
        assert_eq!(status_label("$(heart) Hello 3").len(), 2);
        assert_eq!(status_label("a $(nosuchicon) b").len(), 2);
        assert_eq!(status_label("$(sync~spin)").len(), 1);
        assert_eq!(status_label("plain").len(), 1);
    }
}
