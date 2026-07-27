# Segments mode (`:`)

| Default prefix | `:` |
| --- | --- |
| Configurable | `prefix.segments=<char>` (back-compat: `prefix.elements=` from before note_search's segment redesign still works) |

Segments mode searches `note_search`'s `segments` table — header-anchored sections — rather than whole files. It's the finer-grained sibling of [`@` (Notes) mode](notes.md): a search for a tag or link returns the specific section that references it, not just "this file mentions it somewhere." Requires a `note_search` build with segment-search support; `smarthistory check --prefix :` reports a clear error if the notes database predates it (missing `segments` table).

This mode used to be called "Elements mode" (`note_search`'s prior "element search" feature, indexing individual paragraphs/list-items/headings). Upstream redesigned it into coarser, header-bounded "segments" — see "What counts as a segment" below for what changed.

## What counts as a segment

- A **header** of level 1-4, plus all text below it (including further sub-headers) up to the next header of level <=4, is one segment. The header line itself is part of the segment's text.
- Headers of level **5 or 6** don't start a new segment — they and everything below them just become part of the enclosing level-<=4 segment's text.
- Content **before the first level-<=4 header** (or a file with no headers at all) forms an implicit root segment with no heading. A file that is entirely blank produces no segments at all.

Fenced code blocks are kept as part of the enclosing segment's text (a `#` inside one is never mistaken for a heading), but are otherwise scanned like any other text.

Unlike the old element search's heading cascade, a segment's tags and links come **only from its own text** — a subsection does NOT inherit its parent header's tags/links, and a frontmatter tag/link does not reach into any segment at all (frontmatter sits outside every segment's body). Instead every segment carries a **breadcrumb**: the note's filename followed by its ancestor headers' text (not including its own header), so a nested result is still identifiable without pulling in the whole ancestor chain's tags. The breadcrumb is what `Ctrl-Y` (yank) copies for a segment row — see "Selecting a row" below.

## What it does

- `:` (empty) — every indexed segment across every note.
- `:project reference` — every segment whose text contains "project reference" (bare words AND-match, case-insensitive). Because a segment can be a whole section, a match anywhere in that section returns the section's FULL text, not just the matching line.
- `:#urgent` — every segment tagged `urgent` in its own text (its header line and/or body — NOT a cascade from an ancestor header or the file's frontmatter, see "What counts as a segment" above).
- `:[[ProjectX]]` — every segment linking to `ProjectX` in its own text, same non-cascading rule as tags.
- `:(#urgent OR [[ProjectX]])` — OR-grouping, same Obsidian-like query language [`@` (Notes) mode](notes.md) and [`!` (Todo) mode](todo.md) use (`word`, `"quoted phrase"`, `#tag`, `[[link]]`, `[attr]`, `[attr:value]`, `(a OR b)`, terms AND-ed unless grouped). An invalid query (e.g. unbalanced parens) surfaces a status message rather than silently falling back to a text search.
- A segment with a heading starts with a literal `#`/`##`/... in the result list — that's the segment's own header line (verbatim, as written in the file), not a prefix smarthistory adds.
- A segment spanning multiple lines (its header plus everything below it) is shown with internal newlines joined by `" / "` — the same convention `note_search`'s own default output format uses.

## Debounce

The segments search is debounced: 400ms after the last keystroke. The search (and the initial empty-`:` search) runs on a background thread — same architecture as [`,` (ag) mode](ag.md#debounce) — so typing stays responsive even while a large or unbounded query is still running; results replace the list when the thread finishes. Pressing `Esc` while a search is in flight cancels it.

Segment rows also skip the command-history "labeled rows" merge that other modes apply on every keystroke (there's no equivalent concept for note segments, and results are already sorted server-side) — that merge scans and re-sorts the full result set unconditionally, which would otherwise dominate typing latency on a large notes vault independent of the search itself.

The list widget itself only builds visible rows: for a mode with a very large result count, redrawing the full result set on every keystroke (not just re-searching it) was the actual dominant cost on a large vault — the list view now only constructs what's on screen, regardless of how many rows the search matched.

## Output preview

Selecting any segment row loads context from the underlying **file** into the output preview (`Ctrl-O`), not just re-displaying that segment's own text verbatim — useful even for a short segment near the top or bottom of a long file, where seeing just that section in isolation still loses the surrounding context you'd get from scrolling the real file. The preview is a window of 50 lines **centered on the segment's `start_line`** (its header's line — 25 before, the line itself, 24 after), clamped to the file's boundaries. For a file shorter than the window this covers the entire file; for a longer file the segment's header is always visible without having to scroll down from the top to find it. Note the window centers on where the segment STARTS, not on wherever within a (possibly long) segment the searched text happened to appear.

The window is a **raw, unmodified slice of the file** piped through `bat` — the same "clean markdown in, syntax-highlighted markdown out" pipeline `@` (Notes) / `!` (Todo) mode use — so headings, checkboxes, and links render exactly as they would if you opened the file directly. This is deliberately *not* the `$` (Tags) / `,` (ag) mode convention (`read_source_context_with_cache`, which prefixes every line with a line number and marks the match with `>>`): that annotation isn't valid markdown and would fight `bat`'s own highlighting.

The highlighted result is cached per (file, line) for the session. Every keystroke re-runs the list (see "Debounce" above) and rebuilds the row's raw text, so without this cache the currently-selected row's preview would re-invoke `bat` on every single keystroke — the exact per-keystroke stall the background search thread exists to avoid. A cache hit is a plain map lookup; only the first time a given segment is selected pays for the `bat` process spawn.

## Selecting a row

- `Enter` on a segment row stages `$EDITOR +<start_line> <file>` — the file opens at the segment's header line. Same "open the file at the matching line" convention as [`$` (Tags)](tags.md), [`,` (ag)](ag.md), and [`&` (CodeGraph)](codegraph.md).
- `Ctrl-Y` (Yank selection) copies the row's **breadcrumb** (filename + ancestor headers' text), not the matched segment's own text — a segment's text can be a whole section joined onto one line, which is less useful on the clipboard than knowing exactly which file and section it came from. (If the output preview overlay is open, `Ctrl-Y` still copies what's on screen instead, same as every other mode.)

## Tab completion

`Tab` (the default `Action::JiraFieldComplete` key) works exactly the same as [`@` (Notes) mode](notes.md#tab-completion): cursor after `#` completes a tag name, cursor after `@` completes a link name (inserted as `[[linkname]]`). Same completion source (`notes.database`'s unique tag/link values), same ambiguous-match menu, same unique-match trailing-space behavior — segments mode doesn't have its own separate tag/link namespace, so the completion candidates are identical to what `@` mode offers.

## Required configuration

Same as `@` / `!` mode: `notes.database` and `notes.dir`. See [notes.md](notes.md#required-configuration).

## Cross-references

- [Notes mode — the parent whole-file search this mode complements](notes.md)
- [Todo mode — todo checkbox lines are indexed separately by line, not folded into segments](todo.md)
- [TECHNICAL — JIRA / notes mode implementation details](../../TECHNICAL.md)
