//! The `smarthistory daemon` file-watching loop.
//!
//! Watches configured project directories for file changes and records
//! them as `file_events` rows (created/modified/deleted), attributed to
//! the project the file lives in — the automatic counterpart to the
//! editor-hook `file` command. Both feed the same `project report`
//! "Files viewed/modified/created" sections.
//!
//! Everything is configurable via `daemon.*` keys in the config file
//! (see `docs/daemon.md`):
//!
//! - `daemon.watch=<dir> <dir> ...` — which directories to watch
//!   (default: every `project.<slug>.dir` entry).
//! - `daemon.ignore-dirs=<name> <name> ...` — directory basenames to
//!   skip (reuses `files.ignore` + the built-in `DEFAULT_IGNORES`).
//! - `daemon.ignore-files=<glob> <glob> ...` — file globs to skip
//!   (matched against the basename; `*` and `?` supported).
//! - `daemon.events=created,modified,deleted` — which event kinds to
//!   record.
//! - `daemon.debounce-ms=<N>` — the debounce window that coalesces the
//!   burst of events from a single editor save into one event.
//! - `daemon.merge-window-ms=<N>` — how long a `deleted` event waits
//!   for a matching `created` event at the same path before it's
//!   recorded as a real deletion; a match within the window merges
//!   into a single `modified` event instead (see
//!   [`PendingDeletes`] — this is what covers editors, notably vim's
//!   default save strategy, that save by renaming the original file
//!   away and writing a new one at the same path).
//! - `daemon.enabled=on|off` — kill switch (default on; running the
//!   command is the opt-in).

use crate::Config;
use anyhow::Context;
use notify::{DebouncedEvent, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

/// Run the watch loop until interrupted (or, in `--once` mode, until
/// the current event burst has drained). `cli_watch` overrides the
/// configured watch roots when non-empty.
pub fn run(
    cfg: &Config,
    conn: &Connection,
    cli_watch: &[String],
    once: bool,
) -> anyhow::Result<()> {
    // A long-running daemon connection contends for SQLite's single
    // writer lock against every short-lived CLI invocation (the zsh
    // preexec/precmd hooks each open their own connection). Without a
    // busy timeout, a collision fails immediately with SQLITE_BUSY,
    // which the `?` on the INSERT below would otherwise turn into a
    // fatal exit of the whole watch loop.
    conn.busy_timeout(Duration::from_secs(5))
        .context("failed to set the database busy timeout")?;

    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(tx, Duration::from_millis(cfg.daemon_debounce_ms()))
        .context("failed to create the file watcher")?;

    let roots = if cli_watch.is_empty() {
        cfg.daemon_watch_roots()
    } else {
        cli_watch.to_vec()
    };
    if roots.is_empty() {
        eprintln!(
            "smarthistory daemon: no directories to watch — set `daemon.watch` or \
             `project.<slug>.dir` in the config, or pass `--watch <dir>`"
        );
        return Ok(());
    }
    // A single unwatchable root (deleted/renamed/unmounted project
    // directory) shouldn't prevent watching every other, perfectly
    // valid root — warn and skip it instead of aborting the whole
    // command.
    let mut watched_count = 0usize;
    for root in &roots {
        match watcher.watch(std::path::Path::new(root), RecursiveMode::Recursive) {
            Ok(()) => {
                eprintln!("smarthistory daemon: watching {}", root);
                watched_count += 1;
            }
            Err(e) => {
                eprintln!("smarthistory daemon: failed to watch {}: {}", root, e);
            }
        }
    }
    if watched_count == 0 {
        anyhow::bail!("no configured directory could be watched");
    }

    let dir_ignores = crate::files::IgnoreSet::new(cfg.daemon_ignore_dirs());
    let file_ignores = FileIgnoreSet::new(cfg.daemon_ignore_files());
    let merge_window = Duration::from_millis(cfg.daemon_merge_window_ms());
    let mut pending_deletes = PendingDeletes::new();

    // `--once`: drain the current burst then exit, so a cron-style
    // poll can't hang forever.
    let drain_deadline = if once {
        Some(Instant::now() + once_drain_window(cfg.daemon_debounce_ms()))
    } else {
        None
    };

    loop {
        // Wake up at whichever comes first: the overall `--once`
        // deadline, or the earliest pending `deleted` event's merge
        // window expiring (so a real deletion still gets recorded
        // promptly when nothing shows up to merge it with).
        let wake_at = match (drain_deadline, pending_deletes.earliest_deadline(merge_window)) {
            (Some(d), Some(p)) => Some(d.min(p)),
            (Some(d), None) => Some(d),
            (None, Some(p)) => Some(p),
            (None, None) => None,
        };

        let event = match wake_at {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    None
                } else {
                    match rx.recv_timeout(remaining) {
                        Ok(e) => Some(e),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            flush_all_pending(conn, cfg, &dir_ignores, &file_ignores, &mut pending_deletes);
                            break;
                        }
                    }
                }
            }
            None => match rx.recv() {
                Ok(e) => Some(e),
                Err(_) => {
                    flush_all_pending(conn, cfg, &dir_ignores, &file_ignores, &mut pending_deletes);
                    break;
                }
            },
        };

        let Some(event) = event else {
            // A `recv_timeout` deadline fired. Flush every pending
            // delete whose own window has expired (a real deletion,
            // nothing showed up to merge it with).
            for path in pending_deletes.take_expired(merge_window) {
                record_event(conn, cfg, &dir_ignores, &file_ignores, &path, "deleted");
            }
            // If the overall `--once` deadline is what fired (not
            // just an earlier pending-delete expiry), we're done:
            // flush anything still pending and exit.
            if drain_deadline.is_some_and(|d| Instant::now() >= d) {
                flush_all_pending(conn, cfg, &dir_ignores, &file_ignores, &mut pending_deletes);
                break;
            }
            continue;
        };

        let (path, raw_kind) = match event {
            DebouncedEvent::Create(p) => (p, "created"),
            DebouncedEvent::Write(p) | DebouncedEvent::Chmod(p) => (p, "modified"),
            DebouncedEvent::Remove(p) => (p, "deleted"),
            // Rename/Rescan/Error/Notice — not a discrete
            // create/modify/delete, so skip.
            _ => continue,
        };

        if raw_kind == "deleted" {
            // Don't record yet — stash it and see whether a
            // create/modify for the same path shows up within
            // `merge_window` (see the module doc comment). The
            // ignore-dir/ignore-file filters still apply up front so
            // an ignored path never occupies a pending slot.
            if !is_under_ignored_dir(&path, &dir_ignores) && !file_ignores.matches(&path) {
                pending_deletes.stash(path);
            }
            continue;
        }

        // A create/modify: if a delete was pending for this exact
        // path, this is the rename-based save completing — merge
        // into one `modified` event instead of two. Otherwise record
        // it as whatever it actually is.
        let kind = if pending_deletes.try_merge(&path) {
            "modified"
        } else {
            raw_kind
        };
        record_event(conn, cfg, &dir_ignores, &file_ignores, &path, kind);
    }
    Ok(())
}

/// Record every still-pending delete as a real deletion (nothing
/// showed up in time to merge it with) — called when the watch loop
/// is about to exit for any reason, so a deletion right before
/// shutdown is never silently dropped just because its merge window
/// hadn't fully elapsed yet.
fn flush_all_pending(
    conn: &Connection,
    cfg: &Config,
    dir_ignores: &crate::files::IgnoreSet,
    file_ignores: &FileIgnoreSet,
    pending: &mut PendingDeletes,
) {
    for path in pending.take_all() {
        record_event(conn, cfg, dir_ignores, file_ignores, &path, "deleted");
    }
}

/// Resolve and insert one `file_events` row for `path`/`kind`, after
/// the event-kind and ignore-dir/ignore-file filters. Shared by the
/// immediate create/modify path and the two places a pending delete
/// gets flushed.
fn record_event(
    conn: &Connection,
    cfg: &Config,
    dir_ignores: &crate::files::IgnoreSet,
    file_ignores: &FileIgnoreSet,
    path: &Path,
    kind: &str,
) {
    if !cfg.daemon_events_enabled(kind) {
        return;
    }
    if is_under_ignored_dir(path, dir_ignores) {
        return;
    }
    if file_ignores.matches(path) {
        return;
    }

    // Same attribution the `file` command uses, resolved from the
    // FILE's own directory (not the watcher's cwd).
    let (canonical, dir) = canonicalize_event_path(path);
    let slug = match crate::resolve_current_project(conn, cfg, &dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("smarthistory daemon: failed to resolve project for {dir}: {e}");
            return;
        }
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // A single failed write (e.g. the busy timeout above was still
    // exceeded under heavy concurrent load) must not kill a daemon
    // meant to run for days — log and keep watching rather than
    // propagating with `?`.
    if let Err(e) = conn.execute(
        "INSERT INTO file_events (path, event_kind, project_slug, timestamp) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![canonical, kind, slug, now],
    ) {
        eprintln!("smarthistory daemon: failed to record {kind} event for {canonical}: {e}");
    }
}

/// Paths with a `deleted` event awaiting a possible matching
/// `created`/`modified` event at the same path, keyed by the raw
/// (pre-canonicalization) watcher path — a create/modify for the
/// exact same watched path notify already reported the delete for
/// shares that same raw path, so no canonicalization is needed to
/// match them up.
struct PendingDeletes {
    stashed: HashMap<PathBuf, Instant>,
}

impl PendingDeletes {
    fn new() -> Self {
        Self {
            stashed: HashMap::new(),
        }
    }

    /// Record (or refresh) a pending delete for `path`.
    fn stash(&mut self, path: PathBuf) {
        self.stashed.insert(path, Instant::now());
    }

    /// If `path` has a pending delete, consume it (so it's never
    /// flushed as a real deletion) and report the merge.
    fn try_merge(&mut self, path: &Path) -> bool {
        self.stashed.remove(path).is_some()
    }

    /// The earliest instant any pending delete's merge window
    /// expires, or `None` when nothing is pending.
    fn earliest_deadline(&self, window: Duration) -> Option<Instant> {
        self.stashed.values().map(|&t| t + window).min()
    }

    /// Remove and return every path whose merge window has expired
    /// as of now — these are real deletions, nothing merged with
    /// them in time.
    fn take_expired(&mut self, window: Duration) -> Vec<PathBuf> {
        let now = Instant::now();
        let expired: Vec<PathBuf> = self
            .stashed
            .iter()
            .filter(|&(_, &t)| now >= t + window)
            .map(|(p, _)| p.clone())
            .collect();
        for p in &expired {
            self.stashed.remove(p);
        }
        expired
    }

    /// Remove and return every still-pending path, regardless of
    /// whether its window has expired — used when the watch loop is
    /// exiting and nothing should be silently dropped.
    fn take_all(&mut self) -> Vec<PathBuf> {
        self.stashed.drain().map(|(p, _)| p).collect()
    }
}

/// How long `--once` mode waits for the current event burst to drain
/// before giving up. Must be at least `debounce_ms` — the watcher
/// never emits an event before that much quiet time has passed — plus
/// a fixed margin for the event to actually arrive on the channel and
/// get processed. A hardcoded window shorter than a configured
/// `daemon.debounce-ms` would time out before the watcher ever
/// flushes anything, silently recording zero events on every run.
fn once_drain_window(debounce_ms: u64) -> Duration {
    Duration::from_millis(debounce_ms) + Duration::from_secs(2)
}

/// Resolve a raw watcher event path to `(canonical_path,
/// canonical_parent_dir)`. Canonicalizes the PARENT directory rather
/// than the full path: a `deleted` event's path no longer exists on
/// disk, so canonicalizing it directly would fail (falling back to
/// the raw, possibly symlinked path) while `created`/`modified`
/// events for the same file — which still exist — succeed and
/// resolve through the symlink, splitting one file's history across
/// two different path strings. The parent directory still exists in
/// all three cases (only the file itself is gone on delete), so
/// canonicalizing it keeps every event kind consistent.
fn canonicalize_event_path(path: &Path) -> (String, String) {
    let basename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let raw_dir = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = crate::util::canonicalize_directory(&raw_dir);
    let canonical = if dir.is_empty() {
        path.to_string_lossy().into_owned()
    } else {
        format!("{dir}/{basename}")
    };
    (canonical, dir)
}

/// True when any ancestor directory of `path` (including the file's
/// own basename) is in the ignore set — so an event under
/// `target/`, `.git/`, `node_modules/`, etc. is dropped before it
/// ever touches the database.
fn is_under_ignored_dir(path: &Path, ignores: &crate::files::IgnoreSet) -> bool {
    path.ancestors()
        .any(|a| a.file_name().map(|n| ignores.contains(n)).unwrap_or(false))
}

/// A compiled set of file globs to skip, matched against the event
/// path's basename. Supports `*` (any sequence) and `?` (one char).
struct FileIgnoreSet {
    patterns: Vec<String>,
}

impl FileIgnoreSet {
    fn new(patterns: &[String]) -> Self {
        Self {
            patterns: patterns.to_vec(),
        }
    }

    fn matches(&self, path: &Path) -> bool {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        self.patterns.iter().any(|p| glob_match(p, &name))
    }
}

/// Simple glob matcher supporting `*` and `?`, used for
/// `daemon.ignore-files`. Kept dependency-free (no `glob` crate) for
/// the handful of patterns a user is likely to write (`*.tmp`,
/// `*.swp`, `*.log`).
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    fn rec(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => rec(&p[1..], t) || (!t.is_empty() && rec(p, &t[1..])),
            (Some('?'), Some(_)) => rec(&p[1..], &t[1..]),
            (Some(c), Some(tc)) if c == tc => rec(&p[1..], &t[1..]),
            _ => false,
        }
    }
    rec(&p, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn once_drain_window_is_at_least_the_debounce_duration() {
        // A hardcoded window shorter than a configured debounce would
        // always time out before the watcher flushes anything.
        assert!(once_drain_window(3000) > Duration::from_millis(3000));
        assert!(once_drain_window(0) >= Duration::from_secs(2));
    }

    #[test]
    fn once_drain_window_scales_with_debounce_ms() {
        assert!(once_drain_window(5000) > once_drain_window(500));
    }

    #[test]
    fn pending_deletes_merge_cancels_the_pending_delete() {
        let mut pending = PendingDeletes::new();
        let path = PathBuf::from("/tmp/foo.txt");
        pending.stash(path.clone());
        assert!(pending.try_merge(&path), "a stashed delete should merge");
        // Merging consumes it — a second attempt finds nothing left
        // to merge, and it must not be flushed later either.
        assert!(!pending.try_merge(&path));
        assert_eq!(pending.take_all(), Vec::<PathBuf>::new());
    }

    #[test]
    fn pending_deletes_merge_only_matches_the_same_path() {
        let mut pending = PendingDeletes::new();
        pending.stash(PathBuf::from("/tmp/foo.txt"));
        assert!(
            !pending.try_merge(Path::new("/tmp/bar.txt")),
            "a delete for one path must not merge with a create for a different path"
        );
    }

    #[test]
    fn pending_deletes_take_expired_only_returns_paths_past_the_window() {
        let mut pending = PendingDeletes::new();
        pending.stash(PathBuf::from("/tmp/foo.txt"));
        // A window far in the future hasn't expired yet.
        assert_eq!(
            pending.take_expired(Duration::from_secs(3600)),
            Vec::<PathBuf>::new()
        );
        // A zero-length window is already expired.
        assert_eq!(
            pending.take_expired(Duration::from_secs(0)),
            vec![PathBuf::from("/tmp/foo.txt")]
        );
        // take_expired removes what it returns.
        assert_eq!(
            pending.take_expired(Duration::from_secs(0)),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn pending_deletes_take_all_ignores_the_window_entirely() {
        let mut pending = PendingDeletes::new();
        pending.stash(PathBuf::from("/tmp/foo.txt"));
        pending.stash(PathBuf::from("/tmp/bar.txt"));
        let mut all = pending.take_all();
        all.sort();
        assert_eq!(
            all,
            vec![PathBuf::from("/tmp/bar.txt"), PathBuf::from("/tmp/foo.txt")]
        );
        assert_eq!(pending.take_all(), Vec::<PathBuf>::new());
    }

    #[test]
    fn pending_deletes_earliest_deadline_is_none_when_empty() {
        let pending = PendingDeletes::new();
        assert_eq!(pending.earliest_deadline(Duration::from_secs(1)), None);
    }

    #[test]
    fn canonicalize_event_path_created_and_deleted_agree_for_same_file() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory-daemon-test-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("foo.txt");
        std::fs::write(&file, "hello").unwrap();

        // "created"/"modified": the file still exists on disk.
        let (canonical_created, parent_created) = canonicalize_event_path(&file);

        // "deleted": the file no longer exists, but its parent
        // directory still does.
        std::fs::remove_file(&file).unwrap();
        let (canonical_deleted, parent_deleted) = canonicalize_event_path(&file);

        assert_eq!(
            canonical_created, canonical_deleted,
            "a deleted event must resolve to the same path a created/modified \
             event for the same file would have"
        );
        assert_eq!(parent_created, parent_deleted);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn canonicalize_event_path_falls_back_to_raw_path_when_parent_is_missing_from_disk() {
        let missing = Path::new("/definitely/does/not/exist/anywhere/file.txt");
        let (canonical, dir) = canonicalize_event_path(missing);
        assert_eq!(canonical, missing.to_string_lossy());
        assert_eq!(dir, "/definitely/does/not/exist/anywhere");
    }

    #[test]
    fn canonicalize_event_path_handles_a_bare_filename_with_no_parent() {
        let (canonical, dir) = canonicalize_event_path(Path::new("file.txt"));
        assert_eq!(canonical, "file.txt");
        assert_eq!(dir, "");
    }

    #[test]
    fn glob_match_basic() {
        assert!(glob_match("*.tmp", "foo.tmp"));
        assert!(glob_match("*.tmp", ".tmp"));
        assert!(!glob_match("*.tmp", "foo.txt"));
        assert!(glob_match("*.swp", "file.swp"));
        assert!(glob_match("*.log", "app.log"));
        assert!(!glob_match("*.log", "app.log.1"));
    }

    #[test]
    fn glob_match_question() {
        assert!(glob_match("file?.rs", "file1.rs"));
        assert!(!glob_match("file?.rs", "file12.rs"));
        assert!(glob_match("?.txt", "a.txt"));
    }

    #[test]
    fn glob_match_exact() {
        assert!(glob_match("Cargo.lock", "Cargo.lock"));
        assert!(!glob_match("Cargo.lock", "Cargo.toml"));
    }

    #[test]
    fn glob_match_star_anywhere() {
        assert!(glob_match("a*b", "ab"));
        assert!(glob_match("a*b", "aXb"));
        assert!(glob_match("a*b", "aXYb"));
        assert!(!glob_match("a*b", "ac"));
    }
}
