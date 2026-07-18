//! `=` (LLM command generation) prefix mode.
//!
//! The LLM mode requires non-whitespace text after the
//! prefix — `=` alone is treated as no-mode, not as LLM.
use crate::tui::App;

/// True if the current query is an LLM command-generation
/// request (prefixed with the configured LLM prefix).
/// Only returns true if there's actual description text after
/// the prefix (not just the prefix alone or with only whitespace).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.llm;
    app.query.starts_with(p) && !app.query[p.len_utf8()..].trim().is_empty()
}

/// The LLM query body, i.e. everything after the leading
/// `=` prefix. Empty string when not in LLM mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.llm;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}
