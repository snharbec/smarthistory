//! Paperless-ngx document search for the `<`-prefix TUI mode.
//!
//! A blocking HTTP client that talks to a self-hosted
//! **Paperless-ngx v3** instance's REST API and returns matching
//! documents. Authentication uses an API token (`Authorization:
//! Token <token>` — Paperless-ngx's own scheme, distinct from
//! JIRA's `Bearer` convention in `src/jira.rs`).
//!
//! The search body supports three token kinds, parsed by
//! [`parse_pattern`]:
//! - `#TAG` — the LAST such token; matches documents whose tag
//!   name equals `TAG` (case-insensitive, whole tag).
//! - `@AUTHOR` — the LAST such token; matches documents whose
//!   correspondent name equals `AUTHOR` (case-insensitive, whole
//!   name).
//! - bare words — joined with a space (in typed order); matches
//!   documents whose title *contains* that substring
//!   (case-insensitive).
//!
//! These are sent as Django REST filterset query parameters
//! (`title__icontains`, `tags__name__iexact`,
//! `correspondent__name__iexact` on `GET /api/documents/`) —
//! **not** Paperless-ngx's full-text `query=` search parameter.
//! An earlier version of this module built a `query=` string using
//! the documented `title:`/`tag:`/`correspondent:` advanced-search
//! syntax plus `*wildcard*` substring markers, but real-world
//! testing against a live instance showed the wildcard forms
//! (`*word*`, `word*`, and a bare unscoped `*word*`) all still only
//! matched whole words — apparently a search-index quirk/version
//! difference, not something this client can rely on. The Django
//! filterset lookups are plain ORM `WHERE ... ILIKE '%value%'` /
//! `= value` queries, independent of whatever full-text index
//! Paperless-ngx has built, so they don't have this failure mode.
//!
//! The one-value-per-field nature of these filters means only the
//! LAST `#TAG` / `@AUTHOR` token in a query is honored (Django
//! doesn't AND repeated same-key GET params), and multiple bare
//! title words become one substring (the words joined with a
//! space, in order) rather than independently-ANDed substrings —
//! see `parse_pattern`'s doc comment for the exact semantics.
//!
//! Background-thread orchestration (debounce, cancellation,
//! result channel) mirrors `src/files.rs`'s `FilesState` — see
//! that module's doc comment for the shape rationale. Unlike the
//! files walk, a search failure is meaningful (bad token,
//! unreachable server) so the channel carries a `Result`, closer
//! to `JiraRequest` in `src/tui.rs`.

use crate::tui::state::HistoryRow;
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// How long the paperless-mode search waits after the last
/// keystroke before firing the background request. Matches the
/// JIRA / files debounce (400 ms) — both are simple REST
/// round-trips, not LLM-slow.
pub const PAPERLESS_DEBOUNCE: Duration = Duration::from_millis(400);

/// A single Paperless-ngx document, reduced to the fields the
/// TUI row rendering and details pane care about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaperlessDocument {
    /// Paperless-ngx's numeric document id. Used to build both
    /// the details-view URL (`{url}/documents/{id}/details`) and
    /// the synthetic negative `HistoryRow::id`.
    pub id: i64,
    pub title: String,
    /// Resolved correspondent name (empty if the document has
    /// none, or the correspondent lookup failed).
    pub correspondent: String,
    /// Resolved tag names (empty if the document has none, or
    /// the tag lookup failed).
    pub tags: Vec<String>,
    /// ISO-8601 `created` timestamp, as returned by the API —
    /// the document's own (often user-editable) date, e.g. the
    /// invoice date on a scanned receipt. NOT what the list is
    /// sorted by; see `added` below.
    pub created: String,
    /// ISO-8601 `added` timestamp — when the document was
    /// actually inserted into Paperless-ngx (immutable, set by
    /// the server). `document_to_row` uses THIS as the row's
    /// `timestamp` / sort key, not `created`: a 2015 invoice
    /// scanned in today should sort as "just added", not "9
    /// years old".
    pub added: String,
    /// Plain-text document content (OCR'd body). May be large;
    /// the details pane does its own truncation at render time.
    pub content: String,
}

/// Errors from a Paperless search. Mirrors `jira::JiraError`'s
/// shape: none are fatal, the TUI just surfaces the message and
/// keeps the previous result list.
#[derive(Debug)]
pub enum PaperlessError {
    /// `paperless.url` / `paperless.token` aren't both set in
    /// `~/.config/smarthistory/config`.
    NotConfigured,
    /// The HTTP transport failed (DNS, TLS, connection refused,
    /// timeout).
    Http(String),
    /// The JSON body couldn't be parsed as a documents-search
    /// response.
    Parse(String),
    /// The server returned a non-success HTTP status.
    Api(String),
}

impl std::fmt::Display for PaperlessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PaperlessError::NotConfigured => write!(
                f,
                "paperless not configured: set paperless.url and paperless.token in ~/.config/smarthistory/config"
            ),
            PaperlessError::Http(m) => write!(f, "paperless request failed: {}", m),
            PaperlessError::Parse(m) => write!(f, "paperless parse error: {}", m),
            PaperlessError::Api(m) => write!(f, "paperless API error: {}", m),
        }
    }
}

impl Error for PaperlessError {}

/// Render a `reqwest::Error` with its full cause chain, not just
/// the top-level message. `reqwest::Error`'s own `Display` is
/// terse by design — a connect failure prints only "error
/// sending request for url (...)", with the actual reason
/// (connection refused, DNS failure, TLS error, timeout) buried
/// in `.source()`. Without this, `PaperlessError::Http` surfaced
/// a message that told the user nothing they didn't already
/// know, unlike `RestPaperlessClient::check`'s `ureq`-based
/// probe (whose `Transport` error already prints the full
/// chain).
fn describe_reqwest_error(e: &reqwest::Error) -> String {
    let mut msg = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        source = s.source();
    }
    msg
}

/// Configuration for a Paperless-ngx backend, resolved from
/// `paperless.url=` / `paperless.token=` in the config file (see
/// `Config::parse` in `src/main.rs`). `None` on the `Config` side
/// means the feature is disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperlessConfig {
    /// Base URL of the Paperless-ngx instance, e.g.
    /// `https://paperless.example.com`. Trailing slash stripped
    /// at construction. Used both as the REST API base
    /// (`{url}/api/...`) and the web-UI base
    /// (`{url}/documents/{id}/details`) — the common
    /// single-host deployment.
    pub url: String,
    /// API token, sent as `Authorization: Token <token>`.
    pub token: String,
}

impl PaperlessConfig {
    /// The web-UI URL for a single document's detail page.
    pub fn document_url(&self, id: i64) -> String {
        format!("{}/documents/{}/details", self.url, id)
    }
}

/// The result of a paperless search: the matching documents plus
/// the full tag / correspondent name catalogues. The catalogues
/// are the *instance's* complete set (from `/api/tags/` and
/// `/api/correspondents/`), not just the names that happen to
/// appear on the matched documents — that's what makes `<#` /
/// `<@` Tab completion (see `App::paperless_tab_complete_at_cursor`)
/// work for tags/correspondents that aren't on any currently
/// visible row.
pub struct PaperlessSearchResult {
    pub documents: Vec<PaperlessDocument>,
    pub tag_names: Vec<String>,
    pub correspondent_names: Vec<String>,
}

/// The trait the TUI depends on for Paperless search, so tests
/// can inject canned responses without hitting a real server
/// (same shape as `jira::JiraClient`).
pub trait PaperlessClient: Send + Sync {
    fn search(&self, filters: &PaperlessFilters) -> Result<PaperlessSearchResult, PaperlessError>;
}

/// Parsed paperless-mode search tokens, sent as Django REST
/// filterset query parameters on `GET /api/documents/` (see this
/// module's doc comment for why — `icontains`/`iexact` ORM
/// lookups instead of Paperless-ngx's full-text `query=` search,
/// which turned out not to support substring matching on a real
/// instance despite its documented wildcard syntax).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaperlessFilters {
    /// `title__icontains` value. Empty means "no title filter"
    /// (the param is omitted entirely, not sent as `=`).
    pub title_contains: String,
    /// `tags__name__iexact` value, from the LAST `#TAG` token
    /// typed. `None` means "no tag filter".
    pub tag_exact: Option<String>,
    /// `correspondent__name__iexact` value, from the LAST
    /// `@AUTHOR` token typed. `None` means "no correspondent
    /// filter".
    pub correspondent_exact: Option<String>,
}

/// Parse the paperless-mode query body into [`PaperlessFilters`].
/// Whitespace-separated tokens:
/// - `#TAG` — sets `tag_exact` (overwriting any earlier `#TAG`
///   token — Django's `tags__name__iexact` filter takes exactly
///   one value, so it can't AND multiple tags in a single
///   request; "last one wins" matches this app's existing
///   later-wins config-parsing convention).
/// - `@AUTHOR` — sets `correspondent_exact` (same "last one
///   wins" reasoning).
/// - anything else — appended to `title_contains`, space-joined
///   in typed order. `title__icontains` is a single substring
///   lookup, so `<annual report` searches for the literal
///   substring "annual report" (adjacent, in that order) rather
///   than "title contains annual AND title contains report"
///   independently — a real (API-forced) narrowing from the
///   independently-ANDed-substrings behavior every other
///   multi-word search in this app has, but the only form
///   expressible as one filter value.
///
/// Empty tokens (a bare `#` or `@`) are dropped.
pub fn parse_pattern(pattern: &str) -> PaperlessFilters {
    let mut filters = PaperlessFilters::default();
    let mut title_words: Vec<&str> = Vec::new();
    for tok in pattern.split_whitespace() {
        if let Some(tag) = tok.strip_prefix('#') {
            if !tag.is_empty() {
                filters.tag_exact = Some(tag.to_string());
            }
        } else if let Some(author) = tok.strip_prefix('@') {
            if !author.is_empty() {
                filters.correspondent_exact = Some(author.to_string());
            }
        } else {
            title_words.push(tok);
        }
    }
    filters.title_contains = title_words.join(" ");
    filters
}

/// Real Paperless-ngx backend. Uses `reqwest::blocking` (already
/// a dependency via `note_search` / the JIRA path) with a bounded
/// timeout so a slow/unreachable server can't freeze the
/// background thread indefinitely.
pub struct RestPaperlessClient {
    config: PaperlessConfig,
}

impl RestPaperlessClient {
    pub fn new(config: PaperlessConfig) -> Self {
        RestPaperlessClient { config }
    }

    fn build_client(&self) -> Result<reqwest::blocking::Client, PaperlessError> {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| PaperlessError::Http(describe_reqwest_error(&e)))
    }

    /// Best-effort id→name lookup for `/api/correspondents/` or
    /// `/api/tags/`. Only the first page (`page_size=100`) is
    /// fetched — Paperless-ngx installations rarely have more
    /// correspondents or tags than that, and a failed or
    /// truncated lookup just means some documents show an empty
    /// correspondent / tag name rather than blocking the search
    /// entirely (same "best-effort" policy as
    /// `multiplexer::parse_workspace_labels` for herdr workspace
    /// labels).
    fn fetch_id_name_map(
        &self,
        client: &reqwest::blocking::Client,
        resource: &str,
    ) -> std::collections::HashMap<i64, String> {
        let mut out = std::collections::HashMap::new();
        let url = format!("{}/api/{}/", self.config.url, resource);
        let Ok(resp) = client
            .get(&url)
            .header("Authorization", format!("Token {}", self.config.token))
            .query(&[("page_size", "100")])
            .send()
        else {
            return out;
        };
        let Ok(body) = resp.json::<IdNameResponse>() else {
            return out;
        };
        for entry in body.results {
            out.insert(entry.id, entry.name);
        }
        out
    }
}

#[derive(serde::Deserialize)]
struct IdNameResponse {
    results: Vec<IdNameEntry>,
}

#[derive(serde::Deserialize)]
struct IdNameEntry {
    id: i64,
    name: String,
}

#[derive(serde::Deserialize)]
struct SearchResponse {
    results: Vec<ApiDocument>,
}

#[derive(serde::Deserialize)]
struct ApiDocument {
    id: i64,
    title: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    correspondent: Option<i64>,
    #[serde(default)]
    tags: Vec<i64>,
    #[serde(default)]
    created: String,
    #[serde(default)]
    added: String,
}

/// Build the `GET /api/documents/` query parameters for
/// `filters`. Each is a plain Django ORM lookup (`WHERE ...
/// ILIKE '%value%'` / `= value`), independent of Paperless-ngx's
/// full-text search index — see this module's doc comment for
/// why that index's `query=`/wildcard mechanism was abandoned.
/// A filter is omitted entirely when unset (an empty
/// `title__icontains=` matches everything, which is correct for
/// "no title filter", but there's no reason to send a redundant
/// empty param). Extracted from `RestPaperlessClient::search` so
/// the exact wire params are unit-testable without a live server
/// or network access.
fn filter_query_params(filters: &PaperlessFilters) -> Vec<(&'static str, &str)> {
    let mut params: Vec<(&'static str, &str)> = vec![("page_size", "100")];
    if !filters.title_contains.is_empty() {
        params.push(("title__icontains", &filters.title_contains));
    }
    if let Some(ref tag) = filters.tag_exact {
        params.push(("tags__name__iexact", tag));
    }
    if let Some(ref correspondent) = filters.correspondent_exact {
        params.push(("correspondent__name__iexact", correspondent));
    }
    params
}

impl PaperlessClient for RestPaperlessClient {
    fn search(&self, filters: &PaperlessFilters) -> Result<PaperlessSearchResult, PaperlessError> {
        let client = self.build_client()?;
        let url = format!("{}/api/documents/", self.config.url);
        let params = filter_query_params(filters);
        let resp = client
            .get(&url)
            .header("Authorization", format!("Token {}", self.config.token))
            .query(&params)
            .send()
            .map_err(|e| PaperlessError::Http(describe_reqwest_error(&e)))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            let excerpt: String = body.chars().take(200).collect();
            return Err(PaperlessError::Api(format!("{}: {}", status.as_u16(), excerpt)));
        }
        let parsed: SearchResponse = resp
            .json()
            .map_err(|e| PaperlessError::Parse(e.to_string()))?;

        // Best-effort name resolution — see `fetch_id_name_map`'s
        // doc comment. A failure here just leaves the maps empty;
        // it never turns the whole search into an error.
        let correspondents = self.fetch_id_name_map(&client, "correspondents");
        let tags = self.fetch_id_name_map(&client, "tags");

        let documents = parsed
            .results
            .into_iter()
            .map(|doc| PaperlessDocument {
                id: doc.id,
                title: doc.title,
                correspondent: doc
                    .correspondent
                    .and_then(|id| correspondents.get(&id).cloned())
                    .unwrap_or_default(),
                tags: doc
                    .tags
                    .iter()
                    .filter_map(|id| tags.get(id).cloned())
                    .collect(),
                created: doc.created,
                added: doc.added,
                content: doc.content,
            })
            .collect();
        let mut tag_names: Vec<String> = tags.into_values().collect();
        tag_names.sort();
        let mut correspondent_names: Vec<String> = correspondents.into_values().collect();
        correspondent_names.sort();
        Ok(PaperlessSearchResult {
            documents,
            tag_names,
            correspondent_names,
        })
    }
}

/// Convert an ISO-8601 timestamp (`created` or `added`) to Unix
/// epoch seconds. Returns 0 on any parse failure (empty string,
/// unexpected format) rather than propagating an error.
fn parse_iso8601_epoch(iso: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(iso.trim())
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

/// Build the details-pane text for a document: a small
/// `**label**: value` header (correspondent, tags, added) plus
/// the document content, following the same convention as
/// `JiraIssue`'s `output` field in `src/tui.rs`.
fn build_details(doc: &PaperlessDocument) -> String {
    let none_placeholder = "<none>";
    let correspondent = if doc.correspondent.is_empty() {
        none_placeholder
    } else {
        doc.correspondent.as_str()
    };
    let tags = if doc.tags.is_empty() {
        none_placeholder.to_string()
    } else {
        doc.tags.join(", ")
    };
    let added = if doc.added.is_empty() {
        none_placeholder
    } else {
        doc.added.as_str()
    };
    let mut details = vec![
        format!("**Correspondent**: {}  **Tags**: {}", correspondent, tags),
        format!("**Added**: {}", added),
        "**Content**".to_string(),
    ];
    if doc.content.is_empty() {
        details.push(none_placeholder.to_string());
    } else {
        details.extend(doc.content.lines().map(str::to_string));
    }
    details.join("\n")
}

/// Convert a `PaperlessDocument` to a `HistoryRow`. The document
/// id is negated into a synthetic row id (same convention as
/// `files::walk_dir` / the todo mode's line numbers), so
/// `stage_paperless_selection` recovers it with
/// `row.id.unsigned_abs()`.
///
/// `row.timestamp` is set from `doc.added` (when Paperless-ngx
/// actually inserted the document), NOT `doc.created` (the
/// document's own, often user-edited date) — a 2015 invoice
/// scanned in today should sort as "just added", not "9 years
/// old". `App::build_merged_rows` sorts the paperless-mode list
/// by this timestamp unconditionally (see its `is_paperless_query`
/// early return), so this is the one place that decides the
/// list's order.
pub fn document_to_row(doc: PaperlessDocument) -> HistoryRow {
    let comment = if doc.correspondent.is_empty() && doc.tags.is_empty() {
        String::new()
    } else if doc.correspondent.is_empty() {
        doc.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ")
    } else if doc.tags.is_empty() {
        doc.correspondent.clone()
    } else {
        format!(
            "{} · {}",
            doc.correspondent,
            doc.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ")
        )
    };
    let timestamp = parse_iso8601_epoch(&doc.added);
    let id = -(doc.id.max(1));
    HistoryRow {
        id,
        command: doc.title.clone(),
        directory: String::new(),
        session_id: String::new(),
        exit_code: 0,
        timestamp,
        comment,
        output: build_details(&doc),
        mode: "paperless".to_string(),
        source: "paperless".to_string(),
        ..Default::default()
    }
}

/// The mapped, sorted rows plus the tag / correspondent name
/// catalogues delivered over `PaperlessRequest`'s channel. Kept
/// as its own type (rather than a bare `Vec<HistoryRow>`) so the
/// name catalogues survive the background thread hop for
/// `<#` / `<@` Tab completion — see `PaperlessSearchResult`.
pub struct PaperlessSearchOutcome {
    pub rows: Vec<HistoryRow>,
    pub tag_names: Vec<String>,
    pub correspondent_names: Vec<String>,
}

/// An in-flight paperless-mode search. Same shape as
/// `JiraRequest` in `src/tui.rs`: a background thread sends the
/// result over the channel, the run loop polls it, and the
/// cancelled flag lets a stale search be dropped when the
/// pattern changes mid-flight.
pub struct PaperlessRequest {
    pub receiver: mpsc::Receiver<Result<PaperlessSearchOutcome, PaperlessError>>,
    pub cancelled: Arc<AtomicBool>,
}

/// Aggregated paperless-mode state, owned by `App`. Mirrors
/// `files::FilesState` — see that struct's doc comment for the
/// field-by-field rationale.
pub struct PaperlessState {
    pub debounce_started: Option<std::time::Instant>,
    pub last_pattern: Option<String>,
    pub in_flight: bool,
    pub request: Option<PaperlessRequest>,
    pub rows: Vec<HistoryRow>,
    /// Bumped every time `rows` is replaced by a fresh search
    /// result. See `SegmentsState::rows_version` — same rationale.
    pub rows_version: u64,
    /// Full tag name catalogue from the last successful search
    /// (`/api/tags/`, not just tags on currently-visible rows).
    /// Read by `App::paperless_tab_complete_at_cursor` for `<#`
    /// completion. Empty until the first search completes.
    pub tag_names: Vec<String>,
    /// Full correspondent name catalogue from the last
    /// successful search. Read by
    /// `App::paperless_tab_complete_at_cursor` for `<@`
    /// completion. Empty until the first search completes.
    pub correspondent_names: Vec<String>,
}

impl PaperlessState {
    pub fn new() -> Self {
        PaperlessState {
            debounce_started: None,
            last_pattern: None,
            in_flight: false,
            request: None,
            rows: Vec::new(),
            rows_version: 0,
            tag_names: Vec::new(),
            correspondent_names: Vec::new(),
        }
    }

    /// The canonical pattern for "is this the same search we
    /// just ran?" comparisons — the query body after the leading
    /// prefix char, trimmed.
    pub fn current_pattern(query: &str, prefix: char) -> String {
        let body = if query.starts_with(prefix) {
            &query[prefix.len_utf8()..]
        } else {
            query
        };
        body.trim().to_string()
    }

    pub fn has_results_for(&self, pattern: &str) -> bool {
        self.last_pattern.as_deref() == Some(pattern)
    }
}

impl Default for PaperlessState {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::debounce::Cancellable for PaperlessRequest {
    fn cancelled_flag(&self) -> &Arc<AtomicBool> {
        &self.cancelled
    }
}

impl crate::debounce::Debounced for PaperlessState {
    type Request = PaperlessRequest;
    fn debounce_started(&mut self) -> &mut Option<std::time::Instant> {
        &mut self.debounce_started
    }
    fn last_pattern(&mut self) -> &mut Option<String> {
        &mut self.last_pattern
    }
    fn in_flight(&mut self) -> &mut bool {
        &mut self.in_flight
    }
    fn request(&mut self) -> &mut Option<PaperlessRequest> {
        &mut self.request
    }
}

/// Spawn a background thread that runs `client.search(...)` for
/// `pattern` and sends the result over the returned request's
/// channel. Used by `App::paperless_maybe_autocall`.
pub fn spawn_search(config: PaperlessConfig, pattern: String) -> PaperlessRequest {
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let filters = parse_pattern(&pattern);
    std::thread::spawn(move || {
        let client = RestPaperlessClient::new(config);
        let result = client.search(&filters).map(|result| {
            let mut rows: Vec<HistoryRow> =
                result.documents.into_iter().map(document_to_row).collect();
            // Always newest-added-first, independent of
            // whatever order the Paperless-ngx API returned
            // (and independent of the TUI's Age/Frequency
            // sort-order toggle — see the `is_paperless_query`
            // early return in `App::build_merged_rows`).
            rows.sort_by_key(|r| std::cmp::Reverse(r.timestamp));
            PaperlessSearchOutcome {
                rows,
                tag_names: result.tag_names,
                correspondent_names: result.correspondent_names,
            }
        });
        if !cancelled_clone.load(Ordering::Relaxed) {
            let _ = tx.send(result);
        }
    });
    PaperlessRequest {
        receiver: rx,
        cancelled,
    }
}

#[cfg(test)]
mod tests;
