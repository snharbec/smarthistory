# Search modes

The TUI is a multi-mode launcher. The first character of the query selects a *mode*; each mode answers a different question about your data. The default prefix characters are configurable via `prefix.<mode>=<char>` in `~/.config/smarthistory/config`.

## Index

| Mode | Prefix | Document | One-liner |
| --- | --- | --- | --- |
| History | *(none)* | [`history.md`](history.md) | Plain-text search over the shell history. |
| Output | `+` | [`output.md`](output.md) | Search the captured stdout / stderr of each command. |
| LLM command | `=` | [`llm.md`](llm.md) | Ask the LLM to generate a Bash command from a natural-language description. |
| Question | `%` | [`question.md`](question.md) | Ask the LLM a short factual question; answer is shown in an overlay. |
| Notes | `@` | [`notes.md`](notes.md) | Search the `note_search` SQLite database. |
| Todo | `!` | [`todo.md`](todo.md) | List open todo entries from the `note_search` database. |
| Directories | `#` | [`directories.md`](directories.md) | List every directory the shell has ever been in. |
| Panes | `*` | [`panes.md`](panes.md) | List every pane across every tmux / herdr session. |
| JIRA | `-` | [`jira.md`](jira.md) | Search JIRA issues via the REST API. |
| Files | `~` | [`files.md`](files.md) | List every file under the current directory. |
| Tags | `$` | [`tags.md`](tags.md) | List every symbol from the local ctags `tags` file. |
| CodeGraph | `&` | [`codegraph.md`](codegraph.md) | Search symbols in the local `.codegraph/codegraph.db` index; the selected row's preview shows source context plus callers / callees. |
| ag | `,` | [`ag.md`](ag.md) | Search file contents with [`ag`](https://github.com/ggreer/the_silver_searcher) (The Silver Searcher). |

## Cross-cutting topics

Some flows span multiple modes. These are documented as standalone pages:

- **[`actions.md`](actions.md)** — every TUI action (all 48 in `ALL_ACTIONS`), grouped by category, with config keys, default keys, and mode-specific behavior. The canonical reference for `key.<action>=<spec>` config-file bindings. Read this when you want to rebind a key, find a key you've forgotten, or understand which actions are mode-specific (e.g. `MarkTodoDone` is a no-op outside `!` mode).
- **[`multiplexer.md`](multiplexer.md)** — tmux + herdr support: backend selection, building with the herdr feature, setup guides, troubleshooting. Required reading for anyone who uses `#` or `*` mode (the backend is what produces the `T` marker and what handles the focus / create staging).

## How the prefix is selected

The first character of the query decides the mode. Examples:

| Query | Mode |
| --- | --- |
| `git status` | History (no prefix). |
| `+OutOfMemory` | Output. |
| `=find duplicates in csv` | LLM command. |
| `%when was TCP invented` | Question. |
| `@docker compose` | Notes. |
| `!@new remember to buy milk` | Todo (quick-create). |
| `#src` | Directories. |
| `*nvim` | Panes. |
| `-@me @kramfors status=Open` | JIRA. |
| `~/work/**/*.toml` | Files. |
| `$@rust setUp` | Tags (rust symbols matching `setUp`). |
| `&@java getSymbol` | CodeGraph. |
| `,TODO *.rs` | ag. |

An empty query (just the prefix) is accepted everywhere; it means "show me everything in this view".

## Tokens shared across modes

Some modes share a token language for narrowing the search. The implementations live in [`src/highlight.rs`](../src/highlight.rs) and [`src/jira.rs`](../src/jira.rs).

| Token | Used by | Meaning |
| --- | --- | --- |
| `@<lang>` | tags, codegraph, ag | Filter by language. The value is matched against the `nodes.language` column (codegraph) or the file extension (tags / ag). Examples: `@rust`, `@java`, `@python`. |
| `*<glob>` | ag, files (via `~`) | Restrict to files whose name matches `<glob>`. The glob is a shell-style pattern (`*` = any chars). |
| `<field>=<value>` | jira | JQL-style `key=value` constraint, e.g. `status=Open`, `priority=Blocker`. |
| `@me` / `@today` / `@week` / `@month` | jira | Built-in JIRA aliases. See [`jira.md`](jira.md). |
| `@<name>` | jira | A user-defined JQL fragment from `jira.search.<name>=<jql>` in the config. Requires the leading `@`. |
| `=desc` | llm (no `@` prefix in this case) | The `=` is the LLM-mode prefix; the rest of the body is the natural-language description. |

## Match algorithm (default: substring)

The substring / fuzzy / regex algorithms apply to most modes. Toggle with `Ctrl-F` and see the help overlay (`Ctrl-A` by default) for the live key binding. The fuzzy and regex algorithms are *post-filters* on top of the SQL / SQLite / ag / CodeGraph results — they only ever *narrow* the result set, never *broaden* it.

- `sub` — case-insensitive substring across the relevant text fields (the SQL `LIKE` for modes backed by SQLite, the ag match for `,` mode, the FTS5 match for `&` mode).
- `fuz` — every whitespace-separated term must fuzzy-match (subsequence) some field.
- `reg` — the body is treated as a regex and matched against the relevant text fields.

## Common actions that work in every mode

| Key | Action | What it does |
| --- | --- | --- |
| `↑` / `↓` | Move selection | Navigate the result list. |
| `Enter` | Run | Selects the highlighted row (open the file, stage the command, fire the LLM, etc.). |
| `Ctrl-O` | Show output | Opens the full captured-output / relations overlay for the selected row. |
| `Ctrl-R` | Refresh | Re-runs the search / walk / fetch. |
| `Ctrl-P` / `Ctrl-N` | Previous / next history | Per-mode input history recall (readline `previous-history` / `next-history`). |
| `Ctrl-]` | Smart open | Context-aware "dive" key: opens the callers / callees picker in `&` / `$`; opens the JIRA issue in the browser in `-`; falls through to `Enter` elsewhere. |
| `F1` | Pick prefix | Open the prefix-mode picker. |
| `Ctrl-A` | Help | Open this help overlay (see also the standalone docs in this directory). |
| `Ctrl-Q` | Command palette | Search every action by name. |

## Privacy convention (space prefix)

The TUI honors zsh's `HIST_NO_STORE` convention: **any command whose first character is whitespace is treated as "do not record".** This applies in two places:

1. **The TUI prepends a single space to staged selections in every mode *except* history mode.** `Enter` in `&` / `$` / `~` / `@` / `!` / `-` / `,` / `=` / `%` / `#` / `*`, `Ctrl-]` SmartOpen, `Ctrl-V` EditFileReference, `Ctrl-M-s` DownloadJiraIssue, etc. all stage a one-shot read (`bat README.md`, `note_search edit-note <id>`, `open <jira-url>`, etc.) that the user typically doesn't want cluttering the DB. The space prefix keeps both the shell history and the smarthistory DB focused on commands worth recalling. **History mode (no prefix) is the explicit exception**: picking a row from history is a command the user *wants* recorded — recording it keeps the frequency stats accurate (so `Ctrl-S` next-probable-command suggestions stay useful) and lets the same command surface in future searches.
2. **User-typed space-prefixed commands** get the same treatment. Type `git push` (with a leading space) and the precmd hook skips the DB write; `git push` (no space) is recorded normally.

The convention is also honored by the `Ctrl-S` (next-probable-command) widget: a space-prefixed command is deliberately NOT remembered as `_smarthistory_last_cmd`, so the widget will not suggest a sensitive command as the "next probable" one. The cycle index is still reset so the next `Ctrl-S` press starts with the most probable candidate from the (non-sensitive) recent past.

Implementation:

- TUI side: `maybe_prefix_selection_with_space` in [`src/tui.rs`](../../src/tui.rs) — a mode-aware helper called from `run_tui_to_stdout`'s exit path. The helper checks the active mode char: `MODE_NONE` (history, no prefix) → return the selection unchanged; any other prefix char → prepend a single space via [`prefix_selection_with_space`]. The mode char is computed from `app.query` *before* `app.selection.take()` so the borrow checker is happy.
- zsh side: `_smarthistory_precmd` in [`src/init.zsh`](../../src/init.zsh) — the `[[ "$_smarthistory_cmd" == [[:space:]]* ]]` guard early-returns before the `smarthistory add` / `capture-*` calls.

### When you want the TUI selection recorded

If you want a staged command (from a non-history mode) to land in the smarthistory DB (e.g. you're staging a command you want to recommend later via `Ctrl-S`), you can strip the leading space before pressing `Enter` — but the convention is intentional: the smarthistory DB accumulates commands you'd want to search for later, and TUI picks from prefix modes are usually "look at this once" reads (`bat README.md`, `xdg-open photo.png`) rather than commands worth recording. The space prefix keeps the DB focused. Replaying from history (no prefix) is the canonical "record this" path and is never space-prefixed.

The convention is symmetric: typing a leading space yourself produces the same result as picking a command from a (non-history) TUI mode.

## Where the help text comes from

The TUI's `Ctrl-A` help overlay renders a live summary of all the modes. That summary is generated by `build_help_lines` in [`src/tui/render.rs`](../../src/tui/render.rs) and is the canonical short reference.

The per-mode markdown files in this directory are the *long* reference: each one expands a single mode with example queries, special tokens, related actions, and cross-references to neighbouring modes. They live in version control so you can `git diff` the docs alongside the code, and they read well outside the TUI (e.g. on GitHub, in a pager, or rendered by a Markdown viewer).

## See also

- [`docs/configuration.md`](../configuration.md) — the full config-file reference (every per-mode config key, plus `tuicolor.*`, `key.<action>`, `prefix.<name>`, `multiplexer`, `notes.*`, `todo.line_option`, `files.ignore`, `smart-open.*`, `jira.search.*`, `ollama.*`, `session.*`, `host.*`, and the `SMARTHISTORY_*` / `JIRA_*` / `NOTE_SEARCH_*` / `TMUX*` env-var overrides).
