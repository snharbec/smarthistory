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
- `Ctrl-X` (MarkTodoDone) toggles the checkbox: the row's `id` encodes the 1-based line number as `id = -(line_number)`, the `comment` field carries the filename, and the action replaces `[ ]` with `[x]` (or vice versa) on disk. The list is re-fetched so the row disappears (or the marker changes). **As of this build, `mark-todo-done` ships unbound by default** — the mark-done behaviour is still reachable via the cross-mode `SmartOpen` key (`Ctrl-]` by default) in `!` mode, so pressing `Ctrl-]` on a todo row does exactly what `Ctrl-X` used to. To restore the dedicated binding, add `key.mark-todo-done=C-x` (or any other spec) to `~/.config/smarthistory/config`.
- `Ctrl-E` opens the comment editor for the todo (smarthistory-side annotation, separate from the todo text itself).

## Quick-create

`!@new <text>` appends `- [ ] <text>` to today's daily note via:

```sh
note_search create-note <text> --type daily --timestamp --todo --database <notes.database>
```

The `--todo` flag is what marks the appended line as a todo entry. The TUI exits and the parent shell runs the command. The new todo appears next time you enter `!` mode.

## Required configuration

Same as `@` mode: `notes.database` and `notes.dir`. See [notes.md](notes.md#required-configuration).

## Cross-references

- [Notes mode — the parent database, also searchable via `@`](notes.md)
- [README — quick-create from notes/todo mode](../../README.md#quick-create-from-notestodo-mode)
