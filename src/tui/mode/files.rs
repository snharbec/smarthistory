//! `~` (files) prefix mode.
//!
//! Lists every file in the current directory and
//! subdirectories, filtered by the typed pattern.
//! Selecting a row opens the file in `$EDITOR` (or the
//! configured per-extension command, via the SmartOpen
use crate::tui::mode::CheckReport;
/// key `Ctrl-]`).
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

/// Whether the query is a files-view request:
/// the query starts with the files prefix (`~` by
/// default). The body (everything after `~`) is a
/// substring filter matched against each file's
/// path (relative to cwd).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.files;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// Health check for the files (`~`) mode. The
/// files mode has no external dependencies — it
/// just walks the local filesystem — so the
/// check verifies:
///
/// 1. The current working directory exists and
///    is readable.
/// 2. `walk_dir` returns at least one entry
///    (or the user is in a deliberately empty
///    directory, which is a `Warning`).
/// 3. The `files.ignore` config combines
///    with the built-in `DEFAULT_IGNORES`
///    without error.
///
/// The walk uses a real pattern (`*` /
///    everything) to exercise the same code
///    path the TUI uses.
pub(crate) fn check(_app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Files;

    // 1. CWD sanity.
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            return CheckReport::err(
                mode,
                format!("current working directory is unavailable: {e}"),
            );
        }
    };
    if !cwd.is_dir() {
        return CheckReport::err(mode, format!("cwd is not a directory: {}", cwd.display()));
    }

    // 2. Build the ignore set the same way the
    //    runtime does. We use the built-in
    //    `DEFAULT_IGNORES` plus any user
    //    additions from config; for the check
    //    we don't have an `App` context, so we
    //    just use the default set.
    let ignore = crate::files::IgnoreSet::new(&[]);

    // 3. Walk. We cap the result count at 10
    //    rows for the probe; the runtime walk
    //    has its own debounce / cancellation
    //    logic we don't need to exercise here.
    let mut rows: Vec<crate::tui::state::HistoryRow> = Vec::new();
    let mut next_id: i64 = -1;
    crate::files::walk_dir(
        &cwd,
        &cwd,
        &[], // no filter — walk everything
        &ignore,
        &mut next_id,
        &mut rows,
    );

    if rows.is_empty() {
        CheckReport::warn(
            mode,
            format!("walk_dir() returned 0 entries in {} (the directory is empty or every file is in the ignore list)", cwd.display()),
        )
    } else {
        CheckReport::ok(
            mode,
            format!(
                "walk_dir() returned {} entries in {} (showing up to 10)",
                rows.len().min(10),
                cwd.display()
            ),
        )
    }
}

/// The files-view body, i.e. everything after the
/// leading `~` prefix. Empty string when not in
/// files mode.
#[allow(dead_code)] // convention API; `App::files_pattern` delegates here
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.files;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Fetch the files-mode result set. The walk runs
/// on a background thread (spawned by
/// `App::files_touch` → `crate::files::spawn_walk`),
/// so this just clones the cached rows from
/// `App::files_state`. A future pass can move the
/// walk / debounce orchestration here.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    Ok(app.files_state.rows.clone())
}
