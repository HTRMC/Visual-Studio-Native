# Extensions

How vsn runs VS Code extensions (JavaScript, for compatibility) and its own
native plugins (WebAssembly, for speed and sandboxing) on one shared core.

## Goal

A VS Code user can move to vsn without losing their extensions, and vsn
offers something VS Code can't: plugins that are fast and safe by design.

- **Compatibility.** An extension from Open VSX installs and works
  unchanged, whether it's a theme or snippet pack (no code), a language
  extension (a language server behind a thin JS wrapper) or a plain JS
  extension (commands, messages, status bar, configuration). The bar is
  the 20 most-downloaded JS extensions on Open VSX activating without
  errors.
- **Native plugins.** A plugin written against vsn's own API in Rust, or
  any language that compiles to WebAssembly, runs sandboxed. It has no file
  or network access unless granted, and runs under CPU and memory limits.
  It keeps working across vsn updates without a rebuild.
- **The editor stays native.** No extension, JS or WASM, can freeze the UI
  or crash vsn. The JS host is a separate process that restarts on its own,
  and WASM calls are metered. With no extensions installed, startup costs
  nothing extra.
- **One system.** Both kinds install, update, disable and show in the same
  Extensions view, from the same `package.json` manifest, and reach the
  editor through the same core API.

Not goals: the Microsoft Marketplace, Microsoft's VS Code-only extensions,
webviews, and every corner of the `vscode` API. Coverage grows as real
extensions need it.

## Decisions

| Question | Decision | Why |
|---|---|---|
| Manifest format | VS Code's `package.json`, for both kinds | Existing extensions work unchanged; one manager, one Extensions view |
| JS runtime | Bundled with vsn, **Node preferred**, Bun also works (see [Bun or Node](#bun-or-node)) | No "install Node first"; Node measured faster and smaller here |
| Native plugins | **WebAssembly** (wasmtime, WIT interface) | Sandboxed, any source language, stable ABI across vsn versions |
| Native DLL plugins | **No** | No stable Rust ABI, can't be sandboxed, a bad one crashes the editor |
| Gallery | **Open VSX** (open-vsx.org) and local `.vsix` files | The Microsoft Marketplace's terms allow only Microsoft's products |
| Webviews | Out of scope | Needs a browser engine |

Microsoft's own extensions (Pylance, C/C++, Remote-*, Live Share) are
licensed for VS Code only and aren't on Open VSX; their open counterparts
(basedpyright, clangd, ...) are the targets instead.

## Architecture

```
                  ┌──────────────────── vsn (Rust) ────────────────────┐
  package.json ─► │ Extension manager                                  │
  (.vsix / dir)   │   install · enable/disable · activation events     │
                  │   contribution points (loaded natively, no code)   │
                  │                        │                           │
                  │ Core extension API (Rust)                          │
                  │   commands · providers · status bar · messages ·   │
                  │   configuration · output channels · workspace fs   │
                  │   ▲ also fed by the native LSP client              │
                  └───┬────────────────────────────────┬───────────────┘
                      │ WIT bindings                   │ JSON-RPC over stdio
              ┌───────┴────────┐              ┌────────┴─────────────────┐
              │ WASM host      │              │ JS extension host        │
              │ wasmtime, one  │              │ bundled Bun/Node process │
              │ store per      │              │ running every JS         │
              │ plugin, fuel + │              │ extension, with a        │
              │ memory limits  │              │ `vscode` module shim     │
              └────────────────┘              └──────────────────────────┘
```

- **Contributions are data.** `contributes.*` is read by the manager whatever
  the runtime, so a theme or snippet pack runs no code at all.
- **One core API, two bindings.** `vscode.languages.registerHoverProvider`
  (JS, over RPC) and `register-hover-provider` (WIT) land in the same Rust
  registry, and the LSP client feeds it too, so the editor has one code path
  per feature.
- **The JS host mirrors VS Code's design.** VS Code already runs extensions in a
  separate process talking to the UI over RPC (ExtHost / MainThread). Ours
  is one process for all JS extensions; a crash restarts it, never the
  editor.
- **Which runtime:** `main` → JS host; `"vsn": { "wasm": "plugin.wasm" }`
  → WASM host. A manifest can have neither (contributions only).

## Layout on disk

VS Code's: `%USERPROFILE%\.vsn\extensions\<publisher>.<name>-<version>\`
(the unpacked `extension/` folder of the .vsix), plus `.disabled` listing
the disabled ids, one per line. What's installed is whatever folders are
there.

## Contribution points

| Point | Phase | Notes |
|---|---|---|
| `themes` (color) | 1 | Map VS Code theme keys onto the `col::` palette |
| `snippets` | 1 | Needs snippet insertion in the editor |
| `iconThemes` | 1 | The Seti one is built in already, in that format |
| `languages` (ids, extensions, `configuration`) | 1 | Comment, bracket and indent rules |
| `commands`, `menus`, `keybindings` | 2 | Palette, menus and keys show extension commands |
| `configuration` | 2 | Defaults and the settings schema |
| `grammars` (TextMate) | later | Needs a TextMate engine (new dependency); tree-sitter covers built-ins |
| `views`, `viewsContainers` | 3+ | Tree views from `TreeDataProvider` |
| `debuggers` | 3+ | The DAP client exists; adapters come from extensions |

## API surface (JS `vscode` / WIT)

| Namespace | Phase | First members |
|---|---|---|
| `commands` | 2–3 | `registerCommand`, `executeCommand` |
| `window` | 3 | `showInformation/Warning/ErrorMessage`, `createOutputChannel`, `createStatusBarItem`, `showQuickPick`, `showInputBox`, `activeTextEditor` |
| `workspace` | 3 | `getConfiguration`, `workspaceFolders`, `fs`, `onDidChangeConfiguration`, `openTextDocument` |
| `languages` | 4 | completion, hover, definition, diagnostics collections, formatting |
| `env`, `Uri`, `EventEmitter`, `Disposable` | 3 | Plain JS in the shim |
| `debug`, `tasks`, `scm`, `notebooks`, webviews | later / never | |

## Phases

1. **Manager and contributions.** Install from a .vsix or Open VSX,
   uninstall and disable; a real Extensions view (search, details, Installed);
   color themes, snippets and language configurations applied.
   *Done:* Install from VSIX..., Installed with search, Disable and
   Uninstall; Preferences: Color Theme (Ctrl+K Ctrl+T) applies and saves
   `workbench.colorTheme`; Snippets: Insert Snippet; languages' ids, file
   extensions and comments. *Left:* Open VSX (needs an HTTP client), the
   details page, icon themes, snippet tab stops after the first and
   completion by prefix, brackets and indent rules.
2. **Core API and command registry.** Extension commands in the palette,
   menus and keybindings; activation events (`onCommand`, `onLanguage`,
   `workspaceContains`, `onStartupFinished`, `*`).
   *Done:* contributed commands in the palette as "Category: Title"
   (`%nls%` titles localized, `commandPalette` `when: false` hidden),
   contributed keybindings, configuration defaults; every activation event
   above, plus commands contributing `onCommand` implicitly. *Left:* `menus`,
   `when` clauses, globs in `workspaceContains`, keybindings of a newly
   installed extension before a restart.
3. **JS host.** The bundled runtime and the `vscode` shim; a simple Open VSX
   extension that registers commands, shows messages and adds a status bar
   item works unchanged. The Bun vs Node comparison happens here.
   *Done:* `src/workbench/exthost.js`, one process for all JS extensions,
   NDJSON over stdio; `commands`, `window` messages and status bar items
   (codicons, tooltip, click), `workspace.getConfiguration`/`fs`/folders,
   `extensions`, `env`, `Uri`, `EventEmitter`, `Disposable`, `Range` and
   friends; unknown API members are stubs that log "not implemented"
   instead of throwing. Install, uninstall, enable and disable restart the
   host; a crash restarts it up to 3 times. `require('vscode')` works by
   hooking `Module._resolveFilename` (Bun ignores `Module._load`) to a
   `vscode.js` written next to the host. Output channels and the host's console show in the Output panel.
   Text documents (`workspace.textDocuments` and their events), diagnostic
   collections, `TextDocument`, `CodeActionKind` and the other provider
   types. Stubs chain, iterate and accept assignments, so unsupported API
   doesn't break activation. *Left:*
   quick pick, input box and message buttons, VS Code's built-in
   commands, persistent mementos, `activeTextEditor`, a
   bundled runtime in the installer (today: `runtime\` next to vsn.exe, then
   PATH).
4. **LSP client** into the provider registries; `vscode-languageclient`-based
   extensions then work through the JS host.
   *Done:* `src/workbench/lsp.rs`, a client over stdio with no new
   dependency: one server per language from a built-in table on PATH
   (rust-analyzer, clangd, gopls, zls, basedpyright, typescript-language-server),
   full-text sync (didOpen, didChange, didSave, didClose), `publishDiagnostics` as squiggles, the Problems panel
   (click to jump) and the status bar counts, `window/showMessage` as
   toasts; logs in the Output panel; a crashed server restarts, up to 5 times; Format Document; hover, completion and Go to Definition through gpui-component's
   editor providers (`lsp-types` and `anyhow` became direct dependencies for
   their types). Servers from language extensions: `vscode-languageclient`
   runs in the JS host, which registers its providers for a language and
   then stands in as that language's server (the built-in one steps aside),
   so the Zig extension's own zls serves hover, completion, definition,
   formatting and diagnostics. *Left:* range formatting, rename, references,
   signature help and code actions, on both paths.
5. **WASM host.** wasmtime, a WIT world generated from the core API, and a
   sample plugin in Rust.

## Bun or Node

Both run the same host code, which uses plain Node APIs only. Measured with
the phase 3 test extension (Windows 11, Node 24.14, Bun 1.3.13):

| | Node | Bun |
|---|---|---|
| Spawn to first output | ~21–25 ms | ~620–650 ms |
| Idle working set | 53 MB | 132 MB |
| Phase 3 extension | works | works |

So Node is preferred; `VSN_JS_RUNTIME` picks another runtime. Bun quirks
met on the way: it ignores `Module._load` hooks, and when the main module's
exports have `fetch` it starts `Bun.serve` on its own, so the `vscode` shim
can't be a Proxy exported from the main module. Still to run on both:

- `child_process` spawning with stdio pipes (language servers)
- `net` sockets and named pipes (some LSP/DAP transports)
- `worker_threads`, `fs.watch`, `process.on('exit')` cleanup
- N-API `.node` addons (some extensions ship them)
- Activating the 20 most-downloaded JS extensions on Open VSX
- Startup to first activation, idle memory, installer size added

## Dependencies

| Need | Plan | New? |
|---|---|---|
| Unzip .vsix | Windows' own `tar.exe` (bsdtar reads zip) | No |
| HTTP (Open VSX) | gpui has the `http_client` interface but no implementation built in | Yes: a client (`ureq` is the small option) |
| WASM | `wasmtime` | Yes, large; phase 5 |
| JS runtime | Bun or Node exe in the installer | Bundled, not a crate |
