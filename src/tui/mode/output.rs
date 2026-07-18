//! `+` (output search) prefix mode.
//!
//! See [`crate::docs_configuration`] for the user-facing
//! description; the per-mode dispatch lives in
//! [`crate::tui::mode::mod`].
use crate::tui::App;

/// True if the current query is an output-search request
/// (prefixed with the configured output prefix, default `+`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.output;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The output-search body, i.e. everything after the leading
/// `+` prefix. Empty string when not in output mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.output;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}
