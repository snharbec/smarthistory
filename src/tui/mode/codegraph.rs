//! `&` (CodeGraph symbol search) prefix mode.
//!
//! Searches the local `.codegraph/codegraph.db` index
//! by symbol name (FTS5) and lists matching
//! functions / methods / classes. The selected row's
//! details pane shows the source context plus the
//! symbol's callers and callees (edges with
//! `kind='calls'`). Selecting a row opens the file in
//! `$EDITOR` at `start_line`. When no `.codegraph/`
//! index exists the `$` (tags) mode falls back to this
//! index, so a repo without a `TAGS` file still has
//! symbol navigation as long as CodeGraph has indexed
//! it.
use crate::tui::App;

/// Whether the query is a CodeGraph symbol-search
/// request: the query starts with the codegraph
/// prefix (`&` by default). The body is matched
/// against symbol names in the local
/// `.codegraph/codegraph.db` index via FTS5.
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.codegraph;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The codegraph-search body, i.e. everything after
/// the leading `&` prefix. Empty string when not in
/// codegraph mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.codegraph;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}
