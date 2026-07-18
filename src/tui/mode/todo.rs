//! `!` (todo search) prefix mode.
//!
//! The todo mode scans every file in the configured notes
//! directory for lines that look like todo items
//! (markdown task-list checkboxes: `- [ ] text` / `- [x] text`)
//! and lists each match as its own row in the TUI.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

/// True if the current query is a todo search
/// request (prefixed with the configured todo
/// prefix, default `!`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.todo;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// Health check for the todos (`!`) mode. The todo
/// mode uses the same `note_search` library and
/// the same `notes.database` as the notes mode, so
/// the configuration + DB-connection checks are
/// identical. The mode-specific bit is the
/// `search_todos` round-trip and the "are there
/// actually any open todos?" question.
///
/// Mirrors `notes::check` step-for-step so the
/// failure modes are easy to compare across the
/// two modes (notes / todos). When the notes
/// database is shared (the typical case) and
/// notes is healthy, the user will see
/// `todo: ok` immediately after `notes: ok`
/// without redundant diagnostic depth.
pub(crate) fn check(app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Todo;

    // 1. Configuration.
    let Some(db_path) = app.notes_database.as_ref() else {
        return CheckReport::err(
            mode,
            "notes.database is not configured (the todo index lives in the same database; set notes.database in ~/.config/smarthistory/config)",
        );
    };
    // The todo mode also reads `notes.dir`
    // (the live directory) when previewing
    // todo-line context. A missing
    // `notes.dir` doesn't break the search
    // itself, but it does break the
    // details-pane preview. Surface as a
    // Warning so the user knows.
    let dir_warning = if app.notes_dir.is_none() {
        Some(CheckReport::warn(
            mode,
            "notes.dir is not configured; the todo-line preview pane will be empty (the search itself still works)",
        ))
    } else {
        None
    };

    // 2-4. File / sqlite / required tables — reuse
    //     the notes-mode probes (same DB).
    if !db_path.exists() {
        return CheckReport::err(
            mode,
            format!("notes.database file does not exist: {}", db_path.display()),
        );
    }
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
    let required = ["markdown_data", "todo_entries"];
    for table in &required {
        let present: Result<i64, _> = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            rusqlite::params![table],
            |row| row.get(0),
        );
        if matches!(present, Ok(n) if n > 0) {
            continue;
        }
        return CheckReport::err(
            mode,
            format!("required table `{table}` is missing (the notes DB is incomplete or from an incompatible note_search version)"),
        );
    }

    // 5. Sample `search_todos` (open: Some(true),
    //    no text filter). A success proves the
    //    full search pipeline works. A failure
    //    here is almost always a library
    //    version mismatch (the SQL schema
    //    changed).
    let service = note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());
    let criteria = note_search::SearchCriteria {
        database_path: db_path.to_string_lossy().to_string(),
        note_dir: app
            .notes_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        open: Some(true),
        sort_order: Some(note_search::SortOrder::Modified),
        ..Default::default()
    };
    let todos = match service.search_todos(&criteria) {
        Ok(r) => r,
        Err(e) => {
            return CheckReport::err(mode, format!("search_todos() failed: {e}"));
        }
    };

    // 6. Informational: row count. Zero open
    //    todos is a valid state but the user
    //    probably wants to know.
    let mut report = if todos.is_empty() {
        CheckReport::warn(
            mode,
            "no open todos in the index (search itself is healthy; you may want to add some or run `note_search index`)",
        )
    } else {
        CheckReport::ok(mode, format!("{} open todo(s) indexed", todos.len()))
    };
    if let Some(w) = dir_warning {
        report = report.with(w);
    }
    report
        .with(CheckReport::ok(
            mode,
            format!("opened {}", db_path.display()),
        ))
        .with(CheckReport::ok(
            mode,
            format!("required tables present: {}", required.join(", ")),
        ))
        .with(CheckReport::ok(
            mode,
            format!("sample search_todos() returned {} row(s)", todos.len()),
        ))
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

/// Fetch the todo-mode result set.
///
/// Steps:
/// 1. Bail with a status message if no
///    `notes.database` is configured (mirrors
///    the notes-mode UX: emit a soft message
///    and return empty so the user sees a
///    clear reason, not a confusing blank
///    list).
/// 2. Parse the typed query for date-filter
///    aliases (`@today`, `@week`, `@month`,
///    `@year`) and the Obsidian-like search
///    expression (`#tag`, `[[link]]`,
///    `[attr:value]`). Going through
///    `parse_query` instead of stuffing the
///    raw pattern into `criteria.text` is
///    what makes tags / links / attributes
///    work — the user types `!#urgent older`
///    and gets only the todos tagged `urgent`
///    that also contain `older`.
/// 3. Build a `note_search::SearchCriteria`
///    that always pins `open: Some(true)`
///    (the user explicitly asked for "all
///    open todo entries") and uses
///    `SortOrder::Modified` (newest files
///    first, then by filename and line number
///    within a file).
/// 4. Run `service.search_todos(&criteria)`
///    and map the library's `TodoResult`
///    rows into `HistoryRow`s. Each todo
///    becomes its own row with a synthetic
///    negative `id` (so it doesn't collide
///    with real history rows; the magnitude
///    carries the 1-based line number for
///    staging).
/// 5. Enrich with the file's `updated`
///    timestamp (one batched query against
///    `markdown_data` for the unique
///    filenames, then a per-row lookup) so
///    the details pane shows a real age
///    instead of the `9999M` placeholder.
/// 6. Apply the date-filter alias (if any)
///    post-sort. Rows with `timestamp = 0`
///    (transient — the library never gave us
///    a file mtime, the next indexer run
///    resolves it) are excluded from any
///    active filter.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    // We delegate to the note_search library
    // the same way `fetch_notes` does. The
    // library is the canonical source for
    // todo data: the indexer parses every
    // note in `notes.dir` at update time
    // and stores each todo in the
    // `todo_entries` table, with the line
    // number, the (open/closed) state, the
    // priority, due date, tags, etc. Scanning
    // the filesystem ourselves would re-do
    // that work in Rust, and worse: it
    // wouldn't see todos that the user has
    // indexed through `note_search` but that
    // live in a directory our `notes.dir`
    // path doesn't point at. Going through
    // the library guarantees the user sees
    // exactly what `note_search list` would
    // show.
    let Some(ref db_path) = app.notes_database else {
        // Without a notes database we can't
        // query todos. Mirror the notes-mode
        // UX: emit a soft status message and
        // return an empty list so the user
        // sees a clear "no todos" reason
        // rather than a confusing empty list.
        app.set_status_message("Todo mode: notes.database is not configured".to_string());
        return Ok(Vec::new());
    };

    // Strip the date-filter aliases
    // (`@today`, `@week`, `@month`, `@year`)
    // from the query body. The remaining
    // text is passed to `parse_query`,
    // which understands the Obsidian-like
    // syntax: bare words are AND-matched
    // against each todo line, `#tag` is
    // matched against both the todo's own
    // tags and the note's header fields,
    // `[[link]]` is matched against the
    // todo's links and the note's
    // outgoing links, and `[attr:value]`
    // is matched against the note's
    // header fields. Going through
    // `parse_query` instead of stuffing
    // the raw pattern into `criteria.text`
    // is what makes tags / links /
    // attributes work — the user types
    // `!#urgent older` and gets only the
    // todos tagged `urgent` that also
    // contain `older`.
    let raw_pattern = pattern(app).trim();
    let (pattern, filter) = crate::tui::parse_notes_query(raw_pattern);
    let query_expr = if pattern.is_empty() {
        None
    } else {
        match note_search::parse_query(&pattern) {
            Ok(expr) => Some(expr),
            Err(e) => {
                app.set_status_message(format!("Todo mode: invalid query: {}", e));
                return Ok(Vec::new());
            }
        }
    };

    // Build the criteria. We always pin
    // `open: Some(true)` so the user sees
    // only uncompleted todos — the user
    // explicitly asked for "all open todo
    // entries". The `SortOrder::Modified`
    // matches the user's request to order
    // by timestamp: the library emits
    // `ORDER BY m.updated DESC, t.filename,
    // t.line_number`, i.e. newest files
    // first, then by filename and line
    // number within a file.
    let criteria = note_search::SearchCriteria {
        database_path: db_path.to_string_lossy().to_string(),
        note_dir: app
            .notes_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        open: Some(true),
        sort_order: Some(note_search::SortOrder::Modified),
        query_expr,
        ..Default::default()
    };
    // The `query_expr` field is the
    // modern way to filter; we leave
    // `criteria.text` unset so the
    // library doesn't add a redundant
    // text-LIKE clause on top of the
    // expression tree. The two paths
    // would otherwise AND together,
    // which is harmless but wasteful.
    debug_assert!(criteria.text.is_none());

    let service = note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());
    let results = match service.search_todos(&criteria) {
        Ok(r) => r,
        Err(e) => {
            app.set_status_message(format!("Todo mode: search failed: {}", e));
            return Ok(Vec::new());
        }
    };

    // Map the library's `TodoResult` rows
    // into our `HistoryRow` representation.
    // Each todo line becomes its own row;
    // the library's `line_number` is
    // 1-based, which matches what the
    // editor will use when it opens the
    // file.
    let mut rows: Vec<HistoryRow> = {
        // Read each unique file's
        // `updated` timestamp from the
        // `markdown_data` table so the
        // details pane can show a real
        // age instead of the
        // `9999M` placeholder. The
        // library's `TodoResult` doesn't
        // expose `updated` (only the
        // note's `header_fields`), so we
        // do one extra batched query:
        // distinct filenames from the
        // result set, fetch `updated`
        // for each, build a lookup map,
        // and use it when constructing
        // the rows. Doing one query per
        // file is much cheaper than the
        // per-row N+1 we would otherwise
        // have.
        let mut unique_files: Vec<String> = results.iter().map(|r| r.filename.clone()).collect();
        unique_files.sort();
        unique_files.dedup();
        let mtimes = crate::tui::mode::notes::fetch_file_updated_timestamps(db_path, &unique_files);
        results
            .iter()
            .map(|r| {
                let line_number: usize = r.line_number.max(1) as usize;
                // Fall back to `0` only
                // when the database has
                // no `updated` for this
                // file (the user has
                // never indexed it — a
                // transient state that
                // goes away on next
                // index). Anything better
                // than a placeholder is
                // preferable, so we
                // prefer the actual
                // `updated` value when
                // available.
                let ts = mtimes.get(&r.filename).copied().unwrap_or(0);
                HistoryRow {
                    // Synthetic negative
                    // id so it doesn't
                    // collide with real
                    // history rows; the
                    // magnitude carries
                    // the line number
                    // for human
                    // debugging
                    // (`id = -42` means
                    // line 42).
                    id: -(line_number as i64),
                    command: r.text.clone(),
                    directory: app
                        .notes_dir
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    session_id: String::new(),
                    exit_code: 0,
                    timestamp: ts,
                    comment: r.filename.clone(),
                    output: r.text.clone(),
                    mode: "todo".to_string(),
                    source: String::new(),
                    ..Default::default()
                }
            })
            .collect()
    };
    // The library already returned rows
    // sorted by `m.updated DESC,
    // t.filename, t.line_number` (newest
    // files first, then by line within a
    // file). With the real `updated`
    // timestamps now in `row.timestamp`,
    // a defensive re-sort is still
    // useful — if two files share the
    // same `updated` value (which
    // happens when a single indexing
    // pass touches several files at
    // once), the library's tie-break by
    // filename gives a stable order
    // but it can differ from what we
    // want here (the synthetic `id` is
    // the line number, so reverse-id is
    // a top-to-bottom read within the
    // file).
    rows.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| b.id.cmp(&a.id)));
    // Apply the date-filter alias
    // (if any) post-sort. Each
    // row's `timestamp` is the
    // file's `updated` epoch
    // (populated by
    // `fetch_file_updated_timestamps`),
    // so the `cutoff` math is
    // the same as in
    // `fetch_notes`. Rows with
    // `timestamp = 0` (the
    // library never gave us a
    // file mtime — a transient
    // state that resolves on
    // the next indexer run) are
    // excluded from any active
    // filter, the same way
    // missing timestamps are
    // handled in notes mode.
    if let Some(cutoff) = filter.cutoff(app.now_epoch()) {
        rows.retain(|r| r.timestamp >= cutoff);
    }
    app.notes_date_filter = filter;
    Ok(rows)
}

/// Lazy-load the content of the note file containing the
/// currently-selected todo line into `output` for preview in
/// the output preview pane. Called from `App::refresh()` on
/// every selection change so the preview updates immediately
/// when the user navigates. The todo row carries the
/// filename in `comment` and the notes directory in
/// `directory`; we read the first 50 lines of the file.
pub(crate) fn ensure_selected_context(app: &mut App) {
    

    if !matches(app) {
        return;
    }
    let Some(idx) = app.list_state.selected() else {
        return;
    };

    let (filename, _dir) = match app.merged_rows.get(idx) {
        Some(r) if r.mode == "todo" => (r.comment.clone(), r.directory.clone()),
        _ => return,
    };

    let Some(ref notes_dir) = app.notes_dir else {
        return;
    };
    let filepath = notes_dir.join(&filename);
    if !filepath.is_file() {
        return;
    }

    // Read from the shared cache so multiple rows in the same
    // note file share one disk read.
    let content = {
        use std::collections::HashMap;
        use std::path::PathBuf;
        let cache: &mut HashMap<PathBuf, String> = &mut app.tags_source_cache;
        if !cache.contains_key(&filepath) {
            match std::fs::read_to_string(&filepath) {
                Ok(s) => {
                    cache.insert(filepath.clone(), s);
                }
                Err(_) => return,
            }
        }
        cache.get(&filepath).cloned().unwrap_or_default()
    };

    if content.is_empty() {
        return;
    }

    let preview: String = content
        .lines()
        .take(crate::tui::SOURCE_CONTEXT_LINES)
        .collect::<Vec<_>>()
        .join("\n");

    // Pipe through `bat` for syntax highlighting (same as
    // tags / codegraph / notes modes).
    let filepath_str = filepath.to_string_lossy().into_owned();
    let highlighted =
        crate::highlight::highlight_with_bat_auto(&preview, &filepath_str).unwrap_or(preview);

    if let Some(row) = app.merged_rows.get_mut(idx)
        && row.output != highlighted {
            row.output = highlighted;
        }
}
