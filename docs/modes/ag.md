# ag mode (`,`)

| Default prefix | `,` |
| --- | --- |
| Configurable | `prefix.ag=<char>` |

ag mode searches file *contents* with [`ag`](https://github.com/ggreer/the_silver_searcher) (The Silver Searcher). Every line containing the typed pattern is listed as a row; selecting a row stages `$EDITOR +LINE file` and exits.

## What it does

- `,` (empty) — no results (the ag search needs at least one search term). Use this to confirm the mode is active.
- `,TODO` — every line containing `TODO` in every text file under the cwd.
- `,TODO *.rs` — every `TODO` line in every `.rs` file. The `*` in `*.rs` is a shell-style glob.
- `,@rust TODO` — every `TODO` line in every Rust file. The `@rust` token restricts to `.rs` files via `ag --rust`.
- The first text column is the matched line (trimmed); the second is the file basename; the third is the file path / line number (the path component in `ag`'s `file:line:content` output).

## Selecting a row

- `Enter` stages `$EDITOR +LINE file` and exits.
- `Ctrl-O` (Show output) opens the **5-line source context** (2 before, the line itself, 2 after) with `bat --color=always` syntax highlighting. The match line is prefixed with `>>` so you can spot it at a glance. The full context is scrollable in the overlay.

## Special tokens

| Token | Meaning |
| --- | --- |
| `@<lang>` | Restrict the search to files of a given language. The token is converted to `ag --<lang>` (e.g. `@rust` → `ag --rust`, `@java` → `ag --java`). Unknown languages are silently ignored (ag prints a usage warning to stderr, which we ignore). |
| `*<glob>` (anywhere in the path component) | Restrict the search to files matching the glob. Implemented as `ag -G <regex>` where the shell-style glob is converted to a regex. Examples: `*.rs` (any `.rs` file), `work/**/*.toml` (any `.toml` under `work/...`), `Cargo.toml` (just that file). |
| `<text>` (any other whitespace token) | The first such token is the primary `ag` pattern. Subsequent tokens are post-filters: every additional token must appear (case-insensitive) in the matched line. |

The token language is the same as `tags` and `codegraph` mode for the `@<lang>` part, but the *first* text token is special: it's the ag pattern. A query like `,TODO fix` matches lines containing `TODO` (primary) that ALSO contain `fix` (post-filter). The post-filter is applied client-side after `ag` returns its results.

## Debounce

The ag search is debounced: 300ms after the last keystroke. The search runs in a background thread; the result populates the list when it lands. `ag` itself is fast (typically <100ms on medium repos), so the debounce is mostly to avoid firing on every keystroke.

## Required tooling

`ag` (The Silver Searcher) must be on `PATH`. If not, `,` mode is a no-op and the status bar shows a "command not found" error. Install via:

```sh
# macOS
brew install the_silver_searcher

# Debian / Ubuntu
sudo apt-get install silversearcher-ag

# Arch
sudo pacman -S the_silver_searcher
```

## Per-mode input history

`Ctrl-P` / `Ctrl-N` cycle through the **ag mode's** past queries. Other modes have their own per-mode history (scoped by prefix), so `Ctrl-P` in `&` mode only recalls past `&` queries. See [`README.md`](README.md#common-actions-that-work-in-every-mode) for the full set of common actions.

## Cross-references

- [Tags mode — `,` searches file *contents*; `$` searches the *symbols*; `~` searches the file *names*](tags.md)
- [Files mode — `~` walks the file system; `,` then searches each file's contents](files.md)
- [CodeGraph mode — for symbol-and-relationship navigation, prefer `&`](codegraph.md)
- [TECHNICAL — ag-mode implementation](../../TECHNICAL.md#ag-mode)
