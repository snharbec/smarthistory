//! `@` (note search) prefix mode.
//!
//! Supports the shared `#tag!` / `[[link]]!` / `[attr:value]!` / `[attr]!`
//! negation syntax (see [`crate::tui::mode::query_negation`]) — a trailing
//! `!` on a tag, link, or attribute token excludes notes that match it,
//! instead of requiring it.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use crate::tui::{CompletionKind, NotesDateFilter, char_to_byte_index};
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
    let (pattern, filter) = parse_notes_query(raw_pattern);
    // Record the resolved filter on `self` so
    // the mode-strip chip renderer (and any
    // future helper) can see what's active.
    // We update this on every refresh, even
    // when the pattern is empty (so the chip
    // disappears the moment the user clears
    // the alias token).
    app.notes_date_filter = filter;
    let (pattern, negations, type_restrictions) =
        crate::tui::mode::query_negation::split_negations(&pattern);
    let service = note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());
    let excluded = match excluded_note_filenames(&service, &negations) {
        Ok(excluded) => excluded,
        Err(e) => {
            app.set_status_message(format!("Notes mode: negation lookup failed: {}", e));
            return Ok(Vec::new());
        }
    };
    if pattern.is_empty() && type_restrictions.is_empty() {
        // The user typed only the
        // date alias (e.g. `@today`)
        // and/or only negated terms
        // (e.g. `#urgent!`). We still
        // need to fetch *all* notes
        // (no text filter) so the date
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
        return fetch_recent_with_filter(app, &service, filter, &excluded);
    }

    // `!type` restricts results to ONLY the given type(s) — build a
    // `QueryExpr` directly (native `Or`, ANDed with whatever else was
    // typed) and go through `search_notes` instead of the
    // `search_notes_by_query` string-DSL sugar, which has no hook for
    // a hand-assembled expression tree. Only taken when a restriction
    // is actually present — the plain-string path below is unchanged
    // and untouched otherwise.
    let query_results: std::result::Result<Vec<note_search::NoteResult>, String> =
        if type_restrictions.is_empty() {
            service.search_notes_by_query(&pattern)
        } else {
            let mut query_expr = if pattern.is_empty() {
                None
            } else {
                match note_search::parse_query(&pattern) {
                    Ok(expr) => Some(expr),
                    Err(e) => {
                        app.set_status_message(format!("Notes mode: invalid query: {}", e));
                        return Ok(Vec::new());
                    }
                }
            };
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
            let criteria = note_search::SearchCriteria { query_expr, ..Default::default() };
            service.search_notes(&criteria).map_err(|e| e.to_string())
        };
    match query_results {
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
                .filter(|note| !excluded.contains(&note.filename))
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
    service: &note_search::database_service::DatabaseService,
    filter: NotesDateFilter,
    excluded: &std::collections::HashSet<String>,
) -> Result<Vec<HistoryRow>> {
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
                .filter(|note| !excluded.contains(&note.filename))
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

/// Resolve `#tag!` / `[[link]]!` negated terms (see
/// [`crate::tui::mode::query_negation`]) to the set of note filenames that
/// DO have the tag/link — i.e. the filenames `fetch` must exclude. Runs one
/// ordinary positive `search_notes_by_query` lookup per term via
/// `notes.rs`'s existing string-based convenience API, since `notes::fetch`
/// doesn't otherwise build a `QueryExpr`/`SearchCriteria` directly.
fn excluded_note_filenames(
    service: &note_search::database_service::DatabaseService,
    negations: &[crate::tui::mode::query_negation::NegatedTerm],
) -> std::result::Result<std::collections::HashSet<String>, String> {
    let mut excluded = std::collections::HashSet::new();
    for term in negations {
        let rows = service
            .search_notes_by_query(&term.positive_query_string())
            .map_err(|e| format!("negation lookup for {:?} failed: {}", term, e))?;
        excluded.extend(rows.into_iter().map(|r| r.filename));
    }
    Ok(excluded)
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

/// Lazy-load the first 50 lines of the currently-selected note
/// into `output` for preview in the output preview pane.
/// Called from `App::refresh()` on every selection change so
/// the preview updates immediately when the user navigates.
/// `read_note_preview` returns the entire file, which can be
/// large; the output preview pane clamps the visible lines
/// but loading the full file into every row is wasteful.
/// Here we take only the first `SOURCE_CONTEXT_LINES` lines
/// so the preview is bounded and the pane shows useful
/// content without scrolling.
pub(crate) fn ensure_selected_context(app: &mut App) {
    

    if !matches(app) {
        return;
    }
    let Some(idx) = app.list_state.selected() else {
        return;
    };

    let (filename, existing_output) = match app.merged_rows.get(idx) {
        Some(r) if r.mode == "note" => (r.command.clone(), r.output.clone()),
        _ => return, // Not a note row (e.g., user just switched modes)
    };

    // If the output is already non-empty, we've already loaded
    // the context for this row. `read_note_preview` was called
    // during `fetch_notes` and populated `output` with the
    // full file content. However, we want to re-cap to 50 lines
    // for the preview (the full file may be thousands of lines).
    // If `output` already contains the full content, replace
    // it with just the first 50 lines.
    let Some(ref notes_dir) = app.notes_dir else {
        return;
    };
    let filepath = notes_dir.join(&filename);
    if !filepath.is_file() {
        return;
    }
    let filepath_str = filepath.to_string_lossy().into_owned();

    // Read from cache (or file) using `tags_source_cache` so
    // multiple notes in the same file share one disk read. We
    // use `line_number = 0` to get the file from the cache; the
    // real `source_context` helpers take a line number for
    // context-around-a-line, but we want the full file. Just
    // use `read_to_string` + cache directly.
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

    if content.is_empty() && existing_output.is_empty() {
        return;
    }

    // Take the first 50 lines. If the file is shorter, take all.
    let preview: String = content
        .lines()
        .take(crate::tui::SOURCE_CONTEXT_LINES)
        .collect::<Vec<_>>()
        .join("\n");

    // Syntax-highlight (same as tags / codegraph modes).
    // `highlight_with_bat_auto` detects the language from the
    // file extension.
    let highlighted =
        crate::highlight::highlight_with_bat_auto(&preview, &filepath_str).unwrap_or(preview);

    if let Some(row) = app.merged_rows.get_mut(idx)
        && row.output != highlighted {
            row.output = highlighted;
        }
}

/// Parse a notes-mode query body and extract the
/// date-filter alias.
///
/// Returns `(clean_pattern, filter)`:
/// - `clean_pattern` is the query body with any
///   `@today` / `@week` / `@month` / `@year`
///   aliases removed (and surrounding whitespace
///   collapsed). The cleaned pattern is what we
///   pass to `note_search.search_notes_by_query`.
/// - `filter` is the resolved filter; the latest
///   alias in the query wins (i.e. `@today @week`
///   ends up as `Today` because `@week` is
///   encountered second; multiple aliases is an
///   edge case — the user typically uses just
///   one).
///
/// The aliases are recognised only as
/// whole-word tokens (whitespace-separated).
/// This avoids false positives like
/// `@todayfile.md` (no alias inside) or
/// `email@today` (no alias inside). The
/// match is case-insensitive (`@Today`,
/// `@TODAY`, `@today` all work).
pub(crate) fn parse_notes_query(pattern: &str) -> (String, NotesDateFilter) {
    let mut filter = NotesDateFilter::All;
    let mut cleaned_tokens: Vec<String> = Vec::new();
    for token in pattern.split_whitespace() {
        // Date-alias path. The user types
        // `@today` (or `today`); both
        // should be recognised as the
        // date alias. We strip a leading
        // `@` so the alias is matched on
        // the bare keyword. Date aliases
        // are extracted as a filter
        // rather than passed through to
        // the search query (the
        // `note_search` library doesn't
        // know about them — we apply the
        // cutoff post-query in
        // `fetch_notes`).
        let candidate = token.strip_prefix('@').unwrap_or(token);
        match candidate.to_ascii_lowercase().as_str() {
            "today" => {
                filter = NotesDateFilter::Today;
                continue;
            }
            "week" => {
                filter = NotesDateFilter::Week;
                continue;
            }
            "month" => {
                filter = NotesDateFilter::Month;
                continue;
            }
            "year" => {
                filter = NotesDateFilter::Year;
                continue;
            }
            _ => {}
        }
        // `#TAG` — search for notes
        // tagged `TAG`. The
        // `note_search` query parser
        // already handles `#tagname`
        // syntax, so we pass the token
        // through unchanged. This lets
        // the user combine tag and text
        // search: `#feature rust` finds
        // notes tagged `feature` that
        // also mention `rust`.
        if let Some(tag) = token.strip_prefix('#') {
            if !tag.is_empty() {
                cleaned_tokens.push(format!("#{}", tag));
            }
            continue;
        }
        // `@LINK` — search for notes
        // that have a link to `LINK`.
        // The `note_search` query parser
        // uses `[[linkname]]` (wiki-link
        // syntax) for link search, so
        // we convert the user's `@LINK`
        // shorthand to `[[LINK]]`. The
        // link name preserves the
        // user's original casing
        // (link targets are
        // case-sensitive in Obsidian).
        if let Some(link) = token.strip_prefix('@') {
            if !link.is_empty() {
                cleaned_tokens.push(format!("[[{}]]", link));
            }
            continue;
        }
        // Plain text: push the token
        // verbatim. We no longer strip
        // `@` here because the date-
        // alias path above already
        // consumed the four known
        // aliases; any remaining `@foo`
        // is the user's intent for
        // `@foo` (e.g. searching for
        // the literal word `@foo` in
        // note text).
        cleaned_tokens.push(token.to_string());
    }
    (cleaned_tokens.join(" "), filter)
}

impl App {
    /// True if the current query is a note search request
    /// (prefixed with the configured notes prefix, default `@`).
    pub(crate) fn is_notes_query(&self) -> bool {
        crate::tui::mode::notes::matches(self)
    }

    /// The note search body, i.e. everything after the
    /// leading notes prefix.
    pub(crate) fn notes_pattern(&self) -> &str {
        crate::tui::mode::notes::pattern(self)
    }

    /// Tab-completion for notes (`@`) and todos (`!`)
    /// modes. The completion targets are tags (the
    /// `#TAG` token), wiki-link targets (the `@LINK`
    /// token), and `[attr:value]` / `[attr]`
    /// attribute keys/values. All completion lists
    /// come from the `note_search` database via
    /// `note_search::commands::metadata::get_unique_values`
    /// / `get_all_attributes`.
    ///
    /// Behaviour:
    /// - `#feat<TAB>` → `#feature ` (unique tag
    ///   match, trailing space)
    /// - `#f<TAB>` → `#feature` (LCP when multiple
    ///   tags share the prefix)
    /// - `@Neo<TAB>` → `@NeovimNote ` (unique link
    ///   match)
    /// - `@xyz<TAB>` → no-op + status message (no
    ///   match)
    /// - `[assig<TAB>` → `[assignee:` (unique
    ///   attribute-key match, trailing `:` so the
    ///   cursor is ready for the value)
    /// - `[assignee:sa<TAB>` → `[assignee:sarah `
    ///   (unique attribute-value match for the
    ///   `assignee` key, trailing space; the closing
    ///   `]` is left for the user to type)
    /// - Outside notes/todos mode, or when the word
    ///   before the cursor doesn't start with `#`,
    ///   `@`, or `[`, the function is a no-op so the
    ///   `Tab` key doesn't interfere with any other
    ///   mode.
    pub(crate) fn notes_tab_complete_at_cursor(&mut self) {
        // Active in notes (`@`), todos (`!`), segments (`:`), and
        // similar (`"`) mode — all four share the same
        // `notes.database` tag/link namespace, so the completion
        // source is identical. The four prefixes are each a
        // single char (the `query_prefixes.notes` / `.todo` /
        // `.segments` / `.similar` fields).
        let prefix_len: usize = 1;
        if self.query_cursor < prefix_len {
            return;
        }
        // Check the query's leading char.
        // The first char after the
        // prefix must be `#` (tag) or
        // `@` (link) for the completion
        // to be meaningful; otherwise
        // the user is just typing plain
        // text and Tab should not
        // interfere.
        let first = self
            .query
            .chars()
            .next()
            .expect("query_cursor >= 1 implies non-empty");
        if first != self.query_prefixes.notes
            && first != self.query_prefixes.todo
            && first != self.query_prefixes.segments
            && first != self.query_prefixes.similar
        {
            return;
        }
        // `[attr:value]` / `[attr]` completion: the cursor
        // may be sitting inside an unmatched `[` anywhere in
        // the query, targeting either the key (no `:` yet)
        // or the value (after a `:`) half of the token. This
        // is handled entirely separately from the `#`/`@`
        // word-boundary walk below, which can't span the `:`.
        // Checked (and, on a hit, returns) before that walk
        // so `[[link]]` typing — rejected inside the bracket
        // scan itself — still falls through unchanged.
        if let Some(ctx) = self.attr_completion_context_at_cursor(prefix_len) {
            self.attr_tab_complete(ctx);
            return;
        }
        // Walk left from the cursor
        // (character indices), stopping
        // at the first character that
        // is NOT alphanumeric or
        // underscore. Same walk as
        // JIRA field completion, but
        // applied to the notes/todos
        // body (after the single-char
        // prefix).
        let mut start_char = self.query_cursor;
        while start_char > prefix_len {
            let prev = start_char - 1;
            let ch = self.query[char_to_byte_index(&self.query, prev)
                ..char_to_byte_index(&self.query, start_char)]
                .chars()
                .next()
                .expect("non-empty slice between char indices");
            if ch == '_' || ch.is_ascii_alphanumeric() {
                start_char = prev;
            } else {
                break;
            }
        }
        // After the walk, `start_char`
        // points to the first
        // alphanumeric character of
        // the word. If the character
        // immediately before is `#`
        // (tag prefix) or `@` (link
        // prefix), include it in the
        // word by decrementing
        // `start_char`. This handles
        // `#feat` and `@Neo` where the
        // walk would otherwise stop
        // before the prefix char.
        if start_char > prefix_len {
            let prev_char_byte = char_to_byte_index(&self.query, start_char - 1);
            let curr_char_byte = char_to_byte_index(&self.query, start_char);
            let prev_ch = self.query[prev_char_byte..curr_char_byte]
                .chars()
                .next()
                .expect("non-empty slice between char indices");
            if prev_ch == '#' || prev_ch == '@' {
                start_char -= 1;
            }
        }
        let start_byte = char_to_byte_index(&self.query, start_char);
        let cursor_byte = char_to_byte_index(&self.query, self.query_cursor);
        let word = &self.query[start_byte..cursor_byte];
        if word.is_empty() {
            self.set_status_message(
                "notes-tab-complete: no tag or link name to expand".to_string(),
            );
            return;
        }
        // Determine whether the word
        // is a tag (starts with `#`)
        // or a link (starts with `@`).
        // The prefix char of the word
        // is the char at
        // `start_char` in the query
        // (or one position back if the
        // word is just `#` or `@`
        // with no alphanumeric
        // characters).
        let first_char_of_word = self.query[char_to_byte_index(&self.query, start_char)
            ..char_to_byte_index(&self.query, start_char + 1)]
            .chars()
            .next()
            .expect("start_char < cursor_byte");
        let Some(db_path) = self.notes_database.clone() else {
            // No notes database
            // configured; the
            // completion is a
            // no-op. The user
            // needs `notes.database`
            // in their config
            // for tag/link
            // completion to work.
            self.set_status_message(
                "notes-tab-complete: notes.database is not configured".to_string(),
            );
            return;
        };
        let (name_prefix, completion, kind) = match first_char_of_word {
            '#' => {
                // Tag completion: strip
                // the leading `#` and
                // query the DB for tags
                // starting with the
                // remainder. The
                // completion result from
                // `notes_tag_complete` is
                // the bare tag name
                // (e.g. `"feature "`); we
                // prepend `#` so the
                // replacement includes
                // the tag prefix.
                let name = &word[1..];
                // Check for multiple
                // matches first. If
                // the tag completion
                // is ambiguous, open
                // the completion menu
                // so the user can
                // pick from the
                // candidates rather
                // than just extending
                // to the LCP.
                let matches = crate::jira::notes_tag_matches(&db_path, name);
                if matches.len() >= 2 {
                    // Open the
                    // completion
                    // menu. The
                    // user
                    // picks a
                    // candidate
                    // and the
                    // menu
                    // applies
                    // it with
                    // a
                    // leading
                    // `#` and
                    // a
                    // trailing
                    // space.
                    self.open_completion_menu(
                        matches,
                        start_byte,
                        cursor_byte,
                        start_char,
                        CompletionKind::NotesTag,
                    );
                    return;
                }
                let result = crate::jira::notes_tag_complete(&db_path, name);
                let result = match result {
                    Some(r) => r,
                    None => {
                        self.set_status_message(format!(
                            "notes-tab-complete: no tag starts with `#{}`",
                            name
                        ));
                        return;
                    }
                };
                // Prepend `#` to the
                // completion so the
                // replacement includes
                // the tag prefix.
                let mut with_hash = String::from("#");
                with_hash.push_str(&result);
                (name.to_string(), with_hash, "tag")
            }
            '@' => {
                // Link completion: strip
                // the leading `@` and
                // query the DB for links
                // starting with the
                // remainder. The
                // completion result from
                // `notes_link_complete`
                // is the full `[[...]]`
                // expansion (e.g.
                // `[[NeovimNote]] ` or
                // `[["my note"]] ` for
                // links with spaces),
                // including the brackets
                // and a trailing space.
                // We use the result as-is
                // since the user typed
                // `@` to trigger the
                // completion but the
                // expansion uses the
                // `[[...]]` syntax (which
                // supports link names
                // with spaces, unlike
                // the `@` syntax in
                // `note_search`).
                let name = &word[1..];
                // Check for multiple
                // matches first. If
                // the link completion
                // is ambiguous, open
                // the completion menu
                // so the user can
                // pick from the
                // candidates rather
                // than just extending
                // to the LCP.
                let matches = crate::jira::notes_link_matches(&db_path, name);
                if matches.len() >= 2 {
                    // Open the
                    // completion
                    // menu. The
                    // user
                    // picks a
                    // candidate
                    // and the
                    // menu
                    // applies
                    // it
                    // wrapped
                    // in
                    // `[[...]]`
                    // with a
                    // trailing
                    // space.
                    self.open_completion_menu(
                        matches,
                        start_byte,
                        cursor_byte,
                        start_char,
                        CompletionKind::NotesLink,
                    );
                    return;
                }
                let result = crate::jira::notes_link_complete(&db_path, name);
                let result = match result {
                    Some(r) => r,
                    None => {
                        self.set_status_message(format!(
                            "notes-tab-complete: no link starts with `@{}`",
                            name
                        ));
                        return;
                    }
                };
                (name.to_string(), result, "link")
            }
            _ => {
                // Plain text: no
                // completion. The user
                // is just typing a
                // word; Tab should
                // not interfere.
                return;
            }
        };
        let _ = name_prefix; // silence unused warning; kept for future diagnostics
        // The completion string already
        // includes the leading `#` or
        // `@` and the trailing space
        // (when unique). Replace the
        // word at `start_byte` with
        // the completion.
        self.query
            .replace_range(start_byte..cursor_byte, &completion);
        let completion_chars = completion.chars().count();
        self.query_cursor = start_char + completion_chars;
        // Re-arm the debounce/idle
        // timers and refresh so the
        // new query fires its search
        // immediately. This mirrors
        // the JIRA path.
        self.llm_touch();
        self.recompile_regex();
        self.refresh();
        // Surface a status message
        // for the unique-match case
        // (the user can see the
        // expanded query in the
        // input line, but a
        // confirmation is useful
        // for the tag/link case
        // where the result has
        // many characters). We
        // don't surface a message
        // for the LCP case (no
        // trailing space) because
        // that fires every time
        // the user presses Tab to
        // disambiguate, and the
        // flash would be
        // distracting.
        if completion.ends_with(' ') {
            self.set_status_message(format!("expanded {} `{}`", kind, completion.trim_end()));
        }
    }
}
