//! `#` (directories) prefix mode.
//!
//! The directories view lists every unique directory
//! that's been used in the global history, sorted by
//! the most-recent history row's timestamp DESC, with
//! each directory's most-recently-executed
//! command surfaced for context. Selecting a row
/// stages a `cd <path>` command.
use crate::tui::App;

/// True if the user typed the
/// `directories` prefix
/// (default `#`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.directories;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The directories-search
/// body, i.e. everything
/// after the leading `#
/// prefix. Used to filter
/// the listed directories by
/// path substring. Empty
/// when not in directories
/// mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.directories;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}
