//! EXTENSIONS.md phase 4's native LSP client: one language server per
//! language, started from PATH when a file of that language opens, kept in
//! sync with full-text changes. Its diagnostics show as squiggles, in the
//! Problems panel and in the status bar's counts; hover, completion and Go to
//! Definition reach it through the editor's providers. The JS extension host
//! stands in as one more server, for the languages its extensions provide
//! (vscode-languageclient-based ones run their server from there).

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    hash::{DefaultHasher, Hash as _, Hasher as _},
    io::{BufRead as _, BufReader, Read, Write as _},
    process::{Command, Stdio},
    rc::Rc,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicI64, Ordering},
        mpsc,
    },
};

use futures::channel::oneshot;
use gpui_kit::base::input::{
    CodeLens, Diagnostic, DiagnosticSeverity, FoldRange, InlayHint, InlayLabelPart, RangeDecoration,
    RangeDecorationCollection, RangeDecorationStyle,
};
use input::{CodeActionProvider, CompletionProvider, DefinitionProvider, HoverProvider, Rope};
use lsp_types::{
    CodeAction, CodeActionOrCommand, CompletionContext, CompletionItem, CompletionItemKind,
    CompletionResponse, CompletionTextEdit, DocumentChangeOperation, DocumentChanges,
    GotoDefinitionResponse, Hover, LocationLink, OneOf, TextEdit,
};

use super::palette::Rename;
use serde_json::{Value, json};

use super::*;

/// The server each language id starts, unless an extension provides it.
const SERVERS: &[(&[&str], &[&str])] = &[
    (&["rust"], &["rust-analyzer"]),
    (&["c", "cpp"], &["clangd"]),
    (&["go"], &["gopls"]),
    (&["zig"], &["zls"]),
    (&["python"], &["basedpyright-langserver", "--stdio"]),
    (
        &[
            "typescript",
            "typescriptreact",
            "javascript",
            "javascriptreact",
        ],
        &["typescript-language-server", "--stdio"],
    ),
];

/// The extension hosts' keys among the servers.
pub(super) const HOSTED: &[&str] = &["Extension Host"];
pub(super) const WASM_HOSTED: &[&str] = &["WASM Host"];

/// One diagnostic as a server published it, 0-based.
pub(super) struct Problem {
    line: u32,
    col: u32,
    end: (u32, u32),
    /// LSP's: 1 error, 2 warning, 3 information, 4 hint.
    severity: u64,
    message: String,
    source: String,
    /// As the server sent it, for code actions' context.
    raw: Value,
}

#[derive(Default)]
pub(super) struct Lsp {
    /// By the server's command; None once it failed to start. The only
    /// strong references: dropping one closes the server's stdin.
    servers: HashMap<&'static str, Option<Arc<Server>>>,
    /// Each file's diagnostics, as its server last published them.
    problems: BTreeMap<PathBuf, Vec<Problem>>,
    /// Files whose problems the Problems panel has collapsed.
    collapsed: HashSet<PathBuf>,
    /// How often each server has died on its own.
    crashes: HashMap<&'static str, u32>,
    /// The languages extensions serve, and the host that serves each.
    hosted: HashMap<String, &'static [&'static str]>,
    /// The parameter hints shown above the cursor in this file.
    signature: Option<(PathBuf, lsp_types::SignatureHelp)>,
    /// What clicking an inlay hint or lens does, by the id its editor
    /// reports: a command for its server to run, or a place to go.
    decor: HashMap<usize, (PathBuf, Weak<Server>, Value)>,
    next_decor: usize,
    /// Each file's pending fetch of inlay hints and lenses.
    decorating: HashMap<PathBuf, Task<()>>,
    /// The editor and cursor offset the symbol highlights were last asked
    /// for, the pending ask, and the highlights shown.
    highlight_at: Option<(EntityId, usize)>,
    highlighting: Option<Task<()>>,
    highlights: Option<(Entity<EditorState>, RangeDecorationCollection)>,
}

impl Lsp {
    /// Sends `method` to the server that has `path` open, if one does.
    pub(super) fn request(
        &self,
        path: &Path,
        method: &str,
        params: Value,
    ) -> Option<oneshot::Receiver<Value>> {
        (self.servers.values().flatten())
            .find(|s| s.open.lock().unwrap().contains_key(path))
            .map(|s| s.request(method, params))
    }

    /// Asks every running server for the workspace's symbols matching
    /// `query`; None when no server runs.
    pub(super) fn workspace_symbols(&self, query: &str) -> Option<Vec<oneshot::Receiver<Value>>> {
        let asks: Vec<_> = (self.servers.values().flatten())
            .map(|s| s.request("workspace/symbol", json!({ "query": query })))
            .collect();
        (!asks.is_empty()).then_some(asks)
    }
}

struct Server {
    out: mpsc::Sender<String>,
    next_id: AtomicI64,
    /// Requests awaiting their response, by id.
    pending: Mutex<HashMap<i64, oneshot::Sender<Value>>>,
    /// The open documents' versions and the hash of the text last sent.
    open: Mutex<HashMap<PathBuf, (i64, u64)>>,
    /// What `initialize` answered it can do; null for an extension host.
    caps: Mutex<Value>,
}

impl Server {
    /// Sends `text` as `path`'s content unless the server has it already.
    // ponytail: full sync; incremental changes if big files lag.
    fn sync(&self, path: &Path, lang: &str, text: &str) {
        let mut h = DefaultHasher::new();
        text.hash(&mut h);
        let hash = h.finish();
        let uri = path_uri(path);
        let mut open = self.open.lock().unwrap();
        match open.get_mut(path) {
            Some((_, last)) if *last == hash => {}
            Some((version, last)) => {
                (*version, *last) = (*version + 1, hash);
                notify(
                    &self.out,
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri, "version": *version },
                        "contentChanges": [{ "text": text }],
                    }),
                );
            }
            None => {
                open.insert(path.to_path_buf(), (1, hash));
                notify(
                    &self.out,
                    "textDocument/didOpen",
                    json!({ "textDocument": {
                        "uri": uri, "languageId": lang, "version": 1, "text": text,
                    }}),
                );
            }
        }
    }

    /// Sends a request; the receiver gets its result (null on an error).
    fn request(&self, method: &str, params: Value) -> oneshot::Receiver<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        send(
            &self.out,
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        );
        rx
    }
}

/// LSP's semantic token types, and the theme's name that colors each.
// ponytail: token modifiers (mutable, unsafe, ...) aren't styled.
const SEMANTIC_TYPES: &[(&str, &str)] = &[
    ("namespace", "type"),
    ("type", "type"),
    ("class", "type"),
    ("enum", "type"),
    ("interface", "type"),
    ("struct", "type"),
    ("typeParameter", "type"),
    ("typeAlias", "type"),
    ("union", "type"),
    ("builtinType", "type"),
    ("selfTypeKeyword", "type"),
    ("parameter", "variable"),
    ("variable", "variable"),
    ("static", "variable"),
    ("constParameter", "variable"),
    ("property", "property"),
    ("enumMember", "variant"),
    ("function", "function"),
    ("method", "function"),
    ("macro", "function"),
    ("keyword", "keyword"),
    ("selfKeyword", "variable.special"),
    ("lifetime", "keyword"),
    ("boolean", "boolean"),
    ("comment", "comment"),
    ("string", "string"),
    ("number", "number"),
    ("label", "label"),
    ("attribute", "attribute"),
    ("builtinAttribute", "attribute"),
    ("derive", "attribute"),
    ("escapeSequence", "string.escape"),
    ("formatSpecifier", "string.escape"),
];

/// One editor's hover, completion, definition and code action requests, to
/// its server.
struct Provider {
    server: Weak<Server>,
    path: PathBuf,
    lang: &'static str,
    workbench: WeakEntity<Workbench>,
    snippets: Option<Snippets>,
    /// The editor's indent unit, for snippets' tabs.
    unit: String,
}

/// One editor's snippets as completions: those whose prefix starts with the
/// word before the cursor, replacing it.
// ponytail: read when the file opens, so an extension installed since
// shows its snippets on the next open.
struct Snippets {
    path: PathBuf,
    lang: &'static str,
    unit: String,
    list: Vec<extensions::Snippet>,
}

impl Snippets {
    fn new(doc: &Doc) -> Option<Self> {
        let list = extensions::snippets(doc.lang);
        let unit = match doc.indent.hard_tabs {
            true => "\t".to_string(),
            false => " ".repeat(doc.indent.tab_size),
        };
        (!list.is_empty()).then(|| Snippets {
            path: doc.path.clone(),
            lang: doc.lang,
            unit,
            list,
        })
    }

    fn items(&self, text: &Rope, offset: usize) -> Vec<CompletionItem> {
        let pos = text.offset_to_position(offset);
        let line = text
            .slice(text.line_start_offset(pos.line as usize)..offset)
            .to_string();
        let indent = &line[..line.len() - line.trim_start().len()];
        let word = &line[line
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map_or(0, |i| i + line[i..].chars().next().unwrap().len_utf8())..];
        if word.is_empty() {
            return Vec::new();
        }
        let at = extensions::Place {
            path: &self.path,
            lang: self.lang,
            indent,
            unit: &self.unit,
        };
        let start = offset - word.len();
        let range = lsp_types::Range::new(text.offset_to_position(start), pos);
        let offered: Vec<_> = (self.list.iter())
            .filter(|s| s.prefix.to_lowercase().starts_with(&word.to_lowercase()))
            .map(|s| (s, extensions::fill(&s.body, &at, "")))
            .collect();
        let items = (offered.iter())
            .map(|(s, (new, _))| CompletionItem {
                label: s.prefix.clone(),
                kind: Some(CompletionItemKind::SNIPPET),
                detail: Some(s.name.clone()),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(range, new.clone()))),
                ..Default::default()
            })
            .collect();
        let offered = (offered.into_iter())
            .map(|(_, (new, stops))| (start, new, stops))
            .collect();
        extensions::OFFERED.set(Some((self.path.clone(), offered)));
        items
    }
}

impl CompletionProvider for Snippets {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        Task::ready(Ok(CompletionResponse::Array(self.items(text, offset))))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        let mut chars = new_text.chars();
        matches!((chars.next(), chars.next()), (Some(c), None) if c.is_alphanumeric() || c == '_')
    }
}

impl Provider {
    /// Sends `method` for the position at `offset` in `text`. The text goes
    /// first, as the editor asks before its Change event reaches
    /// [`Workbench::lsp_change`].
    fn ask(&self, method: &str, text: &Rope, offset: usize, cx: &mut App) -> Task<Value> {
        self.ask_with(method, text, offset, json!({}), cx)
    }

    /// [`Self::ask`] with `extra` params beside the position.
    fn ask_with(
        &self,
        method: &str,
        text: &Rope,
        offset: usize,
        extra: Value,
        cx: &mut App,
    ) -> Task<Value> {
        let Some(server) = self.server.upgrade() else {
            return Task::ready(Value::Null);
        };
        server.sync(&self.path, self.lang, &text.to_string());
        let mut params = json!({
            "textDocument": { "uri": path_uri(&self.path) },
            "position": text.offset_to_position(offset),
        });
        if let (Some(params), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
            params.extend(extra.clone());
        }
        let rx = server.request(method, params);
        cx.background_spawn(async move { rx.await.unwrap_or_default() })
    }
}

impl HoverProvider for Provider {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Option<Hover>>> {
        let reply = self.ask("textDocument/hover", text, offset, cx);
        cx.background_spawn(async move { Ok(serde_json::from_value(reply.await)?) })
    }
}

impl CompletionProvider for Provider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let line = text.offset_to_position(offset).line as usize;
        let line = text.slice(text.line_start_offset(line)..offset).to_string();
        // Typed right after one of the server's trigger characters, the ask
        // says so, as VS Code's does; otherwise it's a plain one.
        let context = match line.chars().last().filter(|c| self.triggers().contains(*c)) {
            Some(c) => json!({ "triggerKind": 2, "triggerCharacter": c.to_string() }),
            None => json!({ "triggerKind": 1 }),
        };
        let reply = self.ask_with(
            "textDocument/completion",
            text,
            offset,
            json!({ "context": context }),
            cx,
        );
        extensions::OFFERED.set(None);
        let snippets = (self.snippets.as_ref()).map_or(Vec::new(), |s| s.items(text, offset));
        let indent = line[..line.len() - line.trim_start().len()].to_string();
        let word = line.len() - (line.chars().rev())
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .map(char::len_utf8)
            .sum::<usize>();
        let query = line[word..].to_string();
        let (path, lang, unit) = (self.path.clone(), self.lang, self.unit.clone());
        let text = text.clone();
        // On the main thread: the snippets offered are a thread-local.
        cx.spawn(async move |_| {
            let mut items = match serde_json::from_value(reply.await)? {
                None => vec![],
                Some(CompletionResponse::Array(items)) => items,
                Some(CompletionResponse::List(list)) => list.items,
            };
            let at = extensions::Place {
                path: &path,
                lang,
                indent: &indent,
                unit: &unit,
            };
            let mut offered = Vec::new();
            for item in &mut items {
                if item.insert_text_format != Some(lsp_types::InsertTextFormat::SNIPPET) {
                    continue;
                }
                item.insert_text_format = Some(lsp_types::InsertTextFormat::PLAIN_TEXT);
                let (start, body) = match item.text_edit.as_mut() {
                    Some(CompletionTextEdit::Edit(e)) => (e.range.start, &mut e.new_text),
                    Some(CompletionTextEdit::InsertAndReplace(e)) => {
                        (e.replace.start, &mut e.new_text)
                    }
                    None => (
                        text.offset_to_position(offset),
                        item.insert_text.get_or_insert_with(|| item.label.clone()),
                    ),
                };
                let (new, stops) = extensions::fill(body, &at, "");
                *body = new.clone();
                // Only a stop other than the end needs the cursor moved.
                if stops.iter().any(|r| *r != (new.len()..new.len())) {
                    offered.push((text.position_to_offset(&start), new, stops));
                }
            }
            extensions::offer(&path, offered);
            items.extend(snippets);
            Ok(CompletionResponse::Array(filter_items(items, &query)))
        })
    }

    /// Resolves an accepted item for the import it adds and the command it
    /// runs after, then applies those.
    fn completion_accepted(
        &self,
        item: &CompletionItem,
        state: Entity<EditorState>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let reply = (item.data.is_some() && item.additional_text_edits.is_none())
            .then(|| self.server.upgrade())
            .flatten()
            .map(|server| server.request("completionItem/resolve", json!(item)));
        // The editor applied those the item came with already.
        let had_edits = item.additional_text_edits.is_some();
        let (item, workbench, handle) =
            (item.clone(), self.workbench.clone(), window.window_handle());
        cx.spawn(async move |cx| {
            let resolved = match reply {
                Some(rx) => serde_json::from_value(rx.await.unwrap_or_default()).ok(),
                None => None,
            };
            let resolved: CompletionItem = resolved.unwrap_or(item);
            handle
                .update(cx, |_, window, cx| {
                    if let Some(edits) =
                        (resolved.additional_text_edits).filter(|e| !had_edits && !e.is_empty())
                    {
                        state.update(cx, |e, cx| {
                            let text = e.text().clone();
                            let edits = (edits.into_iter())
                                .map(|t| {
                                    let r = text.position_to_offset(&t.range.start)
                                        ..text.position_to_offset(&t.range.end);
                                    (r, t.new_text)
                                })
                                .collect();
                            e.apply_text_edits(edits, window, cx);
                        });
                    }
                    if let Some(c) = resolved.command {
                        let args = c.arguments.unwrap_or_default();
                        // Once the editor's update asking is done.
                        window.defer(cx, move |window, cx| {
                            workbench
                                .update(cx, |this, cx| {
                                    this.run_any_command(c.command, args, window, cx)
                                })
                                .ok();
                        });
                    }
                })
                .ok();
        })
        .detach();
    }

    /// Typing a word character asks, as do the server's triggerCharacters.
    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        let mut chars = new_text.chars();
        matches!((chars.next(), chars.next()), (Some(c), None)
            if c.is_alphanumeric() || c == '_' || self.triggers().contains(c))
    }
}

impl Provider {
    /// The server's completion triggerCharacters (`.` and `:` before it says).
    fn triggers(&self) -> String {
        let Some(server) = self.server.upgrade() else {
            return String::new();
        };
        let caps = server.caps.lock().unwrap();
        match caps["completionProvider"]["triggerCharacters"].as_array() {
            Some(list) => list.iter().filter_map(Value::as_str).collect(),
            None => ".:".into(),
        }
    }
}

/// The items whose filterText (else label) matches `query` as VS Code's
/// filter does: its first character at the start of a word there, the rest
/// in order after it. Ones starting with it go first, each kept in the
/// server's sortText order.
// ponytail: a match tier and sortText, not VS Code's fuzzy score.
fn filter_items(items: Vec<CompletionItem>, query: &str) -> Vec<CompletionItem> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let mut kept: Vec<(bool, CompletionItem)> = (items.into_iter())
        .filter_map(|item| {
            let text = item.filter_text.as_deref().unwrap_or(&item.label);
            let prefix = text.to_lowercase().starts_with(&query.to_lowercase());
            (q.is_empty() || prefix || fuzzy_match(text, &q)).then_some((!prefix, item))
        })
        .collect();
    kept.sort_by(|(a, x), (b, y)| {
        let key = |i: &CompletionItem| i.sort_text.clone().unwrap_or_else(|| i.label.clone());
        a.cmp(b).then_with(|| key(x).cmp(&key(y)))
    });
    kept.into_iter().map(|(_, item)| item).collect()
}

/// `q` (lowercase) in `text`: the first at a word start (after `_`, a
/// non-letter or a lower-to-upper hump), the rest in order.
fn fuzzy_match(text: &str, q: &[char]) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let starts = (0..chars.len()).filter(|&i| {
        i == 0
            || !chars[i - 1].is_alphanumeric()
            || chars[i - 1].is_lowercase() && chars[i].is_uppercase()
    });
    starts.into_iter().any(|i| {
        let mut rest = chars[i..].iter().flat_map(|c| c.to_lowercase());
        rest.next() == q.first().copied() && q[1..].iter().all(|qc| rest.any(|c| c == *qc))
    })
}

#[cfg(test)]
mod filter_tests {
    use super::{CompletionItem, filter_items, fuzzy_match};

    #[test]
    fn filters_and_orders_like_vs_code() {
        let item = |label: &str, sort: &str| CompletionItem {
            label: label.into(),
            sort_text: Some(sort.into()),
            ..Default::default()
        };
        let items = vec![item("into_iter", "1"), item("iter", "2"), item("is_empty", "0"), item("filter", "3")];
        let labels = |q| filter_items(items.clone(), q).into_iter().map(|i| i.label).collect::<Vec<_>>();
        assert_eq!(labels("it"), ["iter", "is_empty", "into_iter"]);
        assert_eq!(labels("ie"), ["is_empty", "into_iter", "iter"]);
        assert_eq!(labels(""), ["is_empty", "into_iter", "iter", "filter"]);
        assert!(fuzzy_match("toString", &['s', 't']));
        assert!(!fuzzy_match("toString", &['r']));
    }
}

impl DefinitionProvider for Provider {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<LocationLink>>> {
        let reply = self.ask("textDocument/definition", text, offset, cx);
        cx.background_spawn(async move {
            let link = |l: lsp_types::Location| LocationLink {
                origin_selection_range: None,
                target_uri: l.uri,
                target_range: l.range,
                target_selection_range: l.range,
            };
            Ok(match serde_json::from_value(reply.await)? {
                None => vec![],
                Some(GotoDefinitionResponse::Scalar(l)) => vec![link(l)],
                Some(GotoDefinitionResponse::Array(ls)) => ls.into_iter().map(link).collect(),
                Some(GotoDefinitionResponse::Link(ls)) => ls,
            })
        })
    }
}

impl input::DocumentRangeSemanticTokensProvider for Provider {
    /// The theme's names, in [`SEMANTIC_TYPES`]' order; the server's own
    /// legend is mapped onto it as tokens arrive.
    fn legend(&self) -> lsp_types::SemanticTokensLegend {
        lsp_types::SemanticTokensLegend {
            token_types: (SEMANTIC_TYPES.iter())
                .map(|(_, theme)| lsp_types::SemanticTokenType::new(theme))
                .collect(),
            token_modifiers: Vec::new(),
        }
    }

    fn semantic_tokens(
        &self,
        text: &Rope,
        _: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<lsp_types::SemanticTokens>> {
        let Some(server) = self.server.upgrade() else {
            return Task::ready(Ok(Default::default()));
        };
        server.sync(&self.path, self.lang, &text.to_string());
        let params = json!({ "textDocument": { "uri": path_uri(&self.path) } });
        let rx = server.request("textDocument/semanticTokens/full", params);
        cx.background_spawn(async move {
            let reply = rx.await.unwrap_or_default();
            // An extension host sends its legend along; a server said it
            // at initialize.
            let legend = match &reply["legend"] {
                Value::Null => {
                    server.caps.lock().unwrap()["semanticTokensProvider"]["legend"].clone()
                }
                l => l.clone(),
            };
            // Types without a color get an index past the legend, which the
            // editor skips.
            let map: Vec<u32> = (legend["tokenTypes"].as_array().into_iter().flatten())
                .map(|t| {
                    let ix = SEMANTIC_TYPES
                        .iter()
                        .position(|(n, _)| Some(*n) == t.as_str());
                    ix.unwrap_or(SEMANTIC_TYPES.len()) as u32
                })
                .collect();
            let data: Vec<u32> = (reply["data"].as_array().into_iter().flatten())
                .map(|n| n.as_u64().unwrap_or(0) as u32)
                .collect();
            let data = (data.chunks_exact(5))
                .map(|t| lsp_types::SemanticToken {
                    delta_line: t[0],
                    delta_start: t[1],
                    length: t[2],
                    token_type: map.get(t[3] as usize).copied().unwrap_or(u32::MAX),
                    token_modifiers_bitset: 0,
                })
                .collect();
            Ok(lsp_types::SemanticTokens {
                result_id: None,
                data,
            })
        })
    }
}

impl CodeActionProvider for Provider {
    fn id(&self) -> SharedString {
        "lsp".into()
    }

    fn code_actions(
        &self,
        state: Entity<EditorState>,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<CodeAction>>> {
        let Some(server) = self.server.upgrade() else {
            return Task::ready(Ok(vec![]));
        };
        let text = state.read(cx).text().clone();
        server.sync(&self.path, self.lang, &text.to_string());
        let [start, end] = [range.start, range.end].map(|o| text.offset_to_position(o));
        // The diagnostics the range touches, which quick fixes answer.
        let diagnostics: Vec<Value> = (self.workbench.upgrade())
            .and_then(|wb| {
                let problems = wb.read(cx).lsp.problems.get(&self.path)?;
                let touches = |p: &&Problem| {
                    (p.line, p.col) <= (end.line, end.character)
                        && p.end >= (start.line, start.character)
                };
                Some(
                    problems
                        .iter()
                        .filter(touches)
                        .map(|p| p.raw.clone())
                        .collect(),
                )
            })
            .unwrap_or_default();
        let rx = server.request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": path_uri(&self.path) },
                "range": { "start": start, "end": end },
                "context": { "diagnostics": diagnostics },
            }),
        );
        cx.background_spawn(async move {
            let reply = rx.await.unwrap_or_default();
            let items: Option<Vec<CodeActionOrCommand>> = serde_json::from_value(reply)?;
            Ok((items.into_iter().flatten())
                .map(|item| match item {
                    CodeActionOrCommand::CodeAction(action) => action,
                    CodeActionOrCommand::Command(command) => CodeAction {
                        title: command.title.clone(),
                        command: Some(command),
                        ..Default::default()
                    },
                })
                .collect())
        })
    }

    // ponytail: the server's own `workspace/applyEdit` requests are dropped.
    fn perform_code_action(
        &self,
        _: Entity<EditorState>,
        action: CodeAction,
        _: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        let Some(server) = self.server.upgrade() else {
            return Task::ready(Ok(()));
        };
        // An action listed without its edit gets it resolved first.
        let reply = (action.edit.is_none() && action.data.is_some())
            .then(|| server.request("codeAction/resolve", json!(action)));
        let (workbench, handle) = (self.workbench.clone(), window.window_handle());
        let server = Arc::downgrade(&server);
        cx.spawn(async move |cx| {
            let action = match reply {
                Some(rx) => serde_json::from_value(rx.await.unwrap_or_default()).unwrap_or(action),
                None => action,
            };
            if let Some(edit) = action.edit {
                // Called from inside the editor's own update; edit once that ends.
                handle.update(cx, |_, window, cx| {
                    window.defer(cx, move |window, cx| {
                        (workbench
                            .update(cx, |this, cx| this.apply_workspace_edit(edit, window, cx)))
                        .ok();
                    });
                })?;
            }
            if let (Some(command), Some(server)) = (action.command, server.upgrade()) {
                let params = json!({ "command": command.command, "arguments": command.arguments });
                _ = server.request("workspace/executeCommand", params);
            }
            Ok(())
        })
    }
}

/// A formatting request's answer applied to `editor`, unless it was typed
/// over while the server worked (its edits would be stale).
async fn format_reply(
    reply: oneshot::Receiver<Value>,
    editor: Entity<EditorState>,
    before: SharedString,
    cx: &mut AsyncWindowContext,
) {
    let reply = reply.await.unwrap_or_default();
    let Ok(edits) = serde_json::from_value::<Vec<lsp_types::TextEdit>>(reply) else {
        return;
    };
    editor
        .update_in(cx, |e, window, cx| {
            if !edits.is_empty() && e.value() == before {
                apply_edits(e, edits, window, cx);
            }
        })
        .ok();
}

/// Applies a server's `edits` to the editor's text as one undo step.
fn apply_edits(
    e: &mut EditorState,
    edits: Vec<lsp_types::TextEdit>,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) {
    let text = e.text().clone();
    let at = |p: &lsp_types::Position| text.position_to_offset(p);
    let edits = (edits.into_iter())
        .map(|t| (at(&t.range.start)..at(&t.range.end), t.new_text))
        .collect();
    e.apply_text_edits(edits, window, cx);
}

fn send(out: &mpsc::Sender<String>, msg: Value) {
    let body = msg.to_string();
    out.send(format!("Content-Length: {}\r\n\r\n{body}", body.len()))
        .ok();
}

fn notify(out: &mpsc::Sender<String>, method: &str, params: Value) {
    send(
        out,
        json!({ "jsonrpc": "2.0", "method": method, "params": params }),
    );
}

/// `path` as a `file://` URI in VS Code's form, `file:///c%3A/a%20b`: the drive letter lower case and
/// all but unreserved characters encoded, as extensions' URIs compare.
pub(super) fn path_uri(path: &Path) -> String {
    let mut p = path.to_string_lossy().replace('\\', "/");
    if p.as_bytes().get(1) == Some(&b':') {
        p[..1].make_ascii_lowercase();
    }
    let mut uri = String::from(if p.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~' => {
                uri.push(b as char)
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}

/// The path of a `file://` URI; None for other schemes.
pub(super) fn uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?.as_bytes();
    let mut bytes = Vec::with_capacity(rest.len());
    let mut i = 0;
    while i < rest.len() {
        let hex = rest
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok());
        match (rest[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(b)) => {
                bytes.push(b);
                i += 3;
            }
            (b, _) => {
                bytes.push(b);
                i += 1;
            }
        }
    }
    let mut p = String::from_utf8(bytes).ok()?;
    // file:///c:/x → C:\x
    if cfg!(windows) && p.as_bytes().get(2) == Some(&b':') {
        p = p[1..2].to_uppercase() + &p[2..].replace('/', "\\");
    }
    Some(PathBuf::from(p))
}

/// Splits a server's stdout into its JSON-RPC messages.
/// `title` without its `$(icon)` codicons, which vsn can't draw inline.
fn strip_icons(title: &str) -> String {
    let mut out = String::new();
    let mut rest = title;
    while let Some(i) = rest.find("$(") {
        out.push_str(&rest[..i]);
        rest = rest[i..].find(')').map_or("", |j| &rest[i + j + 1..]);
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// The places in a reply of Locations or LocationLinks, one or a list.
fn locations(reply: Value) -> Vec<(PathBuf, lsp_types::Position)> {
    let list = match reply {
        Value::Array(list) => list,
        Value::Null => Vec::new(),
        one => vec![one],
    };
    (list.iter())
        .filter_map(|l| {
            let uri = l["uri"].as_str().or(l["targetUri"].as_str())?;
            let range = match &l["targetSelectionRange"] {
                Value::Null => &l["range"],
                r => r,
            };
            let at = serde_json::from_value(range["start"].clone()).ok()?;
            Some((uri_path(uri)?, at))
        })
        .collect()
}

fn read_frames(stdout: impl Read, mut emit: impl FnMut(Value)) {
    let mut reader = BufReader::new(stdout);
    let (mut line, mut len) = (String::new(), None);
    loop {
        line.clear();
        if !matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
            return;
        }
        if let Some(n) = line.strip_prefix("Content-Length:") {
            len = n.trim().parse::<usize>().ok();
        } else if line.trim().is_empty()
            && let Some(n) = len.take()
        {
            let mut body = vec![0; n];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            if let Ok(msg) = serde_json::from_slice(&body) {
                emit(msg);
            }
        }
    }
}

impl Workbench {
    /// Tells editor `ix`'s language server about its text, starting the
    /// server on the first file of its language, and points the editor's
    /// hover, completion and definition requests at it.
    pub(super) fn lsp_open(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(doc) = self
            .docs
            .get(ix)
            .filter(|d| !d.untitled && !d.binary && !d.large)
        else {
            return;
        };
        let snippets = Snippets::new(doc);
        let unit = match doc.indent.hard_tabs {
            true => "	".to_string(),
            false => " ".repeat(doc.indent.tab_size),
        };
        let cmd = match self.lsp.hosted.get(doc.lang) {
            Some(&host) => host,
            None => match SERVERS.iter().find(|(langs, _)| langs.contains(&doc.lang)) {
                Some(&(_, cmd)) => cmd,
                // No server: the editor completes snippets alone.
                None => {
                    if let Some(snippets) = snippets {
                        let snippets: Rc<dyn CompletionProvider> = Rc::new(snippets);
                        (doc.editor.clone())
                            .update(cx, |e, _| e.lsp_mut().completion_provider = Some(snippets));
                    }
                    return;
                }
            },
        };
        let (path, lang, editor) = (doc.path.clone(), doc.lang, doc.editor.clone());
        if !self.lsp.servers.contains_key(cmd[0]) {
            let server = self.start_server(cmd, cx);
            self.lsp.servers.insert(cmd[0], server);
        }
        let Some(Some(server)) = self.lsp.servers.get(cmd[0]) else {
            return;
        };
        server.sync(&path, lang, &editor.read(cx).value());
        let provider = Rc::new(Provider {
            server: Arc::downgrade(server),
            path: path.clone(),
            lang,
            workbench: cx.weak_entity(),
            snippets,
            unit,
        });
        let this = cx.weak_entity();
        // A definition in another file opens there; within its own file the
        // editor moves by itself.
        let show: input::ShowDocumentHandler = Rc::new(move |params, window, cx| {
            let (Some(target), Some(sel)) = (uri_path(params.uri.as_str()), params.selection)
            else {
                return false;
            };
            if target == path {
                return false;
            }
            let this = this.clone();
            // The editor asking is mid-update; open once it's done.
            window.defer(cx, move |window, cx| {
                this.update(cx, |this, cx| {
                    this.open_path(target, window, cx);
                    if let Some(doc) = this.docs.get(this.active) {
                        reveal(&doc.editor.clone(), sel.start, window, cx);
                    }
                })
                .ok();
            });
            true
        });
        editor.update(cx, |e, _| {
            let lsp = e.lsp_mut();
            lsp.hover_provider = Some(provider.clone());
            lsp.completion_provider = Some(provider.clone());
            lsp.definition_provider = Some(provider.clone());
            lsp.semantic_tokens_provider = Some(provider.clone());
            lsp.code_action_providers = vec![provider];
            lsp.show_document = Some(show);
        });
        self.lsp_decorate(ix, cx);
    }

    /// Fetches editor `ix`'s inlay hints, lenses and folding ranges from its
    /// server, shortly after the last edit.
    pub(super) fn lsp_decorate(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(ix) else {
            return;
        };
        let (path, editor) = (doc.path.clone(), doc.editor.clone());
        let Some(server) = (self.lsp.servers.values().flatten())
            .find(|s| s.open.lock().unwrap().contains_key(&path))
            .cloned()
        else {
            return;
        };
        let key = path.clone();
        // Replacing the last fetch drops it, so stale replies never land.
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(300))
                .await;
            let text = editor.read_with(cx, |e, _| e.text().clone());
            let doc = json!({ "uri": path_uri(&path) });
            let end = text.offset_to_position(text.len());
            let range = json!({ "start": { "line": 0, "character": 0 }, "end": end });
            // A server still loading the workspace answers null (or an
            // error); ask again a little later, a few times.
            let (mut hints, mut lenses) = (Value::Null, Value::Null);
            for wait in [0, 1000, 2000, 4000, 8000] {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(wait))
                    .await;
                let h = server.request(
                    "textDocument/inlayHint",
                    json!({ "textDocument": doc, "range": range }),
                );
                let l = server.request("textDocument/codeLens", json!({ "textDocument": doc }));
                (hints, lenses) = (h.await.unwrap_or_default(), l.await.unwrap_or_default());
                if !hints.is_null() && !lenses.is_null() {
                    break;
                }
            }
            // A server may leave a lens' command for codeLens/resolve.
            for lens in lenses.as_array_mut().into_iter().flatten() {
                if lens["command"].is_null() {
                    *lens = server
                        .request("codeLens/resolve", lens.clone())
                        .await
                        .unwrap_or_default();
                }
            }
            let folds = server.request("textDocument/foldingRange", json!({ "textDocument": doc }));
            // LSP's end is the last line hidden; the editor's is the one after.
            let folds: Vec<_> = (folds.await.unwrap_or_default().as_array().into_iter().flatten())
                .filter_map(|f| Some((f["startLine"].as_u64()? as usize, f["endLine"].as_u64()? as usize)))
                .filter(|(start, end)| end > start)
                .map(|(start, end)| FoldRange::new(start, end + 1))
                .collect();
            if !folds.is_empty() {
                editor.update(cx, |e, cx| e.apply_highlighter_fold_candidates(folds, cx));
            }
            let weak = Arc::downgrade(&server);
            this.update(cx, |this, cx| {
                let lsp = &mut this.lsp;
                lsp.decor.retain(|_, (p, ..)| *p != path);
                let mut target = |what: Value| {
                    let id = lsp.next_decor;
                    lsp.next_decor += 1;
                    lsp.decor.insert(id, (path.clone(), weak.clone(), what));
                    id
                };
                let hints = (hints.as_array().into_iter().flatten())
                    .filter_map(|h| {
                        let at = serde_json::from_value(h["position"].clone()).ok()?;
                        let label = match &h["label"] {
                            Value::String(s) => vec![InlayLabelPart {
                                text: s.clone().into(),
                                id: None,
                            }],
                            Value::Array(parts) => (parts.iter())
                                .map(|p| {
                                    let what = match (&p["command"], &p["location"]) {
                                        (Value::Null, Value::Null) => None,
                                        (Value::Null, l) => Some(l.clone()),
                                        (c, _) => Some(c.clone()),
                                    };
                                    InlayLabelPart {
                                        text: p["value"]
                                            .as_str()
                                            .unwrap_or_default()
                                            .to_string()
                                            .into(),
                                        id: what.map(&mut target),
                                    }
                                })
                                .collect(),
                            _ => return None,
                        };
                        Some(InlayHint {
                            offset: text.position_to_offset(&at),
                            label,
                            padding_left: h["paddingLeft"].as_bool().unwrap_or(false),
                            padding_right: h["paddingRight"].as_bool().unwrap_or(false),
                        })
                    })
                    .collect();
                let lenses = (lenses.as_array().into_iter().flatten())
                    .filter_map(|l| {
                        let title = l["command"]["title"].as_str().filter(|t| !t.is_empty())?;
                        let line = l["range"]["start"]["line"].as_u64()? as usize;
                        let title = strip_icons(title).into();
                        Some(CodeLens {
                            line,
                            items: vec![(title, target(l["command"].clone()))],
                        })
                    })
                    .collect();
                editor.update(cx, |e, cx| {
                    e.set_inlay_hints(hints, cx);
                    e.set_code_lenses(lenses, cx);
                });
            })
            .ok();
        });
        self.lsp.decorating.insert(key, task);
    }

    /// The cursor in `editor` may have moved: shortly after, its server's
    /// highlights of the symbol there replace the last ones.
    pub(super) fn lsp_cursor_moved(
        &mut self,
        editor: &Entity<EditorState>,
        cx: &mut Context<Self>,
    ) {
        let e = editor.read(cx);
        let at = (editor.entity_id(), e.cursor());
        if self.lsp.highlight_at == Some(at) {
            return;
        }
        self.lsp.highlight_at = Some(at);
        let selected = !e.selected_range().is_empty();
        let position = e.text().offset_to_position(at.1);
        if let Some((_, shown)) = self.lsp.highlights.take() {
            shown.dispose(cx);
        }
        let Some(doc) = self
            .docs
            .iter()
            .find(|d| &d.editor == editor)
            .filter(|_| !selected)
        else {
            self.lsp.highlighting = None;
            return;
        };
        let params = json!({
            "textDocument": { "uri": path_uri(&doc.path) },
            "position": position,
        });
        let (path, editor) = (doc.path.clone(), editor.clone());
        self.lsp.highlighting = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(250))
                .await;
            let Ok(Some(ask)) = this.update(cx, |this, _| {
                this.lsp
                    .request(&path, "textDocument/documentHighlight", params)
            }) else {
                return;
            };
            let reply = ask.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                let decos: Vec<_> = (reply.as_array().into_iter().flatten())
                    .filter_map(|h| {
                        let range: lsp_types::Range =
                            serde_json::from_value(h["range"].clone()).ok()?;
                        let text = editor.read(cx).text();
                        let range = text.position_to_offset(&range.start)
                            ..text.position_to_offset(&range.end);
                        // A write reads stronger than a read, as VS Code's.
                        let alpha = if h["kind"].as_u64() == Some(3) {
                            0.25
                        } else {
                            0.15
                        };
                        Some(
                            RangeDecoration::new(range)
                                .with_style(RangeDecorationStyle::Fill)
                                .with_color(col::fg().opacity(alpha)),
                        )
                    })
                    .collect();
                if decos.is_empty() {
                    return;
                }
                let shown =
                    editor.update(cx, |e, cx| e.create_range_decorations_collection(decos, cx));
                this.lsp.highlights = Some((editor, shown));
            })
            .ok();
        }));
    }

    /// Every editor's inlay hints and lenses, again.
    fn lsp_decorate_all(&mut self, cx: &mut Context<Self>) {
        for ix in 0..self.docs.len() {
            self.lsp_decorate(ix, cx);
        }
    }

    /// An inlay hint's part or a lens was clicked: its server runs the
    /// command, or the editor goes to the place.
    pub(super) fn lsp_decor_clicked(
        &mut self,
        id: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, server, what)) = self.lsp.decor.get(&id).cloned() else {
            return;
        };
        if let Some(command) = what["command"].as_str() {
            if let Some(server) = server.upgrade() {
                let params = json!({ "command": command, "arguments": what["arguments"] });
                _ = server.request("workspace/executeCommand", params);
            }
        } else if let Some(path) = what["uri"].as_str().and_then(uri_path)
            && let Ok(at) = serde_json::from_value(what["range"]["start"].clone())
        {
            self.go_to(path, at, window, cx);
        }
    }

    /// Editor `ix` changed: its server gets the whole new text.
    pub(super) fn lsp_change(&mut self, ix: usize, cx: &mut Context<Self>) {
        let doc = &self.docs[ix];
        for server in self.lsp.servers.values().flatten() {
            if server.open.lock().unwrap().contains_key(&doc.path) {
                server.sync(&doc.path, doc.lang, &doc.editor.read(cx).value());
            }
        }
        self.lsp_decorate(ix, cx);
    }

    /// Sends didClose for the files no editor shows any more.
    pub(super) fn lsp_closed(&self) {
        for server in self.lsp.servers.values().flatten() {
            server.open.lock().unwrap().retain(|path, _| {
                let shown = self.docs.iter().any(|d| &d.path == path);
                if !shown {
                    let doc = json!({ "textDocument": { "uri": path_uri(path) } });
                    notify(&server.out, "textDocument/didClose", doc);
                }
                shown
            });
        }
    }

    pub(super) fn lsp_saved(&mut self, path: &Path) {
        for server in self.lsp.servers.values().flatten() {
            if server.open.lock().unwrap().contains_key(path) {
                let uri = path_uri(path);
                notify(
                    &server.out,
                    "textDocument/didSave",
                    json!({ "textDocument": { "uri": uri } }),
                );
            }
        }
    }

    /// Format Document, or Format Selection (the cursor's line when nothing
    /// is selected): the server's edits to the active file, as one undo step.
    pub(super) fn format_document(
        &mut self,
        selection: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let server = (self.lsp.servers.values().flatten())
            .find(|s| s.open.lock().unwrap().contains_key(&doc.path));
        let Some(server) = server else {
            let text = format!("There is no formatter for '{}' files installed.", doc.lang);
            self.note(text.into(), Severity::Info, cx);
            return;
        };
        let (editor, before) = (doc.editor.clone(), doc.editor.read(cx).value());
        let mut params = json!({
            "textDocument": { "uri": path_uri(&doc.path) },
            "options": { "tabSize": doc.indent.tab_size, "insertSpaces": !doc.indent.hard_tabs },
        });
        let mut method = "textDocument/formatting";
        if selection {
            let e = editor.read(cx);
            let sel = e.selected_range();
            let [mut start, mut end] = [sel.start, sel.end].map(|o| e.text().offset_to_position(o));
            if start == end {
                start.character = 0;
                (end.line, end.character) = (end.line + 1, 0);
            }
            params["range"] = json!({ "start": start, "end": end });
            method = "textDocument/rangeFormatting";
        }
        let reply = server.request(method, params);
        cx.spawn_in(window, async move |_, cx| format_reply(reply, editor, before, cx).await)
            .detach();
    }

    /// editor.formatOnSave: Save formats editor `ix` with its server first,
    /// then writes it.
    // ponytail: Save only, not Save All; `[lang]` overrides aren't read; and
    // a server slower than 5s is given up on, where VS Code shows progress
    // with a Cancel button.
    pub(super) fn save_formatted(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let settings = super::terminal::settings();
        let server = (settings["editor.formatOnSave"] == true)
            .then(|| self.docs.get(ix))
            .flatten()
            .and_then(|doc| {
                (self.lsp.servers.values().flatten()).find(|s| s.open.lock().unwrap().contains_key(&doc.path))
            });
        let Some(server) = server else {
            return self.write(ix, window, cx);
        };
        let doc = &self.docs[ix];
        let (editor, before) = (doc.editor.clone(), doc.editor.read(cx).value());
        let params = json!({
            "textDocument": { "uri": path_uri(&doc.path) },
            "options": { "tabSize": doc.indent.tab_size, "insertSpaces": !doc.indent.hard_tabs },
        });
        let reply = server.request("textDocument/formatting", params);
        cx.spawn_in(window, async move |this, cx| {
            let timer = cx.background_executor().timer(std::time::Duration::from_secs(5));
            let format = format_reply(reply, editor.clone(), before, cx);
            futures::future::select(Box::pin(format), Box::pin(timer)).await;
            this.update_in(cx, |this, window, cx| {
                if let Some(ix) = this.docs.iter().position(|d| d.editor == editor) {
                    this.write(ix, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// A server's edits across files: each file opens and takes its edits as
    /// one undo step, then the editor that asked is back in front.
    // ponytail: the files open as tabs where VS Code edits them unseen, and
    // file creates, renames and deletes are dropped.
    pub(super) fn apply_workspace_edit(
        &mut self,
        edit: lsp_types::WorkspaceEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let doc_edits = |e: lsp_types::TextDocumentEdit| {
            let edits = e.edits.into_iter().map(|e| match e {
                OneOf::Left(e) => e,
                OneOf::Right(e) => e.text_edit,
            });
            (e.text_document.uri, edits.collect())
        };
        let mut files: Vec<(lsp_types::Uri, Vec<lsp_types::TextEdit>)> =
            edit.changes.into_iter().flatten().collect();
        match edit.document_changes {
            Some(DocumentChanges::Edits(edits)) => files.extend(edits.into_iter().map(doc_edits)),
            Some(DocumentChanges::Operations(ops)) => {
                files.extend(ops.into_iter().filter_map(|op| match op {
                    DocumentChangeOperation::Edit(e) => Some(doc_edits(e)),
                    DocumentChangeOperation::Op(_) => None,
                }))
            }
            None => {}
        }
        let back = self.docs.get(self.active).map(|d| d.path.clone());
        for (uri, edits) in files {
            let Some(path) = uri_path(uri.as_str()) else {
                continue;
            };
            self.open_path(path.clone(), window, cx);
            if let Some(doc) = self.docs.iter().find(|d| d.path == path) {
                (doc.editor.clone()).update(cx, |e, cx| apply_edits(e, edits, window, cx));
            }
        }
        if let Some(path) = back {
            self.open_path(path, window, cx);
        }
    }

    /// F2: the palette asks for a new name for the symbol at the cursor.
    // ponytail: the palette stands in for VS Code's inline rename box, and
    // the word at the cursor for `textDocument/prepareRename`'s answer.
    pub(super) fn rename_symbol(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let e = doc.editor.read(cx);
        let (text, at) = (e.value(), e.selected_range().start);
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let start = text[..at].trim_end_matches(word).len();
        let end = at + text[at..].find(|c| !word(c)).unwrap_or(text.len() - at);
        if start == end {
            self.note("The element can't be renamed.".into(), Severity::Info, cx);
            return;
        }
        let what = Rename::Symbol(doc.path.clone(), e.text().offset_to_position(at));
        self.rename_input(what, text[start..end].to_string(), window, cx);
    }

    /// Rename Symbol's new name: the server's edits apply across files.
    pub(super) fn rename_symbol_to(
        &mut self,
        path: PathBuf,
        at: lsp_types::Position,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let params =
            json!({ "textDocument": { "uri": path_uri(&path) }, "position": at, "newName": name });
        let Some(reply) = self.request_for(&path, "textDocument/rename", params) else {
            self.note("No result.".into(), Severity::Info, cx);
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let reply = reply.await.unwrap_or_default();
            let edit = serde_json::from_value::<Option<lsp_types::WorkspaceEdit>>(reply);
            this.update_in(cx, |this, window, cx| match edit {
                Ok(Some(edit)) => this.apply_workspace_edit(edit, window, cx),
                _ => this.note("No result.".into(), Severity::Info, cx),
            })
            .ok();
        })
        .detach();
    }

    /// After an edit in the active editor: `(` and `,` ask for the call's
    /// parameter hints, and while they show every edit asks again.
    // ponytail: the server's trigger characters are ignored, and moving the
    // cursor without typing leaves the hints as they were.
    pub(super) fn signature_on_change(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let e = doc.editor.read(cx);
        // Read in place: copying a big file's text per keystroke stalled it.
        let before = e.text().chars_at(e.selected_range().start).prev();
        if self.lsp.signature.is_some() || matches!(before, Some('(' | ',')) {
            self.signature_help(cx);
        }
    }

    /// Ctrl+Shift+Space: parameter hints for the call at the cursor; an
    /// empty answer hides them.
    pub(super) fn signature_help(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let e = doc.editor.read(cx);
        let at = e.text().offset_to_position(e.selected_range().start);
        let path = doc.path.clone();
        let params = json!({ "textDocument": { "uri": path_uri(&path) }, "position": at });
        let Some(reply) = self.request_for(&path, "textDocument/signatureHelp", params) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let reply = reply.await.unwrap_or_default();
            let help = serde_json::from_value::<Option<lsp_types::SignatureHelp>>(reply);
            let help = help.ok().flatten().filter(|h| !h.signatures.is_empty());
            this.update(cx, |this, cx| {
                this.lsp.signature = help.map(|h| (path, h));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn hide_signature(&mut self, cx: &mut Context<Self>) {
        if self.lsp.signature.take().is_some() {
            cx.notify();
        }
    }

    /// The parameter hints: the signature with its active parameter in bold,
    /// and that parameter's documentation or else the signature's.
    pub(super) fn render_signature(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let (path, help) = self.lsp.signature.as_ref()?;
        let doc = self.docs.get(self.active).filter(|d| d.path == *path)?;
        let e = doc.editor.read(cx);
        let at = e.selected_range().start;
        let cursor = e.range_to_bounds(&(at..at))?;
        let sig = (help
            .signatures
            .get(help.active_signature.unwrap_or(0) as usize))
        .or(help.signatures.first())?;
        let label = &sig.label;
        let param = (sig.active_parameter.or(help.active_parameter))
            .and_then(|i| sig.parameters.as_ref()?.get(i as usize));
        let range = param.and_then(|p| match &p.label {
            lsp_types::ParameterLabel::Simple(name) => {
                // Past the name, which could hold the parameter's text too.
                let open = label.find('(').unwrap_or(0);
                let start = open + label[open..].find(name.as_str())?;
                Some(start..start + name.len())
            }
            lsp_types::ParameterLabel::LabelOffsets([a, b]) => {
                let byte = |n: u32| {
                    let mut units = 0;
                    (label.char_indices())
                        .find(|(_, ch)| {
                            let hit = units >= n;
                            units += ch.len_utf16() as u32;
                            hit
                        })
                        .map_or(label.len(), |(i, _)| i)
                };
                Some(byte(*a)..byte(*b))
            }
        });
        let text = |d: &lsp_types::Documentation| match d {
            lsp_types::Documentation::String(s) => s.clone(),
            lsp_types::Documentation::MarkupContent(m) => m.value.clone(),
        };
        let docs = (param.and_then(|p| p.documentation.as_ref()))
            .or(sig.documentation.as_ref())
            .map(text)
            .filter(|d| !d.trim().is_empty());
        let bold = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            color: Some(c(0x48A0C7)),
            ..Default::default()
        };
        let label = StyledText::new(label.clone()).with_highlights(range.map(|r| (r, bold)));
        Some(deferred(
            anchored()
                .anchor(Anchor::BottomLeft)
                .position(cursor.origin)
                .child(
                    v_flex()
                        .mb(px(4.))
                        .max_w(px(600.))
                        .px(px(8.))
                        .py(px(4.))
                        .gap(px(4.))
                        .bg(c(0x202122))
                        .border_1()
                        .border_color(col::input_border())
                        .rounded(popup_radius(4.))
                        .shadow_md()
                        .text_size(px(13.))
                        .text_color(col::fg())
                        .font_family("Consolas")
                        .child(label)
                        .when_some(docs, |d, docs| {
                            d.child(div().font_family(".SystemUIFont").child(docs))
                        }),
                ),
        ))
    }

    /// Sends `method` to the server that has `path` open.
    fn request_for(
        &self,
        path: &Path,
        method: &str,
        params: Value,
    ) -> Option<oneshot::Receiver<Value>> {
        self.lsp.request(path, method, params)
    }

    /// Shift+F12: the symbol's references, a pick list grouped by file.
    // ponytail: a pick list stands in for VS Code's peek view and References
    // panel.
    pub(super) fn find_references(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let context = json!({ "context": { "includeDeclaration": true } });
        self.request_locations(
            "textDocument/references",
            context,
            "references",
            false,
            window,
            cx,
        );
    }

    /// Go to the symbol's type definition, implementations or declaration:
    /// the one there is, or a pick list of them.
    pub(super) fn go_to_locations(
        &mut self,
        method: &str,
        what: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_locations(method, json!({}), what, true, window, cx);
    }

    /// Sends `method` for the cursor's position, with `extra` params, and
    /// shows the places it answers with.
    fn request_locations(
        &mut self,
        method: &str,
        extra: Value,
        what: &'static str,
        jump: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let e = doc.editor.read(cx);
        let at = e.text().offset_to_position(e.selected_range().start);
        let mut params = json!({ "textDocument": { "uri": path_uri(&doc.path) }, "position": at });
        if let (Some(p), Value::Object(extra)) = (params.as_object_mut(), extra) {
            p.extend(extra);
        }
        let none = SharedString::from(format!("No {what} found"));
        let Some(reply) = self.request_for(&doc.path.clone(), method, params) else {
            self.note(none, Severity::Info, cx);
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let mut found = locations(reply.await.unwrap_or_default());
            this.update_in(cx, |this, window, cx| match found.len() {
                0 => this.note(none, Severity::Info, cx),
                1 if jump => {
                    let (path, at) = found.swap_remove(0);
                    this.go_to(path, at, window, cx);
                }
                _ => this.pick_locations(found, format!("Go to {what}"), window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// `found` as a pick list grouped by file, each row its line's text.
    pub(super) fn pick_locations(
        &mut self,
        mut found: Vec<(PathBuf, lsp_types::Position)>,
        placeholder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        found.sort_by_key(|(path, at)| (path.clone(), at.line, at.character));
        found.dedup();
        let (mut rows, mut groups) = (Vec::new(), Vec::new());
        let mut lines = Vec::new();
        for (ix, (path, at)) in found.iter().enumerate() {
            if ix == 0 || found[ix - 1].0 != *path {
                let open = self.docs.iter().find(|d| d.path == *path);
                let text = match open {
                    Some(d) => d.editor.read(cx).value().to_string(),
                    None => fs::read_to_string(path).unwrap_or_default(),
                };
                lines = text.lines().map(str::to_string).collect();
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                groups.push((ix, name.into_owned().into()));
            }
            let line = lines.get(at.line as usize).map_or("", |l| l.trim());
            rows.push((
                line.to_string(),
                format!("{}:{}", at.line + 1, at.character + 1),
            ));
        }
        let items = super::palette::Items {
            picker: super::palette::Picker::Location(found),
            rows,
            groups,
            placeholder: placeholder.into(),
            current: 0,
        };
        self.open_items(items, window, cx);
    }

    /// Shift+Alt+H: the calls to the function at the cursor, a pick list of
    /// the callers.
    // ponytail: a pick list of incoming calls stands in for VS Code's Call
    // Hierarchy view; outgoing calls and going deeper aren't offered.
    pub(super) fn call_hierarchy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let e = doc.editor.read(cx);
        let at = e.text().offset_to_position(e.selected_range().start);
        let path = doc.path.clone();
        let params = json!({ "textDocument": { "uri": path_uri(&path) }, "position": at });
        let none = SharedString::from("No call hierarchy found");
        let Some(server) = (self.lsp.servers.values().flatten())
            .find(|s| s.open.lock().unwrap().contains_key(&path))
            .cloned()
        else {
            self.note(none, Severity::Info, cx);
            return;
        };
        let prepare = server.request("textDocument/prepareCallHierarchy", params);
        cx.spawn_in(window, async move |this, cx| {
            let item = match prepare.await.unwrap_or_default() {
                Value::Array(mut a) if !a.is_empty() => a.swap_remove(0),
                _ => Value::Null,
            };
            let calls = match item.is_object() {
                true => {
                    server
                        .request("callHierarchy/incomingCalls", json!({ "item": item }))
                        .await
                }
                false => Ok(Value::Null),
            };
            let calls = calls.unwrap_or_default();
            this.update_in(cx, |this, window, cx| {
                let calls = calls.as_array().cloned().unwrap_or_default();
                let mut found = Vec::new();
                let mut rows = Vec::new();
                for call in &calls {
                    let from = &call["from"];
                    let Some(path) = from["uri"].as_str().and_then(uri_path) else {
                        continue;
                    };
                    let range = match &call["fromRanges"][0] {
                        Value::Null => &from["selectionRange"],
                        r => r,
                    };
                    let Ok(at) =
                        serde_json::from_value::<lsp_types::Position>(range["start"].clone())
                    else {
                        continue;
                    };
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    let label = from["name"].as_str().unwrap_or_default().to_string();
                    rows.push((label, format!("{name}:{}", at.line + 1)));
                    found.push((path, at));
                }
                if found.is_empty() {
                    return this.note(none, Severity::Info, cx);
                }
                let name = item["name"].as_str().unwrap_or_default();
                let items = super::palette::Items {
                    picker: super::palette::Picker::Location(found),
                    rows,
                    groups: Vec::new(),
                    placeholder: format!("Calls to '{name}'").into(),
                    current: 0,
                };
                this.open_items(items, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Extension host `host` now serves `ids`: their built-in servers make
    /// way and their files go to the host. `forward` passes it a message's
    /// JSON, false once the host is gone.
    pub(super) fn lsp_languages(
        &mut self,
        ids: Vec<String>,
        host: &'static [&'static str],
        forward: impl Fn(&str) -> bool + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.lsp.servers.entry(host[0]).or_insert_with(|| {
            let (tx, rx) = mpsc::channel::<String>();
            std::thread::spawn(move || {
                for frame in rx {
                    let body = frame.split_once("\r\n\r\n").map_or("", |(_, b)| b);
                    if !forward(body) {
                        return;
                    }
                }
            });
            Some(Arc::new(Server {
                out: tx,
                next_id: AtomicI64::new(1),
                pending: Mutex::default(),
                open: Mutex::default(),
                caps: Mutex::default(),
            }))
        });
        let new: Vec<String> = (ids.into_iter())
            .filter(|id| self.lsp.hosted.insert(id.clone(), host) != Some(host))
            .collect();
        // ponytail: a built-in server shared with other languages (TypeScript's)
        // stops for them too, and starts again with their next file.
        for (langs, cmd) in SERVERS {
            if langs.iter().any(|l| new.iter().any(|n| n == l)) {
                self.lsp.servers.remove(cmd[0]);
            }
        }
        for ix in 0..self.docs.len() {
            if new.iter().any(|n| n == self.docs[ix].lang) {
                self.lsp_open(ix, cx);
            }
        }
    }

    /// A message from extension host `host`'s providers: a response, or
    /// diagnostics.
    pub(super) fn lsp_from_host(
        &mut self,
        msg: Value,
        host: &'static [&'static str],
        cx: &mut Context<Self>,
    ) {
        let Some(id) = msg["id"].as_i64() else {
            return self.on_server(msg, cx);
        };
        if let Some(Some(server)) = self.lsp.servers.get(host[0])
            && let Some(tx) = server.pending.lock().unwrap().remove(&id)
        {
            tx.send(msg["result"].clone()).ok();
        }
    }

    /// Extension host `host` is gone: its languages' files go back to the
    /// built-in servers until a new host serves them again.
    pub(super) fn lsp_host_gone(&mut self, host: &'static [&'static str], cx: &mut Context<Self>) {
        self.lsp.servers.remove(host[0]);
        let mut langs = HashSet::new();
        self.lsp
            .hosted
            .retain(|lang, h| *h != host || !langs.insert(lang.clone()));
        for ix in 0..self.docs.len() {
            if langs.contains(self.docs[ix].lang) {
                self.lsp_open(ix, cx);
            }
        }
    }

    /// A new workspace: servers start over in it.
    pub(super) fn lsp_reset(&mut self) {
        self.lsp = Lsp::default();
    }

    fn start_server(
        &mut self,
        cmd: &'static [&'static str],
        cx: &mut Context<Self>,
    ) -> Option<Arc<Server>> {
        let mut command = Command::new(cmd[0]);
        command
            .args(&cmd[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(root) = &self.root {
            command.current_dir(root);
        }
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000); // CREATE_NO_WINDOW
        // Not installed: the language just goes without.
        let mut child = command.spawn().ok()?;
        let (mut stdin, stdout, stderr) = (
            child.stdin.take()?,
            child.stdout.take()?,
            child.stderr.take()?,
        );
        let (out, out_rx) = mpsc::channel::<String>();
        let (ready, ready_rx) = mpsc::channel::<()>();
        // Nothing but `initialize` goes out until the server answers it.
        std::thread::spawn(move || {
            let mut first = true;
            for msg in out_rx {
                if stdin
                    .write_all(msg.as_bytes())
                    .and_then(|()| stdin.flush())
                    .is_err()
                    || (std::mem::take(&mut first) && ready_rx.recv().is_err())
                {
                    return;
                }
            }
        });
        let server = Arc::new(Server {
            out: out.clone(),
            next_id: AtomicI64::new(1),
            pending: Mutex::default(),
            open: Mutex::default(),
            caps: Mutex::default(),
        });
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Value>();
        let log = |text: &str| json!({ "method": "vsn/log", "params": [cmd[0], text] });
        let errors = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                errors.unbounded_send(log(&line)).ok();
            }
        });
        // Weak, so dropping the server closes stdin and ends both threads.
        let weak = Arc::downgrade(&server);
        std::thread::spawn(move || {
            read_frames(stdout, |msg| match (&msg["id"], msg["method"].as_str()) {
                (Value::Null, Some("window/logMessage")) => {
                    let text = msg["params"]["message"].as_str().unwrap_or_default();
                    tx.unbounded_send(log(text)).ok();
                }
                (Value::Null, Some(_)) => _ = tx.unbounded_send(msg),
                (id, None) if id == 0 => {
                    if let Some(server) = weak.upgrade() {
                        *server.caps.lock().unwrap() = msg["result"]["capabilities"].clone();
                    }
                    ready.send(()).ok();
                }
                (id, None) => {
                    let waiting = (weak.upgrade().zip(id.as_i64()))
                        .and_then(|(s, id)| s.pending.lock().unwrap().remove(&id));
                    if let Some(tx) = waiting {
                        tx.send(msg["result"].clone()).ok();
                    }
                }
                (id, Some(method)) => {
                    let Some(server) = weak.upgrade() else {
                        return;
                    };
                    // Requests from the server get an empty answer; settings
                    // are null per item, so servers use their defaults.
                    let items = msg["params"]["items"].as_array().map_or(0, Vec::len);
                    let result = match method {
                        "workspace/configuration" => json!(vec![Value::Null; items]),
                        _ => Value::Null,
                    };
                    if method.ends_with("/refresh") {
                        tx.unbounded_send(json!({ "method": method })).ok();
                    }
                    send(
                        &server.out,
                        json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    );
                }
            });
            child.kill().ok();
            child.wait().ok();
            // Still wanted, so it died on its own.
            if weak.upgrade().is_some() {
                tx.unbounded_send(json!({ "method": "vsn/exited", "params": cmd[0] }))
                    .ok();
            }
        });
        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                if this.update(cx, |this, cx| this.on_server(msg, cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
        let root = self.root.as_deref().map(path_uri);
        let name = self
            .root
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy());
        send(
            &out,
            json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                "processId": std::process::id(),
                "clientInfo": { "name": "vsn" },
                "rootUri": root,
                "workspaceFolders": root.as_ref().map(|uri| json!([{ "uri": uri, "name": name }])),
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "textDocument": {
                        "synchronization": { "didSave": true },
                        "publishDiagnostics": { "relatedInformation": true },
                        "hover": { "contentFormat": ["markdown", "plaintext"] },
                        // No resolveSupport: servers then send auto-imports' edits up front.
                        "completion": { "completionItem": { "snippetSupport": true } },
                        "definition": { "linkSupport": true },
                        "typeDefinition": { "linkSupport": true },
                        "implementation": { "linkSupport": true },
                        "declaration": { "linkSupport": true },
                        "references": {},
                        "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                        "documentHighlight": {},
                        "selectionRange": {},
                        "foldingRange": { "lineFoldingOnly": true },
                        "callHierarchy": {},
                        "onTypeFormatting": {},
                        "inlayHint": {},
                        "codeLens": {},
                        "semanticTokens": {
                            "requests": { "full": true },
                            "tokenTypes": SEMANTIC_TYPES.iter().map(|(t, _)| t).collect::<Vec<_>>(),
                            "tokenModifiers": [],
                            "formats": ["relative"],
                        },
                        "formatting": {},
                        "rangeFormatting": {},
                        "rename": {},
                        "signatureHelp": { "signatureInformation": {
                            "parameterInformation": { "labelOffsetSupport": true },
                        } },
                        "codeAction": {
                            "dataSupport": true,
                            "resolveSupport": { "properties": ["edit"] },
                            "codeActionLiteralSupport": { "codeActionKind": {
                                "valueSet": ["", "quickfix", "refactor", "refactor.extract",
                                    "refactor.inline", "refactor.rewrite", "source",
                                    "source.organizeImports"],
                            }},
                        },
                    },
                    "workspace": {
                        "configuration": true,
                        "workspaceFolders": true,
                        "symbol": {},
                        "semanticTokens": { "refreshSupport": true },
                        "inlayHint": { "refreshSupport": true },
                        "codeLens": { "refreshSupport": true },
                    },
                },
            }}),
        );
        notify(&out, "initialized", json!({}));
        Some(server)
    }

    fn on_server(&mut self, msg: Value, cx: &mut Context<Self>) {
        match msg["method"].as_str() {
            Some("textDocument/publishDiagnostics") => {
                let params = &msg["params"];
                let Some(path) = params["uri"].as_str().and_then(uri_path) else {
                    return;
                };
                let at = |p: &Value| {
                    let n = |k: &str| p[k].as_u64().unwrap_or(0) as u32;
                    (n("line"), n("character"))
                };
                let list: Vec<Problem> = (params["diagnostics"].as_array().into_iter().flatten())
                    .map(|d| {
                        let ((line, col), end) = (at(&d["range"]["start"]), at(&d["range"]["end"]));
                        Problem {
                            line,
                            col,
                            end,
                            severity: d["severity"].as_u64().unwrap_or(1),
                            message: d["message"].as_str().unwrap_or_default().to_string(),
                            source: d["source"].as_str().unwrap_or_default().to_string(),
                            raw: d.clone(),
                        }
                    })
                    .collect();
                if list.is_empty() {
                    self.lsp.problems.remove(&path);
                } else {
                    self.lsp.problems.insert(path.clone(), list);
                }
                self.show_diagnostics(&path, cx);
                cx.notify();
            }
            Some("vsn/log") => {
                let [channel, text] = [0, 1].map(|i| msg["params"][i].as_str().unwrap_or_default());
                self.log(channel, text, cx);
            }
            Some("vsn/exited") => {
                let Some(&(_, cmd)) = SERVERS.iter().find(|(_, c)| msg["params"] == c[0]) else {
                    return;
                };
                let crashes = self.lsp.crashes.entry(cmd[0]).or_default();
                *crashes += 1;
                // VS Code's limit, without its 3-minute window.
                if *crashes >= 5 {
                    self.lsp.servers.insert(cmd[0], None);
                    // Its squiggles go with it: no server is left to clear them.
                    let langs = SERVERS.iter().filter(|(_, c)| *c == cmd).flat_map(|(l, _)| l.iter());
                    let langs: Vec<&str> = langs.copied().collect();
                    let gone: Vec<PathBuf> = (self.lsp.problems.keys())
                        .filter(|p| {
                            let open = self.docs.iter().find(|d| d.path == **p).map(|d| d.lang);
                            langs.contains(&open.unwrap_or_else(|| crate::theme::language_id(p)))
                        })
                        .cloned()
                        .collect();
                    for path in gone {
                        self.lsp.problems.remove(&path);
                        self.show_diagnostics(&path, cx);
                    }
                    for doc in self.docs.iter().filter(|d| langs.contains(&d.lang)) {
                        doc.editor.update(cx, |e, cx| {
                            e.set_inlay_hints(Vec::new(), cx);
                            e.set_code_lenses(Vec::new(), cx);
                        });
                    }
                    let text = format!(
                        "The {} server crashed 5 times. The server will not be restarted.",
                        cmd[0]
                    );
                    self.note(text.into(), Severity::Error, cx);
                    return;
                }
                self.lsp.servers.remove(cmd[0]);
                for ix in 0..self.docs.len() {
                    if SERVERS
                        .iter()
                        .any(|(l, c)| c == &cmd && l.contains(&self.docs[ix].lang))
                    {
                        self.lsp_open(ix, cx);
                    }
                }
            }
            Some("window/showMessage") => {
                let text = msg["params"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let severity = match msg["params"]["type"].as_u64() {
                    Some(1) => Severity::Error,
                    Some(2) => Severity::Warning,
                    _ => Severity::Info,
                };
                self.note(text.into(), severity, cx);
            }
            Some("workspace/semanticTokens/refresh") => self.in_window(cx, |this, window, cx| {
                for doc in &this.docs {
                    (doc.editor.clone()).update(cx, |e, cx| e.refresh_semantic_tokens(window, cx));
                }
            }),
            Some("workspace/inlayHint/refresh" | "workspace/codeLens/refresh") => {
                self.lsp_decorate_all(cx)
            }
            _ => {}
        }
    }

    /// Puts `path`'s diagnostics on its editor as squiggles.
    /// Go to Next / Previous Problem in Files (F8 / Shift+F8): the next
    /// error, warning or info after the cursor, on into the next file with
    /// some and round again.
    // ponytail: moves the cursor (the squiggle's hover tells it); no peek widget.
    pub(super) fn next_problem(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let all: Vec<(&PathBuf, u32, u32)> = (self.lsp.problems.iter())
            .flat_map(|(path, ps)| {
                let mut ps: Vec<_> = ps.iter().filter(|p| p.severity <= 3).map(|p| (path, p.line, p.col)).collect();
                ps.sort();
                ps
            })
            .collect();
        let here = self.docs.get(self.active).map(|d| {
            let at = d.editor.read(cx).cursor_position();
            (d.path.clone(), at.line, at.character)
        });
        let after = |p: &(&PathBuf, u32, u32)| here.as_ref().is_none_or(|h| (p.0, p.1, p.2) > (&h.0, h.1, h.2));
        let before = |p: &(&PathBuf, u32, u32)| here.as_ref().is_none_or(|h| (p.0, p.1, p.2) < (&h.0, h.1, h.2));
        let pick = match forward {
            true => all.iter().find(|p| after(p)).or(all.first()),
            false => all.iter().rev().find(|p| before(p)).or(all.last()),
        };
        if let Some(&(path, line, col)) = pick {
            let at = input::Position::new(line, col);
            self.go_to(path.clone(), at, window, cx);
        }
    }

    pub(super) fn show_diagnostics(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.iter().find(|d| d.path == path) else {
            return;
        };
        let problems = self.lsp.problems.get(path).map_or(&[][..], Vec::as_slice);
        doc.editor.update(cx, |e, cx| {
            let text = e.text().clone();
            let Some(set) = e.diagnostics_mut() else {
                return;
            };
            set.reset(&text);
            for p in problems {
                let severity = match p.severity {
                    1 => DiagnosticSeverity::Error,
                    2 => DiagnosticSeverity::Warning,
                    3 => DiagnosticSeverity::Info,
                    _ => DiagnosticSeverity::Hint,
                };
                // ponytail: LSP columns count UTF-16 units; equal to chars
                // outside the astral planes.
                let range =
                    input::Position::new(p.line, p.col)..input::Position::new(p.end.0, p.end.1);
                set.push(
                    Diagnostic::new(range, p.message.clone())
                        .with_severity(severity)
                        .with_source(p.source.clone()),
                );
            }
            cx.notify();
        });
    }

    /// The status bar's error and warning counts.
    pub(super) fn problem_counts(&self) -> (usize, usize) {
        let all = self.lsp.problems.values().flatten();
        all.fold((0, 0), |(e, w), p| match p.severity {
            1 => (e + 1, w),
            2 => (e, w + 1),
            _ => (e, w),
        })
    }

    /// The Problems panel: each file's diagnostics under it, errors first.
    /// An explorer row's problem decoration: the color of its worst error
    /// or warning, and their count (a dot for a folder).
    pub(super) fn problem_deco(&self, path: &Path, dir: bool) -> Option<(Hsla, String)> {
        let found = (self.lsp.problems.iter())
            .filter(|(p, _)| match dir {
                true => p.starts_with(path),
                false => *p == path,
            })
            .flat_map(|(_, list)| list.iter().filter(|p| p.severity <= 2));
        let (worst, count) = found.fold((3, 0), |(w, n), p| (w.min(p.severity), n + 1));
        if count == 0 {
            return None;
        }
        // list.errorForeground / list.warningForeground.
        let color = c(if worst == 1 { 0xF88070 } else { 0xCCA700 });
        let badge = match dir {
            true => "\u{25CF}".to_string(),
            false if count > 9 => "9+".to_string(),
            false => count.to_string(),
        };
        Some((color, badge))
    }

    /// The Problems panel's Collapse All.
    pub(super) fn collapse_problems(&mut self, cx: &mut Context<Self>) {
        self.lsp.collapsed.extend(self.lsp.problems.keys().cloned());
        cx.notify();
    }

    pub(super) fn render_problems(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.lsp.problems.is_empty() {
            return div()
                .px(px(20.))
                .child("No problems have been detected in the workspace.")
                .into_any_element();
        }
        let filter = self.problems_filter.read(cx).value().to_string();
        let mut rows = Vec::new();
        for (fi, (path, list)) in self.lsp.problems.iter().enumerate() {
            let list: Vec<&Problem> = list.iter().filter(|p| problem_shown(&filter, path, p)).collect();
            if list.is_empty() {
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let dir = (path.parent())
                .map(|d| match &self.root {
                    Some(r) => d.strip_prefix(r).unwrap_or(d),
                    None => d,
                })
                .map(|d| d.display().to_string())
                .unwrap_or_default();
            let collapsed = self.lsp.collapsed.contains(path);
            let file = path.clone();
            rows.push(
                h_flex()
                    .id(("problem-file", fi))
                    .flex_none()
                    .h(px(22.))
                    .pl(px(8.))
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .hover(|s| s.bg(col::hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let set = &mut this.lsp.collapsed;
                        if !set.remove(&file) {
                            set.insert(file.clone());
                        }
                        cx.notify();
                    }))
                    .child(
                        codicon(match collapsed {
                            true => icon::CHEVRON_RIGHT,
                            false => icon::CHEVRON_DOWN,
                        })
                        .w(px(16.)),
                    )
                    .child(div().mr(px(6.)).child(file_icon(path)))
                    .child(name)
                    .child(
                        div()
                            .ml(px(6.))
                            .text_size(px(12.))
                            .text_color(col::fg_muted())
                            .child(dir),
                    )
                    .child(
                        div()
                            .ml(px(6.))
                            .px(px(5.))
                            .min_w(px(18.))
                            .rounded(px(9.))
                            .bg(c(0x616161))
                            .text_size(px(11.))
                            .line_height(px(18.))
                            .text_center()
                            .text_color(white())
                            .child(list.len().to_string()),
                    )
                    .into_any_element(),
            );
            if collapsed {
                continue;
            }
            let mut sorted = list;
            sorted.sort_by_key(|p| (p.severity, p.line, p.col));
            for (pi, p) in sorted.into_iter().enumerate() {
                let (glyph, color) = match p.severity {
                    1 => (icon::ERROR, c(0xF14C4C)),
                    2 => (icon::WARNING, c(0xCCA700)),
                    _ => (icon::INFO, c(0x3794FF)),
                };
                let (path, at) = (path.clone(), input::Position::new(p.line, p.col));
                rows.push(
                    h_flex()
                        .id(("problem", fi * 100_000 + pi))
                        .flex_none()
                        .h(px(22.))
                        .pl(px(46.))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .cursor_pointer()
                        .hover(|s| s.bg(col::hover()))
                        .child(codicon(glyph).text_color(color).mr(px(6.)))
                        .child(
                            div()
                                .flex_none()
                                .child(p.message.lines().next().unwrap_or_default().to_string()),
                        )
                        .when(!p.source.is_empty(), |d| {
                            d.child(
                                div()
                                    .ml(px(6.))
                                    .text_color(col::fg_muted())
                                    .child(p.source.clone()),
                            )
                        })
                        .child(div().ml(px(6.)).text_color(col::fg_muted()).child(format!(
                            "[Ln {}, Col {}]",
                            p.line + 1,
                            p.col + 1
                        )))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_path(path.clone(), window, cx);
                            if let Some(doc) = this.docs.get(this.active) {
                                reveal(&doc.editor.clone(), at, window, cx);
                            }
                        }))
                        .into_any_element(),
                );
                // relatedInformation: "file(line, col): message" rows under it,
                // as VS Code's.
                let related = p.raw["relatedInformation"].as_array().into_iter().flatten();
                for (ri, info) in related.enumerate() {
                    let loc = &info["location"];
                    let Some(path) = loc["uri"].as_str().and_then(uri_path) else {
                        continue;
                    };
                    let start = &loc["range"]["start"];
                    let at = input::Position::new(
                        start["line"].as_u64().unwrap_or(0) as u32,
                        start["character"].as_u64().unwrap_or(0) as u32,
                    );
                    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    let message = info["message"].as_str().unwrap_or_default();
                    rows.push(
                        h_flex()
                            .id(SharedString::from(format!("problem-related-{fi}-{pi}-{ri}")))
                            .flex_none()
                            .h(px(22.))
                            .pl(px(68.))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .cursor_pointer()
                            .hover(|s| s.bg(col::hover()))
                            .child(format!("{name}({}, {}): ", at.line + 1, at.character + 1))
                            .child(
                                div()
                                    .ml(px(4.))
                                    .text_color(col::fg_muted())
                                    .child(message.lines().next().unwrap_or_default().to_string()),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_path(path.clone(), window, cx);
                                if let Some(doc) = this.docs.get(this.active) {
                                    reveal(&doc.editor.clone(), at, window, cx);
                                }
                            }))
                            .into_any_element(),
                    );
                }
            }
        }
        if rows.is_empty() {
            return (div().px(px(20.)))
                .child("No results found with provided filter criteria.")
                .into_any_element();
        }
        v_flex()
            .id("problems")
            .size_full()
            .overflow_y_scroll()
            .children(rows)
            .into_any_element()
    }
}

/// Whether the Problems filter keeps `p`: its text found (in any case) in
/// the message, source or path, or with a leading `!` not found.
// ponytail: one term, no globs or comma lists as in VS Code's filter.
fn problem_shown(filter: &str, path: &Path, p: &Problem) -> bool {
    let filter = filter.trim();
    let (not, text) = match filter.strip_prefix('!') {
        Some(rest) => (true, rest.to_lowercase()),
        None => (false, filter.to_lowercase()),
    };
    if text.is_empty() {
        return true;
    }
    let found = [&p.message, &p.source, &path.to_string_lossy().into_owned()]
        .iter()
        .any(|s| s.to_lowercase().contains(&text));
    found != not
}

#[cfg(test)]
mod tests {
    use super::{Problem, Rope, Snippets, extensions::Snippet, path_uri, problem_shown, uri_path};
    use std::path::Path;

    #[test]
    fn problems_filter() {
        let p = Problem {
            line: 0,
            col: 0,
            end: (0, 0),
            severity: 2,
            message: "unused variable".into(),
            source: "rustc".into(),
            raw: Default::default(),
        };
        let path = Path::new("src/main.rs");
        assert!(problem_shown("", path, &p));
        assert!(problem_shown("UNUSED", path, &p));
        assert!(problem_shown("main.rs", path, &p));
        assert!(!problem_shown("clippy", path, &p));
        assert!(problem_shown("!clippy", path, &p));
        assert!(!problem_shown("!rustc", path, &p));
    }

    #[test]
    fn snippet_completions() {
        let snippets = Snippets {
            path: "a.rs".into(),
            lang: "rust",
            unit: "  ".into(),
            list: ["fn", "for", "if"]
                .map(|p| Snippet {
                    name: format!("{p} snippet"),
                    prefix: p.into(),
                    body: format!("{p} ${{1:x}} {{\n\t$0\n}}"),
                })
                .into(),
        };
        let text = Rope::from("ok\n    a.F");
        let items = snippets.items(&text, text.len());
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].label, "for");
        let Some(lsp_types::CompletionTextEdit::Edit(edit)) = &items[1].text_edit else {
            panic!("no edit");
        };
        // The word after the dot goes; the body takes the line's indent.
        assert_eq!(
            (edit.range.start.character, edit.range.end.character),
            (6, 7)
        );
        assert_eq!(edit.new_text, "for x {\n      \n    }");
        assert!(snippets.items(&text, 2).is_empty());
        assert!(snippets.items(&Rope::from("x."), 2).is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn converts_uris() {
        let path = Path::new(r"C:\a b\c#.zig");
        assert_eq!(path_uri(path), "file:///c%3A/a%20b/c%23.zig");
        assert_eq!(uri_path(&path_uri(path)).unwrap(), path);
        assert_eq!(
            uri_path("file:///c%3A/x/y.ts").unwrap(),
            Path::new(r"C:\x\y.ts")
        );
        assert_eq!(uri_path("untitled:Untitled-1"), None);
    }
}
