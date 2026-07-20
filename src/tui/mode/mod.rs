//! Per-prefix-mode modules.
//!
//! Each prefix mode (output, llm, question, notes, todo, directories,
//! panes, files, tags, ag, codegraph, jira, elements — and the implicit
//! "history" no-prefix mode) is a module of free functions in this
//! directory. The [`ModeKind`] enum is the single dispatch point:
//! every `if app.is_X_query() { ... } else if app.is_Y_query() { ... }`
//! chain across the codebase is collapsed into one
//! `match app.active_mode() { ModeKind::X => ..., ModeKind::Y => ... }`
//! against the enum.
//!
//! The convention is intentionally **not** a `trait PrefixMode`. See
//! the design discussion in `docs/configuration.md` / commit history:
//! a trait would force 12 specialised impls through one
//! `&mut self`-shaped entry point, which the borrow-checker punishes
//! and the modes' wildly different specialisations (notes has a date
//! filter, codegraph has a caller/callee overlay, panes has 3 filter
//! chips, jira has fragments, etc.) make worse. A `ModeKind` enum + a
//! convention of free functions gives the same single-dispatch-point
//! benefit at zero abstraction cost.
//!
//! Every mode module is expected to expose:
//! - `pub fn matches(app: &App) -> bool` — the active predicate.
//! - `pub fn pattern(app: &App) -> &str` — the body after the prefix
//!   char (empty when not in this mode).
//! - `pub const PREFIX_SLOT: &'static str` — the `QueryPrefixes` field
//!   name this mode reads from, so the dispatcher can resolve the
//!   configured prefix character without each module having to know
//!   about `QueryPrefixes`'s internals.
//!
//! Higher-mode-specific helpers (the `fetch_*` orchestration, the
//! per-mode render branches, the per-mode `smart_open` branch, the
//! per-mode help-text row) are exposed as additional free functions
//! when needed. The `App` struct keeps its existing
//! `is_<mode>_query` / `<mode>_pattern` methods as thin shims that
//! delegate here, so call sites can migrate at their own pace.
use crate::tui::App;
use crate::QueryPrefixes;

/// The active prefix mode for the current query. The discriminant
/// is the *first character* of the query (after the active match
/// algorithm's prefix has been applied; for a non-empty query the
/// first char is the prefix mode char). A query without any known
/// prefix character is [`ModeKind::History`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModeKind {
    /// Default mode — no prefix character. The query is matched
    /// against the history rows by command / cwd / directory.
    History,
    /// `+` (default). Searches the captured output of past commands.
    Output,
    /// `=` (default). LLM command generation. Body must be
    /// non-whitespace.
    Llm,
    /// `%` (default). General LLM question. Body must be
    /// non-whitespace.
    Question,
    /// `@` (default). Note search (uses the configured notes
    /// database).
    Notes,
    /// `!` (default). Markdown todo scanner.
    Todo,
    /// `#` (default). Unique directories from the global history.
    Directories,
    /// `*` (default). Multiplexer panes / windows / sessions /
    /// hosts. Reads from the configured backend (tmux or herdr).
    Panes,
    /// `~` (default). File browser rooted at the current directory.
    Files,
    /// `$` (default). ctags / CodeGraph symbol search.
    Tags,
    /// `,` (default). ag content search.
    Ag,
    /// `&` (default). CodeGraph FTS5 symbol search.
    Codegraph,
    /// `-` (default). JIRA issue search.
    Jira,
    /// `:` (default). Element search — finer-grained than `Notes`:
    /// searches individual paragraphs, list items (with nested
    /// children folded in), and headings via `note_search`'s
    /// `elements` table, rather than whole files.
    Elements,
}

impl ModeKind {
    /// The default prefix character for this mode. Matches
    /// [`QueryPrefixes::default`].
    #[allow(dead_code)] // convention API; not every consumer uses every method
    pub fn default_prefix(self) -> char {
        match self {
            ModeKind::History => '\0',
            ModeKind::Output => '+',
            ModeKind::Llm => '=',
            ModeKind::Question => '%',
            ModeKind::Notes => '@',
            ModeKind::Todo => '!',
            ModeKind::Directories => '#',
            ModeKind::Panes => '*',
            ModeKind::Files => '~',
            ModeKind::Tags => '$',
            ModeKind::Ag => ',',
            ModeKind::Codegraph => '&',
            ModeKind::Jira => '-',
            ModeKind::Elements => ':',
        }
    }

    /// Short display name used by the mode strip / prefix picker
    /// / help overlay.
    #[allow(dead_code)] // convention API; not every consumer uses every method
    pub fn display_name(self) -> &'static str {
        match self {
            ModeKind::History => "history",
            ModeKind::Output => "output",
            ModeKind::Llm => "llm",
            ModeKind::Question => "question",
            ModeKind::Notes => "notes",
            ModeKind::Todo => "todo",
            ModeKind::Directories => "directories",
            ModeKind::Panes => "panes",
            ModeKind::Files => "files",
            ModeKind::Tags => "tags",
            ModeKind::Ag => "ag",
            ModeKind::Codegraph => "codegraph",
            ModeKind::Jira => "jira",
            ModeKind::Elements => "elements",
        }
    }

    /// Title-case label for the result-list border.
    /// Used by `draw_list` so the list title reflects
    /// the active mode (e.g. "Notes — 42" instead of
    /// "History — 42" when the user is in notes mode).
    /// Each value is a short, properly-capitalized
    /// noun phrase; multi-word labels are spelled
    /// without the prefix character to keep the title
    /// compact. `ModeKind::History` keeps the
    /// historical "History" label for backwards
    /// compat (users have been reading that for
    /// years).
    #[allow(dead_code)] // convention API; the render layer uses this
    pub fn list_title(self) -> &'static str {
        match self {
            ModeKind::History => "History",
            ModeKind::Output => "Output search",
            ModeKind::Llm => "LLM command",
            ModeKind::Question => "Question",
            ModeKind::Notes => "Notes",
            ModeKind::Todo => "Todo",
            ModeKind::Directories => "Directories",
            ModeKind::Panes => "Panes",
            ModeKind::Files => "Files",
            ModeKind::Tags => "Tags",
            ModeKind::Ag => "Ag search",
            ModeKind::Codegraph => "CodeGraph",
            ModeKind::Jira => "JIRA",
            ModeKind::Elements => "Elements",
        }
    }

    /// The configured prefix character for this mode, read from
    /// `QueryPrefixes`. [`ModeKind::History`] returns `'\0'` (no
    /// prefix — the absence of a known prefix character is
    /// itself the indicator).
    #[allow(dead_code)] // convention API; not every consumer uses every method
    pub fn prefix(self, prefixes: &QueryPrefixes) -> char {
        match self {
            ModeKind::History => '\0',
            ModeKind::Output => prefixes.output,
            ModeKind::Llm => prefixes.llm,
            ModeKind::Question => prefixes.question,
            ModeKind::Notes => prefixes.notes,
            ModeKind::Todo => prefixes.todo,
            ModeKind::Directories => prefixes.directories,
            ModeKind::Panes => prefixes.panes,
            ModeKind::Files => prefixes.files,
            ModeKind::Tags => prefixes.tags,
            ModeKind::Ag => prefixes.ag,
            ModeKind::Codegraph => prefixes.codegraph,
            ModeKind::Jira => prefixes.jira,
            ModeKind::Elements => prefixes.elements,
        }
    }

    /// True for modes whose row list benefits from a
    /// dedup-by-command merge in `build_merged_rows`. The
    /// directories, panes, and jira modes each have their
    /// own dedup / no-dedup policy baked in elsewhere;
    /// this set is the "default dedup, by command" group
    /// (the history rows and per-file lists). Used by
    /// `App::build_merged_rows` to replace a long
    /// `is_X_query() || is_Y_query() || ...` chain.
    pub fn dedup_eligible(self) -> bool {
        matches!(
            self,
            ModeKind::Directories
                | ModeKind::Jira
                | ModeKind::Files
                | ModeKind::Tags
                | ModeKind::Codegraph
                | ModeKind::Ag
        )
    }
}

/// Resolve the active [`ModeKind`] from the current query. The
/// first non-whitespace character of the query is matched against
/// every configured prefix char; the first match wins. A query
/// that's empty or whose first char doesn't match any prefix falls
/// through to [`ModeKind::History`].
pub(crate) fn active_mode(app: &App) -> ModeKind {
    let q = &app.query;
    if q.is_empty() {
        return ModeKind::History;
    }
    let c = q.chars().next().unwrap();
    let p = &app.query_prefixes;
    if c == p.output {
        ModeKind::Output
    } else if c == p.llm {
        ModeKind::Llm
    } else if c == p.question {
        ModeKind::Question
    } else if c == p.notes {
        ModeKind::Notes
    } else if c == p.todo {
        ModeKind::Todo
    } else if c == p.directories {
        ModeKind::Directories
    } else if c == p.panes {
        ModeKind::Panes
    } else if c == p.files {
        ModeKind::Files
    } else if c == p.tags {
        ModeKind::Tags
    } else if c == p.ag {
        ModeKind::Ag
    } else if c == p.codegraph {
        ModeKind::Codegraph
    } else if c == p.jira {
        ModeKind::Jira
    } else if c == p.elements {
        ModeKind::Elements
    } else {
        ModeKind::History
    }
}

pub mod ag;
pub mod codegraph;
pub mod directories;
pub mod elements;
pub mod files;
pub mod jira;
pub mod llm;
pub mod notes;
pub mod output;
pub mod panes;
pub mod question;
pub mod tags;
pub mod todo;

/// Lazy-load the selected row's preview context for every mode that
/// needs it (tags/codegraph/notes/todo/files/panes/elements). Each
/// mode's own `ensure_selected_context` bails out immediately via its
/// own `matches(app)` check, so calling all seven unconditionally is
/// cheap and correct regardless of which mode is active — this is the
/// single dispatch point every call site should use instead of
/// re-listing the calls inline (previously duplicated across
/// `App::refresh`, `App::move_selection`, `App::show_output_view`,
/// and `run_loop`).
pub(crate) fn ensure_selected_context(app: &mut App) {
    crate::tui::mode::tags::ensure_selected_context(app);
    crate::tui::mode::codegraph::ensure_selected_context(app);
    crate::tui::mode::notes::ensure_selected_context(app);
    crate::tui::mode::todo::ensure_selected_context(app);
    crate::tui::mode::files::ensure_selected_context(app);
    crate::tui::mode::panes::ensure_selected_context(app);
    crate::tui::mode::elements::ensure_selected_context(app);
}

/// The colour used to tint the input border / title for a given
/// prefix mode. The history / no-prefix mode is a `None` (the
/// caller falls back to its own default). Implemented as a
/// per-mode lookup rather than a giant `match` in the renderer so
/// the colour rules are documented in one place per mode and the
/// renderer stays a flat dispatch.
pub(crate) fn input_title_style(mode: ModeKind) -> Option<ratatui::style::Style> {
    use crate::tui::theme::Theme;

    match mode {
        ModeKind::Output => Some(Theme::info()),
        ModeKind::Llm => Some(Theme::accent()),
        ModeKind::Question => Some(Theme::info()),
        ModeKind::Notes => Some(Theme::accent()),
        ModeKind::Todo => Some(Theme::warning()),
        ModeKind::Directories => Some(Theme::accent()),
        ModeKind::Panes => Some(Theme::success()),
        ModeKind::Files => Some(Theme::success()),
        ModeKind::Tags => Some(Theme::success()),
        ModeKind::Codegraph => Some(Theme::accent()),
        ModeKind::Ag => Some(Theme::warning()),
        ModeKind::Jira => Some(Theme::info()),
        ModeKind::Elements => Some(Theme::accent()),
        ModeKind::History => None,
    }
}

/// Build the `(prompt, title)` pair shown in the input border
/// for a given prefix mode. The `algo` string is the match-
/// algorithm suffix (e.g. " · fuzzy"). The `jql_last` argument
/// is only used by the JIRA mode (which shows the last issued
/// JQL in the title). The history / no-prefix mode returns
/// `("> ", " history{algo} ")`.
pub(crate) fn input_prompt_title(
    mode: ModeKind,
    algo: &str,
    jql_last: Option<&str>,
) -> (String, String) {
    match mode {
        ModeKind::Output => ("+".to_string(), format!(" output{} ", algo)),
        ModeKind::Llm => ("=".to_string(), " LLM ".to_string()),
        ModeKind::Notes => ("@".to_string(), format!(" notes{} ", algo)),
        ModeKind::Question => ("%".to_string(), " ? ".to_string()),
        ModeKind::Todo => ("!".to_string(), format!(" todo{} ", algo)),
        ModeKind::Directories => ("#".to_string(), format!(" directories{} ", algo)),
        ModeKind::Panes => ("*".to_string(), format!(" panes{} ", algo)),
        ModeKind::Jira => {
            let jql_title =
                jql_last.map_or_else(|| " jira ".to_string(), |j| format!(" jira ({}) ", j));
            ("-".to_string(), jql_title)
        }
        ModeKind::Files => ("~".to_string(), format!(" files{} ", algo)),
        ModeKind::Tags => ("$".to_string(), format!(" symbols{} ", algo)),
        ModeKind::Codegraph => ("&".to_string(), format!(" codegraph{} ", algo)),
        ModeKind::Ag => (",".to_string(), format!(" ag{} ", algo)),
        ModeKind::Elements => (":".to_string(), format!(" elements{} ", algo)),
        ModeKind::History => ("> ".to_string(), format!(" history{} ", algo)),
    }
}

// ============================================================================
// `smarthistory tui check` — prefix-mode health checks
// ============================================================================
//
// Every per-mode module exposes a `check(...)` function that
// returns a `CheckReport` describing whether the mode is
// configured and operational. The check is *progressive* —
// each mode digs down as far as it can:
//
//   1. Verify the configuration keys / env vars are set.
//   2. Verify the referenced files / sockets / DBs are reachable.
//   3. Verify the data layer (sqlite, FTS5, REST) accepts a
//      trivial request.
//   4. Run a representative query to surface deeper errors.
//
// The aggregate `CheckReport` (per `ModeKind`) is collected by
// the `tui check` CLI subcommand and rendered as a human-
// readable report. Exit code: 0 if all checks pass, 1 if any
// `Warning`, 2 if any `Error`.

/// The outcome of a single per-mode health check. The
/// progression `Ok` → `Warning` → `Error` corresponds to "the
/// mode is fully operational" / "the mode works but with
/// caveats" / "the mode cannot be used right now".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Mode is fully configured and operational. The
    /// `details` field carries human-readable info
    /// (row counts, version strings, etc.) for the report.
    Ok,
    /// Mode works but with caveats (e.g. the
    /// configuration is fine but a sample query
    /// returned an empty result, suggesting the data
    /// layer is healthy but there's nothing to find).
    Warning,
    /// Mode cannot be used. The `message` field carries
    /// the root-cause string (e.g. "notes.database
    /// is not configured", "ollama is unreachable",
    /// "tag file not found and CodeGraph index
    /// missing").
    Error,
}

/// A single per-mode health check. `mode` identifies
/// which prefix mode was checked; `status` is the
/// outcome; `message` is a one-line human-readable
/// summary; `details` is an optional list of
/// sub-checks (each with its own status + message)
/// for diagnostic depth.
///
/// The `mode` is included so the aggregate reporter
/// can group by mode and the JSON output can be
/// machine-parseable.
#[derive(Debug, Clone)]
pub struct CheckReport {
    pub mode: ModeKind,
    pub status: CheckStatus,
    pub message: String,
    /// Sub-checks, in order of execution. The first
    /// failure short-circuits the rest (no point
    /// trying a sample query if the DB doesn't
    /// open), so this is usually 1-3 entries.
    pub details: Vec<CheckReport>,
}

impl CheckReport {
    /// Build a successful report with no sub-checks.
    pub(crate) fn ok(mode: ModeKind, message: impl Into<String>) -> Self {
        Self {
            mode,
            status: CheckStatus::Ok,
            message: message.into(),
            details: Vec::new(),
        }
    }

    /// Build a warning report with no sub-checks.
    pub(crate) fn warn(mode: ModeKind, message: impl Into<String>) -> Self {
        Self {
            mode,
            status: CheckStatus::Warning,
            message: message.into(),
            details: Vec::new(),
        }
    }

    /// Build an error report with no sub-checks.
    pub(crate) fn err(mode: ModeKind, message: impl Into<String>) -> Self {
        Self {
            mode,
            status: CheckStatus::Error,
            message: message.into(),
            details: Vec::new(),
        }
    }

    /// Push a sub-check onto this report. Returns
    /// `self` for chaining. If the sub-check is an
    /// `Error` and the parent is `Ok`, the parent
    /// status is *not* automatically downgraded —
    /// each level is independent (a sub-check
    /// failure may be more specific / recoverable
    /// than the parent). The aggregate reporter
    /// considers the worst-case status across
    /// the whole tree.
    pub(crate) fn with(mut self, sub: CheckReport) -> Self {
        self.details.push(sub);
        self
    }

    /// The worst-case status across this report and
    /// all its sub-checks. Used by the aggregate
    /// reporter to compute the overall pass / warn /
    /// fail verdict.
    pub fn worst_status(&self) -> CheckStatus {
        let mut worst = self.status;
        for d in &self.details {
            let sub = d.worst_status();
            if rank(sub) > rank(worst) {
                worst = sub;
            }
        }
        worst
    }

    /// True if this report (or any sub-check)
    /// contains an `Error` status. Used by the CLI
    /// to compute the exit code.
    #[allow(dead_code)] // convention API; kept for consumers that prefer the boolean form
    pub fn has_errors(&self) -> bool {
        self.worst_status() == CheckStatus::Error
    }

    /// True if this report (or any sub-check)
    /// contains a `Warning` status (but not an
    /// `Error`).
    #[allow(dead_code)] // convention API; kept for consumers that prefer the boolean form
    pub fn has_warnings(&self) -> bool {
        matches!(self.worst_status(), CheckStatus::Warning)
    }
}

/// Numeric rank for `CheckStatus` so
/// `worst_status()` is straightforward. Higher = worse.
fn rank(s: CheckStatus) -> u8 {
    match s {
        CheckStatus::Ok => 0,
        CheckStatus::Warning => 1,
        CheckStatus::Error => 2,
    }
}

/// Run the full set of per-mode checks. When
/// `only` is `Some`, only that single mode is
/// checked. The result is a flat list of reports
/// in the same order as `ModeKind::all()`.
pub fn run_all_checks(
    app: &App,
    only: Option<ModeKind>,
) -> Vec<CheckReport> {
    let mut reports = Vec::new();
    // When `only` is set, check just that mode.
    // When `None`, check every mode in `all()`.
    // (History / Output / Question modes are
    // skipped inside the loop since they share
    // the SQL history DB and have no external
    // dependency to check.)
    let modes: Vec<ModeKind> = only.into_iter().collect();
    let modes = if modes.is_empty() {
        ModeKind::all().to_vec()
    } else {
        modes
    };
    for mode in modes {
        if matches!(
            mode,
            ModeKind::History | ModeKind::Output | ModeKind::Question
        ) {
            // The history / output / question modes
            // share the SQL history DB and have no
            // external dependencies. There's nothing
            // to "check" beyond confirming the DB is
            // open, which the TUI startup itself
            // already verified. Skip them with an
            // informational "Ok" so the user sees
            // them in the report (no surprise
            // "missing" mode) but doesn't see false
            // positives.
            reports.push(CheckReport::ok(
                mode,
                "no external dependencies; uses the local history DB",
            ));
            continue;
        }
        let report = match mode {
            ModeKind::Notes => crate::tui::mode::notes::check(app),
            ModeKind::Todo => crate::tui::mode::todo::check(app),
            ModeKind::Elements => crate::tui::mode::elements::check(app),
            ModeKind::Tags => crate::tui::mode::tags::check(app),
            ModeKind::Codegraph => crate::tui::mode::codegraph::check(app),
            ModeKind::Files => crate::tui::mode::files::check(app),
            ModeKind::Ag => crate::tui::mode::ag::check(app),
            ModeKind::Llm => crate::tui::mode::llm::check(app),
            ModeKind::Jira => crate::tui::mode::jira::check(app),
            ModeKind::Directories => crate::tui::mode::directories::check(app),
            ModeKind::Panes => crate::tui::mode::panes::check(app),
            ModeKind::History | ModeKind::Output | ModeKind::Question => unreachable!(),
        };
        reports.push(report);
    }
    reports
}

impl ModeKind {
    /// The canonical list of modes for diagnostic
    /// purposes (one entry per prefix). Excludes
    /// `History` (the no-prefix default) and
    /// `Output` / `Question` (which have no
    /// external dependency to check).
    pub fn all() -> &'static [ModeKind] {
        &[
            ModeKind::Notes,
            ModeKind::Todo,
            ModeKind::Elements,
            ModeKind::Tags,
            ModeKind::Codegraph,
            ModeKind::Files,
            ModeKind::Ag,
            ModeKind::Llm,
            ModeKind::Jira,
            ModeKind::Directories,
            ModeKind::Panes,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::ModeKind;

    /// `ModeKind::Todo` uses "Todo" (singular) to
    /// match the existing `display_name` value
    /// ("todo"). The list is the "todo mode",
    /// not "the todos mode" — the title is the
    /// mode label, not a count noun. Tests
    /// below assert the display_name ↔
    /// list_title round-trip is exact.
    #[test]
    fn list_title_is_title_case_and_non_empty() {
        for kind in [
            ModeKind::History,
            ModeKind::Output,
            ModeKind::Llm,
            ModeKind::Question,
            ModeKind::Notes,
            ModeKind::Todo,
            ModeKind::Directories,
            ModeKind::Panes,
            ModeKind::Files,
            ModeKind::Tags,
            ModeKind::Ag,
            ModeKind::Codegraph,
            ModeKind::Jira,
            ModeKind::Elements,
        ] {
            let title = kind.list_title();
            assert!(!title.is_empty(), "{:?} returned empty title", kind);
            assert_eq!(
                title,
                title.trim(),
                "{:?} title has leading / trailing whitespace",
                kind
            );
            // The FIRST character must be an
            // uppercase ASCII letter (or a
            // non-ASCII capital). The test
            // allows the rest of the title to
            // be lowercase — "History" is a
            // valid title even though it
            // contains lowercase letters
            // after the leading capital.
            let first = title.chars().next().expect("non-empty");
            assert!(
                first.is_ascii_uppercase() || first.is_uppercase(),
                "{:?} title {:?} doesn't start with an uppercase letter",
                kind,
                title
            );
        }
    }

    /// `History` keeps the historical label so the
    /// existing UX is preserved. This is a
    /// regression test for the "don't break
    /// muscle memory" requirement.
    #[test]
    fn history_mode_title_is_history() {
        assert_eq!(ModeKind::History.list_title(), "History");
    }

    /// Every non-history mode has a title that
    /// is DISTINCT from "History" — the user's
    /// whole point of asking for this change was
    /// to see the active mode in the title, not
    /// always "History". This guards against a
    /// future edit accidentally regressing one
    /// variant to the default.
    #[test]
    fn non_history_modes_have_distinct_titles() {
        let others = [
            ModeKind::Output,
            ModeKind::Llm,
            ModeKind::Question,
            ModeKind::Notes,
            ModeKind::Todo,
            ModeKind::Directories,
            ModeKind::Panes,
            ModeKind::Files,
            ModeKind::Tags,
            ModeKind::Ag,
            ModeKind::Codegraph,
            ModeKind::Jira,
        ];
        for kind in others {
            let title = kind.list_title();
            assert_ne!(
                title, "History",
                "{:?} should have a distinct title, not 'History'",
                kind
            );
        }
    }

    /// `ModeKind::display_name` (the existing
    /// lowercase helper used by the mode strip /
    /// help overlay) is independent of
    /// `list_title`. They serve different
    /// surfaces: the strip is a compact
    /// single-line label, the title is a
    /// user-facing border label. They don't
    /// have to be related in form, but for the
    /// common single-word modes (Notes / Files /
    /// Tags / etc.) the lowercase of the title
    /// should equal the display name. The
    /// multi-word titles (Output search / LLM
    /// command / ag search) are excluded from
    /// the round-trip check.
    #[test]
    fn display_name_agrees_for_single_word_modes() {
        let single_word_kinds = [
            ModeKind::History,
            ModeKind::Notes,
            ModeKind::Todo,
            ModeKind::Files,
            ModeKind::Tags,
            ModeKind::Jira,
        ];
        for kind in single_word_kinds {
            let title = kind.list_title();
            let display = kind.display_name();
            assert_eq!(
                display,
                title.to_ascii_lowercase(),
                "{:?} display_name ({:?}) should equal the lowercase of list_title ({:?})",
                kind,
                display,
                title
            );
        }
    }
}
