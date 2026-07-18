//! `@` (note search) prefix mode.
use crate::tui::App;

/// True if the current query is a note search request
/// (prefixed with the configured notes prefix, default `@`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.notes;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The note search body, i.e. everything after the
/// leading notes prefix.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.notes;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}
