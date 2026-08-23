//! `;` (worktree) prefix mode.
//!
//! Lists every `git worktree` checkout for the repo containing the
//! current directory (`git worktree list --porcelain`), filtered by
//! the typed body the same way every other mode filters its rows.
//! Selecting a row reuses the exact same staging `#` Directories mode
//! uses for its "unmarked row" path (`App::stage_directory_selection`)
//! — rows are tagged `mode == "directory"`, the same tag Directories
//! and Zoxide mode rows carry, so that staging function (and the
//! `T`-marker render logic) work unchanged without needing to know
//! which mode produced the row.
//!
//! Phase 1 (this module, as it stands) is read-only: list + select to
//! `cd`. Creating (`Action::CreateWorktree`) and disposing
//! (`Action::DisposeWorktree`) worktrees are later phases — see
//! the plan history for the full design.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

/// True if the user typed the `worktree` prefix (default `;`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.worktree;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The worktree-search body, i.e. everything after the leading `;`
/// prefix. Empty when not in worktree mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.worktree;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Health check for the worktree (`;`) mode: verifies the current
/// directory is inside a git repo, then (if so) reports how many
/// worktrees `git worktree list` returns.
pub(crate) fn check(_app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Worktree;

    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => return CheckReport::err(mode, format!("couldn't read current directory: {e}")),
    };
    let Some(repo_root) = find_repo_root(&cwd) else {
        return CheckReport::err(mode, format!("{} is not inside a git repository", cwd.display()));
    };
    let base = CheckReport::ok(mode, format!("git repository found at {}", repo_root.display()));
    let list_output = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["worktree", "list", "--porcelain"])
        .output();
    let count_report = match list_output {
        Ok(o) if o.status.success() => {
            let n = parse_worktree_list(&String::from_utf8_lossy(&o.stdout)).len();
            CheckReport::ok(mode, format!("{n} worktree(s) found"))
        }
        Ok(o) => CheckReport::warn(
            mode,
            format!(
                "`git worktree list` exited with status {}",
                o.status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".to_string())
            ),
        ),
        Err(e) => CheckReport::warn(mode, format!("`git worktree list` failed to run: {e}")),
    };
    base.with(count_report)
}

/// Resolve the git repo root containing `dir`, via `git -C <dir>
/// rev-parse --show-toplevel` — the same pattern `GitTimestamps::load`
/// (`src/files.rs`) already uses. Returns `None` when `dir` isn't
/// inside a git repo, `git` isn't on `$PATH`, or the command fails.
fn find_repo_root(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = std::str::from_utf8(&output.stdout).ok()?.trim();
    if root.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(root))
}

/// List every worktree for the repo containing the current directory,
/// filtered by the typed query (space-separated AND-filter over the
/// branch name and path, same contract as every other mode). Returns
/// an empty list (not an error) when the cwd isn't inside a git repo
/// or the `git worktree list` call otherwise fails — the mode
/// degrades to "no rows" with a normal empty-list message, same
/// convention `zoxide::fetch` uses for a missing `zoxide` binary.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    let filter = app.worktree_pattern().trim();
    let filter_tokens: Vec<&str> = filter.split_whitespace().filter(|t| !t.is_empty()).collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    let Some(repo_root) = find_repo_root(&cwd) else {
        return Ok(Vec::new());
    };
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["worktree", "list", "--porcelain"])
        .output();
    let entries = match output {
        Ok(o) if o.status.success() => parse_worktree_list(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    };
    Ok(build_rows(entries, &filter_tokens, &app.home_list))
}

/// A single `git worktree list --porcelain` entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct WorktreeEntry {
    pub(crate) path: String,
    /// `Some(<branch name>)` for a normal worktree (`refs/heads/`
    /// prefix stripped); `None` for a detached-HEAD or bare worktree.
    pub(crate) branch: Option<String>,
    pub(crate) is_bare: bool,
}

/// Parse `git worktree list --porcelain` output into entries. The
/// format is repeated blocks, each starting with a `worktree <path>`
/// line, separated by blank lines:
///
/// ```text
/// worktree /path/to/main
/// HEAD <sha>
/// branch refs/heads/main
///
/// worktree /path/to/linked
/// HEAD <sha>
/// detached
///
/// worktree /path/to/bare.git
/// bare
/// ```
///
/// `pub(crate)` (not private) so tests can exercise it directly
/// against canned output without spawning a real `git` process.
pub(crate) fn parse_worktree_list(output: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(WorktreeEntry { path: path.trim().to_string(), ..Default::default() });
        } else if let Some(entry) = current.as_mut() {
            if let Some(branch_ref) = line.strip_prefix("branch ") {
                let name = branch_ref.trim().strip_prefix("refs/heads/").unwrap_or(branch_ref.trim());
                entry.branch = Some(name.to_string());
            } else if line.trim() == "bare" {
                entry.is_bare = true;
            }
            // "HEAD <sha>" / "detached" / a trailing blank line carry
            // no information this mode's row list needs — a detached
            // worktree just keeps `branch: None`.
        }
    }
    if let Some(entry) = current.take() {
        entries.push(entry);
    }
    entries
}

/// The pure part of `fetch`: given parsed `entries` in `git worktree
/// list`'s own order (main worktree first), filters by
/// `filter_tokens` (case-insensitive substring over the branch name
/// and path, AND-matched) and builds the resulting `HistoryRow` list.
///
/// `pub(crate)` so tests can exercise it directly with synthetic
/// entries, without spawning a real `git` process or touching a real
/// repository — see `fetch`'s doc comment.
pub(crate) fn build_rows(
    entries: Vec<WorktreeEntry>,
    filter_tokens: &[&str],
    home_list: &[String],
) -> Vec<HistoryRow> {
    let total = entries.len() as i64;
    let mut rows: Vec<HistoryRow> = Vec::new();
    for (idx, entry) in entries.into_iter().enumerate() {
        let label = if entry.is_bare {
            "(bare)".to_string()
        } else {
            entry.branch.clone().unwrap_or_else(|| "(detached)".to_string())
        };
        let haystack = format!("{} {}", label, entry.path).to_lowercase();
        if !filter_tokens.is_empty() && !filter_tokens.iter().all(|tok| haystack.contains(&tok.to_lowercase())) {
            continue;
        }
        let short_dir = crate::util::shorten_home_path(&entry.path, home_list).into_owned();
        rows.push(HistoryRow {
            id: -(idx as i64) - 1,
            command: label,
            directory: entry.path,
            session_id: String::new(),
            exit_code: 0,
            timestamp: total - idx as i64,
            comment: short_dir,
            output: String::new(),
            mode: "directory".to_string(),
            source: "worktree".to_string(),
            ..Default::default()
        });
    }
    rows
}

impl App {
    /// True if the user typed the `worktree` prefix (default `;`).
    pub(crate) fn is_worktree_query(&self) -> bool {
        crate::tui::mode::worktree::matches(self)
    }

    /// The worktree-search body, i.e. everything after the leading
    /// `;` prefix. Empty when not in worktree mode.
    pub(crate) fn worktree_pattern(&self) -> &str {
        crate::tui::mode::worktree::pattern(self)
    }
}
