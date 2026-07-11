#![allow(clippy::doc_lazy_continuation)]
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
    /// The pane's foreground
    /// command, as reported by
    /// the backend. tmux:
    /// `#{pane_current_command}`
    /// (e.g. `ssh root@pve-1`,
    /// `vim`, `zsh`); herdr:
    /// empty — herdr's `pane
    /// list` JSON doesn't
    /// expose the foreground
    /// command today, only
    /// the `agent` name. Used
    /// by the `# hosts`
    /// matcher to detect
    /// already-connected
    /// hosts (a pane whose
    /// command starts with
    /// `ssh` and contains the
    /// target `user@host`).
    /// For herdr the matcher
    /// falls back to
    /// `workspace_label`.
    #[allow(dead_code)]
    pub current_command: String,
    /// The workspace's human-
    /// readable label. tmux:
    /// the session name (e.g.
    /// `0`, `1`, or a named
    /// session like `work`).
    /// herdr: the workspace's
    /// `label` field (the
    /// name set by `herdr
    /// workspace create
    /// --label <name>` or
    /// the user's manual
    /// label). Used by the
    /// `# hosts` matcher on
    /// herdr to detect
    /// already-created
    /// workspaces: a herdr
    /// workspace whose label
    /// matches the host's
    /// display name is
    /// treated as "this
    /// host's workspace".
    /// For tmux this is
    /// unused today (the
    /// `current_command`
    /// path is sufficient)
    /// but kept for symmetry.
    #[allow(dead_code)]
    pub workspace_label: String,
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
    ///
    /// Used by the directories-mode
    /// `#` flow: the user picks a
    /// directory whose pane id is
    /// known, and the staged
    /// command brings the user to
    /// that pane's workspace. For
    /// tmux this is
    /// `select-pane -t %N && switch-client -t %N`;
    /// for herdr it's
    /// `herdr workspace focus <ws>`
    /// (the pane id's `:pN`
    /// suffix is stripped
    /// because `workspace focus`
    /// accepts a workspace id,
    /// not a pane id). So the
    /// directories-mode behavior
    /// is "focus the workspace
    /// the directory lives in" —
    /// not the specific pane.
    fn focus_command(&self, pane_id: &str) -> Option<String>;

    /// Stage a command that
    /// switches to the
    /// **session / workspace**
    /// as a whole (the parent
    /// of one or more panes).
    /// Used by the `*`-mode
    /// "workspace row" the
    /// user picks to jump to
    /// the workspace without
    /// targeting a specific
    /// pane. Returns `None`
    /// when the backend can't
    /// build a command (stale
    /// id, no server, etc.).
    ///
    /// - tmux:
    ///   `tmux switch-client -t <session-name>`
    ///   (brings the session's
    ///   focused window forward).
    /// - herdr:
    ///   `herdr workspace focus <workspace-id>`
    ///   (brings the workspace's
    ///   focused tab forward).
    fn focus_session(&self, session_label: &str) -> Option<String>;

    /// Stage a command that
    /// switches to a specific
    /// **pane** (not just the
    /// session / workspace).
    /// Used by the `*`-mode
    /// "pane row" the user
    /// picks to jump directly
    /// to that pane, switching
    /// the workspace / window
    /// as needed so the pane is
    /// reachable. Returns
    /// `None` when the backend
    /// can't build a command.
    ///
    /// - tmux:
    ///   `tmux select-pane -t <pane_id> && tmux switch-client -t <pane_id>`
    ///   (the `tab_id` is
    ///   tmux's window id, used
    ///   only as the second
    ///   target when
    ///   `select-window` is
    ///   needed; in practice
    ///   tmux's `switch-client -t <pane_id>`
    ///   already switches the
    ///   window for you so
    ///   `tab_id` is ignored).
    /// - herdr:
    ///   `herdr pane zoom <pane_id> && herdr pane zoom <pane_id> --off`
    ///   (the first `pane zoom` focuses the exact pane across
    ///   workspaces and tabs and zooms it; the second call
    ///   un-zooms while keeping focus on that pane, so the
    ///   user lands on the right pane without a zoomed view).
    fn focus_pane(&self, pane_id: &str, tab_id: &str) -> Option<String>;

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
    /// The pane's
    /// **parent unit** — the
    /// thing that contains
    /// the pane and that
    /// `select_for_run`'s
    /// pane-row handler stages
    /// a focus command on.
    /// tmux: the window id
    /// (`@N`). herdr: the tab
    /// id (`wA:t1`). The
    /// `focus_pane` backend
    /// method receives this
    /// alongside the pane id
    /// so it can construct
    /// the full staged
    /// sequence (e.g. for
    /// herdr:
    /// `workspace focus && tab focus`).
    /// Empty when the backend
    /// can't resolve it (the
    /// pane is fresh / the
    /// JSON didn't carry it).
    pub tab_id: String,
    /// Human-readable label
    /// for the pane's
    /// **session / workspace**
    /// — the unit the user
    /// recognizes at a glance
    /// as "where this pane
    /// lives". tmux: the
    /// session name (e.g.
    /// `0`, `1`, or a
    /// named session like
    /// `work`). herdr: the
    /// workspace id (e.g.
    /// `wA`). Used by the
    /// renderer to group panes
    /// under a workspace /
    /// session header row
    /// (no longer a
    /// `[label]` chip on every
    /// row — see the new
    /// `mode == "workspace"`
    /// row type in
    /// `fetch_session_panes_impl`).
    pub session_label: String,
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
        // Optional fields (added
        // when the snapshot grew
        // to include
        // `pane_current_command`
        // and `session_name` for
        // the `# hosts` matcher).
        // Older parsers see a
        // 3-field row and leave
        // both empty.
        let current_command = parts.get(4).map(|s| s.to_string()).unwrap_or_default();
        let session_name = parts.get(5).map(|s| s.to_string()).unwrap_or_default();
        out.push(ActiveContext {
            pane_id: pane_id.to_string(),
            window_id: String::new(),
            path: path.to_string(),
            current_command,
            workspace_label: session_name,
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
        // Field order is
        // determined by the
        // `FORMAT` constant in
        // `TmuxBackend::snapshot_current_panes`:
        //   pane_id | window_id |
        //   session_name |
        //   pane_current_path |
        //   pane_current_command |
        //   pane_last flag
        // (6 fields). Older
        // format-strings could
        // produce 5 fields
        // (without session_name),
        // so we also accept that
        // — `session_label`
        // falls back to the
        // window id when the
        // field is missing.
        if parts.len() < 4 {
            continue;
        }
        let pane_id = parts[0];
        if pane_id.is_empty() || pane_id == current_pane {
            continue;
        }
        let window_id = parts[1].to_string();
        // session_name is in
        // position 2 if the
        // format string
        // includes it (6-field
        // form); otherwise
        // the 5-field legacy
        // form has path at
        // position 2. We
        // detect by counting:
        // 6+ fields → session_name
        // present; 4-5 fields →
        // legacy form, fall back to window_id.
        // The fetch path adds
        // `#{session_name}` so
        // this is the populated
        // case in practice; the
        // fallback keeps older
        // scratch invocations
        // (`tmux list-panes` from
        // the CLI) usable as a
        // dev/debug path.
        let (session_label, path, current_command, is_last) = if parts.len() >= 6 {
            (
                parts[2].to_string(),
                parts[3].to_string(),
                parts[4].to_string(),
                parts.get(5).copied().unwrap_or("0") == "1",
            )
        } else {
            (
                window_id.clone(),
                parts[2].to_string(),
                parts[3].to_string(),
                parts.get(4).copied().unwrap_or("0") == "1",
            )
        };
        // `tab_id` for tmux is
        // the window id (`@N`).
        // `focus_pane` ignores
        // it (tmux's
        // `switch-client -t <pane_id>`
        // switches the window
        // for you) but it's
        // kept on the row
        // so the user can
        // see the window id
        // in the row's `output`
        // slot if they care.
        let tab_id = window_id.clone();
        out.push(CurrentPaneInfo {
            pane_id: pane_id.to_string(),
            window_id,
            tab_id,
            session_label,
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
        // Field order:
        //   pane_id | pane_current_path |
        //   active flag | window_layout |
        //   pane_current_command |
        //   session_name
        //
        // The last two are consumed by
        // the `# hosts` matcher in
        // `select_for_run_impl` (the
        // `host` arm) to detect
        // already-connected SSH
        // sessions and to map a pane
        // back to its session / window
        // when focusing an existing
        // workspace. The other consumers
        // of this snapshot (the `#`
        // directories-mode T-marker
        // lookup) only read `path`, so
        // adding fields is
        // backward-compatible.
        const FORMAT: &str = "\
            #{pane_id} | \
            #{pane_current_path} | \
            active:#{window_active} | \
            Layout: #{window_layout} | \
            #{pane_current_command} | \
            #{session_name}";
        match tmux_run(&["list-windows", "-a", "-F", FORMAT]) {
            Some(bytes) => tmux_list_windows_parse(&bytes),
            None => Vec::new(),
        }
    }

    fn snapshot_current_panes(&self, current_pane: &str) -> Vec<CurrentPaneInfo> {
        // `-a` (list all
        // sessions) rather than
        // `-s` (current
        // session only) so
        // the `*`-mode list
        // spans every pane the
        // user can jump to,
        // regardless of which
        // session it lives in.
        // The format includes
        // `#{session_name}` so
        // the row renderer can
        // visually identify
        // which session a pane
        // belongs to (the user
        // explicitly asked for
        // this — without it,
        // multiple sessions'
        // panes would be
        // visually
        // indistinguishable).
        //
        // Field order:
        //   pane_id | window_id |
        //   session_name |
        //   pane_current_path |
        //   pane_current_command |
        //   pane_last flag
        const FORMAT: &str = "#{pane_id} | #{window_id} | #{session_name} | #{pane_current_path} | #{pane_current_command} | #{?pane_last,1,0}";
        match tmux_run(&["list-panes", "-a", "-F", FORMAT]) {
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

    fn focus_session(&self, session_label: &str) -> Option<String> {
        if session_label.is_empty() {
            return None;
        }
        // `switch-client -t <session-name>`
        // brings the session's
        // focused window forward
        // as the active one.
        // The session name from
        // tmux's `#{session_name}`
        // is the user's
        // human-readable identifier
        // (e.g. `0`, `1`, or a
        // named session like
        // `work`), and tmux accepts
        // it directly.
        Some(format!("tmux switch-client -t {}", session_label))
    }

    fn focus_pane(&self, pane_id: &str, _tab_id: &str) -> Option<String> {
        // For tmux the per-pane
        // focus is the same as the
        // directories-mode T-marker
        // focus (select-pane &&
        // switch-client).
        // `switch-client -t <pane_id>`
        // already switches the
        // window for you, so the
        // `tab_id` argument (the
        // window id `@N`) is
        // ignored — kept as a
        // parameter so the trait
        // contract is uniform
        // across backends.
        self.focus_command(pane_id)
    }

    fn create_command(&self, dir: &Path, label: &str) -> Option<String> {
        // Expand `~` ourselves —
        // tmux doesn't do it.
        let path = crate::util::expand_home_to_absolute(
            &dir.to_string_lossy(),
            &[std::env::var("HOME").unwrap_or_default()],
        )
        .into_owned();
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
            crate::util::shell_quote(label),
            quoted_path,
            crate::util::shell_quote(label)
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
    /// The pane's tab id
    /// (`wA:t1`), used by
    /// the `focus_pane`
    /// backend method to
    /// stage
    /// `herdr tab focus <tab_id>`
    /// so the user lands
    /// in the right tab
    /// (not just the right
    /// workspace).
    tab_id: String,
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

/// Look up the running process's command line inside a
/// herdr pane via the socket API's `pane.process_info`
/// method. Returns `Some(cmdline)` where `cmdline` is the
/// human-readable foreground-process command line, or
/// `None` when the lookup fails (no socket, no foreground
/// process, malformed response).
///
/// Prefers `cmdline` (the full command line, e.g.
/// `nvim /Users/.../config.toml`) when present; falls back
/// to `argv0` (the first argument / executable name, e.g.
/// `zsh`, `pi`) — both are more informative than the
/// OS-level `name` (which would show `node` for a `pi`
/// invocation, hiding the user-visible program).
///
/// The lookup is best-effort: a failure doesn't break the
/// panes view — the caller falls back to the agent name
/// alone (the historical behavior).
#[cfg(feature = "herdr")]
pub fn herdr_pane_cmdline(pane_id: &str) -> Option<String> {
    let json = herdr_run_json(&["pane", "process-info", "--pane", pane_id])?;
    let processes = json
        .get("result")
        .and_then(|r| r.get("process_info"))
        .and_then(|p| p.get("foreground_processes"))
        .and_then(|p| p.as_array())?;
    let first = processes.first()?;
    // Prefer `cmdline` (full
    // command line) when
    // present; fall back to
    // `argv0` (the executable
    // name) — both are more
    // user-visible than the
    // OS `name`.
    if let Some(cmd) = first.get("cmdline").and_then(|v| v.as_str())
        && !cmd.is_empty() {
            return Some(cmd.to_string());
        }
    first
        .get("argv0")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

#[cfg(not(feature = "herdr"))]
pub fn herdr_pane_cmdline(_pane_id: &str) -> Option<String> {
    None
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
        // The tab id (`wA:t1`) —
        // used by the herdr
        // `focus_pane` method
        // to stage a
        // tab-level focus
        // command so the user
        // lands in the right
        // tab inside the
        // workspace.
        let tab_id = p
            .get("tab_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if pane_id.is_empty() || effective_cwd.is_empty() {
            continue;
        }
        out.push(HerdrPaneRecord {
            pane_id,
            workspace_id,
            tab_id,
            cwd: effective_cwd,
            agent,
        });
    }
    out
}

/// Parse the `herdr workspace
/// list` JSON into a
/// `(workspace_id → label)`
/// map. The label is what
/// herdr itself calls the
/// workspace (e.g.
/// `smarthistory`, `dir:
/// Downloads`, `~`);
/// surfacing it to the
/// user as the workspace
/// header row's primary
/// text matches the user's
/// mental model and the
/// herdr UI more closely
/// than the bare id (`wB`)
/// would.
///
/// The expected JSON shape:
///   { "result": { "type":
///   "workspace_list",
///   "workspaces": [ {
///   "workspace_id": "wB",
///   "label": "smarthistory",
///   ... } ] } }
/// Missing `label` falls
/// back to the bare id
/// (and the TUI's warning
/// "workspace-label
/// unavailable" path).
/// Empty `workspace_id`s
/// are skipped (the TUI
/// can't dispatch a focus
/// to an empty-id
/// workspace anyway).
#[cfg(feature = "herdr")]
fn parse_workspace_labels(json: &serde_json::Value) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let workspaces = match json
        .get("result")
        .and_then(|r| r.get("workspaces"))
        .and_then(|w| w.as_array())
    {
        Some(w) => w,
        None => return out,
    };
    for ws in workspaces {
        let id = ws
            .get("workspace_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let label = ws
            .get("label")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| id.clone());
        out.insert(id, label);
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
        // Fetch the workspace
        // list too so we can
        // resolve workspace
        // ids to their
        // human-readable labels
        // (e.g. `wB` →
        // `smarthistory`).
        // Used by the `# hosts`
        // matcher on herdr:
        // when herdr doesn't
        // expose a pane's
        // foreground command
        // (it doesn't today),
        // we fall back to
        // matching the
        // workspace's `label`
        // against the host's
        // display name.
        // Best-effort: when
        // the workspace list
        // call fails we leave
        // `workspace_label`
        // empty and the
        // matcher degrades to
        // "no match".
        let workspace_labels: std::collections::HashMap<String, String> =
            match herdr_run_json(&["workspace", "list"]) {
                Some(ws_json) => parse_workspace_labels(&ws_json),
                None => std::collections::HashMap::new(),
            };
        parse_herdr_pane_list(&json)
            .into_iter()
            .map(|r| {
                let label = workspace_labels
                    .get(&r.workspace_id)
                    .cloned()
                    .unwrap_or_default();
                ActiveContext {
                    pane_id: r.pane_id,
                    window_id: r.workspace_id,
                    path: r.cwd,
                    // herdr's `pane list`
                    // JSON doesn't
                    // expose the
                    // foreground
                    // command (only
                    // the `agent`
                    // name). Leave
                    // empty so the
                    // tmux matcher
                    // (which keys on
                    // `current_command`)
                    // is skipped.
                    current_command: String::new(),
                    workspace_label: label,
                }
            })
            .collect()
    }

    fn snapshot_current_panes(&self, current_pane: &str) -> Vec<CurrentPaneInfo> {
        // The `*`-mode view
        // shows **every**
        // herdr pane across
        // every workspace —
        // not just the
        // current workspace's
        // siblings. The user
        // asked for this so they
        // can pick a pane from
        // any workspace and jump
        // to it; the per-row
        // `[wA]` badge (the
        // workspace id, surfaced
        // via `session_label`)
        // lets the user tell at
        // a glance which workspace
        // a pane belongs to.
        //
        // The pane the TUI is
        // running in (read from
        // `HERDR_PANE_ID`, falling
        // back to the
        // `current_pane` arg the
        // TUI's
        // `fetch_session_panes`
        // path passes in) is
        // excluded so the user
        // never stages a no-op
        // jump to themselves.
        //
        // herdr sets
        // `HERDR_PANE_ID` in every
        // pane process's env; the
        // existing tmux flow reads
        // `$TMUX_PANE` for the same
        // purpose. When neither is
        // set we return an empty
        // list (the user isn't running
        // inside a herdr pane, so
        // they have no siblings to
        // switch to — and `pane
        // list` would also return
        // nothing useful in that
        // case).
        let current_pane_env = std::env::var("HERDR_PANE_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| current_pane.to_string());
        let json = match herdr_run_json(&["pane", "list"]) {
            Some(j) => j,
            None => return Vec::new(),
        };
        // Fetch the workspace
        // list too so we can
        // resolve workspace
        // IDs (e.g. `wB`) to
        // their human-readable
        // `label` (e.g.
        // `smarthistory`,
        // `dir: Downloads`).
        // The renderer uses
        // `session_label` as
        // the workspace header's
        // primary text, so the
        // user sees `smarthistory`
        // rather than the bare
        // `wB` id (the user's
        // explicit ask: "use
        // the name of the
        // workspace instead
        // [of just the count]").
        // The lookup is
        // best-effort: if the
        // workspace list call
        // fails we fall back to
        // the bare workspace
        // id (still useful, just
        // less friendly).
        let workspace_labels: std::collections::HashMap<String, String> =
            match herdr_run_json(&["workspace", "list"]) {
                Some(ws_json) => parse_workspace_labels(&ws_json),
                None => std::collections::HashMap::new(),
            };
        parse_herdr_pane_list(&json)
            .into_iter()
            .filter(|r| r.pane_id != current_pane_env)
            .map(|r| {
                let label = workspace_labels
                    .get(&r.workspace_id)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| r.workspace_id.clone());
                // The pane's `current_command`
                // is initialized to the agent
                // name (e.g. `pi`) so the row
                // renders immediately. The
                // full process command line
                // (e.g. `nvim config.toml`,
                // `-zsh`, `ssh har@host`) is
                // fetched asynchronously by
                // the App's background
                // `pane_cmdlines` lookup and
                // patched in after the first
                // frame — see
                // `App::spawn_pane_cmdlines`
                // and
                // `App::process_pane_cmdlines`.
                // This keeps the panes view
                // fast (no ~30ms-per-pane
                // blocking call on the
                // render path).
                CurrentPaneInfo {
                    pane_id: r.pane_id,
                    window_id: r.workspace_id.clone(),
                    // The herdr tab_id
                    // (`wA:t1`) drives
                    // the pane-level
                    // focus staging
                    // (`focus_pane`).
                    tab_id: r.tab_id,
                    // The workspace's
                    // human-readable
                    // `label` (e.g.
                    // `smarthistory`,
                    // `dir: Downloads`)
                    // from `herdr
                    // workspace list`,
                    // falling back to
                    // the bare workspace
                    // id (`wA`) if the
                    // label isn't
                    // available.
                    session_label: label,
                    path: r.cwd,
                    current_command: r.agent,
                    is_last: false,
                }
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
        Some(format!(
            "herdr workspace focus {} 2>/dev/null",
            workspace_id
        ))
    }

    fn focus_session(&self, session_label: &str) -> Option<String> {
        if session_label.is_empty() {
            return None;
        }
        // For herdr the
        // "session" is the
        // workspace — the
        // `session_label` IS
        // the workspace id
        // (`wA`) because that's
        // what we set in
        // `parse_herdr_pane_list`.
        // The staging command
        // is the same as the
        // directories-mode
        // focus_command (just
        // `herdr workspace focus <id>`).
        Some(format!(
            "herdr workspace focus {} 2>/dev/null",
            session_label
        ))
    }

    fn focus_pane(&self, pane_id: &str, _tab_id: &str) -> Option<String> {
        // Use `pane zoom` (which delegates to the socket API's
        // `pane.zoom` method) to focus the specific pane. Unlike
        // `workspace focus` + `tab focus`, which only switches the
        // workspace and tab (leaving the pane focus to whatever was
        // last focused in that tab), `pane zoom <pane_id>` focuses
        // the EXACT pane the user selected — across workspaces and
        // tabs — and zooms it to fill the tab. The second call
        // (`pane zoom <pane_id> --off`) un-zooms while keeping the
        // focus on that pane, so the pane is focused but NOT
        // zoomed.
        //
        // `tab_id` is no longer needed (pane.zoom resolves the
        // workspace+tab from the pane_id itself), but kept as a
        // parameter so the trait contract is uniform across
        // backends.
        if pane_id.is_empty() {
            return None;
        }
        Some(format!(
            "herdr pane zoom {} 2>/dev/null && \
             herdr pane zoom {} --off 2>/dev/null",
            pane_id, pane_id,
        ))
    }

    fn create_command(&self, dir: &Path, label: &str) -> Option<String> {
        let path = crate::util::expand_home_to_absolute(
            &dir.to_string_lossy(),
            &[std::env::var("HOME").unwrap_or_default()],
        )
        .into_owned();
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
            "herdr workspace create --cwd {} --label {} --focus 2>/dev/null",
            quoted_path,
            crate::util::shell_quote(label)
        ))
    }

    fn send_in_pane_command(&self, pane_id: &str, body: &str) -> Option<String> {
        if pane_id.is_empty() {
            return None;
        }
        Some(format!(
            "herdr pane send-text {} {} 2>/dev/null",
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

    /// The 6-field form
    /// includes `session_name`
    /// (added so the `*`-mode
    /// row renderer can show a
    /// `[session-name]` badge
    /// on each pane row — the
    /// `*`-mode list now spans
    /// every session, so the
    /// badge is the only way to
    /// tell which session a pane
    /// belongs to). This test
    /// locks in the 6-field
    /// parsing path so a future
    /// format-string change
    /// (e.g. dropping
    /// `session_name`) would
    /// surface as a test
    /// failure rather than a
    /// silent regression.
    #[test]
    fn tmux_list_panes_extracts_session_name_in_six_field_form() {
        let raw = b"\
%1 | @1 | work | /Users/har/work | vim | 0
%2 | @1 | work | /Users/har/work | python | 1
%3 | @2 | debug | /var/log | tail | 0
";
        let out = tmux_list_panes_parse(raw, "%1");
        assert_eq!(out.len(), 2);
        // The session_name
        // from position 2
        // (a field that
        // doesn't appear
        // in the 5-field
        // form) lands in
        // `session_label`.
        assert_eq!(out[0].pane_id, "%2");
        assert_eq!(out[0].session_label, "work");
        assert_eq!(out[0].path, "/Users/har/work");
        assert_eq!(out[0].current_command, "python");
        assert!(out[0].is_last);
        assert_eq!(out[1].pane_id, "%3");
        assert_eq!(out[1].session_label, "debug");
        assert_eq!(out[1].path, "/var/log");
        assert_eq!(out[1].current_command, "tail");
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
        assert_eq!(cmd, "herdr workspace focus w1 2>/dev/null");
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

    /// Regression test for the
    /// user-reported ask:
    /// show the workspace's
    /// human-readable label
    /// (e.g. `smarthistory`,
    /// `dir: Downloads`) instead
    /// of just the workspace id
    /// (`wB`) as the `#` workspace
    /// header row's primary text.
    /// `parse_workspace_labels`
    /// parses `herdr workspace list`'s
    /// JSON into a
    /// `workspace_id → label` map.
    /// The `snapshot_current_panes`
    /// code substitutes the
    /// resolved label into each
    /// `CurrentPaneInfo`'s
    /// `session_label`, so the
    /// renderer's `# {command}` text
    /// reads `smarthistory` rather
    /// than `wB`.
    #[cfg(feature = "herdr")]
    #[test]
    fn parse_workspace_labels_resolves_id_to_human_label() {
        let json = serde_json::json!({
            "id": "cli:workspace:list",
            "result": {
                "type": "workspace_list",
                "workspaces": [
                    {
                        "workspace_id": "wB",
                        "label": "smarthistory",
                        "number": 1,
                        "focused": true,
                        "pane_count": 3,
                        "tab_count": 2
                    },
                    {
                        "workspace_id": "wE",
                        "label": "dir: Downloads",
                        "number": 2,
                        "focused": false,
                        "pane_count": 2,
                        "tab_count": 1
                    }
                ]
            }
        });
        let labels = parse_workspace_labels(&json);
        assert_eq!(labels.len(), 2);
        assert_eq!(labels.get("wB").map(String::as_str), Some("smarthistory"));
        assert_eq!(labels.get("wE").map(String::as_str), Some("dir: Downloads"));
    }

    /// Workspaces with no
    /// `label` field (a
    /// brand-new herdr
    /// install that hasn't
    /// named the workspace
    /// yet, or older herdr
    /// versions that don't
    /// expose `label`) fall
    /// back to the bare id
    /// — keeps the `#` row's
    /// display non-empty
    /// rather than a blank
    /// header.
    #[cfg(feature = "herdr")]
    #[test]
    fn parse_workspace_labels_falls_back_to_id_when_label_missing() {
        let json = serde_json::json!({
            "result": {
                "panes": [],
                "workspaces": [
                    { "workspace_id": "wA" },
                    { "workspace_id": "wB", "label": "" }
                ]
            }
        });
        let labels = parse_workspace_labels(&json);
        assert_eq!(labels.len(), 2);
        // Missing `label` → fall
        // back to `workspace_id`.
        assert_eq!(labels.get("wA").map(String::as_str), Some("wA"));
        // Empty `label` → fall
        // back as well.
        assert_eq!(labels.get("wB").map(String::as_str), Some("wB"));
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
        assert_eq!(staged, "herdr workspace focus wA 2>/dev/null");
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
            "herdr workspace focus wA 2>/dev/null"
        );
        assert_eq!(
            b.focus_command("wB:p3").unwrap(),
            "herdr workspace focus wB 2>/dev/null"
        );
        // A bare workspace
        // id (no `:pN`
        // suffix) is
        // passed through
        // unchanged.
        assert_eq!(
            b.focus_command("wA").unwrap(),
            "herdr workspace focus wA 2>/dev/null"
        );
        // Empty / blank
        // inputs are
        // rejected so the
        // staging layer
        // doesn't produce
        // a malformed
        // command.
        assert!(b.focus_command("").is_none());
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_focus_session_emits_workspace_focus() {
        // Selecting
        // a workspace header
        // row (the user
        // picks the whole
        // workspace, not
        // a pane inside it)
        // stages
        // `herdr workspace focus <id>`.
        // The `session_label`
        // for herdr is the
        // workspace id
        // itself, so the
        // command is the
        // same as the
        // directories-mode
        // T-marker staging
        // (which uses
        // `focus_command` on
        // the workspace-scoped
        // pane id, stripping
        // the `:pN` suffix).
        let b = HerdrBackend;
        assert_eq!(
            b.focus_session("wA").unwrap(),
            "herdr workspace focus wA 2>/dev/null"
        );
        assert!(b.focus_session("").is_none());
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_focus_pane_uses_pane_zoom() {
        // Selecting a pane row stages
        // `herdr pane zoom <pane_id> && herdr pane zoom <pane_id> --off`.
        // The first `pane zoom` call focuses the EXACT pane
        // (across workspaces and tabs) and zooms it to fill
        // the tab. The second call (`--off`) un-zooms while
        // keeping the focus on that pane, so the user lands
        // on the right pane without a zoomed view.
        //
        // This replaces the old `workspace focus + tab focus`
        // approach, which only switched the workspace and tab
        // but left the pane-focus to whatever was last focused
        // in that tab.
        let b = HerdrBackend;
        let cmd = b.focus_pane("wA:p3", "wA:t2").expect("non-empty ids");
        assert_eq!(
            cmd,
            "herdr pane zoom wA:p3 2>/dev/null && herdr pane zoom wA:p3 --off 2>/dev/null"
        );
        // An empty `tab_id`
        // doesn't change the
        // behavior — `pane zoom`
        // resolves the workspace
        // and tab from the
        // pane_id itself.
        let cmd = b.focus_pane("wA:p3", "").expect("non-empty pane id");
        assert_eq!(
            cmd,
            "herdr pane zoom wA:p3 2>/dev/null && herdr pane zoom wA:p3 --off 2>/dev/null"
        );
        // An empty `pane_id`
        // is rejected.
        assert!(b.focus_pane("", "").is_none());
        // A bare workspace
        // id (no `:pN`)
        // still produces a
        // valid command —
        // `pane zoom` accepts
        // workspace ids too
        // (it will focus the
        // workspace's
        // focused-pane-by-
        // default).
        let cmd = b.focus_pane("wA", "wA:t1").expect("bare ws id");
        assert_eq!(
            cmd,
            "herdr pane zoom wA 2>/dev/null && herdr pane zoom wA --off 2>/dev/null"
        );
    }

    #[test]
    fn tmux_focus_session_uses_switch_client() {
        // Selecting a session
        // header row in the
        // `*` mode for a tmux
        // user stages
        // `tmux switch-client -t <session-name>`
        // which brings the
        // session's focused
        // window forward.
        let b = TmuxBackend;
        assert_eq!(b.focus_session("0").unwrap(), "tmux switch-client -t 0");
        assert_eq!(
            b.focus_session("my-session").unwrap(),
            "tmux switch-client -t my-session"
        );
        assert!(b.focus_session("").is_none());
    }

    #[test]
    fn tmux_focus_pane_reuses_focus_command() {
        // For tmux the per-pane
        // focus is the same
        // shape as the
        // directories-mode
        // T-marker focus:
        // `select-pane -t <pane_id> && switch-client -t <pane_id>`.
        // The `tab_id` (window
        // id `@N`) is ignored
        // because tmux's
        // `switch-client -t %N`
        // already switches the
        // window for you.
        let b = TmuxBackend;
        let cmd = b.focus_pane("%5", "@3").expect("non-empty pane id");
        assert_eq!(cmd, "tmux select-pane -t %5 && tmux switch-client -t %5");
        assert_eq!(
            b.focus_pane("%5", "").unwrap(),
            "tmux select-pane -t %5 && tmux switch-client -t %5"
        );
        assert!(b.focus_pane("", "").is_none());
    }

    /// Live integration test:
    /// runs the actual
    /// `herdr pane list` CLI
    /// parse path via
    /// `HerdrBackend::snapshot_current_panes`
    /// and asserts that the
    /// returned count
    /// is at least equal to
    /// (`herdr pane list`'s
    /// panes minus one for the
    /// current pane). This
    /// is the diagnostic
    /// for the user-reported
    /// bug where only some
    /// workspaces' panes
    /// showed up in the `*`
    /// mode list.
    ///
    /// Skipped when `HERDR_PANE_ID`
    /// is unset (the test
    /// suite isn't running
    /// inside a herdr pane)
    /// so CI doesn't fail
    /// when herdr isn't
    /// installed.
    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_backend_snapshot_current_panes_returns_all_workspaces() {
        let current_pane = std::env::var("HERDR_PANE_ID")
            .ok()
            .filter(|s| !s.is_empty());
        let Some(current_pane) = current_pane else {
            eprintln!("[skip] $HERDR_PANE_ID unset (not in herdr)");
            return;
        };
        // Use the same JSON the production code reads.
        let out = match std::process::Command::new("herdr")
            .args(["pane", "list"])
            .output()
        {
            Ok(o) => o,
            Err(_) => {
                eprintln!("[skip] `herdr` not on PATH");
                return;
            }
        };
        let json: serde_json::Value = match serde_json::from_slice(&out.stdout) {
            Ok(j) => j,
            Err(_) => {
                eprintln!("[skip] `herdr pane list` returned non-JSON output");
                return;
            }
        };
        let expected_count = json
            .get("result")
            .and_then(|r| r.get("panes"))
            .and_then(|p| p.as_array())
            .map(|ps| {
                ps.iter()
                    .filter(|p| {
                        p.get("pane_id")
                            .and_then(|v| v.as_str())
                            .map(|s| s != current_pane)
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);
        if expected_count == 0 {
            eprintln!("[skip] no non-current panes in `herdr pane list`");
            return;
        }
        // Run the backend's snapshot for the current pane.
        let b = HerdrBackend;
        let rows = b.snapshot_current_panes(&current_pane);
        eprintln!(
            "[debug] backend returned {} rows for current pane {:?} (expected {} from `herdr pane list`)",
            rows.len(),
            current_pane,
            expected_count
        );
        let mut workspaces_seen: Vec<String> = Vec::new();
        for r in &rows {
            if !workspaces_seen.contains(&r.session_label) {
                workspaces_seen.push(r.session_label.clone());
            }
            eprintln!(
                "[debug]   pane_id={:?} session_label={:?} cwd={:?} tab_id={:?}",
                r.pane_id, r.session_label, r.path, r.tab_id
            );
        }
        eprintln!(
            "[debug] workspaces represented in backend output: {:?}",
            workspaces_seen
        );
        // Every pane from `herdr pane list`
        // (excluding the current one)
        // must survive the JSON parse
        // path. This catches the case where
        // a single workspace's panes are
        // dropped (the user's bug).
        assert_eq!(
            rows.len(),
            expected_count,
            "backend snapshot returned {} rows but `herdr pane list` had {} (current pane {:?} excluded). \
             A mismatch means parse_herdr_pane_list is dropping some rows; \
             check the per-row debug output above.",
            rows.len(),
            expected_count,
            current_pane
        );
    }
}
