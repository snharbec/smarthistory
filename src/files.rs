//! Files-mode directory walker.
//!
//! Walks the current directory tree on a background thread, filters
//! by the user's pattern, and returns rows the TUI can render. The
//! background-thread pattern mirrors the JIRA search path (see
//! `src/jira.rs`): a `std::thread::spawn` does the actual work, an
//! `mpsc::Sender<Vec<HistoryRow>>` reports the result, and
//! an `Arc<AtomicBool>` cancellation flag lets the run loop abort a
//! stale walk when the pattern changes mid-flight.
//!
//! ## Why a separate module
//!
//! Before this split, the files-mode code lived in seven non-adjacent
//! regions of `src/tui.rs` (the App struct fields, the dispatch
//! glue, the request struct, the free walker function, the preview
//! reader, the constant table, and the predicate). Pulling them
//! into one module makes the full feature readable in one place
//! and parallels the JIRA module layout.
//!
//! ## Performance characteristics
//!
//! - **Skip-list:** `DEFAULT_IGNORES` skips common artifact
//!   directories (`target/`, `node_modules/`, etc.) at the entry
//!   level, so the walker never visits them. This is the single
//!   biggest perf win — `target/` alone is 50K+ entries in a
//!   typical Rust project.
//! - **One `stat` per entry:** `entry.metadata()` is called once
//!   per entry; the `is_dir`, the `len`, and the recursion check
//!   all derive from the same `Metadata`.
//! - **Bounded preview reads:** `read_preview_bytes` reads at most
//!   4 KiB per file via `read()` (not `read_to_string`), and
//!   detects binary files (null bytes) to avoid UTF-8 validation
//!   on megabytes of binary data.
//! - **No parallelism:** this is a single-threaded walk. A
//!   parallel walker (via the `ignore` or `walkdir` crate) would
//!   be faster on large trees but adds a dependency.

use crate::tui::state::HistoryRow;
use crate::util::format_size;
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// How long the files-mode walk waits after the last keystroke
/// before spawning the background thread. Matches the JIRA
/// search debounce (400 ms) — both are local/cheap relative to
/// LLM calls, and the user expects fast feedback.
pub const FILES_DEBOUNCE: Duration = Duration::from_millis(400);

/// Default directory basenames to skip during the walk. Hardcoded
/// because almost every project has them; project-specific
/// additions belong in the config (see `Config::files_ignores`).
pub const DEFAULT_IGNORES: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    ".codegraph",
    ".github",
    ".vscode",
    ".idea",
    "build",
    "dist",
    "_build",
    "bazel-out",
    "bazel-testlogs",
    "bazel-bin",
    "__pycache__",
    ".next",
    ".cache",
    ".sass-cache",
    "coverage",
    ".nyc_output",
];

/// A compiled set of basenames to skip, looked up in O(1) per
/// entry. Built once per walk so the hot loop is a single
/// `HashSet::contains` call.
pub struct IgnoreSet {
    inner: HashSet<Box<str>>,
}

impl IgnoreSet {
    /// Build from the config-supplied list plus the built-in
    /// defaults. Duplicates are deduplicated; an empty config
    /// list still gets the defaults.
    pub fn new(config_extras: &[String]) -> Self {
        let mut inner: HashSet<Box<str>> = HashSet::new();
        for name in DEFAULT_IGNORES {
            inner.insert((*name).into());
        }
        for name in config_extras {
            if !name.is_empty() {
                inner.insert(name.as_str().into());
            }
        }
        IgnoreSet { inner }
    }

    /// O(1) lookup. The caller passes the `OsStr` basename via
    /// `as_encoded_bytes()` so we don't have to allocate a
    /// `String` for every entry.
    pub fn contains(&self, name: &std::ffi::OsStr) -> bool {
        self.inner
            .iter()
            .any(|n| n.as_bytes() == name.as_encoded_bytes())
    }
}

/// An in-flight files-mode walk. The background thread sends the
/// result over `receiver`; the run loop polls it. `cancelled`
/// lets the run loop abort a stale walk when the user types more
/// characters (the thread checks the flag just before sending
/// the result, so a walk that completes between the user's edit
/// and the flag check is dropped, not delivered).
pub struct FilesRequest {
    pub receiver: mpsc::Receiver<Vec<HistoryRow>>,
    pub cancelled: Arc<AtomicBool>,
    /// The pattern that was being searched for. Stashed so the
    /// result-processing step can discard stale results (the
    /// user typed more characters while the walk was running).
    pub pattern: String,
}

/// Aggregated files-mode state. The TUI holds one of these and
/// reads it from the run loop's idle tick to decide whether to
/// spawn a background walk.
///
/// `FilesState` doesn't own the `FilesRequest` — it does, but
/// the `Receiver` is moved out by the run loop on poll and
/// the `Request` is moved into `process_files_result`. The
/// `cancelled` flag stays on the request so the run loop
/// can flip it without taking the request out of state.
pub struct FilesState {
    /// When the user last typed in files mode. The debounce
    /// window must elapse before the background walk fires.
    /// `None` means the user hasn't typed anything in files
    /// mode yet (first entry).
    pub debounce_started: Option<std::time::Instant>,
    /// pattern is the same, the walk is not re-triggered
    /// (the cached rows are still fresh).
    pub last_pattern: Option<String>,
    /// Whether a walk is currently in flight (background
    /// thread). Prevents queueing a second walk on every
    /// keystroke.
    pub in_flight: bool,
    /// In-flight walk (background thread). Polled by the run
    /// loop similarly to the JIRA request polls.
    pub request: Option<FilesRequest>,
    /// Cached results of the most recent walk. Populated by
    /// `process_files_result` when the background thread
    /// completes. Empty on first entry (before the first
    /// background walk completes).
    pub rows: Vec<HistoryRow>,
}

impl FilesState {
    /// Empty state — no walk in flight, no debounce armed, no
    /// cached rows.
    pub fn new() -> Self {
        FilesState {
            debounce_started: None,
            last_pattern: None,
            in_flight: false,
            request: None,
            rows: Vec::new(),
        }
    }

    /// Compute the canonical pattern for "is this the same
    /// pattern we just walked?" comparisons. The trim keeps
    /// trailing spaces from re-triggering walks.
    pub fn current_pattern(query: &str, prefix: char) -> String {
        let body = if query.starts_with(prefix) {
            &query[prefix.len_utf8()..]
        } else {
            query
        };
        body.trim().to_string()
    }

    /// True if the given pattern matches what we have cached
    /// (or what's currently walking).
    pub fn has_results_for(&self, pattern: &str) -> bool {
        self.last_pattern.as_deref() == Some(pattern)
    }
}

impl Default for FilesState {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::debounce::Cancellable for FilesRequest {
    fn cancelled_flag(&self) -> &Arc<AtomicBool> {
        &self.cancelled
    }
}

impl crate::debounce::Debounced for FilesState {
    type Request = FilesRequest;
    fn debounce_started(&mut self) -> &mut Option<std::time::Instant> {
        &mut self.debounce_started
    }
    fn last_pattern(&mut self) -> &mut Option<String> {
        &mut self.last_pattern
    }
    fn in_flight(&mut self) -> &mut bool {
        &mut self.in_flight
    }
    fn request(&mut self) -> &mut Option<FilesRequest> {
        &mut self.request
    }
}

/// Recursively walk a directory, adding matching files and
/// directories to `rows`. Hidden entries (names starting with
/// `.`) and `ignore.contains(...)` matches are skipped at the
/// entry level. Permission errors are silently swallowed so a
/// single unreadable subdirectory doesn't abort the whole
/// walk.
///
/// `next_id` is a monotonically-decreasing counter used to
/// generate the synthetic row ids (negative integers so they
/// can't collide with the SQLite-allocated positive history
/// ids; same convention as the directories and todo modes).
///
/// **Filter semantics:** the filter check only controls whether
/// the *current* entry is added to the result list. Directory
/// recursion is unconditional, so `~main.rs` still finds
/// `src/main.rs` even though `src/` itself doesn't match.
/// True iff every token in `tokens` is a case-insensitive
/// substring of `display`. Empty `tokens` always matches — used
/// by both `FilesFilter::Substring` (the whole filter) and
/// `FilesFilter::Glob`'s `extra_tokens` (narrowing on top of the
/// basename regex match).
fn matches_all_tokens(display: &str, tokens: &[String]) -> bool {
    tokens
        .iter()
        .all(|tok| display.to_lowercase().contains(tok))
}

pub fn walk_dir(
    root: &Path,
    dir: &Path,
    filter: &FilesFilter,
    ignore: &IgnoreSet,
    next_id: &mut i64,
    rows: &mut Vec<HistoryRow>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name();
        // Skip hidden entries. Using `as_encoded_bytes()` is
        // allocation-free (no OsString → String conversion)
        // and works on any non-UTF-8 path.
        if name.as_encoded_bytes().first() == Some(&b'.') {
            continue;
        }
        // Skip user/excluded directories by basename.
        if ignore.contains(&name) {
            continue;
        }
        // One stat per entry — derive is_dir, len, and the
        // recursion check from the same Metadata. Without
        // this, `entry.file_type()` (free, no syscall on
        // most platforms) plus `entry.metadata()` (one
        // syscall) would be two passes through the kernel.
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = meta.is_dir();
        // Compute the display path relative to root.
        let path = entry.path();
        let display = compute_display(root, &path, &name);
        // Apply the filter — either the default AND-of-substring-
        // tokens match (against the display path relative to root),
        // or a full-match glob-derived regex against the entry's
        // basename only (`FilesFilter::Glob`, used exclusively by
        // the `--glob-complete` picker).
        let matches_filter = match filter {
            FilesFilter::Substring(tokens) => matches_all_tokens(&display, tokens),
            FilesFilter::Glob { basename, extra_tokens } => {
                basename.is_match(&name.to_string_lossy())
                    && matches_all_tokens(&display, extra_tokens)
            }
        };
        if matches_filter {
            let id = *next_id;
            *next_id -= 1;
            let mode = if is_dir { "directory" } else { "file" };
            let comment = if is_dir {
                String::new()
            } else {
                format_size(meta.len())
            };
            let abs_path = if path.is_absolute() {
                path.to_string_lossy().into_owned()
            } else {
                std::env::current_dir()
                    .unwrap_or_default()
                    .join(&path)
                    .to_string_lossy()
                    .into_owned()
            };
            // The row's `timestamp` is the file's modification
            // time (not "when this row was created" — every row is
            // created at walk time). This is both the value shown
            // in the list's age/time column (`render_row` reads
            // `row.timestamp` uniformly across every mode) and the
            // sort key `spawn_walk` uses to show recently-modified
            // files first. `0` (Unix epoch) on any failure to read
            // it — same convention `directories.rs`/`todo.rs` use
            // for "no meaningful timestamp available" — sorts the
            // row to the bottom rather than erroring the whole walk.
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            // The preview is left empty here. Loading a 4KB
            // snippet of every file in the walk would dominate
            // the runtime on large directories. The render
            // layer populates the preview for the currently-
            // selected row (and a small look-ahead window) on
            // demand. See `read_preview_bytes` for the
            // bounded-read implementation.
            rows.push(HistoryRow {
                id,
                command: display,
                directory: abs_path,
                session_id: String::new(),
                exit_code: 0,
                timestamp: mtime,
                comment,
                output: String::new(),
                mode: mode.to_string(),
                source: String::new(),
                ..Default::default()
            });
        }
        // Always recurse into directories so deep files
        // are found even when the ancestor doesn't match
        // the filter pattern.
        if is_dir {
            walk_dir(root, &path, filter, ignore, next_id, rows);
        }
    }
}

/// The two filtering strategies `walk_dir` supports. `Substring` is
/// the original, default `/` mode behavior (AND of case-insensitive
/// substring tokens against the display path relative to `root`);
/// `Glob` is new, used exclusively by the `--glob-complete` picker
/// (`App::spawn_files_walk`) — a full-match regex (built by
/// `glob_to_regex`) against the entry's basename, AND (if any)
/// case-insensitive substring tokens against the display path
/// relative to `root`. The picker's typed body is split on
/// whitespace: the FIRST word is always the glob (root-scoped via
/// `split_glob_root`), every word after it narrows further as a
/// plain substring — e.g. `*.md jira` matches every markdown file
/// whose relative path contains "jira". The picker's walk root is
/// already scoped to the literal-prefix directory from the first
/// word, so `basename` only needs to check the entry itself;
/// recursion handles "search everywhere under that root."
pub enum FilesFilter<'a> {
    Substring(&'a [String]),
    Glob {
        basename: &'a regex::Regex,
        extra_tokens: &'a [String],
    },
}

/// Translate a shell glob pattern (`*`, `?`, `[...]`, and `**`,
/// which collapses to the same wildcard as `*` — see the module doc
/// on why matching is basename-only + always-recursive rather than
/// literal glob semantics) into an anchored, case-insensitive regex
/// suitable for `FilesFilter::Glob`. Literal runs are regex-escaped
/// so metacharacters like `.` or `+` in the pattern (e.g. `a.b*`)
/// aren't misinterpreted. A leading `!` inside a bracket expression
/// is rewritten to `^` (glob negation → regex negation); everything
/// else inside `[...]` is passed through as-is (POSIX character
/// classes like `[:alpha:]` aren't specially handled — out of scope
/// for this feature's glob subset).
pub fn glob_to_regex(pattern: &str) -> Result<regex::Regex, regex::Error> {
    let mut out = String::with_capacity(pattern.len() + 8);
    out.push_str("(?i)^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                // `**` collapses to the same single wildcard as `*`
                // — this feature doesn't distinguish "any depth"
                // from "one segment" since matching is basename-only
                // and the walk is already unconditionally recursive.
                out.push_str(".*");
                while i < chars.len() && chars[i] == '*' {
                    i += 1;
                }
                continue;
            }
            '?' => {
                out.push('.');
            }
            '[' => {
                out.push('[');
                i += 1;
                if i < chars.len() && chars[i] == '!' {
                    out.push('^');
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    // Escape a literal backslash so it can't break
                    // out of the character class in the generated
                    // regex; everything else (including regex
                    // metacharacters like `-` for ranges) passes
                    // through, matching typical glob bracket syntax.
                    if chars[i] == '\\' {
                        out.push_str("\\\\");
                    } else {
                        out.push(chars[i]);
                    }
                    i += 1;
                }
                if i < chars.len() {
                    out.push(']');
                }
            }
            c => {
                out.push_str(&regex::escape(&c.to_string()));
            }
        }
        i += 1;
    }
    out.push('$');
    regex::Regex::new(&out)
}

/// Split a raw glob word (straight from the shell buffer, e.g.
/// `foo/bar/a*`) into `(root_suffix, basename_pattern)`. Leading
/// path segments before the final `/` become `root_suffix` only if
/// NONE of them contain glob metacharacters (`* ? [`) — so
/// `foo/bar/a*` splits to `("foo/bar", "a*")`, but a globby leading
/// segment like `**/*.rs` or `src/*/test.rs` falls back to an empty
/// `root_suffix` (the walk stays at the base root) with just the
/// FINAL segment (`*.rs`, `test.rs`) as the pattern — matching
/// against a basename can never usefully include a literal `/`
/// anyway. Still fully recursive under the base root, so nothing is
/// missed, just less pruned than a literal-prefix split would be. A
/// word with no `/` at all (e.g. `a*`) returns `("", word)`.
pub fn split_glob_root(word: &str) -> (String, String) {
    let Some(slash_idx) = word.rfind('/') else {
        return (String::new(), word.to_string());
    };
    let leading = &word[..slash_idx];
    let final_segment = word[slash_idx + 1..].to_string();
    let is_globby = |s: &str| s.contains(['*', '?', '[']);
    if leading.split('/').any(is_globby) {
        (String::new(), final_segment)
    } else {
        (leading.to_string(), final_segment)
    }
}

/// Compute the path string shown in the TUI list. For an entry
/// at `<root>/src/main.rs`, the display is `src/main.rs`. For
/// an entry whose `path` is already the root (shouldn't
/// happen via `read_dir`, but be safe), the display falls back
/// to the file name.
fn compute_display(root: &Path, path: &Path, name: &std::ffi::OsStr) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
        _ => name.to_string_lossy().into_owned(),
    }
}

/// Read up to 4 KiB of a file for the preview pane, returning
/// `None` if the file is unreadable, empty, or binary
/// (heuristic: any NUL byte in the first 4 KiB).
///
/// **Why bounded:** the previous implementation used
/// `read_to_string` which allocated the entire file into a
/// `String` (after UTF-8 validation). For a 1 GB binary file
/// in a `/` search that's matched, that's a 1 GB allocation
/// on the walk thread. The bounded `read()` caps the
/// allocation at 4 KiB and the binary check avoids
/// `String::from_utf8_lossy` on megabytes of binary data.
///
/// Returns `Some(text)` for any non-binary file that contains
/// at least one byte — even an incomplete single byte is
/// useful as a hint.
#[allow(dead_code)]
pub fn read_preview_bytes(path: &Path) -> Option<String> {
    const MAX_PREVIEW: usize = 4096;
    let mut file = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; MAX_PREVIEW];
    let n = file.read(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    let buf = &buf[..n];
    // NUL byte is a reliable heuristic for binary files
    // (text files don't contain NUL except in obscure
    // encodings). The check is O(n) on 4 KiB which is
    // cheap.
    if buf.contains(&0) {
        return None;
    }
    // Truncate to the last complete UTF-8 character
    // boundary so the render layer doesn't see an
    // invalid tail. `from_utf8` on the full buffer is
    // the common case; we trim only if the last char
    // is cut off.
    match std::str::from_utf8(buf) {
        Ok(s) => Some(s.to_string()),
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            Some(String::from_utf8_lossy(&buf[..valid_up_to]).into_owned())
        }
    }
}

/// Sort `rows` newest-modified first (the `/` mode is for finding
/// files you just touched, not alphabetical browsing). Ties
/// (identical mtime, or both `0` because the metadata read failed)
/// fall back to path order for a deterministic display. Directories
/// don't get a first-class grouping here — `mode::files::fetch`
/// filters them out of what's actually shown, so sorting them in
/// with the files instead of segregating them first means
/// `spawn_walk`'s `truncate(1000)` quota is spent on the files that
/// will actually be visible, not eaten by directories that never
/// render. Extracted from `spawn_walk` as a pure function so the
/// ordering can be unit-tested directly.
pub(crate) fn sort_rows_newest_modified_first(rows: &mut [HistoryRow]) {
    rows.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| a.command.cmp(&b.command)));
}

/// Owned counterpart of `FilesFilter`, for crossing the
/// `spawn_walk` thread boundary (`FilesFilter` borrows a token slice
/// or `Regex` reference, neither of which can be moved into a
/// spawned closure with a `'static` bound). `Substring` re-derives
/// its tokens from `spawn_walk`'s `pattern` argument internally
/// (unchanged from before this type existed); `Glob` carries a
/// precompiled regex from `glob_to_regex` plus the extra substring
/// tokens (everything after the first whitespace-separated word —
/// see `FilesFilter::Glob`'s doc comment).
pub enum FilesFilterSpec {
    Substring,
    Glob {
        basename: regex::Regex,
        extra_tokens: Vec<String>,
    },
}

/// Spawn a background thread that walks `root`, filters by
/// `pattern` (tokenized per `filter_spec`), and sends the result
/// over `tx`. Used by `App::spawn_files_walk`.
///
/// **The walk happens on a worker thread, not the main
/// thread**, so the TUI never blocks on filesystem I/O.
/// Cancellation is cooperative: the run loop flips
/// `cancelled` to abort a stale walk; the worker checks
/// the flag just before sending.
pub fn spawn_walk(
    pattern: String,
    ignore: IgnoreSet,
    root: PathBuf,
    filter_spec: FilesFilterSpec,
) -> FilesRequest {
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let tokens: Vec<String> = pattern
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();
    std::thread::spawn(move || {
        let mut rows: Vec<HistoryRow> = Vec::new();
        let mut next_id: i64 = -1;
        let filter = match &filter_spec {
            FilesFilterSpec::Substring => FilesFilter::Substring(&tokens),
            FilesFilterSpec::Glob { basename, extra_tokens } => {
                FilesFilter::Glob { basename, extra_tokens }
            }
        };
        walk_dir(&root, &root, &filter, &ignore, &mut next_id, &mut rows);
        sort_rows_newest_modified_first(&mut rows);
        rows.truncate(1000);
        if !cancelled_clone.load(Ordering::Relaxed) {
            // The walker is
            // infallible: permission
            // errors and missing
            // directories are
            // swallowed at the
            // `read_dir` boundary.
            // Errors don't need to
            // flow through the
            // channel.
            let _ = tx.send(rows);
        }
    });
    FilesRequest {
        receiver: rx,
        cancelled,
        pattern,
    }
}

#[cfg(test)]
mod tests;
