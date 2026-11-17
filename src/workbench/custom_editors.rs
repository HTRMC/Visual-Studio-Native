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
    /// `enableFindWidget`: Ctrl+F opens vsn's find over it.
    pub find: bool,
    pub focus: FocusHandle,
}

/// An extension's webview view in the side bar
/// (`registerWebviewViewProvider`), made the first time it shows.
pub(super) struct ViewWebview {
    pub handle: u64,
    pub focus: FocusHandle,
    /// What the extension set: its `title`, `description` and `badge`
    /// (count and tooltip).
    pub title: Option<String>,
    pub description: Option<String>,
    pub badge: Option<(u64, String)>,
}

#[derive(Default)]
pub(super) struct Webviews {
    /// Custom editors' handles; the host's panels count from 1.
    next: u64,
    /// Unsaved binary custom documents' backups (`backupCustomDocument`)
    /// by file, for hot exit; at start, the ones to reopen.
    pub backups: HashMap<PathBuf, String>,
    /// The shortcut last pressed in a webview and when, so the host's echo
    /// of it (draw.io runs `workbench.action.files.save` on its Ctrl+S)
    /// doesn't run it again.
    key_at: Option<(Instant, Keystroke)>,
    /// The webview shown this frame: the active tab's, unless the panel
    /// is maximized over it.
    pub shown: Option<u64>,
    /// The side bar's webview views shown this frame, as the host last
    /// heard.
    views: Vec<u64>,
    /// The theme the host and pages last got.
    theme: Value,
    /// The find widget's box, and the webview it's open over.
    find: Option<Entity<InputState>>,
    finding: Option<u64>,
    find_case: bool,
    /// Find's active match (from 1; 0 or -1 for none) and match count.
    find_count: (i32, i32),
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
                    let at = (Instant::now(), keystroke.clone());
                    let done = this.update(cx, |this, _| this.webviews.key_at = Some(at));
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
                    webview::Event::Find(handle, active, count) => {
                        if this.webviews.finding == Some(handle) {
                            this.webviews.find_count = (active, count);
                            cx.notify();
                        }
                    }
                });
                if done.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Whether a host `execute` of `action` is the echo of the shortcut for
    /// it just pressed in a webview, which vsn already ran.
    pub(super) fn webview_key_echo(&self, action: &dyn Action, cx: &App) -> bool {
        let Some((at, key)) = &self.webviews.key_at else {
            return false;
        };
        at.elapsed() < std::time::Duration::from_millis(300)
            && (cx.key_bindings().borrow().bindings_for_action(action))
                .any(|b| b.match_keystrokes(std::slice::from_ref(key)) == Some(false))
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

    /// A new webview's handle, for a custom editor or a webview view.
    pub(super) fn new_webview_handle(&mut self) -> u64 {
        self.webviews.next += 1;
        self.webviews.next - 1
    }

    /// Shows editor `ix`'s file in custom editor `view_type`.
    pub(super) fn attach_custom_editor(
        &mut self,
        ix: usize,
        view_type: String,
        cx: &mut Context<Self>,
    ) {
        self.activate_extensions(&format!("onCustomEditor:{view_type}"), cx);
        let handle = self.new_webview_handle();
        let doc = &mut self.docs[ix];
        let path = doc.path.clone();
        doc.webview = Some(DocWebview {
            handle,
            view_type: view_type.clone(),
            text: !doc.binary,
            panel: None,
            closing: false,
            find: false,
            focus: cx.focus_handle(),
        });
        let (lang, text, dirty) = (doc.lang, doc.editor.read(cx).value(), doc.dirty);
        self.sync_webview_theme();
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
            "backupId": self.webviews.backups.get(&path),
        }));
        cx.notify();
    }

    /// Editor `ix` closed: so does its webview.
    // ponytail: a closed webview's find stays `finding`, harmlessly: its
    // slot is gone, and the next Ctrl+F replaces it.
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
        let views = self.sidebar_webviews();
        // The host's views learn they're shown or hidden, as VS Code's
        // onDidChangeVisibility.
        for &ViewWebview { handle, .. } in self.ext.webview_views.values() {
            let (was, is) = (self.webviews.views.contains(&handle), views.contains(&handle));
            if was != is {
                let msg = json!({ "m": "webviewView", "handle": handle, "active": is, "visible": is });
                self.ext.send(msg);
            }
        }
        webview::hide_others(&keep.into_iter().chain(views.iter().copied()).collect::<Vec<_>>());
        self.webviews.views = views;
        if self.docs.iter().all(|d| d.webview.is_none()) && self.ext.webview_views.is_empty() {
            return;
        }
        self.sync_webview_theme();
    }

    /// The pages' theme, from the workbench's colors; before a webview is
    /// made, so its page loads with it.
    pub(super) fn sync_webview_theme(&mut self) {
        let dark = col::editor().l < 0.5;
        let font = "\"Segoe WPC\", \"Segoe UI\", sans-serif";
        let mut theme = json!({
            "kind": if dark { "vscode-dark" } else { "vscode-light" },
            "name": terminal::settings()["workbench.colorTheme"],
            "background": hex(col::editor()),
            "sidebarBackground": hex(col::bg()),
            "vars": {
                "--vscode-editor-background": hex(col::editor()),
                "--vscode-editor-foreground": hex(col::fg()),
                "--vscode-foreground": hex(col::fg()),
                "--vscode-focusBorder": hex(col::accent()),
                "--vscode-sideBar-background": hex(col::bg()),
                "--vscode-sideBar-foreground": hex(col::fg()),
                "--vscode-panel-background": hex(col::bg()),
                "--vscode-panel-border": hex(col::border()),
                "--vscode-descriptionForeground": hex(col::fg_muted()),
                "--vscode-disabledForeground": hex(col::fg_dim()),
                "--vscode-errorForeground": "#f85149",
                "--vscode-textLink-foreground": hex(col::accent()),
                "--vscode-textLink-activeForeground": hex(col::accent()),
                "--vscode-button-background": hex(col::accent()),
                "--vscode-button-foreground": "#ffffff",
                "--vscode-button-hoverBackground": hex(col::accent().blend(col::hover())),
                "--vscode-button-secondaryBackground": hex(col::tab_active()),
                "--vscode-button-secondaryForeground": hex(col::fg()),
                "--vscode-button-secondaryHoverBackground": hex(col::status_hover()),
                "--vscode-input-background": hex(col::input()),
                "--vscode-input-foreground": hex(col::fg()),
                "--vscode-input-border": hex(col::input_border()),
                "--vscode-input-placeholderForeground": hex(col::fg_muted()),
                "--vscode-dropdown-background": hex(col::input()),
                "--vscode-dropdown-foreground": hex(col::fg()),
                "--vscode-dropdown-border": hex(col::input_border()),
                "--vscode-checkbox-background": hex(col::input()),
                "--vscode-checkbox-border": hex(col::input_border()),
                "--vscode-list-hoverBackground": hex(col::bg().blend(col::hover())),
                "--vscode-list-activeSelectionBackground": hex(col::bg().blend(col::selected())),
                "--vscode-list-activeSelectionForeground": hex(col::fg()),
                "--vscode-toolbar-hoverBackground": hex(col::bg().blend(col::toolbar_hover())),
                "--vscode-badge-background": hex(col::accent()),
                "--vscode-badge-foreground": "#ffffff",
                "--vscode-widget-border": hex(col::border()),
                "--vscode-editorWidget-background": hex(col::bg()),
                "--vscode-editorWidget-border": hex(col::border()),
                "--vscode-textCodeBlock-background": hex(col::tab_active()),
                "--vscode-textBlockQuote-background": hex(col::tab_active()),
                "--vscode-textPreformat-foreground": hex(col::fg()),
                "--vscode-scrollbarSlider-background": "#79797966",
                "--vscode-scrollbarSlider-hoverBackground": "#646464b3",
                "--vscode-scrollbarSlider-activeBackground": "#bfbfbf66",
                "--vscode-quickInput-background": hex(col::bg()),
                "--vscode-menu-background": hex(col::bg()),
                "--vscode-menu-foreground": hex(col::fg()),
                "--vscode-icon-foreground": hex(col::fg()),
                "--vscode-sidebar-background": hex(col::bg()),
                "--vscode-editorGroup-border": hex(col::border()),
                "--vscode-widget-shadow": "#0000005c",
                "--vscode-textSeparator-foreground": hex(col::fg_dim()),
                "--vscode-textBlockQuote-border": hex(col::accent()),
                "--vscode-editorError-foreground": "#f14c4c",
                "--vscode-editorWarning-foreground": "#cca700",
                "--vscode-editorInfo-foreground": "#3794ff",
                "--vscode-problemsErrorIcon-foreground": "#f14c4c",
                "--vscode-notificationsInfoIcon-foreground": "#3794ff",
                "--vscode-testing-iconPassed": "#73c991",
                "--vscode-testing-iconFailed": "#f14c4c",
                "--vscode-charts-green": "#89d185",
                "--vscode-charts-yellow": "#cca700",
                "--vscode-charts-red": "#f14c4c",
                "--vscode-charts-blue": "#3794ff",
                "--vscode-inputValidation-errorBackground": "#5a1d1d",
                "--vscode-inputValidation-errorBorder": "#be1100",
                "--vscode-inputValidation-errorForeground": hex(col::fg()),
                "--vscode-inputValidation-warningBackground": "#352a05",
                "--vscode-inputValidation-warningBorder": "#b89500",
                "--vscode-inputValidation-warningForeground": hex(col::fg()),
                "--vscode-quickInputList-focusBackground": hex(col::bg().blend(col::selected())),
                "--vscode-quickInputList-focusForeground": hex(col::fg()),
                "--vscode-diffEditor-insertedTextBackground": "#9ccc2c33",
                "--vscode-diffEditor-removedTextBackground": "#ff000033",
                "--vscode-banner-background": hex(col::bg().blend(col::selected())),
                "--vscode-banner-foreground": hex(col::fg()),
                "--vscode-terminal-ansiBlue": "#2472c8",
                "--vscode-font-family": font,
                "--vscode-font-size": "13px",
                "--vscode-font-weight": "normal",
                "--vscode-editor-font-family": "Consolas, \"Courier New\", monospace",
                "--vscode-editor-font-size": "14px",
            },
        });
        // The theme's own colors over vsn's, as VS Code gives every one.
        if let Some(vars) = theme["vars"].as_object_mut() {
            vars.extend(super::extensions::WEBVIEW_COLORS.read().unwrap().clone());
        }
        if theme != self.webviews.theme {
            self.ext
                .send(json!({ "m": "theme", "kind": if dark { 2 } else { 1 } }));
            webview::set_theme(theme.clone());
            self.webviews.theme = theme;
        }
    }

    /// VS Code's webview find widget, over the active tab's webview when
    /// it set `enableFindWidget`: finds as you type, highlighting every
    /// match.
    pub(super) fn find_in_webview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(handle) = (self.docs.get(self.active).and_then(|d| d.webview.as_ref()))
            .filter(|w| w.find)
            .map(|w| w.handle)
        else {
            return;
        };
        let input = match &self.webviews.find {
            Some(input) => input.clone(),
            None => {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
                cx.subscribe(&input, |this, _, ev: &InputEvent, cx| match ev {
                    InputEvent::Change => this.webview_find(false, cx),
                    InputEvent::PressEnter { shift, .. } => this.webview_find(*shift, cx),
                    _ => {}
                })
                .detach();
                self.webviews.find = Some(input.clone());
                input
            }
        };
        if let Some(old) = self.webviews.finding.replace(handle).filter(|h| *h != handle) {
            webview::stop_find(old);
        }
        input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.webview_find(false, cx);
    }

    /// Finds the widget's text, or steps to the next (`back`: previous)
    /// match when it's the text found already.
    pub(super) fn webview_find(&mut self, back: bool, cx: &mut Context<Self>) {
        let (Some(handle), Some(input)) = (self.webviews.finding, &self.webviews.find) else {
            return;
        };
        let term = input.read(cx).value().to_string();
        if term.is_empty() {
            webview::stop_find(handle);
            self.webviews.find_count = (-1, 0);
            return cx.notify();
        }
        webview::find(handle, &term, self.webviews.find_case, back);
    }

    fn close_webview_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = self.webviews.finding.take() {
            webview::stop_find(handle);
            let doc = (self.docs.iter())
                .find(|d| d.webview.as_ref().is_some_and(|w| w.handle == handle));
            if let Some(w) = doc.and_then(|d| d.webview.as_ref()) {
                window.focus(&w.focus, cx);
                webview::focus(handle);
            }
        }
        self.webviews.find_count = (-1, 0);
        cx.notify();
    }

    fn render_webview_find(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let input = self.webviews.find.as_ref()?;
        let (active, count) = self.webviews.find_count;
        let empty = input.read(cx).value().is_empty();
        let label = match (active, count) {
            _ if empty => String::new(),
            (_, 0) => "No results".into(),
            // WebView2 counts matches from 1.
            (a, n) if a >= 1 => format!("{a} of {n}"),
            (_, n) => format!("? of {n}"),
        };
        type Act = fn(&mut Workbench, &mut Window, &mut Context<Workbench>);
        let button = |id: &'static str, ch: char, text: &'static str, act: Act| {
            action_button(id, ch)
                .size(px(20.))
                .tooltip(tip(text))
                .on_click(cx.listener(move |this, _, w, cx| act(this, w, cx)))
        };
        let case = self.webviews.find_case;
        let case = super::search::find_toggle("webview-find-case", icon::CASE_SENSITIVE, case)
            .on_click(cx.listener(|this, _, _, cx| {
                this.webviews.find_case ^= true;
                // A new search, not the next match of the old one.
                if let Some(h) = this.webviews.finding {
                    webview::stop_find(h);
                }
                this.webview_find(false, cx);
            }));
        let prev: Act = |this, _, cx| this.webview_find(true, cx);
        let next: Act = |this, _, cx| this.webview_find(false, cx);
        Some(
            h_flex()
                .id("webview-find")
                .key_context("WebviewFind")
                .occlude()
                .absolute()
                .top_0()
                .right(px(14.))
                .h(px(33.))
                .px(px(4.))
                .gap(px(2.))
                .bg(c(0x252526))
                .shadow_md()
                .text_size(px(13.))
                .capture_action(cx.listener(|this, _: &input::Escape, window, cx| {
                    cx.stop_propagation();
                    this.close_webview_find(window, cx)
                }))
                .capture_action(cx.listener(|this, _: &FindInWebview, window, cx| {
                    cx.stop_propagation();
                    this.find_in_webview(window, cx)
                }))
                .child(
                    div()
                        .w(px(240.))
                        .border_1()
                        .border_color(col::accent())
                        .bg(col::input())
                        .child(Input::new(input).appearance(false).text_size(px(13.)).suffix(case)),
                )
                .child(div().min_w(px(68.)).px(px(4.)).text_size(px(12.)).child(label))
                .child(button("webview-find-up", icon::ARROW_UP, "Previous Match (Shift+Enter)", prev))
                .child(button("webview-find-down", icon::ARROW_DOWN, "Next Match (Enter)", next))
                .child(button("webview-find-close", icon::CLOSE, "Close (Escape)", Self::close_webview_find))
                .into_any_element(),
        )
    }

    /// The editor area of a webview tab: a hole gpui's renderer leaves
    /// for the webview under it, which gets the mouse while it's over it.
    pub(super) fn render_webview(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let doc = &self.docs[ix];
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
        if w.find {
            keys.add("webviewFindWidgetEnabled");
        }
        let find = (self.webviews.finding == Some(handle))
            .then(|| self.render_webview_find(cx))
            .flatten();
        div()
            .size_full()
            .relative()
            .key_context(keys)
            .track_focus(&w.focus)
            .child(webview_canvas(handle, shown, focus))
            .children(find)
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
                self.sync_webview_theme();
                webview::create(handle, roots(msg).unwrap_or_default());
                let (view_type, title) = (text("viewType"), text("title"));
                let find = msg["find"] == true;
                webview::set_find(handle, find);
                webview::set_state(handle, msg["state"].clone());
                // A panel revived at start doesn't take the active tab.
                let revived = msg["revived"] == true;
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
                        find,
                        focus: cx.focus_handle(),
                    });
                    if !revived {
                        this.activate(ix, window, cx);
                    }
                });
            }
            "webviewViewInfo" => {
                let handle = msg["handle"].as_u64();
                let w = self.ext.webview_views.values_mut().find(|w| Some(w.handle) == handle);
                if let Some(w) = w {
                    let some = |k: &str| msg[k].as_str().map(str::to_string);
                    w.title = some("title");
                    w.description = some("description").filter(|d| !d.is_empty());
                    w.badge = (msg["badge"]["value"].as_u64())
                        .filter(|v| *v > 0)
                        .map(|v| (v, msg["badge"]["tooltip"].as_str().unwrap_or_default().to_string()));
                    cx.notify();
                }
            }
            "webviewViewShow" => {
                let id = text("viewId");
                self.show_ext_view(&id, cx);
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
                        w.find = msg["find"] == true;
                        webview::set_find(w.handle, w.find);
                    }
                }
            }
            "customBackup" => {
                if let Some(ix) = self.webview_doc(&msg["handle"])
                    && self.docs[ix].dirty
                {
                    let path = self.docs[ix].path.clone();
                    self.webviews.backups.insert(path, text("id"));
                    self.save_session(cx);
                }
            }
            "customDirty" => {
                let Some(ix) = self.webview_doc(&msg["handle"]) else {
                    return true;
                };
                // Saved As: the tab opens the new file, in the same editor,
                // as VS Code's.
                if let Some(to) = msg["savedAs"].as_str().map(PathBuf::from) {
                    let handle = msg["handle"].clone();
                    self.in_window(cx, move |this, _, cx| {
                        let Some(ix) = this.webview_doc(&handle) else { return };
                        this.webview_closed(&this.docs[ix]);
                        let Some(w) = this.docs[ix].webview.take() else { return };
                        let doc = &mut this.docs[ix];
                        doc.stamp = file_stamp(&to);
                        doc.path = to;
                        doc.dirty = false;
                        this.attach_custom_editor(ix, w.view_type, cx);
                    });
                    return true;
                }
                let doc = &mut self.docs[ix];
                doc.dirty = msg["dirty"] == true;
                if !doc.dirty {
                    self.webviews.backups.remove(&doc.path);
                }
                let doc = &self.docs[ix];
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

/// Where webview `handle` shows, when `shown`: its element's bounds.
pub(super) fn webview_canvas(handle: u64, shown: bool, focus: FocusHandle) -> impl IntoElement {
    // Files dropped with Shift held go to the page, as in VS Code; without,
    // they open in vsn.
    let drop = move |paths: &[PathBuf], window: &mut Window, cx: &mut App| {
        if webview::shift_held() {
            cx.stop_propagation();
            let (at, s) = (window.mouse_position(), window.scale_factor());
            let px = |v: Pixels| (f32::from(v) * s).round() as i32;
            webview::drag_files(handle, paths, (px(at.x), px(at.y)));
            let wait = cx.background_executor().timer(std::time::Duration::from_millis(150));
            cx.spawn(async move |_| {
                wait.await;
                webview::finish_drop(handle);
            })
            .detach();
        }
    };
    div()
        .size_full()
        .on_drop(move |paths: &ExternalPaths, window, cx| drop(paths.paths(), window, cx))
        .on_drop(move |file: &super::DraggedFile, window, cx| {
            drop(std::slice::from_ref(&file.0), window, cx)
        })
        .child(webview_canvas_inner(handle, shown, focus))
}

fn webview_canvas_inner(handle: u64, shown: bool, focus: FocusHandle) -> impl IntoElement {
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
    .size_full()
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
