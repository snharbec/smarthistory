//! `@` (note search) prefix mode.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use crate::tui::NotesDateFilter;
use anyhow::Result;

/// True if the current query is a note search request
/// (prefixed with the configured notes prefix, default `@`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.notes;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// Health check for the notes (`@`) mode. Verifies:
///
/// 1. `notes.database` is configured in
///    `~/.config/smarthistory/config`.
/// 2. The file exists and is readable.
/// 3. It opens as a sqlite database.
/// 4. The required tables (`markdown_data`,
///    `todo_entries`, `note_search_index`) are
///    present.
/// 5. A trivial `search_notes` query succeeds
///    (proves the connection + the library's
///    query-builder work end-to-end).
///
/// Stops at the first failure (no point trying
/// a sample query if the DB doesn't open) and
/// returns the `CheckReport` with the deepest
/// diagnostic available. A successful check
/// also runs a row-count query to surface
/// "the DB is empty" as a `Warning` (the user
/// probably wants to know).
pub(crate) fn check(app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Notes;

    // 1. Configuration check.
    let Some(db_path) = app.notes_database.as_ref() else {
        return CheckReport::err(
            mode,
            "notes.database is not configured (set it in ~/.config/smarthistory/config)",
        )
        .with(CheckReport::err(
            mode,
            "hint: smarthistory notes.database=~/path/to/notes.sqlite (run `smarthistory config check` to validate the config file)",
        ));
    };

    // 2. File existence.
    if !db_path.exists() {
        return CheckReport::err(
            mode,
            format!("notes.database file does not exist: {}", db_path.display()),
        );
    }
    if !db_path.is_file() {
        return CheckReport::err(
            mode,
            format!(
                "notes.database is not a regular file: {}",
                db_path.display()
            ),
        );
    }

    // 3. Open as sqlite.
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => {
            return CheckReport::err(
                mode,
                format!("failed to open notes database as sqlite: {e}"),
            );
        }
    };

    // 4. Required tables. We don't hardcode the full
    //    library schema (it changes between
    //    versions); we just probe for the tables
    //    that `search_notes_by_query` /
    //    `search_todos` actually need. A missing
    //    table is almost always "you indexed
    //    against a different note_search version"
    //    or "the DB got truncated".
    let required_tables = ["markdown_data", "todo_entries"];
    for table in &required_tables {
        let present: Result<i64, _> = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            rusqlite::params![table],
            |row| row.get(0),
        );
        match present {
            Ok(n) if n > 0 => {}
            Ok(_) => {
                return CheckReport::err(
                    mode,
                    format!("required table `{table}` is missing (the notes DB is incomplete or from an incompatible note_search version)"),
                );
            }
            Err(e) => {
                return CheckReport::err(mode, format!("failed to probe for table `{table}`: {e}"));
            }
        }
    }

    // 5. Trivial search. We use the library's own
    //    `DatabaseService` so we exercise the same
    //    code path the TUI uses. A success here
    //    proves the full search pipeline works
    //    end-to-end (sqlite → FTS5 → service).
    let service = note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());
    let criteria = note_search::SearchCriteria::default();
    let rows = match service.search_notes(&criteria) {
        Ok(r) => r,
        Err(e) => {
            return CheckReport::err(
                mode,
                format!("search_notes() failed on an empty query: {e}"),
            );
        }
    };

    // 6. Informational: row count. An empty DB
    //    means the user has never indexed any
    //    notes — the mode will work, but
    //    `search_notes` will always return an
    //    empty list. Surface this as a Warning
    //    so the user knows "the mode is wired up
    //    but the index is empty".
    if rows.is_empty() {
        return CheckReport::warn(
            mode,
            "notes database is reachable but contains 0 indexed notes (run `note_search index` to populate it)".to_string(),
        )
        .with(CheckReport::ok(
            mode,
            format!("opened {}", db_path.display()),
        ));
    }

    CheckReport::ok(
        mode,
        format!("{} notes indexed in {}", rows.len(), db_path.display()),
    )
    .with(CheckReport::ok(
        mode,
        format!("opened {}", db_path.display()),
    ))
    .with(CheckReport::ok(
        mode,
        format!("required tables present: {}", required_tables.join(", ")),
    ))
    .with(CheckReport::ok(
        mode,
        format!("sample search_notes() returned {} row(s)", rows.len()),
    ))
}

/// The note search body, i.e. everything after the
/// leading notes prefix.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.notes;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Fetch the notes-mode result set.
///
/// Steps:
/// 1. Return an empty list if no `notes.database`
///    is configured (the `@` mode is then
///    disabled).
/// 2. Parse the typed query for date-filter
///    aliases (`@today`, `@week`, `@month`,
///    `@year`). The resolved filter is recorded
///    on `app.notes_date_filter` so the mode-strip
///    chip renderer can see what's active (and the
///    chip disappears the moment the user clears
///    the alias token).
/// 3. Empty text pattern + active date alias
///    (e.g. `@today`): fall through to
///    `fetch_recent_with_filter` so the date
///    filter has all-notes to operate on.
/// 4. Otherwise: search the `note_search`
///    database, apply the date filter to the
///    results, and shape each match into a
///    `HistoryRow`. On search error, set
///    `app.notes_query_error = true` (the
///    renderer tints the input border red) and
///    return an empty list.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    let Some(ref db_path) = app.notes_database else {
        return Ok(Vec::new());
    };
    let raw_pattern = pattern(app).trim();
    // Strip any date-filter aliases
    // (`@today`, `@week`, `@month`, `@year`)
    // from the pattern. The cleaned pattern
    // is what we pass to
    // `note_search.search_notes_by_query`
    // (which doesn't know about these
    // aliases); the filter is applied
    // post-query in this method against the
    // `updated` timestamp on each result.
    let (pattern, filter) = crate::tui::parse_notes_query(raw_pattern);
    // Record the resolved filter on `self` so
    // the mode-strip chip renderer (and any
    // future helper) can see what's active.
    // We update this on every refresh, even
    // when the pattern is empty (so the chip
    // disappears the moment the user clears
    // the alias token).
    app.notes_date_filter = filter;
    if pattern.is_empty() {
        // The user typed only the
        // date alias (e.g. `@today`).
        // We still need to fetch
        // *all* notes (no text
        // filter) so the date
        // filter has something to
        // operate on, then apply
        // the cutoff post-hoc. The
        // previous behaviour was to
        // return every note
        // unfiltered, which made
        // `@today` indistinguishable
        // from `@` — the chip lit
        // up but the rows ignored
        // it.
        return fetch_recent_with_filter(app, db_path, filter);
    }

    let service = note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());

    match service.search_notes_by_query(&pattern) {
        Ok(results) => {
            // Apply the date filter (if any)
            // before building `HistoryRow`
            // entries. Notes with `updated =
            // None` fall back to `created`; if
            // both are `None`, the note has
            // no usable timestamp and we
            // exclude it from any active
            // filter (we have no way to know
            // if it's recent). This matches
            // the user's intent: aliases
            // answer "what was updated
            // *recently*", and a note without
            // timestamps is by definition not
            // "recently updated".
            let cutoff = filter.cutoff(app.now_epoch());
            let mut rows: Vec<HistoryRow> = results
                .iter()
                .filter(|note| match cutoff {
                    None => true,
                    Some(c) => note.updated.or(note.created).unwrap_or(0) >= c,
                })
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
                        mode: "note".to_string(),
                        source: String::new(),
                        ..Default::default()
                    }
                })
                .collect();
            // Sort by timestamp descending
            // (newest first)
            rows.sort_by_key(|b| std::cmp::Reverse(b.timestamp));
            app.notes_query_error = false;
            Ok(rows)
        }
        Err(_e) => {
            app.notes_query_error = true;
            Ok(Vec::new())
        }
    }
}

/// Fetch every note in the database (no text
/// filter) and apply the date-filter alias (if
/// any) against each note's `updated` timestamp.
/// Used when the user types a bare alias (e.g.
/// `@today`) — `parse_notes_query` returns an
/// empty text pattern in that case, so we can't
/// push the alias through the library's text
/// search; we fetch every note and filter by
/// mtime post-hoc instead.
///
/// `NotesDateFilter::All` is the no-op case (no
/// cutoff applied); passing it gives the same
/// result as fetching all notes unfiltered.
fn fetch_recent_with_filter(
    app: &App,
    db_path: &std::path::Path,
    filter: NotesDateFilter,
) -> Result<Vec<HistoryRow>> {
    let service = note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());
    // Use default SearchCriteria to get all
    // notes (no query filter).
    let criteria = note_search::SearchCriteria::default();
    match service.search_notes(&criteria) {
        Ok(results) => {
            let cutoff = filter.cutoff(app.now_epoch());
            let mut rows: Vec<HistoryRow> = results
                .iter()
                .filter(|note| match cutoff {
                    // No active filter:
                    // every note qualifies.
                    None => true,
                    // Active filter:
                    // require a recent
                    // `updated` (falling
                    // back to `created`
                    // when missing). Notes
                    // with neither are
                    // excluded — we
                    // can't know if they're
                    // recent.
                    Some(c) => note.updated.or(note.created).unwrap_or(0) >= c,
                })
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
                        mode: "note".to_string(),
                        source: String::new(),
                        ..Default::default()
                    }
                })
                .collect();
            // Sort by timestamp descending
            // (newest first)
            rows.sort_by_key(|b| std::cmp::Reverse(b.timestamp));
            Ok(rows)
        }
        Err(_e) => Ok(Vec::new()),
    }
}

/// Read the `updated` column from
/// `markdown_data` for each filename in
/// `filenames`, returning a map of
/// `filename -> updated_epoch`. Used by
/// `fetch_todos` to populate the
/// Details-pane age column with a real
/// timestamp instead of the
/// `9999M` placeholder that
/// `format_diff(0)` would produce. The
/// query is `WHERE filename IN (?, ?, …)`
/// so it's O(unique-files), not
/// O(rows), regardless of how many todos
/// each file contains.
pub(crate) fn fetch_file_updated_timestamps(
    db_path: &std::path::Path,
    filenames: &[String],
) -> std::collections::HashMap<String, i64> {
    use rusqlite::Connection;
    let mut map = std::collections::HashMap::new();
    if filenames.is_empty() {
        return map;
    }
    let Ok(conn) = Connection::open(db_path) else {
        return map;
    };
    // Build the parameterized IN-list.
    // SQLite has a default limit of 999
    // parameters per statement; with a
    // few hundred todos per page we're
    // nowhere near that, but a
    // short-circuit on an empty list
    // keeps the SQL well-formed.
    let placeholders = std::iter::repeat_n("?", filenames.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT filename, updated FROM markdown_data \
             WHERE filename IN ({placeholders})"
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return map;
    };
    let params: Vec<&dyn rusqlite::ToSql> = filenames
        .iter()
        .map(|f| f as &dyn rusqlite::ToSql)
        .collect();
    let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
        let f: String = row.get(0)?;
        let u: Option<i64> = row.get(1)?;
        Ok((f, u.unwrap_or(0)))
    }) else {
        return map;
    };
    for r in rows.flatten() {
        map.insert(r.0, r.1);
    }
    map
}
