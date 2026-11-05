//! Language-specific bracket pairing and indentation rules.

use gpui::SharedString;
use regex::Regex;
use std::sync::Arc;

use super::SyntaxContext;

/// A structural pair, used for indentation and splitting Enter between delimiters.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BracketPair {
    pub open: SharedString,
    pub close: SharedString,
}

impl BracketPair {
    pub fn new(open: impl Into<SharedString>, close: impl Into<SharedString>) -> Self {
        Self {
            open: open.into(),
            close: close.into(),
        }
    }
}

/// An automatic closing pair and the syntax contexts in which insertion is disabled.
/// Empty delimiters are ignored. Strings support delimiters such as `/*` and `*/`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AutoClosingPair {
    pub open: SharedString,
    pub close: SharedString,
    pub not_in: Vec<SyntaxContext>,
}

impl AutoClosingPair {
    pub fn new(open: impl Into<SharedString>, close: impl Into<SharedString>) -> Self {
        Self {
            open: open.into(),
            close: close.into(),
            not_in: Vec::new(),
        }
    }

    pub fn not_in(mut self, contexts: impl IntoIterator<Item = SyntaxContext>) -> Self {
        self.not_in = contexts.into_iter().collect();
        self
    }
}

/// Language indentation patterns, using Rust's `regex` syntax.
/// Patterns are compiled by the caller, so invalid expressions are reported at setup.
/// Applied on Enter: increase matches text before the caret; decrease matches text
/// after it. This does not reformat existing lines or indentation on paste.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct IndentationRules {
    pub increase_indent_pattern: Option<Arc<Regex>>,
    pub decrease_indent_pattern: Option<Arc<Regex>>,
}

impl IndentationRules {
    pub fn new(increase: Regex, decrease: Regex) -> Self {
        Self {
            increase_indent_pattern: Some(Arc::new(increase)),
            decrease_indent_pattern: Some(Arc::new(decrease)),
        }
    }
}

/// vsn: what Enter does to the new line's indentation under an [`OnEnterRule`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IndentAction {
    /// The current line's indentation.
    #[default]
    None,
    /// One level more.
    Indent,
    /// One level more, and the text after the caret on a further line at the
    /// current indentation.
    IndentOutdent,
    /// One level less.
    Outdent,
}

/// vsn: VS Code's `onEnterRules`: the first rule whose patterns match the
/// text before the caret (and after it, and the previous line, when given)
/// decides the new line's indentation and adds `append_text` to it.
#[derive(Debug, Clone)]
pub struct OnEnterRule {
    pub before: Arc<Regex>,
    pub after: Option<Arc<Regex>>,
    pub previous_line: Option<Arc<Regex>>,
    pub action: IndentAction,
    pub append_text: SharedString,
    /// Characters removed from the end of the new line's indentation.
    pub remove_text: usize,
}

/// Declarative language editing rules, independent of editor preferences and parsers.
///
/// Mirrors the supported subset of Monaco's language configuration. `None` for
/// `auto_closing_pairs` uses `brackets`; `Some(vec![])` disables all automatic pairs.
/// Use the builders to configure a default value; additional language capabilities
/// can be added without breaking callers. Tree-sitter queries are configured in
/// the parser implementation, not in these rules.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LanguageConfig {
    pub brackets: Vec<BracketPair>,
    pub auto_closing_pairs: Option<Vec<AutoClosingPair>>,
    /// Automatic insertion is allowed before these characters, whitespace, or EOF.
    pub auto_close_before: SharedString,
    pub indentation_rules: Option<IndentationRules>,
    // vsn: VS Code's onEnterRules, tried in order.
    pub on_enter_rules: Vec<OnEnterRule>,
}

impl Default for LanguageConfig {
    fn default() -> Self {
        let brackets = vec![
            BracketPair::new("(", ")"),
            BracketPair::new("[", "]"),
            BracketPair::new("{", "}"),
        ];
        let mut pairs: Vec<_> = brackets
            .iter()
            .map(|p| {
                AutoClosingPair::new(p.open.clone(), p.close.clone())
                    .not_in([SyntaxContext::String, SyntaxContext::Comment])
            })
            .collect();
        pairs.extend(
            [
                AutoClosingPair::new("\"", "\""),
                AutoClosingPair::new("'", "'"),
            ]
            .into_iter()
            .map(|p| p.not_in([SyntaxContext::String, SyntaxContext::Comment])),
        );
        Self {
            brackets,
            auto_closing_pairs: Some(pairs),
            auto_close_before: ";:.,=}])>".into(),
            indentation_rules: None,
            on_enter_rules: Vec::new(),
        }
    }
}

impl LanguageConfig {
    pub fn brackets(mut self, pairs: impl IntoIterator<Item = BracketPair>) -> Self {
        self.brackets = pairs.into_iter().collect();
        self
    }
    pub fn auto_closing_pairs(mut self, pairs: impl IntoIterator<Item = AutoClosingPair>) -> Self {
        self.auto_closing_pairs = Some(pairs.into_iter().collect());
        self
    }
    pub fn auto_close_before(mut self, characters: impl Into<SharedString>) -> Self {
        self.auto_close_before = characters.into();
        self
    }
    pub fn indentation_rules(mut self, rules: IndentationRules) -> Self {
        self.indentation_rules = Some(rules);
        self
    }
    /// vsn: VS Code's onEnterRules.
    pub fn on_enter_rules(mut self, rules: impl IntoIterator<Item = OnEnterRule>) -> Self {
        self.on_enter_rules = rules.into_iter().collect();
        self
    }

    /// vsn: the first on-Enter rule for a caret with `before` and `after` on
    /// its line, below `previous`.
    pub(crate) fn on_enter(&self, before: &str, after: &str, previous: &str) -> Option<&OnEnterRule> {
        self.on_enter_rules.iter().find(|r| {
            r.before.is_match(before)
                && r.after.as_ref().is_none_or(|p| p.is_match(after))
                && r.previous_line.as_ref().is_none_or(|p| p.is_match(previous))
        })
    }

    pub(crate) fn closing_pairs(&self) -> impl Iterator<Item = (&str, &str, &[SyntaxContext])> {
        let configured = self
            .auto_closing_pairs
            .as_ref()
            .into_iter()
            .flatten()
            .map(|p| (p.open.as_ref(), p.close.as_ref(), p.not_in.as_slice()));
        let fallback = self
            .brackets
            .iter()
            .filter(|_| self.auto_closing_pairs.is_none())
            .map(|p| (p.open.as_ref(), p.close.as_ref(), &[][..]));
        configured
            .chain(fallback)
            .filter(|(open, close, _)| !open.is_empty() && !close.is_empty())
    }

    pub(crate) fn opens_indent(&self, text: &str) -> bool {
        self.indentation_rules
            .as_ref()
            .and_then(|r| r.increase_indent_pattern.as_ref())
            .map_or_else(
                || {
                    self.brackets
                        .iter()
                        .any(|p| !p.open.is_empty() && text.trim_end().ends_with(p.open.as_ref()))
                },
                |r| r.is_match(text),
            )
    }

    pub(crate) fn closes_indent(&self, text: &str) -> bool {
        self.indentation_rules
            .as_ref()
            .and_then(|r| r.decrease_indent_pattern.as_ref())
            .is_some_and(|r| r.is_match(text))
    }
}
