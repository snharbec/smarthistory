# ag mode (`,`)

| Default prefix | `,`                |
| -------------- | ------------------ |
| Configurable   | `prefix.ag=<char>` |

![ag mode demo](../../assets/demo-ag.gif)

ag mode searches file _contents_, in-process, using the same libraries
[ripgrep](https://github.com/BurntSushi/ripgrep) itself is built from
(`grep-regex`/`grep-matcher`/`grep-searcher` for matching, `ignore` for the
gitignore-aware parallel directory walk) — there's no external binary to
install. Every line containing the typed pattern is listed as a row; selecting
a row stages `$EDITOR +LINE file` and exits.

## What it does

- `,` (empty) — no results (the search needs at least one search term). Use
  this to confirm the mode is active.
- `,TODO` — every line containing `TODO` in every text file under the cwd
  (respecting `.gitignore`, including hidden/dotfiles).
- `,TODO *.rs` — every `TODO` line in every `.rs` file. The `*` in `*.rs` is a
  glob restricting which files are searched.
- `,@rust TODO` — every `TODO` line in every Rust file. The `@rust` token
  also restricts which files are searched (by extension), not just which
  language the preview is highlighted as.
- Each row shows the file's path first, then the matched line. The path is
  shortened as compactly as possible: every directory component is abbreviated
  to its first character (two characters for a dotfile-style directory, e.g.
  `.config` → `.c`), while the filename itself is always shown in full (e.g.
  `~/w/p/src/main.rs` for `~/work/project/src/main.rs`) — so you can immediately
  tell which file a match is in without the path crowding out the match content.
  The timestamp column (shown after the match) is the file's modification time.
  Results are sorted by that timestamp, newest-modified file first — matches in
  a file you just edited surface before matches in files you haven't touched in
  months. Multiple matches within the same file keep line-number order.
- Case-sensitivity is "smart case," matching every other search mode in the
  app: a pattern with no uppercase letters matches case-insensitively; a
  pattern containing an uppercase letter matches case-sensitively.

## Selecting a row

- `Enter` stages `$EDITOR +LINE file` and exits.
- `Ctrl-O` (Show output) opens the **~50-line source context** (roughly half
  before, half after the match) with in-process syntax highlighting. The match
  line is prefixed with `>>` so you can spot it at a glance. The full context is
  scrollable in the overlay.

## Special tokens

| Token                                      | Meaning                                                                                                                                                                                                              |
| ------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `@<lang>`                                  | Restrict the search to files of a given language (by extension) — this restricts which files are WALKED, not just the preview's highlight language. Multiple `@lang` tokens are OR'd; an unrecognized language contributes no restriction. |
| `*<glob>` (anywhere in the path component) | Restrict the search to files matching the glob (gitignore-glob syntax — the same syntax a `.gitignore` line uses). Multiple glob tokens are OR'd. Examples: `*.rs` (any `.rs` file), `Cargo.toml` (just that file). |
| `<text>` (any other whitespace token)      | The first such token is the primary search pattern. Subsequent tokens are post-filters: every additional token must appear (case-insensitive) in the matched line.                                                   |

The token language is the same as `tags` and `codegraph` mode for the `@<lang>`
part, but the _first_ text token is special: it's the search pattern. A query
like `,TODO fix` matches lines containing `TODO` (primary) that ALSO contain
`fix` (post-filter).

## Ignore behavior

`,` mode respects `.gitignore`, `.ignore`, and global git excludes — same as
`git`/ripgrep — but note this only takes effect **inside an actual git
repository** (somewhere with a `.git` directory above the search root). A
bare `.gitignore` file with no git repo at all has no effect, which is a
minor, deliberate divergence from `ag`'s more lenient behavior. Hidden files
and dotfiles/dot-directories are always included; binary files are always
skipped.

## Debounce

The search is debounced: 400ms after the last keystroke, same as JIRA and
files mode. The search runs in a background thread; the result populates the
list when it lands. A superseded search (one more keystroke before it
finishes) is stopped promptly rather than left to run to completion in the
background.

## Per-mode input history

`Ctrl-P` / `Ctrl-N` cycle through the **ag mode's** past queries. Other modes
have their own per-mode history (scoped by prefix), so `Ctrl-P` in `&` mode only
recalls past `&` queries. See
[`README.md`](README.md#common-actions-that-work-in-every-mode) for the full set
of common actions.

## Cross-references

- [Tags mode — `,` searches file _contents_; `$` searches the _symbols_; `/` searches the file _names_](tags.md)
- [Files mode — `/` walks the file system; `,` then searches each file's contents](files.md)
- [CodeGraph mode — for symbol-and-relationship navigation, prefer `&`](codegraph.md)
- [TECHNICAL — ag-mode implementation](../../TECHNICAL.md#ag-mode)
