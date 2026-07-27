# Similar mode (`"`)

| Default prefix | `"` |
| --- | --- |
| Configurable | `prefix.similar=<char>` |

Similar mode ranks `note_search`'s `segments` table (the same table [`:` (Segments) mode](segments.md) searches) by MEANING rather than keyword: the entire typed body is embedded via a local Ollama call and matched against every segment's stored embedding by cosine similarity, highest-first. Unlike Segments mode, there's no query DSL here — `#tag`, `[[link]]`, and `(a OR b)` aren't parsed specially, so any such characters you type (or Tab-complete in) just become part of the literal phrase that gets embedded.

Requires three things, in order: (1) a `note_search` build with segment-embeddings support, (2) a notes database that was imported/re-imported with that build — the `segments.embedding` column is added when the `segments` table is first *created*, not retroactively to an existing one, so an older database needs a fresh `note_search import` — and (3) a reachable local Ollama instance with the `nomic-embed-text` model pulled (same model `note_search import` uses to compute each segment's stored embedding at index time). `smarthistory check --prefix "` reports which of these (if any) is missing.

## What it does

- `"a phrase describing what you're looking for` — embeds the whole string and returns up to 25 segments ranked by similarity to it, most-similar first.
- An empty `"` has nothing to embed or compare, so it's a no-op (no results, no request sent to Ollama).
- Each result is prefixed with its similarity score, e.g. `[0.87] ## Timeline / ...` — since (unlike Segments mode's exact tag/link/text filters) a phrase always returns SOME ranked list, the score is the only signal for how relevant a given result actually is.
- Results are otherwise displayed the same way as Segments mode: a segment with a heading starts with its own literal `#`/`##`/... header line, and multi-line segment text is joined with `" / "`.

## Debounce

Same 400ms-after-last-keystroke, background-thread architecture as [`:` (Segments) mode](segments.md#debounce) — necessary here even more than there, since the embedding call is a synchronous HTTP round-trip to Ollama that can easily take longer than a plain SQL query. Pressing `Esc` while a search is in flight cancels it (the in-flight HTTP request itself can't be aborted mid-call, but a cancelled result is discarded on arrival rather than replacing the list).

## Output preview

Identical to [Segments mode's output preview](segments.md#output-preview): a 50-line raw-markdown window piped through `bat`, centered on the matched segment's `start_line` (its header's line), cached per (file, line) for the session.

## Selecting a row

- `Enter` on a result stages `$EDITOR +<start_line> <file>` — same convention as Segments mode.
- `Ctrl-Y` (Yank selection) copies the row's **breadcrumb** (filename + ancestor headers' text), not the `[score] text` shown in the list — same rationale as Segments mode: the breadcrumb identifies WHERE the match is, which is more useful on the clipboard than a long flattened section of note content.

## Tab completion

Same as [Segments mode](segments.md#tab-completion): `Tab` completes a `#tag` or `@link` token from `notes.database`'s tag/link namespace, inserted into the phrase text. This is purely a typing convenience — the inserted `[[linkname]]` or `#tagname` is embedded as literal text along with the rest of the phrase, not extracted as a separate filter.

## Required configuration

Same as `@` / `!` / `:` mode: `notes.database` and `notes.dir`. See [notes.md](notes.md#required-configuration). Additionally needs Ollama reachable — `smarthistory check --prefix "` verifies this the same way `=` (LLM) mode's check verifies its own model, against `note_search::embeddings`'s own `OLLAMA_HOST`-driven default (independent of smarthistory's own `ollama.url` config used by `=` mode).

## Cross-references

- [Segments mode — the same `segments` table, searched by exact tag/link/text query instead of similarity](segments.md)
- [LLM mode — the other mode that talks to a local Ollama instance, for command generation rather than embeddings](llm.md)
- [TECHNICAL — JIRA / notes mode implementation details](../../TECHNICAL.md)
