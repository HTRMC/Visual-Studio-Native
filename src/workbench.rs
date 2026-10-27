//! The VS Code workbench: title bar, activity bar, side bar, editor group,
//! panel and status bar.

use std::{
    collections::{HashMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    slice,
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use ignore::overrides::Override;
use notify::{EventKind, RecursiveMode, Watcher as _, event::ModifyKind};

use gpui_kit::component::{
    Sizable as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_resizable,
    input::{self, Editor, EditorState, Input, InputEvent, InputState, RopeExt as _, TabSize},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    resizable_panel,
    text::TextView,
    tooltip::Tooltip,
    v_resizable,
};
use gpui_kit::{
    base::{
        ResizeHandleContext, ResizeHandleState, Scrollbar, ScrollbarHandle, ScrollbarMode,
        ScrollbarThumbStyle, h_flex, v_flex,
    },
    prelude::FluentBuilder as _,
    *,
};

mod debug;
pub(crate) mod extensions;
mod exthost;
mod lsp;
mod output;
mod palette;
mod search;
mod settings_ui;
mod terminal;
mod wasmhost;

use crate::theme::{
    action_button, c, ca, codicon, col, file_icon, folder_icon, icon, kbds, language_id,
    language_label, popup_radius, tip,
};

actions!(
    vsn,
    [
        OpenFile,
        OpenFolder,
        CloseFolder,
        Save,
        SaveAll,
        CloseEditor,
        CloseAllEditors,
        CloseSavedEditors,
        CloseOtherEditors,
        CloseEditorsToTheRight,
        CopyPath,
        CopyRelativePath,
        RevealInFileExplorer,
        RevealInExplorerView,
        Quit,
        CloseWindow,
        ToggleSidebar,
        TogglePanel,
        ToggleMaximizedPanel,
        ToggleTerminal,
        ToggleProblems,
        ToggleOutput,
        ToggleDebugConsole,
        ShowExplorer,
        RefreshExplorer,
        CollapseExplorerFolders,
        ShowSearch,
        ReplaceInFiles,
        ReplaceAll,
        ShowScm,
        ShowRun,
        ShowExtensions,
        CommandPalette,
        About,
        QuickOpen,
        OpenView,
        GoToLine,
        GoToSymbol,
        GoToSymbolInWorkspace,
        GoBack,
        GoForward,
        GoToLastEditLocation,
        NextEditor,
        PreviousEditor,
        StartDebugging,
        RunWithoutDebugging,
        StopDebugging,
        RestartDebugging,
        Pause,
        StepOver,
        StepInto,
        StepOut,
        ToggleBreakpoint,
        OpenLaunchJson,
        ClearRecent,
        NewTerminal,
        SplitTerminal,
        RenameTerminal,
        ChangeTerminalColor,
        ChangeTerminalIcon,
        KillTerminal,
        UnsplitTerminal,
        JoinTerminals,
        FocusNextTerminalGroup,
        FocusPreviousTerminalGroup,
        FocusNextTerminal,
        FocusPreviousTerminal,
        FindInTerminal,
        FindNextInTerminal,
        FindPreviousInTerminal,
        ToggleSizeToContentWidth,
        ToggleWordWrap,
        CopyLinesUp,
        CopyLinesDown,
        MoveLinesUp,
        MoveLinesDown,
        ToggleLineComment,
        AddNextOccurrence,
        AddPreviousOccurrence,
        MoveSelectionToNextFindMatch,
        ExpandSelection,
        ShrinkSelection,
        SelectAllOccurrences,
        AddLineComment,
        RemoveLineComment,
        ToggleBlockComment,
        ScrollTerminalUp,
        ScrollTerminalDown,
        ScrollTerminalUpPage,
        ScrollTerminalDownPage,
        ScrollTerminalToTop,
        ScrollTerminalToBottom,
        SelectDefaultProfile,
        ChangeEol,
        ChangeLanguageMode,
        ChangeEncoding,
        IndentUsingSpaces,
        IndentUsingTabs,
        ChangeTabDisplaySize,
        DetectIndentation,
        IndentationToSpaces,
        IndentationToTabs,
        TrimTrailingWhitespace,
        FormatDocument,
        FormatSelection,
        RenameSymbol,
        FindAllReferences,
        TriggerParameterHints,
        NewTextFile,
        RevertFile,
        SaveAs,
        ToggleAutoSave,
        ShowNotifications,
        HideNotifications,
        ClearAllNotifications,
        ToggleDoNotDisturb,
        Noop,
        CutFile,
        CopyFile,
        PasteFile,
        FindInFolder,
        OpenSettingsJson,
        OpenSettings,
        MarkdownShowPreview,
        MarkdownShowPreviewToSide,
        MarkdownShowSource,
        InstallFromVsix,
        SelectColorTheme,
        SelectIconTheme,
        InsertSnippet,
    ]
);

pub fn key_bindings(mapper: &dyn PlatformKeyboardMapper) -> Vec<KeyBinding> {
    // Windows reports shift+symbol keys as the shifted char ("ctrl-%"), so
    // those go through the platform mapper; `KeyBinding::new` skips it.
    let shifted = |keys: &str, action: Box<dyn Action>, context: Option<&str>| {
        let context = context.map(|c| KeyBindingContextPredicate::parse(c).unwrap().into());
        KeyBinding::load(keys, action, context, false, None, mapper).unwrap()
    };
    let mut list = vec![
        KeyBinding::new("ctrl-o", OpenFile, None),
        KeyBinding::new("ctrl-k ctrl-o", OpenFolder, None),
        KeyBinding::new("ctrl-k ctrl-t", SelectColorTheme, None),
        KeyBinding::new("ctrl-s", Save, None),
        KeyBinding::new("ctrl-shift-s", SaveAs, None),
        KeyBinding::new("ctrl-k s", SaveAll, None),
        KeyBinding::new("ctrl-w", CloseEditor, None),
        KeyBinding::new("ctrl-f4", CloseEditor, None),
        KeyBinding::new("ctrl-k ctrl-w", CloseAllEditors, None),
        KeyBinding::new("ctrl-k w", CloseAllEditors, None),
        KeyBinding::new("ctrl-k u", CloseSavedEditors, None),
        KeyBinding::new("shift-alt-c", CopyPath, None),
        KeyBinding::new("ctrl-k ctrl-shift-alt-c", CopyRelativePath, None),
        KeyBinding::new("shift-alt-r", RevealInFileExplorer, None),
        // The menu shows the last binding, so Alt+F4 as VS Code does.
        KeyBinding::new("ctrl-shift-w", CloseWindow, None),
        KeyBinding::new("alt-f4", CloseWindow, None),
        KeyBinding::new("ctrl-b", ToggleSidebar, None),
        KeyBinding::new("ctrl-j", TogglePanel, None),
        KeyBinding::new("ctrl-`", ToggleTerminal, None),
        KeyBinding::new("ctrl-shift-m", ToggleProblems, None),
        KeyBinding::new("ctrl-shift-u", ToggleOutput, None),
        KeyBinding::new("ctrl-shift-y", ToggleDebugConsole, None),
        KeyBinding::new("ctrl-shift-e", ShowExplorer, None),
        KeyBinding::new("ctrl-x", CutFile, Some("Explorer")),
        KeyBinding::new("ctrl-c", CopyFile, Some("Explorer")),
        KeyBinding::new("ctrl-v", PasteFile, Some("Explorer")),
        KeyBinding::new("shift-alt-f", FindInFolder, Some("Explorer")),
        KeyBinding::new("ctrl-shift-f", ShowSearch, None),
        KeyBinding::new("ctrl-shift-h", ReplaceInFiles, None),
        KeyBinding::new("ctrl-alt-enter", ReplaceAll, None),
        KeyBinding::new("ctrl-shift-g", ShowScm, None),
        KeyBinding::new("ctrl-shift-d", ShowRun, None),
        KeyBinding::new("ctrl-shift-x", ShowExtensions, None),
        KeyBinding::new("ctrl-shift-p", CommandPalette, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-shift-v", MarkdownShowPreview, Some("editorLangId == markdown")),
        KeyBinding::new("ctrl-k v", MarkdownShowPreviewToSide, Some("editorLangId == markdown")),
        KeyBinding::new("f1", CommandPalette, None),
        KeyBinding::new("ctrl-p", QuickOpen, None),
        KeyBinding::new("ctrl-q", OpenView, None),
        KeyBinding::new("ctrl-g", GoToLine, None),
        KeyBinding::new("ctrl-shift-o", GoToSymbol, None),
        KeyBinding::new("ctrl-t", GoToSymbolInWorkspace, None),
        KeyBinding::new("f12", input::GoToDefinition, Some("Input")),
        KeyBinding::new("ctrl-space", input::ShowCompletions, Some("Input")),
        KeyBinding::new("alt-left", GoBack, None),
        KeyBinding::new("alt-right", GoForward, None),
        KeyBinding::new("ctrl-k ctrl-q", GoToLastEditLocation, None),
        KeyBinding::new("ctrl-k ctrl-x", TrimTrailingWhitespace, None),
        KeyBinding::new("shift-alt-f", FormatDocument, None),
        KeyBinding::new("ctrl-k ctrl-f", FormatSelection, None),
        KeyBinding::new("f2", RenameSymbol, None),
        KeyBinding::new("shift-f12", FindAllReferences, None),
        KeyBinding::new("ctrl-shift-space", TriggerParameterHints, None),
        KeyBinding::new("ctrl-pagedown", NextEditor, None),
        KeyBinding::new("ctrl-pageup", PreviousEditor, None),
        // Shells complete on Tab; gpui-kit's Root would move focus instead.
        KeyBinding::new("tab", NoAction, Some("Terminal")),
        KeyBinding::new("shift-tab", NoAction, Some("Terminal")),
        KeyBinding::new("f5", StartDebugging, None),
        KeyBinding::new("ctrl-f5", RunWithoutDebugging, None),
        KeyBinding::new("shift-f5", StopDebugging, None),
        KeyBinding::new("ctrl-shift-f5", RestartDebugging, None),
        KeyBinding::new("f6", Pause, None),
        KeyBinding::new("f10", StepOver, None),
        KeyBinding::new("f11", StepInto, None),
        KeyBinding::new("shift-f11", StepOut, None),
        KeyBinding::new("f9", ToggleBreakpoint, None),
        shifted("ctrl-shift-`", Box::new(NewTerminal), None),
        shifted("ctrl-shift-5", Box::new(SplitTerminal), Some("Terminal")),
        KeyBinding::new("ctrl-f", FindInTerminal, Some("Terminal")),
        KeyBinding::new("f3", FindNextInTerminal, Some("Terminal || TerminalFind")),
        KeyBinding::new(
            "shift-f3",
            FindPreviousInTerminal,
            Some("Terminal || TerminalFind"),
        ),
        KeyBinding::new("alt-z", ToggleSizeToContentWidth, Some("Terminal")),
        KeyBinding::new("alt-z", ToggleWordWrap, None),
        KeyBinding::new("shift-alt-up", CopyLinesUp, Some("Input && mode == editor")),
        KeyBinding::new(
            "shift-alt-down",
            CopyLinesDown,
            Some("Input && mode == editor"),
        ),
        KeyBinding::new("alt-up", MoveLinesUp, Some("Input && mode == editor")),
        KeyBinding::new("alt-down", MoveLinesDown, Some("Input && mode == editor")),
        KeyBinding::new("ctrl-/", ToggleLineComment, Some("Input && mode == editor")),
        KeyBinding::new("ctrl-d", AddNextOccurrence, Some("Input && mode == editor")),
        KeyBinding::new(
            "shift-alt-right",
            ExpandSelection,
            Some("Input && mode == editor"),
        ),
        KeyBinding::new(
            "shift-alt-left",
            ShrinkSelection,
            Some("Input && mode == editor"),
        ),
        KeyBinding::new(
            "ctrl-shift-l",
            SelectAllOccurrences,
            Some("Input && mode == editor"),
        ),
        KeyBinding::new(
            "ctrl-k ctrl-d",
            MoveSelectionToNextFindMatch,
            Some("Input && mode == editor"),
        ),
        KeyBinding::new(
            "ctrl-k ctrl-c",
            AddLineComment,
            Some("Input && mode == editor"),
        ),
        KeyBinding::new(
            "ctrl-k ctrl-u",
            RemoveLineComment,
            Some("Input && mode == editor"),
        ),
        KeyBinding::new(
            "shift-alt-a",
            ToggleBlockComment,
            Some("Input && mode == editor"),
        ),
        KeyBinding::new(
            "ctrl-alt-pageup",
            ScrollTerminalUp,
            Some("Terminal && !altBuffer"),
        ),
        KeyBinding::new(
            "ctrl-alt-pagedown",
            ScrollTerminalDown,
            Some("Terminal && !altBuffer"),
        ),
        KeyBinding::new(
            "shift-pageup",
            ScrollTerminalUpPage,
            Some("Terminal && !altBuffer"),
        ),
        KeyBinding::new(
            "shift-pagedown",
            ScrollTerminalDownPage,
            Some("Terminal && !altBuffer"),
        ),
        KeyBinding::new(
            "ctrl-home",
            ScrollTerminalToTop,
            Some("Terminal && !altBuffer"),
        ),
        KeyBinding::new(
            "ctrl-end",
            ScrollTerminalToBottom,
            Some("Terminal && !altBuffer"),
        ),
        KeyBinding::new("ctrl-pagedown", FocusNextTerminalGroup, Some("Terminal")),
        KeyBinding::new("ctrl-pageup", FocusPreviousTerminalGroup, Some("Terminal")),
        KeyBinding::new("ctrl-n", NewTextFile, None),
        // VS Code's notifications.hideList and hideToasts, which skip the shell.
        KeyBinding::new("escape", HideNotifications, Some("notificationsVisible")),
    ];
    // Extensions' after the built-in ones, which they override as in VS Code.
    list.extend(exthost::key_bindings(mapper));
    // VS Code's terminal sends a key to the shell unless it's a chord or
    // its command is in `terminal.integrated.commandsToSkipShell`'s
    // defaults; these are vsn's actions from that list.
    const SKIP_SHELL: &[&str] = &[
        "TogglePanel",
        "ToggleTerminal",
        "CommandPalette",
        "QuickOpen",
        "OpenView",
        "NewTerminal",
        "SplitTerminal",
        "KillTerminal",
        "StartDebugging",
        "RunWithoutDebugging",
        "StopDebugging",
        "RestartDebugging",
        "Pause",
        "StepOver",
        "StepInto",
        "StepOut",
        "NextEditor",
        "PreviousEditor",
        "HideNotifications",
    ];
    let terminal: Rc<_> = KeyBindingContextPredicate::parse("Terminal")
        .unwrap()
        .into();
    let pass: Vec<_> = (list.iter())
        .filter(|b| {
            b.keystrokes().len() == 1
                && b.predicate()
                    .is_none_or(|p| !p.to_string().contains("Terminal"))
        })
        .filter(|b| !SKIP_SHELL.contains(&b.action().name().rsplit("::").next().unwrap_or("")))
        .map(|b| b.keystrokes()[0].unparse())
        // Alt+F4 still closes the window on Windows.
        .filter(|k| k != "alt-f4")
        .collect::<HashSet<_>>()
        .into_iter()
        .filter_map(|k| {
            KeyBinding::load(
                &k,
                Box::new(NoAction),
                Some(terminal.clone()),
                false,
                None,
                mapper,
            )
            .ok()
        })
        .collect();
    list.extend(pass);
    // After the pass-through, which would otherwise outrank them.
    let split = Some("Terminal && terminalSplitPaneActive");
    list.extend([
        KeyBinding::new("alt-left", FocusPreviousTerminal, split),
        KeyBinding::new("alt-up", FocusPreviousTerminal, split),
        KeyBinding::new("alt-right", FocusNextTerminal, split),
        KeyBinding::new("alt-down", FocusNextTerminal, split),
    ]);
    list
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Explorer,
    Search,
    Scm,
    Run,
    Extensions,
    /// An extension's view container, `exthost::view_containers()[ix]`.
    Ext(usize),
}

const PANEL_TABS: [&str; 5] = ["Problems", "Output", "Debug Console", "Terminal", "Ports"];
const TERMINAL: usize = 3;
/// VS Code's default `editor.tabSize`, spaces for indents.
const TAB_SIZE: usize = 4;

/// VS Code's `editor.detectIndentation`, after its `guessIndentation`: tabs
/// when more lines start with a tab than with spaces, else the indent step
/// most often between neighbouring space-indented lines (2, 4, 6, 8, 3, 5,
/// 7 winning ties in that order), else the default.
fn detect_indent(text: &str) -> TabSize {
    let (mut tabs, mut spaces, mut prev) = (0, 0, 0);
    let mut steps = [0usize; 9];
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let lead = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
        if lead.starts_with('\t') {
            tabs += 1;
        } else if !lead.is_empty() {
            spaces += 1;
        }
        if !lead.contains('\t') {
            steps[lead.len().abs_diff(prev).min(8)] += 1;
            prev = lead.len();
        }
    }
    // Equal-indent neighbours say nothing about the step.
    steps[0] = 0;
    let step = [2, 4, 6, 8, 3, 5, 7]
        .into_iter()
        .fold(0, |best, s| if steps[s] > steps[best] { s } else { best });
    TabSize {
        tab_size: if tabs > spaces || step == 0 {
            TAB_SIZE
        } else {
            step
        },
        hard_tabs: tabs > spaces,
    }
}

/// After Ctrl+D, as selections other than the first aren't readable.
struct NextMatch {
    editor: EntityId,
    /// The first selection then.
    sel: Range<usize>,
    /// The matches added since, in order.
    added: Vec<Range<usize>>,
    /// Where the next Add Next / Previous Occurrence searches from.
    after: usize,
    before: usize,
    whole: bool,
}

/// Which of the Selection menu's occurrence commands runs.
#[derive(PartialEq)]
enum Occurrence {
    Next,
    Previous,
    All,
    MoveLast,
}

struct Doc {
    /// `Untitled-N` for a new file that was never saved.
    path: PathBuf,
    editor: Entity<EditorState>,
    dirty: bool,
    /// [`text_hash`] of the text as last opened or saved: an edit back to it
    /// clears `dirty`, as undoing to the saved text does in VS Code.
    saved: u64,
    crlf: bool,
    /// VS Code's language id: the file's, or Change Language Mode's pick.
    lang: &'static str,
    /// Opened without line breaks, so `crlf` is VS Code's files.eol "auto"
    /// (CRLF on Windows) and Enter's bare LFs don't overrule it.
    eol_default: bool,
    encoding: Encoding,
    /// Shows VS Code's binary placeholder instead of the editor (left empty).
    binary: bool,
    /// Detected from the text as it opened.
    indent: TabSize,
    untitled: bool,
    /// Alt+Z's word wrap for this editor alone, overriding editor.wordWrap.
    wrap: Option<bool>,
    /// An extension's details instead of a file.
    page: Option<extensions::Page>,
    /// The Settings editor, when this tab is it.
    settings: Option<Box<settings_ui::Page>>,
    /// The Markdown file this tab previews.
    preview: Option<PathBuf>,
}

fn session_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("vsn").join("session.json"))
}

/// What [`Workbench::save_session`] last wrote: `last`, the folder open (null
/// for none), and `editors`, each folder's open files and active one.
pub fn session() -> serde_json::Value {
    (session_path().and_then(|p| fs::read(p).ok()))
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

/// The encodings VS Code detects without `files.autoGuessEncoding`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Encoding {
    #[default]
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

/// A hash of an editor's text, to tell whether it's back to the saved text.
/// Whether files.autoSave saves after a delay; other modes count as off.
/// Whether settings `s` turn on editor.wordWrap (any mode but "off"; VS Code's
/// default is "off").
// ponytail: "wordWrapColumn"/"bounded" wrap at the window edge, not the column.
fn word_wrap(s: &serde_json::Value) -> bool {
    s["editor.wordWrap"].as_str().is_some_and(|v| v != "off")
}

/// workbench.experimental.modernUI, on by default as in VS Code.
fn modern_ui(s: &serde_json::Value) -> bool {
    let on = s["workbench.experimental.modernUI"] != false;
    crate::theme::MODERN.store(on, std::sync::atomic::Ordering::Relaxed);
    on
}

fn auto_save() -> bool {
    terminal::settings()["files.autoSave"] == "afterDelay"
}

fn text_hash(editor: &Entity<EditorState>, cx: &App) -> u64 {
    use std::hash::Hasher;
    let mut h = std::hash::DefaultHasher::new();
    for chunk in editor.read(cx).text().chunks() {
        h.write(chunk.as_bytes());
    }
    h.finish()
}

/// The edit for Copy/Move Line Up/Down on the lines `sel` touches in
/// `text`: the range to replace, its replacement, and the selection after.
/// None when there's no line to move past.
fn lines_moved(
    text: &str,
    sel: Range<usize>,
    up: bool,
    copy: bool,
) -> Option<(Range<usize>, String, Range<usize>)> {
    let line_start = |i: usize| text[..i].rfind('\n').map_or(0, |n| n + 1);
    let line_end = |i: usize| text[i..].find('\n').map_or(text.len(), |n| i + n);
    let Range { start, end } = selected_lines(text, &sel);
    let block = &text[start..end];
    let shift = |by: isize| {
        let at = |i: usize| i.saturating_add_signed(by);
        at(sel.start)..at(sel.end)
    };
    Some(match (up, copy) {
        (true, true) => (start..start, format!("{block}\n"), sel),
        (false, true) => (
            end..end,
            format!("\n{block}"),
            shift(block.len() as isize + 1),
        ),
        (true, false) => {
            let prev = line_start(start.checked_sub(1)?);
            let above = &text[prev..start - 1];
            (
                prev..end,
                format!("{block}\n{above}"),
                shift(-(above.len() as isize + 1)),
            )
        }
        (false, false) => {
            (end < text.len()).then_some(())?;
            let below = &text[end + 1..line_end(end + 1)];
            let range = start..end + 1 + below.len();
            (
                range,
                format!("{below}\n{block}"),
                shift(below.len() as isize + 1),
            )
        }
    })
}

/// The byte range of the whole lines `sel` touches in `text`, without the
/// last one's line break.
fn selected_lines(text: &str, sel: &Range<usize>) -> Range<usize> {
    let line_start = |i: usize| text[..i].rfind('\n').map_or(0, |n| n + 1);
    // A selection ending at a line's start doesn't take that line, as in VS Code.
    let last = match sel.end > sel.start && sel.end == line_start(sel.end) {
        true => sel.end - 1,
        false => sel.end,
    };
    line_start(sel.start)..text[last..].find('\n').map_or(text.len(), |n| last + n)
}

/// The word (letters, digits, `_`) touching byte `at` of `text`.
fn word_around(text: &str, at: usize) -> Option<Range<usize>> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..at].trim_end_matches(word).len();
    let end = text.len() - text[at..].trim_start_matches(word).len();
    (start < end).then_some(start..end)
}

/// Where `needle` occurs in `text`: as a whole word, case-sensitively;
/// anywhere, in its `case`; or anywhere in any case, which can make a match
/// longer or shorter than it.
fn occurrences(text: &str, needle: &str, whole: bool, case: bool) -> Vec<Range<usize>> {
    if whole || case {
        return text
            .match_indices(needle)
            .map(|(at, _)| at..at + needle.len())
            .filter(|r| !whole || word_around(text, r.start).as_ref() == Some(r))
            .collect();
    }
    let same = |a: char, b: char| {
        a == b || a.to_lowercase().eq(b.to_lowercase()) || a.to_uppercase().eq(b.to_uppercase())
    };
    let mut found: Vec<Range<usize>> = Vec::new();
    for (at, _) in text.char_indices() {
        if found.last().is_some_and(|r| at < r.end) {
            continue;
        }
        let mut rest = text[at..].char_indices();
        let end = needle.chars().try_fold(at, |_, n| match rest.next() {
            Some((i, c)) if same(c, n) => Some(at + i + c.len_utf8()),
            _ => None,
        });
        if let Some(end) = end.filter(|&end| end > at) {
            found.push(at..end);
        }
    }
    found
}

/// The smallest range around `sel` that's bigger than it: the word, the
/// inside of the brackets around it, the brackets too, the line's text, the
/// whole line, or the whole text.
// ponytail: brackets and words, not VS Code's syntax tree, so strings and
// brackets inside strings or comments don't count.
fn expanded(text: &str, sel: Range<usize>) -> Option<Range<usize>> {
    let mut found: Vec<Range<usize>> = word_around(text, sel.start).into_iter().collect();
    let lines = selected_lines(text, &sel);
    // Without a CRLF's \r, which the editor won't end a selection on.
    let line = text[lines.clone()].trim_end_matches('\r');
    let start = lines.start + line.len() - line.trim_start().len();
    found.extend([
        start..lines.start + line.trim_end().len(),
        lines.start..lines.start + line.len(),
        0..text.len(),
    ]);
    let (mut from, mut to) = (sel.start, sel.end);
    while let Some((open, close)) = enclosing_brackets(text, from, to) {
        found.extend([open + 1..close, open..close + 1]);
        (from, to) = (open, close + 1);
    }
    let around = |r: &Range<usize>| r.start <= sel.start && sel.end <= r.end && r.len() > sel.len();
    found.into_iter().filter(around).min_by_key(|r| r.len())
}

/// The nearest bracket pair around `from..to` in `text`: the open and close
/// bracket's offsets.
fn enclosing_brackets(text: &str, mut from: usize, to: usize) -> Option<(usize, usize)> {
    let pairs = [('(', ')'), ('[', ']'), ('{', '}')];
    loop {
        let mut depth = 0;
        let (open, (o, c)) = text[..from].char_indices().rev().find_map(|(i, ch)| {
            if pairs.iter().any(|p| p.1 == ch) {
                depth += 1;
            } else if let Some(&p) = pairs.iter().find(|p| p.0 == ch) {
                if depth == 0 {
                    return Some((i, p));
                }
                depth -= 1;
            }
            None
        })?;
        let mut depth = 0;
        let close = text[open + 1..]
            .char_indices()
            .find_map(|(i, ch)| match ch {
                _ if ch == c && depth == 0 => Some(open + 1 + i),
                _ if ch == c => {
                    depth -= 1;
                    None
                }
                _ if ch == o => {
                    depth += 1;
                    None
                }
                _ => None,
            })?;
        // A pair closing inside the selection doesn't enclose it.
        if close >= to {
            return Some((open, close));
        }
        from = open;
    }
}

/// Language `lang`'s line comment token, from VS Code's language
/// configurations.
fn line_comment(lang: &str) -> Option<&'static str> {
    if let Some((line, _)) = extensions::comments(lang) {
        return line;
    }
    Some(match lang {
        "rust" | "typescript" | "typescriptreact" | "javascript" | "javascriptreact" | "json"
        | "jsonc" | "c" | "cpp" | "csharp" | "go" | "java" | "kotlin" | "php" | "swift" | "zig"
        | "scss" => "//",
        "python" | "ruby" | "toml" | "yaml" | "shellscript" | "powershell" | "makefile"
        | "dockerfile" | "ignore" => "#",
        "lua" | "sql" => "--",
        "bat" => "@REM",
        _ => return None,
    })
}

/// Language `lang`'s block comment start and end, from VS Code's language
/// configurations.
fn block_comment(lang: &str) -> Option<(&'static str, &'static str)> {
    if let Some((_, block)) = extensions::comments(lang) {
        return block;
    }
    Some(match lang {
        "rust" | "typescript" | "typescriptreact" | "javascript" | "javascriptreact" | "json"
        | "jsonc" | "c" | "cpp" | "csharp" | "go" | "java" | "kotlin" | "php" | "swift" | "css"
        | "scss" | "sql" => ("/*", "*/"),
        "html" | "xml" | "markdown" => ("<!--", "-->"),
        "python" => ("\"\"\"", "\"\"\""),
        "lua" => ("--[[", "]]"),
        "powershell" => ("<#", "#>"),
        "ruby" => ("=begin", "=end"),
        _ => return None,
    })
}

/// The edit for Toggle Block Comment with `(open, close)` on `sel` in
/// `text`, as for [`lines_moved`]: unwraps a comment just around or just
/// inside the selection (with a space next to each token), else wraps the
/// selection in "open " and " close" and keeps the selection on it.
fn block_toggled(
    text: &str,
    sel: Range<usize>,
    (open, close): (&str, &str),
) -> Option<(Range<usize>, String, Range<usize>)> {
    let inner = &text[sel.clone()];
    let before = &text[..sel.start];
    let before = (before
        .strip_suffix(' ')
        .unwrap_or(before)
        .strip_suffix(open))
    .map(|b| sel.start - b.len());
    let after = &text[sel.end..];
    let after = (after.strip_prefix(' ').unwrap_or(after).strip_prefix(close))
        .map(|a| after.len() - a.len());
    if let (Some(before), Some(after)) = (before, after) {
        let start = sel.start - before;
        return Some((
            start..sel.end + after,
            inner.into(),
            start..start + inner.len(),
        ));
    }
    let body = inner.trim();
    if body.len() >= open.len() + close.len() && body.starts_with(open) && body.ends_with(close) {
        let lead = inner.len() - inner.trim_start().len();
        let mid = &body[open.len()..body.len() - close.len()];
        let mid = mid.strip_prefix(' ').unwrap_or(mid);
        let mid = mid.strip_suffix(' ').unwrap_or(mid);
        let new = format!("{}{mid}{}", &inner[..lead], &inner[lead + body.len()..]);
        let end = sel.start + new.len();
        return Some((sel.clone(), new, sel.start..end));
    }
    let at = sel.start + open.len() + 1;
    Some((
        sel.clone(),
        format!("{open} {inner} {close}"),
        at..at + inner.len(),
    ))
}

/// The edit for Toggle Line Comment with `token` on the lines `sel` touches
/// in `text`, as for [`lines_moved`]: removes the token (and a space after
/// it) when every non-blank line starts with it, else adds "token " at their
/// least indentation. `add` Some(true) always adds (Add Line Comment), and
/// Some(false) always removes (Remove Line Comment). None when nothing
/// changes.
fn comment_toggled(
    text: &str,
    sel: Range<usize>,
    token: &str,
    add: Option<bool>,
) -> Option<(Range<usize>, String, Range<usize>)> {
    let lines = selected_lines(text, &sel);
    let indent = |l: &str| l.len() - l.trim_start().len();
    let mut offset = lines.start;
    // (where, bytes removed, text inserted) per line, in order.
    let mut edits = Vec::new();
    let body = text[lines.clone()].split('\n').map(|l| {
        let at = offset;
        offset += l.len() + 1;
        (at, l)
    });
    let body: Vec<_> = body.filter(|(_, l)| !l.trim().is_empty()).collect();
    let col = body.iter().map(|(_, l)| indent(l)).min()?;
    let insert = format!("{token} ");
    let commented = |l: &str| l.trim_start().starts_with(token);
    if add.unwrap_or(!body.iter().all(|(_, l)| commented(l))) {
        for (at, _) in &body {
            edits.push((at + col, 0, insert.as_str()));
        }
    } else {
        for (at, l) in body.iter().filter(|(_, l)| commented(l)) {
            let i = indent(l);
            let space = l[i + token.len()..].starts_with(' ') as usize;
            edits.push((at + i, token.len() + space, ""));
        }
    }
    (!edits.is_empty()).then(|| apply_edits(text, lines, &edits, sel))
}

/// `edits` (position, bytes removed, text inserted), in order, applied to
/// `text[lines]`: the range, its replacement, and `sel` moved along.
fn apply_edits(
    text: &str,
    lines: Range<usize>,
    edits: &[(usize, usize, &str)],
    sel: Range<usize>,
) -> (Range<usize>, String, Range<usize>) {
    let mut out = String::new();
    let mut from = lines.start;
    for &(at, removed, inserted) in edits {
        out.push_str(&text[from..at]);
        out.push_str(inserted);
        from = at + removed;
    }
    out.push_str(&text[from..lines.end]);
    let moved = |pos: usize| {
        let mut new = pos;
        for &(at, removed, inserted) in edits {
            if at <= pos {
                new = new - pos.min(at + removed).saturating_sub(at) + inserted.len();
            }
        }
        new
    };
    (lines, out, moved(sel.start)..moved(sel.end))
}

/// `text` with every line ending made CRLF or LF, as VS Code keeps one
/// ending per file.
fn with_eol(text: &str, crlf: bool) -> String {
    let lf = text.replace("\r\n", "\n");
    if crlf { lf.replace('\n', "\r\n") } else { lf }
}

impl Encoding {
    // ponytail: VS Code lists ~50 encodings (Windows 1252, Shift JIS, ...);
    // these 4 are the ones std can read and write without encoding_rs.
    const ALL: [Self; 4] = [Self::Utf8, Self::Utf8Bom, Self::Utf16Le, Self::Utf16Be];

    /// VS Code's `files.encoding` id, shown beside the label in its picker.
    fn id(self) -> &'static str {
        match self {
            Self::Utf8 => "utf8",
            Self::Utf8Bom => "utf8bom",
            Self::Utf16Le => "utf16le",
            Self::Utf16Be => "utf16be",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf8Bom => "UTF-8 with BOM",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
        }
    }

    /// VS Code's `detectEncodingFromBuffer`: a BOM, else UTF-16 when NUL bytes
    /// in the first 512 sit at every other index, else binary when there is any
    /// NUL, else UTF-8 (invalid sequences become U+FFFD, as in VS Code).
    /// `None` means binary.
    fn decode(bytes: &[u8]) -> Option<(String, Self)> {
        let head = &bytes[..bytes.len().min(512)];
        // ASCII as UTF-16 LE has its zero bytes at exactly the odd indexes, BE at the even ones.
        let zeros_at_odd =
            |odd: bool| (head.iter().enumerate()).all(|(i, &b)| (b == 0) == ((i % 2 == 1) == odd));
        let encoding = match bytes {
            [0xEF, 0xBB, 0xBF, ..] => Self::Utf8Bom,
            [0xFF, 0xFE, ..] => Self::Utf16Le,
            [0xFE, 0xFF, ..] => Self::Utf16Be,
            _ if !head.contains(&0) => Self::Utf8,
            _ if zeros_at_odd(true) => Self::Utf16Le,
            _ if zeros_at_odd(false) => Self::Utf16Be,
            _ => return None,
        };
        Some((encoding.decode_as(bytes), encoding))
    }

    /// `bytes` read as this encoding, less a BOM of its own.
    fn decode_as(self, bytes: &[u8]) -> String {
        let utf16 = |bom: [u8; 2], be: bool| {
            let bytes = bytes.strip_prefix(&bom).unwrap_or(bytes);
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&b| match be {
                    true => u16::from_be_bytes(b),
                    false => u16::from_le_bytes(b),
                })
                .collect();
            String::from_utf16_lossy(&units)
        };
        match self {
            Self::Utf8 | Self::Utf8Bom => {
                let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
                String::from_utf8_lossy(bytes).into_owned()
            }
            Self::Utf16Le => utf16([0xFF, 0xFE], false),
            Self::Utf16Be => utf16([0xFE, 0xFF], true),
        }
    }

    fn encode(self, text: &str) -> Vec<u8> {
        let utf16 = |bom: u16, be: bool| {
            std::iter::once(bom)
                .chain(text.encode_utf16())
                .flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() })
                .collect()
        };
        match self {
            Self::Utf8 => text.as_bytes().to_vec(),
            Self::Utf8Bom => [&[0xEF, 0xBB, 0xBF], text.as_bytes()].concat(),
            Self::Utf16Le => utf16(0xFEFF, false),
            Self::Utf16Be => utf16(0xFEFF, true),
        }
    }
}

pub struct Workbench {
    root: Option<PathBuf>,
    expanded: HashSet<PathBuf>,
    /// The explorer folder a drag would drop into, tinted with its children.
    drop_dir: Option<PathBuf>,
    /// The explorer item copied (or, true, cut) for a paste.
    clip: Option<(PathBuf, bool)>,
    tree_scroll: UniformListScrollHandle,
    /// The explorer's selected row: the active editor's file, or a folder
    /// clicked since.
    tree_sel: Option<PathBuf>,
    /// The row with the focus outline that the keys move from: the
    /// selection, or a row right-clicked since.
    tree_cursor: Option<PathBuf>,
    tree_focus: FocusHandle,
    /// Explorer listings (folders first, then name), dropped when the watcher sees them change.
    dirs: HashMap<PathBuf, Vec<(PathBuf, bool)>>,
    watcher: Option<(notify::RecommendedWatcher, Task<()>)>,
    /// Set once the user agreed to quit, so closing doesn't ask twice.
    quitting: bool,
    root_open: bool,
    docs: Vec<Doc>,
    active: usize,
    view: View,
    sidebar: bool,
    panel: bool,
    /// The panel fills the editor's space too (Maximize Panel Size).
    panel_max: bool,
    panel_tab: usize,
    search: Entity<InputState>,
    find: search::Find,
    palette: palette::Palette,
    /// The toasts over the status bar, oldest first: each one's message,
    /// whether the mouse is over it, and the timer that hides it.
    toasts: Vec<(SharedString, bool, Task<()>)>,
    /// Every notification, newest first, as the bell's center lists them.
    /// Notifications, newest first.
    notes: Vec<(SharedString, Severity)>,
    /// The breadcrumb row, scrolled to show the file name.
    crumbs: ScrollHandle,
    /// The file and row width the breadcrumb and the tab strip were last
    /// scrolled for, so they can be scrolled back by hand in between.
    crumbs_at: (PathBuf, Pixels),
    /// The tab strip's scroll, for its scrollbar.
    tabs_scroll: ScrollHandle,
    /// The area under the mouse whose scrollbar shows ("tabs", "crumbs",
    /// "tree").
    scroll_hover: Option<&'static str>,
    /// workbench.experimental.modernUI: the parts float as rounded cards.
    modern: bool,
    /// Notifications added since the center was last open: the bell's dot.
    unseen: usize,
    /// Whether the bell's notification center is open.
    center: bool,
    /// The activity bar item whose hover shows, the one waiting to, and when
    /// a hover last hid (moving to the next item within a moment shows its
    /// hover at once, as VS Code does).
    act_hover: Option<SharedString>,
    act_wait: Option<(SharedString, Task<()>)>,
    act_hid: Option<Instant>,
    /// The OS window's title, as last set.
    title: String,
    /// The Markdown file shown with its preview beside it (Ctrl+K V).
    side_preview: Option<PathBuf>,
    /// The folder and editors as last saved by [`Self::save_session`].
    saved_session: String,
    /// Go Back/Forward's history of editor locations, and the current one.
    nav: Vec<(PathBuf, input::Position)>,
    nav_at: usize,
    /// Where the last edit happened, for Go to Last Edit Location.
    last_edit: Option<(PathBuf, input::Position)>,
    /// The snippet whose tab stops Tab moves between.
    snippet: Option<extensions::Stops>,
    /// files.autoSave "afterDelay": the pending save of dirty editors.
    autosave: Option<Task<()>>,
    /// editor.wordWrap, for editors that Alt+Z hasn't set on their own.
    word_wrap: bool,
    /// What had focus when a menu bar menu was opened, so its Edit and
    /// Selection entries reach that editor rather than the menu's own path.
    menu_focus: Option<FocusHandle>,
    next_match: Option<NextMatch>,
    /// The editor Expand Selection last ran in and its selections, the
    /// current last, for Shrink Selection to step back through.
    expand_history: Option<(EntityId, Vec<Range<usize>>)>,
    debug: debug::Debug,
    /// Spawned when the Terminal tab first shows; the error if the shell didn't start.
    terminals: terminal::Terminals,
    ext: exthost::Host,
    lsp: lsp::Lsp,
    output: output::Output,
    commit: Entity<InputState>,
    ext_search: Entity<InputState>,
    market: extensions::Market,
    pub focus: FocusHandle,
}

impl Workbench {
    pub fn new(root: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let mut this = Self {
            root,
            expanded: HashSet::new(),
            drop_dir: None,
            clip: None,
            tree_scroll: UniformListScrollHandle::new(),
            tree_sel: None,
            tree_cursor: None,
            tree_focus: cx.focus_handle(),
            dirs: HashMap::new(),
            watcher: None,
            quitting: false,
            root_open: true,
            docs: Vec::new(),
            active: 0,
            view: View::Explorer,
            sidebar: true,
            panel: true,
            panel_max: false,
            panel_tab: TERMINAL,
            search: input("Search", window, cx),
            commit: input("Message (Ctrl+Enter to commit)", window, cx),
            ext_search: input("Search Extensions in Marketplace", window, cx),
            market: Default::default(),
            focus: cx.focus_handle(),
            find: search::Find::new(window, cx),
            palette: palette::Palette::new(window, cx),
            toasts: Vec::new(),
            notes: Vec::new(),
            crumbs: ScrollHandle::new(),
            crumbs_at: Default::default(),
            tabs_scroll: ScrollHandle::new(),
            scroll_hover: None,
            modern: modern_ui(&terminal::settings()),
            unseen: 0,
            center: false,
            act_hover: None,
            act_wait: None,
            act_hid: None,
            title: String::new(),
            side_preview: None,
            saved_session: String::new(),
            nav: Vec::new(),
            nav_at: 0,
            last_edit: None,
            snippet: None,
            autosave: None,
            word_wrap: word_wrap(&terminal::settings()),
            menu_focus: None,
            next_match: None,
            expand_history: None,
            debug: debug::Debug::new(window, cx),
            terminals: Default::default(),
            ext: exthost::Host::new(window.window_handle()),
            lsp: Default::default(),
            output: output::Output::new(),
        };
        this.init_search(window, cx);
        this.init_debug(window, cx);
        cx.subscribe(&this.ext_search, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.search_market(cx);
            }
        })
        .detach();
        cx.subscribe(&this.commit, |this, _, ev: &InputEvent, cx| match ev {
            InputEvent::Change => this.scm_input(cx),
            InputEvent::PressEnter {
                secondary: true, ..
            } => this.scm_accept(),
            _ => {}
        })
        .detach();
        this.watch_root(cx);
        this.activate_extensions("*", cx);
        // The title bar's close button (and Alt+F4) land here.
        let wb = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            wb.update(cx, |this, cx| this.can_close(window, cx))
                .unwrap_or(true)
        });
        this
    }

    /// VS Code's window.title: "● {file} - {folder} - {app}", the dot while dirty.
    fn window_title(&self) -> String {
        let doc = self.docs.get(self.active);
        let mut parts: Vec<String> = (doc.iter())
            .map(|d| {
                d.path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into()
            })
            .chain(self.root_name())
            .collect();
        parts.push("Visual Studio Native".into());
        let dirty = if doc.is_some_and(|d| d.dirty) {
            "● "
        } else {
            ""
        };
        format!("{dirty}{}", parts.join(" - "))
    }

    fn root_name(&self) -> Option<String> {
        self.root.as_ref().map(|r| {
            r.file_name()
                .unwrap_or(r.as_os_str())
                .to_string_lossy()
                .to_string()
        })
    }

    // ---- files -------------------------------------------------------------

    pub fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.docs.iter().position(|d| d.path == path) {
            self.activate(ix, window, cx);
            return;
        }
        let Ok(bytes) = fs::read(&path) else {
            return;
        };
        let decoded = Encoding::decode(&bytes);
        let binary = decoded.is_none();
        let (text, encoding) = decoded.unwrap_or_default();
        self.push_doc(path, text, encoding, binary, false, window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn push_doc(
        &mut self,
        path: PathBuf,
        text: String,
        encoding: Encoding,
        binary: bool,
        untitled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let eol_default = !text.contains('\n');
        let crlf = if eol_default {
            cfg!(windows)
        } else {
            text.contains("\r\n")
        };
        let indent = detect_indent(&text);
        let lang = self.file_language(&path);
        let editor = self.new_editor(&path, lang, text, indent, self.word_wrap, window, cx);
        self.docs.push(Doc {
            indent,
            lang,
            path,
            saved: text_hash(&editor, cx),
            editor,
            dirty: false,
            crlf,
            eol_default,
            encoding,
            binary,
            untitled,
            wrap: None,
            page: None,
            settings: None,
            preview: None,
        });
        self.activate(self.docs.len() - 1, window, cx);
        self.open_editors_changed(cx);
        self.activate_extensions(&format!("onLanguage:{lang}"), cx);
        let ix = self.docs.len() - 1;
        self.lsp_open(ix, cx);
        self.show_diagnostics(&self.docs[ix].path.clone(), cx);
    }

    /// Ctrl+N: an `Untitled-N` editor, saved through Save As.
    fn new_text_file(&mut self, _: &NewTextFile, window: &mut Window, cx: &mut Context<Self>) {
        let n = (1..)
            .find(|n| {
                let name = format!("Untitled-{n}");
                !self
                    .docs
                    .iter()
                    .any(|d| d.untitled && d.path == Path::new(&name))
            })
            .unwrap();
        let path = PathBuf::from(format!("Untitled-{n}"));
        self.push_doc(path, String::new(), Encoding::Utf8, false, true, window, cx);
    }

    /// File: Revert File: the active editor's file read again, dropping its
    /// changes; an Untitled editor is emptied.
    fn revert_file(&mut self, _: &RevertFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(self.active).filter(|d| !d.binary) else {
            return;
        };
        if doc.untitled {
            doc.editor.update(cx, |e, cx| e.replace_all("", window, cx));
        } else {
            self.reopen(self.active, doc.encoding, window, cx);
        }
    }

    /// Reads editor `ix`'s file again as `encoding`, dropping its changes:
    /// Reopen with Encoding, and the binary placeholder's Open Anyway (UTF-8,
    /// whatever the file holds).
    fn reopen(
        &mut self,
        ix: usize,
        encoding: Encoding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bytes) = self.docs.get(ix).and_then(|d| fs::read(&d.path).ok()) else {
            return;
        };
        // VS Code keeps a file's UTF-8 BOM when it's reopened as UTF-8.
        let encoding = match (encoding, &bytes[..]) {
            (Encoding::Utf8, [0xEF, 0xBB, 0xBF, ..]) => Encoding::Utf8Bom,
            _ => encoding,
        };
        let text = encoding.decode_as(&bytes);
        let doc = &self.docs[ix];
        let (path, lang, wrap) = (doc.path.clone(), doc.lang, self.wraps(doc));
        let old = doc.editor.read(cx);
        let (pos, scroll) = (old.cursor_position(), old.scroll_offset());
        let crlf = match text.contains('\n') {
            true => text.contains("\r\n"),
            false => doc.crlf,
        };
        let indent = detect_indent(&text);
        let editor = self.new_editor(&path, lang, text, indent, wrap, window, cx);
        editor.update(cx, |e, cx| {
            e.set_cursor_position(pos, window, cx);
            e.set_scroll_offset(scroll, cx);
        });
        let doc = &mut self.docs[ix];
        doc.indent = indent;
        doc.saved = text_hash(&editor, cx);
        doc.editor = editor;
        doc.encoding = encoding;
        doc.crlf = crlf;
        doc.dirty = false;
        doc.binary = false;
        self.activate(ix, window, cx);
        self.lsp_open(ix, cx);
        self.show_diagnostics(&path, cx);
    }

    /// An editor for `path`'s text, highlighted as language id `lang`.
    #[allow(clippy::too_many_arguments)]
    fn new_editor(
        &mut self,
        path: &Path,
        lang: &str,
        text: String,
        indent: TabSize,
        wrap: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<EditorState> {
        if let Some(config) = extensions::editing(lang) {
            gpui_kit::base::input::set_language_config(editor_language(lang), config, cx);
        }
        let lang = editor_language(lang);
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(lang)
                .searchable(true)
                .soft_wrap(wrap)
                .tab_size(indent)
        });
        editor.update(cx, |e, cx| e.set_value(text, window, cx));
        self.attach_breakpoints(path, &editor, cx);
        cx.subscribe_in(
            &editor,
            window,
            |this, editor, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::Change) {
                    // An edit ends a run of Ctrl+D.
                    this.next_match = None;
                }
                if matches!(ev, InputEvent::Change)
                    && let Some(doc) = this.docs.iter_mut().find(|d| &d.editor == editor)
                {
                    // ponytail: hashes the whole text per edit; keep a saved
                    // undo version instead if huge files lag.
                    doc.dirty = text_hash(editor, cx) != doc.saved;
                    // Most endings decide, as Enter inserts a bare LF and undoing
                    // Change End of Line Sequence brings the old endings back.
                    let text = editor.read(cx).text();
                    let breaks = text.lines_len() - 1;
                    if breaks > 0 {
                        let crs: usize = (text.chunks())
                            .map(|c| c.bytes().filter(|&b| b == b'\r').count())
                            .sum();
                        // ponytail: on a doc opened without breaks, undoing Change
                        // End of Line Sequence back to LF keeps the CRLF label;
                        // track the change's old ending if that matters.
                        if crs > 0 || !doc.eol_default {
                            doc.crlf = crs * 2 > breaks;
                        }
                    }
                    let (path, dirty) = (doc.path.clone(), doc.dirty);
                    if dirty && auto_save() {
                        this.schedule_autosave(window, cx);
                    }
                    this.last_edit = Some((path.clone(), editor.read(cx).cursor_position()));
                    this.refresh_search_file(&path, cx);
                    this.breakpoints_moved(&path, editor, cx);
                    if let Some(ix) = this.docs.iter().position(|d| &d.editor == editor) {
                        this.snippet_completed(ix, cx);
                        this.lsp_change(ix, cx);
                        this.signature_on_change(cx);
                    }
                }
                if matches!(ev, InputEvent::Blur) {
                    this.hide_signature(cx);
                }
                // Cursor moves also land here; re-render keeps "Ln, Col" current.
                cx.notify();
            },
        )
        .detach();
        editor
    }

    /// A tab dropped onto another's place: it moves there and is shown.
    /// Dropped files open at tab `at` (else after the others), in order; a
    /// dropped folder becomes the workspace.
    fn drop_paths(
        &mut self,
        paths: &[PathBuf],
        mut at: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for path in paths {
            if path.is_dir() {
                self.set_root(path.clone(), cx);
                continue;
            }
            self.open_recent(path.clone(), window, cx);
            let from = self.docs.iter().position(|d| &d.path == path);
            if let (Some(from), Some(to)) = (from, at) {
                self.move_doc(from, to, window, cx);
                at = Some(to + 1);
            }
        }
    }

    /// Copies files dropped from Windows into the explorer folder `dir`,
    /// asking before replacing any, and opens a single dropped file.
    fn import_paths(
        &mut self,
        paths: &[PathBuf],
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Nothing to copy onto itself, or into itself.
        let paths: Vec<_> = paths
            .iter()
            .filter(|p| p.parent() != Some(&dir) && !dir.starts_with(p))
            .cloned()
            .collect();
        let taken: Vec<_> = paths
            .iter()
            .filter_map(|p| p.file_name())
            .filter(|name| dir.join(name).exists())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        if taken.is_empty() {
            return self.copy_in(&paths, &dir, window, cx);
        }
        let message = match taken.as_slice() {
            [name] => format!(
                "A file or folder with the name '{name}' already exists in the destination folder. Do you want to replace it?"
            ),
            _ => format!(
                "The following {} files and/or folders already exist in the destination folder. Do you want to replace them?",
                taken.len()
            ),
        };
        let detail = match taken.len() {
            1 => "This action is irreversible!".to_string(),
            _ => format!("{}\n\nThis action is irreversible!", taken.join("\n")),
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(&detail),
            &["Replace", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            this.update_in(cx, |this, window, cx| {
                this.copy_in(&paths, &dir, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// An explorer item dropped on the folder `dir`: Ctrl copies it there.
    fn drop_file(
        &mut self,
        from: PathBuf,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match window.modifiers().control {
            true => self.copy_into(&from, dir, cx),
            false => self.move_into(from, dir, true, window, cx),
        }
    }

    /// Copies an explorer item into `dir`, beside any namesake rather than over it.
    fn copy_into(&mut self, from: &Path, dir: PathBuf, cx: &mut Context<Self>) {
        let Some(name) = from.file_name() else {
            return;
        };
        if dir.starts_with(from) {
            return;
        }
        let to = copy_target(&dir, name);
        if let Err(err) = copy_all(from, &to) {
            return self.error(format!("Unable to copy '{}' ({err})", from.display()), cx);
        }
        self.expanded.insert(dir);
        self.tree_sel = Some(to.clone());
        self.tree_cursor = Some(to);
        self.dirs.clear();
        cx.notify();
    }

    /// Pastes the cut or copied explorer item into the folder at `at`, or
    /// the folder of the file there, as VS Code's explorer Paste does.
    fn paste_file(&mut self, at: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((from, cut)), Some(root)) = (self.clip.clone(), self.root.clone()) else {
            return;
        };
        if !from.exists() {
            self.clip = None;
            return cx.notify();
        }
        let at = at.unwrap_or(root);
        let mut dir = match at.is_dir() {
            true => at,
            false => at.parent().unwrap_or(&at).to_path_buf(),
        };
        // A folder pasted onto itself goes beside it.
        if dir == from {
            dir = from.parent().unwrap_or(&from).to_path_buf();
        }
        if dir.starts_with(&from) {
            return self.error("File to paste is an ancestor of the destination folder", cx);
        }
        match cut {
            true => {
                self.clip = None;
                self.move_into(from, dir, false, window, cx);
            }
            false => self.copy_into(&from, dir, cx),
        }
    }

    /// Moves an explorer item into the folder `dir`, once confirmed when
    /// `confirm` (as VS Code's explorer drag does, `explorer.confirmDragAndDrop`)
    /// or when it would replace a namesake.
    fn move_into(
        &mut self,
        from: PathBuf,
        dir: PathBuf,
        confirm: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = from.file_name() else {
            return;
        };
        if dir.starts_with(&from) || from.parent() == Some(&dir) {
            return;
        }
        let to = dir.join(name);
        if !confirm && !to.exists() {
            return self.rename_into(&from, &to, &dir, cx);
        }
        let name = name.to_string_lossy();
        let (message, detail, ok) = match to.exists() {
            true => (
                format!(
                    "A file or folder with the name '{name}' already exists in the destination folder. Do you want to replace it?"
                ),
                Some("This action is irreversible!"),
                "Replace",
            ),
            false => {
                let into = dir.file_name().unwrap_or_default().to_string_lossy();
                (
                    format!("Are you sure you want to move '{name}' into '{into}'?"),
                    None,
                    "Move",
                )
            }
        };
        let answer = window.prompt(PromptLevel::Warning, &message, detail, &[ok, "Cancel"], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            this.update(cx, |this, cx| this.rename_into(&from, &to, &dir, cx))
                .ok();
        })
        .detach();
    }

    /// Moves `from` to `to` in the folder `dir`, replacing what's there.
    fn rename_into(&mut self, from: &Path, to: &Path, dir: &Path, cx: &mut Context<Self>) {
        let replaced = match to.is_dir() {
            true => fs::remove_dir_all(to),
            false if to.exists() => fs::remove_file(to),
            false => Ok(()),
        };
        if let Err(err) = replaced.and_then(|_| fs::rename(from, to)) {
            return self.error(format!("Unable to move '{}' ({err})", from.display()), cx);
        }
        // Open editors and expanded folders follow the move.
        let moved = |p: &Path| p.strip_prefix(from).ok().map(|rest| to.join(rest));
        for doc in &mut self.docs {
            if let Some(path) = moved(&doc.path) {
                doc.path = path;
            }
        }
        self.expanded = (self.expanded.drain())
            .map(|p| moved(&p).unwrap_or(p))
            .collect();
        self.expanded.insert(dir.to_path_buf());
        self.tree_sel = Some(to.to_path_buf());
        self.tree_cursor = Some(to.to_path_buf());
        self.dirs.clear();
        cx.notify();
    }

    /// Points an explorer drag at `dir`, unless dropping the item `moving`
    /// there would leave it where it is (or, `copy`ing, put it inside itself).
    fn drag_target(
        &mut self,
        dir: Option<PathBuf>,
        moving: Option<&Path>,
        copy: bool,
        cx: &mut Context<Self>,
    ) {
        let dir = dir.filter(|d| {
            moving.is_none_or(|from| !d.starts_with(from) && (copy || from.parent() != Some(d)))
        });
        if self.drop_dir != dir {
            self.drop_dir = dir;
            cx.notify();
        }
    }

    fn copy_in(
        &mut self,
        paths: &[PathBuf],
        dir: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut copied = Vec::new();
        for path in paths {
            let Some(name) = path.file_name() else {
                continue;
            };
            let to = dir.join(name);
            let replaced = match to.is_dir() {
                true => fs::remove_dir_all(&to),
                false if to.exists() => fs::remove_file(&to),
                false => Ok(()),
            };
            match replaced.and_then(|_| copy_all(path, &to)) {
                Ok(()) => copied.push(to),
                Err(err) => self.error(format!("Unable to copy '{}' ({err})", path.display()), cx),
            }
        }
        self.dirs.clear();
        self.expanded.insert(dir.to_path_buf());
        if let [file] = copied.as_slice() {
            self.tree_sel = Some(file.clone());
            if file.is_file() {
                self.open_path(file.clone(), window, cx);
            }
        }
        cx.notify();
    }

    fn move_doc(&mut self, from: usize, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let doc = self.docs.remove(from);
        self.docs.insert(to, doc);
        self.activate(to, window, cx);
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = ix;
        if let Some(doc) = self.docs.get(ix) {
            doc.editor.update(cx, |e, cx| e.focus(window, cx));
            if !doc.untitled {
                let path = doc.path.clone();
                self.palette.remember(self.root.as_deref(), &path, cx);
                self.reveal_in_tree(&path, false, cx);
            }
        }
        self.send_active_editor(cx);
        cx.notify();
    }

    /// VS Code's navigation history: an entry per editor switch or cursor
    /// jump of more than 10 lines; smaller moves update the current entry.
    fn record_nav(&mut self, cx: &App) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let pos = doc.editor.read(cx).cursor_position();
        match self.nav.get_mut(self.nav_at) {
            Some((path, at)) if *path == doc.path && at.line.abs_diff(pos.line) <= 10 => *at = pos,
            _ => {
                self.nav.truncate(self.nav_at + 1);
                self.nav.push((doc.path.clone(), pos));
                self.nav_at = self.nav.len() - 1;
            }
        }
    }

    /// Go Back/Forward: reopens the entry's file if it was closed, and drops
    /// entries that can't be (deleted files, closed untitled editors).
    fn navigate(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.record_nav(cx);
        loop {
            let at = self.nav_at;
            let ix = match back {
                true if at > 0 => at - 1,
                false if at + 1 < self.nav.len() => at + 1,
                _ => return,
            };
            let (path, pos) = self.nav[ix].clone();
            if self.go_to(path, pos, window, cx) {
                self.nav_at = ix;
                return;
            }
            self.nav.remove(ix);
            self.nav_at = if back { at - 1 } else { at };
        }
    }

    /// Next/Previous Editor: the tab `step` to the right, wrapping around.
    fn cycle(&mut self, step: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.docs.is_empty() {
            self.activate((self.active + step) % self.docs.len(), window, cx);
        }
    }

    /// Opens `path` with the cursor at `pos`; false if it can't be opened.
    fn go_to(
        &mut self,
        path: PathBuf,
        pos: input::Position,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.open_path(path.clone(), window, cx);
        let Some(doc) = self.docs.get(self.active).filter(|d| d.path == path) else {
            return false;
        };
        reveal(&doc.editor.clone(), pos, window, cx);
        true
    }

    /// Help > About: VS Code's version box, whose Copy puts the details on
    /// the clipboard.
    fn about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            arch => arch,
        };
        let detail = format!(
            "Version: {}\nCommit: {}\nDate: {}\nOS: Windows_NT {arch} {}",
            env!("CARGO_PKG_VERSION"),
            env!("VSN_COMMIT"),
            env!("VSN_COMMIT_DATE"),
            os_version(),
        );
        let answer = window.prompt(
            PromptLevel::Info,
            "Visual Studio Native",
            Some(&detail),
            &["Copy", "OK"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(0) {
                cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(detail)));
            }
        })
        .detach();
    }

    /// File > Open Recent > Clear Recently Opened..., behind VS Code's warning.
    fn clear_recent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            "Do you want to clear all recently opened files and workspaces?",
            Some("This action is irreversible!"),
            &["Clear", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                this.update(cx, |this, cx| this.palette.clear_opened(cx))
                    .ok();
            }
        })
        .detach();
    }

    fn write(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(ix) else {
            return;
        };
        // The placeholder's editor is empty; writing it would wipe the file.
        if doc.binary {
            return;
        }
        if doc.untitled {
            self.save_as(ix, window, cx);
            return;
        }
        let text = with_eol(&doc.editor.read(cx).value(), doc.crlf);
        let settings = terminal::settings_path().is_some_and(|p| p == doc.path);
        let old = settings.then(terminal::settings);
        match fs::write(&doc.path, doc.encoding.encode(&text)) {
            Ok(()) => {
                doc.dirty = false;
                doc.saved = text_hash(&doc.editor, cx);
                let path = doc.path.clone();
                self.lsp_saved(&path);
                if let Some(old) = old {
                    self.settings_changed(&old, window, cx);
                }
            }
            Err(err) => eprintln!("save {}: {err}", doc.path.display()),
        }
        cx.notify();
    }

    /// Applies settings.json's changes from `old`.
    fn settings_changed(
        &mut self,
        old: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.relanguage(old, window, cx);
        let new = terminal::settings();
        let changed = |key: &str| old[key] != new[key];
        if changed("workbench.colorTheme")
            || changed("workbench.iconTheme")
            || changed("workbench.experimental.modernUI")
        {
            extensions::apply_themes(cx);
        }
        self.modern = modern_ui(&new);
    }

    /// File: Save As: asks where to save editor `ix` (an untitled one in the
    /// folder, a file beside itself), then reopens it as that file (its
    /// language follows the new name; undo history starts over). Another tab
    /// on that file closes, as VS Code replaces it.
    fn save_as(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let doc = &self.docs[ix];
        let editor = doc.editor.clone();
        let dir = match doc.untitled {
            true => self.root.clone().or_else(|| std::env::current_dir().ok()),
            false => doc.path.parent().map(Path::to_path_buf),
        };
        let name = doc.path.file_name().unwrap_or_default().to_string_lossy();
        let path = cx.prompt_for_new_path(&dir.unwrap_or_default(), Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                // The tab may have moved or closed while the dialog was open.
                let Some(ix) = this.docs.iter().position(|d| d.editor == editor) else {
                    return;
                };
                let doc = &this.docs[ix];
                let text = with_eol(&editor.read(cx).value(), doc.crlf);
                if let Err(err) = fs::write(&path, doc.encoding.encode(&text)) {
                    eprintln!("save {}: {err}", path.display());
                    return;
                }
                let (indent, wrap) = (this.docs[ix].indent, this.wraps(&this.docs[ix]));
                let lang = this.file_language(&path);
                let editor = this.new_editor(&path, lang, text, indent, wrap, window, cx);
                let doc = &mut this.docs[ix];
                doc.saved = text_hash(&editor, cx);
                doc.lang = lang;
                doc.editor = editor.clone();
                doc.path = path.clone();
                doc.untitled = false;
                doc.dirty = false;
                if ix == this.active {
                    this.activate(ix, window, cx);
                }
                if this
                    .docs
                    .iter()
                    .any(|d| d.path == path && d.editor != editor)
                {
                    this.remove_where(|d| d.path == path && d.editor != editor, window, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        self.write(self.active, window, cx);
    }

    /// files.autoSave "afterDelay": saves dirty file editors once typing
    /// pauses for files.autoSaveDelay ms. Untitled editors wait for Save.
    // ponytail: one debounce for all editors, and only "afterDelay";
    // "onFocusChange"/"onWindowChange" need focus-out hooks.
    fn schedule_autosave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let delay = terminal::settings()["files.autoSaveDelay"]
            .as_u64()
            .unwrap_or(1000);
        self.autosave = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(delay))
                .await;
            _ = this.update_in(cx, |this, window, cx| {
                for ix in 0..this.docs.len() {
                    let d = &this.docs[ix];
                    if d.dirty && !d.untitled && auto_save() {
                        this.write(ix, window, cx);
                    }
                }
            });
        }));
    }

    /// File > Auto Save: flips files.autoSave between "off" and "afterDelay".
    fn toggle_auto_save(&mut self, _: &ToggleAutoSave, _: &mut Window, cx: &mut Context<Self>) {
        let value = if auto_save() { "off" } else { "afterDelay" };
        if let Err(err) = terminal::set_setting("files.autoSave", value.into()) {
            self.settings_error(err, cx);
        }
        cx.notify();
    }

    fn save_all(&mut self, _: &SaveAll, window: &mut Window, cx: &mut Context<Self>) {
        for ix in 0..self.docs.len() {
            if self.docs[ix].dirty {
                self.write(ix, window, cx);
            }
        }
    }

    fn close_editor(&mut self, _: &CloseEditor, window: &mut Window, cx: &mut Context<Self>) {
        self.close(self.active, window, cx);
    }

    fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(ix) else { return };
        if !doc.dirty {
            self.remove(ix, window, cx);
            return;
        }
        let name = doc.path.file_name().unwrap_or_default().to_string_lossy();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Do you want to save the changes you made to {name}?"),
            Some("Your changes will be lost if you don't save them."),
            &["Save", "Don't Save", "Cancel"],
            cx,
        );
        let editor = doc.editor.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else { return };
            this.update_in(cx, |this, window, cx| {
                // The tab may have moved while the dialog was open.
                let Some(ix) = this.docs.iter().position(|d| d.editor == editor) else {
                    return;
                };
                match choice {
                    0 => {
                        this.write(ix, window, cx);
                        if !this.docs[ix].dirty {
                            this.remove(ix, window, cx);
                        }
                    }
                    1 => this.remove(ix, window, cx),
                    _ => {}
                }
            })
            .ok();
        })
        .detach();
    }

    /// Closes the editors `pick` chooses (Close All, Close Others, Close to
    /// the Right), with one prompt for the dirty ones, as VS Code asks.
    fn close_editors(
        &mut self,
        pick: impl Fn(usize, &Doc) -> bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets: Vec<_> = (self.docs.iter().enumerate())
            .filter(|(ix, d)| pick(*ix, d))
            .map(|(_, d)| d.editor.clone())
            .collect();
        let names: Vec<_> = (self.docs.iter())
            .filter(|d| d.dirty && targets.contains(&d.editor))
            .map(|d| d.path.file_name().unwrap_or_default().to_string_lossy())
            .collect();
        let lost = "Your changes will be lost if you don't save them.";
        let (message, detail, save) = match names.as_slice() {
            [] => return self.remove_where(|d| targets.contains(&d.editor), window, cx),
            [name] => (
                format!("Do you want to save the changes you made to {name}?"),
                lost.to_string(),
                "Save",
            ),
            _ => (
                format!(
                    "Do you want to save the changes to the following {} files?",
                    names.len()
                ),
                format!("{}\n\n{lost}", names.join("\n")),
                "Save All",
            ),
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(&detail),
            &[save, "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else { return };
            this.update_in(cx, |this, window, cx| match choice {
                0 => {
                    for ix in 0..this.docs.len() {
                        if this.docs[ix].dirty && targets.contains(&this.docs[ix].editor) {
                            this.write(ix, window, cx);
                        }
                    }
                    // An untitled one still waits on its Save As dialog.
                    this.remove_where(|d| !d.dirty && targets.contains(&d.editor), window, cx);
                }
                1 => this.remove_where(|d| targets.contains(&d.editor), window, cx),
                _ => {}
            })
            .ok();
        })
        .detach();
    }

    /// Copy Path / Copy Relative Path (to the folder, else the full path).
    fn copy_path(&mut self, ix: usize, relative: bool, cx: &mut Context<Self>) {
        if let Some(doc) = self.docs.get(ix) {
            self.copy_file_path(&doc.path.clone(), relative, cx);
        }
    }

    fn copy_file_path(&self, path: &Path, relative: bool, cx: &mut Context<Self>) {
        let path = (self.root.as_ref())
            .filter(|_| relative)
            .and_then(|r| path.strip_prefix(r).ok())
            .unwrap_or(path);
        cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
    }

    /// Reveal in File Explorer: opens the file's folder with it selected.
    fn reveal_in_os(&mut self, ix: usize) {
        if let Some(doc) = self.docs.get(ix).filter(|d| !d.untitled) {
            reveal_path(&doc.path);
        }
    }

    /// Closes every editor `gone` picks, keeping the active one if it stays.
    fn remove_where(
        &mut self,
        gone: impl Fn(&Doc) -> bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.docs.get(self.active).map(|d| d.editor.clone());
        self.docs.retain(|d| !gone(d));
        self.lsp_closed();
        self.open_editors_changed(cx);
        self.active = (active.and_then(|e| self.docs.iter().position(|d| d.editor == e)))
            .unwrap_or(self.docs.len().saturating_sub(1));
        if self.docs.is_empty() {
            window.focus(&self.focus, cx);
        } else {
            self.activate(self.active, window, cx);
        }
        cx.notify();
    }

    fn remove(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.docs.remove(ix);
        self.lsp_closed();
        self.open_editors_changed(cx);
        if self.active > ix || self.active >= self.docs.len() {
            self.active = self.active.saturating_sub(1);
        }
        if self.docs.is_empty() {
            window.focus(&self.focus, cx);
        } else {
            self.activate(self.active, window, cx);
        }
        cx.notify();
    }

    fn open_file(&mut self, _: &OpenFile, _: &mut Window, cx: &mut Context<Self>) {
        self.prompt(false, cx);
    }

    fn open_folder(&mut self, _: &OpenFolder, _: &mut Window, cx: &mut Context<Self>) {
        self.prompt(true, cx);
    }

    /// Opens a file from outside the explorer, which VS Code lists in
    /// File > Open Recent.
    pub fn open_recent(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.palette.remember_opened(false, &path, cx);
        self.open_path(path, window, cx);
    }

    fn set_root(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let moved = self.root.as_ref() != Some(&path);
        self.root = Some(path);
        if moved {
            // VS Code reloads the window for a new folder, so its shells start there.
            self.restart_terminals(cx);
        }
        self.lsp_reset();
        self.expanded.clear();
        self.root_open = true;
        self.watch_root(cx);
        cx.notify();
    }

    fn prompt(&mut self, directories: bool, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: !directories,
            directories,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            this.update(cx, |this, cx| {
                if directories {
                    this.set_root(path, cx);
                } else if let Some(window) = cx.active_window() {
                    let this = cx.entity();
                    window
                        .update(cx, |_, window, cx| {
                            this.update(cx, |this, cx| this.open_recent(path, window, cx))
                        })
                        .ok();
                }
            })
            .ok();
        })
        .detach();
    }

    fn close_folder(&mut self, _: &CloseFolder, _: &mut Window, cx: &mut Context<Self>) {
        self.root = None;
        self.expanded.clear();
        self.watch_root(cx);
        cx.notify();
    }

    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_close(window, cx) {
            cx.quit();
        }
    }

    /// Close Window: the only window, so it quits like VS Code's last one.
    // ponytail: one window per process; close just this one once New Window opens more.
    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.quit(&Quit, window, cx);
    }

    /// Markdown: Open Preview, `source` (else the active Markdown file)
    /// rendered in a tab of its own; `side` shows it beside the file instead.
    /// Either follows the file's edits.
    // ponytail: the side preview splits the editor, not a second editor
    // group; no scroll sync, and relative images don't load.
    fn markdown_preview(
        &mut self,
        source: Option<PathBuf>,
        side: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = (self.docs.get(self.active))
            .filter(|d| d.lang == "markdown" && d.preview.is_none())
            .map(|d| d.path.clone());
        let Some(source) = source.or(active) else {
            return;
        };
        if side {
            self.open_path(source.clone(), window, cx);
            self.side_preview = Some(source);
            return cx.notify();
        }
        if let Some(ix) = self.docs.iter().position(|d| d.preview.as_ref() == Some(&source)) {
            return self.activate(ix, window, cx);
        }
        let name = source.file_name().unwrap_or_default().to_string_lossy();
        let path = PathBuf::from(format!("Preview {name}"));
        // Binary, so saving and the editor's commands leave its empty text alone.
        self.push_doc(path, String::new(), Encoding::Utf8, true, false, window, cx);
        self.docs.last_mut().unwrap().preview = Some(source);
    }

    /// The editor with its Markdown preview beside it, which has a header to
    /// close it in place of a second group's tab.
    fn render_side_preview(&self, code: Div, source: &Path, cx: &mut Context<Self>) -> AnyElement {
        let name = source.file_name().unwrap_or_default().to_string_lossy();
        let header = h_flex()
            .h(px(35.))
            .flex_none()
            .px(px(10.))
            .gap(px(6.))
            .text_size(px(13.))
            .border_b_1()
            .border_color(col::border())
            .child(codicon(icon::OPEN_PREVIEW))
            .child(div().flex_1().whitespace_nowrap().child(format!("Preview {name}")))
            .child(
                action_button("side-preview-close", icon::CLOSE)
                    .tooltip(tip("Close"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.side_preview = None;
                        cx.notify();
                    })),
            );
        h_flex()
            .size_full()
            .child(div().flex_1().min_w_0().h_full().child(code))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .border_l_1()
                    .border_color(col::border())
                    .child(header)
                    .child(div().flex_1().min_h_0().child(self.render_preview(usize::MAX, source, cx))),
            )
            .into_any_element()
    }

    fn render_preview(&self, ix: usize, source: &Path, cx: &App) -> AnyElement {
        let text = match self.docs.iter().find(|d| d.path == source) {
            Some(d) => d.editor.read(cx).value().to_string(),
            None => fs::read_to_string(source).unwrap_or_default(),
        };
        // VS Code's markdown.css: 26px sides, 14px text.
        div()
            .id(("md-preview", ix))
            .size_full()
            .overflow_y_scroll()
            .px(px(26.))
            .py(px(16.))
            .text_size(px(14.))
            .child(TextView::markdown(("md-preview-text", ix), text).selectable(true))
            .into_any_element()
    }

    /// Remembers the folder and its open editors for the next start, as VS
    /// Code's window.restoreWindows "all" reopens them.
    fn save_session(&self) {
        let Some(path) = session_path() else { return };
        let mut s = session();
        let files: Vec<_> = (self.docs.iter())
            .filter(|d| !d.untitled && d.page.is_none() && d.path.is_file())
            .collect();
        let active = (files.iter())
            .position(|d| self.docs.get(self.active).is_some_and(|a| a.path == d.path));
        let key = self
            .root
            .as_ref()
            .map_or(String::new(), |r| r.to_string_lossy().into_owned());
        s["last"] = serde_json::json!(self.root);
        s["editors"][key] = serde_json::json!({ "files": files.iter().map(|d| &d.path).collect::<Vec<_>>(), "active": active });
        fs::create_dir_all(path.parent().unwrap()).ok();
        fs::write(path, s.to_string()).ok();
    }

    /// Reopens the editors the root's folder had open when vsn last quit.
    pub fn restore_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self
            .root
            .as_ref()
            .map_or(String::new(), |r| r.to_string_lossy().into_owned());
        let s = session();
        let saved = &s["editors"][key];
        let files: Vec<PathBuf> =
            serde_json::from_value(saved["files"].clone()).unwrap_or_default();
        for path in files {
            self.open_path(path, window, cx);
        }
        let active = saved["active"].as_u64().and_then(|ix| {
            let path = saved["files"][ix as usize].as_str()?;
            self.docs.iter().position(|d| d.path == Path::new(path))
        });
        if let Some(ix) = active {
            self.activate(ix, window, cx);
        }
    }

    /// True when nothing is unsaved. Otherwise asks Save All / Don't Save / Cancel
    /// and quits by itself once the answer allows it.
    fn can_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let dirty: Vec<String> = self
            .docs
            .iter()
            .filter(|d| d.dirty)
            .map(|d| {
                d.path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        if self.quitting || dirty.is_empty() {
            self.save_session();
            return true;
        }
        let message = match dirty.as_slice() {
            [name] => format!("Do you want to save the changes you made to {name}?"),
            names => format!(
                "Do you want to save the changes to the following {} files?",
                names.len()
            ),
        };
        let detail = if dirty.len() > 1 {
            format!(
                "{}\n\nYour changes will be lost if you don't save them.",
                dirty.join("\n")
            )
        } else {
            "Your changes will be lost if you don't save them.".to_string()
        };
        let save = if dirty.len() > 1 { "Save All" } else { "Save" };
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(&detail),
            &[save, "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else { return };
            this.update_in(cx, |this, window, cx| {
                match choice {
                    0 => this.save_all(&SaveAll, window, cx),
                    1 => {}
                    _ => return,
                }
                // A failed save keeps its editor dirty; stay open so nothing is lost.
                if this.docs.iter().all(|d| !d.dirty) || choice == 1 {
                    this.save_session();
                    this.quitting = true;
                    cx.quit();
                }
            })
            .ok();
        })
        .detach();
        false
    }

    // ---- layout ------------------------------------------------------------

    fn show(&mut self, view: View, cx: &mut Context<Self>) {
        // Clicking the active activity bar item collapses the side bar, like VS Code.
        self.sidebar = !(self.sidebar && self.view == view);
        self.view = view;
        cx.notify();
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar = !self.sidebar;
        cx.notify();
    }

    fn toggle_panel(&mut self, _: &TogglePanel, _: &mut Window, cx: &mut Context<Self>) {
        self.panel = !self.panel;
        cx.notify();
    }

    /// Maximize or Restore Panel Size; a hidden panel shows maximized.
    fn toggle_max_panel(&mut self, cx: &mut Context<Self>) {
        self.panel_max = !self.panel || !self.panel_max;
        self.panel = true;
        cx.notify();
    }

    /// Ctrl+`: hides the focused terminal, else shows and focuses it.
    fn toggle_terminal(&mut self, _: &ToggleTerminal, window: &mut Window, cx: &mut Context<Self>) {
        if !(self.panel && self.panel_tab == TERMINAL && self.terminal_focused(window)) {
            return self.focus_terminal(window, cx);
        }
        self.panel = false;
        self.focus_editor(window, cx);
        cx.notify();
    }

    /// editor.wordWrap changed: wraps (or stops wrapping) the long lines of
    /// every editor that Alt+Z hasn't set on its own.
    fn set_word_wrap(&mut self, wrap: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.word_wrap = wrap;
        for doc in self.docs.iter().filter(|d| d.wrap.is_none()) {
            (doc.editor).update(cx, |e, cx| e.set_soft_wrap(wrap, window, cx));
        }
        cx.notify();
    }

    /// Whether `doc`'s editor wraps: its Alt+Z override, else editor.wordWrap.
    fn wraps(&self, doc: &Doc) -> bool {
        doc.wrap.unwrap_or(self.word_wrap)
    }

    /// Alt+Z (View: Toggle Word Wrap): flips the active editor only, as VS Code does.
    fn toggle_word_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let wrap = !self.wraps(doc);
        let doc = &mut self.docs[self.active];
        doc.wrap = Some(wrap);
        (doc.editor).update(cx, |e, cx| e.set_soft_wrap(wrap, window, cx));
        cx.notify();
    }

    /// Selection > Copy/Move Line Up/Down on the active editor's selected
    /// lines.
    // ponytail: the active selection only; VS Code moves every cursor's lines.
    fn line_op(&mut self, up: bool, copy: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_lines(|text, sel| lines_moved(text, sel, up, copy), window, cx)
    }

    /// Selection > Expand Selection (Shift+Alt+Right), or Shrink Selection
    /// back to what it was before the last expand.
    fn expand_selection(&mut self, expand: bool, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(self.active).filter(|d| !d.binary) else {
            return;
        };
        let id = doc.editor.entity_id();
        let mut history = match self.expand_history.take() {
            Some((i, h)) if i == id => h,
            _ => Vec::new(),
        };
        doc.editor.update(cx, |e, cx| {
            let sel = e.selected_range();
            if history.last() != Some(&sel) {
                history = vec![sel.clone()];
            }
            let next = match expand {
                true => expanded(&e.value(), sel),
                false if history.len() > 1 => {
                    history.pop();
                    history.last().cloned()
                }
                false => None,
            };
            if let Some(next) = next {
                e.set_selected_range(next, cx);
                // As the editor adjusted it, to recognize it next time.
                if expand {
                    history.push(e.selected_range());
                }
            }
        });
        self.expand_history = Some((id, history));
    }

    /// Selection > Add Next Occurrence (Ctrl+D), Add Previous Occurrence,
    /// Select All Occurrences (Ctrl+Shift+L) or Move Last Selection to Next
    /// Find Match (Ctrl+K Ctrl+D), of the active editor's selected text; an
    /// empty selection first selects the word around the cursor, and then
    /// matches whole words only, case-sensitively; otherwise any match, in any
    /// case, as VS Code's find defaults.
    fn add_occurrences(&mut self, op: Occurrence, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(self.active).filter(|d| !d.binary) else {
            return;
        };
        let id = doc.editor.entity_id();
        let last = self.next_match.take();
        self.next_match = doc.editor.update(cx, |e, cx| {
            let text = e.value();
            let mut sel = e.selected_range();
            let mut m = match last {
                Some(m) if m.editor == id && m.sel == sel => m,
                _ => NextMatch {
                    editor: id,
                    sel: sel.clone(),
                    added: Vec::new(),
                    after: sel.end,
                    before: sel.start,
                    whole: false,
                },
            };
            if sel.is_empty() {
                sel = word_around(&text, sel.start)?;
                e.set_selected_range(sel.clone(), cx);
                (m.sel, m.after, m.before, m.whole) = (sel.clone(), sel.end, sel.start, true);
                if op != Occurrence::All {
                    return Some(m);
                }
            }
            // The open find widget's Match Case applies, as in VS Code.
            let find = e.search_session();
            let case = find.open && !find.case_insensitive;
            let found = occurrences(&text, &text[sel.clone()], m.whole, case);
            if op == Occurrence::All {
                for r in found {
                    e.add_selection(r, cx);
                }
                return None;
            }
            let moved = (op == Occurrence::MoveLast).then(|| m.added.pop());
            // Matches not selected already.
            let free: Vec<Range<usize>> = (found.into_iter())
                .filter(|r| r.start != sel.start && m.added.iter().all(|a| a.start != r.start))
                .collect();
            let next = |from| {
                free.iter()
                    .find(|r| r.start >= from)
                    .or(free.first())
                    .cloned()
            };
            let r = match moved {
                None if op == Occurrence::Next => next(m.after)?,
                None => (free.iter().rfind(|r| r.end <= m.before))
                    .or(free.last())
                    .cloned()?,
                Some(moved) => match moved {
                    // The only selection moves.
                    None => {
                        let r = next(sel.end)?;
                        e.set_selected_range(r.clone(), cx);
                        (m.sel, m.after, m.before) = (r.clone(), r.end, r.start);
                        return Some(m);
                    }
                    Some(last) => {
                        let r = next(last.end)?;
                        e.set_selected_range(sel.clone(), cx);
                        for a in &m.added {
                            e.add_selection(a.clone(), cx);
                        }
                        r
                    }
                },
            };
            e.add_selection(r.clone(), cx);
            m.added.push(r.clone());
            match op {
                Occurrence::Previous => m.before = r.start,
                _ => m.after = r.end,
            }
            Some(m)
        });
    }

    /// Toggle (`add` None), Add or Remove Line Comment on the active editor's
    /// selected lines.
    fn comment_lines(&mut self, add: Option<bool>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lang) = self.docs.get(self.active).map(|d| d.lang) else {
            return;
        };
        match (line_comment(lang), block_comment(lang)) {
            (Some(token), _) => self.edit_lines(
                |text, sel| comment_toggled(text, sel, token, add),
                window,
                cx,
            ),
            // Without line comments, VS Code toggles a block comment on the lines.
            (None, Some(tokens)) => self.edit_lines(
                |text, sel| {
                    let lines = selected_lines(text, &sel);
                    let line = &text[lines.clone()];
                    let start = lines.start + line.len() - line.trim_start().len();
                    block_toggled(text, start..lines.end, tokens)
                },
                window,
                cx,
            ),
            (None, None) => {}
        }
    }

    /// Applies `edit`'s (range, replacement, selection after) for the active
    /// editor's text and selection as one undo step.
    fn edit_lines(
        &mut self,
        edit: impl FnOnce(&str, Range<usize>) -> Option<(Range<usize>, String, Range<usize>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.docs.get(self.active).filter(|d| !d.binary) else {
            return;
        };
        doc.editor.update(cx, |e, cx| {
            let text = e.value();
            if let Some((range, new, sel)) = edit(&text, e.selected_range()) {
                e.set_selected_range(range, cx);
                e.replace(new, window, cx);
                e.set_selected_range(sel, cx);
            }
        });
    }

    /// View > Problems / Output / Debug Console: shows the panel on tab
    /// `ix`, or hides it when it's already showing that tab.
    fn toggle_panel_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.panel = !(self.panel && self.panel_tab == ix);
        self.panel_tab = ix;
        cx.notify();
    }

    fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.docs.get(self.active) {
            Some(doc) => doc.editor.update(cx, |e, cx| e.focus(window, cx)),
            None => window.focus(&self.focus, cx),
        }
    }

    // ---- title bar ---------------------------------------------------------

    fn render_titlebar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // Open Recent leaves out the open folder, as VS Code does.
        let root = self.root.as_ref().map(|r| r.to_string_lossy());
        let folders = self.palette.folders.iter();
        let recent = Recent {
            // 20 leaves room for gone ones the menu skips.
            folders: (folders.filter(|f| Some(f.as_str()) != root.as_deref()))
                .take(20)
                .cloned()
                .collect(),
            files: self.palette.opened.iter().take(20).cloned().collect(),
            workbench: cx.entity().downgrade(),
        };
        let menubar = [
            "File",
            "Edit",
            "Selection",
            "View",
            "Go",
            "Run",
            "Terminal",
            "Help",
        ]
        .into_iter()
        .map(|name| {
            let wrap = self.docs.get(self.active).map(|d| self.wraps(d));
            let focus = self.menu_focus.clone();
            let recent = recent.clone();
            Button::new(name)
                .ghost()
                .compact()
                .label(name)
                .h(px(22.))
                .px(px(7.))
                .text_size(px(13.))
                .text_color(col::fg())
                .dropdown_menu(move |menu, window, cx| {
                    let menu = menu.min_w(px(240.));
                    let menu = match focus.clone() {
                        Some(focus) => menu.action_context(focus),
                        None => menu,
                    };
                    app_menu(name, wrap, recent.clone(), menu, window, cx)
                })
        });

        let layout_toggle =
            |id: &'static str, on: bool, on_icon: char, off_icon: char, text: &'static str| {
                action_button(id, if on { on_icon } else { off_icon })
                    .text_color(col::fg())
                    .tooltip(tip(text))
            };

        let window_button = |id: &'static str, ch: char, area: WindowControlArea, close: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .w(px(46.))
                .h_full()
                .text_color(col::fg())
                .window_control_area(area)
                .map(|d| {
                    if close {
                        d.hover(|s| s.bg(col::close_hover()).text_color(white()))
                    } else {
                        d.hover(|s| s.bg(col::hover()))
                    }
                })
                .child(codicon(ch))
        };

        let title = self
            .root_name()
            .unwrap_or_else(|| "Visual Studio Native".into());
        // VS Code's "Search {workspace} (Ctrl+P) — {window title}".
        let tooltip: SharedString =
            format!("Search {title} (Ctrl+P) — {}", self.window_title()).into();

        h_flex()
            .h(px(35.))
            .flex_none()
            .bg(col::bg())
            .border_b_1()
            .border_color(col::border())
            .text_color(col::fg_muted())
            // left: app icon + menubar
            .child(
                h_flex()
                    .flex_1()
                    .flex_basis(px(0.))
                    .h_full()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .w(px(35.))
                            .h_full()
                            .window_control_area(WindowControlArea::Drag)
                            .child(crate::theme::logo(16.)),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            // Before the menu's popover takes focus.
                            .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                                this.menu_focus = window.focused(cx);
                                cx.notify();
                            }))
                            .children(menubar),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .window_control_area(WindowControlArea::Drag),
                    ),
            )
            // center: navigation + command center
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(4.))
                    .child(
                        action_button("back", icon::ARROW_LEFT)
                            .text_color(match self.nav_at > 0 {
                                true => col::fg_muted(),
                                false => col::fg_dim(),
                            })
                            .tooltip(tip("Go Back (Alt+LeftArrow)"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.navigate(true, window, cx)),
                            ),
                    )
                    .child(
                        action_button("forward", icon::ARROW_RIGHT)
                            .text_color(match self.nav_at + 1 < self.nav.len() {
                                true => col::fg_muted(),
                                false => col::fg_dim(),
                            })
                            .tooltip(tip("Go Forward (Alt+RightArrow)"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.navigate(false, window, cx)),
                            ),
                    )
                    .child(
                        h_flex()
                            .id("command-center")
                            .w(px(window
                                .viewport_size()
                                .width
                                .as_f32()
                                .mul_add(0.38, 0.)
                                .clamp(200., 600.)))
                            .h(px(22.))
                            .justify_center()
                            .gap(px(6.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(col::command_center_border())
                            .bg(col::bg())
                            .hover(|s| s.bg(ca_white(0x0F)))
                            .cursor_pointer()
                            .text_size(px(12.))
                            .text_color(col::fg())
                            .child(codicon(icon::SEARCH).text_size(px(14.)))
                            .child(title.clone())
                            .tooltip(move |window, cx| {
                                Tooltip::new(tooltip.clone()).build(window, cx)
                            })
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.open_palette("", window, cx)
                                }),
                            ),
                    ),
            )
            // right: layout controls + window controls
            .child(
                h_flex()
                    .flex_1()
                    .flex_basis(px(0.))
                    .h_full()
                    .min_w_0()
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .window_control_area(WindowControlArea::Drag),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap(px(4.))
                            .pr(px(8.))
                            .child(
                                layout_toggle(
                                    "toggle-sidebar",
                                    self.sidebar,
                                    icon::LAYOUT_SIDEBAR_LEFT,
                                    icon::LAYOUT_SIDEBAR_LEFT_OFF,
                                    "Toggle Primary Side Bar (Ctrl+B)",
                                )
                                .on_click(cx.listener(
                                    |this, _, w, cx| this.toggle_sidebar(&ToggleSidebar, w, cx),
                                )),
                            )
                            .child(
                                layout_toggle(
                                    "toggle-panel",
                                    self.panel,
                                    icon::LAYOUT_PANEL,
                                    icon::LAYOUT_PANEL_OFF,
                                    "Toggle Panel (Ctrl+J)",
                                )
                                .on_click(cx.listener(
                                    |this, _, w, cx| this.toggle_panel(&TogglePanel, w, cx),
                                )),
                            )
                            .child(layout_toggle(
                                "toggle-aux",
                                false,
                                icon::LAYOUT_SIDEBAR_RIGHT_OFF,
                                icon::LAYOUT_SIDEBAR_RIGHT_OFF,
                                "Toggle Secondary Side Bar",
                            ))
                            .child(
                                action_button("customize-layout", icon::LAYOUT)
                                    .text_color(col::fg())
                                    .tooltip(tip("Customize Layout...")),
                            ),
                    )
                    .child(window_button(
                        "min",
                        icon::CHROME_MINIMIZE,
                        WindowControlArea::Min,
                        false,
                    ))
                    .child(window_button(
                        "max",
                        if window.is_maximized() {
                            icon::CHROME_RESTORE
                        } else {
                            icon::CHROME_MAXIMIZE
                        },
                        WindowControlArea::Max,
                        false,
                    ))
                    .child(window_button(
                        "close",
                        icon::CHROME_CLOSE,
                        WindowControlArea::Close,
                        true,
                    )),
            )
    }

    // ---- activity bar ------------------------------------------------------

    fn render_activitybar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // Modern UI: 36px items 8 apart, the active one on a 32px pill.
        let (m, size) = (self.modern, if self.modern { 36. } else { 48. });
        let item = |id: SharedString,
                    glyph: AnyElement,
                    tip: SharedString,
                    view: Option<View>,
                    cx: &mut Context<Self>| {
            let active = self.sidebar && view == Some(self.view);
            let (hover_id, down_id) = (id.clone(), id.clone());
            div()
                .id(id.clone())
                .group("act-item")
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .flex_none()
                .size(px(size))
                .cursor_pointer()
                .text_color(if active { col::fg() } else { col::fg_muted() })
                .hover(|s| s.text_color(col::fg()))
                .when(m, |d| {
                    d.pb(px(4.)).child(
                        div()
                            .absolute()
                            .left(px(2.))
                            .top_0()
                            .size(px(32.))
                            .rounded(px(4.))
                            .when(active, |d| d.bg(col::tab_active()))
                            .when(!active, |d| {
                                d.group_hover("act-item", |s| s.bg(col::hover()))
                            }),
                    )
                })
                .when(active && !m, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .bottom_0()
                            .w(px(2.))
                            .bg(col::fg()),
                    )
                })
                .child(glyph)
                .when(self.act_hover == Some(id), |d| {
                    d.child(
                        div().absolute().left_0().top_0().child(deferred(
                            anchored()
                                .position_mode(AnchoredPositionMode::Local)
                                .position(point(px(size), px(0.)))
                                .child(activity_hover(tip, size)),
                        )),
                    )
                })
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    this.hover_activity(hover_id.clone(), *hovered, cx)
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.hover_activity(down_id.clone(), false, cx)
                    }),
                )
                .when_some(view, |d, view| {
                    d.on_click(cx.listener(move |this, _, _, cx| this.show(view, cx)))
                })
        };
        let glyph = |ch| (codicon(ch).text_size(px(24.)).line_height(px(24.))).into_any_element();
        let builtin = [
            (
                "act-explorer",
                icon::FILES,
                "Explorer (Ctrl+Shift+E)",
                View::Explorer,
            ),
            (
                "act-search",
                icon::SEARCH,
                "Search (Ctrl+Shift+F)",
                View::Search,
            ),
            (
                "act-scm",
                icon::SOURCE_CONTROL,
                "Source Control (Ctrl+Shift+G)",
                View::Scm,
            ),
            (
                "act-run",
                icon::DEBUG_ALT,
                "Run and Debug (Ctrl+Shift+D)",
                View::Run,
            ),
            (
                "act-ext",
                icon::EXTENSIONS,
                "Extensions (Ctrl+Shift+X)",
                View::Extensions,
            ),
        ];
        let mut top: Vec<_> = (builtin.into_iter())
            .map(|(id, ch, text, view)| item(id.into(), glyph(ch), text.into(), Some(view), cx))
            .collect();
        // Extensions' view containers, after the built-in ones as in VS Code.
        // An image icon is drawn as a mask in the item's color, like a codicon.
        // ponytail: a PNG icon keeps its own colors.
        for (ix, c) in exthost::view_containers().into_iter().enumerate() {
            let active = self.sidebar && self.view == View::Ext(ix);
            let glyph = match c.icon {
                Ok(ch) => glyph(ch),
                Err(path)
                    if path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("svg")) =>
                {
                    svg()
                        .external_path(path.to_string_lossy().to_string())
                        .size(px(24.))
                        .text_color(if active { col::fg() } else { col::fg_muted() })
                        .group_hover("act-item", |s| s.text_color(col::fg()))
                        .into_any_element()
                }
                Err(path) => img(path).size(px(24.)).into_any_element(),
            };
            let id = SharedString::from(format!("act-c-{}", c.id));
            top.push(item(id, glyph, c.title.into(), Some(View::Ext(ix)), cx));
        }

        let (gap, inset) = if m { (8., 3.) } else { (0., 0.) };
        let bar = v_flex()
            .w(px(if m { 42. } else { 48. }))
            .flex_none()
            .h_full()
            .items_center()
            .justify_between()
            .bg(col::bg())
            .when(!m, |d| d.border_r_1().border_color(col::border()))
            .child(v_flex().pt(px(inset)).gap(px(gap)).children(top))
            .child(
                v_flex()
                    .pb(px(inset))
                    .gap(px(gap))
                    .child(item(
                        "act-account".into(),
                        glyph(icon::ACCOUNT),
                        "Accounts".into(),
                        None,
                        cx,
                    ))
                    .child(item(
                        "act-manage".into(),
                        glyph(icon::SETTINGS_GEAR),
                        "Manage".into(),
                        None,
                        cx,
                    )),
            );
        if !m {
            return bar.into_any_element();
        }
        // The rail joins the side bar, or closes up without one.
        let r = if self.sidebar {
            [8., 0., 0., 8.]
        } else {
            [8.; 4]
        };
        div()
            .flex_none()
            .w(px(48.))
            .h_full()
            .pl(px(4.))
            .pb(px(4.))
            .child(card(bar, col::bg(), r, false))
            .into_any_element()
    }

    // ---- side bar ----------------------------------------------------------

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        const MORE: (char, &str) = (icon::ELLIPSIS, "Views and More Actions...");
        let containers = exthost::view_containers();
        // A disabled extension's container is gone.
        if let View::Ext(ix) = self.view
            && ix >= containers.len()
        {
            self.view = View::Explorer;
        }
        let (title, actions): (&str, &[(char, &str)]) = match self.view {
            View::Ext(ix) => (&containers[ix].title, &[]),
            View::Explorer => ("Explorer", &[MORE]),
            View::Search => (
                "Search",
                &[
                    (icon::REFRESH, "Refresh"),
                    (icon::CLEAR_ALL, "Clear Search Results"),
                    (icon::COLLAPSE_ALL, "Collapse All"),
                    MORE,
                ],
            ),
            View::Scm => ("Source Control", &[MORE]),
            View::Run => ("Run and Debug", &[MORE]),
            View::Extensions => (
                "Extensions",
                &[
                    (icon::FILTER, "Filter Extensions..."),
                    (icon::REFRESH, "Refresh"),
                    (icon::CLEAR_ALL, "Clear Extensions Search Results"),
                    MORE,
                ],
            ),
        };
        // Extensions' `view/title` buttons, for VS Code's view ids.
        let view_id = match self.view {
            View::Explorer => "workbench.explorer.fileView",
            View::Search => "workbench.view.search",
            View::Scm => "workbench.scm",
            _ => "",
        };
        let mut keys = KeyContext::default();
        keys.set("view", view_id);
        let items = match view_id.is_empty() {
            true => Vec::new(),
            false => exthost::menu_items("view/title", &[keys]),
        };
        let items = items.into_iter().filter(|i| i.navigation).collect();
        let mut view_buttons = self.ext_buttons("ext-view", items, Vec::new(), cx);
        if self.view == View::Scm {
            let (mut items, args) = self.scm_title_items();
            items.retain(|i| i.navigation);
            view_buttons.extend(self.ext_buttons("ext-scm", items, args, cx));
        }
        // Modern UI: a 32px title, 12px semibold, capitalized rather than upper case.
        let header = h_flex()
            .h(px(if self.modern { 32. } else { 35. }))
            .flex_none()
            .pl(px(if self.modern { 12. } else { 20. }))
            .pr(px(if self.modern { 4. } else { 8. }))
            .justify_between()
            .text_size(px(if self.modern { 12. } else { 11. }))
            .text_color(col::fg())
            .child(match self.modern {
                true => div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(capitalize(title)),
                false => div().child(title.to_uppercase()),
            })
            .when(self.view == View::Run, |d| {
                d.children(self.render_run_title(cx))
            })
            .child(h_flex().gap(px(4.)).children(view_buttons).children(
                actions.iter().enumerate().map(|(i, &(ch, text))| {
                    match (self.view, text == MORE.1) {
                        (View::Extensions, true) => extensions::more_button().into_any_element(),
                        (View::Scm, true) => self.scm_more(cx),
                        _ => action_button(("sb-act", i), ch)
                            .tooltip(tip(text))
                            .into_any_element(),
                    }
                }),
            ));

        // Extensions' views in a built-in container go below its own.
        let below = match self.view {
            View::Explorer => self.render_ext_views("explorer", false, cx),
            View::Scm => self.render_ext_views("scm", false, cx),
            View::Run => self.render_ext_views("debug", false, cx),
            _ => Vec::new(),
        };
        let body = match self.view {
            View::Ext(ix) => v_flex()
                .size_full()
                .children(self.render_ext_views(&containers[ix].id, true, cx))
                .into_any_element(),
            View::Explorer => self.render_explorer(cx).into_any_element(),
            View::Search => self.render_search(cx).into_any_element(),
            View::Scm => self.render_scm(cx),
            View::Run => self.render_run(cx),
            View::Extensions => v_flex()
                .size_full()
                .child(
                    div()
                        .px(px(12.))
                        .pb(px(8.))
                        .child(search_box(&self.ext_search)),
                )
                // A query searches Open VSX; the Installed list shows without one or for `@installed`.
                .map(|d| {
                    let list = div().id("ext-list").flex_1().min_h_0().overflow_y_scroll();
                    match !extensions::searches_market(&self.ext_search.read(cx).value()) {
                        true => d
                            .child(section_header("ext-installed", "Installed", true))
                            .child(list.child(self.render_installed(cx))),
                        false => d.child(list.child(self.render_market(cx))),
                    }
                })
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .bg(col::bg())
            .text_color(col::fg())
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .children(below)
            .into_any_element()
    }

    fn render_explorer(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut rows = Vec::new();
        if let (Some(root), true) = (self.root.clone(), self.root_open) {
            // Without a watcher nothing evicts stale listings, so re-read them each frame.
            if self.watcher.is_none() {
                self.dirs.clear();
            }
            self.tree_rows(&root, 0, &mut rows);
        }
        let (wb, root, clip) = (
            cx.weak_entity(),
            self.root.clone().unwrap_or_default(),
            self.clip.is_some(),
        );
        let button = |id: &'static str, ch: char, text: &'static str| {
            (action_button(id, ch).font_weight(FontWeight::NORMAL)).tooltip(tip(text))
        };
        // ponytail: New File/Folder are inert until the explorer has inline
        // rename for the new row.
        let root_actions = h_flex()
            .gap(px(2.))
            .invisible()
            .group_hover("explorer-root-hdr", |s| s.visible())
            .child(button("explorer-new-file", icon::NEW_FILE, "New File..."))
            .child(button(
                "explorer-new-folder",
                icon::NEW_FOLDER,
                "New Folder...",
            ))
            .child(
                button("explorer-refresh", icon::REFRESH, "Refresh Explorer").on_click(
                    cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.dirs.clear();
                        cx.notify()
                    }),
                ),
            )
            .child(
                button(
                    "explorer-collapse",
                    icon::COLLAPSE_ALL,
                    "Collapse Folders in Explorer",
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.expanded.clear();
                    cx.notify()
                })),
            );

        v_flex()
            .id("explorer-view")
            .size_full()
            .on_hover(self.hover_slider("tree", cx))
            .map(|d| match self.root_name() {
                Some(name) => d
                    .child(
                        section_header("explorer-root", &name, self.root_open)
                            .child(root_actions)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.root_open = !this.root_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        // A right-click past the rows gets the workspace folder's menu.
                        div()
                            .id("explorer-empty")
                            .key_context("Explorer filesExplorerFocus explorerViewletFocus")
                            // A right-click drops the focus outline; one on a
                            // row puts it back there after this.
                            .capture_any_mouse_down(cx.listener(
                                |this, e: &MouseDownEvent, window, cx| {
                                    if e.button == MouseButton::Right {
                                        this.tree_cursor = None;
                                        window.focus(&this.tree_focus, cx);
                                        cx.notify();
                                    }
                                },
                            ))
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .child(
                                uniform_list(
                                    "tree",
                                    rows.len(),
                                    cx.processor(move |this, range: Range<usize>, window, cx| {
                                        // Holds while the row's menu has focus too.
                                        let focused = this.tree_focus.contains_focused(window, cx);
                                        range
                                            .map(|ix| this.render_tree_row(&rows[ix], focused, cx))
                                            .collect::<Vec<_>>()
                                    }),
                                )
                                .track_scroll(&self.tree_scroll)
                                .track_focus(&self.tree_focus)
                                .on_key_down(cx.listener(Self::tree_key))
                                .on_action(
                                    cx.listener(|this, _: &CutFile, _, cx| {
                                        this.clip_file(true, cx)
                                    }),
                                )
                                .on_action(cx.listener(|this, _: &CopyFile, _, cx| {
                                    this.clip_file(false, cx)
                                }))
                                .on_action(cx.listener(|this, _: &PasteFile, window, cx| {
                                    this.paste_file(this.tree_cursor.clone(), window, cx)
                                }))
                                .on_action(cx.listener(|this, _: &FindInFolder, window, cx| {
                                    let at = this.tree_cursor.clone().or(this.root.clone());
                                    if let Some(dir) = at.filter(|p| p.is_dir()) {
                                        this.find_in_folder(&dir, window, cx)
                                    }
                                }))
                                // Left alone, a drag over the tree drops into the
                                // root; the row under the mouse moves it after this.
                                .on_drag_move(cx.listener(
                                    |this, e: &DragMoveEvent<DraggedFile>, _, cx| {
                                        let from = e.drag(cx).0.clone();
                                        let inside = e.bounds.contains(&e.event.position);
                                        let copy = e.event.modifiers.control;
                                        this.drag_target(
                                            this.root.clone().filter(|_| inside),
                                            Some(&from),
                                            copy,
                                            cx,
                                        );
                                    },
                                ))
                                .on_drag_move(cx.listener(
                                    |this, e: &DragMoveEvent<ExternalPaths>, _, cx| {
                                        let inside = e.bounds.contains(&e.event.position);
                                        this.drag_target(
                                            this.root.clone().filter(|_| inside),
                                            None,
                                            true,
                                            cx,
                                        );
                                    },
                                ))
                                .when(
                                    self.drop_dir.is_some()
                                        && self.drop_dir == self.root
                                        && cx.has_active_drag(),
                                    |d| d.bg(col::drop()),
                                )
                                .on_drop(cx.listener(|this, file: &DraggedFile, window, cx| {
                                    if let Some(root) = this.root.clone() {
                                        this.drop_file(file.0.clone(), root, window, cx)
                                    }
                                }))
                                .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                                    if let Some(root) = this.root.clone() {
                                        this.import_paths(paths.paths(), root, window, cx)
                                    }
                                }))
                                .group("explorer-tree")
                                .size_full(),
                            )
                            .context_menu(move |menu, _, _| {
                                explorer_menu(menu, &wb, &root, true, true, clip)
                            }),
                    )
                    .child(self.slider("tree", &self.tree_scroll, Axis::Vertical, 10.)),
                None => d
                    .child(section_header("no-folder", "No Folder Opened", true))
                    .child(
                        v_flex()
                            .flex_1()
                            .px(px(20.))
                            .pt(px(6.))
                            .gap(px(14.))
                            .text_size(px(13.))
                            .child("You have not yet opened a folder.")
                            .child(primary_button("open-folder", "Open Folder", None).on_click(
                                cx.listener(|this, _, w, cx| this.open_folder(&OpenFolder, w, cx)),
                            )),
                    ),
            })
            .child(
                div()
                    .border_t_1()
                    .border_color(col::border())
                    .child(section_header("outline", "Outline", false)),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(col::border())
                    .child(section_header("timeline", "Timeline", false)),
            )
    }

    /// (Re)starts watching the workspace so the explorer follows changes on disk.
    fn watch_root(&mut self, cx: &mut Context<Self>) {
        self.dirs.clear();
        self.watcher = None;
        self.palette.files_changed(true);
        self.launch_changed();
        let Some(root) = self.root.clone() else {
            return;
        };
        self.palette.remember_opened(true, &root, cx);
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Vec<PathBuf>>();
        let watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            // Only changes to a folder's entries matter, and launch.json's contents.
            if let Ok(ev) = ev
                && (matches!(
                    ev.kind,
                    EventKind::Create(_)
                        | EventKind::Remove(_)
                        | EventKind::Modify(ModifyKind::Name(_))
                ) || ev.paths.iter().any(|p| p.ends_with(".vscode/launch.json")))
            {
                tx.unbounded_send(ev.paths).ok();
            }
        });
        let Ok(mut watcher) = watcher else { return };
        if let Err(err) = watcher.watch(&root, RecursiveMode::Recursive) {
            eprintln!("watch {}: {err}", root.display());
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            while let Some(mut paths) = rx.next().await {
                // A build touches many files at once; take the whole burst in one go.
                while let Ok(more) = rx.try_recv() {
                    paths.extend(more);
                }
                let alive = this.update(cx, |this, cx| {
                    // Quick Open's walk skips .git, which changes all the time.
                    if paths
                        .iter()
                        .any(|p| !p.components().any(|c| c.as_os_str() == ".git"))
                    {
                        this.palette.files_changed(false);
                    }
                    let mut stale = false;
                    if paths.iter().any(|p| p.ends_with(".vscode/launch.json")) {
                        this.launch_changed();
                        stale = true;
                    }
                    for path in &paths {
                        stale |= this.dirs.remove(path).is_some();
                        if let Some(parent) = path.parent() {
                            stale |= this.dirs.remove(parent).is_some();
                        }
                    }
                    if stale {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        self.watcher = Some((watcher, task));
    }

    /// Opens `path`'s folders in the explorer and scrolls its row into view;
    /// `show` also brings up the explorer (Reveal in Explorer View), else it
    /// follows the active editor as VS Code's `explorer.autoReveal` does:
    /// `false` turns that off, `"focusNoScroll"` doesn't scroll, and files
    /// matching `explorer.autoRevealExclude` aren't followed.
    fn reveal_in_tree(&mut self, path: &Path, show: bool, cx: &mut Context<Self>) {
        let mut scroll = true;
        if !show {
            let settings = terminal::settings();
            let auto = &settings["explorer.autoReveal"];
            if *auto == false {
                return;
            }
            scroll = *auto != "focusNoScroll";
            let globs = search::glob_map(
                &settings,
                &["explorer.autoRevealExclude"],
                &["**/node_modules", "**/bower_components"],
            );
            let root = self.root.as_deref().unwrap_or(path);
            let excluded = search::overrides(root, "", "", &globs);
            if excluded.is_ok_and(|o| o.matched(path, false).is_ignore()) {
                return;
            }
        }
        self.tree_sel = Some(path.to_path_buf());
        self.tree_cursor = self.tree_sel.clone();
        let Some(root) = self.root.clone().filter(|r| path.starts_with(r)) else {
            return;
        };
        let parents = path.ancestors().skip(1).take_while(|a| *a != root);
        self.expanded.extend(parents.map(Path::to_path_buf));
        if show {
            (self.root_open, self.sidebar, self.view) = (true, true, View::Explorer);
        }
        let mut rows = Vec::new();
        self.tree_rows(&root, 0, &mut rows);
        if let Some(ix) = rows.iter().position(|(p, ..)| p == path).filter(|_| scroll) {
            self.tree_scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// A folder toggles open or shut, a file opens in the editor.
    fn tree_open(
        &mut self,
        path: &Path,
        is_dir: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if is_dir {
            self.tree_sel = Some(path.to_path_buf());
            self.tree_cursor = self.tree_sel.clone();
            if !self.expanded.remove(path) {
                self.expanded.insert(path.to_path_buf());
            }
            cx.notify();
        } else {
            self.open_path(path.to_path_buf(), window, cx);
        }
    }

    /// The explorer's keys: Up/Down/PageUp/PageDown/Home/End move the
    /// selection, Right opens a folder or steps into it, Left shuts it or
    /// steps out, Enter opens into the editor and Space opens keeping focus
    /// here.
    fn tree_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if ev.keystroke.modifiers.modified() {
            return;
        }
        let mut rows = Vec::new();
        self.tree_rows(&root, 0, &mut rows);
        let Some(last) = rows.len().checked_sub(1) else {
            return;
        };
        let cur = self
            .tree_cursor
            .as_ref()
            .and_then(|s| rows.iter().position(|(p, ..)| p == s));
        let row = cur.map(|i| rows[i].clone());
        let open = |p: &PathBuf| self.expanded.contains(p);
        // PageUp/Down first go to the top/bottom row in view, then a page on.
        let scroll = self.tree_scroll.0.borrow().base_handle.clone();
        let page = ((scroll.bounds().size.height / px(22.)) as usize)
            .saturating_sub(1)
            .max(1);
        let top = (-scroll.offset().y / px(22.)).ceil() as usize;
        let to = match (ev.keystroke.key.as_str(), cur, &row) {
            ("down" | "up" | "pagedown" | "pageup", None, _) | ("home", ..) => 0,
            ("end", ..) => last,
            ("pagedown", Some(i), _) if i < top + page => (top + page).min(last),
            ("pagedown", Some(i), _) => (i + page).min(last),
            ("pageup", Some(i), _) if i > top => top,
            ("pageup", Some(i), _) => i.saturating_sub(page),
            ("down", Some(i), _) => (i + 1).min(last),
            ("up", Some(i), _) => i.saturating_sub(1),
            ("right", Some(i), Some((p, _, true))) if open(p) => (i + 1).min(last),
            ("left", Some(i), Some((p, _, dir))) if !(*dir && open(p)) => rows[..i]
                .iter()
                .rposition(|(q, ..)| p.parent() == Some(q))
                .unwrap_or(i),
            ("right" | "left", _, Some((p, _, true))) => {
                self.tree_open(&p.clone(), true, window, cx);
                return cx.stop_propagation();
            }
            ("enter" | "space", _, Some((p, _, dir))) => {
                self.tree_open(&p.clone(), *dir, window, cx);
                if ev.keystroke.key == "space" {
                    window.focus(&self.tree_focus, cx);
                }
                return cx.stop_propagation();
            }
            _ => return,
        };
        cx.stop_propagation();
        self.tree_sel = Some(rows[to].0.clone());
        self.tree_cursor = self.tree_sel.clone();
        self.tree_scroll.scroll_to_item(to, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Copy or Cut on the explorer's focused row.
    fn clip_file(&mut self, cut: bool, cx: &mut Context<Self>) {
        if let Some(path) = self.tree_cursor.clone() {
            self.clip = Some((path, cut));
            cx.notify();
        }
    }

    /// Visible explorer rows (path, depth, is_dir), reading folders only on a cache miss.
    fn tree_rows(&mut self, dir: &Path, depth: usize, rows: &mut Vec<(PathBuf, usize, bool)>) {
        let entries = self
            .dirs
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                // ponytail: settings.json is read per folder read, and an edit to
                // `files.exclude` shows on the next refresh, not live.
                let root = self.root.as_deref().unwrap_or(dir);
                let excluded = search::overrides(root, "", "", &search::exclude_globs(false))
                    .unwrap_or_else(|_| Override::empty());
                let siblings = search::sibling_rules(&terminal::settings(), root, false);
                let mut entries: Vec<_> = fs::read_dir(dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| (e.path(), e.file_type().is_ok_and(|t| t.is_dir())))
                    .filter(|(p, is_dir)| {
                        !excluded.matched(p, *is_dir).is_ignore()
                            && !search::sibling_hidden(&siblings, p, *is_dir)
                    })
                    .collect();
                // VS Code's default explorer sort: folders first, then case-insensitive name.
                entries.sort_by_cached_key(|(p, is_dir)| {
                    (
                        !is_dir,
                        p.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_lowercase(),
                    )
                });
                entries
            })
            .clone();
        for (path, is_dir) in entries {
            let open = is_dir && self.expanded.contains(&path);
            rows.push((path.clone(), depth, is_dir));
            if open {
                self.tree_rows(&path, depth + 1, rows);
            }
        }
    }

    /// VS Code's scrollbar slider, `size` px thick: shown while the mouse is
    /// over the `area` (see `hover_slider`) and briefly after a scroll.
    fn slider(
        &self,
        area: &'static str,
        handle: &(impl ScrollbarHandle + Clone),
        axis: Axis,
        size: f32,
    ) -> Scrollbar {
        let thumb = move |bg| {
            move |t: ScrollbarThumbStyle| t.width(px(size)).inset(px(0.)).radius(px(0.)).bg(ca(bg))
        };
        Scrollbar::new(handle)
            .id(area)
            .axis(axis)
            .mode(match self.scroll_hover == Some(area) {
                true => ScrollbarMode::Always,
                false => ScrollbarMode::Scrolling,
            })
            .styles(|s| {
                s.track(|t| t.width(px(size)).bg(transparent_black()))
                    .thumb(thumb(0x79797966))
                    .thumb_hover(thumb(0x646464B3))
                    .thumb_active(thumb(0xBFBFBF66))
            })
    }

    /// Tracks the mouse over a `slider`'s area.
    fn hover_slider(
        &self,
        area: &'static str,
        cx: &mut Context<Self>,
    ) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        cx.listener(move |this, hover: &bool, _, cx| {
            if *hover {
                this.scroll_hover = Some(area);
            } else if this.scroll_hover == Some(area) {
                this.scroll_hover = None;
            }
            cx.notify();
        })
    }

    fn render_tree_row(
        &self,
        (path, depth, is_dir): &(PathBuf, usize, bool),
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (depth, is_dir) = (*depth, *is_dir);
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let open = is_dir && self.expanded.contains(path);
        let active = self.tree_sel.as_ref() == Some(path);
        // The selection's guide is lit and always shown: an open folder's
        // own, else its parent's.
        let lit = self
            .tree_sel
            .as_deref()
            .and_then(|sel| self.expanded.contains(sel).then_some(sel).or(sel.parent()));
        let (click_path, cursor_path) = (path.clone(), path.clone());
        let (wb, menu_path, clip) = (cx.weak_entity(), path.clone(), self.clip.is_some());
        h_flex()
            .id(ElementId::Name(path.to_string_lossy().to_string().into()))
            .on_drag(DraggedFile(path.clone()), |file, _, _, cx| {
                cx.new(|_| file.clone())
            })
            // Explorer items are moved into this folder, or this file's, and
            // files from Windows copied there.
            .map(|d| {
                let dir = match is_dir {
                    true => path.clone(),
                    false => path.parent().unwrap_or(path).to_path_buf(),
                };
                let (into, over, over_ext) = (dir.clone(), dir.clone(), dir.clone());
                d.on_drag_move(
                    cx.listener(move |this, e: &DragMoveEvent<DraggedFile>, _, cx| {
                        if e.bounds.contains(&e.event.position) {
                            let from = e.drag(cx).0.clone();
                            let copy = e.event.modifiers.control;
                            this.drag_target(Some(over.clone()), Some(&from), copy, cx);
                        }
                    }),
                )
                .on_drag_move(
                    cx.listener(move |this, e: &DragMoveEvent<ExternalPaths>, _, cx| {
                        if e.bounds.contains(&e.event.position) {
                            this.drag_target(Some(over_ext.clone()), None, true, cx);
                        }
                    }),
                )
                .on_drop(cx.listener(move |this, file: &DraggedFile, window, cx| {
                    cx.stop_propagation();
                    this.drop_file(file.0.clone(), into.clone(), window, cx)
                }))
                .on_drop(cx.listener(
                    move |this, paths: &ExternalPaths, window, cx| {
                        cx.stop_propagation();
                        this.import_paths(paths.paths(), dir.clone(), window, cx)
                    },
                ))
            })
            .relative()
            .h(px(22.))
            .w_full()
            .pl(px(8. + depth as f32 * 8.))
            .pr(px(12.))
            .text_size(px(13.))
            .whitespace_nowrap()
            .cursor_pointer()
            .border_1()
            .border_color(transparent_black())
            .when(active, |d| d.bg(col::selected()))
            .when(
                self.drop_dir.as_ref().is_some_and(|d| {
                    cx.has_active_drag() && self.root.as_ref() != Some(d) && path.starts_with(d)
                }),
                |d| d.bg(col::drop()),
            )
            .when(focused && self.tree_cursor.as_ref() == Some(path), |d| {
                d.border_color(col::focus())
            })
            .when(!active, |d| d.hover(|s| s.bg(col::hover())))
            .when(self.clip.as_ref() == Some(&(path.clone(), true)), |d| {
                d.opacity(0.5)
            })
            // Indent guides under each ancestor folder's chevron, shown while
            // the mouse is over the tree (`workbench.tree.renderIndentGuides`).
            .children(
                path.ancestors()
                    .skip(1)
                    .take(depth)
                    .enumerate()
                    .map(|(i, owner)| {
                        let guide = div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(15. + (depth - 1 - i) as f32 * 8.))
                            .w(px(1.));
                        if lit == Some(owner) {
                            guide.bg(col::indent_guide())
                        } else {
                            guide
                                .bg(col::inactive_indent_guide())
                                .invisible()
                                .group_hover("explorer-tree", |s| s.visible())
                        }
                    }),
            )
            .child(div().w(px(16.)).flex_none().when(is_dir, |d| {
                d.child(codicon(if open {
                    icon::CHEVRON_DOWN
                } else {
                    icon::CHEVRON_RIGHT
                }))
            }))
            .map(|d| match is_dir {
                true => d.children(folder_icon(path, open).map(|i| div().mr(px(6.)).child(i))),
                false => d.child(div().mr(px(6.)).child(file_icon(path))),
            })
            .child(div().overflow_hidden().text_ellipsis().child(name))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.tree_open(&click_path, is_dir, window, cx);
                // A click opens the file but leaves focus in the explorer.
                window.focus(&this.tree_focus, cx);
            }))
            // A right-click outlines the row without selecting it.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    this.tree_cursor = Some(cursor_path.clone());
                    window.focus(&this.tree_focus, cx);
                    cx.notify();
                }),
            )
            .context_menu(move |menu, _, _| {
                explorer_menu(menu, &wb, &menu_path, is_dir, false, clip)
            })
            .into_any_element()
    }

    // ---- editor group ------------------------------------------------------

    /// Extensions' `editor/title` items for the active editor and its path.
    fn editor_title_items(&self) -> (Vec<exthost::MenuItem>, Option<PathBuf>) {
        let Some(doc) = self.docs.get(self.active) else {
            return (Vec::new(), None);
        };
        let keys = [exthost::editor_keys(&doc.path, doc.lang)];
        let items = exthost::menu_items("editor/title", &keys);
        (items, Some(doc.path.clone()))
    }

    /// Extensions' menu items as buttons, run with `args`.
    fn ext_buttons(
        &self,
        id: &'static str,
        items: Vec<exthost::MenuItem>,
        args: Vec<serde_json::Value>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        (items.into_iter().enumerate())
            .map(|(ix, item)| {
                let title = SharedString::from(item.title);
                let (command, args) = (item.id, args.clone());
                let button = match item.icon {
                    Some(ch) => action_button((id, ix), ch),
                    None => div()
                        .id((id, ix))
                        .px(px(4.))
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(|s| s.bg(col::toolbar_hover()))
                        .child(title.clone()),
                };
                button
                    .tooltip(move |window, cx| {
                        component::tooltip::Tooltip::new(title.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.run_extension_command(command.clone(), args.clone(), cx)
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn render_editor_group(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.docs.is_empty() {
            return watermark().into_any_element();
        }
        let wb = cx.weak_entity();
        let (mut more, more_path) = self.editor_title_items();
        more.retain(|i| !i.navigation);
        let wb_more = wb.clone();
        let tabs_hover = self.hover_slider("tabs", cx);
        let open: Rc<[PathBuf]> = self.docs.iter().map(|d| d.path.clone()).collect();
        let tabs = self.docs.iter().enumerate().map(|(ix, doc)| {
            // A tab dragged from the left lands after this one, anything
            // else before it; the border shows which side.
            let lands_after = {
                let open = open.clone();
                move |path: &Path| open.iter().position(|p| p == path).is_some_and(|f| f < ix)
            };
            let active = ix == self.active;
            let name = doc
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let group = SharedString::from(format!("tab-{ix}"));
            let full = SharedString::from(doc.path.display().to_string());
            h_flex()
                .id(("tab", ix))
                .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                .group(group.clone())
                .relative()
                .flex_none()
                .h_full()
                .pl(px(10.))
                .pr(px(4.))
                .gap(px(6.))
                .min_w(px(80.))
                .cursor_pointer()
                .border_r_1()
                .border_color(col::border())
                .bg(if active { col::editor() } else { col::bg() })
                .text_color(if active { col::fg() } else { col::fg_muted() })
                .when(active, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(px(1.))
                            .bg(col::accent()),
                    )
                })
                .child(match (doc.settings.is_some(), doc.preview.is_some()) {
                    (true, _) => codicon(icon::SETTINGS).into_any_element(),
                    (_, true) => codicon(icon::OPEN_PREVIEW).into_any_element(),
                    _ => file_icon(&doc.path).into_any_element(),
                })
                .child(div().flex_1().whitespace_nowrap().child(name.clone()))
                .child(
                    div()
                        .id(("tab-close", ix))
                        .tooltip(tip("Close (Ctrl+F4)"))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(20.))
                        .rounded(px(4.))
                        .hover(|s| s.bg(col::toolbar_hover()))
                        .map(|d| match (doc.dirty, active) {
                            // The dot turns into the X while hovered.
                            (true, _) => {
                                let hover = SharedString::from(format!("tab-close-{ix}"));
                                let x = if active { col::fg() } else { col::fg_muted() };
                                d.relative()
                                    .group(hover.clone())
                                    .child(
                                        codicon(icon::CIRCLE_FILLED)
                                            .text_size(px(10.))
                                            .group_hover(hover.clone(), |s| s.invisible()),
                                    )
                                    .child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .invisible()
                                            .group_hover(hover, move |s| s.visible().text_color(x))
                                            .child(codicon(icon::CLOSE)),
                                    )
                            }
                            (false, true) => d.child(codicon(icon::CLOSE)),
                            (false, false) => d.child(
                                codicon(icon::CLOSE)
                                    .text_color(transparent_black())
                                    .group_hover(group.clone(), |s| s.text_color(col::fg_muted())),
                            ),
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close(ix, window, cx);
                        })),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.activate(ix, window, cx)))
                .on_aux_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                    if ev.is_middle_click() {
                        this.close(ix, window, cx);
                    }
                }))
                .on_drag(DraggedEditor(ix, name.clone().into()), |tab, _, _, cx| {
                    cx.new(|_| tab.clone())
                })
                .drag_over::<DraggedEditor>(move |s, tab, _, _| match tab.0 {
                    from if from == ix => s,
                    from => drop_side(s, from < ix),
                })
                .on_drop(cx.listener(move |this, tab: &DraggedEditor, window, cx| {
                    this.move_doc(tab.0, ix, window, cx)
                }))
                .drag_over::<DraggedFile>({
                    let after = lands_after.clone();
                    move |s, file, _, _| drop_side(s, after(&file.0))
                })
                .on_drop(cx.listener(move |this, file: &DraggedFile, window, cx| {
                    cx.stop_propagation();
                    if !file.0.is_dir() {
                        this.drop_paths(slice::from_ref(&file.0), Some(ix), window, cx);
                    }
                }))
                .drag_over::<ExternalPaths>(move |s, paths, _, _| {
                    drop_side(s, paths.paths().first().is_some_and(|p| lands_after(p)))
                })
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                    this.drop_paths(paths.paths(), Some(ix), window, cx);
                    cx.stop_propagation();
                }))
                .context_menu({
                    let (wb, editor) = (wb.clone(), doc.editor.clone());
                    let last = ix + 1 == self.docs.len();
                    let (path, lang) = (doc.path.clone(), doc.lang);
                    move |menu, _, _| editor_tab_menu(menu, &wb, &editor, last, &path, lang)
                })
        });

        let doc = &self.docs[self.active];
        let mut crumbs: Vec<AnyElement> = Vec::new();
        let rel = self
            .root
            .as_ref()
            .and_then(|r| doc.path.strip_prefix(r).ok())
            .unwrap_or(&doc.path);
        // "C:" then the folders, without the root's own "\".
        let parts: Vec<_> = (rel.components())
            .filter(|c| !matches!(c, std::path::Component::RootDir))
            .map(|p| p.as_os_str().to_string_lossy().to_string())
            .collect();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                crumbs.push(codicon(icon::CHEVRON_RIGHT).into_any_element());
            }
            if i + 1 == parts.len() {
                crumbs.push(file_icon(&doc.path));
            }
            crumbs.push(div().px(px(2.)).child(part.clone()).into_any_element());
        }
        let at = (doc.path.clone(), self.crumbs.bounds().size.width);
        if self.crumbs_at != at {
            self.crumbs_at = at;
            self.crumbs.scroll_to_item(crumbs.len().saturating_sub(1));
            self.tabs_scroll.scroll_to_item(self.active);
        }

        v_flex()
            .size_full()
            .bg(col::editor())
            .child(
                h_flex()
                    .id("tab-bar")
                    .h(px(35.))
                    .flex_none()
                    .bg(col::bg())
                    .text_size(px(13.))
                    .on_hover(tabs_hover)
                    // Absolute, so many tabs scroll instead of widening the
                    // editor; the tabs paint over the strip's bottom border.
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .h_full()
                            .child(div().absolute().bottom_0().w_full().h(px(1.)).bg(col::border()))
                            .child(
                                div()
                                    .id("tabs")
                                    .absolute()
                                    .size_full()
                                    .flex()
                                    .overflow_x_scroll()
                                    .track_scroll(&self.tabs_scroll)
                                    .children(tabs)
                                    // The space after the tabs: a double click
                                    // opens a new untitled file.
                                    .child(
                                        div()
                                            .id("tabs-empty")
                                            .flex_1()
                                            .h_full()
                                            .drag_over::<DraggedEditor>(|s, _, _, _| s.bg(col::drop()))
                                            .on_drop(cx.listener(|this, tab: &DraggedEditor, window, cx| {
                                                let last = this.docs.len() - 1;
                                                this.move_doc(tab.0, last, window, cx)
                                            }))
                                            .on_click(
                                            |ev, window, cx| {
                                                if ev.click_count() == 2 {
                                                    window.dispatch_action(Box::new(NewTextFile), cx);
                                                }
                                            },
                                        ),
                                    ),
                            ),
                    )
                    .child(self.slider("tabs", &self.tabs_scroll, Axis::Horizontal, 3.))
                    .child(
                        h_flex()
                            .h_full()
                            .px(px(8.))
                            .gap(px(4.))
                            .border_b_1()
                            .border_color(col::border())
                            .text_color(col::fg())
                            .children({
                                let (items, path) = self.editor_title_items();
                                let items = items.into_iter().filter(|i| i.navigation).collect();
                                let args = exthost::uri_args(path.as_deref());
                                self.ext_buttons("ext-title", items, args, cx)
                            })
                            .when(doc.lang == "markdown" && doc.preview.is_none() && !doc.binary, |d| {
                                d.child(
                                    action_button("md-preview-side", icon::OPEN_PREVIEW)
                                        .tooltip(tip("Open Preview to the Side (Ctrl+K V)"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.markdown_preview(None, true, window, cx)
                                        })),
                                )
                            })
                            .when(doc.preview.is_some(), |d| {
                                d.child(
                                    action_button("md-source", icon::FILE_CODE)
                                        .tooltip(tip("Open Source File"))
                                        .on_click(|_, window, cx| {
                                            window.dispatch_action(Box::new(MarkdownShowSource), cx)
                                        }),
                                )
                            })
                            .when(doc.settings.is_some(), |d| {
                                d.child(
                                    action_button("settings-json", icon::GO_TO_FILE)
                                        .tooltip(tip("Open Settings (JSON)"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_settings(window, cx)
                                        })),
                                )
                            })
                            .child(
                                action_button("split-editor", icon::SPLIT_HORIZONTAL)
                                    .tooltip(tip("Split Editor Right")),
                            )
                            .child(
                                Button::new("editor-more")
                                    .ghost()
                                    .compact()
                                    .size(px(22.))
                                    .text_color(col::fg())
                                    .child(codicon(icon::ELLIPSIS))
                                    .tooltip("More Actions...")
                                    .dropdown_menu(move |menu, _, _| {
                                        let menu = chord(menu, "Close All", CloseAllEditors);
                                        let menu = chord(menu, "Close Saved", CloseSavedEditors);
                                        ext_menu_items(menu, &wb_more, &more, exthost::uri_args(more_path.as_deref()))
                                    }),
                            ),
                    ),
            )
            // Absolute, so a long path scrolls instead of widening the editor.
            .child(
                div()
                    .id("breadcrumbs-row")
                    .when(doc.page.is_some() || doc.settings.is_some() || doc.preview.is_some(), |d| d.hidden())
                    .relative()
                    .h(px(22.))
                    .flex_none()
                    .on_hover(self.hover_slider("crumbs", cx))
                    .child(
                    h_flex()
                        .id("breadcrumbs")
                        .absolute()
                        .size_full()
                        .pl(px(12.))
                        .text_size(px(13.))
                        .text_color(col::fg_muted())
                        .overflow_x_scroll()
                        .track_scroll(&self.crumbs)
                        .children(crumbs),
                    )
                    .child(self.slider("crumbs", &self.crumbs, Axis::Horizontal, 3.)),
            )
            .child(
                div().flex_1().min_h_0().map(|d| {
                    if doc.page.is_some() {
                        return d.child(self.render_page(self.active, cx));
                    }
                    if doc.settings.is_some() {
                        return d.child(self.render_settings(self.active, cx));
                    }
                    if let Some(source) = &doc.preview {
                        return d.child(self.render_preview(self.active, source, cx));
                    }
                    if !doc.binary {
                        let keys = exthost::editor_keys(&doc.path, doc.lang);
                        let code = div().size_full().key_context(keys).child(self.render_code(self.active, cx));
                        if self.side_preview.as_ref() != Some(&doc.path) {
                            return d.child(code);
                        }
                        return d.child(self.render_side_preview(code, &doc.path, cx));
                    }
                    // VS Code's binary editor placeholder.
                    let ix = self.active;
                    d.child(
                        v_flex()
                            .size_full()
                            .px(px(16.))
                            .items_center()
                            .justify_center()
                            .gap(px(10.))
                            .child(
                                // editorWarning.foreground
                                codicon(icon::WARNING)
                                    .text_size(px(48.))
                                    .text_color(crate::theme::c(0xCCA700)),
                            )
                            .child(div().max_w(px(450.)).text_size(px(14.)).text_center().child(
                                "The file is not displayed in the text editor because it is either binary or uses an unsupported text encoding.",
                            ))
                            .child(
                                primary_button("open-anyway", "Open Anyway", None)
                                    .w_auto()
                                    .px(px(10.))
                                    .m(px(4.))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.reopen(ix, Encoding::Utf8, window, cx)
                                    })),
                            ),
                    )
                }),
            )
            .into_any_element()
    }

    // ---- panel -------------------------------------------------------------

    fn render_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let m = self.modern;
        let tabs = PANEL_TABS.iter().enumerate().map(|(ix, name)| {
            let active = ix == self.panel_tab;
            // Modern UI: sentence-case labels on a 24px pill.
            if m {
                return div()
                    .id(("panel-tab", ix))
                    .group("panel-tab")
                    .relative()
                    .flex()
                    .items_center()
                    .h_full()
                    .px(px(10.))
                    .cursor_pointer()
                    .text_color(if active { col::fg() } else { col::fg_muted() })
                    .hover(|s| s.text_color(col::fg()))
                    .child(
                        div()
                            .absolute()
                            .left(px(2.))
                            .right(px(2.))
                            .top(px(4.))
                            .h(px(24.))
                            .rounded(px(4.))
                            .when(active, |d| d.bg(col::tab_active()))
                            .when(!active, |d| {
                                d.group_hover("panel-tab", |s| s.bg(col::hover()))
                            }),
                    )
                    .child(div().relative().line_height(px(22.)).child(*name))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.panel_tab = ix;
                        cx.notify();
                    }));
            }
            div()
                .id(("panel-tab", ix))
                .flex()
                .flex_col()
                .justify_center()
                .h_full()
                .px(px(6.))
                .mx(px(4.))
                .cursor_pointer()
                .text_size(px(11.))
                .text_color(if active { col::fg() } else { col::fg_muted() })
                .hover(|s| s.text_color(col::fg()))
                .child(div().py(px(4.)).child(name.to_uppercase()))
                .child(div().h(px(1.)).when(active, |d| d.bg(col::accent())))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.panel_tab = ix;
                    cx.notify();
                }))
        });
        let tabs: Vec<_> = tabs.collect();

        let actions: &[char] = if self.panel_tab == TERMINAL {
            &[
                icon::ADD,
                icon::CHEVRON_DOWN,
                icon::SPLIT_HORIZONTAL,
                icon::TRASH,
                icon::ELLIPSIS,
            ]
        } else {
            &[icon::FILTER, icon::CLEAR_ALL, icon::ELLIPSIS]
        };
        let (max_icon, max_tip) = match self.panel_max {
            false => (icon::CHEVRON_UP, "Maximize Panel Size"),
            true => (icon::CHEVRON_DOWN, "Restore Panel Size"),
        };

        let body = match self.panel_tab {
            0 => self.render_problems(cx),
            TERMINAL => self.render_terminal(window, cx),
            4 => v_flex()
                .px(px(20.))
                .gap(px(10.))
                .items_start()
                .child(
                    "No forwarded ports. Forward a port to access your running services locally.",
                )
                .child(primary_button("forward-port", "Forward a Port", None))
                .into_any_element(),
            debug::DEBUG_CONSOLE => self.render_debug_console(cx),
            output::OUTPUT => self.render_output(cx),
            _ => div().into_any_element(),
        };

        v_flex()
            .size_full()
            .bg(col::bg())
            .when(!m, |d| d.border_t_1().border_color(col::border()))
            .text_size(px(13.))
            .text_color(col::fg())
            .child(
                h_flex()
                    .h(px(if m { 32. } else { 35. }))
                    .flex_none()
                    .pl(px(4.))
                    .pr(px(if m { 4. } else { 8. }))
                    .justify_between()
                    .child(h_flex().h_full().children(tabs))
                    .child(
                        h_flex()
                            .gap(px(4.))
                            .children(
                                (self.panel_tab == TERMINAL)
                                    .then(|| self.single_terminal_tab(cx))
                                    .flatten(),
                            )
                            .children(
                                (self.panel_tab == output::OUTPUT)
                                    .then(|| self.output_menu(cx))
                                    .flatten(),
                            )
                            .children(actions.iter().enumerate().map(|(i, &ch)| {
                                let text = match (ch, self.panel_tab) {
                                    (icon::CLEAR_ALL, debug::DEBUG_CONSOLE) => "Clear Console",
                                    (icon::CLEAR_ALL, output::OUTPUT) => "Clear Output",
                                    _ => action_tip(ch),
                                };
                                let button = action_button(("panel-act", i), ch).tooltip(tip(text));
                                if (ch, self.panel_tab) == (icon::CHEVRON_DOWN, TERMINAL) {
                                    return self.profile_menu(cx).into_any_element();
                                }
                                match (ch, self.panel_tab) {
                                    (icon::CLEAR_ALL, debug::DEBUG_CONSOLE) => {
                                        button.on_click(cx.listener(|this, _, _, cx| {
                                            this.clear_debug_console(cx)
                                        }))
                                    }
                                    (icon::CLEAR_ALL, output::OUTPUT) => button.on_click(
                                        cx.listener(|this, _, _, cx| this.clear_output(cx)),
                                    ),
                                    (icon::ADD, TERMINAL) => {
                                        button.on_click(cx.listener(|this, _, window, cx| {
                                            this.new_terminal(false, window, cx)
                                        }))
                                    }
                                    (icon::SPLIT_HORIZONTAL, TERMINAL) => {
                                        button.on_click(cx.listener(|this, _, window, cx| {
                                            this.new_terminal(true, window, cx)
                                        }))
                                    }
                                    (icon::TRASH, TERMINAL) => {
                                        button.on_click(cx.listener(|this, _, window, cx| {
                                            this.kill_active_terminal(window, cx)
                                        }))
                                    }
                                    _ => button,
                                }
                                .into_any_element()
                            }))
                            .child(
                                action_button("panel-max", max_icon)
                                    .tooltip(tip(max_tip))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_max_panel(cx)),
                                    ),
                            )
                            .child(
                                action_button("panel-close", icon::CLOSE)
                                    .tooltip(tip("Hide Panel (Ctrl+J)"))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.toggle_panel(&TogglePanel, w, cx)
                                    })),
                            ),
                    ),
            )
            .child(div().flex_1().min_h_0().pt(px(4.)).child(body))
    }

    // ---- status bar --------------------------------------------------------

    fn render_statusbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        /// A status bar entry with VS Code's tooltip; Modern UI rounds it.
        fn item(m: bool, id: &'static str, tip: impl Into<SharedString>) -> Stateful<Div> {
            let tip = tip.into();
            h_flex()
                .id(id)
                .h_full()
                .px(px(5.))
                .when(m, |d| d.rounded(px(4.)))
                .gap(px(4.))
                .cursor_pointer()
                .hover(|s| s.bg(col::status_hover()).text_color(col::fg()))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        }
        let m = self.modern;

        let mut right = self.extension_status(true, cx);
        let (errors, warnings) = self.problem_counts();
        let problems_tip = match (errors, warnings) {
            (0, 0) => "No Problems".to_string(),
            _ => format!("Errors: {errors}, Warnings: {warnings}"),
        };
        if let Some(doc) = self.docs.get(self.active).filter(|d| !d.binary) {
            let editor = doc.editor.read(cx);
            let pos = editor.cursor_position();
            let line = editor.text().slice_line(pos.line as usize).to_string();
            let col = palette::status_column(&line, pos.character as usize, doc.indent.tab_size);
            let lang = language_label(doc.lang);
            right.extend([
                item(m, "sb-pos", "Go to Line/Column")
                    .on_click(cx.listener(|this, _, window, cx| this.open_palette(":", window, cx)))
                    .child(format!("Ln {}, Col {col}", pos.line + 1))
                    .into_any_element(),
                item(m, "sb-indent", "Select Indentation")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.select_indentation(window, cx)),
                    )
                    .child(match doc.indent {
                        TabSize {
                            hard_tabs: true,
                            tab_size,
                        } => format!("Tab Size: {tab_size}"),
                        TabSize { tab_size, .. } => format!("Spaces: {tab_size}"),
                    })
                    .into_any_element(),
                item(m, "sb-enc", "Select Encoding")
                    .on_click(cx.listener(|this, _, window, cx| this.change_encoding(window, cx)))
                    .child(doc.encoding.label())
                    .into_any_element(),
                item(m, "sb-eol", "Select End of Line Sequence")
                    .on_click(cx.listener(|this, _, window, cx| this.change_eol(window, cx)))
                    .child(palette::eol(doc.crlf))
                    .into_any_element(),
                item(m, "sb-lang", "Select Language Mode")
                    .on_click(cx.listener(|this, _, window, cx| this.change_language(window, cx)))
                    .child(codicon(icon::CODE).text_size(px(14.)))
                    .child(lang)
                    .into_any_element(),
            ]);
        }
        // VS Code's notificationsStatus tooltips.
        let bell_tip: SharedString = match (self.center, self.notes.len(), self.unseen) {
            _ if self.palette.dnd => "Do Not Disturb Mode is Enabled".into(),
            (true, ..) => "Hide Notifications".into(),
            (_, 0, _) => "No Notifications".into(),
            (_, _, 0) => "No New Notifications".into(),
            (_, _, 1) => "1 New Notification".into(),
            (_, _, n) => format!("{n} New Notifications").into(),
        };
        right.push(
            item(m, "sb-bell", bell_tip)
                .child(
                    codicon(match (self.palette.dnd, self.unseen > 0) {
                        (false, false) => icon::BELL,
                        (false, true) => icon::BELL_DOT,
                        (true, false) => icon::BELL_SLASH,
                        (true, true) => icon::BELL_SLASH_DOT,
                    })
                    .text_size(px(14.)),
                )
                .on_click(cx.listener(|this, _, _, cx| this.show_center(!this.center, cx)))
                .into_any_element(),
        );

        let debugging = self.debug_status();
        // Modern UI: 28px with the items inset 2px 12px, below the cards.
        h_flex()
            .h(px(if m { 28. } else { 22. }))
            .flex_none()
            .justify_between()
            .when(m, |d| d.py(px(2.)).px(px(12.)))
            .bg(col::bg())
            .when(debugging.as_ref().is_some_and(|d| d.1), |d| {
                d.bg(crate::theme::c(debug::STATUS_DEBUGGING))
                    .text_color(white())
            })
            .when(!m, |d| d.border_t_1().border_color(col::border()))
            .text_size(px(12.))
            .text_color(col::fg_muted())
            .child(
                h_flex()
                    .h_full()
                    .child(
                        h_flex()
                            .id("sb-remote")
                            .tooltip(|window, cx| {
                                Tooltip::new("Open a Remote Window").build(window, cx)
                            })
                            .h_full()
                            .px(px(if m { 6. } else { 10. }))
                            .when(m, |d| d.rounded(px(4.)))
                            .bg(col::remote())
                            .text_color(white())
                            .cursor_pointer()
                            .child(codicon(icon::REMOTE).text_size(px(14.))),
                    )
                    .when(self.root.is_some(), |d| {
                        d.child(
                            item(m, "sb-branch", "main, Checkout Branch/Tag...")
                                .child(codicon(icon::SOURCE_CONTROL).text_size(px(14.)))
                                .child("main"),
                        )
                        .child(
                            item(m, "sb-sync", "Synchronize Changes")
                                .child(codicon(icon::SYNC).text_size(px(14.))),
                        )
                    })
                    .child(
                        item(m, "sb-problems", problems_tip)
                            .child(codicon(icon::ERROR).text_size(px(14.)))
                            .child(errors.to_string())
                            .child(codicon(icon::WARNING).text_size(px(14.)))
                            .child(warnings.to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_panel_tab(0, cx))),
                    )
                    .when_some(debugging, |d, (name, _)| {
                        d.child(
                            item(m, "sb-debug", "Select and Start Debug Configuration")
                                .child(codicon(icon::DEBUG_ALT_SMALL).text_size(px(14.)))
                                .child(name)
                                .on_click(cx.listener(|this, _, _, cx| this.show(View::Run, cx))),
                        )
                    })
                    .children(self.extension_status(false, cx)),
            )
            .child(h_flex().h_full().pr(px(4.)).children(right))
    }
}

impl Workbench {
    /// A file's language: its `files.associations` setting, else its name's.
    fn file_language(&self, path: &Path) -> &'static str {
        self.language_in(&terminal::settings(), path)
    }

    fn language_in(&self, settings: &serde_json::Value, path: &Path) -> &'static str {
        let root = self.root.as_deref().or(path.parent()).unwrap_or(path);
        search::associated_language(settings, root, path).unwrap_or_else(|| language_id(path))
    }

    /// After settings change from `old`, gives open files the language their
    /// `files.associations` now name, unless one was picked by hand (VS Code
    /// keeps an explicitly set language).
    fn relanguage(&mut self, old: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        let new = terminal::settings();
        self.ext.send_settings();
        if word_wrap(&new) != word_wrap(old) {
            self.set_word_wrap(word_wrap(&new), window, cx);
        }
        for ix in 0..self.docs.len() {
            let doc = &self.docs[ix];
            if doc.untitled || doc.binary || doc.lang != self.language_in(old, &doc.path) {
                continue;
            }
            let lang = self.language_in(&new, &doc.path);
            if lang != doc.lang {
                self.docs[ix].lang = lang;
                self.rebuild_editor(ix, window, cx);
            }
        }
    }

    /// A warning notification: listed in the bell's center, and shown as a
    /// toast (unless the center is open) for VS Code's 18 seconds, counted
    /// while it shows and the mouse isn't over it. A repeat replaces the old
    /// one, and the newest 3 toasts stack, newest at the bottom; an older one
    /// waits until one of them closes.
    fn warn(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.note(message.into(), Severity::Warning, cx);
    }

    /// An error notification, which toasts even in Do Not Disturb mode.
    fn error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.note(message.into(), Severity::Error, cx);
    }

    /// An error writing settings.json, with VS Code's "Open Settings" button.
    fn settings_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.note(message.into(), Severity::SettingsError, cx);
    }

    fn note(&mut self, message: SharedString, severity: Severity, cx: &mut Context<Self>) {
        let error = matches!(severity, Severity::Error | Severity::SettingsError);
        self.clear_note(&message, cx);
        self.notes.insert(0, (message.clone(), severity));
        if self.center {
            return;
        }
        self.unseen += 1;
        // VS Code's Do Not Disturb still toasts errors.
        if self.palette.dnd && !error {
            return cx.notify();
        }
        let m = message.clone();
        let hide = cx.spawn(async move |this, cx| {
            let mut since = Instant::now();
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let gone = this.update(cx, |this, cx| {
                    let shown = this.toasts.len().saturating_sub(3);
                    let Some(i) = this.toasts.iter().position(|t| t.0 == m) else {
                        return true;
                    };
                    if i < shown || this.toasts[i].1 {
                        since = Instant::now();
                    }
                    if since.elapsed() < Duration::from_secs(18) {
                        return false;
                    }
                    drop(this.toasts.remove(i));
                    cx.notify();
                    true
                });
                if gone.unwrap_or(true) {
                    break;
                }
            }
        });
        self.toasts.push((message, false, hide));
    }

    /// Opens or closes the bell's center; opening it hides the toast and
    /// marks every notification seen.
    fn show_center(&mut self, show: bool, cx: &mut Context<Self>) {
        self.center = show;
        if show {
            self.toasts.clear();
            self.unseen = 0;
        }
        cx.notify();
    }

    /// Shows an activity bar item's hover after VS Code's delay, or at once
    /// while moving from one item to the next; leaving or clicking hides it.
    fn hover_activity(&mut self, id: SharedString, hovered: bool, cx: &mut Context<Self>) {
        if !hovered {
            if self.act_hover.take_if(|h| *h == id).is_some() {
                self.act_hid = Some(Instant::now());
                cx.notify();
            }
            self.act_wait.take_if(|(w, _)| *w == id);
            return;
        }
        let moving = self.act_hover.is_some()
            || (self.act_hid).is_some_and(|t| t.elapsed() < Duration::from_millis(300));
        if moving {
            self.act_hover = Some(id);
            return cx.notify();
        }
        let shown = id.clone();
        let wait = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            this.update(cx, |this, cx| {
                this.act_hover = Some(shown);
                this.act_wait = None;
                cx.notify();
            })
            .ok();
        });
        self.act_wait = Some((id, wait));
    }

    /// Toggle Do Not Disturb Mode; turning it on hides the toast. It
    /// survives restarts, as in VS Code.
    fn toggle_dnd(&mut self, cx: &mut Context<Self>) {
        self.palette.dnd = !self.palette.dnd;
        self.palette.save(cx);
        if self.palette.dnd {
            self.toasts.clear();
        }
        cx.notify();
    }

    /// Clear All Notifications, which closes the center too.
    fn clear_notes(&mut self, cx: &mut Context<Self>) {
        for (_, (id, _)) in std::mem::take(&mut self.ext.buttons) {
            self.ext.answer(id, serde_json::Value::Null);
        }
        self.notes.clear();
        self.toasts.clear();
        self.unseen = 0;
        self.center = false;
        cx.notify();
    }

    /// Clears a notification, from the center and its toast.
    fn clear_note(&mut self, message: &SharedString, cx: &mut Context<Self>) {
        // An extension's buttons, closed without one: dismissed.
        if let Some((id, _)) = self.ext.buttons.remove(message) {
            self.ext.answer(id, serde_json::Value::Null);
        }
        let count = self.notes.len();
        self.notes.retain(|n| &n.0 != message);
        if self.notes.len() < count && !self.center {
            self.unseen = self.unseen.saturating_sub(1);
        }
        self.toasts.retain(|t| &t.0 != message);
        cx.notify();
    }

    /// One notification's severity icon, message, close button and, below
    /// them, an error's Open Settings button.
    fn note_row(
        &self,
        id: impl Into<ElementId>,
        &(ref message, severity): &(SharedString, Severity),
        cx: &mut Context<Self>,
    ) -> Div {
        let id = id.into();
        let gone = message.clone();
        let open = message.clone();
        // Icon and close button stay on a wrapped message's first line, as VS Code's do.
        let row = h_flex()
            .items_start()
            .min_h(px(42.))
            .px(px(8.))
            .py(px(8.))
            .gap(px(8.))
            // notificationsErrorIcon.foreground and notificationsWarningIcon.foreground
            .child(match severity {
                Severity::Info => codicon(icon::INFO).mt(px(5.)).text_color(c(0x3794FF)),
                Severity::Warning => codicon(icon::WARNING).mt(px(5.)).text_color(c(0xCCA700)),
                _ => codicon(icon::ERROR).mt(px(5.)).text_color(c(0xF14C4C)),
            })
            // min width 0 lets a long message wrap instead of pushing out the close button.
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .py(px(4.))
                    .child(message.clone()),
            )
            .child(action_button(id.clone(), icon::CLOSE).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.clear_note(&gone, cx);
                    // The center closes once it's empty, as VS Code's does.
                    this.center &= !this.notes.is_empty();
                },
            )));
        v_flex()
            .child(row)
            .when(severity == Severity::SettingsError, |d| {
                // An action closes its notification, as in VS Code.
                // button.background, button.hoverBackground and button.foreground; the
                // custom variant dims its colour, so bg sets it too.
                let blue = ButtonCustomVariant::new(cx)
                    .color(c(0x0078D4))
                    .hover(c(0x026EC1))
                    .active(c(0x026EC1))
                    .foreground(c(0xFFFFFF));
                let button = Button::new((id.clone(), "open-settings"))
                    .custom(blue)
                    .bg(c(0x0078D4))
                    .small()
                    .label("Open Settings")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.clear_note(&open, cx);
                        this.center &= !this.notes.is_empty();
                        this.open_settings(window, cx);
                    }));
                d.child(h_flex().justify_end().px(px(8.)).pb(px(8.)).child(button))
            })
            // An extension's buttons, the first one primary, as in VS Code.
            .when_some(self.ext.buttons.get(message), |d, (_, titles)| {
                let buttons = titles.iter().enumerate().map(|(i, title)| {
                    let m = message.clone();
                    let button = Button::new(SharedString::from(format!("{id}-button-{i}")))
                        .small()
                        .label(title.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some((q, _)) = this.ext.buttons.remove(&m) {
                                this.ext.answer(q, serde_json::json!(i));
                            }
                            this.clear_note(&m, cx);
                            this.center &= !this.notes.is_empty();
                        }));
                    match i {
                        0 => {
                            let blue = ButtonCustomVariant::new(cx)
                                .color(c(0x0078D4))
                                .hover(c(0x026EC1))
                                .active(c(0x026EC1))
                                .foreground(c(0xFFFFFF));
                            button.custom(blue).bg(c(0x0078D4))
                        }
                        // button.secondaryBackground
                        _ => button.bg(c(0x313131)),
                    }
                });
                d.child(
                    h_flex()
                        .justify_end()
                        .gap(px(8.))
                        .px(px(8.))
                        .pb(px(8.))
                        .children(buttons),
                )
            })
    }

    /// VS Code's notification center, opened from the bell.
    fn render_center(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.center {
            return None;
        }
        // Modern UI: notificationsDialogs.css insets and cornerRadius-large.
        let m = self.modern;
        let header = if self.notes.is_empty() {
            "NO NEW NOTIFICATIONS"
        } else {
            "NOTIFICATIONS"
        };
        Some(
            v_flex()
                .absolute()
                .right(px(if m { 8. } else { 11. }))
                .bottom(px(if m { 36. } else { 33. }))
                .w(px(450.))
                .rounded(px(if m { 8. } else { 6. }))
                .overflow_hidden()
                // notifications.background
                .bg(c(0x202122))
                .border_1()
                .border_color(col::border())
                .shadow_lg()
                .text_size(px(13.))
                .text_color(col::fg())
                .child(
                    h_flex()
                        .h(px(35.))
                        .pl(px(10.))
                        .pr(px(6.))
                        .gap(px(2.))
                        .text_size(px(11.))
                        .child(div().flex_1().child(header))
                        .child(
                            action_button("center-clear", icon::CLEAR_ALL)
                                .tooltip(tip("Clear All Notifications"))
                                .on_click(cx.listener(|this, _, _, cx| this.clear_notes(cx))),
                        )
                        .child(
                            action_button("center-dnd", icon::BELL_SLASH)
                                .tooltip(tip("Toggle Do Not Disturb Mode"))
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_dnd(cx))),
                        )
                        .child(
                            action_button("center-hide", icon::CHEVRON_DOWN)
                                .tooltip(tip("Hide Notification Center"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.center = false;
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    v_flex()
                        .id("center-list")
                        .max_h(px(420.))
                        .overflow_y_scroll()
                        .children(self.notes.iter().enumerate().map(|(i, n)| {
                            self.note_row(("center-close", i), n, cx)
                                .border_t_1()
                                .border_color(col::border())
                        })),
                ),
        )
    }

    fn render_toasts(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let modern = self.modern;
        let shown = self.toasts.len().saturating_sub(3);
        let toasts = self
            .toasts
            .iter()
            .enumerate()
            .skip(shown)
            .filter_map(|(i, (message, ..))| {
                let note = self.notes.iter().find(|n| &n.0 == message)?;
                let m = message.clone();
                Some(
                    self.note_row(("toast-close", i), note, cx)
                        .id(("toast", i))
                        .rounded(px(if modern { 8. } else { 6. }))
                        // notifications.background
                        .bg(c(0x202122))
                        .border_1()
                        .border_color(col::border())
                        .shadow_lg()
                        .text_size(px(13.))
                        .text_color(col::fg())
                        .on_hover(cx.listener(move |this, &over, _, _| {
                            if let Some(t) = this.toasts.iter_mut().find(|t| t.0 == m) {
                                t.1 = over;
                            }
                        }))
                        // VS Code's fade-in: up from below over 300ms, keyed
                        // by message so a toast that moves up doesn't redo it.
                        .relative()
                        .with_animation(
                            SharedString::from(format!("toast-in {message}")),
                            Animation::new(Duration::from_millis(300))
                                .with_easing(|t| 1. - (1. - t).powi(2)),
                            |toast, d| toast.top(px(50. * (1. - d))).opacity(d),
                        ),
                )
            });
        v_flex()
            .absolute()
            .right(px(if modern { 8. } else { 11. }))
            .bottom(px(if modern { 36. } else { 33. }))
            .w(px(450.))
            .gap(px(if modern { 4. } else { 8. }))
            .children(toasts)
    }
}

/// A notification's severity; settings.json errors offer Open Settings.
#[derive(Clone, Copy, PartialEq)]
enum Severity {
    Info,
    Warning,
    Error,
    SettingsError,
}

/// VS Code's hover beside the activity bar: a box with a beak pointing back
/// at the item, centred on its row (48px, or 36px in Modern UI).
fn activity_hover(text: SharedString, row: f32) -> impl IntoElement {
    let m = row < 48.;
    let (bg, border) = (c(0x202122), col::input_border());
    div()
        .relative()
        .flex()
        .items_center()
        .h(px(row))
        .child(
            div()
                .ml(px(6.))
                .px(px(8.))
                .py(px(4.))
                .bg(bg)
                .border_1()
                .border_color(border)
                // Modern UI: monaco-hover takes cornerRadius-large.
                .rounded(px(if m { 8. } else { 4. }))
                .shadow_md()
                .text_size(px(13.))
                .text_color(col::fg())
                .whitespace_nowrap()
                .child(text),
        )
        .child(
            // The border's triangle, then the fill's one pixel in, over the
            // box's left border.
            canvas(
                |_, _, _| {},
                move |b, _, window, _| {
                    for (inset, color) in [(0., border), (1.4, bg)] {
                        let mut path = PathBuilder::fill();
                        path.move_to(point(b.left() + px(inset), b.center().y));
                        path.line_to(point(b.right(), b.top() + px(inset * 0.7)));
                        path.line_to(point(b.right(), b.bottom() - px(inset * 0.7)));
                        path.close();
                        if let Ok(path) = path.build() {
                            window.paint_path(path, color);
                        }
                    }
                },
            )
            .absolute()
            .left_0()
            .top(px(row / 2. - 6.))
            .w(px(7.))
            .h(px(12.)),
        )
}

fn reveal_path(path: &Path) {
    let select = format!("/select,{}", path.display());
    std::process::Command::new("explorer.exe")
        .arg(select)
        .spawn()
        .ok();
}

/// An explorer row's right-click menu, as VS Code's for a file or folder;
/// the `root` one is the empty area's.
// ponytail: no New File/Folder, Open to the Side, Rename or Delete yet (no
// inline rename, split editors or recycle bin).
fn explorer_menu(
    menu: PopupMenu,
    wb: &WeakEntity<Workbench>,
    path: &Path,
    is_dir: bool,
    root: bool,
    clip: bool,
) -> PopupMenu {
    type Act = fn(&mut Workbench, &Path, &mut Window, &mut Context<Workbench>);
    let item = |label: &'static str, action: Option<Box<dyn Action>>, act: Act| {
        let (wb, path) = (wb.clone(), path.to_path_buf());
        PopupMenuItem::element(chord_row(label, action)).on_click(move |_, window, cx| {
            wb.update(cx, |this, cx| act(this, &path, window, cx)).ok();
        })
    };
    let menu = match is_dir {
        true => menu,
        false => menu
            .item(item("Open", None, |this, path, w, cx| {
                this.open_path(path.to_path_buf(), w, cx)
            }))
            .when(language_id(path) == "markdown", |m| {
                m.item(item("Open Preview", None, |this, path, w, cx| {
                    this.markdown_preview(Some(path.to_path_buf()), false, w, cx)
                }))
            })
            .separator(),
    };
    let menu = menu
        .item(item(
            "Reveal in File Explorer",
            Some(Box::new(RevealInFileExplorer)),
            |_, path, _, _| reveal_path(path),
        ))
        .item(item(
            "Open in Integrated Terminal",
            None,
            move |this, path, w, cx| {
                let dir = if path.is_dir() {
                    path
                } else {
                    path.parent().unwrap_or(path)
                };
                this.open_terminal_at(dir.to_path_buf(), w, cx)
            },
        ))
        .separator();
    let menu = match is_dir {
        true => menu
            .item(item(
                "Find in Folder...",
                Some(Box::new(FindInFolder)),
                |this, path, w, cx| this.find_in_folder(path, w, cx),
            ))
            .separator(),
        false => menu,
    };
    // The workspace folder itself can't be cut or copied.
    let menu = match root {
        true => menu,
        false => menu
            .item(item("Cut", Some(Box::new(CutFile)), |this, _, _, cx| {
                this.clip_file(true, cx)
            }))
            .item(item("Copy", Some(Box::new(CopyFile)), |this, _, _, cx| {
                this.clip_file(false, cx)
            })),
    };
    let menu = menu
        .item(
            item("Paste", Some(Box::new(PasteFile)), |this, path, w, cx| {
                this.paste_file(Some(path.to_path_buf()), w, cx)
            })
            .disabled(!clip),
        )
        .separator()
        .item(item(
            "Copy Path",
            Some(Box::new(CopyPath)),
            |this, path, _, cx| this.copy_file_path(path, false, cx),
        ))
        .item(item(
            "Copy Relative Path",
            Some(Box::new(CopyRelativePath)),
            |this, path, _, cx| this.copy_file_path(path, true, cx),
        ));
    let mut focus = KeyContext::parse("Explorer filesExplorerFocus explorerViewletFocus").unwrap();
    if is_dir {
        focus.add("explorerResourceIsFolder");
    }
    exthost::resource_keys(&mut focus, path);
    let items = exthost::menu_items("explorer/context", &[focus]);
    ext_menu_items(menu, wb, &items, exthost::uri_args(Some(path)))
}

/// Extensions' menu items after a separator, run with `args`.
fn ext_menu_items(
    menu: PopupMenu,
    wb: &WeakEntity<Workbench>,
    items: &[exthost::MenuItem],
    args: Vec<serde_json::Value>,
) -> PopupMenu {
    let menu = if items.is_empty() {
        menu
    } else {
        menu.separator()
    };
    items.iter().fold(menu, |menu, item| {
        let (wb, command, args) = (wb.clone(), item.id.clone(), args.clone());
        menu.item(
            PopupMenuItem::new(item.title.clone()).on_click(move |_, _, cx| {
                wb.update(cx, |this, cx| {
                    this.run_extension_command(command.clone(), args.clone(), cx)
                })
                .ok();
            }),
        )
    })
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.record_nav(cx);
        let title = self.window_title();
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        // Saved as it changes, so a kill or crash still reopens it.
        let session = format!("{:?}", (&self.root, self.docs.iter().map(|d| &d.path).collect::<Vec<_>>(), self.active));
        if session != self.saved_session {
            self.save_session();
            self.saved_session = session;
        }
        let sidebar = self.render_sidebar(cx);
        // A file dragged from the explorer opens where it's dropped; so do
        // files from Windows, and a folder from there becomes the workspace.
        let editor = div()
            .relative()
            .size_full()
            .on_drop(cx.listener(|this, file: &DraggedFile, window, cx| {
                this.open_path(file.0.clone(), window, cx)
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_paths(paths.paths(), None, window, cx)
            }))
            .child(self.render_editor_group(cx))
            // The highlight goes over the editor, below the tab strip (whose
            // tabs light up on their own).
            .child(
                div()
                    .absolute()
                    .top(px(if self.docs.is_empty() { 0. } else { 35. }))
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .drag_over::<DraggedFile>(|s, _, _, _| s.bg(col::drop()))
                    .drag_over::<ExternalPaths>(|s, _, _, _| s.bg(col::drop())),
            );
        let panel = self.render_panel(window, cx);
        // Modern UI: floating cards 4px apart, a resize grip in each gap.
        let (sidebar, editor, panel, handles) = match self.modern {
            false => (
                sidebar,
                editor.into_any_element(),
                panel.into_any_element(),
                None,
            ),
            true => {
                let (panel_shown, editor_shown) = (self.panel, !(self.panel && self.panel_max));
                let left = if self.sidebar { 2. } else { 4. };
                let wrap = |top: f32, bottom: f32| {
                    div()
                        .size_full()
                        .pl(px(left))
                        .pr(px(4.))
                        .pt(px(top))
                        .pb(px(bottom))
                };
                (
                    div()
                        .size_full()
                        .pr(px(2.))
                        .pb(px(4.))
                        .child(card(sidebar, col::bg(), [0., 8., 8., 0.], true))
                        .into_any_element(),
                    wrap(0., if panel_shown { 2. } else { 4. })
                        .child(card(editor, col::editor(), [8.; 4], false))
                        .into_any_element(),
                    wrap(if editor_shown { 2. } else { 0. }, 4.)
                        .child(card(panel, col::bg(), [8.; 4], false))
                        .into_any_element(),
                    Some(Rc::new(grip) as gpui_kit::base::ResizeHandleRenderer),
                )
            }
        };
        let resizable = |group: gpui_kit::base::ResizablePanelGroup| match &handles {
            Some(h) => group.with_handle_appearance(h.clone()),
            None => group,
        };
        let palette = self.render_palette(window, cx);
        let toolbar = self.render_debug_toolbar(window, cx);
        let signature = self.render_signature(cx);

        v_flex()
            .key_context(match self.center || !self.toasts.is_empty() {
                true => "Workbench notificationsVisible",
                false => "Workbench",
            })
            .track_focus(&self.focus)
            .children(signature)
            .capture_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    this.hide_signature(cx);
                }
            }))
            .capture_action(cx.listener(|this, _: &input::IndentInline, _, cx| {
                if this.snippet_tab(false, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &input::OutdentInline, _, cx| {
                if this.snippet_tab(true, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &input::Escape, _, _| this.snippet = None))
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(|this, _: &GoBack, window, cx| this.navigate(true, window, cx)))
            .on_action(cx.listener(|this, _: &NextEditor, window, cx| this.cycle(1, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousEditor, window, cx| {
                this.cycle(this.docs.len().saturating_sub(1), window, cx)
            }))
            // The mouse's side buttons.
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, window, cx| this.navigate(true, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _, window, cx| this.navigate(false, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &GoForward, window, cx| this.navigate(false, window, cx)),
            )
            .on_action(cx.listener(|this, _: &GoToLastEditLocation, window, cx| {
                if let Some((path, pos)) = this.last_edit.clone() {
                    this.go_to(path, pos, window, cx);
                }
            }))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::close_folder))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::save_all))
            .on_action(cx.listener(Self::close_editor))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_panel))
            .on_action(cx.listener(|this, _: &RevealInExplorerView, _, cx| {
                if let Some(doc) = this.docs.get(this.active) {
                    this.reveal_in_tree(&doc.path.clone(), true, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &CloseAllEditors, w, cx| {
                this.close_editors(|_, _| true, w, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseOtherEditors, w, cx| {
                let active = this.active;
                this.close_editors(|ix, _| ix != active, w, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseEditorsToTheRight, w, cx| {
                let active = this.active;
                this.close_editors(|ix, _| ix > active, w, cx)
            }))
            .on_action(
                cx.listener(|this, _: &CopyPath, _, cx| this.copy_path(this.active, false, cx)),
            )
            .on_action(cx.listener(|this, _: &CopyRelativePath, _, cx| {
                this.copy_path(this.active, true, cx)
            }))
            .on_action(
                cx.listener(|this, _: &RevealInFileExplorer, _, _| this.reveal_in_os(this.active)),
            )
            .on_action(cx.listener(|this, _: &CloseSavedEditors, w, cx| {
                this.remove_where(|d| !d.dirty, w, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleMaximizedPanel, _, cx| this.toggle_max_panel(cx)),
            )
            .on_action(cx.listener(Self::toggle_terminal))
            .on_action(cx.listener(|this, _: &ToggleProblems, _, cx| this.toggle_panel_tab(0, cx)))
            .on_action(cx.listener(|this, _: &ToggleOutput, _, cx| this.toggle_panel_tab(1, cx)))
            .on_action(cx.listener(|this, _: &ToggleDebugConsole, _, cx| {
                this.toggle_panel_tab(debug::DEBUG_CONSOLE, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowExplorer, window, cx| {
                (this.sidebar, this.view) = (true, View::Explorer);
                window.focus(&this.tree_focus, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &RefreshExplorer, _, cx| {
                this.dirs.clear();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CollapseExplorerFolders, _, cx| {
                this.expanded.clear();
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &OpenSettingsJson, window, cx| {
                    this.open_settings(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings_ui("", window, cx)
            }))
            .on_action(cx.listener(|this, _: &MarkdownShowPreview, window, cx| {
                this.markdown_preview(None, false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &MarkdownShowPreviewToSide, window, cx| {
                this.markdown_preview(None, true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &MarkdownShowSource, window, cx| {
                let source = this.docs.get(this.active).and_then(|d| d.preview.clone());
                if let Some(source) = source {
                    this.open_path(source, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ShowSearch, window, cx| {
                this.sidebar = true;
                this.view = View::Search;
                this.focus_search(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ReplaceInFiles, window, cx| {
                this.sidebar = true;
                this.view = View::Search;
                this.show_replace(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ReplaceAll, window, cx| {
                // VS Code: whenever the Search view shows with replace open.
                if this.sidebar && this.view == View::Search {
                    this.replace_all(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ShowScm, _, cx| this.show(View::Scm, cx)))
            .on_action(cx.listener(|this, _: &ShowRun, _, cx| this.show(View::Run, cx)))
            .on_action(cx.listener(|this, _: &ShowExtensions, window, cx| {
                this.show(View::Extensions, cx);
                if this.sidebar {
                    this.ext_search.update(cx, |s, cx| s.focus(window, cx));
                }
            }))
            .on_action(
                cx.listener(|this, _: &QuickOpen, window, cx| this.open_palette("", window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &GoToLine, window, cx| this.open_palette(":", window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenView, window, cx| {
                    this.open_palette("view ", window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &GoToSymbol, window, cx| this.open_palette("@", window, cx)),
            )
            .on_action(cx.listener(|this, _: &GoToSymbolInWorkspace, window, cx| {
                this.open_palette("#", window, cx)
            }))
            .on_action(cx.listener(|this, _: &CommandPalette, window, cx| {
                this.open_palette(">", window, cx)
            }))
            .on_action(cx.listener(|this, _: &About, window, cx| this.about(window, cx)))
            .on_action(cx.listener(Self::new_text_file))
            .on_action(cx.listener(Self::revert_file))
            .on_action(cx.listener(Self::toggle_auto_save))
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(|this, _: &SaveAs, window, cx| {
                if this.docs.get(this.active).is_some_and(|d| !d.binary) {
                    this.save_as(this.active, window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &NewTerminal, window, cx| {
                    this.new_terminal(false, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &SplitTerminal, window, cx| {
                this.new_terminal(true, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &RenameTerminal, window, cx| {
                    this.rename_terminal(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ChangeTerminalColor, window, cx| {
                this.change_terminal_color(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ChangeTerminalIcon, window, cx| {
                this.change_terminal_icon(window, cx)
            }))
            .on_action(cx.listener(|this, _: &KillTerminal, window, cx| {
                this.kill_active_terminal(window, cx)
            }))
            .on_action(cx.listener(|this, _: &UnsplitTerminal, window, cx| {
                this.unsplit_terminal(window, cx)
            }))
            .on_action(cx.listener(|this, _: &JoinTerminals, window, cx| {
                this.pick_terminal_to_join(window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusNextTerminalGroup, window, cx| {
                this.focus_next_terminal(true, false, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &FocusPreviousTerminalGroup, window, cx| {
                    this.focus_next_terminal(true, true, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &FocusNextTerminal, window, cx| {
                this.focus_next_terminal(false, false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusPreviousTerminal, window, cx| {
                this.focus_next_terminal(false, true, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &FindInTerminal, window, cx| {
                    this.find_in_terminal(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &FindNextInTerminal, window, cx| {
                this.find_step(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FindPreviousInTerminal, window, cx| {
                this.find_step(true, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleWordWrap, window, cx| {
                    this.toggle_word_wrap(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &CopyLinesUp, window, cx| {
                    this.line_op(true, true, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &CopyLinesDown, window, cx| {
                this.line_op(false, true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveLinesUp, window, cx| {
                this.line_op(true, false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveLinesDown, window, cx| {
                this.line_op(false, false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &AddNextOccurrence, _, cx| {
                this.add_occurrences(Occurrence::Next, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectAllOccurrences, _, cx| {
                this.add_occurrences(Occurrence::All, cx)
            }))
            .on_action(cx.listener(|this, _: &AddPreviousOccurrence, _, cx| {
                this.add_occurrences(Occurrence::Previous, cx)
            }))
            .on_action(
                cx.listener(|this, _: &MoveSelectionToNextFindMatch, _, cx| {
                    this.add_occurrences(Occurrence::MoveLast, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ExpandSelection, _, cx| this.expand_selection(true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ShrinkSelection, _, cx| this.expand_selection(false, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleLineComment, window, cx| {
                this.comment_lines(None, window, cx)
            }))
            .on_action(cx.listener(|this, _: &AddLineComment, window, cx| {
                this.comment_lines(Some(true), window, cx)
            }))
            .on_action(cx.listener(|this, _: &RemoveLineComment, window, cx| {
                this.comment_lines(Some(false), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleBlockComment, window, cx| {
                let lang = this.docs.get(this.active).map(|d| d.lang);
                if let Some(tokens) = lang.and_then(block_comment) {
                    this.edit_lines(|text, sel| block_toggled(text, sel, tokens), window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSizeToContentWidth, _, cx| {
                this.toggle_size_to_content_width(cx)
            }))
            .on_action(
                cx.listener(|this, _: &ScrollTerminalUp, _, cx| this.scroll_terminal(1, 0, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ScrollTerminalDown, _, cx| this.scroll_terminal(-1, 0, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ScrollTerminalUpPage, _, cx| this.scroll_terminal(0, 1, cx)),
            )
            .on_action(cx.listener(|this, _: &ScrollTerminalDownPage, _, cx| {
                this.scroll_terminal(0, -1, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollTerminalToTop, _, cx| {
                this.scroll_terminal(isize::MAX, 0, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollTerminalToBottom, _, cx| {
                this.scroll_terminal(isize::MIN, 0, cx)
            }))
            .on_action(cx.listener(|this, _: &ChangeEol, window, cx| this.change_eol(window, cx)))
            .on_action(
                cx.listener(|this, a: &exthost::RunExtensionCommand, window, cx| {
                    let focus = window.context_stack();
                    if (a.when.as_deref()).is_some_and(|w| !exthost::when_holds(w, &focus)) {
                        return cx.propagate();
                    }
                    this.run_extension_command(a.id.clone(), a.args(), cx)
                }),
            )
            .on_action(cx.listener(|this, _: &InstallFromVsix, _, cx| this.install_from_vsix(cx)))
            .on_action(cx.listener(|this, _: &SelectColorTheme, window, cx| {
                this.select_color_theme(window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectIconTheme, window, cx| {
                this.select_icon_theme(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &InsertSnippet, window, cx| this.insert_snippet(window, cx)),
            )
            .on_action(cx.listener(|this, _: &IndentUsingSpaces, window, cx| {
                this.indent_action(0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &IndentUsingTabs, window, cx| {
                this.indent_action(1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ChangeTabDisplaySize, window, cx| {
                this.indent_action(2, window, cx)
            }))
            .on_action(cx.listener(|this, _: &DetectIndentation, window, cx| {
                this.indent_action(3, window, cx)
            }))
            .on_action(cx.listener(|this, _: &IndentationToSpaces, window, cx| {
                this.indent_action(4, window, cx)
            }))
            .on_action(cx.listener(|this, _: &IndentationToTabs, window, cx| {
                this.indent_action(5, window, cx)
            }))
            .on_action(cx.listener(|this, _: &TrimTrailingWhitespace, window, cx| {
                this.indent_action(6, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FormatDocument, window, cx| {
                this.format_document(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FormatSelection, window, cx| {
                this.format_document(true, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &RenameSymbol, window, cx| this.rename_symbol(window, cx)),
            )
            .on_action(cx.listener(|this, _: &FindAllReferences, window, cx| {
                this.find_references(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &TriggerParameterHints, _, cx| this.signature_help(cx)),
            )
            .on_action(cx.listener(|this, _: &ChangeLanguageMode, window, cx| {
                this.change_language(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ChangeEncoding, window, cx| {
                    this.change_encoding(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &SelectDefaultProfile, window, cx| {
                this.select_default_profile(window, cx)
            }))
            .on_action(cx.listener(|this, _: &StartDebugging, window, cx| {
                this.start_debugging(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RunWithoutDebugging, window, cx| {
                this.start_debugging(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &StopDebugging, _, cx| this.stop_debugging(cx)))
            .on_action(cx.listener(|this, _: &RestartDebugging, _, cx| this.restart_debugging(cx)))
            .on_action(cx.listener(|this, _: &Pause, _, cx| this.pause(cx)))
            .on_action(cx.listener(|this, _: &StepOver, _, cx| this.resume("next", cx)))
            .on_action(cx.listener(|this, _: &StepInto, _, cx| this.resume("stepIn", cx)))
            .on_action(cx.listener(|this, _: &StepOut, _, cx| this.resume("stepOut", cx)))
            .on_action(
                cx.listener(|this, _: &ToggleBreakpoint, _, cx| this.toggle_breakpoint(None, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenLaunchJson, window, cx| {
                this.create_launch_json(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ClearRecent, window, cx| this.clear_recent(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowNotifications, _, cx| this.show_center(true, cx)))
            .on_action(cx.listener(|this, _: &HideNotifications, _, cx| {
                // The toasts stay listed in the center.
                this.toasts.clear();
                this.show_center(false, cx);
            }))
            .on_action(cx.listener(|this, _: &ClearAllNotifications, _, cx| this.clear_notes(cx)))
            .on_action(cx.listener(|this, _: &ToggleDoNotDisturb, _, cx| this.toggle_dnd(cx)))
            .relative()
            .size_full()
            .font_family("Segoe UI")
            .text_size(px(13.))
            .bg(if self.modern {
                col::bg()
            } else {
                col::editor()
            })
            .text_color(col::fg())
            .child(self.render_titlebar(window, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_activitybar(cx))
                    .child(
                        resizable(h_resizable("workbench"))
                            .child(
                                resizable_panel()
                                    .size(px(300.))
                                    .size_range(px(170.)..px(800.))
                                    .visible(self.sidebar)
                                    .child(sidebar),
                            )
                            .child(
                                resizable_panel().child(
                                    resizable(v_resizable("editor-panel"))
                                        .child(
                                            resizable_panel()
                                                .visible(!(self.panel && self.panel_max))
                                                .child(editor),
                                        )
                                        .child(
                                            resizable_panel()
                                                .size(px(260.))
                                                .size_range(px(100.)..px(2000.))
                                                .visible(self.panel)
                                                .child(panel),
                                        ),
                                ),
                            ),
                    ),
            )
            .child(self.render_statusbar(cx))
            .children(toolbar)
            .child(self.render_toasts(cx))
            .children(self.render_center(cx))
            .children(palette)
    }
}

// ---- helpers ---------------------------------------------------------------

/// A Modern UI floating card: `content` on `bg` inside a 1px border, corners
/// `r` (tl, tr, br, bl) rounded. gpui clips to rectangles, so shell-colored
/// pieces round the content's corners off, spilling into the 4px gutter.
/// `seam` gives up the left border where the card meets the activity bar.
fn card(content: impl IntoElement, bg: Hsla, r: [f32; 4], seam: bool) -> Div {
    // A 4px border whose inner edge is the card's rounded corner.
    let corner = |ix: usize| {
        let (o, s, w) = (px(-4.), px(r[ix] + 4.), px(4.));
        let d = div().absolute().size(s).border_color(col::bg());
        match ix {
            0 => d.top(o).left(o).border_t(w).border_l(w).rounded_tl(s),
            1 => d.top(o).right(o).border_t(w).border_r(w).rounded_tr(s),
            2 => d.bottom(o).right(o).border_b(w).border_r(w).rounded_br(s),
            _ => d.bottom(o).left(o).border_b(w).border_l(w).rounded_bl(s),
        }
    };
    let [tl, tr, br, bl] = r.map(px);
    div()
        .relative()
        .size_full()
        .p(px(1.))
        .bg(bg)
        .child(content)
        .children((0..4).filter(|&ix| r[ix] > 0.).map(corner))
        .child(
            div()
                .absolute()
                .inset_0()
                .border_t_1()
                .border_r_1()
                .border_b_1()
                .when(!seam, |d| d.border_l_1())
                .border_color(col::border())
                .rounded_tl(tl)
                .rounded_tr(tr)
                .rounded_br(br)
                .rounded_bl(bl),
        )
}

/// Modern UI's sash grip between cards: three dots in foreground at 40%,
/// VS Code's 4px focus line while hovered or dragged.
// ponytail: the line shows at once; VS Code waits workbench.sash.hoverDelay.
fn grip(h: &ResizeHandleContext, _: &mut Window, _: &mut App) -> Option<AnyElement> {
    let vertical = h.axis() == Axis::Horizontal;
    let line = div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .map(|d| match vertical {
            true => d.h_full().w(px(1.)),
            false => d.w_full().h(px(1.)),
        });
    let dot = || {
        div()
            .flex_none()
            .size(px(2.))
            .rounded_full()
            .bg(col::fg().opacity(0.4))
    };
    let child = match h.state() {
        ResizeHandleState::Idle => div()
            .flex_none()
            .flex()
            .when(vertical, |d| d.flex_col())
            .gap(px(3.))
            .children([dot(), dot(), dot()]),
        _ => div()
            .flex_none()
            .rounded(px(2.))
            .bg(col::focus())
            .map(|d| match vertical {
                true => d.h_full().w(px(4.)),
                false => d.w_full().h(px(4.)),
            }),
    };
    Some(line.child(child).into_any_element())
}

/// CSS's `text-transform: capitalize`: "Run and Debug" -> "Run And Debug".
fn capitalize(s: &str) -> String {
    let mut up = true;
    s.chars()
        .map(|c| {
            let out = if up {
                c.to_uppercase().next().unwrap_or(c)
            } else {
                c
            };
            up = c.is_whitespace();
            out
        })
        .collect()
}

fn ca_white(alpha: u8) -> Hsla {
    crate::theme::ca(0xFFFFFF00 | alpha as u32)
}

/// Highlighter language names gpui-component accepts.
fn editor_language(id: &str) -> String {
    match id {
        "typescriptreact" => "tsx".into(),
        "javascriptreact" | "javascript" => "javascript".into(),
        "shellscript" => "bash".into(),
        "makefile" => "make".into(),
        "plaintext" | "ignore" => "text".into(),
        id => id.into(),
    }
}

/// 22px collapsible pane header (bold, uppercase, 11px) with optional hover actions.
fn section_header(id: impl Into<SharedString>, label: &str, open: bool) -> Stateful<Div> {
    let id = id.into();
    h_flex()
        .group(SharedString::from(format!("{id}-hdr")))
        .id(id)
        .h(px(22.))
        .flex_none()
        .pl(px(2.))
        .pr(px(8.))
        .justify_between()
        .cursor_pointer()
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .text_color(col::fg())
        .child(
            h_flex()
                .min_w_0()
                .child(
                    codicon(if open {
                        icon::CHEVRON_DOWN
                    } else {
                        icon::CHEVRON_RIGHT
                    })
                    .w(px(20.)),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(label.to_uppercase()),
                ),
        )
}

/// Puts `editor`'s cursor at `pos`, centering its line when it's out of view
/// like VS Code's jumps (revealPositionInCenterIfOutsideViewport).
fn reveal(editor: &Entity<EditorState>, pos: input::Position, window: &mut Window, cx: &mut App) {
    let laid_out = editor.update(cx, |e, cx| {
        let from = e.scroll_offset();
        e.set_cursor_position(pos, window, cx);
        center_line(e, pos.line, Some(from), cx)
    });
    if !laid_out {
        // A new editor: centered once it's laid out, as seen from the top.
        let editor = editor.clone();
        window.on_next_frame(move |window, cx| center_later(editor, pos.line, 3, window, cx));
    }
}

fn center_later(
    editor: Entity<EditorState>,
    line: u32,
    tries: u8,
    window: &mut Window,
    cx: &mut App,
) {
    let laid_out = editor.update(cx, |e, cx| center_line(e, line, Some(Point::default()), cx));
    if !laid_out && tries > 1 {
        window.on_next_frame(move |window, cx| center_later(editor, line, tries - 1, window, cx));
    }
}

/// Scrolls buffer `line` to the middle of `e`, unless it's in full view at
/// scroll offset `from`; false before `e`'s first layout.
fn center_line(
    e: &mut EditorState,
    line: u32,
    from: Option<Point<Pixels>>,
    cx: &mut Context<EditorState>,
) -> bool {
    let Some(lh) = e.line_height() else {
        return false;
    };
    let height = e.input_bounds().size.height;
    // Wrapped and folded lines make the line's row differ from its number.
    let y = lh * e.display_map().buffer_line_to_display_row(line as usize) as f32;
    if from.is_some_and(|v| -v.y <= y && y + lh <= height - v.y) {
        return true;
    }
    let x = e.scroll_offset().x;
    e.set_scroll_offset(point(x, -(y - (height - lh) / 2.).max(px(0.))), cx);
    true
}

/// Windows' "major.minor.build", as About shows it. RtlGetVersion, since
/// GetVersionEx reports 6.2 to apps without a compatibility manifest.
fn os_version() -> String {
    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
    }
    let mut info = OsVersionInfoW {
        size: size_of::<OsVersionInfoW>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        csd: [0; 128],
    };
    // SAFETY: `info` is a valid OSVERSIONINFOW with its size filled in.
    unsafe { RtlGetVersion(&mut info) };
    format!("{}.{}.{}", info.major, info.minor, info.build)
}

fn search_box(state: &Entity<InputState>) -> impl IntoElement + use<> {
    div()
        .rounded(px(4.))
        .border_1()
        .border_color(col::input_border())
        .bg(col::input())
        .child(Input::new(state).appearance(false).text_size(px(13.)))
}

fn primary_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    ch: Option<char>,
) -> Stateful<Div> {
    h_flex()
        .id(id)
        .w_full()
        .h(px(26.))
        .justify_center()
        .gap(px(4.))
        .rounded(px(4.))
        .bg(crate::theme::c(0x297AA0))
        .hover(|s| s.bg(crate::theme::c(0x2B7DA3)))
        .text_color(white())
        .text_size(px(13.))
        .cursor_pointer()
        .when_some(ch, |d, ch| d.child(codicon(ch)))
        .child(label.into())
}

/// Empty editor group: the shortcut hints VS Code shows instead of a document.
fn watermark() -> impl IntoElement {
    let row = |label: &'static str, keys: &'static [&'static str]| {
        h_flex()
            .gap(px(10.))
            .child(div().w(px(160.)).text_right().child(label))
            .child(h_flex().gap(px(3.)).children(keys.iter().map(|&k| {
                if k == "+" {
                    return div().child(k);
                }
                div()
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(crate::theme::ca(0x33333399))
                    .bg(crate::theme::ca(0x8080802B))
                    .text_size(px(11.))
                    .child(k)
            })))
    };
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .bg(col::editor())
        .text_color(col::fg_muted())
        .child(crate::theme::logo(160.).mb(px(20.)).opacity(0.05))
        .child(row("Show All Commands", &["Ctrl", "+", "Shift", "+", "P"]))
        .child(row("Go to File", &["Ctrl", "+", "P"]))
        .child(row("Find in Files", &["Ctrl", "+", "Shift", "+", "F"]))
        .child(row("Toggle Terminal", &["Ctrl", "+", "`"]))
        .child(row("Open Folder", &["Ctrl", "+", "K", "Ctrl", "+", "O"]))
}

/// File > Open Recent's entries, and the workbench they open in.
#[derive(Clone)]
struct Recent {
    folders: Vec<String>,
    files: Vec<String>,
    workbench: WeakEntity<Workbench>,
}

/// VS Code's application menus, minus anything AI.
/// Menu bar menu `name`; `wrap` checks View > Word Wrap, which is greyed
/// out (`None`) with no editor open.
fn app_menu(
    name: &str,
    wrap: Option<bool>,
    recent: Recent,
    menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let noop = || Box::new(Noop) as Box<dyn Action>;
    match name {
        "File" => menu
            .menu("New Text File", Box::new(NewTextFile))
            .menu("New Window", noop())
            .separator()
            .menu("Open File...", Box::new(OpenFile))
            .map(|m| chord(m, "Open Folder...", OpenFolder))
            .submenu("Open Recent", window, cx, move |mut m, _, _| {
                // VS Code's menu shows 10 of each; gone ones are left out.
                // ponytail: no Reopen Closed Editor
                for (folder, list) in [(true, &recent.folders), (false, &recent.files)] {
                    let paths = list.iter().map(PathBuf::from).filter(|p| p.exists());
                    for path in paths.take(10) {
                        let wb = recent.workbench.clone();
                        // A long path ends in "…" inside the menu's 500px.
                        let label = SharedString::from(path.to_string_lossy().into_owned());
                        let item = PopupMenuItem::element(move |_, _| {
                            (div().max_w(px(460.)).overflow_hidden())
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(label.clone())
                        });
                        m = m.item(item.on_click(move |_, window, cx| {
                            let path = path.clone();
                            wb.update(cx, |wb, cx| match folder {
                                true => wb.set_root(path, cx),
                                false => wb.open_recent(path, window, cx),
                            })
                            .ok();
                        }));
                    }
                    m = m.separator();
                }
                (m.menu("More...", Box::new(Noop)).separator())
                    .menu("Clear Recently Opened...", Box::new(ClearRecent))
            })
            .separator()
            .menu("Save", Box::new(Save))
            .menu("Save As...", Box::new(SaveAs))
            .map(|m| chord(m, "Save All", SaveAll))
            .separator()
            .menu_with_check("Auto Save", auto_save(), Box::new(ToggleAutoSave))
            .submenu("Preferences", window, cx, |m, _, _| {
                m.menu("Settings", Box::new(OpenSettings))
                    .menu("Keyboard Shortcuts", Box::new(Noop))
                    .menu("Themes", Box::new(Noop))
            })
            .separator()
            .menu("Revert File", Box::new(RevertFile))
            .menu("Close Editor", Box::new(CloseEditor))
            .menu("Close Folder", Box::new(CloseFolder))
            .menu("Close Window", Box::new(CloseWindow))
            .separator()
            .menu("Exit", Box::new(Quit)),
        "Edit" => menu
            .menu("Undo", Box::new(input::Undo))
            .menu("Redo", Box::new(input::Redo))
            .separator()
            .menu("Cut", Box::new(input::Cut))
            .menu("Copy", Box::new(input::Copy))
            .menu("Paste", Box::new(input::Paste))
            .separator()
            .menu("Find", Box::new(input::Search))
            .menu("Replace", Box::new(input::Replace))
            .separator()
            .menu("Find in Files", Box::new(ShowSearch))
            .menu("Replace in Files", Box::new(ReplaceInFiles))
            .separator()
            .menu("Toggle Line Comment", Box::new(ToggleLineComment))
            .menu("Toggle Block Comment", Box::new(ToggleBlockComment)),
        "Selection" => menu
            .menu("Select All", Box::new(input::SelectAll))
            .menu("Expand Selection", Box::new(ExpandSelection))
            .menu("Shrink Selection", Box::new(ShrinkSelection))
            .separator()
            .menu("Copy Line Up", Box::new(CopyLinesUp))
            .menu("Copy Line Down", Box::new(CopyLinesDown))
            .menu("Move Line Up", Box::new(MoveLinesUp))
            .menu("Move Line Down", Box::new(MoveLinesDown))
            .separator()
            .menu(
                "Add Cursor Above",
                Box::new(gpui_kit::base::input::AddCursorAbove),
            )
            .menu(
                "Add Cursor Below",
                Box::new(gpui_kit::base::input::AddCursorBelow),
            )
            .menu("Add Next Occurrence", Box::new(AddNextOccurrence))
            .menu("Add Previous Occurrence", Box::new(AddPreviousOccurrence))
            .menu("Select All Occurrences", Box::new(SelectAllOccurrences)),
        "View" => menu
            .menu("Command Palette...", Box::new(CommandPalette))
            .menu("Open View...", Box::new(OpenView))
            .separator()
            .submenu("Appearance", window, cx, |m, _, _| {
                m.menu("Primary Side Bar", Box::new(ToggleSidebar))
                    .menu("Panel", Box::new(TogglePanel))
            })
            .submenu("Editor Layout", window, cx, |m, _, _| {
                m.menu("Split Up", Box::new(Noop))
                    .menu("Split Down", Box::new(Noop))
            })
            .separator()
            .menu("Explorer", Box::new(ShowExplorer))
            .menu("Search", Box::new(ShowSearch))
            .menu("Source Control", Box::new(ShowScm))
            .menu("Run", Box::new(ShowRun))
            .menu("Extensions", Box::new(ShowExtensions))
            .separator()
            .menu("Problems", Box::new(ToggleProblems))
            .menu("Output", Box::new(ToggleOutput))
            .menu("Debug Console", Box::new(ToggleDebugConsole))
            .menu("Terminal", Box::new(ToggleTerminal))
            .separator()
            .menu_with_check_and_disabled(
                "Word Wrap",
                wrap == Some(true),
                Box::new(ToggleWordWrap),
                wrap.is_none(),
            ),
        "Go" => menu
            .menu("Back", Box::new(GoBack))
            .menu("Forward", Box::new(GoForward))
            .map(|m| chord(m, "Last Edit Location", GoToLastEditLocation))
            .separator()
            .submenu("Switch Editor", window, cx, |m, _, _| {
                m.menu("Next Editor", Box::new(NextEditor))
                    .menu("Previous Editor", Box::new(PreviousEditor))
            })
            .separator()
            .menu("Go to File...", Box::new(QuickOpen))
            .menu(
                "Go to Symbol in Workspace...",
                Box::new(GoToSymbolInWorkspace),
            )
            .separator()
            .menu("Go to Symbol in Editor...", Box::new(GoToSymbol))
            .menu("Go to Definition", Box::new(input::GoToDefinition))
            .menu("Go to Line/Column...", Box::new(GoToLine)),
        "Run" => menu
            .menu("Start Debugging", Box::new(StartDebugging))
            .menu("Run Without Debugging", Box::new(RunWithoutDebugging))
            .menu("Stop Debugging", Box::new(StopDebugging))
            .menu("Restart Debugging", Box::new(RestartDebugging))
            .separator()
            .menu("Open Configurations", Box::new(OpenLaunchJson))
            .menu("Add Configuration...", noop())
            .separator()
            .menu("Toggle Breakpoint", Box::new(ToggleBreakpoint)),
        "Terminal" => menu
            .menu("New Terminal", Box::new(NewTerminal))
            .menu("Split Terminal", Box::new(SplitTerminal))
            .separator()
            .menu("Run Task...", noop())
            .menu("Run Build Task...", noop()),
        _ => menu
            .menu("Show All Commands", Box::new(CommandPalette))
            .menu("Documentation", noop())
            .menu("Release Notes", noop())
            .separator()
            .menu("Keyboard Shortcuts Reference", noop())
            .separator()
            .menu("Report Issue", noop())
            .separator()
            .menu("View License", noop())
            .menu("Toggle Developer Tools", noop())
            .separator()
            .menu("About", Box::new(About)),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Encoding, block_toggled, capitalize, comment_toggled, copy_all, copy_target, detect_indent,
        expanded, lines_moved, occurrences, with_eol, word_around,
    };

    #[test]
    fn capitalized() {
        assert_eq!(capitalize("Run and Debug"), "Run And Debug");
        assert_eq!(capitalize("Source Control"), "Source Control");
    }

    #[test]
    fn terminal_keys_reach_the_shell() {
        use crate::gpui::{DummyKeyboardMapper, KeyContext, Keymap, Keystroke};
        let keymap = Keymap::new(super::key_bindings(&DummyKeyboardMapper));
        let stack = |c| {
            [
                KeyContext::parse("Root").unwrap(),
                KeyContext::parse(c).unwrap(),
            ]
        };
        let action = |k: &str, c| {
            let (b, _) = keymap.bindings_for_input(&[Keystroke::parse(k).unwrap()], &stack(c));
            b.first()
                .map(|b| b.action().name().rsplit("::").next().unwrap().to_string())
        };
        let term = "Terminal terminalFocus";
        for k in [
            "ctrl-s",
            "ctrl-b",
            "ctrl-w",
            "ctrl-,",
            "tab",
            "shift-tab",
            "ctrl-g",
        ] {
            assert_eq!(action(k, term), None, "{k}");
        }
        assert_eq!(action("ctrl-j", term).as_deref(), Some("TogglePanel"));
        assert_eq!(action("ctrl-p", term).as_deref(), Some("QuickOpen"));
        assert_eq!(action("f5", term).as_deref(), Some("StartDebugging"));
        assert_eq!(action("ctrl-f", term).as_deref(), Some("FindInTerminal"));
        assert_eq!(action("alt-f4", term).as_deref(), Some("CloseWindow"));
        assert_eq!(action("alt-left", term), None);
        let split = "Terminal terminalFocus terminalSplitPaneActive";
        assert_eq!(
            action("alt-up", split).as_deref(),
            Some("FocusPreviousTerminal")
        );
        assert_eq!(action("ctrl-s", "Workbench").as_deref(), Some("Save"));
    }

    #[test]
    fn expand_selection() {
        let text = "f(a, [bc d])\nx";
        let steps = |mut sel| {
            let mut out = vec![];
            while let Some(next) = expanded(text, sel) {
                out.push(&text[next.clone()]);
                sel = next;
            }
            out
        };
        assert_eq!(
            steps(7..7),
            [
                "bc",
                "bc d",
                "[bc d]",
                "a, [bc d]",
                "(a, [bc d])",
                "f(a, [bc d])",
                text
            ]
        );
        // A CRLF line stops before its \r; an open bracket inside the
        // selection doesn't pair with the enclosing closer.
        let text = "{\r\nif (1) {\r\nx;\r\n}\r\n}";
        assert_eq!(expanded(text, 3..11), Some(1..text.len() - 1));
        assert_eq!(expanded(" ab\r\nc", 1..3), Some(0..3));
    }

    #[test]
    fn find_occurrences() {
        let starts =
            |found: Vec<std::ops::Range<usize>>| found.iter().map(|r| r.start).collect::<Vec<_>>();
        assert_eq!(
            starts(occurrences("a ab a_ A a", "a", true, false)),
            [0, 10]
        );
        assert_eq!(
            starts(occurrences("a ab a_ A a", "a", false, false)),
            [0, 2, 5, 8, 10]
        );
        // Folding past ASCII, and a fold that changes a match's length.
        assert_eq!(occurrences("Été été", "été", false, false), [0..5, 6..11]);
        assert_eq!(
            occurrences("\u{212A}m km", "km", false, false),
            [0..4, 5..7]
        );
        assert_eq!(occurrences("Été été", "été", false, true), [6..11]);
    }

    #[test]
    fn word_around_cursor() {
        assert_eq!(word_around("ab cd_é.", 4), Some(3..8));
        assert_eq!(word_around("ab cd", 2), Some(0..2));
        assert_eq!(word_around("ab cd", 3), Some(3..5));
        assert_eq!(word_around("a  b", 2), None);
    }

    #[test]
    fn toggle_block_comment() {
        let toggle = |text: &str, sel| {
            let (range, new, sel) = block_toggled(text, sel, ("/*", "*/")).unwrap();
            let mut out = text.to_string();
            out.replace_range(range, &new);
            (out, sel)
        };
        // Wraps, keeps the selection on the text, and unwraps around it.
        let (wrapped, sel) = toggle("a bc d", 2..4);
        assert_eq!((wrapped.as_str(), sel.clone()), ("a /* bc */ d", 5..7));
        assert_eq!(toggle(&wrapped, sel), ("a bc d".into(), 2..4));
        // Unwraps a selected comment.
        assert_eq!(toggle("a /*bc*/ d", 2..8), ("a bc d".into(), 2..4));
        // Empty selection: the cursor goes between the tokens.
        assert_eq!(toggle("x", 1..1), ("x/*  */".into(), 4..4));
    }

    #[test]
    fn toggle_line_comment() {
        let toggle = |text: &str, sel| {
            let (range, new, sel) = comment_toggled(text, sel, "//", None)?;
            let mut out = text.to_string();
            out.replace_range(range, &new);
            Some((out, sel))
        };
        let text = "  a\n\n    b\nc";
        // Comments at the least indentation, skipping the blank line.
        let (commented, sel) = toggle(text, 0..9).unwrap();
        assert_eq!(commented, "  // a\n\n  //   b\nc");
        assert_eq!(sel, 0..15);
        assert_eq!(toggle(&commented, sel), Some((text.into(), 0..9)));
        // The cursor stays on its character.
        assert_eq!(toggle("x", 1..1), Some(("// x".into(), 4..4)));
        assert_eq!(toggle("//x", 3..3), Some(("x".into(), 1..1)));
        assert_eq!(toggle("  \n", 0..0), None);
        // Add always adds; Remove skips uncommented lines.
        let force = |text: &str, add| {
            let (range, new, _) = comment_toggled(text, 0..text.len(), "//", Some(add))?;
            let mut out = text.to_string();
            out.replace_range(range, &new);
            Some(out)
        };
        assert_eq!(force("// a", true), Some("// // a".into()));
        assert_eq!(force("// a\nb", false), Some("a\nb".into()));
        assert_eq!(force("a", false), None);
    }

    #[test]
    fn move_and_copy_lines() {
        let text = "a\nbb\nc";
        let edit = |sel, up, copy| {
            let (range, new, sel) = lines_moved(text, sel, up, copy)?;
            let mut out = text.to_string();
            out.replace_range(range, &new);
            Some((out, sel))
        };
        assert_eq!(edit(3..3, false, false), Some(("a\nc\nbb".into(), 5..5)));
        assert_eq!(edit(3..3, true, false), Some(("bb\na\nc".into(), 1..1)));
        assert_eq!(
            edit(0..3, true, true),
            Some(("a\nbb\na\nbb\nc".into(), 0..3))
        );
        assert_eq!(edit(2..2, false, true), Some(("a\nbb\nbb\nc".into(), 5..5)));
        // Ending at line 3's start leaves line 3 out.
        assert_eq!(edit(0..5, false, false), Some(("c\na\nbb".into(), 2..7)));
        assert_eq!(edit(0..0, true, false), None);
        assert_eq!(edit(6..6, false, false), None);
    }

    #[test]
    fn copy_folder() {
        let tmp = std::env::temp_dir().join(format!("vsn-copy-{}", std::process::id()));
        let (from, to) = (tmp.join("from"), tmp.join("to"));
        std::fs::create_dir_all(from.join("sub")).unwrap();
        std::fs::write(from.join("sub/a.txt"), "a").unwrap();
        copy_all(&from, &to).unwrap();
        assert_eq!(std::fs::read_to_string(to.join("sub/a.txt")).unwrap(), "a");
        let name = |p: std::path::PathBuf| p.file_name().unwrap().to_string_lossy().into_owned();
        let sub = from.join("sub");
        assert_eq!(name(copy_target(&sub, "b.txt".as_ref())), "b.txt");
        assert_eq!(name(copy_target(&sub, "a.txt".as_ref())), "a copy.txt");
        std::fs::write(sub.join("a copy.txt"), "").unwrap();
        assert_eq!(name(copy_target(&sub, "a.txt".as_ref())), "a copy 2.txt");
        assert_eq!(name(copy_target(&from, "sub".as_ref())), "sub copy");
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn indent() {
        let guess = |t| (detect_indent(t).tab_size, detect_indent(t).hard_tabs);
        assert_eq!(guess("a\n  b\n    c\n  d\n"), (2, false));
        assert_eq!(guess("fn x() {\n    y\n        z\n}\n"), (4, false));
        assert_eq!(guess("a\n\tb\n\t\tc\n"), (4, true));
        assert_eq!(guess("plain\ntext\n"), (4, false));
    }

    #[test]
    fn eol() {
        assert_eq!(with_eol("a\r\nb\nc", true), "a\r\nb\r\nc");
        assert_eq!(with_eol("a\r\nb\nc", false), "a\nb\nc");
    }

    #[test]
    fn encodings() {
        let utf16le = b"a\0b\0";
        assert_eq!(
            Encoding::decode(utf16le),
            Some(("ab".into(), Encoding::Utf16Le))
        );
        assert_eq!(Encoding::decode(b"\0a\0b").unwrap().1, Encoding::Utf16Be);
        assert_eq!(Encoding::decode(b"\x01\0\0\x02"), None);
        let bom = b"\xEF\xBB\xBFhi";
        assert_eq!(
            Encoding::decode(bom),
            Some(("hi".into(), Encoding::Utf8Bom))
        );
        assert_eq!(Encoding::Utf8Bom.encode("hi"), bom);
        // Reopen with Encoding strips only a BOM of the encoding's own.
        assert_eq!(Encoding::Utf8.decode_as(bom), "hi");
        assert_eq!(Encoding::Utf16Le.decode_as(b"\xFF\xFEa\0b\0"), "ab");
        assert_eq!(
            Encoding::Utf16Be.decode_as(b"\xFF\xFEa\0"),
            "\u{FFFE}\u{6100}"
        );
        assert_eq!(Encoding::decode(b"h\xE9").unwrap().1, Encoding::Utf8);
    }
}

/// VS Code's tooltip for a panel title action, by its icon.
fn action_tip(ch: char) -> &'static str {
    match ch {
        icon::CLEAR_ALL => "Clear All",
        icon::FILTER => "Filter",
        icon::ADD => "New Terminal (Ctrl+Shift+`)",
        icon::SPLIT_HORIZONTAL => "Split Terminal (Ctrl+Shift+5)",
        icon::TRASH => "Kill Terminal",
        _ => "Views and More Actions...",
    }
}

/// A menu item bound to a chord, which the menu's own hint cuts to its first key.
fn chord(menu: PopupMenu, label: &'static str, action: impl Action) -> PopupMenu {
    let row = chord_row(label, Some(action.boxed_clone()));
    menu.menu_element(Box::new(action), row)
}

/// A menu row: the label, and every key of `action`'s binding.
fn chord_row(
    label: &'static str,
    action: Option<Box<dyn Action>>,
) -> impl Fn(&mut Window, &mut App) -> Div + 'static {
    move |window, _| {
        let keys = (action.iter())
            .flat_map(|a| kbds(a.as_ref(), window))
            .map(|k| {
                // As the menu styles its own hint.
                k.p_0().min_w_0().border_0().bg(transparent_white())
            });
        (h_flex().w_full().gap_3().items_center().justify_between())
            .child(label)
            .child(h_flex().gap_1().children(keys))
    }
}

/// An editor tab's right-click menu, acting on that tab as VS Code's does.
fn editor_tab_menu(
    menu: PopupMenu,
    wb: &WeakEntity<Workbench>,
    editor: &Entity<EditorState>,
    last: bool,
    path: &Path,
    lang: &str,
) -> PopupMenu {
    type Act = fn(&mut Workbench, usize, &mut Window, &mut Context<Workbench>);
    let item = |label: &'static str, action: Option<Box<dyn Action>>, act: Act| {
        let (wb, editor) = (wb.clone(), editor.clone());
        PopupMenuItem::element(chord_row(label, action)).on_click(move |_, window, cx| {
            wb.update(cx, |this, cx| {
                if let Some(ix) = this.docs.iter().position(|d| d.editor == editor) {
                    act(this, ix, window, cx)
                }
            })
            .ok();
        })
    };
    let menu = menu
        .item(item(
            "Close",
            Some(Box::new(CloseEditor)),
            |this, ix, w, cx| this.close(ix, w, cx),
        ))
        .item(item("Close Others", None, |this, ix, w, cx| {
            this.close_editors(|i, _| i != ix, w, cx)
        }))
        .item(
            item("Close to the Right", None, |this, ix, w, cx| {
                this.close_editors(|i, _| i > ix, w, cx)
            })
            .disabled(last),
        )
        .item(item(
            "Close Saved",
            Some(Box::new(CloseSavedEditors)),
            |this, _, w, cx| this.remove_where(|d| !d.dirty, w, cx),
        ))
        .item(item(
            "Close All",
            Some(Box::new(CloseAllEditors)),
            |this, _, w, cx| this.close_editors(|_, _| true, w, cx),
        ))
        .separator()
        .item(item(
            "Copy Path",
            Some(Box::new(CopyPath)),
            |this, ix, _, cx| this.copy_path(ix, false, cx),
        ))
        .item(item(
            "Copy Relative Path",
            Some(Box::new(CopyRelativePath)),
            |this, ix, _, cx| this.copy_path(ix, true, cx),
        ))
        .separator()
        .item(item(
            "Reveal in File Explorer",
            Some(Box::new(RevealInFileExplorer)),
            |this, ix, _, _| this.reveal_in_os(ix),
        ))
        .item(item("Reveal in Explorer View", None, |this, ix, w, cx| {
            this.activate(ix, w, cx);
            this.reveal_in_tree(&this.docs[ix].path.clone(), true, cx)
        }))
        .when(lang == "markdown", |m| {
            m.separator().item(item("Open Preview", None, |this, ix, w, cx| {
                this.markdown_preview(Some(this.docs[ix].path.clone()), false, w, cx)
            }))
        });
    let mut focus = KeyContext::default();
    focus.set("resourceLangId", lang);
    exthost::resource_keys(&mut focus, path);
    let items = exthost::menu_items("editor/title/context", &[focus]);
    ext_menu_items(menu, wb, &items, exthost::uri_args(Some(path)))
}

/// An editor tab being dragged: its index, and its name to draw under the mouse.
#[derive(Clone)]
struct DraggedEditor(usize, SharedString);

impl Render for DraggedEditor {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        drag_label(self.1.clone())
    }
}

/// A tab under a drag: tinted, with `tab.dragAndDropBorder` on the side the
/// drop lands.
fn drop_side(s: StyleRefinement, after: bool) -> StyleRefinement {
    let s = s.bg(col::drop()).border_color(col::drag_border());
    match after {
        true => s.border_r(px(2.)),
        false => s.border_l(px(2.)).border_r(px(0.)),
    }
}

/// The name drawn under the mouse while a tab or file is dragged.
fn drag_label(name: SharedString) -> Div {
    div()
        .px(px(8.))
        .py(px(2.))
        .bg(col::selected())
        .text_color(col::fg())
        .child(name)
}

/// `name` in `dir`, or when that's taken VS Code's "simple" incremental
/// name for a copy: `a copy.txt`, `a copy 2.txt`, ...
fn copy_target(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let path = dir.join(name);
    if !path.exists() {
        return path;
    }
    let (stem, ext) = match (path.is_dir(), Path::new(name).extension()) {
        (false, Some(ext)) => (
            Path::new(name)
                .file_stem()
                .unwrap_or(name)
                .to_string_lossy(),
            format!(".{}", ext.to_string_lossy()),
        ),
        _ => (name.to_string_lossy(), String::new()),
    };
    (1..)
        .map(|n| match n {
            1 => dir.join(format!("{stem} copy{ext}")),
            n => dir.join(format!("{stem} copy {n}{ext}")),
        })
        .find(|p| !p.exists())
        .unwrap_or(path)
}

/// Copies a file, or a folder with everything in it.
fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    if !from.is_dir() {
        return fs::copy(from, to).map(drop);
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        copy_all(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

/// A file dragged from the explorer.
#[derive(Clone)]
struct DraggedFile(PathBuf);

impl Render for DraggedFile {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        drag_label(
            self.0
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
                .into(),
        )
    }
}
