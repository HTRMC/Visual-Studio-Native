//! Quick Open (Ctrl+P) and the Command Palette (Ctrl+Shift+P): one popup, and
//! like VS Code a leading `>` switches it to commands, `@` to symbols in the
//! active editor, `#` to symbols in the workspace and `:` to a line
//! (`file:line:col` jumps into a file).

use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::{HashMap, HashSet},
    fs,
    ops::Range,
    path::{MAIN_SEPARATOR, Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

use unicode_segmentation::UnicodeSegmentation as _;

use gpui_kit::base::{
    actions::{Cancel, Confirm},
    input::Rope,
};
use gpui_kit::component::{
    IndexPath, Theme,
    command::{Command, CommandItem, CommandState},
    highlighter::SyntaxHighlighter,
    input::{Position, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle},
};

use super::terminal::{ANSI, COLORS, Profile, profiles, set_default_profile, set_setting};
use super::*;
use crate::theme::{
    LANGUAGES, c, ca, codicons, col, file_icon, icon, language_icon, language_label, popup_radius,
};

/// Workspace files shown at once; the best matches win.
const SHOWN: usize = 50;

// VS Code's fuzzyScorer thresholds: an exact path beats a file-name prefix
// match, which beats any other file-name match, which beats path matches.
const PATH_IDENTITY: i32 = 1 << 18;
const LABEL_PREFIX: i32 = 1 << 17;
const LABEL: i32 = 1 << 16;

// VS Code's history lengths: editors per workspace, commands overall,
// Open Recent's folders and files.
const EDITOR_HISTORY: usize = 200;
const COMMAND_HISTORY: usize = 50;
const RECENTLY_OPENED: usize = 500;

/// Change Icon's grid columns: VS Code's 486px box less 30px, in 36px cells.
const ICON_COLS: usize = 12;

/// What the palette offers: the built-in commands and extensions'.
fn commands(focus: &[KeyContext]) -> Vec<(Cow<'static, str>, Box<dyn Action>)> {
    let ext = super::exthost::palette_commands(focus)
        .into_iter()
        .map(|(label, id)| {
            let action: Box<dyn Action> = Box::new(super::exthost::RunExtensionCommand {
                id,
                path: None,
                when: None,
            });
            (label.into(), action)
        });
    (builtin_commands().into_iter())
        .map(|(label, action)| (label.into(), action))
        .chain(ext)
        .collect()
}

/// VS Code's titles. Only commands that do something.
fn builtin_commands() -> Vec<(&'static str, Box<dyn Action>)> {
    vec![
        ("File: Open File...", Box::new(OpenFile)),
        ("File: Open Folder...", Box::new(OpenFolder)),
        ("File: Clear Recently Opened...", Box::new(ClearRecent)),
        ("View: Reopen Closed Editor", Box::new(ReopenClosedEditor)),
        ("Go to Next Problem in Files (Error, Warning, Info)", Box::new(NextProblem)),
        ("Go to Previous Problem in Files (Error, Warning, Info)", Box::new(PreviousProblem)),
        ("File: Open Recent...", Box::new(OpenRecentPicker)),
        ("View: Toggle Zen Mode", Box::new(ToggleZenMode)),
        ("File: Save", Box::new(Save)),
        ("File: Save As...", Box::new(SaveAs)),
        ("File: Save All", Box::new(SaveAll)),
        ("File: Revert File", Box::new(RevertFile)),
        ("View: Reopen Editor With...", Box::new(ReopenEditorWith)),
        ("File: Toggle Auto Save", Box::new(ToggleAutoSave)),
        ("Workspaces: Close Workspace", Box::new(CloseFolder)),
        ("View: Close Editor", Box::new(CloseEditor)),
        ("View: Close All Editors", Box::new(CloseAllEditors)),
        ("View: Pin Editor", Box::new(PinEditor)),
        ("View: Unpin Editor", Box::new(UnpinEditor)),
        (
            "View: Close Saved Editors in Group",
            Box::new(CloseSavedEditors),
        ),
        (
            "View: Close Other Editors in Group",
            Box::new(CloseOtherEditors),
        ),
        (
            "View: Close Editors to the Right in Group",
            Box::new(CloseEditorsToTheRight),
        ),
        ("File: Copy Path of Active File", Box::new(CopyPath)),
        (
            "File: Copy Relative Path of Active File",
            Box::new(CopyRelativePath),
        ),
        (
            "File: Reveal Active File in File Explorer",
            Box::new(RevealInFileExplorer),
        ),
        (
            "File: Reveal Active File in Explorer View",
            Box::new(RevealInExplorerView),
        ),
        (
            "View: Toggle Primary Side Bar Visibility",
            Box::new(ToggleSidebar),
        ),
        ("View: Toggle Panel Visibility", Box::new(TogglePanel)),
        (
            "View: Toggle Maximized Panel",
            Box::new(ToggleMaximizedPanel),
        ),
        ("View: Toggle Terminal", Box::new(ToggleTerminal)),
        ("Go to Line/Column...", Box::new(GoToLine)),
        ("Go to Symbol in Editor...", Box::new(GoToSymbol)),
        (
            "Go to Symbol in Workspace...",
            Box::new(GoToSymbolInWorkspace),
        ),
        ("View: Open View", Box::new(OpenView)),
        ("Help: About", Box::new(About)),
        (
            "Notifications: Show Notifications",
            Box::new(ShowNotifications),
        ),
        (
            "Notifications: Hide Notifications",
            Box::new(HideNotifications),
        ),
        (
            "Notifications: Clear All Notifications",
            Box::new(ClearAllNotifications),
        ),
        (
            "Notifications: Toggle Do Not Disturb Mode",
            Box::new(ToggleDoNotDisturb),
        ),
        ("Terminal: Create New Terminal", Box::new(NewTerminal)),
        ("Terminal: Split Terminal", Box::new(SplitTerminal)),
        ("Terminal: Rename...", Box::new(RenameTerminal)),
        ("Terminal: Change Color...", Box::new(ChangeTerminalColor)),
        ("Terminal: Change Icon...", Box::new(ChangeTerminalIcon)),
        (
            "Terminal: Kill the Active Terminal Instance",
            Box::new(KillTerminal),
        ),
        ("Terminal: Unsplit Terminal", Box::new(UnsplitTerminal)),
        ("Terminal: Join Terminals", Box::new(JoinTerminals)),
        (
            "Terminal: Focus Next Terminal Group",
            Box::new(FocusNextTerminalGroup),
        ),
        (
            "Terminal: Focus Previous Terminal Group",
            Box::new(FocusPreviousTerminalGroup),
        ),
        (
            "Terminal: Focus Next Terminal in Terminal Group",
            Box::new(FocusNextTerminal),
        ),
        (
            "Terminal: Focus Previous Terminal in Terminal Group",
            Box::new(FocusPreviousTerminal),
        ),
        ("Terminal: Focus Find", Box::new(FindInTerminal)),
        ("Terminal: Find Next", Box::new(FindNextInTerminal)),
        ("Terminal: Find Previous", Box::new(FindPreviousInTerminal)),
        (
            "Terminal: Toggle Size to Content Width",
            Box::new(ToggleSizeToContentWidth),
        ),
        ("Terminal: Scroll Up (Line)", Box::new(ScrollTerminalUp)),
        ("Terminal: Scroll Down (Line)", Box::new(ScrollTerminalDown)),
        ("Terminal: Scroll Up (Page)", Box::new(ScrollTerminalUpPage)),
        (
            "Terminal: Scroll Down (Page)",
            Box::new(ScrollTerminalDownPage),
        ),
        ("Terminal: Scroll to Top", Box::new(ScrollTerminalToTop)),
        (
            "Terminal: Scroll to Bottom",
            Box::new(ScrollTerminalToBottom),
        ),
        (
            "Terminal: Select Default Profile",
            Box::new(SelectDefaultProfile),
        ),
        ("Change End of Line Sequence", Box::new(ChangeEol)),
        ("Change Language Mode", Box::new(ChangeLanguageMode)),
        ("Change File Encoding", Box::new(ChangeEncoding)),
        (INDENT_ACTIONS[0], Box::new(IndentUsingSpaces)),
        (INDENT_ACTIONS[1], Box::new(IndentUsingTabs)),
        (INDENT_ACTIONS[2], Box::new(ChangeTabDisplaySize)),
        (INDENT_ACTIONS[3], Box::new(DetectIndentation)),
        (INDENT_ACTIONS[4], Box::new(IndentationToSpaces)),
        (INDENT_ACTIONS[5], Box::new(IndentationToTabs)),
        (INDENT_ACTIONS[6], Box::new(TrimTrailingWhitespace)),
        ("Format Document", Box::new(FormatDocument)),
        ("Format Selection", Box::new(FormatSelection)),
        ("Rename Symbol", Box::new(RenameSymbol)),
        ("Find All References", Box::new(FindAllReferences)),
        ("Go to Type Definition", Box::new(GoToTypeDefinition)),
        ("Go to Implementations", Box::new(GoToImplementation)),
        ("Go to Declaration", Box::new(GoToDeclaration)),
        ("Calls: Show Call Hierarchy", Box::new(ShowCallHierarchy)),
        ("Trigger Parameter Hints", Box::new(TriggerParameterHints)),
        ("Toggle Line Comment", Box::new(ToggleLineComment)),
        ("Add Line Comment", Box::new(AddLineComment)),
        ("Remove Line Comment", Box::new(RemoveLineComment)),
        ("Toggle Block Comment", Box::new(ToggleBlockComment)),
        ("Copy Line Up", Box::new(CopyLinesUp)),
        ("Copy Line Down", Box::new(CopyLinesDown)),
        ("Move Line Up", Box::new(MoveLinesUp)),
        ("Move Line Down", Box::new(MoveLinesDown)),
        ("Delete Line", Box::new(DeleteLines)),
        ("Insert Line Below", Box::new(InsertLineAfter)),
        ("Insert Line Above", Box::new(InsertLineBefore)),
        ("Expand Line Selection", Box::new(ExpandLineSelection)),
        ("Go to Bracket", Box::new(JumpToBracket)),
        ("Cursor Undo", Box::new(CursorUndo)),
        ("Cursor Redo", Box::new(CursorRedo)),
        ("Fold", Box::new(Fold)),
        ("Unfold", Box::new(Unfold)),
        ("Toggle Fold", Box::new(ToggleFold)),
        ("Fold All", Box::new(FoldAll)),
        ("Unfold All", Box::new(UnfoldAll)),
        ("Fold Recursively", Box::new(FoldRecursively)),
        ("Unfold Recursively", Box::new(UnfoldRecursively)),
        ("Fold Level 1", Box::new(FoldLevel1)),
        ("Fold Level 2", Box::new(FoldLevel2)),
        ("Fold Level 3", Box::new(FoldLevel3)),
        ("Fold Level 4", Box::new(FoldLevel4)),
        ("Fold Level 5", Box::new(FoldLevel5)),
        ("Fold Level 6", Box::new(FoldLevel6)),
        ("Fold Level 7", Box::new(FoldLevel7)),
        ("Change All Occurrences", Box::new(SelectAllOccurrences)),
        (
            "Add Cursor Above",
            Box::new(gpui_kit::base::input::AddCursorAbove),
        ),
        (
            "Add Cursor Below",
            Box::new(gpui_kit::base::input::AddCursorBelow),
        ),
        ("Undo", Box::new(input::Undo)),
        ("Cut", Box::new(gpui_kit::base::input::Cut)),
        ("Copy", Box::new(gpui_kit::base::input::Copy)),
        ("Paste", Box::new(gpui_kit::base::input::Paste)),
        ("Select All", Box::new(gpui_kit::base::input::SelectAll)),
        ("Expand Selection", Box::new(ExpandSelection)),
        ("Shrink Selection", Box::new(ShrinkSelection)),
        (
            "Add Selection To Next Find Match",
            Box::new(AddNextOccurrence),
        ),
        (
            "Add Selection To Previous Find Match",
            Box::new(AddPreviousOccurrence),
        ),
        (
            "Move Last Selection To Next Find Match",
            Box::new(MoveSelectionToNextFindMatch),
        ),
        (
            "Select All Occurrences of Find Match",
            Box::new(SelectAllOccurrences),
        ),
        ("Redo", Box::new(input::Redo)),
        ("View: Show Explorer", Box::new(ShowExplorer)),
        ("View: Toggle Problems", Box::new(ToggleProblems)),
        ("View: Toggle Output", Box::new(ToggleOutput)),
        ("View: Toggle Debug Console", Box::new(ToggleDebugConsole)),
        ("View: Toggle Word Wrap", Box::new(ToggleWordWrap)),
        ("View: Zoom In", Box::new(ZoomIn)),
        ("View: Toggle Minimap", Box::new(ToggleMinimap)),
        ("View: Toggle Sticky Scroll", Box::new(ToggleStickyScroll)),
        ("View: Zoom Out", Box::new(ZoomOut)),
        ("View: Reset Zoom", Box::new(ZoomReset)),
        ("File: Refresh Explorer", Box::new(RefreshExplorer)),
        (
            "View: Collapse Folders in Explorer",
            Box::new(CollapseExplorerFolders),
        ),
        ("View: Show Search", Box::new(ShowSearch)),
        ("View: Show Source Control", Box::new(ShowScm)),
        ("View: Show Run and Debug", Box::new(ShowRun)),
        ("View: Show Extensions", Box::new(ShowExtensions)),
        ("Search: Find in Files", Box::new(ShowSearch)),
        ("Search: Replace in Files", Box::new(ReplaceInFiles)),
        ("Debug: Start Debugging", Box::new(StartDebugging)),
        (
            "Debug: Start Without Debugging",
            Box::new(RunWithoutDebugging),
        ),
        ("Debug: Stop", Box::new(StopDebugging)),
        ("Debug: Restart", Box::new(RestartDebugging)),
        ("Debug: Pause", Box::new(Pause)),
        ("Debug: Step Over", Box::new(StepOver)),
        ("Debug: Step Into", Box::new(StepInto)),
        ("Debug: Step Out", Box::new(StepOut)),
        ("Debug: Toggle Breakpoint", Box::new(ToggleBreakpoint)),
        ("Debug: Open 'launch.json'", Box::new(OpenLaunchJson)),
        ("Preferences: Open User Settings", Box::new(OpenSettings)),
        ("Markdown: Open Preview", Box::new(MarkdownShowPreview)),
        (
            "Markdown: Open Preview to the Side",
            Box::new(MarkdownShowPreviewToSide),
        ),
        ("Markdown: Open Source File", Box::new(MarkdownShowSource)),
        ("Preferences: Color Theme", Box::new(SelectColorTheme)),
        ("Preferences: File Icon Theme", Box::new(SelectIconTheme)),
        (
            "Extensions: Install from VSIX...",
            Box::new(InstallFromVsix),
        ),
        ("Snippets: Insert Snippet", Box::new(InsertSnippet)),
        (
            "Preferences: Open User Settings (JSON)",
            Box::new(OpenSettingsJson),
        ),
        ("File: Exit", Box::new(Quit)),
        ("Close Window", Box::new(CloseWindow)),
    ]
}

/// What the palette's rename input names.
pub(super) enum Rename {
    /// Terminal: Rename, by the terminal's id.
    Terminal(u64),
    /// Rename Symbol, at this position of this file.
    Symbol(PathBuf, Position),
    /// An extension's input box: the host's question and the placeholder.
    Host(u64, SharedString),
}

enum Pick {
    /// A row of a plain pick list (`Palette::items`), matched offsets.
    Item(usize, Vec<usize>),
    /// Path (relative to the root when inside it), with the byte offsets
    /// matched in its file name and in its folder.
    File(String, Vec<usize>, Vec<usize>),
    /// Index into `commands()` and the matched label offsets.
    Command(usize, Vec<usize>),
    /// 0-based line and column in the active editor, and the row's label.
    Line(u32, u32, String),
    /// Index into the defs, with the offsets matched in its name and container.
    Symbol(usize, Vec<usize>, Vec<usize>),
    /// Index into the workspace's defs, with the offsets matched in its name.
    WorkspaceSymbol(usize, Vec<usize>),
    /// Index into the profiles of Select Default Profile, and the matched offsets.
    Profile(usize, Vec<usize>),
    /// Index into the terminal colors, or None to reset, and the matched offsets.
    Color(Option<usize>, Vec<usize>),
    /// A row of Change Icon's grid: codicon names, glyphs and matched offsets.
    Icons(Vec<(&'static str, char, Vec<usize>)>),
    /// A terminal for Join Terminals: id, name, tab icon and color, matched offsets.
    Join(u64, String, char, Hsla, Vec<usize>),
    /// CRLF (or LF) for Change End of Line Sequence, and the matched offsets.
    Eol(bool, Vec<usize>),
    /// A language id for Change Language Mode, its "Name (id)" row, matched offsets.
    Lang(&'static str, String, Vec<usize>),
    /// Change Language Mode's "Configure File Association for '.ext'..." row,
    /// and the matched offsets.
    Associate(String, Vec<usize>),
    /// Select Indentation's action (an index into `INDENT_ACTIONS`) or tab
    /// size, and the matched offsets.
    Indent(usize, Vec<usize>),
    /// Change File Encoding's action: Reopen (0) or Save (1) with Encoding.
    EncodingAction(usize, Vec<usize>),
    /// An encoding to reopen or save with, its description, matched offsets.
    Encoding(Encoding, &'static str, Vec<usize>),
    /// Index into `VIEWS`, and the matched offsets.
    View(usize, Vec<usize>),
    /// An open terminal's id, its "group.index: name" label, matched offsets.
    TermView(u64, String, Vec<usize>),
    /// An Output channel's name, matched offsets.
    OutputView(String, Vec<usize>),
}

/// Open View's views: the side bar's, then the panel's in `PANEL_TABS` order.
// ponytail: whole containers; VS Code lists their sub-views too (Open
// Editors, Outline, Timeline).
const VIEWS: [&str; 10] = [
    "Explorer",
    "Search",
    "Source Control",
    "Run and Debug",
    "Extensions",
    "Problems",
    "Output",
    "Debug Console",
    "Terminal",
    "Ports",
];
const SIDE_BAR_VIEWS: usize = 5;

/// VS Code's Select Indentation actions: "change view" ones, then "convert file" ones.
const INDENT_ACTIONS: [&str; 7] = [
    "Indent Using Spaces",
    "Indent Using Tabs",
    "Change Tab Display Size",
    "Detect Indentation from Content",
    "Convert Indentation to Spaces",
    "Convert Indentation to Tabs",
    "Trim Trailing Whitespace",
];

const ENCODING_ACTIONS: [&str; 2] = ["Reopen with Encoding", "Save with Encoding"];

/// Which list Change File Encoding shows.
#[derive(Clone, Copy, PartialEq)]
enum EncodingStep {
    Action,
    /// The encodings to reopen (or save) with, and the one the file's
    /// bytes look like.
    List {
        reopen: bool,
        guessed: Option<Encoding>,
    },
}

/// Which list Select Indentation shows.
#[derive(Clone, Copy, PartialEq)]
enum IndentStep {
    Action,
    /// Select Tab Size for Current File, then indenting with tabs or spaces
    /// (or as before, for Change Tab Display Size).
    Size(Option<bool>),
}

/// A definition in the active editor, for `@`.
#[derive(Debug, PartialEq)]
struct Def {
    name: String,
    /// VS Code's plural kind label, which also names its `@:` group.
    kind: &'static str,
    line: u32,
    col: u32,
    /// The whole definition in bytes, highlighted while previewed.
    range: Range<usize>,
    /// The definition this one is nested in.
    container: Option<usize>,
}

/// Group headers: the row they start at and their label.
type Groups = Vec<(usize, SharedString)>;

/// A plain pick list: rows of a label and a description, in groups.
pub(super) struct Items {
    pub picker: Picker,
    pub rows: Vec<(String, String)>,
    pub groups: Groups,
    pub placeholder: SharedString,
    /// The row highlighted when the list opens.
    pub current: usize,
}

/// What a plain pick list's rows are.
pub(super) enum Picker {
    /// `workbench.colorTheme` names.
    Theme(Vec<String>),
    /// `workbench.iconTheme` ids, None for no icons.
    IconTheme(Vec<Option<String>>),
    Snippet(Vec<extensions::Snippet>),
    /// Find All References' places.
    Location(Vec<(PathBuf, Position)>),
    /// An extension's quick pick, by the host's question.
    Host(u64),
    /// Reopen Editor With...: a file's custom editors, None for text.
    OpenWith(PathBuf, Vec<Option<String>>),
    /// File: Open Recent...: paths, true for a folder.
    Recent(Vec<(PathBuf, bool)>),
}

pub(super) struct Palette {
    state: Entity<CommandState>,
    open: bool,
    query: String,
    /// Workspace files relative to the root, walked again on the next Quick Open
    /// after the watcher saw something change (`stale`).
    files: Arc<Vec<String>>,
    stale: bool,
    /// Absolute paths of editors in the order they were last active, closed
    /// ones included, per workspace root (see `root_key`).
    editors: HashMap<String, Vec<String>>,
    /// Command labels in the order they were last run.
    recent: Vec<String>,
    /// File > Open Recent: workspace folders, and files opened from outside
    /// the workspace (Open File..., a drop, the command line), latest first.
    pub(super) folders: Vec<String>,
    pub(super) opened: Vec<String>,
    /// Symbols of the active editor, parsed on the first `@` query.
    defs: Option<Vec<Def>>,
    /// Symbols of every workspace file with its path, parsed on the first
    /// `#` query.
    ws_defs: Option<Arc<Vec<(String, Def)>>>,
    /// The `#` query last sent to the language servers, and their answer
    /// on its way; `ws_defs` then holds what they found.
    ws_ask: Option<(String, Task<()>)>,
    /// Set while the palette is Select Default Profile's pick list.
    profiles: Option<Vec<Profile>>,
    /// Set while the palette is a rename's input.
    renaming: Option<Rename>,
    /// Set while the palette is Terminal: Change Color's pick list.
    coloring: Option<u64>,
    /// Set while the palette is Terminal: Change Icon's pick list.
    iconing: Option<u64>,
    /// Set while the palette is Terminal: Join Terminals' pick list.
    joining: bool,
    /// Set while the palette is Change End of Line Sequence's pick list.
    eol: bool,
    /// Set while the palette is Change Language Mode's pick list.
    lang: bool,
    /// Set while the palette is a plain pick list.
    items: Option<Items>,
    /// Set while Change Language Mode's list picks the language to associate
    /// with: the `files.associations` key and the file's language by name.
    associating: Option<(String, &'static str)>,
    /// Set while the palette is one of Select Indentation's pick lists.
    indent: Option<IndentStep>,
    /// Set while the palette is one of Change File Encoding's pick lists.
    encoding: Option<EncodingStep>,
    /// 0-based line and column from a `file:line:col` query.
    goto: Option<(u32, u32)>,
    shown: Vec<Pick>,
    groups: Groups,
    /// The highlighted icon in Change Icon's highlighted grid row.
    col: usize,
    /// The highlighted row, and the one to highlight once the list shows `shown`.
    selected: usize,
    preselect: Option<usize>,
    empty: SharedString,
    /// The editor's scroll position before `:` or `@` previewed a line.
    view: Option<Point<Pixels>>,
    deco: Option<RangeDecorationCollection>,
    /// Where focus goes back to when the palette closes without moving it.
    restore: Option<FocusHandle>,
    /// Its key contexts, for extension commands' `when` clauses.
    focus_path: Vec<KeyContext>,
    focus: FocusHandle,
    walk: Option<Task<()>>,
    rank: Option<Task<()>>,
    /// Parses the active editor for `@` off the UI thread.
    parse: Option<Task<()>>,
    /// Do Not Disturb: warnings only reach the center, not a toast. Kept
    /// here to be saved with the histories.
    pub(super) dnd: bool,
}

impl Palette {
    pub(super) fn new(window: &mut Window, cx: &mut App) -> Self {
        let state: serde_json::Value = state_path()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let mut editors: HashMap<String, Vec<String>> =
            serde_json::from_value(state["editors"].clone()).unwrap_or_default();
        // VS Code drops deleted files from the history too.
        for paths in editors.values_mut() {
            paths.retain(|p| Path::new(p).exists());
        }
        Self {
            state: cx.new(|cx| CommandState::new(window, cx)),
            open: false,
            query: String::new(),
            files: Arc::default(),
            stale: true,
            editors,
            recent: serde_json::from_value(state["commands"].clone()).unwrap_or_default(),
            folders: serde_json::from_value(state["folders"].clone()).unwrap_or_default(),
            opened: serde_json::from_value(state["files"].clone()).unwrap_or_default(),
            dnd: state["doNotDisturb"].as_bool().unwrap_or_default(),
            defs: None,
            ws_defs: None,
            ws_ask: None,
            profiles: None,
            renaming: None,
            coloring: None,
            iconing: None,
            joining: false,
            items: None,
            eol: false,
            lang: false,
            associating: None,
            indent: None,
            encoding: None,
            goto: None,
            shown: Vec::new(),
            groups: Vec::new(),
            col: 0,
            selected: 0,
            preselect: None,
            empty: SharedString::default(),
            view: None,
            deco: None,
            restore: None,
            focus_path: Vec::new(),
            focus: cx.focus_handle(),
            walk: None,
            rank: None,
            parse: None,
        }
    }

    /// The workspace's files changed, or with `clear` the workspace itself did.
    pub(super) fn files_changed(&mut self, clear: bool) {
        self.stale = true;
        if clear {
            self.files = Arc::default();
            self.walk = None;
        }
    }

    /// Moves `path` to the front of the editor history of the workspace at `root`.
    pub(super) fn remember(&mut self, root: Option<&Path>, path: &Path, cx: &App) {
        let history = self.editors.entry(root_key(root)).or_default();
        let path = path.to_string_lossy().into_owned();
        if history.first() != Some(&path) {
            history.retain(|p| *p != path);
            history.insert(0, path);
            history.truncate(EDITOR_HISTORY);
            self.save(cx);
        }
    }

    /// Moves `path` to the front of Open Recent's folders, or else its files.
    pub(super) fn remember_opened(&mut self, folder: bool, path: &Path, cx: &App) {
        let list = if folder {
            &mut self.folders
        } else {
            &mut self.opened
        };
        let path = path.to_string_lossy().into_owned();
        if list.first() != Some(&path) {
            list.retain(|p| *p != path);
            list.insert(0, path);
            list.truncate(RECENTLY_OPENED);
            self.save(cx);
        }
    }

    /// Drops `path` from Open Recent.
    pub(super) fn forget_opened(&mut self, path: &Path, cx: &App) {
        let path = path.to_string_lossy();
        self.folders.retain(|p| *p != path);
        self.opened.retain(|p| *p != path);
        self.save(cx);
    }

    /// Clear Recently Opened: Open Recent's lists and, as in VS Code, Quick
    /// Open's editor history.
    pub(super) fn clear_opened(&mut self, cx: &App) {
        self.folders.clear();
        self.opened.clear();
        self.editors.clear();
        self.save(cx);
    }

    /// Writes the histories on a background thread. Writes can finish out of
    /// order, so each carries a generation and an older one never replaces a
    /// newer file; the temp file and rename keep a crash from leaving half a file.
    pub(super) fn save(&self, cx: &App) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        static WRITTEN: Mutex<u64> = Mutex::new(0);
        let Some(path) = state_path() else {
            return;
        };
        let generation = NEXT.fetch_add(1, Relaxed) + 1;
        let state = serde_json::json!({
            "commands": self.recent,
            "editors": self.editors,
            "doNotDisturb": self.dnd,
            "folders": self.folders,
            "files": self.opened,
        });
        cx.background_spawn(async move {
            let mut written = WRITTEN.lock().unwrap();
            if generation < *written {
                return;
            }
            let tmp = path.with_extension("json.tmp");
            fs::create_dir_all(path.parent().unwrap()).ok();
            if fs::write(&tmp, state.to_string()).is_ok() && fs::rename(&tmp, &path).is_ok() {
                *written = generation;
            }
        })
        .detach();
    }

    fn show(&mut self, shown: Vec<Pick>, groups: Groups, select: usize) {
        // A query can filter the preselected row out.
        let select = if select < shown.len() { select } else { 0 };
        self.shown = shown;
        self.groups = groups;
        self.selected = select;
        self.preselect = Some(select);
    }
}

/// The status bar's and Change End of Line Sequence's name for an ending.
pub(super) fn eol(crlf: bool) -> &'static str {
    if crlf { "CRLF" } else { "LF" }
}

/// VS Code's Convert Indentation to Spaces/Tabs: every line's leading
/// whitespace again at the same width, as spaces, or as tabs then spaces.
fn reindent(text: &str, tab: usize, hard_tabs: bool) -> String {
    (text.split_inclusive('\n'))
        .map(|line| {
            let body = line.trim_start_matches([' ', '\t']);
            let width =
                (line[..line.len() - body.len()].chars()).fold(0, |v, c| next_visible(v, c, tab));
            match hard_tabs {
                true => "\t".repeat(width / tab) + &" ".repeat(width % tab) + body,
                false => " ".repeat(width) + body,
            }
        })
        .collect()
}

/// VS Code's Trim Trailing Whitespace: spaces and tabs off every line's end.
fn trim_trailing(text: &str) -> String {
    (text.split_inclusive('\n'))
        .map(|line| {
            let content = line.trim_end_matches(['\r', '\n']);
            content.trim_end_matches([' ', '\t']).to_string() + &line[content.len()..]
        })
        .collect()
}

/// Where the histories and Do Not Disturb survive restarts: VS Code's user data folder layout
/// (`%APPDATA%`, `~/Library/Application Support`, `~/.config`).
fn state_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("vsn").join("state.json"))
}

/// The editor history's key: the workspace root, or `""` without a folder.
fn root_key(root: Option<&Path>) -> String {
    root.map_or_else(String::new, |r| r.to_string_lossy().into_owned())
}

/// VS Code's `scoreFuzzy`: the best alignment of `query` in `target`, ignoring
/// case, rewarding runs, word starts and exact case. Without `allow_gaps` the
/// query must start where it occurs as a whole. Returns the score and the
/// matched byte offsets.
fn score_fuzzy(target: &str, query: &str, allow_gaps: bool) -> Option<(i32, Vec<usize>)> {
    let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
    let t: Vec<(usize, char)> = target.char_indices().collect();
    let q: Vec<char> = query.chars().collect();
    let (n, m) = (q.len(), t.len());
    if n == 0 || m < n {
        return None;
    }
    let tl: Vec<char> = t.iter().map(|&(_, c)| lower(c)).collect();
    let ql: Vec<char> = q.iter().copied().map(lower).collect();
    let is_sep = |c: char| matches!(c, '/' | '\\');

    let char_score = |qi: usize, ti: usize, run: i32| {
        if ql[qi] != tl[ti] && !(is_sep(ql[qi]) && is_sep(tl[ti])) {
            return 0;
        }
        let mut score = 1 + run.min(3) * 6 + (run - 3).max(0) * 3;
        if q[qi] == t[ti].1 {
            score += 1;
        }
        score
            + match ti.checked_sub(1).map(|i| t[i].1) {
                None => 8,
                Some('/' | '\\') => 5,
                Some('_' | '-' | '.' | ' ' | '\'' | '"' | ':') => 4,
                _ if run == 0 && t[ti].1.is_ascii_uppercase() => 2,
                _ => 0,
            }
    };

    // scores[q][t]: best score of query[..=q] in target[..=t]; runs[q][t]: the
    // run length when query[q] matches target[t] in that alignment, else 0.
    let mut scores = vec![0; n * m];
    let mut runs = vec![0; n * m];
    for qi in 0..n {
        for ti in 0..m {
            let ix = qi * m + ti;
            let left = if ti > 0 { scores[ix - 1] } else { 0 };
            let (diag, run) = if qi > 0 && ti > 0 {
                (scores[ix - m - 1], runs[ix - m - 1])
            } else {
                (0, 0)
            };
            let score = if diag == 0 && qi > 0 {
                0
            } else {
                char_score(qi, ti, run)
            };
            if score > 0
                && diag + score >= left
                && (allow_gaps || qi > 0 || tl[ti..].starts_with(&ql))
            {
                runs[ix] = run + 1;
                scores[ix] = diag + score;
            } else {
                scores[ix] = left;
            }
        }
    }

    let mut hits = Vec::with_capacity(n);
    let (mut qi, mut ti) = (n as isize - 1, m as isize - 1);
    while qi >= 0 && ti >= 0 {
        if runs[qi as usize * m + ti as usize] > 0 {
            hits.push(t[ti as usize].0);
            qi -= 1;
        }
        ti -= 1;
    }
    hits.reverse();
    let score = scores[n * m - 1];
    (score > 0 && hits.len() == n).then_some((score, hits))
}

/// A Quick Open query, VS Code's `prepareQuery`.
struct Query {
    /// Space-separated pieces that must all match, and whether each was quoted
    /// (which asks for a contiguous match).
    pieces: Vec<(String, bool)>,
    /// The query has a path separator, so it matches folders too.
    sep: bool,
    /// The query with `/` as the platform separator, for exact paths.
    path: String,
}

fn prepare_query(original: &str) -> Query {
    let normalize = |s: &str| {
        let path = if cfg!(windows) {
            s.replace('/', "\\")
        } else {
            s.to_string()
        };
        let mut norm: String = path
            .chars()
            .filter(|&c| !matches!(c, '*' | '…' | '"') && !c.is_whitespace())
            .collect();
        // A trailing `#` is VS Code's go-to-symbol suffix.
        if norm.len() > 1 && norm.ends_with('#') {
            norm.pop();
        }
        (path, norm)
    };
    let quoted = |s: &str| s.len() > 1 && s.starts_with('"') && s.ends_with('"');
    let (path, whole) = normalize(original);
    let mut pieces: Vec<_> = original
        .split(' ')
        .map(|p| (normalize(p).1, quoted(p)))
        .filter(|(p, _)| !p.is_empty())
        .collect();
    if pieces.len() < 2 {
        pieces = (!whole.is_empty())
            .then(|| (whole.clone(), quoted(original)))
            .into_iter()
            .collect();
    }
    Query {
        pieces,
        sep: whole.contains(MAIN_SEPARATOR),
        path,
    }
}

/// A match's score with the byte offsets it matched in the label and description.
#[derive(Debug, Default)]
struct Score {
    score: i32,
    label: Vec<usize>,
    desc: Vec<usize>,
}

/// VS Code's `doScoreItemFuzzySingle`: the label alone (a file name) unless the
/// query has a path separator or the label doesn't match, then the whole path.
fn score_piece(piece: &str, label: &str, desc: &str, sep: bool, gaps: bool) -> Option<Score> {
    if !sep || desc.is_empty() {
        if let Some((score, hits)) = score_fuzzy(label, piece, gaps) {
            if label.to_lowercase().starts_with(&piece.to_lowercase()) {
                let n = piece.chars().count();
                let boost = (n as f32 / label.chars().count() as f32 * 100.).round() as i32;
                return Some(Score {
                    score: LABEL_PREFIX + boost + score,
                    label: label.char_indices().take(n).map(|(i, _)| i).collect(),
                    desc: Vec::new(),
                });
            }
            return Some(Score {
                score: LABEL + score,
                label: hits,
                desc: Vec::new(),
            });
        }
    }
    if desc.is_empty() {
        return None;
    }
    let split = desc.len() + MAIN_SEPARATOR.len_utf8();
    let (score, hits) = score_fuzzy(&format!("{desc}{MAIN_SEPARATOR}{label}"), piece, gaps)?;
    let (desc_hits, label_hits): (Vec<usize>, Vec<usize>) =
        hits.into_iter().partition(|&h| h < split);
    Some(Score {
        score,
        label: label_hits.into_iter().map(|h| h - split).collect(),
        desc: desc_hits,
    })
}

/// VS Code's `scoreItemFuzzy`: every query piece must match; scores add up.
fn score_item(q: &Query, label: &str, desc: &str, identity: bool, gaps: bool) -> Option<Score> {
    if identity {
        let all = |s: &str| s.char_indices().map(|(i, _)| i).collect();
        return Some(Score {
            score: PATH_IDENTITY,
            label: all(label),
            desc: all(desc),
        });
    }
    let mut total = Score::default();
    for (piece, quoted) in &q.pieces {
        let s = score_piece(piece, label, desc, q.sep, gaps && !quoted)?;
        total.score += s.score;
        total.label.extend(s.label);
        total.desc.extend(s.desc);
    }
    for hits in [&mut total.label, &mut total.desc] {
        hits.sort_unstable();
        hits.dedup();
    }
    (!q.pieces.is_empty()).then_some(total)
}

/// An item being ranked: its label (file name), description (folder) and path.
struct Scored<'a> {
    label: &'a str,
    desc: &'a str,
    path: &'a str,
    s: Score,
}

/// VS Code's `compareItemsByFuzzyScore`.
fn compare_scored(a: &Scored, b: &Scored) -> Ordering {
    let (sa, sb) = (a.s.score, b.s.score);
    if (sa == PATH_IDENTITY || sb == PATH_IDENTITY) && sa != sb {
        return sb.cmp(&sa);
    }
    // First to last matched char.
    let span = |hits: &[usize]| Some(hits.last()? + 1 - hits.first()?);
    if sa > LABEL || sb > LABEL {
        if sa != sb {
            return sb.cmp(&sa);
        }
        // Compact label matches first, unless both are prefixes (longer is better there).
        if sa < LABEL_PREFIX && sb < LABEL_PREFIX {
            let ord = match (span(&a.s.label), span(&b.s.label)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            };
            if ord.is_ne() {
                return ord;
            }
        }
        if a.label.len() != b.label.len() {
            return a.label.len().cmp(&b.label.len());
        }
    }
    if sa != sb {
        return sb.cmp(&sa);
    }
    match (a.s.label.is_empty(), b.s.label.is_empty()) {
        (false, true) => return Ordering::Less,
        (true, false) => return Ordering::Greater,
        _ => {}
    }
    // From the first description (else label) match to the last label (else
    // description) match, the description's length counting when both matched.
    let distance = |x: &Scored| -> isize {
        let start = x.s.desc.first().or(x.s.label.first());
        let end = match (x.s.label.last(), x.s.desc.last()) {
            (Some(&l), d) => l + 1 + if d.is_some() { x.desc.len() } else { 0 },
            (None, Some(&d)) => d + 1,
            (None, None) => return 0,
        };
        end as isize - start.map_or(-1, |&s| s as isize)
    };
    let (da, db) = (distance(a), distance(b));
    if da != 0 && db != 0 && da != db {
        return da.cmp(&db);
    }
    let lower = |s: &str| s.to_lowercase();
    (a.label.len() + a.desc.len())
        .cmp(&(b.label.len() + b.desc.len()))
        .then(a.path.len().cmp(&b.path.len()))
        .then_with(|| lower(a.label).cmp(&lower(b.label)))
        .then_with(|| lower(a.desc).cmp(&lower(b.desc)))
        .then_with(|| lower(a.path).cmp(&lower(b.path)))
}

/// A path's file name and folder.
fn parts(rel: &str) -> (&str, &str) {
    match rel.rfind(['/', '\\']) {
        Some(i) => (&rel[i + 1..], &rel[..i]),
        None => (rel, ""),
    }
}

/// VS Code's Quick Open: editor history matching the query ("recently opened"),
/// then the best other workspace files ("file results"). Paths are relative to
/// `root` when inside it. An empty query lists the whole history.
fn rank_files(
    query: &str,
    root: &Path,
    history: &[String],
    files: &[String],
) -> (Vec<Pick>, Groups) {
    let q = prepare_query(query);
    if q.pieces.is_empty() {
        let groups = match history {
            [] => Vec::new(),
            _ => vec![(0, "recently opened".into())],
        };
        let picks = history
            .iter()
            .map(|rel| Pick::File(rel.clone(), Vec::new(), Vec::new()));
        return (picks.collect(), groups);
    }
    // An absolute query naming a file matches it outright.
    let ident = Path::new(&q.path).is_absolute().then(|| {
        let path = Path::new(&q.path);
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    });
    let is_ident = |rel: &str| {
        ident
            .as_deref()
            .is_some_and(|i| i.eq_ignore_ascii_case(rel))
    };

    let mut recent: Vec<_> = history
        .iter()
        .filter_map(|rel| {
            // History matches file names only, unless the query has a separator.
            let (label, desc) = parts(rel);
            let desc = if q.sep { desc } else { "" };
            let s = score_item(&q, label, desc, is_ident(rel), false)?;
            Some(Scored {
                label,
                desc,
                path: rel,
                s,
            })
        })
        .collect();
    recent.sort_by(compare_scored);

    let seen: HashSet<&str> = history.iter().map(String::as_str).collect();
    let mut found: Vec<_> = files
        .iter()
        .filter(|rel| !seen.contains(rel.as_str()))
        .filter_map(|rel| {
            let (label, desc) = parts(rel);
            let s = score_item(&q, label, desc, is_ident(rel), true)?;
            Some(Scored {
                label,
                desc,
                path: rel,
                s,
            })
        })
        .collect();
    if found.len() > SHOWN {
        found.select_nth_unstable_by(SHOWN, compare_scored);
        found.truncate(SHOWN);
    }
    found.sort_by(compare_scored);

    let (mut picks, mut groups) = (Vec::new(), Vec::new());
    for (title, list) in [("recently opened", recent), ("file results", found)] {
        if !list.is_empty() {
            groups.push((picks.len(), title.into()));
        }
        picks.extend(list.into_iter().map(|x| {
            let s = x.s;
            // Label-only history scores have no folder matches to show.
            Pick::File(x.path.to_string(), s.label, s.desc)
        }));
    }
    (picks, groups)
}

/// Splits a 1-based line/column suffix off a file query, returning it 0-based.
/// VS Code's `extractRangeFromFilter` pattern: `:12:5`, `#12`, `(12,5)`, `:line 12`,
/// ranges (only the start is used); `file:` alone means line 1.
fn split_line(query: &str) -> (&str, Option<(u32, u32)>) {
    static SUFFIX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"\s?[#:\(](?:line )?(\d*)(?:[#:,](\d*))?(?:-(\d*)(?:[#:,](\d*))?)?\)?:?\s*$",
        )
        .unwrap()
    });
    let Some(m) = SUFFIX.captures(query) else {
        return (query, None);
    };
    let num = |i| {
        m.get(i)
            .and_then(|n| n.as_str().parse::<u32>().ok())
            .map_or(0, |n| n.saturating_sub(1))
    };
    (&query[..m.get(0).unwrap().start()], Some((num(1), num(2))))
}

/// JavaScript's `parseInt(s, 10)`: leading whitespace, a sign and digits,
/// ignoring whatever follows.
fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim_start();
    let digits = s.strip_prefix(['-', '+']).unwrap_or(s);
    let len = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    let n: i64 = digits[..len].parse().ok()?;
    Some(if s.starts_with('-') { -n } else { n })
}

/// VS Code's `_nextVisibleColumn`: where the grapheme after one starting with
/// `c` shows when it shows at `v`. Tabs reach the next tab stop; CJK and emoji
/// take two columns (VS Code's `isFullWidthCharacter`/`isEmojiImprecise`).
fn next_visible(v: usize, c: char, tab: usize) -> usize {
    let wide = matches!(c as u32,
        0x2E80..=0xD7AF | 0xF900..=0xFAFF | 0xFF01..=0xFF5E | 0xFFE0..=0xFFE6
        | 0x1F1E6..=0x1F1FF | 8986 | 8987 | 9200 | 9203 | 9728..=10175 | 11088 | 11093
        | 127744..=128591 | 128640..=128764 | 128992..=129008 | 129280..=129535
        | 129648..=129782);
    match c {
        '\t' => v / tab * tab + tab,
        _ if wide => v + 2,
        _ => v + 1,
    }
}

/// `line`'s grapheme clusters with their first character.
fn graphemes(line: &str) -> impl Iterator<Item = (&str, char)> {
    line.graphemes(true).map(|g| (g, g.chars().next().unwrap()))
}

/// VS Code's `visibleColumnFromColumn`: how wide the first `chars` characters
/// of `line` show.
fn visible_column(line: &str, chars: usize, tab: usize) -> usize {
    let end = line
        .char_indices()
        .nth(chars)
        .map_or(line.len(), |(i, _)| i);
    graphemes(&line[..end]).fold(0, |v, (_, c)| next_visible(v, c, tab))
}

/// VS Code's `toStatusbarColumn`: the 1-based "Col" for the cursor after
/// `chars` characters, where only tabs widen.
pub(super) fn status_column(line: &str, chars: usize, tab: usize) -> usize {
    let step = |v: usize, c| {
        if c == '\t' {
            next_visible(v, c, tab)
        } else {
            v + 1
        }
    };
    line.chars().take(chars).fold(0, step) + 1
}

/// VS Code's `columnFromVisibleColumn`: the character index showing at
/// `visible`, the nearer side when it falls inside a tab or wide character.
fn column_from_visible(line: &str, visible: usize, tab: usize) -> usize {
    let (mut before, mut col) = (0, 0);
    for (g, c) in graphemes(line) {
        let after = next_visible(before, c, tab);
        let after_col = col + g.chars().count();
        if after >= visible {
            return if after - visible < visible - before {
                after_col
            } else {
                col
            };
        }
        (before, col) = (after, after_col);
    }
    col
}

/// VS Code's Go to Line for `:` queries: `line[:,#col]`, 1-based, negative counts
/// from the end, clamped to the text; `::offset` goes to a character. Columns
/// are visible ones (tabs expanded). Ok is the 0-based line and character and
/// the row label; Err is the hint to show instead.
fn goto_line(query: &str, text: &str, tab: usize) -> Result<(u32, u32, String), String> {
    let query = query.trim();
    if let Some(offset) = query.strip_prefix(':') {
        let max = text.chars().count() as i64;
        let Some(mut offset) = parse_int(offset) else {
            return Err(format!(
                "Type a character position to go to (from 1 to {max})."
            ));
        };
        let reverse = offset < 0;
        offset -= offset.signum();
        if reverse {
            offset += max;
        }
        let before: String = text.chars().take(offset.clamp(0, max) as usize).collect();
        let line = before.matches('\n').count() as u32;
        let prefix = before.rsplit('\n').next().unwrap_or("");
        let col = prefix.chars().count() as u32;
        let label = format!(
            "Press 'Enter' to go to line {} at column {}.",
            line + 1,
            visible_column(prefix, usize::MAX, tab) + 1
        );
        return Ok((line, col, label));
    }

    let lines: Vec<&str> = text.split('\n').collect();
    let n = lines.len() as i64;
    let mut parts = query.split([',', ':', '#']);
    let Some(line) = parts.next().and_then(parse_int) else {
        return Err(format!("Type a line number to go to (from 1 to {n})."));
    };
    let line = if line < 0 { n + 1 + line } else { line }.clamp(1, n);
    let content = lines[line as usize - 1].trim_end_matches('\r');
    let max_col = visible_column(content, usize::MAX, tab) as i64 + 1;
    let (col, label) = match parts.next().map(parse_int) {
        Some(Some(col)) => {
            // Unlike lines, -1 is the last character, not past it.
            let col = if col < 0 { max_col + col } else { col }.clamp(1, max_col);
            (
                col,
                format!("Press 'Enter' to go to line {line} at column {col}."),
            )
        }
        Some(None) => (
            1,
            format!(
                "Press 'Enter' to go to line {line} or enter a column number (from 1 to {max_col})."
            ),
        ),
        None => (
            1,
            format!("Press 'Enter' to go to line {line} or enter colon : to add a column number."),
        ),
    };
    let col = column_from_visible(content, col as usize - 1, tab);
    Ok((line as u32 - 1, col as u32, label))
}

fn is_word_sep(c: char) -> bool {
    c.is_whitespace() || "()[]{}<>`'\"-/;:,.?!".contains(c)
}

/// The first index from `start` that starts or ends a word.
fn next_word(t: &[char], start: usize) -> usize {
    (start..t.len())
        .find(|&i| is_word_sep(t[i]) || i > 0 && is_word_sep(t[i - 1]))
        .unwrap_or(t.len())
}

/// VS Code's `_matchesWords`: `w[wi..]` matched from `t[ti]`, each char
/// continuing the current word or starting a later one. Returns the matched
/// indices last first; `failed` memoizes dead ends.
fn match_words(
    w: &[char],
    t: &[char],
    wi: usize,
    ti: usize,
    failed: &mut HashSet<(usize, usize)>,
) -> Option<Vec<usize>> {
    if wi == w.len() {
        return Some(Vec::new());
    }
    if ti == t.len()
        || failed.contains(&(wi, ti))
        || w[wi] != t[ti] && !(is_word_sep(w[wi]) && is_word_sep(t[ti]))
    {
        return None;
    }
    let mut next = ti + 1;
    let mut found = match_words(w, t, wi + 1, next, failed);
    while found.is_none() {
        next = next_word(t, next);
        if next >= t.len() {
            break;
        }
        found = match_words(w, t, wi + 1, next, failed);
        next += 1;
    }
    let Some(mut found) = found else {
        failed.insert((wi, ti));
        return None;
    };
    // Separators match each other but aren't highlighted.
    if w[wi] == t[ti] {
        found.push(ti);
    }
    Some(found)
}

/// VS Code's command filter: the query as one substring of the label, else as
/// word starts and continuations (`vtt` is View: Toggle Terminal). Returns the
/// matched byte offsets.
fn command_matches(query: &str, label: &str) -> Option<Vec<usize>> {
    let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
    let w: Vec<char> = query.chars().map(lower).collect();
    let t: Vec<char> = label.chars().map(lower).collect();
    let offsets: Vec<usize> = label.char_indices().map(|(i, _)| i).collect();
    if let Some(start) = t.windows(w.len().max(1)).position(|s| s == w) {
        return Some(offsets[start..start + w.len()].to_vec());
    }
    let mut failed = HashSet::new();
    let mut ti = 0;
    while ti < t.len() {
        if let Some(hits) = match_words(&w, &t, 0, ti, &mut failed) {
            return Some(hits.into_iter().rev().map(|i| offsets[i]).collect());
        }
        ti = next_word(&t, ti + 1);
    }
    None
}

/// VS Code's SymbolKind for a tree-sitter definition kind, as the plural label
/// of its `@:` group. Functions in types are methods.
fn symbol_kind(kind: &str, container: Option<&str>) -> &'static str {
    let has = |words: &[&str]| words.iter().any(|w| kind.contains(w));
    if has(&["constructor"]) {
        "constructors"
    } else if has(&["method"]) {
        "methods"
    } else if has(&["function", "macro"]) {
        match container {
            Some("objects" | "classes" | "interfaces" | "structs") => "methods",
            _ => "functions",
        }
    } else if has(&["class"]) {
        "classes"
    } else if has(&["struct", "union"]) {
        "structs"
    } else if has(&["enum"]) {
        "enumerations"
    } else if has(&["interface", "trait", "protocol"]) {
        "interfaces"
    } else if has(&["namespace"]) {
        "namespaces"
    } else if has(&["mod", "package"]) {
        "modules"
    } else if has(&["impl", "object"]) {
        "objects"
    } else if has(&["const"]) {
        "constants"
    } else if has(&["field"]) {
        "fields"
    } else if has(&["type"]) {
        "type parameters"
    } else if has(&["var", "let", "static"]) {
        "variables"
    } else {
        "properties"
    }
}

/// LSP's SymbolKind as the plural label of its `@:` group.
fn lsp_symbol_kind(kind: u64) -> &'static str {
    const KINDS: [&str; 26] = [
        "files",
        "modules",
        "namespaces",
        "packages",
        "classes",
        "methods",
        "properties",
        "fields",
        "constructors",
        "enumerations",
        "interfaces",
        "functions",
        "variables",
        "constants",
        "strings",
        "numbers",
        "booleans",
        "arrays",
        "objects",
        "keys",
        "null",
        "enumeration members",
        "structs",
        "events",
        "operators",
        "type parameters",
    ];
    KINDS
        .get(kind.wrapping_sub(1) as usize)
        .copied()
        .unwrap_or("properties")
}

/// An LSP position in a JSON reply.
fn lsp_position(p: &serde_json::Value) -> lsp_types::Position {
    let n = |k: &str| p[k].as_u64().unwrap_or(0) as u32;
    lsp_types::Position::new(n("line"), n("character"))
}

/// A documentSymbol reply as `@`'s definitions, nested ones after their
/// container; empty when the server sent none.
fn symbol_defs(reply: &serde_json::Value, text: &str) -> Vec<Def> {
    fn walk(list: &serde_json::Value, container: Option<usize>, text: &Rope, out: &mut Vec<Def>) {
        for s in list.as_array().into_iter().flatten() {
            // DocumentSymbol, or the flat SymbolInformation.
            let range = match &s["range"] {
                serde_json::Value::Null => &s["location"]["range"],
                r => r,
            };
            let at = lsp_position(&s["selectionRange"]["start"]);
            let at = if s["selectionRange"].is_null() {
                lsp_position(&range["start"])
            } else {
                at
            };
            let start = text.position_to_offset(&lsp_position(&range["start"]));
            let end = text.position_to_offset(&lsp_position(&range["end"]));
            out.push(Def {
                name: s["name"].as_str().unwrap_or_default().to_string(),
                kind: lsp_symbol_kind(s["kind"].as_u64().unwrap_or(0)),
                line: at.line,
                col: at.character,
                range: start..end.max(start),
                container,
            });
            walk(&s["children"], Some(out.len() - 1), text, out);
        }
    }
    let mut out = Vec::new();
    walk(reply, None, &Rope::from_str(text), &mut out);
    out
}

/// A workspace/symbol reply as `#`'s definitions, by path relative to
/// `root` (or whole, outside it).
fn workspace_symbol_defs(reply: &serde_json::Value, root: &Path) -> Vec<(String, Def)> {
    (reply.as_array().into_iter().flatten())
        .filter_map(|s| {
            let path = super::lsp::uri_path(s["location"]["uri"].as_str()?)?;
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let at = lsp_position(&s["location"]["range"]["start"]);
            let def = Def {
                name: s["name"].as_str().unwrap_or_default().to_string(),
                kind: lsp_symbol_kind(s["kind"].as_u64().unwrap_or(0)),
                line: at.line,
                col: at.character,
                range: 0..0,
                container: None,
            };
            Some((rel.to_string_lossy().replace('\\', "/"), def))
        })
        .collect()
}

/// A kind's codicon and VS Code's dark `symbolIcon.*Foreground`.
fn symbol_icon(kind: &str) -> (char, Hsla) {
    let (ch, color) = match kind {
        "variables" => ('\u{EA88}', Some(0x75BEFF)),
        "modules" | "namespaces" | "objects" => ('\u{EA8B}', None),
        "methods" | "functions" | "constructors" => ('\u{EA8C}', Some(0xB180D7)),
        "structs" => ('\u{EA91}', None),
        "type parameters" => ('\u{EA92}', None),
        "enumerations" => ('\u{EA95}', Some(0xEE9D28)),
        "classes" => ('\u{EB5B}', Some(0xEE9D28)),
        "constants" => ('\u{EB5D}', None),
        "enumeration members" => ('\u{EB5E}', Some(0x75BEFF)),
        "fields" => ('\u{EB5F}', Some(0x75BEFF)),
        "interfaces" => ('\u{EB61}', Some(0x75BEFF)),
        _ => ('\u{EB65}', None),
    };
    (ch, color.map_or_else(col::fg, c))
}

/// The grammar's tags query (`@definition.*` with its `@name`), for languages
/// whose definitions the naming heuristic in `defs` misses. Compiled once per
/// language.
fn tags_query(lang: &str, language: &tree_sitter::Language) -> Option<Arc<tree_sitter::Query>> {
    static CACHE: LazyLock<Mutex<HashMap<String, Option<Arc<tree_sitter::Query>>>>> =
        LazyLock::new(Mutex::default);
    let mut cache = CACHE.lock().unwrap();
    if let Some(query) = cache.get(lang) {
        return query.clone();
    }
    let query = |src: &str| tree_sitter::Query::new(language, src).ok();
    let compiled = match lang {
        "c" => query(tree_sitter_c::TAGS_QUERY),
        "cpp" => query(tree_sitter_cpp::TAGS_QUERY),
        "javascript" => query(tree_sitter_javascript::TAGS_QUERY),
        // TypeScript's own tags only add its extra syntax to JavaScript's.
        "typescript" | "tsx" => query(&format!(
            "{}\n{}",
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY
        ))
        .or_else(|| query(tree_sitter_typescript::TAGS_QUERY)),
        "ruby" => query(tree_sitter_ruby::TAGS_QUERY),
        _ => None,
    }
    .map(Arc::new);
    cache.insert(lang.to_string(), compiled.clone());
    compiled
}

/// Definitions in `text`, VS Code's "Go to Symbol in Editor" without a language
/// server: syntax nodes with a `name` whose kind reads like a definition, and
/// whatever the grammar's tags query calls one.
fn defs(lang: &str, text: &str) -> Vec<Def> {
    use tree_sitter::StreamingIterator as _;
    const KINDS: [&str; 5] = [
        "_item",
        "_definition",
        "_declaration",
        "_spec",
        "_signature",
    ];
    let mut highlighter = SyntaxHighlighter::new(lang);
    highlighter.update(None, &Rope::from_str(text), None);
    let Some(tree) = highlighter.tree() else {
        return Vec::new();
    };
    // Tagged definition nodes by id: their name node and the tag's kind.
    let mut tags = HashMap::new();
    let query = tags_query(lang, &tree.language());
    if let Some(query) = query.as_deref() {
        let names = query.capture_names();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
        while let Some(m) = matches.next() {
            let capture = |name: &str| m.captures.iter().find(|c| names[c.index as usize] == name);
            let def = m
                .captures
                .iter()
                .find_map(|c| Some((c.node, names[c.index as usize].strip_prefix("definition.")?)));
            if let (Some((node, kind)), Some(name)) = (def, capture("name")) {
                tags.insert(node.id(), (name.node, kind));
            }
        }
    }
    let mut out: Vec<Def> = Vec::new();
    // Definitions enclosing the current node.
    let mut open: Vec<usize> = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        let kind = node.kind();
        let field = |f| node.child_by_field_name(f);
        let named = KINDS
            .iter()
            .find(|s| kind.ends_with(**s))
            .and_then(|suffix| {
                // `impl Foo` has no name; like rust-analyzer, list it by its type.
                let name = field("name")
                    .or_else(|| (kind == "impl_item").then(|| field("type")).flatten());
                Some((name?, kind.trim_end_matches(suffix), None))
            });
        let tagged = tags
            .get(&node.id())
            .map(|&(name, tag)| (name, kind, Some(tag)));
        if let Some((name, node_kind, tag)) = tagged.or(named) {
            while open
                .last()
                .is_some_and(|&d| out[d].range.end <= node.start_byte())
            {
                open.pop();
            }
            let container = open.last().copied();
            let in_kind = container.map(|c| out[c].kind);
            let mut def_kind = symbol_kind(node_kind, in_kind);
            if let (Some(tag), "properties") = (tag, def_kind) {
                def_kind = symbol_kind(tag, in_kind);
            }
            // A C declarator's definition is the declaration around it.
            let mut whole = node;
            while whole.kind().ends_with("declarator")
                && let Some(parent) = whole.parent()
            {
                whole = parent;
            }
            let mut label = text[name.byte_range()].to_string();
            if kind == "impl_item" {
                label = match field("trait") {
                    Some(t) => format!("impl {} for {label}", &text[t.byte_range()]),
                    None => format!("impl {label}"),
                };
            }
            let start = name.start_byte();
            let line_start = start - name.start_position().column;
            open.push(out.len());
            out.push(Def {
                name: label,
                kind: def_kind,
                line: name.start_position().row as u32,
                col: text[line_start..start].chars().count() as u32,
                range: whole.byte_range(),
                container,
            });
        }
        // Pre-order walk: down, else across, else up until something is across.
        if cursor.goto_first_child() || cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return out;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// VS Code's `filters.fuzzyScore` as Go to Symbol calls it (a weak first match
/// is fine, a full match gets a boost): `pattern` in `word[word_start..]`,
/// rewarding word starts, camel humps, exact case and runs. Returns the score
/// and the matched character indices, counted from `word_start`.
fn fuzzy_score(pattern: &str, word: &str, word_start: usize) -> Option<(i32, Vec<usize>)> {
    const MAX_LEN: usize = 128;
    let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
    let p: Vec<char> = pattern.chars().take(MAX_LEN).collect();
    let w: Vec<char> = word.chars().take(MAX_LEN).collect();
    let pl: Vec<char> = p.iter().copied().map(lower).collect();
    let wl: Vec<char> = w.iter().copied().map(lower).collect();
    let (plen, wlen) = (p.len(), w.len());
    if plen == 0 || word_start >= wlen || plen > wlen - word_start {
        return None;
    }
    // The first and last word position each pattern char can match at.
    let (mut min, mut max) = (vec![0; plen], vec![0; plen]);
    let mut pi = 0;
    for wi in word_start..wlen {
        if pi < plen && pl[pi] == wl[wi] {
            min[pi] = wi;
            pi += 1;
        }
    }
    if pi < plen {
        return None;
    }
    for wi in (word_start..wlen).rev() {
        if pi > 0 && pl[pi - 1] == wl[wi] {
            pi -= 1;
            max[pi] = wi;
        }
    }

    // Out-of-range positions (`wi - 1` at 0 wraps) are neither.
    let upper = |i: usize| w.get(i) != wl.get(i);
    let sep = |i: usize| {
        wl.get(i)
            .is_some_and(|c| "_-. /\\'\":$<>()[]{}".contains(*c))
    };
    let white = |i: usize| wl.get(i).is_some_and(|c| matches!(c, ' ' | '\t'));
    let char_score = |pi: usize, wi: usize, new_match: bool| -> Option<i32> {
        if pl[pi] != wl[wi] {
            return None;
        }
        let case = if p[pi] == w[wi] { 7 } else { 5 };
        let prev = wi.wrapping_sub(1);
        let (mut score, mut gap) = if wi == pi {
            (case, false) // common prefix
        } else if upper(wi) && !upper(prev) {
            (case, true) // camel hump
        } else if sep(wi) && !sep(prev) {
            (5, false)
        } else if sep(prev) || white(prev) {
            (5, true) // word start
        } else {
            (1, false)
        };
        gap = gap || upper(wi) || sep(prev) || white(prev);
        if pi == 0 {
            if wi > word_start {
                score -= if gap { 3 } else { 5 };
            }
        } else if new_match {
            score += if gap { 2 } else { 0 };
        } else {
            score += if gap { 0 } else { 1 };
        }
        // As if a gap followed the word, so ending on its last char isn't a bonus.
        if wi + 1 == wlen {
            score -= if gap { 3 } else { 5 };
        }
        Some(score)
    };

    // table: best score; diag: run length ending here; arrows: 1 matched
    // diagonally, 2 skipped one word char, 3 skipped two.
    let cols = wlen - word_start + 1;
    let mut table = vec![vec![0i32; cols]; plen + 1];
    let mut diag = vec![vec![0i32; cols]; plen + 1];
    let mut arrows = vec![vec![0u8; cols]; plen + 1];
    for pi in 0..plen {
        let row = pi + 1;
        let next_max = if pi + 1 < plen { max[pi + 1] } else { wlen };
        for wi in min[pi]..next_max {
            let col = wi - word_start + 1;
            let diag_score = (wi <= max[pi])
                .then(|| char_score(pi, wi, diag[row - 1][col - 1] == 0))
                .flatten()
                .map(|s| s + table[row - 1][col - 1]);
            // Leaving a run costs 5.
            let gap_start = |c: usize| if diag[row][c] > 0 { -5 } else { 0 };
            let left = (wi > min[pi]).then(|| table[row][col - 1] + gap_start(col - 1));
            let left_left = (wi > min[pi] + 1 && diag[row][col - 1] > 0)
                .then(|| table[row][col - 2] + gap_start(col - 2));
            // Ties go left: an earlier match.
            let beats = |s: i32, other: Option<i32>| other.is_none_or(|o| s >= o);
            let (score, arrow, run) = if let Some(ll) =
                left_left.filter(|&ll| beats(ll, left) && beats(ll, diag_score))
            {
                (ll, 3, 0)
            } else if let Some(l) = left.filter(|&l| beats(l, diag_score)) {
                (l, 2, 0)
            } else {
                (diag_score?, 1, diag[row - 1][col - 1] + 1)
            };
            table[row][col] = score;
            arrows[row][col] = arrow;
            diag[row][col] = run;
        }
    }

    let (mut row, mut col) = (plen, cols - 1);
    let mut score = table[row][col];
    let mut hits = Vec::with_capacity(plen);
    let (mut back, mut max_col) = (0, 0);
    while row >= 1 {
        let mut dc = col;
        loop {
            match arrows[row][dc] {
                3 if dc >= 3 => dc -= 2,
                2 if dc >= 2 => dc -= 1,
                _ => break,
            }
        }
        // Keep extending the run found walking back when it would be longer.
        if back > 1
            && pl[row - 1] == wl[word_start + col - 1]
            && !upper(dc + word_start - 1)
            && back + 1 > diag[row][dc]
        {
            dc = col;
        }
        back = if dc == col { back + 1 } else { 1 };
        if max_col == 0 {
            max_col = dc;
        }
        row -= 1;
        col = dc - 1;
        hits.push(col);
    }
    if wlen - word_start == plen {
        score += 2;
    }
    // A point per word char skipped.
    score -= max_col as i32 - plen as i32;
    hits.reverse();
    Some((score, hits))
}

/// Character indices to byte offsets in `s`.
fn byte_offsets(s: &str, chars: Vec<usize>) -> Vec<usize> {
    let offsets: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    chars
        .into_iter()
        .filter_map(|i| offsets.get(i).copied())
        .collect()
}

/// Every workspace file's definitions, with its path relative to `root`.
// ponytail: parses the whole workspace on each Go to Symbol in Workspace,
// skipping files over 1 MB; VS Code asks language servers, which keep an index.
fn workspace_defs(
    root: &Path,
    settings: &serde_json::Value,
    files: &[String],
) -> Vec<(String, Def)> {
    let read = |path: &Path| {
        let small = fs::metadata(path).is_ok_and(|m| m.len() < 1 << 20);
        small.then(|| fs::read_to_string(path).ok()).flatten()
    };
    files
        .iter()
        .flat_map(|rel| {
            let path = root.join(rel);
            let lang = search::associated_language(settings, root, &path)
                .unwrap_or_else(|| language_id(&path));
            let text = (lang != "plaintext").then(|| read(&path)).flatten();
            let defs = text.map(|text| defs(&editor_language(lang), &text));
            defs.into_iter().flatten().map(move |d| (rel.clone(), d))
        })
        .collect()
}

/// VS Code's Go to Symbol: `@name` by score, `@:name` grouped by kind. With
/// spaces, the first word matches the name and the rest the container, unless
/// the whole query matches the name. Returns the picks, their groups and, for
/// an empty query, the last row whose definition holds `cursor`.
fn rank_symbols(query: &str, defs: &[Def], cursor: usize) -> (Vec<Pick>, Groups, Option<usize>) {
    let (by_kind, q) = match query.strip_prefix(':') {
        Some(q) => (true, q.trim()),
        None => (false, query.trim()),
    };
    let pieces: Vec<String> = prepare_query(q).pieces.into_iter().map(|p| p.0).collect();
    let whole = pieces.concat();
    // VS Code scores `$(icon) name` from the name, so its first char follows a space.
    let in_name = |d: &Def, pattern: &str| {
        let (s, hits) = fuzzy_score(pattern, &format!(" {}", d.name), 1)?;
        Some((s, byte_offsets(&d.name, hits)))
    };
    let score = |d: &Def| -> Option<(i32, Vec<usize>, Vec<usize>)> {
        let Some((first, rest)) = pieces.split_first() else {
            return Some((0, Vec::new(), Vec::new()));
        };
        if !rest.is_empty()
            && let Some((s, hits)) = in_name(d, &whole)
        {
            return Some((s, hits, Vec::new()));
        }
        let (mut total, hits) = in_name(d, first)?;
        let container = d.container.map_or("", |c| defs[c].name.as_str());
        let mut in_container = Vec::new();
        for piece in rest {
            let (s, h) = fuzzy_score(piece, container, 0)?;
            total += s;
            in_container.extend(byte_offsets(container, h));
        }
        in_container.sort_unstable();
        in_container.dedup();
        Some((total, hits, in_container))
    };
    let mut scored: Vec<_> = defs
        .iter()
        .enumerate()
        .filter_map(|(ix, d)| score(d).map(|(s, hits, chits)| (s, ix, hits, chits)))
        .collect();
    scored.sort_by_key(|&(s, ix, ..)| (by_kind.then(|| defs[ix].kind), -s, ix));

    let mut groups = Vec::new();
    if by_kind {
        let mut row = 0;
        for chunk in scored.chunk_by(|a, b| defs[a.1].kind == defs[b.1].kind) {
            let kind = defs[chunk[0].1].kind;
            groups.push((row, format!("{kind} ({})", chunk.len()).into()));
            row += chunk.len();
        }
    } else if !scored.is_empty() {
        groups.push((0, format!("symbols ({})", scored.len()).into()));
    }
    let around = q.is_empty().then(|| {
        scored.iter().rposition(|&(_, ix, ..)| {
            let r = &defs[ix].range;
            (r.start..=r.end).contains(&cursor)
        })
    });
    let picks = scored
        .into_iter()
        .map(|(_, ix, hits, chits)| Pick::Symbol(ix, hits, chits))
        .collect();
    (picks, groups, around.flatten())
}

/// Matched characters: VS Code's list.highlightForeground, white in the focused row.
fn highlights(text: &str, hits: &[usize], selected: bool) -> Vec<(Range<usize>, HighlightStyle)> {
    let style = HighlightStyle {
        color: Some(c(if selected { 0xFFFFFF } else { 0x48A0C7 })),
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    hits.iter()
        .filter_map(|&h| {
            // A whole-path match can hit the separator past the folder's end.
            text.get(h..)?
                .chars()
                .next()
                .map(|c| (h..h + c.len_utf8(), style))
        })
        .collect()
}

enum RowIcon {
    None,
    File,
    Symbol(char, Hsla),
    /// The Seti icon of a language id.
    Lang(&'static str),
}

/// A list row like VS Code's quick pick: icon, highlighted label, dimmer
/// description, keybinding, and the group's label on its first row.
struct Row {
    icon: RowIcon,
    label: SharedString,
    label_hits: Vec<usize>,
    desc: SharedString,
    desc_hits: Vec<usize>,
    action: Option<Box<dyn Action>>,
    group: Option<SharedString>,
    selected: bool,
}

impl Row {
    fn item(self) -> CommandItem {
        // No `.action`: the list would run it before the palette closes, and a
        // command that reopens the palette (Select Default Profile) would close.
        CommandItem::new()
            .label(self.label.clone())
            .child(move |window, cx| self.render(window, cx))
    }

    fn render(&self, window: &Window, cx: &App) -> Div {
        // VS Code dims descriptions to 70% of the row's color.
        let desc_color = if self.selected {
            hsla(0., 0., 1., 0.7)
        } else {
            col::fg_muted()
        };
        let kbd =
            (self.action.as_ref()).map(|a| h_flex().gap_1().children(kbds(a.as_ref(), window)));
        h_flex()
            .flex_1()
            .gap(px(6.))
            .min_w_0()
            .whitespace_nowrap()
            // Covers the list's own row (padded px_2 py_1p5) to paint VS Code's
            // quickInputList.focus colors over the accent it highlights with.
            .mx(rems(-0.5))
            .my(rems(-0.375))
            .px(rems(0.5))
            .py(rems(0.375))
            .rounded(Theme::global(cx).radius)
            .when(self.selected, |this| {
                this.bg(c(0x297AA0)).text_color(c(0xFFFFFF))
            })
            .map(|this| match self.icon {
                RowIcon::None => this,
                RowIcon::File => this.child(file_icon(Path::new(self.label.as_ref()))),
                RowIcon::Symbol(ch, color) => this.child(codicon(ch).text_color(color)),
                RowIcon::Lang(id) => this.child(language_icon(id)),
            })
            .child(
                StyledText::new(self.label.clone()).with_highlights(highlights(
                    &self.label,
                    &self.label_hits,
                    self.selected,
                )),
            )
            .when(!self.desc.is_empty(), |this| {
                this.child(
                    div()
                        .min_w_0()
                        .text_size(px(12.))
                        .text_color(desc_color)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(
                            StyledText::new(self.desc.clone()).with_highlights(highlights(
                                &self.desc,
                                &self.desc_hits,
                                self.selected,
                            )),
                        ),
                )
            })
            .child(div().flex_1())
            .children(kbd)
            .children(self.group.clone().map(|group| {
                // pickerGroup.foreground
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(c(0xBFBFBF))
                    .child(group)
            }))
    }
}

/// A row of Change Icon's grid, the icon at `at` highlighted. Pressing one
/// highlights it, so the list's click confirms that icon.
fn icon_row(
    icons: &[(&str, char, Vec<usize>)],
    at: Option<usize>,
    row: usize,
    this: WeakEntity<Workbench>,
) -> Div {
    let cell = |i: usize, ch: char| {
        let this = this.clone();
        div()
            .flex_1()
            .h(px(36.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .text_size(px(16.))
            .when(at == Some(i), |d| d.bg(c(0x297AA0)).text_color(c(0xFFFFFF)))
            .child(codicon(ch))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                this.update(cx, |w, _| {
                    w.palette.col = i;
                    w.palette.selected = row;
                })
                .ok();
            })
    };
    h_flex()
        .flex_1()
        // Covers the list's row padding and its own highlight, as `Row` does.
        .mx(rems(-0.5))
        .my(rems(-0.375))
        .px(rems(0.25))
        .bg(c(0x202122))
        .children(icons.iter().enumerate().map(|(i, &(_, ch, _))| cell(i, ch)))
        .children((icons.len()..ICON_COLS).map(|_| div().flex_1()))
}

impl Workbench {
    pub(super) fn open_palette(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let p = &mut self.palette;
        if !p.open {
            p.restore = window.focused(cx);
            p.focus_path = window.context_stack();
            p.open = true;
        }
        p.defs = None;
        p.ws_defs = None;
        p.ws_ask = None;
        p.profiles = None;
        p.renaming = None;
        p.coloring = None;
        p.iconing = None;
        p.joining = false;
        p.eol = false;
        p.lang = false;
        p.items = None;
        p.associating = None;
        p.indent = None;
        p.encoding = None;
        p.parse = None;
        p.state
            .update(cx, |s, cx| s.set_query(query.to_string(), window, cx));
        p.state.clone().update(cx, |s, cx| s.focus(window, cx));
        self.palette_query(query.to_string(), cx);
        self.walk_files(cx);
        cx.notify();
    }

    /// VS Code's Select Default Profile: the palette lists the profiles, the
    /// default first, and the one picked becomes the default.
    pub(super) fn select_default_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette("", window, cx);
        self.palette.profiles = Some(profiles());
        self.palette_query(String::new(), cx);
    }

    /// VS Code's Terminal: Rename: the palette becomes an input holding the
    /// active terminal's name.
    pub(super) fn rename_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((id, name)) = self.terminal_name() {
            self.rename_input(Rename::Terminal(id), name, window, cx);
        }
    }

    /// The palette becomes an input holding `name`, for `what`'s new name.
    pub(super) fn rename_input(
        &mut self,
        what: Rename,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_palette("", window, cx);
        self.palette.renaming = Some(what);
        (self.palette.state).update(cx, |s, cx| s.set_query(name.clone(), window, cx));
        self.palette_query(name, cx);
        // Selected, as in VS Code, so typing replaces it. Frame callbacks run
        // before drawing, and the input exists once drawn: wait out one frame.
        let input = self.palette.state.focus_handle(cx);
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                input.dispatch_action(&input::SelectAll, window, cx)
            })
        });
    }

    /// VS Code's Terminal: Change Color: the palette lists the colors for the
    /// active terminal's tab icon.
    pub(super) fn change_terminal_color(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, _)) = self.terminal_name() else {
            return;
        };
        self.open_palette("", window, cx);
        self.palette.coloring = Some(id);
        self.palette_query(String::new(), cx);
    }

    /// VS Code's Terminal: Join Terminals: the palette lists the terminals
    /// alone in their group, to join the active one's.
    pub(super) fn pick_terminal_to_join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_name().is_none() {
            return;
        }
        if self.terminal_count() <= 1 {
            return self.warn("Insufficient terminals for the join action", cx);
        }
        if self.joinable().is_empty() {
            return self.warn("All terminals are joined already", cx);
        }
        self.open_palette("", window, cx);
        self.palette.joining = true;
        self.palette_query(String::new(), cx);
    }

    /// VS Code's Change End of Line Sequence: the palette lists LF and CRLF,
    /// the active editor's highlighted.
    pub(super) fn change_eol(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.docs.get(self.active).is_some_and(|d| !d.binary) {
            return;
        }
        self.open_palette("", window, cx);
        self.palette.eol = true;
        self.palette_query(String::new(), cx);
    }

    /// The palette as the plain pick list `items`.
    pub(super) fn open_items(&mut self, items: Items, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette("", window, cx);
        self.palette.items = Some(items);
        self.palette_query(String::new(), cx);
    }

    /// VS Code's Change Language Mode: the palette lists every language, the
    /// active editor's highlighted.
    pub(super) fn change_language(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.docs.get(self.active).is_some_and(|d| !d.binary) {
            return;
        }
        self.open_palette("", window, cx);
        self.palette.lang = true;
        self.palette_query(String::new(), cx);
    }

    /// VS Code's Change File Encoding (the status bar's encoding): the palette
    /// lists Reopen and Save with Encoding.
    pub(super) fn change_encoding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.docs.get(self.active).is_some_and(|d| !d.binary) {
            return;
        }
        self.open_palette("", window, cx);
        self.palette.encoding = Some(EncodingStep::Action);
        self.palette_query(String::new(), cx);
    }

    /// Runs Change File Encoding's action `ix`, or reopens or saves with the
    /// encoding it picked.
    fn set_encoding(
        &mut self,
        step: EncodingStep,
        pick: Option<Encoding>,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let doc = &mut self.docs[self.active];
        match (step, pick) {
            (EncodingStep::Action, _) => {
                let guessed =
                    (fs::read(&doc.path).ok()).and_then(|b| Some(Encoding::decode(&b)?.1));
                // The list opens once this one has closed.
                cx.defer_in(window, move |this, window, cx| {
                    this.open_palette("", window, cx);
                    let reopen = ix == 0;
                    this.palette.encoding = Some(EncodingStep::List { reopen, guessed });
                    this.palette_query(String::new(), cx);
                });
            }
            (EncodingStep::List { reopen: false, .. }, Some(encoding)) => {
                doc.encoding = encoding;
                // VS Code saves right away, except an untitled file, which keeps
                // the encoding for its Save As.
                if !doc.untitled {
                    self.write(self.active, window, cx);
                }
                cx.notify();
            }
            (EncodingStep::List { .. }, Some(encoding)) if !doc.dirty => {
                self.reopen(self.active, encoding, window, cx)
            }
            (EncodingStep::List { .. }, Some(encoding)) => {
                let answer = window.prompt(
                    PromptLevel::Warning,
                    "Do you want to revert the active text editor and reopen with a different encoding?",
                    Some("This will discard any unsaved changes."),
                    &["Discard Changes and Reopen", "Cancel"],
                    cx,
                );
                let editor = doc.editor.clone();
                cx.spawn_in(window, async move |this, cx| {
                    if answer.await != Ok(0) {
                        return;
                    }
                    this.update_in(cx, |this, window, cx| {
                        // The tab may have moved or closed while the dialog was open.
                        if let Some(ix) = this.docs.iter().position(|d| d.editor == editor) {
                            this.reopen(ix, encoding, window, cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
            _ => {}
        }
    }

    /// VS Code's Select Indentation (the status bar's Spaces/Tab Size): the
    /// palette lists the indentation actions.
    pub(super) fn select_indentation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.docs.get(self.active).is_some_and(|d| !d.binary) {
            return;
        }
        self.open_palette("", window, cx);
        self.palette.indent = Some(IndentStep::Action);
        self.palette_query(String::new(), cx);
    }

    /// Runs Select Indentation's action `ix` from its command.
    pub(super) fn indent_action(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.docs.get(self.active).is_some_and(|d| !d.binary) {
            self.set_indent(IndentStep::Action, ix, window, cx);
        }
    }

    /// Runs Select Indentation's action `ix`, or sets the tab size it picked.
    fn set_indent(
        &mut self,
        step: IndentStep,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let doc = &mut self.docs[self.active];
        let mut text = None;
        match (step, ix) {
            (IndentStep::Size(hard_tabs), n) => {
                doc.indent.tab_size = n;
                doc.indent.hard_tabs = hard_tabs.unwrap_or(doc.indent.hard_tabs);
            }
            (_, 0..=2) => {
                // The size list opens once this one has closed.
                let next = [Some(false), Some(true), None][ix];
                return cx.defer_in(window, move |this, window, cx| {
                    this.open_palette("", window, cx);
                    this.palette.indent = Some(IndentStep::Size(next));
                    this.palette_query(String::new(), cx);
                });
            }
            (_, 3) => doc.indent = detect_indent(&doc.editor.read(cx).value()),
            (_, 4 | 5) => {
                doc.indent.hard_tabs = ix == 5;
                let old = doc.editor.read(cx).value();
                text = Some(reindent(&old, doc.indent.tab_size, ix == 5));
            }
            _ => text = Some(trim_trailing(&doc.editor.read(cx).value())),
        }
        if let Some(text) = text {
            doc.editor.update(cx, |e, cx| {
                let pos = e.cursor_position();
                e.replace_all(text, window, cx);
                e.set_cursor_position(pos, window, cx);
            });
        }
        if ix != 6 {
            self.rebuild_editor(self.active, window, cx);
        }
    }

    /// Change Language Mode's Configure File Association: the palette lists
    /// the languages to associate the active file's extension (or name) with.
    fn pick_association(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.docs.get(self.active).map(|d| d.path.clone()) else {
            return;
        };
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let key = match path.extension() {
            Some(ext) => format!("*.{}", ext.to_string_lossy()),
            None => name.into_owned(),
        };
        let by_name = self.file_language(&path);
        // The list opens once this one has closed.
        cx.defer_in(window, move |this, window, cx| {
            this.open_palette("", window, cx);
            this.palette.lang = true;
            this.palette.associating = Some((key, by_name));
            this.palette_query(String::new(), cx);
        });
    }

    /// Adds `key` → `id` to the user's `files.associations`; open files follow.
    fn associate(
        &mut self,
        key: String,
        id: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = super::terminal::settings();
        let mut map = settings["files.associations"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        map.insert(key, id.into());
        match set_setting("files.associations", map.into()) {
            Ok(()) => self.relanguage(&settings, window, cx),
            Err(err) => self.settings_error(err, cx),
        }
    }

    /// Highlights the active editor as `id`, and the status bar names it.
    fn set_language(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(self.active) else {
            return;
        };
        doc.lang = id;
        self.rebuild_editor(self.active, window, cx);
    }

    /// Builds editor `ix` again for its language and indentation, at the
    /// same place.
    pub(super) fn rebuild_editor(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_extensions(&format!("onLanguage:{}", self.docs[ix].lang), cx);
        let doc = &self.docs[ix];
        let (path, lang, indent) = (doc.path.clone(), doc.lang, doc.indent);
        let wrap = self.wraps(doc);
        let old = doc.editor.read(cx);
        let (text, pos, scroll) = (
            old.value().to_string(),
            old.cursor_position(),
            old.scroll_offset(),
        );
        // ponytail: gpui-base's set_highlighter doesn't re-highlight a shown
        // editor and its tab size has no setter, so it's rebuilt (as Save As
        // does), dropping undo history.
        let editor = self.new_editor(&path, lang, text, indent, wrap, window, cx);
        editor.update(cx, |e, cx| {
            e.set_cursor_position(pos, window, cx);
            e.set_scroll_offset(scroll, cx);
        });
        self.docs[ix].editor = editor;
        self.lsp_open(ix, cx);
        self.show_diagnostics(&path, cx);
        if ix == self.active {
            self.activate(ix, window, cx);
        }
    }

    /// Rewrites every line ending in the active editor, undoably, keeping
    /// the cursor where it was.
    fn set_eol(&mut self, crlf: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(self.active) else {
            return;
        };
        doc.crlf = crlf;
        let old = doc.editor.read(cx).value();
        let text = with_eol(&old, crlf);
        if text != old.as_ref() {
            doc.editor.update(cx, |e, cx| {
                let pos = e.cursor_position();
                e.replace_all(text, window, cx);
                e.set_cursor_position(pos, window, cx);
            });
        }
        cx.notify();
    }

    /// VS Code's Terminal: Change Icon: the palette shows every codicon in a
    /// grid for the active terminal's tab.
    pub(super) fn change_terminal_icon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, _)) = self.terminal_name() else {
            return;
        };
        self.open_palette("", window, cx);
        self.palette.iconing = Some(id);
        self.palette_query(String::new(), cx);
    }

    /// Re-lists the workspace's files when the watcher saw them change. The
    /// first walk shows files as they are found; later ones keep showing the
    /// previous list until they finish.
    fn walk_files(&mut self, cx: &mut Context<Self>) {
        let p = &mut self.palette;
        let (Some(root), true, None) = (self.root.clone(), p.stale, &p.walk) else {
            return;
        };
        // Changes from here on need another walk.
        p.stale = false;
        let stream = p.files.is_empty();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Vec<String>>();
        cx.background_spawn(async move {
            let mut batch = Vec::new();
            let mut sent = Instant::now();
            let excluded = search::overrides(&root, "", "", &search::exclude_globs(true));
            let siblings = search::sibling_rules(&super::terminal::settings(), &root, true);
            let walk = ignore::WalkBuilder::new(&root)
                .hidden(false)
                .overrides(excluded.unwrap_or_else(|_| Override::empty()))
                .build();
            for e in walk.flatten() {
                if !e.file_type().is_some_and(|t| t.is_file())
                    || search::sibling_hidden(&siblings, e.path(), false)
                {
                    continue;
                }
                if let Ok(rel) = e.path().strip_prefix(&root) {
                    batch.push(rel.to_string_lossy().into_owned());
                }
                if sent.elapsed() > Duration::from_millis(100) {
                    // The palette closed or the workspace changed; stop walking.
                    if tx.unbounded_send(std::mem::take(&mut batch)).is_err() {
                        return;
                    }
                    sent = Instant::now();
                }
            }
            tx.unbounded_send(batch).ok();
        })
        .detach();
        p.walk = Some(cx.spawn(async move |this, cx| {
            let mut files = Vec::new();
            while let Some(batch) = rx.next().await {
                let update = this.update(cx, |this, cx| {
                    let p = &mut this.palette;
                    if stream {
                        Arc::make_mut(&mut p.files).extend(batch);
                        if p.open {
                            let query = p.query.clone();
                            this.palette_query(query, cx);
                        }
                    } else {
                        files.extend(batch);
                    }
                });
                if update.is_err() {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                let p = &mut this.palette;
                p.walk = None;
                if !stream {
                    p.files = Arc::new(files);
                    if p.open {
                        let query = p.query.clone();
                        this.palette_query(query, cx);
                    }
                }
            })
            .ok();
        }));
    }

    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.palette.open {
            return;
        }
        self.palette_unpreview(true, cx);
        // A theme picker closed without a pick: back to the configured theme.
        match self.palette.items.as_ref().map(|i| &i.picker) {
            Some(Picker::Theme(_)) => {
                super::extensions::apply_theme(&super::extensions::configured_theme(), cx);
            }
            Some(Picker::IconTheme(_)) => {
                super::extensions::apply_icon_theme(super::extensions::configured_icon_theme().as_deref())
            }
            _ => {}
        }
        let p = &mut self.palette;
        // An extension's quick pick or input box closed without an answer.
        if let (
            Some(Items {
                picker: Picker::Host(id),
                ..
            }),
            _,
        )
        | (_, Some(Rename::Host(id, _))) = (&p.items, &p.renaming)
        {
            self.ext.answer(*id, serde_json::Value::Null);
        }
        p.open = false;
        // The walk keeps going: its files are the next open's index.
        p.rank = None;
        p.parse = None;
        p.profiles = None;
        p.renaming = None;
        p.coloring = None;
        p.iconing = None;
        p.joining = false;
        p.eol = false;
        p.lang = false;
        p.items = None;
        p.associating = None;
        p.indent = None;
        p.encoding = None;
        // A command may have moved focus (Show Search); only restore it when it is still ours.
        if p.focus.contains_focused(window, cx) {
            match p.restore.take() {
                Some(handle) => window.focus(&handle, cx),
                None => window.focus(&self.focus, cx),
            }
        }
        cx.notify();
    }

    fn palette_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.palette_pick(&query, cx);
        if !query.starts_with([':', '@']) {
            self.palette_unpreview(true, cx);
        } else if self.palette.shown.is_empty() {
            if let Some(deco) = self.palette.deco.take() {
                deco.dispose(cx);
            }
        } else {
            self.palette_preview(self.palette.selected, cx);
        }
    }

    /// Fills the list for `query`; Quick Open's file ranking finishes in the background.
    fn palette_pick(&mut self, query: &str, cx: &mut Context<Self>) {
        let active = self.docs.get(self.active);
        let p = &mut self.palette;
        p.query = query.to_string();
        p.empty = "No matching results".into();
        p.goto = None;
        p.rank = None;
        cx.notify();

        if let Some(profiles) = &p.profiles {
            let shown = (profiles.iter().enumerate())
                .filter_map(|(ix, pr)| {
                    Some(Pick::Profile(ix, command_matches(query.trim(), &pr.name)?))
                })
                .collect();
            p.show(shown, Vec::new(), 0);
            return;
        }
        if let Some(items) = &p.items {
            let q = query.trim();
            let shown = (items.rows.iter().enumerate())
                .filter_map(|(ix, (label, _))| Some(Pick::Item(ix, command_matches(q, label)?)))
                .collect();
            match q.is_empty() {
                true => p.show(shown, items.groups.clone(), items.current),
                false => p.show(shown, Vec::new(), 0),
            }
            return;
        }
        if p.lang {
            // Associating lists names alone, the file's current one preselected.
            let (current, associating) = match &p.associating {
                Some((_, by_name)) => (*by_name, true),
                None => (active.map_or("", |d| d.lang), false),
            };
            let mut langs: Vec<_> = (LANGUAGES.iter().copied())
                .chain(
                    extensions::language_ids()
                        .into_iter()
                        .filter(|id| !LANGUAGES.contains(id)),
                )
                .map(|id| match associating {
                    true => (id, language_label(id).to_string()),
                    false => (id, format!("{} ({id})", language_label(id))),
                })
                .collect();
            langs.sort_by_key(|l| l.1.to_lowercase());
            // VS Code offers to associate a file's extension, or its whole name
            // when it has none (.gitignore, Dockerfile).
            let ext = (active.filter(|d| !associating && !d.untitled))
                .and_then(|d| d.path.file_name())
                .map(|name| match Path::new(name).extension() {
                    Some(ext) => format!(".{}", ext.to_string_lossy()),
                    None => name.to_string_lossy().into_owned(),
                });
            let associate = ext.and_then(|ext| {
                let label = format!("Configure File Association for '{ext}'...");
                let hits = command_matches(query.trim(), &label)?;
                Some(Pick::Associate(label, hits))
            });
            let shown: Vec<_> = (associate.into_iter())
                .chain(langs.into_iter().filter_map(|(id, label)| {
                    let hits = command_matches(query.trim(), &label)?;
                    Some(Pick::Lang(id, label, hits))
                }))
                .collect();
            let first = shown.iter().position(|p| matches!(p, Pick::Lang(..)));
            let groups = match (associating, first) {
                (false, Some(at)) => vec![(at, "languages (identifier)".into())],
                _ => Vec::new(),
            };
            let preselect = (shown.iter())
                .position(|p| matches!(p, Pick::Lang(id, ..) if *id == current))
                .unwrap_or(0);
            p.show(shown, groups, preselect);
            return;
        }
        if let Some(step) = p.encoding {
            let q = query.trim();
            let (shown, groups, select) = match step {
                EncodingStep::Action => {
                    // An untitled file has nothing to reopen.
                    let first = active.is_some_and(|d| d.untitled) as usize;
                    let shown = (first..2)
                        .filter_map(|ix| {
                            let hits = command_matches(q, ENCODING_ACTIONS[ix])?;
                            Some(Pick::EncodingAction(ix, hits))
                        })
                        .collect();
                    (shown, Vec::new(), 0)
                }
                EncodingStep::List { reopen, guessed } => {
                    // VS Code lists the guessed encoding first when it isn't the
                    // configured UTF-8, and hides save-only UTF-8 with BOM from Reopen.
                    let guessed = guessed.filter(|&g| g != Encoding::Utf8);
                    let rows = (guessed.map(|g| (g, "Guessed from content")).into_iter()).chain(
                        (Encoding::ALL.into_iter())
                            .filter(|&e| Some(e) != guessed && !(reopen && e == Encoding::Utf8Bom))
                            .map(|e| (e, e.id())),
                    );
                    let shown: Vec<_> = rows
                        .filter_map(|(e, desc)| {
                            Some(Pick::Encoding(e, desc, command_matches(q, e.label())?))
                        })
                        .collect();
                    let current = active.map(|d| d.encoding);
                    let select = shown.iter().position(
                        |p| matches!(p, Pick::Encoding(e, desc, _) if Some(*e) == current && *desc == e.id()),
                    );
                    let guess_shown = matches!(shown.first(), Some(Pick::Encoding(e, _, _)) if Some(*e) == guessed);
                    let groups = match guess_shown && shown.len() > 1 {
                        true => vec![(1, SharedString::default())],
                        false => Vec::new(),
                    };
                    (shown, groups, select.unwrap_or(0))
                }
            };
            p.show(shown, groups, select);
            return;
        }
        if let Some(step) = p.indent {
            let q = query.trim();
            let (shown, groups, select): (Vec<_>, _, _) = match step {
                IndentStep::Action => {
                    let shown: Vec<_> = (INDENT_ACTIONS.iter().enumerate())
                        .filter_map(|(ix, label)| {
                            Some(Pick::Indent(ix, command_matches(q, label)?))
                        })
                        .collect();
                    let first = |convert: bool| {
                        let at = shown
                            .iter()
                            .position(|p| matches!(p, Pick::Indent(ix, _) if (*ix > 2) == convert));
                        at.map(|at| {
                            (
                                at,
                                SharedString::from(
                                    ["change view", "convert file"][convert as usize],
                                ),
                            )
                        })
                    };
                    let groups = [first(false), first(true)].into_iter().flatten().collect();
                    (shown, groups, 0)
                }
                IndentStep::Size(_) => {
                    let tab = active.map_or(TAB_SIZE, |d| d.indent.tab_size);
                    let shown: Vec<_> = (1..=8)
                        .filter_map(|n| Some(Pick::Indent(n, command_matches(q, &n.to_string())?)))
                        .collect();
                    let select = shown
                        .iter()
                        .position(|p| matches!(p, Pick::Indent(n, _) if *n == tab.min(8)));
                    (shown, Vec::new(), select.unwrap_or(0))
                }
            };
            p.show(shown, groups, select);
            return;
        }
        if p.eol {
            let crlf = active.is_some_and(|d| d.crlf);
            let shown = [false, true]
                .into_iter()
                .filter_map(|crlf| Some(Pick::Eol(crlf, command_matches(query.trim(), eol(crlf))?)))
                .collect();
            p.show(shown, Vec::new(), crlf as usize);
            return;
        }
        if p.joining {
            let q = query.trim();
            let shown = (self.joinable().into_iter())
                .filter_map(|(id, name, ch, color)| {
                    let hits = command_matches(q, &name)?;
                    Some(Pick::Join(id, name, ch, color, hits))
                })
                .collect();
            self.palette.show(shown, Vec::new(), 0);
            return;
        }
        if p.coloring.is_some() {
            let q = query.trim();
            let mut shown: Vec<_> = (COLORS.iter().enumerate())
                .filter_map(|(ix, name)| Some(Pick::Color(Some(ix), command_matches(q, name)?)))
                .collect();
            let colors = shown.len();
            shown.extend(command_matches(q, "Reset to default").map(|h| Pick::Color(None, h)));
            let groups = match colors {
                n if n < shown.len() && n > 0 => vec![(n, SharedString::default())],
                _ => Vec::new(),
            };
            p.show(shown, groups, 0);
            return;
        }
        if p.iconing.is_some() {
            let icons: Vec<_> = codicons()
                .filter_map(|(name, ch)| Some((name, ch, command_matches(query.trim(), name)?)))
                .collect();
            let shown = icons.chunks(ICON_COLS).map(|r| Pick::Icons(r.to_vec()));
            p.col = 0;
            p.empty = "No results".into();
            p.show(shown.collect(), Vec::new(), 0);
            return;
        }
        if p.renaming.is_some() {
            p.empty = "Press 'Enter' to confirm your input or 'Escape' to cancel".into();
            p.show(Vec::new(), Vec::new(), 0);
            return;
        }

        if let Some(q) = query.strip_prefix("view ") {
            let q = q.trim();
            // As VS Code: a row whose group name contains the filter stays,
            // unhighlighted.
            let lower = q.to_lowercase();
            let matches = |label: &str, group: &str| {
                command_matches(q, label)
                    .or_else(|| group.to_lowercase().contains(&lower).then(Vec::new))
            };
            let mut shown: Vec<_> = (0..VIEWS.len())
                .filter_map(|ix| {
                    let group = if ix < SIDE_BAR_VIEWS {
                        "Side Bar"
                    } else {
                        "Panel"
                    };
                    Some((group, Pick::View(ix, matches(VIEWS[ix], group)?)))
                })
                .collect();
            for (id, label) in self.terminal_views() {
                if let Some(hits) = matches(&label, "Terminal") {
                    shown.push(("Terminal", Pick::TermView(id, label, hits)));
                }
            }
            for name in self.output.channel_names() {
                if let Some(hits) = matches(&name, "Output") {
                    shown.push(("Output", Pick::OutputView(name, hits)));
                }
            }
            let groups = (0..shown.len())
                .filter(|&row| row == 0 || shown[row].0 != shown[row - 1].0)
                .map(|row| (row, shown[row].0.into()))
                .collect();
            let p = &mut self.palette;
            p.empty = "No matching views".into();
            p.show(shown.into_iter().map(|(_, pick)| pick).collect(), groups, 0);
            return;
        }

        if let Some(q) = query.strip_prefix('>') {
            // VS Code lists recently used commands, then the rest alphabetically.
            let commands = commands(&p.focus_path);
            let mut shown: Vec<_> = (0..commands.len())
                .filter_map(|ix| Some((ix, command_matches(q.trim(), &commands[ix].0)?)))
                .collect();
            let recent = |ix: usize| p.recent.iter().position(|r| *r == commands[ix].0);
            shown.sort_by_key(|&(ix, _)| (recent(ix).unwrap_or(usize::MAX), &commands[ix].0));
            let mut groups = Vec::new();
            if shown.first().is_some_and(|&(ix, _)| recent(ix).is_some()) {
                groups.push((0, "recently used".into()));
                if let Some(row) = shown.iter().position(|&(ix, _)| recent(ix).is_none()) {
                    groups.push((row, "other commands".into()));
                }
            }
            let shown = shown.into_iter().map(|(ix, hits)| Pick::Command(ix, hits));
            p.show(shown.collect(), groups, 0);
            p.empty = "No matching commands".into();
            return;
        }

        if let Some(q) = query.strip_prefix('#') {
            // Language servers answer each query from their index; files
            // they don't know are parsed when they find nothing.
            let asked = q.trim().to_string();
            if p.ws_ask.as_ref().is_none_or(|(q, _)| *q != asked)
                && let Some(asks) = self.lsp.workspace_symbols(&asked)
            {
                let root = self.root.clone().unwrap_or_default();
                let task = cx.spawn(async move |this, cx| {
                    let mut found = Vec::new();
                    for ask in asks {
                        found.extend(workspace_symbol_defs(&ask.await.unwrap_or_default(), &root));
                    }
                    this.update(cx, |this, cx| {
                        if !found.is_empty() {
                            this.palette.ws_defs = Some(Arc::new(found));
                        }
                        let query = this.palette.query.clone();
                        this.palette_query(query, cx);
                    })
                    .ok();
                });
                p.ws_ask = Some((asked, task));
            }
            let Some(ws) = p.ws_defs.clone() else {
                // Parsed once the file walk is done; the list fills then.
                if let (None, None, false) = (&p.parse, &p.walk, p.stale) {
                    let root = self.root.clone().unwrap_or_default();
                    let files = p.files.clone();
                    let settings = super::terminal::settings();
                    p.parse = Some(cx.spawn(async move |this, cx| {
                        let ws = cx
                            .background_spawn(
                                async move { workspace_defs(&root, &settings, &files) },
                            )
                            .await;
                        this.update(cx, |this, cx| {
                            this.palette.ws_defs = Some(Arc::new(ws));
                            let query = this.palette.query.clone();
                            this.palette_query(query, cx);
                        })
                        .ok();
                    }));
                }
                p.empty = SharedString::default();
                p.show(Vec::new(), Vec::new(), 0);
                return;
            };
            let q = q.trim();
            let mut scored: Vec<_> = (ws.iter().enumerate())
                .filter_map(|(ix, (_, d))| {
                    if q.is_empty() {
                        return Some((0, ix, Vec::new()));
                    }
                    // Scored like `@` names: `$(icon) name`.
                    let (s, hits) = fuzzy_score(q, &format!(" {}", d.name), 1)?;
                    Some((s, ix, byte_offsets(&d.name, hits)))
                })
                .collect();
            scored.sort_by_key(|&(s, ix, _)| (-s, ix));
            let shown = scored.into_iter().take(SHOWN);
            p.empty = "No matching workspace symbols".into();
            p.show(
                shown
                    .map(|(_, ix, hits)| Pick::WorkspaceSymbol(ix, hits))
                    .collect(),
                Vec::new(),
                0,
            );
            return;
        }

        if let Some(q) = query.strip_prefix('@') {
            let Some(doc) = active else {
                p.empty =
                    "To go to a symbol, first open a text editor with symbol information.".into();
                p.show(Vec::new(), Vec::new(), 0);
                return;
            };
            let editor = doc.editor.read(cx);
            let Some(defs) = &p.defs else {
                // A big file takes a while to parse; the list fills when it is done.
                if p.parse.is_none() {
                    let lang = editor_language(doc.lang);
                    let text = editor.value().to_string();
                    let uri = super::lsp::path_uri(&doc.path);
                    let params = serde_json::json!({ "textDocument": { "uri": uri } });
                    let ask = self
                        .lsp
                        .request(&doc.path, "textDocument/documentSymbol", params);
                    p.parse = Some(cx.spawn(async move |this, cx| {
                        let reply = match ask {
                            Some(ask) => ask.await.unwrap_or_default(),
                            None => serde_json::Value::Null,
                        };
                        // The language server's symbols, else the syntax tree's.
                        let defs = cx
                            .background_spawn(async move {
                                let found = symbol_defs(&reply, &text);
                                if found.is_empty() {
                                    defs(&lang, &text)
                                } else {
                                    found
                                }
                            })
                            .await;
                        this.update(cx, |this, cx| {
                            this.palette.defs = Some(defs);
                            let query = this.palette.query.clone();
                            this.palette_query(query, cx);
                        })
                        .ok();
                    }));
                }
                p.empty = SharedString::default();
                p.show(Vec::new(), Vec::new(), 0);
                return;
            };
            let (shown, groups, around) = rank_symbols(q, defs, editor.cursor());
            p.empty = if defs.is_empty() {
                "No editor symbols"
            } else {
                "No matching editor symbols"
            }
            .into();
            p.show(shown, groups, around.unwrap_or(0));
            return;
        }

        if let Some(q) = query.strip_prefix(':') {
            let Some(doc) = active else {
                p.empty = "Open a text editor first to go to a line or an offset.".into();
                p.show(Vec::new(), Vec::new(), 0);
                return;
            };
            match goto_line(q, &doc.editor.read(cx).value(), doc.indent.tab_size) {
                Ok((line, col, label)) => p.show(vec![Pick::Line(line, col, label)], Vec::new(), 0),
                Err(hint) => {
                    p.empty = hint.into();
                    p.show(Vec::new(), Vec::new(), 0);
                }
            }
            return;
        }

        let (file_q, goto) = split_line(query);
        p.goto = goto;
        let root = self.root.clone().unwrap_or_default();
        let history: Vec<String> = p
            .editors
            .get(&root_key(self.root.as_deref()))
            .into_iter()
            .flatten()
            .map(|path| Path::new(path))
            .map(|path| path.strip_prefix(&root).unwrap_or(path))
            .map(|rel| rel.to_string_lossy().into_owned())
            .collect();
        let files = p.files.clone();
        let file_q = file_q.to_string();
        // Scoring a big repo takes a while; keep typing responsive.
        p.rank = Some(cx.spawn(async move |this, cx| {
            let (shown, groups) = cx
                .background_spawn(async move { rank_files(&file_q, &root, &history, &files) })
                .await;
            this.update(cx, |this, cx| {
                this.palette.show(shown, groups, 0);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Go to Line/Symbol preview: centers `row`'s line in the active editor and
    /// highlights it (VS Code's rangeHighlight), keeping the view to restore.
    fn palette_preview(&mut self, row: usize, cx: &mut Context<Self>) {
        let p = &self.palette;
        let (line, range) = match p.shown.get(row) {
            Some(Pick::Line(line, ..)) => (*line, None),
            Some(Pick::Symbol(ix, ..)) => {
                let def = &p.defs.as_ref().unwrap()[*ix];
                (def.line, Some(def.range.clone()))
            }
            _ => return,
        };
        let Some(editor) = self.docs.get(self.active).map(|d| d.editor.clone()) else {
            return;
        };
        let (view, range) = editor.update(cx, |e, cx| {
            let view = e.scroll_offset();
            let range = range.unwrap_or_else(|| {
                let text = e.text();
                text.line_start_offset(line as usize)..text.line_end_offset(line as usize)
            });
            center_line(e, line, None, cx);
            (view, range)
        });
        self.palette.view.get_or_insert(view);
        let deco = vec![
            RangeDecoration::new(range)
                .with_style(RangeDecorationStyle::Fill)
                .with_color(ca(0xFFFFFF13)),
        ];
        match &self.palette.deco {
            Some(d) => d.set(deco, cx),
            None => {
                let d = editor.update(cx, |e, cx| e.create_range_decorations_collection(deco, cx));
                self.palette.deco = Some(d);
            }
        }
    }

    /// The color and icon theme pickers show the highlighted theme, as in VS Code.
    fn preview_theme(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(&Pick::Item(ix, _)) = self.palette.shown.get(row) else {
            return;
        };
        match self.palette.items.as_ref().map(|i| &i.picker) {
            Some(Picker::Theme(names)) => {
                super::extensions::apply_theme(&names[ix].clone(), cx);
            }
            Some(Picker::IconTheme(ids)) => super::extensions::apply_icon_theme(ids[ix].as_deref()),
            _ => {}
        }
    }

    /// Ends a preview, scrolling back to where the editor was unless a pick moved it.
    fn palette_unpreview(&mut self, restore: bool, cx: &mut Context<Self>) {
        let p = &mut self.palette;
        if let Some(deco) = p.deco.take() {
            deco.dispose(cx);
        }
        if let (Some(view), true, Some(doc)) = (p.view.take(), restore, self.docs.get(self.active))
        {
            doc.editor.update(cx, |e, cx| e.set_scroll_offset(view, cx));
        }
    }

    fn palette_confirm(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let p = &mut self.palette;
        let (mut command, mut view, mut term, mut channel) = (None, None, None, None);
        let (file, pos) = match p.shown.get(row) {
            Some(Pick::File(rel, ..)) => {
                let path = match &self.root {
                    Some(root) => root.join(rel),
                    None => PathBuf::from(rel),
                };
                (Some(path), p.goto)
            }
            Some(Pick::Line(line, col, _)) => (None, Some((*line, *col))),
            Some(Pick::Symbol(ix, ..)) => {
                let def = &p.defs.as_ref().unwrap()[*ix];
                (None, Some((def.line, def.col)))
            }
            Some(Pick::WorkspaceSymbol(ix, _)) => {
                let (rel, def) = &p.ws_defs.as_ref().unwrap()[*ix];
                let root = self.root.clone().unwrap_or_default();
                (Some(root.join(rel)), Some((def.line, def.col)))
            }
            Some(&Pick::Command(ix, _)) => {
                let (label, action) = commands(&p.focus_path).swap_remove(ix);
                command = Some(action);
                p.recent.retain(|r| *r != label);
                p.recent.insert(0, label.to_string());
                p.recent.truncate(COMMAND_HISTORY);
                p.save(cx);
                (None, None)
            }
            Some(&Pick::Profile(ix, _)) => {
                if let Err(err) = set_default_profile(&p.profiles.as_ref().unwrap()[ix].name) {
                    self.settings_error(err, cx);
                }
                (None, None)
            }
            Some(Pick::Icons(icons)) => {
                if let (Some(id), Some(&(_, ch, _))) = (p.iconing, icons.get(p.col)) {
                    self.set_terminal_icon(id, ch, cx);
                }
                (None, None)
            }
            Some(&Pick::Join(id, ..)) => {
                self.join_active_terminal(id, window, cx);
                (None, None)
            }
            Some(&Pick::Eol(crlf, _)) => {
                self.set_eol(crlf, window, cx);
                (None, None)
            }
            Some(&Pick::Lang(id, ..)) => {
                match p.associating.take() {
                    Some((key, _)) => self.associate(key, id, window, cx),
                    None => self.set_language(id, window, cx),
                }
                (None, None)
            }
            Some(Pick::Associate(..)) => {
                self.pick_association(window, cx);
                (None, None)
            }
            Some(&Pick::Indent(ix, _)) => {
                let step = p.indent.unwrap_or(IndentStep::Action);
                self.set_indent(step, ix, window, cx);
                (None, None)
            }
            Some(&Pick::EncodingAction(ix, _)) => {
                self.set_encoding(EncodingStep::Action, None, ix, window, cx);
                (None, None)
            }
            Some(&Pick::Encoding(encoding, ..)) => {
                let step = p.encoding.unwrap_or(EncodingStep::Action);
                self.set_encoding(step, Some(encoding), 0, window, cx);
                (None, None)
            }
            Some(&Pick::Color(color, _)) => {
                if let Some(id) = p.coloring {
                    self.set_terminal_color(id, color, cx);
                }
                (None, None)
            }
            Some(&Pick::Item(ix, _)) => {
                if let Some(items) = p.items.take() {
                    self.pick_item(items.picker, ix, window, cx);
                }
                (None, None)
            }
            Some(&Pick::View(ix, _)) => {
                view = Some(ix);
                (None, None)
            }
            Some(&Pick::TermView(id, ..)) => {
                term = Some(id);
                (None, None)
            }
            Some(Pick::OutputView(name, _)) => {
                channel = Some(name.clone());
                (None, None)
            }
            None => (None, None),
        };
        self.palette_unpreview(false, cx);
        self.close_palette(window, cx);
        // From where focus went back to, like a keybinding.
        if let Some(action) = command {
            window.dispatch_action(action, cx);
        }
        if let Some(ix) = view {
            self.open_view(ix, window, cx);
        }
        if let Some(id) = term {
            self.focus_terminal(window, cx);
            self.activate_terminal(id, window, cx);
        }
        if let Some(name) = channel {
            self.show_channel(&name, cx);
        }
        if let Some(path) = file {
            self.open_path(path, window, cx);
        }
        if let (Some((line, col)), Some(doc)) = (pos, self.docs.get(self.active)) {
            reveal(&doc.editor.clone(), Position::new(line, col), window, cx);
        }
    }

    /// Shows and focuses `VIEWS[ix]`; unlike its activity bar item, never hides it.
    fn open_view(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        match ix {
            0 => window.dispatch_action(Box::new(ShowExplorer), cx),
            1 => window.dispatch_action(Box::new(ShowSearch), cx),
            2 => (self.sidebar, self.view) = (true, View::Scm),
            3 => (self.sidebar, self.view) = (true, View::Run),
            4 => (self.sidebar, self.view) = (true, View::Extensions),
            _ if ix - SIDE_BAR_VIEWS == TERMINAL => self.focus_terminal(window, cx),
            _ => (self.panel, self.panel_tab) = (true, ix - SIDE_BAR_VIEWS),
        }
        cx.notify();
    }

    pub(super) fn render_palette(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let p = &mut self.palette;
        if !p.open {
            return None;
        }
        // The list keeps its highlighted row across updates; VS Code starts over.
        if let Some(row) = p.preselect.take() {
            let state = p.state.clone();
            window.on_next_frame(move |window, cx| {
                state.update(cx, |s, cx| {
                    s.set_selected_index(Some(IndexPath::new(row)), window, cx)
                })
            });
        }

        let this = cx.weak_entity();
        let (this2, this3) = (this.clone(), this.clone());
        let associate_placeholder;
        let placeholder = if let Some(items) = &p.items {
            &items.placeholder
        } else if p.profiles.is_some() {
            "Select your default terminal profile"
        } else if let Some(Rename::Terminal(_)) = p.renaming {
            "Enter terminal name"
        } else if let Some(Rename::Host(_, placeholder)) = &p.renaming {
            placeholder
        } else if p.renaming.is_some() {
            "Enter the new name"
        } else if p.iconing.is_some() {
            "Search icons"
        } else if p.coloring.is_some() {
            "Choose a color for the terminal"
        } else if p.joining {
            "Select a terminal to join"
        } else if p.eol {
            "Select End of Line Sequence"
        } else if let Some((key, _)) = &p.associating {
            associate_placeholder = format!(
                "Select Language Mode to Associate with '{}'",
                key.trim_start_matches('*')
            );
            &associate_placeholder
        } else if p.lang {
            "Select Language Mode"
        } else if p.indent == Some(IndentStep::Action) || p.encoding == Some(EncodingStep::Action) {
            "Select Action"
        } else if let Some(EncodingStep::List { reopen, .. }) = p.encoding {
            match reopen {
                true => "Select File Encoding to Reopen File",
                false => "Select File Encoding to Save with",
            }
        } else if p.indent.is_some() {
            "Select Tab Size for Current File"
        } else if p.query.starts_with('>') {
            ""
        } else {
            "Search files by name (append : to go to line or @ to go to symbol)"
        };
        let empty = p.empty.clone();
        // VS Code names the highlighted icon under the grid.
        let icon_name = p.iconing.and_then(|_| match p.shown.get(p.selected) {
            Some(Pick::Icons(icons)) => icons.get(p.col).map(|i| (i.0, i.2.clone())),
            _ => None,
        });
        // VS Code's quickInput sizing: 62% wide up to 600px, list 40% of the
        // window in whole 44px steps, at most 20 rows of 22px.
        let viewport = window.viewport_size();
        let mut width = (viewport.width.as_f32() * 0.62).min(600.);
        let mut list_h = ((viewport.height.as_f32() * 0.4 / 44.).floor() * 44. + 6.).min(440.);
        // Change Icon's box is VS Code's 486x260: input, five rows, the name.
        if p.iconing.is_some() {
            (width, list_h) = (486., 180.);
        }

        let mut list = Command::new(&p.state)
            .bordered(false)
            .filterable(false)
            .max_h(px(list_h))
            .placeholder(placeholder);
        let commands = commands(&p.focus_path);
        let defs = p.defs.as_deref().unwrap_or_default();
        let ws_defs = p.ws_defs.as_deref().map_or(&[][..], |w| w.as_slice());
        let profiles = p.profiles.as_deref().unwrap_or_default();
        for (ix, pick) in p.shown.iter().enumerate() {
            let group = p
                .groups
                .iter()
                .find(|(row, _)| *row == ix)
                .map(|(_, g)| g.clone());
            if group.is_some() && ix > 0 {
                list = list.separator();
            }
            let row = |icon, label: &str, label_hits: &Vec<usize>| Row {
                icon,
                label: label.to_string().into(),
                label_hits: label_hits.clone(),
                desc: SharedString::default(),
                desc_hits: Vec::new(),
                action: None,
                group: group.clone(),
                selected: ix == p.selected,
            };
            list = list.item(match pick {
                Pick::Line(.., label) => row(RowIcon::None, label, &Vec::new()).item(),
                Pick::Profile(pr, hits) => row(RowIcon::None, &profiles[*pr].name, hits).item(),
                Pick::Color(Some(ix), hits) => {
                    let icon = RowIcon::Symbol(icon::CIRCLE_FILLED, c(ANSI[*ix]));
                    row(icon, COLORS[*ix], hits).item()
                }
                Pick::Icons(icons) => {
                    let (icons, sel, this) = (icons.clone(), ix == p.selected, this.clone());
                    let at = if sel { Some(p.col) } else { None };
                    CommandItem::new()
                        .label(icons[0].0)
                        .child(move |_, _| icon_row(&icons, at, ix, this.clone()))
                }
                Pick::Color(None, hits) => row(RowIcon::None, "Reset to default", hits).item(),
                Pick::Join(_, name, ch, color, hits) => {
                    row(RowIcon::Symbol(*ch, *color), name, hits).item()
                }
                Pick::Eol(crlf, hits) => row(RowIcon::None, eol(*crlf), hits).item(),
                Pick::View(ix, hits) => row(RowIcon::None, VIEWS[*ix], hits).item(),
                Pick::Item(ix, hits) => {
                    let (label, desc) = &p.items.as_ref().unwrap().rows[*ix];
                    Row {
                        desc: desc.clone().into(),
                        ..row(RowIcon::None, label, hits)
                    }
                    .item()
                }
                Pick::TermView(_, label, hits) | Pick::OutputView(label, hits) => {
                    row(RowIcon::None, label, hits).item()
                }
                Pick::Lang(id, label, hits) => {
                    let current = matches!(&p.associating, Some((_, by_name)) if by_name == id);
                    Row {
                        desc: if current { "Current Association" } else { "" }.into(),
                        ..row(RowIcon::Lang(id), label, hits)
                    }
                    .item()
                }
                Pick::Associate(label, hits) => row(RowIcon::None, label, hits).item(),
                Pick::Indent(ix, hits) if p.indent == Some(IndentStep::Action) => {
                    row(RowIcon::None, INDENT_ACTIONS[*ix], hits).item()
                }
                Pick::EncodingAction(ix, hits) => {
                    row(RowIcon::None, ENCODING_ACTIONS[*ix], hits).item()
                }
                Pick::Encoding(e, desc, hits) => Row {
                    desc: (*desc).into(),
                    ..row(RowIcon::None, e.label(), hits)
                }
                .item(),
                Pick::Indent(n, hits) => {
                    // The default editor.tabSize is VS Code's configured one.
                    let current = self.docs.get(self.active).map(|d| d.indent.tab_size);
                    let desc = match (*n == TAB_SIZE, current == Some(*n)) {
                        (true, true) => "Configured Tab Size",
                        (true, false) => "Default Tab Size",
                        (false, true) => "Current Tab Size",
                        _ => "",
                    };
                    Row {
                        desc: desc.into(),
                        ..row(RowIcon::None, &n.to_string(), hits)
                    }
                    .item()
                }
                Pick::Command(cmd, hits) => {
                    let (label, action) = &commands[*cmd];
                    Row {
                        action: Some(action.boxed_clone()),
                        ..row(RowIcon::None, label, hits)
                    }
                    .item()
                }
                Pick::Symbol(def, hits, container_hits) => {
                    let def = &defs[*def];
                    let (ch, color) = symbol_icon(def.kind);
                    let container = def.container.map(|c| defs[c].name.clone());
                    Row {
                        desc: container.unwrap_or_default().into(),
                        desc_hits: container_hits.clone(),
                        ..row(RowIcon::Symbol(ch, color), &def.name, hits)
                    }
                    .item()
                }
                Pick::WorkspaceSymbol(ix, hits) => {
                    let (rel, def) = &ws_defs[*ix];
                    let (ch, color) = symbol_icon(def.kind);
                    Row {
                        desc: rel.clone().into(),
                        ..row(RowIcon::Symbol(ch, color), &def.name, hits)
                    }
                    .item()
                }
                Pick::File(rel, label_hits, desc_hits) => {
                    let (name, dir) = parts(rel);
                    Row {
                        desc: dir.to_string().into(),
                        desc_hits: desc_hits.clone(),
                        ..row(RowIcon::File, name, label_hits)
                    }
                    .item()
                }
            });
        }

        Some(
            div()
                .id("palette")
                .track_focus(&p.focus)
                .absolute()
                // Over the command center, as in VS Code.
                .top(px(6.))
                .left(relative(0.5))
                .ml(px(-width / 2.))
                .w(px(width))
                // quick-input-widget: cornerRadius-xLarge, large in Modern UI.
                .rounded(popup_radius(12.))
                // quickInput.background
                .bg(c(0x202122))
                .border_1()
                .border_color(col::border())
                .shadow_lg()
                .text_size(px(13.))
                // Escape closes straight away, even with a query (the default clears it first).
                .capture_action(cx.listener(|this, _: &Cancel, window, cx| {
                    cx.stop_propagation();
                    this.close_palette(window, cx);
                }))
                // Rename's input has no rows for the list to confirm.
                .capture_action(cx.listener(|this, _: &Confirm, window, cx| {
                    if let Some(what) = this.palette.renaming.take() {
                        cx.stop_propagation();
                        let name = this.palette.query.clone();
                        this.close_palette(window, cx);
                        match what {
                            Rename::Terminal(id) => this.set_terminal_name(id, &name, cx),
                            Rename::Symbol(path, at) => {
                                this.rename_symbol_to(path, at, name, window, cx)
                            }
                            Rename::Host(id, _) => this.ext.answer(id, name.into()),
                        }
                    }
                }))
                .on_mouse_down_out(
                    cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
                )
                // Left and right walk Change Icon's grid, wrapping across rows.
                .capture_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                    let p = &mut this.palette;
                    let step = match e.keystroke.key.as_str() {
                        "left" => -1,
                        "right" => 1,
                        _ => return,
                    };
                    let Some(Pick::Icons(icons)) = p.shown.get(p.selected) else {
                        return;
                    };
                    cx.stop_propagation();
                    let total = (p.shown.len() - 1) * ICON_COLS
                        + match p.shown.last() {
                            Some(Pick::Icons(last)) => last.len(),
                            _ => 0,
                        };
                    let at = (p.selected * ICON_COLS + p.col.min(icons.len() - 1)) as isize;
                    let at = (at + step).clamp(0, total as isize - 1) as usize;
                    p.col = at % ICON_COLS;
                    if at / ICON_COLS != p.selected {
                        p.selected = at / ICON_COLS;
                        p.preselect = Some(p.selected);
                    }
                    cx.notify();
                }))
                .child(
                    list.on_query(move |q, _, cx| {
                        let q = q.to_string();
                        this.update(cx, |this, cx| this.palette_query(q, cx)).ok();
                    })
                    .on_select(move |ix, _, cx| {
                        this3
                            .update(cx, |this, cx| {
                                this.palette.selected = ix.row;
                                this.preview_theme(ix.row, cx);
                                if this.palette.query.starts_with([':', '@']) {
                                    this.palette_preview(ix.row, cx);
                                }
                                cx.notify();
                            })
                            .ok();
                    })
                    .on_confirm(move |ix, window, cx| {
                        this2
                            .update(cx, |this, cx| this.palette_confirm(ix.row, window, cx))
                            .ok();
                    })
                    .when_some(icon_name, |list, (name, hits)| {
                        list.footer(move |_, _, _| {
                            div()
                                .px(px(10.))
                                .py(px(6.))
                                .text_color(col::fg_muted())
                                .child(
                                    StyledText::new(name)
                                        .with_highlights(highlights(name, &hits, false)),
                                )
                        })
                    })
                    .empty(move |_, _, _| {
                        div()
                            .p(px(8.))
                            .text_color(col::fg_muted())
                            .child(empty.clone())
                    }),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        Pick, command_matches, defs, fuzzy_score, goto_line, highlights, prepare_query, rank_files,
        rank_symbols, reindent, score_fuzzy, split_line, status_column, trim_trailing,
        workspace_defs,
    };

    #[test]
    fn workspace_symbols() {
        let root = std::env::temp_dir().join("vsn-workspace-defs");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(root.join("b.txt"), "fn beta() {}\n").unwrap();
        let files = [
            "a.rs".to_string(),
            "b.txt".to_string(),
            "gone.rs".to_string(),
        ];
        let got = workspace_defs(&root, &serde_json::Value::Null, &files);
        std::fs::remove_dir_all(&root).ok();
        let got: Vec<_> = got
            .iter()
            .map(|(rel, d)| (rel.as_str(), d.name.as_str()))
            .collect();
        assert_eq!(got, [("a.rs", "alpha")]);
    }

    #[test]
    fn indentation() {
        assert_eq!(
            reindent("a\n\tb\n \tc\r\n", 4, false),
            "a\n    b\n    c\r\n"
        );
        assert_eq!(reindent("      b\n  c", 4, true), "\t  b\n  c");
        assert_eq!(trim_trailing("a \t\r\nb  \nc "), "a\r\nb\nc");
    }

    fn ranked(query: &str, history: &[&str], files: &[&str]) -> Vec<String> {
        let own = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (picks, _) = rank_files(query, Path::new("C:\\ws"), &own(history), &own(files));
        picks
            .into_iter()
            .map(|p| match p {
                Pick::File(rel, ..) => rel,
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn fuzzy_ranks() {
        assert_eq!(
            score_fuzzy("workbench.rs", "wbr", true).unwrap().1,
            vec![0, 4, 10]
        );
        // Contiguous: only where the query occurs whole.
        assert!(score_fuzzy("workbench.rs", "wbr", false).is_none());
        assert_eq!(
            score_fuzzy("workbench.rs", "bench", false).unwrap().1,
            vec![4, 5, 6, 7, 8]
        );
        assert!(score_fuzzy("workbench.rs", "xyz", true).is_none());

        let files = [
            "src\\some\\each\\archive.rs",
            "src\\workbench\\search.rs",
            "src\\main.rs",
            "format.rs",
        ];
        assert_eq!(
            ranked("search", &[], &files)[0],
            "src\\workbench\\search.rs"
        );
        assert_eq!(ranked("ma", &[], &files)[0], "src\\main.rs");
        assert_eq!(ranked("src/ma", &[], &files)[0], "src\\main.rs");
        // Every piece must match; quotes ask for a contiguous match.
        assert_eq!(ranked("src main", &[], &files), ["src\\main.rs"]);
        assert!(ranked("\"mn\"", &[], &files).is_empty());
        assert_eq!(
            prepare_query("\"ma\" rs").pieces,
            [("ma".into(), true), ("rs".into(), false)]
        );
        // History comes first and is left out of the file results.
        assert_eq!(ranked("rs", &["format.rs"], &files)[0], "format.rs");
        assert_eq!(ranked("", &["format.rs"], &files), ["format.rs"]);
        // An absolute path matches outright.
        assert_eq!(ranked("C:/ws/src/main.rs", &[], &files)[0], "src\\main.rs");
        // Hits past the end of the folder (the separator) are skipped.
        assert!(highlights("src", &[3], false).is_empty());

        assert!(command_matches("tog term", "View: Toggle Terminal").is_some());
        assert_eq!(
            command_matches("vtt", "View: Toggle Terminal").unwrap(),
            vec![0, 6, 13]
        );
        assert_eq!(
            command_matches("save", "File: Save All").unwrap(),
            vec![6, 7, 8, 9]
        );
        assert!(command_matches("xyz", "View: Toggle Terminal").is_none());
    }

    #[test]
    fn line_suffix() {
        assert_eq!(split_line("main.rs:12"), ("main.rs", Some((11, 0))));
        assert_eq!(split_line("main.rs#12,5"), ("main.rs", Some((11, 4))));
        assert_eq!(split_line("main.rs(12)"), ("main.rs", Some((11, 0))));
        assert_eq!(split_line("main.rs:"), ("main.rs", Some((0, 0))));
        assert_eq!(split_line("a:b"), ("a:b", None));
        assert_eq!(split_line("C:\\src\\main.rs"), ("C:\\src\\main.rs", None));
        let text = "one\ntwo\nthree";
        assert_eq!(goto_line("2", text, 4).unwrap().0, 1);
        assert_eq!(
            goto_line("-1:9", text, 4).unwrap(),
            (2, 5, "Press 'Enter' to go to line 3 at column 6.".into())
        );
        assert_eq!(goto_line(" 3:-1", text, 4).unwrap().1, 4);
        assert_eq!(goto_line("2abc", text, 4).unwrap().0, 1);
        assert_eq!(
            goto_line("9,", text, 4).unwrap().2,
            "Press 'Enter' to go to line 3 or enter a column number (from 1 to 6)."
        );
        assert_eq!(
            goto_line("", text, 4).unwrap_err(),
            "Type a line number to go to (from 1 to 3)."
        );
        // `::offset`, 1-based; negative counts from the end.
        assert_eq!(
            goto_line(":5", text, 4).unwrap(),
            (1, 0, "Press 'Enter' to go to line 2 at column 1.".into())
        );
        assert_eq!(goto_line(":-1", text, 4).unwrap().0, 2);
        assert_eq!(goto_line(":-1", text, 4).unwrap().1, 5);
        assert_eq!(
            goto_line(":", text, 4).unwrap_err(),
            "Type a character position to go to (from 1 to 13)."
        );
        // Columns are visible ones, a tab reaching the next multiple of 4.
        let tabbed = "\tab";
        assert_eq!(goto_line("1:5", tabbed, 4).unwrap().1, 1);
        assert_eq!(goto_line("1:3", tabbed, 4).unwrap().1, 0);
        assert_eq!(
            goto_line("1,", tabbed, 4).unwrap().2,
            "Press 'Enter' to go to line 1 or enter a column number (from 1 to 7)."
        );
        assert_eq!(
            goto_line(":2", tabbed, 4).unwrap(),
            (0, 1, "Press 'Enter' to go to line 1 at column 5.".into())
        );
        // CJK and emoji graphemes take two columns; the status bar widens tabs only.
        assert_eq!(goto_line("1:3", "日本x", 4).unwrap().1, 1);
        assert_eq!(
            goto_line("1,", "日本x", 4).unwrap().2,
            "Press 'Enter' to go to line 1 or enter a column number (from 1 to 6)."
        );
        assert_eq!(goto_line("1:3", "👍🏽x", 4).unwrap().1, 2);
        assert_eq!(status_column(tabbed, 1, 4), 5);
        assert_eq!(status_column(tabbed, 1, 2), 3);
        assert_eq!(status_column("日x", 1, 4), 2);
    }

    #[test]
    fn rust_defs() {
        let src = "struct Foo;\nimpl Foo {\n    fn bar() {}\n}\nfn baz() {}\n";
        let defs = defs("rust", src);
        let got: Vec<_> = defs
            .iter()
            .map(|d| (d.name.as_str(), d.kind, d.line, d.col, d.container))
            .collect();
        assert_eq!(
            got,
            [
                ("Foo", "structs", 0, 7, None),
                ("impl Foo", "objects", 1, 5, None),
                ("bar", "methods", 2, 7, Some(1)),
                ("baz", "functions", 4, 3, None),
            ]
        );
        let names = |q: &str| -> Vec<usize> {
            rank_symbols(q, &defs, 0)
                .0
                .into_iter()
                .map(|p| match p {
                    Pick::Symbol(ix, ..) => ix,
                    _ => unreachable!(),
                })
                .collect()
        };
        // The rest of the query matches the container.
        assert_eq!(names("ba foo"), [2]);
        assert_eq!(names("ba"), [2, 3]);
        let (_, groups, _) = rank_symbols(":", &defs, 0);
        let groups: Vec<_> = groups
            .iter()
            .map(|(row, g)| (*row, g.to_string()))
            .collect();
        assert_eq!(groups[0], (0, "functions (1)".into()));
        // An empty query starts at the innermost symbol around the cursor.
        assert_eq!(rank_symbols("", &defs, src.find("bar").unwrap()).2, Some(2));

        // Camel humps and word starts beat plain letters.
        assert_eq!(fuzzy_score("fb", "fooBar", 0).unwrap().1, [0, 3]);
        assert_eq!(fuzzy_score("b", "abc_bar", 0).unwrap().1, [4]);
        assert!(fuzzy_score("xb", "fooBar", 0).is_none());
    }

    #[test]
    fn tagged_defs() {
        let src = "struct point { int x; };\ntypedef int num;\nstatic char *name(void) { return 0; }\nint add(int a, int b);\n";
        let defs = defs("c", src);
        let got: Vec<_> = defs.iter().map(|d| (d.name.as_str(), d.kind)).collect();
        assert_eq!(
            got,
            [
                ("point", "structs"),
                ("num", "type parameters"),
                ("name", "functions"),
                ("add", "functions"),
            ]
        );
        // The whole definition, not just its declarator.
        assert_eq!(defs[2].range.start, src.find("static").unwrap());
        // TypeScript gets JavaScript's tags too.
        let ts = super::defs("typescript", "const f = () => 1;\n");
        assert_eq!(
            ts.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            ["f"]
        );
    }
}
