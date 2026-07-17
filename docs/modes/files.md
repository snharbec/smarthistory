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
- `Ctrl-]` (Smart open) opens the file with a per-extension command configured via `smart-open.<ext>=<cmd>` in the config file. See [File-type-aware open](#file-type-aware-open-smart-open) below.

## File-type-aware open (Smart open)

`Ctrl-]` (the default `Action::SmartOpen` binding) adapts to the active mode. In `~` (files) mode it looks up the selected file's extension (lowercase, no leading `.`) in a per-extension command table and runs the matched command with the file path appended. The match then exits so the parent shell runs the staged command.

This is the typical "smart-open" workflow: `Ctrl-]` on a `.md` file runs `leaf` (a markdown viewer), on a `.rs` file runs `bat` (a syntax-highlighted read), on a `.png` file runs `xdg-open` (open in the system viewer), etc. — without the user having to remember the per-extension command. The default `Enter` (open in `$EDITOR`) is preserved for files that don't have a mapping, and the user can rebind `key.smart-open=...` to anything.

### Configuration

The per-extension table is configured via `smart-open.<ext>=<cmd>` lines in the config file (NOT `key.<action>=<spec>` — the `key.` prefix is reserved for key bindings, and the values are shell commands, not key specs). The key prefix `smart-open.` (without `key.`) is the discriminator.

```ini
# ~/.config/smarthistory/config
smart-open.md=leaf          # markdown files → `leaf README.md`
smart-open.rs=bat           # rust code → `bat src/main.rs`
smart-open.py=bat --style=numbers   # flags are passed through verbatim
smart-open.png=xdg-open     # images → `xdg-open photo.png`
smart-open.default=bat      # any other extension → `bat README` (catch-all)
```

| Key | What it does |
| --- | --- |
| `smart-open.<ext>=<cmd>` | Match the file extension `<ext>` (lowercase). The command is taken verbatim and the file's absolute path is appended (POSIX single-quote escaped so paths with spaces / shell metacharacters can't break the staged command). |
| `smart-open.default=<cmd>` | Catch-all for any extension without an explicit mapping. The optional `default` key is the convention; pick whatever command you'd want most files to open with. |
| (no `smart-open.*` configured) | `Ctrl-]` falls through to the same `Enter` behavior (open in `$EDITOR`). The per-extension config is purely additive. |

### Matching rules

- **Case-insensitive**: a file named `README.MD` matches the `md` mapping. The lookup is lowercased; the *key* in the config preserves the user's casing (so `smart-open.MD=leaf` and `smart-open.md=leaf` are the same entry, but the stored key is the user's input).
- **`Path::extension()` semantics**: the extension is the part after the **last** `.` of the file name. So `foo.tar.gz` matches `gz`, `README.md` matches `md`, `foo` has no extension, and `Makefile` has no extension.
- **Extensionless files** (`Makefile`, `LICENSE`, …) skip the per-extension lookup and go straight to `default`. If `default` isn't set either, the dispatch falls through to `Enter` (open in `$EDITOR`).
- **Multiple extensions**: list each one on its own line. There is no glob / pattern matching (no `smart-open.*=bat` to match all extensions) — the table is one-key-per-extension, deliberately. The `default` key is the right way to handle the "everything else" case.
- **Hidden files** (`.bashrc`): the files-mode walk skips them by default (see the `files.ignore` config above), so they're not in the result list to begin with.
- **Empty values** (`smart-open.rs=` with nothing after the `=`) are silently dropped so a typo doesn't bind to an empty command.

### Examples

| Selected file | Configured `smart-open.*` | What `Ctrl-]` does |
| --- | --- | --- |
| `~/notes/todo.md` | `smart-open.md=leaf` | stages `leaf '/home/user/notes/todo.md'` and exits. |
| `~/code/main.rs` | `smart-open.rs=bat --style=numbers` | stages `bat --style=numbers /home/user/code/main.rs` and exits. |
| `~/photos/cover.png` | `smart-open.png=xdg-open` | stages `xdg-open /home/user/photos/cover.png` and exits. |
| `~/code/main.rs` | `smart-open.default=bat` (no `rs` mapping) | stages `bat /home/user/code/main.rs` and exits. |
| `~/code/main.rs` | `smart-open.md=leaf` (no `rs` mapping, no `default`) | falls through to `Enter` (open in `$EDITOR`). |
| `~/project/Makefile` | `smart-open.default=bat` (no extension on the file) | stages `bat /home/user/project/Makefile` and exits. |
| `~/project/Makefile` | (no `smart-open.*` at all) | falls through to `Enter`. |
| a directory row | `smart-open.default=bat` (any config) | falls through — `Ctrl-]` in files mode only fires on **file** rows, not directories. Directories are handled by `Enter` (creates / focuses a workspace rooted there). |

The dispatch lives in `App::smart_open_for_file` in [`src/tui.rs`](../src/tui.rs); the config parser in [`src/main.rs`](../src/main.rs) (`Config::parse`) handles the `smart-open.<ext>=<cmd>` lines.

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
