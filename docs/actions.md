# TUI Actions

Every key the TUI responds to is bound to an **action**. An action is a named,
configurable behavior — pick any action by name from the command palette
(`Ctrl-Q` by default), or rebind it via `key.<action>=<spec>` in
`~/.config/smarthistory/config`. This file lists every action in `ALL_ACTIONS`
(in the order the command palette uses), grouped by the
[`Action::category`](../src/tui/bindings.rs) field.

The canonical source is [`src/tui/bindings.rs`](../src/tui/bindings.rs) — the
Rust enum `Action` plus the `config_key()` / `display_name()` / `default_key()`
/ `category()` methods. This file mirrors that source; if they drift, the live
overlay (`Ctrl-A`) is what runs in production, and this doc becomes
documentation debt.

## How actions, keys, and modes interact

| Concept         | What it is                                                                                                                                                                                                                                                                                                                |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Action**      | A named behavior (e.g. `Cancel`, `Run`, `SmartOpen`). 65 actions ship in `ALL_ACTIONS`. Each has a stable kebab-case `config_key` for the config file, a `display_name` for the palette / status messages, a `default_key` (or `"none"` for unbound-by-default), and a `category`.                                        |
| **Key binding** | The mapping from a `KeySpec` (e.g. `C-c`, `F1`, `Up`) to an action. Multiple keys can map to the same action (`delete-word-backward` ships with both `C-w` and `M-Backspace`). The same key can't map to two actions — the first one in `ALL_ACTIONS` order wins (see [`KeyBindings::defaults`](../src/tui/bindings.rs)). |
| **Mode**        | The active prefix mode (history, output, `/`, `$`, `&`, etc. — see [`docs/modes/`](modes/README.md)). Most actions work in every mode; a few are mode-specific (`MarkTodoDone` is a no-op outside `!` mode, `JiraFieldComplete` only completes inside `-`, `CodegraphRelations` is meaningful only in `&` / `$`).         |
| **Overlay**     | When an overlay is open (command palette, prefix picker, theme picker, completion menu, help, output view, describe view, add-entry dialog, note/todo compose dialog, delete-confirmation), it captures key routing until it closes; the global actions don't fire underneath it.                                         |

## Config key syntax

`key.<action>=<spec>` where `<spec>` is a `KeySpec`:

| Spec form   | Example                                                                             | Meaning                                                           |
| ----------- | ----------------------------------------------------------------------------------- | ----------------------------------------------------------------- |
| `C-<key>`   | `C-c`                                                                               | Ctrl + key                                                        |
| `M-<key>`   | `M-h`                                                                               | Alt + key                                                         |
| `S-<key>`   | `S-Return`                                                                          | Shift + key (use `BackTab` for Shift-Tab)                         |
| `C-M-<key>` | `C-M-s`                                                                             | Ctrl + Alt + key (modifiers compose in any order)                 |
| `<named>`   | `Up`, `PageDown`, `F1`, `Insert`, `Backspace`, `Esc`, `Enter`, `Tab`, `Home`, `End` | Named special key                                                 |
| `<char>`    | `T`                                                                                 | A single character                                                |
| `none`      | `none`                                                                              | Unbind (the action ships unbound; rebinding is the user's choice) |

Multiple specs for one action are comma-separated: `key.cancel=C-c,Esc`.

See `parse_key_spec_opt` in [`src/tui/bindings.rs`](../src/tui/bindings.rs) for
the full grammar. Unknown key specs are dropped with a stderr warning; the rest
of the config still loads.

## Categories

Actions are grouped in the command palette by their `category()`:

| Category                    | Actions                                                                                                                                                                                                        |
| --------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`navigation`](#navigation) | Cancel, Run, EditStart, EditEnd, Up, Down, MoveCursorLeft, MoveCursorRight, PageUp, PageDown, Home, End, Backspace, DeleteWordBackward, PreviousHistory, NextHistory, PreviousGlobalHistory, NextGlobalHistory |
| [`search`](#search)         | CycleMode, CycleNavPrefix, ToggleDuplicateFilter, CycleExitFilter, CycleSortOrder, CycleDirectorySource, ClearQuery, ToggleSearchMode, PickPrefix                                                              |
| [`todo`](#todo)             | MarkTodoDone                                                                                                                                                                                                   |
| [`theme`](#theme)           | CycleThemeNext, CycleThemePrev                                                                                                                                                                                 |
| [`tools`](#tools)           | EditComment, ShowOutput, OpenHelp, CommandAction, ThemePicker, YankSelection, EditFileReference, DownloadJiraIssue, DownloadJiraMatching, JiraFieldComplete, SmartOpen, PrefixHelp, ComposeNoteEntry, CreateNote, CreateJiraIssue, CreateJiraIssueFromTemplate |
| [`llm`](#llm)               | Describe, Correct                                                                                                                                                                                              |
| [`delete`](#delete)         | DeleteSelected, DeleteMatching, ToggleMark, ClearMarks, BulkDeleteMarked                                                                                                                                       |
| [`config`](#config)         | AddSession, AddHost                                                                                                                                                                                            |
| [`panes`](#panes)           | FilterPanesWindows, FilterPanesHosts, FilterPanesSessions                                                                                                                                                      |
| [`layout`](#layout)         | TogglePaneVisibility                                                                                                                                                                                           |
| [`codegraph`](#codegraph)   | CodegraphRelations                                                                                                                                                                                             |

---

## navigation

### `Cancel`

| Field        | Value      |
| ------------ | ---------- |
| Config key   | `cancel`   |
| Display name | Cancel     |
| Default key  | `C-c`      |
| Category     | navigation |

Close the TUI / cancel an ongoing operation.

`Cancel` has two default keys (`C-c` and `Esc`) so users from both the readline
/ bash `Ctrl-C` muscle memory and the GUI-editor `Esc` muscle memory get the
expected behavior. When an LLM request is in flight, `Cancel` aborts the request
without leaving the TUI. When an overlay is open (output view, describe view,
command palette, prefix picker, theme picker, completion menu, add-entry dialog,
delete-confirmation), `Cancel` closes the overlay rather than the whole TUI.

### `Run`

| Field        | Value      |
| ------------ | ---------- |
| Config key   | `run`      |
| Display name | Run        |
| Default key  | `Enter`    |
| Category     | navigation |

Run the selected command (Enter).

The primary selection action. The behavior is mode-specific:

- History mode → stages the row's `command` for the parent shell, exits the TUI
- Notes mode (`@`) → stages `note_search edit-note <id>`; `@new <text>`
  quick-creates a daily-note entry
- Todo mode (`!`) → stages `$EDITOR +<LINE> <file>`; `!@new <text>`
  quick-creates a new todo
- Directories mode (`#`) → stages `cd <abs-path>` (and optionally focuses an
  existing workspace)
- Panes mode (`*`) → stages the per-pane or per-workspace focus command
- JIRA (`-`) → stages `open "<browse-url>"` (or `xdg-open` on Linux)
- Files (`/`) → stages `$EDITOR <abs-path>`
- Tags (`$`) → stages `$EDITOR +<LINE> <file>` (symbols from a `tags` file)
- CodeGraph (`&`) → stages `$EDITOR +<LINE> <file>` (symbols from the
  `.codegraph` index)
- ag (`,`) → stages `$EDITOR +<LINE> <file>` (matched lines)
- LLM (`=`) → fires the LLM command-generation request
- Question (`?`) → fires the LLM question request

Every staged selection is space-prefixed before exiting, **except in history
mode** (no prefix), where the command is staged as-is so it's recorded in the
smarthistory DB — replaying from history is a command the user wants to record
(keeps frequency stats and `Ctrl-S` suggestions accurate). See
[Privacy convention](modes/README.md#privacy-convention-space-prefix).

### `EditStart`

| Field        | Value                  |
| ------------ | ---------------------- |
| Config key   | `edit-start`           |
| Display name | Edit (cursor at start) |
| Default key  | `none`                 |
| Category     | navigation             |

Prefill the line for editing, cursor at the start (Left). Unbound by default —
users who prefer a dedicated "edit then jump to start" key can rebind via
`key.edit-start=<spec>`.

### `EditEnd`

| Field        | Value                |
| ------------ | -------------------- |
| Config key   | `edit-end`           |
| Display name | Edit (cursor at end) |
| Default key  | `none`               |
| Category     | navigation           |

Prefill the line for editing, cursor at the end (Right). Unbound by default.

### `Up`

| Field        | Value      |
| ------------ | ---------- |
| Config key   | `up`       |
| Display name | Up         |
| Default key  | `Up`       |
| Category     | navigation |

Move the cursor up in the list (Up). The result list is rendered bottom-aligned
newest-first, so `Up` visually moves up the list (toward older entries).

### `Down`

| Field        | Value      |
| ------------ | ---------- |
| Config key   | `down`     |
| Display name | Down       |
| Default key  | `Down`     |
| Category     | navigation |

Move the cursor down in the list (Down).

### `MoveCursorLeft`

| Field        | Value              |
| ------------ | ------------------ |
| Config key   | `move-cursor-left` |
| Display name | Move cursor left   |
| Default key  | `Left`             |
| Category     | navigation         |

Move the cursor one character to the left inside the search query (Left). The
query string itself is unchanged; only the cursor position moves. The cursor is
clamped at position 0. Only meaningful in LLM (`=`) mode — every other prefix
mode keeps the cursor at the end.

### `MoveCursorRight`

| Field        | Value               |
| ------------ | ------------------- |
| Config key   | `move-cursor-right` |
| Display name | Move cursor right   |
| Default key  | `Right`             |
| Category     | navigation          |

Move the cursor one character to the right inside the search query (Right).
Clamped at the end of the query. Only meaningful in LLM mode.

### `PageUp`

| Field        | Value      |
| ------------ | ---------- |
| Config key   | `page-up`  |
| Display name | Page up    |
| Default key  | `PageUp`   |
| Category     | navigation |

Jump 10 rows up (PageUp). The jump distance is fixed at 10; on tall terminals
this is less than a full page but predictable across window sizes.

### `PageDown`

| Field        | Value       |
| ------------ | ----------- |
| Config key   | `page-down` |
| Display name | Page down   |
| Default key  | `PageDown`  |
| Category     | navigation  |

Jump 10 rows down (PageDown).

### `Home`

| Field        | Value      |
| ------------ | ---------- |
| Config key   | `home`     |
| Display name | Home       |
| Default key  | `Home`     |
| Category     | navigation |

Jump to the oldest entry (Home). In the bottom-aligned newest-first layout, this
scrolls to the top of the visible window.

### `End`

| Field        | Value      |
| ------------ | ---------- |
| Config key   | `end`      |
| Display name | End        |
| Default key  | `End`      |
| Category     | navigation |

Jump to the newest entry (End).

### `Backspace`

| Field        | Value       |
| ------------ | ----------- |
| Config key   | `backspace` |
| Display name | Backspace   |
| Default key  | `Backspace` |
| Category     | navigation  |

Delete one character from the query (Backspace). The character to the left of
the cursor is removed and the cursor moves back one position. In LLM mode the
cursor can be mid-buffer so this respects the cursor position; in every other
mode the cursor is at the end so this is equivalent to `pop()`.

### `DeleteWordBackward`

| Field        | Value                     |
| ------------ | ------------------------- |
| Config key   | `delete-word-backward`    |
| Display name | Delete word backward      |
| Default key  | `C-w` (and `M-Backspace`) |
| Category     | navigation                |

Delete one word backward from the cursor position in the query (readline / bash
`Ctrl-W` semantics). Trailing whitespace immediately before the cursor is eaten
first; the cursor then walks left through non-whitespace until it hits another
whitespace boundary. The action ships with two default keys so users from both
the readline `C-w` muscle memory and the macOS `M-Backspace` muscle memory get
the expected behavior.

### `PreviousHistory`

| Field        | Value                  |
| ------------ | ---------------------- |
| Config key   | `previous-history`     |
| Display name | Previous history entry |
| Default key  | `C-p`                  |
| Category     | navigation             |

Navigate to the previous (older) entry in the current mode's input history.
Readline `previous-history` semantics, scoped to the active prefix mode —
pressing `C-p` in `&` mode recalls past `&` queries only, not all-mode history.
From the live query: saves the in-progress query as a "draft" and shows the most
recent history entry; further `C-p` presses move toward older entries; `C-n`
past the newest restores the draft; any keystroke that edits the recalled query
commits it. See
[Per-mode query history (C-n / C-p)](modes/README.md#privacy-convention-space-prefix).

Was forced off the historical `CycleThemePrev` (`C-p`) default to free the key
for history recall. Theme cycling now ships unbound; rebind via
`key.cycle-theme-prev=<spec>` (e.g. `M-p`).

### `NextHistory`

| Field        | Value              |
| ------------ | ------------------ |
| Config key   | `next-history`     |
| Display name | Next history entry |
| Default key  | `C-n`              |
| Category     | navigation         |

Navigate to the next (newer) entry in the current mode's input history. Mirror
of `PreviousHistory`. Was forced off the historical `CycleThemeNext` (`C-n`)
default.

### `PreviousGlobalHistory`

| Field        | Value                                     |
| ------------ | ----------------------------------------- |
| Config key   | `previous-global-history`                 |
| Display name | Previous global history entry (all modes) |
| Default key  | `C-S-P`                                   |
| Category     | navigation                                |

Navigate to the previous (older) entry in the GLOBAL (cross-mode) query history
— every query submitted or abandoned across ALL prefix modes, in true
chronological order, not just the currently active one. Same readline recall
semantics as `PreviousHistory` (draft-saving, oldest-clamp, commit-on-edit),
just over one flat cross-mode list instead of a per-mode slice. Recalling an
entry restores its original leading prefix char, so the app switches back into
whatever mode that query was originally typed in — like `PreviousHistory`, this
only fills the query box for review/editing; it does not stage or run anything
itself.

Persisted separately from the per-mode history, to
`<db_dir>/global_query_history.json`.

Terminal support for `Ctrl+Shift+<letter>` as a distinct event from plain
`Ctrl+<letter>` varies — many legacy/non-kitty-protocol terminals can't tell
them apart (same limitation noted for `SmartOpen`'s `S-Return` alternative). If
your terminal can't produce a distinct `C-S-p` event, rebind via
`key.previous-global-history=<spec>`.

### `NextGlobalHistory`

| Field        | Value                                 |
| ------------ | ------------------------------------- |
| Config key   | `next-global-history`                 |
| Display name | Next global history entry (all modes) |
| Default key  | `C-S-N`                               |
| Category     | navigation                            |

Navigate to the next (newer) entry in the GLOBAL query history. Mirror of
`PreviousGlobalHistory`.

---

## search

### `CycleMode`

| Field        | Value        |
| ------------ | ------------ |
| Config key   | `cycle-mode` |
| Display name | Cycle scope  |
| Default key  | `C-g`        |
| Category     | search       |

Cycle the search scope (SESS → DIR → GLOBAL → STATS → SESS). Only meaningful in
history mode (no prefix) — the other prefix modes have their own per-mode filter
behavior.

- `SESS` (session) — only rows captured in the current `$SMART_HISTORY_SESSION`
- `DIR` (directory) — only rows captured in the current working directory
- `GLOBAL` — every row in the SQLite database
- `STATS` — the frequency / successor-prediction view (no rows; the list is
  replaced by a stats report)

### `CycleNavPrefix`

| Field        | Value                          |
| ------------ | ------------------------------ |
| Config key   | `cycle-nav-prefix`             |
| Display name | Cycle panes/directories/zoxide |
| Default key  | `C-z`                          |
| Category     | search                         |

Cycle directly between the three navigation prefix modes — `*` (panes), `#`
(directories), `~` (zoxide) — in that order, without going through the full
`PickPrefix` picker. Reads the actual configured prefix chars
(`prefix.panes`/`prefix.directories`/`prefix.zoxide`), so a remapped prefix
still cycles correctly. From any OTHER mode (plain history, another prefix mode,
or an empty query), jumps straight to panes — the first of the three — rather
than no-op-ing. The typed body (if any) is preserved across the switch, same as
picking a new mode from `PickPrefix` does.

### `ToggleDuplicateFilter`

| Field        | Value                     |
| ------------ | ------------------------- |
| Config key   | `toggle-duplicate-filter` |
| Display name | Toggle dedup              |
| Default key  | `none`                    |
| Category     | search                    |

Toggle the duplicate filter. When on (the default), the result list collapses
every command with the same text to a single row (the most-recent instance).
When off, every row appears verbatim — useful for finding commands that ran in a
specific directory or session. Unbound by default; the project's config rebinds
it. Implied ON when the sort order is `FREQ`.

### `CycleThemeNext` / `CycleThemePrev`

| Field         | Value                                  |
| ------------- | -------------------------------------- |
| Config keys   | `cycle-theme-next`, `cycle-theme-prev` |
| Display names | Next theme, Previous theme             |
| Default keys  | `none` (both)                          |
| Category      | theme                                  |

Cycle to the next / previous theme. Ships unbound by default — the `C-n` / `C-p`
keys are now claimed by `NextHistory` / `PreviousHistory` (the per-mode
query-history recall). Users who want keyboard theme cycling can rebind via
`key.cycle-theme-next=...` / `key.cycle-theme-prev=...` (e.g. `M-n` / `M-p` are
free and a natural mnemonic).

### `ClearQuery`

| Field        | Value         |
| ------------ | ------------- |
| Config key   | `clear-query` |
| Display name | Clear query   |
| Default key  | `C-u`         |
| Category     | search        |

Clear the search query (readline `Ctrl-U` semantics). The cursor is reset to
position 0. If a prefix mode is active, the leading prefix char is preserved
(the user stays in the same mode with an empty body — they don't fall back to
plain history mode).

### `CycleExitFilter`

| Field        | Value               |
| ------------ | ------------------- |
| Config key   | `cycle-exit-filter` |
| Display name | Cycle exit filter   |
| Default key  | `C-j`               |
| Category     | search              |

Cycle the exit-code filter: `all` (default) → `ok` (exit 0 only) → `nonzero`
(non-zero exits only) → `all`. The chip in the mode strip shows the active
filter.

### `CycleSortOrder`

| Field        | Value              |
| ------------ | ------------------ |
| Config key   | `cycle-sort-order` |
| Display name | Cycle sort order   |
| Default key  | `F4`               |
| Category     | search             |

Cycle the sort order of the history list: `AGE` (newest first — the default) →
`FREQ` (most-run first) → `AGE`. Frequency sort implicitly enables the duplicate
filter (showing the same command N times would dominate the list otherwise). The
current order is persisted in the session file and restored on the next TUI
invocation.

### `CycleDirectorySource`

| Field        | Value                    |
| ------------ | ------------------------ |
| Config key   | `cycle-directory-source` |
| Display name | Cycle directory source   |
| Default key  | `C-s`                    |
| Category     | search                   |

Cycle the directory-source filter for `#` (directories) mode: `ALL` → `TMUX`
(only directories with an active multiplexer pane) → `CFG` (only
`sessiondirs=...` config entries) → `ALL`. The current source is shown in the
mode strip as a chip.

### `ToggleSearchMode`

| Field        | Value                |
| ------------ | -------------------- |
| Config key   | `toggle-search-mode` |
| Display name | Toggle search mode   |
| Default key  | `C-f`                |
| Category     | search               |

Toggle between substring, fuzzy, and regex match algorithms. Cycle: Substring →
Fuzzy → Regex → Substring. Applied to all prefix modes (history, directories,
panes, notes, etc.) except JIRA — JIRA's server-side JQL parsing is its own
thing. The active algorithm is shown as a `· algoname` suffix in the input
border title.

### `PickPrefix`

| Field        | Value            |
| ------------ | ---------------- |
| Config key   | `pick-prefix`    |
| Display name | Pick prefix mode |
| Default key  | `F1`             |
| Category     | search           |

Open the prefix picker. Centred overlay listing every configured prefix mode
(history, output, LLM, question, notes, todo, directories, panes, JIRA, files,
tags, codegraph, ag). Up/Down navigates, Enter applies the selected prefix to
the current query, Esc closes. Useful when the user has rebound a prefix char
and forgotten what it is.

---

## todo

### `MarkTodoDone`

| Field        | Value            |
| ------------ | ---------------- |
| Config key   | `mark-todo-done` |
| Display name | Mark todo done   |
| Default key  | `none`           |
| Category     | todo             |

Mark the currently-selected todo entry as done inside its note file (or, via
`SmartOpen`, every marked todo — see below). Available only when the active
query is a todo search (`!...`); outside of todo mode the action is a no-op with
a status message so the user knows why their key did nothing. Ships **unbound by
default** — the functionality is reachable via `SmartOpen` (`Ctrl-]` by default)
in `!` mode, which additionally acts on every marked row when at least one is
marked. Users who want a dedicated key can rebind via
`key.mark-todo-done=<spec>`; note that `C-x`, the historical default, is now
`ToggleMark`'s default key, so pick a different spec.

The implementation reads the line on disk, replaces `[ ]` with `[x]`, writes the
file back, and refreshes the in-memory `todo_entries` table via
`note_search::update_files_in_db` so the row disappears from the list on the
next render.

---

## theme

See [`CycleThemeNext` / `CycleThemePrev`](#cyclethemrnext--cyclethemeprev) above
(categorized as `theme`).

---

## tools

### `EditComment`

| Field        | Value          |
| ------------ | -------------- |
| Config key   | `edit-comment` |
| Display name | Edit comment   |
| Default key  | `C-e`          |
| Category     | tools          |

Start editing the comment of the selected entry. The comment is a free-form
annotation that survives across sessions and applies to every row with the same
command text. Switches the input box to a `comment>` prompt; `Enter` commits,
`Esc` cancels. In JIRA mode, the comment editor doubles as the JIRA add-comment
composer (keyed on `jira_add_comment_target` being set).

### `ShowOutput`

| Field        | Value         |
| ------------ | ------------- |
| Config key   | `show-output` |
| Display name | Show output   |
| Default key  | `C-o`         |
| Category     | tools         |

Open the captured-output view. For a JIRA row, fires the background comments
fetch (a separate API call to `/rest/api/2/issue/{key}/comment`) and shows the
description + every comment sorted newest-first. For every other mode, opens the
full scrollable captured-output overlay.

### `OpenHelp`

| Field        | Value       |
| ------------ | ----------- |
| Config key   | `open-help` |
| Display name | Open help   |
| Default key  | `C-a`       |
| Category     | tools       |

Open the help overlay. Lists every search mode, the common actions, and the live
key bindings (so rebinds via the config file are reflected immediately). `Esc` /
`Enter` / `q` / `Ctrl-C` close it.

### `CommandAction`

| Field        | Value            |
| ------------ | ---------------- |
| Config key   | `command-action` |
| Display name | Command palette  |
| Default key  | `C-q`            |
| Category     | tools            |

Open the command palette. A menu where the user can pick any action by name,
with its current binding displayed. Useful when the user has forgotten (or
rebound) a shortcut. Typing filters the list (case-insensitive substring AND);
Up/Down navigates, Enter runs the highlighted action, Esc closes.

Three aligned columns: description (display name — word-wrapped onto extra
lines when it doesn't fit the column, rather than truncated), key binding,
and the internal action name (its `config_key`, useful for writing
`key.<name>=<spec>` bindings). Row order is most-recently-used first: running
an action from the palette (Enter, not any other keybinding) moves it to the
top of the list for next time — deduplicated, so re-running the same action
doesn't create a second entry. Actions never run from the palette keep
`ALL_ACTIONS`' own declaration order further down. This order is saved to
`~/.local/cache/smarthistory/session` (`commandmenurecent=<config_key>` lines,
one per entry, most-recent-first) on exit and reloaded on the next TUI
launch, so it survives across restarts — the same session file `theme=`/
`sortorder=`/etc. already live in. An unrecognized entry (e.g. from an
action that's since been renamed or removed) is silently dropped on load
rather than treated as an error.

The palette's own housekeeping keys (`Cancel`, `ClearQuery`, `Run`) and raw
query-cursor editing (`EditStart`/`EditEnd`/`MoveCursorLeft`/`MoveCursorRight`/
`Home`/`End`/`Backspace`/`DeleteWordBackward`) always sink to the very bottom
of the list, even if run recently — they're not things anyone browses the
palette looking for.

### `ThemePicker`

| Field        | Value          |
| ------------ | -------------- |
| Config key   | `theme-picker` |
| Display name | Theme picker   |
| Default key  | `T`            |
| Category     | tools          |

Open the theme picker. Lists every available theme (manual + built-in).
Navigating the list applies the theme live (so the user sees the effect
immediately), Enter commits, Esc reverts to the original theme. A preview pane
on the right shows the live palette in action.

### `KeyBindingsEditor`

| Field        | Value                                                                     |
| ------------ | -------------------------------------------------------------------------- |
| Config key   | `key-bindings-editor`                                                     |
| Display name | Key bindings editor                                                       |
| Default key  | none (open it via the command palette, or bind `key.key-bindings-editor=<spec>`) |
| Category     | tools                                                                      |

Open the key-bindings editor: every action, filterable, with its current
binding shown — the same listing and column layout the command palette uses.
`Enter` on the highlighted action starts key-capture mode; the next keypress
becomes that action's new (sole) binding, applied immediately in memory and
persisted (best-effort) to the config file as `key.<config_key> = <spec>`.
`Delete` unbinds the highlighted action (writes `key.<config_key> = none`).
`Cancel` (`Esc` by default) during capture backs out to browsing without
changing anything; `Cancel` while browsing closes the editor.

If the captured key is already held by another action, the rebind is only
blocked when the two actions could genuinely compete for it — i.e. when
their [scopes](configuration.md) can both apply in some real prefix mode at
once (see `scopes_conflict`, mirrored from the same check
`smarthistory config check` uses). In that case a warning names the other
action; `y`/`Enter` binds anyway, `n`/`Backspace`/Cancel returns to capture
so you can try a different key. A key already held by an action scoped to a
different, mutually-exclusive prefix mode is never flagged — each fires only
in its own mode.

Rebinding always replaces an action's ENTIRE key list with the one just
captured; multi-key bindings (`key.foo=C-h, F1`) are still possible, just not
editable from this overlay — set them by hand in the config file.

### `YankSelection`

| Field        | Value            |
| ------------ | ---------------- |
| Config key   | `yank-selection` |
| Display name | Yank selection   |
| Default key  | `C-y`            |
| Category     | tools            |

Copy the current selection to the system clipboard. The "selection" picks the
most useful thing to copy at the moment: if the captured-output view is open,
the output text is copied; in `:` (Segments) or `"` (Similar) mode, the row's
**breadcrumb** (filename + ancestor headers' text) is copied instead of the
matched segment's own text (a segment's text can be a whole header-bounded
section joined onto one line, plus a `[score]` prefix in Similar mode, which is
less useful on the clipboard than knowing exactly which file/section it came
from); otherwise the selected history row's `command` is copied. The default
`C-y` is the canonical readline / vim "yank" shortcut.

### `EditFileReference`

| Field        | Value                 |
| ------------ | --------------------- |
| Config key   | `edit-file-reference` |
| Display name | Edit referenced file  |
| Default key  | `C-v`                 |
| Category     | tools                 |

Find a filename referenced in the selected history row and stage
`$EDITOR <filename>` as the next selection. The pick algorithm tokenizes the
row's command, discards tokens containing shell metacharacters (globs,
redirects, subshells, …), scores the rest by how "path-like" each looks, and the
highest-scoring token wins. A no-op with a status message is surfaced when no
row is selected or no filename-shaped token is found.

### `DownloadJiraIssue`

| Field        | Value                       |
| ------------ | --------------------------- |
| Config key   | `download-jira-issue`       |
| Display name | Download JIRA issue as note |
| Default key  | `C-M-s`                     |
| Category     | tools                       |

Download the selected JIRA issue as a markdown file via
`note_search jira-issue <KEY>`. Only meaningful in JIRA search mode (`-...`)
where the selected row's `command` field carries the issue key. The downloaded
note becomes searchable in [`@` (Notes) mode](modes/notes.md) immediately.

### `DownloadJiraMatching`

| Field        | Value                                      |
| ------------ | ------------------------------------------ |
| Config key   | `download-jira-matching`                   |
| Display name | Download all matching JIRA issues as notes |
| Default key  | `none` (unbound)                           |
| Category     | tools                                      |

Download **every** JIRA issue matching the current query, not just the selected
row, via `note_search jira <JQL>` — the `note_search` bulk import subcommand.
The JQL is the exact query the TUI already built for the live search (same
`@me`/`@today`/`@week`/`@month`/fragment/`JIRA_PROJECT` resolution as the
on-screen results). Unlike the in-TUI result list, this is NOT limited by
`JIRA_MAX_RESULTS`: `note_search` paginates the JIRA API itself, so the download
covers everything the query matches. Refuses to stage a command (with a status
message) when the query references an undefined `@fragment`, same as the live
search's own diagnostic. Ships unbound by default — same policy as
`DeleteMatching` — since a bulk action over everything the current query matches
deserves an explicit opt-in key; set `key.download-jira-matching=<spec>` to bind
one. Downloaded notes become searchable in [`@` (Notes) mode](modes/notes.md)
immediately.

### `JiraFieldComplete`

| Field        | Value                 |
| ------------ | --------------------- |
| Config key   | `jira-field-complete` |
| Display name | JIRA field complete   |
| Default key  | `Tab`                 |
| Category     | tools                 |

Tab-completion for JQL field names inside the `-` mode. When the user has typed
a token that matches the prefix of one or more JIRA field names (e.g.
`lab<TAB>`), the token is expanded to the full field name (e.g. `labels=`).
Multiple matches open the completion menu; the user picks from the candidates.
Also handles `@`-prefixed alias / fragment completion (`@m<TAB>` → `@me`).

Cross-mode: in `@` (Notes), `!` (Todo), `:` (Segments), and `"` (Similar) modes,
the same key dispatches to tag / link completion; in `<` (Paperless) mode, to
tag / correspondent completion; in `'` (meta-prefix) mode, to mode-NAME
completion — a unique match activates that mode directly (discarding the typed
`'<name>` text), an ambiguous match (or bare `'`) opens the `PickPrefix` picker
pre-filtered to the matching names. See [`docs/modes/jira.md`](modes/jira.md)
and
[`docs/modes/README.md#meta-prefix-mode--pick-a-mode-by-name`](modes/README.md#meta-prefix-mode--pick-a-mode-by-name)
for the full tables.

In `?` (Question) mode there's nothing to complete, so the same key submits the
question to the LLM instead — identical to pressing `Enter` (`Run`); see
[General question mode](../TECHNICAL.md#general-question-mode).

### `SmartOpen`

| Field        | Value                     |
| ------------ | ------------------------- |
| Config key   | `smart-open`              |
| Display name | Smart open (context dive) |
| Default key  | `C-]`                     |
| Category     | tools                     |

Context-aware "dive" key — a single binding that adapts to the active prefix
mode:

| Active mode                         | SmartOpen behavior                                                                                                                                                                                                                                      |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `&` / `$` (codegraph-backed symbol) | opens the callers / callees picker (`CodegraphRelations`) for the **selected** row only — a picker overlay can't show N rows' relations at once, so marks are ignored here                                                                              |
| `-` (JIRA)                          | opens every **marked** issue's browse URL in the system browser **in the background** (or just the selected one when nothing is marked) — same as pressing Enter on a single row, but spawned detached so the TUI stays open                            |
| `!` (Todo)                          | toggles the checkbox of every **marked** todo (or just the selected one when nothing is marked), reusing the shared `mark_todo_done_for_row` helper; reports an aggregate "Marked N of M todos done" when acting on more than one                       |
| `/` (Files)                         | stages one chained command (`cmd1 ; cmd2 ; ...`) covering every **marked** file that has a configured `smart-open.<ext>=<cmd>` mapping (or just the selected file when nothing is marked)                                                               |
| `^` (Browser)                       | converts the **selected** row's URL into a local markdown note (`note_search convert <url>`) instead of opening it (which is what plain `Enter` still does), then opens the freshly-created note in `$EDITOR`; single-row only, marks are not consulted |
| every other mode                    | falls through to `Run` (select row / open editor / fire LLM) — an ergonomic Enter replacement; acts on the selected row only, marks are not consulted                                                                                                   |

**Multi-select**: the JIRA, Todo, and Files branches act on every row marked via
`Action::ToggleMark` (`C-x` by default) when at least one row is marked, falling
back to just the currently selected row when nothing is marked. This is the
general "act on marks, else the selection" contract shared by
`App::smart_action_targets`. The overlay-opening codegraph/tags branch, the
browser-mode create-note branch, and the generic `Run` fallback are single-row
only — see the source doc comment on `smart_action_targets` in `src/tui.rs` for
why.

The default `C-]` (ASCII GS, 0x1D) is a single-byte control char every terminal
emits reliably. Chosen over the more semantic `S-Return` because many terminals
emit Shift-Return as a non-standard sequence crossterm 0.29 can't decode. Users
on kitty-protocol terminals (Kitty / WezTerm / Alacritty / iTerm2+CSI-u) who
prefer Shift-Return can rebind via `key.smart-open=S-Return` in the config file.

### `PrefixHelp`

| Field        | Value                     |
| ------------ | ------------------------- |
| Config key   | `prefix-help`             |
| Display name | Prefix query syntax help  |
| Default key  | `F3`                      |
| Category     | tools                     |

Open the prefix query-syntax help overlay — a cheatsheet of the QUERY SYNTAX
the active prefix mode accepts (`#tag`, `[[link]]`, `[attr:value]`, `(a OR b)`
grouping, negation, `!!type`/`!type`, etc.), distinct from `OpenHelp`'s
keyboard-shortcut reference. Resolves which mode to show from, in order: the
`PrefixPicker`'s highlighted row if it's open (so `F1` to browse, `F3` on a
highlighted entry shows that mode's syntax without switching to it), otherwise
the currently typed query's prefix. With neither (plain history mode, no
picker open), shows a one-line-per-prefix overview instead. `Esc` / `Enter` /
`q` / `Ctrl-C` close it; `Up`/`Down`/`PageUp`/`PageDown`/`Home`/`End` scroll.
See [`docs/modes/README.md`](modes/README.md) for the per-mode query syntax
this overlay's content is condensed from.

---

## llm

### `Describe`

| Field        | Value                     |
| ------------ | ------------------------- |
| Config key   | `describe`                |
| Display name | Describe selected command |
| Default key  | `C-k`                     |
| Category     | llm                       |

Ask the local ollama instance for a short description (at most four sentences)
of what the selected history line does, and show the response in a full-screen
overlay. The result is _not_ inserted into the history — it's a one-shot read.
Requires `ollama.url` and `ollama.model` to be configured.

### `Correct`

| Field        | Value                    |
| ------------ | ------------------------ |
| Config key   | `correct`                |
| Display name | Correct selected command |
| Default key  | `C-t`                    |
| Category     | llm                      |

Ask the local ollama instance to correct a malformed selected history line,
returning a syntactically valid command that preserves the user's intent. The
result opens in a modal overlay showing the original and the corrected versions
side-by-side; `Enter` stages the corrected version, `Esc` cancels. Requires
`ollama.url` and `ollama.model`.

---

## delete

### `DeleteSelected`

| Field        | Value             |
| ------------ | ----------------- |
| Config key   | `delete-selected` |
| Display name | Delete entry      |
| Default key  | `C-d`             |
| Category     | delete            |

Delete the selected entry (with confirmation). Opens a `y / n` confirmation
overlay; `y` commits the delete, `n` / `Esc` / `Ctrl-C` cancels. The deleted
row's captured output (`history_output`) and comment (`command_comments`) are
also cleaned up if no other history row references the same command.

### `DeleteMatching`

| Field        | Value             |
| ------------ | ----------------- |
| Config key   | `delete-matching` |
| Display name | Delete matches    |
| Default key  | `none`            |
| Category     | delete            |

Delete all entries matching the current query (with confirmation). Unbound by
default — users who want a "delete every match" key can rebind via
`key.delete-matching=<spec>`. The confirmation dialog shows the match count so
the user can verify before committing.

### `ToggleMark`

| Field        | Value                       |
| ------------ | --------------------------- |
| Config key   | `toggle-mark`               |
| Display name | Toggle mark on selected row |
| Default key  | `C-x`                       |
| Category     | delete                      |

Mark (or unmark) the currently selected row for a bulk action. Marked rows
render a `[x]` checkbox prefix (unmarked rows show `[ ]`); the status bar shows
the current mark count when non-zero. Marks are keyed by `HistoryRow::id` and
are cleared automatically whenever the active prefix mode changes (e.g.
switching from plain history to `!` todo mode) — synthetic ids from other prefix
modes aren't guaranteed unique across mode boundaries. Marks DO survive plain
query-text edits within the same mode. A no-op when no row is selected.

### `ClearMarks`

| Field        | Value           |
| ------------ | --------------- |
| Config key   | `clear-marks`   |
| Display name | Clear all marks |
| Default key  | `none`          |
| Category     | delete          |

Clear every mark without deleting anything. Unbound by default; reachable via
the command palette or `key.clear-marks=<spec>`. Surfaces a status message
reporting how many marks were cleared.

### `BulkDeleteMarked`

| Field        | Value                     |
| ------------ | ------------------------- |
| Config key   | `bulk-delete-marked`      |
| Display name | Delete all marked entries |
| Default key  | `none`                    |
| Category     | delete                    |

Delete every marked row (with confirmation) — same `y`/`n`/`Esc`/`Ctrl-C` dialog
machinery as `DeleteMatching`, deleting by the explicit marked-id list rather
than a derived query. Unbound by default, same policy as `DeleteMatching`: a
bulk destructive action deserves an explicit opt-in key
(`key.bulk-delete-marked=<spec>`). A status message explains the no-op when
nothing is marked.

---

## config

### `AddSession`

| Field        | Value                               |
| ------------ | ----------------------------------- |
| Config key   | `add-session`                       |
| Display name | Add selected directory as a session |
| Default key  | `F5`                                |
| Category     | config                              |

Add the selected row's directory as a new `session.<key>` entry. Opens a
multi-field dialog (Name, Dir, Exec) that writes the entry to
`~/.config/smarthistory/sessions` (creating the file if it doesn't exist yet)
and reloads the in-memory session list. The new session appears in the `*` panes
view under the `# Directories` header (renamed from `# sessions` — see
[`panes.md`](modes/panes.md#configured-groups-directories-and-hosts)).

### `AddHost`

| Field        | Value                            |
| ------------ | -------------------------------- |
| Config key   | `add-host`                       |
| Display name | Add selected directory as a host |
| Default key  | `F6`                             |
| Category     | config                           |

Add the selected row's directory as a new `host.<key>` entry. Opens a
multi-field dialog (Name, Host, Hostname, User, Port, Identity, Exec) that
writes the entry to `~/.config/smarthistory/hosts` (creating the file if it
doesn't exist yet) and reloads the in-memory host list. The new host appears in
the `*` panes view under a `# hosts` header.

If the selected row's command looks like an SSH/SCP/SFTP/rsync/mosh invocation
(`ssh root@122.1.1.40`, `scp file.txt user@host:/path`, …), Host (and User, when
present) is pre-filled straight from it instead of the directory basename — the
far more useful default when the row that prompted `F6` is the `ssh` command
itself, not some unrelated directory you happened to be in. Falls back to the
directory basename when the row doesn't look like one of those commands.

A bare target with no explicit user (`ssh machine`) fills Host with `machine`
and User with the current OS login — the same default `ssh` itself uses when no
`user@` is given. Command-line options are stripped out before this is decided
(along with their value, for a flag that takes a separate one — `-p`, `-i`,
`-o`, `-l`, `-F`, `-J`, and a handful of others), so
`ssh -p 2222 -i ~/.ssh/id_ed25519 root@machine` still fills Host with `machine`
and User with `root` — the flags don't count as "other words" in the way that
determines whether the bare-word form applies. This bare-word form is only
recognized when the target is the only thing left once flags are removed; a
genuine second _positional_ word (most commonly a remote command to run, e.g.
`ssh myserver uptime`) is still ambiguous and falls back to the
directory-basename default instead. `ssh root@122.1.1.40` and `ssh pve-1.local`
(dotted or IPv4) are recognized regardless of how many other words are present,
flags or otherwise — only a bare, undotted single-label host needs this
one-target check.

### `ComposeNoteEntry`

| Field        | Value                         |
| ------------ | ----------------------------- |
| Config key   | `compose-note-entry`          |
| Display name | Compose a new note/todo entry |
| Default key  | `F2`                          |
| Category     | tools                         |

Open a multi-line compose overlay for a new note (`@` mode) or todo (`!` mode)
entry — the answer to "the query line is too short for what I want to write."
Available only in Notes or Todo mode; a no-op with a status message elsewhere. A
second press while the dialog is already open keeps the existing buffer (doesn't
reset it).

Inside the dialog, `Enter` inserts a literal newline (the one place in the TUI
where `Enter` doesn't commit) rather than submitting; `Ctrl-S` saves and exits
(stages
`note_search create-note <text> --type daily --timestamp [--todo] --database <db>`
— the same command the single-line `@new <text>` / `!@new <text>` quick-create
stages, just fed the dialog's buffer instead of query text); `Esc` cancels
without staging anything; `Ctrl-U` clears the buffer; `Ctrl-W` deletes one word
backward (`'\n'` counts as whitespace, so it can cross a line boundary). A no-op
(dialog stays open, status message) when the buffer is empty/whitespace-only or
`notes.database` isn't configured.

Embedded newlines are re-indented (`"\n"` → `"\n  "`) before staging so the
committed entry stays a single valid markdown list item with indented
continuation lines — `note_search`'s `create-note` only knows how to format its
`text` argument onto ONE line of the list item (`- [prefix]<text>` for a note,
`- [ ] [prefix]<text> due: <date>` for a todo), so a raw unindented newline
would otherwise break the entry apart.

This is purely additive: the existing single-line `@new <text>` / `!@new <text>`
quick-create (typed directly on the query line, still stages and exits
immediately with no dialog) is completely unchanged and unaffected by this
action.

### `CreateNote`

| Field        | Value                                                                              |
| ------------ | ---------------------------------------------------------------------------------- |
| Config key   | `create-note`                                                                      |
| Display name | Create a new note (Title + Content)                                                |
| Default key  | none (open it via the command palette, `Ctrl-Q`, or bind `key.create-note=<spec>`) |
| Category     | tools                                                                              |

Open the two-field `create-note` dialog: a single-line **Title** and a
multi-line **Content**, `Tab` toggling between them (`Up`/`Down` move a line at
a time within Content, preserving column, but stay within the active field —
they don't cross into Title; `Tab` is the only way to switch fields). `Ctrl-S`
saves and exits — stages `note_search create-note <text> --type daily` (title +
links/tags extracted into a `### Heading` line, content as the body) the same
way `ComposeNoteEntry` above stages its own command. `Ctrl-O` does the same
save, then chains `$EDITOR <path>` onto the staged command so the daily note
opens right after — for when the dialog isn't enough room and you want to keep
writing. The target path is computed independently
(`notes_dir/daily/<year>/<month-abbrev>/<date>.md`, the same convention
`note_search`'s `create-note` uses internally) rather than asked back from the
CLI, since it doesn't report one. `Ctrl-A` selects the whole active field (Title
or Content, not a partial range) — the field's border turns the warning color
and gains a "SELECTED" marker, and its text renders in reverse video, so the
state is visually unmistakable. While selected, `Ctrl-C` yanks it to the
clipboard instead of cancelling the dialog and `Backspace` deletes the whole
field instead of one character — any other key drops the selection (and the
highlight). `Ctrl-U` clears the active field; `Ctrl-W` deletes one word
backward.

`Esc`/`Ctrl-C` cancel (`Ctrl-C` only when nothing is selected via `Ctrl-A` — see
above) — but if either field has unsaved text, a "save or drop?" confirmation
opens first instead of closing immediately, so a reflexive `Esc` can't silently
lose a half-written note. `Enter` is the default action there and saves (same as
`Ctrl-S`); `d`/`D` drops the note; the Cancel binding backs out of the
confirmation WITHOUT discarding anything, returning to the dialog with the text
intact; `Ctrl-C` force-quits the whole TUI immediately, bypassing the
confirmation — the same panic-button semantics as every other confirmation
dialog in the TUI (e.g. the delete confirmations). An empty dialog (both fields
blank) still closes immediately on `Esc`/`Ctrl-C` — nothing to confirm.

**Pre-filled from the selected row** — the dialog opens with Title/Content
seeded from whatever row was selected when the action fired (blank if nothing
was selected), so the note captures what you were just looking at:

| Selected row                                              | Title             | Content                                                                  |
| --------------------------------------------------------- | ----------------- | ------------------------------------------------------------------------ |
| Question (`%`/`?` mode)                                   | the question text | the LLM's stored answer                                                  |
| Note (`@` mode)                                           | _(blank)_         | `[[wiki-link]]` to the note (filename, `.md` stripped)                   |
| JIRA (`-` mode)                                           | _(blank)_         | `[KEY](browse-url)`; falls back to the bare key if JIRA isn't configured |
| Everything else (plain history rows and every other mode) | _(blank)_         | the command text wrapped in a fenced ` ```bash ` block                   |

The user can edit or clear either field before saving; nothing is
auto-committed.

Also launchable standalone via `smarthistory tui --create-note`, which implies
`--exec` (runs the staged command itself instead of just printing it) so it
works from a bare shell invocation, a herdr keybinding, or a shell alias without
needing `eval "$(...)"`.

**Inline link/tag completion** — in either field, `Tab` on a word starting with
one of these prefixes opens a completion menu:

| Prefix        | Matches                                                                                                                                                                                                       |
| ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `@`           | note **links** — real link targets from the vault's `note_links` table (same source and helper, `crate::jira::notes_link_matches`, as the Notes (`@`) prefix mode's own Tab completion), candidate `[[link]]` |
| `[[`          | same as `@` — link completion, for the user who typed the literal wiki-link brackets instead of the `@` shorthand                                                                                             |
| `#`           | note **tags** — real tag names from the vault's `note_tags` table (`crate::jira::notes_tag_matches`), candidate `#tag`                                                                                        |
| `@p:`         | notes with frontmatter `type: project`, candidate `[[basename]]`                                                                                                                                              |
| `@e:`         | notes with frontmatter `type: people`, candidate `[[basename]]`                                                                                                                                               |
| `@d:`         | notes created today (frontmatter `created:` date), candidate `[[basename]]`                                                                                                                                   |
| `@7:` / `@w:` | notes created in the last 7 days (today and the 6 days before it — a rolling window, not the previous calendar week), candidate `[[basename]]`                                                                |
| `@n:`         | all notes, candidate `[[basename]]`                                                                                                                                                                           |

`@`/`[[`/`#` require at least one character typed after the prefix (an empty
prefix returns no candidates, same as the Notes (`@`) prefix mode's query
input); the `@p:`/`@e:`/`@d:`/`@7:`/`@w:`/`@n:` variants list every match with
no text typed. Typed text after any prefix narrows the candidates by a
case-insensitive prefix match, live — while the menu is open, printable
characters and `Backspace` keep filtering the list instead of being swallowed. A
single match is inserted directly (no menu to confirm). Navigate a multi-match
menu with `Up`/`Down`, `Tab`/`Shift-Tab` (`BackTab`), `Home`/`End`; `Enter`
commits the selected candidate, `Esc` dismisses the menu (keeping whatever was
typed) without closing the dialog.

`Ctrl-D` / `Ctrl-N` / `Ctrl-7` are one-keystroke shortcuts for `@d:` / `@n:` /
`@7:` — same as typing the prefix and pressing `Tab`. (`Ctrl-W` was the natural
mnemonic for "last 7 days" but was already taken by delete-word-backward, so
`Ctrl-7` is used instead.)

A no-op (dialog stays open, status message) when both fields are
empty/whitespace-only, or when `notes.database` / `notes.dir` aren't configured.

### `CreateJiraIssue`

| Field        | Value                                                                                    |
| ------------ | ----------------------------------------------------------------------------------------- |
| Config key   | `create-jira-issue`                                                                        |
| Display name | Create JIRA issue                                                                          |
| Default key  | none (open it via the command palette, `Ctrl-Q`, or bind `key.create-jira-issue=<spec>`)  |
| Category     | tools                                                                                      |

Open a dialog to create a new JIRA issue via `POST /rest/api/2/issue`. Five
fields: **Issue Type** and **Project** are closed-set selectors cycled with
`Left`/`Right` (not typed — a wrong value would just fail the POST); **Subject**,
**Labels**, and **Description** are free text, `Tab`/`Shift-Tab` rotating focus
through all five in that order (Issue Type → Project → Subject → Labels →
Description → wraps). `Enter` inserts a literal newline only while Description is
focused; `Ctrl-S` submits, `Esc` cancels.

Description is fully editable even when pre-filled with a long body (from a
note or a JIRA issue — see below): it word-wraps, `Up`/`Down` move the cursor a
line at a time (preserving column, same as `CreateNote`'s Content field), a
reversed-video cursor glyph is rendered on the character it sits on, and the
view auto-scrolls to keep that line visible — so editing deep into a long
pre-filled body doesn't happen invisibly off-screen.

- **Project** comes from `JIRA_AVAILABLE_PROJECTS` (comma-separated), falling
  back to a single entry from `JIRA_PROJECT` when unset, or an empty list when
  neither is set — in which case the dialog refuses to open at all (status
  message; nothing to select).
- **Issue Type** comes from `JIRA_AVAILABLE_ISSUE_TYPES` (comma-separated),
  defaulting to `Epic, Initiative, Story, Task, Bug` when unset.

See [`docs/configuration.md`](configuration.md#jira--mode) for both env vars.

**Pre-filled from the selected row**, same convention as `CreateNote` above:

| Selected row     | Subject                        | Description                                         | Labels                    |
| ----------------- | ------------------------------- | ---------------------------------------------------- | -------------------------- |
| Note (`@` mode)   | the note's filename (no `.md`) | the note's full file content                          | the note's own `#tags`     |
| JIRA (`-` mode)   | the issue's summary (immediate) | the issue's description (async, see below)            | the issue's labels (async) |
| Everything else   | _(blank)_                       | _(blank)_                                             | _(blank)_                  |

The JIRA-row case needs a fresh `search("key = <KEY>")` call — the cached
row only carries a hand-formatted output blob, not structured
description/labels — so Description shows a `(loading…)` placeholder until
that background fetch resolves. The fetch only overwrites a field still at
its placeholder/empty state, never once the user has started typing into it.

**Cloning extra custom fields** — when `JIRA_CLONE_FIELDS` is set (see
[`docs/configuration.md`](configuration.md#jira--mode)) and the dialog was
opened from a selected JIRA row, the listed custom fields (`cf[<id>]`,
same bracket syntax `CreateJiraIssueFromTemplate`'s frontmatter uses) are
fetched from the source issue alongside Description/Labels and shown
between Labels and Description. Unlike a template's own custom fields,
cloned fields are **read-only** — Tab still reaches them, but every editing
keystroke is a no-op, and they render a dim `(cloned)` marker so it's clear
why. Their value is still sent to the new issue on submit, unchanged — a
real clone, not just a reference display. Fetching the cloned fields is
best-effort: if it fails, Subject/Description/Labels prefill still
completes normally and the cloned fields are simply absent.

On submit, the issue is created first; if the dialog was opened from a JIRA
row, a second call links the new issue to that source issue with a `Relates`
link. A link failure doesn't undo or fail the create — the issue really was
created, so the status message shows the new key alongside a warning that the
link didn't take. A create failure keeps the dialog open (with the error
shown) so the user can retry without retyping.

**Epic Name auto-fill** — when `JIRA_EPIC_NAME_FIELD` is set (see
[`docs/configuration.md`](configuration.md#jira--mode)) and Issue Type is
cycled to `Epic`, an "Epic Name" field is auto-inserted (JIRA's own required
field for Epics, distinct from Subject) seeded from the current Subject text.
It's a one-time seed, not a live sync — freely editable afterward, never
overwritten again even if Subject changes later. Switching Issue Type away
from `Epic` removes it; switching back reseeds it fresh from whatever Subject
says at that later moment. Unconfigured, nothing happens.

### `CreateJiraIssueFromTemplate`

| Field        | Value                                                                                                    |
| ------------ | --------------------------------------------------------------------------------------------------------- |
| Config key   | `create-jira-issue-from-template`                                                                          |
| Display name | Create JIRA issue from template                                                                            |
| Default key  | none (open it via the command palette, `Ctrl-Q`, or bind `key.create-jira-issue-from-template=<spec>`)    |
| Category     | tools                                                                                                      |

Opens a picker listing the markdown files under
`~/.config/smarthistory/templates/jira/` (arrow keys to move, `Enter` to pick,
`Esc`/`Ctrl-C` to cancel — no search/filter). Selecting one opens the same
dialog `CreateJiraIssue` does, pre-filled with the template's
frontmatter-defined fields in addition to the usual fields. Refuses to open
(status message) under the same conditions `CreateJiraIssue` does (JIRA not
configured, no selectable projects), plus when the templates directory is
missing or has no `.md` files in it.

**Template file format** — a markdown file with a YAML frontmatter block
followed by the Description body:

```markdown
---
project: ENG
labels:
- project
- test
cf[11601]: "Team ComS"
summary: SUMMARY
assigne: "HAR"
---
Description Content
```

Each frontmatter key is classified into one of four buckets:

| Key                                       | Meaning                                                                                                                            |
| ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------ |
| `project`, `issuetype`, `summary`, `labels` | Sets that standard field's starting value (Project/Issue Type only when the value matches an entry in the configured selector list). |
| `cf[<digits>]` (e.g. `cf[11601]`)          | A real JIRA custom field (`customfield_<digits>`) — becomes an editable dialog field, sent as-is on submit.                          |
| `created`, `updated`                        | Reserved — silently dropped, never shown (metadata this note-tooling format stamps automatically).                                    |
| anything else (e.g. `assigne`)              | A "parameter" field — editable, but its value is folded into Description as a prepended `**name:** value` line on submit, rather than sent as a JIRA field. |

Only scalar values (optionally `"`-quoted) and flat block sequences
(`key:` then indented `- item` lines, e.g. `labels:` above) are supported —
this is a minimal, purpose-built frontmatter reader, not a general YAML
parser.

Extra fields (`cf[...]`/parameter) render between Labels and Description, so
Description always stays last regardless of how many a template defines.

**Precedence when a row is selected**: the row-based prefill (note content,
or a source JIRA issue's summary/description/labels) always wins over a
template's `summary:`/`labels:`/body for Subject/Labels/Description — the
template's values for those three only apply as the fallback (the same case
`CreateJiraIssue` would otherwise leave blank). `project:`/`issuetype:` and
every `cf[...]`/parameter field always apply regardless of row selection.

### `CreateJiraTemplateFromIssue`

| Field        | Value                                                                                                    |
| ------------ | --------------------------------------------------------------------------------------------------------- |
| Config key   | `create-jira-template-from-issue`                                                                          |
| Display name | Create JIRA template from issue                                                                            |
| Default key  | none (open it via the command palette, `Ctrl-Q`, or bind `key.create-jira-template-from-issue=<spec>`)    |
| Category     | tools                                                                                                      |

The reverse of `CreateJiraIssueFromTemplate`: generates a NEW template file
from an existing issue, instead of creating an issue from an existing
template. Only available on a selected row in `-` (JIRA) mode; a no-op with a
status message everywhere else, or when nothing is selected.

Opens a "Template name:" prompt (`Enter` confirms, `Esc`/`Ctrl-C` cancels). An
empty/whitespace-only name is rejected inline — the prompt stays open. On a
valid name, fetches the source issue's full details (`search`, for
summary/description/labels — the cached row doesn't carry them) plus every
populated custom field on the issue (not just whatever
[`JIRA_CLONE_FIELDS`](configuration.md#jira--mode) happens to be configured
to clone into a NEW issue — see [`CreateJiraIssue`](#createjiraissue)), then
writes `~/.config/smarthistory/templates/jira/<slugified-name>.md` in exactly
the frontmatter format `CreateJiraIssueFromTemplate` reads (`project:`,
`issuetype:`, `summary:`, `labels:`, one `cf[<digits>]:` line per populated
custom field, `description` as the body). Refuses to overwrite an existing
template with the same name — the status message says so rather than
silently clobbering it. Either fetch call failing fails the whole request
(no partial template is written); the status message shows the JIRA error.

### `CreateWorktree`

| Field        | Value                                                                                  |
| ------------ | --------------------------------------------------------------------------------------- |
| Config key   | `create-worktree`                                                                        |
| Display name | Create worktree                                                                          |
| Default key  | none (open it via the command palette, or bind `key.create-worktree=<spec>`)             |
| Category     | tools                                                                                    |

Available in `;` (worktree) mode and `-` (JIRA) mode; a no-op with a status
message everywhere else. Opens a step-through dialog that creates a new `git
worktree` checkout for the repo containing the current directory:

1. **Branch** — pick an existing local branch from a filtered list, or type a
   name that matches none of them to create a new branch.
2. **Base branch** (new branch only) — pick the existing branch the new one
   is created from. Preselected from `worktree.defaultbranch` if configured,
   otherwise auto-detected (the remote's `HEAD`, then a local `main`/`master`,
   then the repo's current branch).
3. **Carry over uncommitted changes?** (`y`/`n`, only asked when the current
   checkout is dirty) — `y` runs `git stash push` in the source checkout and
   `git stash apply` in the new worktree.
4. **Assign to a project** (optional) — pick an existing `project.<slug>`
   from a filtered list, type a new one, or submit blank to skip. On
   confirm, this final step creates the worktree (`git worktree add`), under
   `worktree.basedir` if configured or sibling to the repo otherwise
   (`<repo-parent>/<repo-name>-worktrees/<branch>`), applies the carried-over
   stash if requested, writes `project.<slug>.dir=<path>` to the config file
   if a project was assigned, then stages a `cd` into the new worktree the
   same way selecting a Phase-1 worktree row does.

`Esc`/`Ctrl-C` cancel the dialog from any step without creating anything. A
`git` failure at any point (an existing branch collision, a bad base branch,
etc.) is shown inline in the dialog rather than closing it. See
[docs/modes/worktree.md](modes/worktree.md) and
[`worktree.basedir`/`worktree.defaultbranch`](configuration.md) for the
related config keys.

### `DisposeWorktree`

| Field        | Value                                                                                  |
| ------------ | --------------------------------------------------------------------------------------- |
| Config key   | `dispose-worktree`                                                                       |
| Display name | Dispose worktree                                                                         |
| Default key  | none (open it via the command palette, or bind `key.dispose-worktree=<spec>`)            |
| Category     | tools                                                                                    |

Only available on a selected row in `;` (worktree) mode; a no-op with a
status message everywhere else, or when no row is selected. Removes the
worktree under the cursor via `git worktree remove`.

Before opening the confirmation dialog, checks the worktree for uncommitted
changes (`git status --porcelain`) and for commits not yet pushed to its
upstream (`git rev-list --count @{upstream}..HEAD`, or "no upstream
configured" when the branch has never been pushed at all). The dialog warns
about whichever of those actually apply — nothing is shown for a worktree
that's clean and fully pushed. `y` runs `git worktree remove --force`, which
deletes the worktree's directory as part of removing it (`--force` is always
passed, since by this point the user has already seen and accepted any
dirty/unpushed warning); `n`/Cancel/`Ctrl-C` leaves it untouched. The branch
itself is never deleted, only the worktree checkout. A `git` failure (e.g.
attempting to remove the repo's main worktree, which git always refuses) is
shown as a status message rather than a panic. See
[docs/modes/worktree.md](modes/worktree.md).

---

## panes

These three actions only fire inside `*` (panes) mode; they're no-ops with a
status message outside it. Outside of panes mode, they don't interfere with
anything else.

### `FilterPanesWindows`

| Field        | Value                      |
| ------------ | -------------------------- |
| Config key   | `filter-panes-windows`     |
| Display name | Filter panes: windows only |
| Default key  | `F7`                       |
| Category     | panes                      |

Filter the `*`-mode panes view to show only live multiplexer panes (hide
`# Directories` and `# hosts`). Pressing the key again (when already filtered to
Windows) resets to `All`.

### `FilterPanesHosts`

| Field        | Value                    |
| ------------ | ------------------------ |
| Config key   | `filter-panes-hosts`     |
| Display name | Filter panes: hosts only |
| Default key  | `F8`                     |
| Category     | panes                    |

Filter the `*`-mode panes view to show only the `# hosts` block. Pressing the
key again resets to `All`.

### `FilterPanesSessions`

| Field        | Value                          |
| ------------ | ------------------------------ |
| Config key   | `filter-panes-sessions`        |
| Display name | Filter panes: directories only |
| Default key  | `F9`                           |
| Category     | panes                          |

Filter the `*`-mode panes view to show only the `# Directories` block (config
key and value stay `sessions` for backward compatibility — `directories` /
`directory` / `dir` / `dirs` are also accepted as `--panes-filter` values).
Pressing the key again resets to `All`.

---

## layout

### `TogglePaneVisibility`

| Field        | Value                    |
| ------------ | ------------------------ |
| Config key   | `toggle-pane-visibility` |
| Display name | Toggle pane visibility   |
| Default key  | `F10`                    |
| Category     | layout                   |

Toggle detail pane visibility. Cycles through: `BOTH` (details + output preview
side-by-side) → `Details` only → `Output Preview` only → `BOTH`. When only one
pane is visible, the remaining pane uses the full detail-row height — useful on
narrow terminals where the side-by-side layout would be cramped.

### `IncreasePaneHeight`

| Field        | Value                  |
| ------------ | ---------------------- |
| Config key   | `increase-pane-height` |
| Display name | Increase pane height   |
| Default key  | `F11`                  |
| Category     | layout                 |

Grow the detail / output-preview row height by one line, up to a
terminal-size-dependent maximum that always leaves at least a few lines for the
history list. The setting is persisted in the session file (`paneheight=<N>`, a
plain line count) so the user's chosen height carries over to the next TUI
startup. Useful when reading a long source-context preview: hold `F11` to grow
the pane exactly as far as needed, one line at a time.

### `DecreasePaneHeight`

| Field        | Value                  |
| ------------ | ---------------------- |
| Config key   | `decrease-pane-height` |
| Display name | Decrease pane height   |
| Default key  | `S-F11`                |
| Category     | layout                 |

Shrink the detail / output-preview row height by one line, down to the
historical 8-line floor. The mirror image of `IncreasePaneHeight`.

---

## codegraph

### `CodegraphRelations`

| Field        | Value                    |
| ------------ | ------------------------ |
| Config key   | `codegraph-relations`    |
| Display name | Browse callers / callees |
| Default key  | `C-r`                    |
| Category     | codegraph                |

Open a navigable picker listing the CodeGraph callers and callees of the
currently selected `&` / `$` (codegraph-backed) symbol. Up/Down move, Enter
opens the highlighted relation's source file in `$EDITOR` at its line (and exits
the TUI), Esc closes. Only meaningful in codegraph / tags(fallback) mode and
when the selected row carries a CodeGraph node id; otherwise a no-op with a
status message.

The picker is populated from the `edges` table in `.codegraph/codegraph.db`
(`kind='calls'`, `target=<node-id>` for callers, `source=<node-id>` for
callees). Each section is capped at 50 entries. See
[`docs/modes/codegraph.md`](modes/codegraph.md) for the full reference.

`SmartOpen` (`Ctrl-]`) also opens the same picker when the active mode is `&` /
`$` — the two keys are interchangeable for codegraph-backed rows. `SmartOpen` is
the cross-mode "dive" key the user is most likely to be holding;
`CodegraphRelations` is the explicit, dedicated shortcut.

---

## See also

- [`docs/modes/README.md`](modes/README.md) — the per-prefix-mode reference (one
  markdown file per mode).
- [`docs/multiplexer.md`](multiplexer.md) — tmux + herdr backend support.
- [`docs/configuration.md`](configuration.md) — the full config-file reference
  (every `key.<action>`, `prefix.<name>`, `tuicolor.*`, `capturelines.*`,
  `smart-open.*`, `jira.search.*`, `session.*`, `host.*`, `notes.*`, `ollama.*`,
  and env-var override).
- [`README.md`](../README.md#tui-key-bindings-subset) — the high-level key
  bindings table.
- [`TECHNICAL.md`](../TECHNICAL.md) — the implementation reference (the
  `MultiplexerBackend` trait, the `Action` enum, the config parser, etc.).
- [`src/tui/bindings.rs`](../src/tui/bindings.rs) — the canonical source for the
  `Action` enum and the `config_key` / `display_name` / `default_key` /
  `category` methods.
