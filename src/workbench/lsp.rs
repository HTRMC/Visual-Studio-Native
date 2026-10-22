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
use gpui_kit::base::input::{Diagnostic, DiagnosticSeverity};
use input::{CodeActionProvider, CompletionProvider, DefinitionProvider, HoverProvider, Rope};
use lsp_types::{
    CodeAction, CodeActionOrCommand, CompletionContext, CompletionResponse,
    DocumentChangeOperation, DocumentChanges, GotoDefinitionResponse, Hover, LocationLink, OneOf,
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

/// The extension host's key among the servers.
const HOSTED: &[&str] = &["Extension Host"];

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
    /// How often each server has died on its own.
    crashes: HashMap<&'static str, u32>,
    /// The languages the extension host's providers serve.
    hosted: HashSet<String>,
}

struct Server {
    out: mpsc::Sender<String>,
    next_id: AtomicI64,
    /// Requests awaiting their response, by id.
    pending: Mutex<HashMap<i64, oneshot::Sender<Value>>>,
    /// The open documents' versions and the hash of the text last sent.
    open: Mutex<HashMap<PathBuf, (i64, u64)>>,
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

/// One editor's hover, completion, definition and code action requests, to
/// its server.
struct Provider {
    server: Weak<Server>,
    path: PathBuf,
    lang: &'static str,
    workbench: WeakEntity<Workbench>,
}

impl Provider {
    /// Sends `method` for the position at `offset` in `text`. The text goes
    /// first, as the editor asks before its Change event reaches
    /// [`Workbench::lsp_change`].
    fn ask(&self, method: &str, text: &Rope, offset: usize, cx: &mut App) -> Task<Value> {
        let Some(server) = self.server.upgrade() else {
            return Task::ready(Value::Null);
        };
        server.sync(&self.path, self.lang, &text.to_string());
        let params = json!({
            "textDocument": { "uri": path_uri(&self.path) },
            "position": text.offset_to_position(offset),
        });
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
        let reply = self.ask("textDocument/completion", text, offset, cx);
        cx.background_spawn(async move {
            let reply = reply.await;
            if reply.is_null() {
                return Ok(CompletionResponse::Array(vec![]));
            }
            Ok(serde_json::from_value(reply)?)
        })
    }

    // ponytail: typing a word character or one of `.:>` asks; the server's
    // own triggerCharacters are ignored.
    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        let mut chars = new_text.chars();
        matches!((chars.next(), chars.next()), (Some(c), None)
            if c.is_alphanumeric() || "_.:>".contains(c))
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
        // Called from inside the editor's own update; edit once that ends.
        if let Some(edit) = action.edit {
            let workbench = self.workbench.clone();
            window.defer(cx, move |window, cx| {
                (workbench.update(cx, |this, cx| this.apply_workspace_edit(edit, window, cx))).ok();
            });
        }
        if let (Some(command), Some(server)) = (action.command, self.server.upgrade()) {
            let params = json!({ "command": command.command, "arguments": command.arguments });
            _ = server.request("workspace/executeCommand", params);
        }
        Task::ready(Ok(()))
    }
}

/// Applies a server's `edits` to the editor's text as one undo step.
fn apply_edits(
    e: &mut EditorState,
    mut edits: Vec<lsp_types::TextEdit>,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) {
    edits.sort_by_key(|e| std::cmp::Reverse((e.range.start.line, e.range.start.character)));
    let mut text = e.text().clone();
    for edit in edits {
        let start = text.position_to_offset(&edit.range.start);
        text.replace(
            start..text.position_to_offset(&edit.range.end),
            &edit.new_text,
        );
    }
    let pos = e.cursor_position();
    e.replace_all(text.to_string(), window, cx);
    e.set_cursor_position(pos, window, cx);
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

/// `path` as a `file://` URI, percent-encoded as VS Code does (drive colon aside).
pub(super) fn path_uri(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from(if p.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~' | b':' => {
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
        let Some(doc) = self.docs.get(ix).filter(|d| !d.untitled && !d.binary) else {
            return;
        };
        let cmd = match self.lsp.hosted.contains(doc.lang) {
            true => HOSTED,
            false => match SERVERS.iter().find(|(langs, _)| langs.contains(&doc.lang)) {
                Some(&(_, cmd)) => cmd,
                None => return,
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
            lsp.code_action_providers = vec![provider];
            lsp.show_document = Some(show);
        });
    }

    /// Editor `ix` changed: its server gets the whole new text.
    pub(super) fn lsp_change(&mut self, ix: usize, cx: &mut Context<Self>) {
        let doc = &self.docs[ix];
        for server in self.lsp.servers.values().flatten() {
            if server.open.lock().unwrap().contains_key(&doc.path) {
                server.sync(&doc.path, doc.lang, &doc.editor.read(cx).value());
            }
        }
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
        cx.spawn_in(window, async move |_, cx| {
            let reply = reply.await.unwrap_or_default();
            let Ok(edits) = serde_json::from_value::<Vec<lsp_types::TextEdit>>(reply) else {
                return;
            };
            editor
                .update_in(cx, |e, window, cx| {
                    // Typed over while the server worked: its edits are stale.
                    if !edits.is_empty() && e.value() == before {
                        apply_edits(e, edits, window, cx);
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

    /// Sends `method` to the server that has `path` open.
    fn request_for(
        &self,
        path: &Path,
        method: &str,
        params: Value,
    ) -> Option<oneshot::Receiver<Value>> {
        (self.lsp.servers.values().flatten())
            .find(|s| s.open.lock().unwrap().contains_key(path))
            .map(|s| s.request(method, params))
    }

    /// Shift+F12: the symbol's references, a pick list grouped by file.
    // ponytail: a pick list stands in for VS Code's peek view and References
    // panel.
    pub(super) fn find_references(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let e = doc.editor.read(cx);
        let at = e.text().offset_to_position(e.selected_range().start);
        let params = json!({
            "textDocument": { "uri": path_uri(&doc.path) },
            "position": at,
            "context": { "includeDeclaration": true },
        });
        let Some(reply) = self.request_for(&doc.path.clone(), "textDocument/references", params)
        else {
            self.note("No references found".into(), Severity::Info, cx);
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let reply = reply.await.unwrap_or_default();
            let found = serde_json::from_value::<Option<Vec<lsp_types::Location>>>(reply);
            this.update_in(cx, |this, window, cx| {
                let mut found: Vec<_> = (found.ok().flatten().into_iter().flatten())
                    .filter_map(|l| Some((uri_path(l.uri.as_str())?, l.range.start)))
                    .collect();
                if found.is_empty() {
                    return this.note("No references found".into(), Severity::Info, cx);
                }
                found.sort_by_key(|(path, at)| (path.clone(), at.line, at.character));
                let (mut rows, mut groups) = (Vec::new(), Vec::new());
                let mut lines = Vec::new();
                for (ix, (path, at)) in found.iter().enumerate() {
                    if ix == 0 || found[ix - 1].0 != *path {
                        let open = this.docs.iter().find(|d| d.path == *path);
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
                    placeholder: "Go to reference",
                    current: 0,
                };
                this.open_items(items, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// The extension host now serves `ids`: their built-in servers make way
    /// and their files go to the host. `out` is the host's stdin.
    pub(super) fn lsp_languages(
        &mut self,
        ids: Vec<String>,
        out: mpsc::Sender<String>,
        cx: &mut Context<Self>,
    ) {
        self.lsp.servers.entry(HOSTED[0]).or_insert_with(|| {
            // Frames go out as the host's NDJSON `lsp` messages.
            let (tx, rx) = mpsc::channel::<String>();
            std::thread::spawn(move || {
                for frame in rx {
                    let body = frame.split_once("\r\n\r\n").map_or("", |(_, b)| b);
                    if out
                        .send(format!("{{\"m\":\"lsp\",\"msg\":{body}}}\n"))
                        .is_err()
                    {
                        return;
                    }
                }
            });
            Some(Arc::new(Server {
                out: tx,
                next_id: AtomicI64::new(1),
                pending: Mutex::default(),
                open: Mutex::default(),
            }))
        });
        let new: Vec<String> = (ids.into_iter())
            .filter(|id| self.lsp.hosted.insert(id.clone()))
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

    /// A message from the extension host's providers: a response, or
    /// diagnostics.
    pub(super) fn lsp_from_host(&mut self, msg: Value, cx: &mut Context<Self>) {
        let Some(id) = msg["id"].as_i64() else {
            return self.on_server(msg, cx);
        };
        if let Some(Some(server)) = self.lsp.servers.get(HOSTED[0])
            && let Some(tx) = server.pending.lock().unwrap().remove(&id)
        {
            tx.send(msg["result"].clone()).ok();
        }
    }

    /// The extension host is gone: its languages' files go back to the
    /// built-in servers until a new host serves them again.
    pub(super) fn lsp_host_gone(&mut self, cx: &mut Context<Self>) {
        self.lsp.servers.remove(HOSTED[0]);
        let langs = std::mem::take(&mut self.lsp.hosted);
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
                (id, None) if id == 0 => _ = ready.send(()),
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
                        "publishDiagnostics": {},
                        "hover": { "contentFormat": ["markdown", "plaintext"] },
                        "completion": { "completionItem": { "snippetSupport": false } },
                        "definition": { "linkSupport": true },
                        "formatting": {},
                        "rangeFormatting": {},
                        "rename": {},
                        "codeAction": { "codeActionLiteralSupport": { "codeActionKind": {
                            "valueSet": ["", "quickfix", "refactor", "refactor.extract",
                                "refactor.inline", "refactor.rewrite", "source",
                                "source.organizeImports"],
                        }}},
                    },
                    "workspace": { "configuration": true, "workspaceFolders": true },
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
            _ => {}
        }
    }

    /// Puts `path`'s diagnostics on its editor as squiggles.
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
    pub(super) fn render_problems(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.lsp.problems.is_empty() {
            return div()
                .px(px(20.))
                .child("No problems have been detected in the workspace.")
                .into_any_element();
        }
        let mut rows = Vec::new();
        for (fi, (path, list)) in self.lsp.problems.iter().enumerate() {
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
            rows.push(
                h_flex()
                    .id(("problem-file", fi))
                    .h(px(22.))
                    .pl(px(8.))
                    .whitespace_nowrap()
                    .child(codicon(icon::CHEVRON_DOWN).w(px(16.)))
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
            let mut sorted: Vec<&Problem> = list.iter().collect();
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
            }
        }
        v_flex()
            .id("problems")
            .size_full()
            .overflow_y_scroll()
            .children(rows)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{path_uri, uri_path};
    use std::path::Path;

    #[test]
    #[cfg(windows)]
    fn converts_uris() {
        let path = Path::new(r"C:\a b\c#.zig");
        assert_eq!(path_uri(path), "file:///C:/a%20b/c%23.zig");
        assert_eq!(uri_path(&path_uri(path)).unwrap(), path);
        assert_eq!(
            uri_path("file:///c%3A/x/y.ts").unwrap(),
            Path::new(r"C:\x\y.ts")
        );
        assert_eq!(uri_path("untitled:Untitled-1"), None);
    }
}
