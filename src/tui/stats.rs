//! Stats mode: successor-frequency prediction.
//!
//! `Mode::Stats` is a *scope* mode (not a prefix
//! mode) — the user toggles it via the `Mode::Stats`
//! option in the TUI, and the TUI shows every
//! command ranked by how often it has followed
//! the user's most recent command in the global
//! history. The dispatch in `App::fetch` branches
//! on `Mode::Stats` *before* the per-mode
//! `ModeKind` match (stats isn't a prefix mode),
//! so this function lives in its own module rather
//! than under `crate::tui::mode::*`.
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

///     /// Fetch rows ordered by:
    ///   1. probability of following the most-recently-executed
    ///      command (computed via SQLite's `LEAD()` window
    ///      function on the entire global history, ignoring
    ///      session/directory filters),
    ///   2. timestamp DESC (newest first).
    ///
    /// The user's query (when non-empty and not a regex) is honored
    /// as a `LIKE` filter so the user can narrow down what's
    /// ranked. The "last command" itself is the newest row in the
    /// global history that matches the query — the view is
    /// reproducible regardless of which session we're in.
    ///
    /// Tie-breaking within a probability bucket: more recent wins.
    /// Tie-breaking across duplicate commands when the duplicate
    /// filter is on: the most recent instance only.
    pub(crate) fn fetch(app: &App) -> Result<Vec<HistoryRow>> {
        // 1) Determine the "last command" from the global history
        //    (still respecting the user's query so the prediction
        //    makes sense in context).
        let last_cmd: Option<String> = {
            let (where_clause, params) = app.build_where();
            let sql = format!(
                "SELECT h.command FROM history h{} \
                 ORDER BY h.timestamp DESC, h.id DESC LIMIT 1",
                where_clause
            );
            let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = app.conn.prepare(&sql)?;
            let mut rows = stmt.query_map(&params_ref[..], |row| row.get::<_, String>(0))?;
            rows.next().transpose()?
        };
        let Some(last_cmd) = last_cmd else {
            // No matching history at all.
            return Ok(Vec::new());
        };

        // 2) Pull the rows the user is going to see, ranked by:
        //    (a) frequency as a successor of `last_cmd` DESC,
        //    (b) timestamp DESC.
        //    The user's typed query is honored (where possible).
        let (where_clause, params) = app.build_where();
        // The freq CTE compares against `last_cmd`. SQLite parameter
        // binding works inside CTEs, but we splice the value
        // directly here because it's an internal-only slug (not
        // user input) and escaping via `replace('\'')` keeps the
        // query plan simple. Single quotes are doubled to escape.
        let last_sql = last_cmd.replace('\'', "''");
        // We compute frequency in a single SQL query using a CTE so
        // the entire ranking is one round trip. Predicted commands
        // get a `freq` > 0; commands that never followed `last_cmd`
        // get `freq = 0` and are sorted by timestamp DESC.
        // `build_where` already starts with " WHERE 1=1", so we
        // splice the user's filter in directly.
        let sql = format!(
            "WITH pairs AS ( \
                 SELECT h.command AS cmd, \
                        LEAD(h.command) OVER (ORDER BY h.timestamp ASC, h.id ASC) AS next_cmd \
                 FROM history h \
             ), \
             freq AS ( \
                 SELECT next_cmd AS cmd, COUNT(*) AS freq \
                 FROM pairs \
                 WHERE cmd = '{last_sql}' AND next_cmd IS NOT NULL \
                 GROUP BY next_cmd \
             ) \
             SELECT h.id, h.command, h.directory, h.session_id, \
                    h.exit_code, h.timestamp, c.comment, o.output, h.mode, \
                    COALESCE(f.freq, 0) AS freq \
             FROM history h \
             LEFT JOIN command_comments c ON h.command = c.command \
             LEFT JOIN history_output o ON h.id = o.history_id \
             LEFT JOIN freq f ON h.command = f.cmd \
             {where_clause} \
             ORDER BY freq DESC, h.timestamp DESC \
             LIMIT 1000",
        );
        // The user's typed query is the only bound parameter (if any).
        let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = app.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(&params_ref[..], |row| {
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
