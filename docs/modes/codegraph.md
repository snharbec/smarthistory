# CodeGraph mode (`&`)

| Default prefix | `&` |
| --- | --- |
| Configurable | `prefix.codegraph=<char>` |

CodeGraph mode searches symbols in the local `.codegraph/codegraph.db` SQLite index (a separate project: <https://github.com/snharbec/codegraph>), backed by an FTS5 virtual table over symbol names, qualified names, docstrings, and signatures. The selected row's preview shows source context plus a `── callers ──` / `── callees ──` overlay, populated from the `edges` table (`kind='calls'`).

This is the primary "navigate my codebase" mode: it combines symbol search (like `$`), relationship visualisation (like an IDE's "find usages" / "go to implementation"), and source preview (like an IDE's hover) in one overlay.

## What it does

- `&` (empty) — no results (the FTS5 search needs at least one identifier token). Use this to confirm the mode is active.
- `&getSymbol` — every symbol whose name starts with `getSymbol` (FTS5 prefix match, tokenised by `&get*`).
- `&@java getSymbol` — every Java symbol matching `getSymbol`. The `@java` token restricts to `language='java'` (case-insensitive).
- The first text column is the symbol's qualified name (e.g. `com.package.ons.client::Client::getName`); the second is the kind and file path / line.

## Selecting a row

- `Enter` stages `$EDITOR +LINE abs-path` and exits.
- `Ctrl-O` (Show output) opens the **50-line source context** (25 before, the line itself, 24 after) with `bat --color=always` syntax highlighting. The match line is prefixed with `>>` so you can spot it at a glance. The full context is scrollable in the overlay.
- `Ctrl-R` (CodegraphRelations) opens the **callers / callees picker** for the selected symbol — see below.
- `Ctrl-]` (Smart open) does the same thing as `Ctrl-R` in `&` mode (it dispatches to the callers / callees picker when the active mode is `&` or `$`).

## The callers / callees picker

`Ctrl-R` (or `Ctrl-]`) opens a centred overlay listing two sections:

1. `── callers ──` — every function / method that *calls* the selected symbol. Populated from `edges` with `kind='calls'` and `target=<node-id>`.
2. `── callees ──` — every function / method that the selected symbol *calls*. Populated from `edges` with `kind='calls'` and `source=<node-id>`.

Each row shows `qualified_name @file_path:start_line`. The first 15 of each are shown (so hub symbols with thousands of callers don't blow up the overlay).

`Up` / `Down` / `PageUp` / `PageDown` / `Home` / `End` / `Ctrl-N` / `Ctrl-P` navigate. `Enter` opens the highlighted relation's source file in `$EDITOR +LINE` and exits the TUI. `Esc` / `Ctrl-C` closes the overlay without opening anything.

The picker is opened from a single keypress with no extra confirmation — the user can browse relationships without leaving the TUI.

## Special tokens

| Token | Meaning |
| --- | --- |
| `@<lang>` | Filter by language. The value is matched verbatim (case-insensitive) against the `nodes.language` column of the CodeGraph index (e.g. `@java`, `@kotlin`, `@python`, `@typescript`). Unknown values match nothing — same graceful degradation as `ag` and `tags` mode. |
| `<text>` (any other whitespace token) | FTS5 prefix-AND search across the columns `name`, `qualified_name`, `docstring`, `signature` in the `nodes_fts` table. Tokens are sanitised to identifier chars (`[A-Za-z0-9_]`) and quoted before being suffixed with `*` for the prefix match. |

Multiple `@<lang>` tokens are accepted; only the first is used (consistent with `ag` and `tags` modes).

## Output preview rendering

The source context (loaded by `read_source_context_with_cache` in [`src/tui.rs`](../src/tui.rs)) is 50 lines (25 before, the line itself, 24 after). The first `@<lang>` token is forwarded to `bat` for syntax highlighting; if no `@<lang>` was given, `bat` is invoked with `--file-name=<path>` so it auto-detects the language from the source file's extension.

The 50 lines of source context are also appended with a `── callers ──` and `── callees ──` section (up to 15 of each), so even without opening the picker you can see the immediate call graph of the selected symbol.

## `.codegraph/codegraph.db` discovery

`find_codegraph_db` walks up from the cwd looking for `.codegraph/codegraph.db` (exactly the same pattern as `find_tags_file`). The first match wins. When no index is found anywhere in the tree, `&` mode is a no-op and the status bar shows "No .codegraph/index found".

The connection is opened `SQLITE_OPEN_READ_ONLY` + `query_only` and cached on the `App` for the rest of the session. A background indexer that updates the index while the TUI is running is fine — we never write to the database, and the read-only pragma is set defensively.

## Smart open (Ctrl-]` default)

`Ctrl-]` (the [`SmartOpen`](README.md#common-actions-that-work-in-every-mode) action) is bound by default to `C-]` (a single-byte ASCII control char that every terminal emits reliably; chosen over `S-Return` because many terminals emit Shift-Return as a non-standard sequence crossterm 0.29 can't decode). In `&` mode, `Ctrl-]` opens the callers / callees picker directly — no need to first select a row.

## Cross-references

- [Tags mode — the ctags-backed sibling, both fall back to CodeGraph when no `tags` file exists](tags.md)
- [Files mode — `~` searches the file *names*; `&` searches the *symbols inside* the files](files.md)
- [README — how to install the `.codegraph` index in your repo](../../README.md#installation)
- [TECHNICAL — CodeGraph module implementation](../../TECHNICAL.md#codegraph-mode)
