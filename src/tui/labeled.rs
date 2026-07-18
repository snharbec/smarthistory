//! Labeled rows (history entries that have a comment).
//!
//! Used by `build_merged_rows` to populate the
//! labeled-rows partition that mixes in alongside the
//! primary fetch. When the user has typed a query,
//! labeled entries are filtered to only those whose
//! command or comment matches; when the duplicate
//! filter is on, only the newest instance of each
//! command is kept.
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

/// Fetch every history row that has a comment (the
/// labeled-rows partition that `build_merged_rows`
/// mixes in alongside the primary fetch). When the
/// user has typed a query, labeled entries are
/// filtered to only those whose command or comment
/// matches the query (plain text or regex, depending
/// on whether the query starts with `/`); when the
/// duplicate filter is on, only the newest instance
/// of each command is kept.
pub(crate) fn fetch(app: &App) -> Result<Vec<HistoryRow>> {
    let sql = "SELECT h.id, h.command, h.directory, h.session_id, h.exit_code, h.timestamp, c.comment, o.output, h.mode \
               FROM history h \
               JOIN command_comments c ON h.command = c.command \
               LEFT JOIN history_output o ON h.id = o.history_id \
               ORDER BY h.timestamp DESC LIMIT 1000";
    let mut stmt = app.conn.prepare(sql)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(HistoryRow {
                id: row.get(0)?,
                command: row.get(1)?,
                directory: row.get(2)?,
                session_id: row.get(3)?,
                exit_code: row.get(4)?,
                timestamp: row.get(5)?,
                comment: row.get(6).unwrap_or_default(),
                output: row.get(7).unwrap_or_default(),
                mode: row.get(8).unwrap_or_default(),
                source: String::new(),

                ..Default::default()
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
