//! `!` (todo search) prefix mode.
//!
//! The todo mode scans every file in the configured notes
//! directory for lines that look like todo items
//! (markdown task-list checkboxes: `- [ ] text` / `- [x] text`)
//! and lists each match as its own row in the TUI.
use crate::tui::App;

/// True if the current query is a todo search
/// request (prefixed with the configured todo
/// prefix, default `!`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.todo;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The todo search body, i.e. everything
/// after the leading todo prefix. Same
/// contract as `notes::pattern`: empty string
/// when not in todo mode. The body's
/// whitespace-separated tokens are matched
/// against todo-line text.
#[allow(dead_code)] // convention API; `App::todo_pattern` delegates here
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.todo;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}
