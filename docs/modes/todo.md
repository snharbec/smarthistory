# Todo mode (`!`)

| Default prefix | `!` |
| --- | --- |
| Configurable | `prefix.todo=<char>` |

Todo mode scans every note in the `note_search` database for open todo entries (markdown task-list checkboxes `- [ ]` or `- [x]`) and lists each one as its own row. Selecting a row opens the note in `$EDITOR` at the exact line of the checkbox.

## What it does

- `!` (empty) — every open todo in every note. Closed todos (`- [x]`) are filtered out.
- `!@new remember to buy milk` — quick-create a new todo entry in today's daily note (see Quick-create below).
- The displayed list shows the todo text (first column), the note basename (second), the line number, the exit code (`✓` for open, `✗` for done), and the timestamp.

## Selecting a row

- `Enter` on a todo row stages `$EDITOR +LINE_NOTE` (the note opens at the exact line of the checkbox). The `+LINE` template is configurable via `todo.line_option=...` in the config file (default `+$LINE`).
- `Ctrl-X` is now `Action::ToggleMark` (multi-select — see [`docs/actions.md`](../actions.md#togglemark)), NOT `MarkTodoDone`. `mark-todo-done` ships unbound by default (it used to default to `C-x`, before that key was reassigned to `ToggleMark`) — the mark-done behaviour is reachable via the cross-mode `SmartOpen` key (`Ctrl-]` by default) in `!` mode instead. To restore a dedicated `MarkTodoDone` binding, add `key.mark-todo-done=<spec>` (any key other than `C-x`, which is now taken) to `~/.config/smarthistory/config`.
- `Ctrl-]` (SmartOpen) toggles the checkbox of every **marked** todo (or just the selected one when nothing is marked): the row's `id` encodes the 1-based line number as `id = -(line_number)`, the `comment` field carries the filename, and the action replaces `[ ]` with `[x]` (or vice versa) on disk for each target. The list is re-fetched so completed rows disappear. Marking todos across multiple files and pressing `Ctrl-]` once is the fast way to clear several at once; the status message reports "Marked N of M todos done" when acting on more than one.
- `Ctrl-E` opens the comment editor for the todo (smarthistory-side annotation, separate from the todo text itself).

## Quick-create

`!@new <text>` appends `- [ ] <text>` to today's daily note via:

```sh
note_search create-note <text> --type daily --timestamp --todo --database <notes.database>
```

The `--todo` flag is what marks the appended line as a todo entry. The TUI exits and the parent shell runs the command. The new todo appears next time you enter `!` mode.

For a todo body longer than fits on the query line, press `F2` (`Action::ComposeNoteEntry`) instead of typing `!@new <text>` — this opens a multi-line compose overlay (`Enter` inserts a newline, `Ctrl-S` saves and exits, `Esc` cancels) that stages the same `note_search create-note ... --todo` command, with the buffer's embedded newlines re-indented so the entry stays a single valid markdown list item. Purely additive: `!@new <text>` still works unchanged. See [`docs/actions.md`](../actions.md#composenoteentry).

## Required configuration

Same as `@` mode: `notes.database` and `notes.dir`. See [notes.md](notes.md#required-configuration).

## Cross-references

- [Notes mode — the parent database, also searchable via `@`](notes.md)
- [README — quick-create from notes/todo mode](../../README.md#quick-create-from-notestodo-mode)
