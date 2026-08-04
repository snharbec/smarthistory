//! `~` (zoxide) prefix mode.
//!
//! Lists every directory in the local `zoxide` database (`zoxide
//! query -l`, which zoxide itself returns highest frecency score
//! first), filtered by the typed body the same way every other mode
//! filters its rows. Selecting a row reuses the exact same staging
//! `#` Directories mode uses for its "unmarked row" path
//! (`App::stage_directory_selection`) — creating a new tmux session
//! / herdr workspace rooted there, or jumping to an already-active
//! pane there (`T`-marked) if one exists. Rows are tagged
//! `mode == "directory"`, the same tag Directories mode uses, so
//! that staging function (and the `T`-marker render logic) work
//! unchanged without needing to know which mode produced the row.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

/// True if the user typed the `zoxide` prefix (default `~`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.zoxide;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The zoxide-search body, i.e. everything after the leading `~`
/// prefix. Empty when not in zoxide mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.zoxide;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Health check for the zoxide (`~`) mode: verifies the `zoxide`
/// binary is on `$PATH` and reachable, then (if so) reports how many
/// directories are in its database.
pub(crate) fn check(_app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Zoxide;

    let version_output = std::process::Command::new("zoxide").arg("--version").output();
    match version_output {
        Ok(o) if o.status.success() => {
            let version = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let base = CheckReport::ok(mode, format!("zoxide binary found: {version}"));
            let list_output = std::process::Command::new("zoxide").args(["query", "-l"]).output();
            let count_report = match list_output {
                Ok(lo) if lo.status.success() => {
                    let n = String::from_utf8_lossy(&lo.stdout)
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .count();
                    if n > 0 {
                        CheckReport::ok(mode, format!("zoxide database has {n} directories"))
                    } else {
                        CheckReport::warn(
                            mode,
                            "zoxide database is empty (visit some directories first so zoxide learns them)",
                        )
                    }
                }
                Ok(lo) => CheckReport::warn(
                    mode,
                    format!(
                        "`zoxide query -l` exited with status {}",
                        lo.status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".to_string())
                    ),
                ),
                Err(e) => CheckReport::warn(mode, format!("`zoxide query -l` failed to run: {e}")),
            };
            base.with(count_report)
        }
        Ok(o) => CheckReport::err(
            mode,
            format!(
                "`zoxide --version` exited with status {}",
                o.status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".to_string())
            ),
        ),
        Err(e) => CheckReport::err(
            mode,
            format!("zoxide binary not found on $PATH or not executable: {e}"),
        ),
    }
}

/// List every directory in the local `zoxide` database (`zoxide
/// query -l`), filtered by the typed query (space-separated
/// AND-filter, same contract as every other mode). Entries whose
/// path no longer exists on disk are skipped — zoxide's own database
/// can lag behind directories the user has since deleted.
///
/// zoxide returns its list highest-frecency-score first; that order
/// is preserved by assigning each row a synthetic `timestamp`
/// counting down from the list length, so the list's default
/// (timestamp-DESC) sort matches zoxide's own ranking without
/// needing to re-derive or expose the real score.
///
/// Output shape: reuses `HistoryRow` with `mode = "directory"`, the
/// same tag `#` Directories mode's rows carry, so the row-selection
/// staging (`App::stage_directory_selection`) and the `T`-marker
/// render logic both work unmodified — a zoxide row is, as far as
/// the rest of the TUI is concerned, indistinguishable from a
/// Directories-mode row.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    let filter = app.zoxide_pattern().trim();
    let filter_tokens: Vec<&str> = filter.split_whitespace().filter(|t| !t.is_empty()).collect();
    let paths = query_zoxide_paths();
    Ok(build_rows(paths, &filter_tokens, &app.home_list))
}

/// Run `zoxide query -l` and return its output as a list of paths,
/// one per line, in zoxide's own highest-score-first order. Returns
/// an empty list (not an error) when zoxide isn't installed or the
/// query otherwise fails — the mode degrades to "no rows" with a
/// normal empty-list message instead of an error overlay. Split out
/// from `build_rows` (the pure part: filtering, existence-checking,
/// row-building) so tests can exercise that logic with synthetic
/// paths without spawning the real `zoxide` binary or touching the
/// user's real zoxide database.
fn query_zoxide_paths() -> Vec<String> {
    let output = std::process::Command::new("zoxide").args(["query", "-l"]).output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// The pure part of `fetch`: given `paths` in zoxide's own
/// highest-score-first order, filters out entries that no longer
/// exist on disk or don't match every `filter_token` (case-
/// insensitive substring, AND-matched — same contract as every other
/// mode), and builds the resulting `HistoryRow` list.
///
/// `pub(crate)` (not private) so tests can exercise it directly with
/// synthetic paths, without spawning the real `zoxide` binary or
/// touching the user's real zoxide database — see `fetch`'s doc
/// comment.
pub(crate) fn build_rows(
    paths: Vec<String>,
    filter_tokens: &[&str],
    home_list: &[String],
) -> Vec<HistoryRow> {
    let total = paths.len() as i64;
    let mut rows: Vec<HistoryRow> = Vec::new();
    for (idx, directory) in paths.into_iter().enumerate() {
        if !std::path::Path::new(&directory).is_dir() {
            continue;
        }
        if !filter_tokens.is_empty()
            && !filter_tokens
                .iter()
                .all(|tok| directory.to_lowercase().contains(&tok.to_lowercase()))
        {
            continue;
        }
        let short_dir = crate::util::shorten_home_path(&directory, home_list).into_owned();
        rows.push(HistoryRow {
            id: -(idx as i64) - 1,
            command: short_dir,
            directory,
            session_id: String::new(),
            exit_code: 0,
            timestamp: total - idx as i64,
            comment: String::new(),
            output: String::new(),
            mode: "directory".to_string(),
            source: "zoxide".to_string(),

            ..Default::default()
        });
    }
    rows
}

impl App {
    /// True if the user typed the `zoxide` prefix (default `~`).
    pub(crate) fn is_zoxide_query(&self) -> bool {
        crate::tui::mode::zoxide::matches(self)
    }

    /// The zoxide-search body, i.e. everything after the leading `~`
    /// prefix. Empty when not in zoxide mode.
    pub(crate) fn zoxide_pattern(&self) -> &str {
        crate::tui::mode::zoxide::pattern(self)
    }
}
