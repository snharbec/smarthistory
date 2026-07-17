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
  1. **Space trigger** — typing a space inside the JIRA query body fires the search immediately, bypassing the debounce. This matches IDE autocomplete conventions (a space commits the current token to a search).
  2. **3-second idle safety- net timer** — a new `jira_idle_started` field fires the search after 3 seconds of no keystroke activity, independent of the 400ms debounce. The user reported that the query "sometimes isn't executed"; the idle timer guarantees the search runs within 3 seconds of the last keystroke regardless of whether the fast debounce ever elapses (e.g. the user keeps typing slowly, or the run loop is temporarily blocked on background work). The two timers are armed in lock-step by `jira_touch`; either can fire the search when its respective window elapses.
- The TUI's default key bindings now mirror the project config file (`~/.config/smarthistory/config`). Actions that the user has explicitly rebound in the config (e.g. `C-a` for `open-help`, `C-q` for `command-action`, `C-v` for `edit-file-reference`, `C-o` for `show-output`, `C-s` for `cycle-directory-source`, `F5` / `F6` / `F10` for the panes actions, `C-c` + `Esc` for `cancel`) now ship with those bindings as the default so a fresh checkout behaves the same as a configured install. Actions that the user has explicitly unbound (`toggle-duplicate-filter` and `delete-matching`) ship unbound by default (the `none` sentinel is now a valid default-key value; the help overlay and command palette render those actions as `(unbound)`). The `Cancel` action is the second action to ship with two default bindings (alongside `DeleteWordBackward`): `C-c` and `Esc` — both fire the same action so users from the bash / readline tradition (`C-c`) and the GUI-editor tradition (`Esc`) both get the
  expected behaviour
  without remapping.
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
- **JIRA `@` alias tab-completion**
  — the same `Tab` key
  also expands `@`
  aliases and user-
  defined fragments
  inside `-` mode:
  - `@mo<TAB>` →
    `@month` (built-in
    alias with trailing
    space)
  - `@sp<TAB>` →
    `@sprint` (user-
    defined fragment
    from
    `jira.search.sprint=...`)
  - `@me<TAB>` → `@me`
    (exact match)
  - `@xyz<TAB>` → no-op +
    status message (no
    match)
  The alias list is the
  four built-ins (`me`,
  `today`, `week`,
  `month`) plus every
  `jira.search.<name>`
  entry from the config
  file. The same LCP
  logic as field
  completion applies to
  ambiguous prefixes.
  The completion code
  detects the `@`
  character immediately
  before the cursor and
  routes to
  `jira_alias_complete`
  / `jira_alias_complete_with_space`
  (both unit-tested).
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
- Replaced `CyclePrefix` with `PickPrefix` (`F1`). Instead of cycling
    blindly through prefixes, the action now opens a **prefix picker**
    overlay — a centred list of every configured mode (History, Output,
    LLM, Question, Notes, Todos, Directories, Panes, JIRA, Files, Tags,
    ag).
    The list pre-selects the entry that matches the current query's
    leading char (or "History" for a plain text query), so pressing
    `Enter` with no movement is a no-op. `Up` / `Down` (or `Ctrl-N` /
    `Ctrl-P`) navigate the list; `Enter` applies the selected prefix
    (body preserved); `Esc` / the user's `Cancel` binding dismisses
    the picker without changing the query. The new `PrefixPicker` /
    `PrefixOption` structs and `handle_prefix_picker_key` /
    `draw_prefix_picker` functions are modelled on the command palette
    and theme picker so muscle memory transfers across all overlays.
    15 new unit tests cover `apply_prefix`, `PrefixPicker::new`, and
    picker key handling.
- The notes (`@`) and todos (`!`) prefixes now
  support **tag and link search** in addition
  to plain text. The query parser recognizes
  three token shapes:
  - `#TAG` → passed through to the
    `note_search` query parser as a tag
    filter. The parser already supports
    `#tagname` syntax, so no conversion is
    needed.
  - `@LINK` → converted to `[[LINK]]` (the
    `note_search` wiki-link syntax) for link
    search. The link name preserves the
    user's original casing (link targets are
    case-sensitive in Obsidian).
  - `TEXT` → passed through as a plain text
    term (AND-matched against the note/todo
    body).
  All three are AND-joined: `#TAG1 #TAG2 @LINK TEXT`
  finds notes that are tagged `TAG1` and
  `TAG2`, have a link to `LINK`, and contain
  `TEXT` in their body. The date aliases
  (`@today`, `@week`, `@month`, `@year`) are
  still extracted as a filter and applied
  post-query. The `@` prefix for link search
  replaces the old behavior where `@foo` was
  stripped to plain text — that stripping
  was a workaround for the `note_search`
  link-tokenizer, but it prevented users
  from actually searching by link. Users who
  want to search for the literal word
  `@foo` in note text can now do so (the
  token is no longer silently rewritten).
  The change is implemented in
  `parse_notes_query` in `src/tui.rs`; two
  new unit tests cover the tag and link
  tokenization, and the existing
  `fetch_todos_at_prefix_matches_text` test
  was updated to use plain text (without
  `@`) since `@` now means link search.
- The notes (`@`) and todos (`!`) prefixes
  now support **tab-completion of tags
  and links** sourced from the
  `note_search` database. Pressing
  `Tab` after `#feat` expands to
  `#feature` (unique tag match, trailing
  space); `@Neo` expands to `@NeovimNote`
  (unique link match). Ambiguous prefixes
  extend to the longest common prefix so
  the user can keep typing to
  disambiguate. The completion list is
  queried via
  `note_search::commands::metadata::get_unique_values`,
  which reads the union of tags and
  links from every indexed note. The
  `Action::JiraFieldComplete` action (bound
  to `Tab` by default) now routes to
  `notes_tab_complete_at_cursor` when the
  query is in notes or todos mode — the
  same `Tab` key serves JQL field
  completion in JIRA mode and tag / link
  completion in notes / todos mode. Two
  helper functions in `src/jira.rs`
  (`notes_tag_complete` /
  `notes_link_complete`) provide the pure
  completion logic, unit-tested with
  in-memory `note_search` databases. Seven
  TUI-level tests cover the end-to-end
  behaviour (unique match, LCP, no-match,
  todos mode, no-op outside notes/todos).
- Link expansion now wraps the link
  target in `[[...]]` syntax (the
  Obsidian wiki-link form) instead of
  the `@` shorthand, and strips the
  `.md` extension from the target.
  The `[[...]]` syntax is required
  because the `@` tokenizer in
  `note_search` only accepts
  alphanumeric / underscore / slash
  / hyphen / period characters and
  cannot represent link names with
  spaces. `@Neo<TAB>` now expands to
  `[[NeovimNote]]` (the `@` is
  consumed as the notes-mode prefix
  and the `@Neo` word is replaced
  with the full `[[...]]` form);
  `@my<TAB>` expands to
  `[[my note]]` for link names that
  contain spaces — the `[[...]]`
  brackets serve as the delimiter
  so no additional quoting is
  needed. The `.md` suffix is
  stripped from every link before
  matching (matching Obsidian's
  bare-name reference convention);
  non-`.md` extensions are preserved
  since those are actual reference
  targets (e.g. `.org` notes).
  `notes_link_complete` in `src/jira.rs`
  returns the full `[[...]]`
  expansion; the TUI uses the result
  directly without re-wrapping.
- New TUI actions `Action::MoveCursorLeft`
  (default key `Left`) and
  `Action::MoveCursorRight` (default key
  `Right`) move the cursor one
  character at a time inside the
  search query. The query string is
  unchanged; only the cursor position
  moves. The cursor saturates at
  position 0 (Left) and at the end of
  the query (Right), and is measured
  in UTF-8 characters so multi-byte
  characters are stepped over as
  single units. The new actions work
  in every mode (LLM, JIRA, notes,
  todos, or plain text search) since
  the cursor lives on `self.query`
  in all of them. To make room for the
  new default bindings, `EditStart`
  and `EditEnd` ship unbound by
  default (the `"none"` sentinel) —
  users who want the old "stage row
  for editing at cursor start/end"
  behaviour can rebind via
  `key.edit-start=...` /
  `key.edit-end=...` in their config.
  Five new unit tests cover the
  cursor-movement helpers
  (one-step, saturation at boundaries,
  multi-byte handling).

### Fixed

- Resolved `cargo fmt` drift in `src/ag.rs` and `src/files.rs`.
- Fixed `clippy::items_after_test_module` warning in `src/ag.rs`.

### Repository hygiene

- Expanded `.gitignore` to cover `.codegraph/`, `.pi-loop.json.lock`,
  generated `TAGS`, and local scratch files.

## 1.1.0

- Initial reviewed release.
