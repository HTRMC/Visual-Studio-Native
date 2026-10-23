//! The Settings editor Ctrl+, opens: VS Code's settings UI over the user
//! settings.json, for the settings vsn honors and those extensions contribute.

use std::collections::{HashMap, HashSet};
use std::cmp::Ordering;
use std::rc::Rc;
use std::sync::LazyLock;

use gpui_kit::component::text::TextView;
use regex::Regex;
use serde_json::{Map, Value, json};

use super::*;

/// VS Code's Commonly Used, in its order: the ones vsn has.
const COMMONLY_USED: &[&str] = &[
    "files.autoSave",
    "editor.wordWrap",
    "files.exclude",
    "workbench.colorTheme",
];

/// VS Code's table of contents, down to the groups vsn's settings fall in:
/// id, label, level, and the key patterns the group claims.
const TOC: &[(&str, &str, usize, &[&str])] = &[
    ("commonlyUsed", "Commonly Used", 1, &[]),
    ("editor", "Text Editor", 1, &["editor.*"]),
    ("editor/files", "Files", 2, &["files.*"]),
    ("workbench", "Workbench", 1, &["workbench.*"]),
    (
        "workbench/appearance",
        "Appearance",
        2,
        &["workbench.*color*", "workbench.iconTheme"],
    ),
    ("features", "Features", 1, &[]),
    ("features/explorer", "Explorer", 2, &["explorer.*"]),
    ("features/search", "Search", 2, &["search.*"]),
    ("features/terminal", "Terminal", 2, &["terminal.*"]),
    ("extensions", "Extensions", 1, &[]),
];

/// VS Code's registrations of the settings vsn honors, with the enums that
/// depend on what's installed filled in.
fn builtin() -> Map<String, Value> {
    let profiles: Vec<Value> = std::iter::once(Value::Null)
        .chain(super::terminal::profiles().into_iter().map(|p| p.name.to_string().into()))
        .collect();
    let mut themes = extensions::color_themes();
    themes.sort_by_key(|t| (t.dark, t.label.to_lowercase()));
    let mut theme_ids: Vec<Value> = themes.iter().map(|t| t.id.clone().into()).collect();
    let mut theme_labels: Vec<Value> = themes.iter().map(|t| t.label.clone().into()).collect();
    let lights = themes.iter().filter(|t| !t.dark).count();
    theme_ids.insert(lights, extensions::DEFAULT_THEME.into());
    theme_labels.insert(lights, extensions::DEFAULT_THEME.into());
    let mut icons = extensions::icon_themes();
    icons.sort_by_key(|t| t.1.to_lowercase());
    // ponytail: "" for None, as vsn's icon theme picker writes; VS Code writes null.
    let mut icon_ids: Vec<Value> = vec!["".into(), "vs-seti".into()];
    let mut icon_labels: Vec<Value> = vec!["None".into(), "Seti (Visual Studio Code)".into()];
    for (id, label, _) in icons {
        icon_ids.push(id.into());
        icon_labels.push(label.into());
    }
    let glob = "[glob patterns](https://aka.ms/vscode-glob-patterns)";
    let json = json!({
        "editor.wordWrap": {
            "type": "string",
            "enum": ["off", "on", "wordWrapColumn", "bounded"],
            "default": "off",
            "description": "Controls how lines should wrap.",
        },
        "files.autoSave": {
            "type": "string",
            "enum": ["off", "afterDelay", "onFocusChange", "onWindowChange"],
            "default": "off",
            "markdownDescription": "Controls [auto save](https://code.visualstudio.com/docs/editor/codebasics#_save-auto-save) of editors that have unsaved changes.",
        },
        "files.autoSaveDelay": {
            "type": "number",
            "default": 1000,
            "minimum": 0,
            "markdownDescription": "Controls the delay in milliseconds after which an editor with unsaved changes is saved automatically. Only applies when `#files.autoSave#` is set to `afterDelay`.",
        },
        "files.associations": {
            "type": "object",
            "markdownDescription": format!("Configure {glob} of file associations to languages (for example `\"*.extension\": \"html\"`). Patterns will match on the absolute path of a file if they contain a path separator and will match on the name of the file otherwise. These have precedence over the default associations of the languages installed."),
        },
        "files.exclude": {
            "type": "object",
            "default": {
                "**/.git": true, "**/.svn": true, "**/.hg": true, "**/.jj": true,
                "**/.DS_Store": true, "**/Thumbs.db": true,
            },
            "markdownDescription": format!("Configure {glob} for excluding files and folders. For example, the File Explorer decides which files and folders to show or hide based on this setting. Refer to the `#search.exclude#` setting to define search-specific excludes. Refer to the `#explorer.excludeGitIgnore#` setting for ignoring files based on your `.gitignore`."),
        },
        "search.exclude": {
            "type": "object",
            "default": { "**/node_modules": true, "**/bower_components": true, "**/*.code-search": true },
            "markdownDescription": "Configure [glob patterns](https://code.visualstudio.com/docs/editor/codebasics#_advanced-search-options) for excluding files and folders in fulltext searches and file search in quick open. To exclude files from the recently opened list in quick open, patterns must be absolute (for example `**/node_modules/**`). Inherits all glob patterns from the `#files.exclude#` setting.",
        },
        "explorer.autoReveal": {
            "type": ["boolean", "string"],
            "enum": [true, false, "focusNoScroll"],
            "default": true,
            "description": "Controls whether the Explorer should automatically reveal and select files when opening them.",
        },
        "explorer.autoRevealExclude": {
            "type": "object",
            "default": { "**/node_modules": true, "**/bower_components": true },
            "markdownDescription": format!("Configure paths or {glob} for excluding files and folders from being revealed and selected in the Explorer when they are opened. Glob patterns are always evaluated relative to the path of the workspace folder unless they are absolute paths."),
        },
        "terminal.integrated.defaultProfile.windows": {
            "type": ["string", "null"],
            "enum": profiles,
            "default": null,
            "markdownDescription": "The default terminal profile on Windows.",
        },
        "workbench.colorTheme": {
            "type": "string",
            "enum": theme_ids,
            "enumItemLabels": theme_labels,
            "default": extensions::DEFAULT_THEME,
            "markdownDescription": "Specifies the color theme used in the workbench when `#window.autoDetectColorScheme#` is not enabled.",
        },
        "workbench.iconTheme": {
            "type": ["string", "null"],
            "enum": icon_ids,
            "enumItemLabels": icon_labels,
            "default": "vs-seti",
            "description": "Specifies the file icon theme used in the workbench or 'null' to not show any file icons.",
        },
        "workbench.experimental.modernUI": {
            "type": "boolean",
            "default": true,
            "description": "Controls whether the Modern UI Update is enabled. When on, the side bars and bottom panel are shown as floating cards with rounded corners and gaps, and a set of refreshed workbench styles is applied, matching the Agents window design.",
        },
    });
    match json {
        Value::Object(map) => map,
        _ => unreachable!(),
    }
}

pub(crate) struct Group {
    id: String,
    label: String,
    level: usize,
    parent: Option<usize>,
}

pub(crate) struct Entry {
    key: String,
    group: usize,
    schema: Value,
    category: String,
    label: String,
    description: SharedString,
    markdown: bool,
}

pub(crate) enum Row {
    Header(usize),
    Setting(usize),
}

/// The Settings editor's state.
pub(crate) struct Page {
    search: Entity<InputState>,
    list: ListState,
    groups: Vec<Group>,
    entries: Vec<Entry>,
    rows: Vec<Row>,
    /// TOC groups opened by a click.
    expanded: HashSet<usize>,
    /// The text boxes of string and number settings.
    inputs: HashMap<String, Entity<InputState>>,
}

/// VS Code's `wordifyKey`: `files.autoSave` → "Files › Auto Save".
pub(crate) fn wordify(key: &str) -> String {
    static RES: LazyLock<[Regex; 4]> = LazyLock::new(|| {
        [r"\.([a-z0-9])", r"([a-z0-9])([A-Z])", r"([A-Z]+)([A-Z][a-z])", r"\b\w+\b"]
            .map(|r| Regex::new(r).unwrap())
    });
    let s = RES[0].replace_all(key, |c: &regex::Captures| format!(" \u{203A} {}", c[1].to_uppercase()));
    let s = RES[1].replace_all(&s, "$1 $2");
    let s = RES[2].replace_all(&s, "$1 $2");
    let mut s = s.into_owned();
    if let Some(first) = s.chars().next().filter(char::is_ascii_lowercase) {
        s.replace_range(..1, &first.to_ascii_uppercase().to_string());
    }
    const ACRONYMS: &[&str] = &["css", "html", "scss", "less", "json", "js", "ts", "ie", "id", "php", "scm"];
    let s = RES[3].replace_all(&s, |c: &regex::Captures| match ACRONYMS.contains(&c[0].to_lowercase().as_str()) {
        true => c[0].to_uppercase(),
        false => c[0].to_string(),
    });
    let mut s = s.into_owned();
    for (from, to) in [
        ("power shell", "PowerShell"),
        ("powershell", "PowerShell"),
        ("javascript", "JavaScript"),
        ("typescript", "TypeScript"),
        ("github", "GitHub"),
        ("ocaml", "OCaml"),
        ("jet brains", "JetBrains"),
        ("jetbrains", "JetBrains"),
        ("re sharper", "ReSharper"),
        ("resharper", "ReSharper"),
    ] {
        s = Regex::new(&format!(r"(?i)\b{from}\b")).unwrap().replace_all(&s, to).into_owned();
    }
    s
}

/// VS Code's `trimCategoryForGroup`: `category` without the part of it the
/// TOC group `group` (dotted) already says.
fn trim_category(category: &str, group: &str) -> String {
    let insiders = Regex::new(r"(?i)-?insiders$").unwrap();
    let group = match Regex::new(r"(?i)insiders$").unwrap().is_match(category) {
        true => group.to_string(),
        false => insiders.replace(group, "").into_owned(),
    };
    let parts: Vec<String> = (group.split('.'))
        .map(|p| match p.replace('-', "").eq_ignore_ascii_case(category) {
            true => p.replace('-', ""),
            false => p.to_string(),
        })
        .collect();
    for forward in [true, false] {
        let mut parts = parts.clone();
        while !parts.is_empty() {
            let escaped: Vec<_> = parts.iter().map(|p| regex::escape(p)).collect();
            let re = Regex::new(&format!(r"(?i)^{}(\.|$)", escaped.join(r"\."))).unwrap();
            if re.is_match(category) {
                return re.replace(category, "").into_owned();
            }
            match forward {
                true => parts.pop(),
                false => Some(parts.remove(0)),
            };
        }
    }
    category.to_string()
}

/// VS Code's `settingKeyToDisplayFormat`: the title's category and label.
pub(crate) fn display(key: &str, group: &str) -> (String, String) {
    let (category, label) = key.rsplit_once('.').unwrap_or(("", key));
    (wordify(&trim_category(category, &group.replace('/', "."))), wordify(label))
}

/// A setting key pattern of VS Code's TOC, `*` matching anything.
fn matches(pattern: &str, key: &str) -> bool {
    let re = format!("(?i)^{}$", regex::escape(pattern).replace(r"\*", ".*"));
    Regex::new(&re).is_ok_and(|re| re.is_match(key))
}

/// The groups in TOC order (a parent before its children), and the settings
/// in the order the editor lists them.
fn catalog(values: &Value) -> (Vec<Group>, Vec<Entry>) {
    let mut groups = Vec::new();
    let mut parent = None;
    for &(id, label, level, _) in TOC {
        groups.push(Group {
            id: id.into(),
            label: label.into(),
            level,
            parent: if level == 1 { None } else { parent },
        });
        if level == 1 {
            parent = Some(groups.len() - 1);
        }
    }
    let extensions_group = groups.len() - 1;
    // (group, experimental, order, key, schema, description)
    let mut found: Vec<(usize, bool, f64, String, Value, String)> = Vec::new();
    let builtin = builtin();
    for (ix, key) in COMMONLY_USED.iter().enumerate() {
        found.push((0, false, ix as f64, key.to_string(), builtin[*key].clone(), String::new()));
    }
    for (key, schema) in builtin {
        // The deepest group claims it, as VS Code resolves children first.
        let group = (TOC.iter().enumerate())
            .filter(|(_, t)| t.3.iter().any(|p| matches(p, &key)))
            .max_by_key(|(ix, t)| (t.2, usize::MAX - ix))
            .map_or(extensions_group, |(ix, _)| ix);
        found.push((group, false, f64::MAX, key, schema, String::new()));
    }

    // Extensions' groups, by label: one per extension, or one per
    // configuration section under the extension's when it has several.
    let mut ext_groups = Vec::new();
    for ext in extensions::installed().iter().filter(|e| e.enabled) {
        let sections: Vec<&Value> = (ext.contributes("configuration"))
            .filter(|c| c["properties"].as_object().is_some_and(|p| !p.is_empty()))
            .collect();
        if sections.is_empty() {
            continue;
        }
        let name = ext.name();
        let title = |c: &Value| Some(ext.localize(&c["title"])).filter(|t| !t.is_empty());
        let label = match sections.as_slice() {
            [one] => title(one).unwrap_or_else(|| name.clone()),
            _ => name.clone(),
        };
        let mut children = Vec::new();
        for c in &sections {
            let child = match sections.len() {
                1 => None,
                _ => Some(title(c).unwrap_or_else(|| name.clone())).filter(|t| *t != label),
            };
            let order = c["order"].as_f64().unwrap_or(f64::MAX);
            let id = (c["id"].as_str().map(String::from)).or_else(|| child.clone()).unwrap_or_default();
            let props: Vec<(String, Value, String)> = (c["properties"].as_object().into_iter().flatten())
                .map(|(key, schema)| {
                    let desc = schema["markdownDescription"].clone();
                    let (desc, markdown) = match desc.is_null() {
                        true => (ext.localize(&schema["description"]), false),
                        false => (ext.localize(&desc), true),
                    };
                    let mut schema = schema.clone();
                    schema["$markdown"] = markdown.into();
                    (key.clone(), schema, desc)
                })
                .collect();
            children.push((child, id, order, props));
        }
        children.sort_by(|a, b| a.2.total_cmp(&b.2));
        ext_groups.push((label, ext.id.clone(), children));
    }
    ext_groups.sort_by_key(|g| g.0.to_lowercase());
    for (label, id, children) in ext_groups {
        groups.push(Group {
            id: id.clone(),
            label,
            level: 2,
            parent: Some(extensions_group),
        });
        let top = groups.len() - 1;
        for (child, child_id, _, props) in children {
            let group = match child {
                Some(label) => {
                    groups.push(Group {
                        id: child_id,
                        label,
                        level: 3,
                        parent: Some(top),
                    });
                    groups.len() - 1
                }
                None => top,
            };
            for (key, schema, desc) in props {
                // VS Code hides a deprecated setting unless it's set.
                let deprecated = !schema["deprecationMessage"].is_null()
                    || !schema["markdownDeprecationMessage"].is_null();
                if schema["included"] == false || (deprecated && values.get(&key).is_none()) {
                    continue;
                }
                let tags = schema["tags"].as_array();
                let experimental = tags.is_some_and(|t| t.iter().any(|t| t == "experimental"));
                let order = schema["order"].as_f64().unwrap_or(f64::MAX);
                found.push((group, experimental, order, key, schema, desc));
            }
        }
    }
    // Commonly Used keeps its order; the rest sort as VS Code's sortSettings.
    found.sort_by(|a, b| {
        (a.0.cmp(&b.0))
            .then(a.1.cmp(&b.1))
            .then(a.2.total_cmp(&b.2))
            .then(if a.0 == 0 { Ordering::Equal } else { a.3.cmp(&b.3) })
    });
    let link = Regex::new(r"`#([^#`\s]+)#`").unwrap();
    let entries = (found.into_iter())
        .map(|(group, _, _, key, schema, desc)| {
            let markdown = schema["$markdown"].as_bool().unwrap_or(!schema["markdownDescription"].is_null());
            let desc = match desc.is_empty() {
                true => (schema["markdownDescription"].as_str())
                    .or(schema["description"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                false => desc,
            };
            // `#key#` links to that setting, titled as it is outside any group.
            let desc = link.replace_all(&desc, |c: &regex::Captures| {
                let (category, label) = display(&c[1], "");
                format!("[{category}: {label}](#{})", &c[1])
            });
            let (category, label) = display(&key, &groups[group].id);
            Entry {
                key,
                group,
                schema,
                category,
                label,
                description: desc.into_owned().into(),
                markdown,
            }
        })
        .collect();
    (groups, entries)
}

impl Page {
    /// The rows for `query`: every group's heading and settings, or just the
    /// settings that contain all its words.
    fn filter(&mut self, query: &str) {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.rows = match words.is_empty() {
            true => {
                let mut rows = Vec::new();
                let has = |g: usize| {
                    (self.entries.iter()).any(|e| {
                        let mut at = Some(e.group);
                        while let Some(a) = at {
                            if a == g {
                                return true;
                            }
                            at = self.groups[a].parent;
                        }
                        false
                    })
                };
                for g in 0..self.groups.len() {
                    if has(g) {
                        rows.push(Row::Header(g));
                        rows.extend((self.entries.iter().enumerate()).filter(|(_, e)| e.group == g).map(|(i, _)| Row::Setting(i)));
                    }
                }
                rows
            }
            false => (self.entries.iter().enumerate())
                .filter(|(_, e)| e.group != 0)
                .filter(|(_, e)| {
                    let text = format!("{} {}: {} {}", e.key, e.category, e.label, e.description).to_lowercase();
                    words.iter().all(|w| text.contains(w))
                })
                .map(|(i, _)| Row::Setting(i))
                .collect(),
        };
        self.list.reset(self.rows.len());
    }
}

/// A setting's options: its enum's values, each with its label.
fn options(schema: &Value) -> Vec<(Value, String)> {
    let labels = schema["enumItemLabels"].as_array();
    (schema["enum"].as_array().into_iter().flatten().enumerate())
        .map(|(i, v)| {
            let label = (labels.and_then(|l| l.get(i)?.as_str()).map(String::from))
                .or_else(|| v.as_str().map(String::from))
                .unwrap_or_else(|| v.to_string());
            (v.clone(), label)
        })
        .collect()
}

fn is_type(schema: &Value, ty: &str) -> bool {
    match &schema["type"] {
        Value::String(t) => t == ty,
        Value::Array(t) => t.iter().any(|t| t == ty),
        _ => false,
    }
}

/// A number or string setting's value as its text box shows it.
fn input_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

impl Workbench {
    /// Preferences: Open Settings (UI), searching for `query`; the open
    /// Settings editor if there is one.
    pub(super) fn open_settings_ui(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.docs.iter().position(|d| d.settings.is_some()) {
            self.activate(ix, window, cx);
        } else {
            let values = super::terminal::settings();
            let (groups, entries) = catalog(&values);
            let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));
            cx.subscribe_in(&search, window, |this, input, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    let query = input.read(cx).value().to_string();
                    if let Some(page) = this.docs.iter_mut().find_map(|d| d.settings.as_mut()) {
                        page.filter(&query);
                    }
                    cx.notify();
                }
            })
            .detach();
            let mut inputs = HashMap::new();
            for e in &entries {
                let number = is_type(&e.schema, "number") || is_type(&e.schema, "integer");
                if e.schema["enum"].is_array() || inputs.contains_key(&e.key) || !(number || is_type(&e.schema, "string")) {
                    continue;
                }
                let value = values.get(&e.key).unwrap_or(&e.schema["default"]);
                let text = input_text(value);
                let input = cx.new(|cx| InputState::new(window, cx).default_value(text));
                let (key, integer) = (e.key.clone(), is_type(&e.schema, "integer"));
                cx.subscribe_in(&input, window, move |this, input, ev: &InputEvent, window, cx| {
                    if !matches!(ev, InputEvent::Change) {
                        return;
                    }
                    let text = input.read(cx).value().to_string();
                    let value = match (number, integer) {
                        (false, _) => text.into(),
                        (true, true) => match text.trim().parse::<i64>() {
                            Ok(n) => n.into(),
                            Err(_) => return,
                        },
                        (true, false) => match text.trim().parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
                            // 1000, not 1000.0.
                            Some(n) => n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 1e15).map_or(n.into(), |f| (f as i64).into()),
                            None => return,
                        },
                    };
                    this.set_ui_setting(&key, value, window, cx);
                })
                .detach();
                inputs.insert(e.key.clone(), input);
            }
            let mut page = Page {
                search,
                list: ListState::new(0, ListAlignment::Top, px(400.)),
                groups,
                entries,
                rows: Vec::new(),
                expanded: HashSet::new(),
                inputs,
            };
            page.filter("");
            // Binary, so saving and the editor's commands leave its empty text alone.
            self.push_doc(PathBuf::from("Settings"), String::new(), Encoding::Utf8, true, false, window, cx);
            self.docs.last_mut().unwrap().settings = Some(Box::new(page));
        }
        let page = self.docs.iter().find_map(|d| d.settings.as_ref()).unwrap();
        let search = page.search.clone();
        if !query.is_empty() {
            search.update(cx, |s, cx| s.set_value(query.to_string(), window, cx));
        }
        search.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Writes a setting from the Settings editor; its default removes it from
    /// settings.json, as VS Code resets it.
    fn set_ui_setting(&mut self, key: &str, value: Value, window: &mut Window, cx: &mut Context<Self>) {
        let page = self.docs.iter().find_map(|d| d.settings.as_ref());
        let entry = page.and_then(|p| p.entries.iter().find(|e| e.key == key));
        let default = entry.map(|e| e.schema["default"].clone()).unwrap_or_default();
        let value = if value == default { Value::Null } else { value };
        let old = super::terminal::settings();
        if let Err(err) = super::terminal::set_setting(key, value.clone()) {
            return self.settings_error(err, cx);
        }
        // A reset shows the default in the text box.
        if let Some(input) = page.and_then(|p| p.inputs.get(key)).cloned() {
            let text = input_text(if value.is_null() { &default } else { &value });
            if input.read(cx).value() != text && value.is_null() {
                input.update(cx, |i, cx| i.set_value(text, window, cx));
            }
        }
        self.settings_changed(&old, window, cx);
        // An open settings.json without changes shows the new value.
        let json = super::terminal::settings_path();
        if let Some(ix) = (self.docs.iter()).position(|d| !d.dirty && json.as_ref() == Some(&d.path)) {
            self.reopen(ix, self.docs[ix].encoding, window, cx);
        }
        cx.notify();
    }

    /// The Settings editor in doc `ix`: the search box, the User tab, the
    /// table of contents and the settings.
    // ponytail: no Workspace tab (vsn has no workspace settings), no
    // @-filters, and the TOC stays at any width (VS Code hides it under 700px).
    pub(super) fn render_settings(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(page) = self.docs[ix].settings.as_ref() else {
            return div().into_any_element();
        };
        let values = Rc::new(super::terminal::settings());
        let searching = !page.search.read(cx).value().trim().is_empty();
        let found = page.rows.iter().filter(|r| matches!(r, Row::Setting(_))).count();
        let count = match (searching, found) {
            (false, _) => String::new(),
            (true, 0) => "No Settings Found".into(),
            (true, 1) => "1 Setting Found".into(),
            (true, n) => format!("{n} Settings Found"),
        };
        let header = v_flex()
            .px(px(24.))
            .pt(px(14.))
            .child(
                h_flex()
                    .relative()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(c(0x3C3C3C))
                    .bg(col::input())
                    .px(px(6.))
                    .py(px(2.))
                    .child(div().flex_1().child(Input::new(&page.search).appearance(false).text_size(px(13.))))
                    .child(div().pl(px(6.)).text_size(px(11.)).text_color(col::fg_muted()).child(count)),
            )
            .child(
                h_flex().mt(px(10.)).pb(px(4.)).child(
                    div()
                        .px(px(8.))
                        .py(px(2.))
                        .rounded(px(4.))
                        .bg(c(0x2C2D2E))
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(0xFFFFFF))
                        .child("User"),
                ),
            );

        // The TOC: the group at the top of the list, the groups with
        // settings showing, and their counts while searching.
        let top = page.list.logical_scroll_top().item_ix;
        let current = (page.rows.iter().take(top + 1).rev()).find_map(|r| match r {
            Row::Header(g) => Some(*g),
            Row::Setting(e) if searching => Some(page.entries[*e].group),
            Row::Setting(_) => None,
        });
        let mut counts = vec![0; page.groups.len()];
        for r in &page.rows {
            if let Row::Setting(e) = r {
                let mut at = Some(page.entries[*e].group);
                while let Some(g) = at {
                    counts[g] += 1;
                    at = page.groups[g].parent;
                }
            }
        }
        let mut open: HashSet<usize> = page.expanded.clone();
        let mut at = current;
        while let Some(g) = at {
            open.insert(g);
            at = page.groups[g].parent;
        }
        let shown = |g: usize| {
            let mut at = page.groups[g].parent;
            while let Some(p) = at {
                if !(searching || open.contains(&p)) {
                    return false;
                }
                at = page.groups[p].parent;
            }
            counts[g] > 0
        };
        let toc = (0..page.groups.len()).filter(|g| shown(*g)).map(|g| {
            let group = &page.groups[g];
            let parent = (page.groups.iter()).any(|c| c.parent == Some(g)) && !searching;
            let selected = current == Some(g);
            let expanded = open.contains(&g);
            h_flex()
                .id(("settings-toc", g))
                .h(px(22.))
                .flex_none()
                .pl(px((group.level - 1) as f32 * 8.))
                .pr(px(8.))
                .cursor_pointer()
                .text_size(px(13.))
                .map(|d| match selected {
                    true => d.font_weight(FontWeight::BOLD).text_color(c(0xFFFFFF)),
                    false => d.text_color(col::fg()).opacity(0.9),
                })
                .child(
                    div().w(px(16.)).flex_none().when(parent, |d| {
                        d.child(codicon(if expanded { icon::CHEVRON_DOWN } else { icon::CHEVRON_RIGHT }))
                    }),
                )
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().child(group.label.clone()))
                .when(searching, |d| d.child(div().text_color(col::fg_muted()).child(format!("({})", counts[g]))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let Some(page) = this.docs.get_mut(ix).and_then(|d| d.settings.as_mut()) else {
                        return;
                    };
                    if !page.expanded.insert(g) && selected {
                        page.expanded.remove(&g);
                    }
                    let row = page.rows.iter().position(|r| match r {
                        Row::Header(h) => *h == g,
                        Row::Setting(e) => page.entries[*e].group == g,
                    });
                    if let Some(row) = row {
                        page.list.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.) });
                    }
                    cx.notify();
                }))
        });

        let wb = cx.weak_entity();
        let body = list(page.list.clone(), move |row, window, cx| {
            let Some(this) = wb.upgrade() else {
                return div().into_any_element();
            };
            let this = this.read(cx);
            let Some(page) = this.docs.get(ix).and_then(|d| d.settings.as_ref()) else {
                return div().into_any_element();
            };
            match page.rows.get(row) {
                Some(Row::Header(g)) => {
                    let group = &page.groups[*g];
                    div()
                        .p(px(10.))
                        .pl(px(15.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(match group.level {
                            1 => 26.,
                            2 => 22.,
                            _ => 18.,
                        }))
                        .child(group.label.clone())
                        .into_any_element()
                }
                Some(Row::Setting(e)) => setting_row(page, *e, &values, wb.clone(), window, cx),
                None => div().into_any_element(),
            }
        });

        v_flex()
            .size_full()
            .items_center()
            .text_color(col::fg())
            .child(
                v_flex()
                    .size_full()
                    .max_w(px(1400.))
                    .child(header)
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .mt(px(14.))
                            .items_start()
                            .child(v_flex().id("settings-toc").w(px(200.)).flex_none().h_full().pl(px(24.)).overflow_y_scroll().children(toc))
                            .child(
                                div()
                                    .id("settings-body")
                                    .flex_1()
                                    .h_full()
                                    .min_w_0()
                                    .relative()
                                    .child(body.size_full())
                                    .child(self.slider("settings", &page.list, Axis::Vertical, 8.))
                                    .on_hover(self.hover_slider("settings", cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// A setting: its gear menu, title, description and control, the modified
/// bar when settings.json sets it.
fn setting_row(
    page: &Page,
    e: usize,
    values: &Rc<Value>,
    wb: WeakEntity<Workbench>,
    _: &mut Window,
    cx: &App,
) -> AnyElement {
    let entry = &page.entries[e];
    let key = entry.key.clone();
    let modified = values.get(&key).is_some();
    let value = values.get(&key).unwrap_or(&entry.schema["default"]).clone();
    let bool_row = is_type(&entry.schema, "boolean") && !entry.schema["enum"].is_array();
    let hover = SharedString::from(format!("setting-{e}"));

    let description = |id: &'static str| {
        let text = entry.description.clone();
        let d = div().text_size(px(13.)).text_color(col::fg()).opacity(0.9);
        match entry.markdown {
            true => d.child({
                let wb = wb.clone();
                TextView::markdown((id, e), text)
                    .selectable(true)
                    .on_link_click(move |href, _, _, cx| match href.strip_prefix('#') {
                        // A setting's link scrolls to it, if it's listed.
                        Some(key) => _ = wb.update(cx, |this, cx| {
                            let Some(page) = this.docs.iter().find_map(|d| d.settings.as_ref()) else {
                                return;
                            };
                            let row = page.rows.iter().position(|r| {
                                matches!(r, Row::Setting(e) if page.entries[*e].key == key && page.entries[*e].group != 0)
                            });
                            if let Some(item_ix) = row {
                                page.list.scroll_to(ListOffset { item_ix, offset_in_item: px(0.) });
                                cx.notify();
                            }
                        }),
                        None => cx.open_url(href),
                    })
            }),
            false => d.child(text),
        }
    };

    let set = {
        let (wb, key) = (wb.clone(), key.clone());
        move |value: Value, window: &mut Window, cx: &mut App| {
            wb.update(cx, |this, cx| this.set_ui_setting(&key, value, window, cx)).ok();
        }
    };
    let control: AnyElement = if let Some(input) = page.inputs.get(&key) {
        let number = !is_type(&entry.schema, "string");
        div()
            .w(px(if number { 200. } else { 420. }))
            .max_w_full()
            .rounded(px(4.))
            .border_1()
            .border_color(col::input_border())
            .bg(col::input())
            .px(px(6.))
            .py(px(2.))
            .child(Input::new(input).appearance(false).text_size(px(13.)))
            .into_any_element()
    } else if entry.schema["enum"].is_array() {
        let options = options(&entry.schema);
        let current = (options.iter().find(|(v, _)| *v == value))
            .map_or_else(|| input_text(&value), |(_, l)| l.clone());
        let set = set.clone();
        Button::new(("setting-enum", e))
            .w(px(320.))
            .h(px(26.))
            .px(px(6.))
            .rounded(px(4.))
            .border_1()
            .border_color(c(0x3C3C3C))
            .bg(c(0x313131))
            .text_size(px(13.))
            .text_color(col::fg())
            .child(
                h_flex()
                    .w_full()
                    .child(div().flex_1().text_left().overflow_hidden().whitespace_nowrap().child(current))
                    .child(codicon(icon::CHEVRON_DOWN)),
            )
            .dropdown_menu(move |mut menu, _, _| {
                for (v, label) in options.iter().cloned() {
                    let set = set.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(v == value)
                            .on_click(move |_, window, cx| set(v.clone(), window, cx)),
                    );
                }
                menu
            })
            .into_any_element()
    } else if bool_row {
        let on = value == true;
        let set = set.clone();
        h_flex()
            .id(("setting-check", e))
            .items_start()
            .gap(px(9.))
            .cursor_pointer()
            .on_click(move |_, window, cx| set(json!(!on), window, cx))
            .child(
                div()
                    .flex_none()
                    .mt(px(1.))
                    .size(px(18.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(c(0x707070))
                    .bg(c(0x242526))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(c(0xCCCCCC))
                    .when(on, |d| d.child(codicon(icon::CHECK))),
            )
            .child(description("setting-md-bool"))
            .into_any_element()
    } else {
        let wb = wb.clone();
        div()
            .id(("setting-json", e))
            .text_size(px(13.))
            .text_color(c(0x48A0C7))
            .cursor_pointer()
            .hover(|d| d.underline())
            .child("Edit in settings.json")
            .on_click(move |_, window, cx| {
                wb.update(cx, |this, cx| this.open_settings(window, cx)).ok();
            })
            .into_any_element()
    };

    let default = entry.schema["default"].clone();
    let gear = {
        let (key, set) = (key.clone(), set.clone());
        Button::new(("setting-gear", e))
            .ghost()
            .compact()
            .size(px(22.))
            .text_color(col::fg())
            .child(codicon(icon::SETTINGS_GEAR))
            .tooltip("More Actions...")
            .dropdown_menu(move |menu, _, _| {
                let (set, id, key) = (set.clone(), key.clone(), key.clone());
                let as_json = format!("\"{key}\": {}", serde_json::to_string_pretty(&value_or(&key, &default)).unwrap_or_default());
                menu.item(PopupMenuItem::new("Reset Setting").on_click(move |_, window, cx| set(Value::Null, window, cx)))
                    .separator()
                    .item(PopupMenuItem::new("Copy Setting ID").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(id.clone()))
                    }))
                    .item(PopupMenuItem::new("Copy Setting as JSON").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(as_json.clone()))
                    }))
            })
    };
    let _ = cx;

    div()
        .id(("setting-row", e))
        .group(hover.clone())
        .relative()
        .pl(px(22.))
        .pr(px(8.))
        .child(div().absolute().left_0().top(px(8.)).invisible().group_hover(hover, |s| s.visible()).child(gear))
        .child(
            v_flex()
                .relative()
                .rounded(px(6.))
                .px(px(14.))
                .pt(px(12.))
                .pb(px(if bool_row { 26. } else { 18. }))
                .hover(|d| d.bg(ca(0xFFFFFF06)))
                .when(modified, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left(px(5.))
                            .top(px(15.))
                            .bottom(px(if bool_row { 23. } else { 18. }))
                            .w(px(2.))
                            .bg(ca(0xBB800966)),
                    )
                })
                .child(
                    h_flex()
                        .pb(px(2.))
                        .text_size(px(13.))
                        .text_color(c(0xFFFFFF))
                        .when(!entry.category.is_empty(), |d| {
                            d.child(div().opacity(0.9).child(format!("{}: ", entry.category)))
                        })
                        .child(div().mr(px(7.)).font_weight(FontWeight::SEMIBOLD).child(entry.label.clone())),
                )
                .when(!bool_row, |d| d.child(div().mt(px(-1.)).child(description("setting-md"))))
                .child(div().mt(px(if bool_row { 4. } else { 9. })).child(control)),
        )
        .into_any_element()
}

/// Copy Setting as JSON's value: the user's, or else the default.
fn value_or(key: &str, default: &Value) -> Value {
    super::terminal::settings().get(key).cloned().unwrap_or_else(|| default.clone())
}

#[cfg(test)]
mod tests {
    use super::display;

    #[test]
    fn titles() {
        let t = |key, group| {
            let (c, l) = display(key, group);
            if c.is_empty() { l } else { format!("{c}: {l}") }
        };
        assert_eq!(t("files.autoSave", "commonlyUsed"), "Files: Auto Save");
        assert_eq!(t("files.autoSave", "editor/files"), "Auto Save");
        assert_eq!(t("workbench.colorTheme", "workbench/appearance"), "Color Theme");
        assert_eq!(t("workbench.experimental.modernUI", "workbench"), "Experimental: Modern UI");
        assert_eq!(
            t("terminal.integrated.defaultProfile.windows", "features/terminal"),
            "Integrated › Default Profile: Windows"
        );
        assert_eq!(t("explorer.autoReveal", "features/explorer"), "Auto Reveal");
        assert_eq!(t("rust-analyzer.cargo.features", "rust-lang.rust-analyzer"), "Cargo: Features");
        assert_eq!(t("json.schemaDownload.enable", "commonlyUsed"), "JSON › Schema Download: Enable");
    }
}
