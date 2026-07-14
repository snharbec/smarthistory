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
- `DeleteWordBackward` now ships with two default bindings: the
  readline-style `Ctrl-W` **and** the macOS / GUI-editor-style
  `Alt-Backspace`. Both fire the same action, so users coming from
  either muscle memory get the expected behaviour without remapping.
  The action's `Action::default_keys()` API exposes the full list so
  the command palette, help overlay, and config printer can render
  both specs; either can be removed via `key.delete-word-backward=...`
  in the config file.
- The panes (`*`) prefix is now a properly-typed tree: every pane
  row carries a `[<label>]` chip showing the session / workspace it
  belongs to, and the filter is **group-aware**. Typing a token that
  matches a workspace label keeps the whole workspace (header + every
  child pane); typing a token that matches a pane's command / cwd
  keeps that pane and its parent workspace header. The new
  `HistoryRow::workspace_label` field carries the label from
  `fetch_session_panes_impl` to the renderer.

### Fixed

- Resolved `cargo fmt` drift in `src/ag.rs` and `src/files.rs`.
- Fixed `clippy::items_after_test_module` warning in `src/ag.rs`.

### Repository hygiene

- Expanded `.gitignore` to cover `.codegraph/`, `.pi-loop.json.lock`,
  generated `TAGS`, and local scratch files.

## 1.1.0

- Initial reviewed release.
