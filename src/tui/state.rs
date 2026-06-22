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

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRow {
    pub id: i64,
    pub command: String,
    pub directory: String,
    pub session_id: String,
    pub exit_code: i32,
    pub timestamp: i64,
    pub comment: String,
    pub output: String,
}

/// How the parent shell should treat the chosen command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    /// `Enter` — run the command (parent should submit the line).
    Run,
    /// `Left` — prefill the line for editing, cursor at the start.
    EditStart,
    /// `Right` — prefill the line for editing, cursor at the end.
    EditEnd,
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
