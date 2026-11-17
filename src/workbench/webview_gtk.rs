//! Extension webviews on Linux, in WebKitGTK: each page renders offscreen
//! (in a `GtkOffscreenWindow`) on a GTK thread of webviews' own, and gpui
//! paints its frames where the webview sits, so menus and hovers draw over
//! it, as WebView2's composition hosting does on Windows. vsn forwards the
//! page its mouse and keys as GDK events. Pages come from vsn, at
//! `vsn-webview://webview-{handle}/index.html`, and extension files from
//! `asWebviewUri`'s `vsn-resource://file/...` under their
//! `localResourceRoots` (WebKitGTK can't answer https itself).
//!
//! The UI thread talks to the GTK thread through GLib idles (in order), and
//! the GTK thread back through [`Event`]s and the frames in [`Shared`].

use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex, Once},
    time::{Duration, Instant},
};

use futures::channel::mpsc::UnboundedSender;
use gpui_kit::{CursorStyle, Keystroke, Modifiers, RenderImage, Window};
use gtk::{gdk, gio, glib, prelude::*};
use serde_json::{Value, json};
use webkit2gtk::{
    FindControllerExt, FindOptions, HardwareAccelerationPolicy, NavigationPolicyDecision,
    NavigationPolicyDecisionExt, PermissionRequestExt, PolicyDecisionExt, PolicyDecisionType,
    SecurityManagerExt, SettingsExt, URIRequestExt, URISchemeRequest, URISchemeRequestExt,
    URISchemeResponse, URISchemeResponseExt, UserContentInjectedFrames, UserContentManager,
    UserContentManagerExt, UserScript, UserScriptInjectionTime, WebContext, WebContextExt, WebView,
    WebViewExt,
};

use super::webview_page::{PRELUDE, page_html, page_owns, parse_rgb, read_resource};

/// What a webview tells the workbench.
pub enum Event {
    /// The page's `vscode.postMessage`.
    Message(u64, Value),
    /// Unused here: keys reach vsn's keybindings before the page.
    #[allow(dead_code)]
    Key(String),
    /// A link to open in the browser.
    Open(String),
    /// WebKitGTK couldn't start.
    Error(String),
    /// A page drew, so the workbench repaints to show it.
    Redraw,
    /// Find's active match (from 1; 0 for none) and match count changed.
    Find(u64, i32, i32),
}

/// A mouse button, for [`mouse`].
#[derive(Clone, Copy, PartialEq)]
pub enum Button {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

/// Mouse input for a webview.
pub enum Mouse {
    Move,
    Down(Button),
    /// The second press of a double click.
    Double(Button),
    Up(Button),
    /// Wheel turns in 120ths of a notch, up positive.
    Wheel(i32),
    /// Sideways, right positive.
    HWheel(i32),
    Leave,
}

/// What the GTK thread and the UI thread share.
#[derive(Default)]
struct Shared {
    events: Option<UnboundedSender<Event>>,
    /// `{kind, name, background, sidebarBackground, vars}`.
    theme: Value,
    /// What each page last gave `setState`.
    states: HashMap<u64, Value>,
    /// Each page's newest frame, BGRA, until the UI takes it.
    frames: HashMap<u64, (u32, u32, Vec<u8>)>,
    /// The CSS cursor over each page.
    cursors: HashMap<u64, &'static str>,
}

static SHARED: LazyLock<Mutex<Shared>> = LazyLock::new(Mutex::default);

fn shared<R>(f: impl FnOnce(&mut Shared) -> R) -> R {
    f(&mut SHARED.lock().unwrap_or_else(|e| e.into_inner()))
}

fn emit(event: Event) {
    shared(|s| s.events.as_ref().map(|e| e.unbounded_send(event).ok()));
}

/// Sets where webviews' events go.
pub fn init(events: UnboundedSender<Event>) {
    shared(|s| s.events = Some(events));
}

/// Runs `f` on the GTK thread, after what's queued before it; the thread
/// starts with the first.
fn gtk(f: impl FnOnce() + Send + 'static) {
    static START: Once = Once::new();
    START.call_once(|| {
        std::thread::Builder::new()
            .name("webviews".into())
            .spawn(run)
            .ok();
    });
    glib::idle_add_once(f);
}

fn run() {
    if gtk::init().is_err() {
        emit(Event::Error("Unable to start GTK for webviews".into()));
        return;
    }
    // Keys go straight to the page: an input method server (IBus) would
    // hand them back to a window this offscreen one doesn't have.
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_im_module(Some("gtk-im-context-simple"));
    }
    let Some(context) = WebContext::default() else {
        emit(Event::Error("Unable to start WebKitGTK".into()));
        return;
    };
    let security = context.security_manager();
    for scheme in ["vsn-webview", "vsn-resource"] {
        context.register_uri_scheme(scheme, serve);
        if let Some(security) = &security {
            security.register_uri_scheme_as_secure(scheme);
            security.register_uri_scheme_as_cors_enabled(scheme);
        }
    }
    gtk::main();
}

// ---- the UI thread -----------------------------------------------------------

/// A webview as the UI sees it.
#[derive(Default)]
struct Placed {
    /// Where it shows, in the window's physical pixels, and the scale.
    bounds: Option<((i32, i32, i32, i32), f32)>,
    /// `enableFindWidget`: Ctrl+F is vsn's find, not the page's.
    find: bool,
    /// Its newest frame, in gpui's atlas.
    image: Option<Arc<RenderImage>>,
}

thread_local! {
    static UI: RefCell<HashMap<u64, Placed>> = RefCell::default();
    /// Frames of disposed webviews, to drop from the atlas.
    static DEAD: RefCell<Vec<Arc<RenderImage>>> = RefCell::default();
}

/// Opens webview `handle`, which shows nothing until it has `html` and
/// bounds.
pub fn create(handle: u64, roots: Vec<PathBuf>) {
    UI.with_borrow_mut(|u| u.insert(handle, Placed::default()));
    gtk(move || make(handle, roots));
}

/// Sets webview `handle`'s page, reloading it.
pub fn set_html(handle: u64, html: String) {
    gtk(move || {
        let loaded = with(handle, |v| {
            v.html = html;
            v.nav += 1;
        });
        if loaded.is_some() {
            load(handle);
        }
    });
}

pub fn set_roots(handle: u64, roots: Vec<PathBuf>) {
    gtk(move || _ = with(handle, |v| v.roots = roots));
}

/// Sends the page a message, once it has loaded.
pub fn post(handle: u64, json: String) {
    gtk(move || {
        let view = with(handle, |v| match v.ready {
            true => Some(v.view.clone()),
            false => {
                v.queue.push(json.clone());
                None
            }
        });
        if let Some(view) = view.flatten() {
            post_to(&view, &json);
        }
    });
}

/// The theme pages get: `{kind, name, background, vars}`. Open pages
/// switch to it.
pub fn set_theme(theme: Value) {
    let changed = shared(|s| (s.theme != theme).then(|| s.theme = theme.clone()));
    if changed.is_none() {
        return;
    }
    gtk(move || {
        let msg = json!({ "__vsnTheme": theme }).to_string();
        // A page still loading gets it once it has.
        let views: Vec<(u64, WebView, bool)> = VIEWS.with_borrow_mut(|v| {
            (v.iter_mut())
                .map(|(h, v)| {
                    if !v.ready {
                        v.queue.push(msg.clone());
                    }
                    (*h, v.view.clone(), v.ready)
                })
                .collect()
        });
        for (handle, view, ready) in views {
            if ready {
                post_to(&view, &msg);
            }
            set_background(handle, &view);
        }
    });
}

/// Puts webview `handle` in the side bar, for its background.
pub fn set_sidebar(handle: u64) {
    gtk(move || {
        if let Some(view) = with(handle, |v| (v.sidebar = true, v.view.clone()).1) {
            set_background(handle, &view);
        }
    });
}

/// Gives webview `handle` vsn's find widget (`enableFindWidget`).
pub fn set_find(handle: u64, on: bool) {
    UI.with_borrow_mut(|u| u.get_mut(&handle).map(|p| p.find = on));
}

/// Webview `handle`'s page state (`setState`), Null if none.
pub fn state(handle: u64) -> Value {
    shared(|s| s.states.get(&handle).cloned().unwrap_or(Value::Null))
}

/// The state webview `handle`'s next page starts with, as a revived panel's.
pub fn set_state(handle: u64, state: Value) {
    shared(|s| s.states.insert(handle, state));
}

/// Shows webview `handle` at `bounds` (the window's physical pixels), its
/// page at `scale`.
pub fn place(handle: u64, bounds: (i32, i32, i32, i32), scale: f32) {
    let changed = UI.with_borrow_mut(|u| {
        let p = u.get_mut(&handle)?;
        (p.bounds.replace((bounds, scale)) != Some((bounds, scale))).then_some(())
    });
    if changed.is_some() {
        let size = ((bounds.2 - bounds.0).max(1), (bounds.3 - bounds.1).max(1));
        gtk(move || resize(handle, size, scale));
    }
}

/// Hides every webview but `keep`, which a `place` this frame shows.
pub fn hide_others(keep: &[u64]) {
    let hidden: Vec<u64> = UI.with_borrow_mut(|u| {
        (u.iter_mut())
            .filter(|(h, p)| !keep.contains(h) && p.bounds.is_some())
            .map(|(h, p)| (p.bounds = None, *h).1)
            .collect()
    });
    for handle in hidden {
        gtk(move || _ = with(handle, |v| v.shown = false));
    }
}

/// Webview `handle`'s newest frame, for gpui to paint.
pub fn frame(handle: u64, window: &mut Window) -> Option<Arc<RenderImage>> {
    for dead in DEAD.with_borrow_mut(std::mem::take) {
        window.drop_image(dead).ok();
    }
    let fresh = shared(|s| s.frames.remove(&handle));
    let (image, old) = UI.with_borrow_mut(|u| {
        let p = u.get_mut(&handle)?;
        let old = fresh
            .and_then(|(w, h, px)| image::RgbaImage::from_raw(w, h, px))
            .map(|buf| Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
            .and_then(|new| p.image.replace(new));
        Some((p.image.clone(), old))
    })?;
    if let Some(old) = old {
        window.drop_image(old).ok();
    }
    image
}

/// The cursor the page shows where the mouse is.
pub fn cursor(handle: u64) -> CursorStyle {
    match shared(|s| s.cursors.get(&handle).copied()).unwrap_or("default") {
        "pointer" => CursorStyle::PointingHand,
        "text" => CursorStyle::IBeam,
        "vertical-text" => CursorStyle::IBeamCursorForVerticalLayout,
        "crosshair" | "cell" => CursorStyle::Crosshair,
        "grab" => CursorStyle::OpenHand,
        "grabbing" | "move" | "all-scroll" => CursorStyle::ClosedHand,
        "not-allowed" | "no-drop" => CursorStyle::OperationNotAllowed,
        "col-resize" => CursorStyle::ResizeColumn,
        "row-resize" => CursorStyle::ResizeRow,
        "ew-resize" | "e-resize" | "w-resize" => CursorStyle::ResizeLeftRight,
        "ns-resize" | "n-resize" | "s-resize" => CursorStyle::ResizeUpDown,
        "nesw-resize" | "ne-resize" | "sw-resize" => CursorStyle::ResizeUpRightDownLeft,
        "nwse-resize" | "nw-resize" | "se-resize" => CursorStyle::ResizeUpLeftDownRight,
        "copy" => CursorStyle::DragCopy,
        "alias" => CursorStyle::DragLink,
        "context-menu" => CursorStyle::ContextualMenu,
        _ => CursorStyle::Arrow,
    }
}

/// Sends webview `handle` mouse input at `at`, in the window's physical
/// pixels.
pub fn mouse(handle: u64, input: Mouse, at: (i32, i32), mods: Modifiers) {
    let Some(((left, top, ..), _)) = UI.with_borrow(|u| u.get(&handle)?.bounds) else {
        return;
    };
    let (x, y) = ((at.0 - left) as f64, (at.1 - top) as f64);
    gtk(move || send_mouse(handle, input, x, y, mod_mask(mods)));
}

/// Whether `keystroke`, typed in webview `handle`, is vsn's shortcut as
/// well as the page's key (as [`super::webview_page::page_owns`] says).
pub fn is_shortcut(handle: u64, keystroke: &Keystroke) -> bool {
    let m = keystroke.modifiers;
    let function = keystroke
        .key
        .strip_prefix('f')
        .is_some_and(|n| n.parse::<u8>().is_ok());
    let find = UI.with_borrow(|u| u.get(&handle).is_some_and(|p| p.find));
    (m.control || m.alt || function) && !page_owns(m.control, m.alt, m.shift, &keystroke.key, find)
}

/// Types `keystroke` in webview `handle`'s page.
pub fn key(handle: u64, keystroke: &Keystroke) {
    let mods = mod_mask(keystroke.modifiers);
    let keyval = keyval(
        &keystroke.key,
        keystroke.key_char.as_deref(),
        keystroke.modifiers.shift,
    );
    let text = keystroke.key_char.clone().unwrap_or_default();
    gtk(move || send_key(handle, keyval, mods, text));
}

/// Gives webview `handle` the keyboard.
pub fn focus(handle: u64) {
    gtk(move || focus_view(handle));
}

/// Finds `term` in webview `handle`'s page, highlighting every match, or
/// steps to the next (`back`: previous) match when it's the term already
/// found.
pub fn find(handle: u64, term: &str, case: bool, back: bool) {
    let term = term.to_string();
    gtk(move || find_in(handle, term, case, back));
}

/// Ends find in webview `handle`'s page, clearing its highlights.
pub fn stop_find(handle: u64) {
    gtk(move || {
        let view = with(handle, |v| (v.finding = None, v.view.clone()).1);
        if let Some(find) = view.and_then(|v| v.find_controller()) {
            find.search_finish();
        }
    });
}

// ponytail: Shift+drop of files on a page is Windows-only (WebView2 takes
// Explorer's drag); a GTK drag would need a real XDND source.
pub fn drag_files(_: u64, _: &[PathBuf], _: (i32, i32)) {}
pub fn finish_drop(_: u64) {}
pub fn drag_leave(_: u64) {}
pub fn shift_held() -> bool {
    false
}

pub fn dispose(handle: u64) {
    if let Some(image) = UI
        .with_borrow_mut(|u| u.remove(&handle))
        .and_then(|p| p.image)
    {
        DEAD.with_borrow_mut(|d| d.push(image));
    }
    shared(|s| {
        s.frames.remove(&handle);
        s.cursors.remove(&handle);
    });
    gtk(move || {
        if let Some(v) = VIEWS.with_borrow_mut(|v| v.remove(&handle)) {
            // SAFETY: the window is ours and nothing uses it after this.
            unsafe { v.win.destroy() };
        }
    });
}

fn mod_mask(m: Modifiers) -> u32 {
    let mut mask = 0;
    for (on, bit) in [
        (m.shift, gdk::ModifierType::SHIFT_MASK),
        (m.control, gdk::ModifierType::CONTROL_MASK),
        (m.alt, gdk::ModifierType::MOD1_MASK),
        (m.platform, gdk::ModifierType::SUPER_MASK),
    ] {
        if on {
            mask |= bit.bits();
        }
    }
    mask
}

/// The GDK keyval of gpui key `key`, typing `text`.
fn keyval(key: &str, text: Option<&str>, shift: bool) -> u32 {
    let named = match key {
        "enter" => 0xff0d,
        "escape" => 0xff1b,
        "tab" if shift => 0xfe20, // ISO_Left_Tab
        "tab" => 0xff09,
        "backspace" => 0xff08,
        "delete" => 0xffff,
        "insert" => 0xff63,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "space" => 0x20,
        "menu" => 0xff67,
        _ => match key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
            Some(n @ 1..=35) => 0xffbe + n - 1,
            _ => 0,
        },
    };
    if named != 0 {
        return named;
    }
    let one = |s: &str| {
        let mut chars = s.chars();
        chars.next().filter(|_| chars.next().is_none())
    };
    // Shift's letter, when a shortcut (Ctrl+Shift+A) types nothing.
    match text.and_then(one) {
        Some(c) => unicode_keyval(c),
        None => match one(key) {
            Some(c) if shift => unicode_keyval(c.to_ascii_uppercase()),
            Some(c) => unicode_keyval(c),
            None => 0xffffff, // VoidSymbol
        },
    }
}

/// X11's keyval for `c`: Latin-1 is itself, the rest Unicode's own range
/// (gdk_unicode_to_keyval, which needs GTK's thread).
fn unicode_keyval(c: char) -> u32 {
    match c as u32 {
        c @ (0x20..=0x7e | 0xa0..=0xff) => c,
        c => 0x0100_0000 | c,
    }
}

// ---- the GTK thread ----------------------------------------------------------

/// A page, on the GTK thread.
struct View {
    win: gtk::OffscreenWindow,
    view: WebView,
    content: UserContentManager,
    html: String,
    /// Bumped per `html`, so the page reloads.
    nav: u64,
    roots: Vec<PathBuf>,
    /// The page has loaded, so messages go straight to it.
    ready: bool,
    queue: Vec<String>,
    sidebar: bool,
    /// In the window, so its frames are wanted.
    shown: bool,
    /// A frame is on its way.
    due: bool,
    /// The buttons held, as GDK's mask.
    buttons: u32,
    /// The term and case of the find running, and its match and count.
    finding: Option<(String, bool)>,
    found: (i32, i32),
    /// Its cursor is to be looked at.
    cursor_due: bool,
}

thread_local! {
    static VIEWS: RefCell<HashMap<u64, View>> = RefCell::default();
    /// GDK's cursors for CSS's names: WebKit sets them by name, and GDK
    /// keeps one of each.
    static CURSORS: RefCell<Vec<(&'static str, gdk::Cursor)>> = RefCell::default();
}

/// `f` on view `handle`. GTK calls go outside it, as they may call back.
fn with<R>(handle: u64, f: impl FnOnce(&mut View) -> R) -> Option<R> {
    VIEWS.with_borrow_mut(|v| v.get_mut(&handle).map(f))
}

fn page_origin(handle: u64) -> String {
    format!("vsn-webview://webview-{handle}/")
}

/// The page's `window.chrome.webview`, as WebView2 gives it, over WebKit's
/// message handlers, for [`PRELUDE`].
const SHIM: &str = r#"(() => {
  if (window.top !== window) return;
  const listeners = [];
  window.chrome = window.chrome || {};
  window.chrome.webview = {
    postMessage: (m) => window.webkit.messageHandlers.vsn.postMessage(JSON.stringify(m ?? null)),
    addEventListener: (type, f) => type === 'message' && listeners.push(f),
  };
  window.__vsnPost = (data) => listeners.forEach((f) => f({ data }));
})();
"#;

fn make(handle: u64, roots: Vec<PathBuf>) {
    let content = UserContentManager::new();
    content.register_script_message_handler("vsn");
    content.connect_script_message_received(Some("vsn"), move |_, value| {
        use javascriptcore::ValueExt as _;
        let text = value.js_value().map(|v| v.to_str()).unwrap_or_default();
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(mut m)) if m.contains_key("$vsnState") => {
                let state = m.remove("$vsnState").unwrap_or_default();
                shared(|s| s.states.insert(handle, state));
            }
            Ok(msg) => emit(Event::Message(handle, msg)),
            Err(_) => {}
        }
    });
    let view = WebView::with_user_content_manager(&content);
    if let Some(settings) = WebViewExt::settings(&view) {
        // Software drawing, which the offscreen window can hand over.
        settings.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Never);
        settings.set_enable_developer_extras(true);
        settings.set_javascript_can_access_clipboard(true);
        settings.set_enable_back_forward_navigation_gestures(false);
    }
    view.connect_load_changed(move |view, event| {
        if event != webkit2gtk::LoadEvent::Finished {
            return;
        }
        let queue = with(handle, |v| (v.ready = true, std::mem::take(&mut v.queue)).1);
        for msg in queue.unwrap_or_default() {
            post_to(view, &msg);
        }
    });
    // The page stays put: links go to the browser, as VS Code's do.
    // ponytail: WebKitGTK doesn't say which frame navigates, so only a
    // click (or a new window) leaves; an iframe's own loads go ahead.
    view.connect_decide_policy(move |_, decision, kind| {
        let Some(nav) = decision.downcast_ref::<NavigationPolicyDecision>() else {
            return false;
        };
        let Some(action) = nav.navigation_action() else {
            return false;
        };
        let uri = (action.request())
            .and_then(|r| r.uri())
            .map(|u| u.to_string())
            .unwrap_or_default();
        let leaves = kind == PolicyDecisionType::NewWindowAction
            || (action.is_user_gesture() && !uri.starts_with(&page_origin(handle)));
        if !leaves {
            return false;
        }
        decision.ignore();
        if ["http:", "https:", "mailto:"]
            .iter()
            .any(|s| uri.starts_with(s))
        {
            emit(Event::Open(uri));
        }
        true
    });
    view.connect_create(|_, action| {
        if let Some(uri) = action.clone().request().and_then(|r| r.uri()) {
            emit(Event::Open(uri.to_string()));
        }
        None
    });
    view.connect_show_option_menu(|_, options, event, _| show_options(options, event));
    // VS Code's webview iframes allow clipboard-read; Draw.io's paste reads it.
    view.connect_permission_request(|_, request| {
        let clipboard = request.type_().name() == "WebKitClipboardPermissionRequest";
        if clipboard {
            request.allow();
        }
        clipboard
    });
    let win = gtk::OffscreenWindow::new();
    win.set_default_size(1, 1);
    win.add(&view);
    win.connect_damage_event(move |_, _| {
        frame_due(handle);
        false
    });
    win.show_all();
    VIEWS.with_borrow_mut(|v| {
        v.insert(
            handle,
            View {
                win,
                view: view.clone(),
                content,
                html: String::new(),
                nav: 0,
                roots,
                ready: false,
                queue: Vec::new(),
                sidebar: false,
                shown: false,
                due: false,
                buttons: 0,
                finding: None,
                found: (0, 0),
                cursor_due: false,
            },
        )
    });
    set_background(handle, &view);
    let find = view.find_controller();
    if let Some(find) = find {
        find.connect_found_text(move |_, count| {
            let found = with(handle, |v| {
                v.found.1 = count as i32;
                v.found
            });
            if let Some((active, count)) = found {
                emit(Event::Find(handle, active.min(count), count));
            }
        });
        find.connect_failed_to_find_text(move |_| {
            with(handle, |v| v.found = (0, 0));
            emit(Event::Find(handle, 0, 0));
        });
    }
}

/// A `<select>`'s options, at the pointer that opened them: WebKit's own
/// menu places itself by the page's window, which is offscreen (nowhere),
/// as GTK can't hang a menu off it.
fn show_options(options: &webkit2gtk::OptionMenu, event: &gdk::Event) -> bool {
    use webkit2gtk::OptionMenuExt as _;
    let menu = gtk::Menu::new();
    for i in 0..options.n_items() {
        let Some(mut item) = options.item(i) else {
            continue;
        };
        let label = item.label().unwrap_or_default();
        let entry = match item.is_group_label() {
            true => gtk::MenuItem::with_label(&label),
            false => {
                let entry = gtk::CheckMenuItem::with_label(&label);
                entry.set_draw_as_radio(true);
                entry.set_active(item.is_selected());
                entry.upcast()
            }
        };
        entry.set_sensitive(item.is_enabled() && !item.is_group_label());
        let options = options.clone();
        entry.connect_activate(move |_| options.activate_item(i));
        menu.append(&entry);
    }
    // After an item's activate, which closes it anyway.
    let closing = options.clone();
    menu.connect_deactivate(move |_| {
        let options = closing.clone();
        glib::idle_add_local_once(move || options.close());
    });
    menu.show_all();
    menu.popup_at_pointer(Some(event));
    true
}

/// Answers the pages' requests: each its document, and extension files
/// under its `localResourceRoots`.
fn serve(request: &URISchemeRequest) {
    let uri = request.uri().map(|u| u.to_string()).unwrap_or_default();
    let uri = uri.split(['?', '#']).next().unwrap_or_default();
    let (status, mime, body) = if let Some(rest) = uri.strip_prefix("vsn-webview://webview-") {
        let (handle, page) = rest.split_once('/').unwrap_or((rest, ""));
        let html = handle
            .parse()
            .ok()
            .and_then(|h| with(h, |v| v.html.clone()));
        match (page, html) {
            ("index.html", Some(html)) => (200, "text/html", page_html(&html).into_bytes()),
            _ => (404, "text/plain", Vec::new()),
        }
    } else if let Some(path) = uri.strip_prefix("vsn-resource://file") {
        let view = request.web_view();
        let roots = VIEWS.with_borrow(|v| {
            (v.values())
                .find(|v| view.as_ref() == Some(&v.view))
                .map(|v| v.roots.clone())
        });
        read_resource(&roots.unwrap_or_default(), path)
    } else {
        (404, "text/plain", Vec::new())
    };
    let len = body.len() as i64;
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(body));
    let response = URISchemeResponse::new(&stream, len);
    response.set_status(status as u32, None);
    response.set_content_type(mime);
    let headers = soup::MessageHeaders::new(soup::MessageHeadersType::Response);
    headers.append("Access-Control-Allow-Origin", "*");
    headers.append("Cache-Control", "no-cache");
    response.set_http_headers(headers);
    request.finish_with_response(&response);
}

/// Loads the page with the script VS Code's webviews get: the
/// `acquireVsCodeApi` and the theme. The script goes in first, so the
/// page starts with it.
fn load(handle: u64) {
    let Some((view, content, nav)) = with(handle, |v| {
        (v.ready = false, (v.view.clone(), v.content.clone(), v.nav)).1
    }) else {
        return;
    };
    let (theme, state) = shared(|s| {
        (
            s.theme.clone(),
            s.states.get(&handle).cloned().unwrap_or(Value::Null),
        )
    });
    let script = SHIM.to_string()
        + &PRELUDE
            .replace("$THEME", &theme.to_string())
            .replace("$STATE", &state.to_string());
    content.remove_all_scripts();
    content.add_script(&UserScript::new(
        &script,
        UserContentInjectedFrames::TopFrame,
        UserScriptInjectionTime::Start,
        &[],
        &[],
    ));
    view.load_uri(&format!("{}index.html?n={nav}", page_origin(handle)));
}

fn post_to(view: &WebView, json: &str) {
    let script = format!("window.__vsnPost && window.__vsnPost({json})");
    view.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
}

/// Webview `handle`'s background, behind a page that has none: the theme's
/// editor or, in the side bar, side bar background.
fn set_background(handle: u64, view: &WebView) {
    let sidebar = with(handle, |v| v.sidebar).unwrap_or_default();
    let key = if sidebar {
        "sidebarBackground"
    } else {
        "background"
    };
    let bg = shared(|s| s.theme[key].as_str().and_then(parse_rgb));
    let (r, g, b) = bg.unwrap_or((30, 30, 30));
    let f = |c: u8| c as f64 / 255.;
    view.set_background_color(&gdk::RGBA::new(f(r), f(g), f(b), 1.));
}

/// Sizes webview `handle`'s page to `size` physical pixels at `scale`,
/// showing it.
fn resize(handle: u64, size: (i32, i32), scale: f32) {
    let Some((win, view)) = with(handle, |v| {
        (v.shown = true, (v.win.clone(), v.view.clone())).1
    }) else {
        return;
    };
    if (win.allocated_width(), win.allocated_height()) != size {
        view.set_size_request(size.0, size.1);
        win.resize(size.0, size.1);
    }
    // CSS pixels stay vsn's, its frames the screen's.
    if (view.zoom_level() - scale as f64).abs() > 1e-3 {
        view.set_zoom_level(scale as f64);
    }
    frame_due(handle);
}

/// Takes webview `handle`'s next frame soon, once however much it draws.
fn frame_due(handle: u64) {
    if with(handle, |v| std::mem::replace(&mut v.due, true)) == Some(false) {
        glib::timeout_add_local_once(Duration::from_millis(8), move || take_frame(handle));
    }
}

/// Copies webview `handle`'s page for the UI, which repaints.
fn take_frame(handle: u64) {
    let Some((win, shown)) = with(handle, |v| (v.due = false, (v.win.clone(), v.shown)).1) else {
        return;
    };
    let (w, h) = (win.allocated_width(), win.allocated_height());
    let Some(surface) = win.surface().filter(|_| shown && w > 1 && h > 1) else {
        return;
    };
    let Ok(mut image) = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h) else {
        return;
    };
    if let Ok(cr) = gtk::cairo::Context::new(&image) {
        cr.set_source_surface(&surface, 0., 0.).ok();
        cr.paint().ok();
    }
    let stride = image.stride() as usize;
    let (w, h) = (w as usize, h as usize);
    let Ok(data) = image.data() else { return };
    let mut px = Vec::with_capacity(w * h * 4);
    for row in data.chunks(stride).take(h) {
        px.extend_from_slice(&row[..w * 4]);
    }
    shared(|s| s.frames.insert(handle, (w as u32, h as u32, px)));
    emit(Event::Redraw);
    look_at_cursor(handle);
}

/// Notes the cursor WebKit set on webview `handle`, by its CSS name.
fn look_at_cursor(handle: u64) {
    let Some(view) = with(handle, |v| (v.cursor_due = false, v.view.clone()).1) else {
        return;
    };
    let cursor = view.window().and_then(|w| w.cursor());
    let name = cursor.map_or("default", |cursor| {
        CURSORS.with_borrow_mut(|known| {
            if known.is_empty() {
                let display = view.display();
                for name in CSS_CURSORS {
                    if let Some(c) = gdk::Cursor::from_name(&display, name) {
                        known.push((name, c));
                    }
                }
            }
            known
                .iter()
                .find(|(_, c)| *c == cursor)
                .map_or("default", |(n, _)| n)
        })
    });
    if shared(|s| s.cursors.insert(handle, name)) != Some(name) {
        emit(Event::Redraw);
    }
}

const CSS_CURSORS: [&str; 34] = [
    "default",
    "help",
    "pointer",
    "progress",
    "wait",
    "cell",
    "crosshair",
    "text",
    "vertical-text",
    "alias",
    "copy",
    "move",
    "no-drop",
    "not-allowed",
    "grab",
    "grabbing",
    "all-scroll",
    "col-resize",
    "row-resize",
    "n-resize",
    "e-resize",
    "s-resize",
    "w-resize",
    "ne-resize",
    "nw-resize",
    "se-resize",
    "sw-resize",
    "ew-resize",
    "ns-resize",
    "nesw-resize",
    "nwse-resize",
    "zoom-in",
    "zoom-out",
    "context-menu",
];

/// Milliseconds for GDK events' times.
fn now() -> u32 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_millis() as u32
}

/// Dispatches `event` (made by gdk_event_new) to `window`'s widget, as if
/// the display sent it, then frees it.
///
/// # Safety
/// `event` is a fresh GdkEvent whose type matches the fields set.
unsafe fn dispatch(event: *mut gdk::ffi::GdkEvent, window: &gdk::Window, keyboard: bool) {
    use glib::translate::ToGlibPtr as _;
    unsafe {
        let w: *mut gdk::ffi::GdkWindow = window.to_glib_full();
        (*event).any.window = w;
        (*event).any.send_event = 1;
        let seat = gdk::Display::default().and_then(|d| d.default_seat());
        let device = seat.and_then(|s| if keyboard { s.keyboard() } else { s.pointer() });
        if let Some(device) = device {
            gdk::ffi::gdk_event_set_device(event, device.to_glib_none().0);
        }
        // GTK asks X about an offscreen window (where the pointer is, for
        // a focus change), which X has never heard of: it's not an error.
        gdk::ffi::gdk_error_trap_push();
        gtk::ffi::gtk_main_do_event(event);
        gdk::ffi::gdk_error_trap_pop_ignored();
        gdk::ffi::gdk_event_free(event);
    }
}

fn button_number(b: Button) -> (u32, u32) {
    match b {
        Button::Left => (1, gdk::ModifierType::BUTTON1_MASK.bits()),
        Button::Middle => (2, gdk::ModifierType::BUTTON2_MASK.bits()),
        Button::Right => (3, gdk::ModifierType::BUTTON3_MASK.bits()),
        Button::X1 => (8, 0),
        Button::X2 => (9, 0),
    }
}

fn send_mouse(handle: u64, input: Mouse, x: f64, y: f64, mods: u32) {
    use gdk::ffi::*;
    let Some((view, held, due)) = with(handle, |v| {
        let held = v.buttons;
        match input {
            Mouse::Down(b) | Mouse::Double(b) => v.buttons |= button_number(b).1,
            Mouse::Up(b) => v.buttons &= !button_number(b).1,
            _ => {}
        }
        (
            v.view.clone(),
            held,
            std::mem::replace(&mut v.cursor_due, true),
        )
    }) else {
        return;
    };
    let Some(window) = view.window() else { return };
    let state = mods | held;
    // SAFETY: each event's fields are its type's.
    unsafe {
        let event = match input {
            Mouse::Move => {
                let e = gdk_event_new(GDK_MOTION_NOTIFY);
                let m = &mut (*e).motion;
                (m.x, m.y, m.x_root, m.y_root, m.state, m.time) = (x, y, x, y, state, now());
                e
            }
            Mouse::Down(b) | Mouse::Double(b) | Mouse::Up(b) => {
                let kind = match input {
                    Mouse::Up(_) => GDK_BUTTON_RELEASE,
                    _ => GDK_BUTTON_PRESS,
                };
                let e = gdk_event_new(kind);
                let m = &mut (*e).button;
                (m.x, m.y, m.x_root, m.y_root, m.state, m.time) = (x, y, x, y, state, now());
                m.button = button_number(b).0;
                e
            }
            Mouse::Wheel(_) | Mouse::HWheel(_) => {
                let e = gdk_event_new(GDK_SCROLL);
                let m = &mut (*e).scroll;
                (m.x, m.y, m.x_root, m.y_root, m.state, m.time) = (x, y, x, y, state, now());
                m.direction = GDK_SCROLL_SMOOTH;
                // A notch is a step; GDK's down and right are positive.
                (m.delta_x, m.delta_y) = match input {
                    Mouse::Wheel(d) => (0., -d as f64 / 120.),
                    Mouse::HWheel(d) => (d as f64 / 120., 0.),
                    _ => unreachable!(),
                };
                e
            }
            Mouse::Leave => {
                let e = gdk_event_new(GDK_LEAVE_NOTIFY);
                let m = &mut (*e).crossing;
                (m.x, m.y, m.state, m.time) = (x, y, state, now());
                (m.mode, m.detail) = (GDK_CROSSING_NORMAL, GDK_NOTIFY_ANCESTOR);
                e
            }
        };
        dispatch(event, &window, false);
    }
    // WebKit sets the cursor when the page has answered.
    if !due {
        glib::timeout_add_local_once(Duration::from_millis(50), move || look_at_cursor(handle));
    }
}

fn send_key(handle: u64, keyval: u32, mods: u32, text: String) {
    use gdk::ffi::*;
    let Some(win) = with(handle, |v| v.win.clone()) else {
        return;
    };
    let Some(window) = win.window() else { return };
    let keycode = gdk::Keymap::for_display(&win.display())
        .and_then(|k| k.entries_for_keyval(keyval).first().map(|e| e.keycode()))
        .unwrap_or(0);
    for kind in [GDK_KEY_PRESS, GDK_KEY_RELEASE] {
        // SAFETY: a key event's fields; GDK frees `string` with it.
        unsafe {
            let e = gdk_event_new(kind);
            let k = &mut (*e).key;
            (k.time, k.state, k.keyval) = (now(), mods, keyval);
            k.hardware_keycode = keycode as u16;
            let text = std::ffi::CString::new(text.as_str()).unwrap_or_default();
            k.length = text.as_bytes().len() as i32;
            k.string = glib::ffi::g_strdup(text.as_ptr());
            dispatch(e, &window, true);
        }
    }
}

/// Gives webview `handle` the keyboard, its window seeming the active one;
/// the others lose it.
fn focus_view(handle: u64) {
    let views: Vec<(u64, gtk::OffscreenWindow, WebView)> = VIEWS.with_borrow(|v| {
        v.iter()
            .map(|(h, v)| (*h, v.win.clone(), v.view.clone()))
            .collect()
    });
    for (h, win, view) in views {
        let Some(window) = win.window() else { continue };
        if h == handle {
            view.grab_focus();
        }
        // SAFETY: a focus-change event's fields.
        unsafe {
            let e = gdk::ffi::gdk_event_new(gdk::ffi::GDK_FOCUS_CHANGE);
            (*e).focus_change.in_ = (h == handle) as i16;
            dispatch(e, &window, true);
        }
    }
}

fn find_in(handle: u64, term: String, case: bool, back: bool) {
    let Some((view, again)) = with(handle, |v| {
        let key = Some((term.clone(), case));
        let again = v.finding == key;
        v.finding = key;
        let (active, count) = v.found;
        v.found.0 = match (again, back) {
            (false, _) => 1,
            _ if count == 0 => 0,
            (true, false) => active % count + 1,
            (true, true) => (active + count - 2) % count + 1,
        };
        (v.view.clone(), again)
    }) else {
        return;
    };
    let Some(find) = view.find_controller() else {
        return;
    };
    match (again, back) {
        (true, false) => find.search_next(),
        (true, true) => find.search_previous(),
        (false, _) => {
            // From the top, not on from the last term's match, so the
            // first match is 1 as the label counts.
            find.search_finish();
            view.evaluate_javascript(
                "getSelection().removeAllRanges()",
                None,
                None,
                None::<&gio::Cancellable>,
                |_| {},
            );
            let mut options = FindOptions::WRAP_AROUND;
            if !case {
                options |= FindOptions::CASE_INSENSITIVE;
            }
            find.search(&term, options.bits(), 1000);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyvals() {
        assert_eq!(keyval("a", Some("a"), false), 'a' as u32);
        assert_eq!(keyval("a", Some("A"), true), 'A' as u32);
        assert_eq!(keyval("a", None, true), 'A' as u32);
        assert_eq!(keyval("enter", None, false), 0xff0d);
        assert_eq!(keyval("tab", None, true), 0xfe20);
        assert_eq!(keyval("f", None, false), 'f' as u32);
        assert_eq!(keyval("f5", None, false), 0xffc2);
        assert_eq!(keyval("1", Some("!"), true), '!' as u32);
        assert_eq!(keyval("e", Some("€"), false), 0x0100_20ac);
    }
}
