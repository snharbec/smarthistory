//! `*` (session panes) prefix mode.
//!
//! Lists every pane in the *current* multiplexer context
//! (tmux session or herdr workspace), excluding the pane
//! the TUI is running in (read from `$TMUX_PANE`).
//! Selecting a row stages a `select-pane` / `switch-client`
//! command (or the herdr equivalent) and exits the TUI.
use crate::tui::App;

/// Whether the query is a session-panes request:
/// the query starts with the panes prefix (`*` by
/// default). The body (everything after `*`) is a
/// substring filter matched against each pane's
/// current command and cwd.
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.panes;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The session-panes filter body, i.e. everything
/// after the leading `*` prefix. Empty when not in
/// panes mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.panes;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}
