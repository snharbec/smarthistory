//! `-` (JIRA issue search) prefix mode.
//!
//! Lists JIRA issues from a self-hosted instance matching
//! the typed query (issue keys, `field=value` constraints,
//! or free text matched against description / summary).
//! Selecting an issue opens its browse URL in the system
//! browser. Credentials / config come from the
//! `JIRA_SERVER`, `JIRA_API_TOKEN`, `JIRA_URL`, and
//! `JIRA_PROJECT` environment variables.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

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

/// Health check for the JIRA (`-`) issue-search
/// mode. The mode talks to a self-hosted JIRA
/// instance via REST for every search, so the
/// check verifies:
///
/// 1. The required env vars are set
///    (`JIRA_SERVER` + `JIRA_API_TOKEN`).
/// 2. The optional `JIRA_PROJECT` (if set) is
///    a non-empty project key.
/// 3. The JIRA server is reachable
///    (`GET {server}/rest/api/3/myself` with
///    Bearer auth returns HTTP 200).
/// 4. The configured project exists
///    (`GET {server}/rest/api/3/project/{key}`
///    returns HTTP 200, not 404).
///
/// Stops at the first failure.
pub(crate) fn check(_app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Jira;

    // 1. Required env vars.
    let server = match std::env::var("JIRA_SERVER") {
        Ok(s) if !s.trim().is_empty() => s,
        Ok(_) | Err(_) => {
            return CheckReport::err(
                mode,
                "JIRA_SERVER env var is not set or is empty (set it to your JIRA base URL, e.g. https://jira.example.com)",
            );
        }
    };
    let token = match std::env::var("JIRA_API_TOKEN") {
        Ok(s) if !s.trim().is_empty() => s,
        Ok(_) | Err(_) => {
            return CheckReport::err(
                mode,
                "JIRA_API_TOKEN env var is not set or is empty (create an API token at https://id.atlassian.com/manage-profile/security/api-tokens and set this env var)",
            );
        }
    };
    let project = std::env::var("JIRA_PROJECT")
        .ok()
        .filter(|s| !s.trim().is_empty());

    // 2. Optional `JIRA_URL` (browse URL base; defaults
    //    to `JIRA_SERVER`).
    let browse_url = std::env::var("JIRA_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| server.trim_end_matches('/').to_string());

    // 3. Server reachability + auth. The
    //    `/myself` endpoint is the lightest
    //    authenticated request: it returns
    //    200 with the user payload, 401 if
    //    the token is wrong, 403 if the user
    //    is disabled.
    let myself_url = format!("{}/rest/api/3/myself", server.trim_end_matches('/'));
    let client = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(5))
        .build();
    let resp = client
        .get(&myself_url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .call();
    let status = match resp {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(ureq::Error::Transport(t)) => {
            return CheckReport::err(mode, format!("could not reach JIRA at {server}: {t}"));
        }
    };
    if status == 401 {
        return CheckReport::err(
            mode,
            format!("JIRA at {server} returned 401 Unauthorized (the API token is invalid or expired; create a new one at https://id.atlassian.com/manage-profile/security/api-tokens)"),
        );
    }
    if status == 403 {
        return CheckReport::err(
            mode,
            format!("JIRA at {server} returned 403 Forbidden (the user this token belongs to is disabled or lacks the `Browse Projects` permission)"),
        );
    }
    if !(200..300).contains(&status) {
        return CheckReport::err(
            mode,
            format!("JIRA at {server} returned HTTP {status} on /myself"),
        );
    }

    // 4. Project existence (only when
    //    `JIRA_PROJECT` is set — otherwise
    //    the runtime uses a server-wide
    //    search).
    if let Some(key) = project.as_deref() {
        let proj_url = format!(
            "{}/rest/api/3/project/{}",
            server.trim_end_matches('/'),
            key
        );
        let proj_resp = client
            .get(&proj_url)
            .set("Authorization", &format!("Bearer {token}"))
            .set("Accept", "application/json")
            .call();
        let proj_status = match proj_resp {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(ureq::Error::Transport(t)) => {
                return CheckReport::err(mode, format!("could not probe JIRA project {key}: {t}"));
            }
        };
        if proj_status == 404 {
            return CheckReport::err(
                mode,
                format!("JIRA project `{key}` does not exist on {server} (or you don't have permission to view it)"),
            );
        }
        if !(200..300).contains(&proj_status) {
            return CheckReport::err(
                mode,
                format!("JIRA project probe returned HTTP {proj_status}"),
            );
        }
        CheckReport::ok(
            mode,
            format!("JIRA reachable at {browse_url}, project `{key}` is visible"),
        )
    } else {
        CheckReport::ok(
            mode,
            format!("JIRA reachable at {browse_url} (no JIRA_PROJECT set; the runtime uses a server-wide search)"),
        )
    }
}

/// Fetch the JIRA-mode result set. The JIRA search
/// runs on a background thread (spawned by
/// `App::jira_touch` → `crate::jira::spawn_jira_search`),
/// so this just clones the cached rows from
/// `App::jira_rows`. A future pass can move the
/// JIRA background-thread orchestration here.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    Ok(app.jira_rows.clone())
}
