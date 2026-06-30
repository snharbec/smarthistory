# Smart History

A fast, SQLite-backed shell history tool for zsh with a full-screen
picker, multiple search modes, optional LLM integration, and
context-aware navigation. This README is the high-level overview;
for the full feature list, configuration reference, and TUI key
bindings, see [TECHNICAL.md](TECHNICAL.md).

# Overview

Smart History replaces the shell's native history with a single
SQLite database and a `ratatui`-based full-screen picker. Each
shell session gets a unique UUID, every command is captured with
its directory, exit code, timestamp, and captured output, and the
line-editor's Up/Down keys always traverse this database instead
of falling through to zsh's native history.

Highlights:

- **SQLite-backed history** at
  `~/.local/cache/smarthistory/smarthistory.db` — one row per
  command, with directory, session UUID, exit code, captured
  output, and timestamp. Search by substring, regex, fuzzy
  subsequence, captured output, or note/todo content.
- **Context-aware picker.** A full-screen TUI (`Ctrl+R`) for
  searching, picking, and editing. The picker supports multiple
  *modes* (`SESS` → `DIR` → `GLOBAL` → `STATS`), each
  narrowing the same underlying database to a different slice.
- **Smart "next command" predictor** (`Ctrl+S`): ranks the entire
  global history by successor frequency via SQLite's `LEAD()`
  window function. After running `make build`, the next most-
  likely command surfaces immediately.
- **Multiple query modes** in the TUI: plain substring (default),
  regex (`/...`), fuzzy (`?...`), captured-output search
  (`+...`), LLM command generation (`=...`), general question
  (`%...`), note search (`@...`), todo search (`!...`), and
  directory jump (`#...`).
- **LLM features** (opt-in via `ollama.url` / `ollama.model`):
  translate natural-language into a runnable command (`=`),
  describe a command in plain prose (`Ctrl+K`), and correct a
  broken command (`Ctrl+T`).
- **tmux integration:** the `T` marker in directories mode
  shows which directories are already running in a tmux pane;
  pressing Enter on a marked row jumps to that pane.
- **Single Rust binary, no runtime dependencies.** No `fzf`, no
  `uuidgen`, no `/dev/urandom` access. The init snippet is
  self-contained.

# Installation

1. Clone the repository and build:

   ```bash
   cargo build --release
   ```

2. Add the resulting binary to your `PATH`:

   ```bash
   ln -s "$(pwd)/target/release/smarthistory" ~/.local/bin/smarthistory
   ```

   (or copy / move it wherever you keep your executables).

3. Initialize in your `~/.zshrc`:

   ```bash
   eval "$(smarthistory init zsh)"
   ```

   The init snippet embeds a freshly-generated session UUID and
   binds the keyboard shortcuts (`Ctrl+R` for the picker,
   `Ctrl+G` for scope cycling, `Ctrl+S` for next-command
   prediction, and a few more). Re-run it once per shell
   startup; running it twice in the same shell is a no-op.

The first time you press Up in the line editor, the database
file is created at `~/.local/cache/smarthistory/`.

# Base usage

Once the init snippet is sourced, the line editor is wired up
and the history starts collecting automatically.

## From the line editor

- **Up / Down** — step through commands (always from the
  smarthistory DB, never falling through to zsh's native
  history).
- **Ctrl+R** — open the full-screen TUI picker for live
  filtering.
- **Ctrl+G** — cycle the active *scope* (the search context the
  picker filters by): `SESS` → `DIR` → `GLOBAL` → `STATS` →
  `SESS`. The active scope is shown in the RPROMPT.
- **Ctrl+S** — insert the most-probable next command based on
  what historically followed the most recent entry. Each
  subsequent press cycles through the next candidates.
- **Esc** — clear the line.

## From the CLI

The `smarthistory` binary also exposes a few commands for
non-interactive use:

- `smarthistory search [flags]` — print the rows matching a
  filter (substring, regex, fuzzy, output, etc.) without
  opening the TUI. Useful for scripts and one-liners.
- `smarthistory next` — print the most-probable next command
  for a given prefix.
- `smarthistory clean [flags]` — bulk-delete history rows that
  match a filter, with a confirmation prompt before any
  destructive change.
- `smarthistory update` — rewrite stored directory paths in
  the SQLite history table to their `~/x` short form. Run
  this once after upgrading; the operation is idempotent and
  safe to re-run.
- `smarthistory import-atuin` — import history from an
  existing atuin database.
- `smarthistory init zsh` — emit the zsh init snippet (this
  is what `eval "$(...)"` calls).

# TUI

The full-screen picker (`Ctrl+R`) is the main work surface. It
shows a live-filtered list of history rows on the left, a
details pane with the selected row's metadata (command,
directory, exit code, captured output preview) on the right,
and a query bar at the bottom.

The query bar accepts the eight search modes documented in
the [Search modes](#search-modes) section. The visible
prompt character (`/`, `?`, `+`, `=`, `%`, `@`, `!`, `#`)
tells you which mode you're in; the input border is tinted
in the mode's color (yellow for regex, green for fuzzy, blue
for output, magenta for LLM, etc.) so the active mode is
always visible at a glance. The full list of prefix
characters and their modes is in the [Prefix keys](#prefix-keys)
section.

The search mode can also be toggled with `F3` (which cycles
plain → regex → fuzzy → output → plain), independent of the
typed prefix.

## Key bindings (subset)

- `Enter` — stage the selected row (or invoke the active
  query mode's primary action: run the command, open the
  note, jump to the tmux pane, etc.).
- `Ctrl+K` — LLM "describe" the selected command.
- `Ctrl+T` — LLM "correct" the selected command.
- `Ctrl+Y` — yank the selected command (or the captured
  output if the output view is open) to the system clipboard.
- `Ctrl+E` — edit the selected row's comment.
- `Ctrl+L` — open the captured-output view for the selected
  row (scrolling overlay).
- `Ctrl+O` — open the file referenced by the selected
  command in `$EDITOR`.
- `Ctrl+R` — open the reverse-history search (last 1000
  commands; the `precmd` hook in `init.zsh` keeps the
  ring buffer full).
- `Ctrl+G` — cycle the active scope (SESS / DIR / GLOBAL /
  STATS).
- `F3` — cycle the search mode (plain / regex / fuzzy).
- `F4` — cycle the sort order (age / frequency). The choice
  persists across TUI invocations.
- `Ctrl+J` — cycle the exit-code filter (all / success /
  failure).
- `Ctrl+X` — mark a todo row as done (only in `!...` mode).
- `T` — open the theme picker.
- `?` — open the help overlay (lists every action and its
  current binding).
- `:` — open the command palette (search by action name).
- `Esc` — close any open overlay (or quit the TUI when no
  overlay is open).

All keybindings are user-configurable. See the Configuration
section below and the full reference in [TECHNICAL.md](TECHNICAL.md).

# Search modes

The TUI's query bar accepts eight different *search modes*,
selected by the leading character of the query. Each mode
answers a different question about the user's history. The
table below summarises the modes; the paragraphs after it
describe each in detail.

| Mode          | Prefix | Question it answers                              | Filter target        |
|---------------|--------|--------------------------------------------------|----------------------|
| Plain         | (none) | "Which command contains these words?"            | `command` + comment  |
| Regex         | `/`    | "Which command matches this pattern?"            | `command` + comment  |
| Fuzzy         | `?`    | "Which command approximately contains these letters?" | `command` + comment  |
| Output        | `+`    | "Which command *produced* this output?"          | captured output      |
| LLM           | `=`    | "Translate this English description into a shell command." | (LLM call) |
| Question      | `%`    | "Ask the LLM a short factual question."          | (LLM call)           |
| Notes         | `@`    | "Find a note matching this query."               | `note_search` DB     |
| Todo          | `!`    | "Show me my open todos matching this."           | `note_search` DB     |
| Directories   | `#`    | "Jump to a directory I've worked in."            | directory column     |

The directory-jump mode (`#...`) and the four "find
something" modes (plain, regex, fuzzy, output) all search
the same SQLite history table — they just differ in *how*
they match. The two LLM modes (`=...` and `%...`) don't
search the history at all; they go straight to the
configured ollama instance. The two `note_search` modes
(`@...` and `!...`) query a separate database (see the
Configuration section).

The default prefix characters are configurable — see
[Prefix keys](#prefix-keys) below.

## Plain (default, no prefix)

Whitespace-separated, case-insensitive, AND-combined
substring match against the `command` and `comment` text.

```
> git commit
  # every row whose command or comment contains BOTH
  # `git` AND `commit` (case-insensitive)
```

The match is broad by design: the user types a couple of
words and gets a manageable candidate set, which they then
narrow with the list cursor.

## Regex (`/...`)

Compiled to a Rust `regex::Regex`; the match is applied to
the same `command` + `comment` columns as plain mode but
uses regex semantics. Implicit `.*` anchors are added at
both ends unless you provide your own, so `/git commit/`
matches *any* row containing `git commit`, while `/^git/`
only matches rows that *start* with `git`.

```
> /kubectl (apply|delete)/
  # rows whose command matches the alternation
> /^\s*git\s+commit\s+-m/
  # rows starting with `git commit -m` (and arbitrary
  # leading whitespace)
```

If the regex fails to compile (e.g. an unbalanced bracket
halfway through typing), the TUI keeps the previous valid
regex in place so the list doesn't flicker empty during a
transient typo.

## Fuzzy (`?...`)

fzf / `sk` / `peco` style: every character of the query
must appear in the target text, in order, case-insensitive.
Whitespace splits the query into AND-matched words, each of
which is itself a fuzzy subsequence.

```
> ?gsc
  # `git status --short && cargo build`
  # `git stash create`
  # NOT `git log` (no `s` then `c`)
> ?git st
  # `git status`, `git stash`, `git switch trunk`, ...
  # NOT `cargo test` (no `git` subsequence)
```

Plain, regex, and fuzzy are all post-filters: the SQL fetch
returns a broad candidate set and the TUI narrows the
display in Rust on every keystroke. With a few-thousand-row
history this feels instant.

## Output (`+...`)

Target the `history_output.output` column rather than
`command` or `comment`. The mode answers the inverse
question from the others: "which command *produced* this
output?" Rows with no captured output are excluded from
the result set, since the user is asking for a command
by what it generated, not a command with nothing to say.

```
> +segmentation fault
  # every command whose captured output contains BOTH
  # `segmentation` AND `fault`
> +
  # every command that has any captured output at all
  # (an "output inventory" view)
```

Output search is a SQL pre-filter (each term becomes a
`LIKE '%term%'` clause), so the round-trip is one query and
the result set matches the user's mental model exactly.

## LLM command generation (`=...`)

Translate a natural-language description into a runnable
command via a local ollama instance. While you compose the
description, the TUI auto-calls the model after one second
of inactivity and shows the suggestion as a `[LLM]` preview
row at the top of the list. The `Left` / `Right` arrow keys
move the input cursor within the description buffer in
this mode, so you can edit the prompt mid-string before
pressing `Enter`.

```
> =Find all files modified yesterday
  # debounce 1s → ollama returns:
  #   find . -type f -mtime -1
  # shown as a [LLM] preview row; Enter stages and exits
```

`Enter` reuses the live preview without a second round-trip
— the generated command is inserted into the history table
(with the original description as the comment), staged for
the parent shell, and the TUI exits. Requires `ollama.url`
and `ollama.model` in the config; otherwise the TUI
surfaces "not configured" on attempted use.

## General question (`%...`)

Ask a local ollama instance for a short answer (at most
four sentences, plain prose). The answer opens in a
full-screen overlay; `Esc` / `Enter` / `q` / the question
prefix closes it, `↑` / `↓` / `PageUp` / `PageDown` /
`Home` / `End` scroll.

The question is saved to history with the answer stored as
*output* (not as a comment), so typing `%` later shows all
previous questions and selecting one re-displays its
answer. Same `ollama.url` / `ollama.model` configuration
as the `=...` mode.

## Notes (`@...`)

Query a separate
[note_search](https://github.com/snharbec/note_search)
SQLite database for matching notes. The note's filename
is shown as the row label, the title as a secondary hint,
and the body in the details pane. Selecting a row opens the
file in `$EDITOR`. The match uses the `note_search` query
language: plain words AND-matched against filename / title /
body, `#tag` matched against both the note's frontmatter
tags and inline tags, `[[link]]` matched against outgoing
links, and `[attr:value]` matched against the note's
header fields.

The `@today` / `@week` / `@month` / `@year` date-filter
aliases apply: `@orchard @today` shows notes matching
"orchard" that were updated in the last 24 hours. Each
alias is a separate token — `@today` is recognised, but
`@orchard` is not treated as a date alias (it goes to the
`note_search` query language as a plain word).

Requires `notes.database` and `notes.dir` in the config
(or `NOTE_SEARCH_DATABASE` / `NOTE_SEARCH_DIR` env vars).

## Todo (`!...`)

The same `note_search` database, but listing every *open*
todo as its own row (one row per line, not one row per
file). Uses the same Obsidian-style query language as
`@...` and the same date-filter aliases. The list comes
straight from the indexer's `todo_entries` table, so the
TUI and `note_search list` always agree on what's open.

```
> !orchard
  # every open todo whose text or note header contains
  # `orchard`
> !#urgent
  # open todos in notes tagged `urgent` AND open todos
  # with an inline `#urgent` on the same line
> !older @week
  # open todos mentioning `older` AND updated this week
```

Press `Ctrl+X` on a todo row to mark it done: the TUI
rewrites `[ ]` to `[x]` in the source file, re-indexes the
`note_search` database so the row disappears from the list,
and refreshes the view. Pressing `Enter` on a todo row
opens `$EDITOR <file> <line_option>` with the line number
substituted in (default template `+$LINE`, configurable
via `todo.line_option=...`).

Requires the same `notes.database` and `notes.dir` config
keys as the `=...` mode.

## Directories (`#...`)

List every unique directory in the history (one row per
directory, sorted by most-recently-used first). The visible
layout is the inverse of normal history rows: the
**directory** is the primary text (in shell-shortened
`~/x` form) and the **last command run there** is a
secondary italic hint — so the user sees the path
prominently and a peek of *what they were doing there*
without leaving the row.

```
> #
  # every directory in the history
> #home
  # directories whose path contains `home`
```

A bright `T` marker in the capture column means there's at
least one active tmux pane whose cwd matches this
directory. Pressing `Enter` on a `T`-marked row stages
`tmux select-pane -t <pane_id> && tmux switch-client -t
<pane_id>` to jump to that pane; on an unmarked row it
stages `tmux new-session -d -s <basename> -c <dir>; tmux
switch-client -t <basename>` to create a new detached
session rooted in the directory. Both target paths use
the `~/x` form (tmux doesn't do `~` expansion itself, so
the TUI does it before staging).

The matching is homemap-aware: if the user's home is on
an external volume (e.g. `/Volumes/HUGE/har` on macOS),
both the DB row and the tmux pane path are normalised
through `expand_home_with_config` before comparison, so a
DB row stored as `~/x` and a tmux pane reported at
`/Volumes/HUGE/har/x` produce the same string and the
`T` marker appears. The snapshot is fetched once per TUI
session and cached; silent failure if `tmux` is not on
PATH or not running.

#### Pinned directories (`sessiondirs=...`)

Add one or more `sessiondirs=<path>` lines to the
config to pin a directory whose sub-tree is *always*
shown in the `#` list, even if no command has ever
been run there. Each entry is recursively walked at
TUI-startup time and every subdirectory becomes a
row. This is the "show me my projects even when I
haven't touched them yet" hook.

```
# ~/.config/smarthistory/config
sessiondirs=~/work
sessiondirs=~/Sources/playground
```

After restart, `#` lists every subdirectory under
`~/work` and `~/Sources/playground`, in addition to
the directories the user has actually run commands
in. Pinned rows get `timestamp = 0` and so sort to
the bottom of the list (the user's recent history
surfaces first); the user types a pattern to filter
to one. Rows that have a `.command` file (see
below) show `(has .command)` in the secondary slot
so the user knows the row will run a setup script
on select.

#### Per-directory setup scripts (`.command`)

If the user places a file named `.command` in a
directory (or in any ancestor), the directory becomes
a "session" with a setup script. Selecting such a
directory in the TUI runs

```
sh <path-to-.command> <selected-directory>
```

The first argument is always the selected directory;
the script can read it as `$1` (or as the full arg
list with `$@`). This is the "every project gets
its own setup" hook — for example, a
`project/.command` script that exports project-
specific environment variables, activates a virtual
environment, etc.

The lookup walks the **ancestor chain** of the
selected directory: the closest `.command` wins.
This is the same convention as `.envrc` /
`.env.local` / similar tools. So a single
`project/.command` fires for every selection
under `project/`, and a more specific
`project/special/.command` overrides it for
selections under `project/special/`.

The form on Enter:

- **Unmarked row** (no active tmux pane): the TUI
  stages
  `tmux new-session -d -s <basename> -c <dir>; \
   sh <.command> <dir>; \
   tmux switch-client -t <basename>`.
  The `.command` runs *inside* the new session
  before the user lands there, so the project is
  already set up when the switch-client fires.
- **T-marked row** (existing tmux pane matches):
  the TUI stages the existing
  `tmux select-pane && tmux switch-client` chain
  plus
  `; tmux send-keys -t <pane> "sh <.command> <dir>" Enter`
  so the setup script runs in the existing pane.

The `.command` is invoked via `sh` so the file
doesn't need to be executable. A non-zero exit
from the script surfaces via the parent shell's
standard error path.

# Prefix keys

The first character of a TUI query selects the search
mode. The mapping is configurable through the
`prefix.<mode>=<char>` config keys (see the
[Configuration](#configuration) section). The defaults:

| Mode        | Config key              | Default | Prompt color |
|-------------|-------------------------|---------|--------------|
| Regex       | `prefix.regex`          | `/`     | yellow       |
| Fuzzy       | `prefix.fuzzy`          | `?`     | green        |
| Output      | `prefix.output`         | `+`     | blue         |
| LLM         | `prefix.llm`            | `=`     | magenta      |
| Question    | `prefix.question`       | `%`     | blue         |
| Notes       | `prefix.notes`          | `@`     | cyan         |
| Todo        | `prefix.todo`           | `!`     | yellow       |
| Directories | `prefix.directories`    | `#`     | green        |

(Prompt color comes from the active theme's named slots —
`tuicolor.regex`, `tuicolor.fuzzy`, `tuicolor.output`,
etc. Override any of them in the config.)

## How prefix detection works

The TUI looks at the *first* character of the query bar
on every refresh. If it matches a configured prefix, the
TUI switches to that mode. The body of the query is the
text after the prefix, and the prefix itself is *not*
part of the search. So `/git status` searches for the
regex `git status`, not for the literal string
`/git status`.

Stripping the prefix is invisible to the user in most
modes: the input border is tinted in the mode's color
and the prompt character at the left of the input box
*is* the prefix, so the user always sees what's
"consumed" by the mode.

A bare prefix (just `/`, `?`, `+`, etc., with no body
yet) is treated as "show me everything in this mode" —
plain `/` is a no-op (no body, no filter), `+` shows
every row that has captured output, `#` shows every
directory.

## Conflicts with command characters

The defaults are chosen to avoid characters that are
common in shell commands. If one of them *does* conflict
with your workflow (for example, you often run commands
that start with `!` for history expansion, and you don't
want that to look like a todo query), override it in the
config:

```
prefix.todo=°
prefix.question=¿
prefix.regex=§
```

The override takes effect on the next TUI launch. Each
prefix must be a single character; the parser rejects
multi-character prefixes with a config-load error.

The modes themselves are not coupled to their prefix
characters: the *behavior* of the LLM command-generation
mode is the same whether the prefix is `=`, `→`, or
anything else you configure. The only thing the prefix
controls is how the user invokes the mode from the
query bar.

## How `F3` interacts with the prefix

Pressing `F3` (default; rebindable via
`key.toggle-search-mode=...`) cycles the search mode
through plain → regex → fuzzy → output → plain. The
cycle preserves the body of the query and only changes
the leading prefix character, so `git status` →
`/git status` → `?git status` → `+git status` → `git
status` keeps the same text the whole time.

The `=` / `%` / `@` / `!` / `#` modes are *not* part of
the `F3` cycle (they're LLM and cross-database modes,
not "different ways of searching the history table").
To reach them, type the prefix character at the start of
the query, or open the command palette (`:`) and search
for the mode's name (e.g. "Directories" for the `#`
mode).

# Configuration

User-specific settings live in
`~/.config/smarthistory/config`. The file is plain
INI-style `key=value` lines; lines starting with `#` are
comments and blank lines are ignored. When the file is
absent, built-in defaults are used. When present, the keys
it defines override the defaults and any other keys keep
their default values.

## Themes

Pick a built-in theme or override individual colors. The
available themes are listed in `TECHNICAL.md`; switch with
the in-app `T` key (theme picker) or by setting
`theme=<name>` in the config.

## TUI overrides

Override the accent color, background, foreground, or any
named style slot (`accent`, `success`, `error`, `warning`,
`dim`, `highlight`, `bg`, `fg`, etc.) directly in the
config.

## Keybindings

Remap any action via `key.<action>=<spec>` lines. The action
names are documented in the table inside [TECHNICAL.md](TECHNICAL.md)
and in the in-app help overlay (`?`). Multi-key bindings are
supported (`C-h,F1` fires on either). Set a binding to
`none` / `off` / `disable` / `-` to disable a default.

## Query prefix characters

Each query mode has a configurable prefix character. The
defaults are:

- `prefix.regex = /`
- `prefix.fuzzy = ?`
- `prefix.output = +`
- `prefix.llm = =`
- `prefix.question = %`
- `prefix.notes = @`
- `prefix.todo = !`
- `prefix.directories = #`

Set any of them to a different character if the default
conflicts with your workflow.

## Home mapping (macOS external-volume users)

The DB stores directory paths. On macOS, the kernel
canonicalises `/Users/har/...` to `/Volumes/HUGE/har/...`
when the user's home lives on an external volume. To keep
the visible form short and consistent, set:

```
homemap=/Volumes/HUGE/har
```

The TUI then treats this prefix as a "second home" alongside
`$HOME` (longest-prefix-wins), and `smarthistory update`
rewrites stored absolute paths to `~/x` form. The
subcommand is idempotent — running it twice on the same
database is a no-op.

## Pinned directories (`sessiondirs`)

Add one or more `sessiondirs=<path>` lines to the
config to pin a directory whose sub-tree is *always*
shown in the `#`-mode list, even if no command has
ever been run there. Each entry is recursively
walked at TUI startup; every subdirectory becomes a
row in the directories list. This is the
"show me my projects even when I haven't touched
them yet" hook.

```
# ~/.config/smarthistory/config
sessiondirs=~/work
sessiondirs=~/Sources/playground
```

Each `sessiondirs` entry is independent: two
different roots can both contribute rows, and a
subdirectory that lives under two roots appears
once (dedup is on canonical paths, so a symlink
and the real path it points to also collapse to
one entry). A non-existent path is silently
skipped (the walker returns an empty list for a
missing root, so the TUI never errors on
startup). Combine with `.command` files (see the
Search modes / Directories section) to set up
project-specific environment on session start.

## LLM configuration

LLM features (`=`, `%`, `Ctrl+K`, `Ctrl+T`) require a local
ollama instance. Both keys are mandatory; missing either
disables the feature (the TUI surfaces "not configured" on
attempted use):

```
ollama.url=http://localhost:11434
ollama.model=llama3.2
```

## Note / todo database

The `@...` and `!...` modes need a `note_search` SQLite
database. Configure the path and the directory it indexes:

```
notes.database=/path/to/notes.sqlite
notes.dir=/path/to/notes
```

(Or set `NOTE_SEARCH_DATABASE` and `NOTE_SEARCH_DIR` in the
environment.)

For the full configuration reference, see
[TECHNICAL.md](TECHNICAL.md).
