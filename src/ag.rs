//! AG-mode content search.
//!
//! Searches the current directory tree in-process, on a background
//! thread, and returns rows the TUI can render. The pattern mirrors
//! the files-mode walker (`src/files.rs`) and the JIRA search path
//! (`src/jira.rs`): a background thread does the actual work, an
//! mpsc channel reports results, and an `Arc<AtomicBool>`
//! cancellation flag lets the run loop abort stale searches.
//!
//! This used to shell out to the external `ag` (The Silver Searcher)
//! binary. It now searches in-process using the same libraries
//! ripgrep itself is built from: `grep-regex`/`grep-matcher` compile
//! the search term to a matcher, `grep-searcher` streams line
//! matches out of one file at a time (with binary detection), and
//! `ignore` provides the gitignore-aware parallel directory walk —
//! there's no smarthistory-side ignore config of any kind for this
//! mode, so `ignore`'s own `.gitignore`/`.ignore`/global-exclude
//! handling is what replaces `ag`'s built-in VCS-ignore awareness.
//! One behavior note: like `git` itself (and unlike `ag`), a bare
//! `.gitignore` file only takes effect inside an actual git
//! repository (i.e. somewhere with a `.git` directory) — a stray
//! `.gitignore` with no `.git` anywhere above it is not honored. In
//! practice this is a non-issue (a `.gitignore` file with no git repo
//! at all is a rare setup), but it's a real, deliberate divergence
//! from `ag`'s more lenient "any `.gitignore`-looking file, repo or
//! not" behavior.
//!
//! ## Search semantics
//!
//! The query body is split on whitespace via the shared
//! [`crate::highlight::parse_query_tokens`] helper:
//!
//! - **Search terms** (no prefix): the first becomes the search
//!   pattern; any remaining terms further narrow the matched line
//!   (case-insensitive AND-substring), same as before.
//! - **Glob tokens** (`*`): restrict which files are walked, via
//!   `ignore::overrides::OverrideBuilder` (gitignore-glob syntax,
//!   the same syntax users already type). Multiple glob tokens are
//!   OR'd together.
//! - **Language tokens** (`@rust`): restrict which files are walked
//!   AND choose the highlight language for the preview — restricting
//!   the walk (not just cosmetic highlighting) matches how `ag
//!   --rust` behaved. Implemented via `crate::highlight::
//!   extensions_for_language` (the same lang→extension table tags
//!   mode already uses) rather than `ignore`'s own separate file-type
//!   database, so language names stay consistent across the whole
//!   app. Multiple language tokens are OR'd; an unrecognized `@lang`
//!   contributes no constraint at all (safer than silently zeroing
//!   every result over a typo).
//!
//! Examples:
//!   `,result @rust`     -> search for "result", .rs files only
//!   `,tui *.rs @rust`   -> search for "tui", .rs files only (both filters agree here)
//!
//! ## Performance characteristics
//!
//! - **Context/highlight only the rows that survive truncation:**
//!   `run_ag` collects every matched line into a lightweight
//!   `HistoryRow` (file, line, matched text, file mtime) FIRST, sorts
//!   by mtime, and truncates to 1000 — only THEN does it read each
//!   surviving row's source context and (for up to 50) syntax-
//!   highlight it. A broad search term that matches thousands of
//!   lines pays that file-read + highlight cost only for the matches
//!   actually shown, not for every one of them.
//! - **One mtime stat per file, not per match:** `grep-searcher`
//!   naturally groups matches by file (one `Searcher::search_path`
//!   call per file, in the per-entry walk callback), so the file's
//!   mtime is stat'd once and reused for every match line found in
//!   it — no separate cross-file mtime cache is needed.
//! - **One file read per search, not one per match:** the
//!   context-reading pass (after truncation) uses
//!   `read_source_context_with_cache`, so a file with many surviving
//!   matches is read from disk once and reused, instead of a fresh
//!   `read_to_string` per match line.
//! - **A superseded search stops promptly:** `run_ag` checks the
//!   same `cancelled` flag the run loop sets when a newer keystroke
//!   supersedes this search (see `crate::debounce::touch`) at the top
//!   of every per-file walk callback (returning `ignore::WalkState::
//!   Quit`, which stops the *entire* parallel walk, not just that one
//!   worker thread — verified empirically, since no canonical example
//!   of this exact composition exists), inside the per-line match
//!   callback (as a backstop for a single very-large file), and
//!   during the context/highlight pass — so a stale search stops
//!   doing real work as soon as it's noticed, instead of only having
//!   its *result* discarded once finished.

use crate::highlight::{highlight_with_bat, highlight_with_bat_auto, parse_query_tokens};
use crate::tui::read_source_context_with_cache;
use crate::tui::state::HistoryRow;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{WalkBuilder, WalkState};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

/// How long the ag-mode debounce waits after the last
/// keystroke before spawning the background search.
/// Same value as JIRA and files modes (400 ms).
pub const AG_DEBOUNCE: Duration = Duration::from_millis(400);

/// An in-flight ag search. The background thread sends
/// results over `receiver`; the run loop polls it.
/// `cancelled` lets the run loop abort a stale search.
pub struct AgRequest {
    pub receiver: mpsc::Receiver<Vec<HistoryRow>>,
    pub cancelled: Arc<AtomicBool>,
    /// The pattern that was being searched for.
    pub pattern: String,
}

/// Aggregated ag-mode state. Held by the TUI App.
pub struct AgState {
    /// Debounce timer, armed on every keystroke in ag mode.
    pub debounce_started: Option<std::time::Instant>,
    /// Last successfully searched pattern. Prevents re-querying
    /// when the pattern hasn't changed.
    pub last_pattern: Option<String>,
    /// Whether a search is currently in flight.
    pub in_flight: bool,
    /// In-flight request (background thread).
    pub request: Option<AgRequest>,
    /// Cached results of the most recent search.
    pub rows: Vec<HistoryRow>,
}

impl AgState {
    pub fn new() -> Self {
        AgState {
            debounce_started: None,
            last_pattern: None,
            in_flight: false,
            request: None,
            rows: Vec::new(),
        }
    }

    /// Extract the body after the prefix character.
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

impl Default for AgState {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::debounce::Cancellable for AgRequest {
    fn cancelled_flag(&self) -> &Arc<AtomicBool> {
        &self.cancelled
    }
}

impl crate::debounce::Debounced for AgState {
    type Request = AgRequest;
    fn debounce_started(&mut self) -> &mut Option<std::time::Instant> {
        &mut self.debounce_started
    }
    fn last_pattern(&mut self) -> &mut Option<String> {
        &mut self.last_pattern
    }
    fn in_flight(&mut self) -> &mut bool {
        &mut self.in_flight
    }
    fn request(&mut self) -> &mut Option<AgRequest> {
        &mut self.request
    }
}

/// Spawn a background thread that searches the current directory
/// tree in-process and sends the result rows back.
pub fn spawn_ag_search(pattern: String) -> AgRequest {
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let pattern_for_thread = pattern.clone();
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    std::thread::spawn(move || {
        // `run_ag` is handed the SAME flag `touch`/`cancel_in_flight`
        // set when this search gets superseded, so it can stop the
        // walk (and the context/highlight pass) as soon as it's
        // noticed, instead of grinding through a (possibly large)
        // result set that's just going to be thrown away by the
        // `!cancelled` check below anyway — see the module-level doc
        // comment for how that's wired through the walk.
        let rows = run_ag(&pattern_for_thread, &root, &cancelled_clone);
        if !cancelled_clone.load(Ordering::Relaxed) {
            let _ = tx.send(rows);
        }
    });

    AgRequest {
        receiver: rx,
        cancelled,
        pattern,
    }
}

/// A file's modification time as Unix epoch seconds, or `0` on any
/// failure to read it (missing file, permission error, a platform
/// without mtime support) — same "no meaningful timestamp available"
/// convention `files.rs::walk_dir` uses, so a stat error never
/// aborts the whole search.
fn file_mtime(path: &str) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Sort `rows` newest-modified-file first. Ties (multiple matches in
/// the same file, or files whose mtime couldn't be read) keep their
/// relative order — `sort_by` is stable, so same-file matches stay
/// in the line-number order they were found in. Extracted from
/// `run_ag` as a pure function so the ordering can be unit-tested
/// without a real filesystem walk.
fn sort_rows_newest_modified_first(rows: &mut [HistoryRow]) {
    rows.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
}

/// A `grep_searcher::Sink` that turns each matched line in one file
/// into a (context/highlight-free) `HistoryRow` and sends it over
/// `tx`. Constructed fresh per file by the per-entry walk callback in
/// [`run_ag`], so `abs_path`/`mtime`/`basename` are that one file's
/// values, computed once and reused for every match line — see the
/// module-level doc comment on why no cross-file mtime cache is
/// needed anymore.
struct RowSink<'a> {
    tx: mpsc::Sender<HistoryRow>,
    abs_path: String,
    mtime: i64,
    basename: String,
    post_filter: &'a [String],
    cancelled: &'a AtomicBool,
}

impl Sink for RowSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        // Backstop for a single very-large file: the per-entry walk
        // callback already checks `cancelled` before starting this
        // file (and returns `WalkState::Quit`, which stops the whole
        // walk — see the module doc comment), but a file already
        // being scanned won't notice that until it's done. `Ok(false)`
        // stops just this one `search_path` call early.
        if self.cancelled.load(Ordering::Relaxed) {
            return Ok(false);
        }

        let content = String::from_utf8_lossy(mat.bytes());
        let content = content.trim_end_matches(['\n', '\r']);

        // Post-filter: every remaining search term must appear
        // in the matched line (case-insensitive) — unchanged logic,
        // just applied here instead of a flat stdout-line loop.
        if !self.post_filter.is_empty() {
            let content_lower = content.to_lowercase();
            if !self
                .post_filter
                .iter()
                .all(|t| content_lower.contains(&t.to_lowercase()))
            {
                return Ok(true);
            }
        }

        // `mat.line_number()` is a real, structurally-counted line
        // number from the file itself — unlike the old ag-stdout text
        // parse, there's no colon-splitting ambiguity to guard
        // against here. It's still stored as a validated digit
        // string (never anything else) rather than passed through
        // as free-form text, since `session_id` is later spliced
        // unquoted into a `$EDITOR +<line> <file>` shell string (see
        // `stage_editor_open_at_line` in `tui/actions.rs`).
        let line_number = mat.line_number().unwrap_or(0);

        let _ = self.tx.send(HistoryRow {
            id: 0, // assigned by run_ag once every row is collected
            command: content.trim_start().to_string(),
            directory: self.abs_path.clone(),
            session_id: line_number.to_string(),
            exit_code: 0,
            timestamp: self.mtime,
            comment: self.basename.clone(),
            output: String::new(),
            mode: "ag".to_string(),
            source: String::new(),
            ..Default::default()
        });
        Ok(true)
    }
}

/// Search `root` for `pattern`, in-process, and return the matching
/// rows. `root` must be absolute (its caller, `spawn_ag_search`,
/// always passes `std::env::current_dir()`) — entries from the walk
/// inherit `root`'s absoluteness, so no separate cwd-joining step is
/// needed the way the old ag-stdout-relative-path parsing required.
/// `cancelled` is the same flag the run loop flips when a newer
/// keystroke supersedes this search (see `crate::debounce::touch`);
/// this function checks it at the top of every per-file walk
/// callback (stopping the whole walk), inside the per-line match
/// callback (stopping one very-large file early), and while doing
/// the context-read/highlight pass afterward.
fn run_ag(pattern: &str, root: &Path, cancelled: &Arc<AtomicBool>) -> Vec<HistoryRow> {
    // If the pattern is empty, return nothing.
    if pattern.is_empty() {
        return Vec::new();
    }

    // Split into search terms, file-pattern globs, and `@lang`
    // language flags via the shared classifier. See
    // `crate::highlight::parse_query_tokens` for the rules.
    let tokens = parse_query_tokens(pattern);

    // If there are no search terms at all (only globs and/or
    // languages), we have nothing to search for.
    if tokens.terms.is_empty() {
        return Vec::new();
    }

    // First term is the primary search pattern.
    // Remaining terms are post-filtered.
    let primary = tokens.terms[0].clone();
    let post_filter = tokens.terms[1..].to_vec();

    // Smart case: case-insensitive unless the pattern itself has an
    // uppercase character — the same convention `App::is_case_
    // sensitive` already uses elsewhere in the TUI, and it matches
    // `ag`'s own documented default.
    let case_insensitive = !primary.chars().any(|c| c.is_uppercase());
    let matcher = match grep_regex::RegexMatcherBuilder::new()
        .case_insensitive(case_insensitive)
        .build(&primary)
    {
        Ok(m) => m,
        // An invalid pattern (as a regex) yields an empty result,
        // same "bad invocation -> empty" contract the old ag-process
        // path had for e.g. `ag` not being on PATH.
        Err(_) => return Vec::new(),
    };

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false) // ag mode always behaves as if `--hidden` was passed
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .follow_links(false);

    // Glob tokens restrict which files are walked (gitignore-glob
    // syntax, the same syntax users already type — e.g. `*.rs`).
    // Multiple glob tokens are OR'd, matching `ignore::Override`'s
    // native multi-pattern semantics.
    if !tokens.globs.is_empty() {
        let mut ob = ignore::overrides::OverrideBuilder::new(root);
        for g in &tokens.globs {
            let _ = ob.add(g);
        }
        if let Ok(overrides) = ob.build() {
            builder.overrides(overrides);
        }
    }

    // Language tokens ALSO restrict which files are walked (matching
    // `ag --rust`'s behavior of filtering search scope, not just
    // choosing a highlight language). Reuses the same lang->extension
    // table tags mode already uses, rather than `ignore`'s own
    // separate file-type database, so language names stay consistent
    // across the app. Multiple language tokens are OR'd; an
    // unrecognized `@lang` contributes no constraint.
    if !tokens.languages.is_empty() {
        let langs = tokens.languages.clone();
        builder.filter_entry(move |entry| {
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                return true; // never prune directories
            }
            let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) else {
                return false;
            };
            langs.iter().any(|lang| {
                crate::highlight::extensions_for_language(lang)
                    .is_some_and(|exts| exts.iter().any(|e| e.eq_ignore_ascii_case(ext)))
            })
        });
    }

    let walker = builder.build_parallel();
    let (tx, rx) = mpsc::channel::<HistoryRow>();

    walker.run(|| {
        let matcher = matcher.clone();
        let tx = tx.clone();
        let cancelled = cancelled.clone();
        let post_filter = post_filter.clone();
        Box::new(move |result| {
            if cancelled.load(Ordering::Relaxed) {
                // Stops the ENTIRE parallel walk, not just this
                // worker thread — verified empirically (see the
                // module-level doc comment), since no canonical
                // example of this exact composition exists.
                return WalkState::Quit;
            }
            let entry = match result {
                Ok(e) => e,
                Err(_) => return WalkState::Continue,
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            let path = entry.path();
            let abs_path = path.to_string_lossy().into_owned();
            // One stat per FILE, reused for every match line found in
            // it — see the module-level doc comment.
            let mtime = file_mtime(&abs_path);
            let basename = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();

            let mut searcher = SearcherBuilder::new()
                .binary_detection(BinaryDetection::quit(b'\x00'))
                .line_number(true)
                .build();
            let sink = RowSink {
                tx: tx.clone(),
                abs_path,
                mtime,
                basename,
                post_filter: &post_filter,
                cancelled: cancelled.as_ref(),
            };
            let _ = searcher.search_path(&matcher, path, sink);
            WalkState::Continue
        })
    });
    drop(tx);

    // Assign the synthetic negative ids during this single-threaded
    // drain. Their VALUES are no longer deterministic across runs
    // (different worker threads race), which is fine — `HistoryRow::
    // id` is already a synthetic, collision-tolerant value elsewhere
    // in the app; the real sort key below (`timestamp`) is unaffected
    // by walk-thread scheduling.
    let mut next_id: i64 = -1;
    let mut rows: Vec<HistoryRow> = rx
        .into_iter()
        .map(|mut row| {
            row.id = next_id;
            next_id -= 1;
            row
        })
        .collect();

    sort_rows_newest_modified_first(&mut rows);
    // Cap results to keep the UI responsive — applied AFTER sorting
    // (and BEFORE the context/highlight pass below) so a search with
    // more than 1000 total matches only ever pays the context-read/
    // highlight cost for the matches in the most-recently-modified
    // files that will actually be shown, not for every match found.
    rows.truncate(1000);

    // Second pass: read 5 lines of context around each surviving
    // match (2 before, the match line, 2 after — same pattern as
    // tags mode) and, for up to 50, syntax-highlight it. Deferred
    // until now so this — the actually expensive part of a search —
    // is bounded by what's shown, not by the raw hit count.
    // `read_source_context_with_cache` keeps each file's full
    // contents cached across matches, so a file with several
    // surviving matches is read from disk once, not once per match.
    let mut context_cache: HashMap<PathBuf, String> = HashMap::new();
    let mut highlight_count = 0usize;
    const HIGHLIGHT_MAX: usize = 50;
    let highlight_lang = tokens.languages.first().map(String::as_str);
    for row in rows.iter_mut() {
        if cancelled.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let line_number: usize = row.session_id.parse().unwrap_or(0);
        let context = read_source_context_with_cache(&row.directory, line_number, &mut context_cache);

        // If a language was specified, pipe the context through
        // `highlight_with_bat` for syntax highlighting. We cap the
        // number of highlight calls to keep the background thread
        // responsive. With no `@lang`, fall through to
        // `highlight_with_bat_auto`'s extension-based
        // auto-detection so `.rs` / `.java` / `.py` matches still
        // get colored previews.
        row.output = if let Some(lang) = highlight_lang {
            if highlight_count < HIGHLIGHT_MAX {
                highlight_count += 1;
                highlight_with_bat(&context, lang).unwrap_or(context)
            } else {
                context
            }
        } else if highlight_count < HIGHLIGHT_MAX {
            highlight_count += 1;
            highlight_with_bat_auto(&context, &row.directory).unwrap_or(context)
        } else {
            context
        };
        row.source = if let Some(lang) = highlight_lang {
            format!("ag:{}", lang)
        } else {
            "ag".to_string()
        };
    }

    rows
}

#[cfg(test)]
mod tests;
