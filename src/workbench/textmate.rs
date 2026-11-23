//! Extensions' TextMate grammars (`contributes.grammars`), as VS Code
//! highlights with them: tokenized line by line in vsn itself (irosashi, a
//! Rust port of vscode-textmate on a Rust Oniguruma), for the languages
//! tree-sitter doesn't cover.

use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    ops::Range,
    rc::Rc,
    sync::{
        Arc, LazyLock, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use gpui_kit::base::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter,
    InputHighlighterFactory, Rope, RopeExt as _,
};
use gpui_kit::component::highlighter::LanguageRegistry;
use gpui_kit::{
    Context, FontStyle, FontWeight, HighlightStyle, SharedString, StrikethroughStyle,
    UnderlineStyle, Window, px, rgba,
};
use irosashi::{HighlighterBuilder, ScopeListId, Session, StateStack, Theme, TokenizeOptions};
use serde_json::{Map, Value, json};

use super::debug::strip_jsonc;
use super::extensions::{self, SYNTAX};

/// VS Code's `editor.maxTokenizationLineLength`: longer lines stay plain.
const MAX_LINE: usize = 20_000;

/// Every enabled extension's grammars, and the language each one is for.
struct Grammars {
    highlighter: irosashi::Highlighter,
    /// Language id to grammar (registered by its scope name).
    by_language: HashMap<String, String>,
}

/// None until first asked for, and again after extensions change.
static GRAMMARS: LazyLock<RwLock<Option<Arc<Grammars>>>> = LazyLock::new(Default::default);

/// Drops the grammars, for [`extensions::reload`]: read again when next needed.
pub(crate) fn invalidate() {
    *GRAMMARS.write().unwrap() = None;
}

fn grammars() -> Arc<Grammars> {
    if let Some(g) = GRAMMARS.read().unwrap().as_ref() {
        return g.clone();
    }
    let g = Arc::new(load());
    *GRAMMARS.write().unwrap() = Some(g.clone());
    g
}

fn load() -> Grammars {
    let highlighter = empty();
    let mut by_language = HashMap::new();
    for ext in extensions::installed().iter().filter(|e| e.enabled) {
        for g in ext.contributes("grammars") {
            let (Some(scope), Some(path)) = (g["scopeName"].as_str(), ext.file(&g["path"]))
            else {
                continue;
            };
            // ponytail: plist (.tmLanguage) and YAML grammars are skipped;
            // extensions ship JSON ones almost always.
            let Some(mut grammar) = fs::read_to_string(&path).ok().and_then(|t| {
                serde_json::from_str::<Value>(&strip_jsonc(t.trim_start_matches('\u{feff}'))).ok()
            }) else {
                continue;
            };
            // Injections are declared in the manifest, as VS Code reads them.
            if let Some(to) = g["injectTo"].as_array() {
                grammar["injectTo"] = Value::Array(to.clone());
            }
            if grammar["scopeName"].as_str().is_none() {
                grammar["scopeName"] = scope.into();
            }
            let json = serde_json::to_vec(&grammar).unwrap_or_default();
            if let Err(err) = highlighter.load_language(scope, json) {
                eprintln!("vsn: {}: grammar {scope}: {err}", ext.id);
                continue;
            }
            if let Some(lang) = g["language"].as_str() {
                by_language.insert(lang.to_string(), scope.to_string());
            }
        }
    }
    Grammars {
        highlighter,
        by_language,
    }
}

fn empty() -> irosashi::Highlighter {
    HighlighterBuilder::without_bundled_assets()
        .build()
        .expect("an empty highlighter builds")
}

/// The color theme's token rules, when it has its own (an extension's
/// theme); bumped [`THEME_GENERATION`] on every change.
static THEME: LazyLock<RwLock<Option<Arc<Theme>>>> = LazyLock::new(Default::default);
static THEME_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Colors tokens with `tokenColors` (rules after those of the theme they
/// include), or, with None, through the syntax names every theme sets.
pub(crate) fn set_theme(theme: Option<(&str, bool, &Map<String, Value>, &[Value])>) {
    let theme = theme.and_then(|(name, dark, colors, rules)| {
        let json = json!({
            "name": name,
            "type": if dark { "dark" } else { "light" },
            "colors": colors,
            "tokenColors": rules,
        });
        Theme::parse(&serde_json::to_vec(&json).ok()?).ok().map(Arc::new)
    });
    *THEME.write().unwrap() = theme;
    THEME_GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Picks the TextMate highlighter for the languages that have an extension
/// grammar and no tree-sitter one; the rest fall through to tree-sitter.
pub(crate) fn factory() -> InputHighlighterFactory {
    Rc::new(|language| {
        let tree_sitter = LanguageRegistry::singleton()
            .language(language)
            .is_some_and(|c| c.language.is_some());
        if tree_sitter {
            return None;
        }
        let grammars = grammars();
        let scope = grammars.by_language.get(language)?;
        let session = grammars.highlighter.session(scope).ok()?;
        Some(Box::new(TextMate::new(language, session)) as Box<dyn InputHighlighter>)
    })
}

/// One line's tokens: line-relative byte ranges and their scopes.
#[derive(Default)]
struct Line {
    tokens: Vec<(u32, u32, ScopeListId)>,
    /// The state after the line; None until tokenized.
    end: Option<StateStack>,
}

struct TextMate {
    language: SharedString,
    session: Session,
    lines: Vec<Line>,
    /// Each line's first byte in the document.
    starts: Vec<usize>,
    /// Styles by scope stack, for one theme generation.
    styles: RefCell<(u64, HashMap<ScopeListId, HighlightStyle>)>,
}

impl TextMate {
    fn new(language: &str, session: Session) -> Self {
        Self {
            language: language.to_string().into(),
            session,
            lines: Vec::new(),
            starts: Vec::new(),
            styles: RefCell::new((u64::MAX, HashMap::new())),
        }
    }

    /// Tokenizes from line `first` on, until a line past `changed_to` ends
    /// in the state it ended in before.
    fn tokenize_from(&mut self, text: &Rope, first: usize, changed_to: usize) {
        let mut state = match first {
            0 => self.session.initial_state(),
            n => match self.lines.get(n - 1).and_then(|l| l.end.clone()) {
                Some(s) => s,
                None => return self.tokenize_from(text, 0, changed_to),
            },
        };
        let options = TokenizeOptions {
            max_line_length: Some(MAX_LINE),
        };
        let mut buf = String::new();
        for row in first..self.lines.len() {
            buf.clear();
            buf.extend(text.slice_line(row).chunks());
            let bare = buf.trim_end_matches(['\n', '\r']);
            let result = self.session.tokenize_line(bare, &state, row == 0, options);
            let line = &mut self.lines[row];
            let same = row > changed_to && line.end.as_ref() == Some(&result.state);
            line.tokens = (result.tokens.iter())
                .map(|t| (t.start as u32, t.end as u32, t.scopes))
                .collect();
            line.end = Some(result.state.clone());
            state = result.state;
            if same {
                break;
            }
        }
    }

    fn style(&self, scopes: ScopeListId, resolver: &dyn HighlightStyleResolver) -> HighlightStyle {
        let generation = THEME_GENERATION.load(Ordering::Relaxed);
        let mut cache = self.styles.borrow_mut();
        if cache.0 != generation {
            *cache = (generation, HashMap::new());
        }
        *cache.1.entry(scopes).or_insert_with(|| {
            let names = self.session.scope_names(scopes);
            match THEME.read().unwrap().as_ref() {
                Some(theme) => themed(theme, &names),
                None => syntax_name(&names)
                    .and_then(|n| resolver.style(n))
                    .unwrap_or_default(),
            }
        })
    }
}

/// The style `theme`'s token rules give a token with scopes `names`.
fn themed(theme: &Theme, names: &[&str]) -> HighlightStyle {
    let settings = theme.resolve(names);
    let color = (settings.foreground)
        .and_then(|id| extensions::hex(theme.color(id)))
        .map(|n| rgba(n).into());
    let font = settings.font_style.unwrap_or_default();
    HighlightStyle {
        color,
        font_style: font.is_italic().then_some(FontStyle::Italic),
        font_weight: font.is_bold().then_some(FontWeight::BOLD),
        underline: font.is_underline().then(|| UnderlineStyle {
            thickness: px(1.),
            ..Default::default()
        }),
        strikethrough: font.is_strikethrough().then(|| StrikethroughStyle {
            thickness: px(1.),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The syntax name a token with scopes `names` (outermost first) takes:
/// the innermost scope any name's TextMate scopes match, the longest match
/// winning.
fn syntax_name(names: &[&str]) -> Option<&'static str> {
    names.iter().rev().find_map(|scope| {
        let mut best: Option<(usize, &'static str)> = None;
        for (name, selectors) in SYNTAX {
            for sel in *selectors {
                let hit = *scope == *sel
                    || (scope.starts_with(sel) && scope[sel.len()..].starts_with('.'));
                if hit && best.is_none_or(|(len, _)| sel.len() > len) {
                    best = Some((sel.len(), name));
                }
            }
        }
        best.map(|b| b.1)
    })
}

impl InputHighlighter for TextMate {
    fn language(&self) -> SharedString {
        self.language.clone()
    }

    fn update(
        &mut self,
        edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut Window,
        _cx: &mut Context<EditorState>,
    ) {
        let count = text.lines_len();
        let edit = edit.filter(|_| !self.lines.is_empty());
        match edit {
            None => {
                self.lines = (0..count).map(|_| Line::default()).collect();
                self.starts = (0..count).map(|r| text.line_start_offset(r)).collect();
                self.tokenize_from(text, 0, count);
            }
            Some(edit) => {
                let start = edit.start_position.row.min(self.lines.len() - 1);
                let old_end = edit.old_end_position.row.clamp(start, self.lines.len() - 1);
                let new_end = edit.new_end_position.row.max(start);
                self.lines.splice(
                    start..=old_end,
                    (start..=new_end).map(|_| Line::default()),
                );
                // A count off from the rope's (an edit the rows missed) starts
                // over rather than paint the wrong lines.
                if self.lines.len() != count {
                    return self.update(None, text, _folding, _window, _cx);
                }
                self.starts = (0..count).map(|r| text.line_start_offset(r)).collect();
                self.tokenize_from(text, start, new_end);
            }
        }
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        let mut at = range.start;
        let mut push = |r: Range<usize>, style: HighlightStyle, at: &mut usize| {
            let r = r.start.max(*at)..r.end.min(range.end);
            if r.start >= r.end {
                return;
            }
            if r.start > *at {
                out.push((*at..r.start, HighlightStyle::default()));
            }
            match out.last_mut() {
                Some((last, s)) if last.end == r.start && *s == style => last.end = r.end,
                _ => out.push((r.clone(), style)),
            }
            *at = r.end;
        };
        let first = self.starts.partition_point(|&s| s <= range.start).saturating_sub(1);
        for (row, line) in self.lines.iter().enumerate().skip(first) {
            let Some(&base) = self.starts.get(row) else {
                break;
            };
            if base >= range.end {
                break;
            }
            for &(s, e, scopes) in &line.tokens {
                let r = base + s as usize..base + e as usize;
                if r.end <= range.start {
                    continue;
                }
                if r.start >= range.end {
                    break;
                }
                push(r, self.style(scopes, resolver), &mut at);
            }
        }
        if at < range.end {
            out.push((at..range.end, HighlightStyle::default()));
        }
        out
    }

    /// Indentation ranges, VS Code's folding for a language without a
    /// folding provider: a line folds the lines after it indented deeper.
    fn fold_ranges(&self, text: &Rope) -> Vec<FoldRange> {
        let mut out = Vec::new();
        let mut open: Vec<(usize, usize)> = Vec::new();
        let mut last = 0;
        let mut close = |open: &mut Vec<(usize, usize)>, below: usize, last: usize| {
            while let Some(&(indent, row)) = open.last() {
                if indent < below {
                    break;
                }
                open.pop();
                if last > row {
                    out.push(FoldRange::new(row, last));
                }
            }
        };
        for row in 0..text.lines_len() {
            let mut indent = 0;
            let mut blank = true;
            for c in text.slice_line(row).chars() {
                match c {
                    ' ' => indent += 1,
                    '\t' => indent += 4 - indent % 4,
                    '\n' | '\r' => break,
                    _ => {
                        blank = false;
                        break;
                    }
                }
            }
            if blank {
                continue;
            }
            close(&mut open, indent, last);
            open.push((indent, row));
            last = row;
        }
        close(&mut open, 0, last);
        out.sort_by_key(|f| f.start_line);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_take_the_innermost_longest_syntax_name() {
        assert_eq!(
            syntax_name(&["source.wgsl", "keyword.operator.arithmetic.wgsl"]),
            Some("operator")
        );
        assert_eq!(
            syntax_name(&["source.wgsl", "comment.line.double-slash.wgsl"]),
            Some("comment")
        );
        assert_eq!(
            syntax_name(&["source.wgsl", "entity.name.function.wgsl", "meta.x"]),
            Some("function")
        );
        assert_eq!(syntax_name(&["source.wgsl"]), None);
    }

    #[test]
    fn a_wgsl_grammar_tokenizes_line_by_line() {
        let grammar = br#"{
            "scopeName": "source.t",
            "patterns": [
                {"name": "comment.block.t", "begin": "/\\*", "end": "\\*/"},
                {"name": "keyword.control.t", "match": "\\bfn\\b"}
            ]
        }"#;
        let h = HighlighterBuilder::without_bundled_assets()
            .grammar("source.t", grammar.to_vec())
            .unwrap()
            .build()
            .unwrap();
        let mut tm = TextMate::new("t", h.session("source.t").unwrap());
        let text = Rope::from("fn a /* x\ny */ fn\n");
        tm.lines = (0..text.lines_len()).map(|_| Line::default()).collect();
        tm.starts = (0..text.lines_len()).map(|r| text.line_start_offset(r)).collect();
        tm.tokenize_from(&text, 0, text.lines_len());
        let scopes = |row: usize, at: usize| {
            let t = tm.lines[row].tokens.iter().find(|t| t.0 as usize <= at && at < t.1 as usize);
            tm.session.scope_names(t.unwrap().2).last().unwrap().to_string()
        };
        assert_eq!(scopes(0, 0), "keyword.control.t");
        assert_eq!(scopes(0, 6), "comment.block.t");
        // The comment carries over to the next line.
        assert_eq!(scopes(1, 0), "comment.block.t");
        assert_eq!(scopes(1, 5), "keyword.control.t");
    }
}

