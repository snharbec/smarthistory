# Output mode (`+`)

| Default prefix | `+` |
| --- | --- |
| Configurable | `prefix.output=<char>` |

Output mode matches against the *captured stdout / stderr* of each command, not the command text itself. Useful for finding the moment something went wrong ("which run printed `OutOfMemoryError`?") or for re-finding a command by what it printed.

## What it does

- Searches the `history_output.output` column of the SQLite database.
- The displayed list shows `command` (or a snippet of the matching output line, depending on the result shape), the directory, the exit code, and the timestamp.
- Empty query (just `+`) returns every row that has captured output (i.e. every command that was captured — not every history row).

## Capturing output

Output is captured by the precmd hook in `init.zsh` for tmux, or by the herdr `pane read` worker for herdr. The default cap is 20 lines per command (configurable via `capturelines=N` or `capturelines.<cmd>=N` / `ALL`).

`init.zsh` is a no-op in some environments (containers without tmux, plain `bash`, etc.) — when the capture directory is empty, `+` mode returns nothing.

## Selecting a row

- `Enter` stages the command (same as in history mode).
- `Ctrl-O` opens the *full* captured output of the highlighted row in the `Ctrl-O` overlay. This is the primary use case: you typed `+OutOfMemory` to find a row whose output contained `OutOfMemory`, then `Ctrl-O` to see the full 20-line capture.

## Match algorithm

Toggle with `Ctrl-F`. The default is `sub` (case-insensitive substring on the captured output text).

## Special tokens

None. `+` is a pure text search over the captured-output column.

## Cross-references

- [History mode — the default view, searches command text](history.md)
- [JIRA — output is also where JIRA comments show up if you `jira-issue <KEY>`](jira.md)
- [TECHNICAL — output capture details](../../TECHNICAL.md#output-capture-tmux--herdr)
