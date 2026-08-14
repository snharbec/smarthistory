//! `.` (project picker) prefix mode.
//!
//! Lists `type: project` frontmatter notes from `notes.database` —
//! the same note vault the `@` (notes) mode searches, filtered down
//! to the subset time tracking treats as a "project". Selecting a
//! row stages `smarthistory project select <slug>` as the shell
//! command; the parent shell runs it, which upserts `project_current`
//! and closes/opens the active `project_sessions` row via the same
//! `switch_project` helper the directory-detection path in
//! `smarthistory add` uses (see `src/main.rs`).
//!
//! This is the explicit-selection half of time tracking's project
//! resolution — the config-driven `project.<slug>.dir` longest-
//! prefix match is the other half, and wins when both could apply
//! (see `resolve_current_project`).
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

/// True if the user typed the project-picker prefix (default `.`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.project_pick;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The project-picker body, i.e. everything after the leading `.`
/// prefix. Empty when not in this mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.project_pick;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Health check for the project-picker (`.`) mode: verifies
/// `notes.database` is configured and reachable (same requirement as
/// `@` notes mode, since this is a filtered view of the same vault),
/// and reports how many `type: project` notes it finds.
pub(crate) fn check(app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::ProjectPick;

    let Some(db_path) = app.notes_database.as_ref() else {
        return CheckReport::err(
            mode,
            "notes.database is not configured (set it in ~/.config/smarthistory/config)",
        );
    };
    if !db_path.exists() || !db_path.is_file() {
        return CheckReport::err(
            mode,
            format!(
                "notes.database file does not exist: {}",
                db_path.display()
            ),
        );
    }

    let service =
        note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());
    let criteria = note_search::SearchCriteria {
        list_only: true,
        query_expr: Some(note_search::QueryExpr::Attribute {
            key: "type".to_string(),
            value: Some("project".to_string()),
        }),
        ..Default::default()
    };
    match service.search_notes(&criteria) {
        Ok(rows) if rows.is_empty() => CheckReport::warn(
            mode,
            "notes database is reachable but has 0 notes with `type: project` (nothing to pick from)"
                .to_string(),
        ),
        Ok(rows) => CheckReport::ok(mode, format!("{} project note(s) found", rows.len())),
        Err(e) => CheckReport::err(mode, format!("search_notes() failed: {e}")),
    }
}

/// Fetch the project-picker result set: every `type: project` note,
/// further narrowed by whatever the user typed after `.` (AND-ed
/// onto the type filter via `QueryExpr::And` rather than replacing
/// it — the type filter always applies).
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    let Some(ref db_path) = app.notes_database else {
        return Ok(Vec::new());
    };
    let type_filter = note_search::QueryExpr::Attribute {
        key: "type".to_string(),
        value: Some("project".to_string()),
    };
    let user_pattern = app.project_pick_pattern().trim().to_string();
    let query_expr = if user_pattern.is_empty() {
        type_filter
    } else {
        match note_search::parse_query(&user_pattern) {
            Ok(user_expr) => note_search::QueryExpr::And(vec![type_filter, user_expr]),
            // An unparseable fragment (e.g. a dangling `[`) — fall
            // back to the type filter alone rather than erroring the
            // whole mode out over one bad keystroke; the picker just
            // shows every project note until the query is fixed up.
            Err(_) => type_filter,
        }
    };
    let service = note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());
    let criteria = note_search::SearchCriteria {
        list_only: true,
        query_expr: Some(query_expr),
        ..Default::default()
    };
    let rows = match service.search_notes(&criteria) {
        Ok(r) => r,
        Err(_e) => return Ok(Vec::new()),
    };
    let mut rows: Vec<HistoryRow> = rows
        .iter()
        .map(|note| {
            let title = note.title.as_deref().unwrap_or("");
            let comment = if title.is_empty() {
                note.filename.clone()
            } else {
                format!("{} — {}", title, note.filename)
            };
            let ts = note.updated.or(note.created).unwrap_or(0);
            HistoryRow {
                id: 0,
                command: note.filename.clone(),
                directory: String::new(),
                session_id: String::new(),
                exit_code: 0,
                timestamp: ts,
                comment,
                output: app.read_note_preview(&note.filename),
                mode: "project".to_string(),
                source: String::new(),
                ..Default::default()
            }
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.timestamp));
    Ok(rows)
}

impl App {
    /// True if the user typed the project-picker prefix (default `.`).
    pub(crate) fn is_project_pick_query(&self) -> bool {
        matches(self)
    }

    /// The project-picker body, i.e. everything after the leading
    /// `.` prefix. Empty when not in this mode.
    pub(crate) fn project_pick_pattern(&self) -> &str {
        pattern(self)
    }
}
