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
    /// The mode/type of this history entry: "command", "llm", or "question".
    pub mode: String,
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

/// One pane observed in
/// `tmux list-panes -a -F
/// '#S | #P | #{pane_current_path}'`.
/// The directories view shows a
/// per-row marker when at least
/// one pane's `path` matches the
/// row's `directory` (under
/// canonicalization), so the user
/// can see at a glance which
/// directories currently have
/// live tmux sessions attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxPaneInfo {
    /// Session name (`#S`). May
    /// contain spaces and shell
    /// metacharacters
    /// (e.g. `💾 Host PVE-1  `),
    /// so callers must treat it as
    /// an opaque label rather
    /// than a filesystem path.
    pub session: String,
    /// Pane id (`#P`). Format is
    /// `%<n>` where `<n>` is the
    /// pane's index within its
    /// window. Most users see
    /// `%0` (the only pane) or
    /// `%1`/`%2` (after a
    /// `split-window`).
    pub pane: String,
    /// Pane current working
    /// directory
    /// (`#{pane_current_path}`).
    /// Canonicalised at parse time
    /// so `/Users/har/x` and
    /// `/Volumes/HUGE/har/x`
    /// (macOS volume mount) map
    /// to the same string the
    /// directories-fetch code
    /// produces. Empty strings
    /// are filtered out at parse
    /// time (a brand-new pane
    /// has no cwd yet).
    pub path: String,
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
        };
        assert!(!row.is_llm_preview());
    }
}
