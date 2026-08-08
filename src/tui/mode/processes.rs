//! `%` (processes) prefix mode.
//!
//! Lists every running OS process on the machine (macOS + Linux, via
//! the `sysinfo` crate), filtered by the typed pattern. Every process
//! is shown regardless of owner — sending a signal to one the user
//! doesn't own is expected to fail with a permission error, surfaced
//! at signal-send time (`App::send_signal`, `src/tui.rs`) rather than
//! filtered out of the list here.
//!
//! Selecting a row (`Enter`) does NOT stage/run the process name as a
//! shell command — `App::stage_process_signal_prompt` (`src/tui/actions.rs`)
//! opens a confirmation dialog instead (`app.confirm_signal`, see
//! `src/tui.rs`), defaulting to SIGTERM with Tab/Shift-Tab cycling to
//! SIGKILL/SIGHUP/SIGINT before confirming.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// True if the user typed the `processes` prefix (default `%`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.processes;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The processes-mode body, i.e. everything after the leading `%`
/// prefix. Empty when not in processes mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.processes;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Health check for the processes (`%`) mode: refreshes the full
/// process list and reports how many processes were found. A count
/// of 1 or fewer suggests a sandboxed/restricted environment (even a
/// minimal machine has itself plus an init process).
pub(crate) fn check(_app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Processes;

    let sys = System::new_all();
    let n = sys.processes().len();
    if n <= 1 {
        CheckReport::warn(
            mode,
            format!("only {n} process(es) visible — this may be a sandboxed or restricted environment"),
        )
    } else {
        CheckReport::ok(mode, format!("{n} processes visible"))
    }
}

/// List every running process, filtered by the typed pattern
/// (space-separated AND-filter, case-insensitive substring against
/// the process name/cmdline, cwd, and executable path — same
/// contract as every other mode). Deliberately does NOT refresh
/// `environ` here (see `ensure_selected_context`): reading every
/// process's environment on every keystroke would be expensive and
/// would trigger a permission-denied syscall for every unowned
/// process on every refresh, for no benefit until a row is actually
/// selected.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    let filter = pattern(app).trim();
    let filter_tokens: Vec<&str> = filter.split_whitespace().filter(|t| !t.is_empty()).collect();

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cwd(UpdateKind::OnlyIfNotSet)
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet),
    );

    let mut rows: Vec<HistoryRow> = Vec::new();
    for (pid, process) in sys.processes() {
        let cmd: Vec<String> = process
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        let command = if cmd.is_empty() {
            process.name().to_string_lossy().to_string()
        } else {
            cmd.join(" ")
        };
        let directory = process
            .cwd()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let exe_path = process
            .exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default();

        if !filter_tokens.is_empty() {
            let haystack = format!("{command} {directory} {exe_path}").to_lowercase();
            if !filter_tokens
                .iter()
                .all(|tok| haystack.contains(&tok.to_lowercase()))
            {
                continue;
            }
        }

        let pid_num = pid.as_u32() as i64;
        rows.push(HistoryRow {
            id: pid_num,
            command,
            directory,
            session_id: pid_num.to_string(),
            exit_code: 0,
            timestamp: process.start_time() as i64,
            comment: exe_path,
            output: String::new(),
            mode: "process".to_string(),
            ..Default::default()
        });
    }
    Ok(rows)
}

/// The graceful-degradation placeholder shown in `preview` when a
/// selected process's environment can't be read — a permission
/// failure (non-owned/non-child process on macOS or Linux) or the
/// process having already exited between `fetch` and selection.
/// Extracted as a pure function so it's directly unit-testable
/// without needing an actual unreadable process.
pub(crate) fn format_environ_error(pid: i64) -> String {
    format!("(permission denied — cannot read environment for pid {pid})")
}

/// Lazy-load the selected process's environment variables into
/// `preview` (rendered by the docked output-preview pane and the
/// `Ctrl-O` full overlay — both classify `mode == "process"` as
/// preview-only, see `src/tui/render.rs` / `src/tui.rs`). Read-once
/// per row (guarded on `!row.preview.is_empty()`, mirroring
/// `tags::ensure_selected_context` — a process's environment doesn't
/// meaningfully change within a TUI session, so there's no need for
/// panes-mode's TTL re-read). Writes to both `app.rows` and
/// `app.merged_rows` at the same index: `build_merged_rows` gives
/// Processes mode a `self.rows.clone()` early return (see
/// `src/tui.rs`), so the two are index-identical, and writing only
/// to `merged_rows` would be wiped by the next rebuild — same
/// reasoning as `panes::ensure_selected_context`.
pub(crate) fn ensure_selected_context(app: &mut App) {
    if !matches(app) {
        return;
    }
    let Some(idx) = app.list_state.selected() else {
        return;
    };

    let pid = match app.merged_rows.get(idx) {
        Some(r) if r.mode == "process" && r.preview.is_empty() => r.id,
        _ => return,
    };

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(pid as u32)]),
        true,
        ProcessRefreshKind::nothing().with_environ(UpdateKind::Always),
    );

    let preview = match sys.process(sysinfo::Pid::from_u32(pid as u32)) {
        Some(process) => {
            let mut vars: Vec<String> = process
                .environ()
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect();
            if vars.is_empty() {
                format_environ_error(pid)
            } else {
                vars.sort();
                vars.join("\n")
            }
        }
        None => format_environ_error(pid),
    };

    if let Some(row) = app.rows.get_mut(idx) {
        row.preview = preview.clone();
    }
    if let Some(row) = app.merged_rows.get_mut(idx) {
        row.preview = preview;
    }
}

impl App {
    /// True if the user typed the `processes` prefix (default `%`).
    pub(crate) fn is_processes_query(&self) -> bool {
        matches(self)
    }

    /// The processes-mode body, i.e. everything after the leading
    /// `%` prefix. Empty when not in processes mode.
    #[allow(dead_code)] // convention API; mirrors every other mode's `<mode>_pattern` shim
    pub(crate) fn processes_pattern(&self) -> &str {
        pattern(self)
    }

    /// Send `signal` to `pid` via `sysinfo::Process::kill_with`.
    /// Returns `Ok(())` on success, `Err(message)` with a
    /// human-readable failure reason (permission denied, process no
    /// longer exists, or the platform couldn't send that particular
    /// signal) otherwise — the caller surfaces this via
    /// `set_status_message` rather than panicking, exactly like
    /// delete/rename failures already do elsewhere in the TUI.
    pub(crate) fn send_signal(&self, pid: i64, signal: sysinfo::Signal) -> std::result::Result<(), String> {
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(pid as u32)]),
            true,
            ProcessRefreshKind::nothing(),
        );
        let Some(process) = sys.process(sysinfo::Pid::from_u32(pid as u32)) else {
            return Err(format!("pid {pid} no longer exists"));
        };
        match process.kill_with(signal) {
            Some(true) => Ok(()),
            Some(false) => Err(format!("kill(2) failed for pid {pid} (permission denied or the process already exited)")),
            None => Err(format!("signal {signal} is not supported on this platform")),
        }
    }
}
