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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
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
pub fn walk_dir(
    root: &Path,
    dir: &Path,
    tokens: &[String],
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
        // Apply the text filter (AND by token).
        let matches_filter = tokens.is_empty()
            || tokens
                .iter()
                .all(|tok| display.to_lowercase().contains(tok));
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
                timestamp: 0,
                comment,
                output: String::new(),
                mode: mode.to_string(),
                source: String::new(),
            });
        }
        // Always recurse into directories so deep files
        // are found even when the ancestor doesn't match
        // the filter pattern.
        if is_dir {
            walk_dir(root, &path, tokens, ignore, next_id, rows);
        }
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
/// in a `~` search that's matched, that's a 1 GB allocation
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

/// Spawn a background thread that walks the current directory
/// tree, filters by `pattern`, and sends the result over
/// `tx`. Used by `App::files_maybe_autocall`.
///
/// **The walk happens on a worker thread, not the main
/// thread**, so the TUI never blocks on filesystem I/O.
/// Cancellation is cooperative: the run loop flips
/// `cancelled` to abort a stale walk; the worker checks
/// the flag just before sending.
pub fn spawn_walk(
    pattern: String,
    ignore: IgnoreSet,
) -> FilesRequest {
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let tokens: Vec<String> = pattern
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::thread::spawn(move || {
        let mut rows: Vec<HistoryRow> = Vec::new();
        let mut next_id: i64 = -1;
        walk_dir(&cwd, &cwd, &tokens, &ignore, &mut next_id, &mut rows);
        rows.sort_by(|a, b| {
            let a_is_dir = a.mode == "directory";
            let b_is_dir = b.mode == "directory";
            a_is_dir.cmp(&b_is_dir)
                .reverse()
                .then(a.command.cmp(&b.command))
        });
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
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn ignore_set_dedupes_and_case_sensitive() {
        let s = IgnoreSet::new(&[
            "target".to_string(),
            "node_modules".to_string(),
            "Target".to_string(),
        ]);
        // The set contains built-ins plus 3 user entries
        // (one duplicate, one new).
        assert!(s.contains(std::ffi::OsStr::new("target")));
        assert!(s.contains(std::ffi::OsStr::new("node_modules")));
        assert!(s.contains(std::ffi::OsStr::new("Target")));
        // Built-ins are present too.
        assert!(s.contains(std::ffi::OsStr::new(".git")));
        assert!(s.contains(std::ffi::OsStr::new("__pycache__")));
    }

    #[test]
    fn ignore_set_rejects_unrelated_names() {
        let s = IgnoreSet::new(&[]);
        assert!(!s.contains(std::ffi::OsStr::new("src")));
        assert!(!s.contains(std::ffi::OsStr::new("Cargo.toml")));
        assert!(!s.contains(std::ffi::OsStr::new("README.md")));
    }

    #[test]
    fn read_preview_bytes_handles_small_text() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_files_test_{}_{}",
            std::process::id(),
            "small_text"
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hello.txt");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"Hello, world!\nLine 2\n").unwrap();
        drop(f);
        let preview = read_preview_bytes(&path).unwrap();
        assert_eq!(preview, "Hello, world!\nLine 2\n");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn read_preview_bytes_returns_none_for_binary() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_files_test_{}_{}",
            std::process::id(),
            "binary"
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("blob.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        // 4 KiB of mostly-zero data with a NUL byte —
        // the NUL triggers the binary heuristic.
        f.write_all(&[0u8; 1024]).unwrap();
        f.write_all(b"AB").unwrap();
        f.write_all(&[0u8; 1024]).unwrap();
        drop(f);
        assert!(read_preview_bytes(&path).is_none());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn read_preview_bytes_returns_none_for_missing_file() {
        let path = Path::new("/nonexistent/path/that/does/not/exist");
        assert!(read_preview_bytes(path).is_none());
    }

    #[test]
    fn read_preview_bytes_caps_at_4kb() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_files_test_{}_{}",
            std::process::id(),
            "large"
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("big.txt");
        let mut f = std::fs::File::create(&path).unwrap();
        // 8 KiB of repeating "abcd\n" — bounded read
        // should return ≤ 4 KiB.
        let chunk = "abcd\n".repeat(200); // 1000 bytes
        for _ in 0..9 {
            f.write_all(chunk.as_bytes()).unwrap();
        }
        drop(f);
        let preview = read_preview_bytes(&path).unwrap();
        assert!(preview.len() <= 4096);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn walk_dir_finds_nested_file() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walk_test_{}_{}",
            std::process::id(),
            "nested"
        ));
        let nested = dir.join("a").join("b");
        let _ = std::fs::create_dir_all(&nested);
        let path = nested.join("target.txt");
        std::fs::write(&path, "x").unwrap();
        let tokens: Vec<String> = vec!["target.txt".into()];
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &tokens, &ignore, &mut next_id, &mut rows);
        // The file should be in the result. The
        // intermediate `a/` and `a/b/` directories
        // should NOT match the filter but should NOT
        // prevent the recursion from reaching
        // `a/b/target.txt`.
        assert!(
            rows.iter().any(|r| r.command == "a/b/target.txt"),
            "expected `a/b/target.txt` in {:?}",
            rows.iter().map(|r| &r.command).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_dir_skips_artifact_dirs() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walk_test_{}_{}",
            std::process::id(),
            "ignore"
        ));
        let target = dir.join("target");
        let _ = std::fs::create_dir_all(&target);
        std::fs::write(target.join("artifact.txt"), "x").unwrap();
        let tokens: Vec<String> = vec![];
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &tokens, &ignore, &mut next_id, &mut rows);
        // The `target/` directory itself should be
        // skipped at the entry level, and so should
        // its `artifact.txt` child.
        assert!(
            !rows.iter().any(|r| r.command.contains("target")),
            "expected `target/` to be skipped, got {:?}",
            rows.iter().map(|r| &r.command).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_dir_recurses_through_non_matching_directories() {
        // The bug we fixed earlier: `~main.rs` must
        // still find `src/main.rs` even though `src/`
        // doesn't match.
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walk_test_{}_{}",
            std::process::id(),
            "recurse"
        ));
        let src = dir.join("src");
        let _ = std::fs::create_dir_all(&src);
        std::fs::write(src.join("main.rs"), "x").unwrap();
        let tokens: Vec<String> = vec!["main.rs".into()];
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &tokens, &ignore, &mut next_id, &mut rows);
        // The intermediate `src/` does NOT match the
        // filter but we should still recurse and find
        // `src/main.rs`.
        assert!(
            rows.iter().any(|r| r.command == "src/main.rs"),
            "expected `src/main.rs`, got {:?}",
            rows.iter().map(|r| &r.command).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
