# TUI Actions

Every key the TUI responds to is bound to an **action**. An action is a named, configurable behavior — pick any action by name from the command palette (`Ctrl-Q` by default), or rebind it via `key.<action>=<spec>` in `~/.config/smarthistory/config`. This file lists every action in `ALL_ACTIONS` (in the order the command palette uses), grouped by the [`Action::category`](../src/tui/bindings.rs) field.

The canonical source is [`src/tui/bindings.rs`](../src/tui/bindings.rs) — the Rust enum `Action` plus the `config_key()` / `display_name()` / `default_key()` / `category()` methods. This file mirrors that source; if they drift, the live overlay (`Ctrl-A`) is what runs in production, and this doc becomes documentation debt.

## How actions, keys, and modes interact

| Concept | What it is |
| --- | --- |
| **Action** | A named behavior (e.g. `Cancel`, `Run`, `SmartOpen`). 48 actions ship in `ALL_ACTIONS`. Each has a stable kebab-case `config_key` for the config file, a `display_name` for the palette / status messages, a `default_key` (or `"none"` for unbound-by-default), and a `category`. |
| **Key binding** | The mapping from a `KeySpec` (e.g. `C-c`, `F1`, `Up`) to an action. Multiple keys can map to the same action (`delete-word-backward` ships with both `C-w` and `M-Backspace`). The same key can't map to two actions — the first one in `ALL_ACTIONS` order wins (see [`KeyBindings::defaults`](../src/tui/bindings.rs)). |
| **Mode** | The active prefix mode (history, output, `~`, `$`, `&`, etc. — see [`docs/modes/`](modes/README.md)). Most actions work in every mode; a few are mode-specific (`MarkTodoDone` is a no-op outside `!` mode, `JiraFieldComplete` only completes inside `-`, `CodegraphRelations` is meaningful only in `&` / `$`). |
| **Overlay** | When an overlay is open (command palette, prefix picker, theme picker, completion menu, help, output view, describe view, add-entry dialog, delete-confirmation), it captures key routing until it closes; the global actions don't fire underneath it. |

## Config key syntax

`key.<action>=<spec>` where `<spec>` is a `KeySpec`:

| Spec form | Example | Meaning |
| --- | --- | --- |
| `C-<key>` | `C-c` | Ctrl + key |
| `M-<key>` | `M-h` | Alt + key |
| `S-<key>` | `S-Return` | Shift + key (use `BackTab` for Shift-Tab) |
| `C-M-<key>` | `C-M-s` | Ctrl + Alt + key (modifiers compose in any order) |
| `<named>` | `Up`, `PageDown`, `F1`, `Insert`, `Backspace`, `Esc`, `Enter`, `Tab`, `Home`, `End` | Named special key |
| `<char>` | `T` | A single character |
| `none` | `none` | Unbind (the action ships unbound; rebinding is the user's choice) |

Multiple specs for one action are comma-separated: `key.cancel=C-c,Esc`.

See `parse_key_spec_opt` in [`src/tui/bindings.rs`](../src/tui/bindings.rs) for the full grammar. Unknown key specs are dropped with a stderr warning; the rest of the config still loads.

## Categories

Actions are grouped in the command palette by their `category()`:

| Category | Actions |
| --- | --- |
| [`navigation`](#navigation) | Cancel, Run, EditStart, EditEnd, Up, Down, MoveCursorLeft, MoveCursorRight, PageUp, PageDown, Home, End, Backspace, DeleteWordBackward, PreviousHistory, NextHistory |
| [`search`](#search) | CycleMode, ToggleDuplicateFilter, CycleExitFilter, CycleSortOrder, CycleDirectorySource, ClearQuery, ToggleSearchMode, PickPrefix |
| [`todo`](#todo) | MarkTodoDone |
| [`theme`](#theme) | CycleThemeNext, CycleThemePrev |
| [`tools`](#tools) | EditComment, ShowOutput, OpenHelp, CommandAction, ThemePicker, YankSelection, EditFileReference, DownloadJiraIssue, JiraFieldComplete, SmartOpen |
| [`llm`](#llm) | Describe, Correct |
| [`delete`](#delete) | DeleteSelected, DeleteMatching |
| [`config`](#config) | AddSession, AddHost |
| [`panes`](#panes) | FilterPanesWindows, FilterPanesHosts, FilterPanesSessions |
| [`layout`](#layout) | TogglePaneVisibility |
| [`codegraph`](#codegraph) | CodegraphRelations |

---

## navigation

### `Cancel`

| Field | Value |
| --- | --- |
| Config key | `cancel` |
| Display name | Cancel |
| Default key | `C-c` |
| Category | navigation |

Close the TUI / cancel an ongoing operation.

`Cancel` has two default keys (`C-c` and `Esc`) so users from both the readline / bash `Ctrl-C` muscle memory and the GUI-editor `Esc` muscle memory get the expected behavior. When an LLM request is in flight, `Cancel` aborts the request without leaving the TUI. When an overlay is open (output view, describe view, command palette, prefix picker, theme picker, completion menu, add-entry dialog, delete-confirmation), `Cancel` closes the overlay rather than the whole TUI.

### `Run`

| Field | Value |
| --- | --- |
| Config key | `run` |
| Display name | Run |
| Default key | `Enter` |
| Category | navigation |

Run the selected command (Enter).

The primary selection action. The behavior is mode-specific:

- History mode → stages the row's `command` for the parent shell, exits the TUI
- Notes mode (`@`) → stages `note_search edit-note <id>`; `@new <text>` quick-creates a daily-note entry
- Todo mode (`!`) → stages `$EDITOR +<LINE> <file>`; `!@new <text>` quick-creates a new todo
- Directories mode (`#`) → stages `cd <abs-path>` (and optionally focuses an existing workspace)
- Panes mode (`*`) → stages the per-pane or per-workspace focus command
- JIRA (`-`) → stages `open "<browse-url>"` (or `xdg-open` on Linux)
- Files (`~`) → stages `$EDITOR <abs-path>`
- Tags (`$`) → stages `$EDITOR +<LINE> <file>` (symbols from a `tags` file)
- CodeGraph (`&`) → stages `$EDITOR +<LINE> <file>` (symbols from the `.codegraph` index)
- ag (`,`) → stages `$EDITOR +<LINE> <file>` (matched lines)
- LLM (`=`) → fires the LLM command-generation request
- Question (`%`) → fires the LLM question request

Every staged selection is space-prefixed before exiting, **except in history mode** (no prefix), where the command is staged as-is so it's recorded in the smarthistory DB — replaying from history is a command the user wants to record (keeps frequency stats and `Ctrl-S` suggestions accurate). See [Privacy convention](modes/README.md#privacy-convention-space-prefix).

### `EditStart`

| Field | Value |
| --- | --- |
| Config key | `edit-start` |
| Display name | Edit (cursor at start) |
| Default key | `none` |
| Category | navigation |

Prefill the line for editing, cursor at the start (Left). Unbound by default — users who prefer a dedicated "edit then jump to start" key can rebind via `key.edit-start=<spec>`.

### `EditEnd`

| Field | Value |
| --- | --- |
| Config key | `edit-end` |
| Display name | Edit (cursor at end) |
| Default key | `none` |
| Category | navigation |

Prefill the line for editing, cursor at the end (Right). Unbound by default.

### `Up`

| Field | Value |
| --- | --- |
| Config key | `up` |
| Display name | Up |
| Default key | `Up` |
| Category | navigation |

Move the cursor up in the list (Up). The result list is rendered bottom-aligned newest-first, so `Up` visually moves up the list (toward older entries).

### `Down`

| Field | Value |
| --- | --- |
| Config key | `down` |
| Display name | Down |
| Default key | `Down` |
| Category | navigation |

Move the cursor down in the list (Down).

### `MoveCursorLeft`

| Field | Value |
| --- | --- |
| Config key | `move-cursor-left` |
| Display name | Move cursor left |
| Default key | `Left` |
| Category | navigation |

Move the cursor one character to the left inside the search query (Left). The query string itself is unchanged; only the cursor position moves. The cursor is clamped at position 0. Only meaningful in LLM (`=`) mode — every other prefix mode keeps the cursor at the end.

### `MoveCursorRight`

| Field | Value |
| --- | --- |
| Config key | `move-cursor-right` |
| Display name | Move cursor right |
| Default key | `Right` |
| Category | navigation |

Move the cursor one character to the right inside the search query (Right). Clamped at the end of the query. Only meaningful in LLM mode.

### `PageUp`

| Field | Value |
| --- | --- |
| Config key | `page-up` |
| Display name | Page up |
| Default key | `PageUp` |
| Category | navigation |

Jump 10 rows up (PageUp). The jump distance is fixed at 10; on tall terminals this is less than a full page but predictable across window sizes.

### `PageDown`

| Field | Value |
| --- | --- |
| Config key | `page-down` |
| Display name | Page down |
| Default key | `PageDown` |
| Category | navigation |

Jump 10 rows down (PageDown).

### `Home`

| Field | Value |
| --- | --- |
| Config key | `home` |
| Display name | Home |
| Default key | `Home` |
| Category | navigation |

Jump to the oldest entry (Home). In the bottom-aligned newest-first layout, this scrolls to the top of the visible window.

### `End`

| Field | Value |
| --- | --- |
| Config key | `end` |
| Display name | End |
| Default key | `End` |
| Category | navigation |

Jump to the newest entry (End).

### `Backspace`

| Field | Value |
| --- | --- |
| Config key | `backspace` |
| Display name | Backspace |
| Default key | `Backspace` |
| Category | navigation |

Delete one character from the query (Backspace). The character to the left of the cursor is removed and the cursor moves back one position. In LLM mode the cursor can be mid-buffer so this respects the cursor position; in every other mode the cursor is at the end so this is equivalent to `pop()`.

### `DeleteWordBackward`

| Field | Value |
| --- | --- |
| Config key | `delete-word-backward` |
| Display name | Delete word backward |
| Default key | `C-w` (and `M-Backspace`) |
| Category | navigation |

Delete one word backward from the cursor position in the query (readline / bash `Ctrl-W` semantics). Trailing whitespace immediately before the cursor is eaten first; the cursor then walks left through non-whitespace until it hits another whitespace boundary. The action ships with two default keys so users from both the readline `C-w` muscle memory and the macOS `M-Backspace` muscle memory get the expected behavior.

### `PreviousHistory`

| Field | Value |
| --- | --- |
| Config key | `previous-history` |
| Display name | Previous history entry |
| Default key | `C-p` |
| Category | navigation |

Navigate to the previous (older) entry in the current mode's input history. Readline `previous-history` semantics, scoped to the active prefix mode — pressing `C-p` in `&` mode recalls past `&` queries only, not all-mode history. From the live query: saves the in-progress query as a "draft" and shows the most recent history entry; further `C-p` presses move toward older entries; `C-n` past the newest restores the draft; any keystroke that edits the recalled query commits it. See [Per-mode query history (C-n / C-p)](modes/README.md#privacy-convention-space-prefix).

Was forced off the historical `CycleThemePrev` (`C-p`) default to free the key for history recall. Theme cycling now ships unbound; rebind via `key.cycle-theme-prev=<spec>` (e.g. `M-p`).

### `NextHistory`

| Field | Value |
| --- | --- |
| Config key | `next-history` |
| Display name | Next history entry |
| Default key | `C-n` |
| Category | navigation |

Navigate to the next (newer) entry in the current mode's input history. Mirror of `PreviousHistory`. Was forced off the historical `CycleThemeNext` (`C-n`) default.

---

## search

### `CycleMode`

| Field | Value |
| --- | --- |
| Config key | `cycle-mode` |
| Display name | Cycle scope |
| Default key | `C-g` |
| Category | search |

Cycle the search scope (SESS → DIR → GLOBAL → STATS → SESS). Only meaningful in history mode (no prefix) — the other prefix modes have their own per-mode filter behavior.

- `SESS` (session) — only rows captured in the current `$SMART_HISTORY_SESSION`
- `DIR` (directory) — only rows captured in the current working directory
- `GLOBAL` — every row in the SQLite database
- `STATS` — the frequency / successor-prediction view (no rows; the list is replaced by a stats report)

### `ToggleDuplicateFilter`

| Field | Value |
| --- | --- |
| Config key | `toggle-duplicate-filter` |
| Display name | Toggle dedup |
| Default key | `none` |
| Category | search |

Toggle the duplicate filter. When on (the default), the result list collapses every command with the same text to a single row (the most-recent instance). When off, every row appears verbatim — useful for finding commands that ran in a specific directory or session. Unbound by default; the project's config rebinds it. Implied ON when the sort order is `FREQ`.

### `CycleThemeNext` / `CycleThemePrev`

| Field | Value |
| --- | --- |
| Config keys | `cycle-theme-next`, `cycle-theme-prev` |
| Display names | Next theme, Previous theme |
| Default keys | `none` (both) |
| Category | theme |

Cycle to the next / previous theme. Ships unbound by default — the `C-n` / `C-p` keys are now claimed by `NextHistory` / `PreviousHistory` (the per-mode query-history recall). Users who want keyboard theme cycling can rebind via `key.cycle-theme-next=...` / `key.cycle-theme-prev=...` (e.g. `M-n` / `M-p` are free and a natural mnemonic).

### `ClearQuery`

| Field | Value |
| --- | --- |
| Config key | `clear-query` |
| Display name | Clear query |
| Default key | `C-u` |
| Category | search |

Clear the search query (readline `Ctrl-U` semantics). The cursor is reset to position 0. If a prefix mode is active, the leading prefix char is preserved (the user stays in the same mode with an empty body — they don't fall back to plain history mode).

### `CycleExitFilter`

| Field | Value |
| --- | --- |
| Config key | `cycle-exit-filter` |
| Display name | Cycle exit filter |
| Default key | `C-j` |
| Category | search |

Cycle the exit-code filter: `all` (default) → `ok` (exit 0 only) → `nonzero` (non-zero exits only) → `all`. The chip in the mode strip shows the active filter.

### `CycleSortOrder`

| Field | Value |
| --- | --- |
| Config key | `cycle-sort-order` |
| Display name | Cycle sort order |
| Default key | `F4` |
| Category | search |

Cycle the sort order of the history list: `AGE` (newest first — the default) → `FREQ` (most-run first) → `AGE`. Frequency sort implicitly enables the duplicate filter (showing the same command N times would dominate the list otherwise). The current order is persisted in the session file and restored on the next TUI invocation.

### `CycleDirectorySource`

| Field | Value |
| --- | --- |
| Config key | `cycle-directory-source` |
| Display name | Cycle directory source |
| Default key | `C-s` |
| Category | search |

Cycle the directory-source filter for `#` (directories) mode: `ALL` → `TMUX` (only directories with an active multiplexer pane) → `CFG` (only `sessiondirs=...` config entries) → `ALL`. The current source is shown in the mode strip as a chip.

### `ToggleSearchMode`

| Field | Value |
| --- | --- |
| Config key | `toggle-search-mode` |
| Display name | Toggle search mode |
| Default key | `C-f` |
| Category | search |

Toggle between substring, fuzzy, and regex match algorithms. Cycle: Substring → Fuzzy → Regex → Substring. Applied to all prefix modes (history, directories, panes, notes, etc.) except JIRA — JIRA's server-side JQL parsing is its own thing. The active algorithm is shown as a `· algoname` suffix in the input border title.

### `PickPrefix`

| Field | Value |
| --- | --- |
| Config key | `pick-prefix` |
| Display name | Pick prefix mode |
| Default key | `F1` |
| Category | search |

Open the prefix picker. Centred overlay listing every configured prefix mode (history, output, LLM, question, notes, todo, directories, panes, JIRA, files, tags, codegraph, ag). Up/Down navigates, Enter applies the selected prefix to the current query, Esc closes. Useful when the user has rebound a prefix char and forgotten what it is.

---

## todo

### `MarkTodoDone`

| Field | Value |
| --- | --- |
| Config key | `mark-todo-done` |
| Display name | Mark todo done |
| Default key | `none` |
| Category | todo |

Mark the currently-selected todo entry as done inside its note file. Available only when the active query is a todo search (`!...`); outside of todo mode the action is a no-op with a status message so the user knows why their key did nothing. Ships **unbound by default** — the functionality is reachable via `SmartOpen` (`Ctrl-]` by default) in `!` mode. Users who want a dedicated key can rebind via `key.mark-todo-done=<spec>` (e.g. `C-x` restores the historical binding).

The implementation reads the line on disk, replaces `[ ]` with `[x]`, writes the file back, and refreshes the in-memory `todo_entries` table via `note_search::update_files_in_db` so the row disappears from the list on the next render.

---

## theme

See [`CycleThemeNext` / `CycleThemePrev`](#cyclethemrnext--cyclethemeprev) above (categorized as `theme`).

---

## tools

### `EditComment`

| Field | Value |
| --- | --- |
| Config key | `edit-comment` |
| Display name | Edit comment |
| Default key | `C-e` |
| Category | tools |

Start editing the comment of the selected entry. The comment is a free-form annotation that survives across sessions and applies to every row with the same command text. Switches the input box to a `comment>` prompt; `Enter` commits, `Esc` cancels. In JIRA mode, the comment editor doubles as the JIRA add-comment composer (keyed on `jira_add_comment_target` being set).

### `ShowOutput`

| Field | Value |
| --- | --- |
| Config key | `show-output` |
| Display name | Show output |
| Default key | `C-o` |
| Category | tools |

Open the captured-output view. For a JIRA row, fires the background comments fetch (a separate API call to `/rest/api/2/issue/{key}/comment`) and shows the description + every comment sorted newest-first. For every other mode, opens the full scrollable captured-output overlay.

### `OpenHelp`

| Field | Value |
| --- | --- |
| Config key | `open-help` |
| Display name | Open help |
| Default key | `C-a` |
| Category | tools |

Open the help overlay. Lists every search mode, the common actions, and the live key bindings (so rebinds via the config file are reflected immediately). `Esc` / `Enter` / `q` / `Ctrl-C` close it.

### `CommandAction`

| Field | Value |
| --- | --- |
| Config key | `command-action` |
| Display name | Command palette |
| Default key | `C-q` |
| Category | tools |

Open the command palette. A menu where the user can pick any action by name, with its current binding displayed. Useful when the user has forgotten (or rebound) a shortcut. Typing filters the list (case-insensitive substring AND); Up/Down navigates, Enter runs the highlighted action, Esc closes.

### `ThemePicker`

| Field | Value |
| --- | --- |
| Config key | `theme-picker` |
| Display name | Theme picker |
| Default key | `T` |
| Category | tools |

Open the theme picker. Lists every available theme (manual + built-in). Navigating the list applies the theme live (so the user sees the effect immediately), Enter commits, Esc reverts to the original theme. A preview pane on the right shows the live palette in action.

### `YankSelection`

| Field | Value |
| --- | --- |
| Config key | `yank-selection` |
| Display name | Yank selection |
| Default key | `C-y` |
| Category | tools |

Copy the current selection to the system clipboard. The "selection" picks the most useful thing to copy at the moment: if the captured-output view is open, the output text is copied; otherwise the selected history row's `command` is copied. The default `C-y` is the canonical readline / vim "yank" shortcut.

### `EditFileReference`

| Field | Value |
| --- | --- |
| Config key | `edit-file-reference` |
| Display name | Edit referenced file |
| Default key | `C-v` |
| Category | tools |

Find a filename referenced in the selected history row and stage `$EDITOR <filename>` as the next selection. The pick algorithm tokenizes the row's command, discards tokens containing shell metacharacters (globs, redirects, subshells, …), scores the rest by how "path-like" each looks, and the highest-scoring token wins. A no-op with a status message is surfaced when no row is selected or no filename-shaped token is found.

### `DownloadJiraIssue`

| Field | Value |
| --- | --- |
| Config key | `download-jira-issue` |
| Display name | Download JIRA issue as note |
| Default key | `C-M-s` |
| Category | tools |

Download the selected JIRA issue as a markdown file via `note_search jira-issue <KEY>`. Only meaningful in JIRA search mode (`-...`) where the selected row's `command` field carries the issue key. The downloaded note becomes searchable in [`@` (Notes) mode](modes/notes.md) immediately.

### `JiraFieldComplete`

| Field | Value |
| --- | --- |
| Config key | `jira-field-complete` |
| Display name | JIRA field complete |
| Default key | `Tab` |
| Category | tools |

Tab-completion for JQL field names inside the `-` mode. When the user has typed a token that matches the prefix of one or more JIRA field names (e.g. `lab<TAB>`), the token is expanded to the full field name (e.g. `labels=`). Multiple matches open the completion menu; the user picks from the candidates. Also handles `@`-prefixed alias / fragment completion (`@m<TAB>` → `@me`).

Cross-mode: in `@` (Notes) and `!` (Todo) modes, the same key dispatches to tag / link completion. See [`docs/modes/jira.md`](modes/jira.md) for the full table.

### `SmartOpen`

| Field | Value |
| --- | --- |
| Config key | `smart-open` |
| Display name | Smart open (context dive) |
| Default key | `C-]` |
| Category | tools |

Context-aware "dive" key — a single binding that adapts to the active prefix mode:

| Active mode | SmartOpen behavior |
| --- | --- |
| `&` / `$` (codegraph-backed symbol) | opens the callers / callees picker (`CodegraphRelations`) |
| `-` (JIRA) | opens the selected issue's browse URL in the system browser **in the background** (same as pressing Enter, but spawned detached so the TUI stays open) |
| `!` (Todo) | toggles the checkbox of the selected todo (same as `MarkTodoDone`, reusing the shared `mark_todo_done` helper) |
| `~` (Files) | opens the selected file with a per-extension shell command configured via `smart-open.<ext>=<cmd>` in the config file (with an optional `smart-open.default` fallback) |
| every other mode | falls through to `Run` (select row / open editor / fire LLM) — an ergonomic Enter replacement |

The default `C-]` (ASCII GS, 0x1D) is a single-byte control char every terminal emits reliably. Chosen over the more semantic `S-Return` because many terminals emit Shift-Return as a non-standard sequence crossterm 0.29 can't decode. Users on kitty-protocol terminals (Kitty / WezTerm / Alacritty / iTerm2+CSI-u) who prefer Shift-Return can rebind via `key.smart-open=S-Return` in the config file.

---

## llm

### `Describe`

| Field | Value |
| --- | --- |
| Config key | `describe` |
| Display name | Describe selected command |
| Default key | `C-k` |
| Category | llm |

Ask the local ollama instance for a short description (at most four sentences) of what the selected history line does, and show the response in a full-screen overlay. The result is *not* inserted into the history — it's a one-shot read. Requires `ollama.url` and `ollama.model` to be configured.

### `Correct`

| Field | Value |
| --- | --- |
| Config key | `correct` |
| Display name | Correct selected command |
| Default key | `C-t` |
| Category | llm |

Ask the local ollama instance to correct a malformed selected history line, returning a syntactically valid command that preserves the user's intent. The result opens in a modal overlay showing the original and the corrected versions side-by-side; `Enter` stages the corrected version, `Esc` cancels. Requires `ollama.url` and `ollama.model`.

---

## delete

### `DeleteSelected`

| Field | Value |
| --- | --- |
| Config key | `delete-selected` |
| Display name | Delete entry |
| Default key | `C-d` |
| Category | delete |

Delete the selected entry (with confirmation). Opens a `y / n` confirmation overlay; `y` commits the delete, `n` / `Esc` / `Ctrl-C` cancels. The deleted row's captured output (`history_output`) and comment (`command_comments`) are also cleaned up if no other history row references the same command.

### `DeleteMatching`

| Field | Value |
| --- | --- |
| Config key | `delete-matching` |
| Display name | Delete matches |
| Default key | `none` |
| Category | delete |

Delete all entries matching the current query (with confirmation). Unbound by default — users who want a "delete every match" key can rebind via `key.delete-matching=<spec>`. The confirmation dialog shows the match count so the user can verify before committing.

---

## config

### `AddSession`

| Field | Value |
| --- | --- |
| Config key | `add-session` |
| Display name | Add selected directory as a session |
| Default key | `F5` |
| Category | config |

Add the selected row's directory as a new `session.<id>` entry in the config file. Opens a multi-field dialog (Name, Dir, Exec) that writes the entry to `~/.config/smarthistory/config` and reloads the in-memory session list. The new session appears in the `*` panes view under a `# sessions` header.

### `AddHost`

| Field | Value |
| --- | --- |
| Config key | `add-host` |
| Display name | Add selected directory as a host |
| Default key | `F6` |
| Category | config |

Add the selected row's directory as a new `host.<id>` entry in the config file. Opens a multi-field dialog (Name, Host, Hostname, User, Port, Identity, Exec) that writes the entry and reloads the in-memory host list. The new host appears in the `*` panes view under a `# hosts` header.

---

## panes

These three actions only fire inside `*` (panes) mode; they're no-ops with a status message outside it. Outside of panes mode, they don't interfere with anything else.

### `FilterPanesWindows`

| Field | Value |
| --- | --- |
| Config key | `filter-panes-windows` |
| Display name | Filter panes: windows only |
| Default key | `F7` |
| Category | panes |

Filter the `*`-mode panes view to show only live multiplexer panes (hide `# sessions` and `# hosts`). Pressing the key again (when already filtered to Windows) resets to `All`.

### `FilterPanesHosts`

| Field | Value |
| --- | --- |
| Config key | `filter-panes-hosts` |
| Display name | Filter panes: hosts only |
| Default key | `F8` |
| Category | panes |

Filter the `*`-mode panes view to show only the `# hosts` block. Pressing the key again resets to `All`.

### `FilterPanesSessions`

| Field | Value |
| --- | --- |
| Config key | `filter-panes-sessions` |
| Display name | Filter panes: sessions only |
| Default key | `F9` |
| Category | panes |

Filter the `*`-mode panes view to show only the `# sessions` block. Pressing the key again resets to `All`.

---

## layout

### `TogglePaneVisibility`

| Field | Value |
| --- | --- |
| Config key | `toggle-pane-visibility` |
| Display name | Toggle pane visibility |
| Default key | `F10` |
| Category | layout |

Toggle detail pane visibility. Cycles through: `BOTH` (details + output preview side-by-side) → `Details` only → `Output Preview` only → `BOTH`. When only one pane is visible, the remaining pane uses the full detail-row height — useful on narrow terminals where the side-by-side layout would be cramped.

---

## codegraph

### `CodegraphRelations`

| Field | Value |
| --- | --- |
| Config key | `codegraph-relations` |
| Display name | Browse callers / callees |
| Default key | `C-r` |
| Category | codegraph |

Open a navigable picker listing the CodeGraph callers and callees of the currently selected `&` / `$` (codegraph-backed) symbol. Up/Down move, Enter opens the highlighted relation's source file in `$EDITOR` at its line (and exits the TUI), Esc closes. Only meaningful in codegraph / tags(fallback) mode and when the selected row carries a CodeGraph node id; otherwise a no-op with a status message.

The picker is populated from the `edges` table in `.codegraph/codegraph.db` (`kind='calls'`, `target=<node-id>` for callers, `source=<node-id>` for callees). Each section is capped at 50 entries. See [`docs/modes/codegraph.md`](modes/codegraph.md) for the full reference.

`SmartOpen` (`Ctrl-]`) also opens the same picker when the active mode is `&` / `$` — the two keys are interchangeable for codegraph-backed rows. `SmartOpen` is the cross-mode "dive" key the user is most likely to be holding; `CodegraphRelations` is the explicit, dedicated shortcut.

---

## See also

- [`docs/modes/README.md`](modes/README.md) — the per-prefix-mode reference (one markdown file per mode).
- [`docs/multiplexer.md`](multiplexer.md) — tmux + herdr backend support.
- [`docs/configuration.md`](configuration.md) — the full config-file reference (every `key.<action>`, `prefix.<name>`, `tuicolor.*`, `capturelines.*`, `smart-open.*`, `jira.search.*`, `session.*`, `host.*`, `notes.*`, `ollama.*`, and env-var override).
- [`README.md`](../README.md#tui-key-bindings-subset) — the high-level key bindings table.
- [`TECHNICAL.md`](../TECHNICAL.md) — the implementation reference (the `MultiplexerBackend` trait, the `Action` enum, the config parser, etc.).
- [`src/tui/bindings.rs`](../src/tui/bindings.rs) — the canonical source for the `Action` enum and the `config_key` / `display_name` / `default_key` / `category` methods.
