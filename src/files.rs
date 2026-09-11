//! Files-mode directory walker.
//!
//! Walks the current directory tree **once**, on a background
//! thread, and caches every entry. Each keystroke after that
//! filters the cached list in memory (`filter_rows`) — it does NOT
//! re-walk the filesystem. This mirrors how `fzf` (piped from
//! `fd`/`find`) actually works: build the candidate list once, then
//! do fast in-memory filtering per keystroke, rather than re-running
//! the expensive part on every character typed. The background-
//! thread pattern otherwise mirrors the JIRA search path (see
//! `src/jira.rs`): a `std::thread::spawn` does the actual work and
//! an `mpsc::Sender<Vec<HistoryRow>>` reports the result — but
//! unlike JIRA (and unlike this module's own earlier per-pattern-walk
//! design), there's no cancellation flag: the walk isn't tied to any
//! particular pattern, so a later keystroke never makes the in-flight
//! walk stale.
//!
//! **This wasn't always the design.** The walk used to re-run (fresh
//! `read_dir` + `metadata()` calls for every entry, filtered inline
//! via `FilesFilter`) on every debounced keystroke. Fine for a small
//! tree, but scales badly with depth/size: typing a 5-character
//! filter in a large repo triggered 5 full filesystem re-walks
//! instead of 1 walk + 5 cheap in-memory filters. Splitting "walk"
//! (`walk_dir`, I/O-bound, runs once) from "filter" (`filter_rows`,
//! CPU-only, runs on every keystroke against the cached result) is
//! the fix — see `App::files_touch` / `crate::tui::mode::files::fetch`
//! for how the two halves connect. `FilesFilter`'s glob-vs-substring
//! split (and the `--glob-complete` picker's root-scoping via
//! `split_glob_root`) is unchanged in spirit — it just runs against
//! the cached tree in `filter_rows` instead of gating what `walk_dir`
//! collects.
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
//! - **Walk once, filter many:** see the module-level note above —
//!   this is the dominant perf win for deep/large trees, well ahead
//!   of anything below.
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
//! - **Parallel, streamed walk:** [`spawn_walk`] fans the walk out
//!   across a small pool of worker threads (see [`parallel_walk`]),
//!   one directory at a time via a shared work queue — no `ignore`/
//!   `walkdir` crate dependency needed, just `std::thread::scope` +
//!   a mutex/condvar queue. Each thread streams the rows for the
//!   directory it just finished straight to the TUI over the mpsc
//!   channel as soon as that directory is done, instead of
//!   collecting the entire tree before sending anything — the list
//!   fills in progressively as the walk runs rather than staying
//!   empty until the whole tree (which may be huge) has been
//!   walked. This is what actually makes files mode feel as
//!   responsive as `fd`/`fzf`; the single-threaded batch-send design
//!   this replaced was the main source of "nothing appears for a
//!   while" on large trees.
//! - **No Git-commit-timestamp lookup:** an earlier version of this
//!   walker shelled out to `git log --name-only` over the repo's
//!   *entire history* to prefer each tracked file's last-commit time
//!   over its filesystem mtime for sorting. That's O(every commit
//!   ever made) and ran synchronously before a single row could be
//!   shown — on any repo with real history it dominated the time to
//!   first result. Sorting now uses filesystem mtime only.

use crate::tui::state::HistoryRow;
use crate::util::format_size;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{mpsc, Condvar, Mutex};

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

/// The one-shot background walk. Several worker threads (see
/// [`parallel_walk`]) each hold a clone of the same `mpsc` sender, so
/// `receiver` yields many small messages — one per directory
/// processed, in whatever order the threads happen to finish them —
/// rather than a single final batch. The run loop polls it every
/// tick, draining whatever has arrived so far; once every worker
/// thread has exited, every sender clone is dropped and the
/// `receiver` starts reporting `Disconnected`, which is the "walk is
/// fully done" signal. No cancellation handle: unlike the old
/// per-keystroke design, the walk isn't tied to any particular
/// pattern, so a later keystroke never makes an in-flight walk stale.
pub struct FilesRequest {
    pub receiver: mpsc::Receiver<Vec<HistoryRow>>,
}

/// Aggregated files-mode state. The TUI holds one of these and
/// reads it from the run loop's idle tick to decide whether to
/// spawn the one-shot background walk.
pub struct FilesState {
    /// The directory-tree walk result accumulated so far — every
    /// file and directory found under the session's walk root
    /// (`files_root`, or `file_picker_lock.base_root` for a locked
    /// picker). `None` until the first streamed chunk arrives (see
    /// `App::files_touch` / `App::apply_files_walk_update`); grows
    /// incrementally, one directory's worth of rows at a time, as
    /// the background walk streams results in — it's NOT waiting for
    /// the whole tree before the first rows land here. Populated
    /// exactly once per TUI session. Every keystroke filters
    /// whatever's in THIS list so far in memory
    /// (`crate::tui::mode::files::fetch` → `filter_rows`) instead of
    /// re-walking the filesystem.
    pub all_rows: Option<Vec<HistoryRow>>,
    /// Whether the one-shot walk is currently running. Prevents
    /// `files_touch` from spawning a second one while the first
    /// is still in flight.
    pub in_flight: bool,
    /// In-flight walk (background thread). Polled by the run
    /// loop similarly to the JIRA request polls.
    pub request: Option<FilesRequest>,
}

impl FilesState {
    /// Empty state — no walk in flight, no cached tree yet.
    pub fn new() -> Self {
        FilesState {
            all_rows: None,
            in_flight: false,
            request: None,
        }
    }

    /// Strip the files-mode prefix (`/` by default) and
    /// surrounding whitespace from `query`, giving the raw filter
    /// text typed after it. Used to derive the token/glob filter
    /// `crate::tui::mode::files::fetch` matches against.
    pub fn current_pattern(query: &str, prefix: char) -> String {
        let body = if query.starts_with(prefix) {
            &query[prefix.len_utf8()..]
        } else {
            query
        };
        body.trim().to_string()
    }
}

impl Default for FilesState {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared fan-out queue for [`parallel_walk`]. Directories are pushed
/// as they're discovered by whichever thread found them, and popped
/// by whichever worker thread is next free. `pending` counts
/// directories that have been pushed but not yet fully processed
/// (its own entries listed and any subdirectories it contains
/// re-queued) — a plain "is the queue empty" check can't tell "empty
/// because we're between pops" apart from "empty because the whole
/// tree is done", but `pending == 0` can, since a directory is only
/// marked finished (`finish()`) after any subdirectories it found
/// have already been pushed (and thus already counted).
struct WalkQueue {
    queue: Mutex<VecDeque<PathBuf>>,
    pending: AtomicUsize,
    cv: Condvar,
}

impl WalkQueue {
    fn new(start: PathBuf) -> Self {
        WalkQueue {
            queue: Mutex::new(VecDeque::from([start])),
            pending: AtomicUsize::new(1),
            cv: Condvar::new(),
        }
    }

    fn push(&self, dir: PathBuf) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        self.queue.lock().unwrap().push_back(dir);
        self.cv.notify_one();
    }

    /// Blocks until a directory is available, or returns `None` once
    /// every pushed directory has been fully processed — the signal
    /// every worker thread watches for to exit.
    fn pop(&self) -> Option<PathBuf> {
        let mut q = self.queue.lock().unwrap();
        loop {
            if let Some(dir) = q.pop_front() {
                return Some(dir);
            }
            if self.pending.load(Ordering::SeqCst) == 0 {
                return None;
            }
            q = self.cv.wait(q).unwrap();
        }
    }

    /// Mark one previously-popped directory as fully processed
    /// (including having pushed any of its subdirectories first).
    fn finish(&self) {
        if self.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            // `pending` just reached 0 — wake every thread blocked in
            // `pop()` so they can observe it and exit.
            self.cv.notify_all();
        }
    }
}

/// Walk every entry directly inside `dir` (not recursively — that's
/// [`parallel_walk`]'s job), skipping hidden entries and
/// `ignore.contains(...)` matches, and queuing any subdirectories
/// found onto `queue` for a (possibly different) worker thread to
/// pick up. Permission errors are silently swallowed so a single
/// unreadable directory doesn't abort the walk. Returns this
/// directory's own rows — the caller streams/collects them as its
/// own chunk rather than this function appending to some shared
/// list, which is what lets `parallel_walk` report a chunk per
/// directory as soon as it's ready.
fn walk_one_dir(
    root: &Path,
    dir: &Path,
    ignore: &IgnoreSet,
    next_id: &AtomicI64,
    queue: &WalkQueue,
) -> Vec<HistoryRow> {
    let mut rows = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return rows,
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
        // Shared across every worker thread so ids stay unique
        // regardless of which thread processes which directory —
        // see `parallel_walk`.
        let id = next_id.fetch_sub(1, Ordering::Relaxed);
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
        // The row's `timestamp` is the file's modification time (not
        // "when this row was created" — every row is created at walk
        // time). This is both the value shown in the list's age/time
        // column (`render_row` reads `row.timestamp` uniformly across
        // every mode) and the sort key
        // `sort_rows_newest_modified_first` uses to show
        // recently-modified files first.
        let timestamp = meta
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
            timestamp,
            comment,
            output: String::new(),
            mode: mode.to_string(),
            source: String::new(),
            ..Default::default()
        });
        // Queue subdirectories for a (possibly different) worker
        // thread instead of recursing in-place — this is what turns
        // the walk from a single-threaded depth-first recursion into
        // a fanned-out breadth-of-work queue. The walk is
        // pattern-agnostic now, so there's no "ancestor didn't match"
        // case to worry about (that concern only existed when
        // filtering happened during the walk).
        if is_dir {
            queue.push(path);
        }
    }
    rows
}

/// Walk `start` (and everything beneath it, subject to the
/// hidden-entry / `ignore` skips) using a small pool of worker
/// threads that fan out across subdirectories via [`WalkQueue`].
/// `on_chunk` is invoked once per directory processed — with that
/// directory's own (non-recursive) rows — from whichever worker
/// thread just finished it; each thread gets its own `Clone` of
/// `on_chunk` (see [`spawn_walk`], whose callback closes over an
/// `mpsc::Sender`, itself `Clone`, rather than something shared that
/// would need to be `Sync`). Blocks until the whole tree has been
/// walked. Capped at 8 threads: real directory trees have far more
/// directories than that to keep everyone busy, and capping avoids
/// oversubscribing on very-many-core machines for no benefit (this
/// is I/O-bound `read_dir`/`stat` work, not CPU-bound).
fn parallel_walk<F>(root: &Path, start: PathBuf, ignore: &IgnoreSet, on_chunk: F)
where
    F: Fn(Vec<HistoryRow>) + Send + Clone,
{
    let queue = WalkQueue::new(start);
    let next_id = AtomicI64::new(-1);
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let queue = &queue;
            let next_id = &next_id;
            let on_chunk = on_chunk.clone();
            scope.spawn(move || {
                while let Some(dir) = queue.pop() {
                    let rows = walk_one_dir(root, &dir, ignore, next_id, queue);
                    if !rows.is_empty() {
                        on_chunk(rows);
                    }
                    queue.finish();
                }
            });
        }
    });
}

/// Walk a directory tree synchronously, collecting every row into
/// `rows`. Thin blocking wrapper over [`parallel_walk`] for callers
/// that want the whole tree at once (the files-mode health check, and
/// this module's own tests) — interactive callers that want results
/// as they arrive should use [`spawn_walk`] instead, which streams
/// each directory's rows over a channel as soon as it's processed
/// rather than waiting for everything.
///
/// `next_id` is unused: ids are now assigned by an internal counter
/// shared across the worker threads (a single caller-owned `&mut i64`
/// can't be threaded safely). Kept as a parameter so every existing
/// call site keeps working unchanged; nothing reads it back across
/// calls today.
pub fn walk_dir(
    root: &Path,
    dir: &Path,
    ignore: &IgnoreSet,
    _next_id: &mut i64,
    rows: &mut Vec<HistoryRow>,
) {
    let collected = Mutex::new(Vec::new());
    parallel_walk(root, dir.to_path_buf(), ignore, |chunk| {
        collected.lock().unwrap().extend(chunk);
    });
    rows.extend(collected.into_inner().unwrap());
}

/// The two filtering strategies [`filter_rows`] supports, applied
/// in memory against the cached full-tree walk (`FilesState::all_rows`)
/// — NOT during `walk_dir` itself, which is pattern-agnostic (see the
/// module-level doc comment). `Substring` is the original, default `/`
/// mode behavior (AND of case-insensitive substring tokens against the
/// display path); `Glob` is used by the `--glob-complete` picker
/// (`crate::tui::mode::files::fetch`) — a full-match regex (built by
/// `glob_to_regex`) against the entry's basename, AND (if any)
/// case-insensitive substring tokens against the display path relative
/// to the glob's own root-scoping prefix. The picker's typed body is
/// split on whitespace: the FIRST word is always the glob (root-scoped
/// via `split_glob_root`), every word after it narrows further as a
/// plain substring — e.g. `*.md jira` matches every markdown file
/// whose relative path contains "jira".
pub enum FilesFilter<'a> {
    Substring(&'a [String]),
    Glob {
        basename: &'a regex::Regex,
        extra_tokens: &'a [String],
    },
}

/// True iff every token in `tokens` is a case-insensitive substring
/// of `display`. Empty `tokens` always matches — used by both
/// `FilesFilter::Substring` (the whole filter) and
/// `FilesFilter::Glob`'s `extra_tokens` (narrowing on top of the
/// basename regex match).
fn matches_all_tokens(display: &str, tokens: &[String]) -> bool {
    tokens
        .iter()
        .all(|tok| display.to_lowercase().contains(tok))
}

/// Filter `all_rows` (the cached, full-tree `walk_dir` result) by
/// `filter` — the fast, in-memory counterpart to the (expensive,
/// I/O-bound) walk. Called on every keystroke against the cached
/// tree instead of re-walking the filesystem each time (see the
/// module-level doc comment).
///
/// `root_suffix` is `FilesFilter::Glob`'s root-scoping prefix (from
/// `split_glob_root`, e.g. `"foo/bar"` for a typed `foo/bar/a*`) —
/// rows outside it are excluded, and the surviving rows' `command`
/// is rewritten relative to it (e.g. `banana.txt` instead of
/// `foo/bar/banana.txt`), matching how the picker displayed results
/// when `walk_dir` itself used to be scoped to that narrower root.
/// Always empty for `FilesFilter::Substring` (plain `/` mode has no
/// root-scoping concept), in which case this is a no-op passthrough.
pub fn filter_rows(all_rows: &[HistoryRow], root_suffix: &str, filter: &FilesFilter) -> Vec<HistoryRow> {
    let prefix = if root_suffix.is_empty() { None } else { Some(format!("{root_suffix}/")) };
    all_rows
        .iter()
        .filter_map(|r| {
            let trimmed = match &prefix {
                Some(p) => r.command.strip_prefix(p.as_str())?,
                None => r.command.as_str(),
            };
            let matches = match filter {
                FilesFilter::Substring(tokens) => matches_all_tokens(trimmed, tokens),
                FilesFilter::Glob { basename, extra_tokens } => {
                    let name = Path::new(&r.command).file_name().unwrap_or_default().to_string_lossy();
                    basename.is_match(&name) && matches_all_tokens(trimmed, extra_tokens)
                }
            };
            if !matches {
                return None;
            }
            let mut row = r.clone();
            row.command = trimmed.to_string();
            Some(row)
        })
        .collect()
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
/// filters them out of what's actually shown, and does so BEFORE
/// this sort + its own `truncate(1000)` run (on the per-keystroke
/// filtered set, not the raw walk — see the module-level doc
/// comment), so the 1000-row display cap is spent entirely on
/// files, never diluted by directories that were never going to
/// render anyway. Extracted as a pure function so the ordering can
/// be unit-tested directly.
pub(crate) fn sort_rows_newest_modified_first(rows: &mut [HistoryRow]) {
    rows.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| a.command.cmp(&b.command)));
}

/// Owned counterpart of `FilesFilter`, for building a filter locally
/// out of pieces (a freshly-compiled `Regex`, a freshly-tokenized
/// pattern) before borrowing them into `filter_rows` — `FilesFilter`
/// itself just borrows a token slice or `Regex` reference, so
/// something has to own them first. Used by
/// `crate::tui::mode::files::fetch`, which recomputes the filter
/// fresh on every keystroke (cheap: just a regex compile + a
/// whitespace split, not a filesystem walk).
pub enum FilesFilterSpec {
    Substring(Vec<String>),
    Glob {
        basename: regex::Regex,
        extra_tokens: Vec<String>,
    },
}

/// Spawn a pool of background threads (see [`parallel_walk`]) that
/// walks `root` ONCE, unfiltered, streaming each directory's raw
/// (unsorted, untruncated) rows over `tx` as soon as that directory
/// is processed — not waiting for the whole tree first. Used by
/// `App::spawn_files_walk`, exactly once per TUI session:
/// sorting/truncating/filtering by pattern all happen afterward, per
/// keystroke, against the accumulated result (see
/// `crate::tui::mode::files::fetch`), not here.
///
/// **The walk happens on worker threads, not the main thread**, so
/// the TUI never blocks on filesystem I/O. The outer `std::thread::
/// spawn` here just owns those worker threads via `parallel_walk`'s
/// internal `thread::scope`; once it returns (the walk is fully
/// done) every clone of `tx` handed to a worker has been dropped, so
/// the channel disconnects — that's the "walk complete" signal the
/// receiver's `try_recv()` reports, no separate "done" message
/// needed.
pub fn spawn_walk(root: PathBuf, ignore: IgnoreSet) -> FilesRequest {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        parallel_walk(&root, root.clone(), &ignore, move |chunk| {
            // The walker is infallible: permission errors and
            // missing directories are swallowed at the `read_dir`
            // boundary. A `send` failure just means the receiver
            // (the TUI) was dropped — nothing to do about that.
            let _ = tx.send(chunk);
        });
    });
    FilesRequest { receiver: rx }
}

#[cfg(test)]
mod tests;
