# Vendored crates

Copies of crates.io crates with fixes vsn needs, wired in through
`[patch.crates-io]` in Cargo.toml. Each change is marked with a `vsn:`
comment; on a gpui-kit update, copy the new version over and re-apply them.

- **gpui-pre-windows 0.3.9** (`src/direct_write.rs`): tab characters advance
  to the next tab stop (every 4 spaces) instead of rendering zero-width.
- **gpui-pre-windows 0.3.9** (`src/direct_write.rs`): a line's ascent and
  descent come from its own font, so a taller fallback glyph doesn't move the
  baseline.
- **gpui-base 0.8.0** (`src/scrollbar.rs`): a scrollbar thumb drag redraws on
  every mouse move on Windows instead of throttling through a coarse timer.
- **gpui-base 0.8.0** (`src/input/editor/display_map/`, `src/input/base/`):
  big files open fast. The line index is built once, at the first paint, and
  without soft wrap from line lengths alone (`memchr`, a new dependency); a
  font change only rebuilds it with wrap on; edits pass the inserted length
  rather than a Rope of the text; a first value builds its rope in bulk; and
  `install_tokens` compares texts only when there are tokens.
