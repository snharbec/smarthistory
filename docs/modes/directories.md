# Directories mode (`#`)

| Default prefix | `#` |
| --- | --- |
| Configurable | `prefix.directories=<char>` |

Directories mode lists every directory the shell has ever been in, sorted by the most-recent history row's timestamp. Each row carries a `T` marker when at least one tmux / herdr pane is currently rooted there. Selecting a row stages `cd <abs-path>` and exits.

## What it does

- `#` (empty) — every directory in the global history, newest first.
- `#src` — every directory whose path contains `src` (substring AND across tokens).
- The first text column is the absolute path; the second is the leaf directory basename; the third is the most-recent command run there (the "what was I doing in there" hint).
- A `T` chip in the marker column means at least one multiplexer pane is currently in that directory (the snapshot is fetched once at TUI start, see `tmux list-windows -a` below).

## Directory sources

`Ctrl-S` cycles the `directory_source` filter: `all` (default — show rows from all three sources), `tmux` (only rows that have a current multiplexer pane), `cfg` (only rows that come from `sessiondirs=...` in the config — directories the user has pinned even when they've never run a command there).

## Selecting a row

- `Enter` stages `cd <abs-path>` and exits. The parent shell changes cwd. Paths with spaces are quoted by the parent shell so the path is a single argument.
- `Enter` on a `T`-marked row stages `cd <abs-path>` *and* focuses the existing tmux session / herdr workspace whose pane is rooted there. Pressing `Enter` on an unmarked row creates a new tmux session / herdr workspace rooted there.
- `Ctrl-E` opens the comment editor for the directory (a smarthistory-side annotation).

## Source breakdown

The `#` view merges rows from three sources, deduplicated by `directory` (with the duplicate filter on, which is the default):

1. **History** — every distinct `cwd` in the `history` table. Sorted by the most-recent history row's timestamp DESC.
2. **Session directories** — the `sessiondirs=...` config keys. Each entry is recursively walked at TUI startup; every subdirectory is added. These show up even when the user has never run a command there (the rationale: the user has *pinned* them as "places I work", so they belong in the view).
3. **Active multiplexer panes** — every pane's current `cwd` reported by `tmux list-windows -a` (or `herdr workspace list` + `herdr pane list` on herdr). The snapshot is fetched once at TUI start and cached for the session; it doesn't update while you're navigating.

`~`-expansion: rows whose path starts with the user's `$HOME` (or any of the `homemap=...` config keys) are displayed in shortened form (e.g. `~/work/foo`). The actual staging uses the absolute path.

## Cross-references

- [Panes mode — the multiplexer pane view, listed per-pane rather than per-directory](panes.md)
- [Configuration — `sessiondirs`, `homemap`, `multiplexer`](../../README.md#configuration)
- [README — multiplexer integration](../../README.md#multiplexer-integration-tmux--herdr)
- **[Multiplexer backend reference](../../docs/multiplexer.md)** — backend selection, building with the `herdr` feature, setup guides for both backends, troubleshooting.
