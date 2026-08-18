# Project mode (`.`) & time tracking

| Default prefix | `.`                     |
| -------------- | ----------------------- |
| Configurable   | `prefix.project=<char>` |

Project mode is the picker half of **time tracking**: a feature that attributes
time spent — directories, commands, notes created, and websites visited — to a
project, where a project is a `type: project` frontmatter note in your
`notes.database` vault. The `.` prefix lists those notes; selecting one sets the
_explicit_ current project. The other half, directory-based resolution and the
`smarthistory project report` subcommand, requires no interaction with `.` mode
at all — see below.

## What it does

- `.` (empty) — every `type: project` note in `notes.database`, newest-updated
  first.
- `.acme` — narrows to project notes whose text/tags/links/attributes match
  `acme` (same [Obsidian-like query syntax](notes.md) `@` mode uses — `#tag`,
  `[[link]]`, `[attr:value]`, free text — ANDed onto the `type: project` filter,
  which always applies regardless of what you type).
- Same row shape as [`@` (Notes) mode](notes.md): first column is the title (or
  filename when the note has no title), second is the filename.

`.` mode requires `notes.database` to be configured — same requirement as `@`
mode, since it searches the same vault, just pre-filtered. Run
`smarthistory check --prefix .` to confirm it's reachable.

## Selecting a row

`Enter` slugifies the selected note's filename stem (`crate::util::slugify`,
e.g. `Acme Corp.md` → `acme-corp`) and stages:

```sh
smarthistory project select <slug>
```

The TUI exits; the parent shell runs the staged command, which:

1. Upserts the `project_current` table (the "last explicit selection" pointer)
   to `<slug>`.
2. Closes any currently-open project session with `end_reason = "switch"`, then
   opens a new one for `<slug>` — the same `switch_project` lifecycle helper the
   directory-detection path (below) uses.

You can also run `smarthistory project select <slug>` directly, outside the TUI
— useful for scripting a project switch (e.g. from a shell alias or a tmux
session-start hook).

## How the active project is resolved (directories)

Every `smarthistory add` (the shell hook that fires after each command) resolves
"which project is this command in" **before** recording the command, in this
priority order:

1. **In-repo marker file** — `.smarthistory-project` in the current directory or
   any ancestor (bounded search, stops at `$HOME` or 25 levels up). The file's
   first non-blank line names the slug directly. Useful for a portable/shared
   checkout where absolute paths in `project.<slug>.dir` would differ per
   machine.
2. **`project.<slug>.dir` config** — longest-directory-prefix match against the
   current working directory (same matching convention as `session.<key>.dir`).
3. **`project_current`** — the last explicit `.`-mode / `project select` choice,
   as a fallback for directories with no marker file or config entry.
4. **None** — the command isn't attributed to any project; it's later reported
   under `untracked`.

## Sticky project directories

A plain `project.<slug>.dir` binding (step 2 above) is transient: it only
attributes commands to `<slug>` while `pwd` is actually inside `dir`. Leave the
directory, and the next command reverts to whatever `project_current` already
was (step 3) — the directory has no lasting effect once you've left it.

`project.<slug>.sticky = on` changes that. Entering a sticky directory does
everything a plain binding does, but also performs step 1 of `project select`'s
own sequence (see above): it upserts `project_current` to `<slug>`, making it
the new "background" project. So after leaving a sticky directory, any
subsequent directory with no marker file or `.dir` binding of its own keeps
attributing to `<slug>` — the directory you just left, not whatever was set
before you entered it — instead of reverting.

```ini
project.acme.dir = ~/work/acme
project.acme.sticky = on
```

This is useful for a directory that represents "starting work on `acme`" in a
broader sense than the directory itself — leaving it to check something in an
unrelated, unbound scratch directory shouldn't lose that context the way a plain
binding would.

Sticky only fires from the one genuine "entered a directory" event: a shell
command actually running in `pwd` (`smarthistory add`'s directory resolution).
It does **not** fire from [file tracking](#file-tracking)
(`smarthistory file viewed/modified/created`, or the `fileviewcommands`
automatic-`viewed` path below) — those resolve a _file's_ directory, which may
have nothing to do with the shell's own `pwd`, so treating a file event as
"entering" a sticky directory would be surprising. It also never fires while
[paused](#pausing-tracking) or when a marker file (step 1) is what actually won
the resolution instead of the sticky binding.

## Session lifecycle

Time is tracked in the `project_sessions` table: one open/close row per
continuous stretch of activity on a project. There's no daemon — sessions close
**lazily**, piggybacked on the next `smarthistory add` call:

- **Directory change** — a resolved project different from the currently-open
  one closes the old session immediately (`end_reason = "directory_change"`) and
  opens a new one. Immediate, not idle-timeout, since leaving the old session
  open while running commands in a different project's directory would
  misattribute that time.
- **Idle timeout** — no commands recorded for longer than
  `project.idlethreshold` seconds (default 1800 = 30 minutes) closes the
  session, backdated to `last_command_ts + idlethreshold`
  (`end_reason = "idle"`) — not to "now", so the report doesn't count the idle
  gap itself as tracked time.
- **Explicit switch** — `smarthistory project select <slug>` (including via `.`
  mode) always closes the previously-open session with `end_reason = "switch"`,
  even from the same directory.

A session can span multiple simultaneous shell panes — the idle check looks at
global command activity (`history.timestamp`), not per-pane — so running a build
in one pane and editing in another, both in the same project, count as one
continuous session.

## File tracking

```sh
smarthistory file viewed <path>
smarthistory file modified <path>
smarthistory file created <path>
```

Records that a file was viewed, modified, or created — meant to be called from
an editor hook (a Vim `autocmd`, an LSP client, a file-watcher script), not
typed by hand. Each call inserts one row in `file_events`: the path
(canonicalized to absolute; a path that no longer exists on disk at call time is
stored as given rather than dropping the event), the event kind, and the
resolved project slug.

Project attribution uses the **same resolution priority** as directories (marker
file → `project.<slug>.dir` → last explicit selection), but resolved from the
**file's own directory**, not the caller's current working directory — an editor
process's cwd isn't necessarily the file's directory (a single long-running
editor instance with files open from several projects, an LSP server with its
own cwd, …), so resolution has to follow the file, not the shell that happens to
have invoked the hook. One difference from directory resolution: a
[sticky `project.<slug>.dir`](#sticky-project-directories) binding never
persists to `project_current` from a file event, even when the file's directory
matches one — only a real shell command running in `pwd` counts as "entering" a
sticky directory.

Example Vim integration (`~/.vimrc`):

```vim
autocmd BufReadPost  * silent! call system('smarthistory file viewed '   . shellescape(expand('%:p')))
autocmd BufWritePost * silent! call system('smarthistory file modified ' . shellescape(expand('%:p')))
```

(Distinguishing "modified" from "created" requires checking whether the file
existed before the write — e.g. in a `BufWritePre` autocmd, via
`filereadable(expand('%:p'))` — before deciding which of the two to call; the
exact detection is editor-specific and left to the hook.)

### Automatic `viewed` events from shell commands

No editor hook needed for the common case of paging through a file from the
shell: `smarthistory add` (the shell hook that already fires after every
command) checks the command's program name against `fileviewcommands` (config
key, default `less more bat tail head`) and, on a match, records the first
non-flag argument as a `viewed` event automatically.

```ini
# Replace the default list (setting this key replaces it entirely,
# it doesn't append):
fileviewcommands=less more bat tail head cat
```

```sh
tail -f app.log        # -> viewed: app.log (the -f flag is skipped)
less -N config.yaml    # -> viewed: config.yaml
```

The argument is resolved relative to the shell's cwd when it's a relative path,
then attributed by its own directory exactly like `smarthistory file viewed`
does. This is a simple heuristic, not a full argument parser: a flag that takes
a separate value (`head -n 20 file.csv`) is picked up as if `20` were the file —
a known, accepted limitation, not something worth a real parser for a handful of
pager-style commands.

## Pausing tracking

```sh
smarthistory project pause
```

A toggle: the first call **pauses** — closes the currently-open session (if any,
`end_reason = "paused"`) and suppresses all resolution (marker file,
`project.<slug>.dir`, explicit selection) until resumed, so `cd`-ing into a
directory-bound project while paused doesn't quietly restart tracking. The
second call **resumes** — reopens a session for whichever project was active at
the moment you paused (`end_reason = "switch"` on the reopened session), not
whatever the directory you're currently in would resolve to. Useful for a lunch
break or a meeting where you don't want the time attributed to anything.

## Current session's files

```sh
smarthistory project files
```

Prints the files viewed/modified/created since the currently-open project
session started — a quick "what have I touched right now" view, scoped to the
live session rather than a whole calendar day the way `report` is. Reads the
open `project_sessions` row directly (`end_ts IS NULL`) rather than re-resolving
the project from the current directory, so it reflects whatever session
`smarthistory add`/`file` actually has open right now.

```
# acme — session started 09:14:02

### Files viewed
- ~/work/acme/src/main.rs (3x)

### Files modified
- ~/work/acme/src/main.rs

### Files created
(none)
```

Prints "no active project session" (exit code 1) when nothing is currently open
— including while [paused](#pausing-tracking), since pausing closes the open
session.

## `smarthistory project report`

```sh
smarthistory project report [--day <YYYY-MM-DD>|today|yesterday] [--project <slug>] [--min-duration <secs>]
```

Prints a Markdown-ish report for one calendar day (local time, defaults to
`today`), per project:

- **Standard work time** — a day-level total, not per-project: the span from the
  day's first tracked activity (a command or a website visit, across every
  project or none) to its last, minus the excess of any gap beyond
  `project.idlethreshold` — the same idle-capping rule the per-command duration
  and session lifecycle below already use, just applied across the whole day's
  activity at once. Answers "how much of today did I actually spend using the
  computer, accounting for breaks," as distinct from any one project's own
  active time. `--project <slug>` doesn't narrow this — it's always the whole
  day's figure.
- **Total active time** and a **directories** breakdown (both use every command
  in the window, unaffected by `--min-duration`).
- **Commands**, filtered to those whose _derived_ active duration is at least
  `--min-duration` seconds (default 0 — list everything). A command's duration
  is computed at query time, not stored:
  `min(next_command_ts - ts, idlethreshold)`, partitioned per shell session so a
  long idle gap in one pane never inflates a command's duration in another. This
  also means a command followed by a coffee break doesn't inherit the break's
  length — only the idle-capped gap does.
- **Notes created** during a tracked window (requires `notes.database`) — any
  note (not just `type: project` ones) whose `created` timestamp falls inside
  one of the project's open/close intervals for that day.
- **Files viewed / modified / created** — from
  `smarthistory file viewed`/`modified`/`created` (see
  [File tracking](#file-tracking) above), deduplicated by path with an `(Nx)`
  occurrence count when a file shows up more than once in a category.
- **Websites**, resolved through a 3-tier priority (see below) and clustered for
  display via `weburlgroup`.

Commands/visits with no resolved project land under a trailing `untracked`
section. `--project <slug>` narrows the whole report to one project (and drops
the `untracked` section).

```
# Project Report — 2026-08-14
Standard work time: 7h42m

## acme
Total active time: 2h15m

### Directories
- ~/work/acme (2h15m)

### Commands
- 09:14:02  4m12s  (~/work/acme)  cargo build --release
- 09:41:10  30s    (~/work/acme)  git commit -m "fix retry logic"

### Notes created
- [[2026-08-14-standup-notes]]

### Files viewed
- ~/work/acme/src/main.rs (4x)

### Files modified
- ~/work/acme/src/main.rs

### Files created
(none)

### Websites
- JIRA tickets (3 visits)
- https://acme-corp.atlassian.net/wiki/...

## untracked
Total active time: 8m
...
```

## Website resolution (3 tiers)

Each website visit — a browser bookmark/history entry (see
[`^` mode](browser.md)) in the report's day range, plus any `history.command`
row that stages `open "<url>"` (how a `-`-mode JIRA visit lands in the command
history — see [JIRA mode](jira.md)) — is resolved to a project through this
priority:

1. **`jiralabel.<slug>.match`** — the visit's URL/command is scanned for an
   embedded JIRA issue key (`[A-Z][A-Z0-9]*-[0-9]+`); if found and JIRA is
   configured (`JIRA_SERVER`/`JIRA_API_TOKEN`), the issue's labels are fetched
   (cached per report run) and matched against configured
   `jiralabel.<slug>.match` values. Skipped entirely when JIRA isn't configured.
2. **`weburl.<slug>.match`** — a plain substring match against the URL's
   host+path (no query string/fragment), for domains that are structurally
   single-project (a project's own dedicated docs site, say).
3. **Time-based fallback** — whichever `project_sessions` interval (if any) was
   open at the visit's timestamp.

**Display clustering** (`weburlgroup.<name>.match`/`.label`) is independent of
assignment: it just groups same-labeled visits into one `<label> (N visits)`
line in the report instead of listing each URL — e.g. bucketing every JIRA/wiki
visit together regardless of which project the ticket belongs to.

## Configuration

| Key                        | Meaning                                                                                                                                                              |
| -------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `project.<slug>.dir`       | Directory-to-project binding, longest-prefix matched against the cwd. `<slug>` should match a `type: project` note's slug (not required — see `config check` below). |
| `project.idlethreshold`    | Seconds of inactivity before an open session closes (`end_reason = "idle"`). Default `1800`. Must be a positive integer.                                             |
| `jiralabel.<slug>.match`   | A JIRA label that maps to `<slug>` — website-resolution tier 1.                                                                                                      |
| `weburl.<slug>.match`      | A URL host+path substring that maps to `<slug>` — website-resolution tier 2.                                                                                         |
| `weburlgroup.<name>.match` | A URL host+path substring for display clustering (independent of assignment).                                                                                        |
| `weburlgroup.<name>.label` | The label printed for visits matching `weburlgroup.<name>.match`.                                                                                                    |

```ini
project.acme.dir=~/work/acme
project.idlethreshold=1800

jiralabel.acme.match=acme-corp

weburl.acme.match=docs.acme-corp.internal

weburlgroup.jira.match=/browse/
weburlgroup.jira.label=JIRA tickets
```

`smarthistory config check` cross-references
`project.<slug>.dir`/`jiralabel.<slug>.match`/`weburl.<slug>.match` slugs
against `type: project` note slugs (when `notes.database` is configured) and
warns — not errors — on either side having no match: a directory/label/URL-only
project with no note yet, or a note tracked purely by explicit `.`-mode
selection with no directory/label/URL binding, are both legitimate.

See [`docs/configuration.md`](../configuration.md#project--mode) for the full
key reference.

## Cross-references

- [`@` (Notes) — the vault `.` mode filters down to `type: project`, and where "notes created" in the report come from](notes.md)
- [`-` (JIRA) — the source of REST-mode visits scanned for embedded issue keys](jira.md)
- [`^` (Browser) — the source of bookmark/history visits in the report's website section](browser.md)
- [`README.md`](README.md) — mode index, common actions, match algorithms
