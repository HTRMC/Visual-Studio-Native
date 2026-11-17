//! What webview pages get on every platform: the script VS Code's
//! webviews start with, and the extension files they load.

use std::path::{Path, PathBuf};

pub const PRELUDE: &str = r#"(() => {
  if (window.top !== window) return;
  const post = (m) => window.chrome.webview.postMessage(m);
  let acquired = false;
  let state = $STATE;
  const api = Object.freeze({
    postMessage: (m) => post(m),
    getState: () => state ?? undefined,
    setState: (s) => { state = s; post({ $vsnState: s ?? null }); return s },
  });
  window.acquireVsCodeApi = () => {
    if (acquired) throw new Error('An instance of the VS Code API has already been acquired');
    acquired = true;
    return api;
  };
  const theme = (t) => {
    if (!t) return;
    const apply = () => {
      const b = document.body;
      if (!b) return;
      b.classList.remove('vscode-light', 'vscode-dark', 'vscode-high-contrast', 'vscode-high-contrast-light');
      b.classList.add(t.kind);
      b.dataset.vscodeThemeKind = t.kind;
      b.dataset.vscodeThemeName = t.name || '';
      const root = document.documentElement.style;
      for (const [k, v] of Object.entries(t.vars || {})) root.setProperty(k, v);
    };
    if (document.body) apply(); else document.addEventListener('DOMContentLoaded', apply);
  };
  theme($THEME);
  window.chrome.webview.addEventListener('message', (e) => {
    if (e.data && e.data.__vsnTheme) return theme(e.data.__vsnTheme);
    window.dispatchEvent(new MessageEvent('message', { data: e.data, origin: location.origin }));
  });
})();"#;

/// VS Code's default webview styles.
const DEFAULT_STYLES: &str = "
html { scrollbar-color: var(--vscode-scrollbarSlider-background) var(--vscode-editor-background); }
body { background-color: transparent; color: var(--vscode-editor-foreground); font-family: var(--vscode-font-family);
  font-weight: var(--vscode-font-weight); font-size: var(--vscode-font-size); margin: 0; padding: 0 20px; }
img, video { max-width: 100%; max-height: 100%; }
a, a code { color: var(--vscode-textLink-foreground); }
a:hover { color: var(--vscode-textLink-activeForeground); }
code { font-family: var(--vscode-editor-font-family); color: var(--vscode-textPreformat-foreground);
  background-color: var(--vscode-textPreformat-background); padding: 1px 3px; border-radius: 4px; }
pre code { padding: 0; }
blockquote { background: var(--vscode-textBlockQuote-background); border-color: var(--vscode-textBlockQuote-border); }
::-webkit-scrollbar { width: 10px; height: 10px; }
::-webkit-scrollbar-corner { background-color: var(--vscode-editor-background); }
::-webkit-scrollbar-thumb { background-color: var(--vscode-scrollbarSlider-background); }
::-webkit-scrollbar-thumb:hover { background-color: var(--vscode-scrollbarSlider-hoverBackground); }
::-webkit-scrollbar-thumb:active { background-color: var(--vscode-scrollbarSlider-activeBackground); }
";

/// The extension's `html` as the page gets it: VS Code's default styles
/// first in its head, so the page's own win, and in its markup, ahead of
/// the page's CSP, which refuses a style added once it has taken effect.
pub fn page_html(html: &str) -> String {
    let style = format!("<style id=\"_defaultStyles\">{DEFAULT_STYLES}</style>");
    let lower = html.to_ascii_lowercase();
    // Just inside `<tag ...>`, not `<header>`'s.
    let inside = |tag: &str| {
        let at = (lower.match_indices(&format!("<{tag}")))
            .map(|(i, _)| i)
            .find(|&i| {
                lower[i + tag.len() + 1..]
                    .starts_with(|c: char| c == '>' || c.is_ascii_whitespace())
            })?;
        Some(at + lower[at..].find('>')? + 1)
    };
    // Ahead of everything but a doctype, which keeps the page out of quirks mode.
    let doctype = || {
        lower
            .trim_start()
            .starts_with("<!doctype")
            .then(|| lower.find('>').map(|i| i + 1))
            .flatten()
    };
    let at = inside("head")
        .or_else(|| inside("html"))
        .or_else(doctype)
        .unwrap_or(0);
    format!("{}{style}{}", &html[..at], &html[at..])
}

/// The file at `asWebviewUri` path `path` (`/c%3A/Users/...` on Windows,
/// `/home/...` elsewhere), if it's under one of `roots`: status, type, body.
pub fn read_resource(roots: &[PathBuf], path: &str) -> (u16, &'static str, Vec<u8>) {
    let decoded = decode(path);
    // A drive's path drops the slash before it.
    let path = match decoded.get(2..3) {
        Some(":") => PathBuf::from(&decoded[1..]),
        _ => PathBuf::from(&decoded),
    };
    let norm = |p: &Path| {
        let p = p.to_string_lossy().replace('\\', "/");
        if cfg!(windows) { p.to_lowercase() } else { p }
    };
    let file = norm(&path);
    let under =
        |root: &PathBuf| file.starts_with(&(norm(root).trim_end_matches('/').to_owned() + "/"));
    let climbs = path
        .components()
        .any(|c| c == std::path::Component::ParentDir);
    let allowed = !climbs && roots.iter().any(under);
    match allowed.then(|| std::fs::read(&path).ok()).flatten() {
        Some(body) => (200, mime(&path), body),
        None => (404, "text/plain", Vec::new()),
    }
}

/// `#rrggbb`'s channels.
pub fn parse_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let hex = hex.strip_prefix('#')?;
    let at = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some((at(0)?, at(2)?, at(4)?))
}

/// Undoes a URI path's percent-encoding.
pub fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| s.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(b) => (out.push(b), i += 3),
            None => (out.push(bytes[i]), i += 1),
        };
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "xml" => "application/xml",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Whether a key the page has focus for stays the page's: clipboard, undo,
/// select all and find, as VS Code leaves them to its webviews. With `find`
/// (`enableFindWidget`), Ctrl+F is vsn's.
pub fn page_owns(ctrl: bool, alt: bool, shift: bool, key: &str, find: bool) -> bool {
    ctrl && !alt
        && match key {
            "f" => !shift && !find,
            "c" | "v" | "x" | "z" | "y" | "a" => !shift || key == "z",
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_resource_paths() {
        assert_eq!(decode("c%3A/Users/a%20b/x.js"), "c:/Users/a b/x.js");
        assert_eq!(decode("100%"), "100%");
    }

    #[test]
    fn default_styles_go_first_in_the_head() {
        let style = |h: &str| page_html(h).find("<style id=\"_defaultStyles\">");
        let page = "<!DOCTYPE html><html><head><meta http-equiv=\"Content-Security-Policy\"></head></html>";
        assert_eq!(style(page), Some("<!DOCTYPE html><html><head>".len()));
        let header = "<!doctype html><HTML lang=en><body><header>x</header></body></HTML>";
        assert_eq!(style(header), Some("<!doctype html><HTML lang=en>".len()));
        assert_eq!(style("<!DOCTYPE html><p>hi"), Some("<!DOCTYPE html>".len()));
        assert_eq!(style("<p>hi"), Some(0));
    }

    #[test]
    fn serves_only_files_under_roots() {
        let dir = std::env::temp_dir().join("vsn-webview-page-test");
        std::fs::create_dir_all(dir.join("in")).unwrap();
        std::fs::write(dir.join("in/a.js"), "x").unwrap();
        std::fs::write(dir.join("out.js"), "y").unwrap();
        // As asWebviewUri gives them: `/c:/...` on Windows, `/home/...` elsewhere.
        let uri = |p: PathBuf| {
            let s = p.to_string_lossy().replace('\\', "/");
            if s.starts_with('/') {
                s
            } else {
                format!("/{s}")
            }
        };
        let roots = [dir.join("in")];
        assert_eq!(read_resource(&roots, &uri(dir.join("in/a.js"))).0, 200);
        assert_eq!(read_resource(&roots, &uri(dir.join("out.js"))).0, 404);
        assert_eq!(read_resource(&roots, &uri(dir.join("in/../out.js"))).0, 404);
    }
}
