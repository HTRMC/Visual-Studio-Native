//! Search view: find in files across the workspace root.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::component::input::{Input, InputEvent, InputState, Position};
use gpui_kit::{
    base::{h_flex, v_flex},
    prelude::FluentBuilder as _,
    *,
};
use ignore::overrides::{Override, OverrideBuilder};
use regex::{NoExpand, Regex, RegexBuilder};

use super::Workbench;
use crate::theme::{LANGUAGES, action_button, c, ca, codicon, col, file_icon, icon, tip};

/// VS Code's `search.maxResults`: collecting stops here.
const MAX_MATCHES: usize = 20_000;
const PREVIEW_BYTES: usize = 250;

pub struct LineHit {
    line: usize,
    /// Column (in chars) of the first match, for jumping to it.
    column: usize,
    preview: String,
    /// Match ranges within `preview` (only the ones that fit in it).
    ranges: Vec<Range<usize>>,
    /// The whole line and every match in it, so replace previews expand `$1` with full context.
    text: String,
    matches: Vec<Range<usize>>,
}

pub struct FileHit {
    path: PathBuf,
    lines: Vec<LineHit>,
}

pub struct Find {
    case: bool,
    word: bool,
    regex: bool,
    replace_open: bool,
    details_open: bool,
    /// Search only in open editors (the book toggle in "files to include").
    open_only: bool,
    /// Honour .gitignore and .ignore files (the toggle in "files to exclude").
    use_ignore: bool,
    replace: Entity<InputState>,
    include: Entity<InputState>,
    exclude: Entity<InputState>,
    /// Regex behind the current results; replace re-runs it on the live file text.
    re: Option<Regex>,
    results: Vec<FileHit>,
    message: Option<String>,
    collapsed: HashSet<PathBuf>,
    task: Option<Task<()>>,
}

impl Find {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut App| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        Self {
            case: false,
            word: false,
            regex: false,
            replace_open: false,
            details_open: true,
            open_only: false,
            use_ignore: true,
            replace: input("Replace", window, cx),
            include: input("e.g. *.ts, src/**/include", window, cx),
            exclude: input("e.g. *.ts, src/**/exclude", window, cx),
            re: None,
            results: Vec::new(),
            message: None,
            collapsed: HashSet::new(),
            task: None,
        }
    }
}

/// VS Code's default `files.exclude`.
const FILES_EXCLUDE: [&str; 5] = [
    "**/.git",
    "**/.svn",
    "**/.hg",
    "**/.DS_Store",
    "**/Thumbs.db",
];
/// VS Code's default `search.exclude`, on top of `files.exclude`.
const SEARCH_EXCLUDE: [&str; 3] = ["**/node_modules", "**/bower_components", "**/*.code-search"];

/// The globs a VS Code glob map setting turns on: `defaults`, overlaid by
/// `settings[key]` for each of `keys`, where `false` turns one off.
// ponytail: a `{"when": ...}` sibling condition counts as off.
pub(super) fn glob_map(
    settings: &serde_json::Value,
    keys: &[&str],
    defaults: &[&str],
) -> Vec<String> {
    let mut on: BTreeMap<String, bool> = defaults.iter().map(|g| (g.to_string(), true)).collect();
    for key in keys {
        for (g, v) in settings[key].as_object().into_iter().flatten() {
            on.insert(g.clone(), v == true);
        }
    }
    on.into_iter().filter(|(_, v)| *v).map(|(g, _)| g).collect()
}

/// The `files.exclude` (and, for `search`, also `search.exclude`) globs in force.
fn exclude_set(settings: &serde_json::Value, search: bool) -> Vec<String> {
    match search {
        false => glob_map(settings, &["files.exclude"], &FILES_EXCLUDE),
        true => glob_map(
            settings,
            &["files.exclude", "search.exclude"],
            &[&FILES_EXCLUDE[..], &SEARCH_EXCLUDE].concat(),
        ),
    }
}

/// The language `files.associations` gives a file: the first glob (in key
/// order) matching its name or, with a `/`, its path. Unknown ids are skipped.
// ponytail: path globs are relative to the workspace root (VS Code matches
// them on the absolute path), and the file's icon still follows its name.
pub(super) fn associated_language(
    settings: &serde_json::Value,
    root: &Path,
    path: &Path,
) -> Option<&'static str> {
    let map = settings["files.associations"].as_object()?;
    map.iter().find_map(|(glob, id)| {
        let id = *LANGUAGES.iter().find(|l| id.as_str() == Some(**l))?;
        let mut b = OverrideBuilder::new(root);
        b.add(glob).ok()?;
        b.build()
            .ok()?
            .matched(path, false)
            .is_whitelist()
            .then_some(id)
    })
}

/// `exclude_set` from the user's settings.json.
pub(super) fn exclude_globs(search: bool) -> Vec<String> {
    exclude_set(&super::terminal::settings(), search)
}

/// VS Code's include/exclude syntax: comma-separated globs, `./x` is relative to the root,
/// anything else matches at any depth, and a folder matches everything inside it.
/// `excluded` are settings globs (see `exclude_globs`), relative to the root.
pub(super) fn overrides(
    root: &Path,
    include: &str,
    exclude: &str,
    excluded: &[String],
) -> Result<Override, ignore::Error> {
    let mut b = OverrideBuilder::new(root);
    // Includes first: in gitignore matching the last glob wins, so excludes beat includes.
    for (list, neg) in [(include, ""), (exclude, "!")] {
        for p in list
            .split(',')
            .map(|p| p.trim().replace('\\', "/"))
            .filter(|p| !p.is_empty())
        {
            let p = p.trim_end_matches('/');
            let p = match p.strip_prefix("./") {
                Some(rel) => format!("/{rel}"),
                None => format!("**/{p}"),
            };
            b.add(&format!("{neg}{p}"))?;
            b.add(&format!("{neg}{p}/**"))?;
        }
    }
    for g in excluded {
        let g = g.trim_end_matches('/');
        // A settings glob without `/` matches at the root only, unlike gitignore's.
        let g = match g.contains('/') {
            true => g.to_string(),
            false => format!("/{g}"),
        };
        // A bad glob in settings.json is skipped rather than failing the search.
        b.add(&format!("!{g}")).ok();
        b.add(&format!("!{g}/**")).ok();
    }
    b.build()
}

/// Replaces matches line by line (same semantics as the search), keeping line endings.
/// `only` limits it to one 0-based line. With `expand`, `$1`/`${name}` refer to capture groups.
fn replace_text(text: &str, re: &Regex, with: &str, expand: bool, only: Option<usize>) -> String {
    text.split_inclusive('\n')
        .enumerate()
        .map(|(i, l)| {
            if only.is_some_and(|n| n != i) {
                return l.to_string();
            }
            let body = l
                .strip_suffix('\n')
                .map_or(l, |b| b.strip_suffix('\r').unwrap_or(b));
            let eol = &l[body.len()..];
            let new = if expand {
                re.replace_all(body, with)
            } else {
                re.replace_all(body, NoExpand(with))
            };
            format!("{new}{eol}")
        })
        .collect()
}

/// What `with` turns the match at `m` (within `line`) into; `$1` expands against the whole line,
/// exactly like `replace_text` does.
fn expand_at(re: &Regex, line: &str, m: &Range<usize>, with: &str) -> String {
    let mut out = String::new();
    if let Some(caps) = re.captures_at(line, m.start) {
        caps.expand(with, &mut out);
    }
    out
}

/// A match case, whole word or regex toggle in a find input.
pub(super) fn find_toggle(id: &'static str, ch: char, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(20.))
        .rounded(px(3.))
        .cursor_pointer()
        .text_color(col::fg())
        .when(on, |d| {
            d.bg(ca(0x3994BC33)).border_1().border_color(col::accent())
        })
        .when(!on, |d| d.hover(|s| s.bg(col::toolbar_hover())))
        .when_some(
            match ch {
                icon::CASE_SENSITIVE => Some("Match Case (Alt+C)"),
                icon::WHOLE_WORD => Some("Match Whole Word (Alt+W)"),
                icon::REGEX => Some("Use Regular Expression (Alt+R)"),
                icon::BOOK => Some("Search only in Open Editors"),
                icon::EXCLUDE => Some("Use Exclude Settings and Ignore Files"),
                _ => None,
            },
            |d, text| d.tooltip(tip(text)),
        )
        .child(codicon(ch))
}

pub(super) fn build_regex(
    text: &str,
    case: bool,
    word: bool,
    regex: bool,
) -> Result<Regex, regex::Error> {
    let pattern = if regex {
        text.to_string()
    } else {
        regex::escape(text)
    };
    let pattern = if word {
        format!(r"\b(?:{pattern})\b")
    } else {
        pattern
    };
    RegexBuilder::new(&pattern).case_insensitive(!case).build()
}

/// Searches every non-ignored text file under `root` (honours .gitignore, skips `.git`).
/// Files in `buffers` (unsaved editors) are searched as edited, not as on disk.
/// With `only`, the walk keeps to those files (the open editors).
fn search(
    root: &Path,
    re: &Regex,
    overrides: Override,
    buffers: &HashMap<PathBuf, String>,
    only: Option<HashSet<PathBuf>>,
    use_ignore: bool,
) -> (Vec<FileHit>, bool) {
    let mut hits = Vec::new();
    let mut total = 0;
    let keep = move |e: &ignore::DirEntry| {
        (only.as_ref()).is_none_or(|o| o.iter().any(|p| p.starts_with(e.path())))
    };
    for entry in walk(root, overrides, use_ignore, keep).flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let lines = match buffers.get(entry.path()) {
            Some(text) => search_text(text, re),
            None => {
                let Ok(bytes) = fs::read(entry.path()) else {
                    continue;
                };
                if bytes[..bytes.len().min(8192)].contains(&0) {
                    continue; // binary
                }
                let Ok(text) = String::from_utf8(bytes) else {
                    continue;
                };
                search_text(&text, re)
            }
        };
        if lines.is_empty() {
            continue;
        }
        total += lines.iter().map(|l| l.matches.len()).sum::<usize>();
        hits.push(FileHit {
            path: entry.into_path(),
            lines,
        });
        if total >= MAX_MATCHES {
            return (hits, true);
        }
    }
    (hits, false)
}

/// The search's walk: honours .gitignore (with `use_ignore`) and the include/exclude
/// `overrides`, skips `.git`, and descends only into entries `filter` keeps.
fn walk(
    root: &Path,
    overrides: Override,
    use_ignore: bool,
    filter: impl Fn(&ignore::DirEntry) -> bool + Send + Sync + 'static,
) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .standard_filters(use_ignore)
        .hidden(false)
        .overrides(overrides)
        .filter_entry(move |e| e.file_name() != ".git" && filter(e))
        .build()
}

/// Whether `walk` reaches `path`, reading only the folders on the way to it.
fn walks_to(root: &Path, overrides: Override, use_ignore: bool, path: &Path) -> bool {
    let target = path.to_path_buf();
    walk(root, overrides, use_ignore, move |e| {
        target.starts_with(e.path())
    })
    .flatten()
    .any(|e| e.path() == path)
}

fn search_text(text: &str, re: &Regex) -> Vec<LineHit> {
    text.lines()
        .enumerate()
        .filter_map(|(line, s)| {
            let matches: Vec<_> = re
                .find_iter(s)
                .map(|m| m.range())
                .filter(|r| !r.is_empty())
                .collect();
            let first = matches.first()?.start;
            // Preview starts at the first non-blank char, or a bit before the match on long lines.
            let indent = s.len() - s.trim_start().len();
            let near = s[..first]
                .char_indices()
                .rev()
                .nth(30)
                .map_or(0, |(i, _)| i);
            let start = indent.min(first).max(near);
            let mut end = (start + PREVIEW_BYTES).min(s.len());
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            let ellipsis = if start > indent { "…" } else { "" };
            let shift = |i: usize| i - start + ellipsis.len();
            let ranges = matches
                .iter()
                .take_while(|r| r.end <= end)
                .map(|r| shift(r.start)..shift(r.end))
                .collect();
            Some(LineHit {
                line,
                column: s[..first].chars().count(),
                preview: format!("{ellipsis}{}", &s[start..end]),
                ranges,
                text: s.to_string(),
                matches,
            })
        })
        .collect()
}

/// The result count, or VS Code's empty-result message for the current scope.
fn summary(results: &[FileHit], include: &str, exclude: &str, open_only: bool) -> String {
    if results.is_empty() {
        let editors = if open_only { " in open editors" } else { "" };
        let (include, exclude) = (include.trim(), exclude.trim());
        return match (include.is_empty(), exclude.is_empty()) {
            (true, true) => format!(
                "No results found{editors}. Review your settings for configured exclusions and check your gitignore files."
            ),
            (false, true) if open_only => {
                format!("No results found in open editors matching '{include}'.")
            }
            (false, true) => format!("No results found in '{include}'."),
            (true, false) => format!("No results found{editors} excluding '{exclude}'."),
            (false, false) if open_only => format!(
                "No results found in open editors matching '{include}' excluding '{exclude}'."
            ),
            (false, false) => format!("No results found in '{include}' excluding '{exclude}'."),
        };
    }
    let matches: usize = results
        .iter()
        .flat_map(|f| &f.lines)
        .map(|l| l.matches.len())
        .sum();
    let plural = |n: usize, s: &str| format!("{n} {s}{}", if n == 1 { "" } else { "s" });
    format!(
        "{} in {}",
        plural(matches, "result"),
        plural(results.len(), "file")
    )
}

#[derive(Clone, Copy)]
enum Row {
    File(usize),
    Line(usize, usize),
}

impl Workbench {
    fn summary(&self, cx: &App) -> String {
        let f = &self.find;
        let (include, exclude) = (f.include.read(cx).value(), f.exclude.read(cx).value());
        summary(&f.results, &include, &exclude, f.open_only)
    }

    /// Open User Settings (JSON), and the empty result's Open Settings: the user settings.json, created
    /// empty if missing, as VS Code's Open User Settings (JSON) does.
    // ponytail: VS Code opens its Settings UI filtered to ".exclude"; there's
    // no Settings UI here.
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = super::terminal::settings_path() else {
            return;
        };
        if !path.exists() {
            let made = (path.parent().map_or(Ok(()), fs::create_dir_all))
                .and_then(|_| fs::write(&path, "{}\n"));
            if let Err(err) = made {
                return self.error(format!("Unable to create '{}' ({err})", path.display()), cx);
            }
        }
        self.open_path(path, window, cx);
    }

    /// With "Search only in Open Editors" on, opening or closing an editor searches again.
    pub(super) fn open_editors_changed(&mut self, cx: &mut Context<Self>) {
        if self.find.open_only {
            self.run_search(Duration::ZERO, cx);
        }
    }

    pub(super) fn init_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.subscribe_in(
            &self.search,
            window,
            |this, _, ev: &InputEvent, _, cx| match ev {
                InputEvent::Change => this.run_search(Duration::from_millis(250), cx),
                InputEvent::PressEnter { .. } => this.run_search(Duration::ZERO, cx),
                _ => {}
            },
        )
        .detach();
        for input in [self.find.include.clone(), self.find.exclude.clone()] {
            cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| match ev {
                InputEvent::Change => this.run_search(Duration::from_millis(250), cx),
                InputEvent::PressEnter { .. } => this.run_search(Duration::ZERO, cx),
                _ => {}
            })
            .detach();
        }
        // Re-render so the result previews follow the replace text.
        cx.subscribe_in(
            &self.find.replace.clone(),
            window,
            |_, _, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    cx.notify()
                }
            },
        )
        .detach();
    }

    /// Ctrl+Alt+Enter: Replace All, while the replace input shows.
    pub(super) fn replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.replace_open {
            self.replace(None, None, window, cx);
        }
    }

    pub(super) fn show_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.replace_open = true;
        self.find.replace.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Replaces in every result file, one file (`fi`), or one line of it (`fi`, `li`), then searches again.
    /// Open editors get an undoable edit (and turn dirty); other files are rewritten on a background thread.
    fn replace(
        &mut self,
        fi: Option<usize>,
        li: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(re) = self.find.re.clone() else {
            return;
        };
        let with = self.find.replace.read(cx).value().to_string();
        let expand = self.find.regex;
        // Results track open editors live (see `refresh_search_file`), so this line number is current.
        let only = fi
            .zip(li)
            .map(|(fi, li)| self.find.results[fi].lines[li].line);
        // Drop the rows being replaced right away, so a second click can't replace them twice
        // while the disk writes are still running.
        let paths: Vec<PathBuf> = match (fi, li) {
            (None, _) => self.find.results.drain(..).map(|f| f.path).collect(),
            (Some(fi), None) => vec![self.find.results.remove(fi).path],
            (Some(fi), Some(li)) => {
                let path = self.find.results[fi].path.clone();
                self.dismiss(fi, Some(li), cx);
                vec![path]
            }
        };
        let mut on_disk = Vec::new();
        for path in paths {
            match self.docs.iter().find(|d| d.path == path) {
                Some(doc) => {
                    let text = doc.editor.read(cx).value().to_string();
                    let new = replace_text(&text, &re, &with, expand, only);
                    if new != text {
                        doc.editor
                            .update(cx, |e, cx| e.replace_all(new, window, cx));
                    }
                }
                None => on_disk.push(path),
            }
        }
        cx.notify();
        // Detached rather than stored: cancelling halfway would leave the replace half applied.
        cx.spawn(async move |this, cx| {
            cx.background_spawn(async move {
                for path in on_disk {
                    let Ok(text) = fs::read_to_string(&path) else {
                        continue;
                    };
                    let new = replace_text(&text, &re, &with, expand, only);
                    if new != text {
                        if let Err(err) = fs::write(&path, new) {
                            eprintln!("replace {}: {err}", path.display());
                        }
                    }
                }
            })
            .await;
            this.update(cx, |this, cx| this.run_search(Duration::ZERO, cx))
                .ok();
        })
        .detach();
    }

    /// Re-searches one open editor after an edit, keeping its results (and line numbers)
    /// current; a file that starts matching joins them, like in VS Code.
    pub(super) fn refresh_search_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(re) = self.find.re.clone() else {
            return;
        };
        let Some(doc) = self.docs.iter().find(|d| d.path == path) else {
            return;
        };
        let lines = search_text(&doc.editor.read(cx).value(), &re);
        match self.find.results.iter().position(|f| f.path == path) {
            Some(fi) if lines.is_empty() => {
                self.find.results.remove(fi);
            }
            Some(fi) => self.find.results[fi].lines = lines,
            None if lines.is_empty() || !self.in_search_scope(path, cx) => return,
            None => self.find.results.push(FileHit {
                path: path.to_path_buf(),
                lines,
            }),
        }
        self.find.message = Some(self.summary(cx));
    }

    /// Whether the search walk would visit `path`: the same walk, kept to `path`'s ancestors.
    fn in_search_scope(&self, path: &Path, cx: &App) -> bool {
        let Some(root) = self.root.as_deref().filter(|r| path.starts_with(r)) else {
            return false;
        };
        if self.find.open_only && !self.docs.iter().any(|d| d.path == path) {
            return false;
        }
        let (include, exclude) = (
            self.find.include.read(cx).value(),
            self.find.exclude.read(cx).value(),
        );
        let use_ignore = self.find.use_ignore;
        let excluded = if use_ignore {
            exclude_globs(true)
        } else {
            vec![]
        };
        overrides(root, &include, &exclude, &excluded)
            .is_ok_and(|o| walks_to(root, o, use_ignore, path))
    }

    /// The explorer's Find in Folder: Search, limited to files under `dir`.
    pub(super) fn find_in_folder(
        &mut self,
        dir: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rel = (self.root.as_deref()).and_then(|root| dir.strip_prefix(root).ok());
        let include = match rel.map(|r| r.to_string_lossy().replace('\\', "/")) {
            Some(r) if !r.is_empty() => format!("./{r}"),
            _ => String::new(),
        };
        (self.find.include).update(cx, |s, cx| s.set_value(include, window, cx));
        self.find.details_open = true;
        self.sidebar = true;
        self.view = super::View::Search;
        self.focus_search(window, cx);
        self.run_search(Duration::ZERO, cx);
        cx.notify();
    }

    pub(super) fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    fn run_search(&mut self, debounce: Duration, cx: &mut Context<Self>) {
        let text = self.search.read(cx).value().to_string();
        let f = &mut self.find;
        f.task = None; // drops (cancels) the previous search
        f.collapsed.clear();
        if text.is_empty() {
            f.results.clear();
            f.message = None;
            cx.notify();
            return;
        }
        let Some(root) = self.root.clone() else {
            f.message = Some("You have not opened or specified a folder.".into());
            cx.notify();
            return;
        };
        let (include, exclude) = (f.include.read(cx).value(), f.exclude.read(cx).value());
        let excluded = if f.use_ignore {
            exclude_globs(true)
        } else {
            vec![]
        };
        let overrides = match overrides(&root, &include, &exclude, &excluded) {
            Ok(o) => o,
            Err(err) => {
                f.results.clear();
                f.message = Some(format!("Invalid glob: {err}"));
                cx.notify();
                return;
            }
        };
        let re = match build_regex(&text, f.case, f.word, f.regex) {
            Ok(re) => re,
            Err(_) => {
                f.results.clear();
                f.message = Some("Invalid regular expression.".into());
                cx.notify();
                return;
            }
        };
        let buffers: HashMap<PathBuf, String> = self
            .docs
            .iter()
            .filter(|d| d.dirty)
            .map(|d| (d.path.clone(), d.editor.read(cx).value().to_string()))
            .collect();
        let only = (self.find.open_only).then(|| {
            self.docs
                .iter()
                .map(|d| d.path.clone())
                .collect::<HashSet<_>>()
        });
        let f = &mut self.find;
        let use_ignore = f.use_ignore;
        f.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(debounce).await;
            let re2 = re.clone();
            let (results, capped) = cx
                .background_spawn(async move { search(&root, &re2, overrides, &buffers, only, use_ignore) })
                .await;
            this.update(cx, |this, cx| {
                this.find.re = Some(re);
                this.find.results = results;
                let msg = this.summary(cx);
                this.find.message = Some(if capped && !this.find.results.is_empty() {
                    format!(
                        "{msg} - The result set only contains a subset of all matches. Be more specific in your search to narrow down the results."
                    )
                } else {
                    msg
                });
                cx.notify();
            })
            .ok();
        }));
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (fi, file) in self.find.results.iter().enumerate() {
            rows.push(Row::File(fi));
            if !self.find.collapsed.contains(&file.path) {
                rows.extend((0..file.lines.len()).map(|li| Row::Line(fi, li)));
            }
        }
        rows
    }

    /// Removes a file (`li` = None) or one line from the results, like VS Code's dismiss button.
    fn dismiss(&mut self, fi: usize, li: Option<usize>, cx: &mut Context<Self>) {
        let results = &mut self.find.results;
        match li {
            Some(li) => {
                results[fi].lines.remove(li);
                if results[fi].lines.is_empty() {
                    results.remove(fi);
                }
            }
            None => _ = results.remove(fi),
        }
        self.find.message = Some(self.summary(cx));
        cx.notify();
    }

    fn render_row(&self, row: Row, cx: &mut Context<Self>) -> AnyElement {
        let base = |id: ElementId| {
            h_flex()
                .id(id)
                .group("search-row")
                .h(px(22.))
                .pr(px(12.))
                .whitespace_nowrap()
                .overflow_hidden()
                .cursor_pointer()
                .hover(|s| s.bg(col::hover()))
        };
        // Row actions only show while the row is hovered; clicks on them don't reach the row.
        let actions = |replace: Option<(char, Option<usize>)>,
                       fi: usize,
                       li: Option<usize>,
                       cx: &mut Context<Self>| {
            h_flex()
                .flex_none()
                .ml(px(4.))
                .hidden()
                .group_hover("search-row", |s| s.flex())
                .when_some(replace, |d, (ch, li)| {
                    d.child(action_button("replace", ch).on_click(cx.listener(
                        move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.replace(Some(fi), li, window, cx);
                        },
                    )))
                })
                .child(action_button("dismiss", icon::CLOSE).on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.dismiss(fi, li, cx);
                    },
                )))
        };
        let replacing = self.find.replace_open;
        match row {
            Row::File(fi) => {
                let file = &self.find.results[fi];
                let open = !self.find.collapsed.contains(&file.path);
                let name = file
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let dir = self
                    .root
                    .as_ref()
                    .and_then(|r| file.path.parent()?.strip_prefix(r).ok())
                    .map(|d| d.display().to_string())
                    .unwrap_or_default();
                let count: usize = file.lines.iter().map(|l| l.matches.len()).sum();
                let path = file.path.clone();
                base(("search-file", fi).into())
                    .pl(px(8.))
                    .child(
                        codicon(if open {
                            icon::CHEVRON_DOWN
                        } else {
                            icon::CHEVRON_RIGHT
                        })
                        .w(px(16.)),
                    )
                    .child(div().mr(px(6.)).child(file_icon(&file.path)))
                    .child(div().flex_none().child(name))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .ml(px(6.))
                            .text_size(px(12.))
                            .text_color(col::fg_muted())
                            .text_ellipsis()
                            .child(dir),
                    )
                    .child(
                        div()
                            .flex_none()
                            .ml(px(6.))
                            .px(px(5.))
                            .min_w(px(18.))
                            .rounded(px(9.))
                            .bg(c(0x616161))
                            .text_size(px(11.))
                            .line_height(px(18.))
                            .text_center()
                            .text_color(white())
                            .group_hover("search-row", |s| s.hidden())
                            .child(count.to_string()),
                    )
                    .child(actions(
                        replacing.then_some((icon::REPLACE_ALL, None)),
                        fi,
                        None,
                        cx,
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.find.collapsed.remove(&path) {
                            this.find.collapsed.insert(path.clone());
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
            Row::Line(fi, li) => {
                let file = &self.find.results[fi];
                let hit = &file.lines[li];
                let found = HighlightStyle {
                    background_color: Some(ca(0xEA5C0055)),
                    ..Default::default()
                };
                let (preview, highlights) = match (replacing, &self.find.re) {
                    (true, Some(re)) => {
                        // VS Code's replace preview: struck-out match followed by its replacement.
                        let with = self.find.replace.read(cx).value();
                        let removed = HighlightStyle {
                            background_color: Some(ca(0xFF000033)),
                            strikethrough: Some(StrikethroughStyle {
                                thickness: px(1.),
                                color: None,
                            }),
                            ..Default::default()
                        };
                        let inserted = HighlightStyle {
                            background_color: Some(ca(0x9CCC2C33)),
                            ..Default::default()
                        };
                        let (mut out, mut hl, mut last) = (String::new(), Vec::new(), 0);
                        for (r, full) in hit.ranges.iter().zip(&hit.matches) {
                            let m = &hit.preview[r.clone()];
                            out.push_str(&hit.preview[last..r.start]);
                            hl.push((out.len()..out.len() + m.len(), removed));
                            out.push_str(m);
                            let new = if self.find.regex {
                                expand_at(re, &hit.text, full, &with)
                            } else {
                                with.to_string()
                            };
                            hl.push((out.len()..out.len() + new.len(), inserted));
                            out.push_str(&new);
                            last = r.end;
                        }
                        out.push_str(&hit.preview[last..]);
                        hl.retain(|(r, _)| !r.is_empty());
                        (out, hl)
                    }
                    _ => (
                        hit.preview.clone(),
                        hit.ranges.iter().map(|r| (r.clone(), found)).collect(),
                    ),
                };
                let text = StyledText::new(preview).with_highlights(highlights);
                let (path, line, column) = (file.path.clone(), hit.line as u32, hit.column as u32);
                base(("search-line", fi * 1_000_000 + li).into())
                    .pl(px(38.))
                    .text_color(col::fg())
                    .child(div().flex_1().min_w_0().overflow_hidden().child(text))
                    .child(actions(
                        replacing.then_some((icon::REPLACE, Some(li))),
                        fi,
                        Some(li),
                        cx,
                    ))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_path(path.clone(), window, cx);
                        if let Some(doc) = this.docs.iter().find(|d| d.path == path) {
                            doc.editor.update(cx, |e, cx| {
                                e.set_cursor_position(Position::new(line, column), window, cx)
                            });
                        }
                    }))
                    .into_any_element()
            }
        }
    }

    pub(super) fn render_search(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let toggle = |id: &'static str,
                      ch: char,
                      on: bool,
                      set: fn(&mut Find) -> &mut bool,
                      cx: &mut Context<Self>| {
            find_toggle(id, ch, on).on_click(cx.listener(move |this, _, _, cx| {
                let flag = set(&mut this.find);
                *flag = !*flag;
                this.run_search(Duration::ZERO, cx);
            }))
        };
        let suffix = h_flex()
            .gap(px(2.))
            .child(toggle(
                "match-case",
                icon::CASE_SENSITIVE,
                self.find.case,
                |f| &mut f.case,
                cx,
            ))
            .child(toggle(
                "whole-word",
                icon::WHOLE_WORD,
                self.find.word,
                |f| &mut f.word,
                cx,
            ))
            .child(toggle(
                "use-regex",
                icon::REGEX,
                self.find.regex,
                |f| &mut f.regex,
                cx,
            ));
        let boxed = |input: Input| {
            div()
                .flex_1()
                .rounded(px(4.))
                .border_1()
                .border_color(col::input_border())
                .bg(col::input())
                .child(input.appearance(false).text_size(px(13.)))
        };
        let label = |text: &'static str| {
            div()
                .pt(px(4.))
                .pb(px(2.))
                .text_size(px(11.))
                .text_color(col::fg_muted())
                .child(text)
        };
        let f = &self.find;
        let has_results = !f.results.is_empty();

        let rows = self.rows();
        let count = rows.len();

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                h_flex()
                    .items_start()
                    .pl(px(4.))
                    .pr(px(12.))
                    .gap(px(2.))
                    .child(
                        div()
                            .id("toggle-replace")
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(16.))
                            .self_stretch()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(col::toolbar_hover()))
                            .child(codicon(if f.replace_open {
                                icon::CHEVRON_DOWN
                            } else {
                                icon::CHEVRON_RIGHT
                            }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.find.replace_open = !this.find.replace_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.))
                            .child(boxed(Input::new(&self.search).suffix(suffix)))
                            .when(f.replace_open, |d| {
                                d.child(
                                    h_flex()
                                        .gap(px(2.))
                                        .child(boxed(Input::new(&f.replace)))
                                        .child(
                                            action_button("replace-all", icon::REPLACE_ALL)
                                                .when(!has_results, |b| b.opacity(0.4))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.replace(None, None, window, cx)
                                                })),
                                        ),
                                )
                            }),
                    ),
            )
            .child(
                v_flex()
                    .px(px(12.))
                    .pl(px(22.))
                    .child(
                        h_flex().justify_end().child(
                            action_button("toggle-details", icon::ELLIPSIS)
                                .when(f.details_open, |b| b.bg(col::toolbar_hover()))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.find.details_open = !this.find.details_open;
                                    cx.notify();
                                })),
                        ),
                    )
                    .when(f.details_open, |d| {
                        d.child(label("files to include"))
                            .child(boxed(Input::new(&f.include).suffix(toggle(
                                "open-only",
                                icon::BOOK,
                                f.open_only,
                                |f| &mut f.open_only,
                                cx,
                            ))))
                            .child(label("files to exclude"))
                            .child(boxed(Input::new(&f.exclude).suffix(toggle(
                                "use-ignore",
                                icon::EXCLUDE,
                                f.use_ignore,
                                |f| &mut f.use_ignore,
                                cx,
                            ))))
                    }),
            )
            .when_some(f.message.clone(), |d, msg| {
                d.child(
                    div()
                        .px(px(20.))
                        .py(px(6.))
                        .text_size(px(12.))
                        .text_color(col::fg_muted())
                        .child(match msg.strip_suffix('.') {
                            // An empty result links to the settings and the docs, as VS Code's does.
                            Some(m) if m.starts_with("No results found") => {
                                let text = format!("{m} - Open Settings - Learn More");
                                let at = |s: &str| {
                                    let i = text.rfind(s).unwrap_or(0);
                                    i..i + s.len()
                                };
                                let links = vec![at("Open Settings"), at("Learn More")];
                                let link = HighlightStyle {
                                    color: Some(col::accent()),
                                    ..Default::default()
                                };
                                let wb = cx.weak_entity();
                                InteractiveText::new(
                                    "search-message",
                                    StyledText::new(text)
                                        .with_highlights(links.iter().map(|r| (r.clone(), link))),
                                )
                                .on_click(links, move |ix, window, cx| match ix {
                                    0 => {
                                        _ = wb.update(cx, |this, cx| this.open_settings(window, cx))
                                    }
                                    _ => cx
                                        .open_url("https://go.microsoft.com/fwlink/?linkid=853977"),
                                })
                                .into_any_element()
                            }
                            _ => msg.into_any_element(),
                        }),
                )
            })
            .child(
                uniform_list(
                    "search-results",
                    count,
                    cx.processor(move |this, range: Range<usize>, _, cx| {
                        range
                            .map(|ix| this.render_row(rows[ix], cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .text_size(px(13.)),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        associated_language, build_regex, exclude_set, overrides, replace_text, search_text,
        summary, walks_to,
    };

    #[test]
    fn associations_match_name_or_path() {
        let settings = serde_json::json!({"files.associations": {
            "*.txt": "rust", "docs/*.log": "markdown", "*.x": "nope",
        }});
        let root = Path::new("/w");
        let lang = |p: &str| associated_language(&settings, root, Path::new(p));
        assert_eq!(lang("/w/a/b.txt"), Some("rust"));
        assert_eq!(lang("/w/docs/c.log"), Some("markdown"));
        assert_eq!(lang("/w/other/c.log"), None);
        assert_eq!(lang("/w/a.x"), None);
        assert_eq!(
            associated_language(&serde_json::json!({}), root, Path::new("/w/a.txt")),
            None
        );
    }

    #[test]
    fn exclude_settings_overlay_defaults() {
        let settings = serde_json::json!({
            "files.exclude": { "**/.hg": false, "out": true },
            "search.exclude": { "**/node_modules": false },
        });
        let files = exclude_set(&settings, false);
        assert!(files.contains(&"**/.git".into()) && files.contains(&"out".into()));
        assert!(
            !files
                .iter()
                .any(|g| g == "**/.hg" || g.contains("node_modules"))
        );
        let search = exclude_set(&settings, true);
        assert!(search.contains(&"**/bower_components".into()));
        assert!(!search.contains(&"**/node_modules".into()));

        let root = std::path::Path::new("/w");
        let ov = overrides(root, "", "", &files).unwrap();
        let hidden = |p: &str, dir| ov.matched(root.join(p), dir).is_ignore();
        assert!(hidden(".git", true) && hidden("a/.git", true) && hidden("out", true));
        assert!(!hidden(".github", true) && !hidden("a/out", true));
    }

    #[test]
    fn empty_summary_names_scope() {
        assert_eq!(
            summary(&[], "./a", "", true),
            "No results found in open editors matching './a'."
        );
        assert_eq!(
            summary(&[], "", " b ", false),
            "No results found excluding 'b'."
        );
        assert!(summary(&[], "", "", true).starts_with("No results found in open editors. Review"));
    }

    #[test]
    fn scope_follows_gitignore() {
        let root = std::env::temp_dir().join("vsn-scope-test");
        for dir in [".git", "src", "out"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(
            root.join(".gitignore"),
            "out/
",
        )
        .unwrap();
        for file in ["src/a.rs", "out/a.rs", ".git/config"] {
            std::fs::write(root.join(file), "").unwrap();
        }
        let reaches = |rel: &str, exclude: &str| {
            walks_to(
                &root,
                overrides(&root, "", exclude, &[]).unwrap(),
                true,
                &root.join(rel),
            )
        };
        assert!(walks_to(
            &root,
            overrides(&root, "", "", &[]).unwrap(),
            false,
            &root.join("out/a.rs")
        ));
        assert!(reaches("src/a.rs", ""));
        assert!(!reaches("out/a.rs", ""));
        assert!(!reaches("src/a.rs", "src"));
        assert!(!reaches(".git/config", ""));
    }

    fn re_needle() -> regex::Regex {
        build_regex("needle", false, false, false).unwrap()
    }

    #[test]
    fn finds_and_previews() {
        let re = build_regex("foo", false, false, false).unwrap();
        let hits = search_text("a\n    let Foo = foo;\nnone", &re);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].line, hits[0].column), (1, 8));
        assert_eq!(hits[0].preview, "let Foo = foo;");
        assert_eq!(hits[0].ranges, vec![4..7, 10..13]);

        let re = build_regex("foo", true, true, false).unwrap();
        assert_eq!(search_text("foobar foo", &re)[0].ranges, vec![7..10]);
        assert!(build_regex("(", false, false, true).is_err());
        let long = format!("{}needle", "x".repeat(50));
        let hit = &search_text(&long, &re_needle())[0];
        assert!(hit.preview.starts_with('…'));
        assert_eq!(&hit.preview[hit.ranges[0].clone()], "needle");
        assert_eq!(
            build_regex("a.c", false, false, false)
                .unwrap()
                .is_match("abc"),
            false
        );
    }

    #[test]
    fn replaces_and_filters() {
        let re = build_regex(r"(\w+)=(\w+)", true, false, true).unwrap();
        let text = "a=b\r\nx\nc=d";
        assert_eq!(
            replace_text(text, &re, "$2=$1", true, None),
            "b=a\r\nx\nd=c"
        );
        assert_eq!(
            replace_text(text, &re, "$2=$1", false, None),
            "$2=$1\r\nx\n$2=$1"
        );
        assert_eq!(replace_text(text, &re, "", true, Some(2)), "a=b\r\nx\n");

        let root = Path::new("/w");
        let ov = overrides(root, "src, ./docs/*.md", "*.gen.rs", &[]).unwrap();
        let ok = |p: &str| !ov.matched(root.join(p), false).is_ignore();
        assert!(ok("src/main.rs") && ok("a/src/b/c.rs") && ok("docs/x.md"));
        assert!(!ok("src/x.gen.rs") && !ok("tests/t.rs") && !ok("a/docs/x.md"));
        assert!(overrides(root, "a[", "", &[]).is_err());

        // `$1` and `^` see the whole line, not just the cut-down preview.
        let re = build_regex(r"^\s+(\w+)", true, false, true).unwrap();
        let hit = &search_text("    let x", &re)[0];
        assert_eq!(
            super::expand_at(&re, &hit.text, &hit.matches[0], "[$1]"),
            "[let]"
        );
    }
}
