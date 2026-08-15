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
//! - `daemon.enabled=on|off` — kill switch (default on; running the
//!   command is the opt-in).

use crate::Config;
use anyhow::Context;
use notify::{DebouncedEvent, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use std::path::Path;
use std::sync::mpsc::channel;
use std::time::Duration;

/// Run the watch loop until interrupted (or, in `--once` mode, until
/// the current event burst has drained). `cli_watch` overrides the
/// configured watch roots when non-empty.
pub fn run(
    cfg: &Config,
    conn: &Connection,
    cli_watch: &[String],
    once: bool,
) -> anyhow::Result<()> {
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
    for root in &roots {
        watcher
            .watch(std::path::Path::new(root), RecursiveMode::Recursive)
            .with_context(|| format!("failed to watch {}", root))?;
        eprintln!("smarthistory daemon: watching {}", root);
    }

    let dir_ignores = crate::files::IgnoreSet::new(cfg.daemon_ignore_dirs());
    let file_ignores = FileIgnoreSet::new(cfg.daemon_ignore_files());

    // `--once`: drain the current burst (up to a fixed window) then
    // exit, so a cron-style poll can't hang forever.
    let drain_deadline = if once {
        Some(std::time::Instant::now() + Duration::from_secs(2))
    } else {
        None
    };

    loop {
        let event = match drain_deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match rx.recv_timeout(remaining) {
                    Ok(e) => e,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match rx.recv() {
                Ok(e) => e,
                Err(_) => break,
            },
        };

        let (path, kind) = match event {
            DebouncedEvent::Create(p) => (p, "created"),
            DebouncedEvent::Write(p) | DebouncedEvent::Chmod(p) => (p, "modified"),
            DebouncedEvent::Remove(p) => (p, "deleted"),
            // Rename/Rescan/Error/Notice — not a discrete
            // create/modify/delete, so skip.
            _ => continue,
        };

        if !cfg.daemon_events_enabled(kind) {
            continue;
        }
        if is_under_ignored_dir(&path, &dir_ignores) {
            continue;
        }
        if file_ignores.matches(&path) {
            continue;
        }

        // Same attribution the `file` command uses, resolved from the
        // FILE's own directory (not the watcher's cwd).
        let canonical = crate::util::canonicalize_directory(&path.to_string_lossy());
        let dir = Path::new(&canonical)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let slug = crate::resolve_current_project(conn, cfg, &dir)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        conn.execute(
            "INSERT INTO file_events (path, event_kind, project_slug, timestamp) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![canonical, kind, slug, now],
        )?;
    }
    Ok(())
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
