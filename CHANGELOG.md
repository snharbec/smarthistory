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
- New TUI action `Action::DownloadJiraIssue` (default key
  `Ctrl-M-s`) downloads the selected JIRA issue as a local markdown
  note by staging `note_search jira-issue <KEY>`. The action is
  mode-gated to the JIRA search mode (`-...`); outside of JIRA mode
  it's a no-op with a status message so the user understands why
  their key did nothing. The bare command line is staged (no path,
  no flags) so `note_search` writes the markdown into the
  `notes.dir` configured in the same config file.
- The status bar (the footer line at the bottom of the TUI) no
  longer surfaces the two delete actions in its key-binding hints.
  The `del` and `del all` chips have been replaced with a `palette`
  chip showing the current `CommandAction` binding (default `:`).
  The delete actions are still discoverable via the help overlay
  (`Ctrl-H`) and the command palette itself, which lists every
  action with its current binding.
- The JIRA search-as-you-type now has two additional
  trigger paths alongside the existing 400ms fast debounce:
  1. **Space trigger** — typing a
     space inside the JIRA
     query body fires the
     search immediately,
     bypassing the debounce.
     This matches IDE
     autocomplete
     conventions (a space
     commits the current
     token to a search).
  2. **3-second idle safety-
     net timer** — a new
     `jira_idle_started`
     field fires the search
     after 3 seconds of no
     keystroke activity,
     independent of the
     400ms debounce. The
     user reported that the
     query "sometimes isn't
     executed"; the idle
     timer guarantees the
     search runs within 3
     seconds of the last
     keystroke regardless
     of whether the fast
     debounce ever elapses
     (e.g. the user keeps
     typing slowly, or the
     run loop is temporarily
     blocked on background
     work). The two
     timers are armed in
     lock-step by
     `jira_touch`; either
     can fire the search
     when its respective
     window elapses.
- The JIRA search mode now
  supports **tab-completion
  of JQL field names**.
  Inside `-` mode, pressing
  `Tab` expands the
  field-name prefix
  immediately before the
  cursor:
  - `proj<TAB>` → `project=`
    (single match; cursor
    lands right after the
    `=`)
  - `lab<TAB>` → `label`
    (multiple matches —
    `label` and `labels`;
    extends to the longest
    common prefix with no
    `=`, then a second
    Tab on `labels<TAB>`
    → `labels=`)
  - `xyz<TAB>` → no-op +
    status message
    (no match; query
    unchanged)
  The completion list is
  the standard JQL system
  field set (`assignee`,
  `reporter`, `status`,
  `priority`, `labels`,
  `summary`, …) plus a few
  common custom-field
  conventions (`sprint`,
  `epic`, `parent`,
  `storyPoints`, `rank`).
  Outside of JIRA mode
  `Tab` is a no-op, so the
  key doesn't interfere
  with any other mode. The
  action is the new
  `Action::JiraFieldComplete`
  (default key `Tab`); the
  core completion logic
  lives in
  `crate::jira::jira_field_complete`
  / `jira_field_complete_with_value`,
  both unit-tested.
- The search now fires
  immediately on every
  text-mutating action in
  every mode except JIRA.
  The user reported that
  the JIRA search
  "sometimes isn't
  executed" (which we
  fixed with the 400ms
  debounce / 3s idle
  safety-net / space
  trigger) and the
  corresponding complaint
  for the in-process
  search modes is "the
  list lags my typing".
  The new
  `App::trigger_text_change_search`
  helper is called from
  `push_char`,
  `backspace`,
  `delete_word_backward`,
  `clear_query`, and the
  JIRA tab-completion
  path. Behaviour by mode:
  - **Synchronous modes**
    (SESS, DIR, GLOBAL,
    STATS, panes `*`,
    directories `#`,
    symbols `$`, todos
    `!`, notes `@`,
    tags `$`, ag `,`,
    files `~`): the
    helper calls
    `self.refresh()`
    directly, so the
    row set is
    re-fetched on the
    same frame as
    the keystroke.
    The SQL fetch is
    a constant-time
    operation, so
    there's no
    frame-budget
    concern.
  - **LLM (`=`)**: the
    helper bypasses
    the 1s LLM
    debounce by
    temporarily
    setting
    `llm_debounce_started`
    to a past value
    and calling
    `llm_maybe_autocall()`.
    The user has
    typed a
    description; they
    want a preview
    now, not after
    1s of typing
    latency. (The
    `llm_in_flight`
    short-circuit
    still prevents
    duplicate
    concurrent
    LLM calls.)
  - **JIRA (`-`)**: the
    helper is a
    no-op for JIRA
    mode. The JIRA-
    specific
    debounce/idle
    /space-trigger
    paths remain in
    effect; mixing
    in a per-
    keystroke fire
    would defeat the
    debounce and
    re-introduce the
    JIRA-server spam
    the debounce was
    designed to
    prevent.
  - **Empty queries**
    (just-cleared
    box): the
    helper short-
    circuits before
    reaching the
    fetch path, so
    we don't waste
    time re-running
    the same all-
    rows query the
    user just had
    on screen.

### Fixed

- Resolved `cargo fmt` drift in `src/ag.rs` and `src/files.rs`.
- Fixed `clippy::items_after_test_module` warning in `src/ag.rs`.

### Repository hygiene

- Expanded `.gitignore` to cover `.codegraph/`, `.pi-loop.json.lock`,
  generated `TAGS`, and local scratch files.

## 1.1.0

- Initial reviewed release.
