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
    let (pattern, filter) = crate::tui::mode::notes::parse_notes_query(raw_pattern);
    // `#tag!` / `[[link]]!` negation tokens (and the `!!type`/`!type`
    // shorthand) are stripped BEFORE `parse_query` sees the pattern —
    // it has no negation primitive and would either error on the
    // trailing `!` or fold it into a bare-word token. See
    // `crate::tui::mode::query_negation`'s module doc comment.
    let (pattern, negations, type_restrictions) =
        crate::tui::mode::query_negation::split_negations(&pattern);
    let mut query_expr = if pattern.is_empty() {
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
    // `!type` restricts results to ONLY the given type(s) — native
    // `Or`, ANDed with whatever else was typed, no extra round-trip
    // (unlike exclusion below, which needs one).
    if !type_restrictions.is_empty() {
        let restriction = note_search::QueryExpr::Or(
            type_restrictions
                .iter()
                .map(|v| note_search::QueryExpr::Attribute {
                    key: "type".to_string(),
                    value: Some(v.clone()),
                })
                .collect(),
        );
        query_expr = Some(match query_expr {
            Some(existing) => note_search::QueryExpr::And(vec![existing, restriction]),
            None => restriction,
        });
    }

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
    let mut results = match service.search_todos(&criteria) {
        Ok(r) => r,
        Err(e) => {
            app.set_status_message(format!("Todo mode: search failed: {}", e));
            return Ok(Vec::new());
        }
    };

    // For each `#tag!` / `[[link]]!` negation, run the ordinary
    // POSITIVE query and exclude any todo whose (filename,
    // line_number) identity appears in that result — see
    // `crate::tui::mode::query_negation`'s module doc comment for
    // why this needs a separate lookup query per negated term.
    if !negations.is_empty() {
        let mut excluded: std::collections::HashSet<(String, i32)> = std::collections::HashSet::new();
        for term in &negations {
            let lookup_criteria = note_search::SearchCriteria {
                database_path: db_path.to_string_lossy().to_string(),
                query_expr: Some(term.positive_query_expr()),
                open: Some(true),
                list_only: true,
                ..Default::default()
            };
            match service.search_todos(&lookup_criteria) {
                Ok(rows) => excluded.extend(rows.into_iter().map(|r| (r.filename, r.line_number))),
                Err(e) => {
                    app.set_status_message(format!(
                        "Todo mode: negation lookup for {:?} failed: {}",
                        term, e
                    ));
                    return Ok(Vec::new());
                }
            }
        }
        results.retain(|r| !excluded.contains(&(r.filename.clone(), r.line_number)));
    }

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

    // Syntax-highlight (same as tags / codegraph / notes modes).
    let filepath_str = filepath.to_string_lossy().into_owned();
    let highlighted =
        crate::highlight::highlight_with_bat_auto(&preview, &filepath_str).unwrap_or(preview);

    if let Some(row) = app.merged_rows.get_mut(idx)
        && row.output != highlighted {
            row.output = highlighted;
        }
}

impl App {
    /// True if the current query is a todo search
    /// request (prefixed with the configured todo
    /// prefix, default `!`). The todo mode scans
    /// every file in the configured notes
    /// directory for lines that look like todo
    /// items (markdown task-list checkboxes:
    /// `- [ ] text` / `- [x] text`) and lists each
    /// match as its own row in the TUI.
    pub(crate) fn is_todo_query(&self) -> bool {
        crate::tui::mode::todo::matches(self)
    }



    /// Mark the currently-selected todo
    /// entry as done by toggling the
    /// checkbox marker on its line in
    /// the source file from `[ ]` to
    /// `[x]`, then refresh the todo
    /// list. The action is intended to
    /// be invoked only from the todo
    /// search mode (the dispatcher
    /// already gates on
    /// `is_todo_query`); the helper
    /// itself also re-checks the mode
    /// so a stray test or future caller
    /// can't trigger a file write from
    /// outside todo mode.
    ///
    /// The selected row's `id` is
    /// synthetic: `id = -(line_number)`
    /// where `line_number` is the
    /// 1-based line in the note file
    /// that contains the todo. The
    /// row's `comment` field is the
    /// filename (relative to
    /// `notes.dir`).
    ///
    /// On success the todo list is
    /// re-fetched so the toggled row
    /// disappears (the underlying
    /// query filters `open: true`).
    /// On any error (no selection,
    /// missing file, the line no
    /// longer matches a todo
    /// checkbox, write failure) a
    /// status message is surfaced and
    /// the list is left untouched.
    pub(crate) fn mark_todo_done(&mut self) {
        // Re-gate here so a stray
        // caller can't write to a
        // file from outside todo mode.
        // The dispatcher already gates
        // on this, but the helper
        // defends against future
        // refactors that might call it
        // from a different code path.
        if !self.is_todo_query() {
            self.set_status_message(
                "Mark-todo-done is only available in todo search (type `!`)".to_string(),
            );
            return;
        }
        let targets = self.smart_action_targets();
        let Some((first, rest)) = targets.split_first() else {
            self.set_status_message("No todo selected".to_string());
            return;
        };
        if rest.is_empty() {
            // Single target: preserve `mark_todo_done_for_row`'s
            // own precise status message (e.g. "Marked done:
            // file:line", or a specific failure reason) rather
            // than overwriting it with a generic aggregate count.
            self.mark_todo_done_for_row(first);
            return;
        }
        // Multiple marked todos: run each (captured line numbers
        // stay valid across the loop's per-row `refresh()` calls
        // — toggling a checkbox doesn't add/remove lines, so
        // other rows' line numbers don't shift), then report an
        // aggregate count. Marks are left as-is afterward (the
        // rows still exist, just closed — unlike bulk delete,
        // there's no reason to force the user to re-mark them
        // for a follow-up action).
        let mut done = 0usize;
        let total = targets.len();
        for row in &targets {
            if self.mark_todo_done_for_row(row) {
                done += 1;
            }
        }
        self.set_status_message(format!("Marked {} of {} todos done", done, total));
    }

    /// Returns `true` on full success (file written AND, when
    /// notes.database is configured, the DB re-sync succeeded)
    /// so `mark_todo_done`'s multi-target loop can count
    /// successes for its aggregate status message; `false` on
    /// every early-exit / failure branch. Each branch still sets
    /// its own precise `status_message` — the bool is purely for
    /// the caller's tally, not a replacement for that messaging.
    ///
    /// Core implementation of
    /// `mark_todo_done`, factored out
    /// so tests can drive it with a
    /// hand-crafted `HistoryRow`
    /// (the indentation test in
    /// particular needs a row whose
    /// line content the library
    /// wouldn't normally index,
    /// since the library's
    /// `TODO_REGEX` is `^`-anchored
    /// and skips indented
    /// checkboxes).
    pub(crate) fn mark_todo_done_for_row(&mut self, row: &HistoryRow) -> bool {
        // `id = -(line_number)` is the
        // synthetic-id contract from
        // `fetch_todos`. A row that
        // somehow has a non-negative id
        // (a real history row mixed in
        // — shouldn't happen in todo
        // mode, but defensively) is
        // rejected.
        let line_number: usize = match row.id {
            i if i < 0 => (i.unsigned_abs() as usize).max(1),
            _ => {
                self.set_status_message("Selected row is not a todo entry".to_string());
                return false;
            }
        };
        if row.comment.is_empty() {
            self.set_status_message("Selected todo has no source filename".to_string());
            return false;
        }
        let Some(ref notes_dir) = self.notes_dir else {
            self.set_status_message("Cannot mark done: notes.dir is not configured".to_string());
            return false;
        };
        let path = notes_dir.join(&row.comment);
        // Read the file. We use the
        // same error mapping as the
        // note-preview reader for
        // consistency with the rest of
        // the notes subsystem.
        let contents = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                self.set_status_message(format!("Cannot read {}: {}", row.comment, e));
                return false;
            }
        };
        // Locate the targeted line.
        // The note_search indexer uses
        // 1-based line numbers, so we
        // index into a `lines()` iterator
        // by skipping the first
        // `line_number - 1` lines.
        let mut new_lines: Vec<String> = Vec::new();
        let mut toggled = false;
        for (i, line) in contents.lines().enumerate() {
            let n = i + 1; // 1-based
            if n == line_number {
                // The line at this
                // position should still
                // look like an open todo
                // checkbox. We tolerate
                // leading whitespace
                // (indented list items) and
                // both `-` and `*` bullets
                // (even though the
                // library's parser only
                // recognises `-`, the
                // user may have written
                // `* [ ]` by hand and
                // toggling it should
                // still work).
                let trimmed_start = line
                    .trim_start()
                    .trim_start_matches(['-', '*'])
                    .trim_start();
                if !trimmed_start.starts_with("[ ]") {
                    self.set_status_message(format!(
                        "Line {} of {} is no longer an open todo: {:?}",
                        line_number, row.comment, line
                    ));
                    return false;
                }
                // Replace the first
                // occurrence of `[ ]` on
                // this line with `[x]`.
                // We anchor on the
                // prefix-only match so we
                // don't accidentally
                // toggle a `[ ]` inside
                // the todo text (rare,
                // but possible — e.g.
                // "see [ ] in checklist").
                // The first `[ ]` on a
                // markdown-checkbox line
                // is always the checkbox
                // marker.
                let prefix_len = line.len() - trimmed_start.len();
                // Walk past the bullet
                // character(s) so we
                // match the checkbox
                // bracket that follows
                // the bullet, not any
                // bracketed text inside
                // the todo content.
                let rest = &line[prefix_len..];
                let bullet_skip: usize = rest
                    .chars()
                    .take_while(|c| matches!(c, '-' | '*'))
                    .map(|c| c.len_utf8())
                    .sum();
                let after_bullet = prefix_len + bullet_skip;
                let ws_skip: usize = line[after_bullet..]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .map(|c| c.len_utf8())
                    .sum();
                let bracket_at = after_bullet + ws_skip;
                // Defensive: bail out if
                // the bracket isn't where
                // we expect it. The
                // prefix check above
                // already guaranteed
                // `[ ]` is at the
                // trimmed-start, so this
                // is purely belt-and-
                // braces.
                if !line[bracket_at..].starts_with("[ ]") {
                    self.set_status_message(format!(
                        "Cannot locate checkbox bracket on line {} of {}",
                        line_number, row.comment
                    ));
                    return false;
                }
                let mut new_line = String::with_capacity(line.len());
                new_line.push_str(&line[..bracket_at]);
                new_line.push_str("[x]");
                new_line.push_str(&line[bracket_at + 3..]);
                new_lines.push(new_line);
                toggled = true;
            } else {
                new_lines.push(line.to_string());
            }
        }
        if !toggled {
            // Shouldn't happen: we
            // iterated over every line
            // and only mark `toggled`
            // when `n == line_number`. If
            // we exit the loop without
            // toggling, the file has
            // fewer lines than the
            // indexer saw.
            self.set_status_message(format!(
                "Line {} not found in {} (file shorter than expected)",
                line_number, row.comment
            ));
            return false;
        }
        // Preserve the file's trailing
        // newline convention. `lines()`
        // drops the trailing `\n` if
        // any, so we re-attach it when
        // the original ended with one.
        let mut out = new_lines.join("\n");
        if contents.ends_with('\n') {
            out.push('\n');
        }
        if let Err(e) = std::fs::write(&path, out) {
            self.set_status_message(format!("Cannot write {}: {}", row.comment, e));
            return false;
        }
        // Refresh the in-memory
        // `todo_entries` database
        // after the file toggle. We
        // delegate to the
        // `note_search` library's
        // `update_files_in_db`
        // function: it re-parses the
        // modified file, upserts the
        // `markdown_data` row, and
        // replaces the
        // `todo_entries` rows for
        // that file. After the
        // update, the toggled todo
        // has `closed = 1` in the
        // database, and the next
        // `refresh()` (which queries
        // with `open: true`) will
        // exclude it from the list.
        //
        // We open a fresh `Connection`
        // per call rather than
        // keeping one open on `App`,
        // because the existing
        // `DatabaseService::search_todos`
        // already opens its own
        // connection per query and
        // the action is invoked
        // rarely (the user has to
        // press `Ctrl+X` to trigger
        // it). Opening a new
        // connection is cheaper than
        // the file write we just
        // did.
        //
        // The DB update is best-effort:
        // if the library can't open
        // the DB or write to it, the
        // status message reflects
        // the failure but the file
        // is already correct on
        // disk — the user can always
        // run their external indexer
        // to recover. We don't roll
        // back the file write
        // because the user's intent
        // was clearly "mark this
        // done on disk"; reverting
        // would be worse than a
        // temporarily-stale DB.
        let notes_dir_for_db = self.notes_dir.clone();
        let notes_db_for_db = self.notes_database.clone();
        let filename_for_db = row.comment.clone();
        if let (Some(dir), Some(db)) = (notes_dir_for_db.as_ref(), notes_db_for_db.as_ref()) {
            use rusqlite::Connection;
            match Connection::open(db) {
                Ok(conn) => {
                    if let Err(e) = note_search::update_files_in_db(&[filename_for_db], dir, &conn)
                    {
                        self.set_status_message(format!(
                            "Marked done on disk, but DB refresh failed: {}",
                            e
                        ));
                        return false;
                    }
                }
                Err(e) => {
                    self.set_status_message(format!(
                        "Marked done on disk, but DB open failed: {}",
                        e
                    ));
                    return false;
                }
            }
        }
        self.set_status_message(format!("Marked done: {}:{}", row.comment, line_number));
        // Re-fetch so the toggled row
        // disappears from the list.
        // The library's
        // `todo_entries` row for
        // this todo now has
        // `closed = 1`, so the
        // `open: true` filter in the
        // underlying SQL excludes
        // it.
        self.refresh();
        true
    }
}
