//! The VS Code workbench: title bar, activity bar, side bar, editor group,
//! panel and status bar.

use std::{
    collections::{HashMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use futures::StreamExt as _;
use notify::{EventKind, RecursiveMode, Watcher as _, event::ModifyKind};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_resizable,
    input::{self, Editor, EditorState, Input, InputEvent, InputState, RopeExt as _, TabSize},
    menu::{DropdownMenu as _, PopupMenu},
    resizable_panel,
    tooltip::Tooltip,
    v_resizable,
};
use gpui_kit::{
    base::{h_flex, v_flex},
    prelude::FluentBuilder as _,
    *,
};

mod debug;
mod palette;
mod search;
mod terminal;

use crate::theme::{action_button, c, codicon, col, file_icon, icon, language_id, language_label};

actions!(
    vsn,
    [
        OpenFile,
        OpenFolder,
        CloseFolder,
        Save,
        SaveAll,
        CloseEditor,
        Quit,
        ToggleSidebar,
        TogglePanel,
        ToggleTerminal,
        ShowExplorer,
        ShowSearch,
        ReplaceInFiles,
        ReplaceAll,
        ShowScm,
        ShowRun,
        ShowExtensions,
        CommandPalette,
        QuickOpen,
        StartDebugging,
        RunWithoutDebugging,
        StopDebugging,
        RestartDebugging,
        Pause,
        StepOver,
        StepInto,
        StepOut,
        ToggleBreakpoint,
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
        ScrollTerminalUp,
        ScrollTerminalDown,
        ScrollTerminalUpPage,
        ScrollTerminalDownPage,
        ScrollTerminalToTop,
        ScrollTerminalToBottom,
        SelectDefaultProfile,
        NewTextFile,
        ShowNotifications,
        HideNotifications,
        ClearAllNotifications,
        Noop,
    ]
);

pub fn key_bindings(mapper: &dyn PlatformKeyboardMapper) -> Vec<KeyBinding> {
    // Windows reports shift+symbol keys as the shifted char ("ctrl-%"), so
    // those go through the platform mapper; `KeyBinding::new` skips it.
    let shifted = |keys: &str, action: Box<dyn Action>, context: Option<&str>| {
        let context = context.map(|c| KeyBindingContextPredicate::parse(c).unwrap().into());
        KeyBinding::load(keys, action, context, false, None, mapper).unwrap()
    };
    vec![
        KeyBinding::new("ctrl-o", OpenFile, None),
        KeyBinding::new("ctrl-k ctrl-o", OpenFolder, None),
        KeyBinding::new("ctrl-s", Save, None),
        KeyBinding::new("ctrl-k s", SaveAll, None),
        KeyBinding::new("ctrl-w", CloseEditor, None),
        KeyBinding::new("ctrl-f4", CloseEditor, None),
        KeyBinding::new("alt-f4", Quit, None),
        KeyBinding::new("ctrl-b", ToggleSidebar, None),
        KeyBinding::new("ctrl-j", TogglePanel, None),
        KeyBinding::new("ctrl-`", ToggleTerminal, None),
        KeyBinding::new("ctrl-shift-e", ShowExplorer, None),
        KeyBinding::new("ctrl-shift-f", ShowSearch, None),
        KeyBinding::new("ctrl-shift-h", ReplaceInFiles, None),
        KeyBinding::new("ctrl-alt-enter", ReplaceAll, None),
        KeyBinding::new("ctrl-shift-g", ShowScm, None),
        KeyBinding::new("ctrl-shift-d", ShowRun, None),
        KeyBinding::new("ctrl-shift-x", ShowExtensions, None),
        KeyBinding::new("ctrl-shift-p", CommandPalette, None),
        KeyBinding::new("f1", CommandPalette, None),
        KeyBinding::new("ctrl-p", QuickOpen, None),
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
    ]
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Explorer,
    Search,
    Scm,
    Run,
    Extensions,
}

const PANEL_TABS: [&str; 5] = ["Problems", "Output", "Debug Console", "Terminal", "Ports"];
const TERMINAL: usize = 3;
/// VS Code's default `editor.tabSize`, spaces for indents.
const TAB_SIZE: usize = 4;

struct Doc {
    /// `Untitled-N` for a new file that was never saved.
    path: PathBuf,
    editor: Entity<EditorState>,
    dirty: bool,
    crlf: bool,
    encoding: Encoding,
    /// Shows VS Code's binary placeholder instead of the editor (left empty).
    binary: bool,
    untitled: bool,
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

impl Encoding {
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
        let utf16 = |bytes: &[u8], be: bool| {
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
        let utf8 = |bytes| String::from_utf8_lossy(bytes).into_owned();
        match bytes {
            [0xEF, 0xBB, 0xBF, rest @ ..] => return Some((utf8(rest), Self::Utf8Bom)),
            [0xFF, 0xFE, rest @ ..] => return Some((utf16(rest, false), Self::Utf16Le)),
            [0xFE, 0xFF, rest @ ..] => return Some((utf16(rest, true), Self::Utf16Be)),
            _ => {}
        }
        let head = &bytes[..bytes.len().min(512)];
        if !head.contains(&0) {
            return Some((utf8(bytes), Self::Utf8));
        }
        // ASCII as UTF-16 LE has its zero bytes at exactly the odd indexes, BE at the even ones.
        let zeros_at_odd =
            |odd: bool| (head.iter().enumerate()).all(|(i, &b)| (b == 0) == ((i % 2 == 1) == odd));
        if zeros_at_odd(true) {
            Some((utf16(bytes, false), Self::Utf16Le))
        } else if zeros_at_odd(false) {
            Some((utf16(bytes, true), Self::Utf16Be))
        } else {
            None
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
    panel_tab: usize,
    search: Entity<InputState>,
    find: search::Find,
    palette: palette::Palette,
    /// A warning toast over the status bar, whether the mouse is over it,
    /// and the timer that hides it.
    toast: Option<(SharedString, bool, Task<()>)>,
    /// Every notification, newest first, as the bell's center lists them.
    notes: Vec<SharedString>,
    /// Notifications added since the center was last open: the bell's dot.
    unseen: usize,
    /// Whether the bell's notification center is open.
    center: bool,
    debug: debug::Debug,
    /// Spawned when the Terminal tab first shows; the error if the shell didn't start.
    terminals: terminal::Terminals,
    commit: Entity<InputState>,
    ext_search: Entity<InputState>,
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
            dirs: HashMap::new(),
            watcher: None,
            quitting: false,
            root_open: true,
            docs: Vec::new(),
            active: 0,
            view: View::Explorer,
            sidebar: true,
            panel: true,
            panel_tab: TERMINAL,
            search: input("Search", window, cx),
            commit: input("Message (Ctrl+Enter to commit)", window, cx),
            ext_search: input("Search Extensions in Marketplace", window, cx),
            focus: cx.focus_handle(),
            find: search::Find::new(window, cx),
            palette: palette::Palette::new(window, cx),
            toast: None,
            notes: Vec::new(),
            unseen: 0,
            center: false,
            debug: debug::Debug::new(window, cx),
            terminals: Default::default(),
        };
        this.init_search(window, cx);
        this.init_debug(window, cx);
        this.watch_root(cx);
        // The title bar's close button (and Alt+F4) land here.
        let wb = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            wb.update(cx, |this, cx| this.can_close(window, cx))
                .unwrap_or(true)
        });
        this
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
        let crlf = text.contains("\r\n");
        let editor = self.new_editor(&path, text, window, cx);
        self.docs.push(Doc {
            path,
            editor,
            dirty: false,
            crlf,
            encoding,
            binary,
            untitled,
        });
        self.activate(self.docs.len() - 1, window, cx);
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

    /// The binary placeholder's Open Anyway: the file as UTF-8, whatever it holds.
    fn open_anyway(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bytes) = self.docs.get(ix).and_then(|d| fs::read(&d.path).ok()) else {
            return;
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let path = self.docs[ix].path.clone();
        let editor = self.new_editor(&path, text, window, cx);
        let doc = &mut self.docs[ix];
        doc.editor = editor;
        doc.binary = false;
        self.activate(ix, window, cx);
    }

    fn new_editor(
        &mut self,
        path: &Path,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<EditorState> {
        let lang = editor_language(path);
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(lang)
                .searchable(true)
                .tab_size(TabSize {
                    tab_size: TAB_SIZE,
                    hard_tabs: false,
                })
        });
        editor.update(cx, |e, cx| e.set_value(text, window, cx));
        self.attach_breakpoints(path, &editor, cx);
        cx.subscribe(&editor, |this, editor, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change)
                && let Some(doc) = this.docs.iter_mut().find(|d| d.editor == editor)
            {
                doc.dirty = true;
                let path = doc.path.clone();
                this.refresh_search_file(&path, cx);
                this.breakpoints_moved(&path, &editor, cx);
            }
            // Cursor moves also land here; re-render keeps "Ln, Col" current.
            cx.notify();
        })
        .detach();
        editor
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = ix;
        if let Some(doc) = self.docs.get(ix) {
            if !doc.untitled {
                self.palette.remember(self.root.as_deref(), &doc.path, cx);
            }
            doc.editor.update(cx, |e, cx| e.focus(window, cx));
        }
        cx.notify();
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
        let text = doc.editor.read(cx).value();
        match fs::write(&doc.path, doc.encoding.encode(&text)) {
            Ok(()) => doc.dirty = false,
            Err(err) => eprintln!("save {}: {err}", doc.path.display()),
        }
        cx.notify();
    }

    /// Asks where to save an untitled editor, then reopens it as that file
    /// (its language follows the new name; undo history starts over).
    fn save_as(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let doc = &self.docs[ix];
        let editor = doc.editor.clone();
        let dir = (self.root.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let path = cx.prompt_for_new_path(&dir, Some(&doc.path.to_string_lossy()));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                // The tab may have moved or closed while the dialog was open.
                let Some(ix) = this.docs.iter().position(|d| d.editor == editor) else {
                    return;
                };
                let text = editor.read(cx).value().to_string();
                if let Err(err) = fs::write(&path, &text) {
                    eprintln!("save {}: {err}", path.display());
                    return;
                }
                let editor = this.new_editor(&path, text, window, cx);
                let doc = &mut this.docs[ix];
                doc.editor = editor;
                doc.path = path;
                doc.untitled = false;
                doc.dirty = false;
                if ix == this.active {
                    this.activate(ix, window, cx);
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

    fn remove(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.docs.remove(ix);
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
                    this.root = Some(path);
                    this.expanded.clear();
                    this.root_open = true;
                    this.watch_root(cx);
                    cx.notify();
                } else if let Some(window) = cx.active_window() {
                    let this = cx.entity();
                    window
                        .update(cx, |_, window, cx| {
                            this.update(cx, |this, cx| this.open_path(path, window, cx))
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

    /// Ctrl+`: hides the focused terminal, else shows and focuses it.
    fn toggle_terminal(&mut self, _: &ToggleTerminal, window: &mut Window, cx: &mut Context<Self>) {
        if !(self.panel && self.panel_tab == TERMINAL && self.terminal_focused(window)) {
            return self.focus_terminal(window, cx);
        }
        self.panel = false;
        self.focus_editor(window, cx);
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
            Button::new(name)
                .ghost()
                .compact()
                .label(name)
                .h(px(22.))
                .px(px(7.))
                .text_size(px(13.))
                .text_color(col::fg())
                .dropdown_menu(move |menu, window, cx| {
                    app_menu(name, menu.min_w(px(240.)), window, cx)
                })
        });

        let layout_toggle = |id: &'static str, on: bool, on_icon: char, off_icon: char| {
            action_button(id, if on { on_icon } else { off_icon }).text_color(col::fg())
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
                    .child(h_flex().flex_none().children(menubar))
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
                    .child(action_button("back", icon::ARROW_LEFT).text_color(col::fg_muted()))
                    .child(action_button("forward", icon::ARROW_RIGHT).text_color(col::fg_dim()))
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
                            .child(title)
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
                            ))
                            .child(
                                action_button("customize-layout", icon::LAYOUT)
                                    .text_color(col::fg()),
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

    fn render_activitybar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let item = |id: &'static str, ch: char, view: Option<View>, cx: &mut Context<Self>| {
            let active = self.sidebar && view == Some(self.view);
            div()
                .id(id)
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .size(px(48.))
                .cursor_pointer()
                .text_color(if active { col::fg() } else { col::fg_muted() })
                .hover(|s| s.text_color(col::fg()))
                .when(active, |d| {
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
                .child(codicon(ch).text_size(px(24.)).line_height(px(24.)))
                .when_some(view, |d, view| {
                    d.on_click(cx.listener(move |this, _, _, cx| this.show(view, cx)))
                })
        };

        v_flex()
            .w(px(48.))
            .flex_none()
            .h_full()
            .justify_between()
            .bg(col::bg())
            .border_r_1()
            .border_color(col::border())
            .child(
                v_flex()
                    .child(item("act-explorer", icon::FILES, Some(View::Explorer), cx))
                    .child(item("act-search", icon::SEARCH, Some(View::Search), cx))
                    .child(item("act-scm", icon::SOURCE_CONTROL, Some(View::Scm), cx))
                    .child(item("act-run", icon::DEBUG_ALT, Some(View::Run), cx))
                    .child(item(
                        "act-ext",
                        icon::EXTENSIONS,
                        Some(View::Extensions),
                        cx,
                    )),
            )
            .child(
                v_flex()
                    .child(item("act-account", icon::ACCOUNT, None, cx))
                    .child(item("act-manage", icon::SETTINGS_GEAR, None, cx)),
            )
    }

    // ---- side bar ----------------------------------------------------------

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (title, actions): (&str, &[char]) = match self.view {
            View::Explorer => ("Explorer", &[icon::ELLIPSIS]),
            View::Search => (
                "Search",
                &[
                    icon::REFRESH,
                    icon::CLEAR_ALL,
                    icon::COLLAPSE_ALL,
                    icon::ELLIPSIS,
                ],
            ),
            View::Scm => ("Source Control", &[icon::ELLIPSIS]),
            View::Run => ("Run and Debug", &[icon::ELLIPSIS]),
            View::Extensions => (
                "Extensions",
                &[icon::FILTER, icon::REFRESH, icon::CLEAR_ALL, icon::ELLIPSIS],
            ),
        };
        let header = h_flex()
            .h(px(35.))
            .flex_none()
            .pl(px(20.))
            .pr(px(8.))
            .justify_between()
            .text_size(px(11.))
            .text_color(col::fg())
            .child(title.to_uppercase())
            .when(self.view == View::Run, |d| {
                d.children(self.render_run_title(cx))
            })
            .child(
                h_flex().gap(px(4.)).children(
                    actions
                        .iter()
                        .enumerate()
                        .map(|(i, &ch)| action_button(("sb-act", i), ch)),
                ),
            );

        let body = match self.view {
            View::Explorer => self.render_explorer(cx).into_any_element(),
            View::Search => self.render_search(cx).into_any_element(),
            View::Scm => v_flex()
                .child(section_header("scm-repos", "Changes", true, None))
                .child(
                    v_flex()
                        .px(px(12.))
                        .pt(px(4.))
                        .gap(px(6.))
                        .child(search_box(&self.commit))
                        .child(primary_button("commit", "Commit", Some(icon::CHECK))),
                )
                .into_any_element(),
            View::Run => self.render_run(cx),
            View::Extensions => v_flex()
                .child(
                    div()
                        .px(px(12.))
                        .pb(px(8.))
                        .child(search_box(&self.ext_search)),
                )
                .child(section_header("ext-installed", "Installed", false, None))
                .child(section_header(
                    "ext-recommended",
                    "Recommended",
                    false,
                    None,
                ))
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .bg(col::bg())
            .text_color(col::fg())
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
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
        let root_actions = [
            icon::NEW_FILE,
            icon::NEW_FOLDER,
            icon::REFRESH,
            icon::COLLAPSE_ALL,
        ];

        v_flex()
            .size_full()
            .map(|d| match self.root_name() {
                Some(name) => d
                    .child(
                        section_header("explorer-root", &name, self.root_open, Some(&root_actions))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.root_open = !this.root_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        uniform_list(
                            "tree",
                            rows.len(),
                            cx.processor(move |this, range: Range<usize>, _, cx| {
                                range
                                    .map(|ix| this.render_tree_row(&rows[ix], cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .flex_1()
                        .min_h_0(),
                    ),
                None => d
                    .child(section_header("no-folder", "No Folder Opened", true, None))
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
                    .child(section_header("outline", "Outline", false, None)),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(col::border())
                    .child(section_header("timeline", "Timeline", false, None)),
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

    /// Visible explorer rows (path, depth, is_dir), reading folders only on a cache miss.
    fn tree_rows(&mut self, dir: &Path, depth: usize, rows: &mut Vec<(PathBuf, usize, bool)>) {
        let entries = self
            .dirs
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                let mut entries: Vec<_> = fs::read_dir(dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| (e.path(), e.file_type().is_ok_and(|t| t.is_dir())))
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

    fn render_tree_row(
        &self,
        (path, depth, is_dir): &(PathBuf, usize, bool),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (depth, is_dir) = (*depth, *is_dir);
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let open = is_dir && self.expanded.contains(path);
        let active = self.docs.get(self.active).is_some_and(|d| &d.path == path);
        let click_path = path.clone();
        h_flex()
            .id(ElementId::Name(path.to_string_lossy().to_string().into()))
            .h(px(22.))
            .pl(px(8. + depth as f32 * 8.))
            .pr(px(12.))
            .text_size(px(13.))
            .whitespace_nowrap()
            .cursor_pointer()
            .border_1()
            .border_color(transparent_black())
            .when(active, |d| d.bg(col::selected()).border_color(col::focus()))
            .when(!active, |d| d.hover(|s| s.bg(col::hover())))
            .child(div().w(px(16.)).flex_none().when(is_dir, |d| {
                d.child(codicon(if open {
                    icon::CHEVRON_DOWN
                } else {
                    icon::CHEVRON_RIGHT
                }))
            }))
            .when(!is_dir, |d| {
                d.child(div().mr(px(6.)).child(file_icon(path)))
            })
            .child(div().overflow_hidden().text_ellipsis().child(name))
            .on_click(cx.listener(move |this, _, window, cx| {
                if is_dir {
                    if !this.expanded.remove(&click_path) {
                        this.expanded.insert(click_path.clone());
                    }
                    cx.notify();
                } else {
                    this.open_path(click_path.clone(), window, cx);
                }
            }))
            .into_any_element()
    }

    // ---- editor group ------------------------------------------------------

    fn render_editor_group(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.docs.is_empty() {
            return watermark().into_any_element();
        }
        let tabs = self.docs.iter().enumerate().map(|(ix, doc)| {
            let active = ix == self.active;
            let name = doc
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let group = SharedString::from(format!("tab-{ix}"));
            h_flex()
                .id(("tab", ix))
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
                .child(file_icon(&doc.path))
                .child(div().flex_1().whitespace_nowrap().child(name))
                .child(
                    div()
                        .id(("tab-close", ix))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(20.))
                        .rounded(px(4.))
                        .hover(|s| s.bg(col::toolbar_hover()))
                        .map(|d| match (doc.dirty, active) {
                            (true, _) => d.child(codicon(icon::CIRCLE_FILLED).text_size(px(10.))),
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
        });

        let doc = &self.docs[self.active];
        let mut crumbs: Vec<AnyElement> = Vec::new();
        let rel = self
            .root
            .as_ref()
            .and_then(|r| doc.path.strip_prefix(r).ok())
            .unwrap_or(&doc.path);
        let parts: Vec<_> = rel
            .iter()
            .map(|p| p.to_string_lossy().to_string())
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

        v_flex()
            .size_full()
            .bg(col::editor())
            .child(
                h_flex()
                    .h(px(35.))
                    .flex_none()
                    .bg(col::bg())
                    .text_size(px(13.))
                    .child(
                        div()
                            .id("tabs")
                            .flex()
                            .h_full()
                            .overflow_x_scroll()
                            .children(tabs),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .border_b_1()
                            .border_color(col::border()),
                    )
                    .child(
                        h_flex()
                            .h_full()
                            .px(px(8.))
                            .gap(px(4.))
                            .border_b_1()
                            .border_color(col::border())
                            .text_color(col::fg())
                            .child(action_button("split-editor", icon::SPLIT_HORIZONTAL))
                            .child(action_button("editor-more", icon::ELLIPSIS)),
                    ),
            )
            .child(
                h_flex()
                    .h(px(22.))
                    .flex_none()
                    .pl(px(12.))
                    .text_size(px(13.))
                    .text_color(col::fg_muted())
                    .children(crumbs),
            )
            .child(
                div().flex_1().min_h_0().map(|d| {
                    if !doc.binary {
                        return d.child(self.render_code(self.active, cx));
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
                                        this.open_anyway(ix, window, cx)
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
        let tabs = PANEL_TABS.iter().enumerate().map(|(ix, name)| {
            let active = ix == self.panel_tab;
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
                icon::CHEVRON_UP,
            ]
        } else {
            &[
                icon::FILTER,
                icon::CLEAR_ALL,
                icon::ELLIPSIS,
                icon::CHEVRON_UP,
            ]
        };

        let body = match self.panel_tab {
            0 => div()
                .px(px(20.))
                .child("No problems have been detected in the workspace.")
                .into_any_element(),
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
            _ => div().into_any_element(),
        };

        v_flex()
            .size_full()
            .bg(col::bg())
            .border_t_1()
            .border_color(col::border())
            .text_size(px(13.))
            .text_color(col::fg())
            .child(
                h_flex()
                    .h(px(35.))
                    .flex_none()
                    .pl(px(4.))
                    .pr(px(8.))
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
                            .children(actions.iter().enumerate().map(|(i, &ch)| {
                                let button = action_button(("panel-act", i), ch);
                                if (ch, self.panel_tab) == (icon::CHEVRON_DOWN, TERMINAL) {
                                    return self.profile_menu(cx).into_any_element();
                                }
                                match (ch, self.panel_tab) {
                                    (icon::CLEAR_ALL, debug::DEBUG_CONSOLE) => {
                                        button.on_click(cx.listener(|this, _, _, cx| {
                                            this.clear_debug_console(cx)
                                        }))
                                    }
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
                            .child(action_button("panel-close", icon::CLOSE).on_click(
                                cx.listener(|this, _, w, cx| {
                                    this.toggle_panel(&TogglePanel, w, cx)
                                }),
                            )),
                    ),
            )
            .child(div().flex_1().min_h_0().pt(px(4.)).child(body))
    }

    // ---- status bar --------------------------------------------------------

    fn render_statusbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        /// A status bar entry with VS Code's tooltip.
        fn item(id: &'static str, tip: impl Into<SharedString>) -> Stateful<Div> {
            let tip = tip.into();
            h_flex()
                .id(id)
                .h_full()
                .px(px(5.))
                .gap(px(4.))
                .cursor_pointer()
                .hover(|s| s.bg(col::status_hover()).text_color(col::fg()))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        }

        let mut right: Vec<AnyElement> = Vec::new();
        if let Some(doc) = self.docs.get(self.active).filter(|d| !d.binary) {
            let editor = doc.editor.read(cx);
            let pos = editor.cursor_position();
            let line = editor.text().slice_line(pos.line as usize).to_string();
            let col = palette::status_column(&line, pos.character as usize);
            let lang = language_label(language_id(&doc.path));
            right.extend([
                item("sb-pos", "Go to Line/Column")
                    .child(format!("Ln {}, Col {col}", pos.line + 1))
                    .into_any_element(),
                item("sb-indent", "Select Indentation")
                    .child(format!("Spaces: {TAB_SIZE}"))
                    .into_any_element(),
                item("sb-enc", "Select Encoding")
                    .child(doc.encoding.label())
                    .into_any_element(),
                item("sb-eol", "Select End of Line Sequence")
                    .child(if doc.crlf { "CRLF" } else { "LF" })
                    .into_any_element(),
                item("sb-lang", "Select Language Mode")
                    .child(codicon(icon::CODE).text_size(px(14.)))
                    .child(lang)
                    .into_any_element(),
            ]);
        }
        // VS Code's notificationsStatus tooltips.
        let bell_tip: SharedString = match (self.center, self.notes.len(), self.unseen) {
            (true, ..) => "Hide Notifications".into(),
            (_, 0, _) => "No Notifications".into(),
            (_, _, 0) => "No New Notifications".into(),
            (_, _, 1) => "1 New Notification".into(),
            (_, _, n) => format!("{n} New Notifications").into(),
        };
        right.push(
            item("sb-bell", bell_tip)
                .child(
                    codicon(if self.unseen > 0 {
                        icon::BELL_DOT
                    } else {
                        icon::BELL
                    })
                    .text_size(px(14.)),
                )
                .on_click(cx.listener(|this, _, _, cx| this.show_center(!this.center, cx)))
                .into_any_element(),
        );

        let debugging = self.debug_status();
        h_flex()
            .h(px(22.))
            .flex_none()
            .justify_between()
            .bg(col::bg())
            .when(debugging.as_ref().is_some_and(|d| d.1), |d| {
                d.bg(crate::theme::c(debug::STATUS_DEBUGGING))
                    .text_color(white())
            })
            .border_t_1()
            .border_color(col::border())
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
                            .px(px(10.))
                            .bg(col::remote())
                            .text_color(white())
                            .cursor_pointer()
                            .child(codicon(icon::REMOTE).text_size(px(14.))),
                    )
                    .when(self.root.is_some(), |d| {
                        d.child(
                            item("sb-branch", "main, Checkout Branch/Tag...")
                                .child(codicon(icon::SOURCE_CONTROL).text_size(px(14.)))
                                .child("main"),
                        )
                        .child(
                            item("sb-sync", "Synchronize Changes")
                                .child(codicon(icon::SYNC).text_size(px(14.))),
                        )
                    })
                    .child(
                        item("sb-problems", "No Problems")
                            .child(codicon(icon::ERROR).text_size(px(14.)))
                            .child("0")
                            .child(codicon(icon::WARNING).text_size(px(14.)))
                            .child("0")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.panel = true;
                                this.panel_tab = 0;
                                cx.notify();
                            })),
                    )
                    .when_some(debugging, |d, (name, _)| {
                        d.child(
                            item("sb-debug", "Select and Start Debug Configuration")
                                .child(codicon(icon::DEBUG_ALT_SMALL).text_size(px(14.)))
                                .child(name)
                                .on_click(cx.listener(|this, _, _, cx| this.show(View::Run, cx))),
                        )
                    }),
            )
            .child(h_flex().h_full().pr(px(4.)).children(right))
    }
}

impl Workbench {
    /// A warning notification: listed in the bell's center, and shown as a
    /// toast (unless the center is open) until VS Code's 18 seconds pass, or
    /// later while the mouse is over it. A repeat replaces the old one.
    // ponytail: one toast at a time, and warnings only.
    fn warn(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        let message = message.into();
        self.clear_note(&message, cx);
        self.notes.insert(0, message.clone());
        if self.center {
            return;
        }
        self.unseen += 1;
        let hide = cx.spawn(async move |this, cx| {
            loop {
                let wait = std::time::Duration::from_secs(18);
                cx.background_executor().timer(wait).await;
                let gone = this.update(cx, |this, cx| {
                    if this.toast.as_ref().is_some_and(|t| t.1) {
                        return false;
                    }
                    this.toast = None;
                    cx.notify();
                    true
                });
                if gone.unwrap_or(true) {
                    break;
                }
            }
        });
        self.toast = Some((message, false, hide));
    }

    /// Opens or closes the bell's center; opening it hides the toast and
    /// marks every notification seen.
    fn show_center(&mut self, show: bool, cx: &mut Context<Self>) {
        self.center = show;
        if show {
            self.toast = None;
            self.unseen = 0;
        }
        cx.notify();
    }

    /// Clear All Notifications, which closes the center too.
    fn clear_notes(&mut self, cx: &mut Context<Self>) {
        self.notes.clear();
        self.toast = None;
        self.unseen = 0;
        self.center = false;
        cx.notify();
    }

    /// Clears a notification, from the center and its toast.
    fn clear_note(&mut self, message: &SharedString, cx: &mut Context<Self>) {
        let count = self.notes.len();
        self.notes.retain(|n| n != message);
        if self.notes.len() < count && !self.center {
            self.unseen = self.unseen.saturating_sub(1);
        }
        if self.toast.as_ref().is_some_and(|t| &t.0 == message) {
            self.toast = None;
        }
        cx.notify();
    }

    /// One notification's warning icon, message and close button.
    fn note_row(
        &self,
        id: impl Into<ElementId>,
        message: &SharedString,
        cx: &mut Context<Self>,
    ) -> Div {
        let gone = message.clone();
        h_flex()
            .min_h(px(42.))
            .px(px(8.))
            .gap(px(8.))
            // notificationsWarningIcon.foreground
            .child(codicon(icon::WARNING).text_color(c(0xCCA700)))
            .child(div().flex_1().child(message.clone()))
            .child(
                action_button(id, icon::CLOSE).on_click(cx.listener(move |this, _, _, cx| {
                    this.clear_note(&gone, cx);
                    // The center closes once it's empty, as VS Code's does.
                    this.center &= !this.notes.is_empty();
                })),
            )
    }

    /// VS Code's notification center, opened from the bell.
    fn render_center(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.center {
            return None;
        }
        let header = if self.notes.is_empty() {
            "NO NEW NOTIFICATIONS"
        } else {
            "NOTIFICATIONS"
        };
        Some(
            v_flex()
                .absolute()
                .right(px(11.))
                .bottom(px(33.))
                .w(px(450.))
                .rounded(px(6.))
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
                                .on_click(cx.listener(|this, _, _, cx| this.clear_notes(cx))),
                        )
                        .child(action_button("center-hide", icon::CHEVRON_DOWN).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.center = false;
                                cx.notify();
                            }),
                        )),
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

    fn render_toast(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let (message, ..) = self.toast.as_ref()?;
        Some(
            self.note_row("toast-close", message, cx)
                .id("toast")
                .absolute()
                .right(px(11.))
                .bottom(px(33.))
                .w(px(450.))
                .rounded(px(6.))
                // notifications.background
                .bg(c(0x202122))
                .border_1()
                .border_color(col::border())
                .shadow_lg()
                .text_size(px(13.))
                .text_color(col::fg())
                .on_hover(cx.listener(|this, &over, _, _| {
                    if let Some(t) = &mut this.toast {
                        t.1 = over;
                    }
                })),
        )
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar = self.render_sidebar(cx);
        let editor = self.render_editor_group(cx);
        let panel = self.render_panel(window, cx);
        let palette = self.render_palette(window, cx);
        let toolbar = self.render_debug_toolbar(window, cx);

        v_flex()
            .key_context(match self.center || self.toast.is_some() {
                true => "Workbench notificationsVisible",
                false => "Workbench",
            })
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::close_folder))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::save_all))
            .on_action(cx.listener(Self::close_editor))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_panel))
            .on_action(cx.listener(Self::toggle_terminal))
            .on_action(cx.listener(|this, _: &ShowExplorer, _, cx| this.show(View::Explorer, cx)))
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
            .on_action(
                cx.listener(|this, _: &ShowExtensions, _, cx| this.show(View::Extensions, cx)),
            )
            .on_action(
                cx.listener(|this, _: &QuickOpen, window, cx| this.open_palette("", window, cx)),
            )
            .on_action(cx.listener(|this, _: &CommandPalette, window, cx| {
                this.open_palette(">", window, cx)
            }))
            .on_action(cx.listener(Self::new_text_file))
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
            .on_action(cx.listener(|this, _: &ShowNotifications, _, cx| this.show_center(true, cx)))
            .on_action(cx.listener(|this, _: &HideNotifications, _, cx| {
                // The toast stays listed in the center.
                this.toast = None;
                this.show_center(false, cx);
            }))
            .on_action(cx.listener(|this, _: &ClearAllNotifications, _, cx| this.clear_notes(cx)))
            .relative()
            .size_full()
            .font_family("Segoe UI")
            .text_size(px(13.))
            .bg(col::editor())
            .text_color(col::fg())
            .child(self.render_titlebar(window, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_activitybar(cx))
                    .child(
                        h_resizable("workbench")
                            .child(
                                resizable_panel()
                                    .size(px(300.))
                                    .size_range(px(170.)..px(800.))
                                    .visible(self.sidebar)
                                    .child(sidebar),
                            )
                            .child(
                                resizable_panel().child(
                                    v_resizable("editor-panel")
                                        .child(resizable_panel().child(editor))
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
            .children(self.render_toast(cx))
            .children(self.render_center(cx))
            .children(palette)
    }
}

// ---- helpers ---------------------------------------------------------------

fn ca_white(alpha: u8) -> Hsla {
    crate::theme::ca(0xFFFFFF00 | alpha as u32)
}

/// Highlighter language names gpui-component accepts.
fn editor_language(path: &Path) -> String {
    match language_id(path) {
        "typescriptreact" => "tsx".into(),
        "javascriptreact" | "javascript" => "javascript".into(),
        "shellscript" => "bash".into(),
        "makefile" => "make".into(),
        "plaintext" | "ignore" => "text".into(),
        id => id.into(),
    }
}

/// 22px collapsible pane header (bold, uppercase, 11px) with optional hover actions.
fn section_header(
    id: &'static str,
    label: &str,
    open: bool,
    actions: Option<&[char]>,
) -> Stateful<Div> {
    let group = SharedString::from(format!("{id}-hdr"));
    h_flex()
        .id(id)
        .group(group.clone())
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
        .when_some(actions, |d, actions| {
            d.child(
                h_flex()
                    .gap(px(2.))
                    .invisible()
                    .group_hover(group, |s| s.visible())
                    .children(actions.iter().enumerate().map(|(i, &ch)| {
                        action_button((id, i), ch).font_weight(FontWeight::NORMAL)
                    })),
            )
        })
}

fn search_box(state: &Entity<InputState>) -> impl IntoElement + use<> {
    div()
        .rounded(px(4.))
        .border_1()
        .border_color(col::input_border())
        .bg(col::input())
        .child(Input::new(state).appearance(false).text_size(px(13.)))
}

fn primary_button(id: &'static str, label: &'static str, ch: Option<char>) -> Stateful<Div> {
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
        .child(label)
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

/// VS Code's application menus, minus anything AI.
fn app_menu(
    name: &str,
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
            .menu("Open Folder...", Box::new(OpenFolder))
            .submenu("Open Recent", window, cx, |m, _, _| {
                m.menu("More...", Box::new(Noop))
            })
            .separator()
            .menu("Save", Box::new(Save))
            .menu("Save As...", noop())
            .menu("Save All", Box::new(SaveAll))
            .separator()
            .menu("Auto Save", noop())
            .submenu("Preferences", window, cx, |m, _, _| {
                m.menu("Settings", Box::new(Noop))
                    .menu("Keyboard Shortcuts", Box::new(Noop))
                    .menu("Themes", Box::new(Noop))
            })
            .separator()
            .menu("Revert File", noop())
            .menu("Close Editor", Box::new(CloseEditor))
            .menu("Close Folder", Box::new(CloseFolder))
            .menu("Close Window", noop())
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
            .menu("Toggle Line Comment", noop())
            .menu("Toggle Block Comment", noop()),
        "Selection" => menu
            .menu("Select All", Box::new(input::SelectAll))
            .menu("Expand Selection", noop())
            .menu("Shrink Selection", noop())
            .separator()
            .menu("Copy Line Up", noop())
            .menu("Copy Line Down", noop())
            .menu("Move Line Up", noop())
            .menu("Move Line Down", noop())
            .separator()
            .menu("Add Cursor Above", noop())
            .menu("Add Cursor Below", noop()),
        "View" => menu
            .menu("Command Palette...", Box::new(CommandPalette))
            .menu("Open View...", noop())
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
            .menu("Problems", noop())
            .menu("Output", noop())
            .menu("Debug Console", noop())
            .menu("Terminal", Box::new(ToggleTerminal))
            .separator()
            .menu("Word Wrap", noop()),
        "Go" => menu
            .menu("Back", noop())
            .menu("Forward", noop())
            .menu("Last Edit Location", noop())
            .separator()
            .menu("Go to File...", Box::new(QuickOpen))
            .menu("Go to Symbol in Workspace...", noop())
            .separator()
            .menu("Go to Symbol in Editor...", noop())
            .menu("Go to Definition", Box::new(input::GoToDefinition))
            .menu("Go to Line/Column...", noop()),
        "Run" => menu
            .menu("Start Debugging", Box::new(StartDebugging))
            .menu("Run Without Debugging", Box::new(RunWithoutDebugging))
            .menu("Stop Debugging", Box::new(StopDebugging))
            .menu("Restart Debugging", Box::new(RestartDebugging))
            .separator()
            .menu("Open Configurations", noop())
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
            .menu("About", noop()),
    }
}

#[cfg(test)]
mod tests {
    use super::Encoding;

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
        assert_eq!(Encoding::decode(b"h\xE9").unwrap().1, Encoding::Utf8);
    }
}
