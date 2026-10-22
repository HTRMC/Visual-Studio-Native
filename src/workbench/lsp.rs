//! EXTENSIONS.md phase 4's native LSP client: one language server per
//! language, started from PATH when a file of that language opens, kept in
//! sync with full-text changes. Its diagnostics show as squiggles, in the
//! Problems panel and in the status bar's counts.

use std::{
    collections::{BTreeMap, HashMap},
    io::{BufRead as _, BufReader, Read, Write as _},
    process::{Command, Stdio},
    sync::mpsc,
};

use gpui_kit::base::input::{Diagnostic, DiagnosticSeverity};
use serde_json::{Value, json};

use super::*;

/// The server each language id starts.
// ponytail: a fixed table of servers on PATH; language extensions bring
// their own servers once the JS host's vscode-languageclient reaches here.
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

/// One diagnostic as a server published it, 0-based.
pub(super) struct Problem {
    line: u32,
    col: u32,
    end: (u32, u32),
    /// LSP's: 1 error, 2 warning, 3 information, 4 hint.
    severity: u64,
    message: String,
    source: String,
}

#[derive(Default)]
pub(super) struct Lsp {
    /// By the server's command; None once it failed to start.
    servers: HashMap<&'static str, Option<Server>>,
    /// Each file's diagnostics, as its server last published them.
    problems: BTreeMap<PathBuf, Vec<Problem>>,
}

struct Server {
    out: mpsc::Sender<String>,
    /// The open documents' versions.
    // ponytail: closing an editor sends no didClose; the server keeps its
    // last text, which the next didOpen-as-didChange replaces.
    open: HashMap<PathBuf, i64>,
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
    /// server on the first file of its language.
    pub(super) fn lsp_open(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(ix).filter(|d| !d.untitled && !d.binary) else {
            return;
        };
        let Some(&(_, cmd)) = SERVERS.iter().find(|(langs, _)| langs.contains(&doc.lang)) else {
            return;
        };
        let (path, lang, text) = (doc.path.clone(), doc.lang, doc.editor.read(cx).value());
        if !self.lsp.servers.contains_key(cmd[0]) {
            let server = self.start_server(cmd, cx);
            self.lsp.servers.insert(cmd[0], server);
        }
        let Some(Some(server)) = self.lsp.servers.get_mut(cmd[0]) else {
            return;
        };
        let uri = path_uri(&path);
        match server.open.get_mut(&path) {
            Some(version) => {
                *version += 1;
                notify(
                    &server.out,
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri, "version": *version },
                        "contentChanges": [{ "text": text.as_ref() }],
                    }),
                );
            }
            None => {
                server.open.insert(path, 1);
                notify(
                    &server.out,
                    "textDocument/didOpen",
                    json!({ "textDocument": {
                        "uri": uri, "languageId": lang, "version": 1, "text": text.as_ref(),
                    }}),
                );
            }
        }
    }

    /// Editor `ix` changed: its server gets the whole new text.
    // ponytail: full sync per keystroke; incremental changes if big files lag.
    pub(super) fn lsp_change(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self
            .lsp
            .servers
            .values()
            .flatten()
            .any(|s| s.open.contains_key(&self.docs[ix].path))
        {
            self.lsp_open(ix, cx);
        }
    }

    pub(super) fn lsp_saved(&mut self, path: &Path) {
        for server in self.lsp.servers.values().flatten() {
            if server.open.contains_key(path) {
                let uri = path_uri(path);
                notify(
                    &server.out,
                    "textDocument/didSave",
                    json!({ "textDocument": { "uri": uri } }),
                );
            }
        }
    }

    /// A new workspace: servers start over in it.
    pub(super) fn lsp_reset(&mut self) {
        self.lsp = Lsp::default();
    }

    fn start_server(&mut self, cmd: &[&str], cx: &mut Context<Self>) -> Option<Server> {
        let mut command = Command::new(cmd[0]);
        command
            .args(&cmd[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // ponytail: server logs are dropped; the Output panel shows them
            // once it lists channels.
            .stderr(Stdio::null());
        if let Some(root) = &self.root {
            command.current_dir(root);
        }
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000); // CREATE_NO_WINDOW
        // Not installed: the language just goes without.
        let mut child = command.spawn().ok()?;
        let (mut stdin, stdout) = (child.stdin.take()?, child.stdout.take()?);
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
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Value>();
        let reply = out.clone();
        std::thread::spawn(move || {
            read_frames(stdout, |msg| match (&msg["id"], msg["method"].as_str()) {
                (Value::Null, Some(_)) => _ = tx.unbounded_send(msg),
                (id, None) if id == 0 => _ = ready.send(()),
                (id, Some(method)) => {
                    // Requests from the server get an empty answer; settings
                    // are null per item, so servers use their defaults.
                    let items = msg["params"]["items"].as_array().map_or(0, Vec::len);
                    let result = match method {
                        "workspace/configuration" => json!(vec![Value::Null; items]),
                        _ => Value::Null,
                    };
                    send(
                        &reply,
                        json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    );
                }
                _ => {}
            });
            child.kill().ok();
            child.wait().ok();
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
                    },
                    "workspace": { "configuration": true, "workspaceFolders": true },
                },
            }}),
        );
        notify(&out, "initialized", json!({}));
        Some(Server {
            out,
            open: HashMap::new(),
        })
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
