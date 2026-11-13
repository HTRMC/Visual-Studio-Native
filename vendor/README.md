# Vendored crates

Copies of crates.io crates with fixes vsn needs, wired in through
`[patch.crates-io]` in Cargo.toml. Each change is marked with a `vsn:`
comment; on a gpui-kit update, copy the new version over and re-apply them.

- **gpui-pre-windows 0.3.9** (`src/direct_write.rs`): tab characters advance
  to the next tab stop (every 4 spaces) instead of rendering zero-width.
- **gpui-pre-windows 0.3.9** (`src/direct_write.rs`): a line's ascent and
  descent come from its own font, so a taller fallback glyph doesn't move the
  baseline.
- **gpui-pre-windows 0.3.9** (`src/directx_renderer.rs`, `src/gpui_windows.rs`):
  each window's DirectComposition root holds a webview layer under the swap
  chain (`webview_layer`), and quads of exactly `WEBVIEW_HOLE` are cleared to
  transparent, so a WebView2 page in that layer shows through gpui's drawing.
- **gpui-pre-windows 0.3.9** (`src/events.rs`): `set_cursor_override` shows a
  webview's own cursor (such as the grab hand) instead of gpui's; a click
  takes keyboard focus back from a child window (WebView2's focus window).
- **gpui-pre-windows 0.3.9** (`src/events.rs`, `src/window.rs`): `set_zoom`
  multiplies every DPI-derived scale factor (window.zoomLevel) and re-lays
  the window out at the new scale.
- **gpui-base 0.8.0** (`src/scrollbar.rs`): a scrollbar thumb drag redraws on
  every mouse move on Windows instead of throttling through a coarse timer.
- **gpui-base 0.8.0** (`src/input/editor/display_map/`, `src/input/base/`):
  big files open fast. The line index is built once, at the first paint, and
  without soft wrap from line lengths alone (`memchr`, a new dependency); a
  font change only rebuilds it with wrap on; edits pass the inserted length
  rather than a Rope of the text; a first value builds its rope in bulk; and
  `install_tokens` compares texts only when there are tokens.
- **gpui-base 0.8.0** (`src/input/editor/inlays.rs`,
  `src/input/editor/display_map/block_rows.rs`, `src/input/base/element.rs`):
  inlay hints and CodeLens. `set_inlay_hints` paints hints at byte offsets as
  muted chips that take space but aren't text; `set_code_lenses` puts a row
  above a line that the cursor can't reach. Both follow edits until replaced,
  and clicking a hint part or lens item emits `InlayClicked`/`CodeLensClicked`.
  Cursor movement, indent guides and line numbers place each line below its
  lens row.
- **gpui-base 0.8.0** (`src/input/editor/lsp/overlay.rs`,
  `src/input/editor/lsp/completions.rs`): an accepted completion applies its
  `additionalTextEdits` (an auto-import's `use` line) with the main edit as
  one undo step, then calls the provider's new `completion_accepted` hook,
  which runs the item's command.
- **gpui-base 0.8.0** (`src/input/base/state.rs`,
  `src/input/editor/display_map/display_map.rs`): `fold_at_cursor` and
  `fold_all` / `set_folds` for VS Code's Fold / Unfold / Fold All / Fold
  Recursively / Fold Level commands, and
  `fold_candidates` on the display map; `gutter_width`, `editor_style` and
  `syntax_styles` for sticky scroll and the minimap, drawn beside the editor;
  `selected_ranges` so line commands act on every cursor.
- **gpui-base 0.8.0** (`src/input/editor/lsp/semantic_tokens.rs`):
  `refresh_semantic_tokens` asks the provider again, for a server's
  `workspace/semanticTokens/refresh`.
- **gpui-base 0.8.0** (`src/input/editor/language_config.rs`,
  `src/input/base/state.rs`, `src/input/mod.rs`): `LanguageConfig` carries
  VS Code's `onEnterRules` (`OnEnterRule`, `IndentAction`); Enter with no
  selection in a code editor applies the first matching rule before the
  plain indent (so `///` continues as `/// `).
