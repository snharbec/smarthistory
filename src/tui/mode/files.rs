//! `~` (files) prefix mode.
//!
//! Lists every file in the current directory and
//! subdirectories, filtered by the typed pattern.
//! Selecting a row opens the file in `$EDITOR` (or the
//! configured per-extension command, via the SmartOpen
/// key `Ctrl-]`).
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

/// Whether the query is a files-view request:
/// the query starts with the files prefix (`~` by
/// default). The body (everything after `~`) is a
/// substring filter matched against each file's
/// path (relative to cwd).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.files;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The files-view body, i.e. everything after the
/// leading `~` prefix. Empty string when not in
/// files mode.
#[allow(dead_code)] // convention API; `App::files_pattern` delegates here
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.files;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Fetch the files-mode result set. The walk runs
/// on a background thread (spawned by
/// `App::files_touch` → `crate::files::spawn_walk`),
/// so this just clones the cached rows from
/// `App::files_state`. A future pass can move the
/// walk / debounce orchestration here.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    Ok(app.files_state.rows.clone())
}
