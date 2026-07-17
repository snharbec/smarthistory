# Files mode (`~`)

| Default prefix | `~` |
| --- | --- |
| Configurable | `prefix.files=<char>` |

Files mode lists every file under the current working directory (recursively walked), filtered by the typed pattern. Selecting a row stages `cd <dir> && $EDITOR <file>` (well, actually `$EDITOR <file>` — the directory is implied by the file path) and exits.

## What it does

- `~` (empty) — every file under the cwd, newest first.
- `~README` — every file whose name contains `README` (case-insensitive substring).
- `~*.toml` — every `.toml` file. The `*` is a shell-style glob (matches any chars).
- `~work/**/*.md` — every `.md` file under any `work/...` directory. Glob in a path segment restricts the walk.
- The first text column is the basename; the second is the directory (in `~/x` shortened form for paths under `$HOME`); the third is the timestamp / size.

## Selecting a row

- `Enter` stages `$EDITOR <abs-path>` and exits. The parent shell runs the command.
- `Ctrl-E` opens the comment editor for the file (a smarthistory-side annotation, separate from the file's own contents).

## Walk scope

The walk is rooted at the current working directory at TUI start. Subdirectories listed in `files.ignore` (whitespace-separated names) are skipped, in addition to the built-in `DEFAULT_IGNORES` set in [`src/files.rs`](../src/files.rs) (which includes `.codegraph`, `.git`, `node_modules`, etc.).

Large directories (> 100,000 entries) are skipped to keep the walk responsive. The status bar surfaces a "directory too large" message instead of hanging.

## Debounce

The walk is debounced: 300ms after the last keystroke. The walk runs in a background thread; the result populates the list when it lands.

## Special tokens

- `*<glob>` segments (anywhere in the path) restrict the walk to matching paths. Examples: `~*.rs`, `~work/**/*.toml`, `~**/Cargo.toml`.
- Pure `~` (no body) returns everything.

## Cross-references

- [Tags mode — `~` searches files; `$` searches symbols inside those files](tags.md)
- [CodeGraph mode — the same FTS5-backed source-code index `&` uses, with richer relationship data](codegraph.md)
- [TECHNICAL — files-mode walk implementation](../../TECHNICAL.md#files-mode)
