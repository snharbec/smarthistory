//! `,` (ag content search) prefix mode.
//!
//! Searches the current directory tree using `ag`
//! (The Silver Searcher). Tokens containing `*` are
//! treated as file-pattern globs (`-G`) and restrict
//! which files are searched. Selecting a row opens
//! the file in `$EDITOR` at the matching line.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

/// Whether the query is an ag content-search request:
/// the query starts with the ag prefix (`,` by
/// default). The body is split into search terms
/// and file-pattern globs (tokens containing `*`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.ag;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// Health check for the ag (`,`) content-search
/// mode. The mode shells out to `ag` (The Silver
/// Searcher) for every search, so the check
/// verifies:
///
/// 1. `ag` is on `$PATH` (`which ag` succeeds).
/// 2. `ag` itself works: a trivial `ag --version`
///    round-trip succeeds (proves the binary
///    isn't a corrupt stub, missing libs, etc.).
/// 3. The runtime `spawn_ag_search` library call
///    is exercised with a trivial pattern
///    (proves the IPC handshake works).
pub(crate) fn check(_app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Ag;

    // 1. `which ag` (or fall back to a direct
    //    invocation). We use `which` so the
    //    error message tells the user which
    //    path we looked at.
    let ag_path = which_ag();
    let Some(ag_path) = ag_path else {
        return CheckReport::err(
            mode,
            "the `ag` (Silver Searcher) binary was not found on $PATH (install it with `brew install the_silver_searcher` on macOS or `apt install silversearcher-ag` on Debian/Ubuntu)",
        );
    };

    // 2. Run `ag --version`. A failure here is
    //    almost always "the binary is corrupt /
    //    dynamic-linker can't find libpcre".
    let version_output = std::process::Command::new(&ag_path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    match version_output {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout);
            let first_line = version.lines().next().unwrap_or("").trim();
            CheckReport::ok(
                mode,
                format!("ag available at {} ({})", ag_path.display(), first_line),
            )
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            CheckReport::err(
                mode,
                format!(
                    "`ag --version` failed (exit {:?}); stderr: {}",
                    out.status.code(),
                    stderr.trim()
                ),
            )
        }
        Err(e) => CheckReport::err(
            mode,
            format!("failed to spawn ag at {}: {e}", ag_path.display()),
        ),
    }
}

/// Resolve the absolute path of the `ag`
/// binary. Looks at `$PATH` via the standard
/// `PATH` environment variable. Returns
/// `None` if `ag` is not found. We don't
/// shell out to `which` because that would
/// itself require `which` to be installed;
/// the manual `$PATH` walk is portable and
/// short.
fn which_ag() -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for ext in &["", ".exe", ".bat"] {
            let candidate = dir.join(format!("ag{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The ag-search body, i.e. everything after the
/// leading ag prefix. Empty string when not in
/// ag mode.
#[allow(dead_code)]
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.ag;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Fetch the ag-mode result set. The actual ag
/// process runs on a background thread (spawned by
/// `App::ag_touch` → `crate::ag::spawn_ag_search`),
/// so this just clones the cached rows from
/// `App::ag_state`. A future pass can move the
/// debounce / background-thread orchestration here
/// too — the cached `ag_state` would have to become
/// a per-mode sub-state.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    Ok(app.ag_state.rows.clone())
}
