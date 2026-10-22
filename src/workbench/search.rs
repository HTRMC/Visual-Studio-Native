//! Search view: find in files across the workspace root.

use std::{
    collections::{HashMap, HashSet},
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
use crate::theme::{action_button, c, ca, codicon, col, file_icon, icon};

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

/// VS Code's include/exclude syntax: comma-separated globs, `./x` is relative to the root,
/// anything else matches at any depth, and a folder matches everything inside it.
fn overrides(root: &Path, include: &str, exclude: &str) -> Result<Override, ignore::Error> {
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
fn search(
    root: &Path,
    re: &Regex,
    overrides: Override,
    buffers: &HashMap<PathBuf, String>,
) -> (Vec<FileHit>, bool) {
    let mut hits = Vec::new();
    let mut total = 0;
    for entry in walk(root, overrides, |_| true).flatten() {
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

/// The search's walk: honours .gitignore and the include/exclude `overrides`, skips `.git`,
/// and descends only into entries `filter` keeps.
fn walk(
    root: &Path,
    overrides: Override,
    filter: impl Fn(&ignore::DirEntry) -> bool + Send + Sync + 'static,
) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .overrides(overrides)
        .filter_entry(move |e| e.file_name() != ".git" && filter(e))
        .build()
}

/// Whether `walk` reaches `path`, reading only the folders on the way to it.
fn walks_to(root: &Path, overrides: Override, path: &Path) -> bool {
    let target = path.to_path_buf();
    walk(root, overrides, move |e| target.starts_with(e.path()))
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

fn summary(results: &[FileHit]) -> String {
    if results.is_empty() {
        return "No results found. Review your settings for configured exclusions and check your gitignore files.".into();
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
        self.find.message = Some(summary(&self.find.results));
    }

    /// Whether the search walk would visit `path`: the same walk, kept to `path`'s ancestors.
    fn in_search_scope(&self, path: &Path, cx: &App) -> bool {
        let Some(root) = self.root.as_deref().filter(|r| path.starts_with(r)) else {
            return false;
        };
        let (include, exclude) = (
            self.find.include.read(cx).value(),
            self.find.exclude.read(cx).value(),
        );
        overrides(root, &include, &exclude).is_ok_and(|o| walks_to(root, o, path))
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
        let overrides = match overrides(&root, &include, &exclude) {
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
        let f = &mut self.find;
        f.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(debounce).await;
            let re2 = re.clone();
            let (results, capped) = cx
                .background_spawn(async move { search(&root, &re2, overrides, &buffers) })
                .await;
            this.update(cx, |this, cx| {
                this.find.re = Some(re);
                let msg = summary(&results);
                this.find.message = Some(if capped && !results.is_empty() {
                    format!(
                        "{msg} - The result set only contains a subset of all matches. Be more specific in your search to narrow down the results."
                    )
                } else {
                    msg
                });
                this.find.results = results;
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
        self.find.message = Some(summary(results));
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
                            .child(boxed(Input::new(&f.include)))
                            .child(label("files to exclude"))
                            .child(boxed(Input::new(&f.exclude)))
                    }),
            )
            .when_some(f.message.clone(), |d, msg| {
                d.child(
                    div()
                        .px(px(20.))
                        .py(px(6.))
                        .text_size(px(12.))
                        .text_color(col::fg_muted())
                        .child(msg),
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

    use super::{build_regex, overrides, replace_text, search_text, walks_to};

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
                overrides(&root, "", exclude).unwrap(),
                &root.join(rel),
            )
        };
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
        let ov = overrides(root, "src, ./docs/*.md", "*.gen.rs").unwrap();
        let ok = |p: &str| !ov.matched(root.join(p), false).is_ignore();
        assert!(ok("src/main.rs") && ok("a/src/b/c.rs") && ok("docs/x.md"));
        assert!(!ok("src/x.gen.rs") && !ok("tests/t.rs") && !ok("a/docs/x.md"));
        assert!(overrides(root, "a[", "").is_err());

        // `$1` and `^` see the whole line, not just the cut-down preview.
        let re = build_regex(r"^\s+(\w+)", true, false, true).unwrap();
        let hit = &search_text("    let x", &re)[0];
        assert_eq!(
            super::expand_at(&re, &hit.text, &hit.matches[0], "[$1]"),
            "[let]"
        );
    }
}
