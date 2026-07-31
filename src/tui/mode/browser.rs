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
use crate::tui::App;
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
