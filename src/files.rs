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
//! - **No parallelism:** the one-shot walk is still single-threaded.
//!   A parallel walker (via the `ignore` or `walkdir` crate) would
//!   shave more time off that one walk on very large trees, but
//!   isn't needed to fix the "typing is slow" problem, since that
//!   was dominated by walking repeatedly, not by any one walk being
//!   slow in isolation.

use crate::tui::state::HistoryRow;
use crate::util::format_size;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

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

/// The one-shot background walk. The background thread sends the
/// full (unfiltered) tree over `receiver`; the run loop polls it.
/// No cancellation handle: unlike the old per-keystroke design, the
/// walk isn't tied to any particular pattern, so a later keystroke
/// never makes an in-flight walk stale.
pub struct FilesRequest {
    pub receiver: mpsc::Receiver<Vec<HistoryRow>>,
}

/// Aggregated files-mode state. The TUI holds one of these and
/// reads it from the run loop's idle tick to decide whether to
/// spawn the one-shot background walk.
pub struct FilesState {
    /// The full, unfiltered directory-tree walk result — every
    /// file and directory the walk found under the session's
    /// walk root (`files_root`, or `file_picker_lock.base_root`
    /// for a locked picker). `None` until the one-shot background
    /// walk completes (see `App::files_touch`); populated exactly
    /// once per TUI session. Every keystroke after that filters
    /// THIS list in memory (`crate::tui::mode::files::fetch` →
    /// `filter_rows`) instead of re-walking the filesystem.
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

/// Map of relative file paths to their last Git commit timestamp.
pub struct GitTimestamps {
    pub repo_root: PathBuf,
    pub timestamps: HashMap<PathBuf, i64>,
}

impl GitTimestamps {
    /// Attempt to load Git commit timestamps for tracked files under `root`.
    /// Returns `None` if `root` is not in a Git repo, `git` isn't available,
    /// or `git log` fails.
    pub fn load(root: &Path) -> Option<Self> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let repo_root_str = std::str::from_utf8(&output.stdout).ok()?.trim();
        if repo_root_str.is_empty() {
            return None;
        }
        let repo_root = PathBuf::from(repo_root_str);

        let log_output = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo_root)
            .args(["log", "--name-only", "--no-renames", "--format=COMMIT:%ct"])
            .output()
            .ok()?;

        if !log_output.status.success() {
            return None;
        }

        let log_str = std::str::from_utf8(&log_output.stdout).ok()?;
        let mut timestamps: HashMap<PathBuf, i64> = HashMap::new();
        let mut current_ts: Option<i64> = None;

        for line in log_str.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(ts_str) = line.strip_prefix("COMMIT:") {
                current_ts = ts_str.parse::<i64>().ok();
            } else if let Some(ts) = current_ts {
                let rel_path = PathBuf::from(line);
                timestamps.entry(rel_path).or_insert(ts);
            }
        }

        Some(GitTimestamps {
            repo_root,
            timestamps,
        })
    }

    /// Look up the Git last modified timestamp for a file given its `path`
    /// (which may be relative to `root`) or `abs_path`.
    pub fn get(&self, path: &Path, abs_path: &Path) -> Option<i64> {
        let rel: PathBuf = path
            .strip_prefix(&self.repo_root)
            .ok()
            .map(PathBuf::from)
            .or_else(|| abs_path.strip_prefix(&self.repo_root).ok().map(PathBuf::from))
            .or_else(|| {
                // `repo_root` comes from `git rev-parse --show-toplevel`,
                // which resolves symlinks. `abs_path` doesn't, so on
                // platforms where the walk root is itself a symlink (e.g.
                // macOS's `std::env::temp_dir()` returning `/var/folders/...`
                // instead of its canonical `/private/var/folders/...`), the
                // two prefix-strips above silently fail. Canonicalize as a
                // last resort before giving up.
                fs::canonicalize(abs_path)
                    .ok()
                    .and_then(|canon| canon.strip_prefix(&self.repo_root).ok().map(PathBuf::from))
            })?;
        self.timestamps.get(&rel).copied()
    }
}

/// Recursively walk a directory, adding every file and directory
/// entry to `rows`. Hidden entries (names starting with `.`) and
/// `ignore.contains(...)` matches are skipped at the entry level.
/// Permission errors are silently swallowed so a single unreadable
/// subdirectory doesn't abort the whole walk.
///
/// `next_id` is a monotonically-decreasing counter used to
/// generate the synthetic row ids (negative integers so they
/// can't collide with the SQLite-allocated positive history
/// ids; same convention as the directories and todo modes).
///
/// **No pattern filtering here.** This walks and collects
/// EVERYTHING (subject only to the hidden-entry / ignore-list
/// skips above) — the user's typed filter is applied afterward, in
/// memory, by [`filter_rows`]. Keeping the walk pattern-agnostic is
/// what lets it run exactly once per session instead of once per
/// keystroke; see the module-level doc comment.
pub fn walk_dir(
    root: &Path,
    dir: &Path,
    ignore: &IgnoreSet,
    next_id: &mut i64,
    rows: &mut Vec<HistoryRow>,
) {
    let git_timestamps = if root == dir {
        GitTimestamps::load(root)
    } else {
        None
    };
    walk_dir_impl(root, dir, ignore, next_id, rows, git_timestamps.as_ref());
}

fn walk_dir_impl(
    root: &Path,
    dir: &Path,
    ignore: &IgnoreSet,
    next_id: &mut i64,
    rows: &mut Vec<HistoryRow>,
    git_timestamps: Option<&GitTimestamps>,
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
        // sort key `sort_rows_newest_modified_first` uses to
        // show recently-modified files first.
        //
        // If the file is tracked in Git, the last Git commit timestamp
        // is used; otherwise falls back to filesystem `mtime`
        // (or `0` on error).
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let timestamp = if let Some(git_ts) = git_timestamps.and_then(|gt| gt.get(&path, Path::new(&abs_path))) {
            git_ts
        } else {
            mtime
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
            timestamp,
            comment,
            output: String::new(),
            mode: mode.to_string(),
            source: String::new(),
            ..Default::default()
        });
        // Always recurse into directories — the walk is
        // pattern-agnostic now, so there's no "ancestor didn't
        // match" case to worry about anymore (that concern only
        // existed when filtering happened during the walk).
        if is_dir {
            walk_dir_impl(root, &path, ignore, next_id, rows, git_timestamps);
        }
    }
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

/// Spawn a background thread that walks `root` ONCE, unfiltered,
/// and sends the raw (unsorted, untruncated) result over `tx`. Used
/// by `App::spawn_files_walk`, exactly once per TUI session:
/// sorting/truncating/filtering by pattern all happen afterward, per
/// keystroke, against the cached result (see
/// `crate::tui::mode::files::fetch`), not here.
///
/// **The walk happens on a worker thread, not the main
/// thread**, so the TUI never blocks on filesystem I/O.
pub fn spawn_walk(root: PathBuf, ignore: IgnoreSet) -> FilesRequest {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut rows: Vec<HistoryRow> = Vec::new();
        let mut next_id: i64 = -1;
        walk_dir(&root, &root, &ignore, &mut next_id, &mut rows);
        // The walker is infallible: permission errors and missing
        // directories are swallowed at the `read_dir` boundary.
        // Errors don't need to flow through the channel. A `send`
        // failure just means the receiver (the TUI) was dropped —
        // nothing to do about that.
        let _ = tx.send(rows);
    });
    FilesRequest { receiver: rx }
}

#[cfg(test)]
mod tests;
