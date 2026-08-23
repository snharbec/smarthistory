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
///
/// `pub(crate)` (not private) so the `Action::CreateWorktree` flow can
/// resolve the repo root before the dialog even opens.
pub(crate) fn find_repo_root(dir: &std::path::Path) -> Option<std::path::PathBuf> {
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

/// List every local branch of the repo at `repo_root`, in `git
/// for-each-ref`'s own order. Used to populate the `PickBranch` /
/// `PickBaseBranch` steps of the `Action::CreateWorktree` flow.
/// `--format='%(refname:short)'` gives bare branch names with none of
/// `git branch --list`'s `*`/indentation markers to strip.
pub(crate) fn list_branches(repo_root: &std::path::Path) -> Vec<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["for-each-ref", "--format=%(refname:short)", "refs/heads"])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// The branch to preselect as the base for a new worktree branch:
/// the remote `HEAD` symbolic ref when one is set (`origin/main` /
/// `origin/master`, whichever the remote actually points at), falling
/// back to a local `main` or `master` branch, and finally to the
/// repo's own current branch — always something valid to preselect,
/// even in a from-scratch repo with a single branch.
pub(crate) fn default_base_branch(repo_root: &std::path::Path) -> String {
    let symbolic = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output();
    if let Ok(o) = symbolic
        && o.status.success()
    {
        let raw = String::from_utf8_lossy(&o.stdout);
        let trimmed = raw.trim().strip_prefix("refs/remotes/origin/").unwrap_or(raw.trim());
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let branches = list_branches(repo_root);
    for candidate in ["main", "master"] {
        if branches.iter().any(|b| b == candidate) {
            return candidate.to_string();
        }
    }
    let current = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output();
    match current {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// Create a new `git worktree` checkout at `path`. When `is_new_branch`
/// is true, `-b <branch>` creates the branch off `base_branch`;
/// otherwise `branch` must already exist and is simply checked out
/// into the new worktree. `path`'s parent directory is created first
/// so a branch name containing `/` (e.g. `feature/login`) can become a
/// nested worktree directory. Propagates git's stderr on failure —
/// unlike this module's listing functions (which best-effort degrade
/// to empty), a create action must surface real errors to the user.
pub(crate) fn create_worktree(
    repo_root: &std::path::Path,
    path: &std::path::Path,
    branch: &str,
    is_new_branch: bool,
    base_branch: &str,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {}", parent.display(), e))?;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo_root).arg("worktree").arg("add");
    if is_new_branch {
        cmd.arg("-b").arg(branch).arg(path).arg(base_branch);
    } else {
        cmd.arg(path).arg(branch);
    }
    let output = cmd.output().map_err(|e| format!("failed to run git: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// True when `git status --porcelain` on `repo_root` reports any
/// uncommitted changes. Used by the `Action::CreateWorktree` flow to
/// decide whether to ask about carrying changes over into the new
/// worktree. A failed/unparseable `git status` degrades to `false`
/// (skip the prompt) rather than blocking the flow.
pub(crate) fn repo_is_dirty(repo_root: &std::path::Path) -> bool {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["status", "--porcelain"])
        .output();
    matches!(output, Ok(o) if o.status.success() && !o.stdout.is_empty())
}

/// Every project slug already in use, derived the same way
/// `App::stage_project_selection` derives a slug from a selected
/// `type: project` note's filename (`file_stem` +
/// `crate::util::slugify`) — so the `PickProject` step offers exactly
/// the slugs already in use, not a separately invented list. Returns
/// an empty list (not an error) when `notes.database` isn't configured
/// or the query fails, same degrade-to-empty convention `check`/`fetch`
/// use elsewhere in this module.
pub(crate) fn list_project_slugs(app: &App) -> Vec<String> {
    let Some(ref db_path) = app.notes_database else {
        return Vec::new();
    };
    let service = note_search::database_service::DatabaseService::new(&db_path.to_string_lossy());
    let criteria = note_search::SearchCriteria {
        list_only: true,
        query_expr: Some(note_search::QueryExpr::Attribute {
            key: "type".to_string(),
            value: Some("project".to_string()),
        }),
        ..Default::default()
    };
    let Ok(rows) = service.search_notes(&criteria) else {
        return Vec::new();
    };
    rows.iter()
        .map(|note| {
            let stem = std::path::Path::new(&note.filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&note.filename);
            crate::util::slugify(stem, "project")
        })
        .collect()
}

/// Append a `project.<slug>.dir = "<path>"` line to the main config
/// file, binding future `smarthistory add`/time-tracking directory
/// detection under `path` to `slug`. Modeled on
/// `App::write_new_entry_to_config`'s atomic tmp-file-then-`rename`
/// write, but targets `crate::config_path()` (where `project.*` keys
/// live) instead of the dedicated sessions/hosts files, and just
/// appends one line rather than building a multi-field block.
pub(crate) fn write_project_dir_binding(slug: &str, path: &std::path::Path) -> Result<(), String> {
    let target_path =
        crate::config_path().ok_or_else(|| "no config directory path (HOME is not set)".to_string())?;
    let contents = match std::fs::read_to_string(&target_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("failed to read {}: {}", target_path.display(), e)),
    };
    let mut new_contents = contents.clone();
    if !new_contents.is_empty() && !new_contents.ends_with('\n') {
        new_contents.push('\n');
    }
    new_contents.push_str(&format!("project.{}.dir = {:?}\n", slug, path.display()));
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {}", parent.display(), e))?;
    }
    let tmp_path = target_path.with_extension("tmp");
    std::fs::write(&tmp_path, new_contents.as_bytes())
        .map_err(|e| format!("failed to write {}: {}", tmp_path.display(), e))?;
    std::fs::rename(&tmp_path, &target_path).map_err(|e| {
        format!(
            "failed to rename {} to {}: {}",
            tmp_path.display(),
            target_path.display(),
            e,
        )
    })?;
    Ok(())
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
