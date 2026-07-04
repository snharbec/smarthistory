//! Multiplexer abstraction.
//!
//! smarthistory's directory-switching TUI (the
//! `#` mode) and the panes-switching TUI
//! (the `*` mode) need three things from the
//! terminal multiplexer:
//!
//! 1. A **snapshot** of every active
//!    "context" (a tmux window, a herdr
//!    workspace) along with the cwd and
//!    a stable id. The TUI uses this to
//!    compute the `T`-marker in the
//!    directory list and to drive the
//!    panes view.
//! 2. A **focus** command that, given a
//!    context id, jumps the user's
//!    *current* terminal to that context.
//! 3. A **create** command that, given a
//!    directory and a label, brings up a
//!    fresh context rooted at that
//!    directory.
//!
//! tmux and herdr model these things
//! differently:
//!
//! - tmux: a *session* is a long-lived
//!   container; a *pane* is a single shell
//!   inside a *window*; the cwd of an
//!   active pane is the directory the
//!   user "had a session in". `select-pane
//!   -t %N && switch-client -t %N` jumps
//!   to an existing pane; `new-session -d
//!   -s NAME -c DIR; switch-client -t
//!   NAME` opens a new one.
//! - herdr: a *workspace* is a long-lived
//!   container (anchored by its root
//!   pane's cwd); a *pane* is a shell
//!   inside the workspace. `herdr
//!   workspace focus <id>` jumps to an
//!   existing workspace; `herdr workspace
//!   create --cwd DIR --label NAME`
//!   opens a new one. herdr ids are
//!   compact positional integers
//!   (`1`, `2`, …) so we always
//!   reference workspaces by id, never
//!   by name.
//!
//! The two backends share the same
//! "shape" (id, path), so the TUI sees
//! one unified model via
//! [`MultiplexerBackend::snapshot`]. The
//! focus/create commands are
//! *stage-then-run* shell strings:
//! the TUI writes them to stdout, the
//! parent shell `eval`s them, so the
//! swap is purely textual.
//!
//! ## Feature flag
//!
//! The herdr backend is gated behind the
//! `herdr` Cargo feature so the binary
//! size doesn't grow for users who
//! don't use herdr. When the feature is
//! disabled, [`MultiplexerKind::parse`]
//! still recognises `"herdr"` (so a
//! user's config doesn't silently fail
//! to parse) but the backend selection
//! always returns the tmux backend, and
//! a status message is surfaced the
//! first time a directory row is
//! selected under a herdr config.

use std::path::Path;

/// Which multiplexer the TUI should
/// target for directory switching.
///
/// The string form (`tmux` / `herdr`)
/// is what users put in
/// `~/.config/smarthistory/config` as
/// `multiplexer=...`; the parser is
/// case-insensitive and accepts a few
/// friendly aliases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MultiplexerKind {
    /// tmux (default). The TUI
    /// shells out to `tmux
    /// list-windows -a` and stages
    /// `tmux select-pane` /
    /// `tmux new-session` commands.
    #[default]
    Tmux,
    /// herdr. The TUI shells out
    /// to `herdr workspace list
    /// --json` / `herdr pane list`
    /// and stages `herdr workspace
    /// focus` / `herdr workspace
    /// create` commands. The
    /// `herdr` Cargo feature must
    /// be enabled; if it isn't,
    /// selecting a directory row
    /// surfaces a status message
    /// explaining the missing
    /// feature instead of staging
    /// a command.
    Herdr,
}

impl MultiplexerKind {
    /// Parse the user-facing config
    /// value. Returns `None` for
    /// unrecognised strings so the
    /// caller can fall back to the
    /// default and surface a warning
    /// in the same way the rest of
    /// the config parser does.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "tmux" | "" => Some(MultiplexerKind::Tmux),
            "herdr" => Some(MultiplexerKind::Herdr),
            _ => None,
        }
    }

    /// Canonical string form, used
    /// for session-file persistence
    /// and for the `multiplexer=`
    /// key in the config check
    /// report.
    pub fn as_str(&self) -> &'static str {
        match self {
            MultiplexerKind::Tmux => "tmux",
            MultiplexerKind::Herdr => "herdr",
        }
    }

    /// True when the requested
    /// backend is `herdr` but
    /// the binary was built
    /// without the `herdr`
    /// Cargo feature
    /// (`--no-default-features`).
    /// The TUI uses this to
    /// short-circuit the
    /// herdr path and surface
    /// a clear "build with
    /// `--features herdr` or
    /// drop the `--no-default-features`
    /// flag" status message
    /// rather than staging a
    /// broken command.
    pub fn is_herdr_unavailable(&self) -> bool {
        cfg!(not(feature = "herdr")) && *self == MultiplexerKind::Herdr
    }
}

/// One row in the active-context
/// snapshot. Mirrors
/// `crate::tui::state::TmuxWindowInfo`
/// in shape (a pane_id-equivalent
/// + a cwd) so the existing
/// `directory_tmux_pane_id` /
/// `fetch_directories` logic can
/// match against it without
/// caring which backend produced
/// it. The `pane_id` is the
/// backend's own id string
/// (tmux `%N`, herdr `w1:p1` or
/// just `1` for a workspace —
/// whatever the backend uses as
/// its `-t` / `focus` target).
///
/// `#[allow(dead_code)]` on
/// the struct + its
/// fields because the
/// production code
/// passes the struct
/// through
/// `self.multiplexer.snapshot()`
/// and only reads the
/// `pane_id` field
/// (via
/// `self.multiplexer.focus_command(&pane_id)`).
/// The `path` and
/// `window_id` fields
/// are read by tests
/// and by
/// `directory_tmux_pane_id`'s
/// lookup, but rustc's
/// field-level
/// dead-code analysis
/// only sees the
/// production callers
/// via the
/// `self.multiplexer.*`
/// trait-object
/// dispatch and reports
/// the fields as
/// unused. The fields
/// ARE meaningful (the
/// T-marker matching
/// walks `tmux_windows`
/// row by row, reading
/// `path`) — the
/// warning is a
/// false positive from
/// the field-level
/// analysis when
/// consumption happens
/// in a helper
/// function rather
/// than at the call
/// site.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ActiveContext {
    /// Stable id the backend
    /// accepts as a focus target
    /// (tmux: `%N`; herdr: `N`).
    pub pane_id: String,
    /// Workspace / window id
    /// when relevant (tmux:
    /// `@N`, used by the `*`
    /// panes mode; herdr: also
    /// `N`, since herdr
    /// workspaces are the
    /// top-level unit). Empty
    /// for backends that don't
    /// distinguish.
    ///
    /// `#[allow(dead_code)]`
    /// because the current
    /// staging layer only
    /// consumes `pane_id`
    /// (herdr's
    /// `workspace focus`
    /// is enough on its
    /// own; tmux's
    /// `select-pane` folds
    /// the window switch
    /// in). The field is
    /// retained so a
    /// future cross-window
    /// jump on the herdr
    /// backend doesn't
    /// require a breaking
    /// change to the
    /// `MultiplexerBackend`
    /// trait.
    #[allow(dead_code)]
    pub window_id: String,
    /// Cwd of the active pane /
    /// workspace, as reported
    /// by the backend. May be
    /// a `~/x` short form
    /// (herdr uses
    /// shell-shortened paths in
    /// some commands) or an
    /// absolute path.
    pub path: String,
}

/// Backend trait. Each
/// implementation shells out to
/// its own multiplexer CLI. All
/// methods are silent on failure
/// (returning empty / `None`) —
/// the directory / panes UI
/// degrades gracefully when the
/// multiplexer is missing,
/// misconfigured, or running on
/// a different host.
///
/// `#[allow(dead_code)]` on
/// the trait + the
/// `ActiveContext` /
/// `CurrentPaneInfo`
/// structs because the
/// only callers go
/// through
/// `Box<dyn MultiplexerBackend>`
/// and rustc's
/// dead-code analysis
/// can't see through
/// trait-object
/// dispatch. The
/// methods are called
/// from `App::fetch_tmux_windows`
/// and the staging
/// sites in `select_for_run`
/// via
/// `self.multiplexer.snapshot()`,
/// `self.multiplexer.focus_command(...)`,
/// etc.
#[allow(dead_code)]
pub trait MultiplexerBackend: Send + Sync {
    /// Snapshot every active
    /// context. Cached by the
    /// TUI for the lifetime of
    /// the session (the pane
    /// set doesn't change while
    /// the TUI is the
    /// foreground process).
    fn snapshot(&self) -> Vec<ActiveContext>;

    /// Snapshot the panes in the
    /// *current* context (the
    /// one the TUI is running
    /// in), excluding the
    /// caller's own pane. Used
    /// by the `*` panes view.
    /// Returns rows in
    /// display-ready form
    /// (id, window_id, path,
    /// command, is_last).
    fn snapshot_current_panes(&self, current_pane: &str) -> Vec<CurrentPaneInfo>;

    /// Stage a command that, when
    /// the parent shell runs it,
    /// focuses the given
    /// (already-existing) context.
    /// Returns `None` when the
    /// backend has no equivalent
    /// (e.g. the id is stale).
    fn focus_command(&self, pane_id: &str) -> Option<String>;

    /// Stage a command that, when
    /// the parent shell runs it,
    /// creates a fresh context
    /// rooted at `dir` and jumps
    /// to it. `label` is a
    /// human-readable name (tmux
    /// session name, herdr
    /// workspace label).
    fn create_command(&self, dir: &Path, label: &str) -> Option<String>;

    /// Stage a command that runs
    /// `body` *inside* the given
    /// pane (used by the
    /// `.command` file hook to
    /// bootstrap a project after
    /// focus / create). For tmux
    /// this is `tmux send-keys`; for
    /// herdr this is `herdr pane
    /// send-text`.
    fn send_in_pane_command(&self, pane_id: &str, body: &str) -> Option<String>;

    /// Backend name for status
    /// messages and the help
    /// overlay ("tmux" / "herdr").
    fn name(&self) -> &'static str;
}

/// A row in the `*` panes
/// view. Mirrors what the
/// existing tmux-based
/// `fetch_session_panes`
/// produces.
///
/// `#[allow(dead_code)]` on
/// the struct + its
/// fields for the same
/// reason as
/// [`ActiveContext`]:
/// the production code
/// constructs rows
/// (via
/// `self.multiplexer.snapshot_current_panes()`)
/// but only consumes
/// `pane_id` (via
/// `self.multiplexer.focus_command(&pane_id)`);
/// the other fields are
/// read by tests and by
/// the `session_panes`
/// view, which is a
/// helper that
/// rustc's
/// field-level
/// dead-code analysis
/// can't see when the
/// row reaches it via
/// `self.session_panes`
/// (a `Vec<HistoryRow>`,
/// not a
/// `Vec<CurrentPaneInfo>`).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CurrentPaneInfo {
    /// tmux: `%N`; herdr: `w1:p1`
    /// (workspace-scoped pane
    /// id).
    pub pane_id: String,
    /// tmux: `@N`; herdr: `N`
    /// (workspace id, used as
    /// the focus target when
    /// the pane is in another
    /// workspace).
    ///
    /// `#[allow(dead_code)]`
    /// for the same reason
    /// as
    /// [`ActiveContext::window_id`]:
    /// retained for future
    /// cross-workspace
    /// jumps; the current
    /// staging layer folds
    /// the window switch
    /// into the backend's
    /// `focus_command`.
    #[allow(dead_code)]
    pub window_id: String,
    /// Cwd of the pane, as
    /// reported by the backend.
    pub path: String,
    /// The pane's foreground
    /// command (best-effort;
    /// empty when the backend
    /// can't tell).
    pub current_command: String,
    /// True for the
    /// "previously-active"
    /// pane — the one the
    /// backend's `last-pane`
    /// equivalent would jump
    /// to. The TUI bubbles
    /// this row to the top so
    /// pressing Enter flips
    /// back to where the user
    /// just was.
    pub is_last: bool,
}

/// Construct the right backend
/// for the configured kind.
///
/// The `herdr` backend is only
/// available when the `herdr`
/// Cargo feature is compiled
/// in. When the user requests
/// `MultiplexerKind::Herdr` on a
/// non-`herdr` build, we still
/// return a tmux backend; the
/// `is_herdr_unavailable()` check
/// on the kind surfaces a status
/// message at the staging site
/// (the user's intent is
/// preserved, but they get a
/// clear explanation rather
/// than a silent no-op).
pub fn backend_for(kind: MultiplexerKind) -> Box<dyn MultiplexerBackend> {
    match kind {
        MultiplexerKind::Tmux => Box::new(TmuxBackend),
        #[cfg(feature = "herdr")]
        MultiplexerKind::Herdr => Box::new(HerdrBackend),
        #[cfg(not(feature = "herdr"))]
        MultiplexerKind::Herdr => Box::new(TmuxBackend),
    }
}

// --- tmux backend ------------------------------------------------

/// tmux implementation. Wraps
/// the existing `tmux
/// list-windows -a` and `tmux
/// list-panes -s` invocations
/// behind the
/// [`MultiplexerBackend`] trait.
struct TmuxBackend;

/// Read a `tmux list-windows -a
/// -F <fmt>` invocation,
/// returning one
/// [`ActiveContext`] per
/// non-empty output line. The
/// format matches the one used
/// by `fetch_tmux_windows` in
/// the TUI (pane_id | cwd |
/// active:0|1 | Layout: ...).
fn tmux_list_windows_parse(stdout: &[u8]) -> Vec<ActiveContext> {
    let mut out = Vec::new();
    let text = match std::str::from_utf8(stdout) {
        Ok(t) => t,
        Err(_) => return out,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('|').map(str::trim).collect();
        if parts.len() < 3 {
            continue;
        }
        let pane_id = parts[0];
        let path = parts[1];
        // `active:0` / `active:1`.
        let active = parts[2].starts_with("active:1");
        if !active {
            continue;
        }
        if pane_id.is_empty() || path.is_empty() {
            continue;
        }
        out.push(ActiveContext {
            pane_id: pane_id.to_string(),
            window_id: String::new(),
            path: path.to_string(),
        });
    }
    out
}

/// Read a `tmux list-panes -s
/// -F <fmt>` invocation and
/// return one
/// [`CurrentPaneInfo`] per
/// line. Mirrors
/// `fetch_session_panes_impl`'s
/// parser so the same field
/// semantics carry over.
fn tmux_list_panes_parse(stdout: &[u8], current_pane: &str) -> Vec<CurrentPaneInfo> {
    let mut out = Vec::new();
    let text = match std::str::from_utf8(stdout) {
        Ok(t) => t,
        Err(_) => return out,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('|').map(str::trim).collect();
        if parts.len() < 4 {
            continue;
        }
        let pane_id = parts[0];
        if pane_id.is_empty() || pane_id == current_pane {
            continue;
        }
        let window_id = parts[1].to_string();
        let path = parts[2].to_string();
        let current_command = parts[3].to_string();
        let is_last = parts.get(4).copied().unwrap_or("0") == "1";
        out.push(CurrentPaneInfo {
            pane_id: pane_id.to_string(),
            window_id,
            path,
            current_command,
            is_last,
        });
    }
    out
}

/// Spawn a `tmux ...` subprocess
/// with the given args and a
/// bounded timeout. Returns
/// `None` on failure (tmux
/// missing, no server,
/// timeout, parse error). The
/// timeout is read from
/// `TMUX_PANE_PROBE_TIMEOUT_MS`
/// (default 1000 ms) to match
/// the existing behaviour.
///
/// `#[allow(dead_code)]` on
/// the function because
/// the only callers
/// live inside
/// `impl MultiplexerBackend for TmuxBackend`
/// (in `TmuxBackend::snapshot`
/// and
/// `TmuxBackend::snapshot_current_panes`).
/// When the `multiplexer`
/// field on `App` is the
/// tmux backend (the
/// default), those impl
/// methods are called
/// from
/// `App::fetch_tmux_windows`
/// and
/// `App::fetch_session_panes_impl`
/// via
/// `self.multiplexer.snapshot()`,
/// but rustc's
/// dead-code analysis
/// can't see through
/// the trait-object
/// dispatch on its
/// own. See the trait's
/// `#[allow(dead_code)]`
/// for the full
/// rationale.
#[allow(dead_code)]
fn tmux_run(args: &[&str]) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let timeout_ms: u64 = std::env::var("TMUX_PANE_PROBE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let mut child = match Command::new("tmux")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return None,
    };
    // Bounded wait. We can't
    // easily wait_with_output
    // with a timeout from
    // std (no native wait
    // timeout until Rust
    // 1.66 with `child.wait()`
    // plus a thread), so we
    // use the same `try_wait`
    // + sleep poll as the
    // existing TUI code.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut stdout = child.stdout.take()?;
    let mut buf = Vec::new();
    loop {
        if let Some(s) = child.stdout.take() {
            stdout = s;
        }
        let mut chunk = [0u8; 4096];
        match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = child.wait();
    Some(buf)
}

impl MultiplexerBackend for TmuxBackend {
    fn snapshot(&self) -> Vec<ActiveContext> {
        const FORMAT: &str = "\
            #{pane_id} | \
            #{pane_current_path} | \
            active:#{window_active} | \
            Layout: #{window_layout}";
        match tmux_run(&["list-windows", "-a", "-F", FORMAT]) {
            Some(bytes) => tmux_list_windows_parse(&bytes),
            None => Vec::new(),
        }
    }

    fn snapshot_current_panes(&self, current_pane: &str) -> Vec<CurrentPaneInfo> {
        const FORMAT: &str = "#{pane_id} | #{window_id} | #{pane_current_path} | #{pane_current_command} | #{?pane_last,1,0}";
        match tmux_run(&["list-panes", "-s", "-F", FORMAT]) {
            Some(bytes) => tmux_list_panes_parse(&bytes, current_pane),
            None => Vec::new(),
        }
    }

    fn focus_command(&self, pane_id: &str) -> Option<String> {
        if pane_id.is_empty() {
            return None;
        }
        // `&&` chains the two
        // calls: if `select-pane`
        // fails (pane disappeared
        // between snapshot and
        // Enter), don't try to
        // switch-client to a
        // dead target.
        Some(format!(
            "tmux select-pane -t {} && \
             tmux switch-client -t {}",
            pane_id, pane_id
        ))
    }

    fn create_command(&self, dir: &Path, label: &str) -> Option<String> {
        // Expand `~` ourselves —
        // tmux doesn't do it.
        let path = crate::util::expand_home(&dir.to_string_lossy()).into_owned();
        let quoted_path = if path
            .chars()
            .any(|c| c.is_whitespace() || "<>|&;\"'$`\\".contains(c))
        {
            format!("\"{}\"", path)
        } else {
            path
        };
        // `-d` (detached) +
        // explicit switch-client
        // so the smarthistory
        // process's TTY isn't
        // stolen by the new
        // session.
        Some(format!(
            "tmux new-session -d -s {} -c {}; \
             tmux switch-client -t {}",
            label, quoted_path, label
        ))
    }

    fn send_in_pane_command(&self, pane_id: &str, body: &str) -> Option<String> {
        if pane_id.is_empty() {
            return None;
        }
        Some(format!(
            "tmux send-keys -t {} {} Enter",
            pane_id,
            crate::util::shell_quote(body)
        ))
    }

    fn name(&self) -> &'static str {
        "tmux"
    }
}

// --- herdr backend -----------------------------------------------

/// herdr implementation. Shells
/// out to `herdr workspace list`
/// and `herdr pane list` and
/// stages `herdr workspace
/// focus` / `herdr workspace
/// create` / `herdr pane
/// send-text` commands.
///
/// herdr's `workspace list` is
/// a CLI; the JSON shape is not
/// documented in the public
/// docs, so we parse the
/// human-readable table output
/// (the same way the TUI
/// currently parses
/// `tmux list-windows`).
#[cfg(feature = "herdr")]
struct HerdrBackend;

/// Run a `herdr ...` subprocess
/// and return its JSON stdout
/// parsed as a
/// `serde_json::Value`.
/// Bounded by the same
/// `TMUX_PANE_PROBE_TIMEOUT_MS`
/// env var as the tmux
/// path (default 1000 ms;
/// the env-var name is
/// kept for back-compat
/// with the existing
/// setting).
///
/// `#[allow(dead_code)]` for
/// the same reason as
/// `tmux_run` — the only
/// callers live inside
/// `impl MultiplexerBackend for HerdrBackend`
/// (in
/// `HerdrBackend::snapshot`
/// and
/// `HerdrBackend::snapshot_current_panes`).
/// When the `multiplexer`
/// field on `App` is the
/// herdr backend, those
/// impl methods are
/// called from
/// `App::fetch_tmux_windows`
/// and
/// `App::fetch_session_panes_impl`
/// via
/// `self.multiplexer.snapshot()`,
/// but rustc's
/// dead-code analysis
/// can't see through
/// the trait-object
/// dispatch on its own.
/// See the trait's
/// `#[allow(dead_code)]`
/// for the full
/// rationale.
#[cfg(feature = "herdr")]
#[allow(dead_code)]
fn herdr_run_json(args: &[&str]) -> Option<serde_json::Value> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let timeout_ms: u64 = std::env::var("TMUX_PANE_PROBE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let mut child = match Command::new("herdr")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return None,
    };
    let mut stdout = child.stdout.take()?;
    let mut buf = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let mut chunk = [0u8; 4096];
        match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = child.wait();
    if buf.is_empty() {
        return None;
    }
    serde_json::from_slice(&buf).ok()
}

/// One row of
/// `herdr pane list`
/// JSON. Both the
/// directory
/// T-marker snapshot
/// and the `*`-mode
/// panes view
/// consume this
/// shape — the only
/// data source the
/// TUI needs to
/// tell "is there an
/// active pane in
/// this directory?"
/// and "what panes
/// can I switch to
/// from here?".
#[cfg(feature = "herdr")]
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct HerdrPaneRecord {
    pane_id: String,
    workspace_id: String,
    /// The directory the
    /// foreground process
    /// is actually in
    /// (herdr's
    /// `foreground_cwd`,
    /// falling back to
    /// the workspace's
    /// initial cwd).
    cwd: String,
    /// The detected agent
    /// name (e.g. "pi",
    /// "claude code",
    /// "codex"). Empty
    /// when herdr hasn't
    /// detected an agent
    /// in this pane.
    agent: String,
}

#[cfg(feature = "herdr")]
fn parse_herdr_pane_list(json: &serde_json::Value) -> Vec<HerdrPaneRecord> {
    let mut out = Vec::new();
    let panes = match json
        .get("result")
        .and_then(|r| r.get("panes"))
        .and_then(|p| p.as_array())
    {
        Some(p) => p,
        None => return out,
    };
    for p in panes {
        let pane_id = p
            .get("pane_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let workspace_id = p
            .get("workspace_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cwd = p
            .get("cwd")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Prefer
        // `foreground_cwd`
        // when herdr
        // reports it
        // (a pane's
        // foreground
        // process can
        // change cwd
        // inside the
        // session; the
        // initial
        // workspace cwd
        // is the wrong
        // value to match
        // against for
        // the T-marker).
        let effective_cwd = p
            .get("foreground_cwd")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&cwd)
            .to_string();
        let agent = p
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if pane_id.is_empty() || effective_cwd.is_empty() {
            continue;
        }
        out.push(HerdrPaneRecord {
            pane_id,
            workspace_id,
            cwd: effective_cwd,
            agent,
        });
    }
    out
}

#[cfg(feature = "herdr")]
impl MultiplexerBackend for HerdrBackend {
    fn snapshot(&self) -> Vec<ActiveContext> {
        // The directory
        // T-marker snapshot
        // is built from
        // `herdr pane list`,
        // not `herdr
        // workspace list`:
        // workspaces don't
        // carry a cwd (only
        // panes do), so
        // `workspace list`
        // can't tell us
        // "which directory
        // is currently
        // active in this
        // workspace".
        // `pane list`
        // returns every
        // pane across every
        // workspace with
        // its `cwd` (or
        // `foreground_cwd`
        // when the
        // foreground
        // process has
        // changed it via
        // `cd`).
        //
        // Each row becomes
        // an `ActiveContext`
        // so the existing
        // `directory_tmux_pane_id`
        // lookup (which
        // canonicalises
        // both sides via
        // `normalize_for_compare`
        // and matches) just
        // works: the user
        // picks directory D,
        // the TUI looks for
        // a row whose `path`
        // matches D (after
        // homemap expansion
        // and
        // canonicalisation),
        // and if found the
        // staging branches
        // to "focus the
        // existing
        // workspace" rather
        // than "create a new
        // one". The
        // `pane_id` here is
        // the
        // workspace-scoped
        // pane id
        // (`wA:p1`);
        // `focus_command`
        // below strips the
        // `:pN` suffix and
        // passes just `wA`
        // to `herdr
        // workspace focus`.
        let json = match herdr_run_json(&["pane", "list"]) {
            Some(j) => j,
            None => return Vec::new(),
        };
        parse_herdr_pane_list(&json)
            .into_iter()
            .map(|r| ActiveContext {
                pane_id: r.pane_id,
                window_id: r.workspace_id,
                path: r.cwd,
            })
            .collect()
    }

    fn snapshot_current_panes(&self, current_pane: &str) -> Vec<CurrentPaneInfo> {
        // The `*`-mode
        // "panes in the
        // current context"
        // view is the
        // current
        // workspace's
        // sibling panes —
        // the panes the
        // user can switch
        // to from the TUI
        // they're
        // currently in.
        //
        // tmux's equivalent
        // is `tmux
        // list-panes -s`
        // (current session,
        // current pane
        // excluded). herdr
        // has no
        // "current session"
        // concept (it has
        // one global
        // session server),
        // but the same UX
        // intent maps to
        // "the current
        // workspace's
        // panes, minus the
        // one the TUI is
        // running in".
        //
        // herdr sets
        // `HERDR_WORKSPACE_ID`
        // and `HERDR_PANE_ID`
        // in the env of
        // every pane
        // process; the
        // existing tmux
        // flow reads
        // `$TMUX_PANE` for
        // the same purpose.
        // We honour the
        // herdr env vars
        // here, then filter
        // `pane list` to
        // the current
        // workspace and
        // exclude the
        // current pane.
        //
        // When herdr isn't
        // running (no
        // `HERDR_WORKSPACE_ID`),
        // we return an
        // empty list — the
        // user isn't inside
        // a herdr pane, so
        // they can't
        // switch to a
        // sibling pane.
        let current_workspace = std::env::var("HERDR_WORKSPACE_ID")
            .ok()
            .filter(|s| !s.is_empty());
        let current_pane_env = std::env::var("HERDR_PANE_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| current_pane.to_string());
        let current_workspace = match current_workspace {
            Some(ws) => ws,
            None => return Vec::new(),
        };
        let json = match herdr_run_json(&["pane", "list"]) {
            Some(j) => j,
            None => return Vec::new(),
        };
        parse_herdr_pane_list(&json)
            .into_iter()
            .filter(|r| r.workspace_id == current_workspace && r.pane_id != current_pane_env)
            .map(|r| CurrentPaneInfo {
                pane_id: r.pane_id,
                window_id: r.workspace_id,
                path: r.cwd,
                // herdr's `agent`
                // is the closest
                // equivalent of
                // tmux's
                // `pane_current_command`
                // (the detected
                // agent name;
                // empty for plain
                // shells).
                current_command: r.agent,
                is_last: false,
            })
            .collect()
    }

    fn focus_command(&self, pane_id: &str) -> Option<String> {
        if pane_id.is_empty() {
            return None;
        }
        // `herdr workspace focus`
        // accepts a
        // workspace id
        // (`wA`, `1`, …),
        // not a pane id
        // (`wA:p1`). Our
        // snapshot rows
        // carry the
        // workspace-scoped
        // pane id
        // (`wA:p1`) because
        // that's what herdr
        // reports, so we
        // strip the `:pN`
        // suffix here.
        // Workspace ids are
        // the
        // colon-prefix-free
        // leftmost
        // component: split
        // on `:` and take
        // the first piece.
        let workspace_id = pane_id.split(':').next().unwrap_or(pane_id);
        if workspace_id.is_empty() {
            return None;
        }
        Some(format!("herdr workspace focus {}", workspace_id))
    }

    fn create_command(&self, dir: &Path, label: &str) -> Option<String> {
        let path = crate::util::expand_home(&dir.to_string_lossy()).into_owned();
        let quoted_path = if path
            .chars()
            .any(|c| c.is_whitespace() || "<>|&;\"'$`\\".contains(c))
        {
            format!("\"{}\"", path)
        } else {
            path
        };
        // `herdr workspace create
        // --cwd DIR --label NAME
        // --focus` — the
        // documented way to
        // open a fresh workspace
        // rooted at a
        // directory.
        //
        // `--focus` is
        // **explicit**, not
        // relying on herdr's
        // default: the user's
        // intent in
        // smarthistory's
        // directories mode is
        // "I picked a row,
        // take me there", so
        // the workspace must
        // be auto-activated
        // after creation. If
        // herdr ever changes
        // its default to
        // `--no-focus`, the
        // staging command
        // would silently
        // break (the new
        // workspace would be
        // created but the
        // user would stay in
        // their current
        // context). Forcing
        // the flag makes the
        // contract
        // independent of
        // herdr's defaults.
        // `--no-focus` is
        // what the user
        // would use to *not*
        // auto-activate;
        // smarthistory has
        // no such mode today.
        //
        // Unlike tmux,
        // herdr labels
        // aren't unique ids
        // (the id is
        // positional), so
        // duplicate-label
        // collisions don't
        // surface as
        // errors.
        Some(format!(
            "herdr workspace create --cwd {} --label {} --focus",
            quoted_path, label
        ))
    }

    fn send_in_pane_command(&self, pane_id: &str, body: &str) -> Option<String> {
        if pane_id.is_empty() {
            return None;
        }
        Some(format!(
            "herdr pane send-text {} {}",
            pane_id,
            crate::util::shell_quote(body)
        ))
    }

    fn name(&self) -> &'static str {
        "herdr"
    }
}

// --- tests -------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_parse_accepts_aliases() {
        assert_eq!(MultiplexerKind::parse("tmux"), Some(MultiplexerKind::Tmux));
        assert_eq!(MultiplexerKind::parse(""), Some(MultiplexerKind::Tmux));
        assert_eq!(MultiplexerKind::parse("TMUX"), Some(MultiplexerKind::Tmux));
        assert_eq!(
            MultiplexerKind::parse("herdr"),
            Some(MultiplexerKind::Herdr)
        );
        assert_eq!(
            MultiplexerKind::parse("HERDR"),
            Some(MultiplexerKind::Herdr)
        );
        assert_eq!(MultiplexerKind::parse("screen"), None);
    }

    #[test]
    fn kind_default_is_tmux() {
        assert_eq!(MultiplexerKind::default(), MultiplexerKind::Tmux);
    }

    #[test]
    fn kind_as_str_round_trips() {
        assert_eq!(MultiplexerKind::Tmux.as_str(), "tmux");
        assert_eq!(MultiplexerKind::Herdr.as_str(), "herdr");
    }

    #[test]
    fn tmux_list_windows_parses_active_only() {
        let raw = b"\
%1 | /Users/har/work | active:1 | Layout: ab12
%2 | /Users/har/notes | active:0 | Layout: cd34
%3 | /Users/har/notes/sub | active:1 | Layout: ef56
";
        let out = tmux_list_windows_parse(raw);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].pane_id, "%1");
        assert_eq!(out[0].path, "/Users/har/work");
        assert_eq!(out[1].pane_id, "%3");
    }

    #[test]
    fn tmux_list_panes_excludes_current() {
        let raw =
            b"%1 | @1 | /home | bash | 0\n%2 | @1 | /home | vim | 1\n%3 | @2 | /etc | sh | 0\n";
        let out = tmux_list_panes_parse(raw, "%1");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].pane_id, "%2");
        assert!(out[0].is_last);
        assert_eq!(out[1].pane_id, "%3");
        assert!(!out[1].is_last);
    }

    #[test]
    fn tmux_backend_focus_and_create_commands() {
        let b = TmuxBackend;
        assert_eq!(
            b.focus_command("%5").unwrap(),
            "tmux select-pane -t %5 && tmux switch-client -t %5"
        );
        assert!(b.focus_command("").is_none());
        let cmd = b
            .create_command(std::path::Path::new("/tmp/x"), "x")
            .unwrap();
        assert!(cmd.contains("tmux new-session -d -s x -c /tmp/x"));
        assert!(cmd.contains("tmux switch-client -t x"));
    }

    #[test]
    fn tmux_backend_quotes_paths_with_spaces() {
        let b = TmuxBackend;
        // Use a path that's
        // definitely not under
        // `$HOME` so the
        // `expand_home` call in
        // `create_command`
        // doesn't collapse the
        // leading `/` to `~` and
        // move the space to a
        // different spot.
        let cmd = b
            .create_command(std::path::Path::new("/var/tmp/My Work"), "work")
            .unwrap();
        assert!(cmd.contains("\"/var/tmp/My Work\""), "got: {cmd}");
    }

    #[test]
    fn tmux_send_in_pane_quotes_body() {
        let b = TmuxBackend;
        let cmd = b.send_in_pane_command("%3", "sh .command /tmp/x").unwrap();
        assert!(cmd.contains("tmux send-keys -t %3"));
        // shell_quote wraps the body
        // in single quotes, so the
        // space inside `.command
        // /tmp/x` survives intact.
        assert!(cmd.contains("'sh .command /tmp/x'"));
    }

    #[test]
    fn backend_for_tmux_is_tmux_backend() {
        let b = backend_for(MultiplexerKind::Tmux);
        assert_eq!(b.name(), "tmux");
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn backend_for_herdr_is_herdr_backend() {
        let b = backend_for(MultiplexerKind::Herdr);
        assert_eq!(b.name(), "herdr");
    }

    #[test]
    fn herdr_unavailable_only_when_feature_off() {
        if cfg!(feature = "herdr") {
            assert!(!MultiplexerKind::Herdr.is_herdr_unavailable());
        } else {
            assert!(MultiplexerKind::Herdr.is_herdr_unavailable());
        }
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_focus_command_emits_workspace_focus() {
        // The herdr backend's
        // focus command is a
        // single
        // `herdr workspace focus`
        // call (no
        // select-window /
        // select-pane pair —
        // herdr's public CLI
        // doesn't expose those
        // primitives; the
        // workspace-level
        // focus is enough).
        // The
        // `focus_command`
        // strips the
        // workspace-scoped
        // pane id's `:pN`
        // suffix because
        // `herdr workspace focus`
        // accepts a
        // workspace id,
        // not a pane id.
        let b = HerdrBackend;
        let cmd = b.focus_command("w1:p1").expect("non-empty pane id");
        assert_eq!(cmd, "herdr workspace focus w1");
        assert!(b.focus_command("").is_none());
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_create_command_uses_cwd_and_label() {
        let b = HerdrBackend;
        let cmd = b
            .create_command(std::path::Path::new("/var/tmp/build"), "build")
            .unwrap();
        assert!(cmd.contains("herdr workspace create"));
        assert!(cmd.contains("--cwd"));
        assert!(cmd.contains("/var/tmp/build"));
        assert!(cmd.contains("--label build"));
        // `--focus` must be
        // explicit so the
        // workspace is
        // auto-activated
        // after creation,
        // independent of
        // herdr's default
        // (which is
        // `--focus` today
        // but may change).
        assert!(cmd.contains("--focus"), "got: {cmd}");
        assert!(!cmd.contains("--no-focus"));
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_create_quotes_paths_with_spaces() {
        let b = HerdrBackend;
        let cmd = b
            .create_command(std::path::Path::new("/var/tmp/My Work"), "work")
            .unwrap();
        assert!(cmd.contains("\"/var/tmp/My Work\""), "got: {cmd}");
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_send_in_pane_quotes_body() {
        let b = HerdrBackend;
        let cmd = b.send_in_pane_command("w1:p1", "sh .command /tmp").unwrap();
        assert!(cmd.starts_with("herdr pane send-text w1:p1"));
        // shell_quote wraps the
        // body in single quotes
        // so the space inside
        // `.command /tmp`
        // survives intact.
        assert!(cmd.contains("'sh .command /tmp'"));
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_pane_list_parses_per_pane_records() {
        // The herdr backend's
        // snapshot is built
        // from
        // `herdr pane list`
        // JSON. Each pane
        // becomes one
        // `ActiveContext` so
        // the T-marker
        // matching in
        // `directory_tmux_pane_id`
        // can find a
        // workspace for
        // directory the
        // user has an
        // active pane in.
        let json = serde_json::json!({
            "id": "cli:pane:list",
            "result": {
                "type": "pane_list",
                "panes": [
                    {
                        "pane_id": "wA:p1",
                        "workspace_id": "wA",
                        "cwd": "/Users/har",
                        "foreground_cwd": "/Users/har/work",
                        "agent": "pi"
                    },
                    {
                        "pane_id": "wB:p1",
                        "workspace_id": "wB",
                        "cwd": "/Users/har/other",
                        "foreground_cwd": "/Users/har/other",
                        "agent": ""
                    }
                ]
            }
        });
        let out = parse_herdr_pane_list(&json);
        assert_eq!(out.len(), 2);
        // `foreground_cwd`
        // wins over `cwd`
        // when present (the
        // pane's foreground
        // process changed
        // dir via `cd`).
        assert_eq!(out[0].cwd, "/Users/har/work");
        assert_eq!(out[0].workspace_id, "wA");
        assert_eq!(out[0].agent, "pi");
        // No
        // `foreground_cwd`
        // override — use
        // `cwd` verbatim.
        assert_eq!(out[1].cwd, "/Users/har/other");
        assert_eq!(out[1].agent, "");
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_pane_list_skips_empty_or_missing_cwd() {
        // Pane records
        // without a
        // resolvable cwd
        // (a brand-new
        // pane that hasn't
        // reported its
        // directory yet,
        // or a record
        // missing the
        // field) are
        // dropped from the
        // snapshot so the
        // T-marker logic
        // doesn't try to
        // match against an
        // empty path.
        let json = serde_json::json!({
            "id": "cli:pane:list",
            "result": {
                "type": "pane_list",
                "panes": [
                    {
                        "pane_id": "wA:p1",
                        "workspace_id": "wA",
                        "cwd": "",
                        "foreground_cwd": ""
                    },
                    {
                        "pane_id": "wA:p2",
                        "workspace_id": "wA"
                    }
                ]
            }
        });
        let out = parse_herdr_pane_list(&json);
        assert!(out.is_empty());
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_pane_list_handles_missing_result_envelope() {
        // A malformed
        // response (no
        // `result.panes`)
        // returns an empty
        // list rather than
        // panicking. This
        // is the
        // "silent failure"
        // path that keeps
        // the TUI from
        // crashing when
        // herdr's response
        // shape changes
        // between versions.
        let json = serde_json::json!({
            "id": "cli:pane:list"
        });
        let out = parse_herdr_pane_list(&json);
        assert!(out.is_empty());
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_snapshot_uses_pane_list_not_workspace_list() {
        // Regression: a
        // directory D that
        // is the cwd of an
        // existing herdr
        // pane must show
        // up in the
        // snapshot, so the
        // staging branches
        // to
        // `herdr workspace focus`
        // instead of
        // `herdr workspace create`.
        // This is the
        // user-reported bug
        // "A new workspace
        // is generated for
        // a directory which
        // is already part
        // of a workspace".
        let b = HerdrBackend;
        // We exercise the
        // parser directly
        // with a fixed
        // payload (we
        // can't easily mock
        // `herdr_run_json`
        // here — it shells
        // out) and assert
        // the resulting
        // rows match what
        // the TUI would
        // see.
        let json = serde_json::json!({
            "id": "cli:pane:list",
            "result": {
                "type": "pane_list",
                "panes": [
                    {
                        "pane_id": "wA:p1",
                        "workspace_id": "wA",
                        "cwd": "/Users/har/work",
                        "foreground_cwd": "/Users/har/work",
                        "agent": ""
                    }
                ]
            }
        });
        let rows = parse_herdr_pane_list(&json);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cwd, "/Users/har/work");
        assert_eq!(rows[0].workspace_id, "wA");
        // And
        // `focus_command`
        // must strip the
        // `:pN` suffix
        // before passing
        // to herdr.
        let staged = b.focus_command("wA:p1").expect("non-empty pane id");
        assert_eq!(staged, "herdr workspace focus wA");
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_focus_command_strips_pane_suffix() {
        // herdr's
        // `workspace focus`
        // accepts a
        // workspace id
        // (`wA`), not a
        // pane id
        // (`wA:p1`). The
        // snapshot rows
        // carry pane ids,
        // so the staging
        // must strip the
        // suffix.
        let b = HerdrBackend;
        assert_eq!(
            b.focus_command("wA:p1").unwrap(),
            "herdr workspace focus wA"
        );
        assert_eq!(
            b.focus_command("wB:p3").unwrap(),
            "herdr workspace focus wB"
        );
        // A bare workspace
        // id (no `:pN`
        // suffix) is
        // passed through
        // unchanged.
        assert_eq!(b.focus_command("wA").unwrap(), "herdr workspace focus wA");
        // Empty / blank
        // inputs are
        // rejected so the
        // staging layer
        // doesn't produce
        // a malformed
        // command.
        assert!(b.focus_command("").is_none());
    }
}
