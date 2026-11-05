//! Extensions' webviews in editor tabs: custom editors (`customEditors`
//! contributions such as draw.io's) and `createWebviewPanel`'s panels, each
//! a WebView2 page drawn in its tab's editor area (webview.rs).
//! exthost.js's `resolveCustomEditor` fills a custom editor's page; a text
//! one's document is the tab's own editor, kept in step with the host, so
//! dirty tracking and Save work as for any file.

use std::time::Instant;

use serde_json::{Value, json};

use super::extensions::installed;
use super::*;

/// The webview a tab shows instead of its editor.
pub(super) struct DocWebview {
    pub handle: u64,
    /// The custom editor's `viewType`, or the panel's.
    pub view_type: String,
    /// A `CustomTextEditorProvider`'s: the tab's editor holds the document.
    /// Otherwise the extension saves it (`saveCustomDocument`).
    pub text: bool,
    /// A `createWebviewPanel` panel's title; it has no file.
    pub panel: Option<String>,
    /// Closing once its save finishes.
    pub closing: bool,
    pub focus: FocusHandle,
}

#[derive(Default)]
pub(super) struct Webviews {
    /// Custom editors' handles; the host's panels count from 1.
    next: u64,
    /// When a shortcut pressed in a webview last ran, so the host's echo of
    /// it (draw.io posts its own `vscodeShortcut`) doesn't run it again.
    key_at: Option<Instant>,
    /// The webview shown this frame: the active tab's, unless the panel
    /// is maximized over it.
    pub shown: Option<u64>,
    /// The theme the host and pages last got.
    theme: Value,
}

/// A custom editor contributed for a file: its `viewType` and display name.
pub(super) struct CustomEditor {
    pub view_type: String,
    pub name: String,
    /// `priority: "default"`, so it opens the file unless set otherwise.
    pub default: bool,
}

/// Whether `glob` (a `filenamePattern` or `workbench.editorAssociations`
/// key) matches `path`: by name, or by path when it has a `/`.
fn glob_matches(glob: &str, path: &Path) -> bool {
    let Some(dir) = path.parent() else {
        return false;
    };
    let mut b = ignore::overrides::OverrideBuilder::new(dir);
    let glob = match glob.contains('/') {
        // ignore's globs are relative to the builder's folder.
        true => format!(
            "**/{}",
            glob.trim_start_matches("**/").trim_start_matches('/')
        ),
        false => glob.to_string(),
    };
    (b.add(&glob).ok().and_then(|b| b.build().ok()))
        .is_some_and(|o| o.matched(path, false).is_whitelist())
}

/// The custom editors that can open `path`, as extensions contribute them.
pub(super) fn custom_editors(path: &Path) -> Vec<CustomEditor> {
    (installed().iter())
        .filter(|e| e.enabled)
        .flat_map(|e| e.contributes("customEditors").map(move |c| (e, c)))
        .filter(|(_, c)| {
            (c["selector"].as_array().into_iter().flatten())
                .filter_map(|s| s["filenamePattern"].as_str())
                .any(|g| glob_matches(g, path))
        })
        .filter_map(|(e, c)| {
            Some(CustomEditor {
                view_type: c["viewType"].as_str()?.to_string(),
                name: e.localize(&c["displayName"]),
                default: c["priority"].as_str().is_none_or(|p| p == "default"),
            })
        })
        .collect()
}

/// The custom editor `path` opens in, if any: `workbench.editorAssociations`'
/// pick ("default" is the text editor), else a `priority: "default"` one.
pub(super) fn editor_for(path: &Path) -> Option<String> {
    let editors = custom_editors(path);
    let settings = terminal::settings();
    let assoc = (settings["workbench.editorAssociations"]
        .as_object()
        .into_iter()
        .flatten())
    .find(|(g, _)| glob_matches(g, path))
    .and_then(|(_, v)| v.as_str().map(str::to_string));
    match assoc {
        Some(v) if v == "default" => None,
        Some(v) if editors.iter().any(|e| e.view_type == v) => Some(v),
        _ => editors.into_iter().find(|e| e.default).map(|e| e.view_type),
    }
}

/// `#rrggbb` for a color.
fn hex(c: Hsla) -> String {
    let c = Rgba::from(c);
    let b = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b))
}

impl Workbench {
    /// Runs `f` with the window, which host messages arrive without.
    pub(super) fn in_window(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(window) = self.ext.window else {
            return;
        };
        let this = cx.entity();
        cx.defer(move |cx| {
            (window.update(cx, |_, window, cx| {
                this.update(cx, |this, cx| f(this, window, cx))
            }))
            .ok();
        });
    }

    /// Hooks webviews to the window: their pages' messages go to the host,
    /// their shortcuts to vsn's keybindings.
    pub(super) fn start_webviews(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.webviews.next = 1 << 32;
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
            if let Ok(RawWindowHandle::Win32(h)) = window.window_handle().map(|h| h.as_raw()) {
                webview::init(h.hwnd.get(), tx);
            }
        }
        #[cfg(not(windows))]
        drop(tx);
        cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = rx.next().await {
                // Outside the workbench's update: the keystroke's action updates it.
                if let webview::Event::Key(key) = &event {
                    let Ok(keystroke) = Keystroke::parse(key) else {
                        continue;
                    };
                    let done =
                        this.update(cx, |this, _| this.webviews.key_at = Some(Instant::now()));
                    if done.is_err()
                        || cx
                            .update(|window, cx| window.dispatch_keystroke(keystroke, cx))
                            .is_err()
                    {
                        return;
                    }
                    continue;
                }
                let done = this.update_in(cx, |this, _, cx| match event {
                    webview::Event::Message(handle, message) => {
                        let msg =
                            json!({ "m": "webviewMessage", "handle": handle, "message": message });
                        this.ext.send(msg);
                    }
                    webview::Event::Key(_) => {}
                    webview::Event::Redraw => cx.notify(),
                    webview::Event::Open(url) => cx.open_url(&url),
                    webview::Event::Error(err) => this.error(err, cx),
                });
                if done.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Whether a host `execute` of a vsn action is the echo of a shortcut
    /// just pressed in a webview, which vsn already ran.
    pub(super) fn webview_key_echo(&self) -> bool {
        (self.webviews.key_at).is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(300))
    }

    /// The roots a custom editor's page may load: its extension's folder,
    /// the file's and the workspace's.
    fn custom_roots(&self, view_type: &str, path: &Path) -> Vec<PathBuf> {
        let exts = installed();
        let ext = exts.iter().find(|e| {
            e.contributes("customEditors")
                .any(|c| c["viewType"].as_str() == Some(view_type))
        });
        (ext.map(|e| e.dir.clone()).into_iter())
            .chain(path.parent().map(Path::to_path_buf))
            .chain(self.root.clone())
            .collect()
    }

    /// Shows editor `ix`'s file in custom editor `view_type`.
    pub(super) fn attach_custom_editor(
        &mut self,
        ix: usize,
        view_type: String,
        cx: &mut Context<Self>,
    ) {
        self.activate_extensions(&format!("onCustomEditor:{view_type}"), cx);
        let handle = self.webviews.next;
        self.webviews.next += 1;
        let doc = &mut self.docs[ix];
        let path = doc.path.clone();
        doc.webview = Some(DocWebview {
            handle,
            view_type: view_type.clone(),
            text: !doc.binary,
            panel: None,
            closing: false,
            focus: cx.focus_handle(),
        });
        let (lang, text, dirty) = (doc.lang, doc.editor.read(cx).value(), doc.dirty);
        webview::create(handle, self.custom_roots(&view_type, &path));
        self.ext.send(json!({
            "m": "resolveCustomEditor",
            "handle": handle,
            "viewType": view_type,
            "path": path,
            "uri": super::lsp::path_uri(&path),
            "languageId": lang,
            "text": text,
            "dirty": dirty,
        }));
        cx.notify();
    }

    /// Editor `ix` closed: so does its webview.
    pub(super) fn webview_closed(&self, doc: &Doc) {
        if let Some(w) = &doc.webview {
            webview::dispose(w.handle);
            self.ext
                .send(json!({ "m": "webviewClosed", "handle": w.handle }));
        }
    }

    fn webview_doc(&self, handle: &Value) -> Option<usize> {
        let handle = handle.as_u64()?;
        (self.docs.iter()).position(|d| d.webview.as_ref().is_some_and(|w| w.handle == handle))
    }

    /// Tells the host which webviews are active and visible, after a tab
    /// switch, and gives the active one the keyboard.
    pub(super) fn webviews_activated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (ix, doc) in self.docs.iter().enumerate() {
            if let Some(w) = &doc.webview {
                let active = ix == self.active;
                let msg = json!({ "m": "webviewView", "handle": w.handle, "active": active, "visible": active });
                self.ext.send(msg);
            }
        }
        if let Some(w) = self.docs.get(self.active).and_then(|d| d.webview.as_ref()) {
            window.focus(&w.focus, cx);
            webview::focus(w.handle);
        }
    }

    /// Save of a custom editor whose extension saves the file: true if it
    /// went to the extension (`saveCustomDocument`).
    pub(super) fn custom_save(&self, ix: usize) -> bool {
        match self.docs.get(ix).and_then(|d| d.webview.as_ref()) {
            Some(w) if !w.text => {
                self.ext
                    .send(json!({ "m": "customSave", "handle": w.handle }));
                true
            }
            _ => false,
        }
    }

    pub(super) fn custom_save_as(&self, ix: usize, cx: &mut Context<Self>) -> bool {
        let Some(w) = self
            .docs
            .get(ix)
            .and_then(|d| d.webview.as_ref())
            .filter(|w| !w.text)
        else {
            return false;
        };
        let (handle, path) = (w.handle, &self.docs[ix].path);
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let picked = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = picked.await {
                let msg = json!({ "m": "customSaveAs", "handle": handle, "path": path });
                this.update(cx, |this, _| this.ext.send(msg)).ok();
            }
        })
        .detach();
        true
    }

    pub(super) fn custom_revert(&self, ix: usize) -> bool {
        match self.docs.get(ix).and_then(|d| d.webview.as_ref()) {
            Some(w) if !w.text => {
                self.ext
                    .send(json!({ "m": "customRevert", "handle": w.handle }));
                true
            }
            _ => false,
        }
    }

    /// Closing a dirty custom editor after Save: it closes once the
    /// extension has saved it.
    pub(super) fn custom_save_and_close(&mut self, ix: usize) -> bool {
        if !self.custom_save(ix) {
            return false;
        }
        if let Some(w) = self.docs[ix].webview.as_mut() {
            w.closing = true;
        }
        true
    }

    /// A text custom editor's document changed here: the host's copy follows.
    pub(super) fn custom_text_changed(&self, ix: usize, cx: &App) {
        let doc = &self.docs[ix];
        if doc.webview.as_ref().is_some_and(|w| w.text) {
            self.ext.send(json!({
                "m": "docText",
                "uri": super::lsp::path_uri(&doc.path),
                "text": doc.editor.read(cx).value(),
                "dirty": doc.dirty,
            }));
        }
    }

    /// The webview to show this frame, and the pages' theme, from the
    /// workbench's colors.
    pub(super) fn sync_webviews(&mut self) {
        let keep = (self.docs.get(self.active).and_then(|d| d.webview.as_ref()))
            .filter(|_| !(self.panel && self.panel_max))
            .map(|w| w.handle);
        self.webviews.shown = keep;
        webview::hide_others(keep);
        if self.docs.iter().all(|d| d.webview.is_none()) {
            return;
        }
        let dark = col::editor().l < 0.5;
        let font = "\"Segoe WPC\", \"Segoe UI\", sans-serif";
        let theme = json!({
            "kind": if dark { "vscode-dark" } else { "vscode-light" },
            "name": terminal::settings()["workbench.colorTheme"],
            "background": hex(col::editor()),
            "vars": {
                "--vscode-editor-background": hex(col::editor()),
                "--vscode-editor-foreground": hex(col::fg()),
                "--vscode-foreground": hex(col::fg()),
                "--vscode-focusBorder": hex(col::accent()),
                "--vscode-sideBar-background": hex(col::bg()),
                "--vscode-panel-border": hex(col::border()),
                "--vscode-font-family": font,
                "--vscode-font-size": "13px",
                "--vscode-font-weight": "normal",
                "--vscode-editor-font-family": "Consolas, \"Courier New\", monospace",
                "--vscode-editor-font-size": "14px",
            },
        });
        if theme != self.webviews.theme {
            self.ext
                .send(json!({ "m": "theme", "kind": if dark { 2 } else { 1 } }));
            webview::set_theme(theme.clone());
            self.webviews.theme = theme;
        }
    }

    /// The editor area of a webview tab: a hole gpui's renderer leaves
    /// for the webview under it, which gets the mouse while it's over it.
    pub(super) fn render_webview(&self, doc: &Doc) -> AnyElement {
        let w = doc.webview.as_ref().unwrap();
        let (handle, shown, focus) = (
            w.handle,
            self.webviews.shown == Some(w.handle),
            w.focus.clone(),
        );
        let mut keys = KeyContext::default();
        keys.set("activeCustomEditorId", w.view_type.clone());
        keys.set("activeWebviewPanelId", w.view_type.clone());
        if w.panel.is_none() {
            exthost::resource_keys(&mut keys, &doc.path);
        }
        div()
            .size_full()
            .key_context(keys)
            .track_focus(&w.focus)
            .child(
                canvas(
                    move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                    move |bounds, hitbox, window, _| {
                        if !shown {
                            return;
                        }
                        #[cfg(windows)]
                        paint_webview(handle, focus, bounds, hitbox, window);
                        #[cfg(not(windows))]
                        let _ = (handle, focus, bounds, hitbox);
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }

    /// A tab's label: a panel's title, else its file's name.
    pub(super) fn doc_name(doc: &Doc) -> String {
        match doc.webview.as_ref().and_then(|w| w.panel.clone()) {
            Some(title) => title,
            None => (doc.path.file_name().unwrap_or_default().to_string_lossy()).into(),
        }
    }

    /// Reopen Editor With...: the active file's editors to pick from.
    pub(super) fn reopen_with(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(self.active).filter(|d| !d.untitled) else {
            return;
        };
        if doc.webview.as_ref().is_some_and(|w| w.panel.is_some()) {
            return;
        }
        let current = doc.webview.as_ref().map(|w| w.view_type.clone());
        let editors = custom_editors(&doc.path);
        let mut rows = vec![("Text Editor".to_string(), "Built-in".to_string())];
        let mut views = vec![None];
        for e in editors {
            rows.push((e.name, e.view_type.clone()));
            views.push(Some(e.view_type));
        }
        let at = views.iter().position(|v| *v == current).unwrap_or(0);
        rows[at].1 = format!("Active · {}", rows[at].1);
        let items = super::palette::Items {
            picker: super::palette::Picker::OpenWith(doc.path.clone(), views),
            rows,
            groups: Vec::new(),
            placeholder: format!(
                "Select editor for '{}'",
                doc.path.file_name().unwrap_or_default().to_string_lossy()
            )
            .into(),
            current: at,
        };
        self.open_items(items, window, cx);
    }

    /// Opens `path` in custom editor `view_type`, or the text editor for
    /// None, replacing a tab that shows it otherwise.
    pub(super) fn open_with(
        &mut self,
        path: PathBuf,
        view_type: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.docs.iter().position(|d| d.path == path) {
            let now = self.docs[ix].webview.as_ref().map(|w| w.view_type.clone());
            if now == view_type {
                return self.activate(ix, window, cx);
            }
            if self.docs[ix].dirty {
                self.note(
                    "Save the editor before reopening it with another editor.".into(),
                    Severity::Info,
                    cx,
                );
                return;
            }
            self.remove(ix, window, cx);
            self.open_path_as(path.clone(), Some(view_type), window, cx);
            // VS Code replaces the editor in its tab.
            let last = self.docs.len().wrapping_sub(1);
            if last != ix && self.docs.get(last).is_some_and(|d| d.path == path) {
                self.move_doc(last, ix, window, cx);
            }
            return;
        }
        self.open_path_as(path, Some(view_type), window, cx);
    }

    /// The host's webview messages; false for other messages.
    pub(super) fn on_webview_host(&mut self, msg: &Value, cx: &mut Context<Self>) -> bool {
        let text = |k: &str| msg[k].as_str().unwrap_or_default().to_string();
        let id = msg["id"].as_u64();
        let roots = |msg: &Value| -> Option<Vec<PathBuf>> {
            let roots = msg["roots"].as_array()?;
            Some(
                roots
                    .iter()
                    .filter_map(|r| Some(PathBuf::from(r.as_str()?)))
                    .collect(),
            )
        };
        match msg["m"].as_str().unwrap_or_default() {
            "webview" => {
                let Some(handle) = msg["handle"].as_u64() else {
                    return true;
                };
                if let Some(roots) = roots(msg) {
                    webview::set_roots(handle, roots);
                }
                if let Some(html) = msg["html"].as_str() {
                    webview::set_html(handle, html.to_string());
                }
            }
            "webviewPost" => {
                if let Some(handle) = msg["handle"].as_u64() {
                    webview::post(handle, msg["message"].to_string());
                }
            }
            "webviewPanel" => {
                let Some(handle) = msg["handle"].as_u64() else {
                    return true;
                };
                // Made now, so the page the host sends next has its place.
                webview::create(handle, roots(msg).unwrap_or_default());
                let (view_type, title) = (text("viewType"), text("title"));
                self.in_window(cx, move |this, window, cx| {
                    let path = PathBuf::from(format!("webview-panel-{handle}"));
                    this.push_doc(path, String::new(), Encoding::Utf8, true, false, window, cx);
                    let ix = this.docs.len() - 1;
                    this.docs[ix].webview = Some(DocWebview {
                        handle,
                        view_type,
                        text: false,
                        panel: Some(title),
                        closing: false,
                        focus: cx.focus_handle(),
                    });
                    this.activate(ix, window, cx);
                });
            }
            "webviewTitle" => {
                if let Some(ix) = self.webview_doc(&msg["handle"])
                    && let Some(w) = self.docs[ix].webview.as_mut().filter(|w| w.panel.is_some())
                {
                    w.panel = Some(text("title"));
                    cx.notify();
                }
            }
            "webviewReveal" => {
                let handle = msg["handle"].clone();
                self.in_window(cx, move |this, window, cx| {
                    if let Some(ix) = this.webview_doc(&handle) {
                        this.activate(ix, window, cx);
                    }
                });
            }
            "webviewDispose" => {
                let handle = msg["handle"].clone();
                self.in_window(cx, move |this, window, cx| {
                    if let Some(ix) = this.webview_doc(&handle) {
                        this.remove(ix, window, cx);
                    }
                });
            }
            "customKind" => {
                if let Some(ix) = self.webview_doc(&msg["handle"]) {
                    let doc = &mut self.docs[ix];
                    let text = msg["text"] == true && !doc.binary;
                    doc.binary = !text;
                    if let Some(w) = doc.webview.as_mut() {
                        w.text = text;
                    }
                }
            }
            "customDirty" => {
                let Some(ix) = self.webview_doc(&msg["handle"]) else {
                    return true;
                };
                let doc = &mut self.docs[ix];
                doc.dirty = msg["dirty"] == true;
                let close = !doc.dirty
                    && msg["saved"] == true
                    && doc.webview.as_ref().is_some_and(|w| w.closing);
                if close {
                    let handle = msg["handle"].clone();
                    self.in_window(cx, move |this, window, cx| {
                        if let Some(ix) = this.webview_doc(&handle) {
                            this.remove(ix, window, cx);
                        }
                    });
                }
                cx.notify();
            }
            "saveDoc" => {
                let path = PathBuf::from(text("path"));
                self.in_window(cx, move |this, window, cx| {
                    let ix = this.docs.iter().position(|d| d.path == path);
                    if let Some(ix) = ix {
                        this.write(ix, window, cx);
                    }
                    let saved = ix.is_some_and(|ix| !this.docs[ix].dirty);
                    if let Some(id) = id {
                        this.ext.answer(id, saved.into());
                    }
                });
            }
            "saveDialog" => {
                let Some(id) = id else { return true };
                let path = msg["path"].as_str().map(PathBuf::from);
                let dir = match &path {
                    Some(p) if p.is_dir() => p.clone(),
                    Some(p) => p.parent().map(Path::to_path_buf).unwrap_or_default(),
                    None => self.root.clone().unwrap_or_default(),
                };
                let name = (path.as_ref().filter(|p| !p.is_dir()))
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string());
                let picked = cx.prompt_for_new_path(&dir, name.as_deref());
                cx.spawn(async move |this, cx| {
                    let path = match picked.await {
                        Ok(Ok(Some(p))) => json!(p),
                        _ => Value::Null,
                    };
                    this.update(cx, |this, _| this.ext.answer(id, path)).ok();
                })
                .detach();
            }
            "openDialog" => {
                let Some(id) = id else { return true };
                let picked = cx.prompt_for_paths(PathPromptOptions {
                    files: msg["files"] != false,
                    directories: msg["folders"] == true,
                    multiple: msg["many"] == true,
                    prompt: None,
                });
                cx.spawn(async move |this, cx| {
                    let paths = match picked.await {
                        Ok(Ok(Some(p))) => json!(p),
                        _ => Value::Null,
                    };
                    this.update(cx, |this, _| this.ext.answer(id, paths)).ok();
                })
                .detach();
            }
            "clipboardRead" => {
                let text = cx.read_from_clipboard().and_then(|c| c.text());
                if let Some(id) = id {
                    self.ext.answer(id, text.unwrap_or_default().into());
                }
            }
            "clipboardWrite" => cx.write_to_clipboard(ClipboardItem::new_string(text("text"))),
            "openExternal" => cx.open_url(&text("url")),
            _ => return false,
        }
        true
    }
}

/// Webview `handle` at `bounds`: placed under a hole, with the mouse over
/// `hitbox` (and while a button's held, from a press in it) sent to it.
#[cfg(windows)]
fn paint_webview(
    handle: u64,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
    window: &mut Window,
) {
    use webview::{Button, Mouse};
    let s = window.scale_factor();
    let px = move |v: Pixels| (f32::from(v) * s).round() as i32;
    let at = move |p: Point<Pixels>| (px(p.x), px(p.y));
    webview::place(
        handle,
        windows::Win32::Foundation::RECT {
            left: px(bounds.left()),
            top: px(bounds.top()),
            right: px(bounds.right()),
            bottom: px(bounds.bottom()),
        },
    );
    // Until the page is in the window, the hole would show the desktop.
    if !webview::attached(handle) {
        return;
    }
    window.paint_quad(fill(bounds, gpui_windows::WEBVIEW_HOLE));
    let button = |b: MouseButton| match b {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
        MouseButton::Navigate(NavigationDirection::Back) => Button::X1,
        MouseButton::Navigate(NavigationDirection::Forward) => Button::X2,
    };
    let id = hitbox.id;
    window.on_mouse_event({
        let hitbox = hitbox.clone();
        move |e: &MouseMoveEvent, phase, window, _| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let input = if hitbox.is_hovered(window) {
                Mouse::Move
            } else {
                Mouse::Leave
            };
            webview::mouse(handle, input, at(e.position));
        }
    });
    window.on_mouse_event({
        let hitbox = hitbox.clone();
        move |e: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                return;
            }
            window.capture_pointer(id);
            window.focus(&focus, cx);
            webview::focus(handle);
            let input = match e.click_count {
                2 => Mouse::Double(button(e.button)),
                _ => Mouse::Down(button(e.button)),
            };
            webview::mouse(handle, input, at(e.position));
        }
    });
    window.on_mouse_event({
        let hitbox = hitbox.clone();
        move |e: &MouseUpEvent, phase, window, _| {
            if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                webview::mouse(handle, Mouse::Up(button(e.button)), at(e.position));
            }
        }
    });
    window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, _| {
        if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
            return;
        }
        // gpui's lines are notches times the system's lines a notch,
        // 3 by default; WebView2 wants notches of 120.
        let (x, y) = match e.delta {
            ScrollDelta::Lines(d) => (d.x * 40., d.y * 40.),
            ScrollDelta::Pixels(d) => (f32::from(d.x) * 2.5, f32::from(d.y) * 2.5),
        };
        let at = at(e.position);
        // gpui turns a shifted wheel sideways; the page does that itself.
        if e.modifiers.shift && y == 0. {
            return webview::mouse(handle, Mouse::Wheel(x.round() as i32), at);
        }
        if y != 0. {
            webview::mouse(handle, Mouse::Wheel(y.round() as i32), at);
        }
        if x != 0. {
            webview::mouse(handle, Mouse::HWheel(-x.round() as i32), at);
        }
    });
}
