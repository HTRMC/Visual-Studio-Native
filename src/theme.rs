//! Workbench colors (VS Code "Dark 2026"), codicons and Seti file icons.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, RwLock},
};

use gpui_kit::*;

pub fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

pub fn ca(hex: u32) -> Hsla {
    rgba(hex).into()
}

/// The workbench's colors: Dark 2026's, or an extension color theme's
/// mapped onto them (see `extensions::apply_theme`).
// ponytail: only these follow a color theme; the `c(0x..)` literals
// elsewhere stay Dark 2026's.
pub mod col {
    use super::*;
    use std::sync::RwLock;

    macro_rules! colors {
        ($($(#[$doc:meta])* $name:ident: $value:expr,)*) => {
            #[derive(Clone, Copy)]
            pub struct Colors {
                $(pub $name: Hsla,)*
            }
            impl Default for Colors {
                fn default() -> Self {
                    Self { $($name: $value,)* }
                }
            }
            $($(#[$doc])* pub fn $name() -> Hsla {
                COLORS.read().unwrap().$name
            })*
        };
    }

    colors! {
        /// titleBar / activityBar / sideBar / panel / statusBar
        bg: c(0x191A1B),
        editor: c(0x121314),
        border: c(0x2A2B2C),
        fg: c(0xbfbfbf),
        fg_muted: c(0x8C8C8C),
        fg_dim: c(0x555555),
        accent: c(0x3994BC),
        focus: ca(0x3994BCB3),
        hover: ca(0xFFFFFF14),
        selected: ca(0xFFFFFF22),
        /// editorGroup.dropBackground
        drop: ca(0x53595D80),
        /// tab.dragAndDropBorder
        drag_border: c(0xFFFFFF),
        toolbar_hover: ca(0xFFFFFF10),
        input: c(0x191A1B),
        input_border: c(0x333536),
        command_center_border: c(0x2E3031),
        status_hover: c(0x323233),
        remote: c(0x0078D4),
        indent_guide: c(0x585858),
        inactive_indent_guide: ca(0x58585866),
        close_hover: c(0xE81123),
        /// terminal.foreground
        terminal_fg: c(0xCCCCCC),
    }

    static COLORS: LazyLock<RwLock<Colors>> = LazyLock::new(Default::default);

    pub fn set(colors: Colors) {
        *COLORS.write().unwrap() = colors;
    }
}

/// Codicon code points (from @vscode/codicons codicon.csv).
#[allow(dead_code)]
pub mod icon {
    pub const FILES: char = '\u{EAF0}';
    pub const SEARCH: char = '\u{EA6D}';
    pub const SOURCE_CONTROL: char = '\u{EA68}';
    pub const DEBUG_ALT: char = '\u{EB91}';
    pub const EXTENSIONS: char = '\u{EAE6}';
    pub const ACCOUNT: char = '\u{EB99}';
    pub const SETTINGS_GEAR: char = '\u{EB51}';
    pub const CHEVRON_RIGHT: char = '\u{EAB6}';
    pub const CHEVRON_DOWN: char = '\u{EAB4}';
    pub const CHEVRON_UP: char = '\u{EAB7}';
    pub const CLOSE: char = '\u{EA76}';
    pub const ELLIPSIS: char = '\u{EA7C}';
    pub const NEW_FILE: char = '\u{EA7F}';
    pub const NEW_FOLDER: char = '\u{EA80}';
    pub const REFRESH: char = '\u{EB37}';
    pub const COLLAPSE_ALL: char = '\u{EAC5}';
    pub const CLOSE_ALL: char = '\u{EAC1}';
    pub const SPLIT_HORIZONTAL: char = '\u{EB56}';
    pub const LAYOUT_SIDEBAR_LEFT: char = '\u{EBF3}';
    pub const LAYOUT_SIDEBAR_LEFT_OFF: char = '\u{EC02}';
    pub const LAYOUT_PANEL: char = '\u{EBF2}';
    pub const LAYOUT_PANEL_OFF: char = '\u{EC01}';
    pub const LAYOUT_SIDEBAR_RIGHT_OFF: char = '\u{EC00}';
    pub const LAYOUT: char = '\u{EBEB}';
    pub const ARROW_LEFT: char = '\u{EA9B}';
    pub const ARROW_RIGHT: char = '\u{EA9C}';
    pub const ARROW_UP: char = '\u{EAA1}';
    pub const ARROW_DOWN: char = '\u{EA9A}';
    pub const CHROME_MINIMIZE: char = '\u{EABA}';
    pub const CHROME_MAXIMIZE: char = '\u{EAB9}';
    pub const CHROME_RESTORE: char = '\u{EABB}';
    pub const CHROME_CLOSE: char = '\u{EAB8}';
    pub const SYNC: char = '\u{EA77}';
    pub const ERROR: char = '\u{EA87}';
    pub const WARNING: char = '\u{EA6C}';
    pub const INFO: char = '\u{EA74}';
    pub const BELL: char = '\u{EAA2}';
    pub const BELL_DOT: char = '\u{EB9A}';
    pub const BELL_SLASH: char = '\u{EC08}';
    pub const BELL_SLASH_DOT: char = '\u{EC09}';
    pub const REMOTE: char = '\u{EB3A}';
    pub const ADD: char = '\u{EA60}';
    pub const TRASH: char = '\u{EA81}';
    pub const TERMINAL: char = '\u{EA85}';
    pub const CIRCLE_FILLED: char = '\u{EA71}';
    pub const SYMBOL_METHOD: char = '\u{EA8C}';
    pub const FILTER: char = '\u{EAF1}';
    pub const CLEAR_ALL: char = '\u{EABF}';
    pub const CHECK: char = '\u{EAB2}';
    pub const KEBAB_VERTICAL: char = '\u{EB10}';
    pub const LIST_FLAT: char = '\u{EB84}';
    pub const CODE: char = '\u{EAC4}';
    pub const CASE_SENSITIVE: char = '\u{EAB1}';
    pub const WHOLE_WORD: char = '\u{EB7E}';
    pub const REGEX: char = '\u{EB38}';
    pub const BOOK: char = '\u{EAA4}';
    pub const EXCLUDE: char = '\u{EAE5}';
    pub const REPLACE: char = '\u{EB3D}';
    pub const REPLACE_ALL: char = '\u{EB3C}';
    pub const CIRCLE_OUTLINE: char = '\u{EABC}';
    pub const DEBUG_CONTINUE: char = '\u{EACF}';
    pub const DEBUG_PAUSE: char = '\u{EAD1}';
    pub const DEBUG_RESTART: char = '\u{EAD2}';
    pub const DEBUG_START: char = '\u{EAD3}';
    pub const DEBUG_STEP_INTO: char = '\u{EAD4}';
    pub const DEBUG_STEP_OUT: char = '\u{EAD5}';
    pub const DEBUG_STEP_OVER: char = '\u{EAD6}';
    pub const DEBUG_STOP: char = '\u{EAD7}';
    pub const DEBUG_STACKFRAME: char = '\u{EB8B}';
    pub const DEBUG_ALT_SMALL: char = '\u{EBA8}';
    pub const GRIPPER: char = '\u{EB04}';
    pub const GEAR: char = '\u{EAF8}';
}

/// Every codicon by name, as VS Code's icon picker offers them.
pub fn codicons() -> impl Iterator<Item = (&'static str, char)> {
    include_str!("../assets/codicons.txt")
        .lines()
        .filter_map(|l| {
            let (hex, name) = l.split_once(' ')?;
            Some((name, char::from_u32(u32::from_str_radix(hex, 16).ok()?)?))
        })
}

/// A codicon glyph. VS Code renders them at 16px (24px in the activity bar).
pub fn codicon(ch: char) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .font_family("codicon")
        .text_size(px(16.))
        .line_height(px(16.))
        .child(ch.to_string())
}

/// The app logo (assets/icon-*.png), picking the bitmap closest to `size` so it stays crisp.
pub fn logo(size: f32) -> Img {
    static SMALL: LazyLock<Arc<Image>> = LazyLock::new(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Png,
            include_bytes!("../assets/icon-32.png").to_vec(),
        ))
    });
    static BIG: LazyLock<Arc<Image>> = LazyLock::new(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Png,
            include_bytes!("../assets/icon-256.png").to_vec(),
        ))
    });
    let src = if size <= 32. { &SMALL } else { &BIG };
    img((*src).clone()).flex_none().size(px(size))
}

/// A plain tooltip, for `.tooltip(tip("..."))`.
pub fn tip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView {
    move |window, cx| component::tooltip::Tooltip::new(text).build(window, cx)
}

/// A tag per key of `action`'s binding: gpui-component's own hint shows only
/// a chord's first key ("Ctrl+K" for Ctrl+K Ctrl+Q).
pub fn kbds(action: &dyn Action, window: &Window) -> Vec<component::kbd::Kbd> {
    let binding = window.highest_precedence_binding_for_action(action);
    (binding.iter())
        .flat_map(|b| b.keystrokes())
        .map(|k| component::kbd::Kbd::new(k.as_keystroke().clone()))
        .collect()
}

/// Square, hoverable toolbar button with a codicon (22x22, radius 5 like VS Code's action bar).
pub fn action_button(id: impl Into<ElementId>, ch: char) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(22.))
        .rounded(px(5.))
        .cursor_pointer()
        .hover(|s| s.bg(col::toolbar_hover()))
        .child(codicon(ch))
}

// ---- file icon themes ------------------------------------------------------

enum FileIcon {
    /// A Seti font glyph and its color.
    Glyph(char, Hsla),
    Image(PathBuf),
}

/// A file icon theme (`workbench.iconTheme`): icons by id, and which id
/// each file name, extension, language and folder takes.
#[derive(Default)]
struct IconTheme {
    defs: HashMap<String, FileIcon>,
    extensions: HashMap<String, String>,
    names: HashMap<String, String>,
    languages: HashMap<String, String>,
    folder_names: HashMap<String, String>,
    folder_names_expanded: HashMap<String, String>,
    file: Option<String>,
    folder: Option<String>,
    folder_expanded: Option<String>,
}

/// The built-in Seti icon theme's JSON.
pub fn seti_json() -> serde_json::Value {
    serde_json::from_str(include_str!("../assets/vs-seti-icon-theme.json")).expect("seti json")
}

static ICONS: LazyLock<RwLock<Arc<IconTheme>>> =
    LazyLock::new(|| RwLock::new(Arc::new(IconTheme::parse(&seti_json(), None))));

fn icons() -> Arc<IconTheme> {
    ICONS.read().unwrap().clone()
}

/// Shows the icon theme `json` (Null for none): its icon paths start at
/// `dir`, or its glyphs are Seti's when there's no `dir`.
pub fn set_file_icons(json: &serde_json::Value, dir: Option<&Path>) {
    *ICONS.write().unwrap() = Arc::new(IconTheme::parse(json, dir));
}

impl IconTheme {
    // ponytail: an extension's font glyph icons and the `light` and
    // `highContrast` variants are skipped; image icons cover the popular
    // themes.
    fn parse(json: &serde_json::Value, dir: Option<&Path>) -> Self {
        let map = |key: &str| -> HashMap<String, String> {
            (json[key].as_object().into_iter().flatten())
                .filter_map(|(k, v)| Some((k.to_lowercase(), v.as_str()?.to_string())))
                .collect()
        };
        let id = |key: &str| json[key].as_str().map(String::from);
        let defs = (json["iconDefinitions"].as_object().into_iter().flatten())
            .filter_map(|(id, d)| {
                let icon = match dir {
                    Some(dir) => FileIcon::Image(dir.join(d["iconPath"].as_str()?)),
                    None => {
                        let ch = d["fontCharacter"].as_str()?.trim_start_matches('\\');
                        let ch = char::from_u32(u32::from_str_radix(ch, 16).ok()?)?;
                        let color = (d["fontColor"].as_str())
                            .and_then(|s| u32::from_str_radix(&s[1..], 16).ok())?;
                        FileIcon::Glyph(ch, c(color))
                    }
                };
                Some((id.clone(), icon))
            })
            .collect();
        IconTheme {
            defs,
            extensions: map("fileExtensions"),
            names: map("fileNames"),
            languages: map("languageIds"),
            folder_names: map("folderNames"),
            folder_names_expanded: map("folderNamesExpanded"),
            file: id("file"),
            folder: id("folder"),
            folder_expanded: id("folderExpanded"),
        }
    }

    fn icon(&self, id: Option<&String>) -> AnyElement {
        let slot = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(16.))
            .h(px(22.));
        match id.and_then(|id| self.defs.get(id)) {
            Some(FileIcon::Glyph(ch, color)) => slot
                .font_family("seti")
                .text_size(px(19.5))
                .text_color(*color)
                .child(ch.to_string()),
            Some(FileIcon::Image(path)) => slot.child(img(path.clone()).size(px(16.))),
            None => slot,
        }
        .into_any_element()
    }
}

/// VS Code language id for a file, used for both Seti icons and the status bar.
pub fn language_id(path: &Path) -> &'static str {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if let Some(id) = crate::workbench::extensions::language_of(&name) {
        return id;
    }
    match name.as_str() {
        "makefile" => return "makefile",
        "dockerfile" => return "dockerfile",
        "cargo.lock" => return "toml",
        _ => {}
    }
    match ext.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "json" => "json",
        "jsonc" => "jsonc",
        "md" | "markdown" => "markdown",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        "py" => "python",
        "c" => "c",
        "h" | "hpp" | "hh" | "cc" | "cpp" | "cxx" => "cpp",
        "cs" => "csharp",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "zig" => "zig",
        "swift" => "swift",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "sh" | "bash" => "shellscript",
        "ps1" | "psm1" => "powershell",
        "bat" | "cmd" => "bat",
        "sql" => "sql",
        "xml" | "svg" => "xml",
        "gitignore" | "gitattributes" => "ignore",
        _ => "plaintext",
    }
}

/// Every language id [`language_id`] gives, for Change Language Mode.
pub const LANGUAGES: &[&str] = &[
    "bat",
    "c",
    "cpp",
    "csharp",
    "css",
    "dockerfile",
    "go",
    "html",
    "ignore",
    "java",
    "javascript",
    "javascriptreact",
    "json",
    "jsonc",
    "kotlin",
    "lua",
    "makefile",
    "markdown",
    "php",
    "plaintext",
    "powershell",
    "python",
    "ruby",
    "rust",
    "scss",
    "shellscript",
    "sql",
    "swift",
    "toml",
    "typescript",
    "typescriptreact",
    "xml",
    "yaml",
    "zig",
];

/// Display name for the status bar language indicator.
pub fn language_label(id: &str) -> &'static str {
    match id {
        "rust" => "Rust",
        "typescript" => "TypeScript",
        "typescriptreact" => "TypeScript JSX",
        "javascript" => "JavaScript",
        "javascriptreact" => "JavaScript JSX",
        "json" => "JSON",
        "jsonc" => "JSON with Comments",
        "markdown" => "Markdown",
        "toml" => "TOML",
        "yaml" => "YAML",
        "python" => "Python",
        "c" => "C",
        "cpp" => "C++",
        "csharp" => "C#",
        "go" => "Go",
        "java" => "Java",
        "kotlin" => "Kotlin",
        "ruby" => "Ruby",
        "php" => "PHP",
        "lua" => "Lua",
        "zig" => "Zig",
        "swift" => "Swift",
        "html" => "HTML",
        "css" => "CSS",
        "scss" => "SCSS",
        "shellscript" => "Shell Script",
        "powershell" => "PowerShell",
        "bat" => "Batch",
        "sql" => "SQL",
        "xml" => "XML",
        "makefile" => "Makefile",
        "dockerfile" => "Dockerfile",
        "ignore" => "Ignore",
        _ => crate::workbench::extensions::language_label(id).unwrap_or("Plain Text"),
    }
}

/// A file's icon, resolved the way VS Code does: file name, then
/// extensions (longest first), then language id, then the default.
pub fn file_icon(path: &Path) -> AnyElement {
    let s = icons();
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    let by_ext = name
        .match_indices('.')
        .map(|(i, _)| &name[i + 1..])
        .find_map(|ext| s.extensions.get(ext));
    let id = s
        .names
        .get(&name)
        .or(by_ext)
        .or_else(|| s.languages.get(language_id(path)))
        .or(s.file.as_ref());
    s.icon(id)
}

/// A folder's icon, `open` or not, when the icon theme has folder icons.
pub fn folder_icon(path: &Path, open: bool) -> Option<AnyElement> {
    let s = icons();
    let name = (path.file_name()?.to_str()?).to_lowercase();
    let id = match open {
        true => (s.folder_names_expanded.get(&name))
            .or(s.folder_expanded.as_ref())
            .or(s.folder.as_ref()),
        false => s.folder_names.get(&name).or(s.folder.as_ref()),
    };
    id.is_some().then(|| s.icon(id))
}

/// A language id's icon, as Change Language Mode's rows show it.
pub fn language_icon(id: &str) -> AnyElement {
    let s = icons();
    s.icon(s.languages.get(id).or(s.file.as_ref()))
}
