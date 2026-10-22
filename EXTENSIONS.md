# Extensions

How vsn runs VS Code extensions (JavaScript, for compatibility) and its own
native plugins (WebAssembly, for speed and sandboxing) on one shared core.

## Decisions

| Question | Decision | Why |
|---|---|---|
| Manifest format | VS Code's `package.json`, for both kinds | Existing extensions work unchanged; one manager, one Extensions view |
| JS runtime | Bundled with vsn, **Bun preferred**, Node as the fallback (see [Bun or Node](#bun-or-node)) | No "install Node first"; Bun starts faster and is one exe |
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
(the unpacked `extension/` folder of the .vsix), plus `extensions.json`
listing what's installed and disabled.

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
2. **Core API and command registry.** Extension commands in the palette,
   menus and keybindings; activation events (`onCommand`, `onLanguage`,
   `workspaceContains`, `onStartupFinished`, `*`).
3. **JS host.** The bundled runtime and the `vscode` shim; a simple Open VSX
   extension that registers commands, shows messages and adds a status bar
   item works unchanged. The Bun vs Node comparison happens here.
4. **LSP client** into the provider registries; `vscode-languageclient`-based
   extensions then work through the JS host.
5. **WASM host.** wasmtime, a WIT world generated from the core API, and a
   sample plugin in Rust.

## Bun or Node

Both run the same host code, which uses plain Node APIs only. Bun is the
default if it passes this suite on the bundled build; otherwise Node.

- `child_process` spawning with stdio pipes (language servers)
- `net` sockets and named pipes (some LSP/DAP transports)
- `worker_threads`, `fs.watch`, `process.on('exit')` cleanup
- N-API `.node` addons (some extensions ship them)
- Activating the 20 most-downloaded JS extensions on Open VSX
- Startup to first activation, idle memory, installer size added

## Dependencies

| Need | Plan | New? |
|---|---|---|
| Unzip .vsix | A small reader over `flate2` (already built in through `png`) | Direct dependency only |
| HTTP (Open VSX) | gpui has the `http_client` interface but no implementation built in | Yes: a client (`ureq` is the small option) |
| WASM | `wasmtime` | Yes, large; phase 5 |
| JS runtime | Bun or Node exe in the installer | Bundled, not a crate |
