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
    /// selected row (used
    /// purely as a status
    /// hint in the dialog
    /// title — the entry
    /// itself doesn't carry
    /// the command).
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
            AddEntryKind::Host => vec![
                DialogField::new("Name", "", true, "Proxmox"),
                DialogField::prefilled(
                    "Host",
                    ".host",
                    true,
                    "pve-1",
                    std::path::Path::new(&source_directory)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| source_directory.clone()),
                ),
                DialogField::new(
                    "Hostname",
                    ".hostname",
                    false,
                    "real.host (overrides SSH config)",
                ),
                DialogField::new("User", ".user", false, "alice (defaults to $USER)"),
                DialogField::new("Port", ".port", false, "22"),
                DialogField::new("Identity", ".identity", false, "~/.ssh/id_ed25519"),
                DialogField::new("Exec", ".exec", false, "command to run after ssh"),
            ],
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

/// Find the next free
/// `<prefix>.<id>` index
/// in a config file. Scans
/// every line for entries
/// matching
/// `<prefix>.<number>...`
/// (the number is the
/// integer before the
/// first `.` that follows
/// the prefix), tracks the
/// maximum seen, and
/// returns `max + 1`.
///
/// Returns `None` only when
/// the existing indices are
/// at `usize::MAX` (a
/// configuration with
/// `session.18446744073709551615`
/// or similar). In practice
/// this is impossible (the
/// user would have to add
/// entries one at a time
/// for 18 quintillion
/// years) so the `None`
/// case is a defensive
/// guard, not a real-world
/// failure mode.
///
/// Used by the TUI's
/// add-entry dialog to pick
/// the id for a new
/// `session.<id>` or
/// `host.<id>` line before
/// appending it. The scan
/// is line-based and
/// matches only the exact
/// `<prefix>.` prefix at
/// the start of the line
/// (so `sessiondirs=...`
/// config keys, which
/// happen to start with
/// `session`, are NOT
/// matched — the regex
/// requires `<prefix>.`,
/// i.e. a literal dot after
/// the prefix).
pub fn next_config_index(contents: &str, prefix: &str) -> Option<usize> {
    let needle = format!("{}.", prefix);
    let mut max: usize = 0;
    let mut found_any = false;
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
        // Must start with
        // `<prefix>.` AND
        // the rest of the key
        // (after the dot) must
        // be a valid integer
        // (i.e. no further
        // dots, no other
        // suffix characters).
        if let Some(rest) = key.strip_prefix(needle.as_str()) {
            // The `rest` is
            // everything after
            // `<prefix>.`. For
            // `session.3.dir`,
            // that's `3.dir`,
            // which is not a
            // valid integer. We
            // want to match only
            // the bare `session.3`
            // line.
            if let Ok(n) = rest.parse::<usize>() {
                if n >= max {
                    max = n;
                }
                found_any = true;
            }
        }
    }
    if !found_any {
        // No existing entry:
        // start at 1 (the
        // config syntax
        // expects positive
        // integer ids, and
        // `session.0` would
        // be ambiguous in
        // some downstream
        // parsers).
        return Some(1);
    }
    max.checked_add(1)
}

#[cfg(test)]
mod next_config_index_tests;

#[cfg(test)]
mod add_entry_dialog_tests;
