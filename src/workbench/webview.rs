//! Extension webviews (`createWebviewPanel`, custom editors) in WebView2,
//! drawn into vsn's window: each is a WebView2 composition controller
//! whose visual sits in gpui's webview layer, showing through the
//! `WEBVIEW_HOLE` its tab paints, so gpui draws menus and hovers over it.
//! vsn forwards it the mouse; the keyboard reaches it through WebView2's
//! own focus window. Its page
//! vsn serves itself, at `https://webview-{handle}.localhost/index.html`,
//! with the extension's files under `asWebviewUri`'s
//! `https://file.vscode-resource.vscode-cdn.net/` from its
//! `localResourceRoots`. Everything here runs on the UI thread, where
//! WebView2 calls back.

use std::{cell::RefCell, collections::HashMap, path::PathBuf};

use futures::channel::mpsc::UnboundedSender;
use serde_json::{Value, json};
use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    Win32::{
        Foundation::{HWND, POINT, RECT},
        Graphics::DirectComposition::{IDCompositionDevice, IDCompositionVisual},
        UI::{
            Input::KeyboardAndMouse::{
                GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_F1, VK_F24, VK_LBUTTON, VK_LWIN,
                VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_RWIN, VK_SHIFT, VK_XBUTTON1, VK_XBUTTON2,
            },
            Shell::{
                BHID_DataObject, Common::ITEMIDLIST, ILFree, SHCreateMemStream,
                SHCreateShellItemArrayFromIDLists, SHParseDisplayName,
            },
            WindowsAndMessaging::HCURSOR,
        },
    },
    core::{PCWSTR, PWSTR},
};
use windows_core::Interface as _;

use super::webview_page::{PRELUDE, page_html, page_owns, parse_rgb, read_resource};

/// Where `asWebviewUri` points (exthost.js's `cspSource` allows it).
const RESOURCE_ORIGIN: &str = "https://file.vscode-resource.vscode-cdn.net";

/// What a webview tells the workbench.
pub enum Event {
    /// The page's `vscode.postMessage`.
    Message(u64, Value),
    /// A shortcut pressed in the page, for vsn's keybindings.
    Key(String),
    /// A link to open in the browser.
    Open(String),
    /// WebView2 couldn't start.
    Error(String),
    /// A page went in the window, so the workbench redraws to show it.
    Redraw,
    /// Find's active match (from 1; 0 or -1 for none) and match count changed.
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
    /// The second press of a double click, which Windows sends in place
    /// of a down.
    Double(Button),
    Up(Button),
    /// Wheel turns in `WHEEL_DELTA`s (120 a notch), up positive.
    Wheel(i32),
    /// Sideways, right positive.
    HWheel(i32),
    Leave,
}

struct Slot {
    controller: Option<ICoreWebView2Controller>,
    comp: Option<ICoreWebView2CompositionController>,
    /// Its visual and the webview layer it's in, which a new renderer (a
    /// lost device) replaces.
    visual: Option<(IDCompositionVisual, IDCompositionVisual)>,
    /// The page's cursor.
    cursor: Option<isize>,
    /// The mouse is over the page, so leaving it tells it.
    hovered: bool,
    webview: Option<ICoreWebView2>,
    html: String,
    /// Bumped per `html`, so the page reloads.
    nav: u64,
    roots: Vec<PathBuf>,
    /// The page has loaded, so messages go straight to it.
    ready: bool,
    queue: Vec<String>,
    bounds: Option<RECT>,
    /// The id of the injected script, replaced on each load.
    script: Option<String>,
    /// In the side bar, so its background is the side bar's.
    sidebar: bool,
    /// `enableFindWidget`: Ctrl+F is vsn's find, not the page's.
    find: bool,
    /// The term and case sensitivity of the find running, so finding it
    /// again steps to the next match.
    finding: Option<(String, bool)>,
    /// Find's match events are hooked up.
    find_hooked: bool,
    /// Files being dropped on the page, and where: the drop lands once the
    /// page has seen them over it.
    dropping: Option<(windows::Win32::System::Com::IDataObject, POINT)>,
    /// What the page last gave `setState`: its `getState` after a reload,
    /// and kept in the session so a panel's serializer can revive it.
    state: Value,
}

#[derive(Default)]
struct State {
    hwnd: isize,
    events: Option<UnboundedSender<Event>>,
    env: Option<ICoreWebView2Environment>,
    starting: bool,
    slots: HashMap<u64, Slot>,
    /// `{kind, vars}` for the pages' `vscode-dark` classes and `--vscode-*`
    /// variables.
    theme: Value,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

fn emit(event: Event) {
    STATE.with_borrow(|s| s.events.as_ref().map(|e| e.unbounded_send(event).ok()));
}

/// Sets the window webviews live in and where their events go.
pub fn init(hwnd: isize, events: UnboundedSender<Event>) {
    STATE.with_borrow_mut(|s| (s.hwnd, s.events) = (hwnd, Some(events)));
}

/// Opens webview `handle`, which shows nothing until it has `html` and
/// bounds. The environment starts with the first.
pub fn create(handle: u64, roots: Vec<PathBuf>) {
    let start = STATE.with_borrow_mut(|s| {
        s.slots.insert(
            handle,
            Slot {
                controller: None,
                comp: None,
                visual: None,
                cursor: None,
                hovered: false,
                webview: None,
                html: String::new(),
                nav: 0,
                roots,
                ready: false,
                queue: Vec::new(),
                bounds: None,
                script: None,
                sidebar: false,
                find: false,
                finding: None,
                find_hooked: false,
                state: Value::Null,
                dropping: None,
            },
        );
        match &s.env {
            Some(env) => Ok(env.clone()),
            None => Err(!std::mem::replace(&mut s.starting, true)),
        }
    });
    // Outside the borrow: WebView2 may call back before returning.
    match start {
        Ok(env) => create_controller(&env, handle),
        Err(true) => start_env(),
        Err(false) => {}
    }
}

fn start_env() {
    let data = dirs::home_dir()
        .unwrap_or_default()
        .join(".vsn")
        .join("webview2");
    let data = CoTaskMemPWSTR::from(data.to_string_lossy().as_ref());
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(|hr, env| {
        let env = match (hr, env) {
            (Ok(()), Some(env)) => env,
            (Err(err), _) => {
                emit(Event::Error(format!("Unable to start WebView2: {err}")));
                return Ok(());
            }
            _ => return Ok(()),
        };
        let handles: Vec<u64> = STATE.with_borrow_mut(|s| {
            s.env = Some(env.clone());
            s.slots.keys().copied().collect()
        });
        for handle in handles {
            create_controller(&env, handle);
        }
        Ok(())
    }));
    // Extra Chromium switches, for trying them out without a rebuild.
    let options = CoreWebView2EnvironmentOptions::default();
    if let Ok(args) = std::env::var("VSN_WEBVIEW_ARGS") {
        unsafe { options.set_additional_browser_arguments(args) };
    }
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let started = unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            *data.as_ref().as_pcwstr(),
            &options,
            &handler,
        )
    };
    if let Err(err) = started {
        emit(Event::Error(format!(
            "Webviews need the WebView2 Runtime, which wasn't found: {err}"
        )));
    }
}

fn create_controller(env: &ICoreWebView2Environment, handle: u64) {
    let hwnd = HWND(STATE.with_borrow(|s| s.hwnd) as _);
    let env2 = env.clone();
    let handler = CreateCoreWebView2CompositionControllerCompletedHandler::create(Box::new(
        move |hr, comp| {
            let comp = comp.filter(|_| hr.is_ok());
            let controller = comp
                .as_ref()
                .and_then(|c| c.cast::<ICoreWebView2Controller>().ok());
            let (Some(comp), Some(controller)) = (comp, controller) else {
                emit(Event::Error(format!("Unable to open a webview: {hr:?}")));
                return Ok(());
            };
            setup(&env2, handle, comp, controller);
            Ok(())
        },
    ));
    let made = (env.cast::<ICoreWebView2Environment3>())
        .and_then(|env| unsafe { env.CreateCoreWebView2CompositionController(hwnd, &handler) });
    if let Err(err) = made {
        emit(Event::Error(format!("Unable to open a webview: {err}")));
    }
}

fn setup(
    env: &ICoreWebView2Environment,
    handle: u64,
    comp: ICoreWebView2CompositionController,
    controller: ICoreWebView2Controller,
) {
    // Closed before WebView2 finished making it.
    if STATE.with_borrow(|s| !s.slots.contains_key(&handle)) {
        unsafe { controller.Close() }.ok();
        return;
    }
    let Ok(webview) = (unsafe { controller.CoreWebView2() }) else {
        return;
    };
    unsafe {
        controller.SetIsVisible(false).ok();
        set_background(handle, &controller);
        if let Ok(settings) = webview.Settings() {
            settings.SetAreDefaultContextMenusEnabled(true).ok();
            settings.SetAreDevToolsEnabled(true).ok();
            settings.SetIsStatusBarEnabled(false).ok();
            settings.SetIsZoomControlEnabled(false).ok();
            if let Ok(s3) = settings.cast::<ICoreWebView2Settings3>() {
                s3.SetAreBrowserAcceleratorKeysEnabled(false).ok();
            }
        }
        webview
            .AddWebResourceRequestedFilter(
                windows::core::w!("*"),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            )
            .ok();
    }
    let mut token = 0i64;
    let env = env.clone();
    let served = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
        if let Some(args) = args {
            serve(&env, handle, &args);
        }
        Ok(())
    }));
    let message = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut json = PWSTR::null();
        unsafe { args.WebMessageAsJson(&mut json) }?;
        let json = CoTaskMemPWSTR::from(json).to_string();
        match serde_json::from_str::<Value>(&json) {
            Ok(Value::Object(mut m)) if m.contains_key("$vsnState") => {
                let state = m.remove("$vsnState").unwrap_or_default();
                STATE.with_borrow_mut(|s| s.slots.get_mut(&handle).map(|slot| slot.state = state));
            }
            Ok(msg) => emit(Event::Message(handle, msg)),
            Err(_) => {}
        }
        Ok(())
    }));
    let completed = NavigationCompletedEventHandler::create(Box::new(move |_, _| {
        let (webview, queue) = STATE.with_borrow_mut(|s| match s.slots.get_mut(&handle) {
            Some(slot) => {
                slot.ready = true;
                (slot.webview.clone(), std::mem::take(&mut slot.queue))
            }
            None => (None, Vec::new()),
        });
        for msg in queue.iter().filter(|_| webview.is_some()) {
            post_to(webview.as_ref().unwrap(), msg);
        }
        Ok(())
    }));
    // The page stays put: links go to the browser, as VS Code's do.
    let starting = NavigationStartingEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut uri = PWSTR::null();
        unsafe { args.Uri(&mut uri) }?;
        let uri = CoTaskMemPWSTR::from(uri).to_string();
        if !uri.starts_with(&page_origin(handle)) {
            unsafe { args.SetCancel(true) }?;
            if uri.starts_with("http:") || uri.starts_with("https:") || uri.starts_with("mailto:") {
                emit(Event::Open(uri));
            }
        }
        Ok(())
    }));
    let new_window = NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut uri = PWSTR::null();
        unsafe { args.Uri(&mut uri) }?;
        emit(Event::Open(CoTaskMemPWSTR::from(uri).to_string()));
        unsafe { args.SetHandled(true) }
    }));
    // VS Code's webview iframes allow clipboard-read; Draw.io's paste reads it.
    let permission = PermissionRequestedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
        unsafe { args.PermissionKind(&mut kind) }?;
        if kind == COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ {
            unsafe { args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW) }?;
        }
        Ok(())
    }));
    let keys = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
        let mut vk = 0u32;
        unsafe {
            args.KeyEventKind(&mut kind)?;
            args.VirtualKey(&mut vk)?;
        }
        let down = kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
            || kind == COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN;
        let find = STATE.with_borrow(|s| s.slots.get(&handle).is_some_and(|slot| slot.find));
        if let Some(key) = down.then(|| shortcut(vk, find)).flatten() {
            emit(Event::Key(key));
        }
        Ok(())
    }));
    let cursor = CursorChangedEventHandler::create(Box::new(move |comp, _| {
        let mut cursor = HCURSOR::default();
        if let Some(comp) = comp {
            unsafe { comp.Cursor(&mut cursor) }.ok();
        }
        let cursor = (!cursor.is_invalid()).then_some(cursor.0 as isize);
        let hovered = STATE.with_borrow_mut(|s| {
            let slot = s.slots.get_mut(&handle)?;
            slot.cursor = cursor;
            Some(slot.hovered)
        });
        if hovered == Some(true) {
            gpui_windows::set_cursor_override(cursor);
        }
        Ok(())
    }));
    unsafe {
        webview.add_WebResourceRequested(&served, &mut token).ok();
        webview.add_WebMessageReceived(&message, &mut token).ok();
        webview.add_NavigationCompleted(&completed, &mut token).ok();
        webview.add_NavigationStarting(&starting, &mut token).ok();
        webview.add_NewWindowRequested(&new_window, &mut token).ok();
        webview
            .add_PermissionRequested(&permission, &mut token)
            .ok();
        controller.add_AcceleratorKeyPressed(&keys, &mut token).ok();
        comp.add_CursorChanged(&cursor, &mut token).ok();
    }
    let (html, bounds) = STATE.with_borrow_mut(|s| {
        let slot = s.slots.get_mut(&handle).unwrap();
        slot.controller = Some(controller);
        slot.comp = Some(comp);
        slot.webview = Some(webview);
        (!slot.html.is_empty(), slot.bounds)
    });
    if html {
        load(handle);
    }
    show(handle, bounds);
}

fn page_origin(handle: u64) -> String {
    format!("https://webview-{handle}.localhost/")
}

/// Webview `handle`'s background, behind a page that has none: the
/// theme's editor or, in the side bar, side bar background.
fn set_background(handle: u64, controller: &ICoreWebView2Controller) {
    let bg = STATE.with_borrow(|s| {
        let sidebar = s.slots.get(&handle).is_some_and(|slot| slot.sidebar);
        let key = if sidebar { "sidebarBackground" } else { "background" };
        s.theme[key].as_str().and_then(parse_rgb)
    });
    let (r, g, b) = bg.unwrap_or((30, 30, 30));
    if let Ok(c2) = controller.cast::<ICoreWebView2Controller2>() {
        let color = COREWEBVIEW2_COLOR { A: 255, R: r, G: g, B: b };
        unsafe { c2.SetDefaultBackgroundColor(color) }.ok();
    }
}

/// Puts webview `handle` in the side bar, for its background.
pub fn set_sidebar(handle: u64) {
    let controller = STATE.with_borrow_mut(|s| {
        let slot = s.slots.get_mut(&handle)?;
        slot.sidebar = true;
        slot.controller.clone()
    });
    if let Some(controller) = controller {
        set_background(handle, &controller);
    }
}

/// A key pressed with Ctrl or Alt, or a function key, as a gpui keystroke
/// for vsn's keybindings. Clipboard, undo, select all and find stay the
/// page's, as VS Code leaves them to its webviews.
/// With `find` (`enableFindWidget`), Ctrl+F is vsn's too.
fn shortcut(vk: u32, find: bool) -> Option<String> {
    let held = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
    let (ctrl, alt, shift) = (held(VK_CONTROL), held(VK_MENU), held(VK_SHIFT));
    let win = held(VK_LWIN) || held(VK_RWIN);
    let function = (VK_F1.0 as u32..=VK_F24.0 as u32).contains(&vk);
    let key = match vk {
        0x41..=0x5A | 0x30..=0x39 => char::from(vk as u8).to_ascii_lowercase().to_string(),
        _ if function => format!("f{}", vk - VK_F1.0 as u32 + 1),
        0x09 => "tab".into(),
        0x0D => "enter".into(),
        0x1B => "escape".into(),
        0x20 => "space".into(),
        0x21 => "pageup".into(),
        0x22 => "pagedown".into(),
        0x23 => "end".into(),
        0x24 => "home".into(),
        0x25 => "left".into(),
        0x26 => "up".into(),
        0x27 => "right".into(),
        0x28 => "down".into(),
        0x2E => "delete".into(),
        0xBA => ";".into(),
        0xBB => "=".into(),
        0xBC => ",".into(),
        0xBD => "-".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xC0 => "`".into(),
        0xDB => "[".into(),
        0xDC => "\\".into(),
        0xDD => "]".into(),
        0xDE => "'".into(),
        _ => return None,
    };
    if !(ctrl || alt || function) {
        return None;
    }
    if page_owns(ctrl, alt, shift, &key, find) {
        return None;
    }
    let mut keystroke = String::new();
    for (on, name) in [
        (ctrl, "ctrl-"),
        (alt, "alt-"),
        (shift, "shift-"),
        (win, "win-"),
    ] {
        if on {
            keystroke.push_str(name);
        }
    }
    Some(keystroke + &key)
}

/// Answers the page's requests: its document, and extension files under
/// its `localResourceRoots`. Anything else goes to the network.
fn serve(
    env: &ICoreWebView2Environment,
    handle: u64,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
) {
    let Ok(request) = (unsafe { args.Request() }) else {
        return;
    };
    let mut uri = PWSTR::null();
    if unsafe { request.Uri(&mut uri) }.is_err() {
        return;
    }
    let uri = CoTaskMemPWSTR::from(uri).to_string();
    let uri = uri.split(['?', '#']).next().unwrap_or_default();
    let response = if let Some(page) = uri.strip_prefix(&page_origin(handle)) {
        let html = STATE.with_borrow(|s| s.slots.get(&handle).map(|s| s.html.clone()));
        match (page, html) {
            ("index.html", Some(html)) => {
                Some((200, "text/html; charset=utf-8", page_html(&html).into_bytes()))
            }
            _ => Some((404, "text/plain", Vec::new())),
        }
    } else if let Some(path) = uri.strip_prefix(RESOURCE_ORIGIN) {
        let roots = STATE.with_borrow(|s| s.slots.get(&handle).map(|s| s.roots.clone()));
        Some(read_resource(&roots.unwrap_or_default(), path))
    } else {
        None
    };
    let Some((status, mime, body)) = response else {
        return;
    };
    let reason = if status == 200 { "OK" } else { "Not Found" };
    let headers = format!(
        "Content-Type: {mime}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache"
    );
    let stream = unsafe { SHCreateMemStream(Some(&body)) };
    let reason = CoTaskMemPWSTR::from(reason);
    let headers = CoTaskMemPWSTR::from(headers.as_str());
    let made = unsafe {
        env.CreateWebResourceResponse(
            stream.as_ref(),
            status.into(),
            *reason.as_ref().as_pcwstr(),
            *headers.as_ref().as_pcwstr(),
        )
    };
    if let Ok(made) = made {
        unsafe { args.SetResponse(&made) }.ok();
    }
}




/// Sets webview `handle`'s page, reloading it.
pub fn set_html(handle: u64, html: String) {
    let loaded = STATE.with_borrow_mut(|s| {
        let slot = s.slots.get_mut(&handle)?;
        slot.html = html;
        slot.nav += 1;
        slot.webview.as_ref().map(|_| ())
    });
    if loaded.is_some() {
        load(handle);
    }
}

pub fn set_roots(handle: u64, roots: Vec<PathBuf>) {
    STATE.with_borrow_mut(|s| s.slots.get_mut(&handle).map(|slot| slot.roots = roots));
}

/// Loads the page with the script VS Code's webviews get: the
/// `acquireVsCodeApi` and the theme. The script goes in first, so the
/// page starts with it.
fn load(handle: u64) {
    let Some((webview, old, nav, theme, state)) = STATE.with_borrow_mut(|s| {
        let theme = s.theme.clone();
        let slot = s.slots.get_mut(&handle)?;
        slot.ready = false;
        let state = slot.state.clone();
        Some((slot.webview.clone()?, slot.script.take(), slot.nav, theme, state))
    }) else {
        return;
    };
    if let Some(old) = old {
        let old = CoTaskMemPWSTR::from(old.as_str());
        unsafe { webview.RemoveScriptToExecuteOnDocumentCreated(*old.as_ref().as_pcwstr()) }.ok();
    }
    let script = PRELUDE
        .replace("$THEME", &theme.to_string())
        .replace("$STATE", &state.to_string());
    let script = CoTaskMemPWSTR::from(script.as_str());
    let view = webview.clone();
    let added =
        AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(move |_, id| {
            STATE.with_borrow_mut(|s| s.slots.get_mut(&handle).map(|slot| slot.script = Some(id)));
            let url = format!("{}index.html?n={nav}", page_origin(handle));
            let url = CoTaskMemPWSTR::from(url.as_str());
            unsafe { view.Navigate(*url.as_ref().as_pcwstr()) }.ok();
            Ok(())
        }));
    unsafe { webview.AddScriptToExecuteOnDocumentCreated(*script.as_ref().as_pcwstr(), &added) }
        .ok();
}


/// Sends the page a message, once it has loaded.
pub fn post(handle: u64, json: String) {
    let webview = STATE.with_borrow_mut(|s| {
        let slot = s.slots.get_mut(&handle)?;
        match (slot.ready, &slot.webview) {
            (true, Some(w)) => Some(w.clone()),
            _ => {
                slot.queue.push(json.clone());
                None
            }
        }
    });
    if let Some(webview) = webview {
        post_to(&webview, &json);
    }
}

fn post_to(webview: &ICoreWebView2, json: &str) {
    let json = CoTaskMemPWSTR::from(json);
    unsafe { webview.PostWebMessageAsJson(*json.as_ref().as_pcwstr()) }.ok();
}

/// The theme pages get: `{kind, name, background, vars}`. Open pages
/// switch to it.
pub fn set_theme(theme: Value) {
    let webviews: Vec<ICoreWebView2> = STATE.with_borrow_mut(|s| {
        if s.theme == theme {
            return Vec::new();
        }
        s.theme = theme.clone();
        s.slots.values().filter_map(|s| s.webview.clone()).collect()
    });
    let msg = json!({ "__vsnTheme": theme }).to_string();
    for webview in webviews {
        post_to(&webview, &msg);
    }
    let controllers: Vec<(u64, ICoreWebView2Controller)> = STATE.with_borrow(|s| {
        (s.slots.iter()).filter_map(|(h, slot)| Some((*h, slot.controller.clone()?))).collect()
    });
    for (handle, controller) in controllers {
        set_background(handle, &controller);
    }
}

/// Shows webview `handle` at `bounds`, or hides it, its visual going in
/// the window's webview layer first.
fn show(handle: u64, bounds: Option<RECT>) {
    let Some((controller, comp, visual, hwnd)) = STATE.with_borrow(|s| {
        let slot = s.slots.get(&handle)?;
        Some((
            slot.controller.clone()?,
            slot.comp.clone()?,
            slot.visual.clone(),
            s.hwnd,
        ))
    }) else {
        return;
    };
    let layer = gpui_windows::webview_layer(hwnd);
    let mut visual = visual.map(|(v, _)| v);
    if let (Some((device, layer)), Some(_)) = (&layer, bounds) {
        if visual.is_none() || attached_to(handle) != Some(layer.clone()) {
            visual = attach(handle, &comp, device, layer);
            emit(Event::Redraw);
        }
    }
    unsafe {
        match (bounds, visual) {
            // WebView2 clips its page to the bounds' size from its visual's
            // origin, so the visual takes the position.
            (Some(bounds), Some(visual)) => {
                visual.SetOffsetX2(bounds.left as f32).ok();
                visual.SetOffsetY2(bounds.top as f32).ok();
                let size = RECT {
                    right: bounds.right - bounds.left,
                    bottom: bounds.bottom - bounds.top,
                    ..RECT::default()
                };
                controller.SetBounds(size).ok();
                controller.SetIsVisible(true).ok();
            }
            (Some(_), None) => {}
            (None, _) => drop(controller.SetIsVisible(false)),
        }
        if let Some((device, _)) = layer {
            device.Commit().ok();
        }
    }
}

fn attached_to(handle: u64) -> Option<IDCompositionVisual> {
    STATE.with_borrow(|s| Some(s.slots.get(&handle)?.visual.as_ref()?.1.clone()))
}

/// Whether webview `handle`'s page is in the window's webview layer, so a
/// hole shows it rather than the desktop.
pub fn attached(handle: u64) -> bool {
    let hwnd = STATE.with_borrow(|s| s.hwnd);
    let layer = gpui_windows::webview_layer(hwnd).map(|(_, l)| l);
    layer.is_some() && attached_to(handle) == layer
}

/// Moves webview `handle`'s page to a new visual in `layer`.
fn attach(
    handle: u64,
    comp: &ICoreWebView2CompositionController,
    device: &IDCompositionDevice,
    layer: &IDCompositionVisual,
) -> Option<IDCompositionVisual> {
    let visual = unsafe { device.CreateVisual() }.ok()?;
    let old = STATE.with_borrow_mut(|s| {
        let slot = s.slots.get_mut(&handle)?;
        slot.visual.replace((visual.clone(), layer.clone()))
    });
    unsafe {
        if let Some((old, old_layer)) = old {
            old_layer.RemoveVisual(&old).ok();
        }
        layer.AddVisual(&visual, false, None).ok();
        comp.SetRootVisualTarget(&visual).ok();
    }
    Some(visual)
}

/// Shows webview `handle` at `bounds`, in the window's physical pixels.
pub fn place(handle: u64, bounds: RECT) {
    let changed = STATE.with_borrow_mut(|s| {
        let layer = gpui_windows::webview_layer(s.hwnd).map(|(_, l)| l);
        let slot = s.slots.get_mut(&handle)?;
        let moved = slot.visual.as_ref().map(|(_, l)| l) != layer.as_ref();
        let old = slot.bounds.replace(bounds);
        (old != Some(bounds) || moved).then_some(())
    });
    if changed.is_some() {
        show(handle, Some(bounds));
    }
}

/// Drags files `paths` over webview `handle`'s page at window point `at`
/// (physical pixels), for [`finish_drop`] to drop, as VS Code passes a
/// Shift+drop to its webviews: the page gets them as `File`s, as from
/// Windows' Explorer. The drop waits as the browser takes the page's
/// `dragover` answer in its own time, and refuses a drop before it.
pub fn drag_files(handle: u64, paths: &[PathBuf], at: (i32, i32)) {
    let Some((comp, bounds, dragging)) = STATE.with_borrow(|s| {
        let slot = s.slots.get(&handle)?;
        let comp = slot.comp.as_ref()?.cast::<ICoreWebView2CompositionController3>().ok()?;
        Some((comp, slot.bounds?, slot.dropping.is_some()))
    }) else {
        return;
    };
    let point = POINT {
        x: at.0 - bounds.left,
        y: at.1 - bounds.top,
    };
    // MK_SHIFT, and DROPEFFECT_COPY.
    let mut effect = 1;
    if dragging {
        unsafe { comp.DragOver(0x4, point, &mut effect) }.ok();
        STATE.with_borrow_mut(|s| {
            let dropping = s.slots.get_mut(&handle)?.dropping.as_mut()?;
            dropping.1 = point;
            Some(())
        });
        return;
    }
    let pidls: Vec<*mut ITEMIDLIST> = (paths.iter())
        .filter_map(|p| {
            let mut pidl = std::ptr::null_mut();
            let name = windows_core::HSTRING::from(p.as_os_str());
            unsafe { SHParseDisplayName(&name, None, &mut pidl, 0, None) }.ok()?;
            Some(pidl)
        })
        .collect();
    let items: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| p.cast_const()).collect();
    // Explorer's own data object for them, with the CF_HDROP pages read.
    let data = unsafe {
        SHCreateShellItemArrayFromIDLists(&items).and_then(|a| {
            a.BindToHandler::<_, windows::Win32::System::Com::IDataObject>(None, &BHID_DataObject)
        })
    };
    for pidl in pidls {
        unsafe { ILFree(Some(pidl)) };
    }
    let Ok(data) = data else { return };
    unsafe { comp.DragEnter(&data, 0x4, point, &mut effect) }.ok();
    STATE.with_borrow_mut(|s| s.slots.get_mut(&handle).map(|slot| slot.dropping = Some((data, point))));
}

/// Shift is held for the drag under way: files from Windows say so in
/// their drag's key state (a drag from Explorer keeps keys from vsn's
/// window, so its modifiers don't know); vsn's own drags read the key.
pub fn shift_held() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    const MK_SHIFT: u32 = 0x4;
    match gpui_windows::drop_key_state() {
        Some(keys) => keys & MK_SHIFT != 0,
        None => (unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) }) < 0,
    }
}

/// The files [`drag_files`] dragged over webview `handle`'s page left it.
pub fn drag_leave(handle: u64) {
    let comp = STATE.with_borrow_mut(|s| {
        let slot = s.slots.get_mut(&handle)?;
        slot.dropping.take()?;
        slot.comp.as_ref()?.cast::<ICoreWebView2CompositionController3>().ok()
    });
    if let Some(comp) = comp {
        unsafe { comp.DragLeave() }.ok();
    }
}

/// Drops the files [`drag_files`] dragged over webview `handle`'s page.
pub fn finish_drop(handle: u64) {
    let Some((comp, (data, point))) = STATE.with_borrow_mut(|s| {
        let slot = s.slots.get_mut(&handle)?;
        let comp = slot.comp.as_ref()?.cast::<ICoreWebView2CompositionController3>().ok()?;
        Some((comp, slot.dropping.take()?))
    }) else {
        return;
    };
    let mut effect = 1;
    unsafe {
        comp.DragOver(0x4, point, &mut effect).ok();
        effect = 1;
        comp.Drop(&data, 0x4, point, &mut effect).ok();
    }
}

/// Hides every webview but `keep`, which a `place` this frame shows.
pub fn hide_others(keep: &[u64]) {
    let hidden: Vec<u64> = STATE.with_borrow_mut(|s| {
        (s.slots.iter_mut())
            .filter(|(h, slot)| !keep.contains(h) && slot.bounds.is_some())
            .map(|(h, slot)| {
                slot.bounds = None;
                *h
            })
            .collect()
    });
    for handle in hidden {
        show(handle, None);
    }
}

/// Sends webview `handle` mouse input at `at`, in the window's physical
/// pixels.
pub fn mouse(handle: u64, input: Mouse, at: (i32, i32), _: gpui_kit::Modifiers) {
    let leave = matches!(input, Mouse::Leave);
    let Some((comp, bounds, cursor)) = STATE.with_borrow_mut(|s| {
        let slot = s.slots.get_mut(&handle)?;
        let was = std::mem::replace(&mut slot.hovered, !leave);
        if leave && !was {
            return None;
        }
        Some((slot.comp.clone()?, slot.bounds?, slot.cursor))
    }) else {
        return;
    };
    // The page shows its own cursor while the mouse is over it.
    gpui_windows::set_cursor_override(if leave { None } else { cursor });
    let held = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
    let keys = [
        (
            VK_LBUTTON,
            COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_LEFT_BUTTON,
        ),
        (
            VK_RBUTTON,
            COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_RIGHT_BUTTON,
        ),
        (
            VK_MBUTTON,
            COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_MIDDLE_BUTTON,
        ),
        (VK_XBUTTON1, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_X_BUTTON1),
        (VK_XBUTTON2, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_X_BUTTON2),
        (VK_SHIFT, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_SHIFT),
        (VK_CONTROL, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_CONTROL),
    ];
    let keys = (keys.into_iter().filter(|(k, _)| held(*k))).fold(0, |all, (_, bit)| all | bit.0);
    // A button's event kind, of left, right, middle and X kinds, and its
    // mouseData.
    let button = |b: Button, kinds: [COREWEBVIEW2_MOUSE_EVENT_KIND; 4]| match b {
        Button::Left => (kinds[0], 0),
        Button::Right => (kinds[1], 0),
        Button::Middle => (kinds[2], 0),
        Button::X1 => (kinds[3], 1),
        Button::X2 => (kinds[3], 2),
    };
    let (kind, data) = match input {
        Mouse::Move => (COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE, 0),
        Mouse::Leave => (COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE, 0),
        Mouse::Wheel(d) => (COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL, d as u32),
        Mouse::HWheel(d) => (COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL, d as u32),
        Mouse::Down(b) => button(
            b,
            [
                COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN,
                COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN,
                COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN,
                COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_DOWN,
            ],
        ),
        Mouse::Double(b) => button(
            b,
            [
                COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK,
                COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOUBLE_CLICK,
                COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOUBLE_CLICK,
                COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_DOUBLE_CLICK,
            ],
        ),
        Mouse::Up(b) => button(
            b,
            [
                COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP,
                COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP,
                COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP,
                COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_UP,
            ],
        ),
    };
    // Relative to the page; a leave has no point.
    let point = match input {
        Mouse::Leave => POINT::default(),
        _ => POINT {
            x: at.0 - bounds.left,
            y: at.1 - bounds.top,
        },
    };
    let keys = COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(keys);
    unsafe { comp.SendMouseInput(kind, keys, data, point) }.ok();
}

/// Gives webview `handle` the keyboard.
pub fn focus(handle: u64) {
    let controller = STATE.with_borrow(|s| s.slots.get(&handle)?.controller.clone());
    if let Some(controller) = controller {
        unsafe { controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC) }.ok();
    }
}

/// Webview `handle`'s page state (`setState`), Null if none.
pub fn state(handle: u64) -> Value {
    STATE.with_borrow(|s| s.slots.get(&handle).map_or(Value::Null, |slot| slot.state.clone()))
}

/// The state webview `handle`'s next page starts with, as a revived panel's.
pub fn set_state(handle: u64, state: Value) {
    STATE.with_borrow_mut(|s| s.slots.get_mut(&handle).map(|slot| slot.state = state));
}

/// Gives webview `handle` vsn's find widget (`enableFindWidget`).
pub fn set_find(handle: u64, on: bool) {
    STATE.with_borrow_mut(|s| s.slots.get_mut(&handle).map(|slot| slot.find = on));
}

/// Finds `term` in webview `handle`'s page, highlighting every match, or
/// steps to the next (`back`: previous) match when it's the term already
/// found. WebView2's own find bar stays hidden.
pub fn find(handle: u64, term: &str, case: bool, back: bool) {
    let Some((webview, env, again, hooked)) = STATE.with_borrow_mut(|s| {
        let env = s.env.clone()?;
        let slot = s.slots.get_mut(&handle)?;
        let key = Some((term.to_string(), case));
        let again = slot.finding == key;
        slot.finding = key;
        let hooked = std::mem::replace(&mut slot.find_hooked, true);
        Some((slot.webview.clone()?, env, again, hooked))
    }) else {
        return;
    };
    let Ok(find) = webview.cast::<ICoreWebView2_28>().and_then(|w| unsafe { w.Find() }) else {
        return;
    };
    if !hooked {
        let report = move |find: Option<ICoreWebView2Find>| {
            let (mut active, mut count) = (-1, 0);
            if let Some(find) = find {
                unsafe { find.ActiveMatchIndex(&mut active).ok() };
                unsafe { find.MatchCount(&mut count).ok() };
            }
            emit(Event::Find(handle, active, count));
        };
        let changed = FindActiveMatchIndexChangedEventHandler::create(Box::new(move |f, _| {
            report(f);
            Ok(())
        }));
        let counted = FindMatchCountChangedEventHandler::create(Box::new(move |f, _| {
            report(f);
            Ok(())
        }));
        let mut token = 0;
        unsafe {
            find.add_ActiveMatchIndexChanged(&changed, &mut token).ok();
            find.add_MatchCountChanged(&counted, &mut token).ok();
        }
    }
    if again {
        unsafe { if back { find.FindPrevious() } else { find.FindNext() } }.ok();
        return;
    }
    let Ok(options) = env.cast::<ICoreWebView2Environment15>().and_then(|e| unsafe { e.CreateFindOptions() })
    else {
        return;
    };
    let term = CoTaskMemPWSTR::from(term);
    unsafe {
        options.SetFindTerm(*term.as_ref().as_pcwstr()).ok();
        options.SetIsCaseSensitive(case).ok();
        options.SetSuppressDefaultFindDialog(true).ok();
        options.SetShouldHighlightAllMatches(true).ok();
        let done = FindStartCompletedHandler::create(Box::new(|_| Ok(())));
        find.Start(&options, &done).ok();
    }
}

/// Ends find in webview `handle`'s page, clearing its highlights.
pub fn stop_find(handle: u64) {
    let webview = STATE.with_borrow_mut(|s| {
        let slot = s.slots.get_mut(&handle)?;
        slot.finding = None;
        slot.webview.clone()
    });
    if let Some(find) = webview.and_then(|w| w.cast::<ICoreWebView2_28>().ok()).and_then(|w| unsafe { w.Find() }.ok()) {
        unsafe { find.Stop() }.ok();
    }
}

pub fn dispose(handle: u64) {
    let Some(slot) = STATE.with_borrow_mut(|s| s.slots.remove(&handle)) else {
        return;
    };
    if slot.hovered {
        gpui_windows::set_cursor_override(None);
    }
    if let Some(controller) = slot.controller {
        unsafe { controller.Close() }.ok();
    }
    if let Some((visual, layer)) = slot.visual {
        unsafe { layer.RemoveVisual(&visual) }.ok();
    }
}

