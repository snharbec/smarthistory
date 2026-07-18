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
    /// Toggle the duplicate filter.
    ToggleDuplicateFilter,
    /// Toggle between the active color scheme
    /// (light / dark) and the OTHER one. The
    /// "active" scheme is the one auto-detected at
    /// startup (via `detect_color_scheme()` in
    /// `src/tui/theme/mod.rs`); the "other" scheme
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
    /// (which scrolls a list of 73 themes) and is
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
    /// Toggle the detail / output-preview row
    /// height between two presets: `Default`
    /// (8 lines, ~50% of the list area) and `Tall`
    /// (~70% of the list area). Persisted in
    /// the session file so the user's choice
    /// carries over to the next TUI startup.
    ///
    /// Default key: `F11`. Works in any mode.
    TogglePaneHeight,
    /// Open the prefix picker. The
    /// picker is a centred
    /// overlay (modelled on
    /// the command palette) that
    /// lists every configured
    /// prefix mode — output `+`,
    /// LLM `=`, question `%`,
    /// notes `@`, todo `!`,
    /// directories `#`, panes `*`,
    /// JIRA `-`, files `~`,
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
    /// prefix mode (`+`, `=`, `%`, `@`, `!`, `#`, `*`,
    /// `~`, `$`, `&`, `,`, `-`, or plain no-prefix), so
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
    /// alternative); in `~` (Files) mode it opens the selected
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
            Action::ToggleDuplicateFilter => "toggle-duplicate-filter",
            Action::ToggleColorScheme => "toggle-color-scheme",
            Action::EditComment => "edit-comment",
            Action::ShowOutput => "show-output",
            Action::YankSelection => "yank-selection",
            Action::EditFileReference => "edit-file-reference",
            Action::OpenHelp => "open-help",
            Action::DeleteSelected => "delete-selected",
            Action::DeleteMatching => "delete-matching",
            Action::ClearQuery => "clear-query",
            Action::CycleExitFilter => "cycle-exit-filter",
            Action::CycleSortOrder => "cycle-sort-order",
            Action::CycleDirectorySource => "cycle-directory-source",
            Action::AddSession => "add-session",
            Action::AddHost => "add-host",
            Action::FilterPanesWindows => "filter-panes-windows",
            Action::FilterPanesHosts => "filter-panes-hosts",
            Action::FilterPanesSessions => "filter-panes-sessions",
            Action::Describe => "describe",
            Action::Correct => "correct",
            Action::DownloadJiraIssue => "download-jira-issue",
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
            Action::ToggleSearchMode => "toggle-search-mode",
            Action::MarkTodoDone => "mark-todo-done",
            Action::TogglePaneVisibility => "toggle-pane-visibility",
            Action::TogglePaneHeight => "toggle-pane-height",
            Action::PickPrefix => "pick-prefix",
            Action::JiraFieldComplete => "jira-field-complete",
            Action::CodegraphRelations => "codegraph-relations",
            Action::PreviousHistory => "previous-history",
            Action::NextHistory => "next-history",
            Action::SmartOpen => "smart-open",
        }
    }

    /// Human-readable name for help / status displays.
    pub fn display_name(self) -> &'static str {
        match self {
            Action::Cancel => "Cancel",
            Action::CycleMode => "Cycle scope",
            Action::ToggleDuplicateFilter => "Toggle dedup",
            Action::ToggleColorScheme => "Toggle color scheme",
            Action::EditComment => "Edit comment",
            Action::ShowOutput => "Show output",
            Action::YankSelection => "Yank selection",
            Action::EditFileReference => "Edit referenced file",
            Action::OpenHelp => "Open help",
            Action::DeleteSelected => "Delete entry",
            Action::DeleteMatching => "Delete matches",
            Action::ClearQuery => "Clear query",
            Action::CycleExitFilter => "Cycle exit filter",
            Action::CycleSortOrder => "Cycle sort order",
            Action::CycleDirectorySource => "Cycle directory source",
            Action::AddSession => "Add selected directory as a session",
            Action::AddHost => "Add selected directory as a host",
            Action::FilterPanesWindows => "Filter panes: windows only",
            Action::FilterPanesHosts => "Filter panes: hosts only",
            Action::FilterPanesSessions => "Filter panes: sessions only",
            Action::Describe => "Describe selected command",
            Action::Correct => "Correct selected command",
            Action::DownloadJiraIssue => "Download JIRA issue as note",
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
            Action::ToggleSearchMode => "Toggle search mode",
            Action::MarkTodoDone => "Mark todo done",
            Action::TogglePaneVisibility => "Toggle pane visibility",
            Action::TogglePaneHeight => "Toggle pane height",
            Action::PickPrefix => "Pick prefix mode",
            Action::JiraFieldComplete => "JIRA field complete",
            Action::CodegraphRelations => "Browse callers / callees",
            Action::PreviousHistory => "Previous history entry",
            Action::NextHistory => "Next history entry",
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
            Action::CodegraphRelations => "codegraph",
            Action::PreviousHistory => "navigation",
            Action::NextHistory => "navigation",
            Action::SmartOpen => "tools",
            Action::JiraFieldComplete => "tools",
            Action::DeleteSelected | Action::DeleteMatching => "delete",
            // Adding new entries to the config file
            // (session / host). The dialog state
            // machine lives in `tui.rs`; these
            // actions just open it.
            Action::AddSession | Action::AddHost => "config",
            Action::FilterPanesWindows | Action::FilterPanesHosts | Action::FilterPanesSessions => {
                "panes"
            }
            Action::TogglePaneVisibility => "layout",
            Action::TogglePaneHeight => "layout",
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
            Action::ClearQuery => "C-u",
            Action::CycleExitFilter => "C-j",
            Action::CycleSortOrder => "F4",
            Action::CycleDirectorySource => "C-s",
            Action::Describe => "C-k",
            Action::Correct => "C-t",
            Action::DownloadJiraIssue => "C-M-s",
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
            Action::FilterPanesWindows => "F7",
            Action::FilterPanesHosts => "F8",
            Action::FilterPanesSessions => "F9",
            Action::TogglePaneVisibility => "F10",
            Action::TogglePaneHeight => "F11",
            Action::JiraFieldComplete => "Tab",
            Action::PickPrefix => "F1",
            Action::CodegraphRelations => "C-r",
            Action::PreviousHistory => "C-p",
            Action::NextHistory => "C-n",
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
        "f1" => KeyCode::F(1),
        "f2" => KeyCode::F(2),
        "f3" => KeyCode::F(3),
        "f4" => KeyCode::F(4),
        "f5" => KeyCode::F(5),
        "f6" => KeyCode::F(6),
        "f7" => KeyCode::F(7),
        "f8" => KeyCode::F(8),
        "f9" => KeyCode::F(9),
        "f10" => KeyCode::F(10),
        "f11" => KeyCode::F(11),
        "f12" => KeyCode::F(12),
        _ => {
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
    Action::ToggleDuplicateFilter,
    Action::ToggleColorScheme,
    Action::EditComment,
    Action::ShowOutput,
    Action::YankSelection,
    Action::EditFileReference,
    Action::OpenHelp,
    Action::DeleteSelected,
    Action::DeleteMatching,
    Action::ClearQuery,
    Action::CycleExitFilter,
    Action::CycleSortOrder,
    Action::CycleDirectorySource,
    Action::Describe,
    Action::Correct,
    Action::DownloadJiraIssue,
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
    Action::ToggleSearchMode,
    Action::MarkTodoDone,
    Action::AddSession,
    Action::AddHost,
    Action::FilterPanesWindows,
    Action::FilterPanesHosts,
    Action::FilterPanesSessions,
    Action::TogglePaneVisibility,
    Action::TogglePaneHeight,
    Action::JiraFieldComplete,
    Action::PickPrefix,
    Action::CodegraphRelations,
    Action::PreviousHistory,
    Action::NextHistory,
    Action::SmartOpen,
];

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

    // Detect duplicate key bindings (same key bound to multiple actions).
    // The first action in ALL_ACTIONS order wins; the others are silently
    // shadowed. We warn about all shadowed bindings so the user can fix
    // the conflict.
    {
        let mut seen: std::collections::HashMap<(KeyCode, KeyModifiers), &'static str> =
            std::collections::HashMap::new();
        for a in ALL_ACTIONS {
            for spec in bindings.specs(*a) {
                let key = (spec.code, spec.modifiers);
                if let Some(prev) = seen.get(&key) {
                    eprintln!(
                        "warning: key.{}={} is bound to the same key ({}) as {}; \
                         only the first binding wins",
                        a.config_key(),
                        format_key_spec(*spec),
                        format_key_spec(*spec),
                        prev,
                    );
                } else {
                    seen.insert(key, a.config_key());
                }
            }
        }
    }

    bindings
}

/// Try to match a `KeyEvent` against the binding table, returning
/// the first action whose spec matches. Iteration order is the
/// `ALL_ACTIONS` order, so earlier entries win on collisions. (We
/// don't currently try to detect collisions; the help overlay lists
/// every binding so the user can spot duplicates themselves.)
///
/// An action with several bound specs is matched if the event
/// matches *any* of them — pressing F1 or C-h both fire
/// `Action::OpenHelp` if the user wrote `key.open-help=C-h,F1`.
pub fn action_for_key(bindings: &KeyBindings, key: &KeyEvent) -> Option<Action> {
    for a in ALL_ACTIONS {
        for spec in bindings.specs(*a) {
            if spec.code == key.code && spec.modifiers == key.modifiers {
                return Some(*a);
            }
        }
    }
    None
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
