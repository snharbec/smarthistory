# Zoxide mode (`~`)

| Default prefix | `~` |
| --- | --- |
| Configurable | `prefix.zoxide=<char>` |

Zoxide mode lists every directory in the local [`zoxide`](https://github.com/ajeetdsouza/zoxide) database, ordered by zoxide's own frecency score (highest first), filtered by the typed query. Selecting a row creates a new tmux session / herdr workspace rooted there — the exact same staging `#` (Directories) mode uses for an unmarked row, including the `T`-marked "jump to an already-active pane there instead" behavior.

Requires the `zoxide` binary on `$PATH`. Nothing else to configure — zoxide mode reads whatever directories `zoxide` has already learned from your normal shell usage (`z <dir>` / `zoxide add`), it doesn't maintain its own separate list.

## What it does

- `~` (empty) — every directory zoxide knows about, highest frecency score first.
- `~proj` — every zoxide directory whose path contains `proj` (substring AND across whitespace-separated tokens, same contract as every other mode).
- The first text column is the shortened path (`~/work/foo`); the row order itself reflects zoxide's ranking (a synthetic descending timestamp preserves it under the list's default sort).
- A directory whose entry is still in zoxide's database but no longer exists on disk (deleted since zoxide last saw it) is silently skipped — there'd be nothing to jump to.

## Selecting a row

- `Enter` on an unmarked row creates a new tmux session / herdr workspace rooted at the directory and switches to it.
- `Enter` on a `T`-marked row (a tmux / herdr pane is already active there) focuses that existing pane instead of creating a duplicate session.
- Both paths are handled by the same staging function `#` Directories mode uses (`App::stage_directory_selection`) — a zoxide row is tagged `mode == "directory"` internally, so the rest of the TUI treats it identically to a Directories-mode row. A `.command` file in the directory (or an ancestor) is bootstrapped the same way too — see [`directories.md`](directories.md#selecting-a-row) for the full `.command`-chaining behavior.

## Relationship to Directories mode

`#` (Directories) and `~` (Zoxide) both list directories and both create/focus a session the same way on selection — the difference is entirely in the **data source**:

| | Directories (`#`) | Zoxide (`~`) |
| --- | --- | --- |
| Source | smarthistory's own `history` table, `sessiondirs=` config, and live multiplexer panes | zoxide's own database (`zoxide query -l`) |
| Ranking | most-recent history activity | zoxide's frecency score (frequency + recency) |
| Requires | nothing extra (uses the existing history DB) | the `zoxide` binary on `$PATH` |
| Pinned entries | `sessiondirs=...` in the config | whatever `zoxide add` / normal `z`-driven `cd` usage has learned |

Use whichever list already matches how you navigate day to day: `#` if you rely on smarthistory's own history and `sessiondirs=` pins, `~` if `zoxide`/`z` is already your primary way of jumping between projects outside the TUI.

## Health check

`smarthistory check --prefix '~'` (or `smarthistory check` with no filter, which checks every mode) verifies the `zoxide` binary is reachable and reports how many directories are in its database.

## Cross-references

- [Directories mode — the history-derived directory view; shares the exact same row-selection staging](directories.md)
- [Panes mode — the multiplexer pane view, listed per-pane rather than per-directory](panes.md)
- [README — multiplexer integration](../../README.md#multiplexer-integration-tmux--herdr)
- **[Multiplexer backend reference](../../docs/multiplexer.md)** — backend selection, building with the `herdr` feature, setup guides for both backends, troubleshooting.
