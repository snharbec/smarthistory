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
    fn fetch_comments(
        &self,
        key: &str,
    ) -> Result<Vec<JiraComment>, JiraError>;

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
    fn add_comment(
        &self,
        key: &str,
        body: &str,
    ) -> Result<(), JiraError>;
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

    /// The browse URL for a single issue key: `{url}/{key}`.
    /// The url's trailing slash is trimmed first.
    pub fn browse_url(&self, key: &str) -> String {
        format!("{}/{}", self.url.trim_end_matches('/'), key)
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
        let client_builder = Client::builder()
            .timeout(std::time::Duration::from_secs(15));
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
                let password = self
                    .config
                    .certificate_password
                    .as_deref()
                    .unwrap_or("");
                let identity = reqwest::Identity::from_pkcs12_der(&der, password)
                    .map_err(|e| {
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
    fn fetch_comments(
        &self,
        key: &str,
    ) -> Result<Vec<JiraComment>, JiraError> {
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
        let client_builder = Client::builder()
            .timeout(std::time::Duration::from_secs(15));
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
                let password = self
                    .config
                    .certificate_password
                    .as_deref()
                    .unwrap_or("");
                let identity = reqwest::Identity::from_pkcs12_der(&der, password)
                    .map_err(|e| {
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
        let body = resp.text().map_err(|e| {
            JiraError::Http(e.to_string())
        })?;
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
    fn add_comment(
        &self,
        key: &str,
        body: &str,
    ) -> Result<(), JiraError> {
        
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
            return Err(JiraError::Api(format!(
                "{}: {}",
                status,
                excerpt.trim()
            )));
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
        let body = a
            .body
            .as_ref()
            .map(extract_adf_text)
            .unwrap_or_default();
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
        let updated = a
            .updated
            .unwrap_or_else(|| created.clone());
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

fn extract_adf_text_into(
    value: &serde_json::Value,
    out: &mut String,
    _in_paragraph: bool,
) {
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
            let node_type = map
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("");
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
    let key_re = regex::Regex::new(r"^\w+-\d+$").expect("static regex");
    let kv_re = regex::Regex::new(r"^(\w+)=(.*)$").expect("static regex");
    let mut keys: Vec<&str> = Vec::new();
    let mut kvs: Vec<(&str, &str)> = Vec::new();
    let mut text: Vec<&str> = Vec::new();
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
        // Strip a leading `@` so the alias is matched
        // on the bare keyword. This matches the
        // notes-mode parser convention: `@me` and `me`
        // both work.
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
                // Not a built-in alias. Check if it's a
                // user-defined fragment (case-insensitive
                // lookup). The fragment's JQL is trusted
                // verbatim — the user wrote it in their
                // config — so we wrap it in parens
                // defensively to keep any internal
                // `AND` / `OR` operators from breaking
                // the top-level AND-join.
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
                    // typo.
                    if !is_built_in_alias(&lower) && tok.starts_with('@') {
                        let original = bare.to_string();
                        if !undefined_fragments
                            .iter()
                            .any(|n| n.eq_ignore_ascii_case(&original))
                        {
                            undefined_fragments.push(original);
                        }
                    }
                    // Not an alias; fall through to the
                    // existing key / field-value /
                    // free-text classifier. Push the
                    // ORIGINAL token (with the leading
                    // `@` preserved) per the comment
                    // above.
                    if key_re.is_match(tok) {
                        keys.push(tok);
                    } else if let Some(caps) = kv_re.captures(tok) {
                        kvs.push((caps.get(1).unwrap().as_str(), caps.get(2).unwrap().as_str()));
                    } else {
                        text.push(tok);
                    }
                }
            }
        }
    }
    let mut parts: Vec<String> = Vec::new();
    // Always scope to the default project when one is
    // configured, so free-text searches don't leak
    // results from other projects.
    if let Some(p) = default_project {
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
mod tests {
    use super::*;

    /// Fixed "now" used by every `build_jql` test.
    /// Choosing a non-zero value that maps to a known
    /// UTC date (2024-06-30 19:14:39) makes the
    /// date-cutoff strings the alias tests assert
    /// against stable and reproducible across runs.
    /// This is the same instant the `updated_to_epoch`
    /// test uses, so a single epoch underpins both
    /// layers of the test suite.
    const TEST_NOW_EPOCH: i64 = 1_719_774_879;

    /// Empty fragment map shared by every test that
    /// doesn't care about the fragment feature. The
    /// `build_jql` signature takes a fragments map;
    /// passing an empty one is the "no user-defined
    /// fragments" default and is the most common
    /// shape for existing tests.
    fn empty_fragments() -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }

    /// Convenience wrapper that discards the
    /// undefined-fragments return value. Existing
    /// tests that don't exercise the fragment
    /// feature use this so the body of every test
    /// (including its `assert_eq!` against an
    /// exact JQL string) keeps the simple form
    /// it had before `build_jql` started returning
    /// a tuple. Tests that DO care about undefined
    /// fragments call `build_jql` directly.
    fn call_jql(
        body: &str,
        default_project: Option<&str>,
        now_epoch: i64,
        fragments: &std::collections::HashMap<String, String>,
    ) -> String {
        build_jql(body, default_project, now_epoch, fragments).0
    }

    // ---- build_jql ----

    #[test]
    fn build_jql_empty_body_uses_default_project() {
        assert_eq!(
            call_jql("", Some("PROJ"), TEST_NOW_EPOCH, &empty_fragments()),
            r#"project = "PROJ" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_empty_body_no_project_is_global_recent() {
        assert_eq!(
            call_jql("", None, TEST_NOW_EPOCH, &empty_fragments()),
            "ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_single_issue_key() {
        assert_eq!(
            call_jql("PROJ-123", None, TEST_NOW_EPOCH, &empty_fragments()),
            "key = PROJ-123 ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_multiple_issue_keys_collapse_into_in() {
        assert_eq!(
            call_jql("PROJ-1 PROJ-2", None, TEST_NOW_EPOCH, &empty_fragments()),
            "key in (PROJ-1, PROJ-2) ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_field_value_quoted() {
        assert_eq!(
            call_jql("project=PROJ", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"project = "PROJ" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_empty_field_value() {
        // `project=` (empty value) is a valid token by the
        // `\w+=\S*` classifier; value is the empty string.
        assert_eq!(
            call_jql("assignee=", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"assignee = "" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_free_text_searches_description_or_summary() {
        assert_eq!(
            call_jql("login", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"(description ~ "login" OR summary ~ "login") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_combines_all_three_groups_with_and() {
        // keys first, then field=value, then free text.
        assert_eq!(
            call_jql(
                "PROJ-123 project=PROJ crash",
                None,
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"key = PROJ-123 AND project = "PROJ" AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_multiple_free_text_tokens_are_anded() {
        assert_eq!(
            call_jql("login crash", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"(description ~ "login" OR summary ~ "login") AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_escapes_quotes_and_backslashes_in_text() {
        // A free-text token containing `"` and `\` must be
        // escaped so it's a valid JQL string literal.
        let jql = call_jql(r#"a"b\c"#, None, TEST_NOW_EPOCH, &empty_fragments());
        assert!(jql.contains(r#"description ~ "a\"b\\c""#), "{}", jql);
    }

    #[test]
    fn build_jql_whitespace_only_falls_back_to_default() {
        assert_eq!(
            call_jql("   ", Some("PROJ"), TEST_NOW_EPOCH, &empty_fragments()),
            r#"project = "PROJ" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_free_text_with_default_project_is_scoped() {
        // When a default project is configured, free-text
        // searches must NOT leak results from other projects.
        assert_eq!(
            call_jql("crash", Some("PROJ"), TEST_NOW_EPOCH, &empty_fragments()),
            r#"project = "PROJ" AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_issue_key_with_default_project_is_scoped() {
        assert_eq!(
            call_jql("PROJ-123", Some("PROJ"), TEST_NOW_EPOCH, &empty_fragments()),
            r#"project = "PROJ" AND key = PROJ-123 ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_field_value_with_default_project_is_scoped() {
        assert_eq!(
            call_jql(
                "assignee=alice",
                Some("PROJ"),
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"project = "PROJ" AND assignee = "alice" ORDER BY updated DESC"#
        );
    }

    // ---- build_jql: aliases (@me, @today, @week, @month) ----
    //
    // The expected date strings below are computed from
    // TEST_NOW_EPOCH (= 2024-06-30 19:14:39 UTC):
    //   @today  -> 2024-06-29 (today - 1 day)
    //   @week   -> 2024-06-23 (today - 7 days)
    //   @month  -> 2024-05-30 (today - 31 days)
    // If TEST_NOW_EPOCH changes, update these literals
    // in lock-step.

    #[test]
    fn build_jql_at_me_becomes_current_user() {
        assert_eq!(
            call_jql("@me", None, TEST_NOW_EPOCH, &empty_fragments()),
            "assignee = currentUser() ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_at_me_no_at_prefix_also_works() {
        // The notes-mode parser convention: both `@me`
        // and the bare `me` keyword work.
        assert_eq!(
            call_jql("me", None, TEST_NOW_EPOCH, &empty_fragments()),
            "assignee = currentUser() ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_at_today_uses_yesterday_date() {
        assert_eq!(
            call_jql("@today", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"updated >= "2024-06-29" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_at_week_uses_today_minus_7() {
        assert_eq!(
            call_jql("@week", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"updated >= "2024-06-23" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_at_month_uses_today_minus_31() {
        // Spec: `@month` looks back 31 days (vs the
        // notes-mode precedent of 30). The constant
        // JIRA_ALIAS_MONTH_DAYS owns the policy.
        assert_eq!(
            call_jql("@month", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"updated >= "2024-05-30" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_aliases_are_case_insensitive() {
        // @Today, @TODAY, @tOdAy all match.
        assert_eq!(
            call_jql("@TODAY", None, TEST_NOW_EPOCH, &empty_fragments()),
            call_jql("@today", None, TEST_NOW_EPOCH, &empty_fragments()),
        );
        assert_eq!(
            call_jql("@Me", None, TEST_NOW_EPOCH, &empty_fragments()),
            call_jql("@me", None, TEST_NOW_EPOCH, &empty_fragments()),
        );
    }

    #[test]
    fn build_jql_aliases_stripped_from_body() {
        // After alias resolution, the body must be
        // empty of alias tokens — they don't fall
        // through to free text. A typo'd alias
        // (e.g. `@tody`) would still fall through;
        // see `build_jql_unknown_alias_falls_through`.
        let jql = call_jql("@me @today crash", None, TEST_NOW_EPOCH, &empty_fragments());
        assert!(!jql.contains("@me"));
        assert!(!jql.contains("@today"));
        // The free-text token survives.
        assert!(jql.contains(r#"(description ~ "crash" OR summary ~ "crash")"#));
    }

    #[test]
    fn build_jql_unknown_alias_falls_through_to_free_text() {
        // A token like `@tody` isn't a recognised
        // alias. It is NOT stripped of its leading
        // `@` — the user's literal text is preserved
        // in the JQL. This is a deliberate departure
        // from the notes-mode parser, which DOES
        // strip leading `@` from unknown tokens to
        // route them past the note_search library's
        // link-tokenizer. JIRA mode has no upstream
        // tokenizer to satisfy, so we keep the user's
        // text verbatim: a free-text search for
        // `@tody` is a different query from `tody`.
        let jql = call_jql("@tody", None, TEST_NOW_EPOCH, &empty_fragments());
        assert_eq!(
            jql,
            r#"(description ~ "@tody" OR summary ~ "@tody") ORDER BY updated DESC"#
        );
        // No alias fired.
        assert!(!jql.contains("updated >="));
        assert!(!jql.contains("currentUser"));
    }

    #[test]
    fn build_jql_email_like_tokens_are_not_aliases() {
        // `email@today` must NOT be treated as the
        // `@today` alias. The parser only strips a
        // leading `@`; the bare keyword must be the
        // whole token. So `email@today` stays
        // intact and falls through to free text.
        let jql = call_jql("user@today", None, TEST_NOW_EPOCH, &empty_fragments());
        // No `updated >=` clause (the alias didn't fire).
        assert!(!jql.contains("updated >="));
        // The token survives verbatim as free text.
        assert!(jql.contains("user@today"));
    }

    #[test]
    fn build_jql_compound_alias_tokens_are_not_aliases() {
        // `@todayfile` is a single token that
        // doesn't equal `today` (whole-word match).
        // Falls through to free text.
        let jql = call_jql("@todayfile", None, TEST_NOW_EPOCH, &empty_fragments());
        assert!(!jql.contains("updated >="));
        assert!(jql.contains("@todayfile"));
    }

    #[test]
    fn build_jql_date_aliases_last_one_wins() {
        // `@today @week` resolves to @week (last write
        // wins). Same convention as notes mode.
        assert_eq!(
            call_jql("@today @week", None, TEST_NOW_EPOCH, &empty_fragments()),
            call_jql("@week", None, TEST_NOW_EPOCH, &empty_fragments()),
        );
        assert_eq!(
            call_jql(
                "@week @today @month",
                None,
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            call_jql("@month", None, TEST_NOW_EPOCH, &empty_fragments()),
        );
    }

    #[test]
    fn build_jql_at_me_combines_with_date_alias() {
        // `@me` and `@week` are orthogonal; both
        // clauses should appear, AND-joined.
        // Ordering: project (none) -> @me ->
        // @date -> ...; the test asserts the
        // exact JQL string.
        assert_eq!(
            call_jql("@me @week", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"assignee = currentUser() AND updated >= "2024-06-23" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_all_four_aliases_together() {
        assert_eq!(
            call_jql(
                "@me @today @week @month",
                None,
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            // @month is the resolved date filter
            // (last-one-wins); @me is the assignee.
            r#"assignee = currentUser() AND updated >= "2024-05-30" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_alias_with_default_project() {
        // The default-project clause is always
        // prepended, before the alias clauses.
        assert_eq!(
            call_jql(
                "@me @week",
                Some("PROJ"),
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"project = "PROJ" AND assignee = currentUser() AND updated >= "2024-06-23" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_alias_with_field_value() {
        // A regular `field=value` clause coexists
        // with the alias clauses; ordering: project
        // (none) -> @me -> @date -> keys -> kvs -> text.
        assert_eq!(
            call_jql(
                "@me @week status=Open",
                None,
                TEST_NOW_EPOCH,
                &empty_fragments(),
            ),
            r#"assignee = currentUser() AND updated >= "2024-06-23" AND status = "Open" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_alias_with_issue_key() {
        // `@me PROJ-1` -> only PROJ-1, but only
        // when the user is the assignee. Useful
        // for "is this issue mine?".
        assert_eq!(
            call_jql("@me PROJ-1", None, TEST_NOW_EPOCH, &empty_fragments()),
            "assignee = currentUser() AND key = PROJ-1 ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_alias_with_free_text() {
        assert_eq!(
            call_jql("@me @week crash", None, TEST_NOW_EPOCH, &empty_fragments(),),
            r#"assignee = currentUser() AND updated >= "2024-06-23" AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    // ---- build_jql: fragments (jira.search.<name>=<jql>) ----
    //
    // Fragments are user-defined JQL snippets loaded
    // from the config file. They are spliced into the
    // query verbatim (no JQL-quoting) when the body
    // contains `@<name>`. Each fragment is wrapped in
    // parens so internal `AND` / `OR` doesn't break
    // the top-level AND-join.
    //
    // The expected JQL strings in these tests assume
    // the fragment map below (`fragments_with_labels`).
    // Tests that exercise undefined fragments use
    // `&empty_fragments()` to assert the error path.

    /// Fragment map shared by every "happy path"
    /// test below. Three entries of varying
    /// complexity:
    /// - `label1`: a single JQL clause with an
    ///   equals sign (the user's example).
    /// - `sprint`: a single clause, different
    ///   operator, to confirm the parser doesn't
    ///   special-case `=`.
    /// - `complex`: contains an internal `AND`
    ///   so the paren-wrapping is observable in
    ///   the resulting JQL.
    fn fragments_with_labels() -> std::collections::HashMap<String, String> {
        let mut m = std::collections::HashMap::new();
        m.insert("label1".to_string(), r#"labels = "test""#.to_string());
        m.insert("sprint".to_string(), "sprint = \"Sprint 42\"".to_string());
        m.insert(
            "complex".to_string(),
            r#"priority = High AND labels = "security""#.to_string(),
        );
        m
    }

    #[test]
    fn build_jql_simple_fragment_substituted() {
        // The user's example: `jira.search.label1=labels = "test"`,
        // invoked as `@label1` in the body.
        let (jql, undefined) = build_jql("@label1", None, TEST_NOW_EPOCH, &fragments_with_labels());
        assert_eq!(jql, r#"(labels = "test") ORDER BY updated DESC"#);
        // A recognised fragment never appears in the
        // undefined list (that's the whole point of
        // looking it up before recording).
        assert!(undefined.is_empty());
    }

    #[test]
    fn build_jql_fragment_case_insensitive_lookup() {
        // `@Label1` and `@LABEL1` both resolve to
        // the same fragment — the parser lowercases
        // the lookup key.
        let (jql_upper, _) = build_jql("@LABEL1", None, TEST_NOW_EPOCH, &fragments_with_labels());
        let (jql_lower, _) = build_jql("@label1", None, TEST_NOW_EPOCH, &fragments_with_labels());
        assert_eq!(jql_upper, jql_lower);
    }

    #[test]
    fn build_jql_fragment_combines_with_aliases() {
        // `@label1 @me` -> both the fragment and
        // the `@me` alias appear, AND-joined.
        let (jql, undefined) = build_jql(
            "@label1 @me",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert_eq!(
            jql,
            r#"assignee = currentUser() AND (labels = "test") ORDER BY updated DESC"#
        );
        assert!(undefined.is_empty());
    }

    #[test]
    fn build_jql_fragment_with_project_default() {
        // Project clause prepended, then fragment.
        let (jql, _) = build_jql(
            "@label1",
            Some("PROJ"),
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert_eq!(
            jql,
            r#"project = "PROJ" AND (labels = "test") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_fragment_combines_with_field_value_and_text() {
        // `@label1 status=Open crash` -> fragment,
        // then field=value, then free text.
        let (jql, _) = build_jql(
            "@label1 status=Open crash",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert_eq!(
            jql,
            r#"(labels = "test") AND status = "Open" AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_complex_fragment_preserves_internal_and() {
        // The fragment value contains an internal
        // `AND`. The paren-wrap is what keeps the
        // top-level AND-join well-formed.
        let (jql, _) = build_jql("@complex", None, TEST_NOW_EPOCH, &fragments_with_labels());
        assert_eq!(
            jql,
            r#"(priority = High AND labels = "security") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_multiple_fragments_in_order() {
        // Two fragments, typed in order, appear in
        // that order. Order matters when a fragment
        // is asymmetrically selective (e.g. one
        // fragment filters by sprint, another by
        // assignee).
        let (jql, _) = build_jql(
            "@label1 @sprint",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert_eq!(
            jql,
            r#"(labels = "test") AND (sprint = "Sprint 42") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_undefined_fragment_recorded_in_list() {
        // A `@<name>` token that isn't in the map
        // is recorded in the second return value.
        // The JQL still falls through to free text
        // (so the function never produces a parse
        // error) — the caller decides whether to
        // fire the search anyway.
        let (jql, undefined) = build_jql("@nosuch", None, TEST_NOW_EPOCH, &fragments_with_labels());
        // The token survives verbatim (with the `@`)
        // in the free-text path.
        assert!(jql.contains(r#"(description ~ "@nosuch" OR summary ~ "@nosuch")"#));
        // The undefined list has the bare name
        // (without the `@`), in the user's casing.
        assert_eq!(undefined, vec!["nosuch".to_string()]);
    }

    #[test]
    fn build_jql_undefined_fragment_dedupes() {
        // Repeating the same undefined fragment in
        // the body reports it once, not N times.
        let (_, undefined) = build_jql(
            "@nosuch @nosuch @nosuch",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert_eq!(undefined, vec!["nosuch".to_string()]);
    }

    #[test]
    fn build_jql_undefined_fragments_preserve_first_appearance_order() {
        let (_, undefined) = build_jql(
            "@first @second @first @third @second",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert_eq!(
            undefined,
            vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string(),
            ],
        );
    }

    #[test]
    fn build_jql_built_in_alias_not_marked_undefined() {
        // `@me`, `@today`, `@week`, `@month` are
        // built-in aliases. Typing them without a
        // matching config entry is NOT a typo —
        // they're a valid query on their own.
        // The undefined list must stay empty.
        let (_, undefined) = build_jql("@me @today", None, TEST_NOW_EPOCH, &empty_fragments());
        assert!(undefined.is_empty(), "got {:?}", undefined);
    }

    #[test]
    fn build_jql_mix_defined_and_undefined_fragments() {
        // One defined fragment, one undefined.
        // The JQL has the defined fragment spliced
        // and the undefined one as free text. The
        // undefined list has only the missing one.
        let (jql, undefined) = build_jql(
            "@label1 @nosuch",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert!(jql.contains(r#"(labels = "test")"#));
        assert!(jql.contains(r#"(description ~ "@nosuch" OR summary ~ "@nosuch")"#));
        assert_eq!(undefined, vec!["nosuch".to_string()]);
    }

    #[test]
    fn build_jql_empty_fragments_map_does_not_error() {
        // The default case: no user-defined
        // fragments. Every `@`-prefixed token in
        // the body is either a built-in alias or
        // falls through to free text.
        let (jql, undefined) = build_jql("@label1 @me", None, TEST_NOW_EPOCH, &empty_fragments());
        // `@label1` falls through to free text.
        assert!(jql.contains(r#"(description ~ "@label1" OR summary ~ "@label1")"#));
        // `@me` produces its clause.
        assert!(jql.contains("assignee = currentUser()"));
        // `label1` is reported as undefined.
        assert_eq!(undefined, vec!["label1".to_string()]);
    }

    #[test]
    fn build_jql_fragment_alone_in_body_omits_project() {
        // Without a body at all the empty-body
        // branch fires (server-wide or
        // project-scoped). With just `@label1` the
        // parser DOES run (the body is non-empty)
        // and produces `(labels = "test") ...`
        // without a project clause.
        let (jql, _) = build_jql("@label1", None, TEST_NOW_EPOCH, &fragments_with_labels());
        assert!(!jql.contains("project = "));
        assert!(jql.starts_with("(labels = "));
    }

    #[test]
    fn build_jql_empty_body_with_fragments_is_project_or_global() {
        // `-` alone (empty body) is the
        // "all aliases" / "no body" path; fragments
        // defined in the config are NOT
        // auto-included in an empty body. The
        // user has to type the fragment name
        // explicitly. This matches the other
        // built-in aliases: `-` alone shows
        // everything; `-@me` shows just the
        // user's tickets.
        let (jql_no_proj, _) = build_jql("", None, TEST_NOW_EPOCH, &fragments_with_labels());
        assert_eq!(jql_no_proj, "ORDER BY updated DESC");

        let (jql_with_proj, _) =
            build_jql("", Some("PROJ"), TEST_NOW_EPOCH, &fragments_with_labels());
        assert_eq!(jql_with_proj, r#"project = "PROJ" ORDER BY updated DESC"#);
    }

    #[test]
    fn build_jql_undefined_fragment_after_alias_does_not_clobber() {
        // An alias fires correctly even when
        // there's also an undefined fragment in
        // the body.
        let (jql, undefined) = build_jql("@me @nosuch", None, TEST_NOW_EPOCH, &empty_fragments());
        assert!(jql.contains("assignee = currentUser()"));
        assert!(jql.contains(r#"(description ~ "@nosuch" OR summary ~ "@nosuch")"#));
        assert_eq!(undefined, vec!["nosuch".to_string()]);
    }

    // ---- escape_jql_string ----

    #[test]
    fn escape_jql_string_quotes_plain() {
        assert_eq!(escape_jql_string("hello"), r#""hello""#);
    }

    #[test]
    fn escape_jql_string_escapes_backslash_and_quote() {
        assert_eq!(escape_jql_string(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    // ---- updated_to_epoch ----

    #[test]
    fn updated_to_epoch_parses_jira_offset() {
        // JIRA's `+0000` form (no colon).
        let e = updated_to_epoch("2024-06-30T19:14:39.000+0000");
        assert!(e > 1_700_000_000, "epoch should be in 2024+, got {}", e);
    }

    #[test]
    fn updated_to_epoch_parses_rfc3339() {
        let e = updated_to_epoch("2024-06-30T19:14:39.000+00:00");
        assert!(e > 1_700_000_000);
    }

    #[test]
    fn updated_to_epoch_empty_is_zero() {
        assert_eq!(updated_to_epoch(""), 0);
    }

    #[test]
    fn updated_to_epoch_garbage_is_zero() {
        assert_eq!(updated_to_epoch("not a date"), 0);
    }

    // ---- JiraConfig::browse_url ----

    #[test]
    fn browse_url_appends_key_after_trimming_slash() {
        let cfg = JiraConfig {
            server: "https://jira.internal".to_string(),
            token: "tok".to_string(),
            url: "https://jira.company.com/browse/".to_string(),
            project: None,
            max_results: 5,
            certificate_path: None,
            certificate_password: None,
            ca_certificate_path: None,
        };
        assert_eq!(
            cfg.browse_url("PROJ-123"),
            "https://jira.company.com/browse/PROJ-123"
        );
    }

    #[test]
    fn browse_url_default_uses_server_when_url_unset() {
        // Constructed directly (from_env would fall back too,
        // but that depends on the live environment).
        let cfg = JiraConfig {
            server: "https://jira".to_string(),
            token: "t".to_string(),
            url: "https://jira".to_string(),
            project: None,
            max_results: 5,
            certificate_path: None,
            certificate_password: None,
            ca_certificate_path: None,
        };
        assert_eq!(cfg.browse_url("X-1"), "https://jira/X-1");
    }

    // ---- JSON parsing ----

    #[test]
    fn parse_search_response_minimal() {
        let json = r#"{"issues":[{"key":"PROJ-1","fields":{"summary":"s"}}]}"#;
        let parsed: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.issues.len(), 1);
        assert_eq!(parsed.issues[0].key, "PROJ-1");
        let issue = JiraIssue::from(ApiIssue {
            key: parsed.issues[0].key.clone(),
            fields: parsed.issues[0].fields.clone(),
        });
        assert_eq!(issue.summary, "s");
        assert_eq!(issue.status, ""); // absent → empty
    }

    #[test]
    fn parse_search_response_full_fields() {
        let json = r#"{"issues":[{"key":"PROJ-2","fields":{
            "summary":"boom","status":{"name":"Done"},
            "issuetype":{"name":"Bug"},"priority":{"name":"High"},
            "assignee":{"name":"Alice"},
            "updated":"2024-06-30T19:14:39.000+0000",
            "duedate":"2024-07-15",
            "description":{"type":"doc","version":1,"content":[
                {"type":"paragraph","content":[
                    {"type":"text","text":"Hello "},
                    {"type":"text","text":"world."}
                ]}
            ]}
        }}]}"#;
        let parsed: SearchResponse = serde_json::from_str(json).unwrap();
        let issue = JiraIssue::from(parsed.issues.into_iter().next().unwrap());
        assert_eq!(issue.key, "PROJ-2");
        assert_eq!(issue.status, "Done");
        assert_eq!(issue.issuetype, "Bug");
        assert_eq!(issue.priority, "High");
        assert_eq!(issue.assignee, "Alice");
        assert!(updated_to_epoch(&issue.updated) > 0);
        // Due date flows through verbatim.
        assert_eq!(issue.due, "2024-07-15");
        // The ADF description is walked and
        // flattened to plain text.
        assert_eq!(issue.description, "Hello world.");
    }

    /// The `duedate` and `description` fields are
    /// optional on every issue. Both `null` and
    /// absent must degrade to empty strings —
    /// the From impl wraps both in
    /// `unwrap_or_default()` and the description
    /// extractor returns empty for null/missing.
    #[test]
    fn parse_search_response_due_and_description_optional() {
        let json = r#"{"issues":[
            {"key":"PROJ-A","fields":{"summary":"no due"}},
            {"key":"PROJ-B","fields":{"summary":"null due",
              "duedate":null,"description":null}},
            {"key":"PROJ-C","fields":{"summary":"empty desc",
              "description":{}}}
        ]}"#;
        let parsed: SearchResponse = serde_json::from_str(json).unwrap();
        let issues: Vec<JiraIssue> = parsed.issues.into_iter().map(JiraIssue::from).collect();
        // Absent `duedate` and `description` →
        // empty strings.
        assert_eq!(issues[0].due, "");
        assert_eq!(issues[0].description, "");
        // Explicit `null` for both → empty strings.
        assert_eq!(issues[1].due, "");
        assert_eq!(issues[1].description, "");
        // An empty `description` object (no `type`
        // and no `content`) → empty string.
        assert_eq!(issues[2].description, "");
    }

    fn parse_search_response_null_fields_dont_fail() {
        let json = r#"{"issues":[
            {"key":"PROJ-1","fields":{"summary":null,"updated":null}},
            {"key":"PROJ-2","fields":{"summary":"ok","updated":"2024-06-30T19:14:39.000+0000","status":{"name":null}}}
        ]}"#;
        let parsed: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.issues.len(), 2);
        let i1 = JiraIssue::from(parsed.issues.into_iter().next().unwrap());
        assert_eq!(i1.key, "PROJ-1");
        assert_eq!(i1.summary, "");
        assert_eq!(i1.updated, "");
    }

    /// Some JIRA events / webhooks / older versions
    /// return issues with an empty or missing `fields`
    /// object. Must not fail the search.
    #[test]
    fn parse_search_response_empty_fields_dont_fail() {
        let json = r#"{"issues":[{"key":"PROJ-A","fields":{}},{"key":"PROJ-B","fields":{"summary":"ok"}}]}"#;
        let parsed: SearchResponse = serde_json::from_str(json).unwrap();
        let issues: Vec<JiraIssue> = parsed.issues.into_iter().map(JiraIssue::from).collect();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].key, "PROJ-A");
        assert_eq!(issues[0].summary, "");
        assert_eq!(issues[0].status, "");
        assert_eq!(issues[1].summary, "ok");
    }

    // ---- comment response parsing ----

    /// A typical comment response with two
    /// comments. Both have authors, full ADF
    /// bodies, and ISO-8601 timestamps.
    #[test]
    fn parse_comments_response_full() {
        let json = r#"{
            "startAt": 0,
            "maxResults": 100,
            "total": 2,
            "comments": [
                {
                    "id": "10001",
                    "author": {"name": "Alice"},
                    "body": {"type":"doc","version":1,"content":[
                        {"type":"paragraph","content":[
                            {"type":"text","text":"First comment."}
                        ]}
                    ]},
                    "created": "2024-06-30T19:14:39.000+0000",
                    "updated": "2024-06-30T19:14:39.000+0000"
                },
                {
                    "id": "10002",
                    "author": {"name": "Bob"},
                    "body": {"type":"doc","version":1,"content":[
                        {"type":"paragraph","content":[
                            {"type":"text","text":"Second."}
                        ]}
                    ]},
                    "created": "2024-06-29T10:00:00.000+0000",
                    "updated": "2024-06-29T10:00:00.000+0000"
                }
            ]
        }"#;
        let parsed: CommentsResponse = serde_json::from_str(json).unwrap();
        let comments: Vec<JiraComment> =
            parsed.comments.into_iter().map(JiraComment::from).collect();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].id, "10001");
        assert_eq!(comments[0].author, "Alice");
        assert_eq!(comments[0].body, "First comment.");
        assert_eq!(comments[0].created, "2024-06-30T19:14:39.000+0000");
        assert_eq!(comments[0].updated, "2024-06-30T19:14:39.000+0000");
        assert_eq!(comments[1].author, "Bob");
        assert_eq!(comments[1].body, "Second.");
    }

    /// An empty `comments` array (the
    /// common case for issues with no
    /// comments) is parsed cleanly — the
    /// response shape tolerates the empty list
    /// because of the `#[serde(default)]` on
    /// the `comments` field.
    #[test]
    fn parse_comments_response_empty_list() {
        let json = r#"{"comments":[]}"#;
        let parsed: CommentsResponse = serde_json::from_str(json).unwrap();
        let comments: Vec<JiraComment> =
            parsed.comments.into_iter().map(JiraComment::from).collect();
        assert!(comments.is_empty());
    }

    /// `null` and missing optional fields
    /// (`id`, `author`, `body`, `created`,
    /// `updated`) must all degrade to empty
    /// strings, not fail the parse. JIRA's
    /// real responses frequently have system
    /// comments with most of these fields
    /// `null`.
    #[test]
    fn parse_comments_response_null_and_missing_fields() {
        let json = r#"{"comments":[
            {"id":null,"author":null,"body":null,"created":null,"updated":null},
            {"id":"10002","body":{}}
        ]}"#;
        let parsed: CommentsResponse = serde_json::from_str(json).unwrap();
        let comments: Vec<JiraComment> =
            parsed.comments.into_iter().map(JiraComment::from).collect();
        assert_eq!(comments.len(), 2);
        // First comment: every field is
        // either `null` or absent; all
        // degrade to empty strings.
        assert_eq!(comments[0].id, "");
        assert_eq!(comments[0].author, "");
        assert_eq!(comments[0].body, "");
        assert_eq!(comments[0].created, "");
        assert_eq!(comments[0].updated, "");
        // Second comment: `created`
        // missing — `updated` falls back
        // to `created` (which is empty).
        assert_eq!(comments[1].id, "10002");
        assert_eq!(comments[1].author, "");
        // An empty `body` object → empty
        // string (the extractor returns
        // empty for an object with no
        // `type` and no `content`).
        assert_eq!(comments[1].body, "");
        assert_eq!(comments[1].created, "");
        assert_eq!(comments[1].updated, "");
    }

    /// `author.name` can be `null` (rare
    /// but possible for system comments).
    /// The author field degrades to an empty
    /// string rather than failing the parse.
    #[test]
    fn parse_comments_response_author_name_null() {
        let json = r#"{"comments":[
            {"id":"1","author":{"name":null},"body":null,"created":"2024-06-30T00:00:00.000+0000"}
        ]}"#;
        let parsed: CommentsResponse = serde_json::from_str(json).unwrap();
        let comments: Vec<JiraComment> =
            parsed.comments.into_iter().map(JiraComment::from).collect();
        assert_eq!(comments[0].author, "");
    }

    // ---- extract_adf_text ----

    /// ADF helper: parse a JSON snippet into a
    /// `serde_json::Value` for the extractor. Lets
    /// the tests below use a compact inline
    /// notation without re-typing the
    /// `serde_json::from_str` boilerplate.
    fn adf(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("valid ADF JSON")
    }

    #[test]
    fn adf_empty_doc() {
        // A document with no `content` returns
        // an empty string. Common case for an
        // issue that was created with no
        // description.
        assert_eq!(extract_adf_text(&adf(r#"{"type":"doc","version":1}"#)), "");
    }

    #[test]
    fn adf_single_paragraph() {
        // The most common shape: a doc with one
        // paragraph containing one text node.
        assert_eq!(
            extract_adf_text(&adf(
                r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"Hello world."}
                    ]}
                ]}"#
            )),
            "Hello world."
        );
    }

    #[test]
    fn adf_multiple_paragraphs_joined_with_newline() {
        // Two paragraphs become one string with a
        // newline separator. (Earlier designs
        // folded to a single space, but the new
        // multi-line preview / overlay layout
        // wants the paragraph boundaries to
        // survive so the description body can
        // span multiple rendered lines.)
        assert_eq!(
            extract_adf_text(&adf(
                r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"First."}
                    ]},
                    {"type":"paragraph","content":[
                        {"type":"text","text":"Second."}
                    ]}
                ]}"#
            )),
            "First.\nSecond."
        );
    }

    #[test]
    fn adf_text_split_across_nodes_is_concatenated() {
        // JIRA often splits a single visual
        // sentence into multiple `text` nodes
        // (formatting marks between them). The
        // extractor concatenates them in order.
        assert_eq!(
            extract_adf_text(&adf(
                r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"The "},
                        {"type":"text","text":"quick "},
                        {"type":"text","text":"fox."}
                    ]}
                ]}"#
            )),
            "The quick fox."
        );
    }

    #[test]
    fn adf_mention_uses_attrs_text() {
        // A `@user` mention carries
        // `attrs.text` like `"@alice"`. The
        // extractor prefers that over a
        // hand-rolled `@`-prefix concatenation.
        assert_eq!(
            extract_adf_text(&adf(
                r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"Hi "},
                        {"type":"mention","attrs":{
                            "id":"5","text":"@alice","displayName":"Alice"
                        }},
                        {"type":"text","text":" please review."}
                    ]}
                ]}"#
            )),
            "Hi @alice please review."
        );
    }

    #[test]
    fn adf_mention_falls_back_to_display_name() {
        // No `attrs.text` — fall back to
        // `attrs.displayName` with an `@`
        // prefix. Common for Jira Service
        // Management customers whose mention
        // shape omits the `text` shorthand.
        assert_eq!(
            extract_adf_text(&adf(
                r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"mention","attrs":{
                            "id":"5","displayName":"Alice"
                        }}
                    ]}
                ]}"#
            )),
            "@Alice"
        );
    }

    #[test]
    fn adf_link_uses_child_text() {
        // A link with child text nodes renders
        // the child text. The href is silently
        // dropped (we have no way to render it
        // inline in a single-line preview
        // without breaking the line on
        // long URLs).
        assert_eq!(
            extract_adf_text(&adf(
                r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"See "},
                        {"type":"link","attrs":{
                            "href":"https://example.com/wonky/url"
                        },"content":[
                            {"type":"text","text":"docs"}
                        ]},
                        {"type":"text","text":" for more."}
                    ]}
                ]}"#
            )),
            "See docs for more."
        );
    }

    #[test]
    fn adf_link_falls_back_to_href_when_no_children() {
        // A bare link with no child text nodes
        // renders the href. Useful when an
        // author pasted a URL with no
        // descriptive text.
        assert_eq!(
            extract_adf_text(&adf(
                r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"link","attrs":{
                            "href":"https://example.com/wonky/url"
                        }}
                    ]}
                ]}"#
            )),
            "https://example.com/wonky/url"
        );
    }

    #[test]
    fn adf_emoji_renders_short_name() {
        // Emoji nodes carry `:smile:` style
        // short names in `attrs.shortName`. We
        // render them literally so the user
        // gets a hint that an emoji was in the
        // original.
        assert_eq!(
            extract_adf_text(&adf(
                r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"Hello "},
                        {"type":"emoji","attrs":{
                            "shortName":":wave:","id":"1f44b"
                        }}
                    ]}
                ]}"#
            )),
            "Hello :wave:"
        );
    }

    #[test]
    fn adf_hard_break_becomes_newline() {
        // A `hardBreak` inside a paragraph is a
        // soft line break — rendered as a real
        // newline so the author's line structure
        // survives. (Earlier designs folded to
        // a space, but the new multi-line
        // layout wants the line breaks to
        // survive.)
        assert_eq!(
            extract_adf_text(&adf(
                r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"line one"},
                        {"type":"hardBreak"},
                        {"type":"text","text":"line two"}
                    ]}
                ]}"#
            )),
            "line one\nline two"
        );
    }

    #[test]
    fn adf_bullet_list_items_joined() {
        // A bullet list flattens to multiple
        // lines, one per item. (The list-item
        // contains a paragraph, and paragraphs
        // are now separated by newlines in the
        // extractor output. Earlier designs
        // folded to spaces; the new multi-line
        // layout wants each item on its own
        // line in the rendered preview / overlay.)
        assert_eq!(
            extract_adf_text(&adf(
                r#"{"type":"doc","version":1,"content":[
                    {"type":"bulletList","content":[
                        {"type":"listItem","content":[
                            {"type":"paragraph","content":[
                                {"type":"text","text":"first"}
                            ]}
                        ]},
                        {"type":"listItem","content":[
                            {"type":"paragraph","content":[
                                {"type":"text","text":"second"}
                            ]}
                        ]}
                    ]}
                ]}"#
            )),
            "first\nsecond"
        );
    }

    #[test]
    fn adf_plain_string_fallback() {
        // Some JIRA installations or custom
        // apps may return a flat string instead
        // of ADF. The extractor returns the
        // string verbatim.
        assert_eq!(
            extract_adf_text(&adf(r#""just a string""#)),
            "just a string"
        );
    }

    #[test]
    fn adf_null_and_bool_fall_through_to_empty() {
        // Defensive: a `null` or boolean at the
        // top level should render as empty
        // rather than panic.
        assert_eq!(extract_adf_text(&adf("null")), "");
    }

    #[test]
    fn adf_keeps_full_description_no_truncation() {
        // The new design keeps the FULL
        // description text in `JiraIssue`
        // (no character cap) and lets the
        // preview renderer / overlay do their
        // own line-budget truncation. A
        // 500-character description flows
        // through to the issue unchanged.
        let text = "a".repeat(500);
        let json_str = format!(
            r#"{{"type":"doc","version":1,"content":[
                {{"type":"paragraph","content":[
                    {{"type":"text","text":"{}"}}
                ]}}
            ]}}"#,
            text
        );
        let issue: JiraIssue = serde_json::from_str::<SearchResponse>(&format!(
            r#"{{"issues":[{{"key":"P-1","fields":{{"description":{}}}}}]}}"#,
            json_str
        ))
        .unwrap()
        .issues
        .into_iter()
        .next()
        .unwrap()
        .into();
        // No trailing `…` — the text is
        // preserved verbatim.
        assert!(!issue.description.ends_with('…'));
        // The full 500 characters are present.
        assert_eq!(issue.description.chars().count(), 500);
        for c in issue.description.chars() {
            assert_eq!(c, 'a');
        }
    }
}
