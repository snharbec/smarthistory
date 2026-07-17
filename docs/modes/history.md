# History mode (no prefix)

| Default prefix | *(none)* |
| --- | --- |
| Mode name | `history` (the `QueryPrefixes` field `output` is reserved for the `+` mode; plain history mode has no per-mode field) |
| Default key | `Enter` (the [`Run`](../../TECHNICAL.md#run-enter) action) |
| Configurable | n/a — the no-prefix mode is always reachable by typing any character that isn't a configured prefix. |

History mode is the default: a query without a leading prefix char searches the shell history. It's the mode you spend most of your time in.

## What it does

- Matches every row in the `history` SQLite table.
- The first text column is `command` (the executed shell line); the second is `directory` (the cwd when it ran); the third is `timestamp`.
- Empty matches return every history row (the default view when you launch the TUI).

## Selecting a row

- `Enter` stages the row's `command` as the next selection and exits the TUI. The parent shell runs the command.
- `Ctrl-E` opens a comment editor for the row (a free-form annotation that survives across sessions and applies to every row with the same command text).
- `Ctrl-V` picks a filename-shaped token from the row's command and stages `$EDITOR <file>` (see [the `EditFileReference` action](../../TECHNICAL.md)).
- `Ctrl-D` deletes the row (with a `y/n` confirmation overlay).
- `Ctrl-X` is a no-op in history mode (it's the `MarkTodoDone` action, only relevant in `!` mode).

## Match algorithm

Toggle with `Ctrl-F`. The default is `sub` (case-insensitive substring on `command` and `comment`).

| Alg | What it matches |
| --- | --- |
| `sub` | every whitespace-separated term is a literal substring of either `command` or `comment` |
| `fuz` | every whitespace-separated term fuzzy-matches (subsequence) `command` or `comment` |
| `reg` | the body is a regex; matches against `command` or `comment` |

## Duplicate filter

`Ctrl-U` toggles the duplicate filter. When on (the default), the result list collapses every command with the same text to a single row (the most-recent instance). When off, every row appears verbatim — useful for finding commands that ran in a specific directory or session.

## Exit-code filter

`Ctrl-J` cycles the exit-code filter: `all` (default), `ok` (only exit 0), `nonzero` (only non-zero). The chip in the mode strip shows the active filter.

## Sort order

`F4` cycles the sort order between `AGE` (newest first — the default) and `FREQ` (most-run first). Frequency sort implicitly enables the duplicate filter (showing the same command N times would dominate the list).

## Per-mode input history

`Ctrl-P` / `Ctrl-N` cycle through the **history mode's** past queries. Other modes have their own per-mode history (scoped by prefix), so `Ctrl-P` in `&` mode only recalls past `&` queries. See [`README.md`](README.md#common-actions-that-work-in-every-mode) for the full set of common actions.

## Privacy convention (history mode records, other modes don't)

The TUI's space-prefix convention (see [`README.md`#privacy-convention](README.md#privacy-convention-space-prefix)) has a deliberate **exception for history mode**: staging a row from the history list runs the command *without* a leading space, so it IS recorded in the smarthistory DB. Recording it keeps the frequency stats accurate (so `Ctrl-S` next-probable-command suggestions stay useful) and lets the same command surface in future searches. Every other prefix mode (`+`, `=`, `%`, `@`, `!`, `#`, `*`, `-`, `~`, `$`, `&`, `,`) stages a one-shot read (`bat README.md`, `note_search edit-note <id>`, `open <jira-url>`, etc.) that the user typically doesn't want cluttering the DB, so those get the single-space prefix.

## Cross-references

- [`+` (Output) — search the captured stdout / stderr of each command](output.md)
- [`#` (Directories) — list every directory the shell has been in](directories.md)
- [`@` (Notes) — search the `note_search` database](notes.md)
