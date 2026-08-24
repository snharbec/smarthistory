//! `"` (similar / phrase search) prefix mode.
//!
//! Same architecture as `:` (segments) mode — debounced background-
//! thread search over `note_search`'s `segments` table, same
//! `notes.database` tag/link Tab-completion namespace, same windowed
//! syntax-highlighted preview keyed on the matched segment's own
//! `start_line` — but the entire typed body (everything after the
//! prefix) is treated as ONE phrase rather than parsed as a query DSL:
//! there's no `#tag` / `[[link]]` / `(a OR b)` support here, the whole
//! string (including any literal `#`/`[[`/`(` characters the user
//! types or Tab-completes in) is embedded verbatim.
//!
//! The one exception is the shared `#tag!` / `[[link]]!` /
//! `[attr:value]!` / `[attr]!` negation syntax (see
//! `crate::tui::mode::query_negation`): unlike the rest of the query
//! DSL, negated terms ARE recognised here — they're stripped from the
//! phrase before embedding (so they don't pollute the semantic
//! content being ranked) and applied as a post-filter over the
//! similarity-ranked results, the same "run an extra positive lookup
//! query and exclude its identities" mechanism `:` mode uses (see
//! `excluded_similar_identities`).
//!
//! The phrase is embedded with `note_search::embeddings::embed_text`
//! (a synchronous call to a local Ollama instance running the same
//! `nomic-embed-text` model `note_search import` uses to compute each
//! segment's stored embedding at index time) and matched against every
//! segment that already has one via
//! `DatabaseService::search_similar_segments` (cosine similarity),
//! ranked highest-first. Results carry their similarity score
//! (`[0.87]`-style prefix on `command`) since — unlike `:` mode's exact
//! tag/link/text filters — every non-empty phrase always returns SOME
//! ranked list, so the score is the only signal for how relevant a
//! given result actually is.
//!
//! Requires a `note_search` build with segment-embeddings support AND
//! a notes database that was imported/re-imported with that build
//! (the `segments.embedding` column doesn't get added retroactively to
//! an existing `segments` table) AND a reachable local Ollama instance
//! with `nomic-embed-text` pulled — `smarthistory check --prefix "`
//! reports which of these (if any) is missing.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

/// Same value as `:` (segments) / `,` (ag) / JIRA mode's debounce.
pub const SIMILAR_DEBOUNCE: Duration = Duration::from_millis(400);

/// note_search's own CLI (`note_search similar`) defaults to 10; an
/// interactive list can comfortably show a bit more without the
/// long-tail (low-similarity, rarely useful) results dominating.
const SIMILAR_RESULT_LIMIT: usize = 25;

/// An in-flight similar-phrase search. Mirrors `SegmentsRequest`.
pub struct SimilarRequest {
    pub receiver: mpsc::Receiver<Result<Vec<HistoryRow>, String>>,
    pub cancelled: Arc<AtomicBool>,
    /// The phrase that was being searched for, so the caller can tell
    /// whether this result is still relevant when it arrives (the
    /// user may have kept typing in the meantime).
    pub pattern: String,
}

/// Aggregated similar-mode async-search state. Mirrors `SegmentsState`
/// exactly (same debounce / cache / has-results-for contract) — the
/// embedding HTTP round-trip plus the similarity query can easily take
/// longer than a plain SQL query, which makes running it off the main
/// thread even more important here than for `:` mode.
pub struct SimilarState {
    pub debounce_started: Option<std::time::Instant>,
    pub last_pattern: Option<String>,
    pub in_flight: bool,
    pub request: Option<SimilarRequest>,
    pub rows: Vec<HistoryRow>,
    /// Bumped every time `rows` is replaced by a fresh search
    /// result. See `SegmentsState::rows_version` — same rationale.
    pub rows_version: u64,
    /// Syntax-highlighted output preview, keyed by (absolute file
    /// path, 1-based start line) — same rationale as
    /// `SegmentsState::context_cache`.
    pub context_cache: std::collections::HashMap<(String, usize), String>,
}

impl SimilarState {
    pub fn new() -> Self {
        SimilarState {
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
    /// `SegmentsState::current_pattern`.
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

impl Default for SimilarState {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::debounce::Cancellable for SimilarRequest {
    fn cancelled_flag(&self) -> &Arc<AtomicBool> {
        &self.cancelled
    }
}

impl crate::debounce::Debounced for SimilarState {
    type Request = SimilarRequest;
    fn debounce_started(&mut self) -> &mut Option<std::time::Instant> {
        &mut self.debounce_started
    }
    fn last_pattern(&mut self) -> &mut Option<String> {
        &mut self.last_pattern
    }
    fn in_flight(&mut self) -> &mut bool {
        &mut self.in_flight
    }
    fn request(&mut self) -> &mut Option<SimilarRequest> {
        &mut self.request
    }
}

/// Spawn a background thread that embeds `pattern` and runs the
/// `note_search` similarity query, sending the mapped `HistoryRow`s
/// (or an error message) back over the channel. Mirrors
/// `crate::tui::mode::segments::spawn_segments_search`.
pub fn spawn_similar_search(
    db_path: std::path::PathBuf,
    notes_dir: Option<std::path::PathBuf>,
    pattern: String,
    min_words: usize,
) -> SimilarRequest {
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let pattern_for_thread = pattern.clone();

    std::thread::spawn(move || {
        let result = run_similar_search(
            &db_path,
            notes_dir.as_deref(),
            &pattern_for_thread,
            min_words,
        );
        if !cancelled_clone.load(Ordering::Relaxed) {
            let _ = tx.send(result);
        }
    });

    SimilarRequest {
        receiver: rx,
        cancelled,
        pattern,
    }
}

/// The actual (synchronous, but run on a background thread) embed +
/// query + row-mapping. Factored out of `spawn_similar_search` so it
/// has no channel/thread concerns of its own. `min_words` is the
/// same `segments.minwords` threshold `run_segments_search` applies
/// — see `segment_body_word_count`.
fn run_similar_search(
    db_path: &std::path::Path,
    notes_dir: Option<&std::path::Path>,
    phrase: &str,
    min_words: usize,
) -> Result<Vec<HistoryRow>, String> {
    // `#tag!` / `[[link]]!` / `[attr:value]!` / `[attr]!` negation
    // tokens are stripped BEFORE the phrase is embedded — they're a
    // structural exclusion filter, not semantic content to rank
    // against. See this module's doc comment and
    // `crate::tui::mode::query_negation`.
    let (phrase, negations, type_restrictions) =
        crate::tui::mode::query_negation::split_negations(phrase);
    let phrase = phrase.as_str();
    // An empty remaining phrase has nothing to embed or compare
    // against — unlike `:` mode's bare-prefix "list everything",
    // similarity search is meaningless without a phrase. No-op
    // rather than an error. This also covers a query that's ONLY
    // negation/restriction tokens (e.g. `[type:jira]!` or `!jira`
    // alone) — there's no "rank everything, then filter" baseline
    // for similarity search the way there is for `:` mode's bare `:`.
    if phrase.trim().is_empty() {
        return Ok(Vec::new());
    }
    let embedding = note_search::embeddings::embed_text(phrase)
        .map_err(|e| format!("embedding failed: {e}"))?;
    let service = note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());
    let mut results = service
        .search_similar_segments(&embedding, &[], &[], SIMILAR_RESULT_LIMIT)
        .map_err(|e| format!("search failed: {e}"))?;

    if !negations.is_empty() {
        let excluded = excluded_similar_identities(&service, db_path, &negations)?;
        results.retain(|(el, _score)| !excluded.contains(&(el.filename.clone(), el.start_line)));
    }

    // `!type` restricts results to ONLY the given type(s).
    // `search_similar_segments` has no `QueryExpr` hook at all (only a
    // plain embedding vector + tag/link string filters) and
    // `SegmentResult` carries no `type` field to post-filter on
    // directly, so — same as exclusion above — this needs a separate
    // lookup. Unlike exclusion's one-query-per-term loop, this is ONE
    // query covering every restricted type via `Or`.
    if !type_restrictions.is_empty() {
        let included = included_similar_identities(&service, db_path, &type_restrictions)?;
        results.retain(|(el, _score)| included.contains(&(el.filename.clone(), el.start_line)));
    }

    if min_words > 0 {
        results.retain(|(el, _score)| {
            crate::tui::mode::segments::segment_body_word_count(&el.text, el.heading_level)
                > min_words
        });
    }

    Ok(map_similar_results(&results, notes_dir))
}

/// For each negated term (`#tag!` / `[[link]]!` / `[attr:value]!` /
/// `[attr]!`), run the ordinary POSITIVE query and collect the
/// (filename, start_line) identity of every segment that DOES match
/// it — the set `run_similar_search` excludes from its own
/// similarity-ranked results. Mirrors
/// `crate::tui::mode::segments::excluded_segment_identities` exactly
/// (same `SegmentResult` identity, same one-lookup-per-term
/// mechanism) — duplicated rather than shared since each mode module
/// is deliberately self-contained (see `src/tui/mode/mod.rs`'s doc
/// comment on why these are free functions, not a shared trait).
fn excluded_similar_identities(
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

/// The inclusion counterpart to [`excluded_similar_identities`]: one
/// combined lookup (not one per value — `note_search::QueryExpr::Or`
/// expresses "type is any of these" natively) returning the
/// `(filename, start_line)` identity of every segment whose `type`
/// attribute matches ANY of `type_restrictions` — the set
/// `run_similar_search` retains from its own similarity-ranked
/// results, discarding everything else.
fn included_similar_identities(
    service: &note_search::database_service::DatabaseService,
    db_path: &std::path::Path,
    type_restrictions: &[String],
) -> Result<std::collections::HashSet<(String, i32)>, String> {
    let restriction = note_search::QueryExpr::Or(
        type_restrictions
            .iter()
            .map(|v| note_search::QueryExpr::Attribute {
                key: "type".to_string(),
                value: Some(v.clone()),
            })
            .collect(),
    );
    let criteria = note_search::SearchCriteria {
        database_path: db_path.to_string_lossy().to_string(),
        query_expr: Some(restriction),
        list_only: true,
        ..Default::default()
    };
    let rows = service
        .search_segments(&criteria)
        .map_err(|e| format!("type restriction lookup failed: {}", e))?;
    Ok(rows.into_iter().map(|r| (r.filename, r.start_line)).collect())
}

/// Map `note_search`'s `(SegmentResult, similarity_score)` pairs into
/// `HistoryRow`s. Mirrors `crate::tui::mode::segments::map_segment_results`,
/// plus a `[score]` prefix on `command` — the only signal for how
/// relevant a given result actually is, since (unlike `:` mode) every
/// non-empty phrase always returns SOME ranked list.
fn map_similar_results(
    results: &[(note_search::database_service::SegmentResult, f32)],
    notes_dir: Option<&std::path::Path>,
) -> Vec<HistoryRow> {
    results
        .iter()
        .map(|(el, score)| {
            let display_text = el.text.replace('\n', " / ");
            let full_path = notes_dir
                .map(|d| d.join(&el.filename).display().to_string())
                .unwrap_or_default();
            HistoryRow {
                // Synthetic negative id — see the matching comment in
                // `map_segment_results` for why this doesn't need to
                // be globally unique.
                id: -(el.start_line as i64),
                command: format!("[{score:.2}] {display_text}"),
                directory: full_path,
                session_id: el.start_line.to_string(),
                exit_code: 0,
                timestamp: el.updated.unwrap_or(0),
                // Breadcrumb, same as segments mode — see
                // `map_segment_results` for the rationale.
                comment: el.breadcrumb.clone(),
                output: el.text.clone(),
                mode: "similar".to_string(),
                source: String::new(),
                ..Default::default()
            }
        })
        .collect()
}

/// True if the current query is a similar-phrase search request
/// (prefixed with the configured similar prefix, default `"`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.similar;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The similar-phrase search body, i.e. everything after the leading
/// similar prefix — the literal phrase to embed, not a query DSL.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.similar;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Health check for the similar (`"`) mode. Checks everything
/// `:` (segments) mode's check does (the search reads the same
/// `segments` table), plus the two things unique to similarity
/// search: the `embedding` column actually exists on `segments` (a
/// database indexed before segment-embeddings support won't have it
/// — `note_search`'s schema-init only adds new COLUMNS to a table it
/// is also CREATING, so an existing `segments` table needs a fresh
/// `note_search import` to gain it) and a local Ollama instance is
/// reachable with the embedding model loaded (same reachability check
/// `=` (LLM) mode's `check()` does for its own model, just against
/// `note_search::embeddings`'s own `OLLAMA_HOST`-driven default rather
/// than smarthistory's `ollama.url` config — the two are independent).
pub(crate) fn check(app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Similar;

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

    let segments_table_present: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='segments'",
        [],
        |row| row.get(0),
    );
    match segments_table_present {
        Ok(n) if n > 0 => {}
        Ok(_) => {
            return CheckReport::err(
                mode,
                "required table `segments` is missing (re-run `note_search import` with a note_search build that supports segment search, then re-index)",
            );
        }
        Err(e) => {
            return CheckReport::err(mode, format!("failed to probe for table `segments`: {e}"));
        }
    }

    // `embedding` is a column added to `segments`'s `CREATE TABLE`
    // itself (not a separate migration), so an EXISTING `segments`
    // table from before embeddings support won't have it — probe
    // for the column explicitly rather than letting the similarity
    // query fail with a cryptic "no such column: e.embedding".
    let has_embedding_column = match conn.prepare("PRAGMA table_info(segments)") {
        Ok(mut stmt) => {
            let names = stmt.query_map([], |row| row.get::<_, String>(1));
            match names {
                Ok(rows) => rows.filter_map(|r| r.ok()).any(|name| name == "embedding"),
                Err(e) => {
                    return CheckReport::err(mode, format!("failed to inspect `segments` schema: {e}"));
                }
            }
        }
        Err(e) => {
            return CheckReport::err(mode, format!("failed to inspect `segments` schema: {e}"));
        }
    };
    if !has_embedding_column {
        return CheckReport::err(
            mode,
            "the `segments` table has no `embedding` column (it was indexed by a note_search build \
             from before segment-embeddings support; re-run `note_search import` with a build that \
             has it to add embeddings)",
        );
    }

    let embedded_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM segments WHERE embedding IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if embedded_count == 0 {
        return CheckReport::warn(
            mode,
            "the `segments` table exists but no segment has a stored embedding yet \
             (re-run `note_search import` — each segment is embedded via a local Ollama \
             call at import time, so this can also mean Ollama was unreachable during \
             the last import)".to_string(),
        );
    }

    // Reachability + model check for the embedding call itself.
    // `note_search::embeddings::embed_text` reads `OLLAMA_HOST`
    // (default `http://localhost:11434`) independently of
    // smarthistory's own `ollama.url` config (used by `=` mode) —
    // the two are unrelated settings that happen to both point at
    // Ollama.
    let ollama_host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
    const EMBEDDING_MODEL: &str = "nomic-embed-text";
    let tags_url = format!("{}/api/tags", ollama_host.trim_end_matches('/'));
    let client = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(5))
        .build();
    let tags_resp = client.get(&tags_url).call();
    let (status, body) = match tags_resp {
        Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, r)) => (code, r.into_string().unwrap_or_default()),
        Err(ureq::Error::Transport(t)) => {
            return CheckReport::err(mode, format!("could not reach ollama at {ollama_host}: {t}"));
        }
    };
    if !(200..300).contains(&status) {
        return CheckReport::err(
            mode,
            format!("ollama at {ollama_host} returned HTTP {status}: {}", body.trim()),
        );
    }
    let model_in_list = body.contains(&format!("\"name\":\"{EMBEDDING_MODEL}\""))
        || body.contains(&format!("\"name\": \"{EMBEDDING_MODEL}\""))
        || body.contains(EMBEDDING_MODEL);
    if !model_in_list {
        return CheckReport::err(
            mode,
            format!(
                "ollama is reachable at {ollama_host} but the embedding model `{EMBEDDING_MODEL}` \
                 is not loaded (run `ollama pull {EMBEDDING_MODEL}` to fetch it)"
            ),
        );
    }

    CheckReport::ok(
        mode,
        format!(
            "{embedded_count} segments have stored embeddings in {}",
            db_path.display()
        ),
    )
    .with(CheckReport::ok(mode, format!("opened {}", db_path.display())))
    .with(CheckReport::ok(
        mode,
        format!("ollama at {ollama_host} reachable, model `{EMBEDDING_MODEL}` is loaded"),
    ))
}

/// Fetch the similar-mode result set. Mirrors
/// `crate::tui::mode::segments::fetch` — the actual embed + query runs
/// on a background thread (spawned by `App::similar_touch` →
/// `spawn_similar_search`, debounced by `App::similar_maybe_autocall`),
/// so this just clones the cached rows from `App::similar_state`.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    Ok(app.similar_state.rows.clone())
}

/// Lazy-load context around the SELECTED result's own segment start
/// line into `output`. Identical to
/// `crate::tui::mode::segments::ensure_selected_context` (same
/// windowed, raw-markdown-through-syntax-highlighting preview, same
/// per-(file,line) cache) — the results here are `SegmentResult`s
/// too, just ranked by similarity instead of matched by query DSL.
pub(crate) fn ensure_selected_context(app: &mut App) {
    if !matches(app) {
        return;
    }
    let Some(idx) = app.list_state.selected() else {
        return;
    };

    let (filepath, line_number) = match app.merged_rows.get(idx) {
        Some(r) if r.mode == "similar" && !r.directory.is_empty() => {
            let line_number = r.session_id.parse::<usize>().unwrap_or(0);
            (r.directory.clone(), line_number)
        }
        _ => return,
    };
    if line_number == 0 {
        return;
    }

    let cache_key = (filepath.clone(), line_number);
    let highlighted = if let Some(cached) = app.similar_state.context_cache.get(&cache_key) {
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
        app.similar_state
            .context_cache
            .insert(cache_key, highlighted.clone());
        highlighted
    };

    if let Some(row) = app.merged_rows.get_mut(idx)
        && row.output != highlighted
    {
        row.output = highlighted;
        let half = crate::tui::SOURCE_CONTEXT_LINES / 2;
        row.preview_scroll = half.saturating_sub(2) as u16;
    }
}

impl App {
    /// Whether the query is a similar-phrase search request: the
    /// query starts with the similar prefix (`"` by default). Same
    /// underlying `segments` table as `is_segments_query`, but the
    /// body is one literal phrase (embedded + ranked by similarity)
    /// rather than a query DSL — see this module's doc comment.
    pub(crate) fn is_similar_query(&self) -> bool {
        matches(self)
    }

    /// Arm the similar-mode debounce. Mirrors `segments_touch`.
    pub(crate) fn similar_touch(&mut self) {
        let active = self.is_similar_query();
        crate::debounce::touch(&mut self.similar_state, active);
    }

    /// Check whether the similar-mode debounce has elapsed and
    /// spawn a background embed+search if so. Mirrors
    /// `segments_maybe_autocall`.
    pub(crate) fn similar_maybe_autocall(&mut self) {
        if !self.is_similar_query() {
            return;
        }
        if !crate::debounce::debounce_elapsed(&mut self.similar_state, SIMILAR_DEBOUNCE) {
            return;
        }
        let Some(ref db_path) = self.notes_database else {
            self.set_status_message("Similar mode: notes.database is not configured".to_string());
            self.similar_state.debounce_started = None;
            return;
        };
        let pattern = SimilarState::current_pattern(&self.query, self.query_prefixes.similar);
        if self.similar_state.has_results_for(&pattern) {
            return;
        }
        self.similar_state.last_pattern = Some(pattern.clone());
        let db_path = db_path.clone();
        let notes_dir = self.notes_dir.clone();
        self.spawn_similar_search(db_path, notes_dir, pattern, self.segments_min_words);
    }

    /// Spawn a background thread that embeds the phrase and runs
    /// the similarity query. Mirrors `spawn_segments_search`.
    pub(crate) fn spawn_similar_search(
        &mut self,
        db_path: std::path::PathBuf,
        notes_dir: Option<std::path::PathBuf>,
        pattern: String,
        min_words: usize,
    ) {
        let request = spawn_similar_search(db_path, notes_dir, pattern, min_words);
        self.similar_state.in_flight = true;
        self.similar_state.request = Some(request);
    }

    /// Process a similar-mode search result from the background
    /// thread. Mirrors `process_segments_result` — an embedding
    /// failure (e.g. Ollama unreachable) or a search failure
    /// surfaces as a status message.
    pub(crate) fn process_similar_result(
        &mut self,
        request: SimilarRequest,
        result: Result<Vec<HistoryRow>, String>,
    ) {
        self.similar_state.in_flight = false;
        self.similar_state.request = None;
        let current = SimilarState::current_pattern(&self.query, self.query_prefixes.similar);
        if current != request.pattern {
            // Stale result — the user has typed something else
            // since this search was fired. Discard silently.
            return;
        }
        match result {
            Ok(rows) => {
                self.similar_state.rows = rows;
                self.similar_state.rows_version = self.similar_state.rows_version.wrapping_add(1);
                self.refresh();
            }
            Err(e) => {
                self.set_status_message(format!("Similar mode: {}", e));
            }
        }
    }
}
