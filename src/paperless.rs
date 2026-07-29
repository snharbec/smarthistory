//! Paperless-ngx document search for the `<`-prefix TUI mode.
//!
//! A blocking HTTP client that talks to a self-hosted
//! **Paperless-ngx v3** instance's REST API and returns matching
//! documents. Authentication uses an API token (`Authorization:
//! Token <token>` — Paperless-ngx's own scheme, distinct from
//! JIRA's `Bearer` convention in `src/jira.rs`).
//!
//! The search body supports three token kinds, parsed by
//! [`build_query`]:
//! - `#TAG` — matches documents carrying that tag.
//! - `@AUTHOR` — matches documents whose correspondent name
//!   contains `AUTHOR`.
//! - a bare word — matches the document title.
//!
//! These map onto Paperless-ngx's own advanced search syntax
//! (`tag:`, `correspondent:`, `title:` — see the "Basic Usage /
//! Searching" section of the Paperless-ngx docs), so the built
//! query string is sent verbatim as the `query` parameter of
//! `GET /api/documents/`.
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
    fn search(&self, query: &str) -> Result<PaperlessSearchResult, PaperlessError>;
}

/// Split the paperless-mode query pattern into a Paperless-ngx
/// advanced-search query string. Whitespace-separated tokens:
/// - `#TAG` → `tag:TAG`
/// - `@AUTHOR` → `correspondent:AUTHOR`
/// - anything else → `title:WORD`
///
/// Empty tokens (a bare `#` or `@`) are dropped. Values
/// containing characters outside `[A-Za-z0-9_-]` are
/// double-quoted so Paperless-ngx's query parser doesn't choke
/// on embedded punctuation.
pub fn build_query(pattern: &str) -> String {
    pattern
        .split_whitespace()
        .filter_map(|tok| {
            if let Some(tag) = tok.strip_prefix('#') {
                (!tag.is_empty()).then(|| format!("tag:{}", quote_if_needed(tag)))
            } else if let Some(author) = tok.strip_prefix('@') {
                (!author.is_empty()).then(|| format!("correspondent:{}", quote_if_needed(author)))
            } else {
                Some(format!("title:{}", quote_if_needed(tok)))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Wrap `value` in double quotes when it contains any character
/// outside the safe bareword set, so it survives Paperless-ngx's
/// query tokenizer as a single value.
fn quote_if_needed(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('"', "'"))
    }
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

impl PaperlessClient for RestPaperlessClient {
    fn search(&self, query: &str) -> Result<PaperlessSearchResult, PaperlessError> {
        let client = self.build_client()?;
        let url = format!("{}/api/documents/", self.config.url);
        let resp = client
            .get(&url)
            .header("Authorization", format!("Token {}", self.config.token))
            .query(&[("query", query), ("page_size", "25")])
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

/// Spawn a background thread that runs `client.search(...)` for
/// `pattern` and sends the result over the returned request's
/// channel. Used by `App::paperless_maybe_autocall`.
pub fn spawn_search(config: PaperlessConfig, pattern: String) -> PaperlessRequest {
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let query = build_query(&pattern);
    std::thread::spawn(move || {
        let client = RestPaperlessClient::new(config);
        let result = client.search(&query).map(|result| {
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
mod tests {
    use super::*;

    #[test]
    fn build_query_plain_word_is_title() {
        assert_eq!(build_query("invoice"), "title:invoice");
    }

    #[test]
    fn build_query_tag_token() {
        assert_eq!(build_query("#work"), "tag:work");
    }

    #[test]
    fn build_query_author_token() {
        assert_eq!(build_query("@acme"), "correspondent:acme");
    }

    #[test]
    fn build_query_mixed_tokens_join_with_space() {
        assert_eq!(
            build_query("invoice #work @acme"),
            "title:invoice tag:work correspondent:acme"
        );
    }

    #[test]
    fn build_query_quotes_tokens_with_special_chars() {
        assert_eq!(build_query("#foo/bar"), "tag:\"foo/bar\"");
    }

    #[test]
    fn build_query_drops_empty_tag_and_author_tokens() {
        assert_eq!(build_query("# @ invoice"), "title:invoice");
    }

    #[test]
    fn build_query_empty_pattern_is_empty_string() {
        assert_eq!(build_query(""), "");
        assert_eq!(build_query("   "), "");
    }

    #[test]
    fn document_to_row_negates_id() {
        let doc = PaperlessDocument {
            id: 42,
            title: "Annual report".to_string(),
            correspondent: String::new(),
            tags: Vec::new(),
            created: String::new(),
            added: String::new(),
            content: String::new(),
        };
        let row = document_to_row(doc);
        assert_eq!(row.id, -42);
        assert_eq!(row.command, "Annual report");
    }

    #[test]
    fn document_to_row_builds_comment_from_correspondent_and_tags() {
        let doc = PaperlessDocument {
            id: 1,
            title: "Invoice".to_string(),
            correspondent: "Acme Corp".to_string(),
            tags: vec!["work".to_string(), "2024".to_string()],
            created: String::new(),
            added: String::new(),
            content: String::new(),
        };
        let row = document_to_row(doc);
        assert_eq!(row.comment, "Acme Corp · #work #2024");
    }

    #[test]
    fn document_to_row_comment_empty_when_no_metadata() {
        let doc = PaperlessDocument {
            id: 1,
            title: "Invoice".to_string(),
            correspondent: String::new(),
            tags: Vec::new(),
            created: String::new(),
            added: String::new(),
            content: String::new(),
        };
        let row = document_to_row(doc);
        assert_eq!(row.comment, "");
    }

    #[test]
    fn document_url_appends_details_path() {
        let cfg = PaperlessConfig {
            url: "https://paperless.example.com".to_string(),
            token: "secret".to_string(),
        };
        assert_eq!(
            cfg.document_url(42),
            "https://paperless.example.com/documents/42/details"
        );
    }

    #[test]
    fn parse_iso8601_epoch_parses_rfc3339() {
        assert_eq!(parse_iso8601_epoch("2024-01-15T10:30:00+00:00"), 1705314600);
    }

    #[test]
    fn parse_iso8601_epoch_empty_is_zero() {
        assert_eq!(parse_iso8601_epoch(""), 0);
    }

    #[test]
    fn document_to_row_uses_added_not_created_for_timestamp() {
        // A document whose nominal `created` date is old but was
        // only just scanned/inserted (`added`) should sort as
        // recent, not as 9 years old.
        let doc = PaperlessDocument {
            id: 1,
            title: "Old invoice".to_string(),
            correspondent: String::new(),
            tags: Vec::new(),
            created: "2015-01-01T00:00:00+00:00".to_string(),
            added: "2024-01-15T10:30:00+00:00".to_string(),
            content: String::new(),
        };
        let row = document_to_row(doc);
        assert_eq!(row.timestamp, 1705314600);
    }
}
