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

impl App {
    /// Whether the query is a paperless document-search
    /// request: the query starts with the paperless prefix
    /// (`<` by default). The body is parsed by
    /// `crate::paperless::build_query` into tag / correspondent /
    /// title tokens.
    pub(crate) fn is_paperless_query(&self) -> bool {
        matches(self)
    }

    /// Arm or clear the paperless-mode search debounce. Called
    /// from every keystroke path (co-located with `files_touch`
    /// et al.). Re-arms the timer when the user is still in
    /// paperless mode; resets all pending state when they leave.
    pub(crate) fn paperless_touch(&mut self) {
        let active = self.is_paperless_query();
        crate::debounce::touch(&mut self.paperless_state, active);
    }

    /// Check whether the paperless-mode debounce has elapsed
    /// and, if so, spawn a background search. Called from the
    /// run loop's idle tick (same pattern as
    /// `files_maybe_autocall`). Returns immediately when not in
    /// paperless mode, when a search is already in flight, or
    /// when the debounce window hasn't elapsed.
    pub(crate) fn paperless_maybe_autocall(&mut self) {
        if !self.is_paperless_query() {
            return;
        }
        if !crate::debounce::debounce_elapsed(
            &mut self.paperless_state,
            crate::paperless::PAPERLESS_DEBOUNCE,
        ) {
            return;
        }
        let pattern = crate::paperless::PaperlessState::current_pattern(
            &self.query,
            self.query_prefixes.paperless,
        );
        if self.paperless_state.has_results_for(&pattern) {
            return;
        }
        self.paperless_state.last_pattern = Some(pattern.clone());
        let Some(config) = self.paperless_config.clone() else {
            self.set_status_message(crate::paperless::PaperlessError::NotConfigured.to_string());
            self.paperless_state.debounce_started = None;
            return;
        };
        self.paperless_state.debounce_started = None;
        self.paperless_state.in_flight = true;
        self.paperless_state.request = Some(crate::paperless::spawn_search(config, pattern));
        self.set_status_message("Searching paperless…".to_string());
    }

    /// Process a paperless-mode search result that arrived from
    /// the background thread. Caches the rows in
    /// `self.paperless_state.rows` and refreshes the list on
    /// success; surfaces the error as a status message on
    /// failure (the list keeps the previous result). Mirrors
    /// `process_files_result` / `process_jira_result`.
    pub(crate) fn process_paperless_result(
        &mut self,
        request: crate::paperless::PaperlessRequest,
        result: Result<crate::paperless::PaperlessSearchOutcome, crate::paperless::PaperlessError>,
    ) {
        self.paperless_state.in_flight = false;
        self.paperless_state.request = None;
        if request
            .cancelled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        match result {
            Ok(outcome) => {
                self.paperless_state.rows = outcome.rows;
                self.paperless_state.rows_version = self.paperless_state.rows_version.wrapping_add(1);
                // The name catalogues are independent of the
                // matched rows (see `PaperlessSearchResult`'s doc
                // comment) — refreshed on every successful search
                // so `<#` / `<@` Tab completion stays current with
                // tags/correspondents created after the TUI opened.
                self.paperless_state.tag_names = outcome.tag_names;
                self.paperless_state.correspondent_names = outcome.correspondent_names;
                self.status_message = None;
                self.refresh();
            }
            Err(e) => {
                self.set_status_message(e.to_string());
            }
        }
    }
}
