#![allow(clippy::enum_variant_names)]
// Bindings subsystem: Action enum, KeySpec parser, KeyBindings
// table, and the action_for_key lookup.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Close the TUI / cancel an ongoing operation.
    Cancel,
    /// Cycle the search scope (SESS → DIR → GLOBAL → STATS → SESS).
    CycleMode,
    /// Cycle directly between the three navigation prefix modes —
    /// `*` (panes), `#` (directories), `~` (zoxide), in that order —
    /// without going through the full [`Action::PickPrefix`] picker.
    /// From any OTHER mode (plain history, another prefix, no query
    /// yet), jumps straight to panes (the first of the three) rather
    /// than erroring or no-opping. Reuses `App::apply_prefix`, so the
    /// typed body (if any) is preserved across the switch exactly
    /// like picking a new mode from `PickPrefix` does.
    CycleNavPrefix,
    /// Toggle the duplicate filter.
    ToggleDuplicateFilter,
    /// Toggle between the active color scheme
    /// (light / dark) and the OTHER one. The
    /// "active" scheme defaults to `Dark` and is
    /// persisted in the session file's
    /// `colorscheme=` line across TUI invocations
    /// (`src/tui/theme/mod.rs`); the "other" scheme
    /// is its complement. After toggling, the TUI
    /// re-resolves the theme from the config file
    /// (`theme.<scheme>=<slug>` first, then
    /// `theme.<other-scheme>=<slug>`, then the
    /// session file's `theme=` line, then
    /// `SelectedTheme::None`) and re-installs the
    /// palette so the change is visible on the
    /// next frame. A status message confirms the
    /// new active scheme. This is a much faster
    /// way to switch themes than the theme picker
    /// (which scrolls a list of 74 themes) and is
    /// the right key for users who only ever
    /// toggle between two specific themes (e.g.
    /// `theme.light=catppuccin-latte
    /// theme.dark=dracula` — pressing `C-l` once
    /// swaps from dracula to catppuccin-latte and
    /// vice versa, no list navigation needed).
    ToggleColorScheme,
    /// Start editing the comment of the selected entry.
    EditComment,
    /// Open the captured-output view.
    ShowOutput,
    /// Copy the current selection to the system clipboard.
    ///
    /// "Selection" picks the most useful thing to copy at the
    /// moment: if the captured-output view is open, the output
    /// text is copied; otherwise the selected history row's
    /// command is copied. When nothing is selected the action
    /// is a no-op (with a status message so the user knows).
    ///
    /// The default key (`Ctrl-Y`) is the canonical readline/vim
    /// "yank" shortcut, so the muscle memory transfers.
    YankSelection,
    /// Mark the currently-selected todo
    /// entry as done inside its note
    /// file. Available only when the
    /// active query is a todo search
    /// (`!...`); outside of todo mode the
    /// action is a no-op with a status
    /// message so the user knows why
    /// their key did nothing.
    ///
    /// The implementation reads the
    /// selected row's `id` (which
    /// encodes the 1-based line number
    /// in the source file as `id =
    /// -(line_number)`) and `comment`
    /// (the filename), opens the file,
    /// replaces the `[ ]` checkbox
    /// marker on the matched line with
    /// `[x]`, and writes it back. The
    /// todo list is re-fetched so the
    /// row disappears (the underlying
    /// query is `open: true`, and a
    /// closed todo is filtered out).
    ///
    /// The default key (`Ctrl-X`) is
    /// intentionally the same letter
    /// as the user's mental model:
    /// "mark this X / done". The
    /// previously-default `Ctrl-X`
    /// binding for `DeleteMatching` is
    /// moved to `Ctrl-M-D` so the two
    /// actions don't share a key.
    ///
    /// If the file has been edited since
    /// the indexer last looked at it
    /// (e.g. the user toggled the
    /// checkbox manually), the
    /// targeted line may no longer
    /// look like a todo — the action
    /// surfaces a status message in
    /// that case rather than silently
    /// mis-editing the file.
    MarkTodoDone,
    /// Find a filename referenced in the selected history row
    /// and stage `$EDITOR <filename>` as the next selection. The
    /// TUI exits so the parent shell runs the command, which
    /// launches the editor on the file.
    ///
    /// The pick algorithm tokenizes the row's command,
    /// discards tokens containing shell metacharacters
    /// (globs, redirects, subshells, …), and scores the rest by
    /// how "path-like" each looks (starts with `/`, `~`, `./`,
    /// `../`; contains a `/`; has a file extension). The
    /// highest-scoring token wins. A no-op with a status
    /// message is surfaced when no row is selected or no
    /// filename-shaped token is found.
    ///
    /// The default key (`Ctrl-O`) is mnemonic for "Open" in
    /// editor. `$EDITOR` falls back to `vi` (POSIX-mandated)
    /// when unset.
    EditFileReference,
    /// Open the help overlay.
    OpenHelp,
    /// Delete the selected entry (with confirmation).
    DeleteSelected,
    /// Delete all matching entries (with confirmation).
    DeleteMatching,
    /// Toggle the marked state of the selected row (multi-select).
    /// Marked rows render a checkbox prefix and are the target set
    /// for `BulkDeleteMarked`. Marks are cleared automatically on a
    /// prefix-mode switch (see `App`'s `marked_ids` doc comment)
    /// but survive plain query-text edits within the same mode.
    ///
    /// The default key (`Ctrl-X`) was explicitly free — see the
    /// `MarkTodoDone` doc comment below, which notes leaving that
    /// action unbound frees `C-x` "for the user's own use". This is
    /// that use: `C-x` is a common mnemonic for "mark/select" in
    /// terminal file managers.
    ToggleMark,
    /// Clear every mark without deleting anything.
    ClearMarks,
    /// Delete every marked row (with confirmation, same
    /// `ConfirmMode` dialog machinery as `DeleteMatching`).
    ///
    /// Ships unbound by default — same policy as `DeleteMatching`:
    /// a bulk destructive action deserves an explicit opt-in key.
    /// Users who want it set `key.bulk-delete-marked=<spec>`.
    BulkDeleteMarked,
    /// Clear the search query.
    ClearQuery,
    /// Cycle the exit-code filter.
    CycleExitFilter,
    /// Cycle the sort order of the history list. The
    /// current order is also persisted in the session
    /// file and restored on the next TUI invocation, so
    /// the user always lands back on the sort they last
    /// picked.
    ///
    /// Two values are supported: `Age` (newest first,
    /// the historical default) and `Frequency` (most-
    /// run commands first, with timestamp DESC as a
    /// tie-breaker). See `SortOrder` for the full
    /// contract.
    CycleSortOrder,
    /// Ask the local ollama instance for a short
    /// description (at most four sentences) of what the
    /// selected history line does, and show the
    /// response in a full-screen overlay.
    ///
    /// The result is *not* inserted into the history
    /// table — it's a one-shot annotation, not a
    /// persisted comment. Use `EditComment` (`Ctrl-E`)
    /// to save a description into the `command_comments`
    /// table.
    ///
    /// The default key (`Ctrl-K`) is free of the other
    /// default bindings and is not bound by readline /
    /// zsh in any common configuration. Rebindable
    /// via `key.describe=...`.
    Describe,
    /// Ask the local ollama instance to correct a
    /// malformed selected history line, returning a
    /// syntactically valid command that preserves the
    /// user's intent. The result opens in a modal
    /// overlay showing the original and the corrected
    /// command side-by-side; pressing `Enter` stages
    /// the corrected command (inserts it into
    /// history with the original as the comment)
    /// and exits the TUI, while `Esc` cancels.
    ///
    /// The `correct` prompt asks the LLM to fix
    /// typos, missing arguments, and obvious errors
    /// without changing the command's meaning. If
    /// the command is already correct, the LLM
    /// returns it unchanged — the user can press
    /// `Enter` to run it as-is.
    ///
    /// The default key (`Ctrl-T`) is free of the
    /// other default bindings; rebindable via
    /// `key.correct=...`.
    Correct,
    /// Download the currently-selected JIRA
    /// issue as a markdown file via
    /// `note_search jira-issue <KEY>`.
    ///
    /// Only meaningful in the JIRA
    /// search mode (`-...`) where the
    /// selected row's `command` field
    /// carries the issue key (e.g.
    /// `PROJ-42`). Outside of JIRA mode
    /// the action is a no-op with a status
    /// message so the user understands why
    /// their key did nothing — the
    /// `Ctrl-M-s` key fires regardless of
    /// mode (so it's a discoverable key
    /// binding) but the *effect* is gated.
    ///
    /// The staged command is the bare
    /// `note_search jira-issue <KEY>` shell
    /// line (no path, no flags); `note_search`
    /// writes the markdown into the
    /// `notes.dir` configured in the same
    /// config file. The TUI exits so the
    /// parent shell runs the command, which
    /// in turn shells out to the
    /// `note_search` binary on `PATH`.
    ///
    /// The default key (`Ctrl-M-s`) is
    /// mnemonic for "Save" (the JIRA
    /// issue is saved as a local note) and
    /// is not bound by readline / zsh in any
    /// common configuration. Rebindable
    /// via `key.download-jira-issue=...`.
    DownloadJiraIssue,
    /// Download EVERY JIRA issue matching the current
    /// query (not just the selected row) as local
    /// markdown notes, via `note_search jira <JQL>`.
    /// Only meaningful in JIRA search mode (`-...`).
    /// Reuses the same JQL the TUI already built for
    /// the live search (`App::jira_build_query`), so
    /// `note_search`'s own pagination fetches
    /// everything the query matches — unlike the
    /// in-TUI result list, this is NOT limited by
    /// `JIRA_MAX_RESULTS`.
    ///
    /// Ships unbound by default (see `default_key()`):
    /// a bulk action over everything the current query
    /// matches deserves an explicit opt-in key, the
    /// same policy as `DeleteMatching`. Rebindable via
    /// `key.download-jira-matching=...`.
    DownloadJiraMatching,
    /// Run the selected command (Enter).
    Run,
    /// Prefill the line for editing, cursor at the start (Left).
    EditStart,
    /// Prefill the line for editing, cursor at the end (Right).
    EditEnd,
    /// Move the cursor up in the list (Up).
    Up,
    /// Move the cursor down in the list (Down).
    Down,
    /// Move the cursor one character to the
    /// left inside the search query (Left).
    /// The query string itself is unchanged;
    /// only the cursor position moves. The
    /// cursor is clamped at position 0 (the
    /// mode-prefix character) so pressing
    /// Left at the very start of the query
    /// is a no-op. The cursor is measured in
    /// UTF-8 characters (matching the rest
    /// of the query editing logic), so
    /// multi-byte characters are stepped
    /// over as single units.
    MoveCursorLeft,
    /// Move the cursor one character to the
    /// right inside the search query
    /// (Right). The query string itself is
    /// unchanged; only the cursor position
    /// moves. The cursor is clamped at the
    /// end of the query so pressing Right
    /// past the last character is a no-op.
    /// Measured in UTF-8 characters, same
    /// as `MoveCursorLeft`.
    MoveCursorRight,
    /// Jump 10 rows up (PageUp).
    PageUp,
    /// Jump 10 rows down (PageDown).
    PageDown,
    /// Jump to the oldest entry (Home).
    Home,
    /// Jump to the newest entry (End).
    End,
    /// Delete one character from the query (Backspace).
    Backspace,
    /// Delete one word backward from the cursor
    /// position in the query (the readline / bash
    /// `Ctrl-W` semantics). Trailing whitespace
    /// immediately before the cursor is eaten first;
    /// the cursor then walks left through the
    /// preceding run of non-whitespace characters
    /// and removes them. When the cursor is at the
    /// start of the buffer, the action is a no-op
    /// (nothing to delete). Multi-byte UTF-8 input is
    /// handled — the cursor is in characters, and
    /// `String::remove` is given a byte index that
    /// we compute correctly from the character
    /// index.
    ///
    /// This is a much faster way to clear a mistyped
    /// token than pressing Backspace repeatedly —
    /// the same shortcut works in bash / readline /
    /// zsh line editors and the user's muscle
    /// memory transfers.
    ///
    /// Default bindings: `C-w` (the readline convention)
    /// **and** `M-Backspace` (the macOS / many GUI
    /// editors' convention). Both fire the same
    /// action so users coming from either muscle
    /// memory get the expected behaviour. Either
    /// spec can be removed via `key.delete-word-backward=…`
    /// in the config file; see `default_keys()` for
    /// the full list.
    DeleteWordBackward,
    /// Open the command palette: a menu where the user can pick
    /// any action by name, with its current binding displayed.
    /// Useful when the user has forgotten (or rebound) a shortcut.
    CommandAction,
    /// Open the theme picker: a list of every available theme
    /// (manual + built-in) where navigating the list applies the
    /// theme live, Enter commits, Esc reverts to the original.
    ThemePicker,
    /// Open the key-bindings editor: a list of every action, filterable,
    /// with its current binding shown (mirrors the command palette's
    /// listing). Enter on a highlighted action starts key-capture mode —
    /// the next keypress becomes that action's new (sole) binding,
    /// applied immediately and persisted to the config file. `Delete`
    /// unbinds the highlighted action. See `handle_key_bindings_editor_key`.
    KeyBindingsEditor,
    /// Toggle between substring, fuzzy, and regex match
    /// algorithms. Applied to ALL prefix modes (history,
    /// directories, panes, notes, etc.) except JIRA.
    /// Default key: `C-f`. Cycle: Substring → Fuzzy →
    /// Regex → Substring.
    ToggleSearchMode,
    /// Cycle the directory-source
    /// filter for the
    /// `#`-mode list: ALL →
    /// TMUX → CFG → ALL. The
    /// current source is
    /// shown in the mode
    /// strip as a chip.
    CycleDirectorySource,
    /// Add the selected row's
    /// directory as a new
    /// `session.<id>` entry in
    /// the config file. Opens
    /// a multi-field dialog
    /// (Name, Dir, Exec) that
    /// writes the entry to
    /// `~/.config/smarthistory/config`
    /// and reloads the in-memory
    /// session list so the new
    /// row appears in the panes
    /// view immediately.
    ///
    /// Default key: `C-1`. The
    /// key is a no-op (with a
    /// status message) when no
    /// row is selected, when
    /// the selected row has no
    /// directory, or when the
    /// config file can't be
    /// located.
    AddSession,
    /// Add the selected row's
    /// directory as a new
    /// `host.<id>` entry in the
    /// config file. Opens a
    /// multi-field dialog
    /// (Name, Host, Hostname,
    /// User, Port, Identity,
    /// Exec) that writes the
    /// entry and reloads the
    /// in-memory host list. The
    /// Host field is pre-filled
    /// with the basename of the
    /// selected row's
    /// directory.
    ///
    /// Default key: `C-2`. Same
    /// no-op semantics as
    /// `AddSession`.
    AddHost,
    /// Open the multi-line note/todo compose overlay. Available
    /// in `@` (Notes) mode (creates a note) and `!` (Todo) mode
    /// (creates a todo); a no-op with a status message
    /// elsewhere. `Enter` inserts a newline in the buffer;
    /// `Ctrl-S` submits (stages `note_search create-note ...`
    /// and exits, same mechanism as the existing single-line
    /// `@new <text>` quick-create — see the `NoteComposeDialog`
    /// doc comment in `src/tui/state.rs`); `Esc` cancels.
    ///
    /// This is a SEPARATE, additive mechanism — `@new <text>`
    /// typed on the query line still works exactly as before
    /// and is unaffected by this action.
    ComposeNoteEntry,
    /// Open the two-field
    /// `create-note` dialog
    /// (Title + Content +
    /// inline completion for
    /// `@`-prefixed note
    /// links and `#`-prefixed
    /// tags). On submit
    /// (`Ctrl-S`) the dialog
    /// appends a
    /// `### TITLE [[LINKS]] #TAGS`
    /// + `[time:: HH:MM]` +
    /// content block to the
    /// daily note's
    /// `# Yournal` section
    /// (same target as
    /// `Action::ComposeNoteEntry`,
    /// but the heading-level
    /// formatting is
    /// different — this is
    /// the rich variant, that
    /// one is the bullet
    /// variant).
    ///
    /// Default key: `none`.
    /// Bind via the config
    /// file, e.g.
    /// `key.create-note=M-N`.
    /// The action is
    /// mode-agnostic: it's
    /// available from any
    /// prefix mode (history,
    /// panes, notes, …) so
    /// the user can capture a
    /// thought without first
    /// switching to
    /// notes mode.
    CreateNote,
    /// Open the "create JIRA issue" dialog (Project, Subject,
    /// Description, Labels, Issue Type). Pre-filled from the
    /// selected row when it's a note (subject/description/labels
    /// from the note's own filename/content/`#tags`) or a JIRA issue
    /// (copied from the selected issue, which also gets a "Relates"
    /// link to the new one on success); empty otherwise.
    CreateJiraIssue,
    /// Open the "create JIRA issue from template" picker: pick one of
    /// the markdown files under `~/.config/smarthistory/templates/jira/`,
    /// then open the same create-issue dialog `CreateJiraIssue` does,
    /// pre-filled with the template's frontmatter-defined fields (in
    /// addition to the usual note/JIRA-row prefill, which takes
    /// precedence over the template's own defaults when a row is
    /// selected).
    CreateJiraIssueFromTemplate,
    /// On a selected JIRA row (`-` mode), open a "Template name:" prompt,
    /// then generate a NEW "create JIRA issue from template" template
    /// file (under the same `~/.config/smarthistory/templates/jira/`
    /// directory `CreateJiraIssueFromTemplate` reads from) capturing that
    /// issue's project/issue type/summary/labels/description and every
    /// populated custom field — the reverse of `CreateJiraIssueFromTemplate`.
    CreateJiraTemplateFromIssue,
    /// In `;` (worktree) mode, open the "create a new worktree" dialog:
    /// pick or create a branch, optionally pick a base branch for a
    /// new branch, optionally carry over uncommitted changes, and
    /// optionally assign the new worktree to a time-tracking project.
    /// No-op outside of worktree mode (with a status message), same
    /// gate `DownloadJiraIssue` uses for `-` mode.
    ///
    /// Default key: unbound.
    CreateWorktree,
    /// Dispose the selected `;` (worktree) row: `git worktree remove`
    /// after a confirmation dialog that warns about uncommitted or
    /// unpushed changes. No-op outside of worktree mode or with no row
    /// selected (with a status message either way).
    ///
    /// Default key: unbound.
    DisposeWorktree,
    /// Filter the `*`-mode panes view to show
    /// only live multiplexer panes (hide
    /// `# sessions` and `# hosts`). Pressing
    /// the key again (when already filtered
    /// to Windows) resets to `All`.
    ///
    /// Default key: `F7`. No-op outside of
    /// panes mode (with a status message).
    FilterPanesWindows,
    /// Filter the `*`-mode panes view to show
    /// only the `# hosts` block. Pressing
    /// the key again resets to `All`.
    ///
    /// Default key: `F8`. No-op outside of
    /// panes mode.
    FilterPanesHosts,
    /// Filter the `*`-mode panes view to show
    /// only the `# sessions` block. Pressing
    /// the key again resets to `All`.
    ///
    /// Default key: `F9`. No-op outside of
    /// panes mode.
    FilterPanesSessions,
    /// Toggle detail pane visibility. Cycles
    /// through: BOTH → Details only → Output
    /// Preview only → BOTH. When only one
    /// pane is visible, the remaining pane
    /// uses the full detail-row height.
    ///
    /// Default key: `F6`. Works in any mode.
    TogglePaneVisibility,
    /// Grow the detail / output-preview row height
    /// by one line, clamped so the history list
    /// always keeps a few lines of its own.
    /// Persisted in the session file so the user's
    /// chosen height carries over to the next TUI
    /// startup.
    ///
    /// Default key: `F11`. Works in any mode.
    IncreasePaneHeight,
    /// Shrink the detail / output-preview row
    /// height by one line, never below the
    /// historical 8-line floor.
    ///
    /// Default key: `Shift-F11`. Works in any mode.
    DecreasePaneHeight,
    /// Open the prefix picker. The
    /// picker is a centred
    /// overlay (modelled on
    /// the command palette) that
    /// lists every configured
    /// prefix mode — output `+`,
    /// LLM `=`, question `?`,
    /// notes `@`, todo `!`,
    /// directories `#`, panes `*`,
    /// JIRA `-`, files `/`,
    /// tags `$`, ag `,`,
    /// plus a "no prefix"
    /// (history) entry at the
    /// top. Each row shows the
    /// mode name, the current
    /// prefix char (from the
    /// user's `QueryPrefixes`
    /// config, so custom
    /// `prefix.<mode>=<char>`
    /// bindings are honoured),
    /// and a one-line
    /// description. The picker
    /// pre-selects the row
    /// matching the current
    /// query's prefix (so Enter
    /// with no navigation is a
    /// no-op). The user
    /// navigates with Up/Down
    /// (or `j`/`k` / `Ctrl-N` /
    /// `Ctrl-P`), commits with
    /// Enter, and dismisses
    /// with the user's
    /// `Cancel` binding (default
    /// `Esc` or `Ctrl-C`).
    ///
    /// On commit, the
    /// highlighted prefix is
    /// applied to the query:
    /// the leading char is
    /// replaced (or inserted
    /// if the query had no
    /// prefix), the body is
    /// preserved, the cursor
    /// is moved to the end,
    /// the per-mode debounces
    /// are armed, and a
    /// `refresh()` populates
    /// the row set on the
    /// same frame.
    ///
    /// Default key: `F1`. The
    /// `F1`-`F4` range is
    /// the natural home for
    /// mode-picker actions
    /// (F4 is sort order,
    /// F2/F3 are free; F1
    /// was the only free
    /// F-key in the user's
    /// project config).
    /// Override with
    /// `key.pick-prefix=...`
    /// in the config file.
    /// Outside of any
    /// prefixable state
    /// (e.g. inside the
    /// comment editor or
    /// the add-entry
    /// dialog) the action
    /// is a no-op so the
    /// key doesn't
    /// interfere with
    /// anything else.
    PickPrefix,
    /// Tab-completion for JQL field names inside
    /// the `-` mode. When the user has typed a
    /// token that matches the prefix of one or
    /// more JIRA field names (e.g. `lab<TAB>`),
    /// the token is expanded to the full field
    /// name with a trailing `=`, and the cursor
    /// lands right after the `=` so the user can
    /// immediately type the value. When multiple
    /// fields share the prefix (e.g. `label`
    /// and `labels`), the token is extended to
    /// the longest common prefix and the user
    /// keeps typing to disambiguate (standard
    /// readline/bash completion behaviour).
    ///
    /// Default key: `Tab`. Outside of JIRA mode
    /// the action is a no-op so the key doesn't
    /// interfere with anything else (the TUI
    /// doesn't currently use `Tab` for any
    /// other purpose; the add-entry dialog
    /// handles `Tab` as field-next INSIDE the
    /// dialog, but the dialog intercepts the
    /// key before this action fires, so the
    /// two paths never conflict).
    ///
    /// Note: the completion list (`JIRA_FIELDS`
    /// in `src/jira.rs`) is the system field
    /// set plus a few common custom-field
    /// conventions (`sprint`, `epic`, `parent`,
    /// `storyPoints`, `rank`). User-defined
    /// custom fields are intentionally NOT in
    /// the list — those would need a JIRA
    /// round-trip to enumerate, and a static
    /// list is more predictable.
    JiraFieldComplete,
    /// Open a navigable picker listing the CodeGraph callers and
    /// callees of the currently selected `&` / `$` (codegraph-
    /// backed) symbol. Up/Down move, Enter opens the highlighted
    /// relation's source file in `$EDITOR` at its line, Esc closes.
    /// Only meaningful in codegraph / tags(fallback) mode and when
    /// the selected row carries a CodeGraph node id; otherwise a
    /// no-op with a status message.
    CodegraphRelations,
    /// Navigate to the previous (older) entry in the current
    /// mode's input history. Default `C-p` (readline /
    /// bash `previous-history`). Scoped to the active
    /// prefix mode (`+`, `=`, `?`, `@`, `!`, `#`, `*`,
    /// `/`, `$`, `&`, `,`, `-`, or plain no-prefix), so
    /// pressing it in `&` mode recalls past `&` queries
    /// only, not all-mode history. Readline-style
    /// semantics: pressing C-p from the live query saves
    /// the in-progress query as a "draft" and shows the
    /// most recent entry; further C-p presses move toward
    /// older entries; pressing C-n past the newest
    /// restores the draft; any keystroke that edits the
    /// recalled query commits it.
    PreviousHistory,
    /// Navigate to the next (newer) entry in the current
    /// mode's input history. Default `C-n` (readline /
    /// bash `next-history`). Mirror of
    /// [`Action::PreviousHistory`].
    NextHistory,
    /// Navigate to the previous (older) entry in the GLOBAL
    /// (cross-mode) query history — every query submitted or
    /// abandoned across ALL prefix modes, in true chronological
    /// order, not just the currently active mode. Default
    /// `C-S-p`. Same readline recall semantics as
    /// [`Action::PreviousHistory`] (draft-saving, oldest-clamp,
    /// commit-on-edit) applied to the flat cross-mode list
    /// instead of a per-mode slice. Recalling an entry restores
    /// its ORIGINAL leading prefix char, so the app switches
    /// back into whatever mode that query was typed in — this
    /// still only fills the query box for review/editing, it
    /// does not stage or run anything by itself.
    PreviousGlobalHistory,
    /// Navigate to the next (newer) entry in the GLOBAL
    /// (cross-mode) query history. Default `C-S-n`. Mirror of
    /// [`Action::PreviousGlobalHistory`].
    NextGlobalHistory,
    /// Context-aware "dive" key: a single binding (default
    /// `C-]`, ASCII GS 0x1D — chosen over `S-Return` because
    /// many terminals emit Shift-Return as a non-standard
    /// sequence crossterm 0.29 can't decode; rebind to
    /// `S-Return` on kitty-protocol terminals) that adapts to
    /// the active mode. In `&` / `$` (codegraph-backed) symbol
    /// mode it opens the callers/callees picker
    /// ([`Action::CodegraphRelations]); in `-` (JIRA) mode it
    /// opens the selected issue's browse URL in the system
    /// browser in the background (`open_jira_in_background`,
    /// same as `select_for_run_impl`'s JIRA branch but spawned
    /// detached so the TUI stays open); in `!` (Todo) mode it
    /// toggles the checkbox of the selected todo (same as
    /// [`Action::MarkTodoDone`], reusing the shared
    /// `App::mark_todo_done` helper so the behaviour is
    /// identical to `Ctrl-X` — `C-]` is just an ergonomic
    /// alternative); in `/` (Files) mode it opens the selected
    /// file with a per-extension shell command configured
    /// via `smart-open.<ext>=<cmd>` lines in the config
    /// file (with an optional `smart-open.default` fallback
    /// for unrecognised extensions — see
    /// [`crate::Config::smart_open_file_commands`]);
    /// in every other mode it falls through to the normal
    /// `Run` action (select the row / open the editor / fire
    /// the LLM), so the key works as an ergonomic Enter
    /// replacement everywhere.
    SmartOpen,
}

impl Action {
    /// Stable kebab-case identifier used in the config file and the
    /// session file (so users see "key.cycle-theme-next=" in their
    /// editor instead of an opaque enum variant name).
    pub fn config_key(self) -> &'static str {
        match self {
            Action::Cancel => "cancel",
            Action::CycleMode => "cycle-mode",
            Action::CycleNavPrefix => "cycle-nav-prefix",
            Action::ToggleDuplicateFilter => "toggle-duplicate-filter",
            Action::ToggleColorScheme => "toggle-color-scheme",
            Action::EditComment => "edit-comment",
            Action::ShowOutput => "show-output",
            Action::YankSelection => "yank-selection",
            Action::EditFileReference => "edit-file-reference",
            Action::OpenHelp => "open-help",
            Action::DeleteSelected => "delete-selected",
            Action::DeleteMatching => "delete-matching",
            Action::ToggleMark => "toggle-mark",
            Action::ClearMarks => "clear-marks",
            Action::BulkDeleteMarked => "bulk-delete-marked",
            Action::ClearQuery => "clear-query",
            Action::CycleExitFilter => "cycle-exit-filter",
            Action::CycleSortOrder => "cycle-sort-order",
            Action::CycleDirectorySource => "cycle-directory-source",
            Action::AddSession => "add-session",
            Action::AddHost => "add-host",
            Action::ComposeNoteEntry => "compose-note-entry",
            Action::CreateNote => "create-note",
            Action::CreateJiraIssue => "create-jira-issue",
            Action::CreateJiraIssueFromTemplate => "create-jira-issue-from-template",
            Action::CreateJiraTemplateFromIssue => "create-jira-template-from-issue",
            Action::CreateWorktree => "create-worktree",
            Action::DisposeWorktree => "dispose-worktree",
            Action::FilterPanesWindows => "filter-panes-windows",
            Action::FilterPanesHosts => "filter-panes-hosts",
            Action::FilterPanesSessions => "filter-panes-sessions",
            Action::Describe => "describe",
            Action::Correct => "correct",
            Action::DownloadJiraIssue => "download-jira-issue",
            Action::DownloadJiraMatching => "download-jira-matching",
            Action::Run => "run",
            Action::EditStart => "edit-start",
            Action::EditEnd => "edit-end",
            Action::Up => "up",
            Action::Down => "down",
            Action::MoveCursorLeft => "move-cursor-left",
            Action::MoveCursorRight => "move-cursor-right",
            Action::PageUp => "page-up",
            Action::PageDown => "page-down",
            Action::Home => "home",
            Action::End => "end",
            Action::Backspace => "backspace",
            Action::DeleteWordBackward => "delete-word-backward",
            Action::CommandAction => "command-action",
            Action::ThemePicker => "theme-picker",
            Action::KeyBindingsEditor => "key-bindings-editor",
            Action::ToggleSearchMode => "toggle-search-mode",
            Action::MarkTodoDone => "mark-todo-done",
            Action::TogglePaneVisibility => "toggle-pane-visibility",
            Action::IncreasePaneHeight => "increase-pane-height",
            Action::DecreasePaneHeight => "decrease-pane-height",
            Action::PickPrefix => "pick-prefix",
            Action::JiraFieldComplete => "jira-field-complete",
            Action::CodegraphRelations => "codegraph-relations",
            Action::PreviousHistory => "previous-history",
            Action::NextHistory => "next-history",
            Action::PreviousGlobalHistory => "previous-global-history",
            Action::NextGlobalHistory => "next-global-history",
            Action::SmartOpen => "smart-open",
        }
    }

    /// Human-readable name for help / status displays.
    pub fn display_name(self) -> &'static str {
        match self {
            Action::Cancel => "Cancel",
            Action::CycleMode => "Cycle scope",
            Action::CycleNavPrefix => "Cycle panes/directories/zoxide",
            Action::ToggleDuplicateFilter => "Toggle dedup",
            Action::ToggleColorScheme => "Toggle color scheme",
            Action::EditComment => "Edit comment",
            Action::ShowOutput => "Show output",
            Action::YankSelection => "Yank selection",
            Action::EditFileReference => "Edit referenced file",
            Action::OpenHelp => "Open help",
            Action::DeleteSelected => "Delete entry",
            Action::DeleteMatching => "Delete matches",
            Action::ToggleMark => "Toggle mark on selected row",
            Action::ClearMarks => "Clear all marks",
            Action::BulkDeleteMarked => "Delete all marked entries",
            Action::ClearQuery => "Clear query",
            Action::CycleExitFilter => "Cycle exit filter",
            Action::CycleSortOrder => "Cycle sort order",
            Action::CycleDirectorySource => "Cycle directory source",
            Action::AddSession => "Add selected directory as a session",
            Action::AddHost => "Add selected directory as a host",
            Action::ComposeNoteEntry => "Compose a new note/todo entry",
            Action::CreateNote => "Create a new note (Title + Content)",
            Action::CreateJiraIssue => "Create JIRA issue",
            Action::CreateJiraIssueFromTemplate => "Create JIRA issue from template",
            Action::CreateJiraTemplateFromIssue => "Create JIRA template from issue",
            Action::CreateWorktree => "Create worktree",
            Action::DisposeWorktree => "Dispose worktree",
            Action::FilterPanesWindows => "Filter panes: windows only",
            Action::FilterPanesHosts => "Filter panes: hosts only",
            Action::FilterPanesSessions => "Filter panes: directories only",
            Action::Describe => "Describe selected command",
            Action::Correct => "Correct selected command",
            Action::DownloadJiraIssue => "Download JIRA issue as note",
            Action::DownloadJiraMatching => "Download all matching JIRA issues as notes",
            Action::Run => "Run",
            Action::EditStart => "Edit (cursor at start)",
            Action::EditEnd => "Edit (cursor at end)",
            Action::Up => "Up",
            Action::Down => "Down",
            Action::MoveCursorLeft => "Move cursor left",
            Action::MoveCursorRight => "Move cursor right",
            Action::PageUp => "Page up",
            Action::PageDown => "Page down",
            Action::Home => "Home",
            Action::End => "End",
            Action::Backspace => "Backspace",
            Action::DeleteWordBackward => "Delete word backward",
            Action::CommandAction => "Command palette",
            Action::ThemePicker => "Theme picker",
            Action::KeyBindingsEditor => "Key bindings editor",
            Action::ToggleSearchMode => "Toggle search mode",
            Action::MarkTodoDone => "Mark todo done",
            Action::TogglePaneVisibility => "Toggle pane visibility",
            Action::IncreasePaneHeight => "Increase pane height",
            Action::DecreasePaneHeight => "Decrease pane height",
            Action::PickPrefix => "Pick prefix mode",
            Action::JiraFieldComplete => "JIRA field complete",
            Action::CodegraphRelations => "Browse callers / callees",
            Action::PreviousHistory => "Previous history entry",
            Action::NextHistory => "Next history entry",
            Action::PreviousGlobalHistory => "Previous global history entry (all modes)",
            Action::NextGlobalHistory => "Next global history entry (all modes)",
            Action::SmartOpen => "Smart open (context dive)",
        }
    }

    /// Category used to group actions in the command palette.
    /// Stable across builds so the menu ordering is predictable.
    #[allow(dead_code)]
    pub fn category(self) -> &'static str {
        match self {
            Action::Cancel
            | Action::Run
            | Action::EditStart
            | Action::EditEnd
            | Action::Up
            | Action::Down
            | Action::MoveCursorLeft
            | Action::MoveCursorRight
            | Action::PageUp
            | Action::PageDown
            | Action::Home
            | Action::End
            | Action::Backspace
            | Action::DeleteWordBackward => "navigation",
            Action::CycleMode
            | Action::CycleNavPrefix
            | Action::ToggleDuplicateFilter
            | Action::CycleExitFilter
            | Action::CycleSortOrder
            | Action::CycleDirectorySource
            | Action::ClearQuery
            | Action::ToggleSearchMode
            | Action::PickPrefix => "search",
            Action::MarkTodoDone => "todo",
            Action::ToggleColorScheme => "theme",
            Action::EditComment
            | Action::ShowOutput
            | Action::OpenHelp
            | Action::CommandAction
            | Action::ThemePicker
            | Action::KeyBindingsEditor
            | Action::YankSelection
            | Action::EditFileReference => "tools",
            // LLM-backed actions. The `run_llm_query` and
            // `start_describe` paths both call into the
            // configured ollama instance; this category
            // groups them so the command palette shows them
            // together.
            Action::Describe => "llm",
            Action::Correct => "llm",
            Action::DownloadJiraIssue => "tools",
            Action::DownloadJiraMatching => "tools",
            Action::CodegraphRelations => "codegraph",
            Action::PreviousHistory => "navigation",
            Action::NextHistory => "navigation",
            Action::PreviousGlobalHistory => "navigation",
            Action::NextGlobalHistory => "navigation",
            Action::SmartOpen => "tools",
            Action::JiraFieldComplete => "tools",
            Action::DeleteSelected | Action::DeleteMatching => "delete",
            Action::ToggleMark | Action::ClearMarks | Action::BulkDeleteMarked => "delete",
            // Adding new entries to the config file
            // (session / host). The dialog state
            // machine lives in `tui.rs`; these
            // actions just open it.
            Action::AddSession | Action::AddHost => "config",
            Action::ComposeNoteEntry => "tools",
            Action::CreateNote => "tools",
            Action::CreateJiraIssue => "tools",
            Action::CreateJiraIssueFromTemplate => "tools",
            Action::CreateJiraTemplateFromIssue => "tools",
            Action::CreateWorktree => "tools",
            Action::DisposeWorktree => "tools",
            Action::FilterPanesWindows | Action::FilterPanesHosts | Action::FilterPanesSessions => {
                "panes"
            }
            Action::TogglePaneVisibility => "layout",
            Action::IncreasePaneHeight | Action::DecreasePaneHeight => "layout",
        }
    }

    /// The default key binding (as a string in the same format the
    /// config file uses, e.g. `"C-h"`, `"Up"`, `"Esc"`).
    pub fn default_key(self) -> &'static str {
        // These defaults mirror the
        // user-configured bindings in
        // `~/.config/smarthistory/config`.
        // When the config file is
        // absent, these are the keys
        // the TUI ships with; when
        // the config file IS
        // present, the user's
        // `key.<action>=<spec>`
        // entries override these.
        //
        // The `"none"` sentinel is
        // the explicit "no default
        // key" — the action ships
        // unbound. `KeyBindings::defaults()`
        // recognises the sentinel
        // and skips the action
        // (the help overlay and
        // command palette render
        // it as `(unbound)`).
        // This is the right thing
        // for actions the user
        // has explicitly removed
        // from their workflow
        // (e.g. delete-all, the
        // duplicate filter) —
        // making the action
        // `unbound` rather than
        // picking a key the user
        // never asked for.
        match self {
            Action::Cancel => "C-c",
            Action::CycleMode => "C-g",
            Action::CycleNavPrefix => "C-z",
            Action::ToggleDuplicateFilter => "none",
            // `C-l` (ASCII 0x0C, form feed) is a free
            // key and a natural mnemonic for "Light
            // mode" (it's also the conventional
            // readline/vim shortcut for redraw — the
            // TUI doesn't need that, so we reclaim it
            // for the color-scheme toggle). The action
            // swaps the active scheme (Light ↔ Dark) and
            // re-resolves the theme from the config file
            // so the change is visible on the next
            // frame; see `App::toggle_color_scheme`
            // in `src/tui.rs`. Users who prefer a
            // different key can rebind via
            // `key.toggle-color-scheme=<spec>` (e.g.
            // `M-t` is a popular alternative).
            Action::ToggleColorScheme => "C-l",
            Action::EditComment => "C-e",
            Action::ShowOutput => "C-o",
            Action::YankSelection => "C-y",
            Action::EditFileReference => "C-v",
            Action::OpenHelp => "C-a",
            Action::DeleteSelected => "C-d",
            Action::DeleteMatching => "none",
            Action::ToggleMark => "C-x",
            // Unbound by default — clearing marks is a rare,
            // low-stakes convenience action; discoverable via
            // the command palette or `key.clear-marks=<spec>`.
            Action::ClearMarks => "none",
            // Unbound by default — same policy as `DeleteMatching`
            // above: a bulk destructive action deserves an
            // explicit opt-in key. Users who want it set
            // `key.bulk-delete-marked=<spec>`.
            Action::BulkDeleteMarked => "none",
            Action::ClearQuery => "C-u",
            Action::CycleExitFilter => "C-j",
            Action::CycleSortOrder => "F4",
            Action::CycleDirectorySource => "C-s",
            Action::Describe => "C-k",
            Action::Correct => "C-t",
            Action::DownloadJiraIssue => "C-M-s",
            // Unbound by default — same policy as
            // `DeleteMatching` above: a bulk action over
            // EVERY issue the current query matches
            // deserves an explicit opt-in key rather than
            // an arbitrary default binding. Users who want
            // it set `key.download-jira-matching=<spec>`.
            Action::DownloadJiraMatching => "none",
            Action::Run => "Enter",
            Action::EditStart => "none",
            Action::EditEnd => "none",
            Action::Up => "Up",
            Action::Down => "Down",
            Action::MoveCursorLeft => "Left",
            Action::MoveCursorRight => "Right",
            Action::PageUp => "PageUp",
            Action::PageDown => "PageDown",
            Action::Home => "Home",
            Action::End => "End",
            Action::Backspace => "Backspace",
            Action::DeleteWordBackward => "C-w",
            Action::CommandAction => "C-q",
            Action::ThemePicker => "T",
            Action::KeyBindingsEditor => "none",
            Action::ToggleSearchMode => "C-f",
            // `mark-todo-done` ships unbound by default. The
            // mark-todo-done functionality (toggling the
            // checkbox of the selected todo in its source
            // file) is still reachable via the `SmartOpen`
            // action (`C-]` by default) inside `!` mode —
            // see `Action::SmartOpen` in `dispatch_action`
            // for the routing. Leaving `mark-todo-done`
            // itself unbound frees the `C-x` key for the
            // user's own use, and `SmartOpen` is the
            // cross-mode "dive" key the user is most
            // likely to be holding when they're looking at a
            // todo row. Users who want the dedicated key
            // can rebind via
            // `key.mark-todo-done=<spec>` in the config
            // file (e.g. `key.mark-todo-done=C-x` restores
            // the historical binding).
            Action::MarkTodoDone => "none",
            Action::AddSession => "F5",
            Action::AddHost => "F6",
            Action::ComposeNoteEntry => "F2",
            // Unbound by default — discoverable via the
            // command palette (Ctrl-Q → "create note") or
            // bound explicitly via `key.create-note=<spec>`
            // in the config file (the user asked for
            // `M-N` as a recommendation; terminals that
            // don't reliably emit Alt-modified keys can
            // pick any other free spec).
            Action::CreateNote => "none",
            Action::CreateJiraIssue => "none",
            Action::CreateJiraIssueFromTemplate => "none",
            Action::CreateJiraTemplateFromIssue => "none",
            Action::CreateWorktree => "none",
            Action::DisposeWorktree => "none",
            Action::FilterPanesWindows => "F7",
            Action::FilterPanesHosts => "F8",
            Action::FilterPanesSessions => "F9",
            Action::TogglePaneVisibility => "F10",
            Action::IncreasePaneHeight => "F11",
            Action::DecreasePaneHeight => "S-F11",
            Action::JiraFieldComplete => "Tab",
            Action::PickPrefix => "F1",
            Action::CodegraphRelations => "C-r",
            Action::PreviousHistory => "C-p",
            Action::NextHistory => "C-n",
            // Uppercase P/N: crossterm reports the shifted
            // (uppercase) char alongside the SHIFT modifier bit
            // for Ctrl+Shift+<letter> on terminals that can
            // distinguish it at all — many legacy/non-kitty-
            // protocol terminals can't tell Ctrl+Shift+P from
            // Ctrl+P (same limitation noted on `C-]` above for
            // `S-Return`). Users on such a terminal can rebind
            // via `key.previous-global-history=<spec>` /
            // `key.next-global-history=<spec>`.
            Action::PreviousGlobalHistory => "C-S-P",
            Action::NextGlobalHistory => "C-S-N",
            // `C-]` (ASCII GS, 0x1D) instead of the more semantic
            // `S-Return`: many terminals either emit Shift-Return
            // as a non-standard `ESC[27;5;13~` sequence that
            // crossterm 0.29 can't decode (first param `27` isn't
            // in the legacy `~`-terminated special-key table), or
            // merge it into a plain `Enter` with no SHIFT bit.
            // `C-]` is a single-byte ASCII control char every
            // terminal emits reliably, so the dive key works
            // out-of-the-box everywhere. Users on kitty-protocol
            // terminals (Kitty / WezTerm / Alacritty / iTerm2+CSI-u)
            // who prefer Shift-Return can rebind via
            // `key.smart-open=S-Return` in the config file.
            Action::SmartOpen => "C-]",
        }
    }

    /// Every default key spec for this action, in display order.
    ///
    /// Most actions have a single default key, but some
    /// (notably `DeleteWordBackward`, which binds both
    /// `C-w` and `M-Backspace`) ship with two so users from
    /// different muscle-memory backgrounds get the expected
    /// behaviour. `KeyBindings::defaults()` iterates this
    /// list; tests that compare against "the full default
    /// binding" should use this method (or the
    /// `format_key_specs(bindings.specs(action))` form)
    /// rather than `default_key()`, which only returns the
    /// first spec.
    pub fn default_keys(self) -> &'static [&'static str] {
        match self {
            Action::DeleteWordBackward => &["C-w", "M-Backspace"],
            // Cancel has two defaults
            // to match the
            // user-configured
            // `key.cancel=C-c,Esc`
            // in the project's
            // config file: the
            // muscle-memory
            // `Ctrl-C` for
            // power users
            // (matches bash /
            // readline / vim)
            // AND the readline
            // / bash `Esc` for
            // users coming from
            // the GUI-editor
            // background. Both
            // fire the same
            // `Action::Cancel`,
            // so a user pressing
            // either gets the
            // expected behaviour
            // without remapping.
            // Either spec can be
            // removed via
            // `key.cancel=...` in
            // the config file
            // (single spec) or
            // `key.cancel=C-c,Esc`
            // (explicit multi-
            // spec).
            Action::Cancel => &["C-c", "Esc"],
            // Every other action keeps the single-spec form
            // for now. The slice indirection avoids forcing
            // a `Vec` allocation in the hot path.
            _ => &[],
        }
    }

    /// Which prefix mode(s), if any, this action is actually
    /// reachable in. Most actions are [`ActionScope::Global`] — they
    /// either do something meaningful in every mode, or branch
    /// internally on the mode with a meaningful `else` (e.g.
    /// `DeleteSelected`, `SmartOpen`). A handful of actions are
    /// gated: their `dispatch_action` arm (or a helper it calls)
    /// checks `is_X_query()`/an equivalent and bails out with a
    /// status message in every other mode — those are
    /// [`ActionScope::Modes`], scoped to exactly the mode(s) where
    /// they're not a no-op. This powers `action_for_key`'s
    /// mode-aware resolution and `scopes_conflict`'s relaxed
    /// duplicate-key detection: two actions scoped to disjoint modes
    /// can never actually compete for the same keypress, since only
    /// one prefix mode is ever active at a time.
    pub(crate) fn scope(self) -> ActionScope {
        use crate::tui::mode::ModeKind;
        match self {
            Action::DownloadJiraIssue
            | Action::CreateJiraTemplateFromIssue
            | Action::DownloadJiraMatching => ActionScope::Modes(&[ModeKind::Jira]),
            Action::CreateWorktree | Action::DisposeWorktree => {
                ActionScope::Modes(&[ModeKind::Worktree])
            }
            Action::MarkTodoDone => ActionScope::Modes(&[ModeKind::Todo]),
            Action::FilterPanesWindows | Action::FilterPanesHosts | Action::FilterPanesSessions => {
                ActionScope::Modes(&[ModeKind::Panes])
            }
            Action::ComposeNoteEntry => ActionScope::Modes(&[ModeKind::Notes, ModeKind::Todo]),
            // The gate (`open_codegraph_relations`) checks the
            // *selected row's* `mode` field rather than the query
            // prefix directly, but the two coincide in practice —
            // `&`/`$` are the only modes that ever produce
            // "codegraph"/"tags" rows.
            Action::CodegraphRelations => ActionScope::Modes(&[ModeKind::Codegraph, ModeKind::Tags]),
            // Also fires for the `'` meta-prefix pseudo-state
            // (`is_meta_query()`), which has no `ModeKind` variant of
            // its own — it resolves to `ModeKind::History` under
            // `active_mode()`, so it isn't represented here. Tab
            // colliding with something else during meta-entry is a
            // corner case not worth a dedicated `ModeKind::Meta`.
            Action::JiraFieldComplete => ActionScope::Modes(&[
                ModeKind::Jira,
                ModeKind::Notes,
                ModeKind::Todo,
                ModeKind::Segments,
                ModeKind::Similar,
                ModeKind::Paperless,
                ModeKind::Question,
            ]),
            _ => ActionScope::Global,
        }
    }
}

/// Where an [`Action`] is reachable — see [`Action::scope`].
#[derive(Debug, Clone, Copy)]
pub(crate) enum ActionScope {
    /// Meaningfully dispatched regardless of the active prefix mode.
    Global,
    /// Only reachable in one of these prefix modes; a no-op (status
    /// message) everywhere else.
    Modes(&'static [crate::tui::mode::ModeKind]),
}

impl ActionScope {
    /// True if an action with this scope would actually do something
    /// in `mode` (as opposed to hitting its own no-op gate).
    fn applies_to(self, mode: crate::tui::mode::ModeKind) -> bool {
        match self {
            ActionScope::Global => true,
            ActionScope::Modes(modes) => modes.contains(&mode),
        }
    }
}

/// Whether two actions' scopes can genuinely compete for the same
/// keypress. `Global`+`Global` always can (both are always active).
/// `Global`+`Modes(_)` cannot: `action_for_key`'s tiered resolution
/// always prefers the scoped action in its own mode and falls back
/// to the global one everywhere else, so there's no real ambiguity
/// to warn about. `Modes(x)`+`Modes(y)` can only compete where the
/// two mode sets actually overlap.
pub(crate) fn scopes_conflict(a: ActionScope, b: ActionScope) -> bool {
    match (a, b) {
        (ActionScope::Global, ActionScope::Global) => true,
        (ActionScope::Global, ActionScope::Modes(_)) | (ActionScope::Modes(_), ActionScope::Global) => false,
        (ActionScope::Modes(x), ActionScope::Modes(y)) => x.iter().any(|m| y.contains(m)),
    }
}

/// A parsed key binding. `None` means "any key with these
/// modifiers"; otherwise the binding matches only when the
/// keycode and modifiers both match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeySpec {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

/// Parse a `key.<action>=<spec>` value into a `KeySpec`. Accepts:
///
/// - Plain keys: `a`, `B`, `5`, `/`, `?`, `:`…
/// - Prefixed modifiers: `C-<x>` (Ctrl), `M-<x>` (Alt/Meta),
///   `S-<x>` (Shift). Multiple modifiers can be chained:
///   `C-M-h` = Ctrl+Alt+h.
/// - Named keys: `Esc`, `Enter`, `Tab`, `Backspace`, `Up`,
///   `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`,
///   `Space`, `BackTab`. `C-Esc`, `S-Tab`, etc. are also accepted.
///
/// Returns `Err` for unrecognized input; the caller logs a warning
/// and keeps the previous binding.
pub(crate) fn parse_key_spec(s: &str) -> Result<KeySpec, String> {
    parse_key_spec_opt(s)?.ok_or_else(|| {
        // The spec parsed as a valid unbind sentinel ("none").
        // Surface a friendly message if anyone calls the
        // non-Optional variant with that input by mistake.
        "this function does not accept the `none` sentinel; use parse_key_spec_opt".to_string()
    })
}

/// Like `parse_key_spec`, but additionally recognises an "unbind"
/// sentinel (`none`, `off`, `disable`, `-`, or empty). Returns
/// `Ok(Some(spec))` for a normal binding, `Ok(None)` for an
/// explicit unbind, and `Err` for any malformed input.
///
/// The unbind sentinel lets users disable a default binding by
/// writing `key.<action>=none` in the config file. The action
/// will then simply never fire when its key is pressed.
pub(crate) fn parse_key_spec_opt(s: &str) -> Result<Option<KeySpec>, String> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "none" | "off" | "disable" | "-" | "disabled"
    ) {
        return Ok(None);
    }
    if s.is_empty() {
        return Err("empty key spec".into());
    }
    let mut modifiers = KeyModifiers::empty();
    let mut rest = s;
    // Walk modifier prefixes. Allow C-, M-, S- in any order.
    loop {
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with("c-") && rest.len() > 2 {
            modifiers |= KeyModifiers::CONTROL;
            rest = &rest[2..];
        } else if lower.starts_with("m-") && rest.len() > 2 {
            modifiers |= KeyModifiers::ALT;
            rest = &rest[2..];
        } else if lower.starts_with("s-") && rest.len() > 2 {
            modifiers |= KeyModifiers::SHIFT;
            rest = &rest[2..];
        } else {
            break;
        }
    }
    if rest.is_empty() {
        return Err(format!("key spec {:?} has no key after modifiers", s));
    }
    // Try to interpret `rest` as a named key first (case-insensitive).
    let lower = rest.to_ascii_lowercase();
    let code = match lower.as_str() {
        "esc" | "escape" => KeyCode::Esc,
        "enter" | "return" | "cr" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" | "shift-tab" | "shifttab" => KeyCode::BackTab,
        "backspace" | "bs" => KeyCode::Backspace,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" | "page-up" => KeyCode::PageUp,
        "pagedown" | "pgdn" | "page-down" => KeyCode::PageDown,
        "insert" | "ins" => KeyCode::Insert,
        "delete" | "del" => KeyCode::Delete,
        _ => {
            // Function keys: `f` followed by a number — any value
            // crossterm's `KeyCode::F(u8)` can hold, not just F1-F12.
            // Many terminals/keyboards report extended function keys,
            // media keys, or remapped keys (e.g. via Karabiner-Elements)
            // as F13-F24 and beyond. `format_key_code` (below) already
            // formats any `F(n)` generically, and the key-bindings
            // editor's live key capture (`KeySpec { code: key.code, .. }`,
            // src/tui.rs) never went through this parser to begin with —
            // so a captured F13+ binding wrote out fine but then failed
            // to parse back on the next config load, silently reverting
            // and warning. Matching `format_key_code`'s generic handling
            // here closes that gap.
            if let Some(n) = lower.strip_prefix('f').and_then(|d| d.parse::<u8>().ok()) {
                KeyCode::F(n)
            } else {
                // Plain character. For multi-character strings, only
                // accept the single-character form; otherwise emit a
                // clear error so the user notices the typo.
                let mut chars = rest.chars();
                let first = chars.next().unwrap();
                if chars.next().is_some() {
                    return Err(format!(
                        "unknown key spec {:?}: expected a single character or a named key (Up, Esc, …)",
                        s
                    ));
                }
                KeyCode::Char(first)
            }
        }
    };
    Ok(Some(KeySpec { code, modifiers }))
}

/// Format a `KeySpec` back to its canonical display form so it can
/// be shown in the help overlay, status bar, and `smarthistory
/// config check` reports.
pub fn format_key_spec(spec: KeySpec) -> String {
    let mut out = String::new();
    if spec.modifiers.contains(KeyModifiers::CONTROL) {
        out.push_str("C-");
    }
    if spec.modifiers.contains(KeyModifiers::ALT) {
        out.push_str("M-");
    }
    if spec.modifiers.contains(KeyModifiers::SHIFT) {
        out.push_str("S-");
    }
    out.push_str(&format_key_code(spec.code));
    out
}

fn format_key_code(code: KeyCode) -> String {
    match code {
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::BackTab => "BackTab".to_string(),
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PageUp".to_string(),
        KeyCode::PageDown => "PageDown".to_string(),
        KeyCode::Insert => "Ins".to_string(),
        KeyCode::Delete => "Del".to_string(),
        KeyCode::F(n) => format!("F{}", n),
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        _ => format!("{:?}", code),
    }
}

/// User-customizable key bindings. Populated once at TUI startup
/// from the config file; defaults match the original hard-coded
/// `Ctrl-*` bindings so the TUI still behaves the same when no
/// `key.*` entries are configured.
///
/// Each action is associated with a `Vec<KeySpec>` (possibly
/// empty) so a single action can fire on several keys at once.
/// The empty `Vec` means the action is unbound — the user wrote
/// `key.<action>=none` to disable it, or the unbind sentinel
/// `none` appeared in a multi-key value like
/// `key.cancel=none,Esc`. The action still appears in `iter()`
/// (so the help overlay can render it as "unbound") but
/// `action_for_key` will never produce it.
#[derive(Debug, Clone)]
pub struct KeyBindings {
    by_action: HashMap<Action, Vec<KeySpec>>,
}

impl KeyBindings {
    /// Build a fresh binding table with every action wired to its
    /// default key(s). Actions that ship with multiple default
    /// specs (see `Action::default_keys`) get every spec bound
    /// in the listed order; everything else uses the single
    /// `default_key()` spec.
    pub fn defaults() -> Self {
        let mut by_action = HashMap::new();
        for a in ALL_ACTIONS {
            let extra = a.default_keys();
            // The "none" sentinel
            // means the action
            // ships unbound. This
            // is the right thing
            // for actions the user
            // has explicitly
            // removed from their
            // workflow in the
            // project config
            // (e.g. `delete-matching`,
            // `toggle-duplicate-filter`):
            // rather than
            // picking a key the
            // user never asked
            // for, the action is
            // left unbound and
            // the help overlay /
            // command palette
            // render it as
            // `(unbound)`. The
            // user can re-bind
            // it later via
            // `key.<action>=<spec>`.
            //
            // The sentinel is
            // matched on the
            // `default_key()`
            // (single-spec) form
            // because every
            // multi-spec action
            // that ships with
            // two defaults
            // (`Cancel`,
            // `DeleteWordBackward`)
            // is meaningful —
            // the sentinel is
            // only ever used on
            // single-spec
            // actions.
            if a.default_key() == "none" {
                by_action.insert(*a, Vec::new());
                continue;
            }
            let specs: Vec<KeySpec> = if extra.is_empty() {
                vec![parse_key_spec(a.default_key())
                    .expect("default key bindings must always parse")]
            } else {
                extra
                    .iter()
                    .map(|s| parse_key_spec(s).expect("default key bindings must always parse"))
                    .collect()
            };
            by_action.insert(*a, specs);
        }
        KeyBindings { by_action }
    }

    /// Replace the binding list for `action` with the given specs.
    /// An empty vec unbinds the action; a non-empty vec replaces
    /// any previous bindings for that action. Used by the config
    /// parser when the user writes `key.<action>=<spec>,…`.
    pub fn set(&mut self, action: Action, specs: Vec<KeySpec>) {
        self.by_action.insert(action, specs);
    }

    /// Unbind `action` so it never fires when its key is pressed.
    /// The action is still in the table (so the help overlay can
    /// report it as "unbound") but `action_for_key` and `specs`
    /// will return nothing for it.
    pub fn unbind(&mut self, action: Action) {
        self.by_action.insert(action, Vec::new());
    }

    /// All key specs currently bound to `action`. Empty slice when
    /// the action is unbound.
    pub fn specs(&self, action: Action) -> &[KeySpec] {
        self.by_action
            .get(&action)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// True when `action` is currently unbound (zero specs).
    pub fn is_unbound(&self, action: Action) -> bool {
        self.specs(action).is_empty()
    }

    /// `(action, specs)` for every action, in the stable
    /// `ALL_ACTIONS` order. Used by the help overlay, the command
    /// palette, and the `smarthistory config check` tool.
    pub fn iter(&self) -> impl Iterator<Item = (Action, &[KeySpec])> + '_ {
        ALL_ACTIONS.iter().map(move |a| (*a, self.specs(*a)))
    }
}

/// Every action the user can remap, in display order. Kept as a
/// const slice so the iteration order in `KeyBindings::iter` is
/// deterministic (helpful for the help overlay and tests).
pub const ALL_ACTIONS: &[Action] = &[
    Action::Cancel,
    Action::CycleMode,
    Action::CycleNavPrefix,
    Action::ToggleDuplicateFilter,
    Action::ToggleColorScheme,
    Action::EditComment,
    Action::ShowOutput,
    Action::YankSelection,
    Action::EditFileReference,
    Action::OpenHelp,
    Action::DeleteSelected,
    Action::DeleteMatching,
    Action::ToggleMark,
    Action::ClearMarks,
    Action::BulkDeleteMarked,
    Action::ClearQuery,
    Action::CycleExitFilter,
    Action::CycleSortOrder,
    Action::CycleDirectorySource,
    Action::Describe,
    Action::Correct,
    Action::DownloadJiraIssue,
    Action::DownloadJiraMatching,
    Action::Run,
    Action::EditStart,
    Action::EditEnd,
    Action::Up,
    Action::Down,
    Action::MoveCursorLeft,
    Action::MoveCursorRight,
    Action::PageUp,
    Action::PageDown,
    Action::Home,
    Action::End,
    Action::Backspace,
    Action::DeleteWordBackward,
    Action::CommandAction,
    Action::ThemePicker,
    Action::KeyBindingsEditor,
    Action::ToggleSearchMode,
    Action::MarkTodoDone,
    Action::AddSession,
    Action::AddHost,
    Action::ComposeNoteEntry,
    Action::CreateNote,
    Action::CreateJiraIssue,
    Action::CreateJiraIssueFromTemplate,
    Action::CreateJiraTemplateFromIssue,
    Action::CreateWorktree,
    Action::DisposeWorktree,
    Action::FilterPanesWindows,
    Action::FilterPanesHosts,
    Action::FilterPanesSessions,
    Action::TogglePaneVisibility,
    Action::IncreasePaneHeight,
    Action::DecreasePaneHeight,
    Action::JiraFieldComplete,
    Action::PickPrefix,
    Action::CodegraphRelations,
    Action::PreviousHistory,
    Action::NextHistory,
    Action::PreviousGlobalHistory,
    Action::NextGlobalHistory,
    Action::SmartOpen,
];

/// The inverse of `Action::config_key()` — look up an action by its
/// config-key string (e.g. `"create-note"` → `Action::CreateNote`).
/// Used by the command palette's persisted most-recently-used list
/// (`TuiSession::command_menu_recent`), which stores actions as their
/// config-key strings on disk so the session file survives an
/// `Action` variant being renamed/reordered across versions. Returns
/// `None` for an unrecognized string (a stale entry from a removed
/// action, or a hand-edited typo) rather than panicking.
pub(crate) fn action_from_config_key(key: &str) -> Option<Action> {
    ALL_ACTIONS.iter().find(|a| a.config_key() == key).copied()
}

/// Build a `KeyBindings` table from a parsed config map of
/// `key.<action>` → `<spec-list>` strings. Each spec-list is a
/// comma-separated list of key specs (e.g. `"C-h,F1"` or
/// `"C-h, F1"`); every spec in the list is bound to the action
/// in the order given. Whitespace around the commas is ignored.
///
/// Unknown actions are reported on stderr and dropped. Unbind
/// sentinels (`none`, `off`, `disable`, `-`, `disabled`,
/// case-insensitive) anywhere in the list mean the whole action
/// is unbound — there's no meaningful interpretation of
/// `key.cancel=none,Esc` since the user clearly wanted to
/// disable the action, so we honor that. Any other parse error
/// drops the whole binding with a warning rather than
/// half-applying a broken config.
pub fn key_bindings_from_config(entries: &HashMap<String, String>) -> KeyBindings {
    let mut bindings = KeyBindings::defaults();
    // Build a quick lookup so we can detect `key.<unknown>` typos
    // (e.g. `key.toggle-duplication-filter` with the extra "ation")
    // and warn the user about them.
    //
    // The `entries` map is keyed by the bare action name (without
    // the `key.` prefix) — see `Config::parse` — so we compare
    // against the action's `config_key()` directly.
    let known_keys: std::collections::HashSet<&'static str> =
        ALL_ACTIONS.iter().map(|a| a.config_key()).collect();
    for (k, v) in entries {
        if !known_keys.contains(k.as_str()) {
            eprintln!(
                "warning: ignoring unknown key action {:?}={:?} (valid actions: {})",
                k,
                v,
                ALL_ACTIONS
                    .iter()
                    .map(|a| a.config_key())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            continue;
        }
    }
    for a in ALL_ACTIONS {
        let Some(value) = entries.get(a.config_key()) else {
            continue;
        };
        // Split on commas, trim each piece, drop empties. The
        // outer trim handles a leading/trailing comma.
        let parts: Vec<&str> = value
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        if parts.is_empty() {
            eprintln!(
                "warning: ignoring key.{}={:?}: no key specs after splitting on ','",
                a.config_key(),
                value,
            );
            continue;
        }
        let mut specs: Vec<KeySpec> = Vec::with_capacity(parts.len());
        let mut unbind_requested = false;
        let mut bad_piece: Option<String> = None;
        for part in &parts {
            match parse_key_spec_opt(part) {
                Ok(Some(spec)) => specs.push(spec),
                Ok(None) => unbind_requested = unbind_requested || specs.is_empty(),
                Err(e) => {
                    bad_piece = Some(format!("{:?}: {}", part, e));
                    break;
                }
            }
        }
        if let Some(msg) = bad_piece {
            eprintln!(
                "warning: ignoring key.{}={:?}: bad spec {}",
                a.config_key(),
                value,
                msg,
            );
            continue;
        }
        if unbind_requested {
            // An unbind sentinel anywhere in the list means the
            // user wants this action disabled. The other keys in
            // the list are silently discarded so that
            // `key.cancel=none,Esc` (a likely accidental mix-up)
            // doesn't bind Esc to cancel after the user thought
            // they'd disabled it.
            bindings.unbind(*a);
            continue;
        }
        bindings.set(*a, specs);
    }

    // Detect duplicate key bindings (same key bound to multiple actions
    // whose scopes can actually compete — see `scopes_conflict`). Actions
    // scoped to disjoint prefix modes are exempt: only one prefix mode is
    // ever active at a time, so they can never really collide. Among
    // actions that DO conflict, the first in ALL_ACTIONS order wins (see
    // `action_for_key`); the others are silently shadowed. We warn about
    // all shadowed bindings so the user can fix the conflict.
    {
        // Every action seen so far for a given key, not just the first —
        // see the identical fix (and its rationale) in `main.rs`'s
        // `validate_config`, the sibling copy of this same check. A key
        // can legitimately be held by several disjoint-mode-scoped
        // actions at once, so a later action must be checked against ALL
        // of them, not only whichever happened to claim the key first.
        let mut seen: std::collections::HashMap<(KeyCode, KeyModifiers), Vec<(&'static str, Action)>> =
            std::collections::HashMap::new();
        for a in ALL_ACTIONS {
            for spec in bindings.specs(*a) {
                let key = (spec.code, spec.modifiers);
                let holders = seen.entry(key).or_default();
                for (prev_name, prev_action) in holders.iter() {
                    if scopes_conflict(a.scope(), prev_action.scope()) {
                        eprintln!(
                            "warning: key.{}={} is bound to the same key ({}) as {}; \
                             only the first binding wins",
                            a.config_key(),
                            format_key_spec(*spec),
                            format_key_spec(*spec),
                            prev_name,
                        );
                    }
                }
                holders.push((a.config_key(), *a));
            }
        }
    }

    bindings
}

/// Try to match a `KeyEvent` against the binding table, returning
/// the action that should fire given `current_mode` (the active
/// prefix mode — see `crate::tui::mode::active_mode`). When several
/// actions are bound to the same key, resolution is tiered: (1) a
/// mode-scoped action (`ActionScope::Modes`) whose set contains
/// `current_mode` wins — a deliberate same-key override for the
/// active mode; else (2) a `ActionScope::Global` action wins,
/// unchanged from the historical "first in `ALL_ACTIONS` order"
/// behavior; else (3) whatever mode-scoped action comes first anyway
/// — it doesn't apply here, but the key should still be captured
/// (and show that action's own "wrong mode" status message) rather
/// than falling through to typing the character, matching today's
/// behavior for a key bound to a single mode-scoped action pressed
/// outside its mode. Within each tier, `ALL_ACTIONS` order is the
/// tiebreaker.
///
/// An action with several bound specs is matched if the event
/// matches *any* of them — pressing F1 or C-h both fire
/// `Action::OpenHelp` if the user wrote `key.open-help=C-h,F1`.
pub fn action_for_key(
    bindings: &KeyBindings,
    key: &KeyEvent,
    current_mode: crate::tui::mode::ModeKind,
) -> Option<Action> {
    let matches = || {
        ALL_ACTIONS.iter().copied().filter(|a| {
            bindings
                .specs(*a)
                .iter()
                .any(|spec| spec.code == key.code && spec.modifiers == key.modifiers)
        })
    };
    if let Some(a) = matches().find(|a| matches!(a.scope(), ActionScope::Modes(_)) && a.scope().applies_to(current_mode)) {
        return Some(a);
    }
    if let Some(a) = matches().find(|a| matches!(a.scope(), ActionScope::Global)) {
        return Some(a);
    }
    matches().next()
}

/// True if `key` resolves to the user's configured `Action::Cancel`
/// binding. Modal overlays aren't "in" any prefix mode, so this always
/// resolves against `ModeKind::History` — the neutral default every
/// modal handler already uses for `action_for_key`'s mode parameter
/// (see the mode-scoped-key-bindings feature).
pub(crate) fn is_cancel_key(bindings: &KeyBindings, key: &KeyEvent) -> bool {
    action_for_key(bindings, key, crate::tui::mode::ModeKind::History) == Some(Action::Cancel)
}

/// Join a slice of `KeySpec` into the canonical display form
/// (`"C-h, F1, M-x"`) for the help overlay and the command
/// palette. Empty slice returns the empty string; use
/// `KeyBindings::is_unbound` to render the "unbound" label
/// separately.
pub fn format_key_specs(specs: &[KeySpec]) -> String {
    let mut out = String::new();
    for (i, spec) in specs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format_key_spec(*spec));
    }
    out
}

/// Append a `key.<config_key> = <value>` line to the main config file,
/// persisting a rebind (or, when `spec` is `None`, an unbind) made via
/// the in-TUI key-bindings editor. Mirrors
/// `crate::tui::mode::worktree::write_project_dir_binding`'s exact
/// atomic-write shape (read-or-empty, append, tmp-write, rename), but
/// the value here is UNQUOTED plain text — `key.<action>=` lines are
/// parsed as bare `value.trim()` (`Config::parse_multi`), not the
/// `{:?}`-quoted strings `project.*.dir` lines use; a quoted value
/// would fail to parse back into a `KeySpec`.
///
/// A later `key.<config_key>=` line always wins over an earlier one
/// (`Config::parse_multi`'s documented "later line wins" rule), so
/// appending is correct — no need to find and replace an existing line.
pub(crate) fn write_key_binding_to_config(action: Action, specs: &[KeySpec]) -> Result<(), String> {
    let value = if specs.is_empty() {
        "none".to_string()
    } else {
        // `Config::parse_multi` treats `#` as a comment-start
        // anywhere in the line (`raw_line.split('#').next()`), so a
        // formatted spec containing it would silently truncate the
        // line and corrupt the binding. No named key or modifier
        // prefix ever formats to a bare `#`, so this only fires for
        // `KeySpec { code: KeyCode::Char('#'), .. }` — rare, but a
        // real corruption path worth refusing outright. Checked per
        // spec (not the joined string) so one bad spec in a longer
        // list is reported precisely.
        for spec in specs {
            let formatted = format_key_spec(*spec);
            if formatted.contains('#') {
                return Err(format!(
                    "cannot persist a binding that formats to {:?} — the config file's \
                     comment syntax would truncate the line",
                    formatted
                ));
            }
        }
        format_key_specs(specs)
    };
    let target_path =
        crate::config_path().ok_or_else(|| "no config directory path (HOME is not set)".to_string())?;
    let contents = match std::fs::read_to_string(&target_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("failed to read {}: {}", target_path.display(), e)),
    };
    // `key.<config_key>` is the exact left-hand side `Config::parse_multi`
    // matches on (after its own `#`-comment stripping and `=`-splitting) —
    // matched the same way here so a hand-edited file's spacing
    // (`key.foo=X` vs `key.foo = X`) doesn't stop this from finding the
    // line the parser would actually treat as this action's binding.
    let target_key = format!("key.{}", action.config_key());
    let new_line = format!("key.{} = {}", action.config_key(), value);
    // Replace the FIRST existing line for this action in place (keeps the
    // edit visually local instead of always growing the file at the
    // bottom), and drop every OTHER line for the same action — self-
    // healing any duplicates a previous append-only version of this
    // function already left behind, not just preventing new ones. Only
    // the line search key is derived from `config_key()`; unrelated
    // `key.*` lines are copied through untouched.
    let mut replaced = false;
    let mut new_lines: Vec<String> = Vec::new();
    for raw_line in contents.lines() {
        let before_comment = raw_line.split('#').next().unwrap_or("");
        let is_this_action = match before_comment.split_once('=') {
            Some((k, _)) => k.trim() == target_key,
            None => false,
        };
        if is_this_action {
            if !replaced {
                new_lines.push(new_line.clone());
                replaced = true;
            }
            // else: drop this stale duplicate line entirely.
        } else {
            new_lines.push(raw_line.to_string());
        }
    }
    if !replaced {
        new_lines.push(new_line);
    }
    let mut new_contents = new_lines.join("\n");
    new_contents.push('\n');
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {}", parent.display(), e))?;
    }
    let tmp_path = target_path.with_extension("tmp");
    std::fs::write(&tmp_path, new_contents.as_bytes())
        .map_err(|e| format!("failed to write {}: {}", tmp_path.display(), e))?;
    std::fs::rename(&tmp_path, &target_path).map_err(|e| {
        format!(
            "failed to rename {} to {}: {}",
            tmp_path.display(),
            target_path.display(),
            e,
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod write_key_binding_to_config_tests {
    use super::{Action, KeySpec, write_key_binding_to_config};
    use crate::tui::tests::ENV_LOCK;
    use crossterm::event::{KeyCode, KeyModifiers};

    /// Reuses the crate-wide `ENV_LOCK` (`src/tui/tests.rs`) rather than
    /// a lock private to this module — a local `Mutex` here would only
    /// serialise tests WITHIN this module, not against every other
    /// `$HOME`-mutating test in the crate (`write_theme_to_config_tests`
    /// in `src/tui.rs`, `expand_home_*`/`config_parses_user_file` in
    /// `src/util.rs`/`src/main.rs`, …) — `cargo test` runs the whole
    /// crate's tests in one process, so two independently-locked test
    /// suites can still mutate the same process-level `$HOME` at the
    /// same time. This was exactly that bug: a separate local
    /// `HOME_LOCK` here raced `write_theme_to_config_tests::HOME_LOCK`
    /// under CI's parallel test runner (no `--test-threads=1`),
    /// intermittently corrupting unrelated tests' `$HOME`-dependent
    /// state.

    /// Helper: seed a config file, run the write, return the
    /// resulting file text. Uses a temp `$HOME` so the test never
    /// touches the user's real config — same pattern as
    /// `write_theme_to_config_tests::run_with_existing`.
    fn run_with_existing(existing: &str, action: Action, spec: Option<KeySpec>) -> Result<String, String> {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp_home = std::env::temp_dir().join(format!(
            "smarthistory_key_binding_write_test_{}_{:?}",
            std::process::id(),
            action
        ));
        let _ = std::fs::remove_dir_all(&tmp_home);
        std::fs::create_dir_all(&tmp_home).expect("create tmp home");
        let cfg_dir = tmp_home.join(".config/smarthistory");
        std::fs::create_dir_all(&cfg_dir).expect("create cfg dir");
        let cfg_path = cfg_dir.join("config");
        std::fs::write(&cfg_path, existing).expect("write seed config");
        let prev_home = std::env::var("HOME").ok();
        // SAFETY: single-threaded test, serialised by ENV_LOCK,
        // restored on the way out — same convention as
        // `write_theme_to_config_tests::run_with_existing`.
        unsafe {
            std::env::set_var("HOME", &tmp_home);
        }
        let specs: Vec<KeySpec> = spec.into_iter().collect();
        let result = write_key_binding_to_config(action, &specs);
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        let contents = std::fs::read_to_string(&cfg_path);
        let _ = std::fs::remove_dir_all(&tmp_home);
        result?;
        Ok(contents.expect("read back the written config"))
    }

    /// A rebind writes a bare, UNQUOTED `key.<config_key> = <spec>`
    /// line — `key.<action>=` values are parsed as plain trimmed text
    /// (`Config::parse_multi`), so a quoted value (like
    /// `project.*.dir`'s `{:?}`-formatted lines) would fail to parse
    /// back into a `KeySpec`.
    #[test]
    fn writes_unquoted_spec_line() {
        let spec = KeySpec { code: KeyCode::F(19), modifiers: KeyModifiers::empty() };
        let result = run_with_existing("", Action::ThemePicker, Some(spec)).expect("write should succeed");
        assert!(
            result.contains("key.theme-picker = F19\n"),
            "expected an unquoted `key.theme-picker = F19` line; got:\n{}",
            result
        );
    }

    /// An unbind (`spec = None`) writes the canonical `none` sentinel.
    #[test]
    fn unbind_writes_none_sentinel() {
        let result = run_with_existing("", Action::ThemePicker, None).expect("write should succeed");
        assert!(
            result.contains("key.theme-picker = none\n"),
            "expected `key.theme-picker = none`; got:\n{}",
            result
        );
    }

    /// A rebind replaces an existing `key.<config_key>=` line for the
    /// SAME action in place rather than appending a new one below it —
    /// repeatedly rebinding one action through the editor must not
    /// accumulate an ever-growing pile of stale, superseded lines for it
    /// (each rebind used to append a fresh line and leave every earlier
    /// one behind — technically still correct under `Config::parse_multi`'s
    /// "later line wins" rule, but confusing to read and unbounded in
    /// file size).
    #[test]
    fn replaces_existing_line_in_place_instead_of_appending() {
        let spec = KeySpec { code: KeyCode::F(19), modifiers: KeyModifiers::empty() };
        let result = run_with_existing("key.theme-picker = T\n", Action::ThemePicker, Some(spec))
            .expect("write should succeed");
        assert!(
            !result.contains("key.theme-picker = T\n"),
            "the superseded line must be gone, not left behind:\n{}",
            result
        );
        assert_eq!(
            result.matches("key.theme-picker").count(),
            1,
            "exactly one line for this action, not a growing pile:\n{}",
            result
        );
        assert!(result.contains("key.theme-picker = F19\n"));
    }

    /// A file that already accumulated duplicate lines for one action
    /// (e.g. from before this fix) self-heals down to a single line the
    /// next time that action is rebound — not just "stop making it
    /// worse," but "clean up what's already there."
    #[test]
    fn collapses_pre_existing_duplicate_lines_down_to_one() {
        let spec = KeySpec { code: KeyCode::F(19), modifiers: KeyModifiers::empty() };
        let result = run_with_existing(
            "key.theme-picker = T\nkey.theme-picker = C-t\nkey.theme-picker = F5\n",
            Action::ThemePicker,
            Some(spec),
        )
        .expect("write should succeed");
        assert_eq!(
            result.matches("key.theme-picker").count(),
            1,
            "every duplicate collapses to one line:\n{}",
            result
        );
        assert!(result.contains("key.theme-picker = F19\n"));
    }

    /// Replacing one action's binding must not touch another action's
    /// line — the match is on the whole left-hand side up to `=`
    /// (exact equality), not a substring/prefix check, so this can't
    /// accidentally over-match a differently-named action.
    #[test]
    fn leaves_other_actions_lines_untouched() {
        let spec = KeySpec { code: KeyCode::F(19), modifiers: KeyModifiers::empty() };
        let result = run_with_existing(
            "key.cancel = Esc\nkey.theme-picker = T\n",
            Action::ThemePicker,
            Some(spec),
        )
        .expect("write should succeed");
        assert!(
            result.contains("key.cancel = Esc\n"),
            "an unrelated action's line must survive untouched:\n{}",
            result
        );
        assert!(result.contains("key.theme-picker = F19\n"));
        assert!(!result.contains("key.theme-picker = T\n"));
    }

    /// The replacement keeps the line at its original position in the
    /// file rather than always relocating it to the end — a rebind
    /// should read as a small, local edit, not shuffle unrelated
    /// surrounding lines around.
    #[test]
    fn replacement_stays_at_the_original_lines_position() {
        let spec = KeySpec { code: KeyCode::F(19), modifiers: KeyModifiers::empty() };
        let result = run_with_existing(
            "key.cancel = Esc\nkey.theme-picker = T\nkey.open-help = C-a\n",
            Action::ThemePicker,
            Some(spec),
        )
        .expect("write should succeed");
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines, vec!["key.cancel = Esc", "key.theme-picker = F19", "key.open-help = C-a"]);
    }
}
