# Browser mode (`^`)

| Default prefix | `^`                     |
| -------------- | ----------------------- |
| Configurable   | `prefix.browser=<char>` |

Browser mode merges bookmarks and visited-URL history read straight from
locally-installed browsers' own profile files — no browser extension, no
running-browser IPC, no API token. Chrome, Firefox, and Safari are all
supported. Every row is tagged with the literal word `bookmark` or `history` so
you can narrow the merged list to just one source by typing that word.

## What it does

- `^` (empty) — every bookmark and history entry from every configured (or
  auto-detected) browser, newest first.
- `^bookmark rust` — only bookmarks whose tag/title/URL contains "rust".
- `^history github` — only history entries whose tag/title/URL contains
  "github".
- `^rust` (no tag word) — both bookmarks and history matching "rust".
- The first text column is `"<tag> <title>"` (falls back to the URL when a row
  has no title); the second is the URL (shown as the `#`-prefixed comment slot
  every mode uses for a secondary line).

## Sources

| Browser             | Bookmarks                                   | History                                                     | Format                                 |
| ------------------- | ------------------------------------------- | ----------------------------------------------------------- | -------------------------------------- |
| Chrome              | `<profile>/Bookmarks`                       | `<profile>/History`                                         | JSON / SQLite (`urls` table)           |
| Firefox             | `<profile>/places.sqlite` (`moz_bookmarks`) | `<profile>/places.sqlite` (`moz_places`)                    | SQLite                                 |
| Safari (macOS only) | `<profile>/Bookmarks.plist`                 | `<profile>/History.db` (`history_visits` + `history_items`) | Property list (binary or XML) / SQLite |

By default (no `browser.<id>.*` config keys), Chrome, Firefox, and Safari are
all auto-detected at their platform-default profile locations — only a browser
that's actually installed (profile directory exists on disk) is included. Set
explicit sources — or point at a non-default profile — via config:

```ini
# Read a specific Chrome profile instead of "Default":
browser.1.type=chrome
browser.1.profile=~/Library/Application Support/Google/Chrome/Profile 2

# Add Firefox using its auto-detected default profile (no override needed):
browser.2.type=firefox

# Add Safari (also auto-detected by default — Safari has no
# separate profile concept, so there's nothing to override):
browser.3.type=safari
```

See [`docs/configuration.md`](../configuration.md#browser--mode) for the full
key reference.

Every browser's database file is typically held open (and often locked) by a
running browser process, so every read is done against a temporary snapshot copy
(the main db file plus its `-wal` / `-shm` sidecars, for the SQLite sources)
rather than the live file — the same approach every other "read a live browser's
history" tool uses. Safari's `Bookmarks.plist` is a plain file read (not locked
the same way), parsed via the `plist` crate, which auto-detects whether the file
is the binary or XML property-list format.

## Selecting a row

- `Enter` stages `open "<url>"` (macOS) / `xdg-open "<url>"` (other Unixes) and
  exits. The TUI is gone before the browser opens.
- The URL comes straight off the row (no server round-trip needed, unlike
  [JIRA](jira.md) or [Paperless](paperless.md), which reconstruct the browse URL
  from a stored id) — it's read verbatim from the browser's own
  bookmarks/history file, then shell-quoted before staging.
- `Ctrl-]` (`Action::SmartOpen`) converts the entry into a local note instead —
  `note_search convert <url>`, `note_search`'s general "convert a web page or
  document to a markdown note" command, saved to
  `NOTE_SEARCH_DIR`/`NOTE_SEARCH_DATABASE` (or `note_search`'s own defaults if
  those aren't set) — then opens the freshly-created note in `$EDITOR`. No
  `-o`/`-d` flags passed explicitly, same convention
  [JIRA mode's `Ctrl-M-s` download-as-note action](jira.md) uses. Unlike the
  daily-note dialog's `Ctrl-O`, the target path isn't known ahead of time —
  `note_search convert` names the file itself from the page title — so the
  staged command captures it from `note_search`'s own
  `Successfully created note: <path> (type: …)` stdout line rather than
  precomputing it; a failed conversion (bad URL, network error) simply doesn't
  open anything, and you still see `note_search`'s own error output on the
  terminal either way.

## Match algorithm

Toggle with `Ctrl-F`. The default is `sub` (case-insensitive substring, AND'd by
whitespace-separated word across the combined `"<tag> <title> <url>"` text) —
same convention as [Files (`/`) mode](files.md).

## Debounce

The read is debounced: 400ms after the last keystroke (matches the files-mode
walk debounce). The read runs on a background thread; the result populates the
list when it lands.

## Limitations

- **Safari requires Full Disk Access on macOS.** `~/Library/Safari` is protected
  by macOS's TCC privacy layer: the directory itself is visible (`is_dir()` /
  `stat` succeed), but opening `Bookmarks.plist` or `History.db` fails with
  "Operation not permitted" unless the terminal app running `smarthistory` has
  been granted **Full Disk Access** in System Settings → Privacy & Security →
  Full Disk Access (then restart the terminal). Without it, the Safari source
  silently yields zero rows — run `smarthistory check --prefix ^` to get an
  explicit diagnostic instead of guessing why Safari rows aren't showing up.
- **Safari bookmarks have no reliable timestamp.** Unlike Chrome (`date_added`
  on every leaf) or Firefox (`dateAdded`), Safari's bookmarks plist has no
  stable "date added" field for a plain bookmark — only Reading List items carry
  one, in a differently-shaped sub-dictionary this mode doesn't parse. Safari
  bookmark rows get `timestamp: 0` and sort as the oldest rows in the merged,
  newest-first list; Safari _history_ rows have a real timestamp and sort
  normally.
- **Read-only.** There's no way to add/remove a bookmark from inside the TUI —
  this mode is a search-and-open surface, not a bookmark manager.
- **A capped, recent window.** Each source's history read is capped at 5000 rows
  (newest-visited first) and the merged, filtered result is capped at 2000 rows
  — a profile with years of history has far more rows than that, but the TUI has
  no use for anything past a recent window.
- **Windows isn't supported** (matches this app's `open`/`xdg-open`
  macOS/Linux-only convention for every other URL-opening mode).

## Privacy convention

Like every other prefix mode except plain history search, selecting a `^`-mode
row stages a **space-prefixed** command (`_smarthistory_precmd` skips recording
it) — see
[`README.md`'s privacy-convention section](README.md#privacy-convention-space-prefix).
Opening a URL from your own bookmarks/history isn't a command worth re-surfacing
in future searches.

## Cross-references

- [`/` (Files) — the closest architectural sibling: a local-disk background walk, debounced, filtered by AND'd substring tokens](files.md)
- [`-` (JIRA) / `<` (Paperless) — the other two "selecting a row opens a URL in the system browser" modes](jira.md)
- [`README.md`](README.md) — mode index, common actions, match algorithms
