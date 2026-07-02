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
            .map(|s| std::path::PathBuf::from(s));
        let certificate_password = std::env::var("JIRA_HOST_CERTIFICATE_PASSWORD")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let ca_certificate_path = std::env::var("JIRA_CA_CERTIFICATE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| std::path::PathBuf::from(s));
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
        let url = format!(
            "{}/rest/api/2/search?jql={}&maxResults={}&fields=key,summary,status,issuetype,priority,assignee,updated",
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
        let client_builder = Client::builder()
            .timeout(std::time::Duration::from_secs(15));
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
                        let mut msg = format!(
                            "failed to parse CA certificate '{}': {}",
                            path.display(),
                            e,
                        );
                        let mut src: Option<&(dyn std::error::Error + 'static)> =
                            e.source();
                        let mut depth = 0;
                        while let Some(s) = src {
                            if depth >= 5 { break; }
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
                let password = self
                    .config
                    .certificate_password
                    .as_deref()
                    .unwrap_or("");
                let identity = reqwest::Identity::from_pkcs12_der(&der, password)
                    .map_err(|e| {
                        let mut msg = format!(
                            "failed to parse certificate '{}': {}",
                            path.display(),
                            e,
                        );
                        let mut src: Option<&(dyn std::error::Error + 'static)> =
                            e.source();
                        let mut depth = 0;
                        while let Some(s) = src {
                            if depth >= 5 { break; }
                            msg.push_str(&format!(" → {}", s));
                            src = s.source();
                            depth += 1;
                        }
                        JiraError::Http(msg)
                    })?;
                client_builder
                    .identity(identity)
                    .build()
                    .map_err(|e| {
                        let mut msg = e.to_string();
                        let mut src: Option<&(dyn std::error::Error + 'static)> =
                            e.source();
                        let mut depth = 0;
                        while let Some(s) = src {
                            if depth >= 5 { break; }
                            msg.push_str(&format!(" → {}", s));
                            src = s.source();
                            depth += 1;
                        }
                        JiraError::Http(msg)
                    })?
            }
            None => client_builder.build().map_err(|e| {
                let mut msg = e.to_string();
                let mut src: Option<&(dyn std::error::Error + 'static)> =
                    e.source();
                let mut depth = 0;
                while let Some(s) = src {
                    if depth >= 5 { break; }
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
                let mut src: Option<&(dyn std::error::Error + 'static)> =
                    e.source();
                let mut depth = 0;
                while let Some(s) = src {
                    if depth >= 5 { break; }
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
            let mut src: Option<&(dyn std::error::Error + 'static)> =
                e.source();
            let mut depth = 0;
            while let Some(s) = src {
                if depth >= 5 { break; }
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

impl From<ApiIssue> for JiraIssue {
    fn from(a: ApiIssue) -> Self {
        let f = a.fields;
        JiraIssue {
            key: a.key,
            summary: f.summary.unwrap_or_default(),
            status: f.status.map(|n| n.name_or_empty()).unwrap_or_default(),
            issuetype: f.issuetype.map(|n| n.name_or_empty()).unwrap_or_default(),
            priority: f.priority.map(|n| n.name_or_empty()).unwrap_or_default(),
            assignee: f.assignee.map(|n| n.name_or_empty()).unwrap_or_default(),
            updated: f.updated.unwrap_or_default(),
        }
    }
}

// ---- query parsing ----

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
///
/// The three groups are AND-joined in order (keys, then
/// field-values, then free-text). The result always ends
/// with ` ORDER BY updated DESC` so the list is
/// newest-updated first.
///
/// When `default_project` is set, a `project = "<proj>"`
/// clause is always prepended — even when the body is
/// non-empty — so the user's project-scoped view is
/// never accidentally widened by a free-text search.
/// An empty body without a project produces a server-wide
/// `ORDER BY updated DESC` (the "recently touched across
/// all projects" view).
pub fn build_jql(body: &str, default_project: Option<&str>) -> String {
    let body = body.trim();
    if body.is_empty() {
        return match default_project {
            Some(p) => format!("project = {} ORDER BY updated DESC", escape_jql_string(p)),
            None => "ORDER BY updated DESC".to_string(),
        };
    }
    let key_re = regex::Regex::new(r"^\w+-\d+$").expect("static regex");
    let kv_re = regex::Regex::new(r"^(\w+)=(.*)$").expect("static regex");
    let mut keys: Vec<&str> = Vec::new();
    let mut kvs: Vec<(&str, &str)> = Vec::new();
    let mut text: Vec<&str> = Vec::new();
    for tok in body.split_whitespace() {
        if key_re.is_match(tok) {
            keys.push(tok);
        } else if let Some(caps) = kv_re.captures(tok) {
            kvs.push((caps.get(1).unwrap().as_str(), caps.get(2).unwrap().as_str()));
        } else {
            text.push(tok);
        }
    }
    let mut parts: Vec<String> = Vec::new();
    // Always scope to the default project when one is
    // configured, so free-text searches don't leak
    // results from other projects.
    if let Some(p) = default_project {
        parts.push(format!("project = {}", escape_jql_string(p)));
    }
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
        return build_jql("", default_project);
    }
    format!("{} ORDER BY updated DESC", parts.join(" AND "))
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

    // ---- build_jql ----

    #[test]
    fn build_jql_empty_body_uses_default_project() {
        assert_eq!(
            build_jql("", Some("PROJ")),
            r#"project = "PROJ" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_empty_body_no_project_is_global_recent() {
        assert_eq!(build_jql("", None), "ORDER BY updated DESC");
    }

    #[test]
    fn build_jql_single_issue_key() {
        assert_eq!(
            build_jql("PROJ-123", None),
            "key = PROJ-123 ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_multiple_issue_keys_collapse_into_in() {
        assert_eq!(
            build_jql("PROJ-1 PROJ-2", None),
            "key in (PROJ-1, PROJ-2) ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_field_value_quoted() {
        assert_eq!(
            build_jql("project=PROJ", None),
            r#"project = "PROJ" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_empty_field_value() {
        // `project=` (empty value) is a valid token by the
        // `\w+=\S*` classifier; value is the empty string.
        assert_eq!(
            build_jql("assignee=", None),
            r#"assignee = "" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_free_text_searches_description_or_summary() {
        assert_eq!(
            build_jql("login", None),
            r#"(description ~ "login" OR summary ~ "login") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_combines_all_three_groups_with_and() {
        // keys first, then field=value, then free text.
        assert_eq!(
            build_jql("PROJ-123 project=PROJ crash", None),
            r#"key = PROJ-123 AND project = "PROJ" AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_multiple_free_text_tokens_are_anded() {
        assert_eq!(
            build_jql("login crash", None),
            r#"(description ~ "login" OR summary ~ "login") AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_escapes_quotes_and_backslashes_in_text() {
        // A free-text token containing `"` and `\` must be
        // escaped so it's a valid JQL string literal.
        let jql = build_jql(r#"a"b\c"#, None);
        assert!(jql.contains(r#"description ~ "a\"b\\c""#), "{}", jql);
    }

    #[test]
    fn build_jql_whitespace_only_falls_back_to_default() {
        assert_eq!(
            build_jql("   ", Some("PROJ")),
            r#"project = "PROJ" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_free_text_with_default_project_is_scoped() {
        // When a default project is configured, free-text
        // searches must NOT leak results from other projects.
        assert_eq!(
            build_jql("crash", Some("PROJ")),
            r#"project = "PROJ" AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_issue_key_with_default_project_is_scoped() {
        assert_eq!(
            build_jql("PROJ-123", Some("PROJ")),
            r#"project = "PROJ" AND key = PROJ-123 ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_field_value_with_default_project_is_scoped() {
        assert_eq!(
            build_jql("assignee=alice", Some("PROJ")),
            r#"project = "PROJ" AND assignee = "alice" ORDER BY updated DESC"#
        );
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
            "updated":"2024-06-30T19:14:39.000+0000"
        }}]}"#;
        let parsed: SearchResponse = serde_json::from_str(json).unwrap();
        let issue = JiraIssue::from(parsed.issues.into_iter().next().unwrap());
        assert_eq!(issue.key, "PROJ-2");
        assert_eq!(issue.status, "Done");
        assert_eq!(issue.issuetype, "Bug");
        assert_eq!(issue.priority, "High");
        assert_eq!(issue.assignee, "Alice");
        assert!(updated_to_epoch(&issue.updated) > 0);
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
}
