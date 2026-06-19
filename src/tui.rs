use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use rusqlite::{params, Connection};
use std::time::Duration;

use crate::util::{format_diff, format_time};
use crate::Config;
use regex::Regex;
use std::path::PathBuf;

/// Persistent state of the last TUI session. Stored in
/// `~/.cache/smarthistory/session` and reloaded on the next TUI
/// invocation so that the user's mode, query, and duplicate-filter
/// preferences carry over.
#[derive(Debug, Default, Clone)]
struct TuiSession {
    /// Last used search mode (e.g. "SESS", "DIR", "GLOBAL").
    mode: Option<String>,
    /// Last entered search query.
    query: Option<String>,
    /// Last duplicate-filter state. `None` means "no preference" and
    /// falls back to the config-file default.
    duplicate_filter: Option<bool>,
}

impl TuiSession {
    fn path() -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        Some(
            PathBuf::from(home)
                .join(".local")
                .join("cache")
                .join("smarthistory")
                .join("session"),
        )
    }

    /// Load the persisted session from disk, if available. Missing
    /// files or unparseable contents yield a default (empty) session
    /// rather than an error so the TUI can always start.
    fn load() -> Self {
        let Some(path) = Self::path() else { return Self::default() };
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let mut s = Self::default();
        for raw_line in contents.lines() {
            let line = raw_line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = match line.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            };
            match key {
                "mode" => s.mode = Some(value.to_string()),
                "query" => s.query = Some(value.to_string()),
                "duplicatefilter" => s.duplicate_filter = Some(parse_bool(value, true)),
                _ => {}
            }
        }
        s
    }

    /// Persist the current session to disk. Best-effort: any I/O
    /// error is logged to stderr but does not propagate, since the
    /// TUI is exiting anyway.
    fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut out = String::new();
        if let Some(ref m) = self.mode {
            out.push_str(&format!("mode={}\n", m));
        }
        if let Some(ref q) = self.query {
            out.push_str(&format!("query={}\n", q));
        }
        if let Some(d) = self.duplicate_filter {
            out.push_str(&format!("duplicatefilter={}\n", if d { "on" } else { "off" }));
        }
        if let Err(e) = std::fs::write(&path, out) {
            eprintln!("warning: failed to persist TUI session: {}", e);
        }
    }
}

/// Simple boolean parser used by both the global config and the
/// per-session state file. Kept local (not in `util.rs`) so the TUI
/// module stays self-contained for the session file format.
fn parse_bool(s: &str, default: bool) -> bool {
    match s.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => true,
        "off" | "false" | "0" | "no" => false,
        _ => default,
    }
}

/// Search scope for the TUI. Mirrors the line-editor widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Sess,
    Dir,
    Global,
}

impl Mode {
    fn next(self) -> Self {
        match self {
            Mode::Sess => Mode::Dir,
            Mode::Dir => Mode::Global,
            Mode::Global => Mode::Sess,
        }
    }
    /// Parse a string like "SESS", "SESSION", "DIR", "DIRECTORY",
    /// "GLOBAL" (case-insensitive). Returns None for anything else.
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "SESS" | "SESSION" => Some(Mode::Sess),
            "DIR" | "DIRECTORY" => Some(Mode::Dir),
            "GLOBAL" => Some(Mode::Global),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // some fields are kept for future display
struct HistoryRow {
    id: i64,
    command: String,
    directory: String,
    session_id: String,
    exit_code: i32,
    timestamp: i64,
    comment: String,
    output: String,
}

/// How the parent shell should treat the chosen command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickMode {
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
    fn exit_code(self) -> i32 {
        match self {
            PickMode::Run => exit_code::RUN,
            PickMode::EditEnd => exit_code::EDIT_END,
            PickMode::EditStart => exit_code::EDIT_START,
        }
    }
}

/// Consistent color palette and styles for the TUI.
/// Resolve a color string into a ratatui `Color`. Supports the
/// standard CSS-style named colors plus the 16-color terminal palette
/// that `ratatui::Color` exposes. Hex strings of the form `#rrggbb`
/// or `0xrrggbb` are also accepted. Unknown strings fall back to
/// `Color::Reset`, which lets the terminal decide.
fn resolve_color(s: &str) -> Color {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#').or_else(|| s.strip_prefix("0x"))
        && hex.len() == 6
            && let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&hex[0..2], 16),
                u8::from_str_radix(&hex[2..4], 16),
                u8::from_str_radix(&hex[4..6], 16),
            ) {
                return Color::Rgb(r, g, b);
            }
    match s.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        "reset" => Color::Reset,
        _ => Color::Reset,
    }
}

/// Filter by exit status. Cycled with Ctrl+S in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `label` kept for future use (e.g. larger displays)
enum ExitFilter {
    /// No exit-code filter.
    All,
    /// Only successful commands (exit_code == 0).
    Success,
    /// Only failed commands (exit_code != 0).
    Failed,
}

impl ExitFilter {
    #[allow(dead_code)]
    fn next(self) -> Self {
        match self {
            ExitFilter::All => ExitFilter::Success,
            ExitFilter::Success => ExitFilter::Failed,
            ExitFilter::Failed => ExitFilter::All,
        }
    }
}

/// Active TUI palette. Holds the resolved colors used by the draw
/// helpers below. Populated once at TUI startup from the user's
/// `Config::theme()` (via `tuicolor.<field>=<value>` overrides), then
/// read through the `Theme::*` style helpers that the rest of the
/// TUI code already calls. This indirection keeps the call sites
/// unchanged while allowing per-user theming.
#[derive(Debug, Clone, Copy)]
struct Palette {
    bg: Color,
    fg: Color,
    accent: Color,
    success: Color,
    error: Color,
    warning: Color,
    dim: Color,
    #[allow(dead_code)]
    dimmer: Color,
    highlight: Color,
}

impl Palette {
    fn builtin() -> Self {
        Palette {
            bg: Color::Black,
            fg: Color::Gray,
            accent: Color::Cyan,
            success: Color::Green,
            error: Color::Red,
            warning: Color::Yellow,
            dim: Color::Gray,
            dimmer: Color::DarkGray,
            highlight: Color::Yellow,
        }
    }

    fn from_config(theme: &crate::TuiTheme) -> Self {
        Palette {
            bg: resolve_color(&theme.bg),
            fg: resolve_color(&theme.fg),
            accent: resolve_color(&theme.accent),
            success: resolve_color(&theme.success),
            error: resolve_color(&theme.error),
            warning: resolve_color(&theme.warning),
            dim: resolve_color(&theme.dim),
            dimmer: Color::DarkGray,
            highlight: resolve_color(&theme.highlight),
        }
    }
}

thread_local! {
    static PALETTE: std::cell::RefCell<Palette> = std::cell::RefCell::new(Palette::builtin());
}

/// Style helpers used throughout the TUI. Each reads the current
/// color from the active `Palette`. Keeping the original call-site
/// signatures (`Theme::error()`, etc.) means none of the rendering
/// code needs to change.
struct Theme;

impl Theme {
    fn default() -> Style {
        let p = PALETTE.with(|c| *c.borrow());
        Style::default().fg(p.fg).bg(p.bg)
    }

    fn accent() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().accent))
    }

    fn success() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().success))
    }

    fn error() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().error))
    }

    fn dim() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().dim))
    }

    #[allow(dead_code)]
    fn dimmer() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().dimmer))
    }

    fn highlight() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().highlight))
    }

    #[allow(dead_code)]
    fn warning() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().warning))
    }

    #[allow(dead_code)]
    const BG: Color = Color::Black;
    #[allow(dead_code)]
    const FG: Color = Color::Gray;
    #[allow(dead_code)]
    const ACCENT: Color = Color::Cyan;
    #[allow(dead_code)]
    const SUCCESS: Color = Color::Green;
    #[allow(dead_code)]
    const ERROR: Color = Color::Red;
    #[allow(dead_code)]
    const WARNING: Color = Color::Yellow;
    #[allow(dead_code)]
    const DIM: Color = Color::Gray;
    #[allow(dead_code)]
    const DIMMER: Color = Color::DarkGray;
    #[allow(dead_code)]
    const HIGHLIGHT: Color = Color::Yellow;

    /// Read the current accent color from the active palette.
    /// Used by badge renderers that need a raw `Color` rather than
    /// a full `Style`.
    fn accent_color() -> Color {
        PALETTE.with(|c| c.borrow().accent)
    }
    fn success_color() -> Color {
        PALETTE.with(|c| c.borrow().success)
    }
    fn error_color() -> Color {
        PALETTE.with(|c| c.borrow().error)
    }
    fn warning_color() -> Color {
        PALETTE.with(|c| c.borrow().warning)
    }
    fn highlight_color() -> Color {
        PALETTE.with(|c| c.borrow().highlight)
    }
    #[allow(dead_code)]
    fn dim_color() -> Color {
        PALETTE.with(|c| c.borrow().dim)
    }
}

struct App {
    conn: Connection,
    mode: Mode,
    duplicate_filter: bool,
    query: String,
    rows: Vec<HistoryRow>,
    list_state: ListState,
    selection: Option<String>,
    pick_mode: Option<PickMode>,
    cancelled: bool,
    /// When `Some`, we are editing the comment of a history row.
    /// The `String` is the live edit buffer.
    comment_edit: Option<String>,
    /// When `Some`, we are viewing the captured output of a history
    /// row in a full-screen overlay.
    output_view: Option<OutputView>,
    /// When `Some`, we are prompting for deletion confirmation.
    confirm_delete: Option<ConfirmMode>,
    /// Cached set of all history rows that have a comment, used to
    /// populate the optional labeled entries pane.
    labeled_rows: Vec<HistoryRow>,
    /// List state for the labeled entries pane (separate from
    /// `list_state` so the two views can remember their own selection).
    labeled_list_state: ListState,
    /// True when the initial query was loaded from the persisted
    /// session file (so the user is editing a previously-saved query
    /// rather than typing fresh text). The first character typed
    /// replaces the prefilled value instead of appending to it.
    query_prefilled: bool,
    /// True once the user has touched the query buffer (typed,
    /// deleted, or cleared). After this point, additional input
    /// appends normally — even if the buffer is later emptied.
    query_touched: bool,
    /// Compiled regex when the query starts with `/`. `None` when
    /// the query is plain text, the query is empty, or the regex
    /// failed to compile (in which case we silently fall back to
    /// the plain-text path so the user can keep editing).
    query_regex: Option<Regex>,
}

impl App {
    /// True if the current query is a regex (prefixed with `/`).
    fn is_regex_query(&self) -> bool {
        self.query.starts_with('/')
    }

    /// The regex pattern, i.e. everything after the leading `/`.
    /// Empty when the query is just `/`.
    fn regex_pattern(&self) -> &str {
        if self.is_regex_query() {
            &self.query[1..]
        } else {
            ""
        }
    }

    /// Recompile the regex from the current query. Called whenever
    /// the query buffer changes. Failures (invalid regex) leave the
    /// previous compiled regex in place so the user can keep typing
    /// without the list flickering empty.
    fn recompile_regex(&mut self) {
        if !self.is_regex_query() {
            self.query_regex = None;
            return;
        }
        let pattern = self.regex_pattern();
        match Regex::new(pattern) {
            Ok(re) => self.query_regex = Some(re),
            Err(_) => {
                // Leave the previous regex (if any) in place; the
                // user is mid-edit and we'll retry on the next
                // keystroke. This avoids the list briefly going
                // empty for a transient typo like an unbalanced
                // bracket.
            }
        }
    }

    /// Return true if the given text matches the current query:
    /// either the plain-text substring search (multi-word, AND),
    /// or the compiled regex when the query starts with `/`.
    fn query_matches_text(&self, text: &str) -> bool {
        if self.query.is_empty() {
            return true;
        }
        if self.is_regex_query() {
            if let Some(ref re) = self.query_regex {
                return re.is_match(text);
            }
            // Regex mode but no valid compiled regex yet — treat
            // the entire post-slash text as a literal pattern so
            // the user sees at least the matches that contain it.
            return text.to_lowercase().contains(&self.query[1..].to_lowercase());
        }
        // Plain text: every whitespace-separated word must appear
        // (case-insensitive).
        let lower = text.to_lowercase();
        self.query
            .split_whitespace()
            .all(|w| lower.contains(&w.to_lowercase()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmMode {
    DeleteSelected,
    DeleteMatching,
}

/// State for the captured-output overlay: the captured text plus a
/// scroll offset (number of lines scrolled past the top).
struct OutputView {
    text: String,
    scroll: usize,
}

impl App {
    fn new(conn: Connection, initial_mode: Mode, initial_query: String, duplicate_filter: bool, query_prefilled: bool) -> Self {
        let list_state = ListState::default();
        let mut app = App {
            conn,
            mode: initial_mode,
            duplicate_filter,
            query: initial_query,
            rows: Vec::new(),
            list_state,
            selection: None,
            pick_mode: None,
            cancelled: false,
            comment_edit: None,
            output_view: None,
            confirm_delete: None,
            labeled_rows: Vec::new(),
            labeled_list_state: ListState::default(),
            query_prefilled,
            query_touched: false,
            query_regex: None,
        };
        app.recompile_regex();
        app.refresh();
        app.refresh_labeled();
        // Rows are ordered newest first; index 0 is the newest entry.
        // Keep the selection on the newest match so it appears at the
        // bottom of the bottom-aligned list.
        if !app.rows.is_empty() {
            app.list_state.select(Some(0));
        }
        if !app.labeled_rows.is_empty() {
            app.labeled_list_state.select(Some(0));
        }
        app
    }

    /// Re-query the database with the current mode + query.
    /// After re-querying, land on the newest match (index 0 in the
    /// merged list, which is the bottom of the bottom-aligned render).
    /// When the query is a regex, post-filter the SQL results using
    /// `query_matches_text` so the regex can match anywhere in the
    /// command or comment text.
    fn refresh(&mut self) {
        self.rows = self.fetch().unwrap_or_default();
        if self.is_regex_query() {
            // Two-phase borrow: copy the rows out, then post-filter.
            // Avoids the borrow checker complaining about
            // simultaneously borrowing `self.rows` and `self`.
            let query = self.query.clone();
            let regex = self.query_regex.clone();
            self.rows.retain(|r| {
                if let Some(ref re) = regex {
                    re.is_match(&r.command) || re.is_match(&r.comment)
                } else {
                    // No valid regex yet (in-progress typo) — fall
                    // back to a literal substring match on the
                    // post-slash text so the user sees *something*.
                    r.command
                        .to_lowercase()
                        .contains(&query[1..].to_lowercase())
                        || r
                            .comment
                            .to_lowercase()
                            .contains(&query[1..].to_lowercase())
                }
            });
        }
        self.refresh_labeled();
        let n = self.merged_rows().len();
        if n == 0 {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    fn fetch(&self) -> Result<Vec<HistoryRow>> {
        let (where_clause, params) = self.build_where();
        let sql = format!(
            "SELECT h.id, h.command, h.directory, h.session_id, h.exit_code, h.timestamp, c.comment, o.output \
             FROM history h \
             LEFT JOIN command_comments c ON h.command = c.command \
             LEFT JOIN history_output o ON h.id = o.history_id{} \
             ORDER BY h.timestamp DESC LIMIT 1000",
            where_clause
        );
        let params_ref: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(&params_ref[..], |row| {
                Ok(HistoryRow {
                    id: row.get(0)?,
                    command: row.get(1)?,
                    directory: row.get(2)?,
                    session_id: row.get(3)?,
                    exit_code: row.get(4)?,
                    timestamp: row.get(5)?,
                    comment: row.get(6).unwrap_or_default(),
                    output: row.get(7).unwrap_or_default(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Merge `labeled_rows` (entries with a comment that are NOT already
    /// in `rows`) into a single list ordered by timestamp. Labeled
    /// entries that are already present keep their position from the
    /// primary list so their highlighted search state is preserved.
    /// When the user has typed a query, labeled entries are filtered to
    /// only those whose command or comment matches the query (plain
    /// text or regex, depending on whether the query starts with `/`).
    /// When the duplicate filter is on, only the newest instance of each
    /// command is kept.
    fn merged_rows(&self) -> Vec<HistoryRow> {
        let mut merged = self.rows.clone();
        let existing_ids: std::collections::HashSet<i64> =
            merged.iter().map(|r| r.id).collect();
        for row in &self.labeled_rows {
            if !existing_ids.contains(&row.id) {
                if !self.query.is_empty() {
                    let in_command = self.query_matches_text(&row.command);
                    let in_comment = self.query_matches_text(&row.comment);
                    if !in_command && !in_comment {
                        continue;
                    }
                }
                merged.push(row.clone());
            }
        }
        // Newest first.
        merged.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        if self.duplicate_filter {
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            merged.retain(|r| seen.insert(r.command.clone()));
        }

        merged
    }

    fn build_where(&self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut clause = String::from(" WHERE 1=1");
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        // When the query is a regex (prefixed with `/`) we skip the
        // SQL `LIKE` clause entirely and post-filter the rows in
        // `refresh()` via `query_matches_text`. Otherwise we issue
        // one `LIKE` clause per whitespace-separated word so the
        // search is AND-by-word.
        if !self.query.is_empty() && !self.is_regex_query() {
            for word in self.query.split_whitespace() {
                let escaped = crate::util::escape_like(word);
                clause.push_str(
                    " AND (h.command LIKE ? ESCAPE '\\' OR c.comment LIKE ? ESCAPE '\\')",
                );
                params.push(Box::new(format!("%{}%", escaped)));
                params.push(Box::new(format!("%{}%", escaped)));
            }
        }
        match self.mode {
            Mode::Sess => {
                if let Ok(s) = std::env::var("SMART_HISTORY_SESSION")
                    && !s.is_empty()
                {
                    clause.push_str(" AND h.session_id = ?");
                    params.push(Box::new(s));
                }
            }
            Mode::Dir => {
                if let Ok(pwd) = std::env::var("PWD")
                    && !pwd.is_empty()
                {
                    clause.push_str(" AND h.directory = ?");
                    params.push(Box::new(pwd));
                }
            }
            Mode::Global => {}
        }
        (clause, params)
    }

    fn cycle_mode(&mut self) {
        self.mode = self.mode.next();
        self.refresh();
    }

    /// Toggle the duplicate filter on or off. When on (default), only
    /// the newest instance of each command is shown. When off, every
    /// history row is shown as-is.
    fn toggle_duplicate_filter(&mut self) {
        self.duplicate_filter = !self.duplicate_filter;
        // Adjust the selection so it stays on a valid index even after
        // the list shrinks (when turning the filter on).
        let target = self.list_state.selected().unwrap_or(0);
        self.refresh();
        let n = self.rows.len();
        if n == 0 {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(target.min(n - 1)));
        }
    }

    /// Move the selection by `delta` rows within the visible list.
/// The visible list is the union of `rows` (current search results)
/// and `labeled_rows` (entries with comments not already in `rows`),
/// filtered by `duplicate_filter` and the user's `query` on labels.
/// We compute the merged list here so that navigation matches the
/// rendered order exactly, and clamp to the merged length.
fn move_selection(&mut self, delta: isize) {
    let merged_len = self.merged_rows().len();
    if merged_len == 0 {
        return;
    }
    let cur = self.list_state.selected().unwrap_or(0) as isize;
    let next = (cur + delta).clamp(0, merged_len as isize - 1) as usize;
    self.list_state.select(Some(next));
}

    fn select_for_run(&mut self) {
        if let Some(i) = self.list_state.selected()
            && let Some(row) = self.rows.get(i)
        {
            self.selection = Some(row.command.clone());
            self.pick_mode = Some(PickMode::Run);
        }
    }

    /// Stage an external editor invocation as the next "selection".
    /// The TUI prints the command on stdout and exits with the Run
    /// exit code, so the parent shell treats it like any other
    /// command line and runs the editor after the TUI has fully
    /// torn down. The TUI does NOT manage the terminal while the
    /// editor runs.
    fn select_for_editor(&mut self, editor_cmd: String) {
        self.selection = Some(editor_cmd);
        self.pick_mode = Some(PickMode::Run);
        self.close_output_view();
    }

    fn select_for_edit_start(&mut self) {
        if let Some(i) = self.list_state.selected()
            && let Some(row) = self.rows.get(i)
        {
            self.selection = Some(row.command.clone());
            self.pick_mode = Some(PickMode::EditStart);
        }
    }

    fn select_for_edit_end(&mut self) {
        if let Some(i) = self.list_state.selected()
            && let Some(row) = self.rows.get(i)
        {
            self.selection = Some(row.command.clone());
            self.pick_mode = Some(PickMode::EditEnd);
        }
    }

    fn push_char(&mut self, c: char) {
        if let Some(ref mut buf) = self.comment_edit {
            buf.push(c);
        } else {
            // If the query was prefilled from the session cache and the
            // user hasn't touched it yet, the first character should
            // replace it rather than append (so the cached query
            // doesn't accidentally end up as a prefix).
            if self.query_prefilled && !self.query_touched {
                self.query.clear();
            }
            self.query_touched = true;
            self.query.push(c);
            self.recompile_regex();
            self.refresh();
        }
    }

    fn backspace(&mut self) {
        if let Some(ref mut buf) = self.comment_edit {
            buf.pop();
        } else {
            // Only flag the query as user-touched once we've actually
            // removed at least one character (so a stray backspace on
            // an empty, prefilled query still leaves the prefilled
            // value alone until the user starts typing).
            if !self.query.is_empty() {
                self.query_touched = true;
                self.query.pop();
                self.recompile_regex();
                self.refresh();
            }
        }
    }

    fn clear_query(&mut self) {
        if let Some(ref mut buf) = self.comment_edit {
            buf.clear();
        } else {
            self.query.clear();
            self.query_touched = true;
            self.query_regex = None;
            self.refresh();
        }
    }

    fn start_comment_edit(&mut self) {
        if let Some(row) = self.selected_row() {
            self.comment_edit = Some(row.comment.clone());
        }
    }

    fn cancel_comment_edit(&mut self) {
        self.comment_edit = None;
    }

    fn save_comment_edit(&mut self) -> Result<()> {
        if let Some(ref comment) = self.comment_edit
            && let Some(row) = self.selected_row()
        {
            self.conn.execute(
                "INSERT INTO command_comments (command, comment) VALUES (?1, ?2) \
                 ON CONFLICT (command) DO UPDATE SET comment = excluded.comment",
                params![row.command, comment],
            )?;
        }
        self.comment_edit = None;
        self.refresh();
        self.refresh_labeled();
        Ok(())
    }

    fn show_output_view(&mut self) {
        if let Some(row) = self.selected_row().filter(|r| !r.output.is_empty()) {
            self.output_view = Some(OutputView {
                text: row.output.clone(),
                scroll: 0,
            });
        }
    }

    fn close_output_view(&mut self) {
        self.output_view = None;
    }

    fn selected_row(&self) -> Option<&HistoryRow> {
        self.list_state
            .selected()
            .and_then(|i| self.rows.get(i))
    }

    fn is_comment_editing(&self) -> bool {
        self.comment_edit.is_some()
    }

    fn is_output_viewing(&self) -> bool {
        self.output_view.is_some()
    }

    fn is_labeled_view(&self) -> bool {
        // The labeled pane is always available, so the toggle state is
        // determined by the dedicated `labeled_list_state` which we
        // keep synchronized with `list_state` for the moment.
        self.labeled_list_state.selected().is_some() || !self.labeled_rows.is_empty()
    }

    /// Re-query the database for all rows that have an associated
    /// comment. This powers the always-available "labeled" pane.
    fn refresh_labeled(&mut self) {
        self.labeled_rows = self.fetch_labeled().unwrap_or_default();
        if self.labeled_rows.is_empty() {
            self.labeled_list_state.select(None);
        } else {
            self.labeled_list_state.select(Some(0));
        }
    }

    fn fetch_labeled(&self) -> Result<Vec<HistoryRow>> {
        let sql = "SELECT h.id, h.command, h.directory, h.session_id, h.exit_code, h.timestamp, c.comment, o.output \
                   FROM history h \
                   JOIN command_comments c ON h.command = c.command \
                   LEFT JOIN history_output o ON h.id = o.history_id \
                   ORDER BY h.timestamp DESC LIMIT 1000";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(HistoryRow {
                    id: row.get(0)?,
                    command: row.get(1)?,
                    directory: row.get(2)?,
                    session_id: row.get(3)?,
                    exit_code: row.get(4)?,
                    timestamp: row.get(5)?,
                    comment: row.get(6).unwrap_or_default(),
                    output: row.get(7).unwrap_or_default(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn delete_selected(&mut self) -> Result<()> {
        if let Some(row) = self.selected_row() {
            self.conn
                .execute("DELETE FROM history WHERE id = ?1", params![row.id])?;
            self.refresh();
            self.refresh_labeled();
        }
        self.confirm_delete = None;
        Ok(())
    }

    fn delete_matching(&mut self) -> Result<()> {
        let (where_clause, params) = self.build_where();
        let sql = format!("DELETE FROM history WHERE id IN (SELECT h.id FROM history h LEFT JOIN command_comments c ON h.command = c.command{})", where_clause);
        let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        self.conn.execute(&sql, &params_ref[..])?;
        self.refresh();
        self.refresh_labeled();
        self.confirm_delete = None;
        Ok(())
    }
}

/// Run the TUI.
///
/// The TUI renders to **stderr** (so it doesn't pollute the parent
/// shell's `$(...)` capture, which reads stdout). The selected command
/// is printed to **stdout** by the caller (`main`).
pub fn run_tui_to_stdout(
    initial_mode: String,
    initial_query: String,
    conn: Connection,
) -> Result<Option<(String, i32)>> {
    let mode = Mode::parse(&initial_mode).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown TUI mode {:?}; expected one of SESS, SESSION, DIR, DIRECTORY, GLOBAL",
            initial_mode
        )
    })?;
    let cfg = Config::load();
    let session = TuiSession::load();
    let duplicate_filter = session
        .duplicate_filter
        .unwrap_or(cfg.duplicate_filter);
    // Install the user-configured TUI palette (or built-in defaults)
    // into a thread-local so the draw helpers can read it without
    // needing it threaded through every signature.
    let palette = Palette::from_config(cfg.theme());
    PALETTE.with(|p| *p.borrow_mut() = palette);
    // The effective initial mode is decided by precedence:
    //   1. The `initial_mode` argument (already resolved by `main`
    //      from --mode / env / config).
    //   2. The persisted session file.
    let effective_mode = session
        .mode
        .as_deref()
        .and_then(Mode::parse)
        .unwrap_or(mode);
    // The query is considered "prefilled" only when it was loaded
    // from the persisted session file, not when the user supplied
    // a fresh `--query` argument or `$SMARTHISTORY_TUI_QUERY`.
    let prefilled_query = session.query.clone();
    let effective_query = prefilled_query.clone().unwrap_or(initial_query);
    let mut app = App::new(
        conn,
        effective_mode,
        effective_query,
        duplicate_filter,
        prefilled_query.is_some(),
    );
    // If the persisted session requested a different duplicate filter
    // than the one we initialized with, honor it.
    if session.duplicate_filter.is_some() && session.duplicate_filter != Some(duplicate_filter) {
        app.duplicate_filter = session.duplicate_filter.unwrap_or(true);
    }

    let mut render = std::io::stderr();
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        render,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::cursor::Hide
    )?;

    let backend = CrosstermBackend::new(render);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(&mut terminal, &mut app);

    let _ = crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = crossterm::terminal::disable_raw_mode();

    result?;
    let selection = if app.cancelled {
        None
    } else if let Some(sel) = app.selection.take() {
        let pm = app.pick_mode.unwrap_or(PickMode::Run).exit_code();
        Some((sel, pm))
    } else {
        None
    };

    // Persist the user's TUI preferences so the next invocation can
    // restore them. The session file lives at
    // ~/.cache/smarthistory/session.
    let session = TuiSession {
        mode: Some(match app.mode {
            Mode::Sess => "SESS".to_string(),
            Mode::Dir => "DIR".to_string(),
            Mode::Global => "GLOBAL".to_string(),
        }),
        query: Some(app.query.clone()),
        duplicate_filter: Some(app.duplicate_filter),
    };
    session.save();

    Ok(selection)
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stderr>>,
    app: &mut App,
) -> Result<()> {
    let page_size = terminal
        .size()
        .map(|s| s.height.max(3) as usize)
        .unwrap_or(20);
    loop {
        if let Err(e) = terminal.draw(|f| ui(f, app)) {
            return Err(anyhow::anyhow!("terminal draw failed: {}", e));
        }

        if !crossterm::event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };

        if app.is_output_viewing() {
            handle_output_view_key(app, key, page_size);
            // If the overlay was closed AND a selection has been
            // staged (e.g. by pressing ^E to open the captured
            // output in an external editor), exit the TUI so the
            // parent shell can run the staged command.
            if !app.is_output_viewing() && app.selection.is_some() {
                return Ok(());
            }
            continue;
        }

        if handle_key(app, key) {
            return Ok(());
        }
    }
}


/// Returns `true` if the app should exit (selection made or cancelled).
/// The captured-output overlay is handled directly in the run loop
/// so that it can launch an external editor.
fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    // When prompting for deletion, only allow 'y' or 'n' or Esc/Ctrl+C.
    if let Some(mode) = app.confirm_delete {
        return handle_confirm_delete_key(app, key, mode);
    }

    // When editing a comment, most keys go to the comment buffer.
    if app.is_comment_editing() {
        return handle_comment_edit_key(app, key);
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => {
                app.cancelled = true;
                return true;
            }
            KeyCode::Char('g') => {
                app.cycle_mode();
                return false;
            }
            KeyCode::Char('s') => {
                app.toggle_duplicate_filter();
                return false;
            }
            KeyCode::Char('e') => {
                app.start_comment_edit();
                return false;
            }
            KeyCode::Char('l') => {
                app.show_output_view();
                return false;
            }
            KeyCode::Char('u') => {
                app.clear_query();
                return false;
            }
            KeyCode::Char('p') => {
                app.move_selection(1);
                return false;
            }
            KeyCode::Char('n') => {
                app.move_selection(-1);
                return false;
            }
            KeyCode::Char('d') => {
                app.confirm_delete = Some(ConfirmMode::DeleteSelected);
                return false;
            }
            KeyCode::Char('x') => {
                app.confirm_delete = Some(ConfirmMode::DeleteMatching);
                return false;
            }
            _ => {
                return false;
            }
        }
    }

    match key.code {
        KeyCode::Esc => {
            app.cancelled = true;
            true
        }
        KeyCode::Enter => {
            app.select_for_run();
            true
        }
        KeyCode::Left => {
            app.select_for_edit_start();
            true
        }
        KeyCode::Right => {
            app.select_for_edit_end();
            true
        }
        KeyCode::Backspace => {
            app.backspace();
            false
        }
        // Rows are ordered newest-first (index 0 = newest). The list
        // is bottom-aligned, so the newest entry sits at the bottom.
        // Up moves visually upward = older = higher index.
        KeyCode::Up => {
            app.move_selection(1);
            app.query_prefilled = false;
            false
        }
        KeyCode::Down => {
            app.move_selection(-1);
            app.query_prefilled = false;
            false
        }
        KeyCode::PageUp => {
            app.move_selection(10);
            app.query_prefilled = false;
            false
        }
        KeyCode::PageDown => {
            app.move_selection(-10);
            app.query_prefilled = false;
            false
        }
        // Home jumps to the oldest entry (last index), End to the
        // newest (index 0, bottom of the list).
        KeyCode::Home => {
            if !app.rows.is_empty() {
                app.list_state.select(Some(app.rows.len() - 1));
            }
            app.query_prefilled = false;
            false
        }
        KeyCode::End => {
            if !app.rows.is_empty() {
                app.list_state.select(Some(0));
            }
            app.query_prefilled = false;
            false
        }
        KeyCode::Char(c) => {
            app.push_char(c);
            false
        }
        _ => false,
    }
}

fn handle_confirm_delete_key(app: &mut App, key: KeyEvent, mode: ConfirmMode) -> bool {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            match mode {
                ConfirmMode::DeleteSelected => {
                    let _ = app.delete_selected();
                }
                ConfirmMode::DeleteMatching => {
                    let _ = app.delete_matching();
                }
            }
            false
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.confirm_delete = None;
            false
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cancelled = true;
            true
        }
        _ => false,
    }
}

/// Key handler used while viewing captured output. Returns `true` only
/// when the user aborts the whole TUI with Ctrl+C.
/// Result of handling a key event in the captured-output overlay.
enum OutputViewResult {
    /// Stay in the overlay and keep the loop running.
    Continue,
    /// Close the overlay and continue the main loop.
    Close,
}

/// Key handler used while viewing captured output. Returns a result
/// describing what the run loop should do next.
fn handle_output_view_key(
    app: &mut App,
    key: KeyEvent,
    page_size: usize,
) -> OutputViewResult {
    // Helper to compute the max valid scroll offset.
    let max_scroll = |text: &str| -> usize {
        let total = text.lines().count();
        total.saturating_sub(page_size.max(1))
    };

    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => OutputViewResult::Close,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cancelled = true;
            app.close_output_view();
            OutputViewResult::Close
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Write the captured output to a temporary file and stage
            // the editor invocation as the next "selection". The TUI
            // will exit normally, printing the editor command on
            // stdout, and the parent shell runs it like any other
            // command. This avoids all TUI terminal-mode juggling.
            if let Some(ref view) = app.output_view {
                let path = std::env::temp_dir().join(format!(
                    "smarthistory-output-{}.txt",
                    generate_tui_pane_id()
                ));
                if std::fs::write(&path, &view.text).is_ok() {
                    let editor = std::env::var("EDITOR")
                        .ok()
                        .filter(|e| !e.is_empty())
                        .unwrap_or_else(|| "vi".to_string());
                    let cmd = format!("{} {}", editor, path.display());
                    app.select_for_editor(cmd);
                    return OutputViewResult::Close;
                }
            }
            OutputViewResult::Continue
        }
        KeyCode::Up => {
            if let Some(ref mut view) = app.output_view {
                view.scroll = view.scroll.saturating_sub(1);
            }
            OutputViewResult::Continue
        }
        KeyCode::Down => {
            if let Some(ref mut view) = app.output_view {
                let max = max_scroll(&view.text);
                view.scroll = (view.scroll + 1).min(max);
            }
            OutputViewResult::Continue
        }
        KeyCode::PageUp => {
            if let Some(ref mut view) = app.output_view {
                view.scroll = view.scroll.saturating_sub(page_size.max(1));
            }
            OutputViewResult::Continue
        }
        KeyCode::PageDown => {
            if let Some(ref mut view) = app.output_view {
                let max = max_scroll(&view.text);
                view.scroll = (view.scroll + page_size.max(1)).min(max);
            }
            OutputViewResult::Continue
        }
        KeyCode::Home => {
            if let Some(ref mut view) = app.output_view {
                view.scroll = 0;
            }
            OutputViewResult::Continue
        }
        KeyCode::End => {
            if let Some(ref mut view) = app.output_view {
                view.scroll = max_scroll(&view.text);
            }
            OutputViewResult::Continue
        }
        _ => OutputViewResult::Continue,
    }
}

/// A short random ID used in temp file names.
fn generate_tui_pane_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
}



/// Key handler used while editing a comment. Returns `true` only when
/// the user aborts the whole TUI with Ctrl+C.
fn handle_comment_edit_key(app: &mut App, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => {
                app.cancelled = true;
                return true;
            }
            KeyCode::Char('u') => {
                app.clear_query();
                return false;
            }
            _ => return false,
        }
    }

    match key.code {
        KeyCode::Esc => {
            app.cancel_comment_edit();
            false
        }
        KeyCode::Enter => {
            let _ = app.save_comment_edit();
            false
        }
        KeyCode::Backspace => {
            app.backspace();
            false
        }
        KeyCode::Char(c) => {
            app.push_char(c);
            false
        }
        _ => false,
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    if let Some(ref view) = app.output_view {
        draw_output_view(f, view);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(1), // mode strip
                Constraint::Fill(1),   // list: take all remaining space
                Constraint::Length(8), // details: fixed 8 lines incl. header/borders
                Constraint::Length(3), // input
                Constraint::Length(1), // status
            ]
            .as_ref(),
        )
        .split(f.area());

    draw_mode_strip(f, app, chunks[0]);
    draw_list(f, app, chunks[1]);

    let detail_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)].as_ref())
        .split(chunks[2]);

    draw_details(f, app, detail_chunks[0]);
    draw_output_preview(f, app, detail_chunks[1]);

    draw_input(f, app, chunks[3]);
    draw_status(f, app, chunks[4]);

    if let Some(mode) = app.confirm_delete {
        draw_confirm_delete(f, app, mode);
    }

    // If a comment exists, draw the labeled entries pane as an overlay
    // so that labeled history elements are always available.
    // (Labeled entries are now merged into the main list instead.)
    #[allow(clippy::overly_complex_conditional)]
    let _ = !app.labeled_rows.is_empty();
}

fn draw_confirm_delete(f: &mut Frame, app: &App, mode: ConfirmMode) {
    let area = centered_rect(60, 25, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let (title, message) = match mode {
        ConfirmMode::DeleteSelected => (
            " Delete selected entry ",
            "Are you sure you want to delete the selected history entry?".to_string(),
        ),
        ConfirmMode::DeleteMatching => (
            " Delete ALL matching entries ",
            format!(
                "Are you sure you want to delete all {} matching entries?",
                app.rows.len()
            ),
        ),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(title)
        .title_style(Theme::error())
        .border_style(Theme::error());

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            message,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("Press "),
            Span::styled("y", Theme::highlight()),
            Span::raw(" to confirm, "),
            Span::styled("n", Theme::highlight()),
            Span::raw(" or "),
            Span::styled("Esc", Theme::highlight()),
            Span::raw(" to cancel."),
        ]),
    ];

    let paragraph = Paragraph::new(text)
        .block(block)
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(popup_layout[1])[1]
}

fn draw_output_view(f: &mut Frame, view: &OutputView) {
    let area = f.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Captured output (\u{2191}\u{2193} scroll, ^E edit, ^L close) ")
        .title_style(Theme::accent())
        .border_style(Theme::dim());

    let all_lines: Vec<&str> = view.text.lines().collect();
    let total = all_lines.len();
    // Inner height excludes the top and bottom borders.
    let inner_h = area.height.saturating_sub(2) as usize;
    let max_scroll = total.saturating_sub(inner_h);
    let scroll = view.scroll.min(max_scroll);

    // Window of visible lines.
    let end = (scroll + inner_h).min(total);
    let start = scroll;
    let visible: Vec<Line> = all_lines[start..end]
        .iter()
        .map(|l| Line::from(Span::raw(l.to_string())))
        .collect();

    let paragraph = Paragraph::new(visible)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);

    // Footer with scroll position (only if there is room inside the
    // border).
    if area.height >= 3 {
        let footer = format!(" {}/{} ", end, total);
        let para = Paragraph::new(Line::from(Span::styled(footer, Theme::dim())));
        let footer_area = Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width,
            height: 1,
        };
        f.render_widget(para, footer_area);
    }
}

fn draw_mode_strip(f: &mut Frame, app: &App, area: Rect) {
    let dup_label = if app.duplicate_filter { "last only" } else { "all entries" };
    let spans = vec![
        Span::styled("smart", Theme::dim()),
        Span::styled("history", Theme::accent()),
        Span::styled("  ", Theme::default()),
        mode_badge(app.mode),
        Span::styled("  ", Theme::default()),
        duplicate_filter_badge(app.duplicate_filter),
        Span::styled(
            format!(
                "  {} · {} ",
                match app.mode {
                    Mode::Sess => "current session only",
                    Mode::Dir => "current directory only",
                    Mode::Global => "all history",
                },
                dup_label,
            ),
            Theme::dim(),
        ),
    ];
    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    f.render_widget(paragraph, area);
}

fn duplicate_filter_badge(on: bool) -> Span<'static> {
    let (label, color) = if on { ("LAST", Theme::success_color()) } else { ("ALL", Theme::accent_color()) };
    Span::styled(
        format!(" {} ", label),
        Style::default().fg(Color::Black).bg(color).add_modifier(Modifier::BOLD),
    )
}

#[allow(dead_code)]
fn exit_filter_badge(filter: ExitFilter) -> Span<'static> {
    let (label, color) = match filter {
        ExitFilter::All => ("ALL", Theme::accent_color()),
        ExitFilter::Success => ("OK", Theme::success_color()),
        ExitFilter::Failed => ("ERR", Theme::error_color()),
    };
    Span::styled(
        format!(" {} ", label),
        Style::default().fg(Color::Black).bg(color).add_modifier(Modifier::BOLD),
    )
}

fn mode_badge(mode: Mode) -> Span<'static> {
    let (label, color) = match mode {
        Mode::Sess => ("SESS", Theme::success_color()),
        Mode::Dir => ("DIR", Theme::warning_color()),
        Mode::Global => ("GLOBAL", Theme::accent_color()),
    };
    Span::styled(
        format!(" {} ", label),
        Style::default().fg(Color::Black).bg(color).add_modifier(Modifier::BOLD),
    )
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let merged = app.merged_rows();
    let age_width = merged
        .iter()
        .map(|r| format_diff(r.timestamp).chars().count())
        .max()
        .unwrap_or(3)
        .max(3);

    // Build the real row items. Rows are stored newest-first; for
    // display we want oldest at the top and newest at the bottom,
    // so reverse the order. Pass `is_selected` based on the data index.
    let real_items: Vec<ListItem> = merged
        .iter()
        .enumerate()
        .rev()
        .map(|(data_idx, r)| {
            let is_selected = app.list_state.selected() == Some(data_idx);
            ListItem::new(render_row(r, app, is_selected, age_width))
        })
        .collect();

    // Bottom-align: when there are fewer real rows than the visible
    // height, pad the top with empty items so the real rows sit at
    // the bottom of the widget. `area.height` includes the top and
    // bottom borders; subtract 2 for the content area.
    let visible_height = area.height.saturating_sub(2) as usize;
    let real_count = real_items.len();
    let pad = visible_height.saturating_sub(real_count);

    let mut items: Vec<ListItem> = (0..pad).map(|_| ListItem::new("")).collect();
    items.extend(real_items);

    // The stored selection is in data coordinates (0 = newest).
    // Map it to the rendered list coordinates where the newest item
    // is the last real item.
    let rendered_idx = app.list_state.selected().map(|data_idx| {
        pad + (real_count.saturating_sub(1) - data_idx)
    });

    // Always start the list from the bottom of the visible window.
    // When the list fits within the visible height we pad with empty
    // items above; when it is taller, we anchor the offset so the
    // last entry sits at the bottom and the user scrolls upward to
    // see older entries.
    let offset = if real_count >= visible_height {
        // Anchor at the bottom: offset = real_count - visible_height.
        // This positions the newest entry at the bottom row and leaves
        // older entries visible above as the user scrolls up.
        real_count.saturating_sub(visible_height)
    } else {
        0
    };

    // Replace the state so we can set the offset explicitly. Preserve
    // the rendered selection for this frame.
    let mut render_state = ListState::default().with_offset(offset);
    render_state.select(rendered_idx);

let title = format!(" History — {} ", merged.len());
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .title(title)
                .title_style(Theme::accent())
                .border_style(Theme::dim()),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(symbols::line::THICK_VERTICAL_RIGHT)
        .repeat_highlight_symbol(true);

    f.render_stateful_widget(list, area, &mut render_state);

    // ratatui may have scrolled the state; read its final offset and
    // selection back into app.list_state in data coordinates.
    let final_selected = render_state.selected();
    let data_idx = final_selected.and_then(|ri| {
        if ri < pad {
            None
        } else {
            let real = ri - pad;
            Some(real_count.saturating_sub(1) - real)
        }
    });

    // Maintain a separate selection index for the "all labeled" view so
    // that switching back and forth between the two panes preserves the
    // cursor position in each.
    if app.is_labeled_view() {
        app.labeled_list_state = ListState::default().with_offset(0);
        app.labeled_list_state.select(data_idx);
    } else {
        app.list_state = ListState::default().with_offset(0);
        app.list_state.select(data_idx);
    }
}



/// Render a single history row as a `Line` with optional query
/// highlighting. The layout is a fixed-width columnar form:
///
///   [age] [status]  command  ·  time
///
/// `age_width` is the right-aligned width of the age column so rows
/// line up.
fn render_row<'a>(row: &'a HistoryRow, app: &App, is_selected: bool, age_width: usize) -> Line<'a> {
    let age = format_diff(row.timestamp);
    let age_padded = format!("{:>age_width$}", age);

    let exit_marker = if row.exit_code == 0 { "✓" } else { "✗" };
    let exit_style = if row.exit_code == 0 {
        Theme::success()
    } else {
        Theme::error()
    };

    // Capture indicator. A bright `o ` shows the row has captured
    // output available (press ^L to view); a dim `. ` is shown
    // otherwise so columns stay aligned.
    let capture_span = if !row.output.is_empty() {
        Span::styled(
            " o ",
            Style::default()
                .fg(Theme::highlight_color())
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" . ", Theme::dim())
    };

    let mut spans = vec![
        capture_span,
        Span::styled(format!(" {} ", age_padded), Theme::accent()),
        Span::raw(" "),
        Span::styled(format!(" {} ", exit_marker), exit_style),
        Span::raw(" "),
    ];

    // Highlight query matches inside the command. When the query is
    // a regex (prefixed with `/`) we use the compiled regex to find
    // all matches and bold each one. Otherwise the standard plain-
    // text multi-word highlight runs.
    if app.is_regex_query() {
        spans.extend(highlight_regex_matches(
            &row.command,
            app.query_regex.as_ref(),
        ));
    } else {
        spans.extend(highlight_matches(&row.command, &app.query));
    }

    spans.push(Span::styled(
        format!("  · {} ", format_time(row.timestamp)),
        Theme::dim(),
    ));

    // Show a non-empty comment inline for every row, and fall back to
    // the directory on the selected row when there is no comment.
    if !row.comment.is_empty() {
        spans.push(Span::styled(
            format!("# {} ", row.comment),
            Style::default()
                .fg(Theme::warning_color())
                .add_modifier(Modifier::ITALIC),
        ));
    } else if is_selected {
        spans.push(Span::styled(
            format!("· {} ", row.directory),
            Theme::dim(),
        ));
    }

    Line::from(spans)
}

/// Return a sequence of spans that wrap every occurrence of `query`
/// in `text` with a highlight style. Matching is case-insensitive and
/// based on Unicode scalar values. Adjacent non-matching characters
/// are coalesced into a single span.
fn highlight_regex_matches<'a>(text: &'a str, regex: Option<&Regex>) -> Vec<Span<'a>> {
    let Some(re) = regex else {
        return vec![Span::raw(text)];
    };
    let text_chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut last_end = 0usize;
    for m in re.find_iter(text) {
        // `m.start()`/`m.end()` are byte offsets; convert to char
        // indices so we slice `text_chars` (a `Vec<char>`).
        let start_char = text[..m.start()].chars().count();
        let end_char = start_char + m.as_str().chars().count();
        if start_char > last_end {
            let prefix: String = text_chars[last_end..start_char].iter().collect();
            spans.push(Span::raw(prefix));
        }
        let matched: String = text_chars[start_char..end_char].iter().collect();
        spans.push(Span::styled(
            matched,
            Style::default()
                .fg(Theme::highlight_color())
                .add_modifier(Modifier::BOLD),
        ));
        last_end = end_char;
    }
    if last_end < text_chars.len() {
        let tail: String = text_chars[last_end..].iter().collect();
        spans.push(Span::raw(tail));
    }
    if spans.is_empty() {
        spans.push(Span::raw(text));
    }
    spans
}

/// Return a sequence of spans that wrap every occurrence of `query`
fn highlight_matches<'a>(text: &'a str, query: &str) -> Vec<Span<'a>> {
    if query.is_empty() {
        return vec![Span::raw(text)];
    }

    let words: Vec<String> = query
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    if words.is_empty() {
        return vec![Span::raw(text)];
    }

    let lower_text = text.to_lowercase();
    let text_chars: Vec<char> = text.chars().collect();
    let mut highlights = vec![false; text_chars.len()];

    for word in words {
        let word_chars: Vec<char> = word.chars().collect();
        if word_chars.is_empty() {
            continue;
        }
        let mut i = 0;
        while i + word_chars.len() <= text_chars.len() {
            if lower_text.chars().skip(i).take(word_chars.len()).collect::<Vec<char>>() == word_chars
            {
                for j in 0..word_chars.len() {
                    highlights[i + j] = true;
                }
                i += word_chars.len();
            } else {
                i += 1;
            }
        }
    }

    let mut spans = Vec::new();
    let mut i = 0;
    while i < text_chars.len() {
        let start = i;
        let is_highlight = highlights[i];
        while i < text_chars.len() && highlights[i] == is_highlight {
            i += 1;
        }
        let segment: String = text_chars[start..i].iter().collect();
        if is_highlight {
            spans.push(Span::styled(
                segment,
                Style::default()
                    .fg(Theme::highlight_color())
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw(segment));
        }
    }

    spans
}

fn draw_details(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Details ")
        .title_style(Theme::accent())
        .border_style(Theme::dim());

    let Some(row) = app.selected_row() else {
        let empty = Paragraph::new(Line::from(vec![Span::styled(
            "No command selected",
            Theme::dim(),
        )]))
        .block(block);
        f.render_widget(empty, area);
        return;
    };

    let exit_marker = if row.exit_code == 0 { "✓" } else { "✗" };
    let exit_text = if row.exit_code == 0 {
        "success".to_string()
    } else {
        format!("exit {}", row.exit_code)
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Cmd  ", Theme::dim()),
            Span::styled(
                row.command.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Dir  ", Theme::dim()),
            Span::raw(row.directory.clone()),
        ]),
        Line::from(vec![
            Span::styled("Sess ", Theme::dim()),
            Span::raw(row.session_id.clone()),
        ]),
        Line::from(vec![
            Span::styled("Time ", Theme::dim()),
            Span::raw(format!(
                "{} · {}",
                format_time(row.timestamp),
                format_diff(row.timestamp),
            )),
        ]),
        Line::from(vec![
            Span::styled("Stat ", Theme::dim()),
            Span::styled(format!("{} {}", exit_marker, exit_text), Theme::success()),
        ]),
    ];

    // Add the comment line only when one exists.
    if !row.comment.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Rem  ", Theme::dim()),
            Span::styled(
                row.comment.clone(),
                Style::default()
                    .fg(Theme::warning_color())
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
    }

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

fn draw_output_preview(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Output Preview ")
        .title_style(Theme::accent())
        .border_style(Theme::dim());

    let Some(row) = app.selected_row() else {
        f.render_widget(Paragraph::new("").block(block), area);
        return;
    };

    if row.output.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("No output captured", Theme::dim())).block(block),
            area,
        );
        return;
    }

    let preview_lines: Vec<Line> = row
        .output
        .lines()
        .take(4) // Show up to 4 lines to fit the new larger detail pane
        .map(|l| Line::from(Span::raw(l.to_string())))
        .collect();

    let paragraph = Paragraph::new(preview_lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let is_regex = app.is_regex_query();
    let (prompt, title, content) = match app.comment_edit {
        Some(ref buf) => {
            ("comment> ", " comment ", buf.as_str())
        }
        None => {
            if is_regex {
                ("// ", " regex ", app.query.as_str())
            } else {
                ("> ", " search ", app.query.as_str())
            }
        }
    };

    let input = Paragraph::new(Line::from(vec![
        Span::styled(prompt, Theme::accent()),
        Span::raw(content),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(title)
            .title_style(if is_regex {
                Style::default().fg(Theme::warning_color())
            } else {
                Theme::accent()
            })
            .border_style(if app.comment_edit.is_some() {
                Style::default().fg(Theme::warning_color())
            } else if is_regex {
                Style::default().fg(Theme::warning_color())
            } else {
                Theme::dim()
            }),
    )
    .wrap(Wrap { trim: false });
    f.render_widget(input, area);

    // Place the cursor at the end of the active buffer.
    // The visible text starts at area.x + 1 (one cell for the left
    // border). The prompt string includes its own trailing space.
    let prompt_width = prompt.chars().count() as u16;
    let cursor_x = area.x + 1 + prompt_width + content.chars().count() as u16;
    let cursor_y = area.y + 1;
    f.set_cursor_position((
        cursor_x.min(area.x.saturating_add(area.width).saturating_sub(2)),
        cursor_y,
    ));
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let n = app.rows.len();
    let count = match n {
        0 => "0 matches".to_string(),
        1 => "1 match".to_string(),
        x => format!("{} matches", x),
    };

    let help = match app.selected_row() {
        Some(row) if !row.output.is_empty() => "Enter run · ←→ edit · ↑↓ nav · ^G scope · ^S dedup · ^E comment · ^L output · ^D del · ^X del matching · ^U clear · Esc cancel",
        Some(_) => "Enter run · ←→ edit · ↑↓ nav · ^G scope · ^S dedup · ^E comment · ^D del · ^X del matching · ^U clear · Esc cancel",
        None => "Type to search · ^G scope · ^S dedup · ^E comment · ^U clear · Esc cancel",
    };

    let line = Line::from(vec![
        Span::styled(format!(" {}  ", count), Theme::highlight()),
        Span::styled(help, Theme::dim()),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_matches_empty_query() {
        let spans = highlight_matches("hello world", "");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world".to_string());
    }

    #[test]
    fn highlight_matches_single() {
        let spans = highlight_matches("git status", "stat");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["git ", "stat", "us"]);
    }

    #[test]
    fn highlight_matches_case_insensitive() {
        let spans = highlight_matches("Git Status", "stat");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["Git ", "Stat", "us"]);
    }

    #[test]
    fn highlight_matches_multiple() {
        let spans = highlight_matches("foo bar foo", "foo");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["foo", " bar ", "foo"]);
    }

    #[test]
    fn highlight_matches_no_match() {
        let spans = highlight_matches("hello world", "xyz");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world".to_string());
    }

    #[test]
    fn highlight_matches_multi_word() {
        let spans = highlight_matches("git commit -m", "git commit");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["git", " ", "commit", " -m"]);
    }

    #[test]
    fn highlight_matches_multi_word_out_of_order() {
        let spans = highlight_matches("git commit -m", "commit git");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["git", " ", "commit", " -m"]);
    }
}
