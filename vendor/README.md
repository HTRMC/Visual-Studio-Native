# Vendored crates

Copies of crates.io crates with fixes vsn needs, wired in through
`[patch.crates-io]` in Cargo.toml. Each change is marked with a `vsn:`
comment; on a gpui-kit update, copy the new version over and re-apply them.

- **gpui-pre-windows 0.3.9** (`src/direct_write.rs`): tab characters advance
  to the next tab stop (every 4 spaces) instead of rendering zero-width.
