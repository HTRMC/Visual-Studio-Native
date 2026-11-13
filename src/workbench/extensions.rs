//! Extensions, as EXTENSIONS.md lays them out: VS Code's on-disk layout in
//! `~/.vsn/extensions`, installed from a .vsix, and their declarative
//! contributions (color themes, snippets, languages) read straight from
//! their `package.json`.

use std::{
    collections::HashSet,
    fs,
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::{
        LazyLock, Mutex, RwLock, RwLockReadGuard,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde_json::{Map, Value, json};

use gpui_kit::base::input::language_config::LanguageConfig;
use gpui_kit::base::input::{
    AutoClosingPair, BracketPair, IndentAction, IndentationRules, OnEnterRule, SyntaxContext,
};
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry, text::TextView};

use super::debug::strip_jsonc;
use super::*;
use crate::theme::col::Colors;

/// The built-in color theme, which `workbench.colorTheme` defaults to.
pub(crate) const DEFAULT_THEME: &str = "Dark 2026";

pub(crate) struct Extension {
    /// `publisher.name`, lowercased, as VS Code identifies it.
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Value,
    /// package.nls.json: the strings `%key%` in the manifest stand for.
    nls: Value,
    pub enabled: bool,
}

impl Extension {
    fn text(&self, key: &str) -> String {
        self.localize(&self.manifest[key])
    }

    /// A manifest string, `%key%` looked up in package.nls.json.
    pub(super) fn localize(&self, value: &Value) -> String {
        // Some manifests give `{ "value": ..., "original": ... }`.
        let s = (value.as_str().or(value["value"].as_str())).unwrap_or_default();
        match s.strip_prefix('%').and_then(|s| s.strip_suffix('%')) {
            Some(key) => self.nls[key].as_str().unwrap_or(s).to_string(),
            None => s.to_string(),
        }
    }

    pub fn name(&self) -> String {
        let name = self.text("displayName");
        if name.is_empty() {
            self.text("name")
        } else {
            name
        }
    }

    pub fn description(&self) -> String {
        self.text("description")
    }

    pub fn publisher(&self) -> String {
        self.text("publisher")
    }

    pub fn version(&self) -> String {
        self.text("version")
    }

    pub fn icon(&self) -> Option<PathBuf> {
        let icon = self.dir.join(self.manifest["icon"].as_str()?);
        icon.is_file().then_some(icon)
    }

    /// The entries of contribution point `point` (a list, or one object).
    pub(super) fn contributes(&self, point: &str) -> impl Iterator<Item = &Value> {
        let value = &self.manifest["contributes"][point];
        match value {
            Value::Array(list) => list.as_slice(),
            Value::Object(_) => slice::from_ref(value),
            _ => &[],
        }
        .iter()
    }

    /// A path in the extension, from its manifest; None outside it.
    fn file(&self, rel: &Value) -> Option<PathBuf> {
        let rel = Path::new(rel.as_str()?);
        let path = self.dir.join(rel);
        (rel.components())
            .all(|c| {
                matches!(
                    c,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            })
            .then_some(path)
    }
}

/// `~/.vsn/extensions`, VS Code's `~/.vscode/extensions`.
fn root() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".vsn").join("extensions"))
}

/// The ids of disabled extensions, one per line.
fn disabled_path() -> Option<PathBuf> {
    Some(root()?.join(".disabled"))
}

static INSTALLED: LazyLock<RwLock<Vec<Extension>>> = LazyLock::new(Default::default);

/// Every installed extension, by name.
pub(crate) fn installed() -> RwLockReadGuard<'static, Vec<Extension>> {
    INSTALLED.read().unwrap()
}

/// Reads the installed extensions again, and what their languages contribute.
pub(crate) fn reload() {
    let disabled: HashSet<String> = disabled_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let read = |path: PathBuf| -> Option<Value> {
        let text = fs::read_to_string(path).ok()?;
        serde_json::from_str(&strip_jsonc(text.trim_start_matches('\u{feff}'))).ok()
    };
    let mut list: Vec<Extension> = (root().and_then(|r| fs::read_dir(r).ok()).into_iter())
        .flatten()
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| {
            let dir = e.path();
            let manifest = read(dir.join("package.json"))?;
            let id = format!(
                "{}.{}",
                manifest["publisher"].as_str()?,
                manifest["name"].as_str()?
            )
            .to_lowercase();
            Some(Extension {
                enabled: !disabled.contains(&id),
                nls: read(dir.join("package.nls.json")).unwrap_or_default(),
                id,
                dir,
                manifest,
            })
        })
        .collect();
    list.sort_by_key(|e| e.name().to_lowercase());
    *LANGS.write().unwrap() = languages_of(&list);
    *INSTALLED.write().unwrap() = list;
}

/// Installs the .vsix at `vsix` (replacing another version of it) and
/// returns its name.
pub(crate) fn install_vsix(vsix: &Path) -> Result<String, String> {
    // One at a time: they share the unpack folder.
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let root = root().ok_or("No home folder")?;
    let tmp = root.join(format!(".install-{}", std::process::id()));
    fs::remove_dir_all(&tmp).ok();
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let done = unpack(vsix, &tmp).and_then(|()| {
        let unpacked = tmp.join("extension");
        let text = fs::read_to_string(unpacked.join("package.json"))
            .map_err(|_| "Extension manifest not found".to_string())?;
        let manifest: Value =
            serde_json::from_str(&strip_jsonc(text.trim_start_matches('\u{feff}')))
                .map_err(|e| format!("Invalid manifest: {e}"))?;
        // These name a folder, so nothing that could leave `root`.
        let part = |key: &str| {
            (manifest[key].as_str())
                .filter(|s| {
                    !s.is_empty()
                        && !s.starts_with('.')
                        && s.chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
                })
                .ok_or(format!(
                    "Invalid manifest: '{key}' is missing or not a name"
                ))
        };
        let id = format!("{}.{}", part("publisher")?, part("name")?).to_lowercase();
        let dest = root.join(format!("{id}-{}", part("version")?));
        for e in fs::read_dir(&root).into_iter().flatten().flatten() {
            if e.file_name()
                .to_string_lossy()
                .to_lowercase()
                .starts_with(&format!("{id}-"))
            {
                fs::remove_dir_all(e.path()).map_err(|e| e.to_string())?;
            }
        }
        fs::rename(&unpacked, &dest).map_err(|e| e.to_string())?;
        let name = manifest["displayName"]
            .as_str()
            .or(manifest["name"].as_str());
        Ok(name.unwrap_or_default().to_string())
    });
    fs::remove_dir_all(&tmp).ok();
    reload();
    done
}

/// The registry the Extensions view searches and installs from.
const OPEN_VSX: &str = "https://open-vsx.org/api";

/// Whether the Extensions view's query goes to Open VSX rather than the Installed list.
pub(super) fn searches_market(query: &str) -> bool {
    let query = query.trim();
    !query.is_empty() && !query.starts_with("@installed")
}

/// An Open VSX search result.
pub(super) struct Listing {
    /// `namespace.name`, lowercased, as installed extensions' ids are.
    id: String,
    name: String,
    publisher: String,
    version: String,
    description: String,
    /// The .vsix's URL.
    download: String,
}

/// The Extensions view's Open VSX search.
#[derive(Default)]
pub(super) struct Market {
    /// The query searched for, and its results or the error.
    results: Option<(String, Result<Vec<Listing>, String>)>,
    /// Bumped per query, so an earlier search landing late is dropped.
    generation: usize,
    /// The ids being downloaded and installed.
    installing: HashSet<String>,
}

fn search_open_vsx(query: &str) -> Result<Vec<Listing>, String> {
    let text = ureq::get(format!("{OPEN_VSX}/-/search"))
        .query("query", query)
        .query("size", "30")
        .call()
        .and_then(|mut r| r.body_mut().read_to_string())
        .map_err(|e| e.to_string())?;
    let found: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let s = |e: &Value, key: &str| e[key].as_str().unwrap_or_default().to_string();
    let listings = (found["extensions"].as_array().into_iter().flatten()).map(|e| Listing {
        id: format!("{}.{}", s(e, "namespace"), s(e, "name")).to_lowercase(),
        name: (e["displayName"].as_str()).map_or_else(|| s(e, "name"), str::to_string),
        publisher: s(e, "namespace"),
        version: s(e, "version"),
        description: s(e, "description"),
        download: e["files"]["download"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    });
    Ok(listings.collect())
}

/// The .vsix built for this platform, as VS Code installs, when the
/// extension has one: a search lists only the universal one, which some
/// (rust-analyzer's) ship without their native server.
fn platform_download(id: &str) -> Option<String> {
    let target = format!(
        "{}-{}",
        match std::env::consts::OS {
            "windows" => "win32",
            "macos" => "darwin",
            os => os,
        },
        match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            arch => arch,
        }
    );
    let (namespace, name) = id.split_once('.')?;
    let text = ureq::get(format!("{OPEN_VSX}/{namespace}/{name}"))
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    let e: Value = serde_json::from_str(&text).ok()?;
    Some(e["downloads"][target].as_str()?.to_string())
}

/// Downloads the .vsix at `url` and installs it, returning its name.
fn install_from(url: &str) -> Result<String, String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = root().ok_or("No home folder")?;
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let vsix = root.join(format!(".download-{}-{n}.vsix", std::process::id()));
    let download = || -> Result<(), String> {
        let mut r = ureq::get(url).call().map_err(|e| e.to_string())?;
        let mut file = fs::File::create(&vsix).map_err(|e| e.to_string())?;
        std::io::copy(&mut r.body_mut().as_reader(), &mut file).map_err(|e| e.to_string())?;
        Ok(())
    };
    let done = download().and_then(|()| install_vsix(&vsix));
    fs::remove_file(&vsix).ok();
    done
}

/// Unzips a .vsix's `extension/` folder into `into`.
// ponytail: bsdtar (in Windows since 10 1803, and macOS) reads zips and
// refuses absolute and `..` paths; a zip reader over flate2 if a platform lacks it.
fn unpack(vsix: &Path, into: &Path) -> Result<(), String> {
    let tar = match std::env::var_os("SystemRoot") {
        Some(sys) if cfg!(windows) => PathBuf::from(sys).join("System32").join("tar.exe"),
        _ => PathBuf::from("tar"),
    };
    let mut command = Command::new(tar);
    command
        .arg("-xf")
        .arg(vsix)
        .arg("-C")
        .arg(into)
        .arg("extension");
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000); // CREATE_NO_WINDOW
    let out = command
        .output()
        .map_err(|e| format!("Could not unpack: {e}"))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(format!(
            "Not a valid extension package: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

pub(crate) fn uninstall(id: &str) -> Result<(), String> {
    // A reinstall starts enabled.
    set_enabled(id, true)?;
    let dir = installed()
        .iter()
        .find(|e| e.id == id)
        .map(|e| e.dir.clone());
    let done = match dir {
        Some(dir) => fs::remove_dir_all(dir).map_err(|e| e.to_string()),
        None => Ok(()),
    };
    reload();
    done
}

pub(crate) fn set_enabled(id: &str, enabled: bool) -> Result<(), String> {
    let path = disabled_path().ok_or("No home folder")?;
    let mut disabled: Vec<String> = (installed().iter())
        .filter(|e| !e.enabled && e.id != id)
        .map(|e| e.id.clone())
        .collect();
    if !enabled {
        disabled.push(id.to_string());
    }
    fs::create_dir_all(path.parent().unwrap())
        .and_then(|()| fs::write(path, disabled.join("\n")))
        .map_err(|e| e.to_string())?;
    reload();
    Ok(())
}

// ---- languages -------------------------------------------------------------

/// A language from `contributes.languages`, with its configuration's comments
/// and editing rules.
#[derive(Default)]
struct Lang {
    id: &'static str,
    label: Option<&'static str>,
    /// File name endings (".foo") and whole file names, lowercased.
    extensions: Vec<String>,
    filenames: Vec<String>,
    line_comment: Option<&'static str>,
    block_comment: Option<(&'static str, &'static str)>,
    /// Its language-configuration.json, for [`editing`].
    config: Option<Value>,
}

static LANGS: LazyLock<RwLock<Vec<Lang>>> = LazyLock::new(Default::default);

// ponytail: languages live as long as the app (leaked), as the built-ins'
// &'static ids do; each reload leaks a few strings more.
fn leak(s: &str) -> &'static str {
    match crate::theme::LANGUAGES.iter().find(|l| **l == s) {
        Some(l) => l,
        None => Box::leak(s.to_string().into_boxed_str()),
    }
}

/// Merges every enabled extension's languages by id, as VS Code does.
fn languages_of(list: &[Extension]) -> Vec<Lang> {
    let mut langs: Vec<Lang> = Vec::new();
    for ext in list.iter().filter(|e| e.enabled) {
        for l in ext.contributes("languages") {
            let Some(id) = l["id"].as_str() else {
                continue;
            };
            let at = match langs.iter().position(|x| x.id == id) {
                Some(at) => at,
                None => {
                    langs.push(Lang {
                        id: leak(id),
                        ..Default::default()
                    });
                    langs.len() - 1
                }
            };
            let lang = &mut langs[at];
            let strings = |key: &str| {
                (l[key].as_array().into_iter().flatten())
                    .filter_map(|s| Some(s.as_str()?.to_lowercase()))
                    .collect::<Vec<_>>()
            };
            lang.extensions.extend(strings("extensions"));
            lang.filenames.extend(strings("filenames"));
            if let Some(alias) = l["aliases"][0].as_str() {
                lang.label.get_or_insert(leak(alias));
            }
            let config = (ext.file(&l["configuration"]))
                .and_then(|p| fs::read_to_string(p).ok())
                .and_then(|t| {
                    serde_json::from_str::<Value>(&strip_jsonc(t.trim_start_matches('\u{feff}')))
                        .ok()
                });
            if let Some(config) = config {
                let comments = &config["comments"];
                if let Some(line) = comments["lineComment"].as_str() {
                    lang.line_comment = Some(leak(line));
                }
                if let (Some(open), Some(close)) = (
                    comments["blockComment"][0].as_str(),
                    comments["blockComment"][1].as_str(),
                ) {
                    lang.block_comment = Some((leak(open), leak(close)));
                }
                lang.config = Some(config);
            }
        }
    }
    langs
}

/// The language an extension gives a file named `name`.
pub(crate) fn language_of(name: &str) -> Option<&'static str> {
    let name = name.to_lowercase();
    let langs = LANGS.read().unwrap();
    (langs.iter().find(|l| l.filenames.contains(&name)))
        // The longest ending wins, as in VS Code: .drawio.svg over .svg.
        .or_else(|| {
            (langs.iter())
                .filter_map(|l| {
                    let e = l.extensions.iter().filter(|e| name.ends_with(e.as_str()));
                    Some((e.map(|e| e.len()).max()?, l))
                })
                .rev()
                .max_by_key(|(len, _)| *len)
                .map(|(_, l)| l)
        })
        .map(|l| l.id)
}

/// Extension languages' ids, for Change Language Mode.
pub(crate) fn language_ids() -> Vec<&'static str> {
    LANGS.read().unwrap().iter().map(|l| l.id).collect()
}

pub(crate) fn language_label(id: &str) -> Option<&'static str> {
    let langs = LANGS.read().unwrap();
    langs
        .iter()
        .find(|l| l.id == id)
        .map(|l| l.label.unwrap_or(l.id))
}

type BlockComment = (&'static str, &'static str);

/// An extension language's line and block comment tokens.
pub(crate) fn comments(id: &str) -> Option<(Option<&'static str>, Option<BlockComment>)> {
    let langs = LANGS.read().unwrap();
    let l = langs.iter().find(|l| l.id == id)?;
    (l.line_comment.is_some() || l.block_comment.is_some())
        .then_some((l.line_comment, l.block_comment))
}

/// Languages' configurations from extensions' `setLanguageConfiguration`,
/// over their language-configuration.json.
static SET_CONFIGS: LazyLock<RwLock<HashMap<String, Value>>> = LazyLock::new(Default::default);

/// An extension's `setLanguageConfiguration` for `id`, or its dispose (null).
pub(crate) fn set_config(id: &str, config: Value) {
    let mut set = SET_CONFIGS.write().unwrap();
    match config {
        Value::Object(_) => set.insert(id.to_string(), config),
        _ => set.remove(id),
    };
}

/// An extension language's brackets, auto-closing pairs, indentation and
/// on-Enter rules, over the editor's defaults.
pub(crate) fn editing(id: &str) -> Option<LanguageConfig> {
    let langs = LANGS.read().unwrap();
    let file = langs.iter().find(|l| l.id == id).and_then(|l| l.config.as_ref());
    let set = SET_CONFIGS.read().unwrap();
    let mut config = match (file, set.get(id)) {
        (None, None) => return None,
        (file, _) => file.cloned().unwrap_or_else(|| json!({})),
    };
    for (k, v) in set.get(id).and_then(Value::as_object).into_iter().flatten() {
        config[k] = v.clone();
    }
    Some(editing_of(&config))
}

// ponytail: the regex crate has no lookaround, which some VS Code
// indentation patterns use; those patterns are skipped (fancy-regex if it
// matters). Pattern flags are ignored too.
fn editing_of(config: &Value) -> LanguageConfig {
    // `["(", ")"]` or `{ "open": "(", "close": ")" }`
    let pair = |p: &Value| {
        let open = p[0].as_str().or(p["open"].as_str())?;
        let close = p[1].as_str().or(p["close"].as_str())?;
        Some((open.to_string(), close.to_string()))
    };
    let mut out = LanguageConfig::default();
    if let Some(list) = config["brackets"].as_array() {
        out = out.brackets(
            list.iter()
                .filter_map(pair)
                .map(|(o, c)| BracketPair::new(o, c)),
        );
    }
    if let Some(list) = config["autoClosingPairs"].as_array() {
        out = out.auto_closing_pairs(list.iter().filter_map(|p| {
            let (open, close) = pair(p)?;
            let not_in =
                (p["notIn"].as_array().into_iter().flatten()).filter_map(|c| match c.as_str()? {
                    "string" => Some(SyntaxContext::String),
                    "comment" => Some(SyntaxContext::Comment),
                    _ => None,
                });
            Some(AutoClosingPair::new(open, close).not_in(not_in))
        }));
    }
    if let Some(chars) = config["autoCloseBefore"].as_str() {
        out = out.auto_close_before(chars.to_string());
    }
    let rules = &config["indentationRules"];
    if rules.is_object() {
        let pattern = |key: &str| {
            let p = &rules[key];
            let p = p.as_str().or(p["pattern"].as_str())?;
            regex::Regex::new(p).ok().map(std::sync::Arc::new)
        };
        let mut r = IndentationRules::default();
        r.increase_indent_pattern = pattern("increaseIndentPattern");
        r.decrease_indent_pattern = pattern("decreaseIndentPattern");
        out = out.indentation_rules(r);
    }
    // `"x"`, `{ "pattern": "x" }`, or from the JS host a RegExp's source.
    let regex = |p: &Value| {
        let p = p.as_str().or(p["pattern"].as_str())?;
        regex::Regex::new(p).ok().map(std::sync::Arc::new)
    };
    if let Some(list) = config["onEnterRules"].as_array() {
        out = out.on_enter_rules(list.iter().filter_map(|r| {
            let action = &r["action"];
            Some(OnEnterRule {
                before: regex(&r["beforeText"])?,
                after: (!r["afterText"].is_null()).then(|| regex(&r["afterText"])).flatten(),
                previous_line: (!r["previousLineText"].is_null())
                    .then(|| regex(&r["previousLineText"]))
                    .flatten(),
                // Numbers from the JS host, names in language-configuration.json.
                action: match &action["indent"].as_str().or(action["indentAction"].as_str()) {
                    Some("indent") => IndentAction::Indent,
                    Some("indentOutdent") => IndentAction::IndentOutdent,
                    Some("outdent") => IndentAction::Outdent,
                    Some(_) => IndentAction::None,
                    None => match action["indentAction"].as_u64() {
                        Some(1) => IndentAction::Indent,
                        Some(2) => IndentAction::IndentOutdent,
                        Some(3) => IndentAction::Outdent,
                        _ => IndentAction::None,
                    },
                },
                append_text: action["appendText"].as_str().unwrap_or_default().to_string().into(),
                remove_text: action["removeText"].as_u64().unwrap_or(0) as usize,
            })
        }));
    }
    out
}

// ---- snippets --------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct Snippet {
    pub name: String,
    pub prefix: String,
    pub body: String,
}

/// The enabled extensions' snippets for language `lang`, by name.
pub(crate) fn snippets(lang: &str) -> Vec<Snippet> {
    let mut out = Vec::new();
    for ext in installed().iter().filter(|e| e.enabled) {
        for s in ext.contributes("snippets") {
            if s["language"].as_str() != Some(lang) {
                continue;
            }
            let Some(file) = (ext.file(&s["path"])).and_then(|p| fs::read_to_string(p).ok()) else {
                continue;
            };
            let Ok(Value::Object(map)) =
                serde_json::from_str(&strip_jsonc(file.trim_start_matches('\u{feff}')))
            else {
                continue;
            };
            out.extend(map.into_iter().filter_map(|(name, s)| {
                let joined = |v: &Value| match v {
                    Value::Array(lines) => Some(
                        (lines.iter().filter_map(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    v => Some(v.as_str()?.to_string()),
                };
                let prefix = match &s["prefix"] {
                    Value::Array(p) => p.first().and_then(Value::as_str).map(str::to_string),
                    p => p.as_str().map(str::to_string),
                };
                Some(Snippet {
                    prefix: prefix.unwrap_or_else(|| name.clone()),
                    name,
                    body: joined(&s["body"])?,
                })
            }));
        }
    }
    out.sort_by_key(|s| s.prefix.to_lowercase());
    out
}

thread_local! {
    /// The snippets the completion menu last offered: their file, and each
    /// one's start, text and tab stops.
    pub(crate) static OFFERED: std::cell::RefCell<Option<Offered>> = const { std::cell::RefCell::new(None) };
}

pub(crate) type Offered = (PathBuf, Vec<(usize, String, Vec<Range<usize>>)>);

/// Adds `list` to the snippets offered in `path`, replacing those offered in
/// another file.
pub(crate) fn offer(path: &Path, list: Vec<(usize, String, Vec<Range<usize>>)>) {
    OFFERED.with_borrow_mut(|offered| match offered {
        Some((p, old)) if p == path => old.extend(list),
        _ => *offered = Some((path.to_path_buf(), list)),
    });
}

/// A snippet just inserted, while Tab moves between its tab stops: its
/// editor, the stops, the one selected and the text's length then.
pub(crate) struct Stops {
    editor: EntityId,
    stops: Vec<Range<usize>>,
    at: usize,
    len: usize,
}

/// Where a snippet goes: its file and language, the line's indent and the
/// editor's indent unit.
pub(crate) struct Place<'a> {
    pub path: &'a Path,
    pub lang: &'a str,
    pub indent: &'a str,
    pub unit: &'a str,
}

/// A snippet's text at `at`, with `selected` as TM_SELECTED_TEXT, and where
/// its tab stops end up.
pub(crate) fn fill(body: &str, at: &Place, selected: &str) -> (String, Vec<Range<usize>>) {
    let path = at.path;
    let file = |f: fn(&Path) -> Option<&std::ffi::OsStr>| {
        f(path).map(|s| s.to_string_lossy().into_owned())
    };
    let var = |name: &str| -> Option<String> {
        Some(match name {
            "TM_FILENAME" => file(Path::file_name)?,
            "TM_FILENAME_BASE" => file(Path::file_stem)?,
            "TM_DIRECTORY" => path.parent()?.to_string_lossy().into_owned(),
            "TM_FILEPATH" => path.to_string_lossy().into_owned(),
            "TM_SELECTED_TEXT" => selected.to_string(),
            "LINE_COMMENT" => line_comment(at.lang)?.to_string(),
            "BLOCK_COMMENT_START" => block_comment(at.lang)?.0.to_string(),
            "BLOCK_COMMENT_END" => block_comment(at.lang)?.1.to_string(),
            _ => return None,
        })
    };
    // Each line after the first takes the current line's indent, and tabs
    // become the editor's indent unit.
    let body = (body.replace('\t', at.unit)).replace('\n', &format!("\n{}", at.indent));
    expand(&body, &var)
}

/// Snippet syntax made plain text: placeholders become their text, choices
/// their first option and variables their value (`var`, else their name).
/// Also gives where the tab stops end up, in tabbing order: $1, $2, … then
/// $0, or the end when there's no $0.
// ponytail: a stop repeated takes its first place only; no linked mirrors.
pub(crate) fn expand(
    body: &str,
    var: &dyn Fn(&str) -> Option<String>,
) -> (String, Vec<Range<usize>>) {
    struct E<'a> {
        s: Vec<char>,
        i: usize,
        out: String,
        stops: Vec<(u32, Range<usize>)>,
        var: &'a dyn Fn(&str) -> Option<String>,
    }
    impl E<'_> {
        fn peek(&self) -> Option<char> {
            self.s.get(self.i).copied()
        }
        fn eat(&mut self, ch: char) -> bool {
            let ate = self.peek() == Some(ch);
            self.i += ate as usize;
            ate
        }
        fn number(&mut self) -> Option<u32> {
            let start = self.i;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
            String::from_iter(&self.s[start..self.i]).parse().ok()
        }
        fn name(&mut self) -> Option<String> {
            let start = self.i;
            while self
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                self.i += 1;
            }
            (self.i > start && !self.s[start].is_ascii_digit())
                .then(|| String::from_iter(&self.s[start..self.i]))
        }
        /// Text up to an unescaped `end`, which is consumed.
        fn any(&mut self, end: Option<char>) {
            while let Some(ch) = self.peek() {
                self.i += 1;
                match ch {
                    _ if Some(ch) == end => return,
                    '\\' if matches!(self.peek(), Some('$' | '}' | '\\')) => {
                        self.out.push(self.s[self.i]);
                        self.i += 1;
                    }
                    '$' => self.dollar(),
                    _ => self.out.push(ch),
                }
            }
        }
        fn dollar(&mut self) {
            let start = self.out.len();
            if let Some(n) = self.number() {
                return self.stops.push((n, start..start));
            }
            if let Some(name) = self.name() {
                let value = (self.var)(&name).unwrap_or(name);
                return self.out.push_str(&value);
            }
            if !self.eat('{') {
                return self.out.push('$');
            }
            if let Some(n) = self.number() {
                if self.eat('|') {
                    // A choice: its first option.
                    let mut first = true;
                    while let Some(ch) = self.peek() {
                        self.i += 1;
                        match ch {
                            '|' if self.eat('}') => break,
                            ',' => first = false,
                            '\\' => {
                                if first {
                                    self.out.extend(self.peek());
                                }
                                self.i += 1;
                            }
                            _ if first => self.out.push(ch),
                            _ => {}
                        }
                    }
                } else if self.eat(':') {
                    self.any(Some('}'));
                } else {
                    self.eat('}');
                }
                return self.stops.push((n, start..self.out.len()));
            }
            let Some(name) = self.name() else {
                return self.out.push_str("${");
            };
            let value = (self.var)(&name);
            if self.eat(':') {
                self.any(Some('}'));
            } else if self.eat('/') {
                // ponytail: transforms are skipped; the value goes in as is.
                let mut depth = 0;
                while let Some(ch) = self.peek() {
                    self.i += 1;
                    match ch {
                        '\\' => self.i += 1,
                        '{' => depth += 1,
                        '}' if depth == 0 => break,
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
            } else {
                self.eat('}');
            }
            match value {
                Some(v) => {
                    self.out.truncate(start);
                    self.out.push_str(&v);
                }
                None if self.out.len() == start => self.out.push_str(&name),
                None => {}
            }
        }
    }
    let mut e = E {
        s: body.chars().collect(),
        i: 0,
        out: String::new(),
        stops: Vec::new(),
        var,
    };
    e.any(None);
    let end = e.out.len();
    // Stable, so a repeated stop keeps its first place first.
    e.stops.sort_by_key(|s| s.0.wrapping_sub(1));
    e.stops.dedup_by_key(|s| s.0);
    let zero = e.stops.last().is_some_and(|s| s.0 == 0);
    let mut stops: Vec<_> = e.stops.into_iter().map(|s| s.1).collect();
    if !zero {
        stops.push(end..end);
    }
    (e.out, stops)
}

// ---- color themes ----------------------------------------------------------

/// A color theme from `contributes.themes`.
#[derive(Clone)]
pub(crate) struct ColorTheme {
    /// Its `workbench.colorTheme` name.
    pub id: String,
    pub label: String,
    pub dark: bool,
    path: PathBuf,
}

/// Every enabled extension's color themes.
pub(crate) fn color_themes() -> Vec<ColorTheme> {
    let mut out = Vec::new();
    for ext in installed().iter().filter(|e| e.enabled) {
        for t in ext.contributes("themes") {
            let (Some(label), Some(path)) = (t["label"].as_str(), ext.file(&t["path"])) else {
                continue;
            };
            let label = match label.strip_prefix('%').and_then(|l| l.strip_suffix('%')) {
                Some(key) => ext.nls[key].as_str().unwrap_or(label),
                None => label,
            };
            out.push(ColorTheme {
                id: t["id"].as_str().unwrap_or(label).to_string(),
                label: label.to_string(),
                dark: !matches!(t["uiTheme"].as_str(), Some("vs" | "hc-light")),
                path,
            });
        }
    }
    out
}

/// A theme file's colors and token rules, after those of the theme it includes.
fn theme_file(path: &Path, depth: usize) -> Option<(Map<String, Value>, Vec<Value>)> {
    let text = fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&strip_jsonc(text.trim_start_matches('\u{feff}'))).ok()?;
    let (mut colors, mut rules) = match v["include"].as_str() {
        Some(inc) if depth < 8 => theme_file(&path.parent()?.join(inc), depth + 1)?,
        _ => Default::default(),
    };
    colors.extend(v["colors"].as_object().cloned().unwrap_or_default());
    // ponytail: tokenColors naming a .tmTheme (plist) file is skipped.
    rules.extend(v["tokenColors"].as_array().cloned().unwrap_or_default());
    Some((colors, rules))
}

/// "#RGB", "#RGBA", "#RRGGBB" or "#RRGGBBAA" as 0xRRGGBBAA.
fn hex(s: &str) -> Option<u32> {
    let s = s.strip_prefix('#')?;
    let full: String = match s.len() {
        3 | 4 => s.chars().flat_map(|c| [c, c]).collect(),
        6 | 8 => s.to_string(),
        _ => return None,
    };
    let n = u32::from_str_radix(&full, 16).ok()?;
    Some(if full.len() == 6 { n << 8 | 0xFF } else { n })
}

/// The gpui-component syntax names, with the TextMate scopes that color
/// them, most specific first.
const SYNTAX: &[(&str, &[&str])] = &[
    ("attribute", &["entity.other.attribute-name"]),
    (
        "boolean",
        &["constant.language.boolean", "constant.language"],
    ),
    ("comment", &["comment"]),
    ("comment.doc", &["comment.block.documentation", "comment"]),
    (
        "constant",
        &["variable.other.constant", "constant.language", "constant"],
    ),
    ("constructor", &["entity.name.class", "entity.name.type"]),
    ("embedded", &["meta.embedded"]),
    ("emphasis", &["markup.italic"]),
    ("emphasis.strong", &["markup.bold"]),
    ("enum", &["entity.name.type.enum", "entity.name.type"]),
    ("function", &["entity.name.function", "support.function"]),
    ("keyword", &["keyword.control", "keyword", "storage"]),
    ("label", &["entity.name.label"]),
    ("link_text", &["string.other.link", "markup.underline.link"]),
    ("link_uri", &["markup.underline.link"]),
    ("number", &["constant.numeric"]),
    ("operator", &["keyword.operator"]),
    (
        "preproc",
        &["meta.preprocessor", "keyword.control.directive", "keyword"],
    ),
    (
        "property",
        &[
            "variable.other.property",
            "variable.other.object.property",
            "support.variable.property",
        ],
    ),
    ("punctuation", &["punctuation"]),
    ("punctuation.bracket", &["punctuation"]),
    (
        "punctuation.delimiter",
        &["punctuation.separator", "punctuation"],
    ),
    (
        "punctuation.list_marker",
        &["punctuation.definition.list", "markup.list"],
    ),
    (
        "punctuation.special",
        &["punctuation.definition.template-expression", "punctuation"],
    ),
    ("string", &["string"]),
    ("string.escape", &["constant.character.escape"]),
    ("string.regex", &["string.regexp"]),
    ("string.special", &["string"]),
    ("string.special.symbol", &["constant.other.symbol"]),
    ("tag", &["entity.name.tag"]),
    ("text.literal", &["markup.inline.raw"]),
    ("title", &["markup.heading"]),
    (
        "type",
        &["entity.name.type", "support.type", "support.class"],
    ),
    ("variable", &["variable"]),
    ("variable.special", &["variable.language"]),
    ("variant", &["variable.other.enummember", "constant"]),
];

/// The settings of the token rule that colors `scope`: the longest selector
/// it starts with, the later rule on a tie, as VS Code ranks them.
// ponytail: plain selectors only; "a b" parent selectors and exclusions are skipped.
fn token_style<'a>(rules: &'a [Value], scope: &str) -> Option<&'a Value> {
    let mut best: Option<(usize, &Value)> = None;
    for rule in rules {
        let selectors: Vec<&str> = match &rule["scope"] {
            Value::String(s) => s.split(',').collect(),
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => continue,
        };
        for sel in selectors.into_iter().map(str::trim) {
            let hit =
                sel == scope || (scope.starts_with(sel) && scope[sel.len()..].starts_with('.'));
            if hit && !sel.contains(' ') && best.is_none_or(|(len, _)| sel.len() >= len) {
                best = Some((sel.len(), &rule["settings"]));
            }
        }
    }
    best.map(|b| b.1)
}

/// `theme` as gpui-component's theme config, and as the workbench's colors.
fn convert(theme: &ColorTheme) -> Option<(ThemeConfig, Colors)> {
    let (colors, rules) = theme_file(&theme.path, 0)?;
    let get = |keys: &[&str]| keys.iter().find_map(|k| hex(colors.get(*k)?.as_str()?));
    let css = |n: u32| format!("#{n:08X}");
    let hsla = |n: u32| -> Hsla { rgba(n).into() };
    // Token rules without a scope set the editor's own colors.
    let global = rules
        .iter()
        .find(|r| r.get("scope").is_none())
        .map(|r| &r["settings"]);
    let global = |key: &str| global.and_then(|g| hex(g[key].as_str()?));
    let (light_fg, light_bg) = (0x3B3B3BFF, 0xFFFFFFFF);
    let editor_bg = get(&["editor.background"])
        .or(global("background"))
        .unwrap_or(if theme.dark { 0x1F1F1FFF } else { light_bg });
    let editor_fg = get(&["editor.foreground"])
        .or(global("foreground"))
        .unwrap_or(if theme.dark { 0xCCCCCCFF } else { light_fg });
    let fg = get(&["foreground", "sideBar.foreground"]).unwrap_or(editor_fg);
    let bg = get(&["sideBar.background"]).unwrap_or(editor_bg);
    // With an alpha of its own, `fg` at `alpha` (0-255).
    let tint = |alpha: u32| fg & 0xFFFFFF00 | alpha;
    let border = get(&[
        "sideBar.border",
        "panel.border",
        "editorGroup.border",
        "contrastBorder",
    ])
    .unwrap_or(tint(0x26));
    let accent = get(&[
        "focusBorder",
        "activityBar.activeBorder",
        "button.background",
    ])
    .unwrap_or(0x0078D4FF);

    let defaults = Colors::default();
    let workbench = Colors {
        bg: hsla(bg),
        editor: hsla(editor_bg),
        border: hsla(border),
        fg: hsla(fg),
        fg_muted: hsla(get(&["descriptionForeground"]).unwrap_or(tint(0xB3))),
        fg_dim: hsla(get(&["disabledForeground"]).unwrap_or(tint(0x66))),
        accent: hsla(accent),
        focus: hsla(get(&["focusBorder"]).unwrap_or(accent)),
        hover: hsla(get(&["list.hoverBackground"]).unwrap_or(tint(0x14))),
        selected: hsla(
            get(&[
                "list.inactiveSelectionBackground",
                "list.activeSelectionBackground",
            ])
            .unwrap_or(tint(0x22)),
        ),
        tab_active: hsla(
            get(&[
                "modernTab.activeBackground",
                "list.inactiveSelectionBackground",
            ])
            .unwrap_or(tint(0x22)),
        ),
        toolbar_hover: hsla(get(&["toolbar.hoverBackground"]).unwrap_or(tint(0x10))),
        input: hsla(get(&["input.background"]).unwrap_or(bg)),
        input_border: hsla(get(&["input.border"]).unwrap_or(border)),
        command_center_border: hsla(get(&["commandCenter.border"]).unwrap_or(border)),
        status_hover: hsla(get(&["statusBarItem.hoverBackground"]).unwrap_or(tint(0x1F))),
        remote: hsla(get(&["statusBarItem.remoteBackground"]).unwrap_or(0x0078D4FF)),
        indent_guide: hsla(get(&["tree.indentGuidesStroke"]).unwrap_or(tint(0x66))),
        inactive_indent_guide: hsla(
            get(&["tree.inactiveIndentGuidesStroke"]).unwrap_or(tint(0x33)),
        ),
        terminal_fg: hsla(get(&["terminal.foreground"]).unwrap_or(editor_fg)),
        ..defaults
    };

    // Dark 2026's config is the base: its fonts and radii, and for a dark
    // theme the colors it doesn't set; a light one falls back to
    // gpui-component's light colors instead.
    let base: Value =
        serde_json::from_str(include_str!("../../assets/themes/dark-2026.json")).ok()?;
    let mut config = base["themes"][0].clone();
    config["name"] = theme.label.clone().into();
    config["mode"] = if theme.dark { "dark" } else { "light" }.into();
    if !theme.dark {
        config["colors"] = json!({});
        config["highlight"] = json!({});
    }
    let mut set = |section: &str, key: &str, value: Option<u32>| {
        if let Some(v) = value {
            config[section][key] = css(v).into();
        }
    };
    for (key, value) in [
        ("background", Some(editor_bg)),
        ("foreground", Some(fg)),
        ("border", Some(border)),
        ("window.border", Some(border)),
        ("title_bar.border", Some(border)),
        ("status_bar.border", Some(border)),
        (
            "accent.background",
            get(&["list.hoverBackground"]).or(Some(tint(0x14))),
        ),
        ("accent.foreground", Some(fg)),
        (
            "caret",
            get(&["editorCursor.foreground"]).or(Some(editor_fg)),
        ),
        ("input.border", get(&["input.border"]).or(Some(border))),
        ("link.foreground", get(&["textLink.foreground"])),
        (
            "link.hover.foreground",
            get(&["textLink.activeForeground", "textLink.foreground"]),
        ),
        (
            "link.active.foreground",
            get(&["textLink.activeForeground", "textLink.foreground"]),
        ),
        ("list.background", Some(bg)),
        ("list.even.background", Some(bg)),
        ("list.head.background", Some(bg)),
        (
            "list.active.background",
            get(&["list.activeSelectionBackground"]).or(Some(tint(0x22))),
        ),
        (
            "list.hover.background",
            get(&["list.hoverBackground"]).or(Some(tint(0x14))),
        ),
        ("list.active.border", Some(accent)),
        (
            "muted.foreground",
            get(&["descriptionForeground"]).or(Some(tint(0xB3))),
        ),
        (
            "popover.background",
            get(&[
                "menu.background",
                "editorWidget.background",
                "quickInput.background",
            ])
            .or(Some(bg)),
        ),
        (
            "popover.foreground",
            get(&["menu.foreground", "editorWidget.foreground"]).or(Some(fg)),
        ),
        ("primary.background", get(&["button.background"])),
        ("primary.active.background", get(&["button.background"])),
        (
            "primary.hover.background",
            get(&["button.hoverBackground", "button.background"]),
        ),
        ("primary.foreground", get(&["button.foreground"])),
        ("ring", Some(accent)),
        (
            "scrollbar.thumb.background",
            get(&["scrollbarSlider.background"]),
        ),
        (
            "scrollbar.thumb.hover.background",
            get(&["scrollbarSlider.hoverBackground"]),
        ),
        ("selection.background", get(&["editor.selectionBackground"])),
        (
            "tab.background",
            get(&["tab.inactiveBackground", "editorGroupHeader.tabsBackground"]).or(Some(bg)),
        ),
        (
            "tab.active.background",
            get(&["tab.activeBackground"]).or(Some(editor_bg)),
        ),
        (
            "tab.active.foreground",
            get(&["tab.activeForeground"]).or(Some(fg)),
        ),
        (
            "tab.foreground",
            get(&["tab.inactiveForeground"]).or(Some(tint(0xB3))),
        ),
        (
            "tab_bar.background",
            get(&["editorGroupHeader.tabsBackground"]).or(Some(bg)),
        ),
        (
            "title_bar.background",
            get(&["titleBar.activeBackground"]).or(Some(bg)),
        ),
        (
            "status_bar.background",
            get(&["statusBar.background"]).or(Some(bg)),
        ),
    ] {
        set("colors", key, value);
    }
    for (key, value) in [
        ("editor.foreground", Some(editor_fg)),
        ("editor.background", Some(editor_bg)),
        (
            "editor.active_line.background",
            get(&["editor.lineHighlightBackground"]),
        ),
        ("editor.line_number", get(&["editorLineNumber.foreground"])),
        (
            "editor.active_line_number",
            get(&["editorLineNumber.activeForeground"]).or(Some(editor_fg)),
        ),
        ("editor.invisible", get(&["editorWhitespace.foreground"])),
    ] {
        set("highlight", key, value);
    }
    let mut syntax = Map::new();
    for &(name, scopes) in SYNTAX {
        let Some(settings) = scopes.iter().find_map(|s| token_style(&rules, s)) else {
            continue;
        };
        let mut style = Map::new();
        if let Some(color) = settings["foreground"].as_str().and_then(hex) {
            style.insert("color".into(), css(color).into());
        }
        let font = settings["fontStyle"].as_str().unwrap_or_default();
        if font.contains("italic") {
            style.insert("font_style".into(), "italic".into());
        }
        if font.contains("bold") {
            style.insert("font_weight".into(), 700.into());
        }
        syntax.insert(name.into(), style.into());
    }
    config["highlight"]["syntax"] = syntax.into();
    Some((serde_json::from_value(config).ok()?, workbench))
}

/// Shows the color theme named `name` (`workbench.colorTheme`), or the
/// built-in one if no extension has it. False if it's gone or won't load.
pub(crate) fn apply_theme(name: &str, cx: &mut App) -> bool {
    let theme = color_themes()
        .into_iter()
        .find(|t| t.id == name || t.label == name);
    let converted = theme.as_ref().and_then(|t| Some((t.dark, convert(t)?)));
    let Some((dark, (config, colors))) = converted else {
        crate::theme::col::set(Colors::default());
        if let Some(builtin) = ThemeRegistry::global(cx)
            .themes()
            .get(DEFAULT_THEME)
            .cloned()
        {
            Theme::global_mut(cx).dark_theme = builtin;
        }
        Theme::change(ThemeMode::Dark, None, cx);
        return name == DEFAULT_THEME;
    };
    crate::theme::col::set(colors);
    let theme = Theme::global_mut(cx);
    let mode = match dark {
        true => {
            theme.dark_theme = Rc::new(config);
            ThemeMode::Dark
        }
        false => {
            theme.light_theme = Rc::new(config);
            ThemeMode::Light
        }
    };
    Theme::change(mode, None, cx);
    true
}

/// The configured color and file icon themes, shown.
pub(crate) fn apply_themes(cx: &mut App) {
    apply_theme(&configured_theme(), cx);
    apply_icon_theme(configured_icon_theme().as_deref());
    // Modern UI: dialogs take cornerRadius-large.
    // ponytail: menus keep theme.radius (4), which inputs share; VS Code's are 8.
    Theme::global_mut(cx).radius_lg = px(match super::modern_ui(&super::terminal::settings()) {
        true => 8.,
        false => 6.,
    });
}

// ---- file icon themes -----------------------------------------------------

/// The built-in file icon theme, which `workbench.iconTheme` defaults to.
const DEFAULT_ICON_THEME: &str = "vs-seti";

/// Every enabled extension's file icon themes: id, label and file.
pub(crate) fn icon_themes() -> Vec<(String, String, PathBuf)> {
    let mut out = Vec::new();
    for ext in installed().iter().filter(|e| e.enabled) {
        for t in ext.contributes("iconThemes") {
            let (Some(id), Some(path)) = (t["id"].as_str(), ext.file(&t["path"])) else {
                continue;
            };
            let label = t["label"].as_str().unwrap_or(id);
            let label = match label.strip_prefix('%').and_then(|l| l.strip_suffix('%')) {
                Some(key) => ext.nls[key].as_str().unwrap_or(label),
                None => label,
            };
            out.push((id.to_string(), label.to_string(), path));
        }
    }
    out
}

/// Shows file icon theme `id` (`workbench.iconTheme`; None for no icons),
/// or Seti if no extension has it or it won't load.
pub(crate) fn apply_icon_theme(id: Option<&str>) {
    let Some(id) = id else {
        return crate::theme::set_file_icons(&Value::Null, None);
    };
    let theme = (icon_themes().into_iter().find(|t| t.0 == id)).and_then(|(_, _, path)| {
        let json = serde_json::from_str(&strip_jsonc(&fs::read_to_string(&path).ok()?)).ok()?;
        Some((json, path))
    });
    match theme {
        Some((json, path)) => crate::theme::set_file_icons(&json, path.parent()),
        None => crate::theme::set_file_icons(&crate::theme::seti_json(), None),
    }
}

/// The configured `workbench.iconTheme`; None when it's null or "" (no
/// icons).
pub(crate) fn configured_icon_theme() -> Option<String> {
    match super::terminal::settings().get("workbench.iconTheme") {
        Some(Value::Null) => None,
        Some(Value::String(id)) if id.is_empty() => None,
        v => Some(
            v.and_then(Value::as_str)
                .unwrap_or(DEFAULT_ICON_THEME)
                .to_string(),
        ),
    }
}

/// The configured `workbench.colorTheme`.
pub(crate) fn configured_theme() -> String {
    (super::terminal::settings()["workbench.colorTheme"].as_str())
        .unwrap_or(DEFAULT_THEME)
        .to_string()
}

/// The Extensions view's "Views and More Actions..." menu.
/// An Extensions view row: the icon, then the name and version, the
/// description, and the publisher with `action` on its right.
/// An extension's details editor: which one, the tab showing, and its
/// README and CHANGELOG as they were when it opened.
pub(crate) struct Page {
    id: String,
    tab: usize,
    readme: SharedString,
    changelog: SharedString,
}

/// `dir`'s `NAME.md`, any case, or "".
fn markdown_file(dir: &Path, name: &str) -> SharedString {
    let file = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .find(|f| f.file_name().to_string_lossy().to_lowercase() == format!("{name}.md"));
    (file.and_then(|f| fs::read_to_string(f.path()).ok()))
        .unwrap_or_default()
        .into()
}

/// A contribution's row on the Features tab: what it's called, and what
/// it runs or names.
fn feature(e: &Extension, entry: &Value) -> (String, String) {
    let first = |keys: &[&str]| {
        (keys.iter())
            .map(|k| e.localize(&entry[*k]))
            .find(|s| !s.is_empty())
            .unwrap_or_default()
    };
    let name = first(&[
        "title",
        "label",
        "displayName",
        "name",
        "id",
        "language",
        "key",
    ]);
    let more = first(&["command", "id", "path"]);
    (
        name.clone(),
        if more == name { String::new() } else { more },
    )
}

fn extension_row(
    id: impl Into<ElementId>,
    icon: AnyElement,
    [name, version, description, publisher]: [String; 4],
    action: AnyElement,
) -> Stateful<Div> {
    // min width 0 lets a long description or publisher shorten instead of widening the view.
    let line = || {
        div()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
    };
    h_flex()
        .id(id)
        .h(px(72.))
        .px(px(16.))
        .gap(px(14.))
        .hover(|d| d.bg(col::hover()))
        .child(icon)
        .child(
            v_flex()
                .flex_1()
                // A truncated bold name still widened the sidebar without the zero width and clip.
                .w(px(0.))
                .min_w_0()
                .overflow_hidden()
                .text_size(px(13.))
                .child(
                    h_flex()
                        .gap(px(6.))
                        .child(line().font_weight(FontWeight::BOLD).child(name))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(12.))
                                .text_color(col::fg_muted())
                                .child(version),
                        ),
                )
                .child(line().child(description))
                .child(
                    h_flex()
                        .justify_between()
                        .child(line().text_color(col::fg_muted()).child(publisher))
                        .child(div().flex_none().child(action)),
                ),
        )
}

pub(super) fn more_button() -> impl IntoElement {
    Button::new("ext-more")
        .ghost()
        .compact()
        .size(px(22.))
        .text_color(col::fg())
        .child(codicon(icon::ELLIPSIS))
        .tooltip("Views and More Actions...")
        .dropdown_menu(|menu, _, _| menu.menu("Install from VSIX...", Box::new(InstallFromVsix)))
}

impl Workbench {
    /// Extensions: Install from VSIX...
    pub(super) fn install_from_vsix(&mut self, cx: &mut Context<Self>) {
        // ponytail: gpui's open dialog has no file type filter; a non-.vsix fails to unpack.
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Install".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            let done = cx
                .background_spawn(async move { install_vsix(&path) })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(name) = &done {
                    let text = format!("Completed installing {name} extension from VSIX.");
                    this.note(text.into(), Severity::Info, cx);
                }
                this.extension_installed(done, cx);
            })
            .ok();
        })
        .detach();
    }

    fn extension_installed(&mut self, done: Result<String, String>, cx: &mut Context<Self>) {
        if let Err(err) = done {
            self.error(format!("Unable to install extension: {err}"), cx);
        }
        // Its theme or languages may be the configured ones.
        apply_themes(cx);
        // A new version replaces the running one.
        self.restart_extension_host(cx);
        self.activate_extensions("*", cx);
        cx.notify();
    }

    /// The Extensions view's search, sent to Open VSX after a pause in typing.
    /// An empty query or `@installed ...` filters the Installed list instead.
    pub(super) fn search_market(&mut self, cx: &mut Context<Self>) {
        let query = self.ext_search.read(cx).value().trim().to_string();
        self.market.generation += 1;
        let generation = self.market.generation;
        if !searches_market(&query) {
            self.market.results = None;
            return cx.notify();
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let current = |this: &Self| this.market.generation == generation;
            if !this.read_with(cx, |this, _| current(this)).unwrap_or(false) {
                return;
            }
            let q = query.clone();
            let found = cx
                .background_spawn(async move { search_open_vsx(&q) })
                .await;
            this.update(cx, |this, cx| {
                if current(this) {
                    this.market.results = Some((query, found));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn install_listing(&mut self, id: String, url: String, cx: &mut Context<Self>) {
        if !self.market.installing.insert(id.clone()) {
            return;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let of = id.clone();
            let done = (cx.background_spawn(async move {
                install_from(&platform_download(&of).unwrap_or(url))
            }))
            .await;
            this.update(cx, |this, cx| {
                this.market.installing.remove(&id);
                this.extension_installed(done, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Open VSX's results for the Extensions view's search, each with Install
    /// or, once installed, its state.
    // ponytail: no icons (they're URLs), ratings or download counts.
    pub(super) fn render_market(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let muted = |text: &str| {
            div()
                .px(px(16.))
                .py(px(8.))
                .text_size(px(13.))
                .text_color(col::fg_muted())
                .child(text.to_string())
        };
        let Some((_, results)) = &self.market.results else {
            return muted("Searching...").into_any_element();
        };
        let listings = match results {
            Err(err) => {
                return muted(&format!("Error while searching for extensions. {err}"))
                    .into_any_element();
            }
            Ok(l) if l.is_empty() => return muted("No extensions found.").into_any_element(),
            Ok(l) => l,
        };
        let wb = cx.weak_entity();
        let rows = listings.iter().enumerate().map(|(ix, l)| {
            let action = if installed().iter().any(|e| e.id == l.id) {
                div()
                    .text_size(px(12.))
                    .text_color(col::fg_muted())
                    .child("Installed")
                    .into_any_element()
            } else if self.market.installing.contains(&l.id) {
                div()
                    .text_size(px(12.))
                    .text_color(col::fg_muted())
                    .child("Installing")
                    .into_any_element()
            } else {
                // button.background and button.hoverBackground
                let blue = ButtonCustomVariant::new(cx)
                    .color(c(0x0078D4))
                    .hover(c(0x026EC1))
                    .active(c(0x026EC1))
                    .foreground(c(0xFFFFFF));
                let (wb, id, url) = (wb.clone(), l.id.clone(), l.download.clone());
                Button::new(("ext-install", ix))
                    .custom(blue)
                    .bg(c(0x0078D4))
                    .xsmall()
                    .label("Install")
                    .on_click(move |_, _, cx| {
                        wb.update(cx, |this, cx| {
                            this.install_listing(id.clone(), url.clone(), cx)
                        })
                        .ok();
                    })
                    .into_any_element()
            };
            let icon = codicon(icon::EXTENSIONS)
                .size(px(42.))
                .text_size(px(32.))
                .justify_center()
                .text_color(col::fg_muted())
                .into_any_element();
            let text = [&l.name, &l.version, &l.description, &l.publisher].map(|s| s.to_string());
            extension_row(("ext-market", ix), icon, text, action)
        });
        v_flex()
            .children(rows.collect::<Vec<_>>())
            .into_any_element()
    }

    pub(super) fn uninstall_extension(&mut self, id: String, cx: &mut Context<Self>) {
        if let Err(err) = uninstall(&id) {
            self.error(format!("Unable to uninstall extension: {err}"), cx);
        }
        self.restart_extension_host(cx);
        apply_themes(cx);
        cx.notify();
    }

    pub(super) fn enable_extension(&mut self, id: String, enabled: bool, cx: &mut Context<Self>) {
        if let Err(err) = set_enabled(&id, enabled) {
            self.error(format!("Unable to change extension: {err}"), cx);
        }
        self.restart_extension_host(cx);
        self.activate_extensions("*", cx);
        apply_themes(cx);
        cx.notify();
    }

    /// Opens extension `id`'s details in an editor, or shows the open one.
    pub(super) fn open_extension(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self
            .docs
            .iter()
            .position(|d| d.page.as_ref().is_some_and(|p| p.id == id));
        if let Some(ix) = open {
            self.activate(ix, window, cx);
            return;
        }
        let Some((name, dir)) = (installed().iter())
            .find(|e| e.id == id)
            .map(|e| (e.name(), e.dir.clone()))
        else {
            return;
        };
        let page = Page {
            id,
            tab: 0,
            readme: markdown_file(&dir, "readme"),
            changelog: markdown_file(&dir, "changelog"),
        };
        // Binary, so saving and the editor's commands leave its empty text alone.
        let path = PathBuf::from(format!("Extension: {}", name.replace(['/', '\\'], " ")));
        self.push_doc(path, String::new(), Encoding::Utf8, true, false, window, cx);
        self.docs.last_mut().unwrap().page = Some(page);
    }

    /// Doc `ix`'s extension details: VS Code's header, then Details
    /// (README), Features (contributions) and Changelog.
    // ponytail: installed extensions only, and the README's relative
    // images don't load; an Open VSX result has no page yet.
    pub(super) fn render_page(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(page) = self.docs[ix].page.as_ref() else {
            return div().into_any_element();
        };
        let list = installed();
        let Some(e) = list.iter().find(|e| e.id == page.id) else {
            return (div().p(px(20.)).text_color(col::fg_muted()))
                .child("This extension is not installed.")
                .into_any_element();
        };
        let (id, enabled) = (e.id.clone(), e.enabled);
        let icon = match e.icon() {
            Some(path) => img(path).size(px(128.)).flex_none().into_any_element(),
            None => codicon(icon::EXTENSIONS)
                .size(px(128.))
                .flex_none()
                .text_size(px(96.))
                .justify_center()
                .text_color(col::fg_muted())
                .into_any_element(),
        };
        let blue = ButtonCustomVariant::new(cx)
            .color(c(0x0078D4))
            .hover(c(0x026EC1))
            .active(c(0x026EC1))
            .foreground(c(0xFFFFFF));
        let button = |key: &'static str, label: &'static str| {
            Button::new(key)
                .custom(blue)
                .bg(c(0x0078D4))
                .xsmall()
                .label(label)
        };
        let id2 = id.clone();
        let header = h_flex().p(px(20.)).gap(px(20.)).child(icon).child(
            v_flex()
                .gap(px(6.))
                .min_w_0()
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(26.))
                                .font_weight(FontWeight::BOLD)
                                .child(e.name()),
                        )
                        .child(
                            div()
                                .px(px(6.))
                                .rounded(px(3.))
                                .bg(col::hover())
                                .text_size(px(12.))
                                .text_color(col::fg_muted())
                                .child(id.clone()),
                        ),
                )
                .child(
                    h_flex()
                        .gap(px(12.))
                        .text_color(col::fg_muted())
                        .child(e.publisher())
                        .child(format!("v{}", e.version())),
                )
                .child(e.description())
                .child(
                    h_flex()
                        .gap(px(8.))
                        .pt(px(4.))
                        .child(
                            button(
                                "ext-page-enable",
                                if enabled { "Disable" } else { "Enable" },
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.enable_extension(id.clone(), !enabled, cx)
                                },
                            )),
                        )
                        .child(
                            button("ext-page-uninstall", "Uninstall").on_click(cx.listener(
                                move |this, _, _, cx| this.uninstall_extension(id2.clone(), cx),
                            )),
                        ),
                ),
        );
        let tabs = ["DETAILS", "FEATURES", "CHANGELOG"]
            .into_iter()
            .enumerate()
            .map(|(t, label)| {
                let active = t == page.tab;
                div()
                    .id(("ext-page-tab", t))
                    .py(px(4.))
                    .cursor_pointer()
                    .text_color(if active { col::fg() } else { col::fg_muted() })
                    .when(active, |d| d.border_b_1().border_color(col::accent()))
                    .hover(|d| d.text_color(col::fg()))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(page) = this.docs.get_mut(ix).and_then(|d| d.page.as_mut()) {
                            page.tab = t;
                            cx.notify();
                        }
                    }))
            });
        let markdown = |text: &SharedString, none: &'static str| match text.is_empty() {
            true => div()
                .text_color(col::fg_muted())
                .child(none)
                .into_any_element(),
            false => TextView::markdown(("ext-page-md", ix), text.clone())
                .selectable(true)
                .into_any_element(),
        };
        let body = match page.tab {
            0 => markdown(&page.readme, "No README available."),
            2 => markdown(&page.changelog, "No changelog available."),
            _ => {
                let points = e.manifest["contributes"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                let sections = points.keys().map(|point| {
                    let rows: Vec<(String, String)> = match point.as_str() {
                        "configuration" => (e.contributes(point))
                            .flat_map(|c| c["properties"].as_object().cloned().unwrap_or_default())
                            .map(|(key, v)| (key, e.localize(&v["description"])))
                            .collect(),
                        "menus" | "viewsContainers" | "views" => (points[point].as_object())
                            .map(|m| m.keys().map(|k| (k.clone(), String::new())).collect())
                            .unwrap_or_default(),
                        _ => e.contributes(point).map(|v| feature(e, v)).collect(),
                    };
                    v_flex()
                        .gap(px(2.))
                        .child(
                            div()
                                .pb(px(4.))
                                .font_weight(FontWeight::BOLD)
                                .child(format!("{point} ({})", rows.len())),
                        )
                        .children(rows.into_iter().map(|(name, more)| {
                            h_flex()
                                .gap(px(12.))
                                .child(div().w(px(260.)).flex_none().overflow_hidden().child(name))
                                .child(div().min_w_0().text_color(col::fg_muted()).child(more))
                        }))
                });
                v_flex()
                    .gap(px(16.))
                    .when(points.is_empty(), |d| {
                        d.child(div().text_color(col::fg_muted()).child("No contributions."))
                    })
                    .children(sections)
                    .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .text_size(px(13.))
            .text_color(col::fg())
            .child(header)
            .child(
                h_flex()
                    .px(px(20.))
                    .gap(px(20.))
                    .text_size(px(11.))
                    .border_b_1()
                    .border_color(col::border())
                    .children(tabs),
            )
            .child(
                div()
                    .id("ext-page-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(20.))
                    .child(body),
            )
            .into_any_element()
    }

    /// The Extensions view's Installed list, filtered by its search box.
    pub(super) fn render_installed(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.ext_search.read(cx).value().to_lowercase();
        let query = query.trim().trim_start_matches("@installed").trim();
        let wb = cx.weak_entity();
        let rows = installed()
            .iter()
            .filter(|e| {
                query.is_empty()
                    || [e.name(), e.id.clone(), e.description()]
                        .iter()
                        .any(|s| s.to_lowercase().contains(query))
            })
            .enumerate()
            .map(|(ix, e)| {
                let (id, enabled) = (e.id.clone(), e.enabled);
                let icon = match e.icon() {
                    Some(path) => img(path).size(px(42.)).into_any_element(),
                    None => codicon(icon::EXTENSIONS)
                        .size(px(42.))
                        .text_size(px(32.))
                        .justify_center()
                        .text_color(col::fg_muted())
                        .into_any_element(),
                };
                let wb = wb.clone();
                let page = id.clone();
                let manage = Button::new(("ext-manage", ix))
                    .ghost()
                    .compact()
                    .size(px(22.))
                    .text_color(col::fg())
                    .child(codicon(icon::GEAR))
                    .tooltip("Manage")
                    .dropdown_menu(move |menu, _, _| {
                        let (wb2, id2) = (wb.clone(), id.clone());
                        let (wb, id) = (wb.clone(), id.clone());
                        menu.item(
                            PopupMenuItem::new(if enabled { "Disable" } else { "Enable" })
                                .on_click(move |_, _, cx| {
                                    wb.update(cx, |this, cx| {
                                        this.enable_extension(id.clone(), !enabled, cx)
                                    })
                                    .ok();
                                }),
                        )
                        .separator()
                        .item(PopupMenuItem::new("Uninstall").on_click(move |_, _, cx| {
                            wb2.update(cx, |this, cx| this.uninstall_extension(id2.clone(), cx))
                                .ok();
                        }))
                    });
                let version = match enabled {
                    true => e.version(),
                    false => format!("{} (Disabled)", e.version()),
                };
                let text = [e.name(), version, e.description(), e.publisher()];
                extension_row(("ext", ix), icon, text, manage.into_any_element())
                    .when(!enabled, |d| d.opacity(0.6))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_extension(page.clone(), window, cx)
                    }))
            })
            .collect::<Vec<_>>();
        v_flex().children(rows).into_any_element()
    }

    /// Preferences: Color Theme: light themes, then dark ones, as VS Code
    /// groups them.
    // ponytail: no live preview while moving through the list.
    pub(super) fn select_color_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut themes = color_themes();
        themes.sort_by_key(|t| (t.dark, t.label.to_lowercase()));
        let lights = themes.iter().filter(|t| !t.dark).count();
        let mut names: Vec<String> = themes.iter().map(|t| t.id.clone()).collect();
        let mut rows: Vec<(String, String)> = themes
            .iter()
            .map(|t| (t.label.clone(), String::new()))
            .collect();
        names.insert(lights, DEFAULT_THEME.into());
        rows.insert(lights, (DEFAULT_THEME.into(), String::new()));
        let mut groups = vec![(lights, "dark themes".into())];
        if lights > 0 {
            groups.insert(0, (0, "light themes".into()));
        }
        let current = configured_theme();
        let current = (names.iter().zip(&rows))
            .position(|(n, r)| *n == current || r.0 == current)
            .unwrap_or(lights);
        let items = super::palette::Items {
            picker: super::palette::Picker::Theme(names),
            rows,
            groups,
            placeholder: "Select Color Theme".into(),
            current,
        };
        self.open_items(items, window, cx);
    }

    /// Preferences: File Icon Theme: none, Seti, then extensions' themes.
    pub(super) fn select_icon_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut themes = icon_themes();
        themes.sort_by_key(|t| t.1.to_lowercase());
        let mut ids = vec![None, Some(DEFAULT_ICON_THEME.to_string())];
        let mut rows = vec![
            ("None".to_string(), "Disable File Icons".to_string()),
            ("Seti (Visual Studio Code)".to_string(), String::new()),
        ];
        for (id, label, _) in themes {
            ids.push(Some(id));
            rows.push((label, String::new()));
        }
        let current = configured_icon_theme();
        let current = ids.iter().position(|id| *id == current).unwrap_or(1);
        let items = super::palette::Items {
            picker: super::palette::Picker::IconTheme(ids),
            rows,
            groups: Vec::new(),
            placeholder: "Select File Icon Theme".into(),
            current,
        };
        self.open_items(items, window, cx);
    }

    /// Snippets: Insert Snippet, for the active editor's language.
    pub(super) fn insert_snippet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(self.active).filter(|d| !d.binary) else {
            return;
        };
        let snippets = snippets(doc.lang);
        if snippets.is_empty() {
            return self.note(
                format!(
                    "No snippets for {}.",
                    crate::theme::language_label(doc.lang)
                )
                .into(),
                Severity::Info,
                cx,
            );
        }
        let rows = (snippets.iter())
            .map(|s| (s.prefix.clone(), s.name.clone()))
            .collect();
        let items = super::palette::Items {
            picker: super::palette::Picker::Snippet(snippets),
            rows,
            groups: Vec::new(),
            placeholder: "Select a snippet".into(),
            current: 0,
        };
        self.open_items(items, window, cx);
    }

    /// Does what row `ix` of a plain pick list stands for.
    pub(super) fn pick_item(
        &mut self,
        picker: super::palette::Picker,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match picker {
            super::palette::Picker::Theme(names) => {
                let name = &names[ix];
                apply_theme(name, cx);
                if let Err(err) = super::terminal::set_setting("workbench.colorTheme", json!(name))
                {
                    self.settings_error(err, cx);
                }
            }
            super::palette::Picker::IconTheme(ids) => {
                // "" for none, as null would remove the setting.
                apply_icon_theme(ids[ix].as_deref());
                if let Err(err) = super::terminal::set_setting(
                    "workbench.iconTheme",
                    json!(ids[ix].as_deref().unwrap_or("")),
                ) {
                    self.settings_error(err, cx);
                }
                cx.notify();
            }
            super::palette::Picker::Snippet(snippets) => {
                self.expand_snippet(&snippets[ix], window, cx)
            }
            super::palette::Picker::Location(mut places) => {
                let (path, at) = places.swap_remove(ix);
                self.go_to(path, at, window, cx);
            }
            super::palette::Picker::Host(id) => self.ext.answer(id, json!(ix)),
            super::palette::Picker::OpenWith(path, mut views) => {
                self.open_with(path, views.swap_remove(ix), window, cx)
            }
            super::palette::Picker::Recent(mut paths) => match paths.swap_remove(ix) {
                (path, true) => self.set_root(path, cx),
                (path, false) => self.open_recent(path, window, cx),
            },
        }
    }

    fn expand_snippet(&mut self, snippet: &Snippet, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let path = doc.path.clone();
        let indent_unit = match doc.indent.hard_tabs {
            true => "\t".to_string(),
            false => " ".repeat(doc.indent.tab_size),
        };
        let lang = doc.lang;
        let body = snippet.body.clone();
        let mut placed = Vec::new();
        let out = &mut placed;
        self.edit_lines(
            move |text, sel| {
                let start = text[..sel.start].rfind('\n').map_or(0, |i| i + 1);
                let indent: String = text[start..]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                let at = Place {
                    path: &path,
                    lang,
                    indent: &indent,
                    unit: &indent_unit,
                };
                let (new, mut stops) = fill(&body, &at, &text[sel.clone()]);
                for stop in &mut stops {
                    *stop = sel.start + stop.start..sel.start + stop.end;
                }
                let first = stops[0].clone();
                *out = stops;
                Some((sel, new, first))
            },
            window,
            cx,
        );
        if !placed.is_empty() {
            let editor = self.docs[self.active].editor.clone();
            self.tab_stops(&editor, placed, cx);
        }
    }

    /// Selects a snippet's first tab stop and, when it has more, lets Tab
    /// move between them.
    fn tab_stops(
        &mut self,
        editor: &Entity<EditorState>,
        stops: Vec<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        let len = editor.update(cx, |e, cx| {
            e.set_selected_range(stops[0].clone(), cx);
            e.value().len()
        });
        self.snippet = (stops.len() > 1).then(|| Stops {
            editor: editor.entity_id(),
            stops,
            at: 0,
            len,
        });
    }

    /// After doc `ix` changed: when the change was a snippet the completion
    /// menu offered, its first tab stop is selected and Tab moves on.
    // ponytail: told apart by its text ending at the cursor; typing a
    // snippet's whole text out by hand while it's offered counts too.
    pub(crate) fn snippet_completed(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(ix) else {
            return;
        };
        let editor = doc.editor.clone();
        // The rope, not a copy of its text: that stalled big files per keystroke.
        let (text, cursor) =
            editor.read_with(cx, |e, _| (e.text().clone(), e.selected_range().end));
        // An auto-import inserted above along with it moves its start down.
        let found = OFFERED.with_borrow(|offered| {
            let (_, list) = offered.as_ref().filter(|o| o.0 == doc.path)?;
            list.iter().find_map(|(start, new, stops)| {
                let at = cursor.checked_sub(new.len()).filter(|at| at >= start)?;
                (cursor <= text.len() && text.slice(at..cursor) == new.as_str())
                    .then(|| stops.iter().map(|r| at + r.start..at + r.end).collect())
            })
        });
        if let Some(stops) = found {
            OFFERED.set(None);
            self.tab_stops(&editor, stops, cx);
        }
    }

    /// Tab (or Shift+Tab, `back`) in a snippet just inserted: selects its
    /// next (or previous) tab stop. False when there's none to go to, or the
    /// cursor has left the current one, so Tab indents as usual.
    // ponytail: edits are assumed to be at the current stop; one made
    // elsewhere while it lasts puts the later stops off by its length.
    pub(crate) fn snippet_tab(&mut self, back: bool, cx: &mut Context<Self>) -> bool {
        let Some(s) = self.snippet.as_mut() else {
            return false;
        };
        let Some(doc) = (self.docs.get(self.active)).filter(|d| d.editor.entity_id() == s.editor)
        else {
            self.snippet = None;
            return false;
        };
        let (len, sel) = doc
            .editor
            .read_with(cx, |e, _| (e.value().len(), e.selected_range()));
        let delta = len as isize - s.len as isize;
        let cur = s.stops[s.at].clone();
        for (i, r) in s.stops.iter_mut().enumerate() {
            if i != s.at && r.start >= cur.end {
                r.start = r.start.saturating_add_signed(delta);
            }
            if i == s.at || r.end >= cur.end {
                r.end = r.end.saturating_add_signed(delta);
            }
        }
        if sel.start < cur.start || sel.end > s.stops[s.at].end {
            self.snippet = None;
            return false;
        }
        s.len = len;
        s.at = match back {
            true => s.at.saturating_sub(1),
            false => s.at + 1,
        };
        let next = s.stops[s.at].clone();
        if s.at + 1 == s.stops.len() {
            self.snippet = None;
        }
        doc.editor
            .update(cx, |e, cx| e.set_selected_range(next, cx));
        true
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{IndentAction, editing_of, expand, hex, token_style};

    #[test]
    fn language_configuration() {
        let config = serde_json::json!({
            "brackets": [["<", ">"]],
            "autoClosingPairs": [
                ["<", ">"],
                { "open": "`", "close": "`", "notIn": ["string", "comment"] },
            ],
            "autoCloseBefore": ";",
            "indentationRules": {
                "increaseIndentPattern": r"<\w+>$",
                "decreaseIndentPattern": { "pattern": r"^\s*</" },
            },
        });
        let c = editing_of(&config);
        assert_eq!((c.brackets.len(), &*c.brackets[0].open), (1, "<"));
        let pairs = c.auto_closing_pairs.unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!((&*pairs[1].close, pairs[1].not_in.len()), ("`", 2));
        assert_eq!(&*c.auto_close_before, ";");
        let rules = c.indentation_rules.unwrap();
        assert!(rules.increase_indent_pattern.unwrap().is_match("<div>"));
        assert!(rules.decrease_indent_pattern.unwrap().is_match("  </div>"));
        // Lookaround isn't supported: the pattern is skipped.
        let config =
            serde_json::json!({ "indentationRules": { "increaseIndentPattern": "(?!x)" } });
        let c = editing_of(&config);
        assert!(
            c.indentation_rules
                .unwrap()
                .increase_indent_pattern
                .is_none()
        );
        assert_eq!(c.brackets.len(), 3);
    }

    #[test]
    fn on_enter_rules() {
        // As rust-analyzer's setLanguageConfiguration sends them.
        let config = serde_json::json!({ "onEnterRules": [
            { "beforeText": { "pattern": r"^\s*\/{3}.*$" }, "action": { "indentAction": 0, "appendText": "/// " } },
            { "beforeText": { "pattern": r"^(?!\s*\/\/).*$" }, "action": { "indentAction": 1 } },
            { "beforeText": r"^\s*/\*\*", "afterText": r"^\s*\*/$", "action": { "indent": "indentOutdent", "appendText": " * " } },
        ]});
        let c = editing_of(&config);
        // The lookaround rule doesn't compile and is skipped.
        assert_eq!(c.on_enter_rules.len(), 2);
        let doc = &c.on_enter_rules[0];
        assert!(doc.before.is_match("    /// docs"));
        assert_eq!((doc.action, &*doc.append_text), (IndentAction::None, "/// "));
        let block = &c.on_enter_rules[1];
        assert_eq!(block.action, IndentAction::IndentOutdent);
        assert!(block.after.as_ref().unwrap().is_match(" */"));
    }

    #[test]
    fn expands_snippets() {
        let var = |name: &str| (name == "TM_FILENAME").then(|| "a.rs".to_string());
        assert_eq!(
            expand("fn ${1:name}() {\n\t$0\n}", &var),
            ("fn name() {\n\t\n}".into(), vec![3..7, 13..13])
        );
        assert_eq!(
            expand("$TM_FILENAME ${2:b} $1 ${2:c}", &var),
            ("a.rs b  c".into(), vec![7..7, 5..6, 9..9])
        );
        assert_eq!(expand("plain", &var).1, vec![5..5]);
        assert_eq!(
            expand("${1|one,two|} \\$x ${UNKNOWN} ${TM_FILENAME:z}", &var).0,
            "one $x UNKNOWN a.rs"
        );
        assert_eq!(
            expand("${1:outer ${2:inner}}", &var),
            ("outer inner".into(), vec![0..11, 6..11, 11..11])
        );
        assert_eq!(
            expand("${TM_FILENAME/(.*)/${1:/upcase}/}!", &var).0,
            "a.rs!"
        );
    }

    #[test]
    fn ranks_token_rules() {
        let rules: Vec<Value> = serde_json::from_str(
            r##"[{"scope": "keyword", "settings": {"foreground": "#111"}},
                 {"scope": ["keyword.control", "x"], "settings": {"foreground": "#222"}},
                 {"scope": "keyword.control.flow source", "settings": {"foreground": "#333"}}]"##,
        )
        .unwrap();
        let fg = |scope| token_style(&rules, scope).map(|s| s["foreground"].as_str().unwrap());
        assert_eq!(fg("keyword.operator"), Some("#111"));
        assert_eq!(fg("keyword.control.flow"), Some("#222"));
        assert_eq!(fg("keywords"), None);
        assert_eq!(hex("#abc"), Some(0xAABBCCFF));
        assert_eq!(hex("#11223344"), Some(0x11223344));
    }
}
