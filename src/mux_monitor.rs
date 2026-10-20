//! The shell server's background multiplexer monitor.
//!
//! A long-lived child the shell-server listener forks at startup (see
//! `crate::shell_server::run_server`). It keeps two files in the
//! server's state directory up to date (`crate::mux_state`):
//!
//! - the tmux / herdr pane snapshot, re-taken every
//!   `shellserver.monitor.interval` ms (default 2000) and, for herdr,
//!   also right after any workspace / tab / pane change event;
//! - herdr's workspace focus history, from `workspace_focused` /
//!   `workspace_closed` events, which `smarthistory last-session` uses.
//!
//! Why a forked child rather than a thread in the listener: the
//! listener forks a handler per connection, and `fork()` is only safe
//! from a single-threaded process. The monitor is free to use threads
//! (it never forks itself, apart from `std::process::Command`).
//!
//! It exits when the listener goes away (its parent pid changes), and
//! when the configured multiplexer changes — the listener then starts
//! a fresh one. `shellserver.monitor=off` keeps it idle.

use crate::multiplexer::{MultiplexerKind, backend_for};
use crate::mux_state::{self, PanesCache};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Fork the monitor. Must be called from the single-threaded
/// listener. Returns the child's pid, or `None` if `fork` failed.
pub(crate) fn spawn(dir: PathBuf, listener_fd: i32) -> Option<libc::pid_t> {
    // SAFETY: getpid has no preconditions.
    let parent = unsafe { libc::getpid() };
    // SAFETY: the caller (the listener) is single-threaded.
    match unsafe { libc::fork() } {
        -1 => None,
        0 => {
            // SAFETY: closing our copy of the listening socket, so a
            // dead listener's socket can't keep accepting connections
            // that nobody serves. Restoring default SIGCHLD so
            // `std::process::Command` can `waitpid` its children.
            unsafe {
                libc::close(listener_fd);
                libc::signal(libc::SIGCHLD, libc::SIG_DFL);
            }
            run(&dir, parent);
            // SAFETY: leave without running the parent's exit-time
            // cleanup a second time.
            unsafe { libc::_exit(0) }
        }
        pid => Some(pid),
    }
}

fn parent_gone(parent: libc::pid_t) -> bool {
    // SAFETY: getppid has no preconditions.
    unsafe { libc::getppid() != parent }
}

/// What the monitor does, re-read from the config every
/// [`CONFIG_RELOAD`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Settings {
    enabled: bool,
    kind: MultiplexerKind,
    interval: Duration,
}

const CONFIG_RELOAD: Duration = Duration::from_secs(30);
/// Event-triggered refreshes closer together than this are merged.
const MIN_EVENT_REFRESH: Duration = Duration::from_millis(300);

fn load_settings() -> Settings {
    let cfg = crate::Config::load();
    Settings {
        enabled: cfg.shell_server_monitor,
        kind: cfg.multiplexer(),
        interval: Duration::from_millis(cfg.shell_server_monitor_interval_ms),
    }
}

fn run(dir: &Path, parent: libc::pid_t) {
    let settings = load_settings();
    let (refresh_tx, refresh_rx) = mpsc::channel::<()>();
    #[cfg(feature = "herdr")]
    if settings.enabled && settings.kind == MultiplexerKind::Herdr {
        let dir = dir.to_path_buf();
        std::thread::spawn(move || herdr_events_loop(&dir, refresh_tx));
    }
    #[cfg(not(feature = "herdr"))]
    drop(refresh_tx);
    let mut last_reload = Instant::now();
    let mut last_snapshot: Option<Instant> = None;
    loop {
        if parent_gone(parent) {
            return;
        }
        if last_reload.elapsed() >= CONFIG_RELOAD {
            last_reload = Instant::now();
            if load_settings() != settings {
                // The listener notices and starts a fresh monitor.
                return;
            }
        }
        if settings.enabled {
            let due = last_snapshot.is_none_or(|t| t.elapsed() >= settings.interval);
            if due {
                take_snapshot(dir, settings.kind);
                last_snapshot = Some(Instant::now());
            }
        }
        // Sleep until the next poll, waking early for herdr events.
        // Never longer than a second, so a dead parent is noticed.
        let wait = if settings.enabled {
            let since = last_snapshot.map(|t| t.elapsed()).unwrap_or_default();
            settings
                .interval
                .saturating_sub(since)
                .min(Duration::from_secs(1))
        } else {
            Duration::from_secs(1)
        };
        if refresh_rx.recv_timeout(wait).is_ok() {
            // Merge a burst of events (a split creates a pane, focuses
            // it, updates the layout) into one snapshot.
            std::thread::sleep(MIN_EVENT_REFRESH);
            while refresh_rx.try_recv().is_ok() {}
            last_snapshot = None;
        }
    }
}

fn take_snapshot(dir: &Path, kind: MultiplexerKind) {
    let backend = backend_for(kind);
    let contexts = backend.snapshot();
    let panes = backend.snapshot_all_panes();
    #[allow(unused_mut)]
    let mut cmdlines = std::collections::HashMap::new();
    #[cfg(feature = "herdr")]
    if kind == MultiplexerKind::Herdr
        && let Some(socket) = crate::multiplexer::herdr_socket_path()
    {
        for p in &panes {
            if let Some(cmd) = crate::multiplexer::herdr_request(
                "pane.process_info",
                serde_json::json!({ "pane_id": p.pane_id }),
                &socket,
            )
            .and_then(|r| crate::multiplexer::parse_herdr_process_info(&r))
            {
                cmdlines.insert(p.pane_id.clone(), cmd);
            }
        }
    }
    let cache = PanesCache {
        backend: kind.as_str().to_string(),
        updated_ms: mux_state::now_ms(),
        contexts,
        panes,
        cmdlines,
    };
    let _ = mux_state::write_panes(dir, &cache);
}

/// What one herdr event means for the monitor.
#[derive(Debug, PartialEq, Eq)]
enum HerdrEvent {
    Focused(String),
    Closed(String),
    /// Some other structural change: re-take the snapshot.
    Changed,
    Ignore,
}

/// The events the monitor subscribes to: focus changes plus every
/// structural change that alters the `*` / `#` lists. Deliberately
/// not `pane.agent_status_changed` / `pane.output_matched` /
/// `pane.scroll_changed`, which fire constantly.
#[cfg(feature = "herdr")]
const SUBSCRIPTIONS: &[&str] = &[
    "workspace.focused",
    "workspace.created",
    "workspace.closed",
    "workspace.renamed",
    "tab.created",
    "tab.closed",
    "tab.focused",
    "pane.created",
    "pane.closed",
    "pane.exited",
    "pane.focused",
    "pane.moved",
];

/// Classify one line from the subscription stream:
/// `{"event": "...", "data": {"type": "workspace_focused",
/// "workspace_id": "w3"}}`. The subscribe acknowledgement
/// (`{"id": ..., "result": ...}`) and anything unparseable are
/// ignored.
fn classify_herdr_event(line: &str) -> HerdrEvent {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return HerdrEvent::Ignore;
    };
    let Some(data) = v.get("data") else {
        return HerdrEvent::Ignore;
    };
    let kind = data
        .get("type")
        .and_then(|t| t.as_str())
        .or_else(|| v.get("event").and_then(|e| e.as_str()))
        .unwrap_or("")
        .replace('.', "_");
    let workspace = data
        .get("workspace_id")
        .and_then(|w| w.as_str())
        .unwrap_or("")
        .to_string();
    match kind.as_str() {
        "workspace_focused" if !workspace.is_empty() => HerdrEvent::Focused(workspace),
        "workspace_closed" if !workspace.is_empty() => HerdrEvent::Closed(workspace),
        "" => HerdrEvent::Ignore,
        _ => HerdrEvent::Changed,
    }
}

/// Subscribe to herdr's event stream and keep the focus history up to
/// date; reconnects (every 2s) whenever herdr isn't running or the
/// connection drops. Runs until the process exits.
#[cfg(feature = "herdr")]
fn herdr_events_loop(dir: &Path, refresh: mpsc::Sender<()>) {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    loop {
        let socket = crate::multiplexer::herdr_socket_path().filter(|p| p.exists());
        if let Some(socket) = socket
            && let Ok(mut stream) = UnixStream::connect(&socket)
        {
            let key = socket.to_string_lossy().to_string();
            seed_focus(dir, &socket, &key);
            let subs: Vec<_> = SUBSCRIPTIONS
                .iter()
                .map(|t| serde_json::json!({ "type": t }))
                .collect();
            let req = serde_json::json!({
                "id": "smarthistory-monitor",
                "method": "events.subscribe",
                "params": { "subscriptions": subs },
            });
            let mut line = serde_json::to_vec(&req).unwrap_or_default();
            line.push(b'\n');
            if stream.write_all(&line).is_ok() {
                let _ = refresh.send(());
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    match classify_herdr_event(&line) {
                        HerdrEvent::Focused(id) => {
                            update_focus(dir, &key, |h| h.record(&id));
                            let _ = refresh.send(());
                        }
                        HerdrEvent::Closed(id) => {
                            update_focus(dir, &key, |h| h.forget(&id));
                            let _ = refresh.send(());
                        }
                        HerdrEvent::Changed => {
                            let _ = refresh.send(());
                        }
                        HerdrEvent::Ignore => {}
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Read-modify-write the focus history (`last-session` writes it too).
#[cfg(feature = "herdr")]
fn update_focus(
    dir: &Path,
    socket_key: &str,
    change: impl FnOnce(&mut mux_state::FocusHistory) -> bool,
) {
    let mut history = mux_state::read_focus(dir, socket_key);
    if change(&mut history) {
        let _ = mux_state::write_focus(dir, &history);
    }
}

/// On (re)connect, record whichever workspace is focused right now, so
/// the first switch after startup already has something to go back to.
#[cfg(feature = "herdr")]
fn seed_focus(dir: &Path, socket: &Path, socket_key: &str) {
    let focused =
        crate::multiplexer::herdr_request("workspace.list", serde_json::json!({}), socket)
            .map(|r| crate::multiplexer::parse_herdr_workspaces(&r))
            .and_then(|list| list.into_iter().find(|w| w.focused));
    if let Some(ws) = focused {
        update_focus(dir, socket_key, |h| h.record(&ws.id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_focus_close_and_other_events() {
        assert_eq!(
            classify_herdr_event(
                r#"{"event":"workspace_focused","data":{"type":"workspace_focused","workspace_id":"w3"}}"#
            ),
            HerdrEvent::Focused("w3".into())
        );
        assert_eq!(
            classify_herdr_event(
                r#"{"event":"workspace_closed","data":{"type":"workspace_closed","workspace_id":"w3","workspace":null}}"#
            ),
            HerdrEvent::Closed("w3".into())
        );
        assert_eq!(
            classify_herdr_event(
                r#"{"event":"pane_created","data":{"type":"pane_created","workspace_id":"w3"}}"#
            ),
            HerdrEvent::Changed
        );
        // Dotted event name without a data.type falls back to `event`.
        assert_eq!(
            classify_herdr_event(r#"{"event":"workspace.focused","data":{"workspace_id":"w9"}}"#),
            HerdrEvent::Focused("w9".into())
        );
    }

    #[test]
    fn ignores_ack_and_garbage() {
        assert_eq!(
            classify_herdr_event(r#"{"id":"x","result":{"type":"subscription_started"}}"#),
            HerdrEvent::Ignore
        );
        assert_eq!(classify_herdr_event("not json"), HerdrEvent::Ignore);
    }
}
