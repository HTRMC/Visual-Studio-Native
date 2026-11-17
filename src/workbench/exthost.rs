//! The JS extension host, as EXTENSIONS.md lays it out: one Node process running every JS extension against exthost.js's `vscode` shim,
//! started when the first extension activates and talking newline-delimited
//! JSON on stdio. It exits when vsn closes its stdin.

use std::{
    collections::{BTreeMap, HashSet},
    io::{BufRead as _, BufReader, Write as _},
    process::{Command, Stdio},
    sync::{LazyLock, Mutex, mpsc},
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
#[derive(Clone, Debug, Action)]
#[action(namespace = vsn, no_json)]
pub(crate) struct RunExtensionCommand {
    pub id: String,
    pub path: Option<PathBuf>,
    /// A keybinding's `when`: when it doesn't hold, the key goes on to the
    /// next binding.
    pub when: Option<String>,
    /// A user keybinding's `args`, passed instead of the menu's Uri.
    pub args: Option<Value>,
}

/// Equal whatever their `when`, so the palette finds a command's keybinding.
impl PartialEq for RunExtensionCommand {
    fn eq(&self, other: &Self) -> bool {
        (&self.id, &self.path) == (&other.id, &other.path)
    }
}

impl RunExtensionCommand {
    pub(super) fn args(&self) -> Vec<Value> {
        if let Some(args) = &self.args {
            return vec![args.clone()];
        }
        uri_args(self.path.as_deref())
    }
}

/// A menu command's arguments for a file: its Uri.
pub(super) fn uri_args(path: Option<&Path>) -> Vec<Value> {
    (path.iter())
        .map(|p| json!({ "$uri": super::lsp::path_uri(p) }))
        .collect()
}

/// The SourceControl, resource group or resource state at `handle`, `group`
/// and `resource`, as a menu command's argument.
fn scm_args(at: &[u64]) -> Vec<Value> {
    vec![json!({ "$scm": at })]
}

const COMMIT_PLACEHOLDER: &str = "Message (Ctrl+Enter to commit)";

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
    pub(super) window: Option<AnyWindowHandle>,
    /// Notifications with an extension's buttons: the host's question and the
    /// buttons' titles.
    pub(super) buttons: HashMap<SharedString, (u64, Vec<String>)>,
    /// The WASM host, started with the first WASM plugin.
    wasm: Option<wasmhost::WasmHost>,
    wasm_generation: usize,
    /// Extensions' source controls, as exthost.js's `scm` messages last sent
    /// them.
    scm: Vec<Value>,
    /// Extensions' tree views by view id, as exthost.js's `tree` messages
    /// last sent them.
    trees: HashMap<String, Value>,
    /// The extension views collapsed, and those shown so far, whose
    /// extensions `onView` activated.
    closed_views: HashSet<String>,
    seen_views: HashSet<String>,
    /// Webview views by view id, made as they first show.
    pub(super) webview_views: HashMap<String, super::custom_editors::ViewWebview>,
}

impl Host {
    pub(super) fn send(&self, msg: Value) {
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
        match (id >= wasmhost::ASKS, &self.wasm) {
            (true, Some(wasm)) => wasm.answer(id, value),
            (true, None) => {}
            (false, _) => self.send(json!({ "m": "answer", "id": id, "value": value })),
        }
    }

    pub fn send_settings(&self) {
        self.send(json!({ "m": "settings", "settings": terminal::settings() }));
    }
}

/// The Node to run: the one bundled in `runtime` next to vsn, then Node on
/// PATH (a build without the installer).
fn runtimes() -> Vec<std::ffi::OsString> {
    let bundled = std::env::current_exe().ok().and_then(|e| {
        let node = e.parent()?.join("runtime").join("node");
        Some(node.with_extension(std::env::consts::EXE_EXTENSION).into())
    });
    bundled.into_iter().chain(["node".into()]).collect()
}

/// An extension's configuration defaults, by key.
fn config_defaults(e: &Extension) -> serde_json::Map<String, Value> {
    let mut defaults = serde_json::Map::new();
    for c in e.contributes("configuration") {
        for (key, prop) in c["properties"].as_object().into_iter().flatten() {
            if !prop["default"].is_null() {
                defaults.insert(key.clone(), prop["default"].clone());
            }
        }
    }
    defaults
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
        || event.strip_prefix("onView:").is_some_and(|view| {
            (ext.manifest["contributes"]["views"].as_object().into_iter())
                .flat_map(|containers| containers.values())
                .flat_map(|views| views.as_array().into_iter().flatten())
                .any(|v| v["id"].as_str() == Some(view))
        })
        || event
            .strip_prefix("onCustomEditor:")
            .is_some_and(|view_type| {
                ext.contributes("customEditors")
                    .any(|c| c["viewType"].as_str() == Some(view_type))
            })
}

/// An extension's view container in the activity bar.
pub(super) struct ViewContainer {
    pub id: String,
    pub title: String,
    /// A codicon, or an image in the extension.
    pub icon: Result<char, PathBuf>,
}

/// Extensions' activity bar view containers that have views to show.
// ponytail: `panel` containers aren't shown.
pub(super) fn view_containers() -> Vec<ViewContainer> {
    let list: Vec<_> = (installed().iter())
        .filter(|e| e.enabled)
        .flat_map(|e| {
            (e.manifest["contributes"]["viewsContainers"]["activitybar"].as_array())
                .into_iter()
                .flatten()
                .map(move |c| (e, c))
        })
        .filter_map(|(e, c)| {
            let icon = c["icon"].as_str().unwrap_or_default();
            let icon =
                match icon.strip_prefix("$(").and_then(|i| i.strip_suffix(')')) {
                    Some(name) => Ok((codicons().find(|(n, _)| *n == name))
                        .map_or(icon::EXTENSIONS, |(_, ch)| ch)),
                    None => Err(e.dir.join(icon)),
                };
            let id = c["id"].as_str()?.to_string();
            Some(ViewContainer {
                id,
                title: e.localize(&c["title"]),
                icon,
            })
        })
        .collect();
    (list.into_iter())
        .filter(|c| !ext_views(&c.id).is_empty())
        .collect()
}

/// A view an extension contributes.
pub(super) struct ExtView {
    pub id: String,
    pub name: String,
    /// Its `viewsWelcome` content, shown while it's empty: lines of text or
    /// `[Label](command:id)` buttons.
    pub welcome: Vec<String>,
    /// A `"type": "webview"` view, which a webview view provider fills.
    pub webview: bool,
    /// Its extension's folder, which its page may load from.
    pub dir: PathBuf,
}

/// The views extensions contribute to view container `container`, built-in
/// (`explorer`, `scm`, `debug`) or theirs, whose `when` holds.
pub(super) fn ext_views(container: &str) -> Vec<ExtView> {
    let exts = installed();
    let enabled = || exts.iter().filter(|e| e.enabled);
    let holds = |w: &Value| w.as_str().is_none_or(|w| when_holds(w, &[]));
    enabled()
        .flat_map(|e| {
            (e.manifest["contributes"]["views"][container].as_array())
                .into_iter()
                .flatten()
                .map(move |v| (e, v))
        })
        .filter(|(_, v)| holds(&v["when"]))
        .filter_map(|(e, v)| {
            let id = v["id"].as_str()?.to_string();
            let welcome = enabled()
                .flat_map(|e| e.contributes("viewsWelcome").map(move |w| (e, w)))
                .filter(|(_, w)| w["view"] == id.as_str() && holds(&w["when"]))
                .flat_map(|(e, w)| {
                    let text = e.localize(&w["contents"]);
                    text.lines().map(str::to_string).collect::<Vec<_>>()
                })
                .collect();
            Some(ExtView {
                name: e.localize(&v["name"]),
                id,
                welcome,
                webview: v["type"] == "webview",
                dir: e.dir.clone(),
            })
        })
        .collect()
}

/// A tree item's icon: a codicon, an image, or the file icon theme's for
/// its file or folder.
fn tree_icon(icon: &Value, open: bool) -> Option<AnyElement> {
    if let Some(name) = icon["codicon"].as_str() {
        let (_, ch) = codicons().find(|(n, _)| *n == name)?;
        return Some(codicon(ch).into_any_element());
    }
    if let Some(path) = icon["path"].as_str() {
        return Some(img(PathBuf::from(path)).size(px(16.)).into_any_element());
    }
    let path = Path::new(icon["file"].as_str()?);
    match icon["folder"] == true {
        true => crate::theme::folder_icon(path, open),
        false => Some(file_icon(path)),
    }
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
        .filter(|m| m["when"].as_str().is_some_and(|w| !when_holds(w, focus)))
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
    /// In the "inline" group, which `scm/resourceState/context` shows as
    /// buttons on the row.
    pub inline: bool,
}

/// Extensions' items in one of VS Code's `menus` (`editor/context`, ...)
/// whose `when` holds on `focus`, by group.
// ponytail: groups only order the items (navigation first, then by name),
// with no separators or `@order`; `alt` commands are skipped, and submenus
// are flattened into "Submenu › Item"s (one that contains itself directly
// is skipped, longer cycles aren't caught).
pub(super) fn menu_items(menu: &str, focus: &[KeyContext]) -> Vec<MenuItem> {
    let mut list = Vec::new();
    for e in installed().iter().filter(|e| e.enabled) {
        let items = e.manifest["contributes"]["menus"][menu].as_array();
        for m in items.into_iter().flatten() {
            if !m["when"].as_str().is_none_or(|w| when_holds(w, focus)) {
                continue;
            }
            let group = m["group"].as_str().unwrap_or("");
            let group = group.split('@').next().unwrap_or("").to_string();
            let (navigation, inline) = (group == "navigation", group == "inline");
            if let Some(sub) = m["submenu"].as_str().filter(|sub| *sub != menu) {
                let label = (e.contributes("submenus").find(|s| s["id"] == sub))
                    .map_or(sub.to_string(), |s| e.localize(&s["label"]));
                for item in menu_items(sub, focus) {
                    let title = format!("{label} › {}", item.title);
                    list.push((
                        group.clone(),
                        MenuItem {
                            title,
                            navigation,
                            inline,
                            ..item
                        },
                    ));
                }
                continue;
            }
            let Some(id) = m["command"].as_str() else {
                continue;
            };
            let Some(c) = e.contributes("commands").find(|c| c["command"] == id) else {
                continue;
            };
            let icon = (c["icon"].as_str())
                .and_then(|i| i.strip_prefix("$(")?.strip_suffix(')'))
                .and_then(|name| codicons().find(|(n, _)| *n == name))
                .map(|(_, ch)| ch);
            let title = e.localize(&c["title"]);
            let id = id.to_string();
            list.push((
                group,
                MenuItem {
                    title,
                    id,
                    icon,
                    navigation,
                    inline,
                },
            ));
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

/// VS Code's `resource*` keys.
pub(super) fn resource_keys(keys: &mut KeyContext, path: &Path) {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()));
    keys.set("resourceScheme", "file");
    keys.set("resourceFilename", name.to_string());
    keys.set("resourceExtname", ext.unwrap_or_default());
    keys.set("resourcePath", path.to_string_lossy().to_string());
}

/// Context keys extensions set with the `setContext` command.
pub(super) static SET_CONTEXT: Mutex<BTreeMap<String, Value>> = Mutex::new(BTreeMap::new());

/// A context key's value: the deepest on the focus path that has it, else
/// the platform's or an extension's, else null.
fn context_value(key: &str, focus: &[KeyContext]) -> Value {
    if let Some(cx) = focus.iter().rev().find(|cx| cx.contains(key)) {
        return cx
            .get(key)
            .map_or(Value::Bool(true), |v| v.to_string().into());
    }
    let os = match std::env::consts::OS {
        "macos" => "isMac",
        "windows" => "isWindows",
        _ => "isLinux",
    };
    match key {
        "isMac" | "isWindows" | "isLinux" | "isWeb" => (key == os).into(),
        "true" => true.into(),
        "false" => false.into(),
        _ => (SET_CONTEXT.lock().unwrap().get(key).cloned()).unwrap_or_default(),
    }
}

/// Whether a VS Code `when` clause holds on the focus path, with all of its
/// grammar: `!`, `&&`, `||`, parens, `==`, `!=`, `<`, `<=`, `>`, `>=`, `=~`
/// regexes, `in` and `not in`.
pub(super) fn when_holds(when: &str, focus: &[KeyContext]) -> bool {
    When { rest: when, focus }.or()
}

struct When<'a> {
    rest: &'a str,
    focus: &'a [KeyContext],
}

impl When<'_> {
    fn eat(&mut self, tok: &str) -> bool {
        self.rest = self.rest.trim_start();
        let ate = self.rest.starts_with(tok);
        if ate {
            self.rest = &self.rest[tok.len()..];
        }
        ate
    }

    fn or(&mut self) -> bool {
        let mut holds = self.and();
        while self.eat("||") {
            holds |= self.and();
        }
        holds
    }

    fn and(&mut self) -> bool {
        let mut holds = self.unary();
        while self.eat("&&") {
            holds &= self.unary();
        }
        holds
    }

    /// A key or quoted string, up to an operator.
    fn word(&mut self) -> String {
        self.rest = self.rest.trim_start();
        let quote = self.rest.chars().next().filter(|c| *c == '\'' || *c == '"');
        let (word, rest) = match quote {
            Some(q) => {
                let end = self.rest[1..].find(q).map_or(self.rest.len(), |i| i + 2);
                (self.rest[1..end].trim_end_matches(q), &self.rest[end..])
            }
            None => {
                let end = (self.rest)
                    .find(|c: char| c.is_whitespace() || "!=<>()&|".contains(c))
                    .unwrap_or(self.rest.len());
                self.rest.split_at(end)
            }
        };
        self.rest = rest;
        word.to_string()
    }

    fn unary(&mut self) -> bool {
        if self.eat("!") {
            return !self.unary();
        }
        if self.eat("(") {
            let holds = self.or();
            self.eat(")");
            return holds;
        }
        let value = context_value(&self.word(), self.focus);
        let text = match &value {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            v => v.to_string(),
        };
        let number = |s: &str| s.trim().parse::<f64>().ok();
        let cmp = |this: &mut Self, ord: fn(f64, f64) -> bool| {
            let rhs = this.word();
            number(&text)
                .zip(number(&rhs))
                .is_some_and(|(a, b)| ord(a, b))
        };
        if self.eat("=~") {
            self.rest = self.rest.trim_start();
            let Some(body) = self.rest.strip_prefix('/') else {
                return false;
            };
            let mut escaped = false;
            let end = body.char_indices().find(|&(_, c)| {
                let end = c == '/' && !escaped;
                escaped = c == '\\' && !escaped;
                end
            });
            let end = end.map_or(body.len(), |(i, _)| i);
            let flags = body[end..].trim_start_matches('/');
            let flags_end = flags
                .find(|c: char| !c.is_alphabetic())
                .unwrap_or(flags.len());
            let ci = flags[..flags_end].contains('i');
            self.rest = &flags[flags_end..];
            return (regex::RegexBuilder::new(&body[..end])
                .case_insensitive(ci)
                .build())
            .is_ok_and(|re| re.is_match(&text));
        }
        if self.eat("==") {
            self.eat("=");
            return text == self.word();
        }
        if self.eat("!=") {
            self.eat("=");
            return text != self.word();
        }
        if self.eat("<=") {
            return cmp(self, |a, b| a <= b);
        }
        if self.eat(">=") {
            return cmp(self, |a, b| a >= b);
        }
        if self.eat("<") {
            return cmp(self, |a, b| a < b);
        }
        if self.eat(">") {
            return cmp(self, |a, b| a > b);
        }
        let not = self.eat("not ");
        if self.eat("in ") {
            let has = match context_value(&self.word(), self.focus) {
                Value::Array(a) => a.iter().any(|v| v.as_str() == Some(&text)),
                Value::Object(o) => o.contains_key(&text),
                _ => false,
            };
            return has != not;
        }
        !matches!(value, Value::Null | Value::Bool(false))
            && text != "0"
            && !(value.is_string() && text.is_empty())
    }
}

/// The editor's right-click menu with extensions' `editor/context` items:
/// the input's own items, which a custom menu replaces, then theirs.
// ponytail: the input's items are always enabled here, as its capabilities
// aren't passed to a custom menu; vsn only uses one with extension items.
pub(super) fn editor_context_menu(menu: NativeMenu, path: &Path, window: &Window) -> NativeMenu {
    let menu = menu
        .menu("Go to Definition", Box::new(input::GoToDefinition))
        .menu("Go to Type Definition", Box::new(GoToTypeDefinition))
        .menu("Go to Implementations", Box::new(GoToImplementation))
        .menu("Go to References", Box::new(FindAllReferences))
        .menu("Show Call Hierarchy", Box::new(ShowCallHierarchy))
        .separator()
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
        let action = RunExtensionCommand {
            id,
            path,
            when: None,
            args: None,
        };
        menu.menu(item.title, Box::new(action))
    })
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
            list.extend(command_binding(key, id, k, mapper));
        }
    }
    list
}

/// A VS Code keybinding entry `k` binding `key` to command `id`.
fn command_binding(
    key: &str,
    id: &str,
    k: &Value,
    mapper: &dyn PlatformKeyboardMapper,
) -> Option<KeyBinding> {
    let keys = (key.to_lowercase().split_whitespace())
        .map(|chord| chord.replace('+', "-"))
        .collect::<Vec<_>>()
        .join(" ");
    // Matching on the focused element, as VS Code's keybindings
    // rank above its defaults; `when` is checked as the key comes.
    let context = KeyBindingContextPredicate::parse("!vsn_anywhere").ok();
    let action = Box::new(RunExtensionCommand {
        id: id.into(),
        path: None,
        when: k["when"].as_str().map(str::to_string),
        args: (!k["args"].is_null()).then(|| k["args"].clone()),
    });
    let context = context.map(Into::into);
    KeyBinding::load(&keys, action, context, false, None, mapper).ok()
}

/// The user's keybindings.json, beside settings.json.
pub(super) fn keybindings_path() -> Option<PathBuf> {
    Some(super::terminal::settings_path()?.with_file_name("keybindings.json"))
}

/// The user's keybindings.json, as VS Code's: `key`, `command`, `when` and
/// `args`, ranked above the built-in and extensions' bindings.
// ponytail: a "-command" entry (removing a default binding) is skipped.
pub(super) fn user_key_bindings(
    text: &str,
    mapper: &dyn PlatformKeyboardMapper,
) -> Vec<KeyBinding> {
    let text = debug::strip_jsonc(text.trim_start_matches('\u{feff}'));
    let list = serde_json::from_str::<Value>(&text).ok();
    (list.as_ref().and_then(Value::as_array).into_iter().flatten())
        .filter_map(|k| {
            let id = k["command"].as_str().filter(|id| !id.starts_with('-'))?;
            command_binding(k["key"].as_str()?, id, k, mapper)
        })
        .collect()
}

macro_rules! builtins {
    ($($id:literal => $action:expr,)*) => { &[$(($id, || Box::new($action))),*] };
}

/// VS Code's built-in commands that run vsn's actions.
// ponytail: commands without a vsn action log "not found"; the
// `vscode.execute*Provider` ones that return results aren't here.
type Builtin = (&'static str, fn() -> Box<dyn Action>);
const BUILTINS: &[Builtin] = builtins! {
    "workbench.action.files.save" => Save,
    "workbench.action.files.saveAll" => SaveAll,
    "workbench.action.files.saveAs" => SaveAs,
    "workbench.action.files.revert" => RevertFile,
    "workbench.action.reopenWithEditor" => ReopenEditorWith,
    "workbench.action.files.openFile" => OpenFile,
    "workbench.action.files.openFolder" => OpenFolder,
    "workbench.action.files.newUntitledFile" => NewTextFile,
    "workbench.action.closeFolder" => CloseFolder,
    "workbench.action.toggleAutoSave" => ToggleAutoSave,
    "workbench.action.closeActiveEditor" => CloseEditor,
    "workbench.action.closeAllEditors" => CloseAllEditors,
    "workbench.action.closeUnmodifiedEditors" => CloseSavedEditors,
    "workbench.action.closeOtherEditors" => CloseOtherEditors,
    "workbench.action.closeEditorsToTheRight" => CloseEditorsToTheRight,
    "workbench.action.nextEditor" => NextEditor,
    "workbench.action.previousEditor" => PreviousEditor,
    "workbench.action.clearRecentFiles" => ClearRecent,
    "copyFilePath" => CopyPath,
    "copyRelativeFilePath" => CopyRelativePath,
    "revealFileInOS" => RevealInFileExplorer,
    "revealInExplorer" => RevealInExplorerView,
    "filesExplorer.cut" => CutFile,
    "filesExplorer.copy" => CopyFile,
    "filesExplorer.paste" => PasteFile,
    "filesExplorer.findInFolder" => FindInFolder,
    "workbench.files.action.refreshFilesExplorer" => RefreshExplorer,
    "workbench.files.action.collapseExplorerFolders" => CollapseExplorerFolders,
    "workbench.action.quit" => Quit,
    "workbench.action.closeWindow" => CloseWindow,
    "workbench.action.showAboutDialog" => About,
    "workbench.action.toggleSidebarVisibility" => ToggleSidebar,
    "workbench.action.togglePanel" => TogglePanel,
    "workbench.action.toggleMaximizedPanel" => ToggleMaximizedPanel,
    "workbench.actions.view.problems" => ToggleProblems,
    "workbench.action.output.toggleOutput" => ToggleOutput,
    "workbench.debug.action.toggleRepl" => ToggleDebugConsole,
    "workbench.view.explorer" => ShowExplorer,
    "workbench.view.search" => ShowSearch,
    "workbench.action.findInFiles" => ShowSearch,
    "workbench.action.replaceInFiles" => ReplaceInFiles,
    "workbench.view.scm" => ShowScm,
    "workbench.view.debug" => ShowRun,
    "workbench.view.extensions" => ShowExtensions,
    "workbench.extensions.action.installVSIX" => InstallFromVsix,
    "workbench.action.showCommands" => CommandPalette,
    "workbench.action.quickOpen" => QuickOpen,
    "workbench.action.openView" => OpenView,
    "workbench.action.gotoLine" => GoToLine,
    "workbench.action.gotoSymbol" => GoToSymbol,
    "workbench.action.showAllSymbols" => GoToSymbolInWorkspace,
    "workbench.action.navigateBack" => GoBack,
    "workbench.action.navigateForward" => GoForward,
    "workbench.action.navigateToLastEditLocation" => GoToLastEditLocation,
    "workbench.action.openSettings" => OpenSettings,
    "workbench.action.openGlobalSettings" => OpenSettings,
    "workbench.action.openWorkspaceSettings" => OpenSettings,
    "vscode.openFolder" => OpenFolder,
    "workbench.action.openSettingsJson" => OpenSettingsJson,
    "workbench.action.openGlobalKeybindingsFile" => OpenKeybindingsJson,
    "workbench.action.selectTheme" => SelectColorTheme,
    "workbench.action.selectIconTheme" => SelectIconTheme,
    "notifications.showList" => ShowNotifications,
    "notifications.hideList" => HideNotifications,
    "notifications.clearAll" => ClearAllNotifications,
    "notifications.toggleDoNotDisturbMode" => ToggleDoNotDisturb,
    "workbench.action.debug.start" => StartDebugging,
    "workbench.action.debug.run" => RunWithoutDebugging,
    "workbench.action.debug.stop" => StopDebugging,
    "workbench.action.debug.restart" => RestartDebugging,
    "workbench.action.debug.pause" => Pause,
    "workbench.action.debug.stepOver" => StepOver,
    "workbench.action.debug.stepInto" => StepInto,
    "workbench.action.debug.stepOut" => StepOut,
    "editor.debug.action.toggleBreakpoint" => ToggleBreakpoint,
    "debug.addConfiguration" => OpenLaunchJson,
    "workbench.action.terminal.toggleTerminal" => ToggleTerminal,
    "workbench.action.terminal.new" => NewTerminal,
    "workbench.action.terminal.split" => SplitTerminal,
    "workbench.action.terminal.rename" => RenameTerminal,
    "workbench.action.terminal.changeColor" => ChangeTerminalColor,
    "workbench.action.terminal.changeIcon" => ChangeTerminalIcon,
    "workbench.action.terminal.kill" => KillTerminal,
    "workbench.action.terminal.unsplit" => UnsplitTerminal,
    "workbench.action.terminal.join" => JoinTerminals,
    "workbench.action.terminal.focusNext" => FocusNextTerminalGroup,
    "workbench.action.terminal.focusPrevious" => FocusPreviousTerminalGroup,
    "workbench.action.terminal.focusNextPane" => FocusNextTerminal,
    "workbench.action.terminal.focusPreviousPane" => FocusPreviousTerminal,
    "workbench.action.terminal.focusFind" => FindInTerminal,
    "workbench.action.terminal.findNext" => FindNextInTerminal,
    "workbench.action.terminal.findPrevious" => FindPreviousInTerminal,
    "workbench.action.terminal.scrollUp" => ScrollTerminalUp,
    "workbench.action.terminal.scrollDown" => ScrollTerminalDown,
    "workbench.action.terminal.scrollUpPage" => ScrollTerminalUpPage,
    "workbench.action.terminal.scrollDownPage" => ScrollTerminalDownPage,
    "workbench.action.terminal.scrollToTop" => ScrollTerminalToTop,
    "workbench.action.terminal.scrollToBottom" => ScrollTerminalToBottom,
    "workbench.action.terminal.selectDefaultShell" => SelectDefaultProfile,
    "workbench.action.terminal.sizeToContentWidth" => ToggleSizeToContentWidth,
    "undo" => input::Undo,
    "redo" => input::Redo,
    "editor.action.clipboardCopyAction" => input::Copy,
    "editor.action.clipboardCutAction" => input::Cut,
    "editor.action.clipboardPasteAction" => input::Paste,
    "editor.action.selectAll" => input::SelectAll,
    "editor.action.indentLines" => input::Indent,
    "editor.action.outdentLines" => input::Outdent,
    "editor.action.insertCursorAbove" => gpui_kit::base::input::AddCursorAbove,
    "editor.action.insertCursorBelow" => gpui_kit::base::input::AddCursorBelow,
    "cursorUp" => input::MoveUp,
    "cursorDown" => input::MoveDown,
    "cursorLeft" => input::MoveLeft,
    "cursorRight" => input::MoveRight,
    "cursorHome" => input::MoveHome,
    "cursorEnd" => input::MoveEnd,
    "cursorTop" => input::MoveToStart,
    "cursorBottom" => input::MoveToEnd,
    "cursorPageUp" => input::MovePageUp,
    "cursorPageDown" => input::MovePageDown,
    "cursorTopSelect" => input::SelectToStart,
    "cursorBottomSelect" => input::SelectToEnd,
    "cursorHomeSelect" => input::SelectToStartOfLine,
    "cursorEndSelect" => input::SelectToEndOfLine,
    "cursorWordLeftSelect" => input::SelectToPreviousWordStart,
    "cursorWordEndRightSelect" => input::SelectToNextWordEnd,
    "deleteLeft" => input::Backspace,
    "deleteRight" => input::Delete,
    "deleteWordLeft" => input::DeleteToPreviousWordStart,
    "deleteWordRight" => input::DeleteToNextWordEnd,
    "deleteAllLeft" => input::DeleteToBeginningOfLine,
    "deleteAllRight" => input::DeleteToEndOfLine,
    "editor.action.toggleWordWrap" => ToggleWordWrap,
    "editor.action.copyLinesUpAction" => CopyLinesUp,
    "editor.action.copyLinesDownAction" => CopyLinesDown,
    "editor.action.moveLinesUpAction" => MoveLinesUp,
    "editor.action.moveLinesDownAction" => MoveLinesDown,
    "editor.action.addSelectionToNextFindMatch" => AddNextOccurrence,
    "editor.action.addSelectionToPreviousFindMatch" => AddPreviousOccurrence,
    "editor.action.moveSelectionToNextFindMatch" => MoveSelectionToNextFindMatch,
    "editor.action.selectHighlights" => SelectAllOccurrences,
    "editor.action.smartSelect.expand" => ExpandSelection,
    "editor.action.smartSelect.shrink" => ShrinkSelection,
    "editor.action.commentLine" => ToggleLineComment,
    "editor.action.addCommentLine" => AddLineComment,
    "editor.action.removeCommentLine" => RemoveLineComment,
    "editor.action.blockComment" => ToggleBlockComment,
    "editor.action.formatDocument" => FormatDocument,
    "editor.action.formatSelection" => FormatSelection,
    "editor.action.rename" => RenameSymbol,
    "editor.action.goToReferences" => FindAllReferences,
    "editor.action.referenceSearch.trigger" => FindAllReferences,
    "editor.action.goToTypeDefinition" => GoToTypeDefinition,
    "editor.action.goToImplementation" => GoToImplementation,
    "editor.action.revealDeclaration" => GoToDeclaration,
    "editor.showCallHierarchy" => ShowCallHierarchy,
    "references-view.showCallHierarchy" => ShowCallHierarchy,
    "editor.action.triggerParameterHints" => TriggerParameterHints,
    "editor.action.trimTrailingWhitespace" => TrimTrailingWhitespace,
    "editor.action.indentUsingSpaces" => IndentUsingSpaces,
    "editor.action.indentUsingTabs" => IndentUsingTabs,
    "editor.action.changeTabDisplaySize" => ChangeTabDisplaySize,
    "editor.action.detectIndentation" => DetectIndentation,
    "editor.action.indentationToSpaces" => IndentationToSpaces,
    "editor.action.indentationToTabs" => IndentationToTabs,
    "editor.action.insertSnippet" => InsertSnippet,
    "workbench.action.editor.changeEOL" => ChangeEol,
    "workbench.action.editor.changeLanguageMode" => ChangeLanguageMode,
    "workbench.action.editor.changeEncoding" => ChangeEncoding,
};

fn builtin_action(id: &str) -> Option<Box<dyn Action>> {
    BUILTINS.iter().find(|b| b.0 == id).map(|b| b.1())
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
                    let defaults = (installed().iter().find(|e| e.id == id))
                        .map_or_else(Default::default, config_defaults);
                    let world = self.wasm_world(cx);
                    let vsn = (installed().iter().find(|e| e.id == id))
                        .map_or(Value::Null, |e| e.manifest["vsn"].clone());
                    let languages = wasmhost::strings(&vsn["languages"]);
                    if let Some(host) = self.ext.wasm.clone() {
                        host.send(wasmhost::Req::Activate {
                            id: id.clone(),
                            wasm,
                            defaults: defaults.into(),
                            world,
                            vsn,
                        });
                        let forward = move |body: &str| {
                            host.send(wasmhost::Req::Lsp(
                                serde_json::from_str(body).unwrap_or_default(),
                            ));
                            true
                        };
                        self.lsp_languages(languages, super::lsp::WASM_HOSTED, forward, cx);
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
        self.lsp_host_gone(super::lsp::WASM_HOSTED, cx);
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
        let editor = self.active_editor(cx);
        self.ext
            .send(json!({ "m": "activeEditor", "editor": editor }));
    }

    /// The active text editor, its file, text and selection, or null.
    fn active_editor(&self, cx: &App) -> Value {
        // A large file isn't synced, as VS Code's extension host never gets one.
        let editor = (self.docs.get(self.active).filter(|d| !d.binary && !d.large)).map(|d| {
            let e = d.editor.read(cx);
            let sel = e.selected_range();
            let [start, end] = [sel.start, sel.end].map(|o| e.text().offset_to_position(o));
            json!({
                "uri": d.uri.clone().unwrap_or_else(|| super::lsp::path_uri(&d.path)),
                "path": d.path,
                "languageId": d.lang,
                "text": e.value(),
                "selection": [start, end],
            })
        });
        editor.unwrap_or_default()
    }

    /// What a WASM plugin can read of the editor during a call.
    fn wasm_world(&self, cx: &App) -> wasmhost::World {
        wasmhost::World {
            settings: terminal::settings(),
            editor: self.active_editor(cx),
        }
    }

    /// Opens what window.showTextDocument names: a file, or an extension's
    /// read-only document, then selects `selection` there.
    fn show_document(&mut self, msg: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let uri = msg["uri"].as_str().unwrap_or_default().to_string();
        match super::lsp::uri_path(&uri) {
            Some(path) => self.open_path(path, window, cx),
            None => match self.docs.iter().position(|d| d.uri.as_ref() == Some(&uri)) {
                Some(ix) => {
                    let editor = self.docs[ix].editor.clone();
                    set_virtual_text(
                        &editor,
                        msg["text"].as_str().unwrap_or_default(),
                        window,
                        cx,
                    );
                    self.activate(ix, window, cx);
                }
                None => {
                    // Named by its uri's last segment, as VS Code's tab is.
                    let name = uri.trim_end_matches('/').rsplit('/').next().unwrap_or(&uri);
                    let path = PathBuf::from(name);
                    let text = msg["text"].as_str().unwrap_or_default().to_string();
                    self.push_doc(path, text, Encoding::Utf8, false, true, window, cx);
                    let doc = self.docs.last_mut().unwrap();
                    doc.uri = Some(uri);
                    doc.editor.update(cx, |e, cx| e.set_readonly(true, cx));
                }
            },
        }
        let sel = msg["selection"].clone();
        if let (Ok(sel), Some(doc)) = (
            serde_json::from_value::<lsp_types::Range>(sel),
            self.docs.get(self.active),
        ) {
            let editor = doc.editor.clone();
            reveal(&editor, sel.start, window, cx);
            editor.update(cx, |e, cx| {
                let text = e.text().clone();
                let [a, b] = [sel.start, sel.end].map(|p| text.position_to_offset(&p));
                e.set_selected_range(a..b, cx);
            });
        }
    }

    /// Runs command `id`: one of VS Code's own as vsn's action, else an
    /// extension's.
    pub(super) fn run_any_command(
        &mut self,
        id: String,
        args: Vec<Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if id == "workbench.action.terminal.sendSequence" {
            let text = args.first().and_then(|a| a["text"].as_str()).unwrap_or("");
            return self.send_sequence(text, cx);
        }
        match builtin_action(&id) {
            Some(action) => window.dispatch_action(action, cx),
            None => self.run_extension_command(id, args, cx),
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
        // ponytail: a WASM plugin's commands are the ones it contributes; it
        // can't register others.
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
                    args: args.into(),
                    world: self.wasm_world(cx),
                }));
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
        SET_CONTEXT.lock().unwrap().clear();
        super::rebind_keys(cx);
        // Their providers were the old host's; the next show resolves anew.
        for (_, w) in self.ext.webview_views.drain() {
            webview::dispose(w.handle);
        }
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
        self.ext.scm.clear();
        self.ext.trees.clear();
        self.sync_commit_input(cx);
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
            self.error("Extensions need Node.js, and it wasn't found.", cx);
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
                    break;
                }
            }
            // Closed: the host deactivates its extensions and exits, or is
            // ended if one keeps it from that.
            drop(stdin);
            for _ in 0..60 {
                if !matches!(child.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            child.kill().ok();
            child.wait().ok();
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
        });
        self.ext.generation += 1;
        let generation = self.ext.generation;
        self.lsp_host_gone(super::lsp::HOSTED, cx);
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
            defaults.extend(config_defaults(e));
            extensions.push(json!({ "id": e.id, "dir": e.dir, "manifest": e.manifest }));
        }
        self.ext.send(json!({
            "m": "init",
            "settings": terminal::settings(),
            "defaults": defaults,
            "root": self.root,
            "themeKind": if col::editor().l < 0.5 { 2 } else { 1 },
            "extensions": extensions,
            "builtins": (BUILTINS.iter().map(|b| b.0))
                .chain(["vscode.open", "vscode.openWith", "workbench.action.reloadWindow"])
                .collect::<Vec<_>>(),
        }));
        self.send_active_editor(cx);
        true
    }

    fn on_host(&mut self, msg: Value, cx: &mut Context<Self>) {
        if self.on_webview_host(&msg, cx) {
            return;
        }
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
            "log" if msg["append"] == true => self.append_output(&text("channel"), &text("text"), cx),
            "log" => self.log(&text("channel"), &text("text"), cx),
            "outputClear" => self.clear_channel(&text("channel"), cx),
            "outputShow" => self.show_channel(&text("channel"), cx),
            "task" => {
                let (name, line) = (text("name"), text("commandLine"));
                let cwd = msg["cwd"].as_str().map(PathBuf::from);
                let env = (msg["env"].as_array().into_iter().flatten())
                    .filter_map(|kv| Some((kv[0].as_str()?.to_string(), kv[1].as_str()?.to_string())))
                    .collect();
                self.in_window(cx, move |this, window, cx| {
                    this.run_task(name, line, cwd, env, window, cx)
                });
            }
            "languageConfiguration" => {
                let lang = text("lang");
                extensions::set_config(&lang, msg["config"].clone());
                // Disposed with nothing else configured: the editor's defaults.
                let config = extensions::editing(&lang).unwrap_or_default();
                gpui_kit::base::input::set_language_config(editor_language(&lang), config, cx);
            }
            "debug" => {
                let config = msg.get("config").filter(|c| c.is_object()).cloned();
                let name = msg["name"].as_str().map(String::from);
                self.in_window(cx, move |this, window, cx| this.debug_with(config, name, window, cx));
            }
            "languages" => {
                let ids = (msg["ids"].as_array().into_iter().flatten())
                    .filter_map(|id| Some(id.as_str()?.to_string()))
                    .collect();
                if let Some(out) = self.ext.out.clone() {
                    // Frames go out as the host's NDJSON `lsp` messages.
                    let forward = move |body: &str| {
                        out.send(format!("{{\"m\":\"lsp\",\"msg\":{body}}}\n"))
                            .is_ok()
                    };
                    self.lsp_languages(ids, super::lsp::HOSTED, forward, cx);
                }
            }
            "lsp" => {
                let host = if msg["wasm"] == true {
                    super::lsp::WASM_HOSTED
                } else {
                    super::lsp::HOSTED
                };
                self.lsp_from_host(msg["msg"].clone(), host, cx)
            }
            // window.showTextDocument: a file, or an extension's document
            // with its text.
            "showDocument" => {
                let (id, msg) = (msg["id"].as_u64(), msg.clone());
                self.in_window(cx, move |this, window, cx| {
                    this.show_document(&msg, window, cx);
                    if let Some(id) = id {
                        this.ext.answer(id, true.into());
                    }
                });
            }
            // An editor's selection set, or revealRange (`reveal`: scrolled
            // to only).
            "select" => {
                let uri = text("uri");
                let reveal_only = msg["reveal"] == true;
                let Ok(range) = serde_json::from_value::<lsp_types::Range>(msg["range"].clone())
                else {
                    return;
                };
                let doc = self.docs.iter().find(|d| {
                    d.uri.as_ref() == Some(&uri)
                        || super::lsp::uri_path(&uri).is_some_and(|p| p == d.path)
                });
                let Some(editor) = doc.map(|d| d.editor.clone()) else {
                    return;
                };
                self.in_window(cx, move |_, window, cx| {
                    if reveal_only {
                        editor.update(cx, |e, cx| {
                            let from = e.scroll_offset();
                            center_line(e, range.start.line, Some(from), cx)
                        });
                        return;
                    }
                    reveal(&editor, range.start, window, cx);
                    editor.update(cx, |e, cx| {
                        let text = e.text().clone();
                        let [a, b] = [range.start, range.end].map(|p| text.position_to_offset(&p));
                        e.set_selected_range(a..b, cx);
                    });
                });
            }
            "virtualText" => {
                let uri = text("uri");
                if let Some(doc) = self.docs.iter().find(|d| d.uri.as_ref() == Some(&uri)) {
                    let editor = doc.editor.clone();
                    let new = text("text");
                    self.in_window(cx, move |this, window, cx| {
                        set_virtual_text(&editor, &new, window, cx);
                        if let Some(doc) = this.docs.iter_mut().find(|d| d.editor == editor) {
                            doc.saved = text_hash(&editor, cx);
                            doc.dirty = false;
                        }
                    });
                }
            }
            "applyEdit" => {
                let (id, edit) = (msg["id"].as_u64(), msg["edit"].clone());
                let Ok(edit) = serde_json::from_value::<lsp_types::WorkspaceEdit>(edit) else {
                    if let Some(id) = id {
                        self.ext.answer(id, false.into());
                    }
                    return;
                };
                self.in_window(cx, move |this, window, cx| {
                    this.apply_workspace_edit(edit, window, cx);
                    if let Some(id) = id {
                        this.ext.answer(id, true.into());
                    }
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
            "scm" => {
                let at = (self.ext.scm.iter()).position(|s| s["handle"] == msg["handle"]);
                match (at, msg["gone"] == true) {
                    (Some(at), true) => drop(self.ext.scm.remove(at)),
                    (Some(at), false) => self.ext.scm[at] = msg.clone(),
                    (None, false) => self.ext.scm.push(msg.clone()),
                    (None, true) => {}
                }
                self.sync_commit_input(cx);
                cx.notify();
            }
            "tree" => {
                match msg["gone"] == true {
                    true => drop(self.ext.trees.remove(&text("view"))),
                    false => drop(self.ext.trees.insert(text("view"), msg.clone())),
                }
                cx.notify();
            }
            "setContext" => {
                let mut keys = SET_CONTEXT.lock().unwrap();
                keys.insert(text("key"), msg["value"].clone());
                cx.notify();
            }
            // VS Code's own commands, run as vsn's actions in the window.
            "execute" => {
                let command = text("command");
                let path = msg["args"][0]["fsPath"].as_str().map(PathBuf::from);
                if command == "workbench.action.reloadWindow" {
                    return self.restart_extension_host(cx);
                }
                // ponytail: opens in this window, as if `forceNewWindow` were false.
                if let ("vscode.openFolder", Some(path)) = (command.as_str(), &path) {
                    self.set_root(path.clone(), cx);
                    return self.restart_extension_host(cx);
                }
                // VS Code's `<view>.focus` and `<view>.open` for each view.
                let view = command.strip_suffix(".focus").or(command.strip_suffix(".open"));
                if view.is_some_and(|v| self.show_ext_view(v, cx)) {
                    return;
                }
                let container = command.strip_prefix("workbench.view.extension.");
                if let Some(ix) =
                    container.and_then(|c| view_containers().iter().position(|v| v.id == c))
                {
                    (self.sidebar, self.view) = (true, View::Ext(ix));
                    return cx.notify();
                }
                let action = builtin_action(&command);
                // draw.io posts the shortcuts pressed in it, which vsn ran already.
                if action.is_some() && self.webview_key_echo() {
                    return;
                }
                // A lens' "N references": VS Code peeks them, vsn lists them.
                if command == "editor.action.showReferences" {
                    let found: Vec<_> = (msg["args"][2].as_array().into_iter().flatten())
                        .filter_map(|l| {
                            let path = PathBuf::from(l["uri"]["fsPath"].as_str()?);
                            Some((path, serde_json::from_value(l["range"]["start"].clone()).ok()?))
                        })
                        .collect();
                    return self.in_window(cx, move |this, window, cx| match found.len() {
                        0 => this.note("No references found".into(), Severity::Info, cx),
                        _ => this.pick_locations(found, "References".into(), window, cx),
                    });
                }
                if command == "vscode.openWith"
                    && let Some(path) = path
                {
                    let view = msg["args"][1].as_str().filter(|v| *v != "default");
                    let view = view.map(str::to_string);
                    return self.in_window(cx, move |this, window, cx| {
                        this.open_with(path, view, window, cx)
                    });
                }
                let (Some(window), true) = (
                    self.ext.window,
                    action.is_some()
                        || (matches!(&*command, "vscode.open" | "vscode.openWith")
                            && path.is_some()),
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
            self.ext.scm.clear();
            self.ext.trees.clear();
            self.sync_commit_input(cx);
            self.lsp_host_gone(super::lsp::HOSTED, cx);
            return cx.notify();
        }
        self.ext.crashes += 1;
        self.error("Extension host terminated unexpectedly. Restarting...", cx);
        self.reactivate(cx);
    }

    /// The commit message box shows the first source control's input and
    /// placeholder, VS Code's `{0}` as the key that accepts it.
    // ponytail: one box, the first provider's; VS Code has one per repository.
    fn sync_commit_input(&self, cx: &mut Context<Self>) {
        let (value, placeholder) = match self.ext.scm.first() {
            Some(s) => (
                s["input"].as_str().map(str::to_string),
                (s["placeholder"].as_str())
                    .filter(|p| !p.is_empty())
                    .map_or(String::new(), |p| p.replace("{0}", "Ctrl+Enter")),
            ),
            None => (None, COMMIT_PLACEHOLDER.to_string()),
        };
        let (Some(window), commit) = (self.ext.window, self.commit.clone()) else {
            return;
        };
        // A host message comes without the window an input needs.
        cx.defer(move |cx| {
            (window.update(cx, |_, window, cx| {
                commit.update(cx, |input, cx| {
                    input.set_placeholder(placeholder, window, cx);
                    if let Some(value) = value.filter(|v| *v != input.value().as_ref()) {
                        input.set_value(value, window, cx);
                    }
                })
            }))
            .ok();
        });
    }

    /// The commit message box changed: the first source control's input
    /// box follows it.
    pub(super) fn scm_input(&self, cx: &App) {
        if let Some(s) = self.ext.scm.first() {
            let value = self.commit.read(cx).value().to_string();
            (self.ext).send(json!({ "m": "scmInput", "handle": s["handle"], "value": value }));
        }
    }

    /// Ctrl+Enter in the commit message box: the first source control's
    /// `acceptInputCommand`.
    pub(super) fn scm_accept(&self) {
        if let Some(s) = self.ext.scm.first() {
            (self.ext).send(json!({ "m": "scmAccept", "handle": s["handle"] }));
        }
    }

    /// Extensions' `scm/title` items for the first source control, and the
    /// arguments they run with.
    pub(super) fn scm_title_items(&self) -> (Vec<MenuItem>, Vec<Value>) {
        let Some(s) = self.ext.scm.first() else {
            return (Vec::new(), Vec::new());
        };
        let mut keys = KeyContext::default();
        keys.set("view", "workbench.scm");
        keys.set(
            "scmProvider",
            s["id"].as_str().unwrap_or_default().to_string(),
        );
        let handle = s["handle"].as_u64().unwrap_or_default();
        (menu_items("scm/title", &[keys]), scm_args(&[handle]))
    }

    /// The Source Control view's "..." menu: extensions' `scm/title` items
    /// outside "navigation".
    pub(super) fn scm_more(&self, cx: &mut Context<Self>) -> AnyElement {
        let (mut items, args) = self.scm_title_items();
        items.retain(|i| !i.navigation);
        let wb = cx.weak_entity();
        Button::new("scm-more")
            .ghost()
            .compact()
            .size(px(22.))
            .text_color(col::fg())
            .child(codicon(icon::ELLIPSIS))
            .tooltip("Views and More Actions...")
            .dropdown_menu(move |menu, _, _| ext_menu_items(menu, &wb, &items, args.clone()))
            .into_any_element()
    }

    /// The Source Control view: the commit message box, then each source
    /// control's resource groups and their files, with extensions'
    /// `scm/resourceGroup/context` and `scm/resourceState/context` menus.
    // ponytail: groups don't collapse, rows have no status letter or
    // multi-select, and with several source controls each gets a plain
    // header without VS Code's per-repository buttons.
    pub(super) fn render_scm(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let commit = (v_flex().px(px(12.)).pt(px(4.)).pb(px(6.)).gap(px(6.)))
            .child(search_box(&self.commit));
        if self.ext.scm.is_empty() {
            return v_flex()
                .child(section_header("scm-repos", "Changes", true))
                .child(commit.child(primary_button("commit", "Commit", Some(icon::CHECK))))
                .into_any_element();
        }
        let many = self.ext.scm.len() > 1;
        let mut rows = Vec::new();
        for s in self.ext.scm.clone() {
            let handle = s["handle"].as_u64().unwrap_or_default();
            let mut keys = KeyContext::default();
            keys.set("view", "workbench.scm");
            keys.set(
                "scmProvider",
                s["id"].as_str().unwrap_or_default().to_string(),
            );
            if many {
                let label = s["label"].as_str().unwrap_or_default().to_string();
                let header = div().pl(px(8.)).font_weight(FontWeight::BOLD).child(label);
                rows.push(self.scm_row(rows.len(), header, Vec::new(), Vec::new(), None, cx));
            }
            let root = s["root"].as_str().map(PathBuf::from);
            for (gi, g) in s["groups"].as_array().into_iter().flatten().enumerate() {
                let resources = g["resources"].as_array().cloned().unwrap_or_default();
                if resources.is_empty() && g["hideWhenEmpty"] == true {
                    continue;
                }
                let mut keys = keys.clone();
                keys.set(
                    "scmResourceGroup",
                    g["id"].as_str().unwrap_or_default().to_string(),
                );
                let items = menu_items("scm/resourceGroup/context", std::slice::from_ref(&keys));
                let header = h_flex()
                    .flex_1()
                    .min_w_0()
                    .child(codicon(icon::CHEVRON_DOWN).w(px(20.)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_ellipsis()
                            .child(g["label"].as_str().unwrap_or_default().to_string()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .ml(px(6.))
                            .px(px(5.))
                            .min_w(px(18.))
                            .rounded(px(9.))
                            .bg(c(0x616161))
                            .text_size(px(11.))
                            .line_height(px(18.))
                            .text_center()
                            .text_color(white())
                            .group_hover("scm-row", |s| s.invisible())
                            .child(resources.len().to_string()),
                    );
                let args = scm_args(&[handle, gi as u64]);
                rows.push(self.scm_row(rows.len(), header, items, args, None, cx));
                for (ri, r) in resources.iter().enumerate() {
                    let mut keys = keys.clone();
                    keys.set(
                        "scmResourceState",
                        r["context"].as_str().unwrap_or_default().to_string(),
                    );
                    let items = menu_items("scm/resourceState/context", &[keys]);
                    let path = PathBuf::from(r["path"].as_str().unwrap_or_default());
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let dir = (root.as_ref())
                        .and_then(|root| path.parent()?.strip_prefix(root).ok())
                        .map(|d| d.display().to_string())
                        .unwrap_or_default();
                    let tip =
                        SharedString::from(r["tooltip"].as_str().unwrap_or_default().to_string());
                    let at = [handle, gi as u64, ri as u64];
                    let click = json!({ "m": "scmOpen", "ref": at });
                    let row = h_flex()
                        .id("scm-resource")
                        .flex_1()
                        .min_w_0()
                        .pl(px(20.))
                        .when(r["faded"] == true, |d| d.opacity(0.6))
                        .child(div().mr(px(6.)).child(file_icon(&path)))
                        .child(
                            div()
                                .flex_none()
                                .when(r["strike"] == true, |d| d.line_through())
                                .child(name),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .ml(px(6.))
                                .text_size(px(12.))
                                .text_color(col::fg_muted())
                                .text_ellipsis()
                                .child(dir),
                        )
                        .when(!tip.is_empty(), |d| {
                            d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                        });
                    rows.push(self.scm_row(rows.len(), row, items, scm_args(&at), Some(click), cx));
                }
            }
        }
        v_flex()
            .size_full()
            .child(commit)
            .child(
                v_flex()
                    .id("scm-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element()
    }

    /// The views extensions contribute to `container`, each under its
    /// header: its tree, or its welcome content while that's empty. In an
    /// extension's container (`fill`) the open views share the height; below
    /// a built-in view each gets at most 240px.
    // ponytail: views don't resize, reorder or move, and every row renders
    // without virtualization.
    pub(super) fn render_ext_views(
        &mut self,
        container: &str,
        fill: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let views = ext_views(container);
        let mut sections = Vec::new();
        for (vi, v) in views.into_iter().enumerate() {
            let open = !self.ext.closed_views.contains(&v.id);
            if open && self.ext.seen_views.insert(v.id.clone()) {
                let (wb, event) = (cx.weak_entity(), format!("onView:{}", v.id));
                cx.defer(move |cx| {
                    (wb.update(cx, |this, cx| this.activate_extensions(&event, cx))).ok();
                });
            }
            let mut keys = KeyContext::default();
            keys.set("view", v.id.clone());
            let (nav, more): (Vec<_>, Vec<_>) = menu_items("view/title", &[keys])
                .into_iter()
                .partition(|i| i.navigation);
            let hdr = SharedString::from(format!("view-{}", v.id));
            let id = v.id.clone();
            let made = self.ext.webview_views.get(&v.id);
            let title = made.and_then(|w| w.title.clone()).unwrap_or_else(|| v.name.clone());
            let description = made.and_then(|w| w.description.clone());
            let header = described_header(hdr.clone(), &title, description.as_deref(), open)
                .child(
                    h_flex()
                        .invisible()
                        .group_hover(format!("{hdr}-hdr"), |s| s.visible())
                        .children(self.ext_buttons("view-title", nav, Vec::new(), cx)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.ext.closed_views.remove(&id) {
                        this.ext.closed_views.insert(id.clone());
                    }
                    cx.notify();
                }));
            let wb = cx.weak_entity();
            let header = match more.is_empty() {
                true => header.into_any_element(),
                false => (header
                    .context_menu(move |m, _, _| ext_menu_items(m, &wb, &more, Vec::new())))
                .into_any_element(),
            };
            if v.webview {
                let body = self.webview_view_body(&v, fill, cx);
                sections.push(
                    v_flex()
                        .when(vi > 0 || !fill, |d| d.border_t_1())
                        .border_color(col::border())
                        .when(open && fill, |d| d.flex_1().min_h_0())
                        // A lone untitled view, as Cline's, has no header.
                        .when(!(fill && title.is_empty()), |d| d.child(header))
                        .when(open, |d| d.child(body))
                        .into_any_element(),
                );
                continue;
            }
            let tree = self.ext.trees.get(&v.id).cloned().unwrap_or_default();
            let items = tree["items"].as_array().cloned().unwrap_or_default();
            let mut rows = Vec::new();
            if let Some(m) = tree["message"].as_str().filter(|m| !m.is_empty()) {
                let m = m.to_string();
                rows.push(
                    (div().px(px(20.)).py(px(4.)).text_color(col::fg_muted()))
                        .child(m)
                        .into_any_element(),
                );
            }
            if items.is_empty() {
                rows.extend(self.view_welcome(&v.welcome, cx));
            }
            for (ix, it) in items.iter().enumerate() {
                rows.push(self.tree_row(&v.id, ix, it, cx));
            }
            let body = (v_flex().id(SharedString::from(format!("view-body-{}", v.id))))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .when(!fill, |d| d.max_h(px(240.)))
                .children(rows);
            sections.push(
                v_flex()
                    .when(vi > 0 || !fill, |d| d.border_t_1())
                    .border_color(col::border())
                    .when(open && fill, |d| d.flex_1().min_h_0())
                    .child(header)
                    .when(open, |d| d.child(body))
                    .into_any_element(),
            );
        }
        sections
    }

    /// Webview view `v`'s page, its webview made and resolved by its
    /// provider the first time it shows, as VS Code's.
    fn webview_view_body(&mut self, v: &ExtView, fill: bool, cx: &mut Context<Self>) -> AnyElement {
        let made = self.ext.webview_views.get(&v.id).map(|w| (w.handle, w.focus.clone()));
        let (handle, focus) = match made {
            Some(made) => made,
            None => {
                let handle = self.new_webview_handle();
                let focus = cx.focus_handle();
                let roots = [v.dir.clone()].into_iter().chain(self.root.clone()).collect();
                self.sync_webview_theme();
                webview::create(handle, roots);
                webview::set_sidebar(handle);
                (self.ext).send(json!({
                    "m": "resolveWebviewView", "handle": handle, "viewId": v.id, "title": v.name,
                }));
                let w = super::custom_editors::ViewWebview {
                    handle,
                    focus: focus.clone(),
                    title: None,
                    description: None,
                    badge: None,
                };
                self.ext.webview_views.insert(v.id.clone(), w);
                (handle, focus)
            }
        };
        let shown = self.sidebar_webviews().contains(&handle);
        let mut keys = KeyContext::default();
        keys.set("focusedView", v.id.clone());
        keys.add("webviewViewFocus");
        (div().id(SharedString::from(format!("view-body-{}", v.id))))
            .flex_1()
            .min_h_0()
            .when(!fill, |d| d.h(px(240.)))
            .key_context(keys)
            .track_focus(&focus)
            .child(super::custom_editors::webview_canvas(handle, shown, focus))
            .into_any_element()
    }

    /// Container `id`'s badge: its webview views' counts added up, and
    /// their tooltips.
    pub(super) fn container_badge(&self, id: &str) -> (u64, Vec<String>) {
        let badges = (ext_views(id).into_iter())
            .filter_map(|v| self.ext.webview_views.get(&v.id)?.badge.clone());
        badges.fold((0, Vec::new()), |(n, mut tips), (count, tip)| {
            if !tip.is_empty() {
                tips.push(tip);
            }
            (n + count, tips)
        })
    }

    /// The webview views the side bar shows: its container's open ones.
    pub(super) fn sidebar_webviews(&self) -> Vec<u64> {
        if !self.sidebar {
            return Vec::new();
        }
        let containers;
        let container = match self.view {
            View::Explorer => "explorer",
            View::Scm => "scm",
            View::Run => "debug",
            View::Ext(ix) => {
                containers = view_containers();
                match containers.get(ix) {
                    Some(c) => c.id.as_str(),
                    None => return Vec::new(),
                }
            }
            _ => return Vec::new(),
        };
        (ext_views(container).into_iter())
            .filter(|v| v.webview && !self.ext.closed_views.contains(&v.id))
            .filter_map(|v| Some(self.ext.webview_views.get(&v.id)?.handle))
            .collect()
    }

    /// Shows the side bar container with view `id`, open, as VS Code's
    /// `<id>.focus` and a webview view's `show()`. False if no container
    /// has it.
    pub(super) fn show_ext_view(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let has = |c: &str| ext_views(c).iter().any(|v| v.id == id);
        let view = match (view_containers().iter()).position(|c| has(&c.id)) {
            Some(ix) => View::Ext(ix),
            None if has("explorer") => View::Explorer,
            None if has("scm") => View::Scm,
            None if has("debug") => View::Run,
            None => return false,
        };
        self.ext.closed_views.remove(id);
        (self.sidebar, self.view) = (true, view);
        cx.notify();
        true
    }

    /// A view's `viewsWelcome` content: `[Label](command:id)` lines as
    /// buttons, the others as text with their links' labels.
    // ponytail: links inside text don't click, and a command link's
    // arguments are dropped.
    fn view_welcome(&self, lines: &[String], cx: &mut Context<Self>) -> Option<AnyElement> {
        static LINK: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap());
        let lines = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty());
        let parts = lines.enumerate().map(|(i, line)| {
            let button = (line.strip_prefix('[').and_then(|l| l.strip_suffix(')')))
                .and_then(|l| l.split_once("](command:"));
            match button {
                Some((label, command)) => {
                    let command = command.split(['?', ' ']).next().unwrap_or_default();
                    let command = command.to_string();
                    primary_button(("view-welcome", i), label.to_string(), None)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.run_extension_command(command.clone(), Vec::new(), cx)
                        }))
                        .into_any_element()
                }
                None => (div().child(LINK.replace_all(line, "$1").to_string())).into_any_element(),
            }
        });
        let parts: Vec<_> = parts.collect();
        (!parts.is_empty()).then(|| {
            (v_flex()
                .px(px(20.))
                .pt(px(6.))
                .pb(px(10.))
                .gap(px(14.))
                .text_size(px(13.)))
            .children(parts)
            .into_any_element()
        })
    }

    /// A tree view's row, indented by depth, with its `view/item/context`
    /// menu; clicking it toggles it and runs its command.
    fn tree_row(&self, view: &str, ix: usize, it: &Value, cx: &mut Context<Self>) -> AnyElement {
        let mut keys = KeyContext::default();
        keys.set("view", view.to_string());
        let context = it["context"].as_str().unwrap_or_default();
        keys.set("viewItem", context.to_string());
        let items = menu_items("view/item/context", &[keys]);
        let state = it["state"].as_u64().unwrap_or_default();
        let depth = it["depth"].as_u64().unwrap_or_default() as f32;
        let tip = SharedString::from(it["tooltip"].as_str().unwrap_or_default().to_string());
        let text = |k: &str| it[k].as_str().unwrap_or_default().to_string();
        let row = h_flex()
            .id("tree-item")
            .flex_1()
            .min_w_0()
            .pl(px(4. + depth * 8.))
            .child(div().w(px(20.)).flex_none().when(state != 0, |d| {
                d.child(codicon(match state {
                    2 => icon::CHEVRON_DOWN,
                    _ => icon::CHEVRON_RIGHT,
                }))
            }))
            .children(
                tree_icon(&it["icon"], state == 2).map(|i| div().flex_none().mr(px(6.)).child(i)),
            )
            .child(div().flex_none().child(text("label")))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .ml(px(6.))
                    .text_size(px(12.))
                    .text_color(col::fg_muted())
                    .text_ellipsis()
                    .child(text("description")),
            )
            .when(!tip.is_empty(), |d| {
                d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            });
        let h = it["h"].clone();
        let args = vec![json!({ "$tree": [view, h] })];
        let click = json!({ "m": "treeClick", "view": view, "h": h });
        self.scm_row(ix, row, items, args, Some(click), cx)
    }

    /// A row of the Source Control view or a tree view: its `inline` menu
    /// items as buttons on hover and the others on right-click; clicking it
    /// sends `click` to the host.
    fn scm_row(
        &self,
        ix: usize,
        content: impl IntoElement,
        items: Vec<MenuItem>,
        args: Vec<Value>,
        click: Option<Value>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (inline, menu): (Vec<_>, Vec<_>) = items.into_iter().partition(|i| i.inline);
        let row = h_flex()
            .id(("scm-row", ix))
            .group("scm-row")
            .relative()
            .h(px(22.))
            .pr(px(8.))
            .whitespace_nowrap()
            .overflow_hidden()
            .cursor_pointer()
            .hover(|s| s.bg(col::hover()))
            .child(content)
            .child(
                // Over the row's end, so the count badge it hides keeps its place.
                h_flex()
                    .absolute()
                    .right(px(8.))
                    .h_full()
                    .bg(col::hover())
                    .invisible()
                    .group_hover("scm-row", |s| s.visible())
                    .children(self.ext_buttons("scm-inline", inline, args.clone(), cx)),
            )
            .when_some(click, |d, click| {
                d.on_click(cx.listener(move |this, _, _, _| this.ext.send(click.clone())))
            });
        if menu.is_empty() {
            return row.into_any_element();
        }
        let wb = cx.weak_entity();
        (row.context_menu(move |m, _, _| ext_menu_items(m, &wb, &menu, args.clone())))
            .into_any_element()
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
    use super::{KeyContext, SET_CONTEXT, editor_keys, json, when_holds};
    use super::{contains, status_label};
    use std::path::Path;

    #[test]
    fn when_clauses() {
        let mut editor = KeyContext::default();
        editor.add("editorTextFocus");
        editor.set("editorLangId", "markdown");
        let terminal = KeyContext::parse("Terminal terminalFocus").unwrap();
        let eval = |when: &str, cx: &KeyContext| when_holds(when, std::slice::from_ref(cx));
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
        // Anywhere along the focus path.
        let path = [terminal.clone(), editor.clone()];
        assert!(when_holds("terminalFocus && editorTextFocus", &path));
        assert!(!when_holds("false", &path));
        // Keybindings match on the focused element, the deepest.
        let anywhere = super::KeyBindingContextPredicate::parse("!vsn_anywhere").unwrap();
        assert_eq!(anywhere.depth_of(&path), Some(2));
    }

    #[test]
    fn when_grammar() {
        let rs = [editor_keys(Path::new("c:/x/my-file.rs"), "rust")];
        let holds = |when: &str| when_holds(when, &rs);
        assert!(holds(
            "resourceExtname == .rs && resourceFilename == 'my-file.rs'"
        ));
        assert!(holds(
            "editorTextFocus && !(editorLangId == markdown || false)"
        ));
        assert!(!holds("resourceExtname == .md || unsetKey"));
        assert!(holds(r"resourceFilename =~ /^MY-.*\.RS$/i && editorFocus"));
        assert!(!holds(r"resourceExtname =~ /\.md$/"));
        assert!(holds("editorLangId != 'markdown' && isWindows != isMac"));
        let values = json!({ "x.count": 3, "x.langs": ["rust", "go"], "x.on": false });
        SET_CONTEXT
            .lock()
            .unwrap()
            .extend(values.as_object().unwrap().clone());
        assert!(holds("x.count > 2 && x.count <= 3 && !(x.count < 1)"));
        assert!(holds(
            "editorLangId in x.langs && resourceScheme not in x.langs"
        ));
        assert!(!holds("x.on || x.count >= 4"));
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

/// An extension's read-only document given new text.
fn set_virtual_text(editor: &Entity<EditorState>, text: &str, window: &mut Window, cx: &mut App) {
    editor.update(cx, |e, cx| {
        if e.value() == text {
            return;
        }
        e.set_readonly(false, cx);
        e.replace_all(text.to_string(), window, cx);
        e.set_readonly(true, cx);
    });
}
