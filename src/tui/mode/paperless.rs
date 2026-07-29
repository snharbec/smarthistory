//! `<` (Paperless-ngx document search) prefix mode.
//!
//! Searches a configured Paperless-ngx v3 backend for documents
//! by title (bare words), tag (`#TAG`), or correspondent/author
//! (`@AUTHOR`). Selecting a document opens its details page in
//! the system browser. Credentials / backend URL come from the
//! `paperless.url` / `paperless.token` config-file keys (see
//! `Config::paperless` in `src/main.rs`).
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

/// Whether the query is a paperless document-search request:
/// the query starts with the paperless prefix (`<` by default).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.paperless;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The paperless search body, i.e. everything after the leading
/// `<` prefix. Empty string when not in paperless mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.paperless;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Health check for the paperless (`<`) document-search mode.
/// The mode talks to a self-hosted Paperless-ngx instance via
/// REST for every search, so the check verifies:
///
/// 1. `paperless.url` + `paperless.token` are both configured.
/// 2. The server is reachable and the token is accepted
///    (`GET {url}/api/documents/?page_size=1` returns HTTP 200).
pub(crate) fn check(app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Paperless;

    let Some(cfg) = app.paperless_config.as_ref() else {
        return CheckReport::err(
            mode,
            "paperless.url or paperless.token is not set (add both to ~/.config/smarthistory/config)",
        );
    };

    let url = format!("{}/api/documents/", cfg.url);
    let client = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(5))
        .build();
    let resp = client
        .get(&url)
        .set("Authorization", &format!("Token {}", cfg.token))
        .set("Accept", "application/json")
        .query("page_size", "1")
        .call();
    let status = match resp {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(ureq::Error::Transport(t)) => {
            return CheckReport::err(mode, format!("could not reach paperless at {}: {}", cfg.url, t));
        }
    };
    if status == 401 || status == 403 {
        return CheckReport::err(
            mode,
            format!(
                "paperless at {} returned {} (the API token is invalid or lacks permission)",
                cfg.url, status
            ),
        );
    }
    if !(200..300).contains(&status) {
        return CheckReport::err(
            mode,
            format!("paperless at {} returned HTTP {} on /api/documents/", cfg.url, status),
        );
    }
    CheckReport::ok(mode, format!("paperless reachable at {}", cfg.url))
}

/// Fetch the paperless-mode result set. The search runs on a
/// background thread (spawned by `App::paperless_touch` /
/// `App::paperless_maybe_autocall` → `crate::paperless::spawn_search`),
/// so this just clones the cached rows from
/// `App::paperless_state.rows` — same one-line delegation as
/// `jira::fetch`.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    Ok(app.paperless_state.rows.clone())
}
