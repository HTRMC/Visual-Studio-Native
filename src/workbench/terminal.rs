//! The integrated terminals: each a shell on a ConPTY, its output parsed by
//! vt100 into a grid that's drawn one text run per style per row.

use std::{
    cell::Cell,
    io::{Read as _, Write},
    rc::Rc,
    time::Instant,
};

use gpui_kit::base::input::{Escape, Search};
use gpui_kit::component::{
    input::Position,
    menu::{PopupMenu, PopupMenuItem},
};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use super::*;
use crate::theme::{c, ca, tip};

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

/// A link under the mouse: its wrapped line's first line, and its start and
/// end cells counted from there.
type Hover = (usize, usize, usize);

#[derive(Default)]
pub(super) struct Terminals {
    /// Split groups are runs of the same `group`, shown side by side.
    list: Vec<Terminal>,
    active: usize,
    /// Tabs Ctrl/Shift+clicked into a multi-selection, with the active one.
    picked: Vec<u64>,
    /// Why the last terminal failed to launch.
    error: Option<String>,
    next_id: u64,
    /// Find's input, made on first use, and whether it shows over the
    /// active terminal.
    find: Option<Entity<InputState>>,
    finding: bool,
    /// Match case, whole word, regex.
    find_opts: [bool; 3],
    /// Counts activations, for each terminal's `seen`.
    clock: u64,
}

impl Terminals {
    /// Marks the active terminal as the one last focused in its group.
    fn stamp(&mut self) {
        self.clock += 1;
        if let Some(t) = self.list.get_mut(self.active) {
            t.seen = self.clock;
        }
    }
}

pub(super) struct Terminal {
    id: u64,
    /// The id of the terminal its split group started with.
    group: u64,
    name: String,
    /// The tab icon's color, an index into `ANSI`, from Change Color.
    color: Option<usize>,
    /// The tab icon, from Change Icon.
    icon: char,
    /// When it was last made active, so its group returns to it.
    seen: u64,
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
    hover: Option<Hover>,
    /// Wheel movement not yet a whole line.
    scroll: f32,
    /// Toggle Size to Content Width's width, kept through panel resizes, and
    /// how far the grid is scrolled left to show it.
    fixed_cols: Option<u16>,
    scroll_x: f32,
    /// Which scrollbar's thumb a drag holds, and where along it.
    thumb: Option<(Axis, f32)>,
    /// Whether the mouse is over the terminal and since when, to fade the
    /// scrollbars; none until it first comes over.
    over: Option<(bool, Instant)>,
    /// The cell the last mouse report was for, to send motion once per cell.
    mouse_cell: Option<(u16, u16)>,
    /// Find's matches, oldest first, and the one selected.
    matches: Vec<Span>,
    current: Option<usize>,
    _task: Task<()>,
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.killer.kill().ok();
    }
}

impl Terminal {
    /// Moves the selection and find's matches up by `n` lines the scrollback
    /// dropped; an end that went with them clamps to the top.
    fn trim(&mut self, n: usize) {
        let at = |(line, col): (usize, u16)| line.checked_sub(n).map_or((0, 0), |l| (l, col));
        let shift = |(a, b): Span| (a.0.max(b.0) >= n).then(|| (at(a), at(b)));
        self.selection = self.selection.and_then(shift);
        self.unit = self.unit.and_then(|(line, s)| Some((line, shift(s)?)));
        let current = self.current.and_then(|i| shift(self.matches[i]));
        self.matches = self.matches.iter().filter_map(|&m| shift(m)).collect();
        self.current = current.and_then(|m| self.matches.iter().position(|&n| n == m));
    }

    /// Shows the scrollbars after a scroll, fading as if the mouse had just
    /// left, as VS Code does on any scroll.
    fn reveal(&mut self) {
        if !matches!(self.over, Some((true, _))) {
            self.over = Some((false, Instant::now()));
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.selection = None;
        if self.parser.screen().scrollback() > 0 {
            self.parser.screen_mut().set_scrollback(0);
            self.reveal();
        }
        self.writer.write_all(bytes).ok();
        self.writer.flush().ok();
    }

    /// The cell under the mouse, or with `round` the cell boundary nearest it.
    fn cell(&self, position: Point<Pixels>, round: bool) -> (u16, u16) {
        let (rows, cols) = self.parser.screen().size();
        let at = position - self.bounds.get().origin;
        let col = (f32::from(at.x) + self.scroll_x - PAD_L) / self.cell_w;
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

    /// The word under the mouse or, with `line`, its whole wrapped line.
    fn unit_at(&mut self, position: Point<Pixels>, whole: bool) -> Span {
        let (row, col) = self.cell(position, false);
        let line = self.top() + row as usize;
        let screen = self.parser.screen_mut();
        let cols = screen.size().1.get();
        if whole {
            let (l0, l1) = wrapped_lines(screen, line);
            return ((l0, 0), (l1, cols));
        }
        let cols = cols as usize;
        let (l0, chars) = self.wrapped_chars(line);
        let (a, b) = word_span(&chars, (line - l0) * cols + col as usize);
        let at = |i: usize| (l0 + i / cols, (i % cols) as u16);
        (at(a), at(b))
    }

    /// The first line of the wrapped line through `line`, and all its chars.
    fn wrapped_chars(&mut self, line: usize) -> (usize, Vec<char>) {
        let screen = self.parser.screen_mut();
        let (l0, l1) = wrapped_lines(screen, line);
        let mut chars = Vec::new();
        for l in l0..=l1 {
            chars.extend(at_line(screen, l, row_chars));
        }
        (l0, chars)
    }

    /// The selection with its end dragged to the mouse.
    fn dragged(&mut self) -> Option<Span> {
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

    /// The link under the mouse, if any, found along its whole wrapped
    /// line. Paths resolve against the shell's cwd, else `root`.
    fn link(&mut self, position: Point<Pixels>, root: Option<&Path>) -> Option<(Hover, Link)> {
        let (row, col) = self.cell(position, false);
        let at = self.top() + row as usize;
        let (l0, line) = self.wrapped_chars(at);
        let cols = self.parser.screen().size().1.get() as usize;
        let (a, b) = word_at(&line, (at - l0) * cols + col as usize)?;
        let root = self.parser.callbacks().cwd.as_deref().or(root);
        let link = link(&line[a..b].iter().collect::<String>(), root)?;
        Some(((l0, a, b), link))
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
    /// Lines a full scrollback dropped off its top, to move what's counted
    /// from it up with its text.
    trimmed: usize,
}

impl vt100::Callbacks for Replies {
    fn on_scroll(&mut self, _: vt100::capture::RowContents<'_>, alternate_screen: bool) {
        self.trimmed += usize::from(!alternate_screen);
    }

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
            name: shell_name(&profile),
            color: None,
            icon: icon::TERMINAL,
            seen: 0,
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
            fixed_cols: None,
            scroll_x: 0.,
            thumb: None,
            over: None,
            mouse_cell: None,
            matches: Vec::new(),
            current: None,
            _task: task,
        })
    }

    fn terminal(&mut self, id: u64) -> Option<&mut Terminal> {
        self.terminals.list.iter_mut().find(|t| t.id == id)
    }

    /// VS Code's terminal find widget: over the active terminal, finding as
    /// you type.
    pub(super) fn find_in_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminals.list.is_empty() {
            return;
        }
        let input = match &self.terminals.find {
            Some(input) => input.clone(),
            None => {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
                cx.subscribe(&input, |this, _, ev: &InputEvent, cx| match ev {
                    InputEvent::Change => this.search_terminal(cx),
                    // Enter goes up, to older output, as in VS Code.
                    InputEvent::PressEnter { shift, .. } => this.step_match(!shift, cx),
                    _ => {}
                })
                .detach();
                self.terminals.find = Some(input.clone());
                input
            }
        };
        self.terminals.finding = true;
        let seed = self.find_seed();
        // The query starts selected, so typing replaces it.
        input.update(cx, |s, cx| {
            if let Some(seed) = seed {
                s.set_value(seed, window, cx);
            }
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.search_terminal(cx);
    }

    /// The active terminal's selection as a query, as VS Code seeds find:
    /// one line, and not just find's own selected match.
    fn find_seed(&mut self) -> Option<String> {
        let t = self.terminals.list.get_mut(self.terminals.active)?;
        let (a, b) = t
            .selection
            .filter(|(a, b)| a != b && !t.block)
            .map(sorted)?;
        if t.current.is_some_and(|i| t.matches[i] == (a, b)) {
            return None;
        }
        Some(text_between(t.parser.screen_mut(), a, b)).filter(|s| !s.contains('\n'))
    }

    pub(super) fn close_terminal_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminals.finding = false;
        for t in &mut self.terminals.list {
            (t.matches, t.current) = (Vec::new(), None);
        }
        if let Some(t) = self.terminals.list.get(self.terminals.active) {
            window.focus(&t.focus, cx);
        }
        cx.notify();
    }

    /// Finds the query in the active terminal and selects the newest match.
    fn search_terminal(&mut self, cx: &mut Context<Self>) {
        let re = self.find_regex(cx);
        let Some(t) = self.terminals.list.get_mut(self.terminals.active) else {
            return;
        };
        t.matches = re.map_or(Vec::new(), |re| find_all(t.parser.screen_mut(), &re, 0));
        // The old match's selection goes with it.
        if t.current.is_some() {
            t.selection = None;
        }
        t.current = t.matches.len().checked_sub(1);
        self.show_match(cx);
    }

    fn active_id(&self) -> Option<u64> {
        self.terminals.list.get(self.terminals.active).map(|t| t.id)
    }

    /// With find open, find moves to a newly active terminal (`was` the one
    /// before), and closes with the last one.
    fn find_follows(&mut self, was: Option<u64>, cx: &mut Context<Self>) {
        if !self.terminals.finding || self.active_id() == was {
            return;
        }
        if self.terminals.list.is_empty() {
            self.terminals.finding = false;
            return;
        }
        let active = self.terminals.active;
        for (i, t) in self.terminals.list.iter_mut().enumerate() {
            if i != active {
                (t.matches, t.current) = (Vec::new(), None);
            }
        }
        self.search_terminal(cx);
    }

    /// Find's query as a regex, while find is open with one.
    fn find_regex(&self, cx: &App) -> Option<regex::Regex> {
        let input = self
            .terminals
            .find
            .as_ref()
            .filter(|_| self.terminals.finding)?;
        let query = input.read(cx).value();
        let [case, word, regex] = self.terminals.find_opts;
        (!query.is_empty())
            .then(|| super::search::build_regex(&query, case, word, regex).ok())
            .flatten()
    }

    /// Alt+C, Alt+W, Alt+R: flips match case, whole word or regex (`i`).
    fn toggle_find_opt(&mut self, i: usize, cx: &mut Context<Self>) {
        self.terminals.find_opts[i] ^= true;
        self.search_terminal(cx);
    }

    /// F3 and Shift+F3: the match below or above (`up`), opening find first
    /// if it's closed.
    pub(super) fn find_step(&mut self, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.terminals.finding {
            true => self.step_match(up, cx),
            false => self.find_in_terminal(window, cx),
        }
    }

    /// Selects the match above (`up`) or below the current one, wrapping.
    fn step_match(&mut self, up: bool, cx: &mut Context<Self>) {
        let Some(t) = self.terminals.list.get_mut(self.terminals.active) else {
            return;
        };
        let n = t.matches.len();
        t.current = match t.current {
            _ if n == 0 => None,
            None => Some(n - 1),
            Some(i) if up => Some((i + n - 1) % n),
            Some(i) => Some((i + 1) % n),
        };
        self.show_match(cx);
    }

    /// Selects the current match, scrolling it to the middle if it's off
    /// screen.
    fn show_match(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.terminals.list.get_mut(self.terminals.active) else {
            return;
        };
        if let Some(m) = t.current.map(|i| t.matches[i]) {
            (t.selection, t.block, t.unit) = (Some(m), false, None);
            let rows = t.parser.screen().size().0.get() as usize;
            let (top, line) = (t.top(), m.0.0);
            if line < top || line >= top + rows {
                let screen = t.parser.screen_mut();
                let len = screen.scrollback_len();
                screen.set_scrollback(len.saturating_sub(line.saturating_sub(rows / 2)));
                t.reveal();
            }
        }
        cx.notify();
    }

    /// The find widget over the active terminal: input, count, arrows, close.
    fn render_terminal_find(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let input = self
            .terminals
            .find
            .as_ref()
            .filter(|_| self.terminals.finding)?;
        let t = self.terminals.list.get(self.terminals.active)?;
        let count = match t.current {
            Some(i) => format!("{} of {}", i + 1, t.matches.len()),
            None if t.matches.is_empty() => "No results".into(),
            None => format!("? of {}", t.matches.len()),
        };
        type Act = fn(&mut Workbench, &mut Window, &mut Context<Workbench>);
        let button = |id: &'static str, ch: char, text: &'static str, act: Act| {
            action_button(id, ch)
                .size(px(20.))
                .tooltip(tip(text))
                .on_click(cx.listener(move |this, _, w, cx| act(this, w, cx)))
        };
        // A query that won't compile reddens the box, as in VS Code.
        let bad = !input.read(cx).value().is_empty() && self.find_regex(cx).is_none();
        let opts = [
            ("terminal-find-case", icon::CASE_SENSITIVE),
            ("terminal-find-word", icon::WHOLE_WORD),
            ("terminal-find-regex", icon::REGEX),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (id, ch))| {
            super::search::find_toggle(id, ch, self.terminals.find_opts[i])
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_find_opt(i, cx)))
        });
        Some(
            h_flex()
                .id("terminal-find")
                .key_context("TerminalFind")
                .absolute()
                .top_0()
                .right(px(PAD_R + 4.))
                .h(px(33.))
                .px(px(4.))
                .gap(px(2.))
                .bg(c(0x252526))
                .shadow_md()
                .text_size(px(13.))
                // Clicks stay off the terminal under it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                // Ctrl+F in the input is its own Search; reselect instead.
                .capture_action(cx.listener(|this, _: &Search, window, cx| {
                    cx.stop_propagation();
                    this.find_in_terminal(window, cx)
                }))
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                    let m = &ev.keystroke.modifiers;
                    let i = ["c", "w", "r"].iter().position(|k| *k == ev.keystroke.key);
                    if let Some(i) = i.filter(|_| m.alt && !m.control && !m.shift) {
                        cx.stop_propagation();
                        this.toggle_find_opt(i, cx);
                    }
                }))
                .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                    cx.stop_propagation();
                    this.close_terminal_find(window, cx)
                }))
                .child(
                    div()
                        .w(px(280.))
                        .border_1()
                        .border_color(if bad { c(0xBE1100) } else { col::accent() })
                        .bg(col::input())
                        .child(
                            Input::new(input)
                                .appearance(false)
                                .text_size(px(13.))
                                .suffix(h_flex().gap(px(2.)).children(opts)),
                        ),
                )
                .child(
                    div()
                        .min_w(px(68.))
                        .px(px(4.))
                        .text_size(px(12.))
                        .child(count),
                )
                .child(button(
                    "terminal-find-up",
                    icon::ARROW_UP,
                    "Previous Match (Shift+Enter)",
                    |this, _, cx| this.step_match(true, cx),
                ))
                .child(button(
                    "terminal-find-down",
                    icon::ARROW_DOWN,
                    "Next Match (Enter)",
                    |this, _, cx| this.step_match(false, cx),
                ))
                .child(button(
                    "terminal-find-close",
                    icon::CLOSE,
                    "Close (Escape)",
                    Self::close_terminal_find,
                )),
        )
    }

    /// The active terminal's id and name.
    pub(super) fn terminal_name(&self) -> Option<(u64, String)> {
        let t = self.terminals.list.get(self.terminals.active)?;
        Some((t.id, t.name.clone()))
    }

    /// An empty name goes back to the shell's, as in VS Code.
    pub(super) fn set_terminal_name(&mut self, id: u64, name: &str, cx: &mut Context<Self>) {
        for id in self.targets(id) {
            if let Some(t) = self.terminal(id) {
                t.name = match name.trim() {
                    "" => shell_name(&t.profile),
                    name => name.to_string(),
                };
            }
        }
        cx.notify();
    }

    /// A tab's menu acts on every selected tab when it's one of them.
    fn targets(&self, id: u64) -> Vec<u64> {
        match self.terminals.picked.contains(&id) {
            true => self.terminals.picked.clone(),
            false => vec![id],
        }
    }

    pub(super) fn set_terminal_icon(&mut self, id: u64, icon: char, cx: &mut Context<Self>) {
        for id in self.targets(id) {
            if let Some(t) = self.terminal(id) {
                t.icon = icon;
            }
        }
        cx.notify();
    }

    pub(super) fn set_terminal_color(
        &mut self,
        id: u64,
        color: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        for id in self.targets(id) {
            if let Some(t) = self.terminal(id) {
                t.color = color;
            }
        }
        cx.notify();
    }

    /// Output from a shell; true once it exited.
    fn on_terminal(
        &mut self,
        id: u64,
        msg: Msg,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // Find keeps up with the active terminal's output.
        let re = self.find_regex(cx).filter(|_| self.active_id() == Some(id));
        let Some(t) = self.terminal(id) else {
            return true;
        };
        match msg {
            Msg::Out(bytes) => {
                // Output only moves lines above the screen into the scrollback.
                let old = t.parser.screen();
                let kept = (!old.alternate_screen()).then(|| old.scrollback_len());
                t.parser.process(&bytes);
                let trimmed = std::mem::take(&mut t.parser.callbacks_mut().trimmed);
                if trimmed > 0 {
                    t.trim(trimmed);
                }
                if let Some(re) = re {
                    let screen = t.parser.screen_mut();
                    let from = match kept.filter(|_| !screen.alternate_screen()) {
                        Some(len) => wrapped_lines(screen, len.saturating_sub(trimmed)).0,
                        None => 0,
                    };
                    let current = t.current.map(|i| t.matches[i]);
                    t.matches.retain(|m| m.0.0 < from);
                    t.matches.extend(find_all(screen, &re, from));
                    t.current = current.and_then(|m| t.matches.iter().position(|&n| n == m));
                }
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
        let was = self.active_id();
        let list = &mut self.terminals.list;
        list.insert(ix, t);
        self.terminals.active = ix;
        self.terminals.stamp();
        self.find_follows(was, cx);
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
            let was = self.active_id();
            self.terminals.active = ix;
            self.terminals.stamp();
            self.find_follows(was, cx);
            window.focus(&self.terminals.list[ix].focus, cx);
            cx.notify();
        }
    }

    /// VS Code's Focus Next/Previous Terminal Group (`groups`), or Terminal
    /// in Terminal Group, wrapping around. A group is entered at the
    /// terminal last focused in it.
    pub(super) fn focus_next_terminal(
        &mut self,
        groups: bool,
        back: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let list = &self.terminals.list;
        let Some(t) = list.get(self.terminals.active) else {
            return;
        };
        let (ids, at) = if groups {
            let mut ids: Vec<u64> = list.iter().map(|u| u.group).collect();
            ids.dedup();
            (ids, t.group)
        } else {
            let ids = list
                .iter()
                .filter(|u| u.group == t.group)
                .map(|u| u.id)
                .collect();
            (ids, t.id)
        };
        let at = ids.iter().position(|&i| i == at).unwrap();
        let next = ids[(at + if back { ids.len() - 1 } else { 1 }) % ids.len()];
        let id = match groups {
            true => list
                .iter()
                .filter(|u| u.group == next)
                .max_by_key(|u| u.seen),
            false => list.iter().find(|u| u.id == next),
        };
        let id = id.unwrap().id;
        self.activate_terminal(id, window, cx);
        self.focus_terminal(window, cx);
    }

    /// A tab dropped on another: within a split group it takes that one's
    /// place, otherwise its whole group moves to the other's group's place,
    /// as VS Code's tab list does.
    fn move_terminal(&mut self, src: u64, dst: u64, cx: &mut Context<Self>) {
        let was = self.active_id();
        let list = &mut self.terminals.list;
        let pos = |list: &Vec<Terminal>, id| list.iter().position(|t| t.id == id);
        let (Some(from), Some(to)) = (pos(list, src), pos(list, dst)) else {
            return;
        };
        let (group, target) = (list[from].group, list[to].group);
        if group == target {
            let t = list.remove(from);
            list.insert(to, t);
        } else {
            let (moved, mut rest): (Vec<_>, Vec<_>) = std::mem::take(list)
                .into_iter()
                .partition(|t| t.group == group);
            let at = match from < to {
                true => rest.iter().rposition(|t| t.group == target).unwrap() + 1,
                false => rest.iter().position(|t| t.group == target).unwrap(),
            };
            rest.splice(at..at, moved);
            *list = rest;
        }
        self.terminals.active = was.and_then(|id| pos(list, id)).unwrap_or(0);
        cx.notify();
    }

    /// A selected tab dropped on another takes every selected tab along, in
    /// order, each group once.
    fn move_terminals(&mut self, src: u64, dst: u64, cx: &mut Context<Self>) {
        let list = &self.terminals.list;
        let pos = |id| list.iter().position(|t| t.id == id);
        let (Some(to), true) = (pos(dst), self.terminals.picked.contains(&src)) else {
            return self.move_terminal(src, dst, cx);
        };
        let mut ids: Vec<usize> = self
            .terminals
            .picked
            .iter()
            .filter_map(|&id| pos(id))
            .collect();
        ids.sort();
        let mut seen = Vec::new();
        ids.retain(|&i| {
            let g = list[i].group;
            g == list[to].group
                || !seen.contains(&g) && {
                    seen.push(g);
                    true
                }
        });
        let ids: Vec<(usize, u64)> = ids.into_iter().map(|i| (i, list[i].id)).collect();
        // Those above land after the target, so go last-first to keep order.
        for &(_, id) in ids.iter().filter(|&&(i, _)| i < to).rev() {
            self.move_terminal(id, dst, cx);
        }
        for &(_, id) in ids.iter().filter(|&&(i, _)| i > to) {
            self.move_terminal(id, dst, cx);
        }
    }

    /// A tab clicked: alone it activates, with Ctrl it joins or leaves the
    /// selection, with Shift the selection runs from the active tab to it.
    fn click_tab(&mut self, id: u64, m: Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        let ts = &mut self.terminals;
        let active = ts.list.get(ts.active).map(|t| t.id);
        if m.shift {
            let ix = ts.list.iter().position(|t| t.id == id).unwrap_or(ts.active);
            let (a, b) = (ix.min(ts.active), ix.max(ts.active));
            ts.picked = ts.list[a..=b].iter().map(|t| t.id).collect();
        } else if m.control {
            if ts.picked.is_empty() {
                ts.picked.extend(active);
            }
            match ts.picked.iter().position(|&p| p == id) {
                Some(i) => _ = ts.picked.remove(i),
                None => ts.picked.push(id),
            }
        } else {
            ts.picked.clear();
            return self.activate_terminal(id, window, cx);
        }
        cx.notify();
    }

    /// VS Code's Join Terminals: the selected tabs become one split group,
    /// in the place of the first that's alone in its group, else at the end.
    fn join_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ts = &mut self.terminals;
        let picked = std::mem::take(&mut ts.picked);
        let alone = |list: &Vec<Terminal>, g| list.iter().filter(|t| t.group == g).count() == 1;
        let ids: Vec<u64> = (ts.list.iter())
            .map(|t| t.id)
            .filter(|id| picked.contains(id))
            .collect();
        let host = ts
            .list
            .iter()
            .find(|t| ids.contains(&t.id) && alone(&ts.list, t.group));
        let (host, group) = match host {
            Some(t) => (Some(t.id), t.group),
            None => (None, ts.next_id),
        };
        ts.next_id += host.is_none() as u64;
        let (mut moved, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut ts.list)
            .into_iter()
            .partition(|t| ids.contains(&t.id) && Some(t.id) != host);
        moved.iter_mut().for_each(|t| t.group = group);
        ts.list = rest;
        let at = (host.and_then(|h| ts.list.iter().position(|t| t.id == h)))
            .map_or(ts.list.len(), |i| i + 1);
        ts.list.splice(at..at, moved);
        if let Some(&first) = ids.first() {
            self.activate_terminal(first, window, cx);
        }
    }

    /// Join Terminals' picks: every other terminal alone in its group.
    pub(super) fn joinable(&self) -> Vec<(u64, String, char, Hsla)> {
        let list = &self.terminals.list;
        let active = list.get(self.terminals.active).map(|t| t.group);
        let alone = |g| list.iter().filter(|t| t.group == g).count() == 1;
        (list.iter())
            .filter(|t| Some(t.group) != active && alone(t.group))
            .map(|t| {
                let color = t.color.map_or(col::fg(), |i| c(ANSI[i]));
                (t.id, t.name.clone(), t.icon, color)
            })
            .collect()
    }

    /// Join Terminals' pick: the terminal splits in after the active
    /// terminal's group.
    pub(super) fn join_active_terminal(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let list = &self.terminals.list;
        let Some(group) = list.get(self.terminals.active).map(|t| t.group) else {
            return;
        };
        let last = list.iter().rfind(|t| t.group == group).unwrap().id;
        self.terminals.picked.clear();
        self.join_terminal(id, last, true, window, cx);
    }

    /// A tab dropped on a terminal: it leaves its group to split beside that
    /// one, before or `after` it, and becomes active.
    fn join_terminal(
        &mut self,
        src: u64,
        dst: u64,
        after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Every selected tab goes along, in order.
        let ids = self.targets(src);
        let list = &mut self.terminals.list;
        if src == dst || !list.iter().any(|t| t.id == src) {
            return;
        }
        let (mut moved, mut rest): (Vec<_>, Vec<_>) = std::mem::take(list)
            .into_iter()
            .partition(|t| t.id != dst && ids.contains(&t.id));
        let to = rest.iter().position(|u| u.id == dst).unwrap();
        for t in &mut moved {
            t.group = rest[to].group;
        }
        let at = to + after as usize;
        rest.splice(at..at, moved);
        *list = rest;
        self.activate_terminal(src, window, cx);
    }

    /// VS Code's Toggle Size to Content Width (Alt+Z): stops wrapping the
    /// lines on screen at the panel's width, scrolling sideways to show them.
    pub(super) fn toggle_size_to_content_width(&mut self, cx: &mut Context<Self>) {
        let ts = &mut self.terminals;
        let Some(t) = ts.list.get_mut(ts.active) else {
            return;
        };
        if t.fixed_cols.take().is_none() {
            let screen = t.parser.screen();
            let (rows, cols) = screen.size();
            let (mut widest, mut len) = (0, 0);
            for row in 0..rows.get() {
                len += line_end(screen, row);
                if !screen.row_wrapped(row) {
                    widest = widest.max(len);
                    len = 0;
                }
            }
            t.fixed_cols = (widest > cols.get()).then_some(widest);
        }
        t.scroll_x = 0.;
        cx.notify();
    }

    /// VS Code's Terminal: Scroll Up/Down by lines or pages (a page keeps one
    /// line in view), saturating to the top or bottom.
    pub(super) fn scroll_terminal(&mut self, lines: isize, pages: isize, cx: &mut Context<Self>) {
        let ts = &mut self.terminals;
        let Some(t) = ts.list.get_mut(ts.active) else {
            return;
        };
        let screen = t.parser.screen_mut();
        let page = screen.size().0.get() as isize - 1;
        let by = lines.saturating_add(pages.saturating_mul(page));
        screen.set_scrollback(screen.scrollback().saturating_add_signed(by));
        t.reveal();
        cx.notify();
    }

    /// VS Code's Unsplit Terminal: the active terminal leaves its split group
    /// for a group of its own at the end of the list.
    pub(super) fn unsplit_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ts = &mut self.terminals;
        let Some(group) = ts.list.get(ts.active).map(|t| t.group) else {
            return;
        };
        if ts.list.iter().filter(|u| u.group == group).count() < 2 {
            return;
        }
        let mut t = ts.list.remove(ts.active);
        // A fresh id: the group may still carry this terminal's own.
        t.group = ts.next_id;
        ts.next_id += 1;
        let id = t.id;
        ts.list.push(t);
        self.activate_terminal(id, window, cx);
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
        let was = self.active_id();
        self.terminals.list.remove(ix);
        self.terminals.picked.retain(|&p| p != id);
        let list = &self.terminals.list;
        let active = &mut self.terminals.active;
        if *active > ix || *active == list.len() {
            *active = active.saturating_sub(1);
        }
        self.find_follows(was, cx);
        let list = &self.terminals.list;
        let active = self.terminals.active;
        if list.is_empty() && self.panel_tab == TERMINAL {
            self.panel = false;
        }
        if focused {
            match list.get(active) {
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
                .join("\n"),
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
        let delta = ev.delta.pixel_delta(px(LINE_H));
        if t.fixed_cols.is_some() && (ev.modifiers.shift || delta.x != px(0.)) {
            let dx = if delta.x != px(0.) { delta.x } else { delta.y };
            t.scroll_x -= f32::from(dx);
            return cx.notify();
        }
        t.scroll += f32::from(delta.y) / LINE_H;
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
        t.reveal();
        t.selection = t.dragged();
        cx.notify();
        true
    }

    /// Scrolls to keep the grabbed point of a scrollbar's thumb at `at`.
    fn drag_thumb(&mut self, id: u64, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(t) = self.terminal(id) else {
            return;
        };
        let Some((axis, grab)) = t.thumb else {
            return;
        };
        let b = t.bounds.get();
        if axis == Axis::Horizontal {
            let grid_w = PAD_L + t.parser.screen().size().1.get() as f32 * t.cell_w + PAD_R;
            t.scroll_x = (f32::from(at.x - b.left()) - grab) * grid_w / f32::from(b.size.width);
        } else {
            let screen = t.parser.screen_mut();
            let h = f32::from(b.size.height);
            let (_, len) = v_thumb(screen, h);
            let max = screen.scrollback_len() as f32;
            let top = (f32::from(at.y - b.top()) - grab) / (h - len).max(1.) * max;
            screen.set_scrollback((max - top.round().clamp(0., max)) as usize);
        }
        cx.notify();
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
                && let Some((down, right)) =
                    cursor_moves(t.parser.screen(), t.cell(ev.position, false))
            {
                let csi = if t.parser.screen().application_cursor() {
                    "\x1bO"
                } else {
                    "\x1b["
                };
                let arrows = |n: isize, back, on| {
                    format!("{csi}{}", if n < 0 { back } else { on }).repeat(n.unsigned_abs())
                };
                let keys = arrows(down, 'A', 'B') + &arrows(right, 'D', 'C');
                t.write(keys.as_bytes());
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
                let id = self.terminals.list[ix].id;
                // A tab dropped on either half splits beside this terminal.
                let half = |after: bool| {
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(relative(if after { 0.5 } else { 0. }))
                        .w(relative(0.5))
                        .drag_over::<DraggedTab>(|s, _, _, _| s.bg(ca(0x53595D80)))
                        .on_drop(cx.listener(move |this, tab: &DraggedTab, window, cx| {
                            this.join_terminal(tab.0, id, after, window, cx)
                        }))
                };
                let halves = [half(false), half(true)];
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .relative()
                    .when(i > 0, |d| d.border_l_1().border_color(col::border()))
                    .child(self.render_one(ix, window, cx))
                    .when(ix == self.terminals.active, |d| {
                        d.children(self.render_terminal_find(cx))
                    })
                    .children(halves)
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
            .tooltip("Launch Profile...")
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
        let wb = cx.weak_entity();
        let rows = list.iter().enumerate().map(|(ix, t)| {
            let id = t.id;
            let wb = wb.clone();
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
            let picked = self.terminals.picked.contains(&id);
            let joins = picked && self.terminals.picked.len() > 1;
            h_flex()
                .id(("terminal-tab", id as usize))
                .h(px(22.))
                .flex_none()
                .pl(px(8.))
                .pr(px(4.))
                .gap(px(6.))
                .cursor_pointer()
                .when(active || picked, |d| d.bg(col::selected()))
                .when(!active && !picked, |d| d.hover(|s| s.bg(col::hover())))
                .child(div().w(px(8.)).text_color(col::fg_muted()).child(tree))
                .child(tab_icon(t))
                .child(div().flex_1().min_w_0().truncate().child(t.name.clone()))
                .when(active, |d| {
                    d.child(
                        action_button(("terminal-kill", id as usize), icon::TRASH)
                            .tooltip(tip("Kill (Delete)"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.kill_terminal(id, window, cx)
                            })),
                    )
                })
                .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                    this.click_tab(id, ev.modifiers(), window, cx)
                }))
                .on_drag(DraggedTab(id, t.name.clone().into()), |tab, _, _, cx| {
                    cx.new(|_| tab.clone())
                })
                .drag_over::<DraggedTab>(|s, _, _, _| s.bg(c(0x383B3D)))
                .on_drop(cx.listener(move |this, tab: &DraggedTab, _, cx| {
                    this.move_terminals(tab.0, id, cx)
                }))
                .context_menu(move |menu, _, _| tab_menu(menu, &wb, id, !tree.is_empty(), joins))
        });
        v_flex()
            .w(px(TABS_W))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(col::border())
            .children(rows)
    }

    /// VS Code's single terminal tab: with no list of terminals beside it, the
    /// panel's title bar names the one terminal.
    pub(super) fn single_terminal_tab(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let [t] = &self.terminals.list[..] else {
            return None;
        };
        let (id, wb) = (t.id, cx.weak_entity());
        let tab = h_flex()
            .id("terminal-single-tab")
            .h(px(22.))
            .px(px(4.))
            .gap(px(4.))
            .rounded(px(5.))
            .hover(|s| s.bg(col::hover()))
            .child(tab_icon(t))
            .child(t.name.clone())
            .on_click(cx.listener(move |this, _, w, cx| this.activate_terminal(id, w, cx)))
            .context_menu(move |menu, _, _| tab_menu(menu, &wb, id, false, false));
        Some(tab)
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
            let cols = t.fixed_cols.unwrap_or(cols.floor().max(1.) as u16);
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
        // The grid's width past the panel's, when its size is fixed.
        let grid_w = PAD_L + cols.get() as f32 * t.cell_w + PAD_R;
        let over = (grid_w - f32::from(size.width)).max(0.);
        let scrolled = t.fixed_cols.is_some() && over > 0.;
        t.scroll_x = t.scroll_x.clamp(0., over);
        let scroll_x = t.scroll_x;
        let v_bar = (screen.scrollback_len() > 0 && !screen.alternate_screen())
            .then(|| v_thumb(screen, f32::from(size.height)));
        // Find's matches down the right edge, VS Code's overview ruler: the
        // current one gray, the rest orange, one mark per pixel row.
        let ruler = {
            let h = f32::from(size.height);
            let total = (screen.scrollback_len() + rows.get() as usize) as f32;
            let len = (h / total).max(2.);
            let mut marks: Vec<(f32, bool)> = Vec::new();
            for (i, m) in t.matches.iter().enumerate() {
                let y = (m.0.0 as f32 / total * h).min(h - len).round();
                let now = t.current == Some(i);
                match marks.last_mut() {
                    Some(last) if last.0 == y => last.1 |= now,
                    _ => marks.push((y, now)),
                }
            }
            div()
                .absolute()
                .top_0()
                .right_0()
                .w(px(PAD_R))
                .h_full()
                .children(marks.into_iter().map(move |(y, now)| {
                    div()
                        .absolute()
                        .top(px(y))
                        .left(px(PAD_R / 3.))
                        .w(px(PAD_R / 3.))
                        .h(px(len))
                        .bg(ca(if now { 0xA0A0A0CC } else { 0xD186167E }))
                }))
        };
        let (selection, block) = (t.selection, t.block);
        let top = t.top();
        let bottom = top + rows.get() as usize;
        let matches: Vec<Span> = (t.matches.iter())
            .filter(|m| m.0.0 < bottom && m.1.0 >= top)
            .copied()
            .collect();
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
                } else if matches.iter().any(|&m| selected(m, false, at)) {
                    bg = Some(ca(0xEA5C0055));
                }
                let link = hover.is_some_and(|(l, a, b)| {
                    at.0 >= l && (a..b).contains(&((at.0 - l) * cols.get() as usize + col as usize))
                });
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
        let thumb_axis = t.thumb.map(|(axis, _)| axis);
        let thumbing = thumb_axis.is_some();
        // Full-screen programs get the scroll keys.
        let alt = t.parser.screen().alternate_screen();
        // VS Code's scrollbars fade in over 100ms, and out over 800ms
        // starting 500ms after the mouse leaves.
        let shown = match t.over {
            None => 0.,
            Some((true, at)) => (at.elapsed().as_secs_f32() / 0.1).min(1.),
            Some((false, at)) => 1. - ((at.elapsed().as_secs_f32() - 0.5) / 0.8).clamp(0., 1.),
        };
        if t.over
            .is_some_and(|(over, _)| if over { shown < 1. } else { shown > 0. })
        {
            window.request_animation_frame();
        }
        let shown = if thumbing { 1. } else { shown };
        div()
            .id(("terminal", id as usize))
            .relative()
            .size_full()
            .overflow_hidden()
            .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                if let Some(t) = this.terminal(id) {
                    t.over = Some((*over, Instant::now()));
                    cx.notify();
                }
            }))
            .track_focus(&t.focus)
            .key_context(match alt {
                true => "Terminal altBuffer",
                false => "Terminal",
            })
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
                        && let Some((_, link)) = t.link(ev.position, root.as_deref())
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
                if t.thumb.is_some() {
                    this.drag_thumb(id, ev.position, cx);
                } else if !t.selecting && t.mouse_mode(ev.modifiers) {
                    let held = ev.pressed_button.and_then(button_code);
                    t.report(held.map_or(35, |b| b + 32), false, ev.position);
                } else if t.selecting {
                    this.drag_terminal(id, ev, cx);
                } else if ev.pressed_button.is_none() {
                    let hover = t.link(ev.position, root.as_deref()).map(|(hover, _)| hover);
                    if t.hover != hover {
                        t.hover = hover;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, ev, _, cx| {
                    if this.terminal(id).and_then(|t| t.thumb.take()).is_some() {
                        return cx.notify();
                    }
                    this.terminal_release(id, ev)
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if let Some(t) = this.terminal(id) {
                        t.selecting = false;
                        t.thumb = None;
                        cx.notify();
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
                        // A refresh mid-draw is lost; redraw on the next frame.
                        if bounds.replace(b) != b {
                            window.request_animation_frame();
                        }
                    },
                    move |b, _, window, _| {
                        // Mouse moves outside reach only the window.
                        if selecting || thumbing {
                            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                                if phase == DispatchPhase::Bubble && !b.contains(&ev.position) {
                                    drag.update(cx, |this, cx| match thumbing {
                                        true => this.drag_thumb(id, ev.position, cx),
                                        false => this.drag_terminal(id, ev, cx),
                                    })
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
                    // Out of the flow, so a fixed width can't widen the panel.
                    .absolute()
                    .top_0()
                    .left(px(-scroll_x))
                    .map(|d| match scrolled {
                        true => d.w(px(grid_w)),
                        false => d.w_full(),
                    })
                    .child(v_flex().pl(px(PAD_L)).children(lines))
                    .children(cursor),
            )
            .child(ruler)
            .when(scrolled, |d| {
                let w = f32::from(size.width);
                let thumb = (scroll_x / grid_w * w, w / grid_w * w);
                let active = thumb_axis == Some(Axis::Horizontal);
                d.child(scrollbar(id, Axis::Horizontal, thumb, active, shown, cx))
            })
            .when_some(v_bar, |d, thumb| {
                let active = thumb_axis == Some(Axis::Vertical);
                d.child(scrollbar(id, Axis::Vertical, thumb, active, shown, cx))
            })
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

/// A terminal tab's right-click menu. Each item acts on that terminal, made
/// active first as in VS Code. A `split` one can be unsplit; one of several
/// selected `joins` them.
fn tab_menu(
    menu: PopupMenu,
    wb: &WeakEntity<Workbench>,
    id: u64,
    split: bool,
    joins: bool,
) -> PopupMenu {
    type Act = fn(&mut Workbench, u64, &mut Window, &mut Context<Workbench>);
    let item = |label: &'static str, act: Act| {
        let wb = wb.clone();
        PopupMenuItem::new(label).on_click(move |_, window, cx| {
            wb.update(cx, |this, cx| {
                this.activate_terminal(id, window, cx);
                act(this, id, window, cx)
            })
            .ok();
        })
    };
    menu.item(item("Split Terminal", |this, _, w, cx| {
        this.new_terminal(true, w, cx)
    }))
    .item(item("Change Icon...", |this, _, w, cx| {
        this.change_terminal_icon(w, cx)
    }))
    .item(item("Change Color...", |this, _, w, cx| {
        this.change_terminal_color(w, cx)
    }))
    .item(item("Rename...", |this, _, w, cx| {
        this.rename_terminal(w, cx)
    }))
    .separator()
    .when(joins, |menu| {
        let wb = wb.clone();
        menu.item(
            PopupMenuItem::new("Join Terminals").on_click(move |_, window, cx| {
                wb.update(cx, |this, cx| this.join_terminals(window, cx))
                    .ok();
            }),
        )
    })
    .when(split, |menu| {
        menu.item(item("Unsplit Terminal", |this, _, w, cx| {
            this.unsplit_terminal(w, cx)
        }))
        .separator()
    })
    .item(item("Kill Terminal", |this, id, w, cx| {
        for id in this.targets(id) {
            this.kill_terminal(id, w, cx)
        }
    }))
}

/// A terminal tab being dragged: its id, and its name to draw under the mouse.
#[derive(Clone)]
struct DraggedTab(u64, SharedString);

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(2.))
            .bg(col::selected())
            .text_color(col::fg())
            .child(self.1.clone())
    }
}

/// The vertical scrollbar's thumb, top and height, for a track `h` tall:
/// sized to the screen's share of the lines, but at least 20px like VS Code's.
fn v_thumb(screen: &vt100::Screen, h: f32) -> (f32, f32) {
    let rows = screen.size().0.get() as f32;
    let back = screen.scrollback_len() as f32;
    let len = (rows / (rows + back) * h).max(20.).min(h);
    let top = (back - screen.scrollback() as f32) / back.max(1.) * (h - len);
    (top, len)
}

/// A scrollbar along the bottom or right edge, shown while the mouse is
/// over the terminal, with its thumb's start and length. Dragging the thumb
/// scrolls; pressing the track centers the thumb there first, as VS Code's
/// scrollbars do.
fn scrollbar(
    id: u64,
    axis: Axis,
    (at, len): (f32, f32),
    active: bool,
    shown: f32,
    cx: &mut Context<Workbench>,
) -> impl IntoElement {
    let horizontal = axis == Axis::Horizontal;
    let thumb = div()
        .id(("terminal-thumb", horizontal as usize))
        .absolute()
        .map(|d| match horizontal {
            true => d.left(px(at)).w(px(len)).h_full(),
            false => d.top(px(at)).h(px(len)).w_full(),
        })
        .map(|d| match active {
            true => d.bg(ca(0xBFBFBF66)),
            false => d.bg(ca(0x79797966)).hover(|s| s.bg(ca(0x646464B3))),
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if let Some(t) = this.terminal(id) {
                    let b = t.bounds.get();
                    let from = match horizontal {
                        true => ev.position.x - b.left(),
                        false => ev.position.y - b.top(),
                    };
                    t.thumb = Some((axis, f32::from(from) - at));
                    cx.notify();
                }
            }),
        );
    div()
        .id(("terminal-track", horizontal as usize))
        .absolute()
        .map(|d| match horizontal {
            true => d.left_0().right_0().bottom_0().h(px(10.)),
            false => d.top_0().bottom_0().right_0().w(px(PAD_R)),
        })
        .opacity(shown)
        .cursor_default()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if let Some(t) = this.terminal(id) {
                    t.thumb = Some((axis, len / 2.));
                }
                this.drag_thumb(id, ev.position, cx);
            }),
        )
        .child(thumb)
}

/// A terminal tab's icon, in the color Change Color gave it.
fn tab_icon(t: &Terminal) -> Div {
    codicon(t.icon).when_some(t.color, |d, i| d.text_color(c(ANSI[i])))
}

/// A terminal's name until it is renamed: its program's, like `pwsh`.
fn shell_name(profile: &Profile) -> String {
    (profile.program.file_stem()).map_or("shell".into(), |s| s.to_string_lossy().into())
}

/// Every match of `re` along the wrapped lines from `line` on, which starts
/// one, as selection spans, oldest first.
fn find_all(screen: &mut vt100::Screen, re: &regex::Regex, mut line: usize) -> Vec<Span> {
    let cols = screen.size().1.get() as usize;
    let last = screen.scrollback_len() + screen.size().0.get() as usize - 1;
    let mut found = Vec::new();
    while line <= last {
        let (l0, l1) = wrapped_lines(screen, line);
        let text: String = (l0..=l1)
            .flat_map(|l| at_line(screen, l, row_chars))
            .collect();
        let at = |byte: usize| {
            let i = text[..byte].chars().count();
            (l0 + i / cols, (i % cols) as u16)
        };
        found.extend(re.find_iter(&text).map(|m| (at(m.start()), at(m.end()))));
        line = l1 + 1;
    }
    found
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

/// Calls `f` with the screen scrolled so `line` shows, and the row it is
/// on, then scrolls back.
fn at_line<R>(
    screen: &mut vt100::Screen,
    line: usize,
    f: impl FnOnce(&vt100::Screen, u16) -> R,
) -> R {
    let (saved, len) = (screen.scrollback(), screen.scrollback_len());
    screen.set_scrollback(len.saturating_sub(line));
    let row = line - (len - screen.scrollback());
    let r = f(screen, row as u16);
    screen.set_scrollback(saved);
    r
}

/// The first and last lines of the wrapped line through `line`, reaching
/// into the scrollback.
fn wrapped_lines(screen: &mut vt100::Screen, line: usize) -> (usize, usize) {
    let last = screen.scrollback_len() + screen.size().0.get() as usize - 1;
    let mut wrapped = |l| at_line(screen, l, |s, r| s.row_wrapped(r));
    let (mut l0, mut l1) = (line, line);
    while l0 > 0 && wrapped(l0 - 1) {
        l0 -= 1;
    }
    while l1 < last && wrapped(l1) {
        l1 += 1;
    }
    (l0, l1)
}

/// A visible row, one char per cell.
fn row_chars(screen: &vt100::Screen, row: u16) -> Vec<char> {
    (0..screen.size().1.get())
        .map(|col| {
            let cell = screen.cell(row, col);
            cell.and_then(|c| c.contents().chars().next())
                .unwrap_or(' ')
        })
        .collect()
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

/// The arrow presses, (down, right) with negatives going up and left, that
/// walk the cursor to the visible cell `to`. At a prompt they only go along
/// its wrapped line, None when `to` is on another; a full-screen program,
/// told apart as for the wheel, also moves up and down.
fn cursor_moves(screen: &vt100::Screen, (row, col): (u16, u16)) -> Option<(isize, isize)> {
    let (crow, ccol) = screen.cursor_position();
    let (down, right) = (row as isize - crow as isize, col as isize - ccol as isize);
    if screen.alternate_screen() || screen.application_cursor() {
        return Some((down, right));
    }
    let cols = screen.size().1.get() as isize;
    let joined = (row.min(crow)..row.max(crow)).all(|r| screen.row_wrapped(r));
    joined.then_some((0, down * cols + right))
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

/// VS Code's dark terminal colors.
pub(super) const ANSI: [u32; 16] = [
    0x000000, 0xCD3131, 0x0DBC79, 0xE5E510, 0x2472C8, 0xBC3FBC, 0x11A8CD, 0xE5E5E5, 0x666666,
    0xF14C4C, 0x23D18B, 0xF5F543, 0x3B8EEA, 0xD670D6, 0x29B8DB, 0xE5E5E5,
];

/// Change Color's names for the first eight of `ANSI`, as VS Code lists them.
pub(super) const COLORS: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

/// `ANSI` as cell colors; `bold` brightens the first eight like its
/// `terminal.integrated.drawBoldTextInBrightColors`.
fn ansi(color: vt100::Color, bold: bool) -> Option<Hsla> {
    let rgb = |r: u8, g: u8, b: u8| (r as u32) << 16 | (g as u32) << 8 | b as u32;
    Some(c(match color {
        vt100::Color::Default => return None,
        vt100::Color::Idx(i @ 0..8) if bold => ANSI[i as usize + 8],
        vt100::Color::Idx(i @ 0..16) => ANSI[i as usize],
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
        Link, at_line, cursor_moves, find_all, key_bytes, link, mouse_bytes, reflow, selected,
        text_between, word_at, word_span, wrapped_lines,
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
    fn counts_trimmed_lines() {
        let mut p = vt100::Parser::new_with_callbacks(
            2.try_into().unwrap(),
            4.try_into().unwrap(),
            2,
            super::Replies::default(),
        );
        // Five lines: two on screen, two in the scrollback, one dropped.
        p.process(b"a\r\nb\r\nc\r\nd\r\ne");
        assert_eq!(p.callbacks().trimmed, 1);
    }

    #[test]
    fn finds() {
        let mut p = vt100::Parser::new(3.try_into().unwrap(), 10.try_into().unwrap(), 100);
        // "xx item tw" wraps onto "o wraps"; the first line goes to scrollback.
        p.process(b"Item one\r\nxx item two wraps\r\n");
        let mut find = |q: &str| {
            let re = regex::RegexBuilder::new(q).case_insensitive(true).build();
            find_all(p.screen_mut(), &re.unwrap(), 0)
        };
        assert_eq!(find("item"), [((0, 0), (0, 4)), ((1, 3), (1, 7))]);
        assert_eq!(find("two"), [((1, 8), (2, 1))]);
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
    fn wraps_into_scrollback() {
        let mut parser = vt100::Parser::new(3.try_into().unwrap(), 10.try_into().unwrap(), 100);
        parser.process(&[b'x'; 35]);
        let screen = parser.screen_mut();
        assert_eq!(screen.scrollback_len(), 1);
        assert_eq!(wrapped_lines(screen, 3), (0, 3));
        assert_eq!(
            at_line(screen, 0, |s, r| s.contents_between(r, 0, r, 10)),
            "x".repeat(10)
        );
        assert_eq!(screen.scrollback(), 0);
    }

    #[test]
    fn alt_click_moves() {
        let mut parser = vt100::Parser::new(3.try_into().unwrap(), 10.try_into().unwrap(), 0);
        parser.process(b"abcdefghijklmno");
        let screen = parser.screen();
        assert_eq!(cursor_moves(screen, (1, 8)), Some((0, 3)));
        assert_eq!(cursor_moves(screen, (0, 2)), Some((0, -13)));
        assert_eq!(cursor_moves(screen, (2, 0)), None);
        parser.process(b"\x1b[?1h");
        assert_eq!(cursor_moves(parser.screen(), (2, 0)), Some((1, -5)));
    }
}
