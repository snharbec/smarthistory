//! Per-prefix-mode modules.
//!
//! Each prefix mode (output, llm, question, notes, todo, directories,
//! panes, files, tags, ag, codegraph, jira — and the implicit
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
    } else {
        ModeKind::History
    }
}

pub mod ag;
pub mod codegraph;
pub mod directories;
pub mod files;
pub mod jira;
pub mod llm;
pub mod notes;
pub mod output;
pub mod panes;
pub mod question;
pub mod tags;
pub mod todo;

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
        ModeKind::History => ("> ".to_string(), format!(" history{} ", algo)),
    }
}
