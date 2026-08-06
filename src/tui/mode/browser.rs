//! `^` (browser bookmarks + history) prefix mode.
//!
//! Merges bookmarks and visited-URL history from every configured
//! (or auto-detected) browser profile into one list. Each row's
//! `command` text is prefixed with the literal word `bookmark` or
//! `history` — typing that word as part of the query narrows the
//! view to just that source. Selecting a row opens its URL in the
//! system browser; the staging logic lives in
//! `stage_browser_selection` in `src/tui/actions.rs`.
//!
//! The actual file-reading (Chrome `Bookmarks` JSON + `History`
//! SQLite, Firefox `places.sqlite`) lives in `src/browser.rs`; this
//! module is just the thin per-mode glue (`matches` / `pattern` /
//! `check` / `fetch`) every other prefix mode has, following the
//! `paperless` mode module as the closest template (another
//! external-list mode whose selection opens a URL).
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::{App, PickMode};
use anyhow::Result;

/// Whether the query is a browser-mode request: the query starts
/// with the browser prefix (`^` by default).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.browser;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The browser-mode search body, i.e. everything after the leading
/// `^` prefix. Empty string when not in browser mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.browser;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Health check for the browser (`^`) mode. Unlike JIRA / Paperless
/// (which need reachable credentials), this mode reads local files,
/// so the check verifies that at least one browser source resolves
/// to an existing profile directory with a readable bookmarks/
/// history file.
pub(crate) fn check(_app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Browser;

    let sources = crate::browser::resolve_configured();
    if sources.is_empty() {
        return CheckReport::err(
            mode,
            "no browser source configured or auto-detected (set browser.<id>.type= / \
             browser.<id>.profile= in ~/.config/smarthistory/config, or install Chrome / \
             Firefox at their default profile location)",
        );
    }

    let mut report = CheckReport::ok(
        mode,
        format!("{} browser source(s) configured", sources.len()),
    );
    for source in &sources {
        let primary = source.primary_file();
        // A plain `is_dir()` on the profile isn't enough — on
        // macOS, `~/Library/Safari` passes that check even when
        // Terminal (or whatever runs smarthistory) hasn't been
        // granted Full Disk Access, and every actual file read
        // inside it then fails silently at fetch time. Attempting
        // to open the primary file surfaces that distinction here
        // instead of leaving the user staring at an empty list.
        let sub = match std::fs::File::open(&primary) {
            Ok(_) => CheckReport::ok(
                mode,
                format!("{}: readable at {}", source.kind.as_str(), source.profile.display()),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => CheckReport::warn(
                mode,
                format!(
                    "{}: profile not found at {} (this source will yield no rows)",
                    source.kind.as_str(),
                    source.profile.display()
                ),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => CheckReport::err(
                mode,
                format!(
                    "{}: permission denied reading {} — grant Full Disk Access to your \
                     terminal app in System Settings → Privacy & Security → Full Disk Access, \
                     then restart the terminal",
                    source.kind.as_str(),
                    primary.display()
                ),
            ),
            Err(e) => CheckReport::err(
                mode,
                format!("{}: could not read {}: {}", source.kind.as_str(), primary.display(), e),
            ),
        };
        report = report.with(sub);
    }
    report
}

/// Fetch the browser-mode result set. The read runs on a
/// background thread (spawned by `App::browser_touch` /
/// `App::browser_maybe_autocall` → `crate::browser::spawn_fetch`),
/// so this just clones the cached rows from
/// `App::browser_state.rows` — same one-line delegation as
/// `paperless::fetch`.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    Ok(app.browser_state.rows.clone())
}

impl App {
    /// Whether the query is a browser bookmarks/history request:
    /// the query starts with the browser prefix (`^` by default).
    pub(crate) fn is_browser_query(&self) -> bool {
        matches(self)
    }

    /// Arm or clear the browser-mode search debounce. Called from
    /// every keystroke path (co-located with `files_touch` /
    /// `paperless_touch`). Re-arms the timer when the user is still
    /// in browser mode; resets all pending state when they leave.
    pub(crate) fn browser_touch(&mut self) {
        let active = self.is_browser_query();
        crate::debounce::touch(&mut self.browser_state, active);
    }

    /// Check whether the browser-mode debounce has elapsed and, if
    /// so, spawn a background read. Called from the run loop's idle
    /// tick (same pattern as `paperless_maybe_autocall`). Returns
    /// immediately when not in browser mode, when a read is already
    /// in flight, or when the debounce window hasn't elapsed.
    pub(crate) fn browser_maybe_autocall(&mut self) {
        if !self.is_browser_query() {
            return;
        }
        if !crate::debounce::debounce_elapsed(&mut self.browser_state, crate::browser::BROWSER_DEBOUNCE)
        {
            return;
        }
        let pattern = crate::browser::BrowserState::current_pattern(
            &self.query,
            self.query_prefixes.browser,
        );
        if self.browser_state.has_results_for(&pattern) {
            return;
        }
        self.browser_state.last_pattern = Some(pattern.clone());
        self.browser_state.debounce_started = None;
        self.browser_state.in_flight = true;
        // Resolved fresh at point of use, not stored on `App` —
        // see `browser_state`'s field doc comment.
        let sources = crate::browser::resolve_configured();
        self.browser_state.request = Some(crate::browser::spawn_fetch(sources, pattern));
        self.set_status_message("Searching browser bookmarks/history…".to_string());
    }

    /// Process a browser-mode read result that arrived from the
    /// background thread. Caches the rows in
    /// `self.browser_state.rows` and refreshes the list. Stale
    /// results (the pattern changed between spawn and delivery) are
    /// discarded. Mirrors `process_files_result`.
    pub(crate) fn process_browser_result(
        &mut self,
        request: crate::browser::BrowserRequest,
        rows: Vec<HistoryRow>,
    ) {
        self.browser_state.in_flight = false;
        self.browser_state.request = None;
        let current = crate::browser::BrowserState::current_pattern(
            &self.query,
            self.query_prefixes.browser,
        );
        if current == request.pattern {
            self.browser_state.rows = rows;
            self.status_message = None;
            self.refresh();
        }
    }

    /// Convert the currently-selected browser bookmark/history entry
    /// into a local markdown note via `note_search convert <url>` —
    /// `note_search`'s general "convert a web page or document to a
    /// markdown note" command. Bound to `Action::SmartOpen` (`Ctrl-]`
    /// by default) in browser mode instead of the default "open the
    /// URL" fallback every other mode gets — see the
    /// `ModeKind::Browser` arm of the `SmartOpen` dispatch in
    /// `src/tui.rs`.
    ///
    /// The staged command is the bare `note_search convert <url>`
    /// shell line — no `-o`/`-d` flags, same convention
    /// `download_jira_issue` (JIRA mode's analogous "download this
    /// as a note" action) uses: `note_search` picks up
    /// `NOTE_SEARCH_DIR`/`NOTE_SEARCH_DATABASE` from the environment
    /// (or its own `./note.sqlite` default) rather than smarthistory
    /// passing them explicitly. The TUI exits so the parent shell
    /// runs the command.
    ///
    /// On any no-op (no row selected, empty URL, not in browser
    /// mode) a status message is surfaced and `selection` stays
    /// `None`, so the TUI stays open and the user can react to the
    /// feedback.
    pub(crate) fn download_browser_entry_as_note(&mut self) {
        // Re-gate here (the `SmartOpen` dispatcher already gates on
        // `ModeKind::Browser`) so a stray future caller — e.g. a
        // command-palette entry that calls into the helper directly
        // — can't stage the wrong command from outside browser mode.
        if !self.is_browser_query() {
            self.set_status_message(
                "Create-note-from-URL is only available in browser mode (type `^`)".to_string(),
            );
            return;
        }
        let Some(row) = self.selected_row().cloned() else {
            self.set_status_message("No browser entry selected".to_string());
            return;
        };
        // The URL lives in `row.comment` — see `browser_entry_to_row`
        // in `src/browser.rs`. Empty is a no-op (a freshly-inserted
        // row that hasn't been populated yet could in theory have
        // one).
        let url = row.comment.trim().to_string();
        if url.is_empty() {
            self.set_status_message("Selected browser entry has no URL".to_string());
            return;
        }
        let staged = format!("note_search convert {}", crate::util::shell_quote(&url));
        self.selection = Some(staged);
        self.pick_mode = Some(PickMode::Run);
    }
}
