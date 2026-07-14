# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Security

- Harden shell command staging throughout the TUI by consistently using
  POSIX single-quote escaping (`util::shell_quote`) for user-provided paths,
  note text, `.command` script arguments, and multiplexer labels/session names.

### Changed

- Extracted the large `select_for_run_impl` staging method from `src/tui.rs`
  into a new `src/tui/actions.rs` module, shrinking `tui.rs` by ~2,500 lines.
- Moved `parse_bool` into `src/util.rs` and removed the duplicate copy in
  `src/tui.rs`, so the CLI and TUI session parser share one implementation.
- The symbols (`$`) prefix now supports an `@lang` token, mirroring the
  `ag` (`,`) prefix. `$MyStruct @rust` filters the result set to symbols
  defined in `.rs` files and pipes the per-row source-context preview
  through `bat --language <lang>` so the output preview pane shows
  syntax-highlighted code. The shared `parse_query_tokens` helper in the
  new `src/highlight.rs` module backs both modes (and any future content
  view that wants the same classification).

### Fixed

- Resolved `cargo fmt` drift in `src/ag.rs` and `src/files.rs`.
- Fixed `clippy::items_after_test_module` warning in `src/ag.rs`.

### Repository hygiene

- Expanded `.gitignore` to cover `.codegraph/`, `.pi-loop.json.lock`,
  generated `TAGS`, and local scratch files.

## 1.1.0

- Initial reviewed release.
