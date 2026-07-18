//! `%` (general LLM question) prefix mode.
//!
//! Like the LLM mode, requires non-whitespace text after the
//! prefix.
use crate::tui::App;

/// True if the current query is a general question
/// request (prefixed with the configured question prefix).
/// Only returns true if there's actual question text after
/// the prefix (not just the prefix alone or with only whitespace).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.question;
    app.query.starts_with(p) && !app.query[p.len_utf8()..].trim().is_empty()
}

/// The question body, i.e. everything after the leading
/// `%` prefix. Empty string when not in question mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.question;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}
