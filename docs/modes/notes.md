# Notes mode (`@`)

| Default prefix | `@` |
| --- | --- |
| Configurable | `prefix.notes=<char>` |

Notes mode searches the `note_search` SQLite database (a separate project: <https://github.com/snharbec/note_search>) for entries that match the body of the query. The notes database is shared between the standalone `note_search` CLI and smarthistory's `@` / `!` modes.

## What it does

- `@docker compose` — every note that mentions `docker` *and* `compose` (whitespace-AND).
- The first text column is the note's title; the second is the file basename; the third is the timestamp.
- Empty query (just `@`) returns every note — useful as a quick "show me all my notes" view.

## Selecting a row

- `Enter` on a note row stages `note_search edit-note <note-id>` (the note opens in `$EDITOR`).
- `Enter` on the special `@new <text>` quick-create row appends a timestamped line to today's daily note. The TUI exits and the parent shell runs the `note_search create-note` command.
- `Ctrl-E` opens the comment editor for the note (a smarthistory-side annotation, separate from `note_search`'s own metadata).

## Required configuration

`notes.database` (path to a `note_search` SQLite DB) and `notes.dir` (the parent directory the notes live in) must both point to a real location. Example:

```ini
# ~/.config/smarthistory/config
notes.database=~/notes/notes.db
notes.dir=~/notes
```

Either missing → `@` mode is a no-op and the status bar shows a "notes not configured" message.

## Shorthand expansion (TUI → `note_search`)

`note_search`'s own query parser understands the full [Obsidian-style search syntax](https://github.com/snharbec/note_search) — bare words (AND-matched against note text), `#tag` (tag search), `[[link]]` (wiki-link search), `[attr:value]` (header-attribute search). smarthistory's `@` mode accepts the full syntax **plus** several shorthand expansions that the TUI applies *before* handing the cleaned body to `note_search::parse_query`. The expansion is implemented in `parse_notes_query` in [`src/tui.rs`](../../src/tui.rs) and the table below covers every recognised token.

| What you type | What it becomes | Effect |
| --- | --- | --- |
| `docker compose` | `docker compose` (unchanged) | Plain-text AND search across note content. |
| `#urgent` | `#urgent` (unchanged) | Tag search — finds notes tagged `urgent`. Multiple `#tag` tokens combine (AND). |
| `@MyNote` | `[[MyNote]]` | Wiki-link search — finds notes that have a link *to* `MyNote` (i.e. notes whose body contains `[[MyNote]]` or `[MyNote](...)`). **The link name preserves the user's original casing** (Obsidian treats `MyNote` and `mynote` as different links). |
| `@"my note"` (quoted, with a space) | `[["my note"]]` | Wiki-link search for a link with a space in its name. The double quotes inside `[[...]]` are the Obsidian convention for escaping a boundary inside a wiki link. |
| `@today` (or `today`) | *(removed from query body)* | Date filter — only notes updated in the last 24 h. Applied as a post-filter against the `updated` timestamp. |
| `@week` (or `week`) | *(removed)* | Date filter — only notes updated in the last 7 days. |
| `@month` (or `month`) | *(removed)* | Date filter — only notes updated in the last 30 days. |
| `@year` (or `year`) | *(removed)* | Date filter — only notes updated in the last 365 days. |
| `[attr:value]` (e.g. `[assignee:me]`) | `[attr:value]` (unchanged) | Header-attribute search — matches against the front-matter `key: value` fields of each note. Requires the note to have the relevant header key. |
| `email@today.com` (an email with `@today` in the middle) | `email@today.com` (unchanged) | Date-alias matching is **whole-word only** (whitespace-separated tokens). `@today` inside `email@today.com` is not recognised as the alias. |

The date-alias path strips the leading `@` for the alias match (so `today` and `@today` both work — same convention as the notes-mode parser in [notes-mode elsewhere in the project](../../TECHNICAL.md)) but preserves the alias's casing in the matched form (it doesn't show up in the query body, so casing is irrelevant). Link searches preserve the user's original casing because the underlying `note_search` match is case-sensitive on link targets.

### Why expand `@link` to `[[link]]`?

The `note_search` library uses Obsidian's `[[wikilink]]` syntax for link search. smarthistory's `@` shorthand is a convenience — the user types `@MyNote` and the TUI rewrites it to `[[MyNote]]` before passing to `note_search::parse_query`. The `[[...]]` form is the canonical link-search syntax; the `@` form is just smarthistory's user-facing ergonomics on top of it. The rewrite is purely lexical — no link resolution happens in smarthistory, only in `note_search`.

### Combining tokens

Multiple token types combine freely. A query like `@urgent #feature rust` expands to `[[urgent]] #feature rust` (note: `[[urgent]]` would be a *link* to a note called `urgent`, not the date alias — the date alias form is the bare keyword without `[[...]]`). The `note_search` parser then ANDs all three clauses together.

The date filter is the only token that's *removed* from the body rather than rewritten; it's stored as a separate `NotesDateFilter` value and applied post-query against each row's `updated` timestamp. Multiple date aliases collapse to the last one (`@today @week` ends up as `Week` because `@week` is encountered second).

## Tab completion

Press `Tab` while typing in `@` mode to open a completion menu for the token under the cursor. The kind of completion depends on which prefix the cursor is on:

| Cursor shape | Completion kind | Source |
| --- | --- | --- |
| Cursor after `#` (with at least one char) | `NotesTag` — unique tag names from the `note_search` DB. | `note_search::commands::metadata::get_unique_values(db, "tag")` |
| Cursor after `@` (with at least one char) | `NotesLink` — unique link-target names from the `note_search` DB (with `.md` stripped). | `note_search::commands::metadata::get_unique_values(db, "link")` |
| Cursor after a known JIRA prefix and a partial field name | `JiraField` — JIRA field names from the `/rest/api/2/field` endpoint. (JIRA-mode tab completion; also triggered when the prefix is `-` even outside JIRA mode for the JIRA-field-complete dispatch.) | `JiraClient::fetch_fields` |
| Cursor after a JIRA-prefix `@` and a partial name | `JiraAlias` — built-in (`me` / `today` / `week` / `month`) + user-defined `jira.search.*` fragment names. | in-process |

The completion behaviour:

- **Unique match** — `Tab` inserts the full name, followed by a trailing space (so the user can immediately type the next token). For wiki-links, the inserted form is `[[linkname]]` (with quotes around spaced names, e.g. `[["my note"]]`).
- **Ambiguous prefix** (multiple matches share the prefix) — `Tab` inserts the longest common prefix (LCP). Subsequent `Tab` presses cycle through the remaining candidates, applying each one in turn.
- **No match** — `Tab` is a no-op; the cursor stays put.

The completion menu is a sibling overlay of the prefix picker, command palette, and theme picker — it sits above the help overlay so `Esc` / `Ctrl-C` / the configured `Cancel` key dismisses it without scrolling the help underneath.

Link-name completion case-folds the prefix for matching but inserts the **canonical** casing from the database, so `@bern` + Tab inserts `bernd_matthiesen` (whatever casing the note was originally created with). This matches Obsidian's own link-completion behaviour.

## Cross-references

- [Todo mode — the sibling mode for open todo entries; the same shorthand expansion and Tab completion apply because the underlying `note_search::parse_query` is shared](todo.md)
- [JIRA — the `jira-issue` action downloads a JIRA issue as a local note, which then becomes searchable in `@` mode](jira.md)
- [TECHNICAL — note_search integration details](../../TECHNICAL.md#notes-mode-integration)
- [`parse_notes_query` in `src/tui.rs`](../../src/tui.rs) — the implementation of the shorthand expansion
