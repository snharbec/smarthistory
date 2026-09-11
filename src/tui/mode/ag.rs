//! `,` (ag content search) prefix mode.
//!
//! Searches the current directory tree in-process (see the
//! module-level doc comment on `crate::ag` for the engine). Tokens
//! containing `*` are treated as file-pattern globs and restrict
//! which files are searched; `@lang` tokens restrict by file
//! extension and choose a highlight language. Selecting a row opens
//! the file in `$EDITOR` at the matching line.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

/// Whether the query is an ag content-search request:
/// the query starts with the ag prefix (`,` by
/// default). The body is split into search terms
/// and file-pattern globs (tokens containing `*`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.ag;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// Health check for the ag (`,`) content-search mode. The mode has
/// no external dependency anymore (it searches in-process via
/// `grep-regex`/`grep-searcher`/`ignore`), so the check verifies our
/// own code path instead of a third-party binary:
///
/// 1. CWD sanity (same pattern `files::check` uses).
/// 2. A trivial `grep_regex::RegexMatcher` compiles (proves the
///    matcher-construction path works — should never realistically
///    fail, but costs nothing and gives a concrete diagnostic if a
///    future change breaks it).
/// 3. The gitignore-aware walker (`ignore::WalkBuilder`) yields at
///    least one entry under cwd — `Warning`, not `Error`, on zero
///    entries, since an empty/heavily-gitignored cwd is a legitimate
///    non-broken state (mirrors `files::check`'s same precedent).
pub(crate) fn check(_app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Ag;

    // 1. CWD sanity.
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            return CheckReport::err(mode, format!("current working directory is unavailable: {e}"));
        }
    };
    if !cwd.is_dir() {
        return CheckReport::err(mode, format!("cwd is not a directory: {}", cwd.display()));
    }

    // 2. Matcher construction.
    if let Err(e) = grep_regex::RegexMatcherBuilder::new().build("a") {
        return CheckReport::err(mode, format!("failed to build a trivial regex matcher: {e}"));
    }

    // 3. Walk. Unfiltered, hidden files included, same as the
    //    runtime's default — just checking the walker itself works.
    // `ignore::Walk` always yields the root directory itself first
    // (depth 0), so a plain `.next().is_some()` would never observe
    // "empty" — look for at least one FILE entry instead, mirroring
    // `files::check`'s "rows.is_empty()" precedent.
    let walker = ignore::WalkBuilder::new(&cwd).hidden(false).build();
    let found_file = walker
        .filter_map(Result::ok)
        .any(|e| e.file_type().is_some_and(|t| t.is_file()));
    if found_file {
        CheckReport::ok(mode, format!("search engine ready; found at least one file under {}", cwd.display()))
    } else {
        CheckReport::warn(
            mode,
            format!("walker found 0 files under {} (the directory is empty or fully gitignored)", cwd.display()),
        )
    }
}

/// The ag-search body, i.e. everything after the
/// leading ag prefix. Empty string when not in
/// ag mode.
#[allow(dead_code)]
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.ag;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Fetch the ag-mode result set. The actual ag
/// process runs on a background thread (spawned by
/// `App::ag_touch` → `crate::ag::spawn_ag_search`),
/// so this just clones the cached rows from
/// `App::ag_state`. A future pass can move the
/// debounce / background-thread orchestration here
/// too — the cached `ag_state` would have to become
/// a per-mode sub-state.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    Ok(app.ag_state.rows.clone())
}

impl App {
    /// Whether the query is an ag content-search request:
    /// the query starts with the ag prefix (`,` by
    /// default). The body is split into search terms
    /// and file-pattern globs (tokens containing `*`).
    pub(crate) fn is_ag_query(&self) -> bool {
        matches(self)
    }

    /// Arm the ag-mode debounce. Mirrors the other
    /// debounced-fetch modes' `*_touch` (see
    /// `crate::debounce`).
    pub(crate) fn ag_touch(&mut self) {
        let active = self.is_ag_query();
        crate::debounce::touch(&mut self.ag_state, active);
    }

    /// Check whether the ag-mode debounce has elapsed
    /// and spawn a background search if so.
    pub(crate) fn ag_maybe_autocall(&mut self) {
        if !self.is_ag_query() {
            return;
        }
        if !crate::debounce::debounce_elapsed(&mut self.ag_state, crate::ag::AG_DEBOUNCE) {
            return;
        }
        let pattern = crate::ag::AgState::current_pattern(&self.query, self.query_prefixes.ag);
        if self.ag_state.has_results_for(&pattern) {
            return;
        }
        self.ag_state.last_pattern = Some(pattern.clone());
        self.spawn_ag_search(pattern);
    }

    /// Spawn a background thread that searches in-process and
    /// collects the results.
    pub(crate) fn spawn_ag_search(&mut self, pattern: String) {
        let request = crate::ag::spawn_ag_search(pattern);
        self.ag_state.in_flight = true;
        self.ag_state.request = Some(request);
        self.set_status_message("Searching…".to_string());
    }

    /// Process an ag-mode search result from the
    /// background thread.
    pub(crate) fn process_ag_result(&mut self, request: crate::ag::AgRequest, rows: Vec<HistoryRow>) {
        self.ag_state.in_flight = false;
        self.ag_state.request = None;
        let current = crate::ag::AgState::current_pattern(&self.query, self.query_prefixes.ag);
        if current == request.pattern {
            self.ag_state.rows = rows;
            self.refresh();
        }
    }
}
