//! JIRA issue search for the `-`-prefix TUI mode.
//!
//! This module is the smarthistory analogue of `note_search`'s
//! `src/jira.rs`: a blocking HTTP client that talks to a
//! **self-hosted JIRA** instance's REST API v2 and returns
//! issues. smarthistory uses it for *live, in-TUI* search
//! (the `-` prefix), whereas `note_search` uses its copy to
//! *import* issues to markdown files — hence the separate,
//! lighter implementation here (no file writing, no
//! pagination loop: the TUI only ever shows ~50 rows).
//!
//! Authentication uses a bearer token (`JIRA_API_TOKEN`)
//! against the API server (`JIRA_SERVER`), and the
//! **browse** URL (`JIRA_URL`) is kept separate because the
//! user-visible ticket URL may differ from the API host
//! (e.g. an internal hostname for the API, a public
//! CDN/proxy for browsing).

use std::error::Error;

/// A single JIRA issue, reduced to the fields the TUI row
/// rendering and details pane care about. Built from the
/// REST `/search` response (only those `fields` are
/// requested, so the parsing is strict on shape but loose
/// on presence — every non-key field falls back to empty).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JiraIssue {
    /// Issue key, e.g. `PROJ-123`.
    pub key: String,
    /// One-line summary (the issue's `summary` field).
    pub summary: String,
    /// Workflow status name, e.g. `In Progress`.
    pub status: String,
    /// Issue-type name, e.g. `Bug`, `Task`.
    pub issuetype: String,
    /// Priority name, e.g. `High`.
    pub priority: String,
    /// Assignee display name (empty if unassigned).
    pub assignee: String,
    /// ISO-8601 `updated` timestamp, e.g.
    /// `2024-06-30T19:14:39.000+0000`. Used for sorting
    /// (newest-updated first) and the details pane.
    pub updated: String,
    /// Due date in JIRA's `YYYY-MM-DD` form (date-only,
    /// no time component). Empty when the issue has
    /// no due date set. JIRA uses this field for
    /// deadline-style tracking on tasks; epics use a
    /// different `duedate` field on the parent, so
    /// sub-task queries may return many issues with
    /// empty `due`. Populated in the details-pane
    /// preview (per the user spec, "Due" is one of
    /// the five rendered attributes).
    pub due: String,
    /// Plain-text rendering of the issue's `description`
    /// field. JIRA's REST v2 returns the description as
    /// an [Atlassian Document Format][adf] JSON object
    /// (a tree of `doc`/`paragraph`/`text` nodes); we
    /// walk the tree and concatenate the readable text.
    /// Empty when the issue has no description.
    /// The full text is stored verbatim — the preview
    /// pane and the show-output overlay do their own
    /// line-budget truncation at render time.
    ///
    /// [adf]: https://developer.atlassian.com/cloud/jira/platform/apis/document/structure/
    pub description: String,
}

/// A single JIRA comment, flattened to the fields
/// the TUI cares about. Comments are fetched on
/// demand when the user opens the show-output
/// overlay for a JIRA row (separate from the
/// search-time `JiraIssue` fetch).
///
/// The comment body uses the same Atlassian
/// Document Format as the issue description, so
/// the same `extract_adf_text` helper is reused
/// to produce a flat plain-text rendering. We
/// keep the body as `String` (not the raw ADF
/// JSON value) because the rendering is the
/// only thing the TUI does with it — the user
/// can't see the ADF structure in the overlay
/// anyway.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JiraComment {
    /// Display name of the comment's author,
    /// e.g. `Alice Smith`. Empty when the
    /// comment is anonymous (rare but possible
    /// for system comments on workflow
    /// transitions).
    pub author: String,
    /// Plain-text rendering of the comment body.
    /// Empty when the comment has no body or the
    /// body is empty (some JIRA bots post
    /// "no content" comments as workflow
    /// markers).
    pub body: String,
    /// ISO-8601 `created` timestamp, e.g.
    /// `2024-06-30T19:14:39.000+0000`. Used for
    /// sorting (newest first) and the heading
    /// display in the overlay.
    pub created: String,
    /// ISO-8601 `updated` timestamp. Falls back
    /// to `created` when the comment has never
    /// been edited (JIRA leaves `updated` equal
    /// to `created` on a fresh comment, but the
    /// field may also be absent for system
    /// comments). Kept for future use; the
    /// current sort key is `created`.
    pub updated: String,
    /// JIRA's comment ID, used as a stable
    /// tie-breaker when two comments share the
    /// same `created` timestamp (rare but
    /// possible for batch-imported comments).
    pub id: String,
}

/// Errors from a JIRA search. Each variant maps to a
/// user-visible status message; none are fatal (the TUI
/// stays usable — the list just stays empty or stale).
#[derive(Debug)]
pub enum JiraError {
    /// `JIRA_SERVER` or `JIRA_API_TOKEN` is unset — the
    /// user hasn't configured JIRA access yet.
    NotConfigured,
    /// The HTTP transport failed (DNS, TLS, connection
    /// refused, timeout). The wrapped string is the
    /// underlying error message.
    Http(String),
    /// The JSON body couldn't be parsed as a JIRA search
    /// response (the server returned an unexpected shape,
    /// e.g. an error envelope or a non-v2 endpoint).
    Parse(String),
    /// The JIRA server returned a non-success HTTP status.
    /// The wrapped string is `{status}: {body_excerpt}`.
    Api(String),
}

impl std::fmt::Display for JiraError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JiraError::NotConfigured => {
                write!(f, "JIRA not configured: set JIRA_SERVER and JIRA_API_TOKEN")
            }
            JiraError::Http(m) => write!(f, "JIRA request failed: {}", m),
            JiraError::Parse(m) => write!(f, "JIRA parse error: {}", m),
            JiraError::Api(m) => write!(f, "JIRA API error: {}", m),
        }
    }
}

impl Error for JiraError {}

/// The trait the TUI depends on for JIRA search, so tests
/// can inject canned responses without hitting a real
/// server (same shape as `llm::LlmClient`). The only
/// method runs a JQL query and returns matching issues,
/// newest-updated first (the caller sorts again by parsed
/// epoch is unnecessary — the JQL already ends in
/// `ORDER BY updated DESC`, so the server returns them in
/// the right order; we preserve it).
pub trait JiraClient: Send + Sync {
    fn search(&self, jql: &str) -> Result<Vec<JiraIssue>, JiraError>;

    /// Fetch the comments for a single issue by key.
    /// Called when the user opens the show-output
    /// overlay on a JIRA row — the comments aren't
    /// fetched at search time because most JIRA
    /// issues have many comments and a search-result
    /// row only needs the issue metadata. The
    /// returned list is sorted newest-first by the
    /// caller (JIRA's `comment` endpoint returns
    /// comments in `created` ascending order, so the
    /// TUI reverses them on the way in).
    ///
    /// Returning a `Result` rather than an `Option`
    /// so a network failure can be distinguished
    /// from "the issue has no comments". The TUI
    /// shows the error as a status message and
    /// doesn't open the overlay.
    fn fetch_comments(&self, key: &str) -> Result<Vec<JiraComment>, JiraError>;

    /// Post a new comment to a JIRA issue.
    /// Called when the user saves the
    /// `edit-comment` buffer on a JIRA row
    /// (Ctrl-E on a JIRA issue opens the
    /// buffer in JIRA-mode; Enter on save
    /// fires this method). The `body` is the
    /// plain-text comment as the user typed
    /// it; the implementation wraps it in
    /// the minimal Atlassian Document Format
    /// envelope that JIRA's REST v2 accepts.
    ///
    /// The method returns `Ok(())` on a
    /// successful POST (HTTP 201 from JIRA)
    /// and `Err(JiraError::...)` on any
    /// failure (network, auth, validation,
    /// etc.). The TUI surfaces the result as
    /// a status message and either clears
    /// the buffer (on success) or preserves
    /// it (on failure, so the user can
    /// retry without retyping).
    ///
    /// `body` is `&str` rather than the
    /// pre-built ADF JSON because the test
    /// fake (and any other fake) shouldn't
    /// have to reimplement the ADF wrapping.
    /// Centralising the wrapping in
    /// `RestJiraClient` keeps the fake
    /// minimal and ensures all clients
    /// produce the same wire format.
    fn add_comment(&self, key: &str, body: &str) -> Result<(), JiraError>;
}

/// Configuration read from the environment. `None` means
/// JIRA access isn't configured (the TUI shows a status
/// message and an empty list rather than erroring).
#[derive(Debug, Clone)]
pub struct JiraConfig {
    /// Base URL for the REST API, e.g. `https://jira.internal`.
    /// Trailing slash is stripped at construction.
    pub server: String,
    /// Bearer token (the `JIRA_API_TOKEN` env var).
    pub token: String,
    /// Base URL for *browsing* a ticket, e.g.
    /// `https://jira.company.com/browse`. A ticket key is
    /// appended as `{url}/{key}`.
    #[allow(dead_code)]
    pub url: String,
    /// The standard project key (the `JIRA_PROJECT` env
    /// var) used for the default query when the user
    /// enters `-` mode with an empty body. Currently the
    /// live query path reads `JIRA_PROJECT` directly (in
    /// `App::jira_build_query`) so the query builder works
    /// in tests without full env config; this field keeps
    /// the resolved snapshot on the config for any future
    /// consumer (e.g. a `JiraClient::default_query` impl).
    #[allow(dead_code)]
    pub project: Option<String>,
    /// Maximum number of results per search request. Read from
    /// `JIRA_MAX_RESULTS`; defaults to 5. A higher value may
    /// cause timeouts on large instances with broad queries.
    pub max_results: u32,
    /// Path to a PKCS#12 (p12/pfx) client certificate file,
    /// read from `JIRA_HOST_CERTIFICATE`. When set, the
    /// certificate is presented to the JIRA server during
    /// TLS handshake (mutual TLS / client-cert auth).
    pub certificate_path: Option<std::path::PathBuf>,
    /// Password for the PKCS#12 certificate file, read from
    /// `JIRA_HOST_CERTIFICATE_PASSWORD`. Ignored when
    /// `certificate_path` is `None`.
    pub certificate_password: Option<String>,
    /// Path to a CA certificate file (PEM or DER format) used
    /// to verify the JIRA server's TLS certificate, read from
    /// `JIRA_CA_CERTIFICATE`. Useful when the JIRA server uses
    /// an internal CA not in the system trust store.
    pub ca_certificate_path: Option<std::path::PathBuf>,
}

impl JiraConfig {
    /// Read the JIRA config from the environment. Returns
    /// `None` when either `JIRA_SERVER` or
    /// `JIRA_API_TOKEN` is unset (the two hard
    /// requirements for any API call). `JIRA_URL` falls
    /// back to `JIRA_SERVER` when unset (common case:
    /// the API and browse URLs share a host), and
    /// `JIRA_PROJECT` is genuinely optional (the empty-
    /// body query degrades to a server-wide
    /// `ORDER BY updated DESC`).
    pub fn from_env() -> Option<Self> {
        let server = std::env::var("JIRA_SERVER").ok()?;
        let token = std::env::var("JIRA_API_TOKEN").ok()?;
        if server.trim().is_empty() || token.trim().is_empty() {
            return None;
        }
        let url = std::env::var("JIRA_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| server.trim_end_matches('/').to_string());
        let project = std::env::var("JIRA_PROJECT")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let max_results = std::env::var("JIRA_MAX_RESULTS")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(5);
        let certificate_path = std::env::var("JIRA_HOST_CERTIFICATE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from);
        let certificate_password = std::env::var("JIRA_HOST_CERTIFICATE_PASSWORD")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let ca_certificate_path = std::env::var("JIRA_CA_CERTIFICATE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from);
        Some(JiraConfig {
            server: server.trim_end_matches('/').to_string(),
            token,
            url,
            project,
            max_results,
            certificate_path,
            certificate_password,
            ca_certificate_path,
        })
    }

    /// The browse URL for a single issue key:
    /// `{server}/browse/{key}`. This matches the standard
    /// JIRA browse path pattern. The `JIRA_URL` env var is
    /// still read for backwards compatibility but the
    /// default (when `JIRA_URL` is unset) now includes the
    /// `/browse/` path segment.
    pub fn browse_url(&self, key: &str) -> String {
        format!("{}/browse/{}", self.server, key)
    }
}

/// A real `reqwest`-based client. Constructed per-search
/// (cheap — `reqwest::blocking::Client` reuses a
/// connection pool internally) with a bounded timeout so
/// a slow JIRA server can't freeze the TUI's background
/// thread indefinitely.
pub struct RestJiraClient {
    config: JiraConfig,
}

impl RestJiraClient {
    pub fn new(config: JiraConfig) -> Self {
        RestJiraClient { config }
    }

    /// Build a configured `reqwest::blocking::Client`
    /// for REST v2 calls. The TLS setup (CA
    /// certificate, optional mTLS identity,
    /// timeout) is identical across `search`,
    /// `fetch_comments`, and `add_comment`; this
    /// helper centralises the construction so the
    /// three methods can't drift.
    ///
    /// Returns `Err(JiraError::Http)` if the
    /// underlying TLS files can't be read or
    /// parsed. The error message includes the
    /// file path so the user can identify which
    /// cert / key is misconfigured.
    ///
    /// This is an inherent method (not a trait
    /// method) so the `JiraClient` trait stays
    /// minimal. The fake client in the test
    /// module doesn't need this helper because
    /// the test seam is the trait, not the
    /// concrete `RestJiraClient`.
    fn build_blocking_client(&self) -> Result<reqwest::blocking::Client, JiraError> {
        use reqwest::blocking::Client;
        let client_builder = Client::builder().timeout(std::time::Duration::from_secs(15));
        let client_builder = match &self.config.ca_certificate_path {
            Some(path) => {
                let der = std::fs::read(path).map_err(|e| {
                    JiraError::Http(format!(
                        "failed to read CA certificate '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
                let ca = reqwest::Certificate::from_pem(&der)
                    .or_else(|_| reqwest::Certificate::from_der(&der))
                    .map_err(|e| {
                        JiraError::Http(format!(
                            "failed to parse CA certificate '{}': {}",
                            path.display(),
                            e
                        ))
                    })?;
                client_builder.add_root_certificate(ca)
            }
            None => client_builder,
        };
        let client = match &self.config.certificate_path {
            Some(path) => {
                let der = std::fs::read(path).map_err(|e| {
                    JiraError::Http(format!(
                        "failed to read certificate '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
                let password = self.config.certificate_password.as_deref().unwrap_or("");
                let identity = reqwest::Identity::from_pkcs12_der(&der, password).map_err(|e| {
                    JiraError::Http(format!(
                        "failed to parse certificate '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
                client_builder
                    .identity(identity)
                    .build()
                    .map_err(|e| JiraError::Http(e.to_string()))?
            }
            None => client_builder
                .build()
                .map_err(|e| JiraError::Http(e.to_string()))?,
        };
        Ok(client)
    }
}

impl JiraClient for RestJiraClient {
    fn search(&self, jql: &str) -> Result<Vec<JiraIssue>, JiraError> {
        use reqwest::blocking::Client;
        // The fields requested are exactly those `JiraIssue`
        // carries — no more, no less — so the response stays
        // small (descriptions can be large; we deliberately
        // do NOT request `description` to keep payloads
        // light, since the TUI row only needs the summary +
        // status + metadata; the user gets the full
        // description by opening the ticket in the browser).
        //
        // Update: the user-facing spec added `description` to
        // the details-pane preview, so we now request it.
        // The TUI truncates the extracted text to
        // `MAX_DESCRIPTION_CHARS` so a large body doesn't
        // blow out the preview pane.
        let url = format!(
            "{}/rest/api/2/search?jql={}&maxResults={}&fields=key,summary,status,issuetype,priority,assignee,updated,duedate,description",
            self.config.server,
            // `jql` must be URL-encoded. `reqwest`'s
            // `query` form would do this, but the JIRA v2
            // `search` endpoint historically expects the
            // `jql` query parameter as a raw (already-encoded)
            // string, so we encode manually with a tiny
            // percent-encoder (same one `note_search`'s
            // `jira.rs` uses, to match its proven behaviour).
            urlencoding::encode(jql),
            self.config.max_results,
        );
        let client_builder = Client::builder().timeout(std::time::Duration::from_secs(15));
        // If a CA certificate is configured, load it and add
        // it as a trusted root so the JIRA server's TLS cert
        // (signed by an internal CA) can be verified.
        let client_builder = match &self.config.ca_certificate_path {
            Some(path) => {
                let der = std::fs::read(path).map_err(|e| {
                    JiraError::Http(format!(
                        "failed to read CA certificate '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
                // Try PEM first (most common format), fall back
                // to DER. This handles both
                // `-----BEGIN CERTIFICATE-----` text files and
                // raw binary DER files without the user having
                // to know which format their CA cert is in.
                let ca = reqwest::Certificate::from_pem(&der)
                    .or_else(|_| reqwest::Certificate::from_der(&der))
                    .map_err(|e| {
                        let mut msg =
                            format!("failed to parse CA certificate '{}': {}", path.display(), e,);
                        let mut src: Option<&(dyn std::error::Error + 'static)> = e.source();
                        let mut depth = 0;
                        while let Some(s) = src {
                            if depth >= 5 {
                                break;
                            }
                            msg.push_str(&format!(" → {}", s));
                            src = s.source();
                            depth += 1;
                        }
                        JiraError::Http(msg)
                    })?;
                client_builder.add_root_certificate(ca)
            }
            None => client_builder,
        };
        // If a client certificate is configured, load it from
        // the p12 file and attach it to the TLS handshake.
        // reqwest's `Identity::from_pkcs12_der` expects the
        // raw DER bytes of the PKCS#12 archive plus the
        // password string. We read the file, pass the bytes
        // through, and map any I/O or parse error into a
        // user-visible `JiraError::Http`.
        let client = match &self.config.certificate_path {
            Some(path) => {
                let der = std::fs::read(path).map_err(|e| {
                    JiraError::Http(format!(
                        "failed to read certificate '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
                let password = self.config.certificate_password.as_deref().unwrap_or("");
                let identity = reqwest::Identity::from_pkcs12_der(&der, password).map_err(|e| {
                    let mut msg =
                        format!("failed to parse certificate '{}': {}", path.display(), e,);
                    let mut src: Option<&(dyn std::error::Error + 'static)> = e.source();
                    let mut depth = 0;
                    while let Some(s) = src {
                        if depth >= 5 {
                            break;
                        }
                        msg.push_str(&format!(" → {}", s));
                        src = s.source();
                        depth += 1;
                    }
                    JiraError::Http(msg)
                })?;
                client_builder.identity(identity).build().map_err(|e| {
                    let mut msg = e.to_string();
                    let mut src: Option<&(dyn std::error::Error + 'static)> = e.source();
                    let mut depth = 0;
                    while let Some(s) = src {
                        if depth >= 5 {
                            break;
                        }
                        msg.push_str(&format!(" → {}", s));
                        src = s.source();
                        depth += 1;
                    }
                    JiraError::Http(msg)
                })?
            }
            None => client_builder.build().map_err(|e| {
                let mut msg = e.to_string();
                let mut src: Option<&(dyn std::error::Error + 'static)> = e.source();
                let mut depth = 0;
                while let Some(s) = src {
                    if depth >= 5 {
                        break;
                    }
                    msg.push_str(&format!(" → {}", s));
                    src = s.source();
                    depth += 1;
                }
                JiraError::Http(msg)
            })?,
        };
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .header("Accept", "application/json")
            .send()
            .map_err(|e| {
                // `reqwest::Error`'s own `Display` is
                // generic ("error decoding response body" /
                // "error sending request" / etc.) and
                // hides the real underlying cause. Walk
                // the `std::error::Error` source chain so
                // the user sees, e.g., "invalid gzip header"
                // or "invalid UTF-8" or the actual TLS
                // reason — without that, the generic
                // message gives no clue and we can't
                // diagnose the real problem.
                let mut msg = e.to_string();
                let mut src: Option<&(dyn std::error::Error + 'static)> = e.source();
                let mut depth = 0;
                while let Some(s) = src {
                    if depth >= 5 {
                        break;
                    }
                    msg.push_str(&format!(" → {}", s));
                    src = s.source();
                    depth += 1;
                }
                JiraError::Http(msg)
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            let excerpt: String = body.chars().take(200).collect();
            return Err(JiraError::Api(format!("{}: {}", status, excerpt.trim())));
        }
        // Read the body as text first so a parse failure
        // can include a snippet of the raw response. JIRA
        // can return shapes we don't model (e.g. an
        // `errorMessages` envelope on a 200, or a field
        // type that changed in a server upgrade) and the
        // user needs to see *what* the server returned to
        // diagnose it.
        let body = resp.text().map_err(|e| {
            let mut msg = e.to_string();
            let mut src: Option<&(dyn std::error::Error + 'static)> = e.source();
            let mut depth = 0;
            while let Some(s) = src {
                if depth >= 5 {
                    break;
                }
                msg.push_str(&format!(" → {}", s));
                src = s.source();
                depth += 1;
            }
            JiraError::Http(msg)
        })?;
        let snippet: String = body.chars().take(300).collect();
        let parsed: SearchResponse = serde_json::from_str(&body).map_err(|e| {
            JiraError::Parse(format!("{} — body starts with: {}", e, snippet.trim()))
        })?;
        Ok(parsed.issues.into_iter().map(JiraIssue::from).collect())
    }

    /// Fetch the comments for a single issue by key.
    /// The implementation mirrors `search`:
    ///   1. Build a `reqwest::Client` with the same
    ///      TLS extras (CA certificate, mTLS identity)
    ///      as the search client, so a JIRA server
    ///      configured with internal-CA or mTLS auth
    ///      "just works" for both endpoints.
    ///   2. Issue a GET to
    ///      `/rest/api/2/issue/{key}/comment` with the
    ///      bearer token in the `Authorization`
    ///      header. The key is URL-encoded so keys
    ///      with hyphens (the JIRA normal form
    ///      `PROJ-123`) survive transit unchanged —
    ///      hyphens don't need encoding, but other
    ///      characters could appear in custom
    ///      project keys.
    ///   3. Parse the response as a
    ///      `CommentsResponse` (a thin wrapper
    ///      around the `comments` array). Errors
    ///      flow through the same `JiraError`
    ///      variants as `search`.
    ///
    /// Pagination: the TUI fetches the first 100
    /// comments. JIRA's default page size is 50; we
    /// double that to make sure most issues fit in
    /// one round-trip. Issues with more than 100
    /// comments would need a follow-up request
    /// with `startAt=100`; the TUI doesn't
    /// paginate today. This is documented at the
    /// call site (and the test `JIRA_DEBOUNCE` is
    /// independent of the comment fetch).
    fn fetch_comments(&self, key: &str) -> Result<Vec<JiraComment>, JiraError> {
        use reqwest::blocking::Client;
        let url = format!(
            "{}/rest/api/2/issue/{}/comment?maxResults=100",
            self.config.server,
            urlencoding::encode(key),
        );
        // The TLS / client-builder dance is
        // identical to `search`. Refactor
        // opportunity: lift it into a private
        // helper on `RestJiraClient` so the two
        // request methods don't drift. Keeping it
        // duplicated for now because the
        // refactor isn't strictly necessary and
        // the reader benefits from seeing each
        // method's full setup in one place.
        let client_builder = Client::builder().timeout(std::time::Duration::from_secs(15));
        let client_builder = match &self.config.ca_certificate_path {
            Some(path) => {
                let der = std::fs::read(path).map_err(|e| {
                    JiraError::Http(format!(
                        "failed to read CA certificate '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
                let ca = reqwest::Certificate::from_pem(&der)
                    .or_else(|_| reqwest::Certificate::from_der(&der))
                    .map_err(|e| {
                        JiraError::Http(format!(
                            "failed to parse CA certificate '{}': {}",
                            path.display(),
                            e
                        ))
                    })?;
                client_builder.add_root_certificate(ca)
            }
            None => client_builder,
        };
        let client = match &self.config.certificate_path {
            Some(path) => {
                let der = std::fs::read(path).map_err(|e| {
                    JiraError::Http(format!(
                        "failed to read certificate '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
                let password = self.config.certificate_password.as_deref().unwrap_or("");
                let identity = reqwest::Identity::from_pkcs12_der(&der, password).map_err(|e| {
                    JiraError::Http(format!(
                        "failed to parse certificate '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
                client_builder
                    .identity(identity)
                    .build()
                    .map_err(|e| JiraError::Http(e.to_string()))?
            }
            None => client_builder
                .build()
                .map_err(|e| JiraError::Http(e.to_string()))?,
        };
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .header("Accept", "application/json")
            .send()
            .map_err(|e| JiraError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            let excerpt: String = body.chars().take(200).collect();
            return Err(JiraError::Api(format!("{}: {}", status, excerpt.trim())));
        }
        let body = resp.text().map_err(|e| JiraError::Http(e.to_string()))?;
        let snippet: String = body.chars().take(300).collect();
        let parsed: CommentsResponse = serde_json::from_str(&body).map_err(|e| {
            JiraError::Parse(format!("{} — body starts with: {}", e, snippet.trim()))
        })?;
        Ok(parsed.comments.into_iter().map(JiraComment::from).collect())
    }

    /// Post a new comment to a JIRA issue.
    /// Implementation: a POST to
    /// `/rest/api/2/issue/{key}/comment`
    /// with a JSON body of shape
    /// `{ "body": "<adf-envelope>"}`.
    /// The plain-text user input is
    /// wrapped in a minimal Atlassian
    /// Document Format envelope
    /// (`{"type":"doc","version":1,"content":[
    ///   {"type":"paragraph","content":[
    ///     {"type":"text","text":"<text>"}
    ///   ]}
    /// ]}`) — the smallest valid ADF
    /// doc JIRA's REST v2 accepts. A
    /// JIRA server expecting v3 / ADF
    /// strict (rare on self-hosted)
    /// would accept this same shape.
    ///
    /// The `body` parameter is `&str`
    /// (not the pre-built ADF JSON) so
    /// the test fake doesn't have to
    /// reimplement the wrapping. The
    /// wire format is owned by this
    /// method.
    ///
    /// Errors flow through the same
    /// `JiraError` variants as `search`
    /// and `fetch_comments`: `Http` for
    /// network / build failures,
    /// `Api` for non-success HTTP
    /// statuses, `Parse` for
    /// unparseable bodies (we don't
    /// actually need to parse the
    /// response, but a successful
    /// response is still validated
    /// via the status code).
    fn add_comment(&self, key: &str, body: &str) -> Result<(), JiraError> {
        let url = format!(
            "{}/rest/api/2/issue/{}/comment",
            self.config.server,
            urlencoding::encode(key),
        );
        // Plain-string body. JIRA Server (and
        // all Data Center / Cloud versions)
        // accept `{"body": "text"}`. The ADF
        // envelope (`{"body": {"type": "doc",
        // "version": 1, ...}}`) was rejected by
        // some Server instances with "Cannot
        // deserialize value of type
        // java.lang.String from Object value".
        // The plain-string form is the smallest
        // common denominator — all versions
        // accept it, so we never need to probe
        // for the server's preferred format.
        let payload = serde_json::json!({ "body": body });
        let client = self.build_blocking_client()?;
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .map_err(|e| JiraError::Http(e.to_string()))?;
        // JIRA's POST /comment returns
        // 201 Created on success. We
        // treat any 2xx as success;
        // 3xx / 4xx / 5xx are errors.
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            let excerpt: String = body.chars().take(200).collect();
            return Err(JiraError::Api(format!("{}: {}", status, excerpt.trim())));
        }
        // We don't need the response
        // body — JIRA returns the
        // created comment as JSON
        // ADF, but the TUI doesn't
        // need to display it (the
        // user sees the success
        // status message and the
        // overlay refresh would
        // re-fetch the comment list
        // if needed). We consume the
        // body so the connection
        // can be returned to the
        // pool.
        let _ = resp.text().map_err(|e| JiraError::Http(e.to_string()))?;
        Ok(())
    }
}

// ---- response shapes ----

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)] // missing `issues` → empty list (defensive)
struct SearchResponse {
    issues: Vec<ApiIssue>,
}

#[derive(Debug, serde::Deserialize)]
struct ApiIssue {
    key: String,
    #[serde(default)]
    fields: ApiFields,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)] // missing `fields` object → empty struct
struct ApiFields {
    // These text fields can come back as JSON `null` from
    // JIRA (e.g. an epic with no summary, or a system-
    // created issue missing `updated`). `#[serde(default)]`
    // only handles an *absent* field, NOT a *null* value —
    // `null` into `String` fails the whole `SearchResponse`.
    // Using `Option<String>` + `.unwrap_or_default()` below
    // makes `null` degrade to empty rather than failing the
    // entire query.
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    status: Option<Named>,
    #[serde(default)]
    issuetype: Option<Named>,
    #[serde(default)]
    priority: Option<Named>,
    #[serde(default)]
    assignee: Option<Named>,
    #[serde(default)]
    updated: Option<String>,
    /// JIRA's `duedate` is a flat `YYYY-MM-DD` string
    /// (no time component). It's `null` or absent
    /// for issues without a due date. Storing as
    /// `Option<String>` so a `null` (or absent) value
    /// degrades to an empty `JiraIssue.due` rather
    /// than failing the whole search.
    #[serde(default)]
    duedate: Option<String>,
    /// The Atlassian Document Format representation
    /// of the issue's description. This is a nested
    /// JSON tree (`{ "type": "doc", "content": [...] }`),
    /// not a flat string, so we keep the raw JSON
    /// value here and walk it in `From<ApiIssue> for
    /// JiraIssue` via the `extract_adf_text` helper.
    /// `null` and missing both mean "no description"
    /// and degrade to an empty `JiraIssue.description`.
    /// Storing as `Option<serde_json::Value>` (not
    /// `Option<String>`) so we can recurse into the
    /// tree without round-tripping through JSON.
    #[serde(default)]
    description: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Named {
    // JIRA's `name` inside a status/priority/issuetype/
    // user object can be `null` for legacy or custom-
    // workflow issues. Treat `null` as empty rather than
    // failing the whole search response.
    #[serde(default)]
    name: Option<String>,
}

impl Named {
    fn name_or_empty(&self) -> String {
        self.name.clone().unwrap_or_default()
    }
}

// ---- comment response shapes ----

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)] // missing `comments` → empty list (defensive)
struct CommentsResponse {
    /// JIRA's pagination shape. We don't
    /// surface `startAt` / `maxResults` /
    /// `total` to the TUI — the `maxResults=100`
    /// request shape is the only knob today.
    /// Kept in the struct so the parser is
    /// tolerant of JIRA's actual wire shape
    /// (every JIRA REST v2 paginated endpoint
    /// returns these three fields alongside
    /// the data array).
    #[allow(dead_code)]
    start_at: Option<u64>,
    #[allow(dead_code)]
    max_results: Option<u64>,
    #[allow(dead_code)]
    total: Option<u64>,
    comments: Vec<ApiComment>,
}

#[derive(Debug, serde::Deserialize)]
struct ApiComment {
    /// JIRA's comment ID. Optional in the
    /// response — system comments on workflow
    /// transitions may not carry an ID, and a
    /// future JIRA API version could change
    /// the field's type. Storing as
    /// `Option<String>` and serialising the
    /// missing case to an empty string.
    #[serde(default)]
    id: Option<String>,
    /// The comment's author object. JIRA's
    /// `author` field can be `null` for
    /// anonymous comments (rare; some
    /// self-service portals expose
    /// "comment as anonymous") and for system
    /// comments posted by bots.
    #[serde(default)]
    author: Option<Named>,
    /// The comment body. Same Atlassian
    /// Document Format as the issue
    /// description — a tree, not a flat
    /// string. The extractor (`extract_adf_text`)
    /// is reused here; see `JiraComment::from`.
    #[serde(default)]
    body: Option<serde_json::Value>,
    /// ISO-8601 `created` timestamp. Stored as
    /// `Option<String>` for the same
    /// null-vs-absent reason as elsewhere in
    /// this file: JIRA's REST v2 emits
    /// `null` for system comments that have no
    /// creation event.
    #[serde(default)]
    created: Option<String>,
    /// ISO-8601 `updated` timestamp. Falls
    /// back to `created` in the `From` impl
    /// when missing.
    #[serde(default)]
    updated: Option<String>,
}

/// Maximum number of characters of the issue's
/// description rendered into the details-pane
/// preview. Descriptions are folded onto a single
/// line (paragraph separators become spaces) and
/// truncated with a trailing `…` if they exceed
/// this cap. A 240-char preview line is wide
/// enough to read a real-world paragraph at a
/// glance in a typical terminal width, while
/// keeping the metadata block (Status, Priority,
impl From<ApiIssue> for JiraIssue {
    fn from(a: ApiIssue) -> Self {
        let f = a.fields;
        // The full plain-text rendering of the
        // description is stored verbatim — the
        // preview pane and the show-output overlay
        // both do their own line-budget truncation
        // at render time. The earlier design
        // truncated to a fixed character cap
        // (`MAX_DESCRIPTION_CHARS = 240`) inside
        // the `From` impl, but the new spec
        // (3-line header + rest of preview filled
        // with description text) wants the full
        // body to flow into the preview. The
        // `.take(4)` cap in the preview renderer
        // is enough; the overlay is scrollable for
        // long bodies.
        let description = f
            .description
            .as_ref()
            .map(extract_adf_text)
            .unwrap_or_default();
        JiraIssue {
            key: a.key,
            summary: f.summary.unwrap_or_default(),
            status: f.status.map(|n| n.name_or_empty()).unwrap_or_default(),
            issuetype: f.issuetype.map(|n| n.name_or_empty()).unwrap_or_default(),
            priority: f.priority.map(|n| n.name_or_empty()).unwrap_or_default(),
            assignee: f.assignee.map(|n| n.name_or_empty()).unwrap_or_default(),
            updated: f.updated.unwrap_or_default(),
            due: f.duedate.unwrap_or_default(),
            description,
        }
    }
}

impl From<ApiComment> for JiraComment {
    fn from(a: ApiComment) -> Self {
        // The body uses the same ADF as
        // `JiraIssue::description`. The
        // extractor handles all the same edge
        // cases (paragraphs, mentions, links,
        // hard breaks, emoji). We do NOT
        // truncate comment bodies — the
        // show-output overlay is scrollable, so
        // a long comment is fine.
        let body = a.body.as_ref().map(extract_adf_text).unwrap_or_default();
        // `created` is the sort key. When it's
        // missing (system comment), we keep
        // `created` as the empty string so the
        // sort still works (empty strings sort
        // last in lexicographic order, which is
        // a fine degradation).
        let created = a.created.unwrap_or_default();
        // `updated` falls back to `created`
        // when missing. JIRA's `comment` API
        // often sets both to the same value
        // for fresh comments, but the field
        // can also be absent for system
        // comments. The fallback keeps the
        // "last-edited" semantics consistent
        // for callers that want to display it
        // (we don't, today).
        let updated = a.updated.unwrap_or_else(|| created.clone());
        JiraComment {
            id: a.id.unwrap_or_default(),
            author: a.author.map(|n| n.name_or_empty()).unwrap_or_default(),
            body,
            created,
            updated,
        }
    }
}

/// Walk an Atlassian Document Format (ADF) tree
/// and produce a flat plain-text rendering. ADF
/// is the JSON format JIRA v2 returns for the
/// `description` field — a tree of `doc` /
/// `paragraph` / `text` / `heading` / `mention` /
/// `link` / etc. nodes. We don't need to faithfully
/// reproduce the document — we need readable text
/// for the details-pane preview.
///
/// The renderer concatenates the `text` field of
/// every `text` node, separates top-level
/// paragraphs with a single space, and silently
/// drops everything else. `mention` and `link`
/// nodes have a `text` representation we
/// substitute in (`@username` and the link's
/// literal text or href, respectively) so the
/// preview still surfaces a useful hint of who's
/// mentioned and what link was dropped into the
/// body.
///
/// Non-ADF values (plain strings, missing
/// `content`, a leaf, etc.) fall back to a
/// reasonable best-effort: if the value is a
/// string, return it verbatim; if it's an object
/// with a `text` field, return that; otherwise
/// return an empty string. This keeps the
/// extractor robust to JIRA installations that
/// return a non-standard description shape (a
/// legacy plain-text field, an admin-customised
/// schema, etc.).
pub fn extract_adf_text(value: &serde_json::Value) -> String {
    let mut out = String::new();
    extract_adf_text_into(value, &mut out, /* in_paragraph */ false);
    out
}

fn extract_adf_text_into(value: &serde_json::Value, out: &mut String, _in_paragraph: bool) {
    match value {
        serde_json::Value::String(s) => {
            out.push_str(s);
        }
        serde_json::Value::Array(items) => {
            // Top-level node list. Each item is a
            // block-level node (paragraph, heading,
            // etc.). We separate blocks with a
            // single space so the flattened preview
            // stays one line.
            let mut first = true;
            for item in items {
                if !first
                    && matches!(
                        item.get("type").and_then(|t| t.as_str()),
                        Some("paragraph")
                            | Some("heading")
                            | Some("blockquote")
                            | Some("codeBlock")
                            | Some("bulletList")
                            | Some("orderedList")
                            | Some("listItem")
                            | Some("rule")
                    )
                {
                    // Paragraph and other block-level
                    // breaks become a real newline so
                    // the description preserves its
                    // visual structure in the
                    // preview-pane and overlay. The
                    // earlier design folded these to
                    // single spaces, but the new
                    // multi-line layout (3-line header
                    // + description body) wants the
                    // paragraph boundaries to survive.
                    out.push('\n');
                }
                first = false;
                extract_adf_text_into(item, out, false);
            }
        }
        serde_json::Value::Object(map) => {
            let node_type = map.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match node_type {
                "text" => {
                    if let Some(s) = map.get("text").and_then(|t| t.as_str()) {
                        out.push_str(s);
                    }
                }
                "hardBreak" => {
                    // A `hardBreak` inside a paragraph
                    // is a soft line break — render it
                    // as a real newline so the
                    // paragraph preserves the
                    // author's intent. (Earlier
                    // designs folded to space, but the
                    // new multi-line layout wants the
                    // line structure to survive.)
                    out.push('\n');
                }
                "mention" => {
                    // Mentions carry an `attrs.text`
                    // like `@username`. Fall back to
                    // the user-visible `attrs.displayName`
                    // if present, else the literal
                    // `@` + id.
                    if let Some(t) = map
                        .get("attrs")
                        .and_then(|a| a.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        out.push_str(t);
                    } else if let Some(name) = map
                        .get("attrs")
                        .and_then(|a| a.get("displayName"))
                        .and_then(|t| t.as_str())
                    {
                        out.push('@');
                        out.push_str(name);
                    } else if let Some(id) = map
                        .get("attrs")
                        .and_then(|a| a.get("id"))
                        .and_then(|t| t.as_str())
                    {
                        out.push('@');
                        out.push_str(id);
                    }
                }
                "link" => {
                    // Render the link's visible text
                    // (children), falling back to the
                    // href. Recurse into children
                    // first.
                    let had_text_before = out.len();
                    if let Some(content) = map.get("content") {
                        extract_adf_text_into(content, out, true);
                    }
                    if out.len() == had_text_before
                        && let Some(href) = map
                            .get("attrs")
                            .and_then(|a| a.get("href"))
                            .and_then(|t| t.as_str())
                    {
                        out.push_str(href);
                    }
                }
                "emoji" => {
                    // ADF emoji nodes carry the
                    // short-name in `attrs.shortName`
                    // (e.g. `:smile:`). Render that
                    // literally so a `:smile:`-like
                    // token survives in the preview.
                    if let Some(s) = map
                        .get("attrs")
                        .and_then(|a| a.get("shortName"))
                        .and_then(|t| t.as_str())
                    {
                        out.push_str(s);
                    }
                }
                _ => {
                    // Container nodes (`doc`,
                    // `paragraph`, `heading`,
                    // `bulletList`, etc.) — recurse
                    // into their `content` if any.
                    if let Some(content) = map.get("content") {
                        extract_adf_text_into(content, out, true);
                    }
                }
            }
        }
        _ => {
            // Numbers, booleans, null — nothing
            // useful to render.
        }
    }
}

// ---- query parsing ----

/// The number of days that `@month` looks back. The
/// spec says 31; the notes-mode precedent uses 30.
/// Kept as a named constant so flipping the policy
/// (or supporting both — e.g. `@month30` / `@month31`
/// — in the future) is a one-line change.
const JIRA_ALIAS_MONTH_DAYS: i64 = 31;
const JIRA_ALIAS_WEEK_DAYS: i64 = 7;
/// `@today` is documented as "updated today", which
/// we model as "updated in the last 24h" by using
/// yesterday's date as the JQL cutoff. JQL's date-only
/// comparison is midnight-aligned, so the cutoff is
/// `<today's date - 1 day>` to cover a rolling 24h
/// window. (`@today`'s JQL form is `updated >=
/// "<yesterday>"`, which evaluates to "updated
/// yesterday or later".)
const JIRA_ALIAS_TODAY_DAYS: i64 = 1;

/// The standard JQL system field names that are
/// recognised for tab-completion inside the JIRA
/// query. The list mirrors the documented JQL system
/// fields plus a few common custom-field conventions
/// (`sprint`, `epic`, `parent`, `storyPoints`,
/// `rank`). The list is intentionally not exhaustive
/// of every custom field a JIRA instance can have —
/// user-defined custom fields show up in the
/// completion only when the user has bound them via
/// `jira.fields.<name>=...` in the config file, which
/// is appended to this list at completion time.
///
/// Ordering: alphabetical (case-insensitive). The
/// completion algorithm doesn't depend on order, but
/// alphabetical makes diffs against the upstream
/// JIRA documentation easier to audit.
pub static JIRA_FIELDS: &[&str] = &[
    "affectedVersion",
    "assignee",
    "category",
    "component",
    "created",
    "creator",
    "description",
    "due",
    "duedate",
    "environment",
    "epic",
    "fixVersion",
    "issue",
    "issueKey",
    "issues",
    "issuetype",
    "key",
    "label",
    "labels",
    "lastViewed",
    "level",
    "originalEstimate",
    "parent",
    "priority",
    "project",
    "rank",
    "remainingEstimate",
    "reporter",
    "resolution",
    "resolved",
    "sprint",
    "status",
    "statusCategory",
    "storyPoints",
    "summary",
    "text",
    "timeSpent",
    "type",
    "updated",
    "voter",
    "version",
    "votes",
    "watcher",
    "watchers",
    "workRatio",
];

/// Tab-completion of a JQL field-name prefix. The
/// user's example: typing `lab<TAB>` inside the JIRA
/// query should expand to `labels=`. The cursor lands
/// right after the `=` so the user can immediately
/// type the value.
///
/// Returns:
///
/// - `None` when no field starts with `prefix`. The
///   caller surfaces a status message (a soft
///   rejection) and otherwise leaves the query
///   unchanged so a stray `Tab` never destroys
///   text.
/// - `Some(completion)` where `completion` is the
///   expanded text. When multiple fields match
///   (e.g. `lab` matches both `label` and
///   `labels`), the completion is the LONGEST COMMON
///   PREFIX of the matches — i.e. `lab`. The user
///   keeps typing to disambiguate (`labels` vs
///   `label`) and presses `Tab` again. This is the
///   standard readline/bash behaviour and is the
///   least surprising thing for users who already
///   know shell completion.
/// - When the prefix already matches a complete
///   field name (one of the system fields exactly),
///   the function returns `Some(prefix)` (i.e. the
///   expansion is a no-op). The caller is then
///   expected to append `=` if appropriate. See
///   `jira_field_complete_with_value` for the
///   one-shot variant that does this in a single
///   call.
///
/// `prefix` is matched case-insensitively; the
/// returned completion preserves the canonical
/// casing from the `JIRA_FIELDS` table.
pub fn jira_field_complete(prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        // No prefix means we have no idea what the
        // user wants. Don't expand to anything
        // (otherwise the query would lose whatever
        // was at the cursor).
        return None;
    }
    let lower = prefix.to_ascii_lowercase();
    let matches: Vec<&'static str> = JIRA_FIELDS
        .iter()
        .copied()
        .filter(|f| f.to_ascii_lowercase().starts_with(&lower))
        .collect();
    match matches.len() {
        0 => None,
        1 => Some(matches[0].to_string()),
        _ => {
            // Multiple matches: find the longest
            // common prefix. `starts_with` guarantees
            // every match begins with `prefix`, so the
            // common prefix is at least `prefix`. Walk
            // each subsequent character and keep it as
            // long as every match agrees.
            let first = matches[0];
            let mut end = prefix.len();
            while end < first.len() {
                let candidate = &first[..end + 1];
                if matches.iter().all(|m| {
                    m.to_ascii_lowercase()
                        .starts_with(&candidate.to_ascii_lowercase())
                }) {
                    end += 1;
                } else {
                    break;
                }
            }
            Some(first[..end].to_string())
        }
    }
}

/// Tab-completion convenience: returns the expansion
/// with a trailing `=` appended when the prefix
/// matches exactly one field. The user's mental model
/// is `lab<TAB>` → `labels=` (cursor lands after the
/// `=`). For the ambiguous case, returns just the
/// longest-common-prefix (no `=`) because we don't
/// know which field the user is heading toward.
///
/// Returns `None` when no field starts with the
/// prefix.
pub fn jira_field_complete_with_value(prefix: &str) -> Option<String> {
    let lower = prefix.to_ascii_lowercase();
    let matches: Vec<&'static str> = JIRA_FIELDS
        .iter()
        .copied()
        .filter(|f| f.to_ascii_lowercase().starts_with(&lower))
        .collect();
    match matches.len() {
        0 => None,
        1 => Some(format!("{}=", matches[0])),
        // Multiple matches: no trailing `=`. The
        // longest-common-prefix is the right thing
        // to expand to (the user keeps typing).
        _ => jira_field_complete(prefix),
    }
}

/// Tab-completion for JIRA `@` aliases and
/// user-defined fragments. When the user types
/// `@m<TAB>` inside `-` mode, this function
/// expands the prefix to the matching alias or
/// fragment name.
///
/// The known names are the four built-in aliases
/// (`me`, `today`, `week`, `month`) plus every
/// key in the user-supplied `fragments` map
/// ( loaded from `jira.search.<name>=...`
/// config entries). Matching is case-insensitive
/// on the prefix.
///
/// Returns:
/// - `None` if no alias/fragment starts with the
///   prefix.
/// - `Some("me ")` if the prefix matches exactly
///   one name (trailing space so the user can
///   immediately type the next token).
/// - `Some("m")` if multiple names share a
///   longest-common-prefix that extends the
///   user's input.
///
/// The caller ( `jira_field_complete_at_cursor`)
/// prepends the `@` back onto the result.
pub fn jira_alias_complete(
    prefix: &str,
    fragments: &std::collections::HashMap<String, String>,
) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }
    let lower = prefix.to_ascii_lowercase();
    let mut names: Vec<&str> = Vec::new();
    // Built-in aliases.
    for alias in ["me", "today", "week", "month"] {
        if alias.to_ascii_lowercase().starts_with(&lower) {
            names.push(alias);
        }
    }
    // User-defined fragments (keys are already
    // lowercased by `Config::assign_jira_fragment`).
    for key in fragments.keys() {
        if key.to_ascii_lowercase().starts_with(&lower) {
            names.push(key);
        }
    }
    match names.len() {
        0 => None,
        1 => Some(names[0].to_string()),
        _ => {
            // Longest common prefix.
            let first = names[0];
            let mut end = prefix.len();
            while end < first.len() {
                let candidate = &first[..end + 1];
                if names.iter().all(|m| {
                    m.to_ascii_lowercase()
                        .starts_with(&candidate.to_ascii_lowercase())
                }) {
                    end += 1;
                } else {
                    break;
                }
            }
            Some(first[..end].to_string())
        }
    }
}

/// Convenience wrapper: appends a trailing space
/// when the prefix matches exactly one name, so
/// the cursor lands after the space and the user
/// can type the next token immediately.
pub fn jira_alias_complete_with_space(
    prefix: &str,
    fragments: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let lower = prefix.to_ascii_lowercase();
    let mut names: Vec<&str> = Vec::new();
    for alias in ["me", "today", "week", "month"] {
        if alias.to_ascii_lowercase().starts_with(&lower) {
            names.push(alias);
        }
    }
    for key in fragments.keys() {
        if key.to_ascii_lowercase().starts_with(&lower) {
            names.push(key);
        }
    }
    match names.len() {
        0 => None,
        1 => Some(format!("{} ", names[0])),
        _ => jira_alias_complete(prefix, fragments),
    }
}

/// Return all JIRA field names that start with
/// `prefix` (case-insensitive). Used by the
/// completion menu to show candidates when the
/// user presses Tab and there are multiple matches.
/// Returns the matches in their canonical casing
/// (the order from `JIRA_FIELDS`, which is
/// alphabetical). Empty vector means no match.
pub fn jira_field_matches(prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let lower = prefix.to_ascii_lowercase();
    JIRA_FIELDS
        .iter()
        .copied()
        .filter(|f| f.to_ascii_lowercase().starts_with(&lower))
        .map(|f| f.to_string())
        .collect()
}

/// Return all JIRA alias / fragment names that
/// start with `prefix` (case-insensitive). Used by
/// the completion menu. Built-in aliases come
/// first (`me`, `today`, `week`, `month`), then
/// user-defined fragments in alphabetical order.
pub fn jira_alias_matches(
    prefix: &str,
    fragments: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let lower = prefix.to_ascii_lowercase();
    let mut names: Vec<String> = Vec::new();
    for alias in ["me", "today", "week", "month"] {
        if alias.to_ascii_lowercase().starts_with(&lower) {
            names.push(alias.to_string());
        }
    }
    let mut fragment_names: Vec<String> = fragments
        .keys()
        .filter(|k| k.to_ascii_lowercase().starts_with(&lower))
        .cloned()
        .collect();
    fragment_names.sort();
    names.extend(fragment_names);
    names
}

/// Return all note tag names that start with
/// `prefix` (case-insensitive). The completion menu
/// uses this to show candidates when the user
/// presses Tab and there are multiple tag matches.
pub fn notes_tag_matches(db_path: &std::path::Path, prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let tags = match note_search::commands::metadata::get_unique_values(db_path, "tag") {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let lower = prefix.to_ascii_lowercase();
    tags.iter()
        .filter(|t| t.to_ascii_lowercase().starts_with(&lower))
        .cloned()
        .collect()
}

/// Return all note link names that start with
/// `prefix` (case-insensitive). The `.md` suffix is
/// stripped from each link before matching (matching
/// the behaviour of `notes_link_complete`). The
/// completion menu uses this to show candidates
/// when the user presses Tab and there are
/// multiple link matches.
///
/// Link targets in Obsidian are
/// case-insensitive — `Project` and `project`
/// refer to the same note. When the database
/// contains multiple casings of the same link
/// (e.g. `Project.md` and `project.md`), this
/// function deduplicates by lowercased form and
/// returns the lowercase version. This prevents
/// the completion menu from opening for the
/// trivial case where the matches only differ
/// by case. The user always gets the
/// lowercase expansion, matching Obsidian's
/// case-insensitive convention.
pub fn notes_link_matches(db_path: &std::path::Path, prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let links = match note_search::commands::metadata::get_unique_values(db_path, "link") {
        Ok(l) => l,
        Err(_) => return Vec::new(),
    };
    let lower = prefix.to_ascii_lowercase();
    let matches: Vec<String> = links
        .iter()
        .filter(|l| l.to_ascii_lowercase().starts_with(&lower))
        .map(|l| l.strip_suffix(".md").unwrap_or(l).to_string())
        .collect();
    // Deduplicate by lowercased
    // form. If all matches
    // lowercase to the same
    // value, return just the
    // lowercase version (a
    // single-element list) so
    // the TUI's `>= 2` menu
    // check doesn't fire. The
    // user gets the lowercase
    // expansion directly
    // without a menu flash.
    dedupe_case_insensitive(&matches)
}

/// Deduplicate a list of strings by
/// their lowercased form. Links
/// that only differ by case
/// (`Project` vs `project`) are
/// treated as the same link and
/// collapsed to the lowercase
/// version. Genuinely different
/// links (e.g. `project` vs
/// `projects`) are preserved. The
/// order of first occurrence is
/// kept so the TUI's menu shows
/// candidates in a stable order.
///
/// This prevents the completion
/// menu from opening when the
/// only differences between
/// matches are casing — the user
/// always gets the lowercase
/// expansion without a menu
/// flash.
fn dedupe_case_insensitive(matches: &[String]) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut result: Vec<String> = Vec::new();
    for m in matches {
        let lower = m.to_ascii_lowercase();
        if seen.insert(lower) {
            // First time seeing
            // this lowercase
            // form. Keep the
            // lowercase version
            // (Obsidian's
            // case-insensitive
            // convention).
            result.push(m.to_ascii_lowercase());
        }
    }
    result
}

/// Tab-completion for note tags inside the notes (`@`)
/// and todos (`!`) prefixes. The completion list is
/// the set of unique tags stored in the `note_search`
/// database (queried via
/// `note_search::commands::metadata::get_unique_values`).
/// Matching is case-insensitive on the prefix; the
/// returned name preserves the canonical casing from
/// the database (tags are stored as-is in the indexer,
/// so a tag defined as `MyTag` in a note file
/// surfaces here as `MyTag`).
///
/// Returns:
/// - `None` if no tag starts with the prefix.
/// - `Some("tagname ")` if the prefix matches exactly
///   one tag (trailing space so the user can type
///   the next token immediately).
/// - `Some("tagname")` if multiple tags share a
///   longest-common-prefix that extends the user's
///   input.
pub fn notes_tag_complete(db_path: &std::path::Path, prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }
    let tags = note_search::commands::metadata::get_unique_values(db_path, "tag").ok()?;
    notes_complete_inner(&tags, prefix)
}

/// Tab-completion for note links inside the notes
/// (`@`) and todos (`!`) prefixes. The completion
/// list is the set of unique wiki-link targets stored
/// in the `note_search` database. Matching is
/// case-insensitive on the prefix; the returned name
/// preserves the canonical casing (link targets are
/// case-sensitive in Obsidian, so `MyNote` and
/// `mynote` are different links).
///
/// `[[link]]` wiki-link search
/// (Obsidian syntax), which
/// supports link names with
/// spaces. The link name is
/// wrapped in double quotes
/// when it contains a space
/// (e.g. `[["my note"]]`) so
/// the boundary between the
/// link name and the rest of
/// the query is unambiguous.
///
/// Returns the full wiki-link
/// expansion with a trailing
/// space (e.g. `[[bernd_matthiesen]] `),
/// or just the LCP (e.g.
/// `[[bernd]]`) for ambiguous
/// prefixes.
pub fn notes_link_complete(db_path: &std::path::Path, prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }
    let links = note_search::commands::metadata::get_unique_values(db_path, "link").ok()?;
    // The database stores link targets
    // with their `.md` file extension
    // (e.g. `bernd_matthiesen.md`). The
    // user types `@bernd` and expects
    // the expansion to be
    // `[[bernd_matthiesen]]` (no
    // extension), matching the
    // Obsidian convention of
    // referencing notes by their bare
    // name. Strip the `.md` suffix
    // from every link before matching
    // so both the LCP and the
    // unique-match paths produce the
    // bare-name form. Other
    // extensions are left intact
    // (`.txt`, `.org`, etc.) since
    // the user might have indexed
    // non-markdown notes.
    let stripped: Vec<String> = links
        .iter()
        .map(|n| n.strip_suffix(".md").unwrap_or(n).to_string())
        .collect();
    // Link targets in Obsidian
    // are case-insensitive —
    // `Project` and `project`
    // refer to the same note.
    // Deduplicate by lowercased
    // form before passing to
    // `notes_complete_inner` so
    // the expansion is always
    // the lowercase version
    // (matching Obsidian's
    // convention) and the
    // single-match path
    // produces a clean
    // result even when the
    // DB has multiple casings
    // of the same link.
    let deduped = dedupe_case_insensitive(&stripped);
    let bare = notes_complete_inner(&deduped, prefix)?;
    // `notes_complete_inner` returns
    // the completion with a trailing
    // space (for the unique-match
    // case). Strip it before the
    // wrapping, then re-add it
    // after the `[[...]]` so the
    // trailing space lives outside
    // the brackets.
    let (name, trailing_space) = if let Some(stripped) = bare.strip_suffix(' ') {
        (stripped, " ")
    } else {
        (bare.as_str(), "")
    };
    // Wrap the link name in `[[...]]`
    // (Obsidian wiki-link syntax).
    // The brackets unambiguously
    // delimit the link target even
    // when it contains spaces, so
    // no additional quoting is
    // needed: `[[my note]]` is
    // already a single link
    // reference as far as the
    // `note_search` tokenizer is
    // concerned.
    Some(format!("[[{}]]{}", name, trailing_space))
}

/// Return all note attribute (header-field) keys that
/// start with `prefix` (case-insensitive). Backs Tab
/// completion for the `[attr:value]` / `[attr]` query
/// syntax's KEY half (before the `:`). The completion
/// menu uses this to show candidates when there are
/// multiple keys sharing the prefix.
pub fn notes_attr_key_matches(db_path: &std::path::Path, prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let keys = match note_search::commands::metadata::get_all_attributes(db_path) {
        Ok(k) => k,
        Err(_) => return Vec::new(),
    };
    let lower = prefix.to_ascii_lowercase();
    keys.iter()
        .filter(|k| k.to_ascii_lowercase().starts_with(&lower))
        .cloned()
        .collect()
}

/// Tab-completion for the attribute KEY half of
/// `[attr:value]` / `[attr]`. Unlike tag/link
/// completion (which appends a trailing space), a
/// unique key match appends a trailing `:` so the
/// cursor lands ready to type the value — the same
/// convention `jira_field_complete_with_value` uses
/// for `=`.
///
/// Returns:
/// - `None` if no attribute key starts with the prefix.
/// - `Some("key:")` if the prefix matches exactly one
///   key.
/// - `Some("key")` (no `:`) if multiple keys share a
///   longest-common-prefix that extends the user's
///   input.
pub fn notes_attr_key_complete(db_path: &std::path::Path, prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }
    let keys = note_search::commands::metadata::get_all_attributes(db_path).ok()?;
    let lower = prefix.to_ascii_lowercase();
    let matches: Vec<&str> = keys
        .iter()
        .filter(|k| k.to_ascii_lowercase().starts_with(&lower))
        .map(|s| s.as_str())
        .collect();
    match matches.len() {
        0 => None,
        1 => Some(format!("{}:", matches[0])),
        _ => {
            // Longest common prefix of all matches.
            let first = matches[0];
            let mut end = prefix.len();
            while end < first.len() {
                let candidate = &first[..end + 1];
                if matches.iter().all(|m| {
                    m.to_ascii_lowercase()
                        .starts_with(&candidate.to_ascii_lowercase())
                }) {
                    end += 1;
                } else {
                    break;
                }
            }
            Some(first[..end].to_string())
        }
    }
}

/// Return all values recorded for attribute `key`
/// that start with `prefix` (case-insensitive). Backs
/// Tab completion for the `[attr:value]` VALUE half
/// (after the `:`).
///
/// Note: `note_search::commands::metadata::get_unique_values`
/// lowercases `key` internally before looking it up in
/// each note's header-fields JSON, so this silently
/// returns nothing for a `key` containing uppercase
/// letters (an upstream note_search quirk, not
/// something smarthistory controls).
pub fn notes_attr_value_matches(db_path: &std::path::Path, key: &str, prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let field = format!("attr:{}", key);
    let values = match note_search::commands::metadata::get_unique_values(db_path, &field) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let lower = prefix.to_ascii_lowercase();
    values
        .iter()
        .filter(|v| v.to_ascii_lowercase().starts_with(&lower))
        .cloned()
        .collect()
}

/// Tab-completion for the attribute VALUE half of
/// `[attr:value]`. Same trailing-space convention as
/// tag completion (`#feat<TAB>` → `#feature `):
/// `[assignee:sa<TAB>` → `[assignee:sarah ` on a
/// unique match; the closing `]` is left for the user
/// to type, matching tag completion's behaviour rather
/// than risking a duplicate `]` if one is already
/// present after the cursor.
pub fn notes_attr_value_complete(db_path: &std::path::Path, key: &str, prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }
    let field = format!("attr:{}", key);
    let values = note_search::commands::metadata::get_unique_values(db_path, &field).ok()?;
    notes_complete_inner(&values, prefix)
}

/// Shared LCP / single-match logic for tag and link
/// completion. Extracted so the two functions stay
/// one-liners and the matching semantics are
/// identical.
pub(crate) fn notes_complete_inner(names: &[String], prefix: &str) -> Option<String> {
    let lower = prefix.to_ascii_lowercase();
    let matches: Vec<&str> = names
        .iter()
        .filter(|n| n.to_ascii_lowercase().starts_with(&lower))
        .map(|s| s.as_str())
        .collect();
    match matches.len() {
        0 => None,
        1 => Some(format!("{} ", matches[0])),
        _ => {
            // Longest common prefix of all matches.
            // `starts_with` guarantees every match
            // begins with `prefix`, so the LCP is at
            // least `prefix` long.
            let first = matches[0];
            let mut end = prefix.len();
            while end < first.len() {
                let candidate = &first[..end + 1];
                if matches.iter().all(|m| {
                    m.to_ascii_lowercase()
                        .starts_with(&candidate.to_ascii_lowercase())
                }) {
                    end += 1;
                } else {
                    break;
                }
            }
            Some(first[..end].to_string())
        }
    }
}

/// Build a JQL string from the `-`-mode query body. The body
/// is tokenized on whitespace; each token is classified:
///
/// - **Issue key** — matches `^\w+-\d+$` (e.g. `PROJ-123`).
///   Multiple keys collapse into `key in (a, b)`; a single
///   key becomes `key = a`.
/// - **Field=value** — matches `^\w+=\S*$` (e.g.
///   `project=PROJ`, `labels=LABEL`). Becomes
///   `<field> = "<value>"` (value is JQL-quoted).
/// - **Free text** — anything else. Becomes
///   `(description ~ "text" OR summary ~ "text")`.
/// - **Alias** — matches the leading-`@` shortcut set:
///   `@me`, `@today`, `@week`, `@month`. Each alias is
///   recognised as a whole whitespace-separated token
///   (case-insensitive; the leading `@` is optional, so
///   `today` works the same as `@today`, matching the
///   notes-mode precedent). Aliases are stripped from the
///   body before classification and become their own
///   AND-joined JQL clauses:
///   - `@me` → `assignee = currentUser()`.
///   - `@today` → `updated >= "<today - 1d>"` (rolling
///     24h window using yesterday's date).
///   - `@week` → `updated >= "<today - 7d>"`.
///   - `@month` → `updated >= "<today - 31d>"`.
/// - **Fragment** — a user-defined named JQL snippet from
///   the config file (`jira.search.<name>=<jql>`). The
///   token `@<name>` is recognised as a whole-word,
///   case-insensitive identifier (after the leading `@` is
///   stripped) and is **not** one of the four built-in
///   aliases. The fragment's JQL is spliced verbatim,
///   wrapped in parentheses to preserve any internal
///   `AND` / `OR` it contains. Reserved names (`me`,
///   `today`, `week`, `month`) cannot be redefined as
///   fragments (the config loader silently drops them).
///   The fragment is treated as a TRUSTED JQL string: the
///   user wrote it in their config and the JQL builder
///   does NOT JQL-quote it. The TUI thread is
///   responsible for warning the user about undefined
///   fragment names (see the second return value).
///
/// The groups are AND-joined in order (alias / fragment
/// clauses first, then keys, then `field=value`, then
/// free-text). The result always ends with
/// ` ORDER BY updated DESC` so the list is
/// newest-updated first.
///
/// When `default_project` is set, a `project = "<proj>"`
/// clause is always prepended — even when the body is
/// non-empty — so the user's project-scoped view is
/// never accidentally widened by a free-text search.
/// An empty body without a project produces a server-wide
/// `ORDER BY updated DESC` (the "recently touched across
/// all projects" view).
///
/// `now_epoch` is the caller's "now" in Unix epoch
/// seconds; it determines the cutoff date for
/// `@today` / `@week` / `@month`. The TUI passes
/// `self.now_epoch()`; tests pass a fixed value for
/// reproducibility.
///
/// `fragments` is the user-defined name → JQL map
/// loaded from the config file's `jira.search.<name>=...`
/// entries. Empty in tests / no-config cases. Lookups
/// are case-insensitive on the name; the original-case
/// name from the query is preserved in the undefined
/// list for diagnostic messages.
///
/// Returns `(jql, undefined_fragments)`:
/// - `jql` is the JQL string ready to send to the JIRA
///   server. Undefined fragments fall through to free
///   text — the JQL is always valid syntax — but the
///   caller should refuse to fire the search when
///   `undefined_fragments` is non-empty (otherwise the
///   user gets a wrong-result search they didn't ask
///   for).
/// - `undefined_fragments` is the list of fragment
///   names that appeared as `@<name>` in the body but
///   are not present in the `fragments` map. Preserves
///   the order of first appearance and dedupes
///   duplicates so a query like `@foo @foo @bar` yields
///   `["foo", "bar"]`.
pub fn build_jql(
    body: &str,
    default_project: Option<&str>,
    now_epoch: i64,
    fragments: &std::collections::HashMap<String, String>,
) -> (String, Vec<String>) {
    let body = body.trim();
    // If the user has included an `ORDER BY` clause in
    // their query (e.g. from a persisted session), strip
    // it before parsing. `build_jql` always appends
    // `ORDER BY updated DESC` at the end, so a user-
    // supplied ORDER BY would be double-counted and its
    // individual words (`order`, `by`, `updated`,
    // `DESC`) would incorrectly become free-text search
    // tokens.
    let body = if let Some(pos) = body.to_ascii_lowercase().find("order by") {
        body[..pos].trim()
    } else {
        body
    };
    if body.is_empty() {
        // The "all aliases" view: an empty body with
        // `@me` / `@today` / `@week` / `@month` would
        // have shown the chip but the body slice
        // produced by `jira_pattern` is empty (the user
        // typed just `-@me` or similar). We treat that
        // as the no-clauses case and let the
        // server-wide / project-scoped fallback run.
        return (
            match default_project {
                Some(p) => format!("project = {} ORDER BY updated DESC", escape_jql_string(p)),
                None => "ORDER BY updated DESC".to_string(),
            },
            Vec::new(),
        );
    }
    let key_re = regex::Regex::new(r"^[A-Z]+-[0-9]+$").expect("static regex");
    let kv_re = regex::Regex::new(r"^(\w+)=(.*)$").expect("static regex");
    let mut keys: Vec<&str> = Vec::new();
    let mut kvs: Vec<(&str, &str)> = Vec::new();
    let mut text: Vec<&str> = Vec::new();
    // Explicit `project=...` takes precedence over the
    // default project and is placed first in the JQL.
    let mut explicit_project: Option<&str> = None;
    // Alias state. `me` is a boolean (it's orthogonal
    // to the date aliases). `date_filter` is the
    // "last one wins" resolved date window — initially
    // `None`, set to a `Some` whenever we see
    // `@today` / `@week` / `@month`.
    let mut me_alias = false;
    let mut date_filter: Option<DateAlias> = None;
    // User-defined JQL fragments spliced into the
    // query. Stored in the order they appear so the
    // JQL preserves the user's reading order
    // (important when fragments are AND-joined in
    // a meaningful sequence, e.g.
    // `@sprint @blocked`).
    let mut fragment_clauses: Vec<String> = Vec::new();
    // Fragment names that appeared in the body but are
    // NOT in the `fragments` map. Used to surface a
    // diagnostic status message instead of firing a
    // wrong-result search. Deduped + order-preserving
    // — the same fragment appearing twice is
    // diagnostic noise, so we only report it once.
    let mut undefined_fragments: Vec<String> = Vec::new();
    for tok in body.split_whitespace() {
        // Strip a leading `@` so the built-in aliases
        // (`me` / `today` / `week` / `month`) are matched
        // on the bare keyword — this matches the notes-mode
        // parser convention: `@me` and `me` both work.
        //
        // The user-defined fragment lookup below, however,
        // requires the token to be `@`-prefixed. An
        // unprefixed token (e.g. `kramfors` when
        // `jira.search.kramfors=...` is defined) is NOT
        // expanded — it's treated as plain text. The `@` is
        // a deliberate invocation, not an alias the parser
        // silently resolves. This is the user-visible
        // "define a search pattern in config, only expand
        // it when prefixed by `@`" contract.
        //
        // The asymmetry between built-ins (permissive)
        // and user-defined fragments (strict) is
        // deliberate: `me` and `today` are common words
        // and the historical permissive matching is
        // preserved for muscle memory; user-defined
        // patterns are typically named after project
        // words (labels, epics, etc.) that would collide
        // with free-text searches if expanded
        // unprefixed. If you'd rather require `@` for
        // everything, swap the order of the match arms
        // so the built-ins also check `tok.starts_with('@')`.
        //
        // NOTE: only the alias-match path uses the
        // stripped `bare`. Non-matching tokens fall
        // through with the ORIGINAL `tok` (the `@` is
        // preserved). That's a deliberate departure
        // from the notes-mode parser, which strips the
        // `@` from every non-matching `@`-prefixed
        // token too — but notes mode has to dodge the
        // note_search library's link-tokenizer, which
        // would otherwise route `@foo` to the links
        // column. JIRA mode has no such upstream
        // tokenizer to satisfy, so we keep the user's
        // literal text verbatim: a free-text search
        // for `@tody` is a different query from
        // `tody`.
        let bare = tok.strip_prefix('@').unwrap_or(tok);
        let lower = bare.to_ascii_lowercase();
        match lower.as_str() {
            "me" => me_alias = true,
            "today" => date_filter = Some(DateAlias::Today),
            "week" => date_filter = Some(DateAlias::Week),
            "month" => date_filter = Some(DateAlias::Month),
            _ => {
                // User-defined fragment lookup. The
                // gate on `tok.starts_with('@')` is
                // the bug fix: without it, an
                // unprefixed token would match a
                // fragment with the same name (the
                // `bare` variable above already had
                // the `@` stripped, so the lookup
                // key is identical either way). The
                // gate makes `@kramfors` expand to
                // `(labels in ("KR"))` while
                // `kramfors` is a free-text search
                // for the description / summary.
                if tok.starts_with('@') {
                    if let Some(fragment_jql) = fragments.get(&lower) {
                        fragment_clauses.push(format!("({})", fragment_jql));
                    } else {
                        // Not a recognised fragment. We
                        // record the original-case name
                        // for the diagnostic (preserves
                        // the user's casing in the
                        // status message) but only
                        // record the first occurrence
                        // to avoid spamming the user
                        // with duplicates of the same
                        // typo. The `is_built_in_alias`
                        // check filters out the four
                        // built-in aliases — typing
                        // `@me` is never a typo.
                        if !is_built_in_alias(&lower) {
                            let original = bare.to_string();
                            if !undefined_fragments
                                .iter()
                                .any(|n| n.eq_ignore_ascii_case(&original))
                            {
                                undefined_fragments.push(original);
                            }
                        }
                        // Not an alias; fall through to
                        // the existing key /
                        // field-value / free-text
                        // classifier. Push the
                        // ORIGINAL token (with the
                        // leading `@` preserved) per
                        // the comment above.
                        if key_re.is_match(tok) {
                            keys.push(tok);
                        } else if let Some(caps) = kv_re.captures(tok) {
                            let field = caps.get(1).unwrap().as_str();
                            let value = caps.get(2).unwrap().as_str();
                            if field.eq_ignore_ascii_case("project") {
                                explicit_project = Some(value);
                            } else {
                                kvs.push((field, value));
                            }
                        } else {
                            text.push(tok);
                        }
                    }
                } else {
                    // Unprefixed token — not a
                    // fragment, not a built-in alias.
                    // Run the key / field-value /
                    // free-text classifier directly.
                    // This is the `kramfors` (no `@`)
                    // case: the user's config has
                    // `jira.search.kramfors=labels in
                    // ("KR")`, but `kramfors` is a
                    // free-text search for the
                    // description / summary, not a
                    // fragment expansion. Users invoke
                    // the fragment explicitly as
                    // `@kramfors`.
                    if key_re.is_match(tok) {
                        keys.push(tok);
                    } else if let Some(caps) = kv_re.captures(tok) {
                        let field = caps.get(1).unwrap().as_str();
                        let value = caps.get(2).unwrap().as_str();
                        if field.eq_ignore_ascii_case("project") {
                            explicit_project = Some(value);
                        } else {
                            kvs.push((field, value));
                        }
                    } else {
                        text.push(tok);
                    }
                }
            }
        }
    }
    let mut parts: Vec<String> = Vec::new();
    // A bare issue-key query is globally unique —
    // scoping it to a (possibly wrong) default project
    // would make the search fail. When the query
    // contains only issue keys, omit the project clause.
    let is_keys_only = !keys.is_empty()
        && explicit_project.is_none()
        && kvs.is_empty()
        && text.is_empty()
        && !me_alias
        && date_filter.is_none()
        && fragment_clauses.is_empty();
    // Project clause: explicit `project=...` from the
    // query takes precedence over the default project.
    let project_value = if is_keys_only {
        None
    } else {
        explicit_project.or(default_project)
    };
    if let Some(p) = project_value {
        parts.push(format!("project = {}", escape_jql_string(p)));
    }
    // Alias clauses come next, in a stable order: the
    // assignee filter (if any) before the date filter
    // (if any). This ordering is stable across runs so
    // tests can assert exact equality on the JQL string.
    if me_alias {
        parts.push("assignee = currentUser()".to_string());
    }
    if let Some(alias) = date_filter {
        parts.push(format!(
            "updated >= {}",
            escape_jql_string(&date_cutoff(alias, now_epoch))
        ));
    }
    // User-defined fragment clauses, in the order the
    // user typed them. Spliced after the built-in
    // aliases so a `@me @sprint` query reliably
    // produces `assignee = currentUser() AND (sprint = X) ...`.
    parts.extend(fragment_clauses);
    match keys.len() {
        0 => {}
        1 => parts.push(format!("key = {}", keys[0])),
        _ => {
            let list = keys.join(", ");
            parts.push(format!("key in ({})", list));
        }
    }
    for (f, v) in &kvs {
        // Field names are restricted to `\w+` by the
        // classifier, so they're always safe JQL
        // identifiers. Values are quoted.
        parts.push(format!("{} = {}", f, escape_jql_string(v)));
    }
    for t in &text {
        parts.push(format!(
            "(description ~ {} OR summary ~ {})",
            escape_jql_string(t),
            escape_jql_string(t)
        ));
    }
    if parts.is_empty() {
        // Body was all whitespace after trim? Already handled,
        // but be defensive.
        return build_jql("", default_project, now_epoch, fragments);
    }
    (
        format!("{} ORDER BY updated DESC", parts.join(" AND ")),
        undefined_fragments,
    )
}

/// True if `lower` is the lowercase form of one of
/// the four built-in aliases (`me`, `today`, `week`,
/// `month`). Used by `build_jql` to skip the
/// "undefined fragment" diagnostic for `@me` / `@today`
/// / etc. when the user has just typed the bare token
/// (without a config-defined fragment of the same
/// name) — those aren't typos, they're built-ins.
fn is_built_in_alias(lower: &str) -> bool {
    matches!(lower, "me" | "today" | "week" | "month")
}

/// The recognised date-alias kinds, in the order the
/// `build_jql` parser resolves them. The enum is
/// module-private because it's only used to drive
/// `date_cutoff`; callers see the resulting JQL
/// string directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateAlias {
    Today,
    Week,
    Month,
}

/// Compute the JQL date-cutoff string for a date
/// alias, in `YYYY-MM-DD` form. The cutoff is
/// `today - N days` in UTC. JQL's date-only literal
/// is interpreted as midnight UTC, so the cutoff
/// represents "updated on or after this date".
///
/// We use UTC because `chrono::Utc::now()` is what
/// we have without an explicit timezone; JIRA
/// servers that interpret dates in a different
/// timezone may show a one-day boundary mismatch
/// at the edges, but the alias semantics ("updated
/// in the last N days") are close enough for
/// practical use. Document this in the function
/// comment so future maintainers know where to
/// look if a user reports a one-day skew.
fn date_cutoff(alias: DateAlias, now_epoch: i64) -> String {
    use chrono::{DateTime, Utc};
    let days = match alias {
        DateAlias::Today => JIRA_ALIAS_TODAY_DAYS,
        DateAlias::Week => JIRA_ALIAS_WEEK_DAYS,
        DateAlias::Month => JIRA_ALIAS_MONTH_DAYS,
    };
    let now: DateTime<Utc> = DateTime::<Utc>::from_timestamp(now_epoch, 0).unwrap_or_else(|| {
        // Negative epoch (pre-1970) is unreachable
        // in practice, but if it ever happens, fall
        // back to the Unix epoch and let JIRA
        // complain if it cares.
        DateTime::<Utc>::from_timestamp(0, 0).unwrap()
    });
    let cutoff = now - chrono::Duration::days(days);
    cutoff.format("%Y-%m-%d").to_string()
}

/// Quote a string for use as a JQL string literal: wrap in
/// double quotes, escape backslash → `\\` and double-quote →
/// `\"` (the two characters JQL string literals treat as
/// escapes). Everything else passes through verbatim.
fn escape_jql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parse an ISO-8601 timestamp (as JIRA returns it, e.g.
/// `2024-06-30T19:14:39.000+0000`) into a Unix epoch
/// seconds value, for sorting/comparison. Returns 0 on any
/// parse failure (a malformed timestamp sorts as "oldest",
/// which is the safe degradation — the issue still shows,
/// just at the bottom).
pub fn updated_to_epoch(iso: &str) -> i64 {
    let s = iso.trim();
    if s.is_empty() {
        return 0;
    }
    // JIRA returns `...+0000` (no colon in the offset);
    // RFC 3339 (what `chrono`'s `parse_from_rfc3339`
    // expects) requires `...+00:00`. Normalize by
    // inserting a colon inside the trailing `+HHMM` /
    // `-HHMM`. If the string doesn't end in an offset of
    // that exact shape we leave it verbatim and let the
    // parser fail (returning 0).
    let normalized = normalize_offset(s).unwrap_or_else(|| s.to_string());
    chrono::DateTime::parse_from_rfc3339(&normalized)
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

/// If `s` ends in `…<+/->HHMM` (no colon), return the
/// `…<+/->HH:MM` form. `None` if the tail doesn't match
/// the offset shape.
fn normalize_offset(s: &str) -> Option<String> {
    if s.len() < 6 {
        return None;
    }
    let tail = &s[s.len() - 5..];
    let sign = tail.chars().next()?;
    if sign != '+' && sign != '-' {
        return None;
    }
    let hh = tail.get(1..3)?;
    let mm = tail.get(3..5)?;
    if !hh.chars().all(|c| c.is_ascii_digit()) || !mm.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let head = &s[..s.len() - 5];
    Some(format!("{}{}{}:{}", head, sign, hh, mm))
}

mod urlencoding {
    /// Percent-encode a JQL string for the `jql` query
    /// parameter. Kept in-module (matching `note_search`'s
    /// `jira.rs`) so we don't pull a dedicated crate; the
    /// set of chars that need encoding is small.
    pub fn encode(s: &str) -> String {
        s.chars()
            .map(|c| match c {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
                _ => format!("%{:02X}", c as u8),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
