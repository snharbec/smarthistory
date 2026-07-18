//! `-` (JIRA issue search) prefix mode.
//!
//! Lists JIRA issues from a self-hosted instance matching
//! the typed query (issue keys, `field=value` constraints,
//! or free text matched against description / summary).
//! Selecting an issue opens its browse URL in the system
//! browser. Credentials / config come from the
//! `JIRA_SERVER`, `JIRA_API_TOKEN`, `JIRA_URL`, and
//! `JIRA_PROJECT` environment variables.
use crate::tui::App;

/// Whether the query is a JIRA issue-search request:
/// the query starts with the jira prefix (`-` by
/// default). The body is parsed into a JQL query by
/// `crate::jira::build_jql` (issue keys,
/// `field=value` constraints, free text).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.jira;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The JIRA search body, i.e. everything after the
/// leading `-` prefix. Empty string when not in jira
/// mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.jira;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}
