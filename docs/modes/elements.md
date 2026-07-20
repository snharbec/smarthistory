# Elements mode (`:`)

| Default prefix | `:` |
| --- | --- |
| Configurable | `prefix.elements=<char>` |

Elements mode searches `note_search`'s `elements` table — individual paragraphs, list items, and headings — rather than whole files. It's the finer-grained sibling of [`@` (Notes) mode](notes.md): a search for a tag or link returns the specific piece of text that references it, not just "this file mentions it somewhere." Requires a `note_search` build with element-search support (added upstream in the commit titled "Support to search for single elements inside notes", with `#tag` / `[[link]]` query-DSL support following in "Support query for elements"); `smarthistory check --prefix :` reports a clear error if the notes database predates it (missing `elements` table).

## What counts as an element

Same semantics as `note_search`'s own `elements` CLI subcommand:

- **List item** — a bullet (`-`, `*`, `+`) or numbered (`1.`) item, plus all of its more-deeply-indented children, concatenated into one element. Each nested child is *also* its own element in its own right. A plain text line immediately following a list item with no blank-line separator is folded into that list item rather than becoming its own paragraph.
- **Paragraph** — a run of contiguous non-blank, non-heading, non-list lines.
- **Heading** — a single `#`-`######` line.
- **Checkbox/todo lines** (`- [ ] ...`) count as list items too, so the same line shows up in both [`!` (Todo) mode](todo.md) and here.

Fenced code blocks are skipped entirely.

A tag or link on a **heading** applies to every element in that heading's section; a tag or link in the document's **frontmatter** applies to every element in the file. See the upstream `note_search` README's "Element Search" section for the full cascading semantics.

## What it does

- `:` (empty) — every indexed element across every note.
- `:project reference` — every element whose text contains "project reference" (bare words AND-match, case-insensitive).
- `:#urgent` — every element tagged `urgent`, including cascaded matches (a tag on a heading applies to every element in that heading's section; a tag in the document's frontmatter applies to every element in the file — see "What counts as an element" above).
- `:[[ProjectX]]` — every element linking to `ProjectX`, with the same cascade rules as tags.
- `:(#urgent OR [[ProjectX]])` — OR-grouping, same Obsidian-like query language [`@` (Notes) mode](notes.md) and [`!` (Todo) mode](todo.md) use (`word`, `"quoted phrase"`, `#tag`, `[[link]]`, `[attr]`, `[attr:value]`, `(a OR b)`, terms AND-ed unless grouped). An invalid query (e.g. unbalanced parens) surfaces a status message rather than silently falling back to a text search.
- Heading elements are shown with a `#`/`##`/... prefix (matching the heading's level) so they're visually distinguishable from paragraphs and list items in the result list.
- An element spanning multiple lines (a list item with nested children, a multi-line paragraph) is shown with internal newlines joined by `" / "` — the same convention `note_search`'s own default output format uses.

## Debounce

The elements search is debounced: 400ms after the last keystroke. The search (and the initial empty-`:` search) runs on a background thread — same architecture as [`,` (ag) mode](ag.md#debounce) — so typing stays responsive even while a large or unbounded query is still running; results replace the list when the thread finishes. Pressing `Esc` while a search is in flight cancels it.

Elements rows also skip the command-history "labeled rows" merge that other modes apply on every keystroke (there's no equivalent concept for note elements, and results are already sorted server-side) — that merge scans and re-sorts the full result set unconditionally, which would otherwise dominate typing latency on a large notes vault independent of the search itself.

The list widget itself only builds visible rows: for a mode with a very large result count, redrawing the full result set on every keystroke (not just re-searching it) was the actual dominant cost on a large vault — the list view now only constructs what's on screen, regardless of how many rows the search matched.

## Output preview

Selecting any row — heading, paragraph, or list item — loads context from the underlying **file** into the output preview (`Ctrl-O`), not just that element's own isolated text (a bare `[[link]]` reference line's own text can be as short as the link name itself). The preview is a window of 50 lines **centered on the matched element's own line** (25 before, the line itself, 24 after), clamped to the file's boundaries. For a file shorter than the window this covers the entire file; for a longer file the matched line is always visible without having to scroll down from the top to find it.

The window is a **raw, unmodified slice of the file** piped through `bat` — the same "clean markdown in, syntax-highlighted markdown out" pipeline `@` (Notes) / `!` (Todo) mode use — so headings, checkboxes, and links render exactly as they would if you opened the file directly. This is deliberately *not* the `$` (Tags) / `,` (ag) mode convention (`read_source_context_with_cache`, which prefixes every line with a line number and marks the match with `>>`): that annotation isn't valid markdown and would fight `bat`'s own highlighting.

The highlighted result is cached per (file, line) for the session. Every keystroke re-runs the list (see "Debounce" above) and rebuilds the row's raw text, so without this cache the currently-selected row's preview would re-invoke `bat` on every single keystroke — the exact per-keystroke stall the background search thread exists to avoid. A cache hit is a plain map lookup; only the first time a given element is selected pays for the `bat` process spawn.

## Selecting a row

- `Enter` on an element row stages `$EDITOR +<start_line> <file>` — the file opens at the exact line the element starts on. Same "open the file at the matching line" convention as [`$` (Tags)](tags.md), [`,` (ag)](ag.md), and [`&` (CodeGraph)](codegraph.md).
- `Ctrl-Y` (Yank selection) copies the containing note's **filename**, not the matched element's own text — a bare `[[link]]` reference line's own text can be as short as the link name itself, which isn't useful on the clipboard. (If the output preview overlay is open, `Ctrl-Y` still copies what's on screen instead, same as every other mode.)

## Tab completion

`Tab` (the default `Action::JiraFieldComplete` key) works exactly the same as [`@` (Notes) mode](notes.md#tab-completion): cursor after `#` completes a tag name, cursor after `@` completes a link name (inserted as `[[linkname]]`). Same completion source (`notes.database`'s unique tag/link values), same ambiguous-match menu, same unique-match trailing-space behavior — elements mode doesn't have its own separate tag/link namespace, so the completion candidates are identical to what `@` mode offers.

## Required configuration

Same as `@` / `!` mode: `notes.database` and `notes.dir`. See [notes.md](notes.md#required-configuration).

## Cross-references

- [Notes mode — the parent whole-file search this mode complements](notes.md)
- [Todo mode — todo checkbox lines are also indexed as list-item elements](todo.md)
- [TECHNICAL — JIRA / notes mode implementation details](../../TECHNICAL.md)
