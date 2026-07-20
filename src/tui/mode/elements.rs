//! `:` (element search) prefix mode.
//!
//! Finer-grained than `@` (notes): searches individual
//! paragraphs, list items (with nested children folded into
//! their parent, but each child also indexed as its own
//! element), and headings via `note_search`'s `elements` table,
//! rather than whole files. A tag or link on a heading (or in
//! the document's frontmatter) cascades to every element in
//! that heading's section — see the upstream `note_search`
//! README's "Element Search" section for the full semantics.
//!
//! Same query language as `notes` / `todo` mode: the typed
//! pattern is parsed via `note_search::parse_query` into a
//! `QueryExpr` tree and passed as `criteria.query_expr`, so
//! `#tag`, `[[link]]`, `[attr:value]`, `(a OR b)`, and bare-word
//! AND-matching all work here too (`QueryBuilder::build_element_query`
//! recurses the same expression tree `build_query_from_expr` /
//! `build_note_query_from_expr` use, just scoped to the
//! `elements` table). This wasn't always true — `note_search`'s
//! element search originally only took separate `tags`/`links`/
//! `text` fields with no query DSL; upstream added `query_expr`
//! support for elements in a follow-up commit ("Support query
//! for elements").
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

/// How long the elements-mode debounce waits after the last
/// keystroke before spawning the background search. Same value
/// as JIRA / ag / files mode (400 ms).
pub const ELEMENTS_DEBOUNCE: Duration = Duration::from_millis(400);

/// An in-flight elements search. The background thread sends the
/// result over `receiver`; the run loop polls it. `cancelled`
/// lets the run loop abort a stale search (e.g. the user pressed
/// `Cancel` or the query changed again before this one finished).
pub struct ElementsRequest {
    pub receiver: mpsc::Receiver<Result<Vec<HistoryRow>, String>>,
    pub cancelled: Arc<AtomicBool>,
    /// The pattern that was being searched for, so the caller can
    /// tell whether this result is still relevant when it arrives
    /// (the user may have kept typing in the meantime).
    pub pattern: String,
}

/// Aggregated elements-mode async-search state. Held by the TUI
/// `App`, mirrors `AgState` / `FilesState` exactly: a query on
/// this mode's own `SearchCriteria`/`DatabaseService` is a
/// synchronous SQLite round-trip that can take long enough (an
/// unfiltered `:` on a large notes vault touches every indexed
/// paragraph/list-item/heading) to make the very first keystroke
/// after switching into the mode feel like it's not registering.
/// Running it on a background thread, debounced the same way
/// `,` (ag) / `-` (JIRA) / `~` (files) mode already are, decouples
/// typing responsiveness from how long the search itself takes.
pub struct ElementsState {
    /// Debounce timer, armed on every keystroke in elements mode.
    pub debounce_started: Option<std::time::Instant>,
    /// Last successfully searched pattern. Prevents re-querying
    /// when the pattern hasn't changed.
    pub last_pattern: Option<String>,
    /// Whether a search is currently in flight.
    pub in_flight: bool,
    /// In-flight request (background thread).
    pub request: Option<ElementsRequest>,
    /// Cached results of the most recent search.
    pub rows: Vec<HistoryRow>,
    /// `bat`-highlighted output preview, keyed by (absolute file
    /// path, 1-based start line). `App::refresh()` runs on every
    /// keystroke, which rebuilds `merged_rows` from scratch (from
    /// this struct's own `rows`, whose `output` is always the raw
    /// unhighlighted element text) — without this cache,
    /// `ensure_selected_context` would re-spawn `bat` on the same
    /// selected row on every single keystroke, which is exactly
    /// the kind of per-keystroke blocking work the background
    /// search thread was introduced to eliminate. The selected
    /// row's file/line rarely changes between keystrokes, so this
    /// cache turns that into a one-time cost per row.
    pub context_cache: std::collections::HashMap<(String, usize), String>,
}

impl ElementsState {
    pub fn new() -> Self {
        ElementsState {
            debounce_started: None,
            last_pattern: None,
            in_flight: false,
            request: None,
            rows: Vec::new(),
            context_cache: std::collections::HashMap::new(),
        }
    }

    /// Extract the body after the prefix character. Mirrors
    /// `AgState::current_pattern`.
    pub fn current_pattern(query: &str, prefix: char) -> String {
        let body = if query.starts_with(prefix) {
            &query[prefix.len_utf8()..]
        } else {
            query
        };
        body.trim().to_string()
    }

    pub fn has_results_for(&self, pattern: &str) -> bool {
        self.last_pattern.as_deref() == Some(pattern)
    }
}

impl Default for ElementsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn a background thread that runs the `note_search` element
/// query and sends the mapped `HistoryRow`s (or an error message)
/// back over the channel. Mirrors `crate::ag::spawn_ag_search`.
pub fn spawn_elements_search(
    db_path: std::path::PathBuf,
    notes_dir: Option<std::path::PathBuf>,
    pattern: String,
) -> ElementsRequest {
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let pattern_for_thread = pattern.clone();

    std::thread::spawn(move || {
        let result = run_elements_search(&db_path, notes_dir.as_deref(), &pattern_for_thread);
        if !cancelled_clone.load(Ordering::Relaxed) {
            let _ = tx.send(result);
        }
    });

    ElementsRequest {
        receiver: rx,
        cancelled,
        pattern,
    }
}

/// The actual (synchronous, but run on a background thread)
/// query + row-mapping. Factored out of `spawn_elements_search`
/// so it has no channel/thread concerns of its own — just "given
/// a database and a pattern, return rows or an error message".
fn run_elements_search(
    db_path: &std::path::Path,
    notes_dir: Option<&std::path::Path>,
    pattern: &str,
) -> Result<Vec<HistoryRow>, String> {
    let query_expr = if pattern.is_empty() {
        None
    } else {
        Some(note_search::parse_query(pattern).map_err(|e| format!("invalid query: {}", e))?)
    };

    let criteria = note_search::SearchCriteria {
        database_path: db_path.to_string_lossy().to_string(),
        query_expr,
        sort_order: Some(note_search::SortOrder::Modified),
        ..Default::default()
    };
    // `query_expr`, when set, is the sole source of the
    // filter — `criteria.text` stays unset so the library
    // doesn't AND a redundant text-LIKE clause on top of the
    // expression tree (same reasoning `todo::fetch` documents
    // for its own `debug_assert!`).
    debug_assert!(criteria.text.is_none());

    let service = note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());
    let results = service
        .search_elements(&criteria)
        .map_err(|e| format!("search failed: {}", e))?;
    Ok(map_element_results(&results, notes_dir))
}

/// Map `note_search`'s `ElementResult` rows into `HistoryRow`s.
fn map_element_results(
    results: &[note_search::database_service::ElementResult],
    notes_dir: Option<&std::path::Path>,
) -> Vec<HistoryRow> {
    results
        .iter()
        .map(|el| {
            // Headings get a `#`/`##`/... prefix so the list
            // visually distinguishes them from plain
            // paragraphs / list items — the same convention
            // markdown itself uses.
            let heading_prefix = el
                .heading_level
                .filter(|l| *l > 0)
                .map(|l| format!("{} ", "#".repeat(l as usize)))
                .unwrap_or_default();
            // Internal newlines (a list item's nested
            // children, a multi-line paragraph) are joined
            // with " / " for a scannable single line — same
            // convention `note_search`'s own default output
            // format uses (see `ElementResult::formatted_string`
            // upstream).
            let display_text = el.text.replace('\n', " / ");
            let full_path = notes_dir
                .map(|d| d.join(&el.filename).display().to_string())
                .unwrap_or_default();
            HistoryRow {
                // Synthetic negative id, same convention as
                // todo mode's `id = -(line_number)`. Not
                // globally unique across files (two files
                // can both have an element starting on the
                // same line) — `App`'s `marked_ids` already
                // handles that generically by keying on
                // `(id, comment)`, and `directory` +
                // `session_id` (not `id`) are what staging
                // actually uses to open the right file.
                id: -(el.start_line as i64),
                command: format!("{}{}", heading_prefix, display_text),
                // `directory` / `session_id` carry the
                // absolute file path / line number — the
                // same convention `tags` / `ag` / `codegraph`
                // use for `stage_editor_open_at_line`.
                directory: full_path,
                session_id: el.start_line.to_string(),
                exit_code: 0,
                timestamp: el.updated.unwrap_or(0),
                // Set to the element's own text here;
                // `ensure_selected_context` unconditionally
                // replaces this with a window of the full
                // underlying file once the row is actually
                // selected, so this initial value is only
                // what's briefly visible before that runs (or
                // the fallback if the file can't be read).
                comment: el.filename.clone(),
                output: el.text.clone(),
                mode: "element".to_string(),
                source: String::new(),
                ..Default::default()
            }
        })
        .collect()
}

/// True if the current query is an element search request
/// (prefixed with the configured elements prefix, default `:`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.elements;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The element search body, i.e. everything after the leading
/// elements prefix.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.elements;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Health check for the elements (`:`) mode. Mirrors
/// `notes::check` step-for-step (same `notes.database`, same
/// connection), but probes for the `elements` table instead of
/// `todo_entries` — a notes database indexed by a
/// `note_search` version older than the "search for elements"
/// feature won't have it yet, which is exactly the failure mode
/// this check exists to surface clearly (rather than a cryptic
/// "no such table: elements" SQL error at search time).
pub(crate) fn check(app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Elements;

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

    // `elements` is the new table this whole mode depends on —
    // a database indexed by an older `note_search` build won't
    // have it. `markdown_data` is checked too since
    // `search_elements` joins against it for the `updated`
    // timestamp.
    let required_tables = ["markdown_data", "elements"];
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
                    format!(
                        "required table `{table}` is missing (re-run `note_search import` with a note_search build that supports element search, then re-index)"
                    ),
                );
            }
            Err(e) => {
                return CheckReport::err(mode, format!("failed to probe for table `{table}`: {e}"));
            }
        }
    }

    let service = note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());
    let criteria = note_search::SearchCriteria::default();
    let rows = match service.search_elements(&criteria) {
        Ok(r) => r,
        Err(e) => {
            return CheckReport::err(
                mode,
                format!("search_elements() failed on an empty query: {e}"),
            );
        }
    };

    if rows.is_empty() {
        return CheckReport::warn(
            mode,
            "notes database is reachable but contains 0 indexed elements (re-index with a note_search build that supports element search)".to_string(),
        )
        .with(CheckReport::ok(
            mode,
            format!("opened {}", db_path.display()),
        ));
    }

    CheckReport::ok(
        mode,
        format!("{} elements indexed in {}", rows.len(), db_path.display()),
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
        format!("sample search_elements() returned {} row(s)", rows.len()),
    ))
}

/// Fetch the elements-mode result set. The actual query runs on
/// a background thread (spawned by `App::elements_touch` →
/// `spawn_elements_search`, debounced by `App::elements_maybe_autocall`),
/// so this just clones the cached rows from `App::elements_state`
/// — mirrors `crate::tui::mode::ag::fetch` exactly. Decoupling
/// the query from this synchronous `fetch()` call is the whole
/// point: `fetch()` runs on every keystroke (via `App::refresh`),
/// and an unfiltered `:` on a large notes vault touches every
/// indexed paragraph/list-item/heading — synchronously blocking
/// on that from the main thread was making the first keystroke
/// after switching into the mode feel unresponsive.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    Ok(app.elements_state.rows.clone())
}

/// Lazy-load context around the SELECTED element's own line
/// into `output` — regardless of whether it's a heading, a
/// paragraph, or a list item. An element's own
/// `ElementResult::text` (e.g. just the word `"kramfors"` for a
/// bare `[[kramfors]]` reference line) is rarely enough context
/// on its own.
///
/// Unlike `tags` / `ag` mode's `read_source_context_with_cache`
/// (which prefixes every line with a line number and marks the
/// match with `>>`), this passes a RAW, unmodified slice of the
/// file through `bat` — the same "clean markdown in, syntax-
/// highlighted markdown out" pipeline `notes::ensure_selected_context`
/// / `todo::ensure_selected_context` use. The line-number/`>>`
/// prefixing is appropriate for tags/ag's mixed-language source
/// files, but for markdown notes it fights `bat`'s own heading /
/// checkbox / link highlighting (the prefix isn't valid markdown,
/// so headings etc. no longer parse as such).
///
/// The slice is a window of `SOURCE_CONTEXT_LINES` (50) lines
/// CENTERED on the element's `start_line` (25 before, the line
/// itself, 24 after), clamped to the file's boundaries — same
/// centering math as `read_source_context_with_cache`, just
/// without the per-line annotation. For a file shorter than the
/// window this covers the entire file; for a longer file the
/// matched line is always included rather than requiring the
/// user to scroll down from the top to find it.
pub(crate) fn ensure_selected_context(app: &mut App) {
    if !matches(app) {
        return;
    }
    let Some(idx) = app.list_state.selected() else {
        return;
    };

    let (filepath, line_number) = match app.merged_rows.get(idx) {
        Some(r) if r.mode == "element" && !r.directory.is_empty() => {
            let line_number = r.session_id.parse::<usize>().unwrap_or(0);
            (r.directory.clone(), line_number)
        }
        _ => return,
    };
    if line_number == 0 {
        return;
    }

    let cache_key = (filepath.clone(), line_number);
    let highlighted = if let Some(cached) = app.elements_state.context_cache.get(&cache_key) {
        cached.clone()
    } else {
        let path = std::path::PathBuf::from(&filepath);
        if !app.tags_source_cache.contains_key(&path) {
            match std::fs::read_to_string(&path) {
                Ok(s) => {
                    app.tags_source_cache.insert(path.clone(), s);
                }
                Err(_) => return,
            }
        }
        let content = match app.tags_source_cache.get(&path) {
            Some(s) => s,
            None => return,
        };
        let lines: Vec<&str> = content.lines().collect();
        // `line_number` is 1-based; convert to a 0-based target index.
        let target = line_number.saturating_sub(1);
        if target >= lines.len() {
            return;
        }
        let half = crate::tui::SOURCE_CONTEXT_LINES / 2;
        let start = target.saturating_sub(half);
        let end = (target + half).min(lines.len());
        let window = lines[start..end].join("\n");
        if window.is_empty() {
            return;
        }

        let highlighted =
            crate::highlight::highlight_with_bat_auto(&window, &filepath).unwrap_or(window);
        app.elements_state
            .context_cache
            .insert(cache_key, highlighted.clone());
        highlighted
    };

    if let Some(row) = app.merged_rows.get_mut(idx)
        && row.output != highlighted
    {
        row.output = highlighted;
    }
}
