# The `smarthistory daemon` file watcher

The `smarthistory daemon` command watches configured project directories for
file changes and records them as `file_events` rows — the automatic counterpart
to the editor-hook `smarthistory file` command. Both feed the same
`project report` "Files viewed / modified / created / deleted" sections.

The daemon's value is **capturing activity that never goes through the shell**:
time spent editing files in a GUI editor, a browser, or any app that doesn't run
a shell command. The lazy time-tracking model (which piggybacks on
`smarthistory add` after each command) only records activity when a command
runs; the daemon records file changes continuously, so a project session stays
alive and correctly attributed even when you're not touching the terminal.

## How it fits the existing architecture

- **Same table.** The daemon writes to the same `file_events` table the
  `smarthistory file viewed/modified/created` command uses. No new storage.
- **Same attribution.** Each event is attributed to a project using the exact
  same `resolve_current_project` logic (marker file → `project.<slug>.dir` →
  last explicit selection), resolved from the **file's own directory** — the
  same rule the `file` command uses, so a file in a sub-project of a monorepo is
  attributed to the sub-project, not the watcher's cwd.
- **Same report.** `project report` already reads `file_events`; the daemon's
  rows appear there automatically. The daemon adds a new `deleted` event kind
  (see [Schema](#schema)), which the report now prints as a "Files deleted"
  section.

## Usage

```sh
smarthistory daemon [--watch DIR ...] [--once]
```

- Runs in the **foreground** by default. Manage it with a launchd/systemd unit,
  a terminal multiplexer pane, or `smarthistory daemon &`.
- `--watch DIR` overrides the configured watch roots for this invocation.
- `--once` watches, drains the current event burst (up to a fixed window), then
  exits — a cron-style poll fallback for environments that can't keep a
  long-running process alive.

The daemon prints the directories it's watching to stderr, so you can confirm it
picked up the right roots.

## Configuration

All `daemon.*` keys live in `~/.config/smarthistory/config` (INI-style
`key=value` lines).

| Key                      | Default                    | Meaning                                                                                                                                                  |
| ------------------------ | -------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `daemon.enabled`         | `on`                       | Kill switch. `off` makes `smarthistory daemon` exit immediately. Running the command is the opt-in.                                                      |
| `daemon.watch`           | _(derived)_                | Space-separated directories to watch. When empty, the daemon watches every `project.<slug>.dir` entry (tilde-expanded).                                  |
| `daemon.ignore-dirs`     | _(built-ins)_              | Space-separated directory basenames to skip. Combined with the built-in `DEFAULT_IGNORES` list (`target`, `node_modules`, `.git`, …).                    |
| `daemon.ignore-files`    | _(none)_                   | Space-separated file globs to skip, matched against the event path's basename. `*` and `?` supported.                                                    |
| `daemon.events`          | `created,modified,deleted` | Comma-separated event kinds to record.                                                                                                                   |
| `daemon.debounce-ms`     | `500`                      | The debounce window (milliseconds) that coalesces the burst of events from a single editor save into one event.                                          |
| `daemon.merge-window-ms` | `1000`                     | How long a `deleted` event waits for a matching `created` event at the same path before it's recorded as a real deletion. `0` disables merging entirely. |

### Which directories to watch

```ini
# Explicit list (overrides the derived project roots):
daemon.watch=~/work/acme ~/work/other

# Or rely on the default: every project.<slug>.dir entry
project.acme.dir=~/work/acme
project.other.dir=~/work/other
```

### Which directories to ignore

```ini
# Directory basenames to skip (combined with the built-in defaults):
daemon.ignore-dirs=target node_modules .git .venv .terraform
```

An event under any ignored directory is dropped before it touches the database —
so a change inside `target/`, `.git/`, `node_modules/`, etc. is never recorded.

### Which files to ignore

```ini
# File globs to skip, matched against the basename:
daemon.ignore-files=*.tmp *.swp *.log *.pyc
```

### Which event kinds to record

```ini
# Only record creates and deletes, not every save:
daemon.events=created,deleted
```

### Debounce tuning

```ini
# Longer window = fewer rows (more coalescing), at the cost of a
# slightly delayed write:
daemon.debounce-ms=1000
```

### Delete/create merging (atomic editor saves)

Many editors — vim's default save strategy among them — don't overwrite a file
in place. They rename the original file away (as a backup) and write a brand new
file at the same path, which the watcher reports as a `Remove` immediately
followed by a `Create`, not a `Write`. Recorded literally, every vim save would
show up as a spurious delete-then-recreate pair in `project report` instead of
one `modified` row.

To avoid that, a `deleted` event isn't recorded immediately. It's held for up to
`daemon.merge-window-ms` (default `1000`ms) waiting to see whether a
`created`/`modified` event arrives for the _exact same path_. If one does, the
pending delete is discarded and a single `modified` event is recorded instead;
if the window elapses with nothing matching it, it's recorded as a real
`deleted` event, same as before. A genuine deletion is still recorded promptly —
the wait is bounded by the window, not by how long the daemon happens to run —
and nothing pending is ever silently dropped: the daemon flushes every
still-pending delete before it exits, `--once` included.

```ini
# Disable merging entirely — every delete is recorded immediately,
# even ones an editor's atomic save would otherwise produce:
daemon.merge-window-ms=0

# A shorter window for editors/filesystems where the rename-and-recreate
# completes very quickly:
daemon.merge-window-ms=200
```

## Event mapping

| `notify` event                           | `file_events.event_kind`                                                                                                                              |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Create`                                 | `created` (or `modified`, if it completes a merged `deleted`+`created` pair — see [Delete/create merging](#deletecreate-merging-atomic-editor-saves)) |
| `Write` / `Chmod`                        | `modified`                                                                                                                                            |
| `Remove`                                 | `deleted`, after waiting `daemon.merge-window-ms` for a possible merge                                                                                |
| `Rename` / `Rescan` / `Error` / `Notice` | _(skipped)_                                                                                                                                           |

## Schema

The daemon introduces a new `deleted` event kind. The `file_events` table's
CHECK constraint is widened from `('viewed','modified','created')` to
`('viewed','modified','created','deleted')`. Existing databases are migrated
automatically on the next launch (a rename-and-recreate migration, the same
pattern the history-comment migration uses); fresh databases get the widened
CHECK directly.

## Ignore filtering order

For each event, the daemon applies, in order:

1. **Event-kind filter** — is this kind in `daemon.events`?
2. **Directory-ignore filter** — is the path under an ignored directory?
3. **File-ignore filter** — does the basename match an ignored glob?

Only events that pass all three are written to the database.

## Project attribution

Each surviving event is attributed by resolving the project from the file's own
directory (not the watcher's cwd), using the same `resolve_current_project`
logic the `file` command and `smarthistory add` use. A file whose directory
resolves to no project is stored with `project_slug = NULL` and shows up under
"untracked" in the report, exactly like an un-attributable command.

## Why `notify` 4.x

The daemon uses the `notify` crate (FSEvents on macOS, inotify on Linux). The
4.x line is pinned because it keeps the `DebouncedEvent` API, whose debounce
window coalesces the burst of events from a single editor save into one event —
exactly what time tracking wants (one "file saved in project X", not 50 raw
events). notify 5.x/6.x/7.x replaced `DebouncedEvent` with a raw `Event` stream
that has no built-in debounce.
