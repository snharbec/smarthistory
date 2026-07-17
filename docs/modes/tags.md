# Tags mode (`$`)

| Default prefix | `$` |
| --- | --- |
| Configurable | `prefix.tags=<char>` |

Tags mode lists every symbol defined in a universal-ctags `tags` file, walked from the cwd upward (just like vim). Selecting a row stages `$EDITOR +LINE file` and exits — your editor opens at the symbol's definition.

## What it does

- `$` (empty) — every symbol in the nearest `tags` / `TAGS` file (newest first by file path).
- `$setUp` — every symbol whose name contains `setUp` (case-insensitive).
- `$@rust setUp` — every Rust symbol matching `setUp`. The `@rust` token filters by file extension (`.rs`).
- The first text column is the symbol's display name (e.g. `setUp`); the second is the file basename; the third is the kind (`method`, `class`, `field`, …) and the line number.

## Selecting a row

- `Enter` stages `$EDITOR +LINE file` and exits.
- `Ctrl-O` (Show output) opens the **5-line source context** (2 before, the line itself, 2 after) with `bat --color=always` syntax highlighting. The match line is prefixed with `>>` so you can spot it at a glance. The full context is scrollable in the overlay.
- `Ctrl-R` (CodegraphRelations) opens the **callers / callees picker** for the selected symbol. The picker has two sections (callers / callees); `Up` / `Down` navigate, `Enter` opens the highlighted relation's source file, `Esc` closes. The picker is populated from the CodeGraph index (`.codegraph/codegraph.db`) if available; otherwise no relations are shown.
- `Ctrl-]` (Smart open) also opens the callers / callees picker in tags mode (it does the same thing as `Ctrl-R` here, since `$` is a codegraph-backed mode).

## Special tokens

| Token | Meaning |
| --- | --- |
| `@<lang>` | Filter by file extension. The value is matched against a small map in [`src/highlight.rs`](../src/highlight.rs) (e.g. `@rust` → `.rs`, `@java` → `.java`, `@python` → `.py`). Unknown languages fall back to bat's extension detection (no filter applied). |
| `<text>` (any other whitespace token) | Substring AND match on the symbol's display name OR the file basename. |

Multiple `@<lang>` tokens are accepted; only the first is used (consistent with ag mode).

## Output preview rendering

The source context (loaded by `read_source_context_with_cache` in [`src/tui.rs`](../src/tui.rs)) is 50 lines (25 before, the line itself, 24 after) — generous enough to fit a function body or a class body. The first `@<lang>` token is forwarded to `bat` for syntax highlighting; if no `@<lang>` was given, `bat` is invoked with `--file-name=<path>` so it auto-detects the language from the source file's extension.

The first 50 lines of the source context are also appended with a `── callers ──` and `── callees ──` section when a CodeGraph node id is associated with the row (which is the case for the CodeGraph-backed `$` fallback when no `tags` file exists in the repo).

## TAGS file discovery

`find_tags_file` walks up from the cwd looking for either `tags` (lowercase, the ctags default) or `TAGS` (uppercase, the etags / ctags -e default). The first match wins. File paths inside the tag file are resolved relative to the directory containing the `tags` file (not the cwd), so symlink-and-stale-tag scenarios work.

When no `tags` file is found anywhere in the tree, `$` mode falls back to the [CodeGraph mode](codegraph.md): it queries `.codegraph/codegraph.db` and returns the same shape of rows (with `codegraph_node_id` set on each row so the callers/callees overlay still works). The selected row's mode is still `tags` so the existing tags dispatch (`$EDITOR +LINE file`) and the `read_source_context` lazy-load continue to work unchanged. This is the "tags without a TAGS file" experience: any repo with a CodeGraph index gets symbol navigation for free.

## Related actions

- `Ctrl-R` / `Ctrl-]` (CodegraphRelations / Smart open) — open the callers / callees picker. Requires a row carrying a `codegraph_node_id` (set by the CodeGraph fallback; rows from a real `tags` file have an empty id and the picker surfaces a "No CodeGraph node for this row" status message).
- `Ctrl-H` (Toggle search mode) — cycles the match algorithm (substring / fuzzy / regex) for the symbol-name filter.

## Cross-references

- [CodeGraph mode — the same symbol lookup backed by `.codegraph/codegraph.db`, the `$`-mode fallback when no `tags` file exists](codegraph.md)
- [Files mode — `~` searches the file system; `$` searches the symbols *in* those files](files.md)
- [ag mode — searches the file *contents*; `$` searches the symbols; `~` searches the file *names*](ag.md)
