//! The integrated terminal: the shell on a ConPTY, its output parsed by vt100
//! into a grid that's drawn one text run per style per row.

use std::{
    cell::Cell,
    io::{Read as _, Write},
    rc::Rc,
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

pub(super) struct Terminal {
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
    /// Wheel movement not yet a whole line.
    scroll: f32,
    _task: Task<()>,
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.killer.kill().ok();
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

/// VS Code's default profile on Windows: PowerShell 7 when it's installed,
/// else Windows PowerShell.
fn shell() -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("pwsh.exe"))
        .find(|p| p.is_file())
        .map_or("powershell.exe".into(), |p| p.display().to_string())
}

impl Workbench {
    fn spawn_terminal(&self, cx: &mut Context<Self>) -> Result<Terminal, String> {
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| e.to_string())?;
        let mut cmd = CommandBuilder::new(shell());
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
        let task = cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                let done = this.update(cx, |this, cx| this.on_terminal(msg, cx));
                if done.unwrap_or(true) {
                    break;
                }
            }
        });

        Ok(Terminal {
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
            scroll: 0.,
            _task: task,
        })
    }

    /// Output from the shell; true once it exited.
    fn on_terminal(&mut self, msg: Msg, cx: &mut Context<Self>) -> bool {
        let Some(Ok(t)) = &mut self.terminal else {
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
                self.kill_terminal(cx);
                true
            }
        }
    }

    /// The trash button, or the shell exited: VS Code closes the panel with
    /// its last terminal.
    pub(super) fn kill_terminal(&mut self, cx: &mut Context<Self>) {
        self.terminal = None;
        if self.panel_tab == TERMINAL {
            self.panel = false;
        }
        cx.notify();
    }

    /// Ctrl+Shift+` and the + button.
    // ponytail: one terminal; + focuses it rather than adding a second, and
    // split does nothing. A Vec and VS Code's tab list on the right when needed.
    pub(super) fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel = true;
        self.panel_tab = TERMINAL;
        if !matches!(self.terminal, Some(Ok(_))) {
            self.terminal = Some(self.spawn_terminal(cx));
        }
        if let Some(Ok(t)) = &self.terminal {
            window.focus(&t.focus, cx);
        }
        cx.notify();
    }

    pub(super) fn terminal_focused(&self, window: &Window) -> bool {
        matches!(&self.terminal, Some(Ok(t)) if t.focus.is_focused(window))
    }

    fn write_terminal(&mut self, bytes: &[u8]) {
        let Some(Ok(t)) = &mut self.terminal else {
            return;
        };
        t.selection = None;
        t.parser.screen_mut().set_scrollback(0);
        t.writer.write_all(bytes).ok();
        t.writer.flush().ok();
    }

    fn terminal_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(Ok(t)) = &mut self.terminal else {
            return;
        };
        let ks = &ev.keystroke;
        let ctrl = ks.modifiers.control && !ks.modifiers.alt;
        // Ctrl+C copies when there's a selection, else interrupts.
        if ctrl && ks.key == "c" && t.selection.is_some_and(|(a, b)| a != b) {
            let ((r0, c0), (r1, c1)) = t.selection.take().map(sorted).unwrap();
            let text = t.parser.screen().contents_between(r0, c0, r1, c1);
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        } else if ctrl && ks.key == "v" {
            let text = cx.read_from_clipboard().and_then(|c| c.text());
            let text = text
                .unwrap_or_default()
                .replace("\r\n", "\r")
                .replace('\n', "\r");
            let bytes = if t.parser.screen().bracketed_paste() {
                format!("\x1b[200~{text}\x1b[201~")
            } else {
                text
            };
            self.write_terminal(bytes.as_bytes());
        } else if let Some(bytes) = key_bytes(
            ks,
            ev.prefer_character_input,
            t.parser.screen().application_cursor(),
        ) {
            self.write_terminal(&bytes);
        } else {
            return;
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The cell boundary nearest the mouse.
    fn terminal_cell(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
        let Some(Ok(t)) = &self.terminal else {
            return None;
        };
        let (rows, cols) = t.parser.screen().size();
        let at = position - t.bounds.get().origin;
        let col = ((f32::from(at.x) - PAD_L) / t.cell_w).round();
        let row = (f32::from(at.y) / LINE_H).floor();
        Some((
            (row.max(0.) as u16).min(rows.get() - 1),
            (col.max(0.) as u16).min(cols.get()),
        ))
    }

    pub(super) fn render_terminal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.terminal.is_none() {
            self.terminal = Some(self.spawn_terminal(cx));
        }
        let t = match &mut self.terminal {
            Some(Ok(t)) => t,
            Some(Err(err)) => {
                return div()
                    .px(px(PAD_L))
                    .child(format!("The terminal process failed to launch: {err}."))
                    .into_any_element();
            }
            None => unreachable!(),
        };

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
                    underline: cell.underline().then_some(UnderlineStyle {
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
            .id("terminal")
            .relative()
            .size_full()
            .overflow_hidden()
            .track_focus(&t.focus)
            .key_context("Terminal")
            .font_family(FONT)
            .text_size(px(FONT_SIZE))
            .line_height(px(LINE_H))
            .text_color(fg0)
            .cursor_text()
            .on_key_down(cx.listener(|this, ev, _, cx| this.terminal_key(ev, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    let cell = this.terminal_cell(ev.position);
                    if let Some(Ok(t)) = &mut this.terminal {
                        window.focus(&t.focus, cx);
                        t.selection = cell.map(|c| (c, c));
                        t.selecting = true;
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                let cell = this.terminal_cell(ev.position);
                if let Some(Ok(t)) = &mut this.terminal
                    && t.selecting
                    && let (Some((a, _)), Some(b)) = (t.selection, cell)
                {
                    t.selecting = ev.pressed_button == Some(MouseButton::Left);
                    t.selection = Some((a, b));
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    if let Some(Ok(t)) = &mut this.terminal {
                        t.selecting = false;
                    }
                }),
            )
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                let Some(Ok(t)) = &mut this.terminal else {
                    return;
                };
                t.scroll += f32::from(ev.delta.pixel_delta(px(LINE_H)).y) / LINE_H;
                let lines = t.scroll.trunc();
                t.scroll -= lines;
                let screen = t.parser.screen_mut();
                let top = (screen.scrollback() as f32 + lines).max(0.) as usize;
                screen.set_scrollback(top);
                cx.notify();
            }))
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
            .child(v_flex().pl(px(PAD_L)).children(lines))
            .children(cursor)
            .into_any_element()
    }
}

fn sorted<T: Ord>((a, b): (T, T)) -> (T, T) {
    if a <= b { (a, b) } else { (b, a) }
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
    use gpui_kit::Keystroke;

    use super::key_bytes;

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
}
