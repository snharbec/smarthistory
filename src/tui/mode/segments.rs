//! `:` (segment search) prefix mode.
//!
//! Finer-grained than `@` (notes), which searches whole files:
//! searches `note_search`'s `segments` table, where a "segment" is
//! one markdown header (level 1-4) plus everything below it up to
//! the next level-<=4 header — the header line itself is part of
//! the segment's text. Level 5/6 headers do NOT start a new
//! segment; content before the first header (or a file with none)
//! forms an implicit root segment. A tag or link search returns
//! the specific section that references it, not just "this file
//! mentions it somewhere."
//!
//! A segment's tags and links are the UNION of its own text (header
//! line + body), every ancestor header's own text, and the whole
//! document's aggregate tags/links — so every segment always
//! carries the full document's tags/links on top of whatever its
//! own text and ancestor headers add (a frontmatter tag/link
//! reaches every segment in the file). Since tags/links always
//! cascade the same way, they don't tell two matching segments
//! apart on their own — every segment also carries a `breadcrumb`
//! (filename + ancestor headers' text, own header not included) for
//! that, which is what actually distinguishes WHICH section a
//! search hit is in — see `map_segment_results`, which surfaces it
//! in `HistoryRow::comment`. See the upstream `note_search`
//! README's "Segment Search" section for the full semantics
//! (this cascade behavior has changed more than once upstream;
//! `fetch_segments_frontmatter_link_cascades_to_every_segment` in
//! the test suite locks in the CURRENT one).
//!
//! Same query language as `notes` / `todo` mode: the typed
//! pattern is parsed via `note_search::parse_query` into a
//! `QueryExpr` tree and passed as `criteria.query_expr`, so
//! `#tag`, `[[link]]`, `[attr:value]`, `(a OR b)`, and bare-word
//! AND-matching all work here too (`QueryBuilder::build_segment_query`
//! recurses the same expression tree `build_query_from_expr` /
//! `build_note_query_from_expr` use, just scoped to the
//! `segments` table). This wasn't always true — `note_search`'s
//! segment search originally only took separate `tags`/`links`/
//! `text` fields with no query DSL; upstream added `query_expr`
//! support for segments in a follow-up commit ("Support query
//! for segments").
//!
//! `#tag!` / `[[link]]!` / `[attr:value]!` / `[attr]!` (a
//! smarthistory-side extension, not part of `note_search`'s own DSL)
//! matches segments WITHOUT the given tag/link/attribute-value — see
//! `crate::tui::mode::query_negation` and `run_segments_search`.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

/// How long the segments-mode debounce waits after the last
/// keystroke before spawning the background search. Same value
/// as JIRA / ag / files mode (400 ms).
pub const SEGMENTS_DEBOUNCE: Duration = Duration::from_millis(400);

/// An in-flight segments search. The background thread sends the
/// result over `receiver`; the run loop polls it. `cancelled`
/// lets the run loop abort a stale search (e.g. the user pressed
/// `Cancel` or the query changed again before this one finished).
pub struct SegmentsRequest {
    pub receiver: mpsc::Receiver<Result<Vec<HistoryRow>, String>>,
    pub cancelled: Arc<AtomicBool>,
    /// The pattern that was being searched for, so the caller can
    /// tell whether this result is still relevant when it arrives
    /// (the user may have kept typing in the meantime).
    pub pattern: String,
}

/// Aggregated segments-mode async-search state. Held by the TUI
/// `App`, mirrors `AgState` / `FilesState` exactly: a query on
/// this mode's own `SearchCriteria`/`DatabaseService` is a
/// synchronous SQLite round-trip that can take long enough (an
/// unfiltered `:` on a large notes vault touches every indexed
/// segment) to make the very first keystroke after switching into
/// the mode feel like it's not registering.
/// Running it on a background thread, debounced the same way
/// `,` (ag) / `-` (JIRA) / `/` (files) mode already are, decouples
/// typing responsiveness from how long the search itself takes.
pub struct SegmentsState {
    /// Debounce timer, armed on every keystroke in segments mode.
    pub debounce_started: Option<std::time::Instant>,
    /// Last successfully searched pattern. Prevents re-querying
    /// when the pattern hasn't changed.
    pub last_pattern: Option<String>,
    /// Whether a search is currently in flight.
    pub in_flight: bool,
    /// In-flight request (background thread).
    pub request: Option<SegmentsRequest>,
    /// Cached results of the most recent search.
    pub rows: Vec<HistoryRow>,
    /// Bumped every time `rows` is replaced by a fresh search
    /// result. `segments::fetch()` ignores `App::query` entirely
    /// (it just clones `rows`), so `App::refresh()` uses this
    /// instead of the query text to detect whether there's
    /// actually anything new to re-clone into `merged_rows` — the
    /// query text changes on every keystroke, but `rows` only
    /// changes when a debounced search completes.
    pub rows_version: u64,
    /// Syntax-highlighted output preview, keyed by (absolute file
    /// path, 1-based start line). `App::refresh()` runs on every
    /// keystroke, which rebuilds `merged_rows` from scratch (from
    /// this struct's own `rows`, whose `output` is always the raw
    /// unhighlighted segment text) — without this cache,
    /// `ensure_selected_context` would re-highlight the same
    /// selected row on every single keystroke, which is exactly
    /// the kind of per-keystroke blocking work the background
    /// search thread was introduced to eliminate. The selected
    /// row's file/line rarely changes between keystrokes, so this
    /// cache turns that into a one-time cost per row.
    pub context_cache: std::collections::HashMap<(String, usize), String>,
}

impl SegmentsState {
    pub fn new() -> Self {
        SegmentsState {
            debounce_started: None,
            last_pattern: None,
            in_flight: false,
            request: None,
            rows: Vec::new(),
            rows_version: 0,
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

impl Default for SegmentsState {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::debounce::Cancellable for SegmentsRequest {
    fn cancelled_flag(&self) -> &Arc<AtomicBool> {
        &self.cancelled
    }
}

impl crate::debounce::Debounced for SegmentsState {
    type Request = SegmentsRequest;
    fn debounce_started(&mut self) -> &mut Option<std::time::Instant> {
        &mut self.debounce_started
    }
    fn last_pattern(&mut self) -> &mut Option<String> {
        &mut self.last_pattern
    }
    fn in_flight(&mut self) -> &mut bool {
        &mut self.in_flight
    }
    fn request(&mut self) -> &mut Option<SegmentsRequest> {
        &mut self.request
    }
}

/// Spawn a background thread that runs the `note_search` segment
/// query and sends the mapped `HistoryRow`s (or an error message)
/// back over the channel. Mirrors `crate::ag::spawn_ag_search`.
pub fn spawn_segments_search(
    db_path: std::path::PathBuf,
    notes_dir: Option<std::path::PathBuf>,
    pattern: String,
    min_words: usize,
) -> SegmentsRequest {
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let pattern_for_thread = pattern.clone();

    std::thread::spawn(move || {
        let result = run_segments_search(
            &db_path,
            notes_dir.as_deref(),
            &pattern_for_thread,
            min_words,
        );
        if !cancelled_clone.load(Ordering::Relaxed) {
            let _ = tx.send(result);
        }
    });

    SegmentsRequest {
        receiver: rx,
        cancelled,
        pattern,
    }
}

/// The actual (synchronous, but run on a background thread)
/// query + row-mapping. Factored out of `spawn_segments_search`
/// so it has no channel/thread concerns of its own — just "given
/// a database and a pattern, return rows or an error message".
/// `min_words` is the `segments.minwords` threshold: a segment
/// whose body (its text minus its own header line) has this many
/// words or fewer is dropped as noise before mapping — `0` disables
/// the filter. See `segment_body_word_count`.
fn run_segments_search(
    db_path: &std::path::Path,
    notes_dir: Option<&std::path::Path>,
    pattern: &str,
    min_words: usize,
) -> Result<Vec<HistoryRow>, String> {
    // `#tag!` / `[[link]]!` negation tokens (and the `!!type`/`!type`
    // shorthand) are stripped BEFORE `parse_query` sees the pattern —
    // it has no negation primitive and would either error on the
    // trailing `!` or silently fold it into a bare-word token. See
    // `crate::tui::mode::query_negation`'s module doc comment.
    let (pattern, negations, type_restrictions) =
        crate::tui::mode::query_negation::split_negations(pattern);
    let pattern = pattern.as_str();
    let mut query_expr = if pattern.is_empty() {
        None
    } else {
        Some(note_search::parse_query(pattern).map_err(|e| format!("invalid query: {}", e))?)
    };
    // `!type` restricts results to ONLY the given type(s) — `note_search`
    // has a native `Or`, so "type is jira OR meeting" is one query
    // clause, ANDed with whatever else the user typed, not an extra
    // round-trip the way exclusion needs.
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

    let search_start = std::time::Instant::now();
    let service = note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());
    let mut results = service
        .search_segments(&criteria)
        .map_err(|e| format!("search failed: {}", e))?;
    let search_elapsed = search_start.elapsed();

    let negation_start = std::time::Instant::now();
    if !negations.is_empty() {
        let excluded = excluded_segment_identities(&service, db_path, &negations)?;
        results.retain(|r| !excluded.contains(&(r.filename.clone(), r.start_line)));
    }
    let negation_elapsed = negation_start.elapsed();

    if min_words > 0 {
        results.retain(|r| segment_body_word_count(&r.text, r.heading_level) > min_words);
    }

    let mapped = map_segment_results(&results, notes_dir);
    // Runs on the background thread, so it never blocks the UI
    // directly — logged anyway so a slow *search* (vs. a slow main
    // thread) can be told apart when investigating a reported
    // responsiveness stall.
    if search_elapsed.as_millis() >= crate::tui::PERF_LOG_THRESHOLD_MS
        || negation_elapsed.as_millis() >= crate::tui::PERF_LOG_THRESHOLD_MS
    {
        crate::tui::perf_debug_log(&format!(
            "run_segments_search: search={}ms negations={}ms ({} terms) results={} pattern={:?}",
            search_elapsed.as_millis(),
            negation_elapsed.as_millis(),
            negations.len(),
            mapped.len(),
            pattern,
        ));
    }

    Ok(mapped)
}

/// Word count of a segment's body, excluding its own header line.
/// `note_search`'s `SegmentResult::text` includes the header line
/// verbatim as its first line when `heading_level` is `Some` (see
/// `map_segment_results`'s doc comment) — that line is stripped
/// before counting so a heading's own words don't count toward the
/// `segments.minwords` threshold. A segment with no heading
/// (`heading_level == None`, e.g. the implicit root segment before
/// a file's first header) has no header line to strip, so the whole
/// text counts. "Word" is a plain whitespace-separated token — no
/// markdown-aware parsing (list markers, emphasis characters, etc.
/// each count as part of a word like anything else).
///
/// `pub(crate)` (not just `run_segments_search`'s own private
/// helper) because `crate::tui::mode::similar` applies the same
/// `segments.minwords` threshold to its own `SegmentResult` rows —
/// same underlying `segments` table, same filter, one shared
/// definition of "word count" between the two modes.
pub(crate) fn segment_body_word_count(text: &str, heading_level: Option<i32>) -> usize {
    let body = if heading_level.is_some() {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        text
    };
    body.split_whitespace().count()
}

/// For each negated term (`#tag!` / `[[link]]!`), run the ordinary
/// POSITIVE query and collect the (filename, start_line) identity of
/// every segment that DOES have it — the set `run_segments_search`
/// excludes from its own results. One extra local-SQLite round-trip
/// per negated term; there's no single-query way to express "AND
/// NOT" since `note_search` has no negation `QueryExpr`.
fn excluded_segment_identities(
    service: &note_search::database_service::DatabaseService,
    db_path: &std::path::Path,
    negations: &[crate::tui::mode::query_negation::NegatedTerm],
) -> Result<std::collections::HashSet<(String, i32)>, String> {
    let mut excluded = std::collections::HashSet::new();
    for term in negations {
        let criteria = note_search::SearchCriteria {
            database_path: db_path.to_string_lossy().to_string(),
            query_expr: Some(term.positive_query_expr()),
            list_only: true,
            ..Default::default()
        };
        let rows = service
            .search_segments(&criteria)
            .map_err(|e| format!("negation lookup for {:?} failed: {}", term, e))?;
        excluded.extend(rows.into_iter().map(|r| (r.filename, r.start_line)));
    }
    Ok(excluded)
}

/// Map `note_search`'s `SegmentResult` rows into `HistoryRow`s.
fn map_segment_results(
    results: &[note_search::database_service::SegmentResult],
    notes_dir: Option<&std::path::Path>,
) -> Vec<HistoryRow> {
    results
        .iter()
        .map(|el| {
            // Unlike the old element search (which indexed bare
            // paragraphs/list-items with no heading markup of
            // their own), a segment's `text` ALREADY includes its
            // own header line verbatim when `heading_level` is
            // set — note_search's segment boundary IS the header,
            // so there's nothing to prepend here. Internal
            // newlines (the header line + its body) are joined
            // with " / " for a scannable single line — same
            // convention `note_search`'s own default CLI output
            // uses (see `SegmentResult::formatted_string`
            // upstream).
            let display_text = el.text.replace('\n', " / ");
            let full_path = notes_dir
                .map(|d| d.join(&el.filename).display().to_string())
                .unwrap_or_default();
            HistoryRow {
                // Synthetic negative id, same convention as
                // todo mode's `id = -(line_number)`. Not
                // globally unique across files (two files can
                // both have a segment starting on the same line)
                // — `App`'s `marked_ids` already handles that
                // generically by keying on `(id, comment)`, and
                // `directory` + `session_id` (not `id`) are what
                // staging actually uses to open the right file.
                id: -(el.start_line as i64),
                command: display_text,
                // `directory` / `session_id` carry the
                // absolute file path / line number — the
                // same convention `tags` / `ag` / `codegraph`
                // use for `stage_editor_open_at_line`.
                directory: full_path,
                session_id: el.start_line.to_string(),
                exit_code: 0,
                timestamp: el.updated.unwrap_or(0),
                // `breadcrumb` (filename + ancestor headers'
                // text, own header not included — see the module
                // doc comment) rather than the bare filename: a
                // segment's tags/links come only from its own
                // text (no cascade from ancestor headers), so the
                // breadcrumb is what tells the user which section
                // of the file this actually is.
                //
                // `ensure_selected_context` unconditionally
                // replaces `output` with a window of the full
                // underlying file once the row is actually
                // selected, so `output` here is only what's
                // briefly visible before that runs (or the
                // fallback if the file can't be read).
                comment: el.breadcrumb.clone(),
                output: el.text.clone(),
                mode: "segment".to_string(),
                source: String::new(),
                ..Default::default()
            }
        })
        .collect()
}

/// True if the current query is a segment search request
/// (prefixed with the configured segments prefix, default `:`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.segments;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The segment search body, i.e. everything after the leading
/// segments prefix.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.segments;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Health check for the segments (`:`) mode. Mirrors
/// `notes::check` step-for-step (same `notes.database`, same
/// connection), but probes for the `segments` table instead of
/// `todo_entries` — a notes database indexed by a
/// `note_search` version older than the "search for segments"
/// feature won't have it yet, which is exactly the failure mode
/// this check exists to surface clearly (rather than a cryptic
/// "no such table: segments" SQL error at search time).
pub(crate) fn check(app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Segments;

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

    // `segments` is the new table this whole mode depends on —
    // a database indexed by an older `note_search` build won't
    // have it. `markdown_data` is checked too since
    // `search_segments` joins against it for the `updated`
    // timestamp.
    let required_tables = ["markdown_data", "segments"];
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
                        "required table `{table}` is missing (re-run `note_search import` with a note_search build that supports segment search, then re-index)"
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
    let rows = match service.search_segments(&criteria) {
        Ok(r) => r,
        Err(e) => {
            return CheckReport::err(
                mode,
                format!("search_segments() failed on an empty query: {e}"),
            );
        }
    };

    if rows.is_empty() {
        return CheckReport::warn(
            mode,
            "notes database is reachable but contains 0 indexed segments (re-index with a note_search build that supports segment search)".to_string(),
        )
        .with(CheckReport::ok(
            mode,
            format!("opened {}", db_path.display()),
        ));
    }

    CheckReport::ok(
        mode,
        format!("{} segments indexed in {}", rows.len(), db_path.display()),
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
        format!("sample search_segments() returned {} row(s)", rows.len()),
    ))
}

/// Fetch the segments-mode result set. The actual query runs on
/// a background thread (spawned by `App::segments_touch` →
/// `spawn_segments_search`, debounced by `App::segments_maybe_autocall`),
/// so this just clones the cached rows from `App::segments_state`
/// — mirrors `crate::tui::mode::ag::fetch` exactly. Decoupling
/// the query from this synchronous `fetch()` call is the whole
/// point: `fetch()` runs on every keystroke (via `App::refresh`),
/// and an unfiltered `:` on a large notes vault touches every
/// indexed segment (every header-bounded section of every file)
/// — synchronously blocking on that from the main thread was
/// making the first keystroke after switching into the mode feel
/// unresponsive.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    Ok(app.segments_state.rows.clone())
}

/// Lazy-load context around the SELECTED segment's own start
/// line into `output`. A segment's own `SegmentResult::text` can
/// be a whole section (its header plus everything below it up to
/// the next header), but for a short segment near the top or
/// bottom of a long file, seeing just that section in isolation
/// still loses the surrounding context the user would get from
/// scrolling the real file — so this loads a window of the actual
/// file around the segment's start line instead of just
/// re-displaying `text` verbatim.
///
/// Unlike `tags` / `ag` mode's `read_source_context_with_cache`
/// (which prefixes every line with a line number and marks the
/// match with `>>`), this passes a RAW, unmodified slice of the
/// file through `highlight_with_bat_auto` — the same "clean
/// markdown in, syntax-highlighted markdown out" pipeline
/// `notes::ensure_selected_context` / `todo::ensure_selected_context`
/// use. The line-number/`>>` prefixing is appropriate for tags/ag's
/// mixed-language source files, but for markdown notes it fights
/// the highlighter's own heading / checkbox / link highlighting
/// (the prefix isn't valid markdown, so headings etc. no longer
/// parse as such).
///
/// The slice is a window of `SOURCE_CONTEXT_LINES` (50) lines
/// CENTERED on the segment's `start_line` (25 before, the line
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
        Some(r) if r.mode == "segment" && !r.directory.is_empty() => {
            let line_number = r.session_id.parse::<usize>().unwrap_or(0);
            (r.directory.clone(), line_number)
        }
        _ => return,
    };
    if line_number == 0 {
        return;
    }

    let cache_key = (filepath.clone(), line_number);
    let highlighted = if let Some(cached) = app.segments_state.context_cache.get(&cache_key) {
        cached.clone()
    } else {
        let miss_start = std::time::Instant::now();
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
        // Runs synchronously on the main thread (unlike the search
        // itself) — a slow highlight here is a direct candidate for
        // a reported "selecting a result is slow" stall.
        let miss_elapsed = miss_start.elapsed();
        if miss_elapsed.as_millis() >= crate::tui::PERF_LOG_THRESHOLD_MS {
            crate::tui::perf_debug_log(&format!(
                "segments ensure_selected_context (cache miss): {}ms file={:?} line={}",
                miss_elapsed.as_millis(),
                filepath,
                line_number,
            ));
        }
        app.segments_state
            .context_cache
            .insert(cache_key, highlighted.clone());
        highlighted
    };

    if let Some(row) = app.merged_rows.get_mut(idx)
        && row.output != highlighted
    {
        row.output = highlighted;
        // The output preview
        // renderer scrolls
        // `Paragraph` by this
        // offset so the
        // segment line is
        // visible. The
        // windowed source
        // context above is
        // 50 lines centered
        // on `target` (the
        // segment line is
        // the
        // `SOURCE_CONTEXT_LINES / 2`-th
        // line of the
        // window). For a
        // typical preview
        // pane (~10–20
        // lines tall),
        // scrolling to
        // `half - visible_height / 2`
        // would put the
        // segment line
        // near the top of
        // the visible
        // area. The
        // renderer's
        // `min(max_scroll)`
        // clamp handles
        // the case where
        // the file has
        // fewer than 50
        // lines or the
        // segment is
        // near the end
        // of the file
        // (so the window
        // is shorter than
        // 50 and the
        // `half` offset
        // would overshoot).
        let half = crate::tui::SOURCE_CONTEXT_LINES / 2;
        // The renderer's
        // visible height is
        // `area.height - 2`
        // (top + bottom
        // border). The
        // `2` is the same
        // value the
        // renderer uses.
        row.preview_scroll = half.saturating_sub(2) as u16;
    }
}

impl App {
    /// Whether the query is a segment-search request: the
    /// query starts with the segments prefix (`:` by default).
    /// Finer-grained than `is_notes_query` — searches header-
    /// bounded sections (see this module's doc comment) rather
    /// than whole files.
    pub(crate) fn is_segments_query(&self) -> bool {
        matches(self)
    }

    /// Arm the segments-mode debounce. Mirrors `ag_touch`.
    pub(crate) fn segments_touch(&mut self) {
        let active = self.is_segments_query();
        crate::debounce::touch(&mut self.segments_state, active);
    }

    /// Check whether the segments-mode debounce has elapsed and
    /// spawn a background search if so. Mirrors `ag_maybe_autocall`.
    pub(crate) fn segments_maybe_autocall(&mut self) {
        if !self.is_segments_query() {
            return;
        }
        if !crate::debounce::debounce_elapsed(&mut self.segments_state, SEGMENTS_DEBOUNCE) {
            return;
        }
        let Some(ref db_path) = self.notes_database else {
            self.set_status_message("Segments mode: notes.database is not configured".to_string());
            self.segments_state.debounce_started = None;
            return;
        };
        let pattern = SegmentsState::current_pattern(&self.query, self.query_prefixes.segments);
        if self.segments_state.has_results_for(&pattern) {
            return;
        }
        self.segments_state.last_pattern = Some(pattern.clone());
        let db_path = db_path.clone();
        let notes_dir = self.notes_dir.clone();
        self.spawn_segments_search(db_path, notes_dir, pattern, self.segments_min_words);
    }

    /// Spawn a background thread that runs the segments query.
    /// Mirrors `spawn_ag_search`.
    pub(crate) fn spawn_segments_search(
        &mut self,
        db_path: std::path::PathBuf,
        notes_dir: Option<std::path::PathBuf>,
        pattern: String,
        min_words: usize,
    ) {
        let request = spawn_segments_search(db_path, notes_dir, pattern, min_words);
        self.segments_state.in_flight = true;
        self.segments_state.request = Some(request);
    }

    /// Process an segments-mode search result from the
    /// background thread. Unlike `process_ag_result`, the
    /// channel carries a `Result` — an invalid query (unbalanced
    /// parens, etc.) or a search failure surfaces as a status
    /// message, same UX the old synchronous `fetch()` had.
    pub(crate) fn process_segments_result(
        &mut self,
        request: SegmentsRequest,
        result: Result<Vec<HistoryRow>, String>,
    ) {
        self.segments_state.in_flight = false;
        self.segments_state.request = None;
        let current = SegmentsState::current_pattern(&self.query, self.query_prefixes.segments);
        if current != request.pattern {
            // Stale result — the user has typed something else
            // since this search was fired. Discard silently; a
            // fresh search for the current pattern is already
            // debounced/in flight.
            return;
        }
        match result {
            Ok(rows) => {
                self.segments_state.rows = rows;
                self.segments_state.rows_version = self.segments_state.rows_version.wrapping_add(1);
                self.refresh();
            }
            Err(e) => {
                self.set_status_message(format!("Segments mode: {}", e));
            }
        }
    }
}
