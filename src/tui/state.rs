#![allow(clippy::doc_lazy_continuation)]
// Data model used across the TUI: search scope (Mode), the row
// representation loaded from SQLite (HistoryRow), the pick mode
// returned from the line-editor widget (PickMode), the exit-code
// filter (ExitFilter), and the constants consumed by the shell
// (exit_code).

/// Search scope for the TUI. Mirrors the line-editor widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Sess,
    Dir,
    Global,
    /// Rank the global history by:
    ///   1. probability of following the most-recently-executed
    ///      command (via SQLite's `LEAD()` window function),
    ///   2. age (newest first).
    /// The "last command" is determined across the whole global
    /// history so the view is reproducible across mode switches.
    Stats,
}

impl Mode {
    pub fn next(self) -> Self {
        match self {
            Mode::Sess => Mode::Dir,
            Mode::Dir => Mode::Global,
            Mode::Global => Mode::Stats,
            Mode::Stats => Mode::Sess,
        }
    }
    /// Parse a string like "SESS", "SESSION", "DIR", "DIRECTORY",
    /// "GLOBAL", "STATS", "STATISTICS" (case-insensitive). Returns
    /// None for anything else.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "SESS" | "SESSION" => Some(Mode::Sess),
            "DIR" | "DIRECTORY" => Some(Mode::Dir),
            "GLOBAL" => Some(Mode::Global),
            "STATS" | "STATISTICS" => Some(Mode::Stats),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct HistoryRow {
    pub id: i64,
    pub command: String,
    pub directory: String,
    pub session_id: String,
    pub exit_code: i32,
    pub timestamp: i64,
    pub comment: String,
    pub output: String,
    /// The mode/type of this history entry: "command", "llm", or "question".
    pub mode: String,
    /// Sub-source tag for
    /// directory rows
    /// (`mode ==
    /// "directory"`):
    /// one of `"history"`,
    /// `"sessiondir"`,
    /// `"tmux"`. Empty
    /// for non-directory
    /// rows. The TUI
    /// uses this to filter
    /// the `#`-mode list
    /// by the
    /// `directory_source`
    /// filter (ALL / TMUX
    /// / CFG).
    pub source: String,
    /// Workspace or session
    /// name that the row
    /// belongs to in the
    /// `*`-mode tree. Set on
    /// every `pane` row by
    /// `fetch_session_panes_impl`
    /// so the renderer can show
    /// a `[SmartHistory]`-style
    /// badge next to each pane,
    /// and so the group-aware
    /// filter in `fetch_panes`
    /// can attribute child
    /// panes back to their
    /// parent workspace without
    /// re-walking the row list.
    /// Empty on every non-pane
    /// row.
    ///
    /// For tmux this is the
    /// session name (e.g. `0`,
    /// `1`, or a named session
    /// like `work`); for herdr
    /// it's the workspace label
    /// (e.g. `SmartHistory`,
    /// `dir: Downloads`).
    pub workspace_label: String,

    /// CodeGraph symbol-node id for rows produced by the
    /// `&` (codegraph) mode and the `$` (tags) → CodeGraph
    /// fallback. Empty for every other row. Stashed here so
    /// the details pane can resolve the symbol's callers /
    /// callees (`edges` with `kind='calls'`) without
    /// re-running the FTS search to recover the id.
    pub codegraph_node_id: String,

    /// Last-N-lines capture of the row's underlying source, set
    /// lazily by per-mode `ensure_selected_context` helpers so
    /// the output-preview pane can show the user the content
    /// they're about to interact with. For herdr pane rows
    /// (the `*` panes mode) this is the output of
    /// `herdr pane read <pane_id> --lines 50` — the
    /// tail of whatever the agent / shell is currently
    /// displaying in that pane. Empty for every other row, and
    /// for pane rows whose content hasn't been read yet.
    /// Kept separate from `output` (which is the pane's
    /// `tab_id` used by `focus_pane` and is also where other
    /// modes dump their preview text) so the two uses
    /// don't fight over the same field.
    pub preview: String,

    /// Hint for the output
    /// preview renderer: the
    /// desired vertical scroll
    /// offset (in lines) when
    /// rendering this row's
    /// preview text. The
    /// modes that load a
    /// windowed source context
    /// (`tags`, `ag`,
    /// `codegraph`,
    /// `segments`,
    /// `similar`) use this
    /// to scroll the
    /// `Paragraph` so the
    /// matched line is visible
    /// in the typically-shorter
    /// preview pane (the loaded
    /// context is
    /// `SOURCE_CONTEXT_LINES`
    /// = 50 lines centered on
    /// the match, but the
    /// preview area is often
    /// only 10–20 lines tall,
    /// so the matched line is
    /// below the fold without
    /// the scroll hint).
    /// `0` means "no scroll
    /// hint — render the
    /// preview from the top"
    /// (the historical
    /// default for history
    /// rows and other modes
    /// that don't need a
    /// scroll hint).
    pub preview_scroll: u16,

    /// Original agent / process name for
    /// herdr pane rows (`mode == "pane"`),
    /// captured at the time the row was
    /// emitted by
    /// `panes::refresh_session_panes_impl`
    /// (the value of
    /// `CurrentPaneInfo::current_command`,
    /// e.g. `"pi"`, `"claude"`, `"vim"`,
    /// `"ssh"`). Kept separate from
    /// `command` because the
    /// `process_pane_cmdlines` background
    /// patch in `src/tui.rs` REPLACES
    /// `row.command` with the herdr
    /// `pane process-info` cmdline, and
    /// needs the original agent to do
    /// the dedup (`agent == cmd_first`
    /// skips the `agent cmdline` join
    /// to avoid `pi pi` etc.). Reading
    /// the agent back out of
    /// `row.command` after the first
    /// patch would re-read the just-set
    /// combined value as the agent and
    /// concatenate again on the next
    /// tick (`"ssh ..."` + `"ssh ..."`
    /// $\rightarrow$ `"ssh ... ssh ..."`),
    /// which is the bug this field
    /// was introduced to fix. Empty for
    /// every non-pane row.
    pub pane_agent: String,
}

impl HistoryRow {
    /// `true` when this row is a
    /// not-yet-executed LLM
    /// suggestion (the synthetic
    /// preview row inserted into
    /// the merged view while the
    /// user is composing a `=...`
    /// LLM command-generation
    /// query).
    ///
    /// The check is on
    /// `exit_code == -1` (the
    /// "never executed" sentinel),
    /// NOT on `id < 0`. Negative
    /// ids are also used by todo
    /// rows (which encode the
    /// 1-based line number as
    /// `id = -(line_number)`), so
    /// `id < 0` would falsely
    /// classify every todo row as
    /// an LLM preview — that's the
    /// exact bug this predicate
    /// was introduced to fix. The
    /// `exit_code` sentinel is the
    /// load-bearing distinction;
    /// real history rows always
    /// have `exit_code >= 0`,
    /// question-mode rows have
    /// `exit_code >= 0`, and only
    /// LLM previews carry the
    /// `-1` sentinel.
    pub fn is_llm_preview(&self) -> bool {
        self.exit_code == -1
    }
}

/// One active window observed
/// in `tmux list-windows -a -F
/// '#{pane_id} | #{pane_current_path}
/// | active:#{window_active} |
/// Layout: #{window_layout}' |
/// grep 'active:1'`. The
/// directories view shows a
/// per-row marker when at least
/// one window's `path` matches
/// the row's `directory` (under
/// canonicalization), so the user
/// can see at a glance which
/// directories currently have
/// live tmux windows attached.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TmuxWindowInfo {
    /// Pane id (`#{pane_id}`),
    /// e.g. `%2`. Format is
    /// `%<n>` where `<n>` is the
    /// pane's global id. Unique
    /// across all sessions on
    /// the local tmux server, so
    /// it's sufficient as a
    /// `tmux ... -t <pane_id>`
    /// target without
    /// disambiguating by
    /// session:window.pane.
    /// The directories view uses
    /// this id to drive the
    /// "switch to this pane"
    /// action: `tmux select-pane
    /// -t <id> && tmux
    /// switch-client -t <id>`.
    pub pane_id: String,
    /// Window's active-pane
    /// current working directory
    /// (`#{pane_current_path}`).
    /// Canonicalised at parse
    /// time so `/Users/har/x`
    /// and `/Volumes/HUGE/har/x`
    /// (macOS volume mount) map
    /// to the same string the
    /// directories-fetch code
    /// produces. Empty strings
    /// are filtered out at parse
    /// time (a brand-new window
    /// has no cwd yet).
    pub path: String,
    /// The pane's foreground
    /// command
    /// (`#{pane_current_command}`
    /// on tmux, e.g. `ssh
    /// root@pve-1`, `vim`,
    /// `zsh`; empty on herdr —
    /// herdr's `pane list` JSON
    /// doesn't expose it).
    /// Used by the `# hosts`
    /// matcher to detect
    /// already-connected SSH
    /// sessions.
    #[allow(dead_code)]
    pub current_command: String,
    /// The workspace / session
    /// label (tmux:
    /// `#{session_name}`;
    /// herdr: the workspace's
    /// `label`). Used by the
    /// `# hosts` matcher on
    /// herdr to detect
    /// already-created
    /// workspaces by label
    /// match (herdr's
    /// foreground-command
    /// field is empty).
    #[allow(dead_code)]
    pub workspace_label: String,
}

/// A host entry from the config file
/// (`host.<id> = "Name"`,
/// `host.<id>.host = "alias"`,
/// `host.<id>.hostname = "real"`,
/// `host.<id>.user = "u"`,
/// `host.<id>.port = 22`,
/// `host.<id>.identity = "~/.ssh/..."`,
/// `host.<id>.dir = "~/path"`,
/// `host.<id>.exec = "cmd"`).
///
/// Merged with `~/.ssh/config` after parsing:
/// explicit fields win, unset fields inherit
/// from the SSH config block whose `Host`
/// alias matches `host`.
///
/// `host` is the SSH config `Host` alias (not
/// the real hostname) — it doubles as the
/// connection target when the SSH config
/// doesn't override it. For example, with the
/// SSH config:
/// ```text
/// Host proxmox
///     HostName pve-1.example.com
///     User root
/// ```
/// and the smarthistory config
/// `host.1.host = "proxmox"`, the resulting
/// SSH command is `ssh root@pve-1.example.com`.
#[derive(Debug, Clone, Default)]
pub struct HostDef {
    /// Display name shown in the `# hosts`
    /// section of the `*` panes view. Falls
    /// back to `host` (the SSH config alias)
    /// when the user didn't set it.
    pub name: String,
    /// The SSH config `Host` alias. Also used
    /// as the connection target when
    /// `hostname` is unset.
    pub host: String,
    /// The real hostname (`HostName` in SSH
    /// config). When set, takes precedence
    /// over `host` in the SSH argv.
    pub hostname: String,
    /// The login user. When unset, inherits
    /// from the matching SSH config block,
    /// then from the SSH config's `Host *`
    /// defaults, then from `$USER` at
    /// connect time.
    pub user: String,
    /// The TCP port. `0` means "use the SSH
    /// config's value, or fall back to 22".
    pub port: u16,
    /// Path to the private key. Inherits
    /// from the SSH config when unset.
    pub identity: String,
    /// Display-only cwd. Shown in the row
    /// but never used as the connection
    /// target (this is a local-fs
    /// convention that doesn't apply to
    /// remote hosts; included for symmetry
    /// with `SessionDef`).
    pub dir: String,
    /// Optional command to run after the SSH
    /// connection is up (e.g. `tmux a` to
    /// attach to a remote session). Staged
    /// via `send_in_pane_command` after the
    /// `ssh` body, the same way
    /// `SessionDef::exec` works for local
    /// sessions.
    pub exec: String,
}

impl HostDef {
    /// Build the `ssh` command line for this host — only the flags
    /// that are actually set. Shared by `App::stage_pane_selection`
    /// (the `"host"` row arm, staging a connection from the `*`
    /// panes view) and `smarthistory pane-exec` (re-running the same
    /// connection from a freshly-opened pane/window that didn't
    /// exist when the workspace was first created), so the two never
    /// drift apart on how a `HostDef` becomes an `ssh` invocation.
    pub fn ssh_command(&self) -> String {
        let effective_user = if self.user.is_empty() {
            std::env::var("USER").unwrap_or_default()
        } else {
            self.user.clone()
        };
        let target = if self.hostname.is_empty() {
            self.host.clone()
        } else {
            self.hostname.clone()
        };
        let mut ssh_body = String::from("ssh");
        if self.port != 0 && self.port != 22 {
            ssh_body.push_str(&format!(" -p {}", self.port));
        }
        if !self.identity.is_empty() {
            let home_list: Vec<String> = std::iter::once(std::env::var("HOME").unwrap_or_default())
                .filter(|s| !s.is_empty())
                .collect();
            let id_path = crate::util::expand_home_to_absolute(&self.identity, &home_list);
            ssh_body.push_str(&format!(" -i {}", crate::util::shell_quote(&id_path)));
        }
        if !effective_user.is_empty() {
            ssh_body.push_str(&format!(" {}@{}", effective_user, target));
        } else {
            ssh_body.push_str(&format!(" {}", target));
        }
        ssh_body
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    /// `Enter` — run the command (parent should submit the line).
    Run,
    /// `Left` — prefill the line for editing, cursor at the start.
    EditStart,
    /// `Right` — prefill the line for editing, cursor at the end.
    EditEnd,
}

/// Filter applied to the
/// directories list (`#`-mode
/// rows). The TUI cycles
/// through these with
/// `Action::CycleDirectorySource`
/// (default `C-M-g`).
///
/// - `All`: every row,
///   regardless of where
///   it came from
///   (history-driven,
///   tmux pane cwd, or
///   `sessiondirs=...`
///   config).
/// - `Tmux`: only the
///   directories that
///   are the cwd of at
///   least one active
///   tmux pane. Lets
///   the user jump to a
///   session they're
///   already running
///   somewhere else
///   without scrolling
///   past their pinned
///   project list.
/// - `Config`: only the
///   directories from
///   `sessiondirs=...`
///   in the config file
///   (recursively
///   walked). Lets the
///   user see just the
///   pinned projects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectorySource {
    All,
    Tmux,
    Config,
}

impl DirectorySource {
    pub fn next(self) -> Self {
        match self {
            DirectorySource::All => DirectorySource::Tmux,
            DirectorySource::Tmux => DirectorySource::Config,
            DirectorySource::Config => DirectorySource::All,
        }
    }
    /// Short display label
    /// for the mode-strip
    /// chip.
    pub fn label(self) -> &'static str {
        match self {
            DirectorySource::All => "ALL",
            DirectorySource::Tmux => "TMUX",
            DirectorySource::Config => "CFG",
        }
    }
    /// Parse the canonical
    /// `all` / `tmux` /
    /// `config` value as
    /// used in the
    /// persisted session
    /// file. Returns
    /// `None` for anything
    /// else; the caller
    /// falls back to
    /// `All` on parse
    /// failure (the
    /// default).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "all" => Some(DirectorySource::All),
            // `Tmux` is the
            // historical variant
            // name. The directory
            // marker semantics are
            // "an active context
            // in the configured
            // multiplexer", so
            // both `tmux` and
            // `herdr` parse to
            // the same variant
            // (the actual
            // multiplexer is
            // resolved at the
            // snapshot site, not
            // here).
            "tmux" | "herdr" => Some(DirectorySource::Tmux),
            "config" | "cfg" | "sessiondirs" => Some(DirectorySource::Config),
            _ => None,
        }
    }
}

/// Filter for the `*`-mode panes view.
/// Determines which section(s) of the
/// tree are shown:
///
/// - `All` — every section (live
///   multiplexer panes + `# sessions` +
///   `# hosts`). The default.
/// - `Windows` — only live
///   multiplexer panes (rows with
///   `source == "pane"` or `"workspace"`).
/// - `Hosts` — only the `# hosts`
///   block (rows with `source ==
///   "hosts"`).
/// - `Sessions` — only the `# sessions`
///   block (rows with `source ==
///   "sessions"`).
///
/// Toggled by the `FilterPanesWindows`,
/// `FilterPanesHosts`, and
/// `FilterPanesSessions` actions
/// (default keys `F7`, `F8`, `F9`).
/// Pressing the active filter's key
/// again resets to `All` (toggle off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanesFilter {
    /// Show all sections (default).
    #[default]
    All,
    /// Show only live multiplexer
    /// panes / workspaces.
    Windows,
    /// Show only the `# hosts` block.
    Hosts,
    /// Show only the `# sessions` block.
    Sessions,
}

impl PanesFilter {
    /// Short display label
    /// for the mode-strip
    /// chip. Returns the
    /// empty string for `All`
    /// (no chip shown).
    pub fn label(self) -> &'static str {
        match self {
            PanesFilter::All => "",
            PanesFilter::Windows => "PANES",
            PanesFilter::Hosts => "HOSTS",
            // Displayed as "Directories" in the list (see
            // `configured_sections_into` in `panes.rs`); the
            // variant name and config keys stay `sessions` for
            // backward compatibility.
            PanesFilter::Sessions => "DIRECTORIES",
        }
    }

    /// Returns `true` when
    /// the filter is at its
    /// default (`All`). Used
    /// by the renderer to
    /// hide the chip.
    pub fn is_default(self) -> bool {
        self == PanesFilter::All
    }

    /// Parse a string like
    /// "all", "windows",
    /// "hosts", "sessions"
    /// (case-insensitive).
    /// Returns `None` for
    /// anything else.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "all" => Some(PanesFilter::All),
            "windows" | "panes" | "win" => Some(PanesFilter::Windows),
            "hosts" | "host" => Some(PanesFilter::Hosts),
            "sessions" | "session" | "directories" | "directory" | "dir" | "dirs" => {
                Some(PanesFilter::Sessions)
            }
            _ => None,
        }
    }
}

/// Which detail panes are
/// visible in the TUI layout.
/// Toggle order: BOTH →
/// Details → OutputPreview →
/// BOTH.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaneVisibility {
    /// Show both details and
    /// output preview (default).
    #[default]
    Both,
    /// Show only the details
    /// pane; output preview
    /// is hidden.
    Details,
    /// Show only the output
    /// preview pane; details
    /// is hidden.
    OutputPreview,
}

impl PaneVisibility {
    pub fn next(self) -> Self {
        match self {
            PaneVisibility::Both => PaneVisibility::Details,
            PaneVisibility::Details => PaneVisibility::OutputPreview,
            PaneVisibility::OutputPreview => PaneVisibility::Both,
        }
    }

    /// Human-readable label for the status bar.
    pub fn label(self) -> &'static str {
        match self {
            PaneVisibility::Both => "both",
            PaneVisibility::Details => "details",
            PaneVisibility::OutputPreview => "output",
        }
    }

    /// Canonical string for persistence.
    pub fn as_str(self) -> &'static str {
        match self {
            PaneVisibility::Both => "both",
            PaneVisibility::Details => "details",
            PaneVisibility::OutputPreview => "output",
        }
    }

    /// Parse a string like "both", "details", "output"
    /// (case-insensitive). Returns `None` for anything else.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "both" => Some(PaneVisibility::Both),
            "details" => Some(PaneVisibility::Details),
            "output" => Some(PaneVisibility::OutputPreview),
            _ => None,
        }
    }
}

/// The height (in terminal lines) of the details row and the
/// output preview row. Adjustable one line at a time via
/// `Action::IncreasePaneHeight` / `Action::DecreasePaneHeight`
/// (default keys `F11` / `Shift-F11`) so the user can nudge the
/// details pane to exactly the size they want, rather than picking
/// from a fixed set of presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneHeight(u16);

impl Default for PaneHeight {
    fn default() -> Self {
        PaneHeight(Self::MIN)
    }
}

impl PaneHeight {
    /// The historical fixed details-row height, and the floor
    /// `decrease` never goes below.
    pub const MIN: u16 = 8;

    /// Rows reserved for the history list itself, even at the
    /// tallest details pane, so `increase` can never shrink the
    /// list to nothing.
    const MIN_LIST_ROWS: u16 = 3;

    /// One line taller, clamped to `max_for(page_size)`.
    pub fn increase(self, page_size: usize) -> Self {
        PaneHeight(self.0.saturating_add(1).min(Self::max_for(page_size)))
    }

    /// One line shorter, never below `MIN`.
    pub fn decrease(self) -> Self {
        PaneHeight(self.0.saturating_sub(1).max(Self::MIN))
    }

    /// The tallest the details row may grow to for a given
    /// terminal size: total height minus the fixed chrome (mode
    /// strip, input, status — 5 lines) minus `MIN_LIST_ROWS` for
    /// the history list. Never below `MIN`, so a very short
    /// terminal still gets the historical default rather than
    /// something smaller.
    fn max_for(page_size: usize) -> u16 {
        (page_size as u16)
            .saturating_sub(5)
            .saturating_sub(Self::MIN_LIST_ROWS)
            .max(Self::MIN)
    }

    /// The row height (in terminal lines) for the details row and
    /// the output preview row. Clamped against the current
    /// terminal's `max_for(page_size)` — a preference persisted
    /// from a larger terminal degrades gracefully on a smaller one
    /// instead of starving the history list, without mutating the
    /// stored preference itself.
    pub fn detail_row_height(self, page_size: usize) -> u16 {
        self.0.min(Self::max_for(page_size))
    }

    /// Human-readable label for the status bar, e.g. "14 lines".
    pub fn label(self) -> String {
        format!("{} line{}", self.0, if self.0 == 1 { "" } else { "s" })
    }

    /// Canonical string for persistence: the plain line count.
    pub fn as_str(self) -> String {
        self.0.to_string()
    }

    /// Parse a persisted/CLI value: a plain non-negative integer
    /// number of lines. Values below `MIN` are clamped up to `MIN`
    /// (rather than rejected) so a hand-edited config/session file
    /// with e.g. `paneheight=3` can't wedge the details row below
    /// the historical floor. Returns `None` for anything that
    /// isn't a valid integer.
    pub fn parse(s: &str) -> Option<Self> {
        s.trim()
            .parse::<u16>()
            .ok()
            .map(|n| PaneHeight(n.max(Self::MIN)))
    }
}

#[cfg(test)]
mod pane_height_tests;

/// Which kind of entry the
/// `AddEntryDialog` is
/// constructing. The
/// dialog's field list and
/// pre-fill logic branch on
/// this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddEntryKind {
    /// Add a `session.<id>` entry.
    /// Fields: Name (required),
    /// Dir (pre-filled from the
    /// selected row), Exec
    /// (optional).
    Session,
    /// Add a `host.<id>` entry.
    /// Fields: Name (required),
    /// Host (pre-filled from the
    /// directory basename),
    /// Hostname (optional,
    /// overrides SSH config),
    /// User (optional, defaults
    /// to `$USER`), Port
    /// (optional, defaults to
    /// 22), Identity (optional,
    /// inherits from SSH
    /// config), Exec (optional).
    Host,
}

/// One field in the add-entry
/// dialog. Holds the current
/// value as a `String` and the
/// cursor position (in
/// characters, matching the
/// line-editor widget's
/// convention).
#[derive(Debug, Clone)]
pub struct DialogField {
    /// Display name shown in
    /// the dialog (e.g.
    /// `"Name"`, `"Dir"`,
    /// `"Exec"`). Stable per
    /// dialog kind — used as
    /// the on-screen label and
    /// as the config-file
    /// suffix when writing the
    /// entry.
    pub name: &'static str,
    /// The config-file suffix
    /// for this field (e.g.
    /// `""` for the Name
    /// field of a session,
    /// `".host"` for the
    /// Host field of a host).
    /// Empty for the primary
    /// "name" field, dotted
    /// for sub-fields.
    pub config_suffix: &'static str,
    /// Current value the user
    /// has typed.
    pub value: String,
    /// Cursor position in
    /// characters (0..=len).
    pub cursor: usize,
    /// Whether the field must
    /// be non-empty for the
    /// dialog to commit. The
    /// "Name" field of both
    /// dialogs is required;
    /// everything else is
    /// optional.
    pub required: bool,
    /// Placeholder shown in
    /// the input box when the
    /// value is empty (e.g.
    /// `"my-session"`,
    /// `"~/.ssh/id_ed25519"`).
    /// Cosmetic only — never
    /// used as a default value.
    pub placeholder: &'static str,
}

impl DialogField {
    /// Construct a new empty
    /// field. The cursor
    /// starts at position 0.
    pub fn new(
        name: &'static str,
        config_suffix: &'static str,
        required: bool,
        placeholder: &'static str,
    ) -> Self {
        DialogField {
            name,
            config_suffix,
            value: String::new(),
            cursor: 0,
            required,
            placeholder,
        }
    }

    /// Construct a new field
    /// pre-filled with `value`.
    /// The cursor is placed at
    /// the end of the value
    /// (the natural position
    /// for the user to keep
    /// typing).
    pub fn prefilled(
        name: &'static str,
        config_suffix: &'static str,
        required: bool,
        placeholder: &'static str,
        value: String,
    ) -> Self {
        let cursor = value.chars().count();
        DialogField {
            name,
            config_suffix,
            value,
            cursor,
            required,
            placeholder,
        }
    }
}

/// Pending "save this directory to your Directories list?" prompt,
/// shown after selecting a `~` (zoxide) row whose directory isn't
/// already a configured `session.<id>` entry — see
/// `App::stage_zoxide_selection`, which opens this INSTEAD of
/// immediately staging the directory selection, and
/// `App::answer_zoxide_save_prompt`, which — regardless of the
/// answer — always finishes the original action (create/focus the
/// tmux/herdr session there) once the prompt is answered; saying
/// "no" only skips the save, it never blocks the jump. A "yes"
/// answer writes a plain `session.<id>` entry (name + `.dir` only,
/// no `.exec`) by reusing the same `AddEntryDialog` /
/// `write_new_entry_to_config` machinery the `F5` "add session"
/// dialog uses, just constructed and submitted programmatically
/// instead of interactively.
#[derive(Debug, Clone)]
pub struct ZoxideSavePrompt {
    /// The directory's basename — used as the new `session.<id>`
    /// entry's display name (`session.<id> = "<label>"`).
    pub label: String,
    /// The absolute directory path — written as
    /// `session.<id>.dir = "<directory>"`.
    pub directory: String,
}

/// Pending "started how many minutes ago?" prompt, shown after picking
/// a project note in `.`-mode (`Action::Run`/Enter) — see
/// `App::stage_project_selection`, which opens this INSTEAD of
/// immediately staging `smarthistory project select <slug>`, and
/// `App::answer_project_since_prompt`, which stages the command (with
/// `--since <N>m` appended only when `buffer` holds a positive
/// number) and exits the TUI, same as the direct-staging path did
/// before this prompt existed. `buffer` can only ever hold digits or
/// be empty (`handle_project_since_prompt_key` filters every other
/// character at insertion time), so there's no invalid state to
/// report — blank or `"0"` both mean "just now" (today's exact
/// pre-existing behavior, no `--since` at all).
#[derive(Debug, Clone)]
pub struct ProjectSincePrompt {
    /// The slug to stage, resolved from the selected project note the
    /// same way `stage_project_selection` always has.
    pub slug: String,
    /// Digits typed so far (minutes ago). Empty means "just now".
    pub buffer: String,
    /// Character-index cursor into `buffer` (0..=len).
    pub cursor: usize,
}

/// Pending "Template name:" prompt, shown by
/// `Action::CreateJiraTemplateFromIssue` (`-` mode, selected row) before
/// generating a "create JIRA issue from template" template file from that
/// issue's fields. See `App::start_jira_template_fetch`/
/// `App::process_jira_template_fetch_result` in `src/tui.rs` for what
/// happens once a name is confirmed. Modeled directly on
/// `ProjectSincePrompt`, the closest existing single-line-input dialog —
/// the one real difference is `buffer` accepts any printable character
/// (a template name, not a digit-only minute count), so an empty submit
/// is a genuine invalid state `error` reports, rather than a valid
/// "just now" default.
#[derive(Debug, Clone)]
pub struct TemplateNamePrompt {
    /// The JIRA issue key this template will be generated from (e.g.
    /// `PROJ-123`), captured when the action opened the prompt.
    pub source_key: String,
    /// The template name typed so far.
    pub buffer: String,
    /// Character-index cursor into `buffer` (0..=len).
    pub cursor: usize,
    /// Set on an empty-submit (`Enter` with a blank/whitespace-only
    /// `buffer`) — a template needs a name, unlike `ProjectSincePrompt`
    /// where blank is a valid "just now" default. Cleared on the next
    /// keystroke or a non-empty submit.
    pub error: Option<String>,
}

/// Short flags — across `ssh`/`scp`/`sftp`/`rsync`/`mosh` — that take
/// a separate following argument (`-p 2222`, `-i ~/.ssh/id_ed25519`,
/// …), as opposed to a bare boolean flag (`-4`, `-C`, …) or a flag
/// with its value attached directly (`-p2222`, `-oKey=Val`) — those
/// don't have a following word to skip in the first place, so only
/// the separate-argument form needs handling in
/// [`extract_ssh_target`]. Not exhaustive across every flag every one
/// of these programs supports, but covers the common
/// connection-tuning ones likely to appear ahead of the actual
/// target: port (`-p`, and `scp`'s own `-P`), identity (`-i`), an
/// arbitrary ssh option (`-o`), login name (`-l`), config file
/// (`-F`), jump host (`-J`), cipher (`-c`), port forwarding
/// (`-D`/`-L`/`-R`/`-W`/`-w`), escape char (`-e`), bind
/// interface/address (`-B`/`-b`), log file (`-E`), a local-tunnel
/// interface (`-I`), multiplex mode (`-m`/`-O`), and the query/control
/// path pair (`-Q`/`-S`).
const SSH_VALUE_TAKING_FLAGS: &[&str] = &[
    "-p", "-P", "-i", "-o", "-l", "-F", "-J", "-c", "-D", "-L", "-R", "-W", "-w", "-e", "-B",
    "-b", "-E", "-I", "-m", "-O", "-Q", "-S",
];

/// Pull an SSH connection target — `user@host`, or bare `host` — out
/// of a command line, for pre-filling `F6`'s "add host" dialog from
/// the selected history row (e.g. `ssh root@122.1.1.40` → `(Some("root"),
/// "122.1.1.40")`). Only recognizes commands whose first word is a
/// known remote-connection program (`ssh`/`scp`/`sftp`/`rsync`/`mosh`,
/// path prefix stripped) — scanning arbitrary command text for
/// anything `word@word`-shaped would false-positive on email
/// addresses, `git commit --author`, etc.
///
/// Every word starting with `-` is stripped first — along with its
/// value, for a word in [`SSH_VALUE_TAKING_FLAGS`] — so command-line
/// options never factor into what's left to consider. What remains
/// after that is handled in two cases:
///
/// 1. **Exactly one word remains** (`ssh machine`, `ssh -p 2222
///    root@machine`) — for `ssh`/`sftp`/`mosh`, which take a bare
///    `[user@]host` and nothing but flags besides, that lone word has
///    no other possible meaning, so it's accepted as the target
///    whatever shape it's in — no dot or IPv4 pattern required,
///    unlike case 2. This is what makes `ssh machine` (and `ssh -p
///    2222 machine`) recognize `machine` as the host even though it's
///    a bare, undotted single-label name.
/// 2. **More than one word remains** — even with flags already
///    filtered out, the target could be any one of the words that are
///    left (a remote command to run and its own arguments, most
///    commonly), so a looser "any bare word" rule would
///    false-positive on those. `host` must be either an IPv4
///    dotted-quad or a dotted hostname (`pve-1.local`) in this case;
///    a bare single-label hostname isn't recognized here — genuinely
///    indistinguishable from any other bare word without deeper
///    parsing, so it falls back to the caller's own default instead.
///    `scp`/`rsync` take a *pair* of paths (`scp LOCAL [user@]HOST:REMOTE`
///    or the reverse), and a local path can easily look host-shaped
///    by pure accident — `file.txt` parses as a two-label dotted
///    hostname just as well as `pve-1.local` does — so for those two
///    programs specifically, only a colon-suffixed word (the one
///    place a remote target is unambiguous: it always carries the `:`
///    separating it from the remote path, which a local path never
///    does) is considered a candidate at all, and case 1 never applies
///    to them regardless of word count. Scans left to right and
///    returns the first word that matches.
fn extract_ssh_target(command: &str) -> Option<(Option<String>, String)> {
    let mut words = command.split_whitespace();
    let program = words.next()?;
    let program = program.rsplit('/').next().unwrap_or(program);
    let takes_bare_target = match program {
        "ssh" | "sftp" | "mosh" => true,
        "scp" | "rsync" => false,
        _ => return None,
    };
    let mut rest: Vec<&str> = Vec::new();
    while let Some(w) = words.next() {
        if w.starts_with('-') {
            if SSH_VALUE_TAKING_FLAGS.contains(&w) {
                words.next();
            }
            continue;
        }
        rest.push(w);
    }
    if takes_bare_target && rest.len() == 1 {
        return Some(match rest[0].split_once('@') {
            Some((user, host)) if !user.is_empty() && !host.is_empty() => {
                (Some(user.to_string()), host.to_string())
            }
            _ => (None, rest[0].to_string()),
        });
    }
    let requires_colon = !takes_bare_target;
    let re = regex::Regex::new(
        r"^(?:([A-Za-z0-9._-]+)@)?((?:[0-9]{1,3}\.){3}[0-9]{1,3}|[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)+)(:.*)?$",
    )
    .expect("static regex");
    rest.into_iter().find_map(|w| {
        let caps = re.captures(w)?;
        if requires_colon && caps.get(3).is_none() {
            return None;
        }
        let user = caps.get(1).map(|m| m.as_str().to_string());
        let host = caps.get(2).unwrap().as_str().to_string();
        Some((user, host))
    })
}

/// State for the "add session /
/// host" dialog. Opens on
/// `C-1` / `C-2`, walks the
/// user through the fields
/// needed to construct a
/// config-file entry, and on
/// `Enter` writes the entry to
/// `~/.config/smarthistory/config`
/// and reloads the in-memory
/// session / host list.
#[derive(Debug, Clone)]
pub struct AddEntryDialog {
    /// Which kind of entry
    /// this dialog is
    /// constructing.
    pub kind: AddEntryKind,
    /// The fields the user
    /// edits. The order in
    /// this vec is the
    /// display order AND the
    /// Tab navigation order.
    pub fields: Vec<DialogField>,
    /// Index of the field
    /// currently being
    /// edited. Tab /
    /// Shift+Tab move this.
    pub focused: usize,
    /// The directory from the
    /// selected row (used as
    /// the Dir field's
    /// pre-fill for sessions
    /// and as a status hint in
    /// the dialog title).
    pub source_directory: String,
    /// The command from the
    /// selected row. Shown as a
    /// status hint in the
    /// dialog title (the entry
    /// itself doesn't carry the
    /// command), and — for a
    /// `Host` dialog — scanned
    /// by `extract_ssh_target`
    /// to pre-fill the Host/User
    /// fields when the row looks
    /// like an SSH/SCP/etc.
    /// invocation.
    pub source_command: String,
    /// Error message from the
    /// most recent commit
    /// attempt (e.g. "name is
    /// empty"). Cleared on
    /// the next keystroke.
    /// `None` when there's no
    /// error to display.
    pub error: Option<String>,
}

impl AddEntryDialog {
    /// Build the dialog for a
    /// given `kind`, pre-filling
    /// the fields from
    /// `source_directory` and
    /// `source_command`. The
    /// cursor lands on the
    /// first field (Name).
    pub fn new(kind: AddEntryKind, source_directory: String, source_command: String) -> Self {
        let fields = match kind {
            AddEntryKind::Session => vec![
                DialogField::new("Name", "", true, "my-session"),
                DialogField::prefilled("Dir", ".dir", false, "~/path", source_directory.clone()),
                DialogField::new("Exec", ".exec", false, "command to run after create"),
            ],
            AddEntryKind::Host => {
                // If the selected row looks like an SSH/SCP/etc.
                // invocation (`ssh root@122.1.1.40`), pre-fill Host
                // (and User, when present) straight from it — a much
                // more useful default than the directory basename,
                // which for a `cd`-then-`ssh` workflow is usually
                // unrelated to the remote host entirely. Falls back
                // to the pre-existing basename-or-full-path default
                // when the row doesn't match.
                let ssh_target = extract_ssh_target(&source_command);
                let host_value = ssh_target
                    .as_ref()
                    .map(|(_, host)| host.clone())
                    .unwrap_or_else(|| {
                        std::path::Path::new(&source_directory)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| source_directory.clone())
                    });
                // A matched target with no explicit `user@` (`ssh
                // machine`) still has a real user: ssh itself
                // defaults to the current OS login for that case, so
                // mirror that here instead of leaving User blank.
                // `ssh_target.is_none()` (no target matched at all)
                // is the one case that leaves User empty.
                let user_field = match &ssh_target {
                    Some((Some(user), _)) => DialogField::prefilled(
                        "User",
                        ".user",
                        false,
                        "alice (defaults to $USER)",
                        user.clone(),
                    ),
                    Some((None, _)) => {
                        match std::env::var("USER") {
                            Ok(current_user) if !current_user.is_empty() => {
                                DialogField::prefilled(
                                    "User",
                                    ".user",
                                    false,
                                    "alice (defaults to $USER)",
                                    current_user,
                                )
                            }
                            _ => DialogField::new(
                                "User",
                                ".user",
                                false,
                                "alice (defaults to $USER)",
                            ),
                        }
                    }
                    None => DialogField::new("User", ".user", false, "alice (defaults to $USER)"),
                };
                vec![
                    DialogField::new("Name", "", true, "Proxmox"),
                    DialogField::prefilled("Host", ".host", true, "pve-1", host_value),
                    DialogField::new(
                        "Hostname",
                        ".hostname",
                        false,
                        "real.host (overrides SSH config)",
                    ),
                    user_field,
                    DialogField::new("Port", ".port", false, "22"),
                    DialogField::new("Identity", ".identity", false, "~/.ssh/id_ed25519"),
                    DialogField::new("Exec", ".exec", false, "command to run after ssh"),
                ]
            }
        };
        AddEntryDialog {
            kind,
            fields,
            focused: 0,
            source_directory,
            source_command,
            error: None,
        }
    }

    /// Advance the focused
    /// field to the next
    /// (wrapping at the end).
    pub fn focus_next(&mut self) {
        if self.fields.is_empty() {
            return;
        }
        self.focused = (self.focused + 1) % self.fields.len();
    }

    /// Move the focused field
    /// to the previous (wrapping
    /// at the start).
    pub fn focus_prev(&mut self) {
        if self.fields.is_empty() {
            return;
        }
        self.focused = if self.focused == 0 {
            self.fields.len() - 1
        } else {
            self.focused - 1
        };
    }
}

/// The focus positions of [`CreateJiraIssueDialog`], in display order.
/// `IssueType`/`Project` are closed-set selectors (cycled with
/// Left/Right, not typed); `Subject`/`Labels`/`Description` are
/// free-text `DialogField`s, indexed into
/// `CreateJiraIssueDialog::fields` by `CreateJiraIssueFocus::field_index()`.
/// `Extra(i)` is the `i`th "create JIRA issue from template" extra field
/// (`CreateJiraIssueDialog::extra_fields`) — positioned between `Labels`
/// and `Description` in Tab order, so `Description` (the base dialog's
/// existing last variant) stays last regardless of how many extra fields
/// a template contributes. `next`/`prev` take the current extra-field
/// count since that's per-dialog-instance (a plain, template-less dialog
/// has zero) — unlike the fixed 5-variant shape this replaces, the full
/// sequence can no longer be a `const` lookup table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateJiraIssueFocus {
    IssueType,
    Project,
    Subject,
    Labels,
    Extra(usize),
    Description,
}

impl CreateJiraIssueFocus {
    fn next(self, extra_count: usize) -> Self {
        match self {
            CreateJiraIssueFocus::IssueType => CreateJiraIssueFocus::Project,
            CreateJiraIssueFocus::Project => CreateJiraIssueFocus::Subject,
            CreateJiraIssueFocus::Subject => CreateJiraIssueFocus::Labels,
            CreateJiraIssueFocus::Labels => {
                if extra_count > 0 {
                    CreateJiraIssueFocus::Extra(0)
                } else {
                    CreateJiraIssueFocus::Description
                }
            }
            CreateJiraIssueFocus::Extra(i) if i + 1 < extra_count => {
                CreateJiraIssueFocus::Extra(i + 1)
            }
            CreateJiraIssueFocus::Extra(_) => CreateJiraIssueFocus::Description,
            CreateJiraIssueFocus::Description => CreateJiraIssueFocus::IssueType,
        }
    }

    fn prev(self, extra_count: usize) -> Self {
        match self {
            CreateJiraIssueFocus::IssueType => CreateJiraIssueFocus::Description,
            CreateJiraIssueFocus::Project => CreateJiraIssueFocus::IssueType,
            CreateJiraIssueFocus::Subject => CreateJiraIssueFocus::Project,
            CreateJiraIssueFocus::Labels => CreateJiraIssueFocus::Subject,
            CreateJiraIssueFocus::Extra(0) => CreateJiraIssueFocus::Labels,
            CreateJiraIssueFocus::Extra(i) => CreateJiraIssueFocus::Extra(i - 1),
            CreateJiraIssueFocus::Description => {
                if extra_count > 0 {
                    CreateJiraIssueFocus::Extra(extra_count - 1)
                } else {
                    CreateJiraIssueFocus::Labels
                }
            }
        }
    }

    /// The index into `CreateJiraIssueDialog::fields` this focus
    /// position corresponds to, or `None` for the two selectors
    /// (`Project`/`IssueType`) and `Extra` (which indexes
    /// `CreateJiraIssueDialog::extra_fields` instead — see
    /// `CreateJiraIssueDialog::focused_field_mut`).
    fn field_index(self) -> Option<usize> {
        match self {
            CreateJiraIssueFocus::Subject => Some(0),
            CreateJiraIssueFocus::Description => Some(1),
            CreateJiraIssueFocus::Labels => Some(2),
            CreateJiraIssueFocus::Project
            | CreateJiraIssueFocus::IssueType
            | CreateJiraIssueFocus::Extra(_) => None,
        }
    }
}

/// What a `CreateJiraIssueDialog::extra_fields` entry means on submit —
/// see `crate::tui::mode::jira::TemplateFieldKind` for the frontmatter
/// classification this is built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtraFieldKind {
    /// Sent as a real JIRA custom field (`fields.<id>`) on create.
    CustomField(String),
    /// Folded into Description as a prepended `**name:** value` line on
    /// submit, rather than sent as its own JIRA API field.
    Parameter,
    /// A JIRA custom field CLONED from a source issue
    /// (`JiraConfig::clone_fields`/`JIRA_CLONE_FIELDS`) when the "create
    /// JIRA issue" dialog is opened from a selected row. Sent as the
    /// same real custom field as `CustomField` on submit, but — unlike
    /// `CustomField` — READ-ONLY in the dialog: the whole point is an
    /// exact clone, not a value the user might accidentally edit before
    /// submitting. Enforced by `handle_create_jira_issue_dialog_key`
    /// no-oping edit keystrokes when the focused extra field has this
    /// kind; focus/Tab navigation is unaffected.
    ClonedCustomField(String),
}

/// State for the "create JIRA issue" dialog (`Action::CreateJiraIssue`).
/// Deliberately its own struct rather than a third [`AddEntryKind`] on
/// [`AddEntryDialog`] — that dialog's commit path
/// (`write_new_entry_to_config`) is hard-wired to appending lines to a
/// local config file, the wrong fit for a REST `POST`. See
/// `App::open_create_jira_issue_dialog`/`App::create_jira_issue_dialog_submit`
/// in `src/tui.rs` for how this gets populated and submitted.
#[derive(Debug, Clone)]
pub struct CreateJiraIssueDialog {
    /// `[Subject, Description, Labels]`, in that order — see
    /// `CreateJiraIssueFocus::field_index`. Reuses the plain
    /// `DialogField` type; `config_suffix`/`required` are unused
    /// here (this dialog never writes to a config file, and the
    /// only required field, Subject, is checked directly by
    /// `create_jira_issue_dialog_submit` rather than via
    /// `DialogField::required`, since Project/`IssueType` — which
    /// also can't be blank — aren't `DialogField`s at all).
    pub fields: Vec<DialogField>,
    /// Extra fields contributed by a "create JIRA issue from template"
    /// template's frontmatter (`cf[<id>]` custom fields and generic
    /// parameters — see `crate::tui::mode::jira::TemplateFieldKind`).
    /// Empty for a plain (non-template) dialog. Parallel to
    /// `extra_field_kinds`; rendered between Labels and Description
    /// (`CreateJiraIssueFocus::Extra`).
    pub extra_fields: Vec<DialogField>,
    /// What each `extra_fields` entry means on submit — parallel to
    /// `extra_fields` (same index).
    pub extra_field_kinds: Vec<ExtraFieldKind>,
    /// From `JiraConfig::available_projects` at dialog-open time.
    /// Never empty — `open_create_jira_issue_dialog` refuses to
    /// open the dialog at all when this would be empty (nothing to
    /// select).
    pub projects: Vec<String>,
    pub project_index: usize,
    /// From `JiraConfig::available_issue_types`. Never empty — always
    /// has at least the built-in default set.
    pub issue_types: Vec<String>,
    pub issue_type_index: usize,
    pub focused: CreateJiraIssueFocus,
    /// `Some(key)` when the dialog was opened with a JIRA row
    /// selected — the issue the new one gets a "Relates" link to on
    /// successful creation. `None` for a note-sourced or cold-opened
    /// dialog.
    pub source_key: Option<String>,
    /// `true` while `source_key.is_some()` and the background fetch
    /// for that issue's full description/labels
    /// (`JiraPrefillFetchRequest`) hasn't resolved yet — used to
    /// decide whether the async result is still allowed to overwrite
    /// the Description/Labels fields (never once the user has typed
    /// into them) and to render a "(loading…)" hint.
    pub prefill_loading: bool,
    pub error: Option<String>,
}

impl CreateJiraIssueDialog {
    pub fn focus_next(&mut self) {
        self.focused = self.focused.next(self.extra_fields.len());
    }

    pub fn focus_prev(&mut self) {
        self.focused = self.focused.prev(self.extra_fields.len());
    }

    /// Cycle the Project selector, wrapping. No-op when `focused`
    /// isn't `Project` — callers gate on that themselves, this just
    /// keeps the wrapping-index math in one place.
    pub fn cycle_project(&mut self, forward: bool) {
        if self.projects.is_empty() {
            return;
        }
        self.project_index = cycle_index(self.project_index, self.projects.len(), forward);
    }

    pub fn cycle_issue_type(&mut self, forward: bool) {
        if self.issue_types.is_empty() {
            return;
        }
        self.issue_type_index = cycle_index(self.issue_type_index, self.issue_types.len(), forward);
    }

    /// The `DialogField` the current focus points at, or `None` when
    /// a selector (Project/Issue Type) is focused.
    pub fn focused_field_mut(&mut self) -> Option<&mut DialogField> {
        if let CreateJiraIssueFocus::Extra(i) = self.focused {
            return self.extra_fields.get_mut(i);
        }
        self.focused.field_index().and_then(|i| self.fields.get_mut(i))
    }

    /// Whether the focused field is a `ClonedCustomField` — read-only,
    /// so `handle_create_jira_issue_dialog_key` no-ops every editing
    /// keystroke while it's focused. `false` for every other focus
    /// position (the selectors, Subject/Labels/Description, and a
    /// template's own editable `CustomField`/`Parameter` extras).
    pub fn focused_is_read_only(&self) -> bool {
        if let CreateJiraIssueFocus::Extra(i) = self.focused {
            matches!(self.extra_field_kinds.get(i), Some(ExtraFieldKind::ClonedCustomField(_)))
        } else {
            false
        }
    }
}

fn cycle_index(current: usize, len: usize, forward: bool) -> usize {
    if forward {
        (current + 1) % len
    } else {
        (current + len - 1) % len
    }
}

/// State for the "create JIRA issue from template" picker
/// (`Action::CreateJiraIssueFromTemplate`) — pick one of the markdown
/// files under `~/.config/smarthistory/templates/jira/`, opened by
/// `App::open_jira_template_picker`. Deliberately minimal compared to
/// `ThemePicker` (`src/tui/theme/picker.rs`) — no live search/filter,
/// just an arrow-key list, since template counts are expected to be
/// small; the shape otherwise mirrors it (a `Vec` snapshotted at
/// picker-open time plus a clamped `selected` index).
#[derive(Debug, Clone)]
pub struct JiraTemplatePicker {
    /// `(display name, file path)` pairs — display name is the
    /// filename with `.md` stripped, sorted alphabetically. Snapshotted
    /// once at picker-open time; adding/removing template files while
    /// the picker is open isn't picked up until it's reopened.
    pub entries: Vec<(String, std::path::PathBuf)>,
    /// Always a valid index into `entries` (or `0` when `entries` is
    /// empty — though `open_jira_template_picker` refuses to open the
    /// picker at all in that case, so this shouldn't be observed).
    pub selected: usize,
}

impl JiraTemplatePicker {
    /// Move the selection by `delta`, clamped to `entries`' bounds.
    pub fn move_by(&mut self, delta: isize) {
        if self.entries.is_empty() {
            self.selected = 0;
            return;
        }
        let n = self.entries.len() as isize;
        let next = (self.selected as isize + delta).clamp(0, n - 1);
        self.selected = next as usize;
    }

    /// The currently-selected entry, or `None` when `entries` is empty.
    pub fn current(&self) -> Option<&(String, std::path::PathBuf)> {
        self.entries.get(self.selected)
    }
}

/// State for the in-TUI multi-line note/todo compose overlay
/// (`Action::ComposeNoteEntry`, `F2` by default). Opens in `@`
/// (Notes) or `!` (Todo) mode and lets the user type a body
/// spanning multiple lines before committing.
///
/// This is deliberately a SEPARATE mechanism from the existing
/// `@new <text>` / `!@new <text>` single-line quick-create
/// (`stage_note_selection` / `stage_todo_selection` in
/// `src/tui/actions.rs`), which still stages `note_search
/// create-note <text> ...` and exits immediately, unchanged.
/// The one-liner stays the fast path for a short entry; this
/// dialog is for when the user wants to write more than fits
/// on the query line.
///
/// `note_search create-note`'s `text: String` argument becomes
/// ONE line in the daily note (`- [prefix]<text>` or, for a
/// todo, `- [ ] [prefix]<text> due: <date>` — see
/// `note_search_core::commands::create_note::append_to_yournal`).
/// A raw embedded newline in `text` would therefore break out
/// of that markdown list item as an unindented continuation
/// line. `App::note_compose_submit` re-indents embedded
/// newlines (`"\n"` → `"\n  "`) before staging the command so
/// the committed body stays a single valid list item with
/// indented continuation lines.
#[derive(Debug, Clone, Default)]
pub struct NoteComposeDialog {
    /// True for a `!` todo entry (stages `--todo`, which adds
    /// checkbox + due-date formatting downstream); false for a
    /// plain `@` note entry.
    pub todo: bool,
    /// Multi-line body text. Embedded `\n` from `Enter`
    /// keypresses are preserved literally in the buffer — the
    /// re-indenting for markdown-safety happens only at commit
    /// time (`App::note_compose_submit`), not while editing.
    pub text: String,
    /// Cursor position as a CHARACTER index into `text` (same
    /// convention as `App::query_cursor`), not a byte index.
    pub cursor: usize,
}

/// Which field of the
/// `NoteCreateDialog` the
/// user is currently
/// editing. The dialog has
/// two fields (Title, Content);
/// the user toggles between
/// them with `Tab` (or any
/// key that, while the cursor
/// sits on a non-`@`/`#`-prefixed
/// word, advances the focus —
/// see `App::note_create_advance_field`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteCreateField {
    Title,
    Content,
}

/// A two-field
/// "create a new note"
/// dialog opened via
/// `Action::CreateNote` (default
/// key: `none`; the user binds
/// it via the config file,
/// e.g. `key.create-note=M-N`).
/// Unlike `NoteComposeDialog`
/// (a single multi-line body
/// field that drops a bullet
/// into the Yournal), this
/// dialog is a richer
/// composer: a single-line
/// Title, a multi-line
/// Content, and inline
/// completion for note links
/// (`@` or `[[`, plus the
/// attribute/date-filtered
/// `@p:`, `@e:`, `@d:`, `@7:`,
/// `@w:`, `@n:` variants) and
/// `#`-prefixed tags — see
/// `App::try_note_create_completion`'s
/// doc comment for the full
/// mechanics.
///
/// On `Ctrl-S` the dialog
/// formats the body as a
/// level-3 heading
/// (`### TITLE [[LINK1]] ... [[LINKN]] #TAG1 ... #TAGN`)
/// followed by a
/// `[time:: HH:MM]` line and
/// the user's content, and
/// appends it to the same
/// `# Yournal` section of
/// the daily note that the
/// legacy `@new` uses
/// (`note_search::commands::create_note`
/// locates the journal
/// section the same way).
/// The tags and links in the
/// heading are extracted
/// from both the Title and
/// Content fields, so the
/// user can spread them
/// across either field
/// before submitting.
///
/// Completion: the dialog has its own `NoteCreateCompletion` menu
/// (below) with a `candidates` / `selected` shape modeled on the
/// main query input's `CompletionMenu`, so the user navigates with
/// the same arrow-key / Enter pattern they already know. For `@`,
/// `[[`, and `#` the candidate list comes straight from
/// `crate::jira::notes_link_matches` / `notes_tag_matches` — the
/// same `note_tags`/`note_links`-backed helpers the Notes (`@`)
/// prefix mode's own Tab completion uses. The `@p:` / `@e:` / `@d:`
/// / `@7:` / `@w:` / `@n:` variants instead query
/// `note_search`'s `DatabaseService::search_notes` with an
/// attribute or date filter (e.g. `@p:` → `type: project`, `@7:` →
/// a rolling 7-day window) and offer matching notes' basenames.
#[derive(Debug, Clone)]
pub struct NoteCreateDialog {
    /// Single-line title
    /// text. No newlines
    /// allowed (the
    /// `push_char` path
    /// rejects `'\n'`).
    pub title: String,
    /// Character-index
    /// cursor into `title`.
    pub title_cursor: usize,
    /// Multi-line body
    /// text. `\n` from
    /// `Enter` is allowed
    /// (same as
    /// `NoteComposeDialog`).
    pub content: String,
    /// Character-index
    /// cursor into
    /// `content`.
    pub content_cursor: usize,
    /// Which field the
    /// user is currently
    /// editing. Toggled
    /// by `Tab` (or by
    /// pressing a
    /// non-`@`/`#` word
    /// while the other
    /// is the active
    /// field — see the
    /// keymap). When
    /// `None`, no field
    /// is focused
    /// (initial state
    /// right after the
    /// dialog opens;
    /// the keymap
    /// auto-focuses
    /// the title on the
    /// first printable
    /// keypress).
    pub active_field: NoteCreateField,
    /// Inline completion
    /// menu, opened when
    /// the cursor sits on
    /// a word that starts
    /// with one of the
    /// supported prefixes
    /// (`@`, `[[`, `#`, or
    /// the attribute/date-
    /// filtered `@p:`, `@e:`,
    /// `@d:`, `@7:`, `@w:`,
    /// `@n:` variants).
    /// `None` when
    /// the user is typing
    /// freely without a
    /// completion in
    /// flight.
    ///
    /// This is a
    /// `NoteCreateCompletion`
    /// (a small completion
    /// menu specific to the
    /// dialog), NOT the
    /// global `CompletionMenu`
    /// used by the main
    /// query input. The dialog
    /// completion operates on
    /// the active field's
    /// buffer (Title or
    /// Content), so the byte
    /// range it tracks refers
    /// to the active field,
    /// not `app.query`. The
    /// existing
    /// `CompletionMenu`'s
    /// `format_selected`
    /// always wraps the
    /// candidate in a kind
    /// prefix/suffix
    /// (`[[...]]`, `#`, `=`),
    /// which is wrong for the
    /// dialog's pre-formatted
    /// candidates (we
    /// already wrap `[[Title]]`
    /// in the candidate list
    /// — re-wrapping would
    /// produce `[[[Title]]]`).
    /// Keeping the dialog
    /// completion local
    /// avoids that
    /// double-wrap and lets
    /// us scan the active
    /// field's buffer for the
    /// replace range at
    /// commit time (so the
    /// user's exact typed
    /// prefix — including
    /// mid-word cursor
    /// positions — is
    /// replaced verbatim).
    pub completion: Option<NoteCreateCompletion>,
    /// `true` while the "save or drop?" confirmation overlay is
    /// showing — set by `Esc`/`Ctrl-C` when either field has
    /// unsaved text, instead of closing the dialog immediately. See
    /// `App::note_create_confirm_discard_if_dirty`'s doc comment for
    /// the full flow. While `true`, key handling is routed to
    /// `handle_note_create_confirm_key` instead of the normal dialog
    /// keymap (same layering `completion` above uses to take over
    /// the dialog's keymap while a completion menu is open).
    pub confirm_discard: bool,
    /// `true` when the active field (Title or Content) is fully
    /// selected via `Ctrl-A`. Not a partial-range selection — this
    /// dialog only ever has "none" or "everything in the active
    /// field" selected, unlike a real text editor's arbitrary
    /// start/end range. While `true`, `Ctrl-C` yanks the field's
    /// text to the clipboard instead of cancelling the dialog, and
    /// `Backspace` deletes the whole field instead of one character;
    /// any other key clears it back to `false`. See
    /// `App::note_create_select_all` and the guard at the top of
    /// `handle_note_create_key`.
    pub select_all: bool,
}

/// The dialog-local
/// completion menu. Stores
/// the candidate list (each
/// already in the final
/// insertion form, e.g.
/// `[[Title]]` for a note
/// link or `#tag` for a
/// tag) and the currently
/// highlighted index. The
/// replace range is
/// recomputed at commit
/// time (we scan backward
/// from the active field's
/// cursor to the most
/// recent whitespace /
/// buffer start), so we
/// don't have to thread
/// byte ranges through the
/// menu struct — the
/// candidate text is what
/// gets inserted in place
/// of the prefix word.
#[derive(Debug, Clone)]
pub struct NoteCreateCompletion {
    /// The candidates, in
    /// display order.
    /// Each entry is the
    /// FINAL text the user
    /// wants inserted (e.g.
    /// `[[Title]]` or
    /// `#tag`); no further
    /// wrapping happens at
    /// commit time.
    pub candidates: Vec<String>,
    /// Index into
    /// `candidates` of the
    /// currently-highlighted
    /// entry. Clamped to
    /// `0..candidates.len()`
    /// on every menu op.
    pub selected: usize,
}

/// Extract the
/// `[[link]]` and `#tag`
/// mentions from a
/// free-form text. Used by
/// `App::note_create_submit`
/// to pull structured
/// metadata out of the
/// combined Title + Content
/// for the heading line.
///
/// Returns `(links, tags)`:
/// - `links` is the list of
///   `[[...]]`-wrapped
///   mentions, in
///   first-seen order,
///   with no duplicates.
///   Each entry INCLUDES
///   the surrounding
///   `[[ ]]` (the same
///   shape the user typed
///   in the completion
///   menu), so the
///   heading builder can
///   drop them in verbatim
///   without re-wrapping.
/// - `tags` is the list of
///   `#tag` mentions,
///   stripped of the
///   leading `#` (the
///   heading builder
///   re-adds the `#` to
///   each one).
///
/// Both lists are
/// deduped (a tag / link
/// mentioned multiple
/// times appears once) but
/// NOT sorted — first-seen
/// order is preserved so
/// the heading reads
/// top-to-bottom the way
/// the user typed it.
pub fn extract_links_and_tags(text: &str) -> (Vec<String>, Vec<String>) {
    use std::collections::HashSet;
    let mut links: Vec<String> = Vec::new();
    let mut seen_links: HashSet<String> = HashSet::new();
    let mut tags: Vec<String> = Vec::new();
    let mut seen_tags: HashSet<String> = HashSet::new();
    // Scan for `[[...]]`
    // pairs. The
    // regex-free
    // manual scan is
    // robust to nested
    // brackets
    // (`[[Note with
    // [stuff] inside]]`),
    // which a naive
    // regex would not
    // handle.
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'['
            && let Some(end) = find_closing_link(&text[i + 2..]) {
                let inner = &text[i + 2..i + 2 + end];
                let full = format!("[[{}]]", inner);
                if seen_links.insert(full.clone()) {
                    links.push(full);
                }
                i += 2 + end + 2;
                continue;
            }
        i += 1;
    }
    // Scan for `#tag`. A
    // `#` is a tag
    // delimiter when it
    // appears at the
    // start of a token
    // (preceded by
    // whitespace, line
    // start, or a
    // punctuation
    // boundary). This
    // matches obsidian's
    // tag rule.
    for (idx, c) in text.char_indices() {
        if c != '#' {
            continue;
        }
        // Check
        // boundary:
        // either at
        // start of
        // text or
        // preceded
        // by a
        // non-word
        // character.
        let is_boundary = idx == 0
            || !text[..idx]
                .chars()
                .next_back()
                .map(|p| p.is_alphanumeric() || p == '_')
                .unwrap_or(false);
        if !is_boundary {
            continue;
        }
        // Collect the
        // tag body:
        // word chars
        // (alphanumeric
        // + `_` +
        // `-`).
        let rest = &text[idx + 1..];
        let tag_end = rest
            .char_indices()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '-')
            .last()
            .map(|(i, _)| i + rest[i..].chars().next().unwrap().len_utf8())
            .unwrap_or(0);
        if tag_end == 0 {
            continue;
        }
        let tag = rest[..tag_end].to_string();
        if seen_tags.insert(tag.clone()) {
            tags.push(tag);
        }
    }
    (links, tags)
}

/// Build the level-3-heading note body (`### Heading\n[time::
/// HH:MM]\ncontent`) from a title and content: extracts `[[link]]`s
/// and `#tag`s from both fields (via [`extract_links_and_tags`]) and
/// appends any not already present in the title into the heading.
/// Returns `None` when both fields are empty after trimming — the
/// only validation this function does; caller-specific checks (e.g.
/// `notes.database`/`notes.dir` being configured) are the caller's
/// responsibility.
///
/// Shared by the TUI's create-note dialog
/// (`App::note_create_build_body`) and `smarthistory create-note`'s
/// direct CLI path, so both build the exact same body from the exact
/// same title/content shape.
pub fn build_note_body(title: &str, content: &str) -> Option<String> {
    let title = title.trim();
    let content = content.trim();
    if title.is_empty() && content.is_empty() {
        return None;
    }
    let combined = format!("{}\n{}", title, content);
    let (links, tags) = extract_links_and_tags(&combined);
    let mut heading = title.to_string();
    for link in &links {
        if !heading.contains(link) {
            if !heading.is_empty() {
                heading.push(' ');
            }
            heading.push_str(link);
        }
    }
    for tag in &tags {
        if !heading.contains(tag) {
            if !heading.is_empty() {
                heading.push(' ');
            }
            heading.push('#');
            heading.push_str(tag);
        }
    }
    let now = chrono::Local::now();
    let time_str = now.format("%H:%M").to_string();
    Some(format!("### {}\n[time:: {}]\n{}", heading, time_str, content))
}

/// Find the byte index of the
/// closing `]]` for a `[[link]]`
/// span that starts at offset 0
/// of `s`. Returns `None` if no
/// closing `]]` is found.
///
/// Walks the string char-by-char
/// so multi-byte UTF-8
/// boundaries are respected.
/// Nested `[[...]]` are not
/// supported (we take the first
/// `]]` after the opening
/// `[[`) — obsidian doesn't
/// support nested link syntax
/// either, so this matches
/// user expectations.
fn find_closing_link(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b']' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Exit codes returned by the TUI binary, also used by the line-editor
/// widget to dispatch on. The shell snippet in `init zsh` reads these
/// to decide what to do with the chosen command.
pub mod exit_code {
    /// User pressed `Enter` — run the command (parent should submit
    /// the line).
    pub const RUN: i32 = 0;
    /// User pressed `Esc` / `Ctrl+C` — cancel, no command was chosen.
    pub const CANCEL: i32 = 1;
    /// User pressed `Right` — prefill the line for editing, cursor at
    /// the end.
    pub const EDIT_END: i32 = 2;
    /// User pressed `Left` — prefill the line for editing, cursor at
    /// the start.
    pub const EDIT_START: i32 = 3;
}

impl PickMode {
    pub fn exit_code(self) -> i32 {
        match self {
            PickMode::Run => exit_code::RUN,
            PickMode::EditEnd => exit_code::EDIT_END,
            PickMode::EditStart => exit_code::EDIT_START,
        }
    }
}

/// Filter the visible history by exit status. Cycled with
/// `Ctrl-J` (the `CycleExitFilter` action).
///
/// - `All`     — no filter; every row is shown (the default).
/// - `Success` — only rows with `exit_code == 0`.
/// - `Failed`  — only rows with `exit_code != 0`.
///
/// `next()` advances through the cycle in this order. The
/// `as_str()` and `parse()` helpers round-trip the value
/// through the persisted session file (`~/.cache/smarthistory/
/// session`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExitFilter {
    /// No exit-code filter.
    #[default]
    All,
    /// Only successful commands (exit_code == 0).
    Success,
    /// Only failed commands (exit_code != 0).
    Failed,
}

impl ExitFilter {
    /// Cycle to the next value. `All` → `Success` → `Failed` → `All`.
    pub fn next(self) -> Self {
        match self {
            ExitFilter::All => ExitFilter::Success,
            ExitFilter::Success => ExitFilter::Failed,
            ExitFilter::Failed => ExitFilter::All,
        }
    }

    /// Lowercase identifier for the session file and any future
    /// config-file knob: `all`, `ok`, `err`. Short and stable so
    /// it doesn't churn on display-name tweaks.
    pub fn as_str(self) -> &'static str {
        match self {
            ExitFilter::All => "all",
            ExitFilter::Success => "ok",
            ExitFilter::Failed => "err",
        }
    }

    /// Parse the persisted/config form. Accepts the canonical
    /// `as_str()` value plus a few friendly aliases (`success`/
    /// `failed` for the same thing as `ok`/`err`, and the
    /// upper-case versions for hand-edited session files).
    /// Returns `None` for anything else so the caller can fall
    /// back to the default.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "all" | "any" | "none" => Some(ExitFilter::All),
            "ok" | "success" | "0" => Some(ExitFilter::Success),
            "err" | "error" | "fail" | "failed" | "nonzero" | "non-zero" => {
                Some(ExitFilter::Failed)
            }
            _ => None,
        }
    }
}

/// The order rows are sorted in within the TUI history
/// list. Cycled with `F4` (the `CycleSortOrder` action).
///
/// - `Age`      — sort by timestamp DESC (the historical
///   default; newest commands at the bottom of the
///   bottom-aligned list).
/// - `Frequency` — sort by how many times each command
///   appears in the currently-filtered set, DESC.
///   Ties are broken by timestamp DESC (newest wins among
///   commands with the same count). Commands that appear
///   once still appear, just sorted alongside the more
///   frequent ones.
///
/// The counts are computed *within the current filtered
/// set* (the rows returned by the SQL `build_where` /
/// `fetch_stats` query, plus any labeled rows that
/// survived the filter). This means switching modes
/// (SESS/DIR/GLOBAL) or filters changes what "most
/// frequent" means — the count is always relative to
/// what the user is looking at. This is the same model
/// the user has when they say "show me my most-run
/// commands" while looking at a particular session or
/// directory: it's the most-run *here*, not globally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    /// Newest first (the historical default).
    #[default]
    Age,
    /// Most-frequent first, with timestamp DESC as a
    /// tie-breaker.
    Frequency,
}

impl SortOrder {
    /// Cycle to the next value. `Age` → `Frequency` → `Age`.
    /// Two values is the smallest useful cycle; the user
    /// can always press the key again to flip back.
    pub fn next(self) -> Self {
        match self {
            SortOrder::Age => SortOrder::Frequency,
            SortOrder::Frequency => SortOrder::Age,
        }
    }

    /// Lowercase identifier for the session file: `age`
    /// or `frequency`. Short and stable so it doesn't
    /// churn on display-name tweaks.
    pub fn as_str(self) -> &'static str {
        match self {
            SortOrder::Age => "age",
            SortOrder::Frequency => "frequency",
        }
    }

    /// Parse the persisted form. Accepts the canonical
    /// `as_str()` value plus a few friendly aliases
    /// (`freq`/`count`/`occurrences` for the same thing
    /// as `frequency`, and upper-case / dash-separated
    /// variants for hand-edited session files). Returns
    /// `None` for anything else so the caller can fall
    /// back to the default.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "age" | "time" | "newest" => Some(SortOrder::Age),
            "frequency" | "freq" | "count" | "occurrence" | "occurrences" => {
                Some(SortOrder::Frequency)
            }
            _ => None,
        }
    }
}

/// The active match algorithm, toggled by
/// `Action::CycleMatchAlgorithm` (default key `C-f`).
/// Applies to ALL prefix modes (history, directories,
/// panes, notes, todos, files, output) — wherever
/// `query_matches_text` is consulted. JIRA (`-` mode)
/// is exempt because it parses its own JQL syntax.
///
/// Defaults to `Substring` (the historical plain-text
/// behavior). The cycle is Substring → Fuzzy → Regex
/// → Substring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatchAlgorithm {
    /// Every whitespace-separated word must appear as
    /// a case-insensitive substring (the historical
    /// default — AND-by-word across command and comment
    /// text).
    #[default]
    Substring,
    /// Fuzzy subsequence match: every character of each
    /// word must appear in order (case-insensitive).
    /// Implements the same subsequence match as `fzf`,
    /// `sk`, `peco`, and similar fuzzy finders.
    Fuzzy,
    /// Regular expression match (uses the `regex` crate).
    /// Implicit `.*` anchors are added at both ends
    /// unless the user provides explicit `^` / `$`
    /// anchors.
    Regex,
}

impl MatchAlgorithm {
    /// Cycle to the next value.
    /// Substring → Fuzzy → Regex → Substring.
    pub fn next(self) -> Self {
        match self {
            MatchAlgorithm::Substring => MatchAlgorithm::Fuzzy,
            MatchAlgorithm::Fuzzy => MatchAlgorithm::Regex,
            MatchAlgorithm::Regex => MatchAlgorithm::Substring,
        }
    }

    /// Short display label for the mode-strip chip.
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            MatchAlgorithm::Substring => "SUB",
            MatchAlgorithm::Fuzzy => "FUZZY",
            MatchAlgorithm::Regex => "REGEX",
        }
    }

    /// Short prompt prefix shown in the input box.
    /// The body of the query is displayed after this.
    #[allow(dead_code)]
    pub fn prompt(self) -> &'static str {
        match self {
            MatchAlgorithm::Substring => "> ",
            MatchAlgorithm::Fuzzy => "? ",
            MatchAlgorithm::Regex => "/ ",
        }
    }

    /// Border title shown in the input box.
    #[allow(dead_code)]
    pub fn title(self) -> &'static str {
        match self {
            MatchAlgorithm::Substring => " history ",
            MatchAlgorithm::Fuzzy => " fuzzy ",
            MatchAlgorithm::Regex => " regex ",
        }
    }
}

#[cfg(test)]
mod tests;

/// Compute a unique `<prefix>.<key>` slug for a NEW config entry
/// named `name`, given the raw (as read from disk) contents of the
/// target config file. Scans every line for a key starting with
/// `<prefix>.` (bare `session.foo = ...` or sub-fielded
/// `session.foo.dir = ...`) and collects the part up to the first
/// `.` after the prefix — this picks up every key already in use,
/// including legacy numeric ids from `session.<id>`-style entries a
/// pre-slug config may still have (those collide with a slug just
/// like any other key would, so they're disambiguated the same way).
///
/// `crate::util::slugify(name, prefix)` derives the candidate slug
/// from the entry's display name (e.g. "SmartHistory" →
/// "smarthistory"); `crate::util::unique_slug` appends `-2`, `-3`, …
/// if it collides with something already in the file.
///
/// Used by the TUI's add-entry dialog (F5/F6) and the zoxide save
/// prompt to pick the key for a new `session.<key>`/`host.<key>`
/// line before appending it. The scan is line-based and matches only
/// the exact `<prefix>.` prefix at the start of the line (so
/// `sessiondirs=...` config keys, which happen to start with
/// `session`, are NOT matched — the match requires `<prefix>.`, i.e.
/// a literal dot after the prefix).
pub fn unique_config_slug(contents: &str, prefix: &str, name: &str) -> String {
    let needle = format!("{}.", prefix);
    let mut existing: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in contents.lines() {
        let line = line.trim_start();
        // The config syntax
        // is `key = value` —
        // we only care about
        // the key, so trim
        // at the first `=`
        // (or whitespace,
        // which separates the
        // key from the
        // value).
        let key = match line.find(|c: char| c == '=' || c.is_whitespace()) {
            Some(i) => &line[..i],
            None => line,
        };
        if let Some(rest) = key.strip_prefix(needle.as_str()) {
            // `rest` is everything after `<prefix>.` — for
            // `session.foo.dir` that's `foo.dir`; take up to the
            // first `.` to recover just the key (`foo`).
            let existing_key = rest.split('.').next().unwrap_or(rest);
            if !existing_key.is_empty() {
                existing.insert(existing_key.to_string());
            }
        }
    }
    crate::util::unique_slug(existing.iter().map(String::as_str), name, prefix)
}

#[cfg(test)]
mod unique_config_slug_tests;

#[cfg(test)]
mod add_entry_dialog_tests;
