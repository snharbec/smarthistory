//! Multiplexer state the shell server's background monitor
//! (`crate::mux_monitor`) shares with the rest of smarthistory, plus
//! `smarthistory last-session`.
//!
//! Two small JSON files live next to the shell-server socket, in its
//! private (mode 0700, owner-checked) directory:
//!
//! - [`PANES_FILE`]: the latest tmux / herdr snapshot — every pane,
//!   every active context, and (herdr) each pane's foreground command
//!   line. The TUI's `*` and `#` modes show it instead of waiting on a
//!   live `tmux` / `herdr` round trip, then re-fetch live in the
//!   background (see `crate::tui::mode::panes::backend_pane_rows`).
//! - [`FOCUS_FILE`]: herdr's workspace focus history, most recent
//!   first, recorded from herdr's `workspace_focused` events. herdr has
//!   no "previous workspace" command of its own; tmux does
//!   (`switch-client -l`), so nothing is recorded for tmux.
//!
//! Both are written atomically (temp file + rename) and only ever
//! trusted when the directory passes the same ownership/permission
//! check the socket does.

use crate::multiplexer::{ActiveContext, CurrentPaneInfo, MultiplexerKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The snapshot cache file name.
pub(crate) const PANES_FILE: &str = "mux-panes.json";
/// The herdr focus-history file name.
pub(crate) const FOCUS_FILE: &str = "herdr-focus.json";
/// A cached snapshot older than this is ignored and the caller
/// fetches live. A few poll intervals (default 2s), so one slow poll
/// doesn't bounce the TUI back to a blocking fetch.
pub(crate) const FRESH_MS: u64 = 6_000;
/// How many workspaces the focus history remembers.
const FOCUS_HISTORY_LEN: usize = 32;

/// The cached snapshot, as written by the monitor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PanesCache {
    /// `tmux` / `herdr` ([`MultiplexerKind::as_str`]).
    pub backend: String,
    /// When the snapshot was taken (ms since the Unix epoch).
    pub updated_ms: u64,
    /// `MultiplexerBackend::snapshot` (the `#` mode's T-markers).
    pub contexts: Vec<ActiveContext>,
    /// `MultiplexerBackend::snapshot_all_panes` (the `*` mode).
    pub panes: Vec<CurrentPaneInfo>,
    /// herdr only: pane id → foreground command line.
    #[serde(default)]
    pub cmdlines: HashMap<String, String>,
}

impl PanesCache {
    /// Whether this snapshot is for `kind` and recent enough to use
    /// at `now_ms`. A timestamp from the future (clock jump) counts
    /// as stale.
    pub(crate) fn is_fresh_for(&self, kind: MultiplexerKind, now_ms: u64) -> bool {
        self.backend == kind.as_str()
            && self.updated_ms <= now_ms
            && now_ms - self.updated_ms <= FRESH_MS
    }
}

/// herdr's workspace focus history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FocusHistory {
    /// The herdr socket this history belongs to. Named herdr sessions
    /// each have their own socket and their own workspace ids, so a
    /// history recorded against another socket is ignored.
    pub socket: String,
    /// Workspace ids, most recently focused first, no duplicates.
    pub stack: Vec<String>,
}

impl FocusHistory {
    /// Move `id` to the front. Returns whether anything changed.
    pub(crate) fn record(&mut self, id: &str) -> bool {
        if id.is_empty() || self.stack.first().map(String::as_str) == Some(id) {
            return false;
        }
        self.stack.retain(|s| s != id);
        self.stack.insert(0, id.to_string());
        self.stack.truncate(FOCUS_HISTORY_LEN);
        true
    }

    /// Drop a closed workspace. Returns whether anything changed.
    pub(crate) fn forget(&mut self, id: &str) -> bool {
        let before = self.stack.len();
        self.stack.retain(|s| s != id);
        self.stack.len() != before
    }

    /// The workspace to switch back to: the most recent one that
    /// isn't `current` and still exists. With `current` unknown, the
    /// front entry is taken to be the current one and skipped.
    pub(crate) fn previous(
        &self,
        current: Option<&str>,
        exists: impl Fn(&str) -> bool,
    ) -> Option<String> {
        let skip_front = current.is_none();
        self.stack
            .iter()
            .enumerate()
            .filter(|(i, id)| !(skip_front && *i == 0) && Some(id.as_str()) != current)
            .map(|(_, id)| id)
            .find(|id| exists(id))
            .cloned()
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The shell server's state directory (the socket's parent), if it
/// exists and is safe to trust: owned by us, not accessible by others.
#[cfg(unix)]
pub(crate) fn state_dir() -> Option<PathBuf> {
    let socket = crate::shell_server::socket_path();
    let dir = socket.parent()?.to_path_buf();
    crate::shell_server::check_socket_dir(&dir).ok()?;
    Some(dir)
}

#[cfg(not(unix))]
pub(crate) fn state_dir() -> Option<PathBuf> {
    None
}

/// Write `bytes` to `dir/name` atomically (temp file in the same
/// directory, mode 0600, then rename), so a reader never sees a
/// half-written file.
pub(crate) fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
    }
    std::fs::rename(&tmp, dir.join(name)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

pub(crate) fn write_panes(dir: &Path, cache: &PanesCache) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(cache).map_err(std::io::Error::other)?;
    write_atomic(dir, PANES_FILE, &bytes)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The cached snapshot for `kind`, if the shell server's monitor
/// wrote one recently. `None` means "fetch live".
///
/// Always `None` under `cfg(test)`: tests build `App`s with real
/// backends and must not pick up a cache written by a shell server
/// running on the developer's machine.
pub(crate) fn read_fresh_panes(kind: MultiplexerKind) -> Option<PanesCache> {
    if cfg!(test) {
        return None;
    }
    let cache: PanesCache = read_json(&state_dir()?.join(PANES_FILE))?;
    cache.is_fresh_for(kind, now_ms()).then_some(cache)
}

/// The focus history recorded for herdr `socket` (empty if none, or
/// if it was recorded for a different socket).
pub(crate) fn read_focus(dir: &Path, socket: &str) -> FocusHistory {
    match read_json::<FocusHistory>(&dir.join(FOCUS_FILE)) {
        Some(h) if h.socket == socket => h,
        _ => FocusHistory {
            socket: socket.to_string(),
            stack: Vec::new(),
        },
    }
}

pub(crate) fn write_focus(dir: &Path, history: &FocusHistory) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(history).map_err(std::io::Error::other)?;
    write_atomic(dir, FOCUS_FILE, &bytes)
}

/// Which multiplexer `last-session` acts on: the one this shell is
/// running inside (herdr first, same precedence as
/// `current_session_name`), else the configured one.
fn detect_kind(configured: MultiplexerKind) -> MultiplexerKind {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if set("HERDR_PANE_ID") || set("HERDR_SOCKET_PATH") {
        MultiplexerKind::Herdr
    } else if set("TMUX") {
        MultiplexerKind::Tmux
    } else {
        configured
    }
}

/// `smarthistory last-session [--print]`: switch to the previously
/// active tmux session / herdr workspace. `print` only names it.
pub(crate) fn last_session_cli(configured: MultiplexerKind, print: bool) -> Result<(), String> {
    match detect_kind(configured) {
        MultiplexerKind::Tmux => tmux_last_session(print),
        MultiplexerKind::Herdr => herdr_last_session(print),
    }
}

/// tmux tracks the last session per client itself.
fn tmux_last_session(print: bool) -> Result<(), String> {
    if print {
        let out = std::process::Command::new("tmux")
            .args(["display-message", "-p", "#{client_last_session}"])
            .output()
            .map_err(|e| format!("could not run tmux: {e}"))?;
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || name.is_empty() {
            return Err("tmux has no previous session for this client".to_string());
        }
        println!("{name}");
        return Ok(());
    }
    let out = std::process::Command::new("tmux")
        .args(["switch-client", "-l"])
        .output()
        .map_err(|e| format!("could not run tmux: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if msg.is_empty() {
            "tmux could not switch to the previous session".to_string()
        } else {
            format!("tmux: {msg}")
        })
    }
}

#[cfg(feature = "herdr")]
fn herdr_last_session(print: bool) -> Result<(), String> {
    use crate::multiplexer::{herdr_request, herdr_socket_path, parse_herdr_workspaces};
    let socket = herdr_socket_path()
        .filter(|p| p.exists())
        .ok_or_else(|| "herdr socket not found (is herdr running?)".to_string())?;
    let workspaces = herdr_request("workspace.list", serde_json::json!({}), &socket)
        .map(|r| parse_herdr_workspaces(&r))
        .ok_or_else(|| "herdr did not answer workspace.list".to_string())?;
    let current = workspaces.iter().find(|w| w.focused).map(|w| w.id.clone());
    let dir =
        state_dir().ok_or_else(|| "smarthistory state directory is not available".to_string())?;
    let socket_key = socket.to_string_lossy().to_string();
    let mut history = read_focus(&dir, &socket_key);
    let target = history
        .previous(current.as_deref(), |id| {
            workspaces.iter().any(|w| w.id == id)
        })
        .ok_or_else(|| {
            let running = crate::shell_server::ping(
                &crate::shell_server::socket_path(),
                std::time::Duration::from_millis(200),
            )
            .is_some();
            if running {
                "no previous herdr workspace recorded yet".to_string()
            } else {
                "no previous herdr workspace recorded (the shell server, which records herdr \
                 focus changes, is not running; see `smarthistory shell-server --status`)"
                    .to_string()
            }
        })?;
    let label = workspaces
        .iter()
        .find(|w| w.id == target)
        .map(|w| w.label.clone())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| target.clone());
    if print {
        println!("{label}");
        return Ok(());
    }
    herdr_request(
        "workspace.focus",
        serde_json::json!({ "workspace_id": target }),
        &socket,
    )
    .ok_or_else(|| format!("herdr refused to focus workspace {label}"))?;
    // Record the switch ourselves too, so toggling works even without
    // the monitor (it would record the same thing from the event).
    if let Some(cur) = &current {
        history.record(cur);
    }
    history.record(&target);
    let _ = write_focus(&dir, &history);
    Ok(())
}

#[cfg(not(feature = "herdr"))]
fn herdr_last_session(_print: bool) -> Result<(), String> {
    Err("this build has no herdr support (rebuild without `--no-default-features`)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(ids: &[&str]) -> FocusHistory {
        FocusHistory {
            socket: "s".into(),
            stack: ids.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn record_moves_to_front_without_duplicates() {
        let mut h = hist(&["a", "b", "c"]);
        assert!(h.record("c"));
        assert_eq!(h.stack, ["c", "a", "b"]);
        assert!(!h.record("c"));
        assert!(!h.record(""));
    }

    #[test]
    fn record_caps_history_length() {
        let mut h = FocusHistory::default();
        for i in 0..(FOCUS_HISTORY_LEN + 5) {
            h.record(&format!("w{i}"));
        }
        assert_eq!(h.stack.len(), FOCUS_HISTORY_LEN);
        assert_eq!(h.stack[0], format!("w{}", FOCUS_HISTORY_LEN + 4));
    }

    #[test]
    fn forget_drops_closed_workspace() {
        let mut h = hist(&["a", "b"]);
        assert!(h.forget("a"));
        assert!(!h.forget("zz"));
        assert_eq!(h.stack, ["b"]);
    }

    #[test]
    fn previous_skips_current_and_dead_workspaces() {
        let h = hist(&["a", "b", "c"]);
        assert_eq!(h.previous(Some("a"), |_| true).as_deref(), Some("b"));
        // `b` was closed behind our back: fall through to `c`.
        assert_eq!(h.previous(Some("a"), |id| id != "b").as_deref(), Some("c"));
        // Current workspace isn't the front entry (event missed).
        assert_eq!(h.previous(Some("b"), |_| true).as_deref(), Some("a"));
        // Current unknown: the front entry is assumed current.
        assert_eq!(h.previous(None, |_| true).as_deref(), Some("b"));
        assert_eq!(hist(&["a"]).previous(Some("a"), |_| true), None);
    }

    #[test]
    fn freshness_checks_backend_and_age() {
        let cache = PanesCache {
            backend: "herdr".into(),
            updated_ms: 10_000,
            contexts: vec![],
            panes: vec![],
            cmdlines: HashMap::new(),
        };
        assert!(cache.is_fresh_for(MultiplexerKind::Herdr, 10_000 + FRESH_MS));
        assert!(!cache.is_fresh_for(MultiplexerKind::Herdr, 10_001 + FRESH_MS));
        assert!(!cache.is_fresh_for(MultiplexerKind::Tmux, 10_000));
        // Clock went backwards: don't trust it.
        assert!(!cache.is_fresh_for(MultiplexerKind::Herdr, 9_000));
    }

    #[test]
    fn focus_history_round_trips_and_ignores_other_socket() {
        let dir = std::env::temp_dir().join(format!("sh-mux-state-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let h = FocusHistory {
            socket: "/a.sock".into(),
            stack: vec!["w1".into(), "w2".into()],
        };
        write_focus(&dir, &h).unwrap();
        assert_eq!(read_focus(&dir, "/a.sock"), h);
        assert!(read_focus(&dir, "/other.sock").stack.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
