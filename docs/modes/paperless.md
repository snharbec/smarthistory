# Paperless mode (`<`)

| Default prefix | `<` |
| --- | --- |
| Configurable | `prefix.paperless=<char>` |

Paperless mode searches documents on a self-hosted [Paperless-ngx](https://docs.paperless-ngx.com/) v3 instance via its REST API. Documents matching the query are listed with their title, correspondent, and tags. Selecting a document opens its details page in the system browser.

## What it does

- `<invoice` — every document whose title contains "invoice" (substring match).
- `<#work` — every document tagged exactly `work` (whole-word match).
- `<@acme` — every document whose correspondent name is exactly `acme` (whole-word match, case-insensitive — but not partial).
- `<invoice #work @acme` — title AND tag AND correspondent, all combined.
- The primary text is the document title; the secondary `# ...` text is the correspondent name and tags (e.g. `Acme Corp · #work #2024`).

## Special tokens

The body is split on whitespace by [`crate::paperless::parse_pattern`](../../src/paperless.rs) into a [`PaperlessFilters`](../../src/paperless.rs), sent as Django REST filterset query parameters on `GET /api/documents/` — **not** Paperless-ngx's full-text `query=` search parameter. An earlier version used `query=` with the documented `title:`/`tag:`/`correspondent:` syntax plus `*wildcard*` markers for substring matching, but real-world testing against a live instance showed the wildcard forms (`*word*`, `word*`, and a bare unscoped `*word*`) all still only matched whole words. The Django filterset lookups below are plain ORM queries (`WHERE ... ILIKE '%value%'` / `= value`), independent of whatever full-text index Paperless-ngx has built, so they don't have this failure mode — verified against a live instance.

| Pattern | Category | Filter param | Behaviour |
| --- | --- | --- | --- |
| anything else | title words | `title__icontains` | **substring**, case-insensitive. Multiple bare words are joined with a space (in typed order) into ONE value — `<annual report` searches for the literal substring "annual report", not "annual" AND "report" independently. This is a real narrowing forced by the API (`icontains` takes one value; Django can't AND two of the same filter in one request), not a design choice. |
| `#TAG` | tag | `tags__name__iexact` | whole-word / exact, case-insensitive. Only the LAST `#TAG` token in the query is used — same one-value-per-field reason. |
| `@AUTHOR` | correspondent | `correspondent__name__iexact` | whole-word / exact, case-insensitive. Only the LAST `@AUTHOR` token is used. |

A bare `#` or `@` (empty tag/author) is dropped rather than producing an empty filter. An unset filter is omitted from the request entirely (not sent as `field=`).

## Tab completion

Press `Tab` (`Action::JiraFieldComplete` — the same binding JIRA/Notes/Todo/Segments/Similar use, dispatched by active mode) after typing `#` or `@` to complete against the Paperless-ngx instance's tag / correspondent catalogues:

| What you type | What it expands to (with `Tab`) |
| --- | --- |
| `<#inv` + `Tab` (unique match, e.g. only `invoice`) | `<#invoice ` — trailing space |
| `<#inv` + `Tab` (ambiguous, e.g. `invoice` and `invalid`) | opens the completion menu; the user picks one |
| `<@ac` + `Tab` (unique match, e.g. `Acme Corp`) | `<@Acme Corp ` — canonical casing preserved verbatim (correspondent names aren't a case-insensitive namespace, unlike notes-mode links) |
| a plain word (no `#`/`@`) + `Tab` | no-op — title search has no completion candidates |

The candidate lists (`tag_names` / `correspondent_names` on `PaperlessState`) come from `/api/tags/` and `/api/correspondents/`, fetched as part of every search (see `PaperlessSearchResult` in [`src/paperless.rs`](../../src/paperless.rs)) — the **full** instance catalogue, not just names appearing on the currently-visible rows. Before the first search completes (e.g. `Tab` pressed the instant `<` mode is entered), both lists are empty and `Tab` surfaces a "no tags/correspondents loaded yet" status message rather than silently doing nothing.

## Selecting a row

`Enter` on a paperless row stages `open "<url>/documents/<id>/details"` (macOS) or `xdg-open "<url>/documents/<id>/details"` (other Unixes) and exits — the same convention as [JIRA mode](jira.md)'s browse-URL staging. The document id is recovered from the row's synthetic negative `HistoryRow::id` (see `paperless::document_to_row`); the URL is rebuilt from `paperless.url` rather than stored on the row.

## Sort order

The list is always sorted newest-first by the document's `added` date (when Paperless-ngx actually inserted it), not `created` (the document's own, often user-edited date) — a 2015 invoice scanned in today sorts as "just added", not "9 years old". This order is fixed: unlike most other modes, paperless mode ignores the `F4` Age/Frequency sort-order toggle (a per-command-string frequency sort has no meaning for documents), the same way [Segments](segments.md) and [Similar](similar.md) mode override the toggle with their own ranking. `Added` is also shown in the details pane alongside correspondent and tags.

## Debounce

The search is debounced 400ms after the last keystroke (`PAPERLESS_DEBOUNCE` in [`src/paperless.rs`](../../src/paperless.rs)), matching the JIRA / files-mode debounce. Leaving `<` mode (or editing the query further before the debounce elapses) cancels any in-flight search — the same cancellation policy as [Files mode](files.md).

## Required configuration

- `paperless.url=<base-url>` (e.g. `https://paperless.example.com`) — used both as the REST API base and the web-UI base for the details-page URL. Trailing slash is stripped.
- `paperless.token=<token>` — sent as `Authorization: Token <token>` (Paperless-ngx's own auth scheme — distinct from JIRA's `Bearer` convention).

Both keys are required; a half-configured pair (only one of the two set) disables the mode with a stderr warning at startup, same pairing policy as `ollama.url` / `ollama.model`. Without both, `<` mode is a no-op and the status bar shows a "paperless not configured" message. Run `smarthistory check --prefix '<'` to verify the backend is reachable and the token is accepted.

## Cross-references

- [JIRA mode — the closest analog (external REST API, browse-URL staging)](jira.md)
- [Files mode — the background-thread / debounce / cancellation pattern paperless mode reuses](files.md)
- [TECHNICAL — per-mode module contract](../../TECHNICAL.md)
