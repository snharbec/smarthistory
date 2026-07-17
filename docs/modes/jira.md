# JIRA mode (`-`)

| Default prefix | `-` |
| --- | --- |
| Configurable | `prefix.jira=<char>` |

JIRA mode searches issues on a self-hosted JIRA instance via the REST API. Issues matching the query are listed with their key, summary, and status. Selecting an issue opens its browse URL in the system browser; `Ctrl-M-s` downloads it as a local markdown note.

## What it does

- `-` (empty) — every issue visible to the API token, scoped to the default project (if configured) or project-wide.
- `-@me @project` — every issue assigned to you, with the `KR` label, ANDed. (`@project` is a user-defined JQL fragment from `jira.search.project=...` in the config.)
- `-JOB-1234` — every issue that matches the key (a single issue).
- `-status=Open priority=Blocker crash` — every issue with status=Open *and* priority=Blocker *and* "crash" in the description / summary (free-text).
- The first text column is the issue key; the second is the summary; the third is the status / priority metadata.

## Selecting a row

- `Enter` on a JIRA row stages `open "<jira-server>/browse/<KEY>"` (macOS) or `xdg-open "<jira-server>/browse/<KEY>"` (other Unixes) and exits. The TUI is gone before the browser opens.
- `Ctrl-O` (Show output) opens the captured-output overlay, which for a JIRA row fires the **background comments fetch** (a separate API call to `/rest/api/2/issue/{key}/comment`) and shows the description + every comment sorted newest-first. This is the primary way to read a JIRA issue inside the TUI.
- `Ctrl-M-s` (Download JIRA issue as note) stages `note_search jira-issue <KEY>` and exits. The issue is downloaded as a local markdown note (via the `note_search` jira-issue command) and becomes searchable in [`@` (Notes) mode](notes.md).
- `Ctrl-]` (Smart open) opens the issue's URL in the browser **in the background** — same action as `Enter`, but spawned as a detached child process so the TUI stays open. The default key is `C-]`, an ASCII control char every terminal emits reliably.
- `Tab` (JIRA field complete) is a sub-action for JQL-style queries: in the middle of a `field=` token, pressing `Tab` completes the field name from the list of fields returned by the JIRA `/rest/api/2/field` endpoint.

## Special tokens

The body is parsed by [`src/jira.rs`](../src/jira.rs) using a shared token language. Whitespace separates tokens; each token falls into one of the following categories.

| Pattern | Category | Behaviour |
| --- | --- | --- |
| `^[A-Z]+-[0-9]+$` (e.g. `PROJ-1234`) | issue key | `key = PROJ-1234` (or `key in (..., ...)` for multiple) |
| `^(\w+)=(.*)$` (e.g. `status=Open`) | field=value | `field = "value"` (quoted, JQL-escaped) |
| `@me` (built-in alias) | assignee filter | `assignee = currentUser()` |
| `@today` / `@week` / `@month` (built-in) | date filter | `updated >= "<today-1d>"` / `"<today-7d>"` / `"<today-31d>"` |
| `@<name>` with a defined fragment in config | user-defined JQL fragment | spliced verbatim, wrapped in parens |
| anything else | free text | `(description ~ "..." OR summary ~ "...")` |

**Important**: user-defined fragments (the `@<name>` row) are only expanded when the token starts with `@`. Typing `project` (no `@`) is a free-text search for the description / summary, NOT a fragment expansion. The `@` is a deliberate invocation. See the [JIRA fragment behaviour](../../TECHNICAL.md) for the full semantics.

## Tab completion (JQL field names + aliases / fragments)

Press `Tab` (the default `Action::JiraFieldComplete` key) while typing in `-` mode to open a completion menu for the token under the cursor. The completion kind is decided by the character immediately before the alphanumeric word: `@` triggers alias / fragment completion; anything else triggers JQL field-name completion. The implementation lives in `jira_field_complete_at_cursor` in [`src/tui.rs`](../../src/tui.rs), with the candidate-list builders in [`src/jira.rs`](../../src/jira.rs).

### JQL field-name completion

Triggered when the cursor is positioned after an alphanumeric word **without** a leading `@`. The static list comes from the `JIRA_FIELDS` table in `src/jira.rs` (the JIRA REST API's `/rest/api/2/field` is the source of truth for the full list, but the TUI ships a curated subset that covers the fields a typical JQL query actually uses).

The full list (in alphabetical order):

| | | | |
| --- | --- | --- | --- |
| `affectedVersion` | `assignee` | `category` | `component` |
| `created` | `creator` | `description` | `due` |
| `duedate` | `environment` | `epic` | `fixVersion` |
| `issue` | `issueKey` | `issues` | `issuetype` |
| `key` | `label` | `labels` | `lastViewed` |
| `level` | `originalEstimate` | `parent` | `priority` |
| `project` | `rank` | `remainingEstimate` | `reporter` |
| `resolution` | `resolved` | `sprint` | `status` |
| `statusCategory` | `storyPoints` | `summary` | `text` |
| `timeSpent` | `type` | `updated` | `voter` |
| `version` | `votes` | `watcher` | `watchers` |
| `workRatio` | | | |

Examples:

| What you type | What it expands to (with `Tab`) |
| --- | --- |
| `la` + `Tab` | `lab` (ambiguous: `label`, `labels`) |
| `lab` + `Tab` (still ambiguous) | opens the completion menu with `label` and `labels`; the user picks one |
| `label` + `Tab` (unique match) | `labels=` — the trailing `=` is appended so the user can immediately type the value |
| `stat` + `Tab` | `status=` (unique match — `status`, `statusCategory` are the candidates but `stat` is a prefix of both, so the LCP `status` is returned, and because `status` is the full prefix → unique → trailing `=` is appended) |
| `statuscateg` + `Tab` | `statusCategory=` |
| `xyz` + `Tab` | no-op + status message `jira-field-complete: no field starts with "xyz"` |

The matching is case-insensitive (`Stat` matches `status`); the returned completion preserves the canonical casing from the `JIRA_FIELDS` table. The longest-common-prefix (LCP) is returned when multiple fields share the prefix; `Tab` again on the LCP opens the completion menu so the user can pick from the candidates rather than typing the disambiguating character manually. This is standard readline / bash completion behaviour and is the least-surprising default for users who already know shell completion.

### Alias / fragment completion

Triggered when the cursor is positioned immediately after `@` followed by an alphanumeric word. The completion list is the four built-in aliases (`me`, `today`, `week`, `month`) merged with every key in the user-supplied `fragments` map (loaded from `jira.search.<name>=...` config entries). Matching is case-insensitive on the prefix; the returned expansion preserves the canonical alias / fragment name (which is case-sensitive in the config — `jira.search.MyProject=...` is distinct from `jira.search.myproject=...`).

Examples:

| What you type | What it expands to (with `Tab`) |
| --- | --- |
| `@m` + `Tab` | `@me` — unique match, trailing space so the user can type the next token |
| `@tod` + `Tab` | `@today` |
| `@` (bare) + `Tab` | no-op + status message `jira-alias-complete: no alias starts with ""` (a bare `@` is too short to match anything) |
| `@proj` + `Tab` (with `jira.search.project=...` defined) | `@project` — the user's config fragment, preserved exactly |
| `@p` + `Tab` (multiple fragments starting with `p`) | opens the completion menu with every `p*` alias / fragment, the user picks one |
| `@` + `Tab` (with no aliases / fragments) | no-op + status message `jira-alias-complete: no alias starts with ""` |

The expansion includes the `@` in the replacement range — the user typed `@m` and the cursor lands after `@me` (with the leading `@` part of the expansion, not re-typed by the user). The case-folding matches the user's prefix against the canonical-cased names; the inserted expansion preserves the canonical casing.

### Cross-mode behaviour

The same `Tab` binding (`Action::JiraFieldComplete`) is also the tag / link completion key in [`@` (Notes) mode](notes.md) and [`!` (Todo) mode](todo.md). The dispatch site checks the active mode and routes to the right completion function:

| Active mode | Tab dispatches to | Completes |
| --- | --- | --- |
| `-` (JIRA) | `jira_field_complete_at_cursor` | JQL field names (and `@`-prefixed aliases / fragments) |
| `@` (Notes) and `!` (Todo) | `notes_tab_complete_at_cursor` | Note tags (after `#`) and link names (after `@`) |
| every other mode | no-op | — |

`Tab` in the add-entry dialog (`Ctrl-S` / `Ctrl-F6` to open a new session / host) is reserved for *field-next* INSIDE the dialog, so the two paths never collide.

## Debounce

The search is debounced: 400ms after the last keystroke (fast) plus a 3s idle safety net. A 1-second pause after typing still fires the search; the JQL is built from the current body when the debounce fires.

A space inside the body forces the search to fire immediately (no debounce). The space acts as a "I'm done with this word" signal.

## Required configuration

- `JIRA_SERVER` (e.g. `https://jira.example.com`) — the REST API base.
- `JIRA_API_TOKEN` — the bearer token.
- `JIRA_URL` (or fallback to `JIRA_SERVER`) — the browse URL prefix (the server URL + `/browse`).
- `JIRA_PROJECT` (optional) — the default project to scope empty queries to.

The first three are required for the mode to function; the last is optional (an empty `JIRA_PROJECT` = project-wide queries). Without all required vars, `-` mode is a no-op and the status bar shows a "JIRA not configured" message.

## JIRA-mode tags

The `[JIRA-mode tags]` sub-section of the help overlay lists every built-in alias and a placeholder for user-defined fragments. The full list is in [`src/tui/render.rs`](../src/tui/render.rs) (`build_help_lines`).

## Cross-references

- [Notes mode — `Ctrl-M-s` downloads the selected issue as a local note](notes.md)
- [TECHNICAL — JIRA mode implementation details](../../TECHNICAL.md#jira-mode)
- [Configuration — env-var precedence](../../README.md#configuration)
