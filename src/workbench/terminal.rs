//! The integrated terminals: each a shell on a ConPTY, its output parsed by
//! vt100 into a grid that's drawn one text run per style per row.

use std::{
    cell::Cell,
    io::{Read as _, Write},
    rc::Rc,
};

use gpui_kit::component::{
    input::Position,
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use super::*;
use crate::theme::c;

const FONT: &str = "Consolas";
const FONT_SIZE: f32 = 14.;
const LINE_H: f32 = 17.;
/// VS Code's padding left of the grid, and room for the scrollbar right of it.
const PAD_L: f32 = 20.;
const PAD_R: f32 = 14.;
/// `terminal.integrated.scrollback`.
const SCROLLBACK: usize = 1000;
/// The tab list VS Code shows right of the terminals once there are two.
const TABS_W: f32 = 160.;

#[derive(Default)]
pub(super) struct Terminals {
    /// Split groups are runs of the same `group`, shown side by side.
    list: Vec<Terminal>,
    active: usize,
    /// Why the last terminal failed to launch.
    error: Option<String>,
    next_id: u64,
}

pub(super) struct Terminal {
    id: u64,
    /// The id of the terminal its split group started with.
    group: u64,
    name: String,
    parser: vt100::Parser<Replies>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    focus: FocusHandle,
    /// Where the grid was last laid out, to size the pty and hit-test the mouse.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    cell_w: f32,
    /// Drag start and end as cell boundaries (row, col) on the visible screen.
    selection: Option<((u16, u16), (u16, u16))>,
    selecting: bool,
    /// The link under the mouse: row and column range.
    hover: Option<(u16, u16, u16)>,
    /// Wheel movement not yet a whole line.
    scroll: f32,
    _task: Task<()>,
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.killer.kill().ok();
    }
}

impl Terminal {
    fn write(&mut self, bytes: &[u8]) {
        self.selection = None;
        self.parser.screen_mut().set_scrollback(0);
        self.writer.write_all(bytes).ok();
        self.writer.flush().ok();
    }

    /// The cell under the mouse, or with `round` the cell boundary nearest it.
    fn cell(&self, position: Point<Pixels>, round: bool) -> (u16, u16) {
        let (rows, cols) = self.parser.screen().size();
        let at = position - self.bounds.get().origin;
        let col = (f32::from(at.x) - PAD_L) / self.cell_w;
        let col = if round { col.round() } else { col.floor() };
        let row = (f32::from(at.y) / LINE_H).floor();
        (
            (row.max(0.) as u16).min(rows.get() - 1),
            (col.max(0.) as u16).min(cols.get() - !round as u16),
        )
    }

    /// The link under the mouse, if any.
    fn link(&self, position: Point<Pixels>, root: Option<&Path>) -> Option<(u16, u16, u16, Link)> {
        let screen = self.parser.screen();
        let (row, col) = self.cell(position, false);
        let line: Vec<char> = (0..screen.size().1.get())
            .map(|col| {
                let cell = screen.cell(row, col);
                cell.and_then(|c| c.contents().chars().next())
                    .unwrap_or(' ')
            })
            .collect();
        let (a, b) = word_at(&line, col as usize)?;
        let link = link(&line[a..b].iter().collect::<String>(), root)?;
        Some((row, a as u16, b as u16, link))
    }
}

/// Answers the queries a program waits on. ConPTY asks for the cursor
/// position before the shell starts.
#[derive(Default)]
struct Replies(Vec<u8>);

impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        match (i1, c, params) {
            (None, 'n', [[6]]) => {
                let (row, col) = screen.cursor_position();
                write!(self.0, "\x1b[{};{}R", row + 1, col + 1).ok();
            }
            (None, 'c', [] | [[0]]) => self.0.extend_from_slice(b"\x1b[?1;2c"),
            _ => {}
        }
    }
}

enum Msg {
    Out(Vec<u8>),
    Exit,
}

#[derive(Debug, PartialEq)]
enum Link {
    Url(String),
    /// A file with its 1-based line and column.
    File(PathBuf, u32, u32),
}

/// VS Code's default profile on Windows: PowerShell 7 when it's installed,
/// else Windows PowerShell.
fn shell() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("pwsh.exe"))
        .find(|p| p.is_file())
        .unwrap_or("powershell.exe".into())
}

impl Workbench {
    fn spawn_terminal(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<Terminal, String> {
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| e.to_string())?;
        let shell = shell();
        let mut cmd = CommandBuilder::new(&shell);
        if let Some(root) = &self.root {
            cmd.cwd(root);
        }
        let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let out = tx.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0; 1 << 16];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if out.unbounded_send(Msg::Out(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });
        // ConPTY keeps its output open after the shell exits, so wait on the shell.
        std::thread::spawn(move || {
            child.wait().ok();
            tx.unbounded_send(Msg::Exit).ok();
        });
        let id = self.terminals.next_id;
        self.terminals.next_id += 1;
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let done =
                    this.update_in(cx, |this, window, cx| this.on_terminal(id, msg, window, cx));
                if done.unwrap_or(true) {
                    break;
                }
            }
        });

        Ok(Terminal {
            id,
            group: id,
            name: shell
                .file_stem()
                .map_or("shell".into(), |s| s.to_string_lossy().into()),
            parser: vt100::Parser::new_with_callbacks(
                24.try_into().unwrap(),
                80.try_into().unwrap(),
                SCROLLBACK,
                Replies::default(),
            ),
            writer,
            master: pair.master,
            killer,
            focus: cx.focus_handle(),
            bounds: Rc::default(),
            cell_w: 8.,
            selection: None,
            selecting: false,
            hover: None,
            scroll: 0.,
            _task: task,
        })
    }

    fn terminal(&mut self, id: u64) -> Option<&mut Terminal> {
        self.terminals.list.iter_mut().find(|t| t.id == id)
    }

    /// Output from a shell; true once it exited.
    fn on_terminal(
        &mut self,
        id: u64,
        msg: Msg,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(t) = self.terminal(id) else {
            return true;
        };
        match msg {
            Msg::Out(bytes) => {
                t.parser.process(&bytes);
                let replies = std::mem::take(&mut t.parser.callbacks_mut().0);
                if !replies.is_empty() {
                    t.writer.write_all(&replies).ok();
                    t.writer.flush().ok();
                }
                cx.notify();
                false
            }
            Msg::Exit => {
                self.kill_terminal(id, window, cx);
                true
            }
        }
    }

    /// Starts a shell beside the active one, or in a new group, and makes it
    /// active. False when it failed to launch.
    fn add_terminal(&mut self, split: bool, window: &Window, cx: &mut Context<Self>) -> bool {
        let mut t = match self.spawn_terminal(window, cx) {
            Ok(t) => t,
            Err(err) => {
                self.terminals.error = Some(err);
                return false;
            }
        };
        self.terminals.error = None;
        let list = &mut self.terminals.list;
        let ix = match list.get(self.terminals.active) {
            Some(active) if split => {
                t.group = active.group;
                list.iter().rposition(|u| u.group == t.group).unwrap() + 1
            }
            _ => list.len(),
        };
        list.insert(ix, t);
        self.terminals.active = ix;
        true
    }

    /// The + button, Ctrl+Shift+`, and split (Ctrl+Shift+5).
    pub(super) fn new_terminal(
        &mut self,
        split: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_terminal(split, window, cx);
        self.focus_terminal(window, cx);
    }

    /// Ctrl+`: shows the panel and focuses the active terminal, starting one
    /// if there's none.
    pub(super) fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel = true;
        self.panel_tab = TERMINAL;
        if self.terminals.list.is_empty() {
            self.add_terminal(false, window, cx);
        }
        if let Some(t) = self.terminals.list.get(self.terminals.active) {
            window.focus(&t.focus, cx);
        }
        cx.notify();
    }

    fn activate_terminal(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.terminals.list.iter().position(|t| t.id == id) {
            self.terminals.active = ix;
            window.focus(&self.terminals.list[ix].focus, cx);
            cx.notify();
        }
    }

    pub(super) fn kill_active_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.terminals.list.get(self.terminals.active) {
            self.kill_terminal(t.id, window, cx);
        }
    }

    /// The trash button, or the shell exited: VS Code closes the panel with
    /// its last terminal.
    fn kill_terminal(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.terminals.list.iter().position(|t| t.id == id) else {
            return;
        };
        let focused = self.terminal_focused(window);
        self.terminals.list.remove(ix);
        let list = &self.terminals.list;
        let active = &mut self.terminals.active;
        if *active > ix || *active == list.len() {
            *active = active.saturating_sub(1);
        }
        if list.is_empty() && self.panel_tab == TERMINAL {
            self.panel = false;
        }
        if focused {
            match list.get(*active) {
                Some(t) => window.focus(&t.focus, cx),
                None => self.focus_editor(window, cx),
            }
        }
        cx.notify();
    }

    pub(super) fn terminal_focused(&self, window: &Window) -> bool {
        self.terminals
            .list
            .iter()
            .any(|t| t.focus.is_focused(window))
    }

    fn copy_terminal(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        let Some(t) = self.terminal(id) else {
            return false;
        };
        let Some(((r0, c0), (r1, c1))) = t.selection.filter(|(a, b)| a != b).map(sorted) else {
            return false;
        };
        let text = t.parser.screen().contents_between(r0, c0, r1, c1);
        t.selection = None;
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        cx.notify();
        true
    }

    fn paste_terminal(&mut self, id: u64, cx: &mut Context<Self>) {
        let text = cx.read_from_clipboard().and_then(|c| c.text());
        let Some(t) = self.terminal(id) else {
            return;
        };
        let text = text
            .unwrap_or_default()
            .replace("\r\n", "\r")
            .replace('\n', "\r");
        let bytes = if t.parser.screen().bracketed_paste() {
            format!("\x1b[200~{text}\x1b[201~")
        } else {
            text
        };
        t.write(bytes.as_bytes());
        cx.notify();
    }

    // ponytail: the visible screen only, not the scrollback.
    fn select_all_terminal(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(t) = self.terminal(id) {
            let (rows, cols) = t.parser.screen().size();
            t.selection = Some(((0, 0), (rows.get() - 1, cols.get())));
            cx.notify();
        }
    }

    /// Scrolls the prompt line to the top and drops everything above it.
    // ponytail: ConPTY keeps its own copy of the screen and repaints it on a
    // resize; sending it a clear too (as VS Code does) when that matters.
    fn clear_terminal(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(t) = self.terminal(id) {
            let (row, _) = t.parser.screen().cursor_position();
            if row > 0 {
                t.parser
                    .process(format!("\x1b[{row}S\x1b[{row}A").as_bytes());
            }
            t.parser.screen_mut().clear_scrollback();
            t.parser.screen_mut().set_scrollback(0);
            t.selection = None;
            cx.notify();
        }
    }

    fn terminal_key(&mut self, id: u64, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let ctrl = ks.modifiers.control && !ks.modifiers.alt;
        // Ctrl+C copies when there's a selection, else interrupts.
        if ctrl && ks.key == "c" && self.copy_terminal(id, cx) {
        } else if ctrl && ks.key == "v" {
            self.paste_terminal(id, cx);
        } else if let Some(t) = self.terminal(id)
            && let Some(bytes) = key_bytes(
                ks,
                ev.prefer_character_input,
                t.parser.screen().application_cursor(),
            )
        {
            t.write(&bytes);
        } else {
            return;
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The wheel goes to a full-screen program: as mouse reports when it
    /// asked for the mouse, else as arrow keys like xterm's alternateScroll.
    fn terminal_wheel(&mut self, id: u64, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(t) = self.terminal(id) else {
            return;
        };
        t.scroll += f32::from(ev.delta.pixel_delta(px(LINE_H)).y) / LINE_H;
        let lines = t.scroll.trunc();
        t.scroll -= lines;
        if lines == 0. {
            return;
        }
        let screen = t.parser.screen();
        let up = lines > 0.;
        let n = lines.abs() as usize;
        if screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None {
            let report = wheel_bytes(
                up,
                t.cell(ev.position, false),
                screen.mouse_protocol_encoding(),
            );
            t.write(&report.repeat(n));
        // Windows' own ConPTY swallows the switch to the alternate screen, but
        // passes on the cursor-key mode full-screen programs like less set.
        // ponytail: a shell that sets it too loses wheel scrollback; ship
        // OpenConsole's conpty.dll like VS Code if that bites.
        } else if screen.alternate_screen() || screen.application_cursor() {
            let arrow = match (screen.application_cursor(), up) {
                (false, true) => "\x1b[A",
                (false, false) => "\x1b[B",
                (true, true) => "\x1bOA",
                (true, false) => "\x1bOB",
            };
            t.write(arrow.repeat(n).as_bytes());
        } else {
            let screen = t.parser.screen_mut();
            let top = (screen.scrollback() as f32 + lines).max(0.) as usize;
            screen.set_scrollback(top);
        }
        cx.notify();
    }

    fn open_link(&mut self, link: Link, window: &mut Window, cx: &mut Context<Self>) {
        match link {
            Link::Url(url) => cx.open_url(&url),
            Link::File(path, line, col) => {
                self.open_path(path, window, cx);
                if let Some(doc) = self.docs.get(self.active) {
                    let at = Position::new(line.saturating_sub(1), col.saturating_sub(1));
                    doc.editor
                        .update(cx, |e, cx| e.set_cursor_position(at, window, cx));
                }
            }
        }
    }

    pub(super) fn render_terminal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.terminals.list.is_empty() && self.terminals.error.is_none() {
            self.add_terminal(false, window, cx);
        }
        if self.terminals.list.is_empty() {
            let err = self.terminals.error.clone().unwrap_or_default();
            return div()
                .px(px(PAD_L))
                .child(format!("The terminal process failed to launch: {err}."))
                .into_any_element();
        }

        let group = self.terminals.list[self.terminals.active].group;
        let shown: Vec<usize> = (0..self.terminals.list.len())
            .filter(|&ix| self.terminals.list[ix].group == group)
            .collect();
        let panes: Vec<_> = shown
            .into_iter()
            .enumerate()
            .map(|(i, ix)| {
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .when(i > 0, |d| d.border_l_1().border_color(col::border()))
                    .child(self.render_one(ix, window, cx))
            })
            .collect();

        h_flex()
            .size_full()
            .child(h_flex().flex_1().min_w_0().h_full().children(panes))
            .when(self.terminals.list.len() > 1, |d| {
                d.child(self.render_terminal_tabs(cx))
            })
            .into_any_element()
    }

    /// VS Code's list of terminals, with split groups drawn as a tree.
    fn render_terminal_tabs(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let list = &self.terminals.list;
        let rows = list.iter().enumerate().map(|(ix, t)| {
            let id = t.id;
            let same = |j: Option<usize>| {
                j.and_then(|j| list.get(j))
                    .is_some_and(|u| u.group == t.group)
            };
            let tree = match (same(ix.checked_sub(1)), same(Some(ix + 1))) {
                (false, false) => "",
                (false, true) => "┌",
                (true, true) => "├",
                (true, false) => "└",
            };
            let active = ix == self.terminals.active;
            h_flex()
                .id(("terminal-tab", id as usize))
                .h(px(22.))
                .flex_none()
                .pl(px(8.))
                .pr(px(4.))
                .gap(px(6.))
                .cursor_pointer()
                .when(active, |d| d.bg(col::selected()))
                .when(!active, |d| d.hover(|s| s.bg(col::hover())))
                .child(div().w(px(8.)).text_color(col::fg_muted()).child(tree))
                .child(codicon(icon::TERMINAL))
                .child(div().flex_1().min_w_0().truncate().child(t.name.clone()))
                .when(active, |d| {
                    d.child(
                        action_button(("terminal-kill", id as usize), icon::TRASH).on_click(
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.kill_terminal(id, window, cx)
                            }),
                        ),
                    )
                })
                .on_click(
                    cx.listener(move |this, _, window, cx| this.activate_terminal(id, window, cx)),
                )
        });
        v_flex()
            .w(px(TABS_W))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(col::border())
            .children(rows)
    }

    fn render_one(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let wb = cx.weak_entity();
        let t = &mut self.terminals.list[ix];
        let id = t.id;
        let font = font(FONT);
        let ts = window.text_system();
        let cell_w = ts.advance(ts.resolve_font(&font), px(FONT_SIZE), 'm');
        t.cell_w = cell_w.map_or(8., |s| f32::from(s.width));
        let size = t.bounds.get().size;
        if size.width > px(0.) {
            let rows = (f32::from(size.height) / LINE_H).floor().max(1.) as u16;
            let cols = (f32::from(size.width) - PAD_L - PAD_R) / t.cell_w;
            let cols = cols.floor().max(1.) as u16;
            let (r, c) = t.parser.screen().size();
            if (r.get(), c.get()) != (rows, cols) {
                t.parser
                    .screen_mut()
                    .set_size(rows.try_into().unwrap(), cols.try_into().unwrap());
                let size = PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                };
                t.master.resize(size).ok();
            }
        }

        let screen = t.parser.screen();
        let (rows, cols) = screen.size();
        let (fg0, bg0) = (c(0xCCCCCC), col::bg());
        let selection = t.selection.map(sorted);
        let hover = t.hover;
        let lines = (0..rows.get()).map(|row| {
            let mut text = String::new();
            let mut runs: Vec<TextRun> = Vec::new();
            for col in 0..cols.get() {
                let Some(cell) = screen.cell(row, col) else {
                    break;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let s = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                let (mut fg, mut bg) = (
                    ansi(cell.fgcolor(), cell.bold()).unwrap_or(fg0),
                    ansi(cell.bgcolor(), false),
                );
                if cell.inverse() {
                    (fg, bg) = (bg.unwrap_or(bg0), Some(fg));
                }
                if cell.dim() {
                    fg = fg.opacity(0.5);
                }
                if selection.is_some_and(|(a, b)| a <= (row, col) && (row, col) < b) {
                    bg = Some(c(0x264F78));
                }
                let link = hover.is_some_and(|(r, a, b)| r == row && (a..b).contains(&col));
                let run = TextRun {
                    len: s.len(),
                    font: Font {
                        weight: if cell.bold() {
                            FontWeight::BOLD
                        } else {
                            FontWeight::NORMAL
                        },
                        style: if cell.italic() {
                            FontStyle::Italic
                        } else {
                            FontStyle::Normal
                        },
                        ..font.clone()
                    },
                    color: fg,
                    background_color: bg,
                    underline: (cell.underline() || link).then_some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(fg),
                        wavy: false,
                    }),
                    strikethrough: None,
                };
                text.push_str(s);
                match runs.last_mut() {
                    Some(last)
                        if *last
                            == TextRun {
                                len: last.len,
                                ..run.clone()
                            } =>
                    {
                        last.len += run.len
                    }
                    _ => runs.push(run),
                }
            }
            div()
                .h(px(LINE_H))
                .child(StyledText::new(text).with_runs(runs))
        });

        let (row, col) = screen.cursor_position();
        let focused = t.focus.is_focused(window);
        let under = screen.cell(row, col).map(|c| c.contents().to_string());
        let cursor = (!screen.hide_cursor() && screen.scrollback() == 0).then(|| {
            div()
                .absolute()
                .left(px(PAD_L + col as f32 * t.cell_w))
                .top(px(row as f32 * LINE_H))
                .w(px(t.cell_w))
                .h(px(LINE_H))
                .map(|d| match focused {
                    true => d.bg(fg0).text_color(bg0).children(under),
                    false => d.border_1().border_color(fg0),
                })
        });

        let bounds = t.bounds.clone();
        div()
            .id(("terminal", id as usize))
            .relative()
            .size_full()
            .overflow_hidden()
            .track_focus(&t.focus)
            .key_context("Terminal")
            .map(|d| match hover {
                Some(_) => d.cursor_pointer(),
                None => d.cursor_text(),
            })
            .on_key_down(cx.listener(move |this, ev, _, cx| this.terminal_key(id, ev, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    let root = this.root.clone();
                    let Some(t) = this.terminal(id) else {
                        return;
                    };
                    if ev.modifiers.control
                        && let Some((.., link)) = t.link(ev.position, root.as_deref())
                    {
                        // Keep the opened editor focused.
                        window.prevent_default();
                        return this.open_link(link, window, cx);
                    }
                    t.selection = Some(t.cell(ev.position, true)).map(|c| (c, c));
                    t.selecting = true;
                    this.activate_terminal(id, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| this.activate_terminal(id, window, cx)),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                let root = this.root.clone();
                let Some(t) = this.terminal(id) else {
                    return;
                };
                if t.selecting
                    && let Some((a, _)) = t.selection
                {
                    t.selecting = ev.pressed_button == Some(MouseButton::Left);
                    t.selection = Some((a, t.cell(ev.position, true)));
                    cx.notify();
                } else if ev.pressed_button.is_none() {
                    let hover = t
                        .link(ev.position, root.as_deref())
                        .map(|(r, a, b, _)| (r, a, b));
                    if t.hover != hover {
                        t.hover = hover;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, _| {
                    if let Some(t) = this.terminal(id) {
                        t.selecting = false;
                    }
                }),
            )
            .on_scroll_wheel(cx.listener(move |this, ev, _, cx| this.terminal_wheel(id, ev, cx)))
            .child(
                canvas(
                    move |b, window, _| {
                        if bounds.replace(b) != b {
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            // Styled inside so the context menu keeps the UI font.
            .child(
                div()
                    .font_family(FONT)
                    .text_size(px(FONT_SIZE))
                    .line_height(px(LINE_H))
                    .text_color(fg0)
                    .child(v_flex().pl(px(PAD_L)).children(lines))
                    .children(cursor),
            )
            .context_menu(move |menu: PopupMenu, _, _| {
                type Act = fn(&mut Workbench, u64, &mut Window, &mut Context<Workbench>);
                let item = |label: &'static str, act: Act| {
                    let wb = wb.clone();
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        wb.update(cx, |this, cx| act(this, id, window, cx)).ok();
                    })
                };
                menu.item(item("Split Terminal", |this, _, w, cx| {
                    this.new_terminal(true, w, cx)
                }))
                .item(item("New Terminal", |this, _, w, cx| {
                    this.new_terminal(false, w, cx)
                }))
                .separator()
                .item(item("Copy", |this, id, _, cx| {
                    this.copy_terminal(id, cx);
                }))
                .item(item("Paste", |this, id, _, cx| this.paste_terminal(id, cx)))
                .item(item("Select All", |this, id, _, cx| {
                    this.select_all_terminal(id, cx)
                }))
                .separator()
                .item(item("Clear Terminal", |this, id, _, cx| {
                    this.clear_terminal(id, cx)
                }))
                .separator()
                .item(item("Kill Terminal", Workbench::kill_terminal))
            })
            .into_any_element()
    }
}

fn sorted<T: Ord>((a, b): (T, T)) -> (T, T) {
    if a <= b { (a, b) } else { (b, a) }
}

/// The run of characters around `col` that could be a link, without the
/// punctuation that usually ends a sentence.
// ponytail: one screen row; a link wrapped onto the next row is cut there.
fn word_at(line: &[char], col: usize) -> Option<(usize, usize)> {
    let sep = |c: char| c.is_whitespace() || "\"'<>()[]{}|`".contains(c);
    if sep(*line.get(col)?) {
        return None;
    }
    let mut a = col;
    while a > 0 && !sep(line[a - 1]) {
        a -= 1;
    }
    let mut b = col + 1;
    while b < line.len() && !sep(line[b]) {
        b += 1;
    }
    while b > a && ".,;:!?".contains(line[b - 1]) {
        b -= 1;
    }
    (col < b).then_some((a, b))
}

/// A URL, or an existing file with an optional `:line:col`.
// ponytail: relative paths resolve against the workspace root, not the
// shell's cwd; track the cwd via shell integration (OSC 633) when it matters.
fn link(word: &str, root: Option<&Path>) -> Option<Link> {
    if word.starts_with("https://") || word.starts_with("http://") {
        return Some(Link::Url(word.into()));
    }
    let mut path = word;
    let mut at = Vec::new();
    while at.len() < 2
        && let Some((p, n)) = path.rsplit_once(':')
        && let Ok(n) = n.parse::<u32>()
    {
        at.insert(0, n);
        path = p;
    }
    let path = Path::new(path);
    let path = match root {
        Some(root) if path.is_relative() => root.join(path),
        _ => path.to_path_buf(),
    };
    let at = |i: usize| at.get(i).copied().unwrap_or(1);
    path.is_file().then(|| Link::File(path, at(0), at(1)))
}

/// A wheel notch as an xterm mouse report at a 0-based (row, col).
fn wheel_bytes(
    up: bool,
    (row, col): (u16, u16),
    encoding: vt100::MouseProtocolEncoding,
) -> Vec<u8> {
    let button = if up { 64 } else { 65 };
    let (x, y) = (col as u32 + 1, row as u32 + 1);
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => format!("\x1b[<{button};{x};{y}M").into_bytes(),
        vt100::MouseProtocolEncoding::Utf8 => [button, x, y]
            .iter()
            .map(|&v| char::from_u32(32 + v).unwrap_or(' '))
            .fold("\x1b[M".to_string(), |mut s, c| {
                s.push(c);
                s
            })
            .into_bytes(),
        vt100::MouseProtocolEncoding::Default => {
            let byte = |v: u32| (32 + v).min(255) as u8;
            vec![0x1b, b'[', b'M', byte(button), byte(x), byte(y)]
        }
    }
}

/// VS Code's dark terminal colors; `bold` brightens the first eight like its
/// `terminal.integrated.drawBoldTextInBrightColors`.
fn ansi(color: vt100::Color, bold: bool) -> Option<Hsla> {
    const BASE: [u32; 16] = [
        0x000000, 0xCD3131, 0x0DBC79, 0xE5E510, 0x2472C8, 0xBC3FBC, 0x11A8CD, 0xE5E5E5, 0x666666,
        0xF14C4C, 0x23D18B, 0xF5F543, 0x3B8EEA, 0xD670D6, 0x29B8DB, 0xE5E5E5,
    ];
    let rgb = |r: u8, g: u8, b: u8| (r as u32) << 16 | (g as u32) << 8 | b as u32;
    Some(c(match color {
        vt100::Color::Default => return None,
        vt100::Color::Idx(i @ 0..8) if bold => BASE[i as usize + 8],
        vt100::Color::Idx(i @ 0..16) => BASE[i as usize],
        // The 6x6x6 cube, then 24 grays.
        vt100::Color::Idx(i @ 16..232) => {
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            let i = i - 16;
            rgb(level(i / 36), level(i / 6 % 6), level(i % 6))
        }
        vt100::Color::Idx(i) => {
            let v = 8 + (i - 232) * 10;
            rgb(v, v, v)
        }
        vt100::Color::Rgb(r, g, b) => rgb(r, g, b),
    }))
}

/// What a key sends to the shell, as xterm encodes it.
fn key_bytes(ks: &Keystroke, text: bool, app_cursor: bool) -> Option<Vec<u8>> {
    let m = ks.modifiers;
    if let Some(ch) = &ks.key_char {
        if text || !(m.control || m.alt) {
            return Some(ch.as_bytes().to_vec());
        }
        if m.alt && !m.control {
            return Some([b"\x1b", ch.as_bytes()].concat());
        }
    }
    let modifier = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.control as u8;
    let cursor = |end: char| match (modifier, app_cursor) {
        (1, false) => format!("\x1b[{end}"),
        (1, true) => format!("\x1bO{end}"),
        _ => format!("\x1b[1;{modifier}{end}"),
    };
    let tilde = |n: u8| match modifier {
        1 => format!("\x1b[{n}~"),
        _ => format!("\x1b[{n};{modifier}~"),
    };
    let s = match ks.key.as_str() {
        "enter" => "\r".into(),
        "backspace" if m.alt => "\x1b\x7f".into(),
        "backspace" if m.control => "\x08".into(),
        "backspace" => "\x7f".into(),
        "tab" if m.shift => "\x1b[Z".into(),
        "tab" => "\t".into(),
        "escape" => "\x1b".into(),
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "space" if m.control => "\0".into(),
        key if key.len() > 1 && key.starts_with('f') => match key[1..].parse::<usize>().ok()? {
            n @ 1..=4 if modifier == 1 => format!("\x1bO{}", "PQRS".as_bytes()[n - 1] as char),
            n @ 1..=4 => format!("\x1b[1;{modifier}{}", "PQRS".as_bytes()[n - 1] as char),
            n @ 5..=12 => tilde([15, 17, 18, 19, 20, 21, 23, 24][n - 5]),
            _ => return None,
        },
        key if m.control => match key.as_bytes() {
            [b @ b'a'..=b'z'] => ((b - b'a' + 1) as char).to_string(),
            [b'['] => "\x1b".into(),
            [b'\\'] => "\x1c".into(),
            [b']'] => "\x1d".into(),
            _ => return None,
        },
        _ => return None,
    };
    Some(s.into_bytes())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use gpui_kit::Keystroke;

    use super::{Link, key_bytes, link, wheel_bytes, word_at};

    #[test]
    fn keys() {
        let key = |s: &str| key_bytes(&Keystroke::parse(s).unwrap(), false, false);
        assert_eq!(key("ctrl-c"), Some(b"\x03".to_vec()));
        assert_eq!(key("enter"), Some(b"\r".to_vec()));
        assert_eq!(key("ctrl-left"), Some(b"\x1b[1;5D".to_vec()));
        assert_eq!(key("shift-tab"), Some(b"\x1b[Z".to_vec()));
        assert_eq!(key("f5"), Some(b"\x1b[15~".to_vec()));
        assert_eq!(key("f1"), Some(b"\x1bOP".to_vec()));
        let up = Keystroke::parse("up").unwrap();
        assert_eq!(key_bytes(&up, false, true), Some(b"\x1bOA".to_vec()));
    }

    #[test]
    fn links() {
        let line: Vec<char> = "see (https://code.visualstudio.com/docs). or src/main.rs:12:5: err"
            .chars()
            .collect();
        let word = |col| word_at(&line, col).map(|(a, b)| line[a..b].iter().collect::<String>());
        assert_eq!(
            word(10).as_deref(),
            Some("https://code.visualstudio.com/docs")
        );
        assert_eq!(word(48).as_deref(), Some("src/main.rs:12:5"));
        assert_eq!(word(3), None);

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            link("src/main.rs:12:5", Some(root)),
            Some(Link::File(root.join("src/main.rs"), 12, 5))
        );
        assert_eq!(
            link("Cargo.toml", Some(root)),
            Some(Link::File(root.join("Cargo.toml"), 1, 1))
        );
        assert_eq!(link("nope.rs:3", Some(root)), None);
        assert!(matches!(link("http://x.y/z", None), Some(Link::Url(_))));
    }

    #[test]
    fn wheel() {
        use vt100::MouseProtocolEncoding::*;
        assert_eq!(wheel_bytes(true, (4, 9), Sgr), b"\x1b[<64;10;5M");
        assert_eq!(wheel_bytes(false, (0, 0), Default), b"\x1b[M\x61\x21\x21");
    }
}
