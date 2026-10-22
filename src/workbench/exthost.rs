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
use super::palette::{Items, Picker, Rename};
use super::wasmhost;
use super::*;
use crate::theme::codicons;
use gpui_kit::component::native_menu::NativeMenu;

const HOST: &str = include_str!("exthost.js");
/// VS Code gives up on a host that crashes this many times.
const RESTARTS: usize = 3;

/// Runs an extension's command, from the palette, a keybinding or a menu;
/// a menu passes its file's Uri.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = vsn, no_json)]
pub(crate) struct RunExtensionCommand {
    pub id: String,
    pub path: Option<PathBuf>,
}

impl RunExtensionCommand {
    pub(super) fn args(&self) -> Vec<Value> {
        (self.path.iter())
            .map(|p| json!({ "$uri": super::lsp::path_uri(p) }))
            .collect()
    }
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
    /// From a WASM plugin, so it outlives a JS host restart.
    wasm: bool,
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
    /// The window, for the palette an extension's quick pick or input box opens.
    window: Option<AnyWindowHandle>,
    /// Notifications with an extension's buttons: the host's question and the
    /// buttons' titles.
    pub(super) buttons: HashMap<SharedString, (u64, Vec<String>)>,
    /// The WASM host, started with the first WASM plugin.
    wasm: Option<mpsc::Sender<wasmhost::Req>>,
    wasm_generation: usize,
}

impl Host {
    fn send(&self, msg: Value) {
        if let Some(out) = &self.out {
            out.send(msg.to_string() + "\n").ok();
        }
    }

    pub(super) fn new(window: AnyWindowHandle) -> Self {
        Self {
            window: Some(window),
            ..Default::default()
        }
    }

    /// Answers the host's question `id`; null is cancelled.
    pub(super) fn answer(&self, id: u64, value: Value) {
        self.send(json!({ "m": "answer", "id": id, "value": value }));
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

/// A WASM plugin's module, from its manifest's `"vsn": { "wasm": ... }`.
fn wasm_path(ext: &Extension) -> Option<PathBuf> {
    Some(ext.dir.join(ext.manifest["vsn"]["wasm"].as_str()?))
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
                .is_some_and(|p| root.is_some_and(|r| contains(r, p)))
    });
    listed
        || event.strip_prefix("onCommand:").is_some_and(|cmd| {
            ext.contributes("commands")
                .any(|c| c["command"].as_str() == Some(cmd))
        })
}

/// Whether folder `root` has a file matching `pattern`, a path or a glob
/// relative to it (`**/*.zig`).
// ponytail: walks on the UI thread, honouring .gitignore and giving up after
// 10,000 entries; VS Code searches in the background with a timeout.
fn contains(root: &Path, pattern: &str) -> bool {
    if !pattern.contains(['*', '?', '[', '{']) {
        return root.join(pattern).exists();
    }
    let mut b = ignore::overrides::OverrideBuilder::new(root);
    let anchored = format!("/{}", pattern.trim_start_matches('/'));
    let Ok(overrides) = b.add(&anchored).map(|_| ()).and_then(|()| b.build()) else {
        return false;
    };
    // Whitelisted overrides keep only the matching files, but every folder.
    (ignore::WalkBuilder::new(root)
        .hidden(false)
        .overrides(overrides)
        .build())
    .take(10_000)
    .filter_map(Result::ok)
    .any(|e| e.depth() > 0 && e.file_type().is_some_and(|t| t.is_file()))
}

/// The palette's extension commands, "Category: Title" and the command id,
/// less those whose `commandPalette` menu `when` is false where focus was.
pub(super) fn palette_commands(focus: &[KeyContext]) -> Vec<(String, String)> {
    let mut list = Vec::new();
    for e in installed().iter().filter(|e| e.enabled) {
        let hidden: HashSet<&str> = (e.manifest["contributes"]["menus"]["commandPalette"]
            .as_array())
        .into_iter()
        .flatten()
        .filter(|m| {
            m["when"]
                .as_str()
                .is_some_and(|w| when_predicate(w).is_some_and(|p| p.depth_of(focus).is_none()))
        })
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

/// Whether an enabled extension adds to one of VS Code's `menus`.
pub(super) fn has_menu(menu: &str) -> bool {
    (installed().iter()).any(|e| e.enabled && e.manifest["contributes"]["menus"][menu].is_array())
}

/// An extension's item in one of VS Code's `menus`.
pub(super) struct MenuItem {
    pub title: String,
    pub id: String,
    /// Its command's `$(codicon)` icon.
    pub icon: Option<char>,
    /// In the "navigation" group, which `editor/title` shows as buttons.
    pub navigation: bool,
}

/// Extensions' items in one of VS Code's `menus` (`editor/context`, ...)
/// whose `when` holds on `focus`, by group.
// ponytail: groups only order the items (navigation first, then by name),
// with no separators or `@order`; `submenu`s and `alt` commands are skipped.
pub(super) fn menu_items(menu: &str, focus: &[KeyContext]) -> Vec<MenuItem> {
    let mut list = Vec::new();
    for e in installed().iter().filter(|e| e.enabled) {
        let items = e.manifest["contributes"]["menus"][menu].as_array();
        for m in items.into_iter().flatten() {
            let Some(id) = m["command"].as_str() else {
                continue;
            };
            let shown = (m["when"].as_str())
                .is_none_or(|w| when_predicate(w).is_some_and(|p| p.depth_of(focus).is_some()));
            let Some(c) = e.contributes("commands").find(|c| c["command"] == id) else {
                continue;
            };
            if shown {
                let group = m["group"].as_str().unwrap_or("");
                let group = group.split('@').next().unwrap_or("");
                let icon = (c["icon"].as_str())
                    .and_then(|i| i.strip_prefix("$(")?.strip_suffix(')'))
                    .and_then(|name| codicons().find(|(n, _)| *n == name))
                    .map(|(_, ch)| ch);
                list.push((
                    group.to_string(),
                    MenuItem {
                        title: e.localize(&c["title"]),
                        id: id.to_string(),
                        icon,
                        navigation: group == "navigation",
                    },
                ));
            }
        }
    }
    list.sort_by(|a, b| (!a.1.navigation, &a.0).cmp(&(!b.1.navigation, &b.0)));
    list.into_iter().map(|(_, item)| item).collect()
}

/// VS Code's context keys for a file's editor, for `when` clauses.
pub(super) fn editor_keys(path: &Path, lang: &str) -> KeyContext {
    let mut keys = KeyContext::default();
    for key in ["editorFocus", "editorTextFocus", "textInputFocus"] {
        keys.add(key);
    }
    keys.set("editorLangId", lang);
    keys.set("resourceLangId", lang);
    resource_keys(&mut keys, path);
    keys
}

/// VS Code's `resource*` keys, their values as `when_predicate` spells them.
pub(super) fn resource_keys(keys: &mut KeyContext, path: &Path) {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()));
    keys.set("resourceScheme", "file");
    keys.set("resourceFilename", mangle(&name));
    keys.set("resourceExtname", mangle(&ext.unwrap_or_default()));
}

/// A `when` clause's word as a gpui identifier.
fn mangle(word: &str) -> String {
    word.replace(|c: char| !c.is_alphanumeric() && c != '-', "_")
}

/// The editor's right-click menu with extensions' `editor/context` items:
/// the input's own items, which a custom menu replaces, then theirs.
// ponytail: the input's items are always enabled here, as its capabilities
// aren't passed to a custom menu; vsn only uses one with extension items.
pub(super) fn editor_context_menu(menu: NativeMenu, path: &Path, window: &Window) -> NativeMenu {
    let menu = menu
        .menu("Go to Definition", Box::new(input::GoToDefinition))
        .menu("Show Code Actions", Box::new(input::ToggleCodeActions))
        .separator()
        .menu("Cut", Box::new(input::Cut))
        .menu("Copy", Box::new(input::Copy))
        .menu("Paste", Box::new(input::Paste))
        .separator()
        .menu("Select All", Box::new(input::SelectAll));
    let items = menu_items("editor/context", &window.context_stack());
    let menu = if items.is_empty() {
        menu
    } else {
        menu.separator()
    };
    items.into_iter().fold(menu, |menu, item| {
        let path = Some(path.to_path_buf());
        let id = item.id;
        menu.menu(item.title, Box::new(RunExtensionCommand { id, path }))
    })
}

/// A VS Code `when` clause as a gpui context predicate. vsn's editor,
/// terminal and explorer set VS Code's focus keys and `editorLangId`; other
/// keys are unset, so false as in VS Code when nothing sets them.
// ponytail: all keys of a clause are matched on one element's context, so
// it's focus keys plus the editor's language; `<`, `=~` and `in` aren't
// expressible in gpui's and their bindings are skipped.
fn when_predicate(when: &str) -> Option<KeyBindingContextPredicate> {
    if ["<", ">", "=~", " in "].iter().any(|op| when.contains(op)) {
        return None;
    }
    let os = match std::env::consts::OS {
        "macos" => "isMac",
        "windows" => "isWindows",
        _ => "isLinux",
    };
    let word = |w: &str| -> String {
        let w = w.trim_matches(['\'', '"']);
        match w {
            "true" => "!false_".into(),
            "isMac" | "isWindows" | "isLinux" | "isWeb" if w == os => "!false_".into(),
            "false" | "isMac" | "isWindows" | "isLinux" | "isWeb" => "false_".into(),
            _ => mangle(w),
        }
    };
    let mut out = String::new();
    let mut rest = when;
    while let Some(c) = rest.chars().next() {
        let len = match "!&|=() \t".contains(c) {
            true => c.len_utf8(),
            false => rest.find(|c| "!&|=() \t".contains(c)).unwrap_or(rest.len()),
        };
        let (tok, tail) = rest.split_at(len);
        out += &match "!&|=() \t".contains(c) {
            true => tok.to_string(),
            false => word(tok),
        };
        rest = tail;
    }
    KeyBindingContextPredicate::parse(&out).ok()
}

/// Extensions' keybindings, VS Code's "ctrl+shift+h" as gpui's "ctrl-shift-h".
pub(super) fn key_bindings(mapper: &dyn PlatformKeyboardMapper) -> Vec<KeyBinding> {
    let platform = match std::env::consts::OS {
        "macos" => "mac",
        "windows" => "win",
        _ => "linux",
    };
    let mut list = Vec::new();
    for e in installed().iter().filter(|e| e.enabled) {
        for k in e.contributes("keybindings") {
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
            let context = match k["when"].as_str() {
                Some(when) => match when_predicate(when) {
                    Some(p) => Some(p.into()),
                    None => continue,
                },
                None => None,
            };
            let action = Box::new(RunExtensionCommand {
                id: id.into(),
                path: None,
            });
            list.extend(KeyBinding::load(&keys, action, context, false, None, mapper).ok());
        }
    }
    list
}

/// vsn's action for VS Code built-in command `id`.
// ponytail: the commands extensions run most; others log "not found".
fn builtin_action(id: &str) -> Option<Box<dyn Action>> {
    Some(match id {
        "workbench.action.files.save" => Box::new(Save),
        "workbench.action.files.saveAll" => Box::new(SaveAll),
        "workbench.action.files.openFile" => Box::new(OpenFile),
        "workbench.action.files.openFolder" => Box::new(OpenFolder),
        "workbench.action.files.newUntitledFile" => Box::new(NewTextFile),
        "workbench.action.closeActiveEditor" => Box::new(CloseEditor),
        "workbench.action.closeAllEditors" => Box::new(CloseAllEditors),
        "workbench.action.closeWindow" => Box::new(CloseWindow),
        "workbench.action.toggleSidebarVisibility" => Box::new(ToggleSidebar),
        "workbench.action.togglePanel" => Box::new(TogglePanel),
        "workbench.action.terminal.toggleTerminal" => Box::new(ToggleTerminal),
        "workbench.action.terminal.new" => Box::new(NewTerminal),
        "workbench.actions.view.problems" => Box::new(ToggleProblems),
        "workbench.action.output.toggleOutput" => Box::new(ToggleOutput),
        "workbench.view.explorer" => Box::new(ShowExplorer),
        "workbench.view.search" => Box::new(ShowSearch),
        "workbench.view.scm" => Box::new(ShowScm),
        "workbench.view.debug" => Box::new(ShowRun),
        "workbench.view.extensions" => Box::new(ShowExtensions),
        "workbench.action.showCommands" => Box::new(CommandPalette),
        "workbench.action.quickOpen" => Box::new(QuickOpen),
        "workbench.action.gotoLine" => Box::new(GoToLine),
        "workbench.action.navigateBack" => Box::new(GoBack),
        "workbench.action.navigateForward" => Box::new(GoForward),
        "workbench.action.openSettings" => Box::new(OpenSettings),
        "workbench.action.openSettingsJson" => Box::new(OpenSettingsJson),
        "workbench.action.selectTheme" => Box::new(SelectColorTheme),
        "workbench.action.debug.start" => Box::new(StartDebugging),
        "workbench.action.debug.stop" => Box::new(StopDebugging),
        "editor.action.formatDocument" => Box::new(FormatDocument),
        "editor.action.formatSelection" => Box::new(FormatSelection),
        "editor.action.rename" => Box::new(RenameSymbol),
        "editor.action.commentLine" => Box::new(ToggleLineComment),
        _ => return None,
    })
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
        let exts: Vec<_> = (installed().iter())
            .filter(|e| e.enabled && (e.manifest["main"].is_string() || wasm_path(e).is_some()))
            .filter(|e| !self.ext.activated.contains(&e.id))
            .filter(|e| wants(e, event, self.root.as_deref()))
            .map(|e| (e.id.clone(), wasm_path(e)))
            .collect();
        self.activate_in_hosts(exts, cx);
    }

    /// Activates extensions in the host each runs in, a WASM module's or the
    /// JS one, starting it if needed.
    fn activate_in_hosts(&mut self, exts: Vec<(String, Option<PathBuf>)>, cx: &mut Context<Self>) {
        let mut js_failed = false;
        for (id, wasm) in exts {
            match wasm {
                Some(wasm) => {
                    if self.ext.wasm.is_none() {
                        self.start_wasm(cx);
                    }
                    if let Some(host) = &self.ext.wasm {
                        host.send(wasmhost::Req::Activate {
                            id: id.clone(),
                            wasm,
                        })
                        .ok();
                    }
                }
                None => {
                    if js_failed || (self.ext.out.is_none() && !self.start_host(cx)) {
                        js_failed = true;
                        continue;
                    }
                    self.ext.send(json!({ "m": "activate", "id": id }));
                }
            }
            self.ext.activated.insert(id);
        }
    }

    fn start_wasm(&mut self, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Value>();
        self.ext.wasm = Some(wasmhost::start(tx));
        self.ext.wasm_generation += 1;
        let generation = self.ext.wasm_generation;
        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let current = |this: &mut Self| this.ext.wasm_generation == generation;
                if (this.update(cx, |this, cx| current(this).then(|| this.on_host(msg, cx))))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    /// Tells the JS host the active editor, its text and selection, for
    /// `window.activeTextEditor`.
    // ponytail: sent on a tab switch and before each command; closing a tab
    // doesn't send the next one.
    pub(super) fn send_active_editor(&self, cx: &App) {
        if self.ext.out.is_none() {
            return;
        }
        let editor = (self.docs.get(self.active).filter(|d| !d.binary)).map(|d| {
            let e = d.editor.read(cx);
            let sel = e.selected_range();
            let [start, end] = [sel.start, sel.end].map(|o| e.text().offset_to_position(o));
            json!({
                "uri": super::lsp::path_uri(&d.path),
                "languageId": d.lang,
                "text": e.value(),
                "selection": [start, end],
            })
        });
        self.ext
            .send(json!({ "m": "activeEditor", "editor": editor }));
    }

    /// Runs extension command `id`, activating its extension first.
    pub(super) fn run_extension_command(
        &mut self,
        id: String,
        args: Vec<Value>,
        cx: &mut Context<Self>,
    ) {
        self.activate_extensions(&format!("onCommand:{id}"), cx);
        // ponytail: a WASM plugin's commands are the ones it contributes; it
        // can't register others, and gets no arguments.
        let plugin = (installed().iter())
            .filter(|e| e.enabled && wasm_path(e).is_some())
            .find(|e| {
                e.contributes("commands")
                    .any(|c| c["command"].as_str() == Some(&id))
            })
            .map(|e| e.id.clone());
        match (plugin, &self.ext.wasm, &self.ext.out) {
            (Some(plugin), Some(host), _) => {
                (host.send(wasmhost::Req::Run {
                    id: plugin,
                    command: id,
                }))
                .ok();
            }
            (None, _, Some(_)) => {
                self.send_active_editor(cx);
                (self.ext).send(json!({ "m": "run", "command": id, "args": args }));
            }
            _ => self.error(format!("command '{id}' not found"), cx),
        }
    }

    /// Closes the host so a disabled or removed extension stops; the others
    /// activate again in a new one. Keybindings follow the installed set.
    pub(super) fn restart_extension_host(&mut self, cx: &mut Context<Self>) {
        let mapper = cx.keyboard_mapper().clone();
        cx.clear_key_bindings();
        cx.bind_keys(super::key_bindings(mapper.as_ref()));
        let js = self.ext.out.take().is_some();
        let wasm = self.ext.wasm.take().is_some();
        if js || wasm {
            self.reactivate(cx);
        }
    }

    /// Starts new hosts for the extensions the closed ones had, still
    /// enabled. A WASM host still running keeps its plugins.
    fn reactivate(&mut self, cx: &mut Context<Self>) {
        let wasm_alive = self.ext.wasm.is_some();
        self.ext.status.retain(|i| i.wasm && wasm_alive);
        let ids = std::mem::take(&mut self.ext.activated);
        let (kept, exts): (Vec<_>, Vec<_>) = (installed().iter())
            .filter(|e| e.enabled && ids.contains(&e.id))
            .map(|e| (e.id.clone(), wasm_path(e)))
            .partition(|(_, wasm)| wasm.is_some() && wasm_alive);
        self.ext
            .activated
            .extend(kept.into_iter().map(|(id, _)| id));
        self.activate_in_hosts(exts, cx);
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
        self.send_active_editor(cx);
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
                let message: SharedString = text("text").into();
                self.note(message.clone(), severity, cx);
                let buttons: Vec<String> = (msg["buttons"].as_array().into_iter().flatten())
                    .filter_map(|b| Some(b.as_str()?.to_string()))
                    .collect();
                if let (Some(id), false) = (msg["id"].as_u64(), buttons.is_empty()) {
                    self.ext.buttons.insert(message, (id, buttons));
                }
            }
            "quickPick" | "inputBox" => {
                let (Some(id), Some(window)) = (msg["id"].as_u64(), self.ext.window) else {
                    return;
                };
                let placeholder: SharedString = text("placeholder").into();
                let rows = (msg["rows"].as_array().into_iter().flatten())
                    .map(|r| {
                        (
                            r[0].as_str().unwrap_or_default().into(),
                            r[1].as_str().unwrap_or_default().into(),
                        )
                    })
                    .collect();
                let (value, quick) = (text("value"), msg["m"] == "quickPick");
                let this = cx.entity();
                // The palette needs the window, which a host message comes without.
                cx.defer(move |cx| {
                    (window.update(cx, |_, window, cx| {
                        this.update(cx, |this, cx| {
                            this.close_palette(window, cx);
                            match quick {
                                true => this.open_items(
                                    Items {
                                        picker: Picker::Host(id),
                                        rows,
                                        groups: Vec::new(),
                                        placeholder,
                                        current: 0,
                                    },
                                    window,
                                    cx,
                                ),
                                false => this.rename_input(
                                    Rename::Host(id, placeholder),
                                    value,
                                    window,
                                    cx,
                                ),
                            }
                        })
                    }))
                    .ok();
                });
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
            "applyEdit" => {
                let (Ok(edit), Some(window)) = (
                    serde_json::from_value::<lsp_types::WorkspaceEdit>(msg["edit"].clone()),
                    self.ext.window,
                ) else {
                    return;
                };
                let this = cx.entity();
                cx.defer(move |cx| {
                    (window.update(cx, |_, window, cx| {
                        this.update(cx, |this, cx| this.apply_workspace_edit(edit, window, cx))
                    }))
                    .ok();
                });
            }
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
                        wasm: msg["wasm"] == true,
                    });
                }
                cx.notify();
            }
            "setting" => {
                if let Err(err) = terminal::set_setting(&text("key"), msg["value"].clone()) {
                    self.settings_error(err, cx);
                }
            }
            // VS Code's own commands, run as vsn's actions in the window.
            "execute" => {
                let command = text("command");
                let path = msg["args"][0]["fsPath"].as_str().map(PathBuf::from);
                if command == "workbench.action.reloadWindow" {
                    return self.restart_extension_host(cx);
                }
                let action = builtin_action(&command);
                let (Some(window), true) = (
                    self.ext.window,
                    action.is_some() || (command == "vscode.open" && path.is_some()),
                ) else {
                    let text = format!("command '{command}' not found");
                    return self.log("Extension Host", &text, cx);
                };
                let this = cx.entity();
                cx.defer(move |cx| {
                    (window.update(cx, |_, window, cx| match action {
                        Some(action) => window.dispatch_action(action, cx),
                        None => this.update(cx, |this, cx| {
                            this.open_path(path.unwrap_or_default(), window, cx)
                        }),
                    }))
                    .ok();
                });
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
            self.ext.status.retain(|i| i.wasm);
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
    use super::{KeyContext, editor_keys};
    use super::{contains, status_label, when_predicate};
    use std::path::Path;

    #[test]
    fn when_clauses() {
        let mut editor = KeyContext::default();
        editor.add("editorTextFocus");
        editor.set("editorLangId", "markdown");
        let terminal = KeyContext::parse("Terminal terminalFocus").unwrap();
        let eval = |when: &str, cx: &KeyContext| when_predicate(when).unwrap().eval(&[cx.clone()]);
        let md = "editorTextFocus && editorLangId == 'markdown' && !editorReadonly";
        assert!(eval(md, &editor));
        assert!(!eval(md, &terminal));
        assert!(!eval("editorLangId != markdown", &editor));
        assert!(eval(
            "terminalFocus || isWindows || isMac || isLinux",
            &terminal
        ));
        assert!(!eval("config.foo.enabled", &editor));
        assert!(eval("!(suggestWidgetVisible || false)", &editor));
        // The palette's check: anywhere along the focus path.
        let path = [terminal.clone(), editor.clone()];
        assert!(
            when_predicate("terminalFocus")
                .unwrap()
                .depth_of(&path)
                .is_some()
        );
        assert!(when_predicate("false").unwrap().depth_of(&path).is_none());
        assert!(
            when_predicate("editorTextFocus && view == 'x' || resourceExtname =~ /md/").is_none()
        );
        assert!(when_predicate("listFocus && foo > 2").is_none());
        let rs = editor_keys(Path::new("c:/x/my-file.rs"), "rust");
        let when = "resourceExtname == .rs && resourceFilename == 'my-file.rs'";
        assert!(eval(when, &rs));
        assert!(!eval("resourceExtname == .md", &rs));
    }

    #[test]
    fn workspace_contains_globs() {
        let root = std::env::temp_dir().join("vsn-contains");
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::write(root.join("src/deep/a.zig"), "").unwrap();
        assert!(contains(&root, "**/*.zig"));
        assert!(contains(&root, "src/deep/a.zig"));
        assert!(contains(&root, "src/**/a.zig"));
        assert!(!contains(&root, "*.zig"));
        assert!(!contains(&root, "**/*.rs"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn splits_status_icons() {
        assert_eq!(status_label("$(heart) Hello 3").len(), 2);
        assert_eq!(status_label("a $(nosuchicon) b").len(), 2);
        assert_eq!(status_label("$(sync~spin)").len(), 1);
        assert_eq!(status_label("plain").len(), 1);
    }
}
