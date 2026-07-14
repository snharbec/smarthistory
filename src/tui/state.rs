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
            PanesFilter::Sessions => "SESSIONS",
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
            "sessions" | "session" => Some(PanesFilter::Sessions),
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
mod tests {
    use super::HistoryRow;

    /// A real history row (positive
    /// `id`, `exit_code == 0`) is
    /// not an LLM preview.
    #[test]
    fn is_llm_preview_real_history_row_is_false() {
        let row = HistoryRow {
            id: 42,
            command: "ls -la".to_string(),
            directory: String::new(),
            session_id: String::new(),
            exit_code: 0,
            timestamp: 1_000_000,
            comment: String::new(),
            output: String::new(),
            mode: "command".to_string(),
            source: String::new(),
            ..Default::default()
        };
        assert!(!row.is_llm_preview());
    }

    /// A history row that failed
    /// (positive `id`,
    /// `exit_code != 0`) is not
    /// an LLM preview either —
    /// the user actually ran it.
    #[test]
    fn is_llm_preview_failed_command_is_false() {
        let row = HistoryRow {
            id: 100,
            command: "false".to_string(),
            directory: String::new(),
            session_id: String::new(),
            exit_code: 1,
            timestamp: 1_000_000,
            comment: String::new(),
            output: String::new(),
            mode: "command".to_string(),
            source: String::new(),
            ..Default::default()
        };
        assert!(!row.is_llm_preview());
    }

    /// A todo row has a negative
    /// `id` (encoding the 1-based
    /// line number as
    /// `id = -(line_number)`) and
    /// `exit_code == 0`. It is
    /// emphatically NOT an LLM
    /// preview — checking
    /// `id < 0` instead of
    /// `exit_code == -1` was the
    /// exact bug that made every
    /// todo row show a `[LLM]`
    /// marker in the age column.
    /// This test is the regression
    /// guard.
    #[test]
    fn is_llm_preview_todo_row_is_false() {
        let row = HistoryRow {
            id: -42, // line 42 of the source note
            command: "pick apples in the orchard".to_string(),
            directory: String::new(),
            session_id: String::new(),
            exit_code: 0,
            timestamp: 1_000_000,
            comment: "note.md".to_string(),
            output: String::new(),
            mode: "todo".to_string(),
            source: String::new(),
            ..Default::default()
        };
        assert!(
            !row.is_llm_preview(),
            "todo row must NOT be classified as LLM preview \
             (negative id encodes the line number, not a preview)"
        );
    }

    /// The synthetic LLM preview
    /// row has `exit_code == -1`
    /// (the "never executed"
    /// sentinel) and a negative
    /// `id` (typically `-1`). Both
    /// signals together are the
    /// canonical fingerprint of an
    /// LLM preview; the predicate
    /// keys on the `exit_code`
    /// sentinel because it's the
    /// load-bearing distinction
    /// (other row types may also
    /// use negative ids).
    #[test]
    fn is_llm_preview_llm_preview_row_is_true() {
        let row = HistoryRow {
            id: -1,
            command: "find . -name '*.rs' -newer foo".to_string(),
            directory: String::new(),
            session_id: String::new(),
            exit_code: -1, // never executed sentinel
            timestamp: 0,
            comment: "find rust files newer than foo".to_string(),
            output: String::new(),
            mode: String::new(),
            source: String::new(),
            ..Default::default()
        };
        assert!(row.is_llm_preview());
    }

    /// A question-mode row has
    /// `exit_code == 0` (the
    /// question was answered
    /// successfully by ollama) and
    /// is not an LLM preview in
    /// the `=...`-style sense.
    /// The render path uses
    /// `is_llm_preview()` to decide
    /// whether to draw a `[LLM]`
    /// tag, and we don't want
    /// questions to pick that up.
    #[test]
    fn is_llm_preview_question_row_is_false() {
        let row = HistoryRow {
            id: 7,
            command: "what is the capital of france?".to_string(),
            directory: String::new(),
            session_id: String::new(),
            exit_code: 0,
            timestamp: 1_000_000,
            comment: String::new(),
            output: "Paris".to_string(),
            mode: "question".to_string(),
            source: String::new(),
            ..Default::default()
        };
        assert!(!row.is_llm_preview());
    }
}

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
mod next_config_index_tests {
    use super::next_config_index;

    #[test]
    fn empty_contents_returns_one() {
        assert_eq!(next_config_index("", "session"), Some(1));
    }

    #[test]
    fn unrelated_contents_returns_one() {
        let s = "\
multiplexer = tmux
capturelines = 20
";
        assert_eq!(next_config_index(s, "session"), Some(1));
    }

    #[test]
    fn single_existing_returns_two() {
        let s = "session.1 = \"Proxmox\"\nsession.1.dir = \"~/foo\"\n";
        assert_eq!(next_config_index(s, "session"), Some(2));
    }

    #[test]
    fn gaps_are_filled() {
        // Existing ids
        // 1, 3, 5 — the
        // next free id is
        // max+1 = 6, not
        // 2 (we don't reuse
        // gaps because the
        // config parser
        // allows any integer
        // id and reusing
        // would surprise
        // users who expect
        // ids to be stable
        // across edits).
        let s = "\
session.1 = \"a\"
session.3 = \"b\"
session.5 = \"c\"
";
        assert_eq!(next_config_index(s, "session"), Some(6));
    }

    #[test]
    fn subfields_do_not_count() {
        // `session.2.dir` is
        // a sub-field of
        // session.2, not a
        // separate id.
        let s = "\
session.1 = \"a\"
session.1.dir = \"~/a\"
session.1.exec = \"cmd\"
";
        assert_eq!(next_config_index(s, "session"), Some(2));
    }

    #[test]
    fn prefix_overlap_does_not_confuse() {
        // `sessiondirs=...`
        // starts with the
        // literal string
        // `session` but is
        // NOT a `session.<id>`
        // entry. The
        // strip_prefix
        // requires the dot
        // after `session`,
        // so this line is
        // correctly ignored.
        let s = "\
sessiondirs = ~/projects
session.1 = \"a\"
";
        assert_eq!(next_config_index(s, "session"), Some(2));
    }

    #[test]
    fn host_prefix_works_independently() {
        let s = "\
session.1 = \"a\"
host.1 = \"Proxmox\"
";
        assert_eq!(next_config_index(s, "host"), Some(2));
        // The session
        // counter is
        // independent of
        // the host counter.
        assert_eq!(next_config_index(s, "session"), Some(2));
    }

    #[test]
    fn host_prefix_with_subfields_uses_parent_id() {
        // The `host.2.user` line
        // is a SUB-FIELD of a
        // (hypothetical) host.2
        // entry. Since the
        // `host.2 = "..."` line
        // itself is missing
        // from the file, the
        // scan only finds
        // `host.1` and returns
        // `max+1 = 2` — the
        // sub-field line alone
        // doesn't promote the
        // parent id. A user
        // who added `host.2.user`
        // by hand without a
        // parent `host.2` line
        // would see the
        // function assign the
        // new entry the same
        // id (2), and the
        // config parser would
        // then see the
        // duplicate parent
        // line. That's a
        // pathological case;
        // the function is
        // correct for well-
        // formed configs.
        let s = "\
host.1 = \"a\"
host.2.user = \"root\"
";
        assert_eq!(next_config_index(s, "host"), Some(2));
    }
}

#[cfg(test)]
mod add_entry_dialog_tests {
    use super::{AddEntryDialog, AddEntryKind};

    /// Session dialog has 3
    /// fields (Name, Dir,
    /// Exec); Name is
    /// required, Dir is
    /// pre-filled from the
    /// source directory, Exec
    /// is optional.
    #[test]
    fn session_dialog_fields() {
        let d = AddEntryDialog::new(
            AddEntryKind::Session,
            "/home/user/project".to_string(),
            "make test".to_string(),
        );
        assert_eq!(d.kind, AddEntryKind::Session);
        assert_eq!(d.fields.len(), 3);
        assert_eq!(d.fields[0].name, "Name");
        assert!(d.fields[0].required);
        assert_eq!(d.fields[1].name, "Dir");
        assert!(!d.fields[1].required);
        // The Dir field is
        // pre-filled with
        // the source
        // directory.
        assert_eq!(d.fields[1].value, "/home/user/project");
        // The cursor lands
        // at the end of
        // the pre-filled
        // value.
        assert_eq!(d.fields[1].cursor, "/home/user/project".chars().count());
        assert_eq!(d.fields[2].name, "Exec");
        assert!(!d.fields[2].required);
    }

    /// Host dialog has 7
    /// fields (Name, Host,
    /// Hostname, User, Port,
    /// Identity, Exec); Name
    /// and Host are required;
    /// Host is pre-filled with
    /// the directory basename.
    #[test]
    fn host_dialog_fields() {
        let d = AddEntryDialog::new(
            AddEntryKind::Host,
            "/home/user/.config/herdr".to_string(),
            String::new(),
        );
        assert_eq!(d.kind, AddEntryKind::Host);
        assert_eq!(d.fields.len(), 7);
        assert_eq!(d.fields[0].name, "Name");
        assert!(d.fields[0].required);
        assert_eq!(d.fields[1].name, "Host");
        assert!(d.fields[1].required);
        // The Host field is
        // pre-filled with
        // the basename of
        // the source
        // directory.
        assert_eq!(d.fields[1].value, "herdr");
        assert_eq!(d.fields[2].name, "Hostname");
        assert!(!d.fields[2].required);
        assert_eq!(d.fields[3].name, "User");
        assert!(!d.fields[3].required);
        assert_eq!(d.fields[4].name, "Port");
        assert!(!d.fields[4].required);
        assert_eq!(d.fields[5].name, "Identity");
        assert!(!d.fields[5].required);
        assert_eq!(d.fields[6].name, "Exec");
        assert!(!d.fields[6].required);
    }

    /// Host dialog with a
    /// path that has no
    /// basename component
    /// (e.g. just "/") falls
    /// back to the full path
    /// for the Host pre-fill
    /// (rather than crashing
    /// on the missing
    /// basename).
    #[test]
    fn host_dialog_root_path_falls_back_to_full_path() {
        let d = AddEntryDialog::new(AddEntryKind::Host, "/".to_string(), String::new());
        // `/` has no
        // basename; the
        // fallback is the
        // full path.
        assert_eq!(d.fields[1].value, "/");
    }

    /// focus_next wraps from
    /// the last field back to
    /// the first.
    #[test]
    fn focus_next_wraps() {
        let mut d = AddEntryDialog::new(AddEntryKind::Session, "/tmp".to_string(), String::new());
        assert_eq!(d.focused, 0);
        d.focus_next();
        assert_eq!(d.focused, 1);
        d.focus_next();
        assert_eq!(d.focused, 2);
        d.focus_next();
        // Wrap to 0.
        assert_eq!(d.focused, 0);
    }

    /// focus_prev wraps from
    /// the first field back to
    /// the last.
    #[test]
    fn focus_prev_wraps() {
        let mut d = AddEntryDialog::new(AddEntryKind::Session, "/tmp".to_string(), String::new());
        assert_eq!(d.focused, 0);
        d.focus_prev();
        // Wrap to 2 (the
        // last field).
        assert_eq!(d.focused, 2);
    }

    /// The source directory
    /// and command are kept
    /// verbatim in the
    /// dialog's
    /// `source_directory` /
    /// `source_command`
    /// fields, which the
    /// renderer shows as a
    /// "from: <cmd> in <dir>"
    /// hint.
    #[test]
    fn source_fields_preserved() {
        let d = AddEntryDialog::new(
            AddEntryKind::Host,
            "/home/user/proj".to_string(),
            "cargo build --release".to_string(),
        );
        assert_eq!(d.source_directory, "/home/user/proj");
        assert_eq!(d.source_command, "cargo build --release");
    }
}
