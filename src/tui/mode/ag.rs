//! `,` (ag content search) prefix mode.
//!
//! Searches the current directory tree using `ag`
//! (The Silver Searcher). Tokens containing `*` are
//! treated as file-pattern globs (`-G`) and restrict
//! which files are searched. Selecting a row opens
//! the file in `$EDITOR` at the matching line.
use crate::tui::App;

/// Whether the query is an ag content-search request:
/// the query starts with the ag prefix (`,` by
/// default). The body is split into search terms
/// and file-pattern globs (tokens containing `*`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.ag;
    !app.query.is_empty() && app.query.starts_with(p)
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
