//! `|` (source) prefix mode.
//!
//! Lists the source directories recorded by `smarthistory
//! record-source`. Enter on a git source offers its worktrees when it
//! has more than one, then opens the selection in a tmux session or
//! herdr workspace. Listing rows touches only the database. Everything
//! that can block (`git`, multiplexer calls, stat on a possibly dead
//! mount) runs on a background thread and reports through
//! `App::source_job`.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::multiplexer::MultiplexerBackend;
use crate::sources::{SessionTarget, list_sources, resolve_session_target};
use crate::tui::App;
use crate::tui::mode::worktree::{WorktreeEntry, parse_worktree_list_bytes};
use crate::tui::mode::{CheckReport, ModeKind};
use crate::tui::state::{HistoryRow, SourcePicker, SourcePickerPhase, SourceWorktreeOption};

const GIT_TIMEOUT: Duration = Duration::from_secs(5);
const STAT_TIMEOUT: Duration = Duration::from_secs(3);

/// A finished background step of the source picker.
pub(crate) enum SourceJobResult {
    Worktrees(Result<Vec<WorktreeEntry>, String>),
    /// The shell command that opens the session.
    Opened(Result<String, String>),
}

pub(crate) fn matches(app: &App) -> bool {
    !app.query.is_empty() && app.query.starts_with(app.query_prefixes.source)
}

pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) { &app.query[app.query_prefixes.source.len_utf8()..] } else { "" }
}

pub(crate) fn check(app: &App) -> CheckReport {
    match list_sources(&app.conn) {
        Ok(rows) if rows.is_empty() => CheckReport::warn(
            ModeKind::Source,
            "no source directories recorded yet (cd into a directory with .git or build.xml)",
        ),
        Ok(rows) => {
            CheckReport::ok(ModeKind::Source, format!("{} source directories recorded", rows.len()))
        }
        Err(e) => CheckReport::warn(ModeKind::Source, format!("could not read sources: {e}")),
    }
}

pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    let tokens: Vec<String> =
        pattern(app).split_whitespace().map(str::to_lowercase).collect();
    let sources = list_sources(&app.conn)?;
    let mut rows = Vec::new();
    for (idx, source) in sources.into_iter().enumerate() {
        let marker = if source.is_git { "git" } else { "build" };
        let haystack = format!("{marker} {}", source.path).to_lowercase();
        if !tokens.iter().all(|t| haystack.contains(t)) {
            continue;
        }
        rows.push(HistoryRow {
            id: -(idx as i64) - 1,
            command: crate::util::shorten_home_path(&source.path, &app.home_list).into_owned(),
            directory: source.path,
            timestamp: source.last_visited,
            mode: "directory".to_string(),
            source: marker.to_string(),
            ..Default::default()
        });
    }
    Ok(rows)
}

/// `git worktree list --porcelain` with a deadline. A child stuck on a
/// dead mount is killed and reaped on a detached thread, because even
/// `wait()` can block on uninterruptible I/O.
fn worktrees_for(repo: &str) -> Result<Vec<WorktreeEntry>, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run git: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("could not read git output")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let deadline = Instant::now() + GIT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                std::thread::spawn(move || child.wait());
                return Err(format!("git timed out listing worktrees: {repo}"));
            }
            Err(e) => return Err(format!("could not wait for git: {e}")),
        }
    };
    if !status.success() {
        return Err(format!("git could not list worktrees: {repo}"));
    }
    let bytes = rx
        .recv_timeout(Duration::from_secs(1))
        .map_err(|_| "could not read git output".to_string())?;
    parse_worktree_list_bytes(&bytes).map_err(|()| "a worktree path is not valid UTF-8".to_string())
}

/// `is_dir` with a deadline, for paths that may sit on a dead mount.
fn is_dir_bounded(dir: &str) -> Option<bool> {
    let (tx, rx) = mpsc::channel();
    let path = dir.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(Path::new(&path).is_dir());
    });
    rx.recv_timeout(STAT_TIMEOUT).ok()
}

/// Build the command that opens `dir`, reusing a session already rooted
/// there. An unavailable session list is an error rather than "no
/// sessions": creating blindly could collide with an existing name, and
/// tmux would then switch to that unrelated session.
fn open_command(backend: &dyn MultiplexerBackend, dir: &str) -> Result<String, String> {
    match is_dir_bounded(dir) {
        Some(true) => {}
        Some(false) => return Err(format!("no longer exists: {dir}")),
        None => return Err(format!("not responding: {dir}")),
    }
    let mut sessions = backend
        .list_sessions()
        .ok_or_else(|| format!("could not list {} sessions", backend.name()))?;
    for session in &mut sessions {
        for root in &mut session.roots {
            *root = crate::util::canonicalize_directory(root);
        }
    }
    let canonical = crate::util::canonicalize_directory(dir);
    let base = Path::new(dir).file_name().and_then(|n| n.to_str()).unwrap_or("source");
    let command = match resolve_session_target(base, &canonical, &sessions) {
        SessionTarget::Reuse(id) => backend.focus_session(&id),
        SessionTarget::Create(name) => backend.create_command(Path::new(dir), &name),
    };
    command.ok_or_else(|| format!("could not build a {} command", backend.name()))
}

/// Worktrees that can be opened, labelled by branch. Bare entries have
/// no checkout to switch to.
fn worktree_options(entries: Vec<WorktreeEntry>) -> Vec<SourceWorktreeOption> {
    entries
        .into_iter()
        .filter(|e| !e.is_bare)
        .map(|e| SourceWorktreeOption {
            label: e.branch.unwrap_or_else(|| "(detached)".to_string()),
            path: e.path,
        })
        .collect()
}

impl App {
    pub(crate) fn is_source_query(&self) -> bool {
        matches(self)
    }

    /// Enter on a source row.
    pub(crate) fn stage_source_selection(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let path = row.directory.clone();
        let is_git = row.source == "git";
        self.source_picker = Some(SourcePicker {
            source_path: path.clone(),
            phase: SourcePickerPhase::Loading,
            options: Vec::new(),
            filter: String::new(),
            cursor: 0,
            selected: 0,
            error: None,
        });
        if is_git {
            let (tx, rx) = mpsc::channel();
            self.source_job = Some(rx);
            std::thread::spawn(move || {
                let _ = tx.send(SourceJobResult::Worktrees(worktrees_for(&path)));
            });
        } else {
            self.start_source_open(path);
        }
    }

    /// Resolve and stage the session for `dir` in the background.
    pub(crate) fn start_source_open(&mut self, dir: String) {
        if let Some(picker) = self.source_picker.as_mut() {
            picker.phase = SourcePickerPhase::Opening;
            picker.error = None;
        }
        let (tx, rx) = mpsc::channel();
        self.source_job = Some(rx);
        match self.multiplexer.kind() {
            Some(kind) => {
                std::thread::spawn(move || {
                    let backend = crate::multiplexer::backend_for(kind);
                    let _ = tx.send(SourceJobResult::Opened(open_command(&*backend, &dir)));
                });
            }
            // Test doubles have no `kind` to rebuild on another thread.
            None => {
                let _ = tx.send(SourceJobResult::Opened(open_command(&*self.multiplexer, &dir)));
            }
        }
    }

    /// Drain a finished background step. Returns true when a session
    /// command was staged and the TUI should exit to run it.
    pub(crate) fn process_source_job(&mut self) -> bool {
        let Some(rx) = self.source_job.as_ref() else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.source_job = None;
                self.fail_source_picker("the background task stopped unexpectedly".to_string());
                return false;
            }
        };
        self.source_job = None;
        match result {
            SourceJobResult::Worktrees(Ok(entries)) => {
                let mut options = worktree_options(entries);
                match options.len() {
                    0 => self.fail_source_picker("no checked-out worktrees found".to_string()),
                    1 => self.start_source_open(options.remove(0).path),
                    _ => {
                        if let Some(picker) = self.source_picker.as_mut() {
                            picker.options = options;
                            picker.phase = SourcePickerPhase::Pick;
                        }
                    }
                }
                false
            }
            SourceJobResult::Worktrees(Err(e)) | SourceJobResult::Opened(Err(e)) => {
                self.fail_source_picker(e);
                false
            }
            SourceJobResult::Opened(Ok(command)) => {
                self.source_picker = None;
                self.selection = Some(command);
                self.pick_mode = Some(crate::tui::PickMode::Run);
                true
            }
        }
    }

    /// Show `error` in the overlay and stop any spinner state, leaving
    /// the worktree list usable if there is one.
    fn fail_source_picker(&mut self, error: String) {
        if let Some(picker) = self.source_picker.as_mut() {
            picker.phase = SourcePickerPhase::Pick;
            picker.error = Some(error);
        }
    }

    /// Enter inside the worktree picker.
    pub(crate) fn confirm_source_picker(&mut self) {
        let Some(picker) = self.source_picker.as_ref() else {
            return;
        };
        if picker.phase != SourcePickerPhase::Pick {
            return;
        }
        let filtered = crate::tui::state::source_picker_filtered(picker);
        let Some(option) = filtered.get(picker.selected.min(filtered.len().saturating_sub(1)))
        else {
            return;
        };
        let path = option.path.clone();
        self.start_source_open(path);
    }

    /// Remove a source from the list after confirmation.
    pub(crate) fn delete_source_row(&mut self, path: &str) -> Result<()> {
        crate::sources::delete_source(&self.conn, path)?;
        self.refresh();
        self.refresh_labeled();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, branch: Option<&str>, is_bare: bool) -> WorktreeEntry {
        WorktreeEntry { path: path.to_string(), branch: branch.map(str::to_string), is_bare }
    }

    #[test]
    fn worktree_options_drop_bare_and_label_detached() {
        let options = worktree_options(vec![
            entry("/repo.git", None, true),
            entry("/wt/main", Some("main"), false),
            entry("/wt/fix", None, false),
        ]);
        assert_eq!(
            options,
            vec![
                SourceWorktreeOption { path: "/wt/main".to_string(), label: "main".to_string() },
                SourceWorktreeOption { path: "/wt/fix".to_string(), label: "(detached)".to_string() },
            ]
        );
    }
}
