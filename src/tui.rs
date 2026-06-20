use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::CrosstermBackend, widgets::ListState, Terminal};
use rusqlite::{params, Connection};
use std::time::Duration;

pub mod bindings;
pub mod state;
pub mod theme;
pub mod render;

use crate::util::{format_diff, format_time};
use crate::Config;
use regex::Regex;
use std::path::PathBuf;

pub use bindings::{action_for_key, format_key_spec, format_key_specs, Action, KeyBindings, ALL_ACTIONS};
pub use state::{ExitFilter, Mode, HistoryRow, PickMode, exit_code};
pub use theme::{install_palette, SelectedTheme, BuiltinTheme, ThemePicker};

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
    /// Last exit-code filter, persisted as the lowercase
    /// `ExitFilter::as_str()` ("all", "ok", "err"). `None` means
    /// "no preference" and falls back to `ExitFilter::default()`
    /// (i.e. `All`, no filter).
    exit_filter: Option<String>,
    /// Last selected theme slug (e.g. `"dracula"`, `"tokyo-night"`,
    /// or `"none"` for the manual-config palette). Persisted across
    /// TUI invocations so the user always lands back on their
    /// preferred colors.
    theme: Option<String>,
}

/// All theme choices available in the TUI. The first entry, `None`,
/// represents the "no theme" mode where the manually-configured
/// `tuicolor.*` settings from `~/.config/smarthistory/config` are
/// used. Every other entry corresponds to a built-in theme —
/// see `BuiltinTheme` for the full list (upstream `ratatui-themes`
/// plus a small set of hand-curated themes shipped with this
/// crate).
// (The enum itself lives in `theme::SelectedTheme`.)

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
                // Accept the lowercase canonical form ("all"/"ok"/"err")
                // and any alias `ExitFilter::parse` recognises.
                // Garbled values are silently dropped so a hand-edited
                // session file can't wedge the TUI on startup.
                "exitfilter" => {
                    if ExitFilter::parse(value).is_some() {
                        s.exit_filter = Some(value.to_string());
                    }
                }
                "theme" => s.theme = Some(value.to_string()),
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
        if let Some(ref f) = self.exit_filter {
            out.push_str(&format!("exitfilter={}\n", f));
        }
        if let Some(ref t) = self.theme {
            out.push_str(&format!("theme={}\n", t));
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

/// Decide what text `yank_to_clipboard` should copy.
///
/// Priority:
/// 1. The captured-output overlay, if open. This is "the output
///    of this command" — the user is looking at the output and
///    yanking it is the natural action.
/// 2. The command of the currently-selected history row. This
///    is "the current document" — the command line itself,
///    in the same sense as a text editor's "current buffer".
/// 3. `None` — nothing to yank.
///
/// Kept as a free function (not a method) so the decision logic
/// is testable without standing up a full `App` and a SQLite
/// database. The caller in `App::yank_to_clipboard` just passes
/// `&self`.
fn pick_text_to_yank(app: &App) -> Option<String> {
    if let Some(view) = &app.output_view
        && !view.text.is_empty()
    {
        return Some(view.text.clone());
    }
    app.selected_row().map(|r| r.command.clone())
}

/// Truncate a string for use in a status-bar message, with a
/// trailing ellipsis when it doesn't fit. The status bar is one
/// line tall and a long shell command can be 200+ characters;
/// the user already has the full text in their history list,
/// so the status only needs a useful hint.
fn truncate_for_status(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// Tokenize a command line into shell-quote-aware tokens.
///
/// Returns one entry per whitespace-separated word, with
/// surrounding ASCII single or double quotes stripped (so
/// `"my file.txt"` becomes the single token `my file.txt`).
/// Backslash escapes are honoured inside double quotes per the
/// POSIX rules; backslash is literal inside single quotes.
///
/// This is a deliberately small tokenizer. It handles the
/// shapes we care about for filename detection (a path passed
/// as a bare token, a path passed as a single quoted argument)
/// and ignores everything else. It does not parse redirections
/// or subshells.
fn tokenize_command(cmd: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut had_content = false;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                had_content = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                had_content = true;
            }
            '\\' if !in_single => {
                // Inside double quotes only certain chars are
                // escapable per POSIX; outside quotes the
                // backslash is also literal. We just take the
                // next character verbatim either way.
                if let Some(&next) = chars.peek() {
                    current.push(next);
                    chars.next();
                    had_content = true;
                }
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if had_content {
                    tokens.push(std::mem::take(&mut current));
                    had_content = false;
                }
            }
            _ => {
                current.push(c);
                had_content = true;
            }
        }
    }
    if had_content {
        tokens.push(current);
    }
    tokens
}

/// True when `token` contains a shell metacharacter that
/// disqualifies it from being treated as a literal filename.
/// The list is intentionally conservative: any token that
/// contains one of these is a glob, a redirect, a subshell, a
/// variable reference, or similar — not a literal file path.
fn has_shell_metachar(token: &str) -> bool {
    const META: &[char] = &[
        '*', '?', '[', ']', '{', '}', ';', '|', '&', '<', '>',
        '(', ')', '`', '$', '=', '\'', '"', '\\', '!', '#',
    ];
    token.chars().any(|c| META.contains(&c))
}

/// Score a token for "how path-like" it is. Higher scores are
/// better. The score is composed of bonuses for path-shaped
/// features (leading slash, tilde, directory separator) and
/// penalties for obviously non-file shapes (flags, lone `.` or
/// `..`).
///
/// Returns a negative score for tokens that should never be
/// picked, so the caller can use `> 0` as a sanity filter.
fn score_filename_token(token: &str) -> i32 {
    if token.is_empty() {
        return -100;
    }
    if token == "." || token == ".." {
        // The literal current/parent directory entries are not
        // files to edit. `cd ..` and `cd .` would otherwise pick
        // these, which is nonsense.
        return -10;
    }
    if token.starts_with('-') {
        // Looks like a flag (`-rf`, `--all`, etc.). Even
        // `--file=foo` is more usefully interpreted as a flag
        // than a path.
        return -5;
    }
    let mut score = 0;
    if token.starts_with('/')
        || token.starts_with("~/")
        || token == "~"
        || token.starts_with("./")
        || token == "."
        || token.starts_with("../")
        || token == ".."
    {
        // Absolute, home-relative, or current/parent relative
        // paths are the most reliable indicators that this
        // token is a file the user wants to edit.
        score += 10;
    }
    if token.contains('/') {
        // Anything with a directory separator in it is
        // `dir/...` shaped and almost certainly a path.
        score += 5;
    }
    if let Some(slash) = token.rfind('/') {
        let tail = &token[slash + 1..];
        if tail.contains('.') {
            // `foo.txt`, `.bashrc`, `Makefile.in` all qualify.
            // The dot in the directory part (`/home/user/.config`)
            // doesn't count, which is what we want.
            score += 3;
        }
    } else if token.contains('.') {
        // No directory separator but a dot — probably a
        // filename like `README.md` invoked from cwd. Worth
        // a small bonus.
        score += 2;
    }
    score
}

/// Pick the most filename-shaped token in `cmd`.
///
/// See `Action::EditFileReference` for the rationale. Returns
/// `None` when no row, no command, or no path-like token. Kept
/// as a free function so it can be unit-tested in isolation.
fn find_filename_in_command(cmd: &str) -> Option<String> {
    let mut best: Option<(i32, String)> = None;
    for token in tokenize_command(cmd) {
        if has_shell_metachar(&token) {
            continue;
        }
        let score = score_filename_token(&token);
        if score <= 0 {
            continue;
        }
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, token));
        }
    }
    best.map(|(_, t)| t)
}

/// Copy `text` to the system clipboard via `arboard`.
///
/// `arboard::Clipboard::new()` opens a connection to the platform
/// clipboard daemon (X11, Wayland, macOS pasteboard, Windows
/// clipboard, …). On headless systems or when no clipboard
/// daemon is running, the call returns an error. We surface that
/// to the user as a status-bar message rather than a panic, so a
/// broken clipboard never crashes the TUI.
///
/// The clipboard handle is created fresh on every yank rather
/// than being held for the TUI's lifetime. arboard's connection
/// model is "open → write → close" and some platforms (notably
/// X11) require the connection to be released promptly so other
/// applications can read the clipboard contents. Re-opening per
/// yank is also cheap (a single syscall on every platform).
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new()
        .map_err(|e| format!("clipboard unavailable: {}", e))?;
    cb.set_text(text.to_string())
        .map_err(|e| format!("write failed: {}", e))?;
    Ok(())
}

/// A high-level action that the TUI can take in response to a key
/// press. Action names appear in the user-facing config file as
/// `key.<action>=<key-spec>`, e.g. `key.help=C-h`.
// (The enum itself lives in `bindings::Action`.)

struct App {
    conn: Connection,
    mode: Mode,
    duplicate_filter: bool,
    /// Active exit-code filter. Defaults to `ExitFilter::All`.
    /// Cycled with `Action::CycleExitFilter` (default key `Ctrl-J`).
    exit_filter: ExitFilter,
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
    /// When `Some`, the help overlay is open. The contained `scroll`
    /// tracks how far down the user has scrolled.
    help_view: Option<HelpView>,
    /// When `Some`, the command-palette overlay is open.
    command_menu: Option<CommandMenu>,
    /// When `Some`, the theme-picker overlay is open. Navigating
    /// the list applies the selected theme live; `Enter` commits,
    /// `Esc` reverts to the original.
    theme_picker: Option<ThemePicker>,
    /// When `Some`, we are prompting for deletion confirmation.
    confirm_delete: Option<ConfirmMode>,
    /// Cached set of all history rows that have a comment, used to
    /// populate the optional labeled entries pane.
    labeled_rows: Vec<HistoryRow>,
    /// List state for the labeled entries pane (separate from
    /// `list_state` so the two views can remember their own selection).
    labeled_list_state: ListState,
    /// Cached merged view (`rows` + filtered `labeled_rows`,
    /// sorted by timestamp). The cursor in `list_state` is an
    /// index into this list. Caching avoids rebuilding the merged
    /// list on every render and on every action that needs to
    /// look up the selected row.
    ///
    /// This is the source of truth for `selected_row()`. A row
    /// that's in `labeled_rows` but excluded from `self.rows`
    /// (e.g. by a session/directory filter) lives in this list
    /// but not in `self.rows`; the old `selected_row()` read
    /// from `self.rows` alone and silently returned `None` for
    /// such rows, which is the bug the cache fixes.
    merged_rows: Vec<HistoryRow>,
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
    /// The currently-selected TUI palette. Defaults to
    /// `SelectedTheme::None`, which means the manually-configured
    /// colors from `tuicolor.*` are used.
    theme: SelectedTheme,
    /// Active key bindings, resolved from the user's config file.
    /// Defaults match the original hard-coded Ctrl-* shortcuts.
    bindings: KeyBindings,
    /// Transient message rendered in the status bar (e.g.
    /// "Yanked 23 chars" or "Yank failed: …"). `Some` while the
    /// message is fresh; cleared after a short delay by
    /// `tick_status_message()`.
    status_message: Option<(String, std::time::Instant)>,
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
    ///
    /// The query is the post-slash text. Implicit `.*` anchors are
    /// added at both ends unless the user provided an explicit
    /// anchor (`^` at the start, `$` at the end), so a query like
    /// `git commit` behaves as `.*git commit.*` instead of needing
    /// to match from the very first character.
    fn recompile_regex(&mut self) {
        if !self.is_regex_query() {
            self.query_regex = None;
            return;
        }
        let pattern = build_implicit_regex(self.regex_pattern());
        match Regex::new(&pattern) {
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

    /// Cycle to the next theme. `Ctrl-N` calls this; `Ctrl-P` calls
    /// `cycle_theme_prev`. Updates the global palette immediately so
    /// the change is visible on the next frame, and triggers a full
    /// repaint by marking the terminal as needing a redraw.
    fn cycle_theme_next(&mut self) {
        self.theme = self.theme.next();
        install_palette(self.theme);
    }

    /// Cycle to the previous theme.
    fn cycle_theme_prev(&mut self) {
        self.theme = self.theme.prev();
        install_palette(self.theme);
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

/// Wrap `pattern` with implicit `.*` anchors unless the user
/// already provided an explicit anchor (`^` at the start, `$` at
/// the end). This means `/git commit/` matches any command that
/// contains `git commit` (i.e. behaves as `/.*git commit.*/`),
/// while `/^git commit/` still only matches commands that start
/// with `git commit`, and `/git commit$/` only matches commands
/// that end with `git commit`.
fn build_implicit_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 4);
    if !pattern.starts_with('^') {
        out.push_str(".*");
    }
    out.push_str(pattern);
    if !pattern.ends_with('$') {
        out.push_str(".*");
    }
    out
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

/// State for the help overlay. Just a scroll offset; the help text
/// is computed from the live app state on each render so the
/// "current settings" section is always accurate.
struct HelpView {
    scroll: usize,
}

/// State for the command-palette overlay. The user types into
/// `query` to filter the action list; `selected` is the index of
/// the currently-highlighted action (relative to the filtered
/// list). Pressing Enter runs the highlighted action; Esc closes
/// the overlay; arrows navigate.
struct CommandMenu {
    query: String,
    selected: usize,
    /// The full ordered list of actions shown in the palette. We
    /// snapshot this when the menu opens so the displayed order
    /// stays stable while the user types.
    actions: Vec<Action>,
    /// Whether the user has typed anything. Once true, the first
    /// character no longer replaces a cached query — same
    /// behavior as the main search box.
    touched: bool,
}

impl CommandMenu {
    fn new() -> Self {
        CommandMenu {
            query: String::new(),
            selected: 0,
            actions: ALL_ACTIONS.to_vec(),
            touched: false,
        }
    }

    /// Return the indices (into `self.actions`) of the actions that
    /// match `query`. Matching is case-insensitive substring AND
    /// across words, against either the action's display name or
    /// its `config_key`. Empty query returns every action.
    fn filtered_indices(&self) -> Vec<usize> {
        if self.query.is_empty() {
            return (0..self.actions.len()).collect();
        }
        let q = self.query.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        self.actions
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                let name = a.display_name().to_lowercase();
                let key = a.config_key().to_lowercase();
                words
                    .iter()
                    .all(|w| name.contains(w) || key.contains(w))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Clamp `self.selected` so it remains a valid index into the
    /// filtered list (which may shrink as the user types).
    fn clamp_selection(&mut self) {
        let n = self.filtered_indices().len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }
}

impl App {
    fn new(conn: Connection, initial_mode: Mode, initial_query: String, duplicate_filter: bool, exit_filter: ExitFilter, query_prefilled: bool, theme: SelectedTheme, bindings: KeyBindings) -> Self {
        let list_state = ListState::default();
        let mut app = App {
            conn,
            mode: initial_mode,
            duplicate_filter,
            exit_filter,
            query: initial_query,
            rows: Vec::new(),
            list_state,
            selection: None,
            pick_mode: None,
            cancelled: false,
            comment_edit: None,
            output_view: None,
            help_view: None,
            command_menu: None,
            theme_picker: None,
            confirm_delete: None,
            labeled_rows: Vec::new(),
            labeled_list_state: ListState::default(),
            // Refreshed by `refresh()`; initialized empty so a
            // `selected_row()` call before the first refresh
            // returns `None` cleanly.
            merged_rows: Vec::new(),
            query_prefilled,
            query_touched: false,
            query_regex: None,
            theme,
            bindings,
            status_message: None,
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
        // Rebuild the merged list once per refresh so subsequent
        // `selected_row()` lookups are O(1). The previous design
        // re-allocated this on every action dispatch (and three
        // times per render frame); caching is a measurable win
        // for long lists and also gives us a stable borrow for
        // `selected_row()`.
        self.merged_rows = self.build_merged_rows();
        let n = self.merged_rows.len();
        if n == 0 {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    /// Compute the merged view: primary list + labeled rows
    /// (filtered by the current query, deduped by id, sorted by
    /// timestamp). Extracted from `merged_rows()` so we can
    /// compute it once per `refresh()` and cache the result.
    fn build_merged_rows(&self) -> Vec<HistoryRow> {
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
        // Only re-sort when the primary list is in timestamp-DESC
        // order. Stats mode uses a frequency-aware ordering from
        // `fetch_stats` that we must preserve.
        if !matches!(self.mode, Mode::Stats) {
            merged.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        }
        if self.duplicate_filter {
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            merged.retain(|r| seen.insert(r.command.clone()));
        }
        merged
    }

    fn fetch(&self) -> Result<Vec<HistoryRow>> {
        if matches!(self.mode, Mode::Stats) {
            return self.fetch_stats();
        }
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

    /// Fetch rows ordered by:
    ///   1. probability of following the most-recently-executed
    ///      command (computed via SQLite's `LEAD()` window
    ///      function on the entire global history, ignoring
    ///      session/directory filters),
    ///   2. timestamp DESC (newest first).
    ///
    /// The user's query (when non-empty and not a regex) is honored
    /// as a `LIKE` filter so the user can narrow down what's
    /// ranked. The "last command" itself is the newest row in the
    /// global history that matches the query — the view is
    /// reproducible regardless of which session we're in.
    ///
    /// Tie-breaking within a probability bucket: more recent wins.
    /// Tie-breaking across duplicate commands when the duplicate
    /// filter is on: the most recent instance only.
    fn fetch_stats(&self) -> Result<Vec<HistoryRow>> {
        // 1) Determine the "last command" from the global history
        //    (still respecting the user's query so the prediction
        //    makes sense in context).
        let last_cmd: Option<String> = {
            let (where_clause, params) = self.build_where();
            let sql = format!(
                "SELECT h.command FROM history h{} \
                 ORDER BY h.timestamp DESC, h.id DESC LIMIT 1",
                where_clause
            );
            let params_ref: Vec<&dyn rusqlite::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query_map(&params_ref[..], |row| {
                row.get::<_, String>(0)
            })?;
            rows.next().transpose()?
        };
        let Some(last_cmd) = last_cmd else {
            // No matching history at all.
            return Ok(Vec::new());
        };

        // 2) Pull the rows the user is going to see, ranked by:
        //    (a) frequency as a successor of `last_cmd` DESC,
        //    (b) timestamp DESC.
        //    The user's typed query is honored (where possible).
        let (where_clause, params) = self.build_where();
        // The freq CTE compares against `last_cmd`. SQLite parameter
        // binding works inside CTEs, but we splice the value
        // directly here because it's an internal-only slug (not
        // user input) and escaping via `replace('\'')` keeps the
        // query plan simple. Single quotes are doubled to escape.
        let last_sql = last_cmd.replace('\'', "''");
        // We compute frequency in a single SQL query using a CTE so
        // the entire ranking is one round trip. Predicted commands
        // get a `freq` > 0; commands that never followed `last_cmd`
        // get `freq = 0` and are sorted by timestamp DESC.
        // `build_where` already starts with " WHERE 1=1", so we
        // splice the user's filter in directly.
        let sql = format!(
            "WITH pairs AS ( \
                 SELECT h.command AS cmd, \
                        LEAD(h.command) OVER (ORDER BY h.timestamp ASC, h.id ASC) AS next_cmd \
                 FROM history h \
             ), \
             freq AS ( \
                 SELECT next_cmd AS cmd, COUNT(*) AS freq \
                 FROM pairs \
                 WHERE cmd = '{last_sql}' AND next_cmd IS NOT NULL \
                 GROUP BY next_cmd \
             ) \
             SELECT h.id, h.command, h.directory, h.session_id, \
                    h.exit_code, h.timestamp, c.comment, o.output, \
                    COALESCE(f.freq, 0) AS freq \
             FROM history h \
             LEFT JOIN command_comments c ON h.command = c.command \
             LEFT JOIN history_output o ON h.id = o.history_id \
             LEFT JOIN freq f ON h.command = f.cmd \
             {where_clause} \
             ORDER BY freq DESC, h.timestamp DESC \
             LIMIT 1000",
        );
        // The user's typed query is the only bound parameter (if any).
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
    ///
    /// **Stats mode is special**: the primary list arrives already
    /// sorted by (successor-frequency DESC, timestamp DESC). We
    /// preserve that ordering instead of re-sorting, so the
    /// ranking the user sees in the SQL query survives into the
    /// rendered list.
    /// The merged view of the history list: `self.rows` plus
    /// labeled rows (deduped by id, filtered by the current
    /// query, sorted by timestamp).
    ///
    /// Returns a slice into the cache; the cache is rebuilt by
    /// `refresh()`. Callers that need an owned list (rare) can
    /// clone via `.to_vec()`.
    fn merged_rows(&self) -> &[HistoryRow] {
        &self.merged_rows
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
            // Stats mode always uses the global history regardless of
            // session or directory; the only filter is the user's
            // typed query (handled above).
            Mode::Stats => {}
        }
        // Exit-code filter. Applies to every mode — including
        // Stats — so the user can flip between "all history",
        // "only green commands", and "only red commands" without
        // having to leave the Stats view. `All` is a no-op (no
        // clause added) so the SQL plan stays as simple as
        // possible for the common case.
        match self.exit_filter {
            ExitFilter::All => {}
            ExitFilter::Success => clause.push_str(" AND h.exit_code = 0"),
            ExitFilter::Failed => clause.push_str(" AND h.exit_code != 0"),
        }
        (clause, params)
    }

    fn cycle_mode(&mut self) {
        self.mode = self.mode.next();
        self.refresh();
    }

    /// Cycle the exit-code filter (All → Success → Failed → All).
    /// The current filter is also reflected in the badge rendered
    /// in the mode strip, so the user can see at a glance whether
    /// they're looking at the full history, only the green
    /// commands, or only the red ones.
    ///
    /// Persistence: the new value is written to the session file
    /// in `~.cache/smarthistory/session` when the TUI exits, and
    /// restored on the next launch.
    fn cycle_exit_filter(&mut self) {
        self.exit_filter = self.exit_filter.next();
        // `refresh` resets `list_state` to a valid index (or
        // `None` when the new filter is empty), so we don't need
        // to clamp the selection ourselves.
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
        if let Some(row) = self.selected_row() {
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

    /// Copy something useful to the system clipboard. The pick
    /// order mirrors the user's mental model: if the captured-
    /// output overlay is open, copy its text; otherwise copy the
    /// command of the currently-selected history row. When
    /// nothing is selected, the action is a no-op and a status
    /// message tells the user why.
    ///
    /// Status messages:
    /// - `"Yanked N chars"`  on success
    /// - `"Yank failed: …"`   when arboard cannot reach a clipboard
    /// - `"Nothing to yank"`  when there's no row and no output view
    fn yank_to_clipboard(&mut self) {
        let Some(text) = pick_text_to_yank(self) else {
            self.set_status_message("Nothing to yank".to_string());
            return;
        };
        match copy_to_clipboard(&text) {
            Ok(()) => self.set_status_message(format!(
                "Yanked {} chars to clipboard",
                text.chars().count()
            )),
            Err(e) => self.set_status_message(format!("Yank failed: {}", e)),
        }
    }

    /// Find a filename referenced in the selected history row
    /// and stage `$EDITOR <filename>` as the next selection. The
    /// TUI exits so the parent shell runs the command, which
    /// launches the editor on the file.
    ///
    /// Failure modes (all surfaced as status messages; the TUI
    /// never panics and never silently does nothing):
    /// - No row is selected.
    /// - The row's command has no path-like token.
    /// - The staged command is otherwise empty (defensive).
    ///
    /// On success, `selection` and `pick_mode` are set so the
    /// caller (the dispatcher) returns `true` to terminate the
    /// TUI. The parent shell sees the editor command on stdout
    /// and runs it after the TUI has torn down.
    fn edit_referenced_file(&mut self) {
        let Some(row) = self.selected_row() else {
            self.set_status_message("No command selected".to_string());
            return;
        };
        let Some(path) = find_filename_in_command(&row.command) else {
            self.set_status_message(format!(
                "No filename found in: {}",
                truncate_for_status(&row.command, 40)
            ));
            return;
        };
        // `vi` is POSIX-mandated, so it exists on every
        // supported platform even when the user hasn't set
        // `$EDITOR`. Failing the action with a status message
        // would be a regression vs. the current behaviour where
        // most users get a working editor out of the box.
        let editor = std::env::var("EDITOR")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "vi".to_string());
        // No shell quoting: the parent shell tokenises the
        // staged command on whitespace, so a path with spaces
        // would mis-split. In practice `find_filename_in_command`
        // already strips shell metacharacters when tokenising,
        // and the vast majority of real paths contain no
        // whitespace. Users with spaces-in-paths can still
        // work around it by typing their own command — this
        // action is the convenient 99% case.
        let staged = format!("{} {}", editor, path);
        if staged.trim().is_empty() {
            // Defensive: the inputs are all non-empty so this
            // is unreachable in practice, but we'd rather show
            // a message than stage an empty command and let the
            // shell do something unexpected.
            self.set_status_message("Refusing to stage empty editor command".to_string());
            return;
        }
        self.select_for_editor(staged);
    }

    /// Set the transient status message. The status bar shows
    /// it for a few seconds and then it's automatically cleared
    /// by `tick_status_message`.
    fn set_status_message(&mut self, msg: String) {
        self.status_message = Some((msg, std::time::Instant::now()));
    }

    /// Drop the status message if it has been on screen long
    /// enough. Called from the input loop so the user always
    /// sees fresh feedback after a yank (or any other action that
    /// sets a message), but the message doesn't linger forever.
    fn tick_status_message(&mut self) {
        const MESSAGE_TTL: std::time::Duration = std::time::Duration::from_secs(3);
        if let Some((_, when)) = &self.status_message
            && when.elapsed() > MESSAGE_TTL
        {
            self.status_message = None;
        }
    }

    /// The row the user is currently looking at, regardless of
    /// whether it came from the primary list or the labeled
    /// list. Returns `None` when no row is selected (e.g. empty
    /// history, or the cursor was reset to `None`).
    ///
    /// The cursor in `list_state` is an index into the *merged*
    /// list, not just `self.rows`. That's important when a row
    /// lives only in `self.labeled_rows` (e.g. a "very old"
    /// labeled row from a different session that the active
    /// `Mode::Sess` filter excludes from `self.rows`). The old
    /// `self.rows.get(i)` lookup silently returned `None` for
    /// such rows, which made `select_for_run` and friends do
    /// nothing — the user reported the resulting symptom as
    /// "the command line stays empty when I select a very old
    /// labelled item". Reading from the merged list fixes it.
    fn selected_row(&self) -> Option<&HistoryRow> {
        self.list_state
            .selected()
            .and_then(|i| self.merged_rows.get(i))
    }

    fn is_comment_editing(&self) -> bool {
        self.comment_edit.is_some()
    }

    fn is_output_viewing(&self) -> bool {
        self.output_view.is_some()
    }

    fn is_help_viewing(&self) -> bool {
        self.help_view.is_some()
    }

    fn open_help(&mut self) {
        self.help_view = Some(HelpView { scroll: 0 });
    }

    fn close_help(&mut self) {
        self.help_view = None;
    }

    fn is_command_menu_open(&self) -> bool {
        self.command_menu.is_some()
    }

    fn open_command_menu(&mut self) {
        self.command_menu = Some(CommandMenu::new());
    }

    fn close_command_menu(&mut self) {
        self.command_menu = None;
    }

    fn is_theme_picker_open(&self) -> bool {
        self.theme_picker.is_some()
    }

    fn open_theme_picker(&mut self) {
        self.theme_picker = Some(ThemePicker::new(self.theme));
    }

    /// Restore the picker to the theme that was active when it
    /// opened, then close. Used on `Esc`.
    fn close_theme_picker_revert(&mut self) {
        if let Some(picker) = self.theme_picker.take() {
            self.theme = picker.original;
            install_palette(self.theme);
        }
    }

    /// Commit the currently selected theme and close the picker.
    /// The picker is already applying live updates as the user
    /// navigates, so on `Enter` we just close the overlay.
    fn close_theme_picker_commit(&mut self) {
        self.theme_picker = None;
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
    let app_cfg = Config::load();
    let bindings = app_cfg.key_bindings().clone();
    let session = TuiSession::load();
    let duplicate_filter = session
        .duplicate_filter
        .unwrap_or(app_cfg.duplicate_filter);
    // Install the user-configured TUI palette (or built-in defaults)
    // into a thread-local so the draw helpers can read it without
    // needing it threaded through every signature.
    let initial_theme = session
        .theme
        .as_deref()
        .map(SelectedTheme::from_slug)
        .unwrap_or(SelectedTheme::None);
    install_palette(initial_theme);
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
    // Honor the persisted exit filter. `None` means "no
    // preference" — fall back to the global default, which is
    // "no filter" (every row shown).
    let initial_exit_filter = session
        .exit_filter
        .as_deref()
        .and_then(ExitFilter::parse)
        .unwrap_or_default();
    let mut app = App::new(
        conn,
        effective_mode,
        effective_query,
        duplicate_filter,
        initial_exit_filter,
        prefilled_query.is_some(),
        initial_theme,
        bindings,
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
            Mode::Stats => "STATS".to_string(),
        }),
        query: Some(app.query.clone()),
        duplicate_filter: Some(app.duplicate_filter),
        // Persist only when the user has changed the filter
        // away from the default — same policy as the other
        // session fields (we only remember what differs from
        // the config-file defaults, so deleting the file resets
        // the user to the same state they'd get on first run).
        exit_filter: if app.exit_filter == ExitFilter::default() {
            None
        } else {
            Some(app.exit_filter.as_str().to_string())
        },
        theme: Some(app.theme.slug().to_string()),
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
        if let Err(e) = terminal.draw(|f| render::ui(f, app)) {
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
    // Every keypress is a chance to clear a stale status message
    // (e.g. the "Yanked 12 chars" feedback that should fade after
    // a few seconds). Doing this at the top of the input loop
    // means the message stays visible while the user is reading
    // it and disappears as soon as they interact again.
    app.tick_status_message();

    // The command palette sits above the help overlay so it can
    // dispatch actions (including open-help) without the overlay
    // intercepting keys.
    if app.is_command_menu_open() {
        return handle_command_menu_key(app, key);
    }

    // The theme picker also takes precedence over the help
    // overlay so it can receive the same arrow / Ctrl-N / Ctrl-P
    // keys that the cycling actions use.
    if app.is_theme_picker_open() {
        return handle_theme_picker_key(app, key);
    }

    // When the help overlay is open, route all input to its handler
    // (Esc / q / Enter / Ctrl-C close it, arrows scroll).
    if app.is_help_viewing() {
        return handle_help_view_key(app, key);
    }

    // When prompting for deletion, only allow 'y' or 'n' or Esc/Ctrl+C.
    if let Some(mode) = app.confirm_delete {
        return handle_confirm_delete_key(app, key, mode);
    }

    // When editing a comment, most keys go to the comment buffer.
    if app.is_comment_editing() {
        return handle_comment_edit_key(app, key);
    }

    // Action-based dispatch: look up the user-configured binding
    // for this key. Anything not explicitly bound falls through to
    // the default "type a character into the query" behavior.
    if let Some(action) = action_for_key(&app.bindings, &key) {
        return dispatch_action(app, action);
    }

    // Unbound characters extend the query. We accept any plain
    // printable character (Shift is allowed — terminals report
    // uppercase letters as `Char('G')` + `SHIFT`, so excluding
    // SHIFT would silently swallow every uppercase letter and
    // every shifted symbol). Ctrl / Alt are still ignored so we
    // don't accidentally trigger terminal-specific shortcuts.
    if !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && let KeyCode::Char(c) = key.code {
            app.push_char(c);
        }
    false
}

/// Execute a single `Action`. Returns `true` when the action
/// terminates the TUI (selection made or cancelled); `false`
/// otherwise. Mirrors the structure of the previous hand-written
/// match blocks but is now driven entirely by the binding table.
fn dispatch_action(app: &mut App, action: Action) -> bool {
    match action {
        Action::Cancel => {
            app.cancelled = true;
            true
        }
        Action::CycleMode => {
            app.cycle_mode();
            false
        }
        Action::ToggleDuplicateFilter => {
            app.toggle_duplicate_filter();
            false
        }
        Action::CycleThemeNext => {
            app.cycle_theme_next();
            false
        }
        Action::CycleThemePrev => {
            app.cycle_theme_prev();
            false
        }
        Action::EditComment => {
            app.start_comment_edit();
            false
        }
        Action::ShowOutput => {
            app.show_output_view();
            false
        }
        Action::YankSelection => {
            app.yank_to_clipboard();
            false
        }
        Action::EditFileReference => {
            // The action stages `$EDITOR <path>` as the next
            // selection. When `selection` is set, the TUI is
            // done and the parent shell runs the command. When
            // the action is a no-op (no row, no path, …) the
            // status message has been set and the TUI stays
            // open so the user can react to the feedback.
            app.edit_referenced_file();
            app.selection.is_some()
        }
        Action::OpenHelp => {
            app.open_help();
            false
        }
        Action::DeleteSelected => {
            app.confirm_delete = Some(ConfirmMode::DeleteSelected);
            false
        }
        Action::DeleteMatching => {
            app.confirm_delete = Some(ConfirmMode::DeleteMatching);
            false
        }
        Action::ClearQuery => {
            app.clear_query();
            false
        }
        Action::CycleExitFilter => {
            app.cycle_exit_filter();
            false
        }
        Action::Run => {
            app.select_for_run();
            true
        }
        Action::EditStart => {
            app.select_for_edit_start();
            true
        }
        Action::EditEnd => {
            app.select_for_edit_end();
            true
        }
        // Movement keys share a "user is navigating, so the cached
        // prefilled query should be appended to" side effect.
        Action::Up => {
            app.move_selection(1);
            app.query_prefilled = false;
            false
        }
        Action::Down => {
            app.move_selection(-1);
            app.query_prefilled = false;
            false
        }
        Action::PageUp => {
            app.move_selection(10);
            app.query_prefilled = false;
            false
        }
        Action::PageDown => {
            app.move_selection(-10);
            app.query_prefilled = false;
            false
        }
        Action::Home => {
            if !app.rows.is_empty() {
                app.list_state.select(Some(app.rows.len() - 1));
            }
            app.query_prefilled = false;
            false
        }
        Action::End => {
            if !app.rows.is_empty() {
                app.list_state.select(Some(0));
            }
            app.query_prefilled = false;
            false
        }
        Action::Backspace => {
            app.backspace();
            false
        }
        Action::CommandAction => {
            app.open_command_menu();
            false
        }
        Action::ThemePicker => {
            app.open_theme_picker();
            false
        }
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

/// Key handler used while the help overlay is open. Returns `true`
/// only when the user aborts the whole TUI with Ctrl+C.
fn handle_help_view_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
            app.close_help();
            false
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cancelled = true;
            app.close_help();
            true
        }
        KeyCode::Up => {
            if let Some(ref mut view) = app.help_view {
                view.scroll = view.scroll.saturating_sub(1);
            }
            false
        }
        KeyCode::Down => {
            if let Some(ref mut view) = app.help_view {
                view.scroll = view.scroll.saturating_add(1);
            }
            false
        }
        KeyCode::PageUp => {
            if let Some(ref mut view) = app.help_view {
                view.scroll = view.scroll.saturating_sub(10);
            }
            false
        }
        KeyCode::PageDown => {
            if let Some(ref mut view) = app.help_view {
                view.scroll = view.scroll.saturating_add(10);
            }
            false
        }
        KeyCode::Home => {
            if let Some(ref mut view) = app.help_view {
                view.scroll = 0;
            }
            false
        }
        KeyCode::End => {
            // Clamped on render. We just push the scroll forward so
            // the user can always reach the bottom.
            if let Some(ref mut view) = app.help_view {
                view.scroll = view.scroll.saturating_add(usize::MAX / 2);
            }
            false
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

/// Key handler used while the command palette is open. Mirrors
/// the help-overlay pattern but executes the highlighted action
/// instead of scrolling.
fn handle_command_menu_key(app: &mut App, key: KeyEvent) -> bool {
    // Esc / q / Ctrl-C close the palette without running anything.
    if matches!(
        key.code,
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q')
    ) {
        app.close_command_menu();
        return false;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.cancelled = true;
        app.close_command_menu();
        return true;
    }

    // Capture a mutable borrow of the menu once so the closures
    // below can use it without conflicting with the immutable
    // borrow on `app`.
    let menu = match app.command_menu.as_mut() {
        Some(m) => m,
        None => return false,
    };

    match key.code {
        KeyCode::Enter => {
            // Run the highlighted action.
            let indices = menu.filtered_indices();
            if let Some(&idx) = indices.get(menu.selected) {
                let action = menu.actions[idx];
                // Close the palette BEFORE dispatching so the
                // action runs against the un-modified app state.
                app.close_command_menu();
                return dispatch_action(app, action);
            }
            // Empty list — just close the palette.
            app.close_command_menu();
            false
        }
        KeyCode::Up => {
            if menu.selected > 0 {
                menu.selected -= 1;
            }
            false
        }
        KeyCode::Down => {
            let n = menu.filtered_indices().len();
            if n > 0 && menu.selected + 1 < n {
                menu.selected += 1;
            }
            false
        }
        KeyCode::PageUp => {
            menu.selected = menu.selected.saturating_sub(5);
            menu.clamp_selection();
            false
        }
        KeyCode::PageDown => {
            let n = menu.filtered_indices().len();
            menu.selected = (menu.selected + 5).min(n.saturating_sub(1));
            false
        }
        KeyCode::Home => {
            menu.selected = 0;
            false
        }
        KeyCode::End => {
            let n = menu.filtered_indices().len();
            if n > 0 {
                menu.selected = n - 1;
            }
            false
        }
        KeyCode::Backspace => {
            if !menu.query.is_empty() {
                menu.touched = true;
                menu.query.pop();
                menu.clamp_selection();
            }
            false
        }
        KeyCode::Char(c) => {
            // Allow plain printable characters; ignore Ctrl/Alt
            // so we don't trigger accidental shortcuts.
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
            {
                if menu.query_prefilled_replacement_armed() {
                    menu.query.clear();
                }
                menu.touched = true;
                menu.query.push(c);
                menu.clamp_selection();
            }
            false
        }
        _ => false,
    }
}

impl CommandMenu {
    /// Internal helper: `true` when the user has just opened the
    /// palette and not yet typed anything, so the first character
    /// should replace any prefilled value rather than append.
    /// (We always start with an empty query, so this is currently
    /// a synonym for `!self.touched`, but keeping the indirection
    /// leaves room for future "last-used query" support.)
    fn query_prefilled_replacement_armed(&self) -> bool {
        !self.touched && !self.query.is_empty()
    }
}

/// Key handler for the theme picker. Up/Down (and the Ctrl-N /
/// Ctrl-P shortcuts that also drive the live cycling) move the
/// selection and immediately apply the new theme via
/// `install_palette`. Enter commits, Esc reverts to the original
/// theme, Home/End jump to the first/last entry.
fn handle_theme_picker_key(app: &mut App, key: KeyEvent) -> bool {
    // Esc / Ctrl-C always revert to the original theme and
    // close. Ctrl-C additionally aborts the whole TUI.
    if key.code == KeyCode::Esc {
        app.close_theme_picker_revert();
        return false;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.cancelled = true;
        app.close_theme_picker_revert();
        return true;
    }

    // Enter commits the currently-highlighted theme. The live
    // preview is already in effect because we apply each move
    // immediately, so "commit" just means "close the overlay".
    if key.code == KeyCode::Enter {
        app.close_theme_picker_commit();
        return false;
    }

    // Determine the navigation delta. We treat the same keys as the
    // standalone cycling actions so muscle memory works either way:
    //   Down / C-n  -> next theme
    //   Up / C-p    -> previous theme
    //   Home        -> first theme (the manual "no theme" entry)
    //   End         -> last theme
    let delta: Option<isize> = match key.code {
        KeyCode::Down => Some(1),
        KeyCode::Up => Some(-1),
        KeyCode::PageDown => Some(5),
        KeyCode::PageUp => Some(-5),
        KeyCode::Home => {
            if let Some(picker) = app.theme_picker.as_mut() {
                picker.selected = 0;
                app.theme = picker.current();
                install_palette(app.theme);
            }
            return false;
        }
        KeyCode::End => {
            if let Some(picker) = app.theme_picker.as_mut() {
                picker.selected = picker.themes.len().saturating_sub(1);
                app.theme = picker.current();
                install_palette(app.theme);
            }
            return false;
        }
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(1),
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(-1),
        _ => None,
    };

    if let Some(delta) = delta
        && let Some(picker) = app.theme_picker.as_mut() {
            picker.move_by(delta);
            app.theme = picker.current();
            install_palette(app.theme);
        }
    false
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_matches_empty_query() {
        let spans = super::render::highlight_matches("hello world", "");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world".to_string());
    }

    #[test]
    fn highlight_matches_single() {
        let spans = super::render::highlight_matches("git status", "stat");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["git ", "stat", "us"]);
    }

    #[test]
    fn highlight_matches_case_insensitive() {
        let spans = super::render::highlight_matches("Git Status", "stat");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["Git ", "Stat", "us"]);
    }

    #[test]
    fn highlight_matches_multiple() {
        let spans = super::render::highlight_matches("foo bar foo", "foo");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["foo", " bar ", "foo"]);
    }

    #[test]
    fn highlight_matches_no_match() {
        let spans = super::render::highlight_matches("hello world", "xyz");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world".to_string());
    }

    #[test]
    fn highlight_matches_multi_word() {
        let spans = super::render::highlight_matches("git commit -m", "git commit");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["git", " ", "commit", " -m"]);
    }

    #[test]
    fn highlight_matches_multi_word_out_of_order() {
        let spans = super::render::highlight_matches("git commit -m", "commit git");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["git", " ", "commit", " -m"]);
    }

    #[test]
    fn build_implicit_regex_plain() {
        // No anchors → wrap with `.*` on both sides.
        assert_eq!(build_implicit_regex("git commit"), ".*git commit.*");
        assert_eq!(build_implicit_regex("foo"), ".*foo.*");
    }

    #[test]
    fn build_implicit_regex_start_anchor() {
        // Leading `^` suppresses the implicit `.*` on the left.
        assert_eq!(build_implicit_regex("^git commit"), "^git commit.*");
        assert_eq!(build_implicit_regex("^foo"), "^foo.*");
    }

    #[test]
    fn build_implicit_regex_end_anchor() {
        // Trailing `$` suppresses the implicit `.*` on the right.
        assert_eq!(build_implicit_regex("git$"), ".*git$");
        assert_eq!(build_implicit_regex("foo bar$"), ".*foo bar$");
    }

    #[test]
    fn build_implicit_regex_both_anchors() {
        // Both anchors present → no implicit `.*` added.
        assert_eq!(build_implicit_regex("^git$"), "^git$");
        assert_eq!(build_implicit_regex("^foo bar$"), "^foo bar$");
    }

    #[test]
    fn build_implicit_regex_empty() {
        // Empty pattern still gets `.*` wrappers — useful for
        // `/` alone (matches everything).
        assert_eq!(build_implicit_regex(""), ".*.*");
    }

    #[test]
    fn parse_key_spec_plain() {
        let spec = bindings::parse_key_spec("a").unwrap();
        assert_eq!(spec.code, KeyCode::Char('a'));
        assert!(spec.modifiers.is_empty());

        let spec = bindings::parse_key_spec("/").unwrap();
        assert_eq!(spec.code, KeyCode::Char('/'));
    }

    #[test]
    fn parse_key_spec_ctrl() {
        let spec = bindings::parse_key_spec("C-h").unwrap();
        assert_eq!(spec.code, KeyCode::Char('h'));
        assert!(spec.modifiers.contains(KeyModifiers::CONTROL));
        assert!(!spec.modifiers.contains(KeyModifiers::ALT));

        // Uppercase and lowercase both work.
        let spec = bindings::parse_key_spec("c-H").unwrap();
        assert_eq!(spec.code, KeyCode::Char('H'));
        assert!(spec.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn parse_key_spec_alt_and_combinations() {
        let spec = bindings::parse_key_spec("M-x").unwrap();
        assert_eq!(spec.code, KeyCode::Char('x'));
        assert!(spec.modifiers.contains(KeyModifiers::ALT));

        let spec = bindings::parse_key_spec("C-M-h").unwrap();
        assert_eq!(spec.code, KeyCode::Char('h'));
        assert!(spec.modifiers.contains(KeyModifiers::CONTROL));
        assert!(spec.modifiers.contains(KeyModifiers::ALT));
    }

    #[test]
    fn parse_key_spec_named_keys() {
        assert_eq!(bindings::parse_key_spec("Esc").unwrap().code, KeyCode::Esc);
        assert_eq!(bindings::parse_key_spec("Enter").unwrap().code, KeyCode::Enter);
        assert_eq!(bindings::parse_key_spec("Backspace").unwrap().code, KeyCode::Backspace);
        assert_eq!(bindings::parse_key_spec("Up").unwrap().code, KeyCode::Up);
        assert_eq!(bindings::parse_key_spec("PageUp").unwrap().code, KeyCode::PageUp);
        assert_eq!(bindings::parse_key_spec("F5").unwrap().code, KeyCode::F(5));
    }

    #[test]
    fn parse_key_spec_invalid() {
        assert!(bindings::parse_key_spec("").is_err());
        assert!(bindings::parse_key_spec("not-a-single-char").is_err());
    }

    #[test]
    fn action_for_key_roundtrip() {
        let bindings = KeyBindings::defaults();
        // C-h is the default for OpenHelp.
        let evt = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(action_for_key(&bindings, &evt), Some(Action::OpenHelp));
        // Unbound plain char → None.
        let evt = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::empty());
        assert_eq!(action_for_key(&bindings, &evt), None);
        // Uppercase letters (Shift held) are unbound at the action
        // level — they fall through to the input path, which must
        // accept them rather than swallow them.
        let evt = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(action_for_key(&bindings, &evt), None);
        // Shift+symbol also falls through (e.g. "?" typed via
        // Shift+/).
        let evt = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(action_for_key(&bindings, &evt), None);
    }

    #[test]
    fn key_bindings_from_config_overrides() {
        // Entries are keyed by the bare action name (without the
        // `key.` prefix); `Config::parse` strips the prefix before
        // inserting into the map.
        let mut entries = std::collections::HashMap::new();
        entries.insert("open-help".to_string(), "M-h".to_string());
        entries.insert("cancel".to_string(), "C-q".to_string());
        let bindings = bindings::key_bindings_from_config(&entries);
        assert_eq!(
            format_key_specs(bindings.specs(Action::OpenHelp)),
            "M-h".to_string()
        );
        assert_eq!(
            format_key_specs(bindings.specs(Action::Cancel)),
            "C-q".to_string()
        );
        // Unmentioned actions keep their defaults.
        assert_eq!(
            format_key_specs(bindings.specs(Action::DeleteSelected)),
            "C-d".to_string()
        );
    }

    #[test]
    fn key_bindings_from_config_unknown_action_is_reported() {
        // `toggle-duplication-filter` (extra "ation") is a typo of
        // `toggle-duplicate-filter` and must not silently bind to
        // anything. Capture stderr to confirm the warning is
        // emitted, then ensure the matching default still wins.
        let mut entries = std::collections::HashMap::new();
        entries.insert(
            "toggle-duplication-filter".to_string(),
            "C-d".to_string(),
        );
        let bindings = bindings::key_bindings_from_config(&entries);
        // Unknown action does not pollute any known action.
        assert_eq!(
            format_key_specs(bindings.specs(Action::ToggleDuplicateFilter)),
            Action::ToggleDuplicateFilter.default_key().to_string()
        );
    }

    #[test]
    fn parse_key_spec_unbind_sentinels() {
        // `none`, `off`, `disable`, `-`, `disabled` (case
        // insensitive) all map to `Ok(None)` — the action is
        // unbound, not bound to a literal "None" key.
        for sentinel in ["none", "NONE", "off", "disable", "-", "disabled"] {
            let parsed = bindings::parse_key_spec_opt(sentinel).unwrap();
            assert!(parsed.is_none(), "sentinel {sentinel:?} should unbind");
        }
    }

    #[test]
        fn key_bindings_from_config_unbind_action() {
                let mut entries = std::collections::HashMap::new();
                entries.insert("open-help".to_string(), "none".to_string());
                let bindings = bindings::key_bindings_from_config(&entries);
                assert!(bindings.is_unbound(Action::OpenHelp));
                assert!(bindings.specs(Action::OpenHelp).is_empty());
                // Unbinding one action must not affect siblings.
                assert!(!bindings.is_unbound(Action::Cancel));
                assert!(!bindings.specs(Action::Cancel).is_empty());
                // `action_for_key` must not fire for unbound actions.
                let evt = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
                assert_eq!(action_for_key(&bindings, &evt), None);
        }

        #[test]
        fn key_bindings_from_config_multi_key() {
                // `key.open-help=C-h, F1` binds the help overlay to
                // both Ctrl-H and F1. Whitespace around the comma
                // is allowed.
                let mut entries = std::collections::HashMap::new();
                entries.insert(
                        "open-help".to_string(),
                        "C-h, F1".to_string(),
                );
                let bindings = bindings::key_bindings_from_config(&entries);
                let specs = bindings.specs(Action::OpenHelp);
                assert_eq!(specs.len(), 2);
                // Both keys must fire the action.
                let ctrl_h = KeyEvent::new(
                        KeyCode::Char('h'),
                        KeyModifiers::CONTROL,
                );
                let f1 = KeyEvent::new(KeyCode::F(1), KeyModifiers::empty());
                assert_eq!(
                        action_for_key(&bindings, &ctrl_h),
                        Some(Action::OpenHelp)
                );
                assert_eq!(
                        action_for_key(&bindings, &f1),
                        Some(Action::OpenHelp)
                );
                // The display string is comma-joined.
                assert_eq!(format_key_specs(specs), "C-h, F1");
        }

        #[test]
        fn key_bindings_from_config_multi_key_three_way() {
                // Three specs in one entry, no surrounding spaces.
                let mut entries = std::collections::HashMap::new();
                entries.insert(
                        "cancel".to_string(),
                        "Esc,C-c,C-g".to_string(),
                );
                let bindings = bindings::key_bindings_from_config(&entries);
                assert_eq!(
                        bindings.specs(Action::Cancel).len(),
                        3
                );
                assert_eq!(
                        format_key_specs(bindings.specs(Action::Cancel)),
                        "Esc, C-c, C-g"
                );
        }

        #[test]
        fn key_bindings_from_config_multi_key_with_none_unbinds() {
                // The unbind sentinel anywhere in a comma list
                // means the action is unbound. `Esc` is silently
                // discarded — we don't want to half-apply a
                // binding the user thought they disabled.
                let mut entries = std::collections::HashMap::new();
                entries.insert(
                        "cancel".to_string(),
                        "none,Esc".to_string(),
                );
                let bindings = bindings::key_bindings_from_config(&entries);
                assert!(bindings.is_unbound(Action::Cancel));
                assert!(bindings.specs(Action::Cancel).is_empty());
        }

        #[test]
        fn key_bindings_from_config_multi_key_bad_spec_drops_all() {
                // One bad spec in a list drops the whole binding
                // (no half-applied config). The default wins.
                let mut entries = std::collections::HashMap::new();
                entries.insert(
                        "open-help".to_string(),
                        "C-h,not-a-key,F1".to_string(),
                );
                let bindings = bindings::key_bindings_from_config(&entries);
                assert_eq!(
                        bindings.specs(Action::OpenHelp).len(),
                        1,
                        "should keep only the default for OpenHelp"
                );
                assert_eq!(
                        format_key_specs(bindings.specs(Action::OpenHelp)),
                        Action::OpenHelp.default_key()
                );
        }

        #[test]
        fn command_menu_filter_matches() {
                let menu = CommandMenu::new();
                // Empty query returns every action.
                assert_eq!(menu.filtered_indices().len(), ALL_ACTIONS.len());
                // Substring match against the display name.
                let m = CommandMenu {
                        query: "delete".into(),
                        ..CommandMenu::new()
                };
                let filtered = m.filtered_indices();
                assert!(filtered
                        .iter()
                        .all(|&i| ALL_ACTIONS[i].display_name().to_lowercase().contains("delete")));
                assert!(filtered
                        .iter()
                        .any(|&i| ALL_ACTIONS[i] == Action::DeleteSelected));
                assert!(filtered
                        .iter()
                        .any(|&i| ALL_ACTIONS[i] == Action::DeleteMatching));
                // Multi-word AND: "open help" matches OpenHelp (also
                // ShowOutput because its name contains "open"? — actually
                // it doesn't, so only OpenHelp should match).
                let m = CommandMenu {
                        query: "open help".into(),
                        ..CommandMenu::new()
                };
                let filtered = m.filtered_indices();
                assert!(filtered
                        .iter()
                        .any(|&i| ALL_ACTIONS[i] == Action::OpenHelp));
                assert!(!filtered
                        .iter()
                        .any(|&i| ALL_ACTIONS[i] == Action::ShowOutput));
                // `clamp_selection` keeps the cursor inside the filtered
                // list when items disappear (e.g. user deletes the last char).
                let mut m = CommandMenu::new();
                m.selected = ALL_ACTIONS.len() - 1;
                m.query = "no-such-action".into();
                m.clamp_selection();
                assert_eq!(m.selected, 0);
        }

        #[test]
        fn command_action_has_default_binding_and_routes() {
                let bindings = KeyBindings::defaults();
                // The default key for CommandAction is ":" (matches the
                // vim-style command palette convention).
                assert_eq!(
                        format_key_specs(bindings.specs(Action::CommandAction)),
                        ":".to_string()
                );
                // Pressing ":" fires the CommandAction.
                let evt = KeyEvent::new(KeyCode::Char(':'), KeyModifiers::empty());
                assert_eq!(
                        action_for_key(&bindings, &evt),
                        Some(Action::CommandAction)
                );
        }

        #[test]
        fn theme_picker_default_binding_and_list_layout() {
                let bindings = KeyBindings::defaults();
                // Default key is `T` so it doesn't collide with the
                // Ctrl-N / Ctrl-P cycling shortcuts.
                assert_eq!(
                        format_key_specs(bindings.specs(Action::ThemePicker)),
                        "T".to_string()
                );
                // Pressing T fires the ThemePicker.
                let evt = KeyEvent::new(KeyCode::Char('T'), KeyModifiers::empty());
                assert_eq!(
                        action_for_key(&bindings, &evt),
                        Some(Action::ThemePicker)
                );
                // Picker contains every theme: `None` plus the
                // canonical `ratatui-themes::ThemeName::all()` list.
                let p = ThemePicker::new(SelectedTheme::None);
                assert_eq!(p.themes.len(), BuiltinTheme::all().len() + 1);
                assert_eq!(p.themes[0], SelectedTheme::None);
                assert!(p
                        .themes
                        .iter()
                        .skip(1)
                        .all(|t| matches!(t, SelectedTheme::Builtin(_))));
                // `move_by` clamps to the list bounds.
                let mut p = ThemePicker::new(SelectedTheme::None);
                p.move_by(-10);
                assert_eq!(p.selected, 0);
                p.move_by(9999);
                assert_eq!(p.selected, p.themes.len() - 1);
        }

        #[test]
        fn curated_themes_parse_and_cycle() {
                // Every curated theme must:
                //   * have a unique, kebab-case slug,
                //   * round-trip through `from_slug`,
                //   * show up in `BuiltinTheme::all()` exactly once.
                let mut seen = std::collections::HashSet::new();
                for t in BuiltinTheme::curated() {
                        let s = t.slug();
                        assert!(!s.is_empty(), "empty slug for {:?}", t);
                        assert!(s.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                                "slug {:?} not kebab-case", s);
                        assert!(seen.insert(s), "duplicate slug {}", s);
                        let parsed = SelectedTheme::from_slug(s);
                        assert_eq!(parsed, SelectedTheme::Builtin(*t),
                                "from_slug round-trip failed for {:?}", s);
                }
                // Upstream themes still parse (regression check).
                assert_eq!(
                        SelectedTheme::from_slug("dracula"),
                        SelectedTheme::Builtin(BuiltinTheme::Dracula)
                );
                // Unknown slug falls back to None.
                assert_eq!(SelectedTheme::from_slug("totally-made-up"), SelectedTheme::None);
        }

        #[test]
        fn mode_cycle_and_parse() {
                // Cycling wraps through the four modes.
                assert_eq!(Mode::Sess.next(), Mode::Dir);
                assert_eq!(Mode::Dir.next(), Mode::Global);
                assert_eq!(Mode::Global.next(), Mode::Stats);
                assert_eq!(Mode::Stats.next(), Mode::Sess);
                // String parsing is case-insensitive and accepts the
                // documented aliases.
                assert_eq!(Mode::parse("stats"), Some(Mode::Stats));
                assert_eq!(Mode::parse("STATISTICS"), Some(Mode::Stats));
                assert_eq!(Mode::parse("Stats"), Some(Mode::Stats));
                assert!(Mode::parse("not-a-mode").is_none());
        }

        #[test]
        fn exit_filter_cycles_through_three_states() {
                // The action is bound to Ctrl-J by default; the
                // user cycles All → OK → ERR → All.
                assert_eq!(ExitFilter::All.next(), ExitFilter::Success);
                assert_eq!(ExitFilter::Success.next(), ExitFilter::Failed);
                assert_eq!(ExitFilter::Failed.next(), ExitFilter::All);
                // Default is `All` (no filter, see every row).
                assert_eq!(ExitFilter::default(), ExitFilter::All);
        }

        #[test]
        fn exit_filter_as_str_round_trips_through_parse() {
                // The session file and any future config-file knob
                // use the lowercase form returned by `as_str()`.
                for value in [ExitFilter::All, ExitFilter::Success, ExitFilter::Failed] {
                        assert_eq!(ExitFilter::parse(value.as_str()), Some(value));
                }
                // `parse` is case-insensitive and accepts aliases.
                assert_eq!(ExitFilter::parse("OK"), Some(ExitFilter::Success));
                assert_eq!(ExitFilter::parse("success"), Some(ExitFilter::Success));
                assert_eq!(ExitFilter::parse("err"), Some(ExitFilter::Failed));
                assert_eq!(ExitFilter::parse("FAILED"), Some(ExitFilter::Failed));
                // Unknown values fall through to `None` so the
                // caller can keep the default.
                assert!(ExitFilter::parse("maybe").is_none());
                assert!(ExitFilter::parse("").is_none());
        }

        /// Build a fresh in-memory `App` whose `history` table is
        /// pre-populated with the rows in `rows`. `rows` is a slice
        /// of `(command, timestamp_offset_secs)` — the timestamp is
        /// `now - offset` so the tests are stable regardless of when
        /// they run.
        fn stats_test_app(rows: &[(&str, i64)]) -> App {
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now'))
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
                )
                .expect("schema");
                let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                for (i, (cmd, offset)) in rows.iter().enumerate() {
                        conn.execute(
                                "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
                                 VALUES (?1, ?2, '/tmp', 'sess', 0, ?3)",
                                rusqlite::params![i as i64 + 1, *cmd, now - *offset],
                        )
                        .expect("insert");
                }
                let mut app = App::new(
                        conn,
                        Mode::Stats,
                        String::new(),
                        false,
                        ExitFilter::All,
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                );
                app.refresh();
                app
        }

        #[test]
        fn stats_mode_ranks_by_follow_frequency_then_age() {
                // Sequence (oldest first):
                //   A B A B C A D
                // The "last command" is D. Its successors in the
                // global history are: A (once). So A should rank
                // first. The remaining rows are sorted by timestamp
                // DESC: D is excluded (it's the last command itself
                // in this test since we always pick the newest), A
                // (just after D), B, C — with the duplicate filter
                // off, every occurrence is shown.
                //
                // For a cleaner test we use a sequence where the
                // last command has multiple distinct successors with
                // known frequencies:
                //   seq:    X Y X Y Z X Y W
                //   newest: W
                //   successors of W: none (it's the most recent)
                //   but we want a non-W last, so add a trailing W2:
                //   seq:    X Y X Y Z X Y W W2
                //   last:   W2 (newest). Successors of W2: none yet.
                //
                // We rebuild the sequence so the *last* command has
                // many successors: arrange so the global newest row
                // is `git status`. Successors of `git status` in
                // the history should be ranked first; everything else
                // falls back to timestamp DESC.
                let rows: &[(&str, i64)] = &[
                    ("vim Cargo.toml", 50),
                    ("cargo build", 45),
                    ("vim Cargo.toml", 40),
                    ("git status", 35),
                    ("vim Cargo.toml", 30),
                    ("cargo build", 25),
                    ("git status", 20),
                    ("cargo build", 15),
                    ("git status", 10), // oldest
                ];
                let _ = rows; // appease unused-warning fixers
                let rows: &[(&str, i64)] = &[
                    ("vim Cargo.toml", 90),
                    ("cargo build", 85),
                    ("vim Cargo.toml", 80),
                    ("git status", 75),
                    ("vim Cargo.toml", 70),
                    ("cargo build", 65),
                    ("git status", 60),
                    ("cargo build", 55),
                    ("git status", 50),
                    ("cargo build", 45),
                    ("git status", 40),
                    ("cargo build", 35),
                    ("git status", 30),
                    ("cargo build", 25),
                    ("ls", 20),
                    ("echo hello", 15),
                    // Newest: `git status` — so it's the "last command".
                    ("git status", 10),
                ];
                let app = stats_test_app(rows);
                assert_eq!(app.mode, Mode::Stats);
                let merged = app.merged_rows();
                // The newest row is `git status` (timestamp 10).
                // Its successors in the entire history are
                // `cargo build` and `vim Cargo.toml`. Counting pairs:
                //   git status -> cargo build: 5 times
                //   git status -> vim Cargo.toml: 3 times
                // So cargo build ranks above vim, then the rest of
                // the history sorted by timestamp DESC.
                let cmds: Vec<&str> = merged.iter().map(|r| r.command.as_str()).collect();
                // 6 cargo build entries with freq=4, 3 vim with
                // freq=1, then the rest sorted by timestamp DESC.
                assert_eq!(cmds.len(), 17,
                        "expected every history row to come back, got {} rows: {:?}",
                        cmds.len(), cmds);
                assert_eq!(cmds[0], "cargo build",
                        "expected highest frequency successor first, got {:?}",
                        cmds);
                assert_eq!(cmds[5], "cargo build",
                        "6 cargo build rows should share freq=4, got {:?}",
                        cmds);
                assert_eq!(cmds[6], "vim Cargo.toml",
                        "vim should follow cargo's freq=4 rows, got {:?}",
                        cmds);
                assert!(!cmds.is_empty());
        }

        #[test]
        fn stats_mode_duplicate_filter_keeps_newest_only() {
                let rows: &[(&str, i64)] = &[
                    ("git status", 30),
                    ("cargo build", 25),
                    ("git status", 20),
                    ("vim Cargo.toml", 15),
                    ("git status", 10), // newest
                ];
                let mut app = stats_test_app(rows);
                // Duplicate filter on: only one `cargo build`,
                // one `vim Cargo.toml`, one `git status`.
                app.duplicate_filter = true;
                app.refresh();
                let binding = app.merged_rows();
                let cmds: Vec<&str> = binding
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                // Each unique command appears at most once.
                let mut sorted = cmds.clone();
                sorted.sort();
                sorted.dedup();
                assert_eq!(sorted.len(), cmds.len(),
                        "duplicate filter should remove duplicates: {:?}",
                        cmds);
        }

        /// The exit-code filter is implemented in SQL: the
        /// `build_where` helper appends a clause to the SELECT
        /// statement. These tests confirm the clause is present
        /// (or absent, in the All case) regardless of mode.
        #[test]
        fn exit_filter_all_adds_no_clause() {
                let app = stats_test_app(&[("git status", 1)]);
                let (clause, _) = app.build_where();
                assert!(
                        !clause.contains("exit_code"),
                        "All should not add an exit_code clause, got: {:?}",
                        clause
                );
        }

        #[test]
        fn exit_filter_success_matches_only_zero() {
                let mut app = stats_test_app(&[("true", 1), ("false", 1)]);
                // Cycle from All → Success.
                app.cycle_exit_filter();
                let (clause, _) = app.build_where();
                assert!(
                        clause.contains("h.exit_code = 0"),
                        "Success should add `h.exit_code = 0`, got: {:?}",
                        clause
                );
        }

        #[test]
        fn exit_filter_failed_matches_only_nonzero() {
                let mut app = stats_test_app(&[("true", 1), ("false", 1)]);
                app.cycle_exit_filter(); // All → Success
                app.cycle_exit_filter(); // Success → Failed
                let (clause, _) = app.build_where();
                assert!(
                        clause.contains("h.exit_code != 0"),
                        "Failed should add `h.exit_code != 0`, got: {:?}",
                        clause
                );
        }

        /// End-to-end: cycle the filter and confirm `refresh`
        /// actually changes the row set. The test inserts rows
        /// with a mix of exit codes, so the filter should split
        /// them cleanly.
        #[test]
        fn cycle_exit_filter_refreshes_rows() {
                // The `stats_test_app` helper hard-codes
                // `exit_code = 0` for every row, which would make
                // "Success" and "All" indistinguishable. Insert
                // our own mixed table here.
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
                )
                .expect("create tables");
                let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64;
                // (command, timestamp_offset, exit_code)
                let rows: &[(&str, i64, i32)] = &[
                    ("true",         30, 0),  // success
                    ("false",        25, 1),  // failure
                    ("git status",   20, 0),  // success
                    ("segfault",     15, 139), // failure
                ];
                for (i, (cmd, offset, code)) in rows.iter().enumerate() {
                        conn.execute(
                                "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
                                 VALUES (?1, ?2, '/tmp', 'sess', ?3, ?4)",
                                rusqlite::params![
                                        i as i64 + 1,
                                        *cmd,
                                        *code,
                                        now - *offset,
                                ],
                        )
                        .expect("insert");
                }
                let mut app = App::new(
                        conn,
                        Mode::Stats,
                        String::new(),
                        false,
                        ExitFilter::All,
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                );
                app.refresh();
                let all_count = app.merged_rows().len();
                assert_eq!(all_count, 4, "All should show every row");

                app.cycle_exit_filter(); // → Success
                let ok_count = app.merged_rows().len();
                assert_eq!(ok_count, 2, "Success should keep only exit_code == 0");
                for r in app.merged_rows() {
                        assert_eq!(r.exit_code, 0, "Success row had nonzero exit_code");
                }

                app.cycle_exit_filter(); // → Failed
                let err_count = app.merged_rows().len();
                assert_eq!(err_count, 2, "Failed should keep only exit_code != 0");
                for r in app.merged_rows() {
                        assert_ne!(r.exit_code, 0, "Failed row had zero exit_code");
                }

                app.cycle_exit_filter(); // → All (wraps)
                assert_eq!(app.merged_rows().len(), 4);
                assert_eq!(app.exit_filter, ExitFilter::All);
        }

        /// The default key for `CycleExitFilter` is `Ctrl-J`; make
        /// sure that still works after the refactor.
        #[test]
        fn cycle_exit_filter_default_key_routes() {
                let bindings = KeyBindings::defaults();
                assert_eq!(
                        format_key_specs(bindings.specs(Action::CycleExitFilter)),
                        "C-j"
                );
                let evt = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
                assert_eq!(
                        action_for_key(&bindings, &evt),
                        Some(Action::CycleExitFilter)
                );
        }

        /// `YankSelection` is bound to `Ctrl-Y` (the canonical
        /// readline/vim yank shortcut) and the action_for_key
        /// lookup routes the keystroke correctly.
        #[test]
        fn yank_selection_default_key_routes() {
                let bindings = KeyBindings::defaults();
                assert_eq!(
                        format_key_specs(bindings.specs(Action::YankSelection)),
                        "C-y"
                );
                let evt = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
                assert_eq!(
                        action_for_key(&bindings, &evt),
                        Some(Action::YankSelection)
                );
        }

        /// `pick_text_to_yank` falls back to the selected row's
        /// command when the output view is closed.
        #[test]
        fn pick_text_to_yank_uses_selected_row() {
                let app = stats_test_app(&[("echo hello", 30)]);
                // Default `list_state` from `App::new` selects
                // index 0, so the first row is the selection.
                let text = pick_text_to_yank(&app).expect("a row is selected");
                assert_eq!(text, "echo hello");
        }

        /// `pick_text_to_yank` prefers the output view text over
        /// the selected row's command. This is the "or the
        /// output of this command" branch the user asked for.
        #[test]
        fn pick_text_to_yank_prefers_output_view() {
                let mut app = stats_test_app(&[("cargo test", 30)]);
                // Simulate the output overlay being open with a
                // specific captured text. We use a string that
                // differs from any command in the table so the
                // test catches a mix-up between the two sources.
                let output_text = "test result: ok. 12 passed; 0 failed";
                app.output_view = Some(OutputView {
                        text: output_text.to_string(),
                        scroll: 0,
                });
                let text = pick_text_to_yank(&app).expect("output view is set");
                assert_eq!(text, output_text);
                // Even though there's a selected row, the
                // output view wins.
                assert_ne!(text, "cargo test");
        }

        /// `pick_text_to_yank` returns `None` when there's no
        /// output view and no selected row. The caller surfaces
        /// this as a "Nothing to yank" status message.
        #[test]
        fn pick_text_to_yank_returns_none_when_empty() {
                // Empty history — no rows, no selection.
                let app = stats_test_app(&[]);
                assert!(pick_text_to_yank(&app).is_none());
        }

        /// `App::yank_to_clipboard` with no output view and a
        /// selected row sets a "Yanked N chars" status message
        /// on success. The actual clipboard write goes through
        /// arboard; in CI without a display server it may fail,
        /// so the test accepts either outcome but always
        /// confirms that *some* feedback was surfaced (the
        /// yank never crashes the TUI).
        #[test]
        fn yank_to_clipboard_with_selected_row_sets_status() {
                let mut app = stats_test_app(&[("ls -la", 30)]);
                assert!(app.status_message.is_none());
                app.yank_to_clipboard();
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.clone())
                        .expect("yank must set a status message");
                // On success: "Yanked N chars to clipboard".
                // On failure: "Yank failed: <reason>".
                // Either is acceptable — we just want to confirm
                // the action did not silently no-op.
                assert!(
                        msg.starts_with("Yanked ") || msg.starts_with("Yank failed"),
                        "unexpected status message: {:?}",
                        msg
                );
        }

        /// `yank_to_clipboard` is a no-op with a clear status
        /// message when there's nothing to copy. The clipboard
        /// must never be touched in that case (we'd just be
        /// putting whatever stale data was already on the
        /// clipboard back).
        #[test]
        fn yank_to_clipboard_with_nothing_to_copy() {
                let mut app = stats_test_app(&[]);
                app.yank_to_clipboard();
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("yank must report when there's nothing to copy");
                assert_eq!(msg, "Nothing to yank");
        }

        // --- tokenize_command -------------------------------------------------

        #[test]
        fn tokenize_splits_on_whitespace() {
                assert_eq!(
                        tokenize_command("git log --oneline"),
                        vec!["git", "log", "--oneline"]
                );
        }

        #[test]
        fn tokenize_strips_double_quotes() {
                assert_eq!(
                        tokenize_command("cat \"my file.txt\""),
                        vec!["cat", "my file.txt"]
                );
        }

        #[test]
        fn tokenize_strips_single_quotes() {
                assert_eq!(
                        tokenize_command("vim 'weird name'"),
                        vec!["vim", "weird name"]
                );
        }

        #[test]
        fn tokenize_handles_multiple_spaces_and_tabs() {
                assert_eq!(
                        tokenize_command("  git\tlog  \t  oneline  "),
                        vec!["git", "log", "oneline"]
                );
        }

        #[test]
        fn tokenize_empty_command() {
                assert_eq!(tokenize_command(""), Vec::<String>::new());
                assert_eq!(tokenize_command("   \t  "), Vec::<String>::new());
        }

        // --- find_filename_in_command -----------------------------------------

        #[test]
        fn find_filename_picks_absolute_path() {
                assert_eq!(
                        find_filename_in_command("cat /etc/hosts"),
                        Some("/etc/hosts".to_string())
                );
        }

        #[test]
        fn find_filename_picks_tilde_path() {
                assert_eq!(
                        find_filename_in_command("vim ~/.bashrc"),
                        Some("~/.bashrc".to_string())
                );
        }

        #[test]
        fn find_filename_picks_relative_path() {
                assert_eq!(
                        find_filename_in_command("less ./README.md"),
                        Some("./README.md".to_string())
                );
        }

        #[test]
        fn find_filename_picks_dotdot_path() {
                assert_eq!(
                        find_filename_in_command("vim ../sibling.txt"),
                        Some("../sibling.txt".to_string())
                );
        }

        #[test]
        fn find_filename_picks_subdir_path() {
                // No leading slash, but contains a separator
                // and a dot in the filename. The directory part
                // is `notes`, the file part is `plan.md`.
                assert_eq!(
                        find_filename_in_command("cat notes/plan.md"),
                        Some("notes/plan.md".to_string())
                );
        }

        #[test]
        fn find_filename_picks_bare_filename_with_extension() {
                // No slash, but a dot in the name: README.md
                // invoked from the working directory.
                assert_eq!(
                        find_filename_in_command("code README.md"),
                        Some("README.md".to_string())
                );
        }

        #[test]
        fn find_filename_skips_flags() {
                // `-rf` starts with `-` and is rejected. The
                // path after it still wins.
                assert_eq!(
                        find_filename_in_command("rm -rf /tmp/foo"),
                        Some("/tmp/foo".to_string())
                );
        }

        #[test]
        fn find_filename_skips_glob() {
                // `/tmp/foo*` is a glob, not a file. The TUI
                // should not pick it.
                assert_eq!(
                        find_filename_in_command("rm /tmp/foo*"),
                        None
                );
        }

        #[test]
        fn find_filename_skips_variable_interpolation() {
                // `$HOME` is a shell variable reference, not a
                // literal path. We don't try to resolve it.
                assert_eq!(
                        find_filename_in_command("vim $HOME/.profile"),
                        None
                );
        }

        #[test]
        fn find_filename_skips_command_substitution() {
                // `$(echo foo)` is a subshell expansion, not a
                // path.
                assert_eq!(
                        find_filename_in_command("cat $(echo /etc/hosts)"),
                        None
                );
        }

        #[test]
        fn find_filename_skips_redirect_operator() {
                // The `>` token is a redirect, not a file.
                assert_eq!(
                        find_filename_in_command("echo hello > /tmp/out"),
                        Some("/tmp/out".to_string())
                );
        }

        #[test]
        fn find_filename_handles_lone_dot() {
                // `cd .` — the `.` is the current directory, not
                // a file. The algorithm should not pick it.
                assert_eq!(find_filename_in_command("cd ."), None);
        }

        #[test]
        fn find_filename_handles_lone_dotdot() {
                // `cd ..` — same as above for `..`.
                assert_eq!(find_filename_in_command("cd .."), None);
        }

        #[test]
        fn find_filename_picks_best_among_multiple() {
                // Both `/etc/passwd` and `temp.txt` look like
                // paths. The absolute one scores higher (leading
                // `/` +10 vs leading-with-`.`/extension +5+3)
                // and wins.
                assert_eq!(
                        find_filename_in_command("diff /etc/passwd temp.txt"),
                        Some("/etc/passwd".to_string())
                );
        }

        #[test]
        fn find_filename_returns_none_for_pure_command() {
                // `ls -la` has no path-like token at all.
                assert_eq!(find_filename_in_command("ls -la"), None);
        }

        #[test]
        fn find_filename_handles_quoted_path_with_spaces() {
                // Quoted form is collapsed into one token by the
                // tokenizer, so the score picks it up.
                assert_eq!(
                        find_filename_in_command("cat \"my notes.txt\""),
                        Some("my notes.txt".to_string())
                );
        }

        // --- App::edit_referenced_file end-to-end ------------------------------

        #[test]
        fn edit_referenced_file_stages_editor_command() {
                // Use a row whose command has a clear path.
                // We can't easily inject an arbitrary command
                // through `stats_test_app` (it hard-codes
                // `exit_code = 0`); use a row whose command
                // shape is the only thing we care about.
                let mut app = stats_test_app(&[("vim /etc/hosts", 30)]);
                app.edit_referenced_file();
                // `selection` is the staged editor command.
                // We don't pin the editor (it depends on the
                // host's $EDITOR) so we anchor on the
                // unquoted-path form. The trailing
                // `path-without-quotes` is the contract:
                // `vim /etc/hosts`, not `vim '/etc/hosts'`.
                let sel = app
                        .selection
                        .as_deref()
                        .expect("staged command must be set");
                assert!(
                        sel.ends_with(" /etc/hosts"),
                        "staged command should end with unquoted path, got {:?}",
                        sel
                );
                assert!(
                        !sel.contains('\''),
                        "staged command must not contain shell quotes, got {:?}",
                        sel
                );
                // `pick_mode` is `Run` so the parent shell will
                // execute it.
                assert_eq!(app.pick_mode, Some(PickMode::Run));
        }

        #[test]
        fn edit_referenced_file_with_no_row_is_a_no_op() {
                let mut app = stats_test_app(&[]);
                // Empty history — no row selected.
                app.edit_referenced_file();
                assert!(app.selection.is_none());
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("must surface a status message");
                assert_eq!(msg, "No command selected");
        }

        #[test]
        fn edit_referenced_file_with_no_path_surfaces_message() {
                let mut app = stats_test_app(&[("ls -la", 30)]);
                app.edit_referenced_file();
                assert!(app.selection.is_none());
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("must surface a status message");
                assert!(
                        msg.starts_with("No filename found in:"),
                        "got {:?}",
                        msg
                );
        }

        // --- Action routing ---------------------------------------------------

        #[test]
        fn edit_file_reference_default_key_routes() {
                let bindings = KeyBindings::defaults();
                assert_eq!(
                        format_key_specs(bindings.specs(Action::EditFileReference)),
                        "C-o"
                );
                let evt = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
                assert_eq!(
                        action_for_key(&bindings, &evt),
                        Some(Action::EditFileReference)
                );
        }

        // --- Labeled-only selection bug ---------------------------------------
        //
        // Regression test: when the user navigates down to a row
        // that lives in `self.labeled_rows` but not `self.rows`
        // (i.e. a "very old" entry that's only surfaced because it
        // has a comment), the actions that operate on the
        // selected row used to silently no-op. The cursor stores
        // an index into the *merged* list (rows + labeled_rows),
        // but `selected_row()` was reading from `self.rows` alone.
        // The fix: `selected_row()` looks at the merged list
        // directly, so any index in `self.list_state` resolves
        // to the row the user is actually looking at.
        // --- Labeled-only selection bug ---------------------------------------
        //
        // Regression test: when the user navigates down to a
        // row that lives in `self.labeled_rows` but not
        // `self.rows` (e.g. a "very old" labeled row from a
        // different session), the actions that operate on the
        // selected row used to silently no-op. The cursor
        // stores an index into the *merged* list (rows +
        // labeled_rows), but `selected_row()` was reading from
        // `self.rows` alone. The fix: `selected_row()` looks
        // at the merged list directly, so any index in
        // `self.list_state` resolves to the row the user is
        // actually looking at.
        #[test]
        fn selected_row_finds_labeled_only_rows() {
                // Build a DB with two rows that both match
                // the search query "git": one in the current
                // session (recent) and one in a *different*
                // session (ancient, with a comment). The
                // ancient row matches the query but is
                // excluded by the `Mode::Sess` SQL filter
                // (different session_id). So it appears in
                // `self.labeled_rows` and in `merged_rows`,
                // but NOT in `self.rows` — exactly the shape
                // that triggered the user's bug report.
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
                )
                .expect("create tables");
                let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64;
                conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp)                          VALUES (1, 'git status', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 10],
                )
                .expect("insert recent");
                conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp)                          VALUES (2, 'git pull', '/tmp', 'ancient', 0, ?1)",
                        rusqlite::params![now - 100_000],
                )
                .expect("insert ancient");
                conn.execute(
                        "INSERT INTO command_comments (command, comment)                          VALUES ('git pull', 'remembered for the README example')",
                        [],
                )
                .expect("insert comment");

                // Pin `SMART_HISTORY_SESSION` so the SQL
                // filter consistently excludes the ancient
                // row. `set_var` is `unsafe` in Rust 2024 but
                // safe in practice for tests (single-threaded
                // test runner, restored at the end).
                let prev_session = std::env::var("SMART_HISTORY_SESSION").ok();
                unsafe { std::env::set_var("SMART_HISTORY_SESSION", "current"); }
                let mut app = App::new(
                        conn,
                        Mode::Sess,
                        "git".to_string(),
                        false,
                        ExitFilter::All,
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                );
                app.refresh();
                // Restore the env var as soon as the initial
                // state is built so we don't leak the override
                // into the rest of the test run.
                unsafe {
                        match prev_session {
                                Some(v) => std::env::set_var("SMART_HISTORY_SESSION", v),
                                None => std::env::remove_var("SMART_HISTORY_SESSION"),
                        }
                }
                assert_eq!(app.rows.len(), 1, "primary list excludes the ancient row");
                assert_eq!(app.labeled_rows.len(), 1, "labeled list has the ancient row");

                // Simulate the user pressing Down to move
                // the cursor past the primary list. This is
                // what `move_selection` does when the user
                // navigates through the merged view.
                app.move_selection(1);
                let merged_len = app.merged_rows().len();
                assert!(merged_len >= 2, "merged list should have both rows");
                assert_eq!(
                        app.list_state.selected().unwrap(),
                        merged_len - 1,
                        "cursor should be on the last merged row"
                );
                // The cursor's index is past `self.rows.len()`
                // — this is the position where the bug used
                // to make `selected_row()` return `None`.
                assert!(app.list_state.selected().unwrap() >= app.rows.len());

                // `selected_row()` MUST find the labeled-only
                // row. This is the regression assertion.
                let row = app
                        .selected_row()
                        .expect("selected_row must find the labeled row");
                assert_eq!(row.command, "git pull");
        }

        /// Companion to the test above: when the action is
        /// `Run`, staging a selection from a labeled-only row
        /// works — which is the user-visible symptom the bug
        /// report described ("the command line stays empty").
        #[test]
        fn select_for_run_on_labeled_only_row_stages_command() {
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
                )
                .expect("create tables");
                let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64;
                conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp)                          VALUES (1, 'git status', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 10],
                )
                .expect("insert recent");
                conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp)                          VALUES (2, 'git pull', '/tmp', 'ancient', 0, ?1)",
                        rusqlite::params![now - 100_000],
                )
                .expect("insert ancient");
                conn.execute(
                        "INSERT INTO command_comments (command, comment)                          VALUES ('git pull', 'remembered for the README example')",
                        [],
                )
                .expect("insert comment");

                let prev_session = std::env::var("SMART_HISTORY_SESSION").ok();
                unsafe { std::env::set_var("SMART_HISTORY_SESSION", "current"); }
                let mut app = App::new(
                        conn,
                        Mode::Sess,
                        "git".to_string(),
                        false,
                        ExitFilter::All,
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                );
                app.refresh();
                unsafe {
                        match prev_session {
                                Some(v) => std::env::set_var("SMART_HISTORY_SESSION", v),
                                None => std::env::remove_var("SMART_HISTORY_SESSION"),
                        }
                }
                // Navigate to the labeled-only row.
                app.move_selection(1);
                // The bug: `select_for_run` would leave
                // `self.selection = None` because
                // `self.rows.get(idx)` returned `None`.
                app.select_for_run();
                let staged = app
                        .selection
                        .as_deref()
                        .expect("Run on a labeled-only row must stage its command");
                assert_eq!(staged, "git pull");
                assert_eq!(app.pick_mode, Some(PickMode::Run));
        }
}
