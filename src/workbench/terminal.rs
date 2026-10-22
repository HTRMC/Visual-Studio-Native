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

/// A selection's start and end, as (line, col) cell boundaries.
type Span = ((usize, u16), (usize, u16));

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
    /// What it was started with, for splits of it.
    profile: Profile,
    parser: vt100::Parser<Replies>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    focus: FocusHandle,
    /// Where the grid was last laid out, to size the pty and hit-test the mouse.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    cell_w: f32,
    /// Drag start and end as cell boundaries (line, col), lines counted from
    /// the oldest in the scrollback, so a selection scrolls with its text.
    // ponytail: once the scrollback is full, output trims its oldest lines
    // and shifts a standing selection up; count trimmed lines if that bites.
    selection: Option<Span>,
    selecting: bool,
    /// The word, or with true the line, a double or triple click selected;
    /// a drag from it extends by whole words or lines.
    unit: Option<(bool, Span)>,
    /// Alt+drag selects the columns between the ends on each line.
    block: bool,
    /// The mouse while dragging, and whether a timer scrolls toward it.
    drag_at: Point<Pixels>,
    autoscrolling: bool,
    /// The link under the mouse: row and column range.
    hover: Option<(u16, u16, u16)>,
    /// Wheel movement not yet a whole line.
    scroll: f32,
    /// The cell the last mouse report was for, to send motion once per cell.
    mouse_cell: Option<(u16, u16)>,
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

    /// The line shown on the screen's top row.
    fn top(&self) -> usize {
        let screen = self.parser.screen();
        screen.scrollback_len() - screen.scrollback()
    }

    /// The cell boundary nearest the mouse, as a selection end.
    fn selection_end(&self, position: Point<Pixels>) -> (usize, u16) {
        let (row, col) = self.cell(position, true);
        (self.top() + row as usize, col)
    }

    /// A visible row, one char per cell.
    fn row_chars(&self, row: u16) -> Vec<char> {
        let screen = self.parser.screen();
        (0..screen.size().1.get())
            .map(|col| {
                let cell = screen.cell(row, col);
                cell.and_then(|c| c.contents().chars().next())
                    .unwrap_or(' ')
            })
            .collect()
    }

    /// The word under the mouse or, with `line`, its whole wrapped line.
    fn unit_at(&self, position: Point<Pixels>, line: bool) -> Span {
        let screen = self.parser.screen();
        let (row, col) = self.cell(position, false);
        let top = self.top();
        if line {
            let (rows, cols) = screen.size();
            let (mut r0, mut r1) = (row, row);
            while r0 > 0 && screen.row_wrapped(r0 - 1) {
                r0 -= 1;
            }
            while r1 < rows.get() - 1 && screen.row_wrapped(r1) {
                r1 += 1;
            }
            return ((top + r0 as usize, 0), (top + r1 as usize, cols.get()));
        }
        let (a, b) = word_span(&self.row_chars(row), col as usize);
        (
            (top + row as usize, a as u16),
            (top + row as usize, b as u16),
        )
    }

    /// The selection with its end dragged to the mouse.
    fn dragged(&self) -> Option<Span> {
        let (a, _) = self.selection?;
        let Some((line, (start, end))) = self.unit else {
            return Some((a, self.selection_end(self.drag_at)));
        };
        // Whole units, keeping the clicked one.
        let (c, d) = self.unit_at(self.drag_at, line);
        Some(if c < start { (end, c) } else { (start, d) })
    }

    /// Lines to scroll while a drag is above (+) or below (-) the terminal,
    /// more the further away it is.
    fn overshoot(&self) -> isize {
        let b = self.bounds.get();
        let y = self.drag_at.y;
        let lines = |d: Pixels| 1 + (f32::from(d) / LINE_H) as isize;
        if y < b.top() {
            lines(b.top() - y)
        } else if y >= b.bottom() {
            -lines(y - b.bottom())
        } else {
            0
        }
    }

    /// The link under the mouse, if any. Paths resolve against the shell's
    /// cwd, else `root`.
    fn link(&self, position: Point<Pixels>, root: Option<&Path>) -> Option<(u16, u16, u16, Link)> {
        let root = self.parser.callbacks().cwd.as_deref().or(root);
        let (row, col) = self.cell(position, false);
        let line = self.row_chars(row);
        let (a, b) = word_at(&line, col as usize)?;
        let link = link(&line[a..b].iter().collect::<String>(), root)?;
        Some((row, a as u16, b as u16, link))
    }

    /// Sends a mouse report when the program asked for this kind of event.
    fn report(&mut self, button: u8, release: bool, position: Point<Pixels>) {
        use vt100::MouseProtocolMode::*;
        let screen = self.parser.screen();
        let wanted = match screen.mouse_protocol_mode() {
            None => false,
            Press => !release && button < 32,
            PressRelease => button < 32,
            ButtonMotion => button != 35,
            AnyMotion => true,
        };
        let cell = self.cell(position, false);
        if !wanted || (button >= 32 && self.mouse_cell == Some(cell)) {
            return;
        }
        self.mouse_cell = Some(cell);
        let bytes = mouse_bytes(button, release, cell, screen.mouse_protocol_encoding());
        self.write(&bytes);
    }

    /// Mouse events belong to the program when it asked for them; Shift
    /// keeps them for selecting, as in xterm.
    fn mouse_mode(&self, modifiers: Modifiers) -> bool {
        self.parser.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
            && !modifiers.shift
    }

    /// Re-wraps the lines to a new width; see `reflow`.
    fn resize(&mut self, rows: u16, cols: u16) {
        self.selection = None;
        self.hover = None;
        reflow(&mut self.parser, rows, cols);
    }
}

/// Re-wraps the lines to a new width, as conhost does on its side of the pty
/// (it doesn't repaint after a resize). Conhost only holds the screen, so
/// the screen is re-wrapped from the top on its own, below the scrollback,
/// and the cursor lands where PSReadLine's absolute moves expect it. The
/// alternate screen is left to its program to redraw.
fn reflow<C: vt100::Callbacks>(parser: &mut vt100::Parser<C>, rows: u16, cols: u16) {
    let size = |n: u16| n.max(1).try_into().unwrap();
    let screen = parser.screen_mut();
    if screen.alternate_screen() {
        return screen.set_size(size(rows), size(cols));
    }
    let height = screen.size().0.get();
    let (row, col) = screen.cursor_position();
    let attrs = screen.attributes_formatted();
    let (mut back, mut text, mut sgr) = (String::new(), String::new(), String::new());
    // The scrollback a screenful at a time.
    let len = screen.scrollback_len();
    for top in (0..len).step_by(height as usize) {
        screen.set_scrollback(len - top);
        for r in 0..(len - top).min(height as usize) as u16 {
            line(screen, r, &mut sgr, &mut back);
        }
    }
    if !back.is_empty() && !back.ends_with('\n') {
        back.push_str("\r\n");
    }
    screen.set_scrollback(0);
    for r in 0..row {
        line(screen, r, &mut sgr, &mut text);
    }
    cells(screen, row, 0..col, &mut sgr, &mut text);
    // What's right of and below the cursor, e.g. mid-edit or a status line.
    let last = (row..height)
        .rev()
        .find(|&r| line_end(screen, r) > 0)
        .unwrap_or(row);
    let mut rest = String::new();
    for r in row..=last {
        let from = if r == row { col } else { 0 };
        cells(
            screen,
            r,
            from..line_end(screen, r).max(from),
            &mut sgr,
            &mut rest,
        );
        if r < last && !screen.row_wrapped(r) {
            rest.push_str("\r\n");
        }
    }

    screen.clear_scrollback();
    screen.set_size(size(rows), size(cols));
    parser.process(b"\x1b[0m\x1b[H\x1b[2J");
    parser.process(back.as_bytes());
    // Scroll what's above into the scrollback, leaving the screen empty.
    let (r, _) = parser.screen().cursor_position();
    let rows = parser.screen().size().0.get();
    parser.process(format!("\x1b[{rows};1H{}\x1b[H", "\n".repeat(r as usize)).as_bytes());
    parser.process(text.as_bytes());
    if !rest.is_empty() {
        let (r, c) = parser.screen().cursor_position();
        let before = parser.screen().scrollback_len();
        parser.process(rest.as_bytes());
        let scrolled = parser.screen().scrollback_len() - before;
        let r = r.saturating_sub(scrolled as u16);
        parser.process(format!("\x1b[{};{}H", r + 1, c + 1).as_bytes());
    }
    parser.process(attrs.as_bytes());
}

/// A row's cells as text with SGR codes, then a newline unless it wraps.
fn line(screen: &vt100::Screen, row: u16, sgr: &mut String, out: &mut String) {
    cells(screen, row, 0..line_end(screen, row), sgr, out);
    if !screen.row_wrapped(row) {
        out.push_str("\r\n");
    }
}

/// Where a row's content ends: its width when it wraps, else after the last
/// cell that shows something.
fn line_end(screen: &vt100::Screen, row: u16) -> u16 {
    let cols = screen.size().1.get();
    if screen.row_wrapped(row) {
        return cols;
    }
    (0..cols)
        .rev()
        .find(|&col| {
            screen.cell(row, col).is_some_and(|c| {
                c.has_contents() || c.inverse() || c.bgcolor() != vt100::Color::Default
            })
        })
        .map_or(0, |col| col + 1)
}

/// Cells as text, with an SGR code wherever the style changes from `sgr`.
fn cells(
    screen: &vt100::Screen,
    row: u16,
    cols: std::ops::Range<u16>,
    sgr: &mut String,
    out: &mut String,
) {
    for col in cols {
        let Some(cell) = screen.cell(row, col) else {
            break;
        };
        if cell.is_wide_continuation() {
            continue;
        }
        let mut s = String::from("\x1b[0");
        for (on, code) in [
            (cell.bold(), ";1"),
            (cell.dim(), ";2"),
            (cell.italic(), ";3"),
            (cell.underline(), ";4"),
            (cell.inverse(), ";7"),
        ] {
            if on {
                s.push_str(code);
            }
        }
        for (color, base) in [(cell.fgcolor(), 38), (cell.bgcolor(), 48)] {
            match color {
                vt100::Color::Default => {}
                vt100::Color::Idx(i) => s.push_str(&format!(";{base};5;{i}")),
                vt100::Color::Rgb(r, g, b) => s.push_str(&format!(";{base};2;{r};{g};{b}")),
            }
        }
        s.push('m');
        if s != *sgr {
            out.push_str(&s);
            *sgr = s;
        }
        out.push_str(if cell.has_contents() {
            cell.contents()
        } else {
            " "
        });
    }
}

/// Answers the queries a program waits on (ConPTY asks for the cursor
/// position before the shell starts), and tracks the shell's cwd.
#[derive(Default)]
struct Replies {
    out: Vec<u8>,
    cwd: Option<PathBuf>,
}

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
                write!(self.out, "\x1b[{};{}R", row + 1, col + 1).ok();
            }
            (None, 'c', [] | [[0]]) => self.out.extend_from_slice(b"\x1b[?1;2c"),
            _ => {}
        }
    }

    /// Windows Terminal's cwd sequence, which `PROMPT_HOOK` sends.
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        if let [b"9", b"9", path @ ..] = params {
            let path = String::from_utf8_lossy(&path.join(&b';')).into_owned();
            self.cwd = Some(path.trim_matches('"').into());
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

/// PowerShell's startup, done by hand so the prompt can be wrapped after the
/// profiles load: the banner `-Command` suppresses, the profiles, then a
/// prompt that reports the cwd (`OSC 9;9`, which ConPTY passes on) like VS
/// Code's shell integration.
const POWERSHELL_HOOK: &str = "\
    if ($PSVersionTable.PSEdition -eq 'Core') { \"PowerShell $($PSVersionTable.PSVersion)\" } \
    else { \"Windows PowerShell`nCopyright (C) Microsoft Corporation. All rights reserved.`n\" }; \
    foreach ($__vsn_p in $PROFILE.AllUsersAllHosts, $PROFILE.AllUsersCurrentHost, \
    $PROFILE.CurrentUserAllHosts, $PROFILE.CurrentUserCurrentHost) { if (Test-Path $__vsn_p) { . $__vsn_p } }; \
    $__vsn_prompt = $function:prompt; function global:prompt { \
    [char]27 + ']9;9;' + $PWD.ProviderPath + [char]7 + (& $__vsn_prompt) }";
const POWERSHELL_ARGS: &[&str] = &["-NoProfile", "-NoExit", "-Command", POWERSHELL_HOOK];

/// A shell the terminal can start, like VS Code's terminal profiles. Each
/// reports its cwd with `OSC 9;9`.
#[derive(Clone)]
pub(super) struct Profile {
    pub(super) name: SharedString,
    program: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

/// Starts the WSL user's login shell with a prompt hook that reports the
/// folder (OSC 9;9): PROMPT_COMMAND for bash and the like, a precmd hook from
/// a throwaway ZDOTDIR for zsh (which then reads the user's own files), and a
/// fish_prompt handler for fish.
const WSL_INIT: &str = r#"unset VSN_SHELL_INIT
s=$(getent passwd "$(id -un)" | cut -d: -f7)
s=${s:-${SHELL:-/bin/sh}}
cwd='printf "\033]9;9;%s\007" "$(wslpath -w "$PWD")"'
case ${s##*/} in
zsh)
  d=$(mktemp -d)
  cat > "$d/.zshenv" <<EOF
ZDOTDIR=\${VSN_ZDOTDIR:-\$HOME}; unset VSN_ZDOTDIR; rm -rf '$d'
[ -r "\$ZDOTDIR/.zshenv" ] && . "\$ZDOTDIR/.zshenv"
__vsn_cwd() { $cwd; }
autoload -Uz add-zsh-hook; add-zsh-hook precmd __vsn_cwd
EOF
  export VSN_ZDOTDIR="$ZDOTDIR" ZDOTDIR="$d"
  exec "$s" -l;;
fish)
  exec "$s" -l -C 'function __vsn_cwd --on-event fish_prompt; printf "\033]9;9;%s\007" (wslpath -w $PWD); end';;
*)
  export PROMPT_COMMAND="$cwd"
  exec "$s" -l;;
esac"#;

/// VS Code's setting for which profile new terminals start with.
const DEFAULT_PROFILE: &str = "terminal.integrated.defaultProfile.windows";

/// The profiles VS Code detects on Windows, the default first.
pub(super) fn profiles() -> Vec<Profile> {
    let find = |exe: &str| {
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path)
            .map(|dir| dir.join(exe))
            .find(|p| p.is_file())
    };
    // Git's own bin\bash.exe, from cmd\git.exe or mingw64\bin\git.exe.
    let git_bash = find("git.exe").and_then(|git| {
        (git.ancestors().skip(2).take(2))
            .map(|dir| dir.join("bin").join("bash.exe"))
            .find(|bash| bash.is_file())
    });
    let profile = |name: &str, program: PathBuf, args: &[&str], env: &[(&str, &str)]| Profile {
        name: name.to_string().into(),
        program,
        args: args.iter().map(|a| a.to_string()).collect(),
        env: env.iter().map(|&(k, v)| (k.into(), v.into())).collect(),
    };
    let bash_cwd = |to_windows: &str| format!(r#"printf '\e]9;9;%s\a' "$({to_windows} "$PWD")""#);
    // WSLENV carries the launcher script over into the distro.
    let wslenv = match std::env::var("WSLENV") {
        Ok(v) if !v.is_empty() => format!("{v}:VSN_SHELL_INIT/u"),
        _ => "VSN_SHELL_INIT/u".into(),
    };
    let wsl = wsl_distros().iter().map(|distro| Profile {
        name: format!("{distro} (WSL)").into(),
        program: "wsl.exe".into(),
        args: [
            "-d",
            distro,
            "-e",
            "/bin/sh",
            "-c",
            r#"eval "$VSN_SHELL_INIT""#,
        ]
        .map(Into::into)
        .into(),
        env: vec![
            ("VSN_SHELL_INIT".into(), WSL_INIT.into()),
            ("WSLENV".into(), wslenv.clone()),
        ],
    });
    let mut list: Vec<Profile> = [
        find("pwsh.exe").map(|p| profile("PowerShell", p, POWERSHELL_ARGS, &[])),
        Some(profile(
            "Windows PowerShell",
            "powershell.exe".into(),
            POWERSHELL_ARGS,
            &[],
        )),
        Some(profile(
            "Command Prompt",
            "cmd.exe".into(),
            &[],
            &[("PROMPT", "$e]9;9;$P$e\\$P$G")],
        )),
        git_bash.map(|p| {
            let mut git_bash = profile("Git Bash", p, &["--login", "-i"], &[]);
            git_bash
                .env
                .push(("PROMPT_COMMAND".into(), bash_cwd("cygpath -w")));
            git_bash
        }),
    ]
    .into_iter()
    .flatten()
    .chain(wsl)
    .collect();
    let default = settings()[DEFAULT_PROFILE].as_str().map(str::to_owned);
    if let Some(ix) = list
        .iter()
        .position(|p| Some(&*p.name) == default.as_deref())
    {
        let default = list.remove(ix);
        list.insert(0, default);
    }
    list
}

/// The installed WSL distros, as `wsl -l -q` lists them (in UTF-16), minus
/// Docker's internal ones, which VS Code hides too. Asked once.
fn wsl_distros() -> &'static [String] {
    static DISTROS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    DISTROS.get_or_init(|| {
        let mut wsl = std::process::Command::new("wsl.exe");
        wsl.args(["-l", "-q"]);
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut wsl, 0x0800_0000); // CREATE_NO_WINDOW
        let Some(out) = wsl.output().ok().filter(|o| o.status.success()) else {
            return Vec::new();
        };
        let utf16: Vec<u16> = (out.stdout.as_chunks().0.iter())
            .map(|&b| u16::from_le_bytes(b))
            .collect();
        (String::from_utf16_lossy(&utf16).lines())
            .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\0'))
            .filter(|l| !l.is_empty() && !l.starts_with("docker-desktop"))
            .map(Into::into)
            .collect()
    })
}

/// User settings, a subset of VS Code's `settings.json`.
fn settings_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("vsn").join("settings.json"))
}

fn settings() -> serde_json::Value {
    settings_path()
        .and_then(|p| fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub(super) fn set_default_profile(name: &str) {
    let Some(path) = settings_path() else { return };
    let mut settings = settings();
    if !settings.is_object() {
        settings = serde_json::json!({});
    }
    settings[DEFAULT_PROFILE] = name.into();
    fs::create_dir_all(path.parent().unwrap()).ok();
    fs::write(path, serde_json::to_string_pretty(&settings).unwrap()).ok();
}

impl Workbench {
    fn spawn_terminal(
        &mut self,
        profile: Profile,
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
        let mut cmd = CommandBuilder::new(&profile.program);
        cmd.args(&profile.args);
        for (k, v) in &profile.env {
            cmd.env(k, v);
        }
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
            name: profile
                .program
                .file_stem()
                .map_or("shell".into(), |s| s.to_string_lossy().into()),
            profile,
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
            unit: None,
            block: false,
            drag_at: Point::default(),
            autoscrolling: false,
            hover: None,
            scroll: 0.,
            mouse_cell: None,
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
                let replies = std::mem::take(&mut t.parser.callbacks_mut().out);
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
    /// active. A split runs the active one's profile, else the default.
    /// False when it failed to launch.
    fn add_terminal(
        &mut self,
        split: bool,
        profile: Option<Profile>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let active = self.terminals.list.get(self.terminals.active);
        let profile = profile
            .or_else(|| active.filter(|_| split).map(|t| t.profile.clone()))
            .unwrap_or_else(|| profiles().remove(0));
        let mut t = match self.spawn_terminal(profile, window, cx) {
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
        self.add_terminal(split, None, window, cx);
        self.focus_terminal(window, cx);
    }

    /// The profile list beside the + button.
    pub(super) fn launch_profile(
        &mut self,
        profile: Profile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_terminal(false, Some(profile), window, cx);
        self.focus_terminal(window, cx);
    }

    /// Ctrl+`: shows the panel and focuses the active terminal, starting one
    /// if there's none.
    pub(super) fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel = true;
        self.panel_tab = TERMINAL;
        if self.terminals.list.is_empty() {
            self.add_terminal(false, None, window, cx);
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
        let Some(((l0, c0), (l1, c1))) = t.selection.filter(|(a, b)| a != b).map(sorted) else {
            return false;
        };
        let screen = t.parser.screen_mut();
        let text = match t.block {
            true => (l0..=l1)
                .map(|l| text_between(screen, (l, c0.min(c1)), (l, c0.max(c1))))
                .collect::<Vec<_>>()
                .join(
                    "
",
                ),
            false => text_between(screen, (l0, c0), (l1, c1)),
        };
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

    /// The scrollback and the screen.
    fn select_all_terminal(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(t) = self.terminal(id) {
            let screen = t.parser.screen();
            let (rows, cols) = screen.size();
            let last = screen.scrollback_len() + rows.get() as usize - 1;
            t.selection = Some(((0, 0), (last, cols.get())));
            t.block = false;
            cx.notify();
        }
    }

    /// Drops the scrollback and has the shell clear the screen with Ctrl+L,
    /// so ConPTY's copy of the screen, which it places the cursor by, is
    /// cleared too. PSReadLine redraws the prompt with what's typed;
    /// full-screen programs take it as a redraw.
    fn clear_terminal(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(t) = self.terminal(id) {
            t.parser.screen_mut().clear_scrollback();
            t.write(b"\x0c");
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
            let report = mouse_bytes(
                if up { 64 } else { 65 },
                false,
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

    /// The middle and right buttons: to the program in mouse mode, where the
    /// right one doesn't open the context menu.
    fn terminal_press(
        &mut self,
        id: u64,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(t) = self.terminal(id)
            && t.mouse_mode(ev.modifiers)
            && let Some(button) = button_code(ev.button)
        {
            t.report(button, false, ev.position);
            cx.stop_propagation();
        }
        self.activate_terminal(id, window, cx);
    }

    /// Extends a drag selection to the mouse, scrolling while it is above or
    /// below the terminal.
    fn drag_terminal(&mut self, id: u64, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(t) = self.terminal(id) else {
            return;
        };
        if t.selection.is_none() {
            return;
        }
        t.selecting = ev.pressed_button == Some(MouseButton::Left);
        t.drag_at = ev.position;
        t.selection = t.dragged();
        if t.selecting && t.overshoot() != 0 && !t.autoscrolling {
            t.autoscrolling = true;
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(50))
                        .await;
                    let going = this.update(cx, |this, cx| this.autoscroll_terminal(id, cx));
                    if !going.unwrap_or(false) {
                        break;
                    }
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// One tick of scrolling toward a drag outside the terminal; false to stop.
    fn autoscroll_terminal(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        let Some(t) = self.terminal(id) else {
            return false;
        };
        let n = t.overshoot();
        if !t.selecting || n == 0 {
            t.autoscrolling = false;
            return false;
        }
        let screen = t.parser.screen_mut();
        screen.set_scrollback(screen.scrollback().saturating_add_signed(n));
        t.selection = t.dragged();
        cx.notify();
        true
    }

    fn terminal_release(&mut self, id: u64, ev: &MouseUpEvent) {
        let Some(t) = self.terminal(id) else {
            return;
        };
        if !t.selecting
            && t.mouse_mode(ev.modifiers)
            && let Some(button) = button_code(ev.button)
        {
            t.report(button, true, ev.position);
        }
        if ev.button == MouseButton::Left {
            // Alt+click without a drag moves the prompt cursor there.
            if t.selecting
                && t.block
                && t.selection.is_some_and(|(a, b)| a == b)
                && t.parser.screen().scrollback() == 0
                && let Some(n) = cursor_moves(t.parser.screen(), t.cell(ev.position, false))
            {
                let arrow = match (t.parser.screen().application_cursor(), n < 0) {
                    (false, false) => "[C",
                    (false, true) => "[D",
                    (true, false) => "OC",
                    (true, true) => "OD",
                };
                t.write(arrow.repeat(n.unsigned_abs()).as_bytes());
            }
            t.selecting = false;
        }
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
            self.add_terminal(false, None, window, cx);
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

    /// The chevron beside +: starts a terminal with a chosen profile.
    pub(super) fn profile_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let wb = cx.weak_entity();
        Button::new("terminal-profiles")
            .ghost()
            .compact()
            .size(px(22.))
            .text_color(col::fg())
            .child(codicon(icon::CHEVRON_DOWN))
            .dropdown_menu(move |mut menu, _, _| {
                for profile in profiles() {
                    let wb = wb.clone();
                    let name = profile.name.clone();
                    menu = menu.item(PopupMenuItem::new(name).on_click(move |_, window, cx| {
                        let profile = profile.clone();
                        wb.update(cx, |this, cx| this.launch_profile(profile, window, cx))
                            .ok();
                    }));
                }
                menu.separator()
                    .menu("Select Default Profile", Box::new(SelectDefaultProfile))
            })
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
                t.resize(rows, cols);
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
        let (selection, block) = (t.selection, t.block);
        let top = t.top();
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
                let at = (top + row as usize, col);
                if selection.is_some_and(|s| selected(s, block, at)) {
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
        let (selecting, drag) = (t.selecting, wb.clone());
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
                    if t.mouse_mode(ev.modifiers) {
                        t.report(0, false, ev.position);
                        return this.activate_terminal(id, window, cx);
                    }
                    t.selecting = true;
                    t.block = ev.modifiers.alt && ev.click_count < 2;
                    let line = ev.click_count > 2;
                    t.unit = (ev.click_count > 1).then(|| (line, t.unit_at(ev.position, line)));
                    let end = t.selection_end(ev.position);
                    t.selection = Some(match t.unit {
                        Some((_, unit)) => unit,
                        // Shift+click extends from the selection's anchor.
                        None if ev.modifiers.shift => (t.selection.map_or(end, |(a, _)| a), end),
                        None => (end, end),
                    });
                    this.activate_terminal(id, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, ev, window, cx| this.terminal_press(id, ev, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev, window, cx| this.terminal_press(id, ev, window, cx)),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                let root = this.root.clone();
                let Some(t) = this.terminal(id) else {
                    return;
                };
                if !t.selecting && t.mouse_mode(ev.modifiers) {
                    let held = ev.pressed_button.and_then(button_code);
                    t.report(held.map_or(35, |b| b + 32), false, ev.position);
                } else if t.selecting {
                    this.drag_terminal(id, ev, cx);
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
                cx.listener(move |this, ev, _, _| this.terminal_release(id, ev)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _, _, _| {
                    if let Some(t) = this.terminal(id) {
                        t.selecting = false;
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, ev, _, _| this.terminal_release(id, ev)),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(move |this, ev, _, _| this.terminal_release(id, ev)),
            )
            .on_scroll_wheel(cx.listener(move |this, ev, _, cx| this.terminal_wheel(id, ev, cx)))
            .child(
                canvas(
                    move |b, window, _| {
                        if bounds.replace(b) != b {
                            window.refresh();
                        }
                    },
                    move |b, _, window, _| {
                        // Mouse moves outside reach only the window.
                        if selecting {
                            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                                if phase == DispatchPhase::Bubble && !b.contains(&ev.position) {
                                    drag.update(cx, |this, cx| this.drag_terminal(id, ev, cx))
                                        .ok();
                                }
                            });
                        }
                    },
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

/// The text between two selection ends, a screenful of scrollback at a time.
fn text_between(
    screen: &mut vt100::Screen,
    (l0, c0): (usize, u16),
    (l1, c1): (usize, u16),
) -> String {
    let (saved, len) = (screen.scrollback(), screen.scrollback_len());
    let (rows, cols) = (screen.size().0.get() as usize, screen.size().1.get());
    let l1 = l1.min(len + rows - 1);
    let mut text = String::new();
    let mut line = l0;
    while line <= l1 {
        // `line` on the top row, or as near as the scrollback goes.
        screen.set_scrollback(len.saturating_sub(line));
        let top = len - screen.scrollback();
        let last = l1.min(top + rows - 1);
        let (a, b) = ((line - top) as u16, (last - top) as u16);
        let start = if line == l0 { c0 } else { 0 };
        let end = if last == l1 { c1 } else { cols };
        text += &screen.contents_between(a, start, b, end);
        if last < l1 && !screen.row_wrapped(b) {
            text.push('\n');
        }
        line = last + 1;
    }
    screen.set_scrollback(saved);
    text
}

/// The word around `col` for a double click, split on VS Code's default
/// `terminal.integrated.wordSeparators`; a separator selects only itself.
fn word_span(line: &[char], col: usize) -> (usize, usize) {
    let sep = |c: char| " ()[]{}',\"`─‘’“”|".contains(c);
    if line.get(col).is_none_or(|&c| sep(c)) {
        return (col, col + 1);
    }
    let mut a = col;
    while a > 0 && !sep(line[a - 1]) {
        a -= 1;
    }
    let mut b = col + 1;
    while b < line.len() && !sep(line[b]) {
        b += 1;
    }
    (a, b)
}

/// The arrow presses, + right and - left, that walk the cursor to the visible
/// cell `to` along its wrapped line, or None when `to` is on another line.
// ponytail: horizontal only; full-screen programs need up/down moves too.
fn cursor_moves(screen: &vt100::Screen, (row, col): (u16, u16)) -> Option<isize> {
    let (crow, ccol) = screen.cursor_position();
    let cols = screen.size().1.get() as isize;
    let joined = (row.min(crow)..row.max(crow)).all(|r| screen.row_wrapped(r));
    (!screen.alternate_screen() && joined)
        .then(|| (row as isize - crow as isize) * cols + col as isize - ccol as isize)
}

/// Whether the cell at `at` is in a selection, or with `block` in the
/// columns between its ends.
fn selected(selection: Span, block: bool, at: (usize, u16)) -> bool {
    let ((l0, c0), (l1, c1)) = sorted(selection);
    match block {
        true => (l0..=l1).contains(&at.0) && (c0.min(c1)..c0.max(c1)).contains(&at.1),
        false => (l0, c0) <= at && at < (l1, c1),
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

/// The xterm number of a mouse button.
fn button_code(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        _ => None,
    }
}

/// An xterm mouse report at a 0-based (row, col): `button` from
/// `button_code`, +32 for motion (35 with none held), 64/65 the wheel.
fn mouse_bytes(
    button: u8,
    release: bool,
    (row, col): (u16, u16),
    encoding: vt100::MouseProtocolEncoding,
) -> Vec<u8> {
    let (x, y) = (col as u32 + 1, row as u32 + 1);
    let button = button as u32;
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            let end = if release { 'm' } else { 'M' };
            format!("\x1b[<{button};{x};{y}{end}").into_bytes()
        }
        // The older encodings can't say which button was released.
        _ if release => mouse_bytes(3, false, (row, col), encoding),
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

    use super::{
        Link, cursor_moves, key_bytes, link, mouse_bytes, reflow, selected, text_between, word_at,
        word_span,
    };

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
        let span = |col| {
            let (a, b) = word_span(&line, col);
            line[a..b].iter().collect::<String>()
        };
        assert_eq!(span(10), "https://code.visualstudio.com/docs");
        assert_eq!(span(48), "src/main.rs:12:5:");
        assert_eq!(span(3), " ");

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
    fn mouse() {
        use vt100::MouseProtocolEncoding::*;
        assert_eq!(mouse_bytes(64, false, (4, 9), Sgr), b"\x1b[<64;10;5M");
        assert_eq!(mouse_bytes(0, true, (4, 9), Sgr), b"\x1b[<0;10;5m");
        assert_eq!(
            mouse_bytes(65, false, (0, 0), Default),
            b"\x1b[M\x61\x21\x21"
        );
        assert_eq!(mouse_bytes(0, true, (0, 0), Default), b"\x1b[M\x23\x21\x21");
    }

    #[test]
    fn reflows() {
        let mut p = vt100::Parser::new(4.try_into().unwrap(), 10.try_into().unwrap(), 100);
        // Scrollback "old", "", screen "", "", "> abcdefgh", "ijkl"; the
        // cursor after "> ", mid-edit.
        p.process(b"old\r\n\r\n\r\n\r\n> abcdefghijkl\x1b[3;3H");
        let mut q = vt100::Parser::new(3.try_into().unwrap(), 10.try_into().unwrap(), 100);
        q.process(b"> \r\nbelow\x1b[1;3H");
        reflow(&mut q, 3, 5);
        assert_eq!(q.screen().contents(), "> \nbelow");
        assert_eq!(q.screen().cursor_position(), (0, 2));
        reflow(&mut p, 3, 5);
        let rows: Vec<String> = p.screen().rows(0, 5).collect();
        assert_eq!(rows, ["> abc", "defgh", "ijkl"]);
        assert_eq!(p.screen().cursor_position(), (0, 2));
        reflow(&mut p, 3, 20);
        assert_eq!(p.screen().contents(), "> abcdefghijkl");
        assert_eq!(p.screen().cursor_position(), (0, 2));
        p.screen_mut().set_scrollback(100);
        assert!(p.screen().contents().starts_with("old"));
    }

    #[test]
    fn selects_through_scrollback() {
        let mut p = vt100::Parser::new(2.try_into().unwrap(), 4.try_into().unwrap(), 100);
        // Lines "l0", "l1", "l2", "abcdef" wrapped as "abcd", "ef", "end".
        p.process(b"l0\r\nl1\r\nl2\r\nabcdef\r\nend");
        let screen = p.screen_mut();
        screen.set_scrollback(1);
        assert_eq!(
            text_between(screen, (0, 0), (6, 4)),
            "l0\nl1\nl2\nabcdef\nend"
        );
        assert_eq!(text_between(screen, (1, 1), (4, 1)), "1\nl2\nabcde");
        assert_eq!(screen.scrollback(), 1);
        // A block dragged from (1, 3) back to (3, 1) covers columns 1 and 2.
        let block = ((1, 3), (3, 1));
        assert!(selected(block, true, (2, 1)) && selected(block, true, (3, 2)));
        assert!(!selected(block, true, (2, 3)) && !selected(block, true, (0, 1)));
        assert!(selected(block, false, (2, 3)));
    }

    #[test]
    fn alt_click_moves() {
        let mut parser = vt100::Parser::new(3.try_into().unwrap(), 10.try_into().unwrap(), 0);
        parser.process(b"abcdefghijklmno");
        let screen = parser.screen();
        assert_eq!(cursor_moves(screen, (1, 8)), Some(3));
        assert_eq!(cursor_moves(screen, (0, 2)), Some(-13));
        assert_eq!(cursor_moves(screen, (2, 0)), None);
    }
}
