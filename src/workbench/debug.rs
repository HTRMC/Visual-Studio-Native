//! Run and Debug: a Debug Adapter Protocol client over the adapter's stdio or
//! a socket, launch.json, breakpoints in the glyph margin, the Run and Debug
//! panes, the Debug Console and the floating debug toolbar.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufRead as _, BufReader, Read, Write},
    net::{Shutdown, TcpStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
};

use gpui_kit::component::{
    input::{Position, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle},
    menu::PopupMenuItem,
};
use regex::Regex;
use serde_json::{Value, json};

use super::*;
use crate::theme::{c, ca, tip};

pub(super) const DEBUG_CONSOLE: usize = 2;
/// Width of the glyph margin left of the line numbers.
const GLYPH_MARGIN: f32 = 18.;
/// The debug toolbar: gripper, six 22px buttons, 4px gaps and padding.
const TOOLBAR_W: f32 = 2. + 16. + 6. * 22. + 6. * 4. + 4.;
const TOOLBAR_H: f32 = 28.;
const REPL_IDLE: &str = "Please start a debug session to evaluate expressions";

// VS Code's debug colors (dark defaults).
const BREAKPOINT: u32 = 0xE51400;
const BREAKPOINT_UNVERIFIED: u32 = 0x848484;
const STACKFRAME: u32 = 0xFFCC00;
const FOCUSED_STACKFRAME: u32 = 0x89D185;
const STACKFRAME_LINE: u32 = 0xFFFF0033;
const FOCUSED_STACKFRAME_LINE: u32 = 0x7ABD7A4D;
const ICON_GREEN: u32 = 0x89D185;
const ICON_BLUE: u32 = 0x75BEFF;
const ICON_RED: u32 = 0xF48771;
const ERROR: u32 = 0xF48771;
const INFO: u32 = 0x59A4F9;
/// statusBar.debuggingBackground in Dark 2026.
pub(super) const STATUS_DEBUGGING: u32 = 0x3994BC;

type Then =
    Box<dyn FnOnce(&mut Workbench, Result<Value, String>, &mut Window, &mut Context<Workbench>)>;

pub struct Debug {
    /// 0-based breakpoint lines per file, sorted.
    breakpoints: BTreeMap<PathBuf, Vec<Breakpoint>>,
    /// Index into launch.json's configurations.
    selected: usize,
    /// launch.json's configurations, until the file watcher sees it change.
    launch: Option<Option<Vec<Value>>>,
    session: Option<Session>,
    sessions: u64,
    /// Debug Console lines, each ending in `\n` unless more output may follow.
    console: Vec<(String, Hsla)>,
    console_list: ListState,
    repl: Entity<InputState>,
    /// The glyph margin line under the mouse, where VS Code hints a breakpoint.
    hover: Option<u32>,
    /// Watch expressions, and the row being edited (`watch.len()` adds one).
    watch: Vec<String>,
    watch_input: Entity<InputState>,
    watch_edit: Option<usize>,
    /// The toolbar's center as a fraction of the window width, and its top.
    toolbar: (f32, f32),
    /// The mouse and toolbar positions where a gripper drag started.
    drag: Option<(Point<Pixels>, (f32, f32))>,
}

struct Breakpoint {
    line: u32,
    id: Option<i64>,
    verified: bool,
    /// Follows the line through edits while its file is open.
    deco: Option<RangeDecorationCollection>,
}

impl Breakpoint {
    fn new(line: u32) -> Self {
        Self {
            line,
            id: None,
            verified: false,
            deco: None,
        }
    }
}

struct Session {
    id: u64,
    name: String,
    /// The configuration as sent to the adapter, for the restart request.
    config: Value,
    no_debug: bool,
    /// None when the adapter is a debugServer someone else started.
    child: Option<Child>,
    /// Framed messages to the writer thread; dropping it closes the connection.
    out: Option<mpsc::Sender<String>>,
    seq: i64,
    /// Reply handlers, and whether a failure goes to the Debug Console.
    pending: HashMap<i64, (bool, Then)>,
    caps: Value,
    stop: Option<Stop>,
    /// Terminate or disconnect went out; a second Stop kills the adapter.
    ending: bool,
    /// Launch again once this session ends.
    restart: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Windows kills a debuggee whose debugger dies.
        if let Some(child) = &mut self.child {
            child.kill().ok();
            child.wait().ok();
        }
    }
}

/// Where the debuggee is paused.
struct Stop {
    thread: i64,
    reason: String,
    frames: Vec<Value>,
    /// The focused frame.
    frame: usize,
    scopes: Vec<Value>,
    /// Children of each fetched `variablesReference`, and which are expanded.
    vars: HashMap<i64, Vec<Value>>,
    open: HashSet<i64>,
    /// Watch expressions' evaluate results in this frame.
    watch: HashMap<String, Result<Value, String>>,
}

enum Msg {
    Dap(Value),
    /// Debuggee output that shares the adapter's stdout.
    Out(String),
    Err(String),
    /// The adapter's socket couldn't be reached.
    Fail(String),
}

/// How to reach a configuration's debug adapter.
#[derive(Debug, PartialEq)]
enum Adapter {
    /// Speaks DAP on its stdio.
    Stdio(String, Vec<String>),
    /// Already listening, as VS Code's `debugServer` attribute says.
    Tcp(u16),
    /// Prints the address it listens on (Delve's `dap --listen`).
    Listen(String, Vec<String>),
}

impl Debug {
    pub(super) fn new(window: &mut Window, cx: &mut App) -> Self {
        let mut state: Value = state_path()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let breakpoints = (state["breakpoints"].as_object().into_iter().flatten())
            .map(|(path, lines)| {
                let lines = (lines.as_array().into_iter().flatten())
                    .filter_map(|l| l.as_u64())
                    .map(|line| Breakpoint::new(line as u32));
                (PathBuf::from(path), lines.collect())
            })
            .collect();
        let console_list = ListState::new(0, ListAlignment::Top, px(200.));
        console_list.set_follow_mode(FollowMode::Tail);
        Self {
            breakpoints,
            selected: 0,
            launch: None,
            session: None,
            sessions: 0,
            console: Vec::new(),
            console_list,
            repl: cx.new(|cx| InputState::new(window, cx).placeholder(REPL_IDLE)),
            hover: None,
            watch: serde_json::from_value(state["watch"].take()).unwrap_or_default(),
            watch_input: cx
                .new(|cx| InputState::new(window, cx).placeholder("Expression to watch")),
            watch_edit: None,
            toolbar: serde_json::from_value(state["toolbar"].take()).unwrap_or((0.5, 0.)),
            drag: None,
        }
    }

    fn save(&self) {
        let Some(path) = state_path() else { return };
        let breakpoints: serde_json::Map<String, Value> = (self.breakpoints.iter())
            .map(|(p, bps)| {
                let lines = bps.iter().map(|b| b.line).collect::<Vec<_>>();
                (p.to_string_lossy().into_owned(), json!(lines))
            })
            .collect();
        let state = json!({
            "breakpoints": breakpoints,
            "watch": self.watch,
            "toolbar": self.toolbar,
        });
        fs::create_dir_all(path.parent().unwrap()).ok();
        fs::write(path, state.to_string()).ok();
    }

    /// Appends adapter output, continuing an unfinished line of the same color.
    fn print(&mut self, text: &str, color: Hsla) {
        for part in text.split_inclusive('\n') {
            let n = self.console.len();
            match self.console.last_mut() {
                Some((line, c)) if !line.ends_with('\n') && *c == color => {
                    line.push_str(part);
                    self.console_list.splice(n - 1..n, 1);
                }
                _ => {
                    self.console.push((part.to_string(), color));
                    self.console_list.splice(n..n, 1);
                }
            }
        }
    }

    fn clear_console(&mut self) {
        self.console.clear();
        self.console_list.reset(0);
        self.console_list.set_follow_mode(FollowMode::Tail);
    }

    fn stop(&self) -> Option<&Stop> {
        self.session.as_ref()?.stop.as_ref()
    }

    fn stop_mut(&mut self) -> Option<&mut Stop> {
        self.session.as_mut()?.stop.as_mut()
    }
}

/// Breakpoints, watches and the toolbar's spot survive restarts, as in VS
/// Code's workspace storage.
fn state_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("vsn").join("debug.json"))
}

impl Workbench {
    pub(super) fn init_debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repl = self.debug.repl.clone();
        cx.subscribe_in(&repl, window, |this, input, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let expr = input.read(cx).value().trim().to_string();
                if !expr.is_empty() && this.debug.session.is_some() {
                    input.update(cx, |i, cx| i.set_value("", window, cx));
                    this.evaluate(expr, cx);
                }
            }
        })
        .detach();
        let watch = self.debug.watch_input.clone();
        cx.subscribe(&watch, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.commit_watch(cx);
            }
        })
        .detach();
    }

    /// launch.json's configurations, read once per change on disk.
    fn launch_configs(&mut self) -> Option<&Vec<Value>> {
        let root = self.root.as_deref();
        (self.debug.launch)
            .get_or_insert_with(|| root.and_then(launch_configs))
            .as_ref()
    }

    /// The file watcher saw launch.json change, or the folder changed.
    pub(super) fn launch_changed(&mut self) {
        self.debug.launch = None;
    }

    // ---- session -------------------------------------------------------------

    /// F5: continue when paused, else launch the selected configuration.
    pub(super) fn start_debugging(
        &mut self,
        no_debug: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(s) = &self.debug.session {
            if !no_debug && s.stop.is_some() {
                self.resume("continue", cx);
            }
            return;
        }
        // VS Code picks a debugger for the active file without a launch.json;
        // gdb on the executable built next to it is the one here.
        let selected = self.debug.selected;
        let config = match self.launch_configs() {
            Some(configs) => configs.get(selected).or(configs.first()).cloned(),
            None => Some(default_config()),
        };
        let Some(mut config) = config else {
            self.sidebar = true;
            self.view = View::Run;
            cx.notify();
            return;
        };
        substitute(&mut config, &self.variables(cx));
        if no_debug {
            config["noDebug"] = true.into();
        }
        self.launch(config, no_debug, window, cx);
    }

    fn launch(
        &mut self,
        mut config: Value,
        no_debug: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.debug.clear_console();
        self.panel = true;
        self.panel_tab = DEBUG_CONSOLE;
        cx.notify();
        let name = config["name"].as_str().unwrap_or_default().to_string();
        let ty = config["type"].as_str().unwrap_or_default().to_string();
        let adapter = match adapter(&mut config) {
            Ok(adapter) => adapter,
            Err(err) => return self.debug.print(&format!("{err}\n"), c(ERROR)),
        };
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let (out, out_rx) = mpsc::channel::<String>();
        let child = match adapter {
            Adapter::Tcp(port) => {
                connect(format!("127.0.0.1:{port}"), out_rx, tx);
                None
            }
            Adapter::Stdio(program, args) => Some(spawn_adapter(program, args, false, out_rx, tx)),
            Adapter::Listen(program, args) => Some(spawn_adapter(program, args, true, out_rx, tx)),
        };
        let child = match child.transpose() {
            Ok(child) => child,
            Err(err) => return self.debug.print(&err, c(ERROR)),
        };
        self.debug.sessions += 1;
        let id = self.debug.sessions;
        cx.spawn_in(window, async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let msg = this.update_in(cx, |this, window, cx| this.on_dap(id, msg, window, cx));
                if msg.is_err() {
                    return;
                }
            }
            this.update_in(cx, |this, window, cx| this.end_session(id, window, cx))
                .ok();
        })
        .detach();

        let request = match config["request"].as_str() {
            Some("attach") => "attach",
            _ => "launch",
        };
        self.debug.session = Some(Session {
            id,
            name,
            config: config.clone(),
            no_debug,
            child,
            out: Some(out),
            seq: 0,
            pending: HashMap::new(),
            caps: Value::Null,
            stop: None,
            ending: false,
            restart: false,
        });
        (self.debug.repl).update(cx, |i, cx| i.set_placeholder("", window, cx));
        let init = json!({
            "clientID": "vscode",
            "clientName": "Visual Studio Code Native",
            "adapterID": ty,
            "pathFormat": "path",
            "linesStartAt1": true,
            "columnsStartAt1": true,
            "locale": "en",
            "supportsVariableType": true,
        });
        self.dap("initialize", init, move |this, caps, _, _| {
            let Ok(caps) = caps else {
                return this.kill_adapter();
            };
            if let Some(s) = &mut this.debug.session {
                s.caps = caps;
            }
            // gdb answers launch only after configurationDone, which waits
            // for the `initialized` event, so nothing chains on this reply.
            this.dap(request, config, |this, res, _, _| {
                if res.is_err() {
                    this.kill_adapter();
                }
            });
        });
    }

    /// Sends a request; `then` gets its body, or the error already printed.
    fn dap(
        &mut self,
        command: &str,
        arguments: Value,
        then: impl FnOnce(&mut Workbench, Result<Value, String>, &mut Window, &mut Context<Workbench>)
        + 'static,
    ) {
        self.request(command, arguments, true, then);
    }

    /// `dap`, printing a failure to the Debug Console only when `report`.
    fn request(
        &mut self,
        command: &str,
        arguments: Value,
        report: bool,
        then: impl FnOnce(&mut Workbench, Result<Value, String>, &mut Window, &mut Context<Workbench>)
        + 'static,
    ) {
        let Some(s) = &mut self.debug.session else {
            return;
        };
        s.seq += 1;
        s.pending.insert(s.seq, (report, Box::new(then)));
        let msg =
            json!({ "seq": s.seq, "type": "request", "command": command, "arguments": arguments });
        send(&s.out, &msg);
    }

    fn on_dap(&mut self, id: u64, msg: Msg, window: &mut Window, cx: &mut Context<Self>) {
        if self.debug.session.as_ref().is_none_or(|s| s.id != id) {
            return;
        }
        cx.notify();
        let mut msg = match msg {
            Msg::Dap(msg) => msg,
            Msg::Out(text) => return self.debug.print(&text, col::fg()),
            Msg::Err(text) => return self.debug.print(&text, c(ERROR)),
            Msg::Fail(text) => {
                self.debug.print(&text, c(ERROR));
                return self.kill_adapter();
            }
        };
        let body = msg["body"].take();
        match msg["type"].as_str() {
            Some("response") => {
                let seq = msg["request_seq"].as_i64().unwrap_or_default();
                let Some((report, then)) = self
                    .debug
                    .session
                    .as_mut()
                    .and_then(|s| s.pending.remove(&seq))
                else {
                    return;
                };
                if msg["success"] == true {
                    then(self, Ok(body), window, cx);
                } else {
                    let err = (body["error"]["format"].as_str())
                        .or(msg["message"].as_str())
                        .unwrap_or("Unknown error")
                        .to_string();
                    if report {
                        self.debug.print(&format!("{err}\n"), c(ERROR));
                    }
                    then(self, Err(err), window, cx);
                }
            }
            Some("event") => {
                self.on_event(msg["event"].as_str().unwrap_or_default(), body, window, cx)
            }
            // Reverse requests (runInTerminal, startDebugging) aren't offered in initialize.
            Some("request") => {
                if let Some(s) = &mut self.debug.session {
                    s.seq += 1;
                    let reply = json!({
                        "seq": s.seq, "type": "response", "request_seq": msg["seq"],
                        "command": msg["command"], "success": false, "message": "not supported",
                    });
                    send(&s.out, &reply);
                }
            }
            _ => {}
        }
    }

    fn on_event(&mut self, event: &str, body: Value, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            "initialized" => self.configure(),
            "output" => {
                let color = match body["category"].as_str() {
                    Some("telemetry") => return,
                    Some("stderr") => c(ERROR),
                    Some("important") => c(INFO),
                    _ => col::fg(),
                };
                self.debug
                    .print(body["output"].as_str().unwrap_or_default(), color);
            }
            "stopped" => {
                let reason = body["reason"].as_str().unwrap_or_default().to_string();
                match body["threadId"].as_i64() {
                    Some(thread) => self.load_stack(thread, reason),
                    None => self.dap("threads", json!({}), |this, res, _, _| {
                        if let Some(thread) = res.ok().and_then(|b| b["threads"][0]["id"].as_i64())
                        {
                            this.load_stack(thread, reason);
                        }
                    }),
                }
            }
            "continued" => {
                if let Some(s) = &mut self.debug.session {
                    s.stop = None;
                }
            }
            "breakpoint" => {
                let bp = &body["breakpoint"];
                let found = (self.debug.breakpoints.values_mut().flatten())
                    .find(|b| b.id.is_some() && b.id == bp["id"].as_i64());
                if let Some(b) = found {
                    b.verified = bp["verified"] == true;
                }
            }
            "terminated" => self.dap(
                "disconnect",
                json!({ "terminateDebuggee": true }),
                |this, _, _, _| this.kill_adapter(),
            ),
            _ => {}
        }
        let _ = (window, cx);
    }

    /// The `initialized` event: breakpoints, exception filters, configurationDone.
    fn configure(&mut self) {
        let Some(s) = &self.debug.session else { return };
        let caps = s.caps.clone();
        if !s.no_debug {
            let paths: Vec<PathBuf> = self.debug.breakpoints.keys().cloned().collect();
            for path in paths {
                self.send_breakpoints(&path);
            }
            if let Some(filters) = caps["exceptionBreakpointFilters"].as_array() {
                let on: Vec<&Value> = (filters.iter())
                    .filter(|f| f["default"] == true)
                    .map(|f| &f["filter"])
                    .collect();
                self.dap(
                    "setExceptionBreakpoints",
                    json!({ "filters": on }),
                    |_, _, _, _| {},
                );
            }
        }
        if caps["supportsConfigurationDoneRequest"] == true {
            self.dap("configurationDone", json!({}), |_, _, _, _| {});
        }
    }

    fn send_breakpoints(&mut self, path: &Path) {
        if self.debug.session.as_ref().is_none_or(|s| s.no_debug) {
            return;
        }
        let lines: Vec<u32> = (self.debug.breakpoints.get(path).into_iter().flatten())
            .map(|b| b.line)
            .collect();
        let args = json!({
            "source": { "path": path, "name": path.file_name().map(|n| n.to_string_lossy()) },
            "breakpoints": lines.iter().map(|l| json!({ "line": l + 1 })).collect::<Vec<_>>(),
            "lines": lines.iter().map(|l| l + 1).collect::<Vec<_>>(),
            "sourceModified": false,
        });
        let path = path.to_path_buf();
        self.dap("setBreakpoints", args, move |this, res, _, _| {
            let (Ok(body), Some(bps)) = (res, this.debug.breakpoints.get_mut(&path)) else {
                return;
            };
            let Some(sent) = body["breakpoints"]
                .as_array()
                .filter(|b| b.len() == bps.len())
            else {
                // Toggled again meanwhile; that request's reply updates them.
                return;
            };
            for (bp, b) in bps.iter_mut().zip(sent) {
                bp.id = b["id"].as_i64();
                bp.verified = b["verified"] == true;
            }
        });
    }

    fn load_stack(&mut self, thread: i64, reason: String) {
        let args = json!({ "threadId": thread, "startFrame": 0, "levels": 20 });
        self.dap("stackTrace", args, move |this, res, window, cx| {
            let (Ok(mut body), Some(s)) = (res, &mut this.debug.session) else {
                return;
            };
            let frames = body["stackFrames"].take();
            let frames = frames.as_array().cloned().unwrap_or_default();
            // VS Code focuses the top frame that has source.
            let frame = (frames.iter())
                .position(|f| f["source"]["path"].is_string())
                .unwrap_or(0);
            s.stop = Some(Stop {
                thread,
                reason,
                frames,
                frame,
                scopes: Vec::new(),
                vars: HashMap::new(),
                open: HashSet::new(),
                watch: HashMap::new(),
            });
            this.sidebar = true;
            this.view = View::Run;
            window.activate_window();
            this.focus_frame(frame, window, cx);
        });
    }

    /// Shows a frame's line and loads its variables.
    fn focus_frame(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(stop) = self.debug.stop_mut() else {
            return;
        };
        let Some(frame) = stop.frames.get(ix).cloned() else {
            return;
        };
        stop.frame = ix;
        stop.scopes.clear();
        stop.vars.clear();
        stop.open.clear();
        stop.watch.clear();
        self.evaluate_watches();
        if let Some(path) = frame["source"]["path"].as_str() {
            self.open_path(PathBuf::from(path), window, cx);
            let line = frame["line"].as_u64().unwrap_or(1).saturating_sub(1) as u32;
            let col = frame["column"].as_u64().unwrap_or(1).saturating_sub(1) as u32;
            if let Some(doc) = self.docs.get(self.active) {
                reveal(&doc.editor.clone(), Position::new(line, col), window, cx);
            }
        }
        self.dap(
            "scopes",
            json!({ "frameId": frame["id"] }),
            |this, res, _, cx| {
                let (Ok(mut body), Some(stop)) = (res, this.debug.stop_mut()) else {
                    return;
                };
                stop.scopes = body["scopes"]
                    .take()
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                // VS Code expands the first inexpensive scope.
                let first = (stop.scopes.iter())
                    .find(|s| s["expensive"] != true)
                    .and_then(|s| s["variablesReference"].as_i64());
                if let Some(r) = first {
                    this.toggle_variable(r, cx);
                }
            },
        );
    }

    fn toggle_variable(&mut self, r: i64, cx: &mut Context<Self>) {
        let Some(stop) = self.debug.stop_mut() else {
            return;
        };
        cx.notify();
        if !stop.open.insert(r) {
            stop.open.remove(&r);
            return;
        }
        if stop.vars.contains_key(&r) {
            return;
        }
        self.dap(
            "variables",
            json!({ "variablesReference": r }),
            move |this, res, _, _| {
                if let (Ok(mut body), Some(stop)) = (res, this.debug.stop_mut()) {
                    let vars = body["variables"].take();
                    stop.vars
                        .insert(r, vars.as_array().cloned().unwrap_or_default());
                }
            },
        );
    }

    fn evaluate(&mut self, expr: String, cx: &mut Context<Self>) {
        self.debug.print(&format!("{expr}\n"), col::fg());
        let mut args = json!({ "expression": expr, "context": "repl" });
        if let Some(stop) = self.debug.stop() {
            args["frameId"] = stop.frames[stop.frame]["id"].clone();
        }
        self.dap("evaluate", args, |this, res, _, _| {
            if let Ok(body) = res {
                let result = body["result"].as_str().unwrap_or_default();
                this.debug.print(&format!("{result}\n"), col::fg());
            }
        });
        cx.notify();
    }

    /// Evaluates the watch expressions in the focused frame.
    fn evaluate_watches(&mut self) {
        let Some(stop) = self.debug.stop() else {
            return;
        };
        let frame = stop.frames[stop.frame]["id"].clone();
        for expr in self.debug.watch.clone() {
            let args = json!({ "expression": expr, "frameId": frame, "context": "watch" });
            let frame = frame.clone();
            self.request("evaluate", args, false, move |this, res, _, _| {
                // Stepped meanwhile: the new frame's own request answers.
                if let Some(stop) = this.debug.stop_mut()
                    && stop.frames[stop.frame]["id"] == frame
                {
                    stop.watch.insert(expr, res);
                }
            });
        }
    }

    /// Opens the inline editor on watch row `ix`; `watch.len()` adds one.
    fn edit_watch(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.debug.watch.get(ix).cloned().unwrap_or_default();
        self.debug.watch_edit = Some(ix);
        self.debug.watch_input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Enter or focus loss: an empty expression removes the row.
    fn commit_watch(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.debug.watch_edit.take() else {
            return;
        };
        let expr = self.debug.watch_input.read(cx).value().trim().to_string();
        let watch = &mut self.debug.watch;
        match (ix < watch.len(), expr.is_empty()) {
            (true, true) => drop(watch.remove(ix)),
            (true, false) => watch[ix] = expr,
            (false, false) => watch.push(expr),
            (false, true) => {}
        }
        self.debug.save();
        self.evaluate_watches();
        cx.notify();
    }

    fn remove_watch(&mut self, ix: Option<usize>, cx: &mut Context<Self>) {
        match ix {
            Some(ix) => drop(self.debug.watch.remove(ix)),
            None => self.debug.watch.clear(),
        }
        self.debug.watch_edit = None;
        self.debug.save();
        cx.notify();
    }

    /// continue / next / stepIn / stepOut on the paused thread.
    pub(super) fn resume(&mut self, command: &str, cx: &mut Context<Self>) {
        let Some(stop) = self.debug.session.as_mut().and_then(|s| s.stop.take()) else {
            return;
        };
        self.dap(command, json!({ "threadId": stop.thread }), |_, _, _, _| {});
        cx.notify();
    }

    pub(super) fn pause(&mut self, cx: &mut Context<Self>) {
        if self.debug.session.as_ref().is_none_or(|s| s.stop.is_some()) {
            return;
        }
        self.dap("threads", json!({}), |this, res, _, _| {
            if let Some(thread) = res.ok().and_then(|b| b["threads"][0]["id"].as_i64()) {
                this.dap("pause", json!({ "threadId": thread }), |_, _, _, _| {});
            }
        });
        cx.notify();
    }

    /// Shift+F5: terminate (or disconnect) first, kill the adapter when pressed again.
    pub(super) fn stop_debugging(&mut self, cx: &mut Context<Self>) {
        let Some(s) = &mut self.debug.session else {
            return;
        };
        if s.ending {
            return self.kill_adapter();
        }
        s.ending = true;
        let command = match s.caps["supportsTerminateRequest"] == true {
            true => "terminate",
            false => "disconnect",
        };
        self.dap(
            command,
            json!({ "terminateDebuggee": true }),
            |_, _, _, _| {},
        );
        cx.notify();
    }

    pub(super) fn restart_debugging(&mut self, cx: &mut Context<Self>) {
        let Some(s) = &mut self.debug.session else {
            return;
        };
        if s.caps["supportsRestartRequest"] == true {
            s.stop = None;
            let args = json!({ "arguments": s.config });
            self.dap("restart", args, |_, _, _, _| {});
        } else {
            s.restart = true;
            self.stop_debugging(cx);
        }
        cx.notify();
    }

    /// Its end of stream then ends the session.
    fn kill_adapter(&mut self) {
        if let Some(s) = &mut self.debug.session {
            s.out = None;
            if let Some(child) = &mut s.child {
                child.kill().ok();
            }
        }
    }

    fn end_session(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.debug.session.as_ref().is_none_or(|s| s.id != id) {
            return;
        }
        let s = self.debug.session.take().unwrap();
        for b in self.debug.breakpoints.values_mut().flatten() {
            b.id = None;
            b.verified = false;
        }
        (self.debug.repl).update(cx, |i, cx| i.set_placeholder(REPL_IDLE, window, cx));
        if s.restart {
            let no_debug = s.no_debug;
            drop(s);
            self.start_debugging(no_debug, window, cx);
        }
        cx.notify();
    }

    // ---- breakpoints ---------------------------------------------------------

    /// F9 (cursor line) or a glyph margin click (`line`).
    pub(super) fn toggle_breakpoint(&mut self, line: Option<u32>, cx: &mut Context<Self>) {
        let Some(doc) = self
            .docs
            .get(self.active)
            .filter(|d| !d.untitled && !d.binary)
        else {
            return;
        };
        let line = line.unwrap_or_else(|| doc.editor.read(cx).cursor_position().line);
        let path = doc.path.clone();
        let editor = doc.editor.clone();
        let bps = self.debug.breakpoints.entry(path.clone()).or_default();
        match bps.iter().position(|b| b.line == line) {
            Some(ix) => {
                if let Some(deco) = bps.remove(ix).deco {
                    deco.dispose(cx);
                }
            }
            None => {
                let ix = bps.partition_point(|b| b.line < line);
                let deco = track_line(&editor, line, cx);
                bps.insert(
                    ix,
                    Breakpoint {
                        deco,
                        ..Breakpoint::new(line)
                    },
                );
            }
        }
        if bps.is_empty() {
            self.debug.breakpoints.remove(&path);
        }
        self.debug.save();
        self.send_breakpoints(&path);
        cx.notify();
    }

    /// A file opened: its breakpoints follow its edits from now on.
    pub(super) fn attach_breakpoints(
        &mut self,
        path: &Path,
        editor: &Entity<EditorState>,
        cx: &mut Context<Self>,
    ) {
        for b in self.debug.breakpoints.get_mut(path).into_iter().flatten() {
            b.deco = track_line(editor, b.line, cx);
        }
    }

    /// An edit in `path`: breakpoints move with their lines, as in VS Code.
    pub(super) fn breakpoints_moved(
        &mut self,
        path: &Path,
        editor: &Entity<EditorState>,
        cx: &mut Context<Self>,
    ) {
        let Some(bps) = self.debug.breakpoints.get_mut(path) else {
            return;
        };
        let mut moved = false;
        for b in bps.iter_mut() {
            let Some(deco) = &b.deco else { continue };
            let ranges = deco.get_ranges(cx);
            let text = editor.read(cx).text();
            let line = match ranges.first() {
                Some(r) => text.offset_to_position(r.start).line,
                // Its whole line went: it stays where the text closed up.
                None => b.line.min(text.lines_len().saturating_sub(1) as u32),
            };
            if ranges.is_empty() {
                deco.dispose(cx);
                b.deco = track_line(editor, line, cx);
            }
            moved |= b.line != line;
            b.line = line;
        }
        if !moved {
            return;
        }
        bps.sort_by_key(|b| b.line);
        bps.dedup_by(|b, kept| {
            let same = b.line == kept.line;
            if let Some(deco) = b.deco.as_ref().filter(|_| same) {
                deco.dispose(cx);
            }
            same
        });
        self.debug.save();
        self.send_breakpoints(path);
    }

    /// Values for launch.json's `${...}` variables.
    fn variables(&self, cx: &App) -> HashMap<&'static str, String> {
        let root = self.root.as_deref();
        let doc = self.docs.get(self.active).filter(|d| !d.untitled);
        let file = doc.map(|d| d.path.as_path());
        let s = |p: Option<&Path>| p.map(|p| p.to_string_lossy().into_owned());
        let name = |p: Option<&std::ffi::OsStr>| p.map(|p| p.to_string_lossy().into_owned());
        let pairs = [
            ("workspaceFolder", s(root)),
            ("workspaceRoot", s(root)),
            (
                "workspaceFolderBasename",
                name(root.and_then(Path::file_name)),
            ),
            ("file", s(file)),
            ("fileBasename", name(file.and_then(Path::file_name))),
            (
                "fileBasenameNoExtension",
                name(file.and_then(Path::file_stem)),
            ),
            (
                "fileExtname",
                name(file.and_then(Path::extension)).map(|e| format!(".{e}")),
            ),
            ("fileDirname", s(file.and_then(Path::parent))),
            (
                "fileDirnameBasename",
                name(file.and_then(Path::parent).and_then(Path::file_name)),
            ),
            (
                "relativeFile",
                s(file.zip(root).and_then(|(f, r)| f.strip_prefix(r).ok())),
            ),
            (
                "relativeFileDirname",
                s(file
                    .and_then(Path::parent)
                    .zip(root)
                    .and_then(|(f, r)| f.strip_prefix(r).ok())),
            ),
            ("cwd", s(std::env::current_dir().ok().as_deref())),
            ("userHome", s(dirs::home_dir().as_deref())),
            (
                "lineNumber",
                doc.map(|d| (d.editor.read(cx).cursor_position().line + 1).to_string()),
            ),
            ("pathSeparator", Some(std::path::MAIN_SEPARATOR.to_string())),
            ("/", Some(std::path::MAIN_SEPARATOR.to_string())),
        ];
        pairs
            .into_iter()
            .filter_map(|(k, v)| Some((k, v?)))
            .collect()
    }

    /// Opens the workspace's launch.json (the Run view's gear, Run > Open
    /// Configurations), first creating it with the comments VS Code writes.
    pub(super) fn create_launch_json(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = &self.root else { return };
        let path = root.join(".vscode").join("launch.json");
        if !path.exists() {
            let config = serde_json::to_string_pretty(&default_config()).unwrap();
            let config = config.replace('\n', "\n        ");
            let text = format!(
                "{{\n    // Use IntelliSense to learn about possible attributes.\n    // Hover to view descriptions of existing attributes.\n    // For more information, visit: https://go.microsoft.com/fwlink/?linkid=830387\n    \"version\": \"0.2.0\",\n    \"configurations\": [\n        {config}\n    ]\n}}\n"
            );
            fs::create_dir_all(path.parent().unwrap()).ok();
            if let Err(err) = fs::write(&path, text) {
                eprintln!("save {}: {err}", path.display());
                return;
            }
            self.launch_changed();
        }
        self.open_path(path, window, cx);
    }

    // ---- rendering -----------------------------------------------------------

    /// The editor with VS Code's glyph margin: breakpoints, the hint under the
    /// mouse and the paused frame's arrow and line highlight.
    pub(super) fn render_code(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let doc = &self.docs[ix];
        let editor = doc.editor.clone();
        let live = self.debug.session.as_ref().is_some_and(|s| !s.no_debug);
        let mut marks: Vec<(u32, char, Hsla)> =
            (self.debug.breakpoints.get(&doc.path).into_iter().flatten())
                .map(|b| match live && !b.verified {
                    true => (b.line, icon::CIRCLE_OUTLINE, c(BREAKPOINT_UNVERIFIED)),
                    false => (b.line, icon::CIRCLE_FILLED, c(BREAKPOINT)),
                })
                .collect();
        if let Some(line) = self
            .debug
            .hover
            .filter(|&l| !marks.iter().any(|m| m.0 == l))
        {
            marks.push((line, icon::CIRCLE_FILLED, c(BREAKPOINT).opacity(0.4)));
        }
        let frame = self.debug.stop().and_then(|stop| {
            let f = &stop.frames[stop.frame];
            let here = Path::new(f["source"]["path"].as_str()?) == doc.path;
            let line = f["line"].as_u64()?.checked_sub(1)? as u32;
            here.then_some((line, stop.frame == 0))
        });
        let mut highlight = None;
        if let Some((line, top)) = frame {
            let (arrow, back) = match top {
                true => (STACKFRAME, STACKFRAME_LINE),
                false => (FOCUSED_STACKFRAME, FOCUSED_STACKFRAME_LINE),
            };
            marks.push((line, icon::DEBUG_STACKFRAME, c(arrow)));
            highlight = Some((line, ca(back)));
        }

        let line_at = {
            let editor = editor.clone();
            move |y: Pixels, cx: &App| line_at(editor.read(cx), y)
        };
        let hover_line = line_at.clone();
        let paint_editor = editor.clone();
        div()
            .relative()
            .size_full()
            .child(
                h_flex()
                    .size_full()
                    .child(
                        div()
                            .id("glyph-margin")
                            .flex_none()
                            .w(px(GLYPH_MARGIN))
                            .h_full()
                            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                                let line = hover_line(ev.position.y, cx);
                                if this.debug.hover != line {
                                    this.debug.hover = line;
                                    cx.notify();
                                }
                            }))
                            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                                if !hovered {
                                    this.debug.hover = None;
                                    cx.notify();
                                }
                            }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                                    if let Some(line) = line_at(ev.position.y, cx) {
                                        this.toggle_breakpoint(Some(line), cx);
                                    }
                                }),
                            ),
                    )
                    .child(div().flex_1().min_w_0().h_full().child(
                        Editor::new(&editor).h_full().bordered(false).when(
                            exthost::has_menu("editor/context"),
                            |e| {
                                let path = doc.path.clone();
                                e.context_menu(move |menu, window, _| {
                                    exthost::editor_context_menu(menu, &path, window)
                                })
                            },
                        ),
                    )),
            )
            // Painted after the editor, so it reads this frame's layout.
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        let e = paint_editor.read(cx);
                        let Some(lh) = e.line_height() else { return };
                        let text = e.text();
                        let top_of = |line: u32| {
                            let line = line as usize;
                            if line >= text.lines_len() {
                                return None;
                            }
                            let start = text.line_start_offset(line);
                            e.range_to_bounds(&(start..start))
                        };
                        let quads: Vec<_> = (highlight
                            .and_then(|(line, color)| Some((top_of(line)?, color))))
                        .map(|(b, color)| {
                            fill(
                                Bounds::from_corners(b.origin, point(bounds.right(), b.top() + lh)),
                                color,
                            )
                        })
                        .into_iter()
                        .collect();
                        let glyphs: Vec<_> = (marks.iter())
                            .filter_map(|&(line, ch, color)| Some((top_of(line)?.top(), ch, color)))
                            .collect();
                        window.with_content_mask(Some(ContentMask { bounds }), |window| {
                            for quad in quads {
                                window.paint_quad(quad);
                            }
                            for (y, ch, color) in glyphs {
                                paint_glyph(ch, color, point(bounds.left(), y), lh, window, cx);
                            }
                        });
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .into_any_element()
    }

    /// The Run and Debug title bar's start button and configuration picker.
    pub(super) fn render_run_title(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let configs = self.launch_configs()?;
        let names: Vec<String> = (configs.iter())
            .map(|c| c["name"].as_str().unwrap_or_default().to_string())
            .collect();
        let selected = self.debug.selected.min(names.len().saturating_sub(1));
        let label = names
            .get(selected)
            .cloned()
            .unwrap_or("No Configurations".into());
        let wb = cx.weak_entity();
        Some(
            h_flex()
                .gap(px(2.))
                .child(
                    action_button("dbg-start", icon::DEBUG_START)
                        .text_color(c(ICON_GREEN))
                        .tooltip(tip("Start Debugging"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.start_debugging(false, window, cx)
                        })),
                )
                .child(
                    Button::new("dbg-config")
                        .ghost()
                        .compact()
                        .label(label)
                        .h(px(22.))
                        .text_size(px(12.))
                        .text_color(col::fg())
                        .dropdown_menu(move |mut menu, _, _| {
                            for (ix, name) in names.iter().enumerate() {
                                let wb = wb.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(name.clone())
                                        .checked(ix == selected)
                                        .on_click(move |_, _, cx| {
                                            wb.update(cx, |this, cx| {
                                                this.debug.selected = ix;
                                                cx.notify();
                                            })
                                            .ok();
                                        }),
                                );
                            }
                            menu
                        }),
                )
                .child(
                    action_button("dbg-gear", icon::GEAR)
                        .tooltip(tip("Open 'launch.json'"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.create_launch_json(window, cx)),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_run(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.launch_configs().is_none() && self.debug.session.is_none() {
            return self.render_run_welcome(cx);
        }
        let mut rows: Vec<AnyElement> =
            vec![section_header("dbg-vars", "Variables", true).into_any_element()];
        if let Some(stop) = self.debug.stop() {
            self.variable_rows(stop, &stop.scopes, 0, &mut rows, cx);
        }

        let header_button =
            |id: &'static str, ch: char| action_button(id, ch).font_weight(FontWeight::NORMAL);
        rows.push(
            section_header("dbg-watch", "Watch", true)
                .child(
                    h_flex()
                        .gap(px(2.))
                        .invisible()
                        .group_hover("dbg-watch-hdr", |s| s.visible())
                        .child(
                            header_button("dbg-watch-add", icon::ADD).on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    let ix = this.debug.watch.len();
                                    this.edit_watch(ix, window, cx);
                                },
                            )),
                        )
                        .child(
                            header_button("dbg-watch-collapse", icon::COLLAPSE_ALL).on_click(
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    if let Some(stop) = this.debug.stop_mut() {
                                        let refs: Vec<i64> = (stop.watch.values().flatten())
                                            .filter_map(|v| v["variablesReference"].as_i64())
                                            .collect();
                                        for r in refs {
                                            stop.open.remove(&r);
                                        }
                                    }
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(header_button("dbg-watch-clear", icon::CLOSE_ALL).on_click(
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove_watch(None, cx);
                            }),
                        )),
                )
                .into_any_element(),
        );
        self.watch_rows(&mut rows, cx);

        let state = self.debug.session.as_ref().map(|s| match &s.stop {
            Some(stop) if !stop.reason.is_empty() => format!("Paused on {}", stop.reason),
            Some(_) => "Paused".into(),
            None => "Running".into(),
        });
        rows.push(
            section_header("dbg-stack", "Call Stack", true)
                .when_some(state, |d, state| {
                    d.child(
                        div()
                            .px(px(4.))
                            .rounded(px(3.))
                            .bg(ca(0x88888844))
                            .text_size(px(10.))
                            .font_weight(FontWeight::NORMAL)
                            .child(state.to_uppercase()),
                    )
                })
                .into_any_element(),
        );
        for (ix, frame) in self
            .debug
            .stop()
            .into_iter()
            .flat_map(|s| s.frames.iter().enumerate())
        {
            let focused = self.debug.stop().is_some_and(|s| s.frame == ix);
            let file = frame["source"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let line = frame["line"].as_u64().unwrap_or_default();
            rows.push(
                h_flex()
                    .id(("dbg-frame", ix))
                    .h(px(22.))
                    .pl(px(20.))
                    .pr(px(12.))
                    .gap(px(6.))
                    .justify_between()
                    .cursor_pointer()
                    .when(focused, |d| d.bg(col::status_hover()))
                    .hover(|s| s.bg(col::status_hover()))
                    .child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(frame["name"].as_str().unwrap_or_default().to_string()),
                    )
                    .when(!file.is_empty(), |d| {
                        d.child(
                            h_flex()
                                .flex_none()
                                .gap(px(6.))
                                .text_color(col::fg_muted())
                                .child(file)
                                .child(
                                    div()
                                        .px(px(4.))
                                        .rounded(px(8.))
                                        .bg(ca(0x88888844))
                                        .text_size(px(11.))
                                        .child(line.to_string()),
                                ),
                        )
                    })
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.focus_frame(ix, window, cx)),
                    )
                    .into_any_element(),
            );
        }

        rows.push(section_header("dbg-bps", "Breakpoints", true).into_any_element());
        let root = self.root.clone();
        for (path, bps) in &self.debug.breakpoints {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let dir = path.parent().unwrap_or(path);
            let dir = root
                .as_deref()
                .and_then(|r| dir.strip_prefix(r).ok())
                .unwrap_or(dir);
            for b in bps {
                let path = path.clone();
                let line = b.line;
                rows.push(
                    h_flex()
                        .id(SharedString::from(format!(
                            "dbg-bp-{}:{line}",
                            path.display()
                        )))
                        .h(px(22.))
                        .pl(px(20.))
                        .pr(px(12.))
                        .gap(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(col::status_hover()))
                        .child(
                            codicon(icon::CIRCLE_FILLED)
                                .text_color(c(BREAKPOINT))
                                .text_size(px(12.)),
                        )
                        .child(name.clone())
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_size(px(12.))
                                .text_color(col::fg_muted())
                                .child(dir.display().to_string()),
                        )
                        .child(
                            div()
                                .px(px(4.))
                                .rounded(px(8.))
                                .bg(ca(0x88888844))
                                .text_size(px(11.))
                                .child((line + 1).to_string()),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_path(path.clone(), window, cx);
                            if let Some(doc) = this.docs.get(this.active) {
                                let at = Position::new(line, 0);
                                reveal(&doc.editor.clone(), at, window, cx);
                            }
                        }))
                        .into_any_element(),
                );
            }
        }
        div()
            .id("run-view")
            .size_full()
            .overflow_y_scroll()
            .child(v_flex().text_size(px(13.)).children(rows))
            .into_any_element()
    }

    fn variable_rows(
        &self,
        stop: &Stop,
        vars: &[Value],
        depth: usize,
        rows: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        for var in vars {
            let r = var["variablesReference"].as_i64().unwrap_or_default();
            let open = stop.open.contains(&r);
            // Scopes have a `name` and no `value`.
            let scope = depth == 0;
            let value = var["value"]
                .as_str()
                .or(var["result"].as_str())
                .unwrap_or_default()
                .to_string();
            rows.push(
                h_flex()
                    .id(SharedString::from(format!(
                        "dbg-var-{depth}-{r}-{}",
                        var["name"]
                    )))
                    .h(px(22.))
                    .pl(px(8. + 8. * depth as f32))
                    .pr(px(12.))
                    .cursor_pointer()
                    .hover(|s| s.bg(col::status_hover()))
                    .child(match r > 0 {
                        true => codicon(if open {
                            icon::CHEVRON_DOWN
                        } else {
                            icon::CHEVRON_RIGHT
                        })
                        .w(px(20.)),
                        false => div().flex_none().w(px(20.)),
                    })
                    .map(|d| match scope {
                        true => d.child(var["name"].as_str().unwrap_or_default().to_string()),
                        false => d
                            .font_family("Consolas")
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(
                                div()
                                    .text_color(c(0xC586C0))
                                    .child(var["name"].as_str().unwrap_or_default().to_string()),
                            )
                            .child(div().pr(px(6.)).child(" ="))
                            .child(div().text_color(value_color(&value)).child(value)),
                    })
                    .when(r > 0, |d| {
                        d.on_click(cx.listener(move |this, _, _, cx| this.toggle_variable(r, cx)))
                    })
                    .into_any_element(),
            );
            if let (true, Some(children)) = (open, stop.vars.get(&r)) {
                self.variable_rows(stop, children, depth + 1, rows, cx);
            }
        }
    }

    /// The Watch pane: `expression: value` rows that expand like variables.
    fn watch_rows(&self, rows: &mut Vec<AnyElement>, cx: &mut Context<Self>) {
        let stop = self.debug.stop();
        let editor = |id: usize| {
            h_flex()
                .id(("dbg-watch-edit", id))
                .h(px(22.))
                .pl(px(20.))
                .pr(px(12.))
                .child(
                    div().flex_1().border_1().border_color(col::accent()).child(
                        Input::new(&self.debug.watch_input)
                            .appearance(false)
                            .text_size(px(13.)),
                    ),
                )
                .into_any_element()
        };
        for (ix, expr) in self.debug.watch.iter().enumerate() {
            if self.debug.watch_edit == Some(ix) {
                rows.push(editor(ix));
                continue;
            }
            let result = stop.and_then(|s| s.watch.get(expr));
            let r = (result.and_then(|r| r.as_ref().ok()))
                .and_then(|v| v["variablesReference"].as_i64())
                .unwrap_or_default();
            let open = stop.is_some_and(|s| s.open.contains(&r));
            let value = match result {
                Some(Ok(v)) => {
                    let v = v["result"].as_str().unwrap_or_default().to_string();
                    Some((value_color(&v), v))
                }
                Some(Err(err)) => Some((c(ERROR), err.clone())),
                None if self.debug.session.is_some() => {
                    Some((col::fg_muted(), "not available".into()))
                }
                None => None,
            };
            rows.push(
                h_flex()
                    .id(("dbg-watch", ix))
                    .group("dbg-watch-row")
                    .h(px(22.))
                    .pl(px(8.))
                    .pr(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(col::status_hover()))
                    .font_family("Consolas")
                    .whitespace_nowrap()
                    .child(match r > 0 {
                        true => codicon(if open {
                            icon::CHEVRON_DOWN
                        } else {
                            icon::CHEVRON_RIGHT
                        })
                        .w(px(20.)),
                        false => div().flex_none().w(px(20.)),
                    })
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(div().text_color(c(0xC586C0)).child(expr.clone()))
                            .when_some(value, |d, (color, value)| {
                                d.child(div().pr(px(6.)).child(":"))
                                    .child(div().text_color(color).child(value))
                            }),
                    )
                    .child(
                        action_button(("dbg-watch-rm", ix), icon::CLOSE)
                            .invisible()
                            .group_hover("dbg-watch-row", |s| s.visible())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove_watch(Some(ix), cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                        match ev.click_count() {
                            2 => this.edit_watch(ix, window, cx),
                            _ if r > 0 => this.toggle_variable(r, cx),
                            _ => {}
                        }
                    }))
                    .into_any_element(),
            );
            if let (true, Some(stop)) = (open, stop)
                && let Some(children) = stop.vars.get(&r)
            {
                self.variable_rows(stop, children, 1, rows, cx);
            }
        }
        if self.debug.watch_edit == Some(self.debug.watch.len()) {
            rows.push(editor(self.debug.watch.len()));
        }
    }

    fn render_run_welcome(&mut self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .px(px(20.))
            .pt(px(4.))
            .gap(px(12.))
            .text_size(px(13.))
            .child(primary_button("run-debug", "Run and Debug", None).on_click(
                cx.listener(|this, _, window, cx| this.start_debugging(false, window, cx)),
            ))
            .map(|d| match self.root {
                None => d.child(div().child(
                    "To customize Run and Debug, open a folder and create a launch.json file.",
                )),
                Some(_) => d.child(
                    h_flex()
                        .flex_wrap()
                        .child("To customize Run and Debug\u{a0}")
                        .child(
                            div()
                                .id("create-launch")
                                .text_color(col::accent())
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .child("create a launch.json file")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.create_launch_json(window, cx)
                                })),
                        )
                        .child("."),
                ),
            })
            .into_any_element()
    }

    pub(super) fn render_debug_console(&self, cx: &Context<Self>) -> AnyElement {
        // Only the visible lines are laid out; it follows the tail.
        let wb = cx.weak_entity();
        let lines = list(self.debug.console_list.clone(), move |ix, _, cx| {
            let line = wb.upgrade().and_then(|wb| {
                let (text, color) = wb.read(cx).debug.console.get(ix)?;
                let text = text.strip_suffix('\n').unwrap_or(text).to_string();
                Some(div().min_h(px(19.)).text_color(*color).child(text))
            });
            line.unwrap_or_else(div).into_any_element()
        });
        v_flex()
            .size_full()
            .font_family("Consolas")
            .text_size(px(14.))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(20.))
                    .child(lines.size_full()),
            )
            .child(
                h_flex()
                    .flex_none()
                    .border_t_1()
                    .border_color(ca(0x80808060))
                    .child(codicon(icon::CHEVRON_RIGHT).pl(px(8.)).pr(px(6.)))
                    .child(
                        div().flex_1().child(
                            Input::new(&self.debug.repl)
                                .appearance(false)
                                .text_size(px(14.)),
                        ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn clear_debug_console(&mut self, cx: &mut Context<Self>) {
        self.debug.clear_console();
        cx.notify();
    }

    /// While a session runs: VS Code's floating toolbar, dragged by its gripper.
    pub(super) fn render_debug_toolbar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let s = self.debug.session.as_ref()?;
        let size = window.viewport_size();
        let (w, h) = (f32::from(size.width), f32::from(size.height));
        let (x, y) = self.debug.toolbar;
        let left = (x * w - TOOLBAR_W / 2.).min(w - TOOLBAR_W).max(0.);
        let top = y.min(h - TOOLBAR_H).max(0.);
        let paused = s.stop.is_some();
        let button = |id: &'static str, ch: char, color: u32, enabled: bool| {
            let text = match ch {
                icon::DEBUG_CONTINUE => "Continue (F5)",
                icon::DEBUG_PAUSE => "Pause (F6)",
                icon::DEBUG_STEP_OVER => "Step Over (F10)",
                icon::DEBUG_STEP_INTO => "Step Into (F11)",
                icon::DEBUG_STEP_OUT => "Step Out (Shift+F11)",
                icon::DEBUG_RESTART => "Restart (Ctrl+Shift+F5)",
                _ => "Stop (Shift+F5)",
            };
            action_button(id, ch)
                .text_color(c(color))
                .tooltip(tip(text))
                .when(!enabled, |d| d.opacity(0.4))
        };
        let toolbar = h_flex()
            .id("debug-toolbar")
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(TOOLBAR_W))
            .h(px(TOOLBAR_H))
            .pl(px(2.))
            .gap(px(4.))
            .pr(px(4.))
            .rounded(px(8.))
            .bg(c(0x181818))
            .shadow(vec![BoxShadow {
                color: hsla(0., 0., 0., 0.36),
                offset: point(px(0.), px(0.)),
                blur_radius: px(12.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(
                div()
                    .id("dbg-gripper")
                    .h_full()
                    .flex()
                    .items_center()
                    .cursor_grab()
                    .child(
                        codicon(icon::GRIPPER)
                            .w(px(16.))
                            .opacity(0.5)
                            .text_color(col::fg()),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                            this.debug.drag = Some((ev.position, this.debug.toolbar));
                            cx.notify();
                        }),
                    )
                    // VS Code puts it back on a double click.
                    .on_click(cx.listener(|this, ev: &ClickEvent, _, cx| {
                        if ev.click_count() == 2 {
                            this.debug.toolbar = (0.5, 0.);
                            this.debug.save();
                            cx.notify();
                        }
                    })),
            )
            .child(match paused {
                true => button("dbg-continue", icon::DEBUG_CONTINUE, ICON_BLUE, true)
                    .on_click(cx.listener(|this, _, _, cx| this.resume("continue", cx))),
                false => button("dbg-pause", icon::DEBUG_PAUSE, ICON_BLUE, true)
                    .on_click(cx.listener(|this, _, _, cx| this.pause(cx))),
            })
            .child(
                button("dbg-over", icon::DEBUG_STEP_OVER, ICON_BLUE, paused)
                    .on_click(cx.listener(|this, _, _, cx| this.resume("next", cx))),
            )
            .child(
                button("dbg-into", icon::DEBUG_STEP_INTO, ICON_BLUE, paused)
                    .on_click(cx.listener(|this, _, _, cx| this.resume("stepIn", cx))),
            )
            .child(
                button("dbg-out", icon::DEBUG_STEP_OUT, ICON_BLUE, paused)
                    .on_click(cx.listener(|this, _, _, cx| this.resume("stepOut", cx))),
            )
            .child(
                button("dbg-restart", icon::DEBUG_RESTART, ICON_GREEN, true)
                    .on_click(cx.listener(|this, _, _, cx| this.restart_debugging(cx))),
            )
            .child(
                button("dbg-stop", icon::DEBUG_STOP, ICON_RED, true)
                    .on_click(cx.listener(|this, _, _, cx| this.stop_debugging(cx))),
            );
        // While dragging, a window-sized layer takes every mouse event.
        let end_drag = |this: &mut Self, cx: &mut Context<Self>| {
            this.debug.drag = None;
            this.debug.save();
            cx.notify();
        };
        Some(
            div()
                .id("dbg-drag")
                .absolute()
                .size_full()
                .when(self.debug.drag.is_some(), |d| {
                    d.occlude()
                        .cursor_grabbing()
                        .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                            let Some((start, (x, y))) = this.debug.drag else {
                                return;
                            };
                            if ev.pressed_button != Some(MouseButton::Left) {
                                return end_drag(this, cx);
                            }
                            let half = TOOLBAR_W / 2. / w;
                            let d = ev.position - start;
                            this.debug.toolbar = (
                                (x + f32::from(d.x) / w).min(1. - half).max(half),
                                (y + f32::from(d.y)).min(h - TOOLBAR_H).max(0.),
                            );
                            cx.notify();
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| end_drag(this, cx)),
                        )
                })
                .child(toolbar)
                .into_any_element(),
        )
    }

    /// The status bar item naming the running configuration.
    pub(super) fn debug_status(&self) -> Option<(String, bool)> {
        let s = self.debug.session.as_ref()?;
        Some((s.name.clone(), !s.no_debug))
    }
}

/// The buffer line shown at window `y`, from the editor's last layout.
fn line_at(e: &EditorState, y: Pixels) -> Option<u32> {
    let text = e.text();
    let lh = e.line_height()?;
    let map = e.display_map();
    let rows = e.visible_row_range()?;
    rows.filter_map(|row| {
        let line = map.display_row_to_buffer_line(row);
        let start = text.line_start_offset(line);
        let top = e.range_to_bounds(&(start..start))?.top();
        // A wrapped line spans several rows.
        let height = lh
            * map
                .buffer_line_to_display_row_range(line)
                .map_or(1, |r| r.len().max(1)) as f32;
        (top <= y && y < top + height).then_some(line as u32)
    })
    .next()
}

fn paint_glyph(
    ch: char,
    color: Hsla,
    origin: Point<Pixels>,
    line_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let text = SharedString::from(ch.to_string());
    let run = TextRun {
        len: text.len(),
        font: font("codicon"),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(text, px(16.), &[run], None);
    let x = origin.x + (px(GLYPH_MARGIN) - line.width) / 2.;
    line.paint(
        point(x, origin.y),
        line_height,
        TextAlign::Left,
        None,
        window,
        cx,
    )
    .ok();
}

/// VS Code's tokenExpression colors for a value.
fn value_color(value: &str) -> Hsla {
    let v = value.trim();
    if v.starts_with('"') || v.starts_with('\'') {
        c(0xCE9178)
    } else if v == "true" || v == "false" {
        c(0x4E94CE)
    } else if v.parse::<f64>().is_ok() || v.starts_with("0x") {
        c(0xB5CEA8)
    } else {
        ca(0xCCCCCC99)
    }
}

/// An invisible decoration over `line`, which the editor moves through edits.
fn track_line(
    editor: &Entity<EditorState>,
    line: u32,
    cx: &mut App,
) -> Option<RangeDecorationCollection> {
    editor.update(cx, |e, cx| {
        let text = e.text();
        let line = line as usize;
        let range = match (line + 1).cmp(&text.lines_len()) {
            std::cmp::Ordering::Less => {
                text.line_start_offset(line)..text.line_start_offset(line + 1)
            }
            std::cmp::Ordering::Equal => text.line_start_offset(line)..text.line_end_offset(line),
            std::cmp::Ordering::Greater => return None,
        };
        // An empty last line has nothing to hold on to; it keeps its number.
        if range.is_empty() {
            return None;
        }
        let deco = RangeDecoration::new(range)
            .with_style(RangeDecorationStyle::Fill)
            .with_color(transparent_black());
        Some(e.create_range_decorations_collection(vec![deco], cx))
    })
}

fn send(out: &Option<mpsc::Sender<String>>, msg: &Value) {
    let body = msg.to_string();
    // A dead adapter shows up as the reader's end of stream.
    if let Some(out) = out {
        out.send(format!("Content-Length: {}\r\n\r\n{body}", body.len()))
            .ok();
    }
}

/// Starts an adapter that talks DAP on its stdio, or with `listen`, on the
/// socket whose address it prints first.
fn spawn_adapter(
    program: String,
    args: Vec<String>,
    listen: bool,
    out: mpsc::Receiver<String>,
    emit: futures::channel::mpsc::UnboundedSender<Msg>,
) -> Result<Child, String> {
    let mut command = Command::new(&program);
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000); // CREATE_NO_WINDOW
    let mut child = command
        .spawn()
        .map_err(|err| format!("Could not start the debug adapter '{program}': {err}\n"))?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let err = emit.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            err.unbounded_send(Msg::Err(line + "\n")).ok();
        }
    });
    if !listen {
        std::thread::spawn(move || pump(stdin, out));
        std::thread::spawn(move || {
            read_dap(stdout, |m| {
                emit.unbounded_send(m).ok();
            })
        });
        return Ok(child);
    }
    // Connect once it says where; the rest of its stdout is the debuggee's.
    std::thread::spawn(move || {
        let mut out = Some(out);
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            match (listen_addr(&line), out.take()) {
                (Some(addr), Some(rx)) => connect(addr.to_string(), rx, emit.clone()),
                (_, rx) => {
                    out = rx;
                    emit.unbounded_send(Msg::Out(line + "\n")).ok();
                }
            }
        }
        if out.is_some() {
            let msg = format!("'{program}' exited before it listened.\n");
            emit.unbounded_send(Msg::Fail(msg)).ok();
        }
    });
    Ok(child)
}

/// Writes framed messages until the session drops its sender.
fn pump(mut to: impl Write, out: mpsc::Receiver<String>) {
    for msg in out {
        if to
            .write_all(msg.as_bytes())
            .and_then(|_| to.flush())
            .is_err()
        {
            return;
        }
    }
}

/// Talks DAP over a socket; the session ends when either side closes it.
fn connect(
    addr: String,
    out: mpsc::Receiver<String>,
    emit: futures::channel::mpsc::UnboundedSender<Msg>,
) {
    std::thread::spawn(move || {
        let stream = match TcpStream::connect(&addr) {
            Ok(stream) => stream,
            Err(err) => {
                let msg = format!("Could not connect to the debug adapter at {addr}: {err}\n");
                emit.unbounded_send(Msg::Fail(msg)).ok();
                return;
            }
        };
        if let Ok(reader) = stream.try_clone() {
            std::thread::spawn(move || {
                read_dap(reader, |m| {
                    emit.unbounded_send(m).ok();
                })
            });
        }
        pump(&stream, out);
        stream.shutdown(Shutdown::Both).ok();
    });
}

/// The address in Delve's "DAP server listening at: 127.0.0.1:port".
fn listen_addr(line: &str) -> Option<&str> {
    Some(line.split_once("listening at:")?.1.trim())
}

/// Splits the adapter's stdout into DAP messages; anything outside a
/// `Content-Length` frame is the debuggee's output on the shared pipe.
fn read_dap(stdout: impl Read, mut emit: impl FnMut(Msg)) {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    let mut len = None;
    loop {
        buf.clear();
        if !matches!(reader.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
            return;
        }
        let line = String::from_utf8_lossy(&buf);
        if let Some(at) = line.find("Content-Length: ") {
            if at > 0 {
                emit(Msg::Out(line[..at].to_string()));
            }
            len = line[at + 16..].trim().parse::<usize>().ok();
        } else if let (Some(n), "") = (len, line.trim()) {
            len = None;
            let mut body = vec![0; n];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            if let Ok(msg) = serde_json::from_slice(&body) {
                emit(Msg::Dap(msg));
            }
        } else if len.is_none() {
            emit(Msg::Out(line.into_owned()));
        }
    }
}

/// `.vscode/launch.json`'s configurations.
fn launch_configs(root: &Path) -> Option<Vec<Value>> {
    let text = fs::read_to_string(root.join(".vscode").join("launch.json")).ok()?;
    let json: Value = serde_json::from_str(&strip_jsonc(&text)).ok()?;
    json["configurations"].as_array().cloned()
}

/// The configuration F5 runs without a launch.json, and the one it starts with.
fn default_config() -> Value {
    let exe = if cfg!(windows) { ".exe" } else { "" };
    json!({
        "name": "(gdb) Launch",
        "type": "gdb",
        "request": "launch",
        "program": format!("${{fileDirname}}/${{fileBasenameNoExtension}}{exe}"),
        "args": [],
        "cwd": "${fileDirname}",
    })
}

/// JSON with comments (`//`, `/* */`) and trailing commas, as VS Code reads it.
pub(super) fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                out.push(ch);
                while let Some(ch) = chars.next() {
                    out.push(ch);
                    match ch {
                        '\\' => out.extend(chars.next()),
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => while chars.next_if(|&c| c != '\n').is_some() {},
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for ch in chars.by_ref() {
                    if prev == '*' && ch == '/' {
                        break;
                    }
                    prev = ch;
                }
            }
            // Strings end in a quote, so a comma last is always structural.
            '}' | ']' => {
                let kept = out.trim_end().len();
                if out[..kept].ends_with(',') {
                    out.truncate(kept - 1);
                }
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Replaces `${name}` and `${env:NAME}` in every string of `value`; unknown
/// variables stay as written.
fn substitute(value: &mut Value, vars: &HashMap<&str, String>) {
    static VAR: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\$\{([^}]+)\}").unwrap());
    match value {
        Value::String(s) => {
            *s = (VAR.replace_all(s, |caps: &regex::Captures| {
                let name = &caps[1];
                match name.strip_prefix("env:") {
                    Some(env) => std::env::var(env).unwrap_or_default(),
                    None => vars
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| caps[0].to_string()),
                }
            }))
            .into_owned();
        }
        Value::Array(items) => items.iter_mut().for_each(|v| substitute(v, vars)),
        Value::Object(map) => map.values_mut().for_each(|v| substitute(v, vars)),
        _ => {}
    }
}

/// The adapter for a configuration's `type`, with VS Code's C++ and Go
/// extension options mapped onto the debuggers' own DAP ones. Any other
/// adapter works once started in server mode and named by `debugServer`.
fn adapter(config: &mut Value) -> Result<Adapter, String> {
    if let Some(port) = config["debugServer"].as_u64() {
        return Ok(Adapter::Tcp(port as u16));
    }
    let gdb = |path: &Value| {
        Adapter::Stdio(
            path.as_str().unwrap_or("gdb").to_string(),
            vec!["-i".into(), "dap".into()],
        )
    };
    let tool = |key: &str, default: &str| config[key].as_str().unwrap_or(default).to_string();
    match config["type"].as_str().unwrap_or_default() {
        "gdb" => Ok(gdb(&config["debuggerPath"])),
        "cppdbg" => {
            if config["stopAtEntry"] == true {
                config["stopAtBeginningOfMainSubprogram"] = true.into();
            }
            if let Some(env) = config["environment"].as_array() {
                let env: serde_json::Map<String, Value> = (env.iter())
                    .filter_map(|e| Some((e["name"].as_str()?.to_string(), e["value"].clone())))
                    .collect();
                config["env"] = env.into();
            }
            match config["MIMode"].as_str() {
                Some("lldb") => Ok(Adapter::Stdio("lldb-dap".into(), vec![])),
                _ => Ok(gdb(&config["miDebuggerPath"])),
            }
        }
        "lldb" | "lldb-dap" => Ok(Adapter::Stdio(tool("lldbDapPath", "lldb-dap"), vec![])),
        "debugpy" | "python" => Ok(Adapter::Stdio(
            tool("python", "python"),
            vec!["-m".into(), "debugpy.adapter".into()],
        )),
        "coreclr" | "netcoredbg" => Ok(Adapter::Stdio(
            tool("netcoredbgPath", "netcoredbg"),
            vec!["--interpreter=vscode".into()],
        )),
        "go" => {
            let dlv = tool("dlvToolPath", "dlv");
            if config["mode"].as_str().is_none_or(|m| m == "auto") {
                let test = (config["program"].as_str()).is_some_and(|p| p.ends_with("_test.go"));
                config["mode"] = if test { "test" } else { "debug" }.into();
            }
            let args = vec!["dap".into(), "--listen=127.0.0.1:0".into()];
            Ok(Adapter::Listen(dlv, args))
        }
        ty => Err(format!(
            "Configured debug type '{ty}' is not supported.\nStart its debug adapter in server mode and add \"debugServer\": <port> to the configuration."
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::{Value, json};

    use super::{Adapter, Msg, adapter, listen_addr, read_dap, strip_jsonc, substitute};

    #[test]
    fn launch_json() {
        let text = r#"{
            // comment, "not a string"
            "url": "http://x/*y*/", /* block */
            "configurations": [ { "name": "a,]", "args": ["${file}", "${nope}", "${env:VSN_TEST_UNSET}"], }, ],
        }"#;
        let mut json: Value = serde_json::from_str(&strip_jsonc(text)).unwrap();
        assert_eq!(json["url"], "http://x/*y*/");
        let vars = HashMap::from([("file", "C:\\a.c".to_string())]);
        substitute(&mut json, &vars);
        assert_eq!(json["configurations"][0]["name"], "a,]");
        assert_eq!(
            json["configurations"][0]["args"],
            json!(["C:\\a.c", "${nope}", ""])
        );
    }

    #[test]
    fn dap_frames() {
        let input = b"hi\nContent-Length: 2\r\n\r\n{}outContent-Length: 7\r\n\r\n{\"a\":1}tail";
        let mut msgs = Vec::new();
        read_dap(&input[..], |m| msgs.push(m));
        let shown: Vec<String> = (msgs.iter())
            .map(|m| match m {
                Msg::Dap(v) => v.to_string(),
                Msg::Out(s) | Msg::Err(s) | Msg::Fail(s) => format!("out:{s}"),
            })
            .collect();
        assert_eq!(
            shown,
            ["out:hi\n", "{}", "out:out", "{\"a\":1}", "out:tail"]
        );
    }

    #[test]
    fn adapters() {
        let mut go = json!({"type": "go", "program": "x/a_test.go"});
        let Ok(Adapter::Listen(dlv, args)) = adapter(&mut go) else {
            panic!()
        };
        assert_eq!((dlv.as_str(), args[0].as_str()), ("dlv", "dap"));
        assert_eq!(go["mode"], "test");
        let mut net = json!({"type": "coreclr"});
        assert!(matches!(adapter(&mut net), Ok(Adapter::Stdio(p, _)) if p == "netcoredbg"));
        let mut srv = json!({"type": "whatever", "debugServer": 4711});
        assert_eq!(adapter(&mut srv), Ok(Adapter::Tcp(4711)));
        assert!(adapter(&mut json!({"type": "whatever"})).is_err());
        let line = "API server listening at: 127.0.0.1:51234";
        assert_eq!(listen_addr(line), Some("127.0.0.1:51234"));
    }
}
