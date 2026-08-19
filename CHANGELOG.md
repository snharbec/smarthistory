# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Added

- New `Action::CreateJiraIssue` (unbound by default; bind via
  `key.create-jira-issue=<spec>` or open via the command palette): opens a
  dialog to create a JIRA issue (Project, Subject, Description, Labels, Issue
  Type) via `POST /rest/api/2/issue`. Project and Issue Type are closed-set
  selectors cycled with `Left`/`Right`, sourced from the new
  `JIRA_AVAILABLE_PROJECTS`/`JIRA_AVAILABLE_ISSUE_TYPES` env vars (the latter
  defaulting to `Epic, Initiative, Story, Task, Bug`). Opened from a selected
  note row, Subject/Description/Labels are pre-filled from the note's
  filename/content/tags; opened from a selected JIRA row, they're pre-filled
  from the selected issue and the new issue gets a `Relates` link back to it
  on creation. See
  [docs/actions.md#createjiraissue](docs/actions.md#createjiraissue) and
  [docs/modes/jira.md](docs/modes/jira.md#creating-a-new-issue).
- `smarthistory project report` and the web dashboard's day overview now show a
  "Standard work time" total — the day's first tracked activity (a command or a
  website visit) to its last, minus the excess of any gap beyond
  `project.idlethreshold`, the same idle-capping rule the per-command duration
  and `project_sessions` lifecycle already use, just applied across the whole
  day at once instead of per-project or per-session. Day-level, not per-project
  — `--project`/`?project=` don't narrow it. New `standard_work_secs` field on
  `/api/report`'s JSON response.
- New `smarthistory project current --with-time` flag: also prints today's
  accumulated active seconds for the resolved project on a second stdout line.
  New `prompt.project`-published env vars `$SMARTHISTORY_PROJECT_TIME` (raw
  seconds) and `$SMARTHISTORY_PROJECT_TIME_HM` (`hh:mm`, computed in zsh from
  the raw-seconds value — no extra subprocess call), kept in sync alongside the
  existing `$SMARTHISTORY_PROJECT` by
  `_smarthistory_precmd`/`_smarthistory_sync_prompt_env` — see
  [Published environment variables](docs/configuration.md#published-environment-variables)
  for prompt examples.
- New `project.<slug>.sticky = on` config key: entering a sticky
  `project.<slug>.dir` directory also persists `<slug>` into `project_current`
  (the same upsert `project select`/`.`-mode selection does), making it the new
  background project — a plain (non-sticky) `.dir` binding is purely transient
  and has no lasting effect once you leave it, reverting to whatever
  `project_current` was before. With `sticky` on, leaving the directory
  afterward, any subsequent directory with no marker file or `.dir` binding of
  its own keeps attributing to `<slug>` instead of reverting. Only fires from a
  real shell command running in `pwd` — never from file-tracking events (which
  resolve a file's own directory, not necessarily the shell's `pwd`), while
  paused, or when a `.smarthistory-project` marker file is what actually won the
  resolution instead of the sticky binding. See
  [Sticky project directories](docs/modes/project.md#sticky-project-directories).
- New `prompt.project = on` config key (default `off`): `_smarthistory_precmd`
  resolves the current time-tracking project after every command and publishes
  it as `$SMARTHISTORY_PROJECT` (kept in sync by
  `_smarthistory_sync_prompt_env`, same as the existing
  `$SMARTHISTORY_MODE`/`$SMARTHISTORY_MATCHMODE`), for a prompt segment showing
  which project a directory tracks to — a `starship`/
  `oh-my-posh`/plain-zsh-`RPROMPT` example is in
  [Published environment variables](docs/configuration.md#published-environment-variables).
  Off by default since, unlike the mode/matchmode vars, publishing it costs a
  real `smarthistory project current` subprocess call per command.
- `smarthistory serve`'s day overview now shows a donut chart of that day's
  active time per project, next to the existing project list — a plain CSS
  `conic-gradient` (no chart library, no canvas/SVG), with a clickable legend
  that links straight to each project's detail view for that day. Purely a
  dashboard rendering change; the API response shape is unchanged.
- New `smarthistory serve`: an HTTP server exposing the same time-tracking data
  `project report` prints as text — a JSON API
  (`/api/report`/`/api/history`/`/api/projects`) plus an embedded single-page
  web UI (overview of every project active on a day, a drill-down detail view
  with collapsible files/notes/links lists, date navigation, and a project
  search view showing the last 7 days of tracked time, with real browser
  back/forward via `history.pushState`/`popstate`). No authentication — binds to
  `127.0.0.1` by default (`serve.host`/`serve.port` config keys, or
  `--host`/`--port`), with a startup warning if widened past loopback. Every
  request opens its own short-lived read-only DB connection. `axum`/`tokio` were
  already fully compiled into the dependency tree transitively via
  `note_search`, so adding them directly costs no extra binary weight. The
  day-report aggregation previously inline in `ProjectAction::Report`'s handler
  is now the shared `build_day_report` function, so the CLI's text report and
  the web API can never drift apart. See `docs/server.md`.
- New `smarthistory daemon`: a file-watching loop that records file changes in
  configured project directories as `file_events` (created/modified/deleted),
  attributed to the project the file lives in — the automatic counterpart to the
  editor-hook `smarthistory file` command, capturing activity that never goes
  through the shell (GUI editors, browsers, any non-terminal app). Configurable
  via `daemon.*` keys: `daemon.watch` (which directories to watch; defaults to
  every `project.<slug>.dir` entry), `daemon.ignore-dirs` (directory basenames
  to skip, combined with the built-in `DEFAULT_IGNORES`), `daemon.ignore-files`
  (file globs to skip), `daemon.events` (which event kinds to record),
  `daemon.debounce-ms` (the window that coalesces an editor save's event burst
  into one row), and `daemon.enabled` (kill switch). Uses the `notify` crate
  (FSEvents on macOS, inotify on Linux). See `docs/daemon.md`.
- The `file_events` table now supports a `deleted` event kind (recorded by the
  daemon on file removal); `project report` prints a new "Files deleted"
  section. Existing databases are migrated automatically.
- New `daemon.merge-window-ms` config key (default `1000`): many editors — vim's
  default save strategy among them — save by renaming the original file away and
  writing a new one at the same path, which the daemon's watcher reports as
  `Remove` immediately followed by `Create`, not a `Write`. A `deleted` event
  now waits up to this window for a matching `created` event at the same path
  before being recorded; a match merges the pair into a single `modified` event
  instead of a spurious delete-then-recreate. `0` disables merging. A genuine
  deletion is still recorded once the window elapses, and nothing pending is
  ever silently dropped — the daemon flushes every still-pending delete before
  it exits, `--once` included.

- New `smarthistory comments list|add|delete`: manage comment-expansion entries
  (`command_comments`) directly instead of only setting one via `add --comment`
  on the original command. `list` prints every stored comment with its exact
  command, flagging any orphaned ones (no matching `history` row, so `expand`
  won't resolve them yet). `add <command> <comment>` attaches/overwrites a
  comment; `delete <command>` removes one.
- New `tui.highlight=on|off` config key (default off): the TUI's history list
  syntax-highlights each `command`-mode row via
  [`syntect`](https://github.com/trishume/syntect) — the same engine `bat`
  itself is built on, compiled directly into `smarthistory` (no external `bat`
  binary required, unlike `dropdown.highlight`) — while there's no active search
  query (the matched-substring search highlight takes over instead the moment
  you start typing). Highlighted results are cached per
  `(color scheme, command text)`, since the TUI redraws roughly every 100ms
  regardless of input.
- `Ctrl-S`/`smarthistory next`/`dropdown.predict` (the "next command" predictor)
  now scope by the current search mode (SESS/DIR/GLOBAL) instead of always
  predicting from the entire unscoped global history. New `Commands::Next` flags
  `--session`/`--directory <DIR>` restrict which rows are even eligible to be
  paired as a predecessor/successor -- not just filter the output -- so a
  command from a different, concurrently-active pane (or an unrelated directory)
  can never spuriously count as "next" for this scope. `Ctrl-S` and the
  dropdown's prediction both pass the current mode automatically; GLOBAL keeps
  the previous unscoped behavior.
- New `dropdown.predict=on|off` config key (default off): when the command line
  is empty, the live dropdown widget shows predicted next commands instead of
  nothing, using the same successor-frequency data `Ctrl-S`/`smarthistory next`
  already computes. Capped at 3 candidates regardless of `dropdown.limit`.
  Selecting a prediction works exactly like selecting a normal search result.
- `Ctrl-R` now takes over any text already on the command line as the TUI's
  starting search query, instead of always restoring the last search regardless
  of what's typed. An empty command line still restores the persisted last
  search, unchanged. The positional `smarthistory tui [QUERY]` argument itself
  now takes final precedence over the persisted `session.query` whenever it's
  non-empty, matching `--prefix`'s existing precedence (an empty or omitted
  value still falls back to the session as before).
- New `smarthistory ask <question>` / console question mode: type
  `?question text` directly at the normal zsh prompt (no TUI) and press Enter. A
  transient `Thinking…` line shows while the request is in flight, then -- on an
  interactive terminal -- gets replaced in place by the colorized
  `LLM Answer`-headed answer, using the same last-command context as the TUI's
  `?` mode. If the answer suggests one or more commands, an interactive numbered
  pick list stages the chosen one into the next prompt for review -- it is never
  run automatically. Intercepted at `accept-line` in `init.zsh` before the line
  would ever reach real shell execution; the question is recorded to history the
  same way a TUI-asked question is, so it appears identically in `?`-mode search
  and `project report`.
- Question mode (`?`): the last command run in the session — its command line,
  exit code, and captured output, if any — is now automatically included as
  context, so `?what does that do` or `?why did that fail` resolve "that"/"this"
  without retyping the command; a question unrelated to any command still works
  the same as before. `Tab` also submits the question now, same as `Enter`, so
  you don't have to reach for Enter after typing.

### Changed

- `smarthistory export`/`import` now cover the whole database, not just
  `history`/comments/output. Export format bumped to version 2 (version 1 files
  still import fine): every `command_comments` row is now exported independent
  of `history` (so a comment on a command with no `history` row, or whose only
  row falls outside `--since`/`--until`, still round-trips — previously it was
  silently dropped), plus new `file_events`, `project_sessions`, and the
  `project_current`/`project_pause` singleton state. Import upserts
  comments/sessions and inserts new file events (deduplicated, since the table
  has no unique constraint of its own); the `project_current`/`project_pause`
  snapshot is only applied if it's newer than what's already in the target
  database, so importing an old backup onto a live one can't revert the actual
  current project to a stale value.
- `highlight_with_bat`/`highlight_with_bat_auto` (the preview-pane syntax
  highlighters used by `ag`, `$` tags, CodeGraph, notes, todo, segments,
  similar, and files modes) are now backed by
  [`syntect`](https://github.com/trishume/syntect) in-process instead of
  shelling out to the `bat` binary, matching `tui.highlight`'s engine. No config
  or behavior change — same `Option<String>` ANSI output — but these preview
  panes now work without `bat` installed and no longer pay a subprocess call per
  render.

### Fixed

- Project time tracking (`project_sessions`, `project_current`, and the new
  `sticky` behavior above) never actually ran for commands recorded via
  `smarthistory capture-tmux`/`capture-herdr` — only the plain
  `smarthistory add` path resolved a project and opened/closed sessions. Since
  `_smarthistory_precmd` routes through `capture-tmux`/`capture-herdr` for
  anyone inside a real tmux pane (with pipe-pane output logging set up) or a
  herdr workspace — likely most interactive usage, not the exception — this
  meant project tracking silently never engaged for a large share of users, with
  `project report` always showing everything as `untracked` regardless of
  `project.<slug>.dir` config. Both commands now call the same project
  resolution/session-lifecycle sequence `add` always has, via a new shared
  `track_project_for_pwd` helper, so tracking works the same no matter which of
  the three recording paths a given command happens to take.
- `smarthistory serve`'s `/api/report` was slow to load a day, and could
  outright fail to load one at all. Three separate issues, all in
  `build_day_report`'s website-visits section:
  - Every request re-copied each configured/auto-detected browser's _entire_
    history database from scratch (Chrome/Firefox/Safari history files are
    routinely tens to hundreds of MB) just to filter it down to one day's
    entries in-process afterward. Now cached process-wide for 30 seconds, keyed
    by the resolved source list — clicking through several days in one sitting
    pays for the copy once, not once per click.
  - Every request re-issued a live JIRA REST call for every distinct issue key
    referenced that day, even for a day just viewed a second ago — the
    label-lookup cache `resolve_project_for_website_visit` uses was rebuilt
    empty on every call instead of persisting. Now cached process-wide for 5
    minutes, shared across requests, so a ticket referenced across multiple days
    only costs one round-trip for the life of the server.
  - When JIRA is configured, resolving a JIRA-linked visit builds (and, at the
    end of the request, drops) a `reqwest::blocking::Client` — which internally
    spins up its own tokio runtime. Doing that from a worker thread already
    inside `axum::serve`'s own runtime panicked ("Cannot drop a runtime in a
    context where blocking is not allowed"), crashing the connection outright
    for any day with a JIRA-linked visit. Every handler now runs its
    (synchronous, DB/filesystem/network-bound) work via
    `tokio::task::spawn_blocking` instead of directly on the async runtime's
    worker thread, which fixes this and, incidentally, stops that same
    synchronous work from blocking the runtime's worker threads in general.
- `Up` on an empty command line no longer gets hijacked by the
  `dropdown.predict` prediction dropdown. With both `dropdown.enabled=on` and
  `dropdown.predict=on`, an empty prompt after running a command shows a "what
  usually comes next" hint — but `Up`/`Down` unconditionally routed into
  navigating _that_ list whenever it was visible, so pressing `Up` to recall the
  command you'd just run silently did nothing (or landed on an unrelated
  predicted successor) whenever `smarthistory next` happened to have a
  suggestion for it. `Up` now always walks real history first. `Down` keeps a
  path to predictions, but only once real history is genuinely exhausted walking
  forward (pressed first with nothing navigated yet, or `Down`'d all the way
  back after `Up`'ing into history) — at that point it activates the prediction
  dropdown and highlights its first candidate, same as navigating there
  manually; `Up`/`Down` then cycle the predictions like a normal typed-search
  dropdown until reset, and pressing `Up` again at the very top candidate exits
  predictions back into real history right where `Down` left off, mirroring how
  `Down` got in. `Up`/`Down` on a normal typed-search dropdown (non-empty line)
  are unaffected. `Down`'s predictions also work on the very first empty prompt
  of a brand-new shell now: with no last command run yet, `smarthistory next`
  has no successor to predict from, so it falls back to the most frequent
  commands among the last 100 history rows instead of showing nothing, excluding
  `smarthistory ask`/`?`-mode question entries (also real `history` rows, but
  not commands worth suggesting back) (DIR scope still applies; SESS scope is
  skipped for this fallback specifically, since a session-scoped query is
  guaranteed empty at that exact moment). `smarthistory next`'s `command`
  argument is now optional for this. (Also fixed a pre-existing off-by-one in
  `Down`'s "start of list" boundary check, masked until now because it happened
  to produce the same empty-buffer result either way.)
- Added three indexes on `history` (`timestamp`, `session_id, timestamp`,
  `directory, timestamp`) that the schema was missing. The main history fetch
  sorts by `timestamp DESC` and the SESS/DIR scopes additionally filter by
  `session_id`/`directory` equality, none of which the existing
  `idx_history_dedup` (command, directory, session_id) could serve — every fetch
  fell back to a full table scan plus an external sort before `LIMIT 1000` could
  truncate anything. At small history sizes this was unnoticeable; at very large
  ones (hundreds of thousands to millions of rows) it meant every keystroke
  re-scanned the whole table. Verified via `EXPLAIN QUERY PLAN`: SESS-scoped
  fetches now do an index seek
  (`SEARCH ... USING COVERING INDEX idx_history_session_ts`) instead of a full
  scan, and the unscoped/global fetch does an ordered index scan
  (`SCAN ... USING COVERING INDEX idx_history_timestamp`) instead of
  scan-then-sort. `Mode::Stats`'s `LEAD()` window query still has to visit every
  row (window functions can't skip rows), but no longer needs a separate
  temp-B-tree sort pass first.
- The TUI re-fetched `labeled_rows` (every history row with a comment) from the
  database on every single keystroke, even though its SQL has no dependency on
  the typed query at all — query-based filtering happens in-memory afterward, in
  `build_merged_rows`. The data only actually changes when a comment is
  added/edited/deleted, and every action that does that already re-fetches it
  explicitly right after; the extra call inside `refresh()` was pure repeated
  waste, worse the more commented history entries exist.
- `smarthistory project report`'s per-command duration query computed its
  `LEAD()` window over every `mode = 'command'` row in the entire history table,
  regardless of how narrow the requested `--day`/date range was — the range
  filter was only applied after the window function ran. Now scoped to
  `[range_start, range_end + idle_threshold)`: the lower bound is exact (the
  window only ever looks forward in time, so earlier rows can never affect an
  in-range row's computed duration), and the upper bound is padded by the idle
  threshold so a command near the end of the range still sees its real next
  command if one exists within the idle window — a row whose real successor
  falls beyond the padding is, by construction, more than the idle threshold
  away either way, so the capped result comes out identical whether the exact
  gap is known or conservatively missing.
- Comment-expansion (`smarthistory expand`, the space-triggered zsh/bash widget)
  matched case-insensitively (SQLite's `COLLATE NOCASE`), so a short common
  lowercase word (e.g. `rust`) could unintentionally expand a comment stored
  with different casing (e.g. `RUST`). Matching is now case-sensitive (SQLite's
  default `BINARY` collation): `rust` stays a normal command-line word, only the
  exact-case `RUST` triggers expansion.

## 2.0.0 - 2026-08-14

### Added

- New `smarthistory project files`: prints the files viewed/modified/created
  since the currently-open project session started — scoped to the live session
  rather than a whole calendar day like `project report`. Reads the open
  `project_sessions` row directly (`end_ts IS NULL`) instead of re-resolving the
  project from the cwd. Prints "no active project session" (exit 1) when nothing
  is open, including while paused.
- New `fileviewcommands` config key (default `less more bat tail head`):
  `smarthistory add` now automatically records a `viewed` file event for these
  commands' file argument — no editor hook needed for the common case of paging
  through a file from the shell (`tail -f app.log`). The first non-flag argument
  is taken as the file (`tail -f app.log` → `app.log`, flags before it are
  skipped); a flag that takes a separate value (`head -n 20 file.csv`) isn't
  understood and picks up the value instead — a known, accepted limitation for a
  handful of pager-style commands, not worth a real argument parser. Setting the
  config key replaces the default list entirely, same convention `ignorecapture`
  uses.
- New `smarthistory file viewed|modified|created <path>`: records a file
  view/edit/creation event, meant to be called from an editor hook (a Vim
  `autocmd`, an LSP client, a file-watcher script). Stored in a new
  `file_events` table, attributed to a project using the same
  marker-file/`project.<slug>.dir`/last-explicit-selection resolution
  directories already use, but resolved from the file's own directory rather
  than the caller's cwd (an editor process's cwd isn't necessarily the file's
  directory). `smarthistory project report` gains Files viewed/modified/created
  sections per project, deduplicated by path with an occurrence count.
- New `smarthistory project pause`: a toggle to manually stop project time
  tracking (e.g. for a lunch break or a meeting) and resume it later. The first
  call closes the open session (`end_reason = "paused"`) and suppresses all
  project resolution — directory, marker file, explicit selection — until
  resumed, so `cd`-ing into a directory-bound project's tree while paused
  doesn't quietly restart tracking. The second call restores the exact project
  that was active at the moment of pausing, not whatever the current directory
  resolves to.
- New `smarthistory project current`: prints the project the current directory
  resolves to (same priority `smarthistory add` uses), for scripting or
  shell-prompt embedding.
- **Time tracking**: attributes directories, commands, notes created, and
  websites visited to a project — a `type: project` note in `notes.database` —
  resolved by directory (`project.<slug>.dir`, longest-prefix match, or an
  in-repo `.smarthistory-project` marker file) or by explicit selection via the
  new `.` prefix mode. No daemon: sessions open/close lazily, piggybacked on the
  existing `smarthistory add` command-recording path — a directory change closes
  the prior session immediately, an idle gap (`project.idlethreshold`, default
  30 minutes) closes it backdated to the last real activity, and an explicit
  switch always closes it. New
  `smarthistory project report [--day ...] [--project <slug>] [--min-duration <secs>]`
  prints a per-project daily rollup (directories, commands with duration derived
  at query time and capped at the idle threshold, notes created during a tracked
  window, and websites). Websites — browser bookmarks/history plus JIRA
  REST-mode visits — resolve through a 3-tier priority (`jiralabel.<slug>.match`
  → `weburl.<slug>.match` → a time-based fallback against whichever project
  session was open) and cluster for display via
  `weburlgroup.<name>.match`/`.label`, independent of assignment.
  `smarthistory config check` cross-references
  `project.<slug>.dir`/`jiralabel.<slug>.match`/`weburl.<slug>.match` against
  `type: project` note slugs and warns on either side having no match. See
  [`docs/modes/project.md`](docs/modes/project.md) for the full reference.

## 1.5.0 - 2026-08-09

### Added

- New `Ctrl-z` (`CycleNavPrefix`) action: cycles directly between the three
  navigation prefix modes — `*` (panes), `#` (directories), `~` (zoxide) —
  without going through the full `PickPrefix` picker. Reads the actual
  configured prefix chars, so a remapped `prefix.*` still cycles correctly. From
  any other mode (plain history, another prefix, or an empty query), jumps
  straight to panes rather than no-op-ing; the typed body (if any) is preserved
  across the switch, same as `PickPrefix`.
- `session.<key>`/`host.<key>` (`~/.config/smarthistory/sessions`/`hosts`) no
  longer require a manually-numbered `<key>` (`session.1`, `session.2`, …) —
  error-prone to hand-edit, since inserting an entry meant renumbering
  everything after it, and two entries could silently collide on the same
  number. New entries written by the TUI (F5/F6, the `~` Zoxide save prompt) now
  get a `<key>` slugified from the display name instead (`session.monorepo`,
  `host.prod-db`), deduplicated with a `-2`/`-3`/… suffix on collision
  (`crate::util::slugify`/`unique_slug`). Display order is file declaration
  order either way — nothing to renumber. Fully backward compatible: `<key>` was
  always just an opaque join key to the parser, so existing numeric-keyed
  entries keep working unmodified, no migration needed.
- The `--glob-complete[-dir]`/`--pid-complete` pickers (`vi a*<TAB>`,
  `cd proj*<TAB>`, `kill sleep<TAB>`) now prefill the query with a trailing
  space (`a*`, `proj*`, `sleep`) instead of no space — ready to keep typing an
  extra narrowing word immediately, no need to press space first.
- `init.zsh` now exports `SMARTHISTORY_MODE` (`sess`/`dir`/`global`) and
  `SMARTHISTORY_MATCHMODE` (`prefix`/`substring`) as real environment variables,
  kept in sync on every `Ctrl-g`/`Ctrl-t` toggle
  (`_smarthistory_sync_prompt_env`) — lets an external prompt system
  (oh-my-posh, starship, …) show the current widget state itself, since those
  run as a separate subprocess per prompt render and can't see zsh-internal
  shell variables. See "Published environment variables" in
  docs/configuration.md for oh-my-posh/starship segment examples.
- New `dropdown.matchmode=prefix|substring` config key (default `prefix`,
  matching the historical hardcoded behavior): the live dropdown-completion
  widget's match mode against history. `Ctrl-t`
  (`_smarthistory_cycle_matchmode`) toggles between `prefix` (only commands
  STARTING WITH what's typed) and `substring` (matches anywhere in the command,
  the same broader match `Up`/`Down` and the TUI's own search already use) at
  runtime, same relationship `zsh.mode`/`Ctrl-g` has to the search-scope cycle.
  Each press confirms the new mode with a transient `zle -M` status message
  ("smarthistory match set to substring"/"smarthistory mode set to DIR" for
  `Ctrl-g` too), replacing the earlier RPROMPT-text approach entirely.

## 1.4.0 - 2026-08-09

### Added

- New `globcomplete.enabled` zsh feature (off by default): replaces
  fzf-tab-style completion for files, directories, and processes. Pressing `Tab`
  on a word containing shell-glob syntax (`* ? [`) — e.g. `vi a*<TAB>` or
  `vi foo/a*<TAB>` — launches `smarthistory tui --glob-complete <word>`, the TUI
  locked into a file-completion picker instead of running normal zsh completion;
  anything else still falls through unchanged. The word is prefilled/expanded
  (not replaced) as the filter, scoped to the directory before the last `/` when
  one is given, and matched recursively against basenames (fzf-style fuzzy find,
  not literal single-level glob semantics) via a new glob-to-regex translator
  (`crate::files::glob_to_regex`). Typing a space then more text inside the
  picker narrows further by plain substring against each file's path
  (`*.md jira` matches every markdown file whose path contains "jira", not just
  files literally named `jira*.md`). Inside the picker, mode-switching is locked
  (the query can never leave files mode, `F1`/`Ctrl-]` are disabled); `Ctrl-A`
  marks every visible row, and `Enter` returns every marked row's path —
  relative to the shell's cwd (matching how a real shell glob expansion reads),
  space-joined, shell-quoted — or just the current row if nothing is marked,
  spliced into the command line in place of the typed word (never runs the
  line). When the command being completed is `cd`, the SAME
  glob/root-scoping/narrowing rules open a directory picker instead
  (`smarthistory tui --glob-complete-dir <PATTERN>`) — only real directories on
  disk are shown, and there's no multi-select (`Ctrl-A` is a no-op, Enter always
  returns just the single highlighted directory) since cd-ing into more than one
  directory doesn't mean anything. Selecting a directory row shows its immediate
  contents (directories first, then files, hidden entries excluded) in the
  output preview pane. When the command being completed is `kill`, Tab opens the
  processes (`%`) mode picker instead — with or without glob syntax, since PIDs
  have no glob concept (`kill <TAB>` shows every process, `kill firefox<TAB>`
  narrows by name/cmdline/cwd/exe). Multi-select IS available here (`Ctrl-A`
  marks every visible row); `Enter` returns every marked (or just the selected)
  process's PID, space-joined, instead of opening `%` mode's normal
  signal-confirmation dialog. New `smarthistory tui --glob-complete <PATTERN>`,
  `--glob-complete-dir <PATTERN>`, `--pid-complete <PATTERN>`, and
  `--root <DIR>` CLI flags (the last also usable to override the base directory
  for plain `/` mode's walk; unused by `--pid-complete`, which has no filesystem
  walk to root).

### Fixed

- `/` (files) mode: glob syntax (`* ? [`) in the typed filter's first word now
  actually works in plain interactive use, not just inside the `--glob-complete`
  picker — `docs/modes/files.md` already documented `/*.toml` and `*<glob>` path
  segments as supported, but the walker only ever did literal AND-of-substring
  matching, so a query like `* tui` required a literal `*` character in the
  filename (never happens) and matched nothing, forever. The first word is now
  glob-matched (root-scoped, recursive, basename-only) exactly like the picker;
  every word after it still narrows further by substring, and a query with no
  glob-looking first word is completely unaffected.
- Preview-pane markdown renderer: an unclosed italic marker that was actually a
  literal underscore (e.g. a directory or file name like `alpha_sub`) was
  silently rewritten to an asterisk on render (`alpha_sub` → `alpha*sub`) — the
  fallback-to-plain-text path for an unclosed marker reconstructed the marker
  from a fixed per-_kind_ string (`MarkerKind::Italic` always spelled `"*"`)
  instead of the actual character that opened it, since `*` and `_` are both
  valid italic openers but only one was ever remembered. Found via the new
  glob-completion directory picker's content preview, but affects any preview
  text containing a bare underscore in any mode.

## 1.3.0 - 2026-08-08

### Added

- New `%` (processes) mode: lists every running OS process (macOS + Linux, all
  users), via the new `sysinfo` dependency. The typed body filters by substring
  against the process's name/cmdline, working directory, and executable path.
  `Enter` on a row does not stage/run the process name as a shell command — it
  opens a confirmation dialog to send it a signal, defaulting to SIGTERM with
  `Tab`/`Shift-Tab` cycling to SIGKILL/SIGHUP/ SIGINT before confirming with
  `y`; the dialog's message updates live as the signal is cycled. Sending a
  signal to a process you don't own fails with a status-line message rather than
  crashing, the same way `kill(1)` itself would. The details/preview pane shows
  the process's working directory, executable path, and (loaded lazily on first
  selection) its full environment (`NAME=value`, one per line); a process whose
  environment can't be read (permission denied, or it already exited) shows a
  graceful placeholder instead of erroring. No `#[cfg(target_os = ...)]` needed
  anywhere — `sysinfo` abstracts macOS vs. Linux for everything this mode needs;
  the only platform-observable difference is permission behavior on the
  environment read, which both platforms funnel into the same placeholder text.
- Live dropdown completion: each candidate row now shows a `✓`/`✗` exit-status
  marker (green/red, same palette as `dropdown.highlight`'s command-validity
  check) right after the selection marker, so a previously-failed command can be
  spotted without opening the TUI or re-running it. Backed by
  `smarthistory search`'s existing `exit_code` field (no CLI/backend changes
  needed) — the row-parsing regex and `marker_len` layout constant both grow to
  make room for it; a row whose exit code couldn't be parsed draws a blank
  marker instead of guessing.
- New curated theme: `luna` (from
  [luna.nvim](https://github.com/WTFox/luna.nvim)) — a low-saturation,
  near-black dark theme with a blue accent and warm orange secondary. Now 74
  built-in themes ship in total (15 upstream + 59 curated).
- README: noted that `cargo install --path .` needs `--locked` to avoid a fresh
  dependency re-resolution that can pick a broken transitive version (e.g.
  `zune-jpeg`, via `image`/`arboard`) instead of the tested `Cargo.lock`
  versions `cargo build`/`cargo test`/CI already use.
- The age column is now color-coded by recency — brightest for entries from the
  last minute, fading through green (minutes), the previous flat accent color
  (hours), dim (days), to dimmest (months or older) — a glanceable freshness
  gradient on top of the existing text, reading the bucket straight off the
  existing seconds/minutes/hours/days/months unit ladder rather than adding a
  second time calculation.
- The `[x]`/`[ ]` multi-select mark column now only appears in the modes where
  marking a row actually has an effect — plain history and `+` Output (real ids,
  `BulkDeleteMarked` works), `/` Files, `!` Todo, and `-` JIRA (each has a
  `SmartOpen` handler that acts on every marked row). Hidden everywhere else,
  where a mark would just be an inert checkbox nothing ever reads.
- `/` (files) mode: each row's displayed path (relative to the walked root) is
  now shortened for display — every directory component abbreviated to its first
  character, filename always shown in full — same convention `,` (ag) mode uses.
  Search/filtering still matches against the real, unabbreviated path; only the
  on-screen text changes.
- Help overlay (`C-a` by default): new "Row indicators" section explains what
  the `[x]`, `o`/`.`, `T`/`.`, and `✓`/`✗`/`~` row glyphs mean and which mode(s)
  each one appears in.
- List rows now hide three fixed-width indicator columns (exit-status
  `✓`/`✗`/`~`, output-capture `o`/`.`, and tmux-pane `T`/`.`) entirely in modes
  where they never carry any information, instead of always reserving their
  column width with a permanently-identical placeholder. The exit-status column
  now only appears in modes whose rows can carry a genuinely varying exit code —
  plain history, `+` Output, `=` LLM, `?` Question (all backed by the shared
  history table), and `-` JIRA (which repurposes it as a closed/open indicator)
  — hidden everywhere else, where it hardcoded `exit_code: 0` and so was always
  the same `✓`. The output-capture column now only appears in plain history
  mode; the tmux-pane column only appears in `#` Directories and `~` Zoxide mode
  (including no longer showing in `*` Panes mode itself, whose own rows already
  are the live panes).
- `,` (ag) mode: each row now shows the matched file's path first, shortened as
  compactly as possible (every directory component abbreviated to its first
  character, filename always shown in full — e.g. `~/w/p/src/main.rs` for
  `~/work/project/src/main.rs`), followed by the matched line content. New
  `util::shorten_path_dirs` helper backs it.
- New `smarthistory prune-directories [-f]` CLI command: checks every
  `session.<id>.dir` (in `sessions` and the main `config` file) against the
  filesystem and removes the whole entry — name, `.dir`, `.exec`,
  `.startup_command` — for any directory that no longer exists, after listing
  what will be removed and asking for confirmation (`-f`/`--force` skips it).
  Entries with no `.dir` set are left alone; `host.<id>` entries are untouched.
- `~` zoxide mode: selecting a directory not already saved as a `session.<id>`
  entry now asks "Save directory?" first. `Enter`/`y` writes a new
  `session.<id>` entry (name + `.dir` only, no `.exec`) before jumping there;
  `n`/Cancel skips the save. Either answer still completes the jump — the prompt
  never blocks it, and an already-saved directory skips the prompt entirely.
- `^` browser mode: `Ctrl-]` (`SmartOpen`) now converts the selected
  bookmark/history entry into a local markdown note
  (`note_search convert <url>`) and opens it in `$EDITOR`, instead of the
  default "open the URL" (still what plain `Enter` does). The target path is
  captured from `note_search`'s own output, since `convert` names the file
  itself.
- `*` panes mode: `Enter` on the `# Sessions` / `# Directories` / `# hosts`
  group header now collapses/expands it (`▾`/`▸` triangle) instead of trying to
  focus something. In-memory for the current launch only; an individual live
  workspace's own `##` sub-heading is unaffected and still stages its focus
  command as before.
- `*` panes mode: every pane's name now renders bold, always — not just panes
  that get the dominant `▶` running marker.
- `*` panes mode: live tmux/herdr workspaces now wrap under a common synthetic
  `# Sessions` heading, with each individual workspace rendered as a `##`
  sub-heading underneath (panes indented one level deeper) — matching the
  `Directories`/`hosts` sections' own `#`-headed look. Purely presentational:
  each workspace remains its own independently filterable/group-scopable group,
  unchanged.
- `,` (ag) mode: each row's timestamp is now the matched file's real
  modification time (was always `0`/Unix-epoch), and results sort
  newest-modified file first (was `ag`'s own arbitrary output order). Matches
  within the same file keep their line-number order.

### Fixed

- Line-editor live dropdown: `Enter` now commits AND runs a highlighted
  candidate in one press, same as before the Tab/Enter key-model rework —
  navigate with `Up`/`Down`, then `Enter` alone accepts it. Previously `Enter`
  only ever ran the raw typed text; committing a candidate required pressing
  `Tab` first, then `Enter` separately. Gated on the same "must be explicitly
  selected first" condition `Tab` already uses, so this doesn't reopen the
  single-candidate auto-complete bug that rework fixed — an unmodified `Enter`
  on a fresh, not-yet-navigated dropdown still just runs what you typed.
- TUI startup: the `sessiondirs=...` walk (recursively listing every
  subdirectory of each configured root) used to run unconditionally on every
  launch, even for sessions that never visit `#` (Directories) or `~` (Zoxide)
  mode. It's now lazy — deferred to the first actual entry into one of those
  modes, and cached for the rest of the session, same pattern already used for
  the tmux/herdr pane snapshot. `smarthistory check` is unaffected (it still
  walks eagerly up front, since it's a one-shot report with no interactive mode
  entry to defer to).
- Help overlay: the `SmartOpen` row's summary said `~` opens the selected file
  via the per-extension command — `~` is Zoxide; the Files-mode prefix is `/`.
  Text corrected.
- Line-editor live dropdown (`dropdown.enabled`): reworked the key model so
  history completion can never silently rewrite the command line to something
  you didn't select. `Up`/`Down` are now the only way to select a candidate;
  `Tab` copies the highlighted one into the command line (or falls through to
  normal zsh completion when nothing's highlighted — including when there's only
  a single matching candidate, which used to auto-complete on the very first
  `Tab` press); `Enter` always runs whatever's on the line and no longer
  substitutes a highlighted candidate on its own. `Shift-Tab` is no longer a
  selection key (reverts to zsh's default `reverse-menu-complete`) — `Up`/`Down`
  cover both directions.
- `*` (panes) mode, herdr backend: the background per-pane process-command
  lookup could respawn a full round of subprocess lookups (one per pane) on
  every ~100ms run-loop tick, forever, as soon as the previous round finished —
  instead of once per actual panes-list change. This made the view painfully
  sluggish on a slow/high-latency connection. Now spawned at most once per
  snapshot.
- `/` (files) mode: rows now carry the file's real modification time (was always
  `0`/Unix-epoch) and the list sorts newest-modified first (was
  directories-first then alphabetical). Matches what `docs/modes/files.md`
  already documented.

## 1.2.6 - 2026-08-05

### Added

- `*` panes mode: a pane actually running something (not just an idle shell
  prompt) now gets a dominant `▶` marker in bold + the highlight color, so busy
  panes stand out immediately in a long list.
- `create-note` dialog: `Ctrl-A` selects the whole active field (Title or
  Content). While selected, `Ctrl-C` yanks it to the clipboard instead of
  cancelling the dialog, and `Backspace` deletes the whole field instead of one
  character; any other key drops the selection.

## 1.2.5 - 2026-08-04

### Added

- New `~` prefix mode: zoxide directories. Lists every directory in the local
  `zoxide` database (`zoxide query -l`, highest frecency score first), filtered
  by the typed query. Selecting a row creates a new tmux session / herdr
  workspace rooted there — the same staging `#` Directories mode uses for an
  unmarked row, including the `T`-marked "jump to an already-active pane there"
  behavior. Requires the `zoxide` binary on `$PATH`; see `docs/modes/zoxide.md`.
- Release CI: added `ubuntu-22.04` (glibc 2.35) as a second Linux build target
  alongside `ubuntu-latest` (now tracking 24.04/glibc 2.39), so the released
  binary still runs on older distros whose glibc predates 2.39.

## 1.2.4 - 2026-08-03

### Added

- New `smarthistory create-note <title> <content>` [--edit]: create a note
  directly from the CLI, no TUI or interactive dialog needed. Builds the same
  level-3-heading body (title/content, extracted `[[link]]`s and `#tag`s) the
  interactive dialog stages on `Ctrl-S` and runs `note_search create-note`
  directly; `--edit` opens `$EDITOR` on the daily note right after saving.
  `build_note_body()` extracts the heading-building logic so the TUI dialog and
  this CLI path share one implementation (also fixed a latent bug where tags
  pulled from the content field lost their leading `#` when merged into the
  heading).
- `smarthistory create-note` now launches the same interactive dialog
  `Action::CreateNote` opens (equivalent to `smarthistory tui --create-note`),
  pre-filled from `--title`/`--content`, instead of writing the note headlessly.
  Also adds `Up`/`Down` arrow navigation inside the dialog's Content field (move
  a line at a time, preserving column) — `Left`/`Right` already worked but
  `Up`/`Down` were unhandled.

### Fixed

- The create-note dialog's `Esc`/`Ctrl-C` used to close it immediately, silently
  discarding whatever was typed. Now, if either field has text, a confirmation
  opens first: `Enter` (default) saves, `d`/`D` drops, the Cancel binding backs
  out to editing without losing anything, and `Ctrl-C` force-quits the whole TUI
  immediately (same panic-button semantics as the existing delete
  confirmations). An empty dialog still closes immediately.

## 1.2.3 - 2026-08-02

### Added

- New `smarthistory pane-exec`: reconnects a freshly opened pane/window (one not
  opened via smarthistory's own `*` panes picker) by looking up its current
  session name/workspace label against the matching `session.<id>`/`host.<id>`
  config entry — no separate registration step needed. A session match re-runs
  `.exec` directly; a host match re-runs just the `ssh` connection.
- New `smarthistory init bash`: bash/readline support, deliberately smaller in
  scope than zsh (history capture,
  `Ctrl-R`/`Up`/`Down`/comment-expansion/`Ctrl-S`/`Ctrl-G` widgets — no live
  dropdown box, since that's built entirely on zsh's
  POSTDISPLAY/region_highlight, which Readline has no equivalent of). The
  capture pipeline works down to bash 3.2 (macOS's stock default); the
  line-editor widgets need bash >= 4.0 for `READLINE_LINE`/`READLINE_POINT` via
  `bind -x`.

## 1.2.2 - 2026-08-02

### Added

- New `segments.minwords` config key (default `5`, `0` disables): drops any note
  segment (`:` and `"` mode) whose body has this many words or fewer, not
  counting its own header line — a heading with little or nothing under it is
  noise most of the time. Applies to both `run_segments_search` (`:` mode) and
  `run_similar_search` (`"` mode) via a shared `segment_body_word_count` helper.
- Dropdown shadow text: once a candidate is actually highlighted
  (`Tab`/`Shift-Tab`/`Up`/`Down` navigated to a specific row, not just the fresh
  unhighlighted box), its not-yet-typed remainder now previews inline right
  after the cursor, dimmed — the same visual convention zsh-autosuggestions
  uses. `Right`/`Ctrl-E`/`Enter` already committed exactly this text; always on
  whenever `dropdown.enabled=on`, no new config key needed since there's no new
  subprocess cost.

## 1.2.0 - 2026-08-01

### Added

- New `dropdown.highlight=on|off` config key (default off): syntax-color each
  dropdown candidate via `bat`. Off by default, mirroring
  `dropdown.enabled`/`commentexpand.enabled`'s config plumbing.
- New `commentexpand.enabled=on|off` config key (default off): the
  space-triggered comment-expansion zsh widget — typing a comment's text (set
  via `smarthistory add ... --comment ...`) at the start of the line, then a
  space, replaces it with the most recently used command carrying that comment,
  the same UX as zsh-abbr/fish abbreviations.
- Dropdown `Tab`: the first press on a fresh candidate set now extends the
  command line to the longest prefix common to every current candidate
  (readline-style "expand to unambiguous completion"), re-queries against that
  longer prefix, then jumps straight to "chosen" so the next `Tab` cycles
  normally instead of re-expanding a no-op.
- New `Ctrl-O` binding in the create-note dialog: saves then opens the note in
  `$EDITOR` (chains `note_search create-note` with `&& $EDITOR <path>`),
  alongside the existing `Ctrl-S` (save and exit only).
- Release CI: pushing a `v*` tag now builds native binaries on macOS (arm64 +
  x86_64) and Linux (x86_64), runs the test suite, strips and packages each as
  `smarthistory-<target-triple>.tar.gz` with a `.sha256` checksum, and uploads
  both as GitHub Release assets.

### Fixed

- Comment-expansion and dropdown widgets could reference each other's nested
  wrapper functions after re-sourcing `init.zsh`, causing "maximum nested
  function level reached" on the next keystroke. Replaced with a single
  dispatcher per widget backed by a dedup'd hook list, which re-sourcing only
  appends to. Also hooked `magic-space` (not just `self-insert`), since many
  setups — including stock oh-my-zsh — rebind the space key to it, which
  previously meant comment-expansion never fired at all.
- `dropdown.highlight`'s `bat` call was missing `--theme` entirely, falling back
  to `bat`'s own default theme instead of the light/dark choice the Rust side
  already makes everywhere else. Now computed from the resolved `tuicolor.bg`
  value at shell-init time (`smarthistory config get palette`), same ITU-R
  BT.601 brightness formula the Rust side uses.
- `smarthistory config get palette` always resolved colors as the Dark scheme
  regardless of what the user last had active in the TUI. Now reads the
  persisted `colorscheme=` line from the session file
  (`TuiSession::persisted_scheme()`), so with `theme.dark`/`theme.light` both
  configured, toggling the scheme in the TUI and opening a new shell changes the
  dropdown's colors to match.

## 1.1.0 - 2026-08-01

### Added

- New `^` prefix mode: browser bookmarks + history, merged from every configured
  (or auto-detected) Chrome / Firefox / Safari profile. Each row is tagged
  `bookmark` / `history` so typing that word narrows the list to one source;
  `Enter` opens the URL in the system browser. Configure via
  `browser.<id>.type=chrome|firefox|safari` + `browser.<id>.profile=<path>`; see
  `docs/modes/browser.md`.
- `session.<id>` and `host.<id>` entries can now live in their own dedicated
  `~/.config/smarthistory/hosts` and `~/.config/smarthistory/sessions` files
  instead of (or split across, alongside) the main config file. Both are read
  only by the TUI (`Config::load_tui`), not the plain CLI subcommands (`search`,
  `add`, `capture-*`, …), since session/host data is exclusively a `*`-mode
  (panes) concern. The in-TUI "add session" (`F5`) / "add host" (`F6`) dialogs
  now write new entries to these dedicated files, creating them if they don't
  exist yet.
- New `'` meta-prefix mode: type `'` then a partial mode name (e.g. `'jir`) and
  press Tab to jump straight into that mode by name instead of memorizing its
  single-character prefix. A unique match activates immediately (query becomes
  just the target prefix, e.g. `-`); an ambiguous match, or bare `'` + Tab,
  opens the same picker `F1` (`PickPrefix`) uses, pre-filtered to the matching
  names. Configurable via `prefix.meta=<char>` (default `'`). Also fixes a
  pre-existing bug where `apply_prefix` (the `F1` picker's commit path) didn't
  recognize the paperless (`<`) or browser (`^`) prefixes as strippable when
  switching modes.
- `CreateNote` (the Title + Content dialog) now pre-fills from the row that was
  selected when the action fired: a question row splits into Title (the
  question) + Content (the LLM's answer); a note row inserts a `[[wiki-link]]`;
  a JIRA row inserts a markdown link to the issue's browse URL (bare key if JIRA
  isn't configured); every other row (plain history, or any other mode) wraps
  the command text in a fenced ` ```bash ` block.

### Changed

- Extracted the large `select_for_run_impl` staging method from `src/tui.rs`
  into a new `src/tui/actions.rs` module, shrinking `tui.rs` by ~2,500 lines.
- Moved `parse_bool` into `src/util.rs` and removed the duplicate copy in
  `src/tui.rs`, so the CLI and TUI session parser share one implementation.
- The symbols (`$`) prefix now supports an `@lang` token, mirroring the `ag`
  (`,`) prefix. `$MyStruct @rust` filters the result set to symbols defined in
  `.rs` files and pipes the per-row source-context preview through
  `bat --language <lang>` so the output preview pane shows syntax-highlighted
  code. The shared `parse_query_tokens` helper in the new `src/highlight.rs`
  module backs both modes (and any future content view that wants the same
  classification).
- `DeleteWordBackward` now ships with two default bindings: the readline-style
  `Ctrl-W` **and** the macOS / GUI-editor-style `Alt-Backspace`. Both fire the
  same action, so users coming from either muscle memory get the expected
  behaviour without remapping. The action's `Action::default_keys()` API exposes
  the full list so the command palette, help overlay, and config printer can
  render both specs; either can be removed via `key.delete-word-backward=...` in
  the config file.
- The panes (`*`) prefix is now a properly-typed tree: every pane row carries a
  `[<label>]` chip showing the session / workspace it belongs to, and the filter
  is **group-aware**. Typing a token that matches a workspace label keeps the
  whole workspace (header + every child pane); typing a token that matches a
  pane's command / cwd keeps that pane and its parent workspace header. The new
  `HistoryRow::workspace_label` field carries the label from
  `fetch_session_panes_impl` to the renderer.
- New TUI action `Action::DownloadJiraIssue` (default key `Ctrl-M-s`) downloads
  the selected JIRA issue as a local markdown note by staging
  `note_search jira-issue <KEY>`. The action is mode-gated to the JIRA search
  mode (`-...`); outside of JIRA mode it's a no-op with a status message so the
  user understands why their key did nothing. The bare command line is staged
  (no path, no flags) so `note_search` writes the markdown into the `notes.dir`
  configured in the same config file.
- The status bar (the footer line at the bottom of the TUI) no longer surfaces
  the two delete actions in its key-binding hints. The `del` and `del all` chips
  have been replaced with a `palette` chip showing the current `CommandAction`
  binding (default `:`). The delete actions are still discoverable via the help
  overlay (`Ctrl-H`) and the command palette itself, which lists every action
  with its current binding.
- The JIRA search-as-you-type now has two additional trigger paths alongside the
  existing 400ms fast debounce:
  1. **Space trigger** — typing a space inside the JIRA query body fires the
     search immediately, bypassing the debounce. This matches IDE autocomplete
     conventions (a space commits the current token to a search).
  2. **3-second idle safety- net timer** — a new `jira_idle_started` field fires
     the search after 3 seconds of no keystroke activity, independent of the
     400ms debounce. The user reported that the query "sometimes isn't
     executed"; the idle timer guarantees the search runs within 3 seconds of
     the last keystroke regardless of whether the fast debounce ever elapses
     (e.g. the user keeps typing slowly, or the run loop is temporarily blocked
     on background work). The two timers are armed in lock-step by `jira_touch`;
     either can fire the search when its respective window elapses.
- The TUI's default key bindings now mirror the project config file
  (`~/.config/smarthistory/config`). Actions that the user has explicitly
  rebound in the config (e.g. `C-a` for `open-help`, `C-q` for `command-action`,
  `C-v` for `edit-file-reference`, `C-o` for `show-output`, `C-s` for
  `cycle-directory-source`, `F5` / `F6` / `F10` for the panes actions, `C-c` +
  `Esc` for `cancel`) now ship with those bindings as the default so a fresh
  checkout behaves the same as a configured install. Actions that the user has
  explicitly unbound (`toggle-duplicate-filter` and `delete-matching`) ship
  unbound by default (the `none` sentinel is now a valid default-key value; the
  help overlay and command palette render those actions as `(unbound)`). The
  `Cancel` action is the second action to ship with two default bindings
  (alongside `DeleteWordBackward`): `C-c` and `Esc` — both fire the same action
  so users from the bash / readline tradition (`C-c`) and the GUI-editor
  tradition (`Esc`) both get the expected behaviour without remapping.
- The JIRA search mode now supports **tab-completion of JQL field names**.
  Inside `-` mode, pressing `Tab` expands the field-name prefix immediately
  before the cursor:
  - `proj<TAB>` → `project=` (single match; cursor lands right after the `=`)
  - `lab<TAB>` → `label` (multiple matches — `label` and `labels`; extends to
    the longest common prefix with no `=`, then a second Tab on `labels<TAB>` →
    `labels=`)
  - `xyz<TAB>` → no-op + status message (no match; query unchanged) The
    completion list is the standard JQL system field set (`assignee`,
    `reporter`, `status`, `priority`, `labels`, `summary`, …) plus a few common
    custom-field conventions (`sprint`, `epic`, `parent`, `storyPoints`,
    `rank`). Outside of JIRA mode `Tab` is a no-op, so the key doesn't interfere
    with any other mode. The action is the new `Action::JiraFieldComplete`
    (default key `Tab`); the core completion logic lives in
    `crate::jira::jira_field_complete` / `jira_field_complete_with_value`, both
    unit-tested.
- **JIRA `@` alias tab-completion** — the same `Tab` key also expands `@`
  aliases and user- defined fragments inside `-` mode:
  - `@mo<TAB>` → `@month` (built-in alias with trailing space)
  - `@sp<TAB>` → `@sprint` (user- defined fragment from
    `jira.search.sprint=...`)
  - `@me<TAB>` → `@me` (exact match)
  - `@xyz<TAB>` → no-op + status message (no match) The alias list is the four
    built-ins (`me`, `today`, `week`, `month`) plus every `jira.search.<name>`
    entry from the config file. The same LCP logic as field completion applies
    to ambiguous prefixes. The completion code detects the `@` character
    immediately before the cursor and routes to `jira_alias_complete` /
    `jira_alias_complete_with_space` (both unit-tested).
- The search now fires immediately on every text-mutating action in every mode
  except JIRA. The user reported that the JIRA search "sometimes isn't executed"
  (which we fixed with the 400ms debounce / 3s idle safety-net / space trigger)
  and the corresponding complaint for the in-process search modes is "the list
  lags my typing". The new `App::trigger_text_change_search` helper is called
  from `push_char`, `backspace`, `delete_word_backward`, `clear_query`, and the
  JIRA tab-completion path. Behaviour by mode:
  - **Synchronous modes** (SESS, DIR, GLOBAL, STATS, panes `*`, directories `#`,
    symbols `$`, todos `!`, notes `@`, tags `$`, ag `,`, files `~`): the helper
    calls `self.refresh()` directly, so the row set is re-fetched on the same
    frame as the keystroke. The SQL fetch is a constant-time operation, so
    there's no frame-budget concern.
  - **LLM (`=`)**: the helper bypasses the 1s LLM debounce by temporarily
    setting `llm_debounce_started` to a past value and calling
    `llm_maybe_autocall()`. The user has typed a description; they want a
    preview now, not after 1s of typing latency. (The `llm_in_flight`
    short-circuit still prevents duplicate concurrent LLM calls.)
  - **JIRA (`-`)**: the helper is a no-op for JIRA mode. The JIRA- specific
    debounce/idle /space-trigger paths remain in effect; mixing in a per-
    keystroke fire would defeat the debounce and re-introduce the JIRA-server
    spam the debounce was designed to prevent.
- **Empty queries** (just-cleared box): the helper short- circuits before
  reaching the fetch path, so we don't waste time re-running the same all- rows
  query the user just had on screen.
- Replaced `CyclePrefix` with `PickPrefix` (`F1`). Instead of cycling blindly
  through prefixes, the action now opens a **prefix picker** overlay — a centred
  list of every configured mode (History, Output, LLM, Question, Notes, Todos,
  Directories, Panes, JIRA, Files, Tags, ag). The list pre-selects the entry
  that matches the current query's leading char (or "History" for a plain text
  query), so pressing `Enter` with no movement is a no-op. `Up` / `Down` (or
  `Ctrl-N` / `Ctrl-P`) navigate the list; `Enter` applies the selected prefix
  (body preserved); `Esc` / the user's `Cancel` binding dismisses the picker
  without changing the query. The new `PrefixPicker` / `PrefixOption` structs
  and `handle_prefix_picker_key` / `draw_prefix_picker` functions are modelled
  on the command palette and theme picker so muscle memory transfers across all
  overlays. 15 new unit tests cover `apply_prefix`, `PrefixPicker::new`, and
  picker key handling.
- The notes (`@`) and todos (`!`) prefixes now support **tag and link search**
  in addition to plain text. The query parser recognizes three token shapes:
  - `#TAG` → passed through to the `note_search` query parser as a tag filter.
    The parser already supports `#tagname` syntax, so no conversion is needed.
  - `@LINK` → converted to `[[LINK]]` (the `note_search` wiki-link syntax) for
    link search. The link name preserves the user's original casing (link
    targets are case-sensitive in Obsidian).
  - `TEXT` → passed through as a plain text term (AND-matched against the
    note/todo body). All three are AND-joined: `#TAG1 #TAG2 @LINK TEXT` finds
    notes that are tagged `TAG1` and `TAG2`, have a link to `LINK`, and contain
    `TEXT` in their body. The date aliases (`@today`, `@week`, `@month`,
    `@year`) are still extracted as a filter and applied post-query. The `@`
    prefix for link search replaces the old behavior where `@foo` was stripped
    to plain text — that stripping was a workaround for the `note_search`
    link-tokenizer, but it prevented users from actually searching by link.
    Users who want to search for the literal word `@foo` in note text can now do
    so (the token is no longer silently rewritten). The change is implemented in
    `parse_notes_query` in `src/tui.rs`; two new unit tests cover the tag and
    link tokenization, and the existing `fetch_todos_at_prefix_matches_text`
    test was updated to use plain text (without `@`) since `@` now means link
    search.
- The notes (`@`) and todos (`!`) prefixes now support **tab-completion of tags
  and links** sourced from the `note_search` database. Pressing `Tab` after
  `#feat` expands to `#feature` (unique tag match, trailing space); `@Neo`
  expands to `@NeovimNote` (unique link match). Ambiguous prefixes extend to the
  longest common prefix so the user can keep typing to disambiguate. The
  completion list is queried via
  `note_search::commands::metadata::get_unique_values`, which reads the union of
  tags and links from every indexed note. The `Action::JiraFieldComplete` action
  (bound to `Tab` by default) now routes to `notes_tab_complete_at_cursor` when
  the query is in notes or todos mode — the same `Tab` key serves JQL field
  completion in JIRA mode and tag / link completion in notes / todos mode. Two
  helper functions in `src/jira.rs` (`notes_tag_complete` /
  `notes_link_complete`) provide the pure completion logic, unit-tested with
  in-memory `note_search` databases. Seven TUI-level tests cover the end-to-end
  behaviour (unique match, LCP, no-match, todos mode, no-op outside
  notes/todos).
- Link expansion now wraps the link target in `[[...]]` syntax (the Obsidian
  wiki-link form) instead of the `@` shorthand, and strips the `.md` extension
  from the target. The `[[...]]` syntax is required because the `@` tokenizer in
  `note_search` only accepts alphanumeric / underscore / slash / hyphen / period
  characters and cannot represent link names with spaces. `@Neo<TAB>` now
  expands to `[[NeovimNote]]` (the `@` is consumed as the notes-mode prefix and
  the `@Neo` word is replaced with the full `[[...]]` form); `@my<TAB>` expands
  to `[[my note]]` for link names that contain spaces — the `[[...]]` brackets
  serve as the delimiter so no additional quoting is needed. The `.md` suffix is
  stripped from every link before matching (matching Obsidian's bare-name
  reference convention); non-`.md` extensions are preserved since those are
  actual reference targets (e.g. `.org` notes). `notes_link_complete` in
  `src/jira.rs` returns the full `[[...]]` expansion; the TUI uses the result
  directly without re-wrapping.
- New TUI actions `Action::MoveCursorLeft` (default key `Left`) and
  `Action::MoveCursorRight` (default key `Right`) move the cursor one character
  at a time inside the search query. The query string is unchanged; only the
  cursor position moves. The cursor saturates at position 0 (Left) and at the
  end of the query (Right), and is measured in UTF-8 characters so multi-byte
  characters are stepped over as single units. The new actions work in every
  mode (LLM, JIRA, notes, todos, or plain text search) since the cursor lives on
  `self.query` in all of them. To make room for the new default bindings,
  `EditStart` and `EditEnd` ship unbound by default (the `"none"` sentinel) —
  users who want the old "stage row for editing at cursor start/end" behaviour
  can rebind via `key.edit-start=...` / `key.edit-end=...` in their config. Five
  new unit tests cover the cursor-movement helpers (one-step, saturation at
  boundaries, multi-byte handling).

### Fixed

- Resolved `cargo fmt` drift in `src/ag.rs` and `src/files.rs`.
- Fixed `clippy::items_after_test_module` warning in `src/ag.rs`.

### Security

- Harden shell command staging throughout the TUI by consistently using POSIX
  single-quote escaping (`util::shell_quote`) for user-provided paths, note
  text, `.command` script arguments, and multiplexer labels/session names.

### Repository hygiene

- Expanded `.gitignore` to cover `.codegraph/`, `.pi-loop.json.lock`, generated
  `TAGS`, and local scratch files.
