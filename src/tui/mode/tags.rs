//! `$` (tags) prefix mode.
//!
//! Lists every symbol defined in a universal tag file
//! (`tags`) in the current directory, filtered by the
//! typed pattern. When no `tags` file exists, falls back
//! to the local `.codegraph/codegraph.db` FTS5 index
//! (see [`crate::tui::mode::codegraph`]).
use crate::tui::App;

/// Whether the query is a tags-search request:
/// the query starts with the tags prefix (`$` by
/// default). The body is matched against the
/// symbol names AND the source-line text from the
/// `tags` file in the current directory.
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.tags;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The tags-search body, i.e. everything after the
/// leading `$` prefix. Empty string when not in
/// tags mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.tags;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}
