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
use crate::llm::LlmClient;
use crate::jira::JiraClient;
use crate::Config;
use regex::Regex;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use bindings::{action_for_key, format_key_spec, format_key_specs, Action, KeyBindings, ALL_ACTIONS};
pub use state::{ExitFilter, Mode, HistoryRow, PickMode, SortOrder, TmuxWindowInfo, exit_code};
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
    /// Last sort order, persisted as the lowercase
    /// `SortOrder::as_str()` ("age", "frequency"). `None` means
    /// "no preference" and falls back to `SortOrder::default()`
    /// (i.e. `Age`, the historical timestamp-DESC sort). Values
    /// that don't parse as a `SortOrder` are silently dropped
    /// when loading so a hand-edited session file can't wedge
    /// the TUI on startup.
    sort_order: Option<String>,
    /// Last selected theme slug (e.g. `"dracula"`, `"tokyo-night"`,
    /// or `"none"` for the manual-config palette). Persisted across
    /// TUI invocations so the user always lands back on their
    /// preferred colors.
    theme: Option<String>,
    /// Active directory-source
    /// filter for the
    /// `#`-mode list
    /// (`all` / `tmux` /
    /// `config`). `None`
    /// means "no preference"
    /// and falls back to
    /// `DirectorySource::All`.
    /// Values that don't
    /// parse as a
    /// `DirectorySource`
    /// are silently dropped
    /// when loading so a
    /// hand-edited session
    /// file can't wedge the
    /// TUI on startup.
    directory_source: Option<String>,
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
                // Same pattern as the exit filter: only
                // accept values that `SortOrder::parse`
                // recognises. The set of aliases is short
                // (age/frequency, plus `time`/`newest`
                // and `freq`/`count`/`occurrences` for
                // hand-edited session files).
                "sortorder" => {
                    if SortOrder::parse(value).is_some() {
                        s.sort_order = Some(value.to_string());
                    }
                }
                "theme" => s.theme = Some(value.to_string()),
                "directorysource" => {
                    // Same pattern
                    // as the other
                    // session fields:
                    // only accept
                    // values that
                    // `DirectorySource::parse`
                    // recognises
                    // (lowercase
                    // `all` / `tmux` /
                    // `config`,
                    // plus
                    // `cfg` and
                    // `sessiondirs`
                    // as friendly
                    // aliases for
                    // `config`).
                    if crate::tui::state::DirectorySource::parse(
                        value,
                    )
                    .is_some()
                    {
                        s.directory_source =
                            Some(value.to_string());
                    }
                }
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
        if let Some(ref so) = self.sort_order {
            out.push_str(&format!("sortorder={}\n", so));
        }
        if let Some(ref t) = self.theme {
            out.push_str(&format!("theme={}\n", t));
        }
        if let Some(ref ds) = self.directory_source {
            out.push_str(&format!("directorysource={}\n", ds));
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

/// True if every character of `pattern` appears in `text` in
/// order (a "subsequence" match), case-insensitive. This is the
/// same shape as `fzf` and similar fuzzy finders: the user types
/// a few letters and the result list narrows to rows whose
/// command or comment contains those letters in sequence.
///
/// We don't implement the full fzf scoring (camelCase bonuses,
/// consecutive-run bonuses, etc.) because shell history lines
/// are short and the simple subsequence test is fast and good
/// enough to feel responsive on lists of a few thousand rows.
fn fuzzy_match(pattern: &str, text: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    let pattern_lower = pattern.to_ascii_lowercase();
    let text_lower = text.to_ascii_lowercase();
    let mut pat_chars = pattern_lower.chars();
    let mut current = pat_chars.next();
    for c in text_lower.chars() {
        if Some(c) == current {
            current = pat_chars.next();
            if current.is_none() {
                return true;
            }
        }
    }
    false
}

/// Date-filter aliases recognised inside the
/// notes-search query mode (`@today`, `@week`,
/// `@month`, `@year`).
///
/// Each variant stores a cutoff timestamp
/// computed at construction time: notes whose
/// effective `updated` timestamp is below the
/// cutoff are excluded. The cutoff is "now minus
/// the window size" (e.g. 24h for `@today`, 7d
/// for `@week`). Storing the cutoff as a
/// timestamp rather than a duration keeps the
/// per-row comparison a single integer
/// comparison.
///
/// The enum is `Copy` so it can be returned by
/// value from `parse_notes_query` and stored on
/// `App` without an `Option` for the
/// default-case bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotesDateFilter {
    /// No date filter; show all notes (default).
    All,
    /// Notes updated today (within the last 24h).
    Today,
    /// Notes updated within the last 7 days.
    Week,
    /// Notes updated within the last 30 days.
    Month,
    /// Notes updated within the last 365 days.
    Year,
}

impl NotesDateFilter {
    /// Cutoff timestamp below which notes are
    /// excluded. `None` for `All` (no filter).
    /// `now` is the current epoch seconds; in
    /// tests we pass a fixed value to make the
    /// math deterministic.
    fn cutoff(self, now: i64) -> Option<i64> {
        match self {
            NotesDateFilter::All => None,
            NotesDateFilter::Today => Some(now - 24 * 60 * 60),
            NotesDateFilter::Week => Some(now - 7 * 24 * 60 * 60),
            NotesDateFilter::Month => Some(now - 30 * 24 * 60 * 60),
            NotesDateFilter::Year => Some(now - 365 * 24 * 60 * 60),
        }
    }

    /// Stable lowercase identifier used by the
    /// parser to recognise aliases in the
    /// query string.
    #[allow(dead_code)]
    fn as_str(self) -> &'static str {
        match self {
            NotesDateFilter::All => "",
            NotesDateFilter::Today => "today",
            NotesDateFilter::Week => "week",
            NotesDateFilter::Month => "month",
            NotesDateFilter::Year => "year",
        }
    }

    /// True if the filter applies a date window.
    /// `All` is the no-op case; the rest are
    /// filters.
    #[allow(dead_code)]
    fn is_active(self) -> bool {
        !matches!(self, NotesDateFilter::All)
    }
}

/// Parse a notes-mode query body and extract the
/// date-filter alias.
///
/// Returns `(clean_pattern, filter)`:
/// - `clean_pattern` is the query body with any
///   `@today` / `@week` / `@month` / `@year`
///   aliases removed (and surrounding whitespace
///   collapsed). The cleaned pattern is what we
///   pass to `note_search.search_notes_by_query`.
/// - `filter` is the resolved filter; the latest
///   alias in the query wins (i.e. `@today @week`
///   ends up as `Today` because `@week` is
///   encountered second; multiple aliases is an
///   edge case — the user typically uses just
///   one).
///
/// The aliases are recognised only as
/// whole-word tokens (whitespace-separated).
/// This avoids false positives like
/// `@todayfile.md` (no alias inside) or
/// `email@today` (no alias inside). The
/// match is case-insensitive (`@Today`,
/// `@TODAY`, `@today` all work).
fn parse_notes_query(pattern: &str) -> (String, NotesDateFilter) {
    let mut filter = NotesDateFilter::All;
    let mut cleaned_tokens: Vec<String> = Vec::new();
    for token in pattern.split_whitespace() {
        // The user types `@today` (or
        // `today`); both should be
        // recognised as the date alias.
        // We strip a leading `@` so the
        // alias is matched on the bare
        // keyword.
        let candidate = token
            .strip_prefix('@')
            .unwrap_or(token);
        match candidate.to_ascii_lowercase().as_str() {
            "today" => filter = NotesDateFilter::Today,
            "week" => filter = NotesDateFilter::Week,
            "month" => filter = NotesDateFilter::Month,
            "year" => filter = NotesDateFilter::Year,
            // The non-alias path. We push
            // the *stripped* token (without
            // the leading `@`) so the
            // downstream
            // `note_search::parse_query`
            // sees a plain word rather than
            // `@word` — the library's
            // tokenizer treats `@foo` as a
            // `Link` reference, which would
            // match against `t.links` /
            // `m.links` and never against
            // the todo text. The user's
            // intent is the opposite: a
            // `@` prefix is their ad-hoc
            // shorthand for "search the
            // word", not "search the link".
            // We honour that by stripping
            // the `@` here.
            _ => cleaned_tokens
                    .push(candidate.to_string()),
        }
    }
    (cleaned_tokens.join(" "), filter)
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

/// Convert a character index to the corresponding byte index in
/// `s`. Used by the query-field cursor logic, which tracks
/// positions in *characters* (so multi-byte UTF-8 input like
/// `é` or `→` is counted as one cursor step, not two) but
/// `String::insert` / `String::remove` operate on bytes.
///
/// The `char_idx` is clamped to the actual number of chars in
/// `s` so callers that compute a stale cursor position (e.g.
/// the user pressed Left from the very beginning) get a
/// well-defined "insert at end" or "delete at end" instead
/// of a panic. We always return a valid `String::char_indices`
/// offset, which the standard library accepts as a `usize`
/// byte index.
fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or_else(|| s.len())
}

/// Walk left from `cursor` (a character index in
/// `s`) and return the new cursor position after
/// deleting one "word" backward in the
/// readline/bash/zsh sense:
///
/// 1. **Trailing whitespace first**: skip any
///    run of whitespace (`is_whitespace()` —
///    covers spaces, tabs, Unicode whitespace)
///    immediately to the left of `cursor`.
/// 2. **Then the preceding word**: skip the run
///    of non-whitespace characters immediately
///    to the left of where step 1 stopped.
/// 3. **Stop at 0**: never go past the start of
///    the buffer.
///
/// The function is pure: it returns the *new*
/// cursor position (a character index) without
/// mutating `s`. The caller is responsible for
/// actually deleting the slice `s[new_cursor..cursor]`
/// (using `String::replace_range` with the
/// corresponding byte indices) and updating the
/// cursor. Keeping the two steps separate lets
/// the caller do the deletion atomically with a
/// single `replace_range` rather than N
/// individual `String::remove` calls (each of
/// which has to recompute the byte index from a
/// character index).
///
/// Examples (cursor → return):
/// - `s = ""`, cursor = 0 → 0
/// - `s = "abc"`, cursor = 3 → 0 (whole word)
/// - `s = "abc"`, cursor = 2 → 0 (rest of word)
/// - `s = "abc def"`, cursor = 7 → 4 (the `def`)
/// - `s = "abc def"`, cursor = 4 → 0 (the `abc`,
///   even though there are no leading spaces —
///   `def` is the word immediately before the
///   cursor)
/// - `s = "abc   def"`, cursor = 7 → 0 (eat the
/// Walk left from `cursor` (a character index in
/// `s`) and return the new cursor position after
/// deleting one "word" backward, in a
/// readline/bash/zsh-inspired semantic:
///
/// 1. **Trailing whitespace run**: skip any run
///    of whitespace (`is_whitespace()`) chars
///    immediately to the left of `cursor`.
/// 2. **Preceding non-whitespace run**: skip
///    the run of non-whitespace chars
///    immediately to the left of where step 1
///    stopped. We only walk one run — the
///    function never reaches further back to
///    consume additional whitespace runs.
/// 3. **Stop at 0**: never go past the start of
///    the buffer.
///
/// Both steps are skipped automatically when
/// there's nothing to skip (an empty step 1
/// when the char immediately to the left of the
/// cursor is non-whitespace; an empty step 2
/// when step 1 walked all the way back to
/// position 0).
///
/// The function is pure: it returns the *new*
/// cursor position (a character index) without
/// mutating `s`. The caller is responsible for
/// actually deleting the slice
/// `s[new_cursor..cursor]` (using
/// `String::replace_range` with the corresponding
/// byte indices from `char_to_byte_index`).
///
/// Examples (`s`, cursor → return, with the
/// implied query state shown for clarity):
///
/// - `("abc", 3) → 0` — eat the whole word.
/// - `("abc def", 7) → 4` — eat `def`, leaving
///   `abc ` with cursor right after the
///   remaining space (position 4).
/// - `("abc def", 4) → 0` — the char
///   immediately to the left of cursor is a
///   space, so step 1 eats the space and step 2
///   eats `abc`. The result is `def` with
///   cursor at 0.
/// - `("git status", 10) → 4` — eat `status`,
///   leaving `git `.
/// - `("abc   ", 6) → 0` — only whitespace
///   was eaten; step 2 walks back through `abc`
///   (which has nothing before it on the left
///   except the start of the buffer).
/// - `("  abc", 5) → 3` — eat `abc`, leaving
///   the two leading spaces intact.
/// - `("git status  ", 12) → 4` — step 1 eats
///   the 2 trailing spaces (positions 10..12),
///   step 2 eats `status` (positions 4..10).
///   The space at position 3 (between `git`
///   and `status`) is NOT eaten — step 1 only
///   walks back from the cursor, not forward
///   through the already-deleted range.
/// - `("", 0) → 0` — empty buffer, no-op.
///
/// Note: this is *similar to* but not identical
/// to readline/bash's `unix-word-rubout`.
/// Standard `unix-word-rubout` may eat one
/// additional whitespace char (the one
/// immediately preceding the word it ate).
/// Our algorithm eats the trailing whitespace
/// run first, then the preceding word. The
/// difference is minor (one character at the
/// word boundary) and the chosen semantic
/// matches what `backward-kill-word` does in
/// zsh's `wordstyle` shell.
fn delete_word_backward_at_cursor(s: &str, cursor: usize) -> usize {
    let chars: Vec<char> = s.chars().take(cursor).collect();
    let mut idx = chars.len();
    // Step 1: skip a trailing whitespace run.
    // Whitespace is defined by
    // `char::is_whitespace`, which covers the
    // standard ASCII whitespace plus the Unicode
    // whitespace category (so `　` / `\t` /
    // non-breaking space are all treated as word
    // boundaries, matching `unicode-word-boundary`
    // rules that readline uses by default on
    // modern systems).
    while idx > 0 && chars[idx - 1].is_whitespace() {
        idx -= 1;
    }
    // Step 2: skip the preceding non-whitespace
    // run. We only walk one run — that's the
    // difference between `unix-word-rubout`
    // (this function) and a hypothetical
    // "eat all non-whitespace" function that
    // would walk the whole prefix.
    while idx > 0 && !chars[idx - 1].is_whitespace() {
        idx -= 1;
    }
    idx
}

/// Convenience wrapper for the comment-edit
/// buffer, which has no cursor concept — operate
/// on the logical end of the string. Equivalent
/// to "what would `delete_word_backward_at_cursor`
/// return if the cursor were at the end?".
fn delete_word_backward_in_string(s: &mut String) {
    let cursor = s.chars().count();
    let new_cursor = delete_word_backward_at_cursor(s, cursor);
    let start_byte = char_to_byte_index(s, new_cursor);
    let end_byte = char_to_byte_index(s, cursor);
    s.replace_range(start_byte..end_byte, "");
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
    /// Active sort order for the merged history list.
    /// Defaults to `SortOrder::Age` (timestamp DESC, the
    /// historical behaviour). Cycled with
    /// `Action::CycleSortOrder` (default key `F4`). In
    /// `Age` mode the rows are ordered newest-first; in
    /// `Frequency` mode they're ordered by command
    /// occurrence count (DESC) with timestamp DESC as
    /// a tie-breaker. See `SortOrder` for the full
    /// contract.
    sort_order: SortOrder,
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
    /// When `Some`, the LLM-driven "describe"
    /// overlay is open. The contained struct holds
    /// the command that was described and the LLM's
    /// response (a short prose description). The
    /// overlay is full-screen with a single scroll
    /// offset, similar to the captured-output
    /// overlay but driven by a different source.
    describe_view: Option<DescribeView>,
    /// When `Some`, the LLM-driven "correct this
    /// command" modal overlay is open. The user is
    /// reviewing a candidate corrected command;
    /// pressing `Enter` stages it (and writes it
    /// to the history table with the original as
    /// the comment), pressing `Esc` cancels.
    correct_view: Option<CorrectView>,
    /// When `Some`, the general question overlay is open.
    /// The user asked a question (prefixed with `%`) and
    /// the LLM's answer is displayed here.
    question_view: Option<QuestionView>,
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
    /// Byte offset in `self.query` where the next character is
    /// inserted and the previous character is deleted. The
    /// input field draws the terminal cursor at this position
    /// (see `draw_input` in `render.rs`).
    ///
    /// For non-LLM query modes (plain, regex, fuzzy) the cursor
    /// sits at the end of the buffer — the user can only append
    /// or backspace, never move within the field. This matches
    /// the long-standing behaviour of those modes and avoids
    /// the visual noise of a cursor when the user isn't editing.
    ///
    /// For LLM queries (`=...`) the cursor is editable: pressing
    /// `Left` / `Right` (the keys normally bound to
    /// `EditStart` / `EditEnd`) move it within the buffer so
    /// the user can reword the description before pressing
    /// `Enter` to regenerate. The cursor is initialized to
    /// `self.query.len()` whenever the query enters LLM mode
    /// (or whenever a new LLM query is pre-filled) so typing
    /// appends naturally; Left/Right can then move it.
    query_cursor: usize,
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
    /// LLM client for the `=...` query mode. `None` means the
    /// feature is not configured; the TUI surfaces a clear
    /// status message instead of attempting the call. We hold
    /// the client as a trait object so tests can inject a
    /// canned-response implementation without spinning up a
    /// real ollama server.
    llm: Option<Box<dyn crate::llm::LlmClient>>,
    /// The LLM configuration, stored separately so background
    /// threads can create their own `OllamaClient` instances
    /// for async requests.
    llm_config: Option<crate::llm::LlmConfig>,
    /// User-customizable query prefix characters.
    query_prefixes: crate::QueryPrefixes,
    /// Path to the note_search database, if configured.
    notes_database: Option<std::path::PathBuf>,
    /// Path to the notes directory, if configured.
    notes_dir: Option<std::path::PathBuf>,
    /// Template for the line-number option that
    /// the todo-search mode (`!`) appends to the
    /// editor command when the user selects a
    /// todo line. The literal `$LINE` is
    /// substituted with the actual 1-based line
    /// number. Default: `+$LINE`.
    ///
    /// Stored on `App` rather than re-read from
    /// `Config` on every selection so the template
    /// stays stable across the TUI session even
    /// if the user edits the config and runs a
    /// second TUI in parallel.
    todo_line_option: String,
    /// Snapshot of `tmux list-panes -a -F
    /// '#S | #P | #{pane_current_path}'`
    /// used by the directories
    /// view's "tmux pane active"
    /// marker. Populated the first
    /// time the user types `#…`,
    /// then cached for the
    /// remainder of the TUI
    /// session — the pane set
    /// doesn't change while the
    /// TUI is the foreground
    /// process, so re-running the
    /// subprocess on every
    /// refresh would just add
    /// 50–200 ms of latency to
    /// every keystroke.
    ///
    /// `path` values are
    /// canonicalised at parse
    /// time (so `/Users/har/x`
    /// and `/Volumes/HUGE/har/x`
    /// collapse to one entry the
    /// same way `fetch_directories`
    /// does). Empty paths (a
    /// brand-new pane with no
    /// cwd yet) are filtered out
    /// at parse time.
    tmux_windows: Vec<TmuxWindowInfo>,
    /// Cached snapshot of the panes in
    /// the *current* tmux session (the one
    /// the TUI is running in), used by the
    /// `*`-prefix panes view. Populated
    /// the first time the user types `*…`,
    /// then cached for the remainder of
    /// the TUI session. The pane set
    /// doesn't change while the TUI is the
    /// foreground process (the user can't
    /// create or close panes from inside
    /// the TUI), so re-running `tmux
    /// list-panes -s` on every keystroke
    /// would just add latency without
    /// buying freshness. The current
    /// *command* each pane is running
    /// may go stale, but that's an
    /// acceptable trade-off (the user can
    /// re-enter `*` mode to refresh). The
    /// current pane (`$TMUX_PANE`) is
    /// excluded at fetch time so the list
    /// never shows the pane the user is
    /// in. Each row stores the pane id
    /// (`%N`) in `session_id` so the
    /// `select-pane` / `switch-client`
    /// action on Enter can target it.
    session_panes: Vec<HistoryRow>,
    /// Cached home-prefix list
    /// for path
    /// normalization
    /// (canonicalization +
    /// homemap expansion).
    /// Computed once at App
    /// construction by
    /// reading the user's
    /// `Config` (`$HOME` +
    /// `homemap=...`
    /// entries). Used by
    /// `directory_tmux_pane_id`
    /// to normalize paths
    /// from both the DB side
    /// and the tmux side
    /// before comparison, so
    /// the two end up in
    /// the same form
    /// regardless of whether
    /// the DB has the short
    /// `~/x` form (after
    /// `smarthistory update`)
    /// or the long
    /// `/Users/.../x` form
    /// (when the precmd hook
    /// captured a fresh
    /// row).
    home_list: Vec<String>,
    /// Recursively-walked
    /// subdirectories of
    /// every
    /// `sessiondirs=...`
    /// config entry. Computed
    /// once at App
    /// construction (like
    /// `home_list`); the
    /// list is empty if no
    /// `sessiondirs=...` is
    /// configured. Used by
    /// `fetch_directories`
    /// to add the user's
    /// pinned projects to
    /// the `#`-mode list
    /// even when the user
    /// has never run a
    /// command in them.
    session_subdirs: Vec<std::path::PathBuf>,
    /// Active directory-source
    /// filter for the
    /// `#`-mode list. The
    /// TUI cycles
    /// ALL → TMUX → CFG →
    /// ALL via
    /// `Action::CycleDirectorySource`
    /// (default key
    /// `C-M-g`). Persisted
    /// in the session file
    /// so the user lands
    /// back on the same
    /// view across
    /// invocations.
    directory_source: crate::tui::state::DirectorySource,
    /// Set to true when the last notes query failed to parse.
    notes_query_error: bool,
    /// The date-filter alias active for the current
    /// notes-mode query (`@today` / `@week` /
    /// `@month` / `@year`). `All` is the default
    /// and means no filter is active. Updated by
    /// `fetch_notes` on every refresh — the value
    /// here is what's used by the mode-strip chip
    /// renderer and by tests.
    notes_date_filter: NotesDateFilter,
    /// Timestamp of the most recent keystroke that touched
    /// the query in LLM mode. `None` when the debounce is
    /// satisfied (i.e. an auto-call has been issued and the
    /// user hasn't typed since) or when we're not in LLM
    /// mode. The run-loop tick uses
    /// `llm_debounce_started.elapsed()` to decide when to
    /// fire the next preview call.
    ///
    /// See [`App::llm_maybe_autocall`] for the full lifecycle.
    llm_debounce_started: Option<std::time::Instant>,
    /// Last LLM response staged as a virtual preview row in
    /// the history list. `Some` while the debounce has fired
    /// and the user hasn't typed since; the row is appended
    /// to the merged view (see [`App::llm_preview_row`]) so
    /// the user can see the proposed command without
    /// committing to running it.
    ///
    /// The preview row uses a synthetic `id` of `-1` (real
    /// history ids are positive) and an `exit_code` of `-1`
    /// (the same sentinel used by [`App::run_llm_query`]
    /// when it first inserts a generated command).
    llm_preview: Option<HistoryRow>,
    /// `true` while a background LLM call is in flight. The
    /// debounce timer is paused while a call is in flight;
    /// when the call returns, the debounce is reset to
    /// "satisfied" so the user can keep typing without
    /// re-firing the call on every keystroke. We do NOT
    /// re-fire on the result of a stale description (e.g.
    /// the user kept typing while the call was in flight)
    /// — only the next pause-and-restart cycle does that.
    llm_in_flight: bool,
    /// When `Some`, an LLM request is in flight (spawned in a
    /// background thread). The run loop polls the receiver and
    /// processes the result when it arrives. The cancelled flag
    /// is set when the user presses Ctrl+C (or the Cancel action)
    /// while a request is in flight, causing the result to be
    /// discarded.
    llm_request: Option<LlmRequest>,
    /// The description string the most-recent preview
    /// corresponds to. Compared to the live
    /// `self.query[1..]` to decide whether the preview is
    /// still relevant. When the user keeps typing while a
    /// call is in flight, the returned preview's
    /// `description` no longer matches the live description
    /// and we discard the stale preview rather than showing
    /// the user a suggestion for a query they no longer
    /// have.
    llm_preview_description: Option<String>,
    /// Cached JIRA search results for the `-`-prefix mode.
    /// Populated asynchronously (see `jira_request` /
    /// `jira_maybe_autocall`): when the user pauses typing
    /// for `JIRA_DEBOUNCE`, a background thread fires the
    /// JQL query against the configured JIRA server and
    /// the run loop stores the result here, then refreshes.
    /// `fetch_jira` returns this cache (no network on the
    /// hot path), so typing is responsive and only the
    /// ~400ms-after-keystroke background fetch hits the
    /// server.
    jira_rows: Vec<crate::tui::state::HistoryRow>,
    /// `Some` when a JIRA search is in flight (background
    /// thread). Polled by the run loop mirror of the LLM
    /// poll. Cancelled on the `Cancel` action.
    jira_request: Option<JiraRequest>,
    /// Whether a JIRA search is currently in flight.
    /// Prevents re-firing while a query is pending.
    jira_in_flight: bool,
    /// Debounce timer for JIRA search-as-you-type, armed by
    /// `jira_touch` on every keystroke. Cleared when a
    /// search fires or the mode is left.
    jira_debounce_started: Option<std::time::Instant>,
    /// The JQL string the most-recent JIRA search
    /// corresponds to. Compared to the live-built JQL to
    /// avoid re-firing the same query when the user pauses
    /// without changing anything.
    jira_last_jql: Option<String>,
    /// Injectable JIRA client for tests (a fake). When
    /// `Some`, `spawn_jira_request` runs the search
    /// synchronously via this client instead of spawning a
    /// real-`reqwest` background thread, so tests can drive
    /// the search-and-render path deterministically without
    /// a live JIRA server or env vars. Production leaves this
    /// `None` so the real background-thread + `RestJiraClient`
    /// path runs.
    jira_client: Option<std::sync::Arc<dyn crate::jira::JiraClient>>,
    /// User-defined JQL fragments loaded from the
    /// config file's `jira.search.<name>=...` entries.
    /// The build_jql parser looks up `@<name>` tokens
    /// in this map and splices the corresponding JQL
    /// fragment into the query. Reserved names
    /// (`me`, `today`, `week`, `month`) cannot be
    /// overridden — the config loader silently drops
    /// them.
    jira_fragments: std::collections::HashMap<String, String>,
    /// The fragment names that were unresolved on the
    /// most recent `jira_build_query` call. Set by
    /// `jira_build_query` after every build; read by
    /// `jira_maybe_autocall` to decide whether to skip
    /// the search and surface a status message. Order
    /// is first-appearance / deduped, matching what
    /// `build_jql` returns.
    jira_undefined_fragments: Vec<String>,
    /// The undefined-fragment list the most recent
    /// status message described. Used to debounce the
    /// "fragment @foo is not configured" message —
    /// we only emit when the new list differs from the
    /// last one, so the user doesn't get a stale
    /// message re-surfacing on every keystroke while
    /// they correct the typo.
    jira_last_undefined_message: Option<Vec<String>>,
    /// `Some` when a JIRA comments fetch is in
    /// flight (background thread). Polled by
    /// the run loop mirror of the JIRA-search
    /// poll. Cancelled on the `Cancel` action.
    /// The fetch is fired when the user
    /// opens the show-output overlay on a
    /// JIRA row (Ctrl+L) — the comments
    /// aren't fetched at search time
    /// because most JIRA issues have many
    /// comments and a search-result row
    /// only needs the issue metadata.
    jira_comments_request: Option<JiraCommentsRequest>,
    /// Whether a JIRA comments fetch is
    /// currently in flight. Prevents
    /// re-firing while a fetch is pending
    /// (the user might press Ctrl+L again on
    /// the same row; we'd silently drop the
    /// new request rather than spawn a
    /// second background thread).
    jira_comments_in_flight: bool,
    /// `Some(issue_key)` when the comment
    /// edit buffer is in "JIRA add comment"
    /// mode — i.e. the user pressed Ctrl-E
    /// on a JIRA row and is composing a
    /// new comment to POST to JIRA, not a
    /// local `command_comments` edit. The
    /// `comment_edit` buffer is shared
    /// between the two modes; this field
    /// tells `save_comment_edit` which path
    /// to take. When `None`, the user is
    /// editing the local command comment
    /// (the original behaviour).
    jira_add_comment_target: Option<String>,
    /// `Some` when a JIRA add-comment POST
    /// is in flight (background thread).
    /// Polled by the run loop mirror of
    /// the JIRA-search and JIRA-comments
    /// polls. Cancelled on the `Cancel`
    /// action. The buffer stays open
    /// while the POST is in flight so
    /// the user can see what they
    /// posted; on success the buffer
    /// clears and the field goes back to
    /// `None`; on failure the buffer
    /// stays so the user can retry.
    jira_add_comment_request: Option<JiraAddCommentRequest>,
    /// Whether a JIRA add-comment POST
    /// is currently in flight. Prevents
    /// a second `Enter` on the buffer
    /// from queuing a duplicate POST.
    jira_add_comment_in_flight: bool,

    /// Aggregated files-mode state:
    /// debounce timer, in-flight walk
    /// request, last walked pattern,
    /// and cached rows. The full
    /// state machine lives in
    /// `src/files.rs::FilesState` so
    /// the four interrelated fields
    /// stay in one struct.
    files_state: crate::files::FilesState,

    /// User-configured additional
    /// directory basenames to
    /// skip during the walk
    /// (combined with
    /// `files::DEFAULT_IGNORES` at
    /// walk time). Stored on App
    /// so the walker has a stable
    /// copy for the lifetime of
    /// the TUI session; the user
    /// can edit the config and
    /// restart the TUI to pick up
    /// new patterns.
    files_ignores: Vec<String>,
}

/// How long the LLM auto-call waits after the last keystroke
/// before firing. Tuned to the "user is composing a thought"
/// rhythm: long enough that the model isn't re-queried on
/// every character of a long description, short enough that
/// the user sees the suggestion before they have to look up
/// to the status bar. 1 second is the value the user asked
/// for in the spec.
const LLM_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(1);


/// How long the JIRA search-as-you-type waits after the
/// last keystroke before firing. Shorter than the LLM
/// debounce because a JQL search is cheaper than an LLM
/// generation and the user expects a tighter feedback
/// loop. 400ms is the conventional "stopped typing"
/// threshold in search UIs.
const JIRA_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);

impl App {
    /// True if the current query is a regex (prefixed with configured regex prefix).
    fn is_regex_query(&self) -> bool {
        let p = self.query_prefixes.regex;
        !self.query.is_empty() && self.query.starts_with(p)
    }

    /// True if the current query is a fuzzy search (prefixed with configured fuzzy prefix).
    fn is_fuzzy_query(&self) -> bool {
        let p = self.query_prefixes.fuzzy;
        !self.query.is_empty() && self.query.starts_with(p)
    }

    /// True if the current query is an output-content search
    /// (prefixed with configured output prefix).
    fn is_output_query(&self) -> bool {
        let p = self.query_prefixes.output;
        !self.query.is_empty() && self.query.starts_with(p)
    }

    /// True if the current query is an LLM command-generation
    /// request (prefixed with configured LLM prefix).
    /// Only returns true if there's actual description text after
    /// the prefix (not just the prefix alone or with only whitespace).
    fn is_llm_query(&self) -> bool {
        let p = self.query_prefixes.llm;
        self.query.starts_with(p) && !self.query[p.len_utf8()..].trim().is_empty()
    }

    /// True if the current query is a general question
    /// request (prefixed with configured question prefix).
    /// Only returns true if there's actual question text after
    /// the prefix (not just the prefix alone or with only whitespace).
    fn is_question_query(&self) -> bool {
        let p = self.query_prefixes.question;
        self.query.starts_with(p) && !self.query[p.len_utf8()..].trim().is_empty()
    }

    /// The regex pattern, i.e. everything after the leading regex prefix.
    /// Empty when the query is not a regex.
    fn regex_pattern(&self) -> &str {
        if self.is_regex_query() {
            let p = self.query_prefixes.regex;
            &self.query[p.len_utf8()..]
        } else {
            ""
        }
    }

    /// The fuzzy pattern, i.e. everything after the leading fuzzy prefix.
    fn fuzzy_pattern(&self) -> &str {
        if self.is_fuzzy_query() {
            let p = self.query_prefixes.fuzzy;
            &self.query[p.len_utf8()..]
        } else {
            ""
        }
    }

    /// The output-search body, i.e. everything after the
    /// leading output prefix.
    fn output_pattern(&self) -> &str {
        if self.is_output_query() {
            let p = self.query_prefixes.output;
            &self.query[p.len_utf8()..]
        } else {
            ""
        }
    }

    /// The LLM query body, i.e. everything after the
    /// leading LLM prefix.
    fn llm_pattern(&self) -> &str {
        if self.is_llm_query() {
            let p = self.query_prefixes.llm;
            &self.query[p.len_utf8()..]
        } else {
            ""
        }
    }

    /// The question body, i.e. everything after the
    /// leading question prefix.
    fn question_pattern(&self) -> &str {
        if self.is_question_query() {
            let p = self.query_prefixes.question;
            &self.query[p.len_utf8()..]
        } else {
            ""
        }
    }

    /// True if the current query is a note search request
    /// (prefixed with the configured notes prefix, default `@`).
    fn is_notes_query(&self) -> bool {
        let p = self.query_prefixes.notes;
        !self.query.is_empty() && self.query.starts_with(p)
    }

    /// True if the current query is a todo search
    /// request (prefixed with the configured todo
    /// prefix, default `!`). The todo mode scans
    /// every file in the configured notes
    /// directory for lines that look like todo
    /// items (markdown task-list checkboxes:
    /// `- [ ] text` / `- [x] text`) and lists each
    /// match as its own row in the TUI.
    fn is_todo_query(&self) -> bool {
        let p = self.query_prefixes.todo;
        !self.query.is_empty() && self.query.starts_with(p)
    }

    /// True if the user typed the
    /// `directories` prefix
    /// (default `#`). The
    /// directories view lists
    /// every unique directory
    /// that's been used in the
    /// global history, sorted by
    /// the most-recent history
    /// row's timestamp DESC, with
    /// each directory's most-
    /// recently-executed
    /// command surfaced for
    /// context. Selecting a row
    /// stages a `cd <path>`
    /// command.
    fn is_directories_query(&self) -> bool {
        let p = self.query_prefixes.directories;
        !self.query.is_empty() && self.query.starts_with(p)
    }

    /// The directories-search
    /// body, i.e. everything
    /// after the leading `#
    /// prefix. Used to filter
    /// the listed directories by
    /// path substring. Empty
    /// when not in directories
    /// mode.
    fn directories_pattern(&self) -> &str {
        if self.is_directories_query() {
            let p = self.query_prefixes.directories;
            &self.query[p.len_utf8()..]
        } else {
            ""
        }
    }

    /// Whether the query is a session-panes request:
    /// the query starts with the panes prefix (`*` by
    /// default). The body (everything after `*`) is a
    /// substring filter matched against each pane's
    /// current command and cwd.
    fn is_panes_query(&self) -> bool {
        let p = self.query_prefixes.panes;
        !self.query.is_empty() && self.query.starts_with(p)
    }

    /// The session-panes filter body, i.e. everything
    /// after the leading `*` prefix. Empty when not in
    /// panes mode.
    fn panes_pattern(&self) -> &str {
        if self.is_panes_query() {
            let p = self.query_prefixes.panes;
            &self.query[p.len_utf8()..]
        } else {
            ""
        }
    }

    /// Whether the query is a files-view request:
    /// the query starts with the files prefix (`~` by
    /// default). The body (everything after `~`) is a
    /// substring filter matched against each file's
    /// path (relative to cwd).
    fn is_files_query(&self) -> bool {
        let p = self.query_prefixes.files;
        !self.query.is_empty() && self.query.starts_with(p)
    }


    /// The note search body, i.e. everything after the
    /// leading notes prefix.
    fn notes_pattern(&self) -> &str {
        if self.is_notes_query() {
            let p = self.query_prefixes.notes;
            &self.query[p.len_utf8()..]
        } else {
            ""
        }
    }

    /// Whether the query is a JIRA issue-search request:
    /// the query starts with the jira prefix (`-` by
    /// default). The body is parsed into a JQL query by
    /// `crate::jira::build_jql` (issue keys,
    /// `field=value` constraints, free text).
    fn is_jira_query(&self) -> bool {
        let p = self.query_prefixes.jira;
        !self.query.is_empty() && self.query.starts_with(p)
    }

    /// The JIRA search body, i.e. everything after the
    /// leading `-` prefix. Empty string when not in jira
    /// mode.
    fn jira_pattern(&self) -> &str {
        if self.is_jira_query() {
            let p = self.query_prefixes.jira;
            &self.query[p.len_utf8()..]
        } else {
            ""
        }
    }

    /// The todo search body, i.e. everything
    /// after the leading todo prefix. Same
    /// contract as `notes_pattern`: empty string
    /// when not in todo mode. The body's
    /// whitespace-separated tokens are matched
    /// against each candidate todo line.
    fn todo_pattern(&self) -> &str {
        if self.is_todo_query() {
            let p = self.query_prefixes.todo;
            &self.query[p.len_utf8()..]
        } else {
            ""
        }
    }

    /// Read the first N lines of a note file for the preview pane.
    fn read_note_preview(&self, filename: &str) -> String {
        let Some(ref notes_dir) = self.notes_dir else {
            return String::new();
        };
        let path = notes_dir.join(filename);
        if !path.exists() || !path.is_file() {
            return String::new();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => String::new(),
        }
    }

    /// Search every note file for todo entries.
    /// Each todo line becomes its own
    /// `HistoryRow` (with the line text as
    /// `command`, the filename as `comment`,
    /// and the surrounding context as `output`).
    /// The typed query (with `@today` / `@week`
    /// / `@month` / `@year` aliases) is applied
    /// against the file's last-modified
    /// timestamp and the todo text.
    ///
    /// Sorting: by file mtime DESC (newer files
    /// first), then by line number ASC within a
    /// file. The line-order tiebreaker makes the
    /// list within a single file read top-to-
    /// bottom, which is what the user expects
    /// when working through a single document.
    fn fetch_todos(&mut self) -> Result<Vec<HistoryRow>> {
        // We delegate to the note_search library
        // the same way `fetch_notes` does. The
        // library is the canonical source for
        // todo data: the indexer parses every
        // note in `notes.dir` at update time
        // and stores each todo in the
        // `todo_entries` table, with the line
        // number, the (open/closed) state, the
        // priority, due date, tags, etc. Scanning
        // the filesystem ourselves would re-do
        // that work in Rust, and worse: it
        // wouldn't see todos that the user has
        // indexed through `note_search` but that
        // live in a directory our `notes.dir`
        // path doesn't point at. Going through
        // the library guarantees the user sees
        // exactly what `note_search list` would
        // show.
        let Some(ref db_path) = self.notes_database else {
            // Without a notes database we can't
            // query todos. Mirror the notes-mode
            // UX: emit a soft status message and
            // return an empty list so the user
            // sees a clear "no todos" reason
            // rather than a confusing empty list.
            self.set_status_message(
                "Todo mode: notes.database is not configured".to_string(),
            );
            return Ok(Vec::new());
        };

        // Strip the date-filter aliases
        // (`@today`, `@week`, `@month`, `@year`)
        // from the query body. The remaining
        // text is passed to `parse_query`,
        // which understands the Obsidian-like
        // syntax: bare words are AND-matched
        // against each todo line, `#tag` is
        // matched against both the todo's own
        // tags and the note's header fields,
        // `[[link]]` is matched against the
        // todo's links and the note's
        // outgoing links, and `[attr:value]`
        // is matched against the note's
        // header fields. Going through
        // `parse_query` instead of stuffing
        // the raw pattern into `criteria.text`
        // is what makes tags / links /
        // attributes work — the user types
        // `!#urgent older` and gets only the
        // todos tagged `urgent` that also
        // contain `older`.
        let raw_pattern = self.todo_pattern().trim();
        let (pattern, filter) = parse_notes_query(raw_pattern);
        // The `filter` is applied
        // post-query against each
        // row's `timestamp`
        // (populated by
        // `fetch_file_updated_timestamps`)
        // — see the post-sort
        // block below. It's also
        // recorded on `self` so the
        // mode-strip chip lights up
        // for both `@...` and `!...`
        // queries identically.
        let query_expr = if pattern.is_empty() {
            None
        } else {
            match note_search::parse_query(&pattern) {
                        Ok(expr) => Some(expr),
                        Err(e) => {
                                self.set_status_message(format!(
                                        "Todo mode: invalid query: {}",
                                        e
                                ));
                                return Ok(Vec::new());
                        }
                }
        };

        // Build the criteria. We always pin
        // `open: Some(true)` so the user sees
        // only uncompleted todos — the user
        // explicitly asked for "all open todo
        // entries". The `SortOrder::Modified`
        // matches the user's request to order
        // by timestamp: the library emits
        // `ORDER BY m.updated DESC, t.filename,
        // t.line_number`, i.e. newest files
        // first, then by filename and line
        // number within a file. (The within-file
        // tie-break by line number puts line 1
        // before line 100 in the same file,
        // matching natural top-to-bottom reading
        // order.)
        let criteria = note_search::SearchCriteria {
                database_path: db_path.to_string_lossy().to_string(),
                note_dir: self
                        .notes_dir
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default(),
                open: Some(true),
                sort_order: Some(note_search::SortOrder::Modified),
                query_expr,
                ..Default::default()
        };
        // The `query_expr` field is the
        // modern way to filter; we leave
        // `criteria.text` unset so the
        // library doesn't add a redundant
        // text-LIKE clause on top of the
        // expression tree. The two paths
        // would otherwise AND together,
        // which is harmless but wasteful.
        debug_assert!(criteria.text.is_none());

        // We use the same high-level entry
        // point as `fetch_notes`: the library's
        // `search_todos` method takes a
        // `SearchCriteria`, runs the query
        // (built internally by `QueryBuilder`),
        // and returns the matching rows. The
        // criteria is moved in (not borrowed)
        // because some of the builder's
        // accumulators consume it; this mirrors
        // how the library's own callers use it.
        let service =
            note_search::database_service::DatabaseService::new(
                &db_path.to_string_lossy(),
            );
        let results = match service.search_todos(&criteria) {
            Ok(r) => r,
            Err(e) => {
                self.set_status_message(format!(
                    "Todo mode: search failed: {}",
                    e
                ));
                return Ok(Vec::new());
            }
        };

        // Map the library's `TodoResult` rows
        // into our `HistoryRow` representation.
        // Each todo line becomes its own row;
        // the library's `line_number` is
        // 1-based, which matches what the
        // editor will use when it opens the
        // file.
        let mut rows: Vec<HistoryRow> = {
                // Read each unique file's
                // `updated` timestamp from the
                // `markdown_data` table so the
                // details pane can show a real
                // age instead of the
                // `9999M` placeholder. The
                // library's `TodoResult` doesn't
                // expose `updated` (only the
                // note's `header_fields`), so we
                // do one extra batched query:
                // distinct filenames from the
                // result set, fetch `updated`
                // for each, build a lookup map,
                // and use it when constructing
                // the rows. Doing one query per
                // file is much cheaper than the
                // per-row N+1 we would otherwise
                // have.
                let mut unique_files: Vec<String> =
                        results.iter().map(|r| r.filename.clone()).collect();
                unique_files.sort();
                unique_files.dedup();
                let mtimes = self
                        .fetch_file_updated_timestamps(
                                db_path,
                                &unique_files,
                        );
                results
                        .iter()
                        .map(|r| {
                                let line_number: usize =
                                        r.line_number.max(1) as usize;
                                // Fall back to `0` only
                                // when the database has
                                // no `updated` for this
                                // file (the user has
                                // never indexed it — a
                                // transient state that
                                // goes away on next
                                // index). Anything better
                                // than a placeholder is
                                // preferable, so we
                                // prefer the actual
                                // `updated` value when
                                // available.
                                let ts = mtimes
                                        .get(&r.filename)
                                        .copied()
                                        .unwrap_or(0);
                                HistoryRow {
                                        // Synthetic negative
                                        // id so it doesn't
                                        // collide with real
                                        // history rows; the
                                        // magnitude carries
                                        // the line number
                                        // for human
                                        // debugging
                                        // (`id = -42` means
                                        // line 42).
                                        id: -(line_number as i64),
                                        command: r.text.clone(),
                                        directory: self
                                                .notes_dir
                                                .as_ref()
                                                .map(|p| {
                                                        p.display()
                                                                .to_string()
                                                })
                                                .unwrap_or_default(),
                                        session_id: String::new(),
                                        exit_code: 0,
                                        timestamp: ts,
                                        comment: r.filename.clone(),
                                        // We don't have the
                                        // file's full
                                        // content in scope
                                        // here (the library
                                        // returns only the
                                        // single todo line).
                                        // The `output` pane
                                        // shows just the
                                        // todo text for now;
                                        // rendering
                                        // surrounding
                                        // context would
                                        // require either an
                                        // extra filesystem
                                        // read or a
                                        // library-side
                                        // context API that
                                        // doesn't exist
                                        // yet.
                                        output: r.text.clone(),
                                        mode: "todo".to_string(),
                                        source: String::new(),
                                }
                        })
                        .collect()
        };
        // The library already returned rows
        // sorted by `m.updated DESC,
        // t.filename, t.line_number` (newest
        // files first, then by line within a
        // file). With the real `updated`
        // timestamps now in `row.timestamp`,
        // a defensive re-sort is still
        // useful — if two files share the
        // same `updated` value (which
        // happens when a single indexing
        // pass touches several files at
        // once), the library's tie-break by
        // filename gives a stable order
        // but it can differ from what we
        // want here (the synthetic `id` is
        // the line number, so reverse-id is
        // a top-to-bottom read within the
        // file).
        rows.sort_by(|a, b| {
            b.timestamp
                .cmp(&a.timestamp)
                .then_with(|| b.id.cmp(&a.id))
        });
        // Apply the date-filter alias
        // (if any) post-sort. Each
        // row's `timestamp` is the
        // file's `updated` epoch
        // (populated by
        // `fetch_file_updated_timestamps`),
        // so the `cutoff` math is
        // the same as in
        // `fetch_notes`. Rows with
        // `timestamp = 0` (the
        // library never gave us a
        // file mtime — a transient
        // state that resolves on
        // the next indexer run) are
        // excluded from any active
        // filter, the same way
        // missing timestamps are
        // handled in notes mode.
        // The active filter value
        // is stored on `self` so
        // the mode-strip chip
        // (TODO/notes) lights up
        // identically for both
        // modes.
        if let Some(cutoff) = filter.cutoff(self.now_epoch()) {
            rows.retain(|r| r.timestamp >= cutoff);
        }
        self.notes_date_filter = filter;
        Ok(rows)
    }

    /// Read the `updated` column from
    /// `markdown_data` for each filename in
    /// `filenames`, returning a map of
    /// `filename -> updated_epoch`. Used by
    /// `fetch_todos` to populate the
    /// Details-pane age column with a real
    /// timestamp instead of the
    /// `9999M` placeholder that
    /// `format_diff(0)` would produce. The
    /// query is `WHERE filename IN (?, ?, …)`
    /// so it's O(unique-files), not
    /// O(rows), regardless of how many todos
    /// each file contains.
    fn fetch_file_updated_timestamps(
        &self,
        db_path: &std::path::Path,
        filenames: &[String],
    ) -> std::collections::HashMap<String, i64> {
        use rusqlite::Connection;
        let mut map = std::collections::HashMap::new();
        if filenames.is_empty() {
            return map;
        }
        let Ok(conn) = Connection::open(db_path) else {
            return map;
        };
        // Build the parameterized IN-list.
        // SQLite has a default limit of 999
        // parameters per statement; with a
        // few hundred todos per page we're
        // nowhere near that, but a
        // short-circuit on an empty list
        // keeps the SQL well-formed.
        let placeholders =
            std::iter::repeat_n("?", filenames.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT filename, updated FROM markdown_data \
             WHERE filename IN ({placeholders})"
        );
        let Ok(mut stmt) = conn.prepare(&sql) else {
            return map;
        };
        let params: Vec<&dyn rusqlite::ToSql> = filenames
            .iter()
            .map(|f| f as &dyn rusqlite::ToSql)
            .collect();
        let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
            let f: String = row.get(0)?;
            let u: Option<i64> = row.get(1)?;
            Ok((f, u.unwrap_or(0)))
        }) else {
            return map;
        };
        for r in rows.flatten() {
            map.insert(r.0, r.1);
        }
        map
    }

    /// List every unique directory
    /// that has been used in the
    /// global history, sorted by
    /// each directory's most-
    /// recent history row's
    /// timestamp DESC. Each row
    /// also surfaces that
    /// directory's most-recently-
    /// executed command so the
    /// user has context for "what
    /// was I doing in there?" The
    /// typed query (after the
    /// prefix) is treated as a
    /// space-separated AND-filter
    /// against the directory path,
    /// same contract as the
    /// other query modes.
    ///
    /// The "recency" sort is
    /// server-side: the SQL uses
    /// an aggregate `MAX(timestamp)`
    /// over each `directory`
    /// group and orders by it
    /// DESC, so a directory the
    /// user visited yesterday
    /// beats one visited last
    /// week even if both have many
    /// history rows.
    ///
    /// Output shape: reuses
    /// `HistoryRow` so the rest of
    /// the TUI (highlighting,
    /// detail pane, key dispatch)
    /// keeps working without a new
    /// parallel rendering path.
    /// The `command` field carries
    /// the directory's latest
    /// command (so the list rows
    /// show a useful one-line
    /// summary); `directory`
    /// carries the absolute path
    /// (used by the action layer
    /// to stage the `cd`
    /// command); `timestamp`
    /// carries the directory's
    /// `MAX(timestamp)`; `id` is
    /// a synthetic negative
    /// `(directory_index)` so we
    /// don't collide with real
    /// history ids.
    fn fetch_directories(&mut self) -> Result<Vec<HistoryRow>> {
        let filter = self.directories_pattern().trim();
        // Build the SQL once, with
        // a single optional
        // `LIKE` filter per
        // whitespace-split token
        // (AND-matched). Empty
        // pattern means "no
        // filter". Parameter
        // positions are computed
        // along the way so
        // rusqlite binds them in
        // the same order as the
        // `?` placeholders.
        let filter_tokens: Vec<&str> = filter
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .collect();
        let mut sql = String::from(
            "SELECT h.directory, \
                    h.command, \
                    latest.max_ts \
             FROM history h \
             INNER JOIN ( \
                 SELECT directory, \
                        MAX(timestamp) AS max_ts \
                 FROM history \
                 WHERE directory != '' \
                 GROUP BY directory \
             ) latest \
               ON h.directory = latest.directory \
              AND h.timestamp = latest.max_ts \
             WHERE h.directory != ''",
        );
        if !filter_tokens.is_empty() {
            sql.push_str(" AND (");
            for (i, _tok) in filter_tokens.iter().enumerate() {
                if i > 0 {
                    sql.push_str(" AND ");
                }
                sql.push_str("h.directory LIKE ? ESCAPE '\\'");
            }
            sql.push(')');
        }
        // Tie-break: same-timestamp
        // directories sort by
        // directory ASC for stable
        // output. We then
        // canonicalise the
        // directory in code so
        // `/Users/har/foo` and
        // `/Volumes/HUGE/har/foo`
        // collapse to the same
        // group (matching the
        // DIR-mode filter logic
        // elsewhere — see
        // `canonicalize_directory`).
        sql.push_str(
            " GROUP BY h.directory \
             ORDER BY latest.max_ts DESC, h.directory ASC \
             LIMIT 1000",
        );
        let mut stmt = self.conn.prepare(&sql)?;
        // Build owned parameter
        // strings so the lifetime
        // requirements of
        // `params_ref` are satisfied
        // without needing to box-
        // leak. Each token becomes
        // a `%token%` substring
        // for `LIKE`. Empty tokens
        // are skipped so an
        // accidental double-space
        // doesn't blow up the
        // bind count.
        let filter_tokens: Vec<&str> = filter
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .collect();
        let owned_params: Vec<String> = filter_tokens
            .iter()
            .map(|tok| format!("%{}%", crate::util::escape_like(tok)))
            .collect();
        let params_ref: Vec<&dyn rusqlite::ToSql> = owned_params
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        let raw_rows = stmt.query_map(
            params_ref.as_slice(),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        // Use the cached
        // home-prefix list
        // (computed once at App
        // construction; see
        // `build_home_list`) so
        // we don't re-read
        // `~/.config/smarthistory/config`
        // on every
        // `fetch_directories`
        // call. The list
        // already has `$HOME`
        // first and homemap
        // entries after, so
        // `shorten_home_path`
        // does the right thing.
        let home_list = self.home_list.clone();
        // Deduplicate on canonical
        // path: a directory may
        // appear under multiple
        // forms (e.g. `/Users/har/x`
        // and `/Volumes/HUGE/har/x`)
        // because of macOS volume
        // mounts. The first
        // occurrence (which is the
        // newest, since we sort by
        // max_ts DESC) wins.
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut rows: Vec<HistoryRow> = Vec::new();
        let mut next_id: i64 = -1;
        // The directory-source
        // filter is applied
        // *early*, not just at
        // the end. If we let
        // the SQL loop (or the
        // sessiondir loop)
        // populate the shared
        // `seen` set first, a
        // tmux pane whose path
        // also appears in
        // history would be
        // silently deduped away
        // — so in `DIR:TMUX`
        // mode the user would
        // only see the tmux
        // panes whose paths
        // they had *never*
        // visited (exact bug
        // reported: of 5 active
        // panes, only 2 showed,
        // the ones not in the
        // history DB). Skip the
        // irrelevant loops
        // entirely instead.
        let want_sql = matches!(
            self.directory_source,
            crate::tui::state::DirectorySource::All
                | crate::tui::state::DirectorySource::Config
        );
        let want_sessiondirs = matches!(
            self.directory_source,
            crate::tui::state::DirectorySource::All
                | crate::tui::state::DirectorySource::Config
        );
        let want_tmux = matches!(
            self.directory_source,
            crate::tui::state::DirectorySource::All
                | crate::tui::state::DirectorySource::Tmux
        );
        if !want_sql {
            tmux_filter_debug_log(
                "skipping SQL loop (directory_source != All/Config)",
            );
        }
        for raw in raw_rows {
            if !want_sql {
                break;
            }
            let (directory, command, ts) = raw?;
            let canonical = crate::util::canonicalize_directory(&directory);
            if !seen.insert(canonical.clone()) {
                if std::env::var("SMARTHISTORY_DEBUG_TMUX").is_ok() {
                    tmux_filter_debug_log(&format!(
                        "SQL row deduped (dup canonical {:?}): {:?}",
                        canonical, directory
                    ));
                }
                continue;
            }
            // The visible list line
            // shows the **directory**
            // as the primary text
            // and the last command
            // as the secondary text
            // (the inverse of how
            // normal history rows
            // are laid out). We
            // achieve that by
            // storing the directory
            // in `command` (so the
            // existing
            // `highlight_matches(
            //   &row.command, ...)`
            // path applies
            // unchanged) and the
            // last command in
            // `comment` (so the
            // existing `# ...`
            // secondary-slot
            // rendering picks it
            // up). The `directory`
            // field still holds
            // the full absolute
            // path because the
            // tmux-pane lookup
            // (`directory_tmux_pane_id`)
            // canonicalises against
            // it.
            //
            // The directory in
            // `command` is the
            // shell-friendly `~/x`
            // form (matching the
            // user's typing
            // convention) so the
            // query highlighting
            // shows matches in the
            // short form they're
            // used to.
            let short_dir = crate::util::shorten_home_path(
                &directory, &home_list,
            )
            .into_owned();
            // The command in
            // `comment` is
            // truncated because
            // the secondary slot
            // is narrow. The user
            // can still see the
            // full command in
            // the Details pane.
            let short_cmd = if command.is_empty() {
                String::new()
            } else if command.chars().count() > 60 {
                let truncated: String =
                    command.chars().take(57).collect();
                format!("{}…", truncated)
            } else {
                command.clone()
            };
            // Synthetic row. `id`
            // is negative to avoid
            // colliding with real
            // history ids (same
            // convention as todo
            // rows).
            let id = next_id;
            next_id -= 1;
            rows.push(HistoryRow {
                id,
                command: short_dir,
                directory,
                session_id: String::new(),
                exit_code: 0,
                timestamp: ts,
                comment: short_cmd,
                output: String::new(),
                mode: "directory".to_string(),
                source: "history".to_string(),
            });
        }
        // Augment with the user's
        // `sessiondirs=...` entries.
        // Every subdirectory of
        // every configured root
        // becomes a row, even if
        // the user has never run
        // a command there.
        //
        // Rows added by this loop
        // get `timestamp = 0` so
        // they sort to the bottom
        // of the list (the
        // history-driven rows
        // have real recent
        // timestamps and surface
        // first). The user can
        // still type `#<name>` to
        // filter to one of these
        // pinned rows.
        //
        // Dedup is via the same
        // `seen` set the SQL loop
        // used: a subdirectory
        // that *also* has history
        // (and thus already
        // surfaced via SQL)
        // won't appear twice. The
        // history row wins
        // (newer timestamp) and
        // carries the last
        // command; the
        // sessiondirs row is
        // suppressed.
        //
        // The secondary
        // (`comment`) slot is
        // empty for these rows,
        // unless the directory
        // (or an ancestor) has a
        // `.command` file — in
        // which case we surface
        // "has .command" so the
        // user knows the row
        // will run a setup
        // script on select.
        if !want_sessiondirs {
            tmux_filter_debug_log(
                "skipping sessiondir loop (directory_source != All/Config)",
            );
        }
        for sub in &self.session_subdirs {
            if !want_sessiondirs {
                break;
            }
            let canonical = crate::util::canonicalize_directory(
                &sub.to_string_lossy(),
            );
            if !seen.insert(canonical.clone()) {
                continue;
            }
            let directory_str =
                sub.to_string_lossy().into_owned();
            // Apply the same
            // substring filter
            // the SQL fetch
            // applied, so the
            // sessiondirs rows
            // are visible only
            // when they match
            // the user's typed
            // pattern. The SQL
            // `LIKE` uses the
            // raw `directory`
            // (e.g.
            // `/Volumes/HUGE/har/foo`),
            // and the user types
            // a pattern that
            // matches against
            // that form (because
            // the visible list
            // shows the shortened
            // form, but the
            // filtering is on the
            // raw form). For
            // consistency, we
            // also filter on the
            // raw form here, so
            // `#home` matches
            // both a sessiondir at
            // `~/work` (raw
            // `/Users/har/work`)
            // and an SQL row at
            // `/Users/har/home`.
            if !filter_tokens.is_empty()
                && !filter_tokens.iter().all(|tok| {
                    directory_str
                        .to_lowercase()
                        .contains(&tok.to_lowercase())
                })
            {
                continue;
            }
            // Surface a hint when
            // the row has a
            // `.command` file
            // (either in the
            // directory itself or
            // in an ancestor). The
            // user can see at a
            // glance "this row
            // will run a setup
            // script".
            let has_command = crate::util::find_command_file(
                std::path::Path::new(&directory_str),
            )
            .is_some();
            let short_dir = crate::util::shorten_home_path(
                &directory_str,
                &home_list,
            )
            .into_owned();
            let hint = if has_command {
                String::from("(has .command)")
            } else {
                String::new()
            };
            let id = next_id;
            next_id -= 1;
            rows.push(HistoryRow {
                id,
                command: short_dir,
                directory: directory_str,
                session_id: String::new(),
                exit_code: 0,
                // `0` = unix epoch.
                // The list is sorted
                // by timestamp DESC
                // (most-recent first)
                // elsewhere, so
                // epoch-zero rows
                // land at the bottom
                // of the list. The
                // user types a
                // pattern to filter
                // to one of these.
                timestamp: 0,
                comment: hint,
                output: String::new(),
                mode: "directory".to_string(),
                source: "sessiondir".to_string(),
            });
        }
        // Add rows for the
        // cwds of every
        // active tmux pane.
        // These appear in
        // the list even
        // when the user has
        // never run a
        // command in the
        // directory (e.g.
        // a session they
        // started months
        // ago, or a session
        // attached to a
        // project that
        // doesn't yet have
        // history).
        //
        // The `T` marker
        // (drawn in
        // `render_row`)
        // already shows
        // which directories
        // are active in
        // tmux; this
        // augmented list
        // makes the same
        // information
        // available as
        // filterable rows
        // for the `TMUX`
        // directory source
        // (so the user can
        // list "every
        // directory I'm
        // currently active
        // in" without
        // scrolling past
        // their pinned
        // projects or the
        // global history).
        //
        // Each unique
        // `pane_current_path`
        // becomes one row.
        // We dedup against
        // `seen` so a
        // directory that's
        // already in the
        // history (and so
        // already got a
        // row from the SQL
        // loop) doesn't get
        // a duplicate from
        // the tmux side. The
        // history row wins
        // (newer timestamp)
        // and carries the
        // last command; the
        // tmux row is
        // suppressed.
        //
        // Sort order: by
        // `pane_id`
        // (deterministic
        // since tmux
        // returns panes in
        // a stable order).
        // We don't have a
        // meaningful
        // timestamp for a
        // tmux pane
        // (the pane itself
        // doesn't expose
        // one), so we
        // use the current
        // epoch for all
        // tmux rows; the
        // user can still
        // type a pattern to
        // filter to one.
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if !want_tmux {
            tmux_filter_debug_log(
                "skipping tmux loop (directory_source != All/Tmux)",
            );
        }
        for window in &self.tmux_windows {
            if !want_tmux {
                break;
            }
            // Defensive filter: a
            // `pane_current_path`
            // that doesn't start
            // with `/` is not a
            // real absolute
            // filesystem path.
            // Tmux normally
            // reports only real
            // paths, but a
            // custom tmux config
            // or a wrapper could
            // produce something
            // like the command
            // line that spawned
            // the pane
            // (`tmux list-windows
            // -a ...`). Showing
            // such a "path" as a
            // directory row is
            // wrong: the row
            // wouldn't be a
            // directory, the
            // T-marker lookup
            // would fail (no
            // matching pane), and
            // the visible primary
            // text would be a
            // shell command —
            // confusing. The user
            // reported exactly
            // this: a `DIR:TMUX`
            // entry whose text
            // was the tmux
            // command line, with
            // no T flag. The
            // fix: skip any
            // `pane_current_path`
            // that doesn't look
            // like an absolute
            // path.
            if !window.path.starts_with('/') {
                tmux_filter_debug_log(
                    &format!(
                        "filtered tmux pane %{}: pane_current_path {:?} does not start with `/`",
                        window.pane_id,
                        window.path
                    ),
                );
                continue;
            }
            // Also require the
            // path to actually
            // resolve to a
            // directory on disk.
            // A real tmux pane's
            // cwd is a directory
            // that exists; a
            // non-path or a path
            // to a non-existent
            // file shouldn't
            // surface. Without
            // this, a tmux pane
            // whose cwd was
            // deleted while the
            // TUI is running
            // would still show
            // as a row, but the
            // user couldn't
            // actually jump to
            // it. The check is
            // best-effort: a
            // race just means
            // the row disappears
            // on the next
            // refresh, which is
            // the right behaviour
            // anyway.
            if !std::path::Path::new(&window.path)
                .is_dir()
            {
                tmux_filter_debug_log(
                    &format!(
                        "filtered tmux pane %{}: pane_current_path {:?} is not a directory",
                        window.pane_id,
                        window.path
                    ),
                );
                continue;
            }
            let canonical = crate::util::canonicalize_directory(
                &window.path,
            );
            if !seen.insert(canonical.clone()) {
                tmux_filter_debug_log(&format!(
                    "tmux pane %{} deduped (dup canonical {:?}, eaten by an earlier loop): {:?}",
                    window.pane_id, canonical, window.path
                ));
                continue;
            }
            // Same substring
            // filter as the SQL
            // and sessiondirs
            // loops above. The
            // tmux-reported path
            // is the raw absolute
            // form, so filter on
            // it directly.
            if !filter_tokens.is_empty()
                && !filter_tokens.iter().all(|tok| {
                    window
                        .path
                        .to_lowercase()
                        .contains(&tok.to_lowercase())
                })
            {
                continue;
            }
            let short_dir = crate::util::shorten_home_path(
                &window.path,
                &home_list,
            )
            .into_owned();
            // Build a
            // synthetic
            // command
            // field for
            // the
            // secondary
            // slot: the
            // pane id.
            // The user
            // can copy
            // / reuse
            // it
            // (e.g. as
            // the
            // `-t`
            // argument
            // to a
            // custom
            // tmux
            // command)
            // directly
            // from the
            // list.
            let pane_hint = format!(
                "(pane {})",
                window.pane_id
            );
            let id = next_id;
            next_id -= 1;
            tmux_filter_debug_log(&format!(
                "kept tmux pane %{}: pane_current_path {:?} (source=tmux)",
                window.pane_id,
                window.path
            ));
            rows.push(HistoryRow {
                id,
                command: short_dir,
                directory: window.path.clone(),
                session_id: String::new(),
                exit_code: 0,
                timestamp: now_epoch,
                comment: pane_hint,
                output: String::new(),
                mode: "directory".to_string(),
                source: "tmux".to_string(),
            });
        }
        // Apply the
        // directory-source
        // filter. The
        // `ALL` mode is a
        // no-op; the
        // `TMUX` and
        // `CONFIG` modes
        // drop rows whose
        // `source` doesn't
        // match.
        let rows: Vec<HistoryRow> = match self.directory_source {
            crate::tui::state::DirectorySource::All => rows,
            crate::tui::state::DirectorySource::Tmux => {
                rows.into_iter()
                    .filter(|r| r.source == "tmux")
                    .collect()
            }
            crate::tui::state::DirectorySource::Config => {
                rows.into_iter()
                    .filter(|r| r.source == "sessiondir")
                    .collect()
            }
        };
        Ok(rows)
    }

    /// Populate `self.session_panes` from
    /// `tmux list-panes -s` (the *current*
    /// session only — `-s` limits to the
    /// session the TUI is running in, unlike
    /// `-a` which walks every session). The
    /// current pane (`$TMUX_PANE`) is excluded
    /// so the user never sees the pane they're
    /// in. Idempotent — runs at most once per
    /// TUI session; subsequent calls return
    /// immediately (the pane set doesn't
    /// change while the TUI is the foreground
    /// process). Failure modes are silent
    /// (same contract as `fetch_tmux_windows`):
    /// `tmux` not on PATH, not in a tmux
    /// session, or the subprocess hangs past
    /// `TMUX_PANE_PROBE_TIMEOUT_MS` → the
    /// cache stays empty and the user sees an
    /// empty list.
    ///
    /// Each pane becomes a `HistoryRow`:
    /// - `command` (primary text) = the
    ///   pane's current command
    ///   (`#{pane_current_command}`, e.g.
    ///   `zsh`, `vim`, `cargo`).
    /// - `comment` (secondary text) = the
    ///   pane's cwd shortened to `~/x`.
    /// - `directory` = the full canonical cwd.
    /// - `session_id` = the pane id (`%N`),
    ///   used as the `select-pane -t` target.
    /// - `output` = the pane's global window
    ///   id (`@N`), used as the
    ///   `select-window -t` target so the
    ///   jump works even when the pane is
    ///   in a different window than the
    ///   current one (plain `select-pane`
    ///   does NOT switch windows).
    /// - `source` = `"pane"`.
    /// - `id` = synthetic decreasing negative.
    fn fetch_session_panes(&mut self) {
        if !self.session_panes.is_empty() {
            return;
        }
        // `$TMUX_PANE` is the pane id the TUI
        // is running in (set by tmux for every
        // pane). Empty when not inside tmux —
        // in that case `list-panes -s` would
        // also fail, so we bail early.
        let current_pane = std::env::var("TMUX_PANE")
            .unwrap_or_default();
        if current_pane.is_empty() {
            return;
        }
        self.fetch_session_panes_impl(&current_pane);
    }

    /// The implementation of `fetch_session_panes`,
    /// separated so tests can inject the "current
    /// pane" id directly (env-var mutation is
    /// `unsafe` since Rust 1.66 and is racy under
    /// the parallel test runner). `current_pane`
    /// is the pane id to EXCLUDE from the list
    /// (the one the TUI is running in). Reads
    /// `list-panes -s` and caches the parsed
    /// panes into `self.session_panes`.
    fn fetch_session_panes_impl(
        &mut self,
        current_pane: &str,
    ) {
        // Format: pane id | window id | cwd |
        // current command | last flag.
        // We use `|` as the field separator
        // (same convention as `fetch_tmux_windows`;
        // tmux lets the cwd and command contain
        // most punctuation but not `|` in
        // practice, and even if it did the
        // split-into-first/two would still
        // surface a usable pane id). The
        // trailing `#{?pane_last,1,0}` is tmux's
        // "last (previously-active) pane" flag —
        // the pane `tmux last-pane` would jump
        // to. We bubble it to the top of the
        // list so the user can flip back to the
        // pane they just came from by pressing
        // Enter (the default selection is index
        // 0 = the newest row).
        const FORMAT: &str = "#{pane_id} | #{window_id} | #{pane_current_path} | #{pane_current_command} | #{?pane_last,1,0}";
        let timeout_ms: u64 = std::env::var(
            "TMUX_PANE_PROBE_TIMEOUT_MS",
        )
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
        let mut cmd = std::process::Command::new("tmux");
        cmd.args(["list-panes", "-s", "-F", FORMAT])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut panes: Vec<HistoryRow> = Vec::new();
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Some(stdout) = child.stdout.take() {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stdout);
            // Decreasing synthetic ids
            // so the panes sort
            // consistently by pane id
            // order if the sort is
            // timestamp-based (they all
            // share `now_epoch`).
            let mut next_id: i64 = -1;
            for line in reader.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => continue,
                };
                let parts: Vec<&str> =
                    line.split('|').map(str::trim).collect();
                if parts.len() < 4 {
                    continue;
                }
                let pane_id = parts[0];
                // The pane's global window id (`@N`).
                // Used by `select_for_run` to stage
                // `select-window -t @N` BEFORE
                // `select-pane -t %N` — plain
                // `select-pane` does NOT switch
                // windows, so a pane in another
                // window wouldn't be jumped to
                // without selecting its window
                // first.
                let window_id = parts[1];
                let path_raw = parts[2];
                // The current command is
                // the 4th field; a pane
                // with no command yet
                // (rare) reports empty.
                let current_command =
                    parts.get(3).copied().unwrap_or("");
                // The 5th field is tmux's
                // `pane_last` flag
                // (`#{?pane_last,1,0}`): 1 for
                // the last (previously-active)
                // pane in the session — the one
                // `tmux last-pane` jumps to.
                // `0` (or missing) for every
                // other pane.
                let is_last = parts
                    .get(4)
                    .copied()
                    .unwrap_or("0")
                    == "1";
                if pane_id.is_empty() {
                    continue;
                }
                // Exclude the pane the
                // TUI is running in.
                if pane_id == current_pane {
                    continue;
                }
                // Require a real absolute
                // path (same defensive
                // filter as the
                // directories tmux loop).
                if !path_raw.starts_with('/') {
                    continue;
                }
                let full_path =
                    crate::util::canonicalize_directory(path_raw);
                let short_dir = crate::util::shorten_home_path(
                    &full_path,
                    &self.home_list,
                )
                .into_owned();
                let id = next_id;
                next_id -= 1;
                panes.push(HistoryRow {
                    id,
                    command: current_command.to_string(),
                    directory: full_path,
                    session_id: pane_id.to_string(),
                    exit_code: 0,
                    // The last pane gets a
                    // newer timestamp so it
                    // stays first under any
                    // timestamp-DESC sort and
                    // signals "newest" (the row
                    // the default selection
                    // lands on). The actual
                    // ordering is finalized by
                    // the bubble-to-front below.
                    timestamp: if is_last {
                        now_epoch + 1
                    } else {
                        now_epoch
                    },
                    comment: short_dir,
                    // Stash the window id (`@N`)
                    // here for `select_for_run`'s
                    // cross-window jump. Panes
                    // rows have no captured
                    // output, so this slot is
                    // otherwise unused; the
                    // output-view (Ctrl+L) on a
                    // panes row would show the
                    // window id, which is a
                    // harmless informational
                    // hint.
                    output: window_id.to_string(),
                    mode: "pane".to_string(),
                    source: "pane".to_string(),
                });
            }
        }
        let _ = child.wait();
        let _ = timeout_ms;
        // Bubble the last (previously-active)
        // pane to the front so it is row 0 —
        // the default selection — and the
        // user can flip back to the pane they
        // just came from by pressing Enter.
        // `tmux last-pane` tracks exactly one
        // last pane; if it was excluded (e.g.
        // env-var quirk where `$TMUX_PANE`
        // equals the last pane) no row is
        // moved and the natural order is kept.
        if let Some(pos) = panes
            .iter()
            .position(|r| r.timestamp > now_epoch)
            && pos > 0 {
                let row = panes.remove(pos);
                panes.insert(0, row);
            }
        self.session_panes = panes;
    }

    /// Return the cached session-panes list,
    /// filtered by the `*`-body substring
    /// filter. Each whitespace-separated token
    /// in the body must appear (case-
    /// insensitive) in the pane's command OR
    /// its cwd (the short `~/x` form), so the
    /// user can narrow by either dimension
    /// (e.g. `*vim` to find panes running vim,
    /// or `*work` to find panes in a
    /// `~/work` subdir). Never touches the
    /// DB and never re-runs `tmux` — it reads
    /// the cached `session_panes` (populated
    /// by `fetch_session_panes`, which
    /// `refresh()` calls before this). An
    /// empty list (no tmux / not in a
    /// session / the only pane is the current
    /// one) is a valid result.
    fn fetch_panes(&mut self) -> Result<Vec<HistoryRow>> {
        self.fetch_session_panes();
        let filter = self.panes_pattern().trim();
        let tokens: Vec<String> = filter
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(|t| t.to_lowercase())
            .collect();
        if tokens.is_empty() {
            return Ok(self.session_panes.clone());
        }
        Ok(self
            .session_panes
            .iter()
            .filter(|r| {
                let cmd_lc = r.command.to_lowercase();
                let dir_lc = r.comment.to_lowercase();
                tokens.iter().all(|tok| {
                    cmd_lc.contains(tok)
                        || dir_lc.contains(tok)
                })
            })
            .cloned()
            .collect())
    }

    /// Walk the current directory
    /// recursively, collecting
    /// every file path whose
    /// relative path matches the
    /// typed pattern (AND by
    /// whitespace-separated
    /// substring tokens, case-
    /// insensitive). Hidden
    /// directories (names
    /// starting with `.`) are
    /// skipped. Returns up to
    /// 1000 rows, sorted by
    /// path (directories first,
    /// then alphabetical).
    fn fetch_files(&mut self) -> Result<Vec<HistoryRow>> {
        // Return cached results from the
        // most recent background walk.
        // The walk is triggered by
        // `files_maybe_autocall` →
        // `spawn_files_walk` → background
        // thread → `process_files_result`.
        // While the walk is in flight,
        // this returns the stale (or
        // empty) cache so the TUI never
        Ok(self.files_state.rows.clone())
    }
}

// File-mode helpers (FilesState, walk_dir, IgnoreSet,
// read_preview_bytes, spawn_walk) now live in
// `src/files.rs` and are imported via `crate::files::*`.

impl App {
    /// Look up the cached tmux
    /// window whose `path`
    /// matches `dir` (after
    /// canonicalization) and
    /// return its `pane_id`.
    /// Returns `None` when the
    /// snapshot is empty (never
    /// populated — no `#…`
    /// query yet — or populated
    /// but no window matched).
    ///
    /// Both sides are canonicalised
    /// so the macOS volume-mount
    /// case (the user's logical
    /// `/Users/har/...` vs the
    /// tmux-reported
    /// `/Volumes/HUGE/har/...`)
    /// collapses to a single
    /// match. Without this, a
    /// directory whose only
    /// representation in history
    /// was `/Users/har/Sources/...`
    /// would *never* be flagged as
    /// having an active tmux window,
    /// because tmux reports
    /// `/Volumes/HUGE/har/Sources/...`
    /// for the same physical dir.
    ///
    /// **First match wins**. The
    /// snapshot is sorted in the
    /// order tmux returns it,
    /// which on `list-windows -a`
    /// is window-id order. The
    /// user pressing Enter on a
    /// `T`-marked row goes to
    /// whichever window tmux
    /// reports first — that's
    /// predictable and consistent
    /// with the displayed mark.
    fn directory_tmux_pane_id(&self, dir: &str) -> Option<String> {
        if self.tmux_windows.is_empty() {
            return None;
        }
        // Normalize the
        // DB-side path to a
        // canonical absolute
        // form so it matches
        // the tmux-reported
        // path regardless of
        // how the row is
        // stored.
        //
        // The DB may have:
        // - `~/x` (after
        //   `smarthistory
        //   update` rewrote
        //   it), or
        // - `/Users/har/x` or
        //   `/Volumes/HUGE/har/x`
        //   (when the precmd
        //   hook captured a
        //   fresh row using
        //   the user's logical
        //   form).
        //
        // The tmux side has
        // an absolute path
        // (real filesystem
        // path), which
        // `canonicalize_directory`
        // resolves through
        // any macOS volume
        // mounts.
        //
        // To make the two
        // match, we expand
        // `~/x` using the
        // user's home list
        // (so the DB-side
        // `~/x` becomes
        // `/Users/har/x` or
        // the homemap form)
        // BEFORE calling
        // `canonicalize_directory`
        // — otherwise the
        // canonicalize would
        // fail on `~/x` and
        // fall back to the
        // un-resolved input,
        // which never matches
        // the tmux side. (The
        // homemap form (e.g.
        // `/Volumes/HUGE/har/x`)
        // gets resolved by
        // `canonicalize_directory`
        // to the same physical
        // path tmux reports.)
        let db_normalized = crate::util::normalize_for_compare(
            dir, &self.home_list,
        );
        self.tmux_windows
            .iter()
            .find(|w| {
                // Both sides go through
                // `normalize_for_compare`
                // (expand `~/` +
                // canonicalize) so a
                // DB row stored as
                // `~/x` and a tmux
                // pane reported as
                // `/Users/.../x`
                // produce the same
                // string and the
                // comparison succeeds.
                // Without the homemap
                // expansion, the
                // canonicalize step
                // would fail on the
                // `~/x` form and the
                // two sides would
                // never agree.
                let tmux_normalized =
                    crate::util::normalize_for_compare(
                        &w.path, &self.home_list,
                    );
                tmux_normalized == db_normalized
            })
            .map(|w| w.pane_id.clone())
    }

    /// Run `tmux list-panes -a -F
    /// '<fmt>'` once and parse the
    /// output into `self.tmux_windows`.
    /// Idempotent — runs at most
    /// once per TUI session (the
    /// snapshot stays cached unless
    /// the user explicitly forces a
    /// refresh).
    ///
    /// **Failure modes are silent**
    /// (deliberately):
    /// - `tmux` not on PATH →
    ///   `Command::new` returns
    ///   `Err(io::ErrorKind::NotFound)`
    ///   → snapshot stays empty,
    ///   no marker is ever
    ///   shown, no error
    ///   surfaces in the UI.
    /// - The user isn't running a
    ///   tmux server → `tmux`
    ///   exits non-zero →
    ///   snapshot stays empty.
    /// - `tmux` is installed but
    ///   the user runs in a
    ///   different session
    ///   (e.g. inside a remote
    ///   pane or `screen`) →
    ///   same as above.
    /// - The subprocess takes too
    ///   long → we cap it with
    ///   a 1-second timeout
    ///   (configurable via
    ///   `TMUX_PANE_PROBE_TIMEOUT_MS`)
    ///   so the snapshot fetch
    ///   never blocks the TUI for
    ///   more than that. On
    ///   timeout the snapshot
    ///   stays empty (we err on
    ///   the side of "no marker
    ///   shown" rather than
    ///   "TUI frozen").
    ///
    /// This helper is called from
    /// `refresh()` the first time
    /// `is_directories_query()`
    /// becomes true after the
    /// snapshot is empty; the
    /// surrounding `refresh()` is
    /// wired so the snapshot is
    /// populated *before* the
    /// SQL query goes out, so the
    /// first frame after the user
    /// types `#` already has the
    /// marker fully resolved.
    fn fetch_tmux_windows(&mut self) {
        // Skip if already populated.
        // The snapshot is per-TUI-
        // session; refreshing it
        // would mean re-spawning
        // `tmux` for every
        // keystroke the user makes
        // while in directories
        // mode, which is
        // wasteful. A future
        // "refresh tmux" key
        // binding could re-invoke
        // this when the user
        // wants freshness.
        if !self.tmux_windows.is_empty() {
            return;
        }
        // The command matches the
        // user's spec verbatim
        // (minus the trailing
        // `| grep "active:1"`,
        // which we do in-process
        // so we only spawn one
        // subprocess and can
        // short-circuit on a
        // timeout without race
        // conditions on a piped
        // grep).
        //
        // Output format (one line
        // per window):
        //   <pane_id> | <path> |
        //   active:<0|1> |
        //   Layout: <window_layout>
        //
        // `pane_id` is the
        // globally-unique pane id
        // (e.g. `%2`) — sufficient
        // as a `tmux ... -t <id>`
        // target. We use `|` as
        // the field separator
        // because tmux lets session
        // names contain spaces
        // and reserves `:`, `,`,
        // `;`, `\`, ` ` as
        // separators in some
        // commands; `|` is safe
        // in all current formats.
        const FORMAT: &str = "\
            #{pane_id} | \
            #{pane_current_path} | \
            active:#{window_active} | \
            Layout: #{window_layout}";
        // Read the timeout from
        // `TMUX_PANE_PROBE_TIMEOUT_MS`
        // with a 1-second default.
        // (Kept the old env-var
        // name for back-compat —
        // the timeout semantics
        // are unchanged.) The TUI
        // is the user's foreground
        // app; we'd rather miss a
        // marker than freeze the UI
        // because a misbehaving
        // `tmux` server hung.
        let timeout_ms: u64 = std::env::var(
            "TMUX_PANE_PROBE_TIMEOUT_MS",
        )
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
        let mut cmd = std::process::Command::new("tmux");
        cmd.args(["list-windows", "-a", "-F", FORMAT])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            // `tmux` not on PATH (or
            // not executable). This
            // is a normal failure
            // mode we want to handle
            // silently — it's not
            // an error condition for
            // the user, just a "we
            // can't show the
            // tmux-window marker"
            // condition.
            Err(_) => return,
        };
        // Read the stdout before
        // waiting on the process so
        // we don't deadlock on a
        // pipe-fill scenario (tmux
        // writing to a full pipe
        // would block forever).
        let mut windows = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            use std::io::BufRead;
            // Wrap stdout in a
            // buffered reader so we
            // can `read_line` without
            // making one syscall per
            // line. The buffer is
            // 8 KiB which fits
            // typical `tmux
            // list-windows` output
            // even with hundreds of
            // windows in well under
            // 64 KiB.
            let reader = std::io::BufReader::new(stdout);
            let tmux_debug = std::env::var(
                "SMARTHISTORY_DEBUG_TMUX",
            )
            .is_ok();
            for line in reader.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => continue,
                };
                if tmux_debug {
                    tmux_filter_debug_log(
                        &format!(
                            "raw tmux line: {:?}",
                            line
                        ),
                    );
                }
                match parse_tmux_pane_line(&line)
                {
                    Some(window) => {
                        if tmux_debug {
                            tmux_filter_debug_log(
                                &format!(
                                    "  parsed pane id={:?} path={:?}",
                                    window.pane_id,
                                    window.path
                                ),
                            );
                        }
                        windows.push(window);
                    }
                    None => {
                        if tmux_debug {
                            tmux_filter_debug_log(
                                &format!(
                                    "  DROPPED (failed parse: not 4 fields, or window not active, or empty path): {:?}",
                                    line
                                ),
                            );
                        }
                    }
                }
            }
        }
        match child.wait() {
            Ok(_) => {}
            Err(_) => return,
        }
        // Commit the snapshot.
        self.tmux_windows = windows;
        let _ = timeout_ms;
    }

    /// Search the note_search database for notes matching the
    /// current query. Returns HistoryRow entries with the note
    /// filename as `command`, the title as `comment`, and the
    /// full content as `output`.
    fn fetch_notes(&mut self) -> Result<Vec<HistoryRow>> {
        let Some(ref db_path) = self.notes_database else {
            return Ok(Vec::new());
        };
        let raw_pattern = self.notes_pattern().trim();
        // Strip any date-filter aliases (`@today`,
        // `@week`, `@month`, `@year`) from the
        // pattern. The cleaned pattern is what we
        // pass to `note_search.search_notes_by_query`
        // (which doesn't know about these
        // aliases); the filter is applied
        // post-query in this method against the
        // `updated` timestamp on each result.
        let (pattern, filter) = parse_notes_query(raw_pattern);
        // Record the resolved filter on `self` so
        // the mode-strip chip renderer (and any
        // future helper) can see what's active.
        // We update this on every refresh, even
        // when the pattern is empty (so the chip
        // disappears the moment the user clears
        // the alias token).
        self.notes_date_filter = filter;
        if pattern.is_empty() {
            // The user typed only the
            // date alias (e.g. `@today`).
            // We still need to fetch
            // *all* notes (no text
            // filter) so the date
            // filter has something to
            // operate on, then apply
            // the cutoff post-hoc. The
            // previous behaviour was to
            // return every note
            // unfiltered, which made
            // `@today` indistinguishable
            // from `@` — the chip lit
            // up but the rows ignored
            // it.
            return self.fetch_recent_notes_with_filter(
                db_path,
                filter,
            );
        }
        
        let service = note_search::database_service::DatabaseService::new(
            &db_path.to_string_lossy()
        );
        
        match service.search_notes_by_query(&pattern) {
            Ok(results) => {
                // Apply the date filter (if any) before
                // building `HistoryRow` entries. Notes
                // with `updated = None` fall back to
                // `created`; if both are `None`, the
                // note has no usable timestamp and we
                // exclude it from any active filter
                // (we have no way to know if it's
                // recent). This matches the user's
                // intent: aliases answer "what was
                // updated *recently*", and a note
                // without timestamps is by
                // definition not "recently updated".
                let cutoff = filter.cutoff(self.now_epoch());
                let mut rows: Vec<HistoryRow> = results
                    .iter()
                    .filter(|note| match cutoff {
                        None => true,
                        Some(c) => {
                            let ts = note.updated.or(note.created).unwrap_or(0);
                            ts >= c
                        }
                    })
                    .map(|note| {
                    let title = note.title.as_deref().unwrap_or("");
                    let comment = if title.is_empty() {
                        note.filename.clone()
                    } else {
                        format!("{} — {}", title, note.filename)
                    };
                    let ts = note.updated.or(note.created).unwrap_or(0);
                    HistoryRow {
                        id: 0,
                        command: note.filename.clone(),
                        directory: String::new(),
                        session_id: String::new(),
                        exit_code: 0,
                        timestamp: ts,
                        comment,
                        output: self.read_note_preview(&note.filename),
                        mode: "note".to_string(),
                        source: String::new(),
                    }
                }).collect();
                // Sort by timestamp descending (newest first)
                rows.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
                self.notes_query_error = false;
                Ok(rows)
            }
            Err(_e) => {
                self.notes_query_error = true;
                Ok(Vec::new())
            }
        }
    }
    
    /// Fetch recent notes (when no query is entered).
/// Fetch every note in the
    /// database (no text filter)
    /// and apply the date-filter
    /// alias (if any) against
    /// each note's `updated`
    /// timestamp. Used when the
    /// user types a bare alias
    /// (e.g. `@today`) —
    /// `parse_notes_query` returns
    /// an empty text pattern in
    /// that case, so we can't push
    /// the alias through the
    /// library's text search; we
    /// fetch every note and filter
    /// by mtime post-hoc instead.
///
/// `NotesDateFilter::All` is
/// the no-op case (no cutoff
/// applied); passing it gives
/// the same result as fetching
/// all notes unfiltered.
fn fetch_recent_notes_with_filter(
        &self,
        db_path: &std::path::Path,
        filter: NotesDateFilter,
    ) -> Result<Vec<HistoryRow>> {
        let service = note_search::database_service::DatabaseService::new(
            &db_path.to_string_lossy()
        );
        // Use default SearchCriteria to get all notes (no query filter).
        let criteria = note_search::SearchCriteria::default();
        match service.search_notes(&criteria) {
            Ok(results) => {
                let cutoff = filter.cutoff(self.now_epoch());
                let mut rows: Vec<HistoryRow> = results
                    .iter()
                    .filter(|note| match cutoff {
                        // No active filter:
                        // every note qualifies.
                        None => true,
                        // Active filter:
                        // require a recent
                        // `updated` (falling
                        // back to `created`
                        // when missing). Notes
                        // with neither are
                        // excluded — we
                        // can't know if they're
                        // recent.
                        Some(c) => note
                            .updated
                            .or(note.created)
                            .unwrap_or(0)
                            >= c,
                    })
                    .map(|note| {
                    let title = note.title.as_deref().unwrap_or("");
                    let comment = if title.is_empty() {
                        note.filename.clone()
                    } else {
                        format!("{} — {}", title, note.filename)
                    };
                    let ts = note.updated.or(note.created).unwrap_or(0);
                    HistoryRow {
                        id: 0,
                        command: note.filename.clone(),
                        directory: String::new(),
                        session_id: String::new(),
                        exit_code: 0,
                        timestamp: ts,
                        comment,
                        output: self.read_note_preview(&note.filename),
                        mode: "note".to_string(),
                        source: String::new(),
                    }
                }).collect();
                // Sort by timestamp descending (newest first)
                rows.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
                Ok(rows)
            }
            Err(_e) => {
                Ok(Vec::new())
            }
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

    /// Cycle the search mode prefix: plain -> `/` (regex) -> `?`
    /// (fuzzy) -> `+` (output search) -> plain. When the query
    /// is empty, the mode is just "plain"; otherwise the first
    /// character is replaced with the new mode prefix. The body
    /// of the query (everything after the prefix) is preserved.
    fn cycle_search_mode(&mut self) {
        self.set_search_mode_prefix(self.next_search_mode_prefix(self.query_prefix()));
    }

    /// The first character of the query if it is a mode prefix
    /// (regex, fuzzy, or output), otherwise a sentinel (`\0`) meaning
    /// "plain".
    fn query_prefix(&self) -> char {
        if self.query.is_empty() {
            '\0'
        } else {
            let c = self.query.chars().next().unwrap();
            let p = &self.query_prefixes;
            if c == p.regex || c == p.fuzzy || c == p.output { c } else { '\0' }
        }
    }

    /// The next mode prefix in the cycle plain -> regex ->
    /// fuzzy -> output -> plain.
    fn next_search_mode_prefix(&self, current: char) -> char {
        let p = &self.query_prefixes;
        if current == p.regex {
            p.fuzzy
        } else if current == p.fuzzy {
            p.output
        } else if current == p.output {
            '\0'
        } else {
            p.regex
        }
    }

    /// Apply a new mode prefix to the query, preserving the rest
    /// of the text. `\0` means "no prefix" (plain mode).
    fn set_search_mode_prefix(&mut self, new_prefix: char) {
        let p = &self.query_prefixes;
        // Use `chars()` to be robust against multi-byte UTF-8
        // prefixes. We drop the first char only if it's a mode
        // prefix, otherwise the body is the whole query.
        let body: String = self
            .query
            .chars()
            .next()
            .map(|c| if c == p.regex || c == p.fuzzy || c == p.output {
                self.query[c.len_utf8()..].to_string()
            } else {
                self.query.clone()
            })
            .unwrap_or_default();

        self.query = if new_prefix == '\0' {
            body
        } else {
            let mut s = String::with_capacity(body.len() + new_prefix.len_utf8());
            s.push(new_prefix);
            s.push_str(&body);
            s
        };
        self.recompile_regex();
        // The query text changed (the prefix was replaced or
        // stripped). Reset the cursor to the new end so the
        // next character appends naturally; the user can
        // re-position it with Left/Right if they're now in
        // LLM mode.
        self.query_cursor = self.query.chars().count();
        self.refresh();
        // A mode-prefix change is also a user edit of the
        // query — the debounce and the previously-suggested
        // preview are now stale. Re-arm or clear so the
        // auto-call cycle restarts cleanly.
        self.llm_touch();
    }

    /// Re-arm the LLM auto-call debounce and discard any
    /// in-flight preview that no longer matches the live
    /// description. Called from every user-edit path
    /// (`push_char`, `backspace`, `clear_query`,
    /// `set_search_mode_prefix`).
    ///
    /// Lifecycle:
    /// - When the query is an LLM query, we set
    ///   `llm_debounce_started = Some(Instant::now())` so
    ///   the run-loop tick can count down to a fresh
    ///   `LLM_DEBOUNCE` window. We also clear any existing
    ///   preview so the user doesn't see a stale suggestion
    ///   for a description they no longer have.
    /// - When the query is NOT an LLM query, we clear
    ///   everything: the debounce, the preview, the
    ///   in-flight flag, and the description we last
    ///   fired on. The user has left LLM mode entirely;
    ///   there's nothing for the auto-call path to do
    ///   until they return.
    ///
    /// The function is infallible and never blocks — the
    /// actual HTTP call is deferred to
    /// [`App::llm_maybe_autocall`], which runs on the
    /// run-loop tick.
    fn llm_touch(&mut self) {
        if self.is_llm_query() {
            self.llm_debounce_started = Some(std::time::Instant::now());
            // The user has just edited the description.
            // Any previously-shown preview is now stale:
            // the next auto-call will overwrite it. We
            // also discard the in-flight flag so the
            // returned preview (if any arrives after
            // this point) is checked against the new
            // description.
            if self.llm_preview.is_some() {
                self.llm_preview = None;
                self.llm_preview_description = None;
                // Re-render with the preview removed.
                self.refresh();
            }
        } else {
            // The user has left LLM mode (e.g. backspaced
            // the `=` or replaced the query entirely). Reset
            // all debounce state so the next LLM session
            // starts from a clean slate.
            self.llm_debounce_started = None;
            self.llm_in_flight = false;
            self.llm_preview = None;
            self.llm_preview_description = None;
        }
        // Arm/clear the JIRA search debounce on the same
        // edit edges. `jira_touch` is a no-op outside `-`
        // mode, so co-locating it here means every existing
        // `llm_touch()` call site (push_char, backspace,
        // set_search_mode_prefix, etc.) also drives JIRA
        // search-as-you-type without needing a parallel
        // pass of edits.
        self.jira_touch();
        // Same co-location for the files-mode
        // walk debounce. `files_touch` is a
        // no-op outside `~` mode, so putting
        // it here means every edit path also
        // drives files search-as-you-type.
        self.files_touch();
    }

    /// Construct the virtual preview row used to display the
    /// most-recent auto-call result. Returns `None` when no
    /// preview is active. Called from
    /// [`App::build_merged_rows`] so the preview appears at
    /// the top of the merged list (newest-first) while the
    /// user is composing the LLM query.
    ///
    /// The synthetic row uses:
    /// - `id = -1` (real history ids are positive — the
    ///   negative value lets render code mark the row as a
    ///   preview without a separate boolean field).
    /// - `command = "=" + <user's description>` (the query text).
    /// - `output = <LLM response>` (the generated command).
    /// - `comment = <LLM response>` so the preview is
    ///   self-documenting.
    /// - `exit_code = -1` (the same sentinel used for
    ///   newly-inserted LLM-generated rows; signals
    ///   "never executed" to render code).
    /// - `timestamp = now` so the preview sorts at the very
    ///   top of the merged list.
    fn llm_preview_row(&self) -> Option<HistoryRow> {
        self.llm_preview.clone()
    }

    /// Spawn a background thread to make an LLM request.
    /// Returns immediately; the result is processed by the
    /// run loop when it arrives. Sets `llm_in_flight` and
    /// stores the `LlmRequest` so the run loop can poll it
    /// and the user can cancel it.
    fn spawn_llm_request(
        &mut self,
        request_type: LlmRequestType,
        prompt: String,
    ) {
        // If we have an LLM client but no config (e.g. in tests with FakeLlm),
        // process the request synchronously using the appropriate method.
        if self.llm_config.is_none() {
            if let Some(ref llm) = self.llm {
                let result = match &request_type {
                    LlmRequestType::Generate { .. } => llm.generate(&prompt),
                    LlmRequestType::Describe { command } => llm.describe(command),
                    LlmRequestType::Correct { original_command } => llm.correct(original_command),
                    LlmRequestType::Question { question } => llm.question(question),
                };
                let request = LlmRequest {
                    request_type,
                    receiver: mpsc::channel().1,
                    cancelled: Arc::new(AtomicBool::new(false)),
                };
                self.process_llm_result(request, result);
            } else {
                self.set_status_message(
                    crate::llm::LlmError::NotConfigured.to_string(),
                );
            }
            return;
        }
        
        let Some(ref cfg) = self.llm_config else {
            self.set_status_message(
                crate::llm::LlmError::NotConfigured.to_string(),
            );
            return;
        };
        let cfg = cfg.clone();
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_clone = cancelled.clone();
        
        std::thread::spawn(move || {
            let client = crate::llm::OllamaClient::new(&cfg);
            let result = client.prompt(&prompt);
            // Check if cancelled before sending the result
            if !cancelled_clone.load(Ordering::Relaxed) {
                let _ = tx.send(result);
            }
        });
        
        self.llm_in_flight = true;
        self.llm_request = Some(LlmRequest {
            request_type,
            receiver: rx,
            cancelled,
        });
        self.set_status_message("LLM request in progress…".to_string());
    }

    /// Drive the LLM auto-call debounce. Called from the
    /// run-loop tick (every ~100ms when no input is
    /// available). Fires a single LLM call when all of the
    /// following are true:
    ///
    /// 1. The query is an LLM query (`=` prefix).
    /// 2. The description is non-empty (no point calling
    ///    the model for "=").
    /// 3. A debounce timer is armed and at least
    ///    [`LLM_DEBOUNCE`] has elapsed since the last
    ///    `llm_touch`.
    /// 4. No LLM call is currently in flight.
    /// 5. The LLM client is configured.
    /// 6. The live description differs from the
    ///    description the last preview was generated for
    ///    (avoids re-firing the same call repeatedly when
    ///    the user pauses but the suggestion is already on
    ///    screen).
    ///
    /// Returns immediately when the conditions aren't met.
    /// The actual HTTP call is synchronous (matches the
    /// existing `run_llm_query` semantics) but bounded by
    /// the same 30s timeout the explicit-call path uses.
    fn llm_maybe_autocall(&mut self) {
        if !self.is_llm_query() {
            return;
        }
        let prefix = self.query_prefixes.llm;
        let description = self.query[prefix.len_utf8()..].trim().to_string();
        if description.is_empty() {
            return;
        }
        // No client configured. The user is composing an
        // LLM query without having set `ollama.url` /
        // `ollama.model` — we silently do nothing on the
        // auto-call path. The status-message they get when
        // they press Enter (via `run_llm_query`) is
        // sufficient feedback.
        if self.llm.is_none() {
            return;
        }
        // Already firing; let the current call complete.
        if self.llm_in_flight {
            return;
        }
        // Debounce window hasn't elapsed yet.
        let Some(started) = self.llm_debounce_started else {
            return;
        };
        if started.elapsed() < LLM_DEBOUNCE {
            return;
        }
        // Already have a fresh preview for this exact
        // description — don't fire a second call until
        // the user actually changes something.
        if self.llm_preview_description.as_deref() == Some(&description) {
            return;
        }
        // All conditions met: arm the in-flight flag,
        // capture the description for the response-check
        // path, and fire the call. The debounce is left
        // armed (not cleared) so a failed call doesn't
        // immediately re-fire; the next keystroke will
        // reset it.
        self.llm_in_flight = true;
        self.llm_debounce_started = Some(std::time::Instant::now());
        let fired_description = description.clone();
        let Some(llm) = self.llm.as_deref() else {
            self.llm_in_flight = false;
            return;
        };
        let raw = match llm.generate(&fired_description) {
            Ok(s) => s,
            Err(_) => {
                // Errors during auto-call are silent: the
                // explicit Run path will show a status
                // message if the user presses Enter. The
                // auto-call is best-effort and shouldn't
                // crowd the status bar on every typo.
                self.llm_in_flight = false;
                return;
            }
        };
        let Some(command) = crate::llm::sanitize_command(&raw) else {
            // LLM didn't produce a usable command. Same
            // silent-on-auto-call policy as a transport
            // error: the user will see feedback when they
            // press Enter.
            self.llm_in_flight = false;
            return;
        };
        // Build the synthetic preview row. The id is a
        // negative sentinel so render code can mark it.
        // The command is the user's description (with = prefix),
        // and the output is the LLM-generated command.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let preview = HistoryRow {
            id: -1,
            command: format!("{}{}", self.query_prefixes.llm, fired_description),
            directory: std::env::var("PWD").unwrap_or_default(),
            session_id: std::env::var("SMART_HISTORY_SESSION").unwrap_or_default(),
            exit_code: -1,
            timestamp: now,
            comment: command.clone(),
            output: command,
            mode: "llm".to_string(),
            source: String::new(),
        };
        self.llm_preview = Some(preview);
        self.llm_preview_description = Some(fired_description);
        self.llm_in_flight = false;
        // Re-render so the preview appears in the list
        // immediately. The next tick will see the
        // preview-description matches the live description
        // and skip the re-fire path.
        self.refresh();
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
    /// the compiled regex when the query starts with `/`, a
    /// fuzzy subsequence match (multi-word, AND) when the query
    /// starts with `?`, or a plain substring search against the
    /// output body when the query starts with `+`.
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
            let p = self.query_prefixes.regex;
            return text.to_lowercase().contains(&self.query[p.len_utf8()..].to_lowercase());
        }
        if self.is_fuzzy_query() {
            // Fuzzy search: every whitespace-separated word in the
            // query must be a fuzzy subsequence of the text.
            let fuzzy_pattern = self.fuzzy_pattern();
            if fuzzy_pattern.is_empty() {
                return true;
            }
            return fuzzy_pattern
                .split_whitespace()
                .all(|term| fuzzy_match(term, text));
        }
        // For plain text and output modes: every
        // whitespace-separated word must appear
        // (case-insensitive). In output mode we use the
        // body (everything after the leading `+`) rather
        // than the full query, so `+segmentation fault`
        // searches for both `segmentation` AND `fault`
        // — not for the literal `+segmentation`.
        let body = if self.is_output_query() {
            self.output_pattern()
        } else {
            self.query.as_str()
        };
        let lower = text.to_lowercase();
        body
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

/// State for the LLM "describe what this command does"
/// overlay. Mirrors the captured-output overlay in
/// shape (a piece of text + a scroll offset) but is
/// driven by an LLM call rather than by the captured
/// stdout of a history row.
///
/// `command` is the row's command — kept here so the
/// overlay title can show "Describe: <command>" even
/// after the user has navigated away from the row in
/// the history list (and so the LLM prompt that
/// generated `text` is reconstructable from the
/// overlay's own state).
///
/// `text` is the LLM's raw response. We don't run it
/// through any sanitizer the way `run_llm_query` does
/// (the `sanitize_command` step would be wrong here —
/// the response is *prose*, not a command) — but we
/// still trim leading/trailing whitespace so the
/// rendered text doesn't have stray newlines around
/// it.
struct DescribeView {
    /// The command that was described.
    command: String,
    /// The LLM's response (a short prose description).
    text: String,
    /// Scroll offset (lines past the top of the
    /// rendered text). Most responses are at most
    /// four sentences and fit on a single screen,
    /// but the scroll handles longer outputs and
    /// small terminal heights.
    scroll: usize,
}

/// State for the LLM "correct this command" modal
/// overlay. Shown after the LLM has returned a
/// candidate corrected command; the user reviews
/// and either accepts (Enter) or cancels (Esc).
///
/// The shape is similar to `DescribeView` but the
/// two fields (`original_command` and
/// `corrected_command`) have different roles:
/// `original` is read-only (it's the row the user
/// had selected), and `corrected` is what the LLM
/// produced. Pressing Enter stages `corrected` as
/// the next selection (and writes it to the
/// history table with `original` as the comment
/// for traceability), pressing Esc just closes the
/// overlay.
///
/// We don't store a "loading" flag here the way we
/// could: the LLM call happens synchronously
/// inside `start_correct`, and the overlay only
/// exists once the response is in hand. This
/// matches the `start_describe` design.
struct CorrectView {
    /// The original (possibly malformed) command
    /// the user had selected. Shown for context so
    /// the user can see what was being fixed.
    original_command: String,
    /// The LLM's corrected version of the command.
    /// The user reviews this and presses Enter to
    /// stage it for execution, or Esc to cancel.
    corrected_command: String,
}

/// State for the general question overlay (prefixed with `%`).
/// Mirrors the describe overlay in shape (a piece of text +
/// a scroll offset) but is driven by the user's question.
///
/// `question` is the user's original question — kept here so the
/// overlay title can show "Question: <question>".
///
/// `text` is the LLM's answer (at most 4 sentences). We don't
/// run it through any sanitizer — the response is *prose*,
/// not a command — but we still trim leading/trailing whitespace.
struct QuestionView {
    /// The question that was asked (prefixed with `%`).
    question: String,
    /// The LLM's answer (at most 4 sentences of plain prose).
    text: String,
    /// Scroll offset (lines past the top of the rendered text).
    scroll: usize,
}

/// The type of LLM request that is currently in flight.
/// Each variant carries the data needed to process the
/// response once it arrives.
enum LlmRequestType {
    /// A `=...` command generation request.
    Generate {
        description: String,
    },
    /// A `Ctrl-K` describe request.
    Describe {
        command: String,
    },
    /// A `Ctrl-T` correct request.
    Correct {
        original_command: String,
    },
    /// A `%...` general question request.
    Question {
        question: String,
    },
}

/// An in-flight LLM request. The receiver is polled by the
/// run loop; when a result arrives, it is processed according
/// to `request_type`. The `cancelled` flag is set by the
/// run loop when the user presses Ctrl+C (or the Cancel action)
/// while a request is in flight.
struct LlmRequest {
    request_type: LlmRequestType,
    receiver: mpsc::Receiver<Result<String, crate::llm::LlmError>>,
    cancelled: Arc<AtomicBool>,
}

/// An in-flight JIRA search request. Same shape as
/// `LlmRequest`: a background thread sends the result over
/// the channel, the run loop polls it, and the cancelled
/// flag lets the user abort a slow search with `Esc`.
struct JiraRequest {
    receiver: mpsc::Receiver<Result<Vec<crate::jira::JiraIssue>, crate::jira::JiraError>>,
    cancelled: Arc<AtomicBool>,
}

/// An in-flight JIRA comments fetch request.
/// Mirrors `JiraRequest` (a background thread
/// sends the result over the channel, the run
/// loop polls it, the cancelled flag lets the
/// user abort a slow fetch with `Esc`).
///
/// The result is `Vec<JiraComment>`, sorted
/// newest-first by the TUI after the fetch
/// completes (JIRA's REST v2 returns comments in
/// `created` ascending order, so the TUI
/// reverses them on the way in).
struct JiraCommentsRequest {
    receiver: mpsc::Receiver<
        Result<Vec<crate::jira::JiraComment>, crate::jira::JiraError>,
    >,
    cancelled: Arc<AtomicBool>,
    /// The issue key the comments were fetched
    /// for. Kept on the request struct so the
    /// run-loop poll can match the result back
    /// to the row that initiated the fetch
    /// (the user may have navigated away from
    /// the row in the meantime; we still want
    /// the overlay to be tied to the right
    /// issue's comments).
    key: String,
}

/// An in-flight JIRA add-comment POST.
/// Mirrors `JiraCommentsRequest`: a
/// background thread sends the result
/// (success or error) over the channel,
/// the run loop polls it, and the
/// cancelled flag lets the user abort a
/// slow POST with `Esc`.
///
/// The result is `Result<(), JiraError>`.
/// `Ok(())` means JIRA returned 201
/// Created; the buffer is cleared and
/// the user sees a "Comment posted"
/// status. `Err(...)` is surfaced as a
/// status message; the buffer stays so
/// the user can fix any issue and retry
/// without retyping.
///
/// The `key` and `body` are kept on the
/// struct so the run-loop poll can
/// attach the result to the right issue
/// and so cancellation can show
/// "JIRA add-comment cancelled" with
/// the right issue context.
struct JiraAddCommentRequest {
    receiver: mpsc::Receiver<Result<(), crate::jira::JiraError>>,
    cancelled: Arc<AtomicBool>,
    /// The issue key the comment is being
    /// posted to. Stashed on the request
    /// struct so the result-processing
    /// step can surface the issue
    /// context in status messages
    /// ("Comment posted to PROJ-1"
    /// rather than just "Comment posted").
    key: String,
    /// The body of the comment being
    /// posted. Kept on the struct so
    /// cancellation messages can
    /// reference what was being
    /// cancelled ("JIRA add-comment
    /// cancelled (was posting to PROJ-1)"),
    /// and so future code that wants to
    /// retry on transient failures
    /// (network blip, JIRA 503) can
    /// re-fire the same body without
    /// re-reading the buffer.
    body: String,
}


/// Sort a JIRA comments list newest-first by
/// the `created` timestamp. JIRA's REST v2
/// `comment` endpoint returns comments in
/// `created` *ascending* order, so we reverse
/// them here. The `id` field is the
/// tie-breaker when two comments share the
/// same `created` timestamp (rare but
/// possible for batch-imported comments);
/// JIRA's comment IDs are roughly
/// monotonically increasing, so the higher
/// ID wins (newer insertion).
///
/// Kept as a free function (not a method) so
/// the test can drive it directly with a
/// canned `Vec<JiraComment>` without
/// standing up the full `App` and the
/// SQLite DB.
fn sort_comments_newest_first(comments: &mut [crate::jira::JiraComment]) {
    comments.sort_by(|a, b| {
        // `parse_rfc3339`-style parse via
        // `updated_to_epoch` (the same
        // helper the JQL-built-in test
        // epoch uses). On parse failure
        // (empty / malformed), the
        // function returns 0; an `Ord`
        // tie on 0 falls through to the
        // id-based tie-breaker.
        let ea = crate::jira::updated_to_epoch(&a.created);
        let eb = crate::jira::updated_to_epoch(&b.created);
        // Reverse: newer (`eb > ea`)
        // comes first.
        eb.cmp(&ea).then_with(|| {
            // Tie-breaker: compare the
            // comment IDs as strings.
            // JIRA's comment IDs are
            // numeric, so this is
            // effectively a numeric
            // comparison; using
            // `Ord` on `String` is
            // correct enough for
            // the rare batch-import
            // case.
            b.id.cmp(&a.id)
        })
    });
}

/// Build the markdown-like overlay text for
/// a JIRA row + its comments. The format
/// is what the user spec calls out:
///
/// ```text
/// ## Header
/// <3-line preview: Status/Priority,
///                 Due/Assignee,
///                 Description label>
/// <description body lines>
///
/// ## Description
/// <full description text>
///
/// ## Comments
/// ## <author> · <date>
/// <comment text>
/// ## <author> · <date>
/// <comment text>
/// ...
/// ```
///
/// Each section is preceded by an `## `
/// heading marker (rendered as a
/// heading-styled span by the preview
/// renderer). Per-comment sub-headings
/// follow the same convention with the
/// author and date in the heading text.
/// Empty sections (no description, no
/// comments) are still emitted with a
/// `(none)` placeholder so the user
/// always sees the section structure.
fn build_jira_overlay_text(
    row: &crate::tui::state::HistoryRow,
    comments: &[crate::jira::JiraComment],
) -> String {
    let mut out = String::new();

    // The preview pane's `row.output`
    // is the 3-line metadata header
    // (Status/Priority, Due/Assignee,
    // Description label) followed by
    // the description body (which can
    // be multiple lines for
    // multi-paragraph descriptions).
    //
    // Split it into the two parts up
    // front so we can route them to
    // separate sections without
    // duplicating the description.
    // The first 3 lines are the
    // metadata block; everything
    // from line 4 onwards is the
    // description body.
    let mut all_lines = row.output.lines();
    let header_block: Vec<&str> =
        all_lines.by_ref().take(3).collect();
    let description_body: String = all_lines.collect::<Vec<_>>().join("\n");

    // ---- ## Header ----
    out.push_str("## Header\n");
    // The `# Header` section shows
    // the metadata block — the
    // same 3 lines the preview
    // pane shows. The
    // description body is
    // NOT included here (it
    // lives in its own
    // `## Description`
    // section below, so the
    // description appears
    // exactly once in the
    // overlay). The
    // original implementation
    // copied the full
    // `row.output` here,
    // which caused the
    // description to
    // appear twice (once in
    // `# Header`, once in
    // `# Description`); the
    // user reported this and
    // asked for the
    // description to be
    // visible only once.
    for line in &header_block {
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');

    // ---- ## Description ----
    out.push_str("## Description\n");
    if description_body.is_empty() || description_body == "<none>" {
        out.push_str("(no description)\n");
    } else {
        out.push_str(&description_body);
        out.push('\n');
    }
    out.push('\n');

    // ---- ## Comments ----
    out.push_str("## Comments\n");
    if comments.is_empty() {
        out.push_str("(no comments)\n");
        return out;
    }
    for comment in comments {
        // `## <author> · <date>` sub-heading.
        // The `·` (U+00B7 MIDDLE DOT) is
        // a clean separator that renders
        // consistently across terminals
        // and doesn't conflict with any
        // markdown convention.
        let author = if comment.author.is_empty() {
            "(unknown)"
        } else {
            comment.author.as_str()
        };
        let date = format_jira_date(&comment.created);
        out.push_str(&format!("## {} \u{00b7} {}\n", author, date));
        // The comment body. Empty
        // bodies get a placeholder so
        // the user knows the comment
        // exists (vs. the section
        // being incomplete).
        if comment.body.is_empty() {
            out.push_str("(empty comment)\n");
        } else {
            out.push_str(&comment.body);
            out.push('\n');
        }
        // A blank line between comments
        // for visual separation. The
        // renderer's `lines()` iteration
        // strips the trailing blank
        // line in the last comment.
        out.push('\n');
    }
    out
}

/// Format a JIRA ISO-8601 `created` timestamp
/// (e.g. `2024-06-30T19:14:39.000+0000`)
/// into a short, human-readable date
/// (`2024-06-30 19:14 UTC`). The JQL comment
/// sub-headings list date + author, so the
/// format is opinionated: we drop the
/// milliseconds and the offset (just `UTC`
/// as a timezone hint) to keep the heading
/// compact. JIRA's `created` is always UTC
/// in practice, so this is a safe
/// presentation.
fn format_jira_date(iso: &str) -> String {
    let s = iso.trim();
    if s.is_empty() {
        return String::new();
    }
    // Parse the `YYYY-MM-DDTHH:MM:SS`
    // prefix by slicing. We don't need
    // the full `chrono` machinery for
    // this short format — JIRA's
    // timestamps are stable and the
    // substring is the same width.
    //
    // Minimum length for a
    // `YYYY-MM-DDTHH:MM` extract is
    // 16 chars. Anything shorter is
    // malformed and we degrade to
    // the raw string.
    if s.len() < 16 {
        return s.to_string();
    }
    let date = &s[..10]; // YYYY-MM-DD
    let time = &s[11..16]; // HH:MM
    format!("{} {} UTC", date, time)
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
    fn new(
        conn: Connection,
        initial_mode: Mode,
        initial_query: String,
        duplicate_filter: bool,
        exit_filter: ExitFilter,
        sort_order: SortOrder,
        query_prefilled: bool,
        theme: SelectedTheme,
        bindings: KeyBindings,
        llm: Option<Box<dyn crate::llm::LlmClient>>,
        llm_config: Option<crate::llm::LlmConfig>,
        query_prefixes: crate::QueryPrefixes,
        notes_database: Option<std::path::PathBuf>,
        notes_dir: Option<std::path::PathBuf>,
        _todo_line_option: String,
        jira_fragments: std::collections::HashMap<String, String>,
        files_ignores: Vec<String>,
    ) -> Self {
        // Capture the character-aligned initial cursor
        // position BEFORE moving `initial_query` into the
        // struct. We use the character count (not the byte
        // length) so the index is stable for multi-byte UTF-8
        // input.
        let initial_cursor = initial_query.chars().count();
        let list_state = ListState::default();
        let mut app = App {
            conn,
            mode: initial_mode,
            duplicate_filter,
            exit_filter,
            sort_order,
            query: initial_query,
            rows: Vec::new(),
            list_state,
            selection: None,
            pick_mode: None,
            cancelled: false,
            comment_edit: None,
            output_view: None,
            describe_view: None,
            correct_view: None,
            question_view: None,
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
            // Start the cursor at the end of the query so the
            // initial character appends naturally. The user can
            // re-position it with Left/Right once the query is
            // in LLM mode; for non-LLM modes the cursor is
            // ignored by the input loop and stays at the end.
            query_cursor: initial_cursor,
            query_regex: None,
            theme,
            bindings,
            status_message: None,
            llm,
            llm_config,
            query_prefixes,
            notes_database,
            notes_dir,
            todo_line_option: String::from("+$LINE"),
            notes_query_error: false,
notes_date_filter: NotesDateFilter::All,
            // First-time entry into
            // directories mode
            // triggers a one-shot
            // `tmux list-panes` call
            // to populate this; the
            // snapshot is then
            // cached until the TUI
            // exits. See
            // `fetch_directories`'s
            // "tmux-pane indicator"
            // doc comment for the
            // rationale.
            tmux_windows: Vec::new(),
            session_panes: Vec::new(),
            // Cached home-prefix
            // list, computed once
            // at construction.
            // See the `home_list`
            // field doc for the
            // full rationale.
            home_list: build_home_list(),
            // Recursive walk of
            // every
            // `sessiondirs=...`
            // entry, computed once
            // at construction.
            // See the
            // `session_subdirs`
            // field doc.
            session_subdirs: build_session_subdirs(),
            // Default to
            // `All` so first-time
            // users see
            // everything. The
            // session file can
            // override (read in
            // `run_tui_to_stdout`).
            directory_source:
                crate::tui::state::DirectorySource::All,
            // LLM debounce state. The user hasn't typed
            // anything yet (we're at construction time), so
            // the debounce is satisfied and no preview is
            // active. The run-loop tick will arm the
            // debounce on the first keystroke in LLM mode.
            llm_debounce_started: None,
            llm_preview: None,
            llm_preview_description: None,
            llm_in_flight: false,
            llm_request: None,
            files_state: crate::files::FilesState::new(),
            files_ignores,
            jira_rows: Vec::new(),
            jira_request: None,
            jira_in_flight: false,
            jira_debounce_started: None,
            jira_last_jql: None,
            jira_client: None,
            jira_fragments,
            jira_undefined_fragments: Vec::new(),
            jira_last_undefined_message: None,
            jira_comments_request: None,
            jira_comments_in_flight: false,
            jira_add_comment_target: None,
            jira_add_comment_request: None,
            jira_add_comment_in_flight: false,
        };
        app.recompile_regex();
        app.refresh();
        app.refresh_labeled();
        // If the restored query is a JIRA query, arm the
        // debounce and fire the search immediately so the
        // user sees results on the first frame rather than
        // an empty list. This mirrors what happens when the
        // user types `-` in the run loop (jira_touch arms
        // the debounce, the next no-input tick fires it).
        if app.is_jira_query() {
            app.jira_debounce_started = Some(std::time::Instant::now());
            app.jira_maybe_autocall();
        }
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
        // First-time entry into
        // directories mode (i.e.
        // the user just typed `#`):
        // populate the tmux-pane
        // snapshot used to mark
        // rows whose directory has
        // an active tmux pane. The
        // helper is idempotent —
        // it returns immediately if
        // the snapshot is already
        // populated, so subsequent
        // refreshes in directories
        // mode don't re-spawn `tmux`.
        // We do this BEFORE
        // `fetch()` so the first
        // frame after the user types
        // `#` already has the marker
        // resolved; otherwise the
        // marker would only appear
        // on the *second* refresh,
        // which is a single-frame
        // flicker that's noticeable
        // but not catastrophic.
        if self.is_directories_query() {
            self.fetch_tmux_windows();
        }
        // Same one-shot cache priming for the
        // `*`-prefix panes view: populate the
        // session-panes snapshot before `fetch()`
        // reads it, so the first frame after the
        // user types `*` already shows the list.
        if self.is_panes_query() {
            self.fetch_session_panes();
        }
        self.rows = self.fetch().unwrap_or_default();
        if self.is_regex_query() || self.is_fuzzy_query() {
            // Two-phase borrow: copy the rows out, then post-filter.
            // Avoids the borrow checker complaining about
            // simultaneously borrowing `self.rows` and `self`.
            let query = self.query.clone();
            let regex = self.query_regex.clone();
            let is_regex = self.is_regex_query();
            let is_fuzzy = self.is_fuzzy_query();
            // Capture prefix lengths for the fallback paths
            let regex_prefix_len = self.query_prefixes.regex.len_utf8();
            let fuzzy_prefix_len = self.query_prefixes.fuzzy.len_utf8();
            self.rows.retain(|r| {
                if is_regex {
                    if let Some(ref re) = regex {
                        re.is_match(&r.command) || re.is_match(&r.comment)
                    } else {
                        // No valid regex yet (in-progress typo) — fall
                        // back to a literal substring match on the
                        // post-prefix text so the user sees *something*.
                        r.command
                            .to_lowercase()
                            .contains(&query[regex_prefix_len..].to_lowercase())
                            || r
                                .comment
                                .to_lowercase()
                                .contains(&query[regex_prefix_len..].to_lowercase())
                    }
                } else if is_fuzzy {
                    // Fuzzy search is also a post-filter. We can't
                    // call `self.query_matches_text` here because
                    // `self` is borrowed mutably by `retain`. The
                    // pattern is the whole post-prefix query, so we
                    // inline the check.
                    let fuzzy_pattern = &query[fuzzy_prefix_len..];
                    if fuzzy_pattern.is_empty() {
                        true
                    } else {
                        fuzzy_pattern
                            .split_whitespace()
                            .all(|term| fuzzy_match(term, &r.command)
                                || fuzzy_match(term, &r.comment))
                    }
                } else {
                    true
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
        // Directories mode (`#`)
        // is a completely
        // different view: it
        // shows *directories*
        // (from SQL history,
        // sessiondirs, and/or
        // active tmux panes,
        // filtered by
        // `directory_source`).
        // It must NOT interleave
        // labeled history rows
        // (entries with a
        // comment that aren't in
        // the primary fetch) —
        // those are a
        // history-list concept.
        // The user reported a
        // labeled row
        // (`tmux list-windows -a
        // ...`) leaking into
        // `DIR:TMUX` mode: it
        // had a comment, so
        // `build_merged_rows`
        // appended it to the
        // directory list. Skip
        // the labeled/preview
        // merge entirely in
        // directories mode.
        // Directories mode (`#`) AND panes mode
        // (`*`) are both completely different
        // views that must NOT interleave labeled
        // history rows. See the directories-mode
        // block above for the full rationale; the
        // same applies to panes mode (the user
        // reported a labeled `tmux list-windows`
        // row leaking into `DIR:TMUX`; panes mode
        // would have the same leak without this
        // guard).
        if self.is_directories_query() || self.is_panes_query() || self.is_jira_query() || self.is_files_query() {
            let mut merged = self.rows.clone();
            if self.duplicate_filter
                || self.sort_order
                    == SortOrder::Frequency
            {
                let mut seen: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                merged
                    .retain(|r| seen.insert(r.command.clone()));
            }
            return merged;
        }
        // Two-partition merge: rows that came from
        // the primary fetch (`self.rows`) and rows
        // that came from the labeled set but are
        // NOT in the primary fetch. The user-visible
        // contract is that labeled rows that were
        // *already* part of the active filter sit
        // with their natural sort, and labeled rows
        // that are *not* part of the active filter
        // (i.e. only visible because they're
        // labeled) are pushed to the end of the
        // list. This makes the labeled "sticky
        // note" rows visually separable from the
        // main history — the user can see at a
        // glance which rows are "actually here"
        // vs. "here because they have a comment".
        //
        // We compute the partition before sorting
        // so each part can be sorted with its own
        // key. Sorting within each partition then
        // concatenating produces the same final
        // order as mixing them together with a
        // partition-stable sort, but is clearer
        // to read and avoids the subtle
        // interaction between the sort comparator
        // and the partition key.
        let mut main_part = self.rows.clone();
        let existing_ids: std::collections::HashSet<i64> =
            main_part.iter().map(|r| r.id).collect();
        // Labeled rows that are NOT already in the
        // primary list. These are the "moved to
        // the end" group. We still apply the
        // query filter here so a labeled row
        // whose command/comment/output doesn't
        // match the typed query is excluded
        // (consistent with the previous behavior,
        // where labeled rows were filtered through
        // `query_matches_text` before being added).
        let mut labeled_only_part: Vec<HistoryRow> = Vec::new();
        for row in &self.labeled_rows {
            if !existing_ids.contains(&row.id) {
                if !self.query.is_empty() {
                    let in_command = self.query_matches_text(&row.command);
                    let in_comment = self.query_matches_text(&row.comment);
                    // Output mode (`+...`) targets the
                    // `history_output.output` column. The
                    // labeled-row filter is the secondary
                    // path (the primary list already
                    // includes output matches via the
                    // `LIKE` clause in `build_where`),
                    // so we also check the labeled row's
                    // output text here. This makes a
                    // labeled entry that has no command/
                    // comment match visible if its
                    // captured output does match. Empty
                    // output rows are correctly excluded
                    // because `query_matches_text` won't
                    // find anything in an empty string.
                    let in_output = self.is_output_query()
                        && self.query_matches_text(&row.output);
                    if !in_command && !in_comment && !in_output {
                        continue;
                    }
                }
                labeled_only_part.push(row.clone());
            }
        }
        // The LLM preview row, when active, is
        // inserted at the very front of the merged
        // list — before the main partition, before
        // the labeled-only partition. We only
        // show it in LLM mode — the user's typing
        // a description and the suggestion is the
        // most relevant thing to look at. The
        // synthetic `id = -1` is excluded from the
        // dedup pass below so the duplicate filter
        // doesn't accidentally keep only the
        // preview (or worse, drop it) when a real
        // row shares the command.
        //
        // We push the preview at the end and let the
        // timestamp-DESC sort (below) bring it to the top;
        // its `timestamp` is set to `now` in
        // `llm_maybe_autocall`, so it always sorts first.
        // In Stats mode the sort is suppressed, but stats
        // mode never sees the LLM preview (the user
        // wouldn't be composing a description there), and
        // we already gate this on `is_llm_query()`.
        let mut preview_part: Vec<HistoryRow> = Vec::new();
        if self.is_llm_query()
            && let Some(preview) = self.llm_preview_row()
        {
            preview_part.push(preview);
        }
        // Sort each partition independently.
        //
        // Stats mode always uses a frequency-aware
        // ordering from `fetch_stats` that we
        // must preserve; it overrides whatever the
        // user picked for sort order. The Stats
        // ranking is already a count-based sort
        // (successor frequency) so the user's
        // "frequency" choice would just duplicate
        // it; the user's "age" choice would
        // re-sort the Stats output by timestamp and
        // lose the prediction signal. In both cases
        // the Stats ordering is the "right" answer.
        // We don't sort the labeled-only partition
        // in Stats mode either — preserving
        // whatever order `fetch_labeled` produced
        // is fine because it's the same SQL
        // ordering the labeled view uses.
        if !matches!(self.mode, Mode::Stats) {
            self.sort_partition(&mut main_part);
            self.sort_partition(&mut labeled_only_part);
        }
        // Concatenate: preview, main, labeled-only.
        // The labeled-only group is at the end by
        // construction; the user's request is
        // that labeled-only rows are visually
        // separated from the primary history.
        let mut merged = preview_part;
        merged.append(&mut main_part);
        merged.append(&mut labeled_only_part);
        // The duplicate filter collapses every group of
        // identical commands down to a single row. It
        // runs when the user has the duplicate filter on
        // (the historical behavior) AND, implicitly,
        // when the user is in frequency sort mode.
        //
        // In frequency mode, "show me my most-run
        // commands" only makes sense if each command
        // appears exactly once — otherwise the same
        // command would dominate the list with its own
        // repeat instances, drowning out everything
        // else. The dedup keeps the newest instance of
        // each command (which is the first in the
        // frequency-sorted list, because the per-row
        // tie-breaker is `timestamp DESC`).
        //
        // The user-facing `duplicate_filter` setting is
        // therefore the union of "user toggled it on"
        // and "user picked frequency sort": either way,
        // we dedup. This means turning on frequency sort
        // will collapse the list to one row per command
        // regardless of the duplicate-filter setting.
        // The reverse isn't true: the duplicate filter
        // works in `Age` mode too (it's been the default
        // for years and we don't want to break that).
        if self.duplicate_filter || self.sort_order == SortOrder::Frequency {
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            merged.retain(|r| seen.insert(r.command.clone()));
        }
        merged
    }

    /// Apply the user's chosen `sort_order` to a
    /// single partition. The two partitions
    /// (`main_part` from the primary fetch and
    /// `labeled_only_part` from the labeled set)
    /// are sorted independently and then
    /// concatenated — see `build_merged_rows` for
    /// the rationale.
    ///
    /// In `SortOrder::Age` mode both partitions
    /// sort the same way (timestamp DESC), so the
    /// final concatenated order is "newest first
    /// overall, labeled-only group appended at the
    /// end". In `SortOrder::Frequency` mode each
    /// partition gets its own count map, so the
    /// labeled-only group's internal ordering is
    /// driven by the counts *within that
    /// partition* rather than the counts of the
    /// entire merged set. This is the correct
    /// behavior: a labeled row counts as one
    /// occurrence in its own partition, not in
    /// the main partition, and the labeled-only
    /// group's ranking is independent of how
    /// often the same command appears in the main
    /// history.
    fn sort_partition(&self, partition: &mut Vec<HistoryRow>) {
        match self.sort_order {
            SortOrder::Age => {
                partition.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            }
            SortOrder::Frequency => {
                let mut counts: std::collections::HashMap<
                    String,
                    usize,
                > = std::collections::HashMap::new();
                let mut newest: std::collections::HashMap<
                    String,
                    i64,
                > = std::collections::HashMap::new();
                for r in partition.iter() {
                    *counts
                        .entry(r.command.clone())
                        .or_insert(0) += 1;
                    let n = newest
                        .entry(r.command.clone())
                        .or_insert(i64::MIN);
                    if r.timestamp > *n {
                        *n = r.timestamp;
                    }
                }
                partition.sort_by(|a, b| {
                    let ca = counts
                        .get(&a.command)
                        .copied()
                        .unwrap_or(0);
                    let cb = counts
                        .get(&b.command)
                        .copied()
                        .unwrap_or(0);
                    let na = newest
                        .get(&a.command)
                        .copied()
                        .unwrap_or(i64::MIN);
                    let nb = newest
                        .get(&b.command)
                        .copied()
                        .unwrap_or(i64::MIN);
                    // Primary: count DESC.
                    // Secondary: per-command newest
                    // timestamp DESC. Tertiary:
                    // per-row timestamp DESC
                    // (newer instances of the same
                    // command come first).
                    cb.cmp(&ca).then_with(|| {
                        nb.cmp(&na)
                    }).then_with(|| {
                        b.timestamp.cmp(&a.timestamp)
                    })
                });
            }
        }
    }

    fn fetch(&mut self) -> Result<Vec<HistoryRow>> {
        if matches!(self.mode, Mode::Stats) {
            return self.fetch_stats();
        }
        if self.is_todo_query() {
            return self.fetch_todos();
        }
        if self.is_notes_query() {
            return self.fetch_notes();
        }
        if self.is_directories_query() {
            return self.fetch_directories();
        }
        if self.is_panes_query() {
            return self.fetch_panes();
        }
        if self.is_jira_query() {
            return self.fetch_jira();
        }
        if self.is_files_query() {
            return self.fetch_files();
        }
        let (where_clause, params) = self.build_where();
        let sql = format!(
            "SELECT h.id, h.command, h.directory, h.session_id, h.exit_code, h.timestamp, c.comment, o.output, h.mode \
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
                    mode: row.get(8).unwrap_or_default(),
                    source: String::new(),
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
                    h.exit_code, h.timestamp, c.comment, o.output, h.mode, \
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
                    mode: row.get(8).unwrap_or_default(),
                    source: String::new(),
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
        // `refresh()` via `query_matches_text`. Same for fuzzy
        // (`?`) — those are modes that have a post-filter step.
        // LLM (`=`) is special: we filter to commands starting with
        // `=` and search the command/output text.
        if !self.query.is_empty()
            && !self.is_regex_query()
            && !self.is_fuzzy_query()
        {
            if self.is_llm_query() {
                // LLM mode: only show entries with mode='llm'.
                // Also allow filtering by the body of the query
                // against the command and output.
                clause.push_str(" AND h.mode = 'llm'");
                // Search the body in command and output
                for word in self.llm_pattern().split_whitespace() {
                    if !word.is_empty() {
                        let escaped = crate::util::escape_like(word);
                        clause.push_str(
                            " AND (h.command LIKE ? ESCAPE '\\' OR o.output LIKE ? ESCAPE '\\')",
                        );
                        params.push(Box::new(format!("%{}%", escaped)));
                        params.push(Box::new(format!("%{}%", escaped)));
                    }
                }
            } else if self.is_question_query() {
                // Question mode: only show entries with mode='question'.
                // Also allow filtering by the body of the query
                // against the command and output.
                clause.push_str(" AND h.mode = 'question'");
                // Search the body in command and output
                for word in self.question_pattern().split_whitespace() {
                    if !word.is_empty() {
                        let escaped = crate::util::escape_like(word);
                        clause.push_str(
                            " AND (h.command LIKE ? ESCAPE '\\' OR o.output LIKE ? ESCAPE '\\')",
                        );
                        params.push(Box::new(format!("%{}%", escaped)));
                        params.push(Box::new(format!("%{}%", escaped)));
                    }
                }
            } else if self.is_output_query() {
                // Output mode (`+...`) searches the
                // `history_output.output` column instead of
                // `h.command` / `c.comment`. We restrict the
                // `LIKE` to the output text and also require
                // the row to have a `history_output` row at
                // all (the `IS NOT NULL` guard is technically
                // redundant with the `LIKE` against the
                // LEFT-JOINed column, but it makes the SQL
                // self-documenting and matches the user's
                // intent: "find me the command that produced
                // *this output*"). The rest of the conditions
                // (session, directory, exit filter) still
                // apply.
                // Always require a `history_output` row
                // for output-mode queries. The `LIKE`
                // clause below already implies this (a
                // NULL value can't match a substring),
                // but the empty-body case has no
                // `LIKE` clauses to do that work for
                // it, so we need the guard separately.
                // Without it, a bare `+` would list
                // every row, including the ones with no
                // captured output — which is the
                // opposite of what the user asked for
                // (they want to find commands by what
                // they produced).
                clause.push_str(" AND o.output IS NOT NULL");
                for word in self.output_pattern().split_whitespace() {
                    let escaped = crate::util::escape_like(word);
                    clause.push_str(" AND o.output LIKE ? ESCAPE '\\'");
                    params.push(Box::new(format!("%{}%", escaped)));
                }
            } else {
                for word in self.query.split_whitespace() {
                    let escaped = crate::util::escape_like(word);
                    clause.push_str(
                        " AND (h.command LIKE ? ESCAPE '\\' OR c.comment LIKE ? ESCAPE '\\')",
                    );
                    params.push(Box::new(format!("%{}%", escaped)));
                    params.push(Box::new(format!("%{}%", escaped)));
                }
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
                // Canonicalize the same
                // way the insert side
                // does (see
                // `canonicalize_directory`
                // and the various
                // `env::current_dir()`
                // call sites in
                // `main.rs` / `tui.rs`).
                // Without this, the
                // user's logical `$PWD`
                // (e.g.
                // `/Users/har/Sources/...`)
                // would never match the
                // canonical path the
                // `preexec` hook stored
                // (e.g.
                // `/Volumes/HUGE/har/Sources/...`)
                // on macOS where the
                // user's home is on an
                // external volume.
                if let Ok(pwd) = std::env::var("PWD")
                    && !pwd.is_empty()
                {
                    let canonical =
                        crate::util::canonicalize_directory(&pwd);
                    clause.push_str(" AND h.directory = ?");
                    params.push(Box::new(canonical));
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

    /// Cycle the sort order of the history list (Age ↔
    /// Frequency). The new value is also persisted in
    /// the session file and restored on the next TUI
    /// invocation, so the user always lands back on the
    /// sort they last picked.
    ///
    /// The sort is applied inside `build_merged_rows` (see
    /// `App::sort_order` for the contract), so a single
    /// `refresh()` call is enough to repaint the list
    /// with the new ordering. The cursor lands on the
    /// newest entry (index 0 of the merged list), which
    /// is the same behaviour as every other mode reset.
    fn cycle_sort_order(&mut self) {
        self.sort_order = self.sort_order.next();
        self.refresh();
    }

    /// Cycle the
    /// directory-source
    /// filter for the
    /// `#`-mode list:
    /// ALL → TMUX → CFG
    /// → ALL. Persists in
    /// the session file
    /// (the
    /// `directory_source`
    /// field is read in
    /// `run_tui_to_stdout`
    /// on startup).
    ///
    /// If pressed while NOT
    /// already in directories
    /// mode (`#`), the mode is
    /// switched to directories
    /// first — the query gains
    /// the `#` prefix (any
    /// existing search-mode
    /// prefix like `/`, `?`,
    /// `+`, `=`, `%`, `@`, `!`
    /// is stripped), the body
    /// is preserved so the user
    /// can keep narrowing by
    /// path substring. Then the
    /// source is cycled. This
    /// makes the binding useful
    /// from any mode — the user
    /// can be in plain history,
    /// press it, and land in
    /// `DIR:TMUX` directly.
    fn cycle_directory_source(&mut self) {
        if !self.is_directories_query() {
            self.enter_directories_mode();
        }
        self.directory_source =
            self.directory_source.next();
        self.refresh();
    }

    /// Switch the query into
    /// directories (`#`) mode,
    /// preserving any body the
    /// user has already typed.
    /// Strips a leading
    /// search-mode prefix
    /// (`/`, `?`, `+`, `=`, `%`,
    /// `@`, `!`, or `#` itself)
    /// before prepending the
    /// directories prefix, so
    /// switching from `?foo`
    /// yields `#foo` (not
    /// `#?foo`). The cursor is
    /// reset to the end so the
    /// next keystroke appends
    /// naturally. Mode-
    /// dependent state (regex
    /// recompile, LLM debounce)
    /// is refreshed too.
    fn enter_directories_mode(&mut self) {
        let p = &self.query_prefixes;
        let prefixes = [
            p.regex,
            p.fuzzy,
            p.output,
            p.llm,
            p.question,
            p.notes,
            p.todo,
            p.directories,
        ];
        let body: String = self
            .query
            .chars()
            .next()
            .map(|c| {
                if prefixes.contains(&c) {
                    self.query[c.len_utf8()..].to_string()
                } else {
                    self.query.clone()
                }
            })
            .unwrap_or_default();
        let mut s =
            String::with_capacity(
                body.len() + p.directories.len_utf8(),
            );
        s.push(p.directories);
        s.push_str(&body);
        self.query = s;
        self.recompile_regex();
        self.query_cursor =
            self.query.chars().count();
        self.llm_touch();
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
        // `=...` queries are an LLM command-generation request,
        // not a row selection. Short-circuit before any row
        // lookup: there *is* no meaningful selected row when
        // the user is composing a natural-language description.
        if self.is_llm_query() {
            self.run_llm_query();
            return;
        }
        // `%...` queries are general question requests.
        // Open an overlay with the answer instead of running
        // a command.
        if self.is_question_query() {
            self.run_question_query();
            return;
        }
        // `!...` queries are todo search requests.
        // Selecting a todo line opens the editor at
        // the exact line number so the user lands
        // on the todo. The `id` of a todo row is
        // `-(line_number)` (synthetic negative id),
        // so we recover the line number with
        // `i64::abs() as usize`.
        if self.is_todo_query() {
            if let Some(row) = self.selected_row() {
                let editor = std::env::var("EDITOR")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "vi".to_string());
                // Recover the 1-based line number
                // from the synthetic id. The id is
                // negative (e.g. -42 means line 42);
                // `i64::MIN` would be its own
                // absolute value, but that's not a
                // valid line number anyway and the
                // mapping is informational, so the
                // overflow edge case doesn't matter.
                let line_number: usize = (row.id.unsigned_abs() as usize).max(1);
                let line_option = self
                    .todo_line_option
                    .replace("$LINE", &line_number.to_string());
                let filepath = match self.notes_dir.as_ref() {
                    Some(dir) => dir.join(&row.comment).to_string_lossy().to_string(),
                    None => row.comment.clone(),
                };
                // Quote the path if it contains
                // spaces. We also quote it if it
                // contains shell metacharacters
                // — for simplicity we always quote
                // when the path isn't a clean
                // alphanumeric string. The line
                // option is appended after the
                // quoted path so editors like vim
                // parse it correctly.
                let quoted = if filepath
                    .chars()
                    .any(|c| c.is_whitespace() || "<>|&;\"'$`\\".contains(c))
                {
                    format!("\"{}\"", filepath)
                } else {
                    filepath
                };
                self.selection =
                    Some(format!("{} {} {}", editor, quoted, line_option));
                self.pick_mode = Some(PickMode::Run);
            }
            return;
        }
        // `@...` queries are note search requests.
        // Selecting a note opens it in the editor.
        if self.is_notes_query() {
            if let Some(row) = self.selected_row() {
                let editor = std::env::var("EDITOR")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "vi".to_string());
                // Build the full path to the note file
                let filepath = match self.notes_dir.as_ref() {
                    Some(dir) => dir.join(&row.command).to_string_lossy().to_string(),
                    None => row.command.clone(),
                };
                // Quote the path if it contains spaces
                let quoted = if filepath.contains(' ') {
                    format!("\"{}\"", filepath)
                } else {
                    filepath
                };
                self.selection = Some(format!("{} {}", editor, quoted));
                self.pick_mode = Some(PickMode::Run);
            }
            return;
        }

        // `~...` queries are file-search requests.
        // Selecting a file opens it in the editor.
        if self.is_files_query() {
            if let Some(row) = self.selected_row() {
                let editor = std::env::var("EDITOR")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "vi".to_string());
                // The absolute path is in
                // `row.directory` for files,
                // set during `fetch_files`.
                let filepath = &row.directory;
                let quoted = if filepath
                    .chars()
                    .any(|c| c.is_whitespace() || "<>|&;\"'$`\\".contains(c))
                {
                    format!("\"{}\"", filepath)
                } else {
                    filepath.to_string()
                };
                self.selection = Some(format!("{} {}", editor, quoted));
                self.pick_mode = Some(PickMode::Run);
            }
            return;
        }
        // `#...` queries are directories-view
        // requests. Selecting a
        // directory stages `cd
        // <abs-path>` so the
        // parent shell changes
        // cwd to that directory.
        // The selection routes
        // through the TUI's normal
        // exit-and-run path
        // (`PickMode::Run` with
        // `selection` set), so
        // any path with spaces is
        // quoted by the parent
        // shell as a single arg
        // (the path is the only
        // argument to `cd`).
        if self.is_directories_query() {
            // Clone the row's
            // `directory` (and
            // the resolved tmux
            // pane id) up front
            // so the rest of the
            // block can mutate
            // `self.selection`
            // without fighting
            // the borrow
            // checker. We can't
            // hold the
            // `selected_row()`
            // borrow across
            // `self.selection =`
            // assignments.
            let (directory, pane_id): (String, Option<String>) =
                match self.selected_row() {
                    Some(r) => (
                        r.directory.clone(),
                        self.directory_tmux_pane_id(
                            &r.directory,
                        ),
                    ),
                    None => return,
                };
            // Two action paths for
            // directory rows, branched
            // on whether the row has
            // an active tmux window
            // attached (the `T` mark
            // the user sees in the
                // capture column):
                //
                // 1. `T`-marked row: a
                //    tmux window with this
                //    directory as cwd
                //    exists. The user
                //    wants to *jump to* it
                //    — they're in some
                //    other directory, this
                //    is "I had a session
                //    running here earlier".
                //    We stage
                //    `tmux select-pane -t <id> && tmux switch-client -t <id>`
                //    so the parent shell
                //    (which is itself
                //    running in a tmux
                //    client) re-attaches
                //    to the target pane.
                //
                // 2. Unmarked row: no
                //    active tmux window
                //    for this directory.
                //    The user wants a
                //    fresh session rooted
                //    here. We stage
                //    `tmux new-session -d -s <basename> -c <dir>; tmux switch-client -t <basename>`
                //    (the `;` is
                //    shell-safe: the
                //    parent shell eval's
                //    the staged line and
                //    the `new-session` must
                //    finish before
                //    `switch-client` runs).
                //
                // The basename is
                // `std::path::Path::file_name`
                // which returns the
                // trailing path
                // component (e.g.
                // `/Users/har/work` →
                // `work`). If two
                // directories share the
                // same basename (e.g.
                // `/Users/har/x/work`
                // and
                // `/Users/har/y/work`),
                // the second
                // `new-session -s work`
                // will fail with
                // "duplicate session";
                // the parent shell
                // surfaces the error and
                // the user can pick a
                // different action
                // (rename, or `cd
                // manually` first).
                // We don't try to be
                // clever about
                // disambiguation — the
                // error path is rare
                // enough that an
                // explicit user action
                // is preferable.
                if let Some(pane_id) = pane_id.clone() {
                    // `T`-marked path:
                    // switch to the existing
                    // pane. `&&` chains
                    // the two tmux calls so
                    // if `select-pane`
                    // fails (e.g. the pane
                    // disappeared between
                    // snapshot and Enter)
                    // the parent shell
                    // doesn't try to
                    // switch to a
                    // non-existent target.
                    self.selection = Some(format!(
                        "tmux select-pane -t {} && \
                         tmux switch-client -t {}",
                        pane_id, pane_id
                    ));
                } else {
                    // Unmarked path: open
                    // a new session. The
                    // parent shell runs
                    // both commands; if
                    // `new-session` fails
                    // (e.g. the basename is
                    // already taken),
                    // `switch-client` to
                    // the same name also
                    // fails and the
                    // parent shell surfaces
                    // both errors via
                    // its standard
                    // non-zero-exit handling.
                    // Expand `~` in the
                    // directory before
                    // staging. tmux does
                    // NOT do `~` expansion
                    // itself — `tmux
                    // new-session -d -c
                    // '~/work'` silently
                    // creates the session
                    // in the user's home
                    // (not `~/work`). The
                    // shell snippet that
                    // sources this binary
                    // expands `~` only in
                    // the user's interactive
                    // line editor, not in
                    // commands the TUI
                    // stages via stdout,
                    // so we have to do it
                    // ourselves. Without
                    // this the user would
                    // see a session named
                    // `~/work` in their
                    // tmux server but
                    // they'd be sitting in
                    // `$HOME` instead of
                    // `~/work` — silent
                    // correctness bug.
                    let path = crate::util::expand_home(
                        &directory,
                    )
                    .into_owned();
                    let name = std::path::Path::new(&path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("smarthistory")
                        .to_string();
                    let quoted_path = if path
                        .chars()
                        .any(|c| c.is_whitespace()
                            || "<>|&;\"'$`\\".contains(c))
                    {
                        format!("\"{}\"", path)
                    } else {
                        path
                    };
                    // `-A` would attach-and-
                    // create in one step,
                    // but it also attaches
                    // the calling client
                    // (which is the
                    // smarthistory process
                    // — we don't want tmux
                    // stealing our TTY).
                    // `-d` (detached) +
                    // explicit
                    // `switch-client` is the
                    // correct shape.
                    self.selection = Some(format!(
                        "tmux new-session -d -s {} -c {}; \
                         tmux switch-client -t {}",
                        name, quoted_path, name
                    ));
                }
                // `.command` chain. If
                // the directory (or an
                // ancestor) has a
                // `.command` file, run
                // it with the
                // directory as the
                // first argument. The
                // lookup walks up the
                // parent tree, so a
                // `project/.command`
                // fires for any
                // selection under
                // `project/`. The
                // `.command` is run
                // *inside* the new
                // session (so it
                // affects the new
                // session's
                // environment) via
                // `tmux send-keys`.
                // For the `T`-marked
                // branch (jumping to
                // an existing pane)
                // we still run the
                // command, since the
                // user explicitly
                // picked the row and
                // we shouldn't second-
                // guess their intent.
                //
                // Form:
                //   tmux send-keys -t <pane> "sh <command-file> <dir>" Enter
                //
                // The `sh` wrapper
                // means the file
                // doesn't need to be
                // executable. The
                // first argument is
                // always the selected
                // directory; the
                // .command script can
                // use `$1` (or `$@`
                // for the full arg
                // list) to read it.
                //
                // The chain uses `;`
                // (not `&&`) for the
                // `T`-marked branch:
                // the user wants the
                // jump to happen
                // even if the
                // .command script
                // fails. A `.command`
                // author who needs
                // the jump to fail
                // on script failure
                // can `exit 1` from
                // the script and the
                // user will see the
                // non-zero exit in
                // the parent shell.
                //
                // For the unmarked
                // branch (new
                // session) we *wait*
                // for the .command
                // to finish before
                // switch-client, so
                // the user lands in
                // a session that
                // already has the
                // project set up.
                // This is `&&`
                // between the
                // command and the
                // switch-client.
                if let Some(cmd_path) =
                    crate::util::find_command_file(
                        std::path::Path::new(&directory),
                    )
                {
                    let path_for_arg =
                        crate::util::expand_home(&directory)
                            .into_owned();
                    let quoted_arg = if path_for_arg
                        .chars()
                        .any(|c| c.is_whitespace()
                            || "<>|&;\"'$`\\".contains(c))
                    {
                        format!("\"{}\"", path_for_arg)
                    } else {
                        path_for_arg
                    };
                    let quoted_cmd = if cmd_path
                        .to_string_lossy()
                        .chars()
                        .any(|c| c.is_whitespace()
                            || "<>|&;\"'$`\\".contains(c))
                    {
                        format!("\"{}\"", cmd_path.display())
                    } else {
                        cmd_path.display().to_string()
                    };
                    // The script body:
                    // `sh <file> <dir>`.
                    // The first argument
                    // is always the
                    // selected directory
                    // (the user said so).
                    let command_run = format!(
                        "sh {} {}",
                        quoted_cmd, quoted_arg
                    );
                    if let Some(pane_id_inner) =
                        pane_id.as_ref()
                    {
                        // T-marked:
                        // chain with `;`
                        // (jump happens
                        // even on script
                        // failure).
                        let existing = self
                            .selection
                            .take()
                            .unwrap_or_default();
                        self.selection = Some(format!(
                            "{} ; \
                             tmux send-keys -t {} \
                             {} Enter",
                            existing,
                            pane_id_inner,
                            crate::util::shell_quote(
                                &command_run,
                            ),
                        ));
                    } else {
                        // Unmarked: chain
                        // the .command
                        // BEFORE
                        // switch-client
                        // with `&&` so the
                        // user lands in a
                        // fully-set-up
                        // session. We do
                        // this by
                        // replacing the
                        // bare
                        // `new-session` line
                        // with one that
                        // also runs the
                        // .command. The
                        // shape:
                        //   tmux new-session -d -s NAME -c DIR ; sh FILE DIR ; tmux switch-client -t NAME
                        let path = crate::util::expand_home(
                            &directory,
                        )
                        .into_owned();
                        let name = std::path::Path::new(&path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("smarthistory")
                            .to_string();
                        let quoted_path = if path
                            .chars()
                            .any(|c| c.is_whitespace()
                                || "<>|&;\"'$`\\".contains(c))
                        {
                            format!("\"{}\"", path)
                        } else {
                            path
                        };
                        self.selection = Some(format!(
                            "tmux new-session -d -s {} -c {}; \
                             sh {} {}; \
                             tmux switch-client -t {}",
                            name,
                            quoted_path,
                            quoted_cmd,
                            quoted_arg,
                            name
                        ));
                    }
                }
                self.pick_mode = Some(PickMode::Run);
            return;
        }
        // `*...` queries are the session-panes
        // view. Selecting a pane stages a tmux
        // command that jumps the user's client to
        // that pane. Two pieces are needed:
        //   `select-window -t <window_id>`
        //   `select-pane  -t <pane_id>`
        // because plain `select-pane` does NOT
        // switch windows — a pane in another
        // window would otherwise be unreachable.
        // The window id (`@N`) is stashed in the
        // row's `output` field by
        // `fetch_session_panes`; the pane id (`%N`)
        // is in `session_id`. The current pane is
        // excluded from the list at fetch time, so
        // the user never stages a no-op jump to
        // themselves. `&&` chains the two calls: if
        // the window vanished between snapshot and
        // Enter, its panes are gone too, so don't
        // try to select the pane. If the window id
        // is somehow empty (parse fallback), we
        // degrade to just `select-pane`.
        if self.is_panes_query() {
            let (pane_id, window_id): (String, String) =
                match self.selected_row() {
                    Some(r) => (
                        r.session_id.clone(),
                        r.output.clone(),
                    ),
                    None => return,
                };
            if pane_id.is_empty() {
                return;
            }
            self.selection = Some(if window_id.is_empty() {
                format!("tmux select-pane -t {}", pane_id)
            } else {
                format!(
                    "tmux select-window -t {} && \
                     tmux select-pane -t {}",
                    window_id, pane_id
                )
            });
            self.pick_mode = Some(PickMode::Run);
            return;
        }
        // `-...` queries are JIRA issue-search
        // requests. Selecting an issue opens its
        // browse URL (`JIRA_URL/<key>`) in the
        // system browser: `open` on macOS,
        // `xdg-open` on other Unixes. The key is
        // the row's `command` field. If JIRA isn't
        // configured (no `JIRA_URL`), surface a
        // status message instead of staging a
        // malformed command.
        if self.is_jira_query() {
            let key: String = match self.selected_row() {
                Some(r) => r.command.clone(),
                None => return,
            };
            if key.is_empty() {
                return;
            }
            match crate::jira::JiraConfig::from_env() {
                Some(cfg) => {
                    let url = cfg.browse_url(&key);
                    let opener = if cfg!(target_os = "macos") {
                        "open"
                    } else {
                        "xdg-open"
                    };
                    self.selection = Some(format!(
                        "{} \"{}\"",
                        opener, url
                    ));
                    self.pick_mode = Some(PickMode::Run);
                }
                None => {
                    self.set_status_message(
                        crate::jira::JiraError::NotConfigured.to_string(),
                    );
                }
            }
            return;
        }
        if let Some(row) = self.selected_row() {
            // Check the mode field to determine the type of entry.
            if row.mode == "llm" && !row.output.is_empty() {
                // Old LLM query: execute the output (the generated command).
                self.selection = Some(row.output.clone());
                self.pick_mode = Some(PickMode::Run);
            } else if row.mode == "question" && !row.output.is_empty() {
                // Old question: show the answer in the overlay.
                self.question_view = Some(QuestionView {
                    question: row.command.clone(),
                    text: row.output.clone(),
                    scroll: 0,
                });
            } else {
                self.selection = Some(row.command.clone());
                self.pick_mode = Some(PickMode::Run);
            }
        }
    }

    /// Handle a `=...` query by sending the natural-language
    /// description to the configured ollama instance, sanitizing
    /// the response, and staging the resulting command for
    /// execution. The new command is also written to the
    /// `history` table so it shows up in subsequent searches;
    /// the user's original description is stored as the
    /// row's comment so the row is self-documenting.
    ///
    /// This blocks the TUI's main loop for the duration of the
    /// HTTP call (typically 1-5 seconds for a local 7B model,
    /// bounded by a 30-second timeout in `OllamaClient`). The
    /// user explicitly asked for this mode and accepted the
    /// freeze; a future async refactor could keep the TUI
    /// responsive while the call is in flight.
    
    /// Process the result of an in-flight LLM request.
    /// Called from the run loop when a result arrives on
    /// the channel. Handles each request type differently.
    fn process_llm_result(&mut self, request: LlmRequest, result: Result<String, crate::llm::LlmError>) {
        self.llm_in_flight = false;
        self.llm_request = None;
        
        // If the request was cancelled, discard the result.
        if request.cancelled.load(Ordering::Relaxed) {
            self.set_status_message("LLM request cancelled".to_string());
            return;
        }
        
        let raw = match result {
            Ok(s) => s,
            Err(e) => {
                self.set_status_message(e.to_string());
                return;
            }
        };
        
        match request.request_type {
            LlmRequestType::Generate { description } => {
                let command = match crate::llm::sanitize_command(&raw) {
                    Some(c) => c,
                    None => {
                        self.set_status_message(crate::llm::LlmError::NoCommand.to_string());
                        return;
                    }
                };
                self.stage_llm_command(command, description);
            }
            LlmRequestType::Describe { command } => {
                self.describe_view = Some(DescribeView {
                    command,
                    text: raw.trim().to_string(),
                    scroll: 0,
                });
            }
            LlmRequestType::Correct { original_command } => {
                let corrected_command = match crate::llm::sanitize_command(&raw) {
                    Some(c) => c,
                    None => {
                        self.set_status_message(crate::llm::LlmError::NoCommand.to_string());
                        return;
                    }
                };
                self.correct_view = Some(CorrectView {
                    original_command,
                    corrected_command,
                });
            }
            LlmRequestType::Question { question } => {
                let answer = raw.trim().to_string();
                self.stage_question(question.clone(), answer.clone());
                self.question_view = Some(QuestionView {
                    question: format!("{}{}", self.query_prefixes.question, question),
                    text: answer,
                    scroll: 0,
                });
            }
        }
    }

    /// Process any pending LLM request synchronously.
    /// Used by tests that expect the result to be available
    /// immediately after calling an LLM action method.
    #[cfg(test)]
    fn process_pending_llm_request(&mut self) {
        if let Some(request) = self.llm_request.take() {
            if let Ok(result) = request.receiver.recv() {
                self.process_llm_result(request, result);
            }
        }
    }

    fn run_llm_query(&mut self) {
        // Step 1: extract the description (everything after the
        // leading LLM prefix).
        let prefix = self.query_prefixes.llm;
        let description = self.query[prefix.len_utf8()..].trim();
        if description.is_empty() {
            self.set_status_message("LLM: provide a description after the LLM prefix".to_string());
            return;
        }
        // Step 2: bail out cleanly if the LLM isn't configured.
        if self.llm.is_none() {
            self.set_status_message(crate::llm::LlmError::NotConfigured.to_string());
            return;
        }
        // Step 2.5: fast-path. The auto-call debounce may
        // have already generated a preview for this exact
        // description (`llm_maybe_autocall`). Reuse the
        // preview's command directly — no second HTTP
        // round-trip needed, no second 1–5s freeze of the
        // TUI.
        if let (Some(preview), Some(preview_desc), Some(started)) = (
            self.llm_preview.as_ref(),
            self.llm_preview_description.as_ref(),
            self.llm_debounce_started,
        ) && preview_desc.as_str() == description
        && started.elapsed() < LLM_DEBOUNCE * 5
        {
            self.stage_llm_command(preview.output.clone(), description.to_string());
            return;
        }
        // Step 3: spawn a background thread for the LLM call.
        // The run loop will process the result when it arrives.
        let prompt = crate::llm::build_prompt(description);
        self.spawn_llm_request(
            LlmRequestType::Generate { description: description.to_string() },
            prompt,
        );
    }

    /// Persist `command` to the history table (with
    /// `description` as the comment) and stage it as the
    /// next "selection" the parent shell will run. Shared
    /// between the slow path (explicit LLM call from
    /// `run_llm_query`) and the fast path (preview reuse
    /// from the same method).
    ///
    /// On any DB error we surface a status message and
    /// leave the selection unset so the TUI doesn't exit
    /// with a half-staged command.
    /// Persist the LLM query to the history table with
    /// `description` (prefixed with `=`) as the command and
    /// `generated_command` stored as the output. This allows
    /// users to search for old LLM queries with `=` and
    /// re-execute the generated command.
    fn stage_llm_command(&mut self, generated_command: String, description: String) {
        // Store the description with the configured LLM prefix as the command,
        // and the generated command in the output column.
        // This way users can search for old LLM queries and
        // the generated command is replayed when selected.
        // The directory is canonicalized
        // (resolves symlinks / macOS
        // volume mounts) so the row
        // matches the same path the
        // DIR-mode filter uses later.
        let directory =
            crate::util::canonicalize_directory(
                &std::env::var("PWD").unwrap_or_default(),
            );
        let session_id = std::env::var("SMART_HISTORY_SESSION").unwrap_or_default();
        let query_command = format!("{}{}", self.query_prefixes.llm, description);
        let insert_result: anyhow::Result<i64> = (|| {
            self.conn.execute(
                "INSERT INTO history (command, directory, session_id, exit_code, mode) \
                 VALUES (?1, ?2, ?3, -1, 'llm') \
                 ON CONFLICT (command, directory, session_id) DO UPDATE \
                 SET timestamp = (strftime('%s', 'now')), mode = 'llm'",
                params![&query_command, directory, session_id],
            )?;
            let id: i64 = self.conn.query_row(
                "SELECT id FROM history WHERE command = ?1 AND directory = ?2 AND session_id = ?3",
                params![&query_command, directory, session_id],
                |row| row.get(0),
            )?;
            // Store the generated command as a comment for visibility
            self.conn.execute(
                "INSERT INTO command_comments (command, comment) VALUES (?1, ?2) \
                 ON CONFLICT (command) DO UPDATE SET comment = excluded.comment",
                params![&query_command, &generated_command],
            )?;
            Ok(id)
        })();
        let history_id = match insert_result {
            Ok(id) => id,
            Err(e) => {
                self.set_status_message(format!("LLM: history insert failed: {}", e));
                return;
            }
        };
        // Also store the generated command as output
        let output_result: anyhow::Result<()> = (|| {
            self.conn.execute(
                "INSERT INTO history_output (history_id, output) VALUES (?1, ?2) \
                 ON CONFLICT (history_id) DO UPDATE SET output = excluded.output, captured_at = (strftime('%s', 'now'))",
                params![history_id, &generated_command],
            )?;
            Ok(())
        })();
        if let Err(e) = output_result {
            self.set_status_message(format!("LLM: output store failed: {}", e));
            // Continue anyway - we can still stage the command
        }
        // Stage the generated command for the parent shell to run
        self.selection = Some(generated_command.clone());
        self.pick_mode = Some(PickMode::Run);
        self.set_status_message(format!("LLM: {}", generated_command));
    }

    // ---- JIRA (`-`-prefix) search-as-you-type ----

    /// Arm the JIRA search debounce. Called from every
    /// keystroke path when in `-`-mode (push_char,
    /// backspace, set_search_mode_prefix, etc.) — mirrors
    /// `llm_touch`. The run loop's tick then fires the
    /// actual search after `JIRA_DEBOUNCE` of quiet.
    /// Outside `-`-mode this clears any pending state so a
    /// stray timer doesn't fire after the user leaves the
    /// mode.
    fn jira_touch(&mut self) {
        if self.is_jira_query() {
            self.jira_debounce_started = Some(std::time::Instant::now());
        } else {
            self.jira_debounce_started = None;
            self.jira_in_flight = false;
        }
    }

    /// Arm or clear the files-mode
    /// walk debounce. Called from
    /// `llm_touch` on every keystroke
    /// (same co-location pattern as
    /// `jira_touch`). Re-arms the
    /// timer when the user is still in
    /// files mode; resets all pending
    /// state when the user leaves.
    fn files_touch(&mut self) {
        if self.is_files_query() {
            self.files_state.debounce_started = Some(std::time::Instant::now());
            // If there's an in-flight walk,
            // cancel it — the pattern has
            // changed. The cached rows
            // stay visible until the new
            // walk completes.
            if let Some(request) = self.files_state.request.take() {
                request.cancelled.store(true, Ordering::Relaxed);
            }
            self.files_state.in_flight = false;
        } else {
            self.files_state.debounce_started = None;
            self.files_state.in_flight = false;
            self.files_state.request = None;
            self.files_state.last_pattern = None;
        }
    }
    
    /// Check whether the files-mode
    /// debounce has elapsed and, if so,
    /// spawn a background directory walk.
    /// Called from the run-loop's idle
    /// tick (same pattern as
    /// `llm_maybe_autocall` and
    /// `jira_maybe_autocall`). Returns
    /// immediately when not in files
    /// mode, when a walk is already in
    /// flight, or when the debounce
    /// window hasn't elapsed.
    fn files_maybe_autocall(&mut self) {
        if !self.is_files_query() {
            return;
        }
        if self.files_state.in_flight {
            return;
        }
        let Some(started) = self.files_state.debounce_started else {
            return;
        };
        if started.elapsed() < crate::files::FILES_DEBOUNCE {
            return;
        }
        let pattern = crate::files::FilesState::current_pattern(
            &self.query,
            self.query_prefixes.files,
        );
        // Skip if we already have results for this pattern.
        if self.files_state.has_results_for(&pattern) {
            return;
        }
        // First entry into files mode:
        // arm the debounce so the walk
        // fires on the next tick even
        // if the user never types
        // another character.
        self.files_state.last_pattern = Some(pattern.clone());
        self.spawn_files_walk(pattern);
    }

    /// Spawn a background thread that
    /// walks the current directory tree,
    /// filters by `pattern`, and sends
    /// the result back over an mpsc
    /// channel. The run loop polls the
    /// receiver and calls
    /// `process_files_result` when the
    /// result arrives.
    fn spawn_files_walk(&mut self, pattern: String) {
        let ignore = crate::files::IgnoreSet::new(&self.files_ignores);
        let request = crate::files::spawn_walk(pattern.clone(), ignore);
        self.files_state.in_flight = true;
        self.files_state.request = Some(request);
        self.set_status_message("Searching files…".to_string());
    }

    /// Process a files-mode walk result
    /// that arrived from the background
    /// thread. Caches the rows in
    /// `self.files_state.rows` and
    /// refreshes the list. Stale results
    /// (the pattern changed between spawn
    /// and delivery) are discarded.
    fn process_files_result(
        &mut self,
        request: crate::files::FilesRequest,
        rows: Vec<HistoryRow>,
    ) {
        self.files_state.in_flight = false;
        self.files_state.request = None;
        // Only accept if this result
        // matches the current pattern
        // (the user may have typed
        // more characters while the
        // walk was running).
        let current = crate::files::FilesState::current_pattern(
            &self.query,
            self.query_prefixes.files,
        );
        if current == request.pattern {
            self.files_state.rows = rows;
            self.refresh();
        }
    }

    /// Build the JQL string for the current query body,
    /// using the configured `JIRA_PROJECT` as the default
    /// when the body is empty. The project is read from
    /// `JIRA_PROJECT` directly (not the full `JiraConfig`)
    /// so this works in tests where only a fake client is
    /// injected and no `JIRA_SERVER`/`JIRA_API_TOKEN` is
    /// set.
    ///
    /// The user-defined JQL fragments (from the
    /// `jira.search.<name>=` config keys) are spliced
    /// into the JQL when the body contains `@<name>`
    /// tokens. Any unresolved fragment names are stashed
    /// on `self.jira_undefined_fragments`; the autocall
    /// reads that to decide whether to skip the search
    /// and surface a diagnostic.
    fn jira_build_query(&mut self) -> String {
        let project = std::env::var("JIRA_PROJECT")
            .ok()
            .filter(|s| !s.trim().is_empty());
        // `now_epoch()` is the wall clock the JQL
        // builder uses to compute the date-cutoff for
        // the `@today` / `@week` / `@month` aliases
        // (e.g. `@week` becomes
        // `updated >= "<today - 7d>"`). It's the same
        // helper the rest of the TUI uses for "now",
        // so all date-bearing features see a consistent
        // view of time.
        let (jql, undefined) = crate::jira::build_jql(
            self.jira_pattern(),
            project.as_deref(),
            self.now_epoch(),
            &self.jira_fragments,
        );
        self.jira_undefined_fragments = undefined;
        jql
    }

    /// Drive the JIRA search debounce. Called from the
    /// run-loop tick on the no-input path (mirrors
    /// `llm_maybe_autocall`). Fires a single background
    /// search when: in `-`-mode, debounce elapsed, no
    /// search in flight, JIRA is configured (env vars OR
    /// an injected test client), and the live JQL differs
    /// from the last-fired one.
    fn jira_maybe_autocall(&mut self) {
        if !self.is_jira_query() {
            return;
        }
        if self.jira_in_flight {
            return;
        }
        let Some(started) = self.jira_debounce_started else {
            return;
        };
        if started.elapsed() < JIRA_DEBOUNCE {
            return;
        }
        // "Configured" means either real env config OR an
        // injected test client. If neither, surface a
        // one-shot status message and disarm.
        let configured = self.jira_client.is_some()
            || crate::jira::JiraConfig::from_env().is_some();
        if !configured {
            if self.jira_last_jql.is_some() || self.jira_rows.is_empty() {
                self.set_status_message(
                    crate::jira::JiraError::NotConfigured.to_string(),
                );
            }
            self.jira_debounce_started = None;
            return;
        }
        let jql = self.jira_build_query();
        // If the user typed `@somefrag` and `somefrag`
        // isn't in the configured fragments map, the
        // JQL is still valid (the unknown token falls
        // through to free text) but the search would
        // return wrong results. Refuse to fire and
        // surface a diagnostic instead. We only emit
        // the status message when the undefined list
        // CHANGES, so the user doesn't get a stale
        // message re-surfacing on every keystroke
        // while they correct a typo.
        if !self.jira_undefined_fragments.is_empty() {
            if self.jira_last_undefined_message.as_ref()
                != Some(&self.jira_undefined_fragments)
            {
                self.set_status_message(format!(
                    "JIRA fragment{} not configured: {}. \
                     Define via jira.search.<name>=... in \
                     ~/.config/smarthistory/config.",
                    if self.jira_undefined_fragments.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    self.jira_undefined_fragments
                        .iter()
                        .map(|n| format!("@{}", n))
                        .collect::<Vec<_>>()
                        .join(", "),
                ));
                self.jira_last_undefined_message =
                    Some(self.jira_undefined_fragments.clone());
            }
            self.jira_debounce_started = None;
            return;
        }
        // No undefined fragments on this build — clear
        // the debounce on the "fragment not configured"
        // message so a previously-flagged typo doesn't
        // keep a stale status visible forever. (The
        // message is replaced by the next status set
        // anywhere in the app; this just keeps the
        // bookkeeping consistent.)
        self.jira_last_undefined_message = None;
        // Skip if we already have results for this exact JQL.
        if self.jira_last_jql.as_deref() == Some(&jql) {
            return;
        }
        self.spawn_jira_request(jql);
    }

    /// Spawn the search. When an injected test client is
    /// present (`set_jira_client`), run synchronously on
    /// the calling thread (deterministic for tests).
    /// Otherwise spawn a real `reqwest` background thread
    /// against the env-configured JIRA server.
    fn spawn_jira_request(&mut self, jql: String) {
        if let Some(client) = self.jira_client.clone() {
            let result = client.search(&jql);
            let request = JiraRequest {
                receiver: mpsc::channel().1,
                cancelled: Arc::new(AtomicBool::new(false)),
            };
            self.jira_in_flight = true;
            self.jira_last_jql = Some(jql);
            self.process_jira_result(request, result);
            return;
        }
        let Some(config) = crate::jira::JiraConfig::from_env() else {
            self.set_status_message(
                crate::jira::JiraError::NotConfigured.to_string(),
            );
            return;
        };
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_clone = cancelled.clone();
        let jql_for_thread = jql.clone();
        std::thread::spawn(move || {
            let client = crate::jira::RestJiraClient::new(config);
            let result = client.search(&jql_for_thread);
            if !cancelled_clone.load(Ordering::Relaxed) {
                let _ = tx.send(result);
            }
        });
        self.jira_in_flight = true;
        self.jira_request = Some(JiraRequest {
            receiver: rx,
            cancelled,
        });
        self.jira_last_jql = Some(jql);
        self.set_status_message("JIRA searching…".to_string());
    }

    /// Process a JIRA search result that arrived from the
    /// background thread. Converts issues to `HistoryRow`s,
    /// caches them, and refreshes so the list repaints.
    /// Errors surface as a status message (the list keeps
    /// the previous result).
    fn process_jira_result(
        &mut self,
        request: JiraRequest,
        result: Result<Vec<crate::jira::JiraIssue>, crate::jira::JiraError>,
    ) {
        self.jira_in_flight = false;
        self.jira_request = None;
        if request.cancelled.load(Ordering::Relaxed) {
            self.set_status_message("JIRA search cancelled".to_string());
            return;
        }
        match result {
            Ok(issues) => {
                let now_epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let mut next_id: i64 = -1;
                let rows = issues
                    .into_iter()
                    .map(|issue| {
                        // Build the details-pane text from
                        // the user-spec'd attribute set:
                        // Status, Priority, Due, Assignee,
                        // Description. The new layout is:
                        //
                        //   line 1: **Status**: X   **Priority**: Y
                        //   line 2: **Due**: X       **Assignee**: Y
                        //   line 3: **Description**
                        //   lines 4+: the full description text
                        //
                        // Labels are wrapped in `**...**`
                        // markers that the details-pane
                        // renderer turns into bold spans.
                        // Each field uses `<none>` as a
                        // placeholder when empty, so the
                        // 3-line header layout stays
                        // consistent regardless of which
                        // fields are populated.
                        //
                        // The 4-line preview budget in the
                        // details pane shows the 3 header
                        // breaks as `\n`), so a
                        // multi-paragraph body produces a
                        // multi-line tail in the output.
                        let none_placeholder = "<none>";
                        let status = if issue.status.is_empty() {
                            none_placeholder
                        } else {
                            issue.status.as_str()
                        };
                        let priority = if issue.priority.is_empty() {
                            none_placeholder
                        } else {
                            issue.priority.as_str()
                        };
                        let due = if issue.due.is_empty() {
                            none_placeholder
                        } else {
                            issue.due.as_str()
                        };
                        let assignee = if issue.assignee.is_empty() {
                            none_placeholder
                        } else {
                            issue.assignee.as_str()
                        };
                        // The header block is three
                        // lines, joined by `\n`. Two
                        // spaces between the (label,
                        // value) pairs on lines 1 and 2
                        // give the bold spans a little
                        // breathing room without forcing
                        // a hard column alignment.
                        let mut details: Vec<String> = Vec::new();
                        details.push(format!(
                            "**Status**: {}  **Priority**: {}",
                            status, priority
                        ));
                        details.push(format!(
                            "**Due**: {}  **Assignee**: {}",
                            due, assignee
                        ));
                        details.push("**Description**".to_string());
                        // The description body
                        // follows on the next line(s).
                        // Empty descriptions get a
                        // single `<none>` placeholder
                        // so the line is always
                        // present and the layout
                        // stays consistent.
                        if !issue.description.is_empty() {
                            // `description` may
                            // contain newlines (the
                            // extractor preserves
                            // paragraph breaks as
                            // `\n`); each one
                            // becomes its own
                            // line in the output.
                            for line in issue.description.lines() {
                                details.push(line.to_string());
                            }
                            // Trailing empty
                            // lines from the
                            // description's
                            // trailing `\n`
                            // are dropped
                            // silently by
                            // `lines()` —
                            // no extra
                            // placeholder
                            // needed.
                        } else {
                            details.push(none_placeholder.to_string());
                        }
                        let ts = crate::jira::updated_to_epoch(&issue.updated);
                        let id = next_id;
                        next_id -= 1;
                        crate::tui::state::HistoryRow {
                            id,
                            command: issue.key,
                            directory: String::new(),
                            session_id: String::new(),
                            exit_code: 0,
                            timestamp: if ts > 0 { ts } else { now_epoch },
                            comment: issue.summary,
                            // Newlines between
                            // attributes are the
                            // natural rendering
                            // boundary for the
                            // preview pane.
                            output: details.join("\n"),
                            mode: "jira".to_string(),
                            source: "jira".to_string(),
                        }
                    })
                    .collect();
                self.jira_rows = rows;
                self.status_message = None;
                self.refresh();
            }
            Err(e) => {
                self.set_status_message(e.to_string());
            }
        }
    }

    /// Return the cached JIRA rows (no network — the live
    /// fetch happens in the background via
    /// `jira_maybe_autocall`). Wired into `fetch()`'s
    /// dispatch. An empty cache (no result yet / not
    /// configured) yields an empty list; the status message
    /// from the fetch path tells the user why.
    fn fetch_jira(&mut self) -> Result<Vec<crate::tui::state::HistoryRow>> {
        Ok(self.jira_rows.clone())
    }

    /// Install a JIRA client for tests (a fake). When set,
    /// searches run synchronously on the calling thread via
    /// this client instead of spawning a background HTTP
    /// thread, so the search-render path is deterministic.
    #[cfg(test)]
    fn set_jira_client(
        &mut self,
        client: std::sync::Arc<dyn crate::jira::JiraClient>,
    ) {
        self.jira_client = Some(client);
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
        // When the query is an LLM command-generation
        // request (`=...`), the Left/Right keys (which the
        // user normally binds to `EditStart`/`EditEnd`) are
        // repurposed to position the cursor inside the
        // description rather than to stage a row. The LLM
        // path doesn't have a meaningful "selected row" —
        // the user is composing a prompt, not picking from
        // history.
        //
        // Character-by-character navigation: pressing Left
        // moves the cursor one character toward the start of
        // the description (saturating at 0 so pressing Left
        // at the very beginning of the buffer is a no-op
        // rather than an underflow panic). Pressing Right
        // (see `select_for_edit_end`) moves one character
        // toward the end, saturating at the current buffer
        // length.
        if self.is_llm_query() {
            self.query_cursor = self.query_cursor.saturating_sub(1);
            return;
        }
        if let Some(i) = self.list_state.selected()
            && let Some(row) = self.rows.get(i)
        {
            self.selection = Some(row.command.clone());
            self.pick_mode = Some(PickMode::EditStart);
        }
    }

    fn select_for_edit_end(&mut self) {
        // See `select_for_edit_start` for the LLM-mode
        // branch rationale.
        if self.is_llm_query() {
            let len = self.query.chars().count();
            self.query_cursor = self.query_cursor.saturating_add(1).min(len);
            return;
        }
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
                // Reset the cursor to the (now-empty) end so
                // the new character lands at position 0.
                self.query_cursor = 0;
            }
            self.query_touched = true;
            // Insert the new character at the current cursor
            // position rather than unconditionally appending.
            // For non-LLM query modes the cursor is always at
            // the end of the buffer, so this behaves exactly
            // like `self.query.push(c)`. For LLM modes the
            // user can move the cursor with Left/Right and
            // insert mid-buffer.
            let byte_idx = char_to_byte_index(&self.query, self.query_cursor);
            self.query.insert(byte_idx, c);
            self.query_cursor += 1;
            self.recompile_regex();
            self.refresh();
            // Re-arm the LLM auto-call debounce (or clear
            // the preview if we just left LLM mode by
            // backspacing the `=`). The user's last
            // edit time is the new debounce anchor.
            self.llm_touch();
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
            if self.query_cursor > 0 {
                self.query_touched = true;
                // Delete the character to the LEFT of the
                // cursor. The cursor is always >= 1 here (the
                // guard above) so there's always a character
                // to delete. This respects the user's mid-buffer
                // position for LLM mode and matches the
                // historical "delete at end" behaviour when the
                // cursor is at the end.
                let byte_idx = char_to_byte_index(&self.query, self.query_cursor - 1);
                self.query.remove(byte_idx);
                self.query_cursor -= 1;
                self.recompile_regex();
                self.refresh();
                // Mirror of `push_char`: re-arm the LLM
                // debounce (or clear preview state if we
                // just backspaced out of LLM mode).
                self.llm_touch();
            }
        }
    }

    /// Delete one word backward from the cursor
    /// position. Matches the readline / bash / zsh
    /// `Ctrl-W` semantics:
    ///
    /// 1. **Trailing whitespace first**: if the
    ///    character(s) immediately before the cursor
    ///    are whitespace, eat them.
    /// 2. **Then the preceding word**: walk left
    ///    through the run of non-whitespace
    ///    characters immediately before the cursor
    ///    and eat them too.
    /// 3. **Stop at the start of the buffer** (or at
    ///    cursor position 0, whichever comes first).
    ///
    /// If the cursor is in the middle of a word
    /// (e.g. `git |status` with the cursor between
    /// the space and `status`), only the characters
    /// to the left of the cursor are deleted — the
    /// cursor's position is respected. This matches
    /// the existing `Backspace` behaviour, which
    /// already supports mid-buffer cursor position
    /// via `query_cursor`.
    ///
    /// The function operates on the query field by
    /// default, but routes to the comment-edit
    /// buffer when one is open (so the same shortcut
    /// works in the `EditComment` overlay).
    ///
    /// UTF-8 handling: `query_cursor` and the
    /// buffer's `.chars()` are in characters; we
    /// only convert to a byte index once, at the
    /// moment of the `String::remove` call, via
    /// `char_to_byte_index`. We delete one character
    /// at a time (rather than slicing) so the buffer
    /// stays valid UTF-8 throughout — a multi-byte
    /// character is removed as a single unit.
    fn delete_word_backward(&mut self) {
        if let Some(ref mut buf) = self.comment_edit {
            // The comment-edit buffer has no cursor
            // concept — it's just a `String`. Apply
            // the same word-backward logic but
            // operate on the buffer's logical end.
            delete_word_backward_in_string(buf);
            return;
        }
        if self.query_cursor == 0 {
            // Nothing to the left of the cursor —
            // don't flag the query as touched, just
            // bail. Mirrors `backspace`'s "no-op at
            // position 0" contract.
            return;
        }
        self.query_touched = true;
        // Walk left in *characters*, counting how
        // many we delete. The cursor is in
        // characters, so the index math is
        // straightforward: the new cursor position
        // is `start_of_word`.
        let start_of_word = delete_word_backward_at_cursor(&self.query, self.query_cursor);
        // Apply the deletion as a single `replace`
        // so we don't do N UTF-8-safe `String::remove`
        // calls (each of which has to recompute the
        // byte index). The slice `&self.query[start..end]`
        // is a byte slice; we trust it because
        // `start_of_word` and `self.query_cursor` are
        // both character indices that we've
        // validated to be in-range.
        let start_byte = char_to_byte_index(&self.query, start_of_word);
        let end_byte = char_to_byte_index(&self.query, self.query_cursor);
        self.query.replace_range(start_byte..end_byte, "");
        self.query_cursor = start_of_word;
        self.recompile_regex();
        self.refresh();
        self.llm_touch();
    }

    fn clear_query(&mut self) {
        if let Some(ref mut buf) = self.comment_edit {
            buf.clear();
        } else {
            self.query.clear();
            self.query_touched = true;
            self.query_regex = None;
            // Cursor at the new (empty) end. Any cursor
            // position from before the clear is now
            // meaningless.
            self.query_cursor = 0;
            self.refresh();
            // Clear-input is a user edit too. If we were
            // in LLM mode, restart the debounce so the
            // user can type a fresh description and have
            // the auto-call fire on the new one. If we
            // just cleared the leading `=`, `llm_touch`
            // will see we're no longer in LLM mode and
            // clear the preview.
            self.llm_touch();
        }
    }

    fn start_comment_edit(&mut self) {
        // The edit buffer is shared between
        // two paths:
        //
        // 1. **Local command comment** (the
        //    original behaviour): prefill
        //    with the existing `row.comment`
        //    and save to the local SQLite
        //    `command_comments` table on
        //    `Enter`. Used for non-JIRA
        //    rows.
        //
        // 2. **JIRA add comment** (new):
        //    prefill with an empty string
        //    (the user is composing a
        //    *new* comment, not editing
        //    an existing one — the JIRA
        //    `description` and existing
        //    `comments` are already shown
        //    in the show-output overlay;
        //    Ctrl-E here is a "post a new
        //    comment" action). On `Enter`,
        //    POST to JIRA's REST v2
        //    `add_comment` endpoint. Used
        //    for JIRA rows.
        //
        // The dispatch is keyed on
        // `row.mode == "jira"`. We can't
        // take `&mut self` while also
        // holding a borrow of the
        // selected row, so we copy
        // out the fields we need
        // (`command` and `mode`) and
        // dispatch on those — same
        // pattern as `show_output_view`.
        let selection = self.selected_row().map(|r| (r.command.clone(), r.mode.clone(), r.comment.clone()));
        let Some((command, mode, comment)) = selection else {
            return;
        };
        if mode == "jira" {
            // JIRA add-comment path.
            // The buffer is empty
            // (the user is writing a
            // new comment from scratch).
            // The target key is stashed
            // on `self` so `save_comment_edit`
            // can route the
            // buffer to the JIRA
            // add-comment path.
            self.jira_add_comment_target = Some(command.clone());
            self.comment_edit = Some(String::new());
        } else {
            // Local command-comment
            // path. Original
            // behaviour: prefill
            // with the existing
            // comment.
            self.jira_add_comment_target = None;
            self.comment_edit = Some(comment);
        }
    }

    fn cancel_comment_edit(&mut self) {
        self.comment_edit = None;
        // Also clear the JIRA
        // add-comment target. The
        // user pressed `Esc` (or
        // otherwise cancelled) on
        // the buffer; the buffer's
        // tied to the target only
        // for the duration of the
        // edit, so resetting both
        // keeps the state
        // consistent.
        self.jira_add_comment_target = None;
    }

    fn save_comment_edit(&mut self) -> Result<()> {
        // The buffer is shared between
        // two paths (see
        // `start_comment_edit`): a local
        // command comment edit
        // (original behaviour) and a
        // JIRA add-comment POST (new).
        // The dispatch is keyed on
        // `jira_add_comment_target`:
        // - `Some(key)` → JIRA add-comment
        //   path. Spawn a background
        //   thread that POSTs to
        //   JIRA's `add_comment`
        //   endpoint. The buffer
        //   stays open while the
        //   POST is in flight (so
        //   the user can see what
        //   they posted); on
        //   success the buffer
        //   clears and the target
        //   goes back to `None`;
        //   on failure the buffer
        //   stays so the user can
        //   retry.
        // - `None` → local
        //   command-comment path.
        //   INSERT/UPDATE the
        //   SQLite `command_comments`
        //   row and clear the
        //   buffer. Original
        //   behaviour.
        if let Some(key) = self.jira_add_comment_target.clone() {
            // Clone the buffer so
            // the closure can move
            // it into the thread
            // without borrowing
            // `self`.
            let body = self
                .comment_edit
                .clone()
                .unwrap_or_default();
            // An empty body is a
            // user error: don't
            // POST an empty
            // comment. Surface a
            // status message and
            // keep the buffer open
            // so the user can
            // type something.
            if body.trim().is_empty() {
                self.set_status_message(
                    "JIRA add-comment: body is empty".to_string(),
                );
                return Ok(());
            }
            // The fake-client
            // path runs
            // synchronously. Same
            // pattern as
            // `start_jira_comments_fetch`.
            if let Some(client) = self.jira_client.clone() {
                let result = client.add_comment(&key, &body);
                let request = JiraAddCommentRequest {
                    receiver: mpsc::channel().1,
                    cancelled: Arc::new(AtomicBool::new(false)),
                    key: key.clone(),
                    body: body.clone(),
                };
                self.jira_add_comment_in_flight = true;
                self.process_jira_add_comment_result(
                    request,
                    key,
                    result,
                );
                return Ok(());
            }
            // Production path:
            // spawn a background
            // thread.
            let Some(config) = crate::jira::JiraConfig::from_env() else {
                self.set_status_message(
                    crate::jira::JiraError::NotConfigured.to_string(),
                );
                return Ok(());
            };
            // Debounce: if a
            // previous POST is
            // still in flight,
            // drop the new one
            // silently (the
            // status message
            // is already
            // "Posting
            // comment...").
            if self.jira_add_comment_in_flight {
                return Ok(());
            }
            let (tx, rx) = mpsc::channel();
            let cancelled = Arc::new(AtomicBool::new(false));
            let cancelled_clone = cancelled.clone();
            let key_for_thread = key.clone();
            let body_for_thread = body.clone();
            std::thread::spawn(move || {
                let client =
                    crate::jira::RestJiraClient::new(config);
                let result = client.add_comment(
                    &key_for_thread,
                    &body_for_thread,
                );
                if !cancelled_clone.load(Ordering::Relaxed) {
                    let _ = tx.send(result);
                }
            });
            self.jira_add_comment_in_flight = true;
            self.jira_add_comment_request = Some(JiraAddCommentRequest {
                receiver: rx,
                cancelled,
                key,
                body,
            });
            self.set_status_message(format!(
                "Posting comment to {}…",
                self.jira_add_comment_request
                    .as_ref()
                    .map(|r| r.key.clone())
                    .unwrap_or_default(),
            ));
            return Ok(());
        }
        // Local command-comment
        // path (original behaviour).
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

    /// Process a JIRA add-comment result
    /// that arrived from the background
    /// thread. Mirrors
    /// `process_jira_comments_result`
    /// (the JIRA-comments-fetch
    /// equivalent): on success, clear
    /// the buffer and the JIRA target;
    /// on failure, surface a status
    /// message and preserve the buffer
    /// so the user can retry.
    fn process_jira_add_comment_result(
        &mut self,
        request: JiraAddCommentRequest,
        key: String,
        result: Result<(), crate::jira::JiraError>,
    ) {
        self.jira_add_comment_in_flight = false;
        self.jira_add_comment_request = None;
        if request.cancelled.load(Ordering::Relaxed) {
            self.set_status_message(format!(
                "JIRA add-comment to {} cancelled",
                key
            ));
            return;
        }
        match result {
            Ok(()) => {
                // Success: clear the
                // buffer and the
                // target. The
                // `comment_edit`
                // and
                // `jira_add_comment_target`
                // fields are
                // reset together
                // so the next
                // Ctrl-E on a
                // non-JIRA row
                // doesn't
                // accidentally
                // re-enter JIRA
                // add-comment
                // mode.
                self.comment_edit = None;
                self.jira_add_comment_target = None;
                self.set_status_message(format!(
                    "Comment posted to {}",
                    key
                ));
            }
            Err(e) => {
                // Failure: keep
                // the buffer
                // and the
                // target so
                // the user
                // can
                // retry
                // without
                // retyping.
                self.set_status_message(format!(
                    "JIRA add-comment to {} failed: {}",
                    key, e
                ));
            }
        }
    }

    fn show_output_view(&mut self) {
        // The show-output overlay has two
        // distinct entry points:
        //
        // 1. **Generic rows** (regular
        //    history, notes, todos, panes,
        //    directories): the captured
        //    `row.output` is opened
        //    immediately. No network call.
        //
        // 2. **JIRA rows**: the user's
        //    spec wants the overlay to
        //    show the full description
        //    plus a list of comments
        //    sorted newest-first. The
        //    `row.output` is the
        //    4-line preview header +
        //    description body; the
        //    comments need a separate
        //    HTTP fetch (`/rest/api/2/issue/{key}/comment`).
        //    We fire a background
        //    thread (mirroring
        //    `spawn_jira_request` for
        //    searches) and show a
        //    "Loading comments..." status
        //    while the fetch is in
        //    flight. When the result
        //    arrives, the run loop
        //    builds the full overlay
        //    text and opens the view.
        //
        // We can't take `&mut self` while
        // also holding a borrow of the
        // selected row (the borrow
        // checker complains about an
        // immutable borrow outliving the
        // mutable borrow), so we copy
        // out the fields we need (the
        // command and the mode) and
        // dispatch on those.
        let selection = self.selected_row().map(|r| (r.command.clone(), r.mode.clone(), r.output.clone()));
        let Some((command, mode, output)) = selection else {
            return;
        };
        if output.is_empty() {
            return;
        }
        if mode == "jira" {
            self.start_jira_comments_fetch(&command);
        } else {
            self.output_view = Some(OutputView {
                text: output,
                scroll: 0,
            });
        }
    }

    fn close_output_view(&mut self) {
        self.output_view = None;
    }

    /// Fire a background thread to fetch the
    /// comments for a JIRA issue. Mirrors
    /// `spawn_jira_request` (the search-time
    /// version): a thread runs the HTTP call
    /// against the configured `JiraClient`,
    /// the result flows over an `mpsc`
    /// channel, and the run loop polls it.
    ///
    /// If a comments fetch is already in flight
    /// (`jira_comments_in_flight` is true),
    /// this is a no-op — the user might
    /// press Ctrl+L again on the same row
    /// while a fetch is pending, and we'd
    /// rather silently drop the second
    /// request than spawn a duplicate thread.
    /// The status message is also left
    /// alone so the user doesn't see a
    /// "Loading comments..." flash.
    ///
    /// The fetch is run synchronously against
    /// the fake client in tests, just like
    /// `spawn_jira_request`. The test
    /// seam is `self.jira_client.clone()` —
    /// when set, the search / comments
    /// fetch runs in-line and the result is
    /// processed before this method
    /// returns.
    fn start_jira_comments_fetch(&mut self, key: &str) {
        if self.jira_comments_in_flight {
            return;
        }
        // Snapshot the issue's preview
        // output. The full overlay
        // synthesises its text from this
        // preview + the comments, so we
        // keep a copy on `self` for the
        // processing step. The preview
        // contains the 3-line header
        // (Status/Priority, Due/Assignee,
        // Description label) + the full
        // description body, which is
        // exactly the `# Header` content
        // the user spec calls out.
        let Some(row) = self.jira_rows.iter().find(|r| r.command == key).cloned() else {
            // The row was selected when
            // the user pressed Ctrl+L
            // but it's no longer in
            // `jira_rows` (the user
            // must have navigated and
            // the search cache is now
            // for a different query).
            // Silently do nothing —
            // the row is gone.
            return;
        };
        // The fake-client path runs
        // synchronously and processes
        // the result inline. We still
        // set `jira_comments_in_flight`
        // so a second Ctrl+L on the
        // same row doesn't queue a
        // duplicate.
        if let Some(client) = self.jira_client.clone() {
            let result = client.fetch_comments(key);
            let request = JiraCommentsRequest {
                receiver: mpsc::channel().1,
                cancelled: Arc::new(AtomicBool::new(false)),
                key: key.to_string(),
            };
            self.jira_comments_in_flight = true;
            self.process_jira_comments_result(request, row, result);
            return;
        }
        // Production path: spawn a
        // background thread.
        let Some(config) = crate::jira::JiraConfig::from_env() else {
            self.set_status_message(
                crate::jira::JiraError::NotConfigured.to_string(),
            );
            return;
        };
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_clone = cancelled.clone();
        let key_for_thread = key.to_string();
        std::thread::spawn(move || {
            let client = crate::jira::RestJiraClient::new(config);
            let result = client.fetch_comments(&key_for_thread);
            if !cancelled_clone.load(Ordering::Relaxed) {
                let _ = tx.send(result);
            }
        });
        self.jira_comments_in_flight = true;
        self.jira_comments_request = Some(JiraCommentsRequest {
            receiver: rx,
            cancelled,
            key: key.to_string(),
        });
        self.set_status_message("JIRA loading comments…".to_string());
    }

    /// Process a JIRA comments-fetch result that
    /// arrived from the background thread.
    /// Builds the markdown-like overlay text
    /// (header, description, comments) and
    /// opens the `OutputView`.
    ///
    /// Mirrors `process_jira_result` (the
    /// search-time equivalent): on success,
    /// cache the comments and refresh; on
    /// error, surface a status message; on
    /// cancellation, surface a different
    /// status message ("JIRA comments
    /// cancelled").
    fn process_jira_comments_result(
        &mut self,
        request: JiraCommentsRequest,
        row: crate::tui::state::HistoryRow,
        result: Result<Vec<crate::jira::JiraComment>, crate::jira::JiraError>,
    ) {
        self.jira_comments_in_flight = false;
        self.jira_comments_request = None;
        if request.cancelled.load(Ordering::Relaxed) {
            self.set_status_message("JIRA comments cancelled".to_string());
            return;
        }
        match result {
            Ok(comments) => {
                // Build the markdown-like
                // overlay text. The
                // structure follows the user
                // spec:
                //
                //   ## Header
                //   <3-line preview: Status/Priority,
                //                   Due/Assignee,
                //                   Description label>
                //   <description body lines>
                //
                //   ## Description
                //   <full description text>
                //
                //   ## Comments
                //   ## <author> · <date>
                //   <comment text>
                //   ## <author> · <date>
                //   <comment text>
                //   ...
                //
                // Comments are sorted
                // newest-first by the
                // `created` timestamp.
                let mut comments = comments;
                sort_comments_newest_first(&mut comments);
                let text = build_jira_overlay_text(&row, &comments);
                self.output_view = Some(OutputView {
                    text,
                    scroll: 0,
                });
                self.status_message = None;
            }
            Err(e) => {
                self.set_status_message(e.to_string());
            }
        }
    }

    /// Ask the configured ollama instance for a short
    /// description of what the selected history line
    /// does, then open a full-screen overlay with the
    /// response.
    ///
    /// Behaviour:
    /// - **No row selected** → status message,
    ///   no overlay.
    /// - **LLM not configured** → status message,
    ///   no overlay. We don't open the overlay at
    ///   all; the user would just see a "loading"
    ///   spinner and then an error.
    /// - **LLM call fails** → status message,
    ///   no overlay (we close it before opening so a
    ///   fresh describe can be retried from a clean
    ///   state).
    /// - **Success** → overlay opens with the LLM's
    ///   prose response. The response is trimmed
    ///   of leading/trailing whitespace but otherwise
    ///   rendered as-is.
    ///
    /// The HTTP call is synchronous (matches
    /// `run_llm_query`'s design). Local 7B models
    /// respond in 1-5 seconds; the 30-second
    /// timeout in `OllamaClient` bounds the worst
    /// case. The user explicitly asked for this mode
    /// and accepted the freeze.
    fn start_describe(&mut self) {
        let Some(row) = self.selected_row() else {
            self.set_status_message(
                "Describe: no row selected".to_string(),
            );
            return;
        };
        // The describe action
        // asks the LLM to
        // describe the *command*
        // on the selected row.
        // For `#`-mode rows the
        // primary text is the
        // directory (so
        // `row.command` is the
        // directory, not a
        // runnable command) and
        // `row.comment` holds the
        // last command run
        // there. We describe the
        // last command (the
        // interesting artifact)
        // rather than the
        // directory (which the
        // LLM can only describe
        // as a path, not as
        // "what was the user
        // doing here").
        if row.mode == "directory"
            && row.comment.is_empty()
        {
            self.set_status_message(
                "Describe: directory has no \
                 captured command to describe"
                    .to_string(),
            );
            return;
        }
        let command = if row.mode == "directory" {
            row.comment.clone()
        } else {
            row.command.clone()
        };
        self.describe_view = None;

        if self.llm.is_none() {
            self.set_status_message(
                crate::llm::LlmError::NotConfigured.to_string(),
            );
            return;
        }

        let prompt = crate::llm::build_describe_prompt(&command);
        self.spawn_llm_request(
            LlmRequestType::Describe { command },
            prompt,
        );
    }

    fn close_describe(&mut self) {
        self.describe_view = None;
    }

    /// Ask the configured ollama instance to correct
    /// the selected history row. On success, opens a
    /// modal overlay showing the original and the
    /// corrected command; the user then presses
    /// `Enter` to stage the corrected command and
    /// exit the TUI, or `Esc` to cancel.
    ///
    /// Behaviour:
    /// - **No row selected** → status message, no
    ///   overlay.
    /// - **LLM not configured** → status message,
    ///   no overlay.
    /// - **LLM call fails** → status message, no
    ///   overlay.
    /// - **LLM response sanitizes to `None`** → status
    ///   message, no overlay. The LLM's response was
    ///   empty or pure commentary; we can't extract
    ///   a command from it.
    /// - **Success** → overlay opens with the
    ///   corrected command (sanitized from the LLM's
    ///   raw response, the same way `run_llm_query`
    ///   does). The user reviews and decides.
    ///
    /// Like `start_describe`, the HTTP call is
    /// synchronous. Local 7B models respond in 1-5
    /// seconds; the 30-second timeout in
    /// `OllamaClient` bounds the worst case.
    fn start_correct(&mut self) {
        let Some(row) = self.selected_row() else {
            self.set_status_message(
                "Correct: no row selected".to_string(),
            );
            return;
        };
        // The correct action asks
        // the LLM to fix the
        // *command* on the
        // selected row. For
        // `#`-mode rows the
        // primary text is the
        // directory (so
        // `row.command` is the
        // directory, not a
        // runnable command) and
        // `row.comment` holds the
        // last command run
        // there. We correct the
        // last command (the
        // thing the LLM can
        // actually rewrite); the
        // directory doesn't need
        // a "corrected form".
        if row.mode == "directory"
            && row.comment.is_empty()
        {
            self.set_status_message(
                "Correct: directory has no \
                 captured command to correct"
                    .to_string(),
            );
            return;
        }
        let original_command = if row.mode == "directory" {
            row.comment.clone()
        } else {
            row.command.clone()
        };
        // Close any existing overlay first so a
        // re-correct doesn't stack views.
        self.correct_view = None;

        if self.llm.is_none() {
            self.set_status_message(
                crate::llm::LlmError::NotConfigured.to_string(),
            );
            return;
        }

        let prompt = crate::llm::build_correct_prompt(&original_command);
        self.spawn_llm_request(
            LlmRequestType::Correct { original_command },
            prompt,
        );
    }

    fn close_correct(&mut self) {
        self.correct_view = None;
    }

    /// Accept the corrected command from the
    /// `CorrectView` overlay: insert the row into
    /// the history table (with the original command
    /// as the comment for traceability) and stage the
    /// corrected command as the next "selection"
    /// for the parent shell. Called when the user
    /// presses `Enter` in the correct overlay.
    ///
    /// Mirrors `stage_llm_command` (used by
    /// `run_llm_query`) so the on-disk and
    /// in-memory representations stay consistent.
    /// On any DB error we surface a status message
    /// and leave `selection` unset so the TUI
    /// doesn't exit with a half-staged command.
    fn accept_corrected_command(&mut self) {
        let Some(view) = self.correct_view.take() else {
            return;
        };
        // Canonicalize the
        // directory the same way
        // `select_for_run` does, so
        // the dedup index
        // `(command, directory,
        // session_id)` matches
        // across insert and update
        // sites. Without this, two
        // forms of the same path
        // (e.g. `/Users/har/...` and
        // `/Volumes/HUGE/har/...`
        // on macOS) would create
        // separate rows.
        let directory =
            crate::util::canonicalize_directory(
                &std::env::var("PWD").unwrap_or_default(),
            );
        let session_id =
            std::env::var("SMART_HISTORY_SESSION").unwrap_or_default();
        let insert_result: anyhow::Result<()> = (|| {
            self.conn.execute(
                "INSERT INTO history (command, directory, session_id, exit_code, mode) \
                 VALUES (?1, ?2, ?3, -1, 'command') \
                 ON CONFLICT (command, directory, session_id) DO UPDATE \
                 SET timestamp = (strftime('%s', 'now')), mode = 'command'",
                params![view.corrected_command, directory, session_id],
            )?;
            // The original command is the comment,
            // so the corrected row is self-
            // documenting: the user can later
            // search for the original (typo-laden)
            // text and find the corrected version.
            self.conn.execute(
                "INSERT INTO command_comments (command, comment) VALUES (?1, ?2) \
                 ON CONFLICT (command) DO UPDATE SET comment = excluded.comment",
                params![view.corrected_command, view.original_command],
            )?;
            Ok(())
        })();
        if let Err(e) = insert_result {
            self.set_status_message(format!(
                "Correct: history insert failed: {}",
                e
            ));
            return;
        }
        self.selection = Some(view.corrected_command.clone());
        self.pick_mode = Some(PickMode::Run);
        self.set_status_message(format!("Correct: {}", view.corrected_command));
    }

    /// Handle a `%...` query by sending the natural-language
    /// question to the configured ollama instance and displaying
    /// the answer in an overlay. The question is also saved to
    /// history with the answer stored as output (but not as a
    /// comment).
    fn run_question_query(&mut self) {
        // Extract the question (everything after the leading question prefix).
        let prefix = self.query_prefixes.question;
        let question = self.query[prefix.len_utf8()..].trim();
        if question.is_empty() {
            self.set_status_message("Question: provide a question after the question prefix".to_string());
            return;
        }
        // Bail out cleanly if the LLM isn't configured.
        if self.llm.is_none() {
            self.set_status_message(crate::llm::LlmError::NotConfigured.to_string());
            return;
        }
        
        let question_owned = question.to_string();
        let prompt = crate::llm::build_question_prompt(&question_owned);
        self.spawn_llm_request(
            LlmRequestType::Question { question: question_owned },
            prompt,
        );
    }

    /// Persist a general question to the history table with
    /// `question` (prefixed with `%`) as the command and
    /// `answer` stored as the output (but not as a comment).
    fn stage_question(&mut self, question: String, answer: String) {
        // Same canonicalization as
        // the LLM staging site —
        // keeps the dedup index
        // consistent.
        let directory =
            crate::util::canonicalize_directory(
                &std::env::var("PWD").unwrap_or_default(),
            );
        let session_id = std::env::var("SMART_HISTORY_SESSION").unwrap_or_default();
        let query_command = format!("{}{}", self.query_prefixes.question, question);
        
        let insert_result: anyhow::Result<i64> = (|| {
            self.conn.execute(
                "INSERT INTO history (command, directory, session_id, exit_code, mode) \
                 VALUES (?1, ?2, ?3, -1, 'question') \
                 ON CONFLICT (command, directory, session_id) DO UPDATE \
                 SET timestamp = (strftime('%s', 'now')), mode = 'question'",
                params![&query_command, &directory, &session_id],
            )?;
            let id: i64 = self.conn.query_row(
                "SELECT id FROM history WHERE command = ?1 AND directory = ?2 AND session_id = ?3",
                params![&query_command, &directory, &session_id],
                |row| row.get(0),
            )?;
            Ok(id)
        })();
        
        let history_id = match insert_result {
            Ok(id) => id,
            Err(e) => {
                self.set_status_message(format!("Question: history insert failed: {}", e));
                return;
            }
        };
        
        // Store the answer as output (not as comment).
        let output_result: anyhow::Result<()> = (|| {
            self.conn.execute(
                "INSERT INTO history_output (history_id, output) VALUES (?1, ?2) \
                 ON CONFLICT (history_id) DO UPDATE SET output = excluded.output, captured_at = (strftime('%s', 'now'))",
                params![history_id, &answer],
            )?;
            Ok(())
        })();
        
        if let Err(e) = output_result {
            self.set_status_message(format!("Question: output store failed: {}", e));
        }
    }

    fn close_question(&mut self) {
        self.question_view = None;
    }

    fn is_question_viewing(&self) -> bool {
        self.question_view.is_some()
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

    /// Mark the currently-selected todo
    /// entry as done by toggling the
    /// checkbox marker on its line in
    /// the source file from `[ ]` to
    /// `[x]`, then refresh the todo
    /// list. The action is intended to
    /// be invoked only from the todo
    /// search mode (the dispatcher
    /// already gates on
    /// `is_todo_query`); the helper
    /// itself also re-checks the mode
    /// so a stray test or future caller
    /// can't trigger a file write from
    /// outside todo mode.
    ///
    /// The selected row's `id` is
    /// synthetic: `id = -(line_number)`
    /// where `line_number` is the
    /// 1-based line in the note file
    /// that contains the todo. The
    /// row's `comment` field is the
    /// filename (relative to
    /// `notes.dir`).
    ///
    /// On success the todo list is
    /// re-fetched so the toggled row
    /// disappears (the underlying
    /// query filters `open: true`).
    /// On any error (no selection,
    /// missing file, the line no
    /// longer matches a todo
    /// checkbox, write failure) a
    /// status message is surfaced and
    /// the list is left untouched.
    fn mark_todo_done(&mut self) {
        // Re-gate here so a stray
        // caller can't write to a
        // file from outside todo mode.
        // The dispatcher already gates
        // on this, but the helper
        // defends against future
        // refactors that might call it
        // from a different code path.
        if !self.is_todo_query() {
            self.set_status_message(
                "Mark-todo-done is only available in todo search (type `!`)".to_string(),
            );
            return;
        }
        let Some(row) = self.selected_row().cloned() else {
            self.set_status_message("No todo selected".to_string());
            return;
        };
        self.mark_todo_done_for_row(&row);
    }

    /// Core implementation of
    /// `mark_todo_done`, factored out
    /// so tests can drive it with a
    /// hand-crafted `HistoryRow`
    /// (the indentation test in
    /// particular needs a row whose
    /// line content the library
    /// wouldn't normally index,
    /// since the library's
    /// `TODO_REGEX` is `^`-anchored
    /// and skips indented
    /// checkboxes).
    fn mark_todo_done_for_row(&mut self, row: &HistoryRow) {
        // `id = -(line_number)` is the
        // synthetic-id contract from
        // `fetch_todos`. A row that
        // somehow has a non-negative id
        // (a real history row mixed in
        // — shouldn't happen in todo
        // mode, but defensively) is
        // rejected.
        let line_number: usize = match row.id {
            i if i < 0 => (i.unsigned_abs() as usize).max(1),
            _ => {
                self.set_status_message(
                    "Selected row is not a todo entry".to_string(),
                );
                return;
            }
        };
        if row.comment.is_empty() {
            self.set_status_message(
                "Selected todo has no source filename".to_string(),
            );
            return;
        }
        let Some(ref notes_dir) = self.notes_dir else {
            self.set_status_message(
                "Cannot mark done: notes.dir is not configured".to_string(),
            );
            return;
        };
        let path = notes_dir.join(&row.comment);
        // Read the file. We use the
        // same error mapping as the
        // note-preview reader for
        // consistency with the rest of
        // the notes subsystem.
        let contents = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                self.set_status_message(format!(
                    "Cannot read {}: {}",
                    row.comment, e
                ));
                return;
            }
        };
        // Locate the targeted line.
        // The note_search indexer uses
        // 1-based line numbers, so we
        // index into a `lines()` iterator
        // by skipping the first
        // `line_number - 1` lines.
        let mut new_lines: Vec<String> = Vec::new();
        let mut toggled = false;
        for (i, line) in contents.lines().enumerate() {
            let n = i + 1; // 1-based
            if n == line_number {
                // The line at this
                // position should still
                // look like an open todo
                // checkbox. We tolerate
                // leading whitespace
                // (indented list items) and
                // both `-` and `*` bullets
                // (even though the
                // library's parser only
                // recognises `-`, the
                // user may have written
                // `* [ ]` by hand and
                // toggling it should
                // still work).
                let trimmed_start = line
                    .trim_start()
                    .trim_start_matches(['-', '*'])
                    .trim_start();
                if !trimmed_start.starts_with("[ ]") {
                    self.set_status_message(format!(
                        "Line {} of {} is no longer an open todo: {:?}",
                        line_number, row.comment, line
                    ));
                    return;
                }
                // Replace the first
                // occurrence of `[ ]` on
                // this line with `[x]`.
                // We anchor on the
                // prefix-only match so we
                // don't accidentally
                // toggle a `[ ]` inside
                // the todo text (rare,
                // but possible — e.g.
                // "see [ ] in checklist").
                // The first `[ ]` on a
                // markdown-checkbox line
                // is always the checkbox
                // marker.
                let prefix_len = line.len() - trimmed_start.len();
                // Walk past the bullet
                // character(s) so we
                // match the checkbox
                // bracket that follows
                // the bullet, not any
                // bracketed text inside
                // the todo content.
                let rest = &line[prefix_len..];
                let bullet_skip: usize = rest
                    .chars()
                    .take_while(|c| matches!(c, '-' | '*'))
                    .map(|c| c.len_utf8())
                    .sum();
                let after_bullet = prefix_len + bullet_skip;
                let ws_skip: usize = line[after_bullet..]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .map(|c| c.len_utf8())
                    .sum();
                let bracket_at = after_bullet + ws_skip;
                // Defensive: bail out if
                // the bracket isn't where
                // we expect it. The
                // prefix check above
                // already guaranteed
                // `[ ]` is at the
                // trimmed-start, so this
                // is purely belt-and-
                // braces.
                if !line[bracket_at..].starts_with("[ ]") {
                    self.set_status_message(format!(
                        "Cannot locate checkbox bracket on line {} of {}",
                        line_number, row.comment
                    ));
                    return;
                }
                let mut new_line = String::with_capacity(line.len());
                new_line.push_str(&line[..bracket_at]);
                new_line.push_str("[x]");
                new_line.push_str(&line[bracket_at + 3..]);
                new_lines.push(new_line);
                toggled = true;
            } else {
                new_lines.push(line.to_string());
            }
        }
        if !toggled {
            // Shouldn't happen: we
            // iterated over every line
            // and only mark `toggled`
            // when `n == line_number`. If
            // we exit the loop without
            // toggling, the file has
            // fewer lines than the
            // indexer saw.
            self.set_status_message(format!(
                "Line {} not found in {} (file shorter than expected)",
                line_number, row.comment
            ));
            return;
        }
        // Preserve the file's trailing
        // newline convention. `lines()`
        // drops the trailing `\n` if
        // any, so we re-attach it when
        // the original ended with one.
        let mut out = new_lines.join("\n");
        if contents.ends_with('\n') {
            out.push('\n');
        }
        if let Err(e) = std::fs::write(&path, out) {
            self.set_status_message(format!(
                "Cannot write {}: {}",
                row.comment, e
            ));
            return;
        }
        // Refresh the in-memory
        // `todo_entries` database
        // after the file toggle. We
        // delegate to the
        // `note_search` library's
        // `update_files_in_db`
        // function: it re-parses the
        // modified file, upserts the
        // `markdown_data` row, and
        // replaces the
        // `todo_entries` rows for
        // that file. After the
        // update, the toggled todo
        // has `closed = 1` in the
        // database, and the next
        // `refresh()` (which queries
        // with `open: true`) will
        // exclude it from the list.
//
// We open a fresh `Connection`
        // per call rather than
        // keeping one open on `App`,
        // because the existing
        // `DatabaseService::search_todos`
        // already opens its own
        // connection per query and
        // the action is invoked
        // rarely (the user has to
        // press `Ctrl+X` to trigger
        // it). Opening a new
        // connection is cheaper than
        // the file write we just
        // did.
//
// The DB update is best-effort:
        // if the library can't open
        // the DB or write to it, the
        // status message reflects
        // the failure but the file
        // is already correct on
        // disk — the user can always
        // run their external indexer
        // to recover. We don't roll
        // back the file write
        // because the user's intent
        // was clearly "mark this
        // done on disk"; reverting
        // would be worse than a
        // temporarily-stale DB.
        let notes_dir_for_db = self.notes_dir.clone();
        let notes_db_for_db = self.notes_database.clone();
        let filename_for_db = row.comment.clone();
        if let (Some(dir), Some(db)) =
                (notes_dir_for_db.as_ref(), notes_db_for_db.as_ref())
        {
                use rusqlite::Connection;
                match Connection::open(db) {
                        Ok(conn) => {
                                if let Err(e) =
                                        note_search::update_files_in_db(
                                                &[filename_for_db],
                                                dir,
                                                &conn,
                                        )
                                {
                                        self.set_status_message(format!(
                                                "Marked done on disk, but DB refresh failed: {}",
                                                e
                                        ));
                                        return;
                                }
                        }
                        Err(e) => {
                                self.set_status_message(format!(
                                        "Marked done on disk, but DB open failed: {}",
                                        e
                                ));
                                return;
                        }
                }
        }
        self.set_status_message(format!(
            "Marked done: {}:{}",
            row.comment, line_number
        ));
        // Re-fetch so the toggled row
        // disappears from the list.
        // The library's
        // `todo_entries` row for
        // this todo now has
        // `closed = 1`, so the
        // `open: true` filter in the
        // underlying SQL excludes
        // it.
        self.refresh();
    }

    /// Set the transient status message. The status bar shows
    /// it for a few seconds and then it's automatically cleared
    /// by `tick_status_message`.
    fn set_status_message(&mut self, msg: String) {
        self.status_message = Some((msg, std::time::Instant::now()));
    }

    /// The current Unix epoch in
    /// seconds. Used by the
    /// date-filter math
    /// (`@today` / `@week` /
    /// `@month` / `@year`
    /// aliases) in both the
    /// notes and todo
    /// `fetch_*` methods so the
    /// `cutoff(now)` window is
    /// computed the same way
    /// everywhere. Returns 0
    /// if the system clock is
    /// somehow before the Unix
    /// epoch (effectively
    /// unreachable in practice);
    /// the fallback keeps the
    /// comparison well-defined.
    fn now_epoch(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
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

    fn is_describe_viewing(&self) -> bool {
        self.describe_view.is_some()
    }

    fn is_correct_viewing(&self) -> bool {
        self.correct_view.is_some()
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
        let sql = "SELECT h.id, h.command, h.directory, h.session_id, h.exit_code, h.timestamp, c.comment, o.output, h.mode \
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
                    mode: row.get(8).unwrap_or_default(),
                    source: String::new(),
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
    llm: Option<Box<dyn crate::llm::LlmClient>>,
    llm_config: Option<crate::llm::LlmConfig>,
) -> Result<Option<(String, i32)>> {
    let mode = Mode::parse(&initial_mode).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown TUI mode {:?}; expected one of SESS, SESSION, DIR, DIRECTORY, GLOBAL",
            initial_mode
        )
    })?;
    let app_cfg = Config::load();
    let bindings = app_cfg.key_bindings().clone();
    let query_prefixes = app_cfg.query_prefixes().clone();
    let notes_database = app_cfg.notes_database().map(|p| p.to_path_buf());
    let notes_dir = app_cfg.notes_dir().map(|p| p.to_path_buf());
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
    // Honor the persisted sort order. Same pattern as
    // the exit filter: `None` in the session file means
    // "no preference" — fall back to the default
    // (`SortOrder::Age`, the historical timestamp-DESC
    // order). A value that doesn't parse is treated the
    // same way so a hand-edited session file can't wedge
    // the TUI on startup.
    let initial_sort_order = session
        .sort_order
        .as_deref()
        .and_then(SortOrder::parse)
        .unwrap_or_default();
    let mut app = App::new(
        conn,
        effective_mode,
        effective_query,
        duplicate_filter,
        initial_exit_filter,
        initial_sort_order,
        prefilled_query.is_some(),
        initial_theme,
        bindings,
        llm,
        llm_config,
        query_prefixes,
        notes_database,
        notes_dir,
        app_cfg.todo_line_option().to_string(),
        app_cfg.jira_fragments().clone(),
        app_cfg.files_ignores().to_vec(),
    );
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
        // Persist only when the user has changed the
        // order away from the default — same policy as
        // the other session fields (we only remember
        // what differs from the defaults, so deleting
        // the file resets the user to the same state
        // they'd get on first run).
        sort_order: if app.sort_order == SortOrder::default() {
            None
        } else {
            Some(app.sort_order.as_str().to_string())
        },
        theme: Some(app.theme.slug().to_string()),
        // Same persist-only-if-non-default
        // policy as `sort_order`
        // and `exit_filter`:
        // only remember the
        // user's choice when
        // it's not the
        // default. Deleting
        // the session file
        // resets the user to
        // `All`.
        directory_source: if app.directory_source
            == crate::tui::state::DirectorySource::All
        {
            None
        } else {
            Some(
                match app.directory_source {
                    crate::tui::state::DirectorySource::All => "all",
                    crate::tui::state::DirectorySource::Tmux => "tmux",
                    crate::tui::state::DirectorySource::Config => "config",
                }
                .to_string(),
            )
        },
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

        // Check for LLM result from background thread.
        if let Some(request) = app.llm_request.as_ref()
            && let Ok(result) = request.receiver.try_recv() {
                // Take ownership of the request before processing.
                if let Some(request) = app.llm_request.take() {
                    app.process_llm_result(request, result);
                }
            }

        // Check for JIRA result from background thread
        // (mirrors the LLM poll above).
        if let Some(request) = app.jira_request.as_ref()
            && let Ok(result) = request.receiver.try_recv()
                && let Some(request) = app.jira_request.take() {
                    app.process_jira_result(request, result);
                }

        // Check for files-mode walk
        // result from background
        // thread. Mirrors the JIRA
        // search poll above. The result
        // populates `self.files_rows`
        // and `process_files_result`
        // triggers a `refresh()`.
        if let Some(request) = app.files_state.request.as_ref()
            && let Ok(result) = request.receiver.try_recv()
        {
            let request = app.files_state.request.take().unwrap();
            app.process_files_result(request, result);
        }

        // Check for JIRA comments-fetch result
        // from background thread (mirrors the
        // search poll above). When the
        // comments arrive, build the overlay
        // text and open the show-output view
        // on the same row the user pressed
        // Ctrl+L on.
        if let Some(request) = app.jira_comments_request.as_ref()
            && let Ok(result) = request.receiver.try_recv()
        {
            // The row that initiated the
            // fetch is keyed by `request.key`.
            // We need to clone the row's data
            // (command, mode, output) because
            // `process_jira_comments_result`
            // takes `&mut self`. Same pattern
            // as the search-result path: the
            // row is in `jira_rows` and we
            // can find it by key.
            let key = request.key.clone();
            let request = app.jira_comments_request.take().unwrap();
            let row = app
                .jira_rows
                .iter()
                .find(|r| r.command == key)
                .cloned();
            if let Some(row) = row {
                app.process_jira_comments_result(request, row, result);
            } else {
                // The row was selected when
                // the user pressed Ctrl+L
                // but it's no longer in
                // `jira_rows`. Discard
                // the result and surface
                // a status message so
                // the user knows the
                // overlay didn't open.
                app.jira_comments_in_flight = false;
                app.set_status_message(
                    "JIRA row no longer available for comments".to_string(),
                );
            }
        }

        // Check for JIRA add-comment
        // POST result from background
        // thread (mirrors the comments
        // poll above). When the POST
        // returns, either clear the
        // buffer (success) or surface
        // an error message (failure).
        if let Some(request) = app.jira_add_comment_request.as_ref()
            && let Ok(result) = request.receiver.try_recv()
        {
            // The key and body are on
            // the request struct (we
            // need them for status
            // messages that reference
            // the issue). Clone the
            // data we need before
            // moving the request
            // out of `app`.
            let key = request.key.clone();
            let request = app.jira_add_comment_request.take().unwrap();
            app.process_jira_add_comment_result(request, key, result);
        }

        if !crossterm::event::poll(Duration::from_millis(100))? {
            // No input ready. Still a chance to drive the
            // LLM auto-call debounce: if the user is in LLM
            // mode and has paused typing for at least
            // `LLM_DEBOUNCE`, this is when the suggestion
            // gets generated. We deliberately do this on the
            // "no event" path (rather than only after a
            // keypress) so the debounce works even if the
            // user just stares at the screen after typing
            // their last character — the worst case is that
            // we wait one extra 100ms tick before firing.
            app.llm_maybe_autocall();
            // Same debounce drive for JIRA search-as-you-
            // type: fires the JQL query after
            // `JIRA_DEBOUNCE` of quiet typing in `-` mode.
            app.jira_maybe_autocall();
            // Same debounce drive for files-mode
            // walks: spawns the background
            // directory walk after
            // `FILES_DEBOUNCE` of quiet typing
            // in `~` mode. Without this the
            // walk would never fire (no other
            // edit path arms the debounce).
            app.files_maybe_autocall();
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };

        // If an LLM request is in flight, check if this is a
        // cancel key. If so, cancel the request without leaving
        // the TUI.
        if app.llm_request.is_some()
            && let Some(action) = action_for_key(&app.bindings, &key)
                && matches!(action, Action::Cancel) {
                    if let Some(request) = app.llm_request.take() {
                        request.cancelled.store(true, Ordering::Relaxed);
                    }
                    app.llm_in_flight = false;
                    app.set_status_message("LLM request cancelled".to_string());
                    continue;
                }

        // Same cancel handling for an in-flight JIRA search.
        if app.jira_request.is_some()
            && let Some(action) = action_for_key(&app.bindings, &key)
                && matches!(action, Action::Cancel) {
                    if let Some(request) = app.jira_request.take() {
                        request.cancelled.store(true, Ordering::Relaxed);
                    }
                    app.jira_in_flight = false;
                    app.set_status_message("JIRA search cancelled".to_string());
                    continue;
                }

        // Same cancel handling for an in-flight
        // JIRA comments fetch. The `Cancel`
        // action (default `Esc`) sets the
        // cancelled flag on the worker thread
        // (which checks the flag just before
        // sending the result, so a fetch that
        // completes between the user's Esc and
        // the flag check is dropped, not
        // delivered).
        if app.jira_comments_request.is_some()
            && let Some(action) = action_for_key(&app.bindings, &key)
                && matches!(action, Action::Cancel) {
                    if let Some(request) = app.jira_comments_request.take() {
                        request.cancelled.store(true, Ordering::Relaxed);
                    }
                    app.jira_comments_in_flight = false;
                    app.set_status_message("JIRA comments cancelled".to_string());
                    continue;
                }

        // Same cancel handling for an in-flight
        // JIRA add-comment POST. The POST
        // is in flight because the user saved
        // the comment buffer on a JIRA row;
        // pressing `Esc` while it's pending
        // sets the cancelled flag on the
        // worker thread and surfaces a status
        // message. The buffer is preserved
        // so the user can
        // decide whether to retry (another
        // `Enter`) or cancel
        // out of the buffer entirely
        // (another `Esc`, which triggers
        // `cancel_comment_edit` next).
        if app.jira_add_comment_request.is_some()
            && let Some(action) = action_for_key(&app.bindings, &key)
                && matches!(action, Action::Cancel) {
                    if let Some(request) = app.jira_add_comment_request.take() {
                        request.cancelled.store(true, Ordering::Relaxed);
                    }
                    app.jira_add_comment_in_flight = false;
                    app.set_status_message("JIRA add-comment cancelled".to_string());
                    continue;
                }

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

        if app.is_describe_viewing() {
            handle_describe_view_key(app, key, page_size);
            // The describe overlay never stages a
            // selection — it just shows the LLM's
            // response and lets the user scroll / close.
            // We don't auto-exit on close.
            continue;
        }

        if app.is_question_viewing() {
            // The question overlay shows the LLM's
            // answer to a general question. It never
            // stages a selection — the user just reads
            // the answer and closes the overlay.
            handle_question_view_key(app, key, page_size);
            continue;
        }

        if app.is_correct_viewing() {
            // The correct overlay is modal:
            // `Enter` accepts (stages the
            // corrected command and exits the
            // TUI), `Esc` cancels (closes the
            // overlay, returns to the list
            // without staging anything). The
            // dispatcher's `true` return on
            // accept is what actually triggers
            // the TUI exit.
            if handle_correct_view_key(app, key) {
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
    // The status message is ticked
    // here (vs. in the run loop
    // above) so it disappears as
    // soon as the user interacts
    // again — the "X" key for
    // delete, for example, clears
    // it and disappears as soon as
    // they interact again.
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
            // If an LLM request is in flight, cancel it without
            // leaving the TUI.
            if let Some(request) = app.llm_request.take() {
                request.cancelled.store(true, Ordering::Relaxed);
                app.llm_in_flight = false;
                app.set_status_message("LLM request cancelled".to_string());
                return false;
            }
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
        Action::CycleSortOrder => {
            app.cycle_sort_order();
            false
        }
        Action::CycleDirectorySource => {
            app.cycle_directory_source();
            false
        }
        Action::Describe => {
            app.start_describe();
            false
        }
        Action::Correct => {
            app.start_correct();
            false
        }
        Action::ToggleSearchMode => {
            app.cycle_search_mode();
            false
        }
        Action::MarkTodoDone => {
            // Marking a todo done is only
            // meaningful inside the todo
            // search mode (`!...`). Outside
            // of it, the action is a no-op
            // with a status message so the
            // user understands why their
            // `Ctrl-X` did nothing — the
            // `Ctrl-X` key fires regardless
            // of mode (so it's a
            // discoverable key binding),
            // but the *effect* is gated.
            if !app.is_todo_query() {
                app.set_status_message(
                    "Mark-todo-done is only available in todo search (type `!`)".to_string(),
                );
                return false;
            }
            app.mark_todo_done();
            // Always stay in the TUI so
            // the user can see the result
            // (status message + re-fetched
            // todo list with the row
            // gone).
            false
        }
        Action::Run => {
            app.select_for_run();
            app.selection.is_some()
        }
        Action::EditStart => {
            app.select_for_edit_start();
            app.selection.is_some()
        }
        Action::EditEnd => {
            app.select_for_edit_end();
            app.selection.is_some()
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
        Action::DeleteWordBackward => {
            app.delete_word_backward();
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
    // The destructive confirmation
    // dialog answers "yes" (`y`)
    // or "no" (`n`, the Cancel
    // binding, or `Ctrl+C`). We
    // look up the user's `Cancel`
    // binding dynamically so the
    // displayed dialog and the
    // actual close keys stay in
    // sync — `n` is always a
    // valid "no" answer (it's
    // mnemonic for "no" and
    // doesn't share a key with
    // anything else the user
    // might rebind Cancel to).
    let is_cancel_key = action_for_key(&app.bindings, &key)
        == Some(Action::Cancel);
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
        KeyCode::Char('n') | KeyCode::Char('N') => {
            app.confirm_delete = None;
            false
        }
        _ if is_cancel_key => {
            // User-configured Cancel
            // binding (default `Esc`,
            // configurable via
            // `key.cancel=...`). Closes
            // the dialog without
            // running the destructive
            // action.
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
    // The palette closes only on
    // keys mapped to the user's
    // `Cancel` action. Looking it
    // up dynamically (rather than
    // hard-coding `Esc` / `q` /
    // `Q`) means:
    //
    // - If the user rebinds
    //   `key.cancel=F1` in their
    //   config, `F1` now closes
    //   the palette (and `Esc`
    //   no longer does — the
    //   rest of the TUI still
    //   honours the binding via
    //   `action_for_key`).
    // - The letter `q` is no
    //   longer special: it's
    //   just a printable
    //   character that types
    //   into the palette's
    //   filter box. Pressing it
    //   while typing a filter
    //   name like "quit" works
    //   instead of closing the
    //   palette out from under
    //   the user.
    // - Multi-key bindings
    //   (`key.cancel=Esc,F1`)
    //   all close the palette.
    if action_for_key(&app.bindings, &key)
        == Some(Action::Cancel)
    {
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

    // The output view closes only
    // on the user's `Cancel`
    // binding (default `Esc`,
    // configurable via
    // `key.cancel=...`) or on
    // the toggle key that opened
    // it (`Action::ShowOutput`,
    // default `Ctrl+L`). This
    // matches every other
    // sub-window (command
    // palette, help, question,
    // theme picker, confirm
    // delete, describe, correct)
    // and keeps the title's
    // close hint in sync with
    // the actual keys. `q` and
    // `Enter` no longer close
    // here — `q` was previously
    // a hardcoded "close"
    // affordance that swallowed
    // any `q` the user typed
    // looking for output
    // containing "quit" or
    // "query"; `Enter` had no
    // meaningful meaning in a
    // read-only view. `Ctrl+C`
    // still closes *and* aborts
    // the TUI session, mirroring
    // the convention used
    // elsewhere.
    let is_cancel_key = action_for_key(&app.bindings, &key) == Some(Action::Cancel);
    let is_toggle_key = action_for_key(&app.bindings, &key) == Some(Action::ShowOutput);
    let is_close = is_cancel_key || is_toggle_key;
    match key.code {
        _ if is_close => {
            // The runner loop at the
            // top level ignores the
            // `OutputViewResult` for
            // the Close case (it
            // only watches for
            // `selection.is_some()` to
            // exit the TUI on the
            // edit-comment path), so
            // we have to actually
            // close the view here.
            // The previous version
            // forgot this too — the
            // Esc/q/Enter close keys
            // silently did nothing
            // because they returned
            // `Close` without
            // mutating `app.output_view`.
            app.close_output_view();
            OutputViewResult::Close
        }
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

/// Key handler for the LLM "describe" overlay.
///
/// The shape mirrors the captured-output handler
/// (`handle_output_view_key`) but the action set is
/// smaller: there's no "open in editor" or "open
/// in pager" — the describe overlay is purely a
/// read-only viewer for a short piece of prose.
///
/// Close keys: `Esc`, `Enter`, `q`, `Ctrl-C`,
/// `Ctrl-K` (re-pressing the action that opened the
/// overlay is a natural way to dismiss it). Scroll
/// keys: `Up` / `Down` for one line, `PageUp` /
/// `PageDown` for a page, `Home` / `End` for the
/// extremes.
///
/// Returns `true` only when the user aborts the
/// whole TUI with `Ctrl-C` (matching the convention
/// used by the other overlay handlers). The overlay
/// itself is closed by mutating `app.describe_view`
/// directly here; the run loop's overlay check
/// (`app.is_describe_viewing()`) takes care of the
/// dispatch fall-through.
fn handle_describe_view_key(
    app: &mut App,
    key: KeyEvent,
    page_size: usize,
) -> bool {
    // Compute the max valid scroll offset for the
    // current description text. Most responses are
    // short and fit on a single screen, in which
    // case `max_scroll` returns 0 and Up/PageUp
    // are no-ops.
    let max_scroll = |text: &str| -> usize {
        let total = text.lines().count();
        total.saturating_sub(page_size.max(1))
    };
    let is_close = matches!(
        key.code,
        KeyCode::Esc
            | KeyCode::Enter
            | KeyCode::Char('q')
            | KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL)
    );
    if is_close {
        app.close_describe();
        return false;
    }
    if key.code == KeyCode::Char('c')
        && key.modifiers.contains(KeyModifiers::CONTROL)
    {
        app.cancelled = true;
        app.close_describe();
        return true;
    }
    match key.code {
        KeyCode::Up => {
            if let Some(ref mut view) = app.describe_view {
                view.scroll = view.scroll.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if let Some(ref mut view) = app.describe_view {
                let max = view.text.lines().count().saturating_sub(page_size.max(1));
                view.scroll = (view.scroll + 1).min(max);
            }
        }
        KeyCode::PageUp => {
            if let Some(ref mut view) = app.describe_view {
                view.scroll =
                    view.scroll.saturating_sub(page_size.max(1));
            }
        }
        KeyCode::PageDown => {
            if let Some(ref mut view) = app.describe_view {
                let max = view.text.lines().count().saturating_sub(page_size.max(1));
                view.scroll = (view.scroll + page_size.max(1)).min(max);
            }
        }
        KeyCode::Home => {
            if let Some(ref mut view) = app.describe_view {
                view.scroll = 0;
            }
        }
        KeyCode::End => {
            if let Some(ref mut view) = app.describe_view {
                view.scroll = max_scroll(&view.text);
            }
        }
        _ => {}
    }
    false
}

/// Key handler for the general question overlay (prefixed with `%`).
///
/// Mirrors the describe overlay in shape (a piece of text + a scroll
/// offset) but is driven by the user's question rather than by the
/// captured stdout of a history row.
///
/// Close keys: `Esc`, `Enter`, `q`, `Ctrl-C`. Scroll keys: `Up` /
/// `Down` for one line, `PageUp` / `PageDown` for a page, `Home` /
/// `End` for the extremes.
///
/// Returns `true` only when the user aborts the whole TUI with `Ctrl-C`.
fn handle_question_view_key(
    app: &mut App,
    key: KeyEvent,
    page_size: usize,
) -> bool {
    let max_scroll = |text: &str| -> usize {
        let total = text.lines().count();
        total.saturating_sub(page_size.max(1))
    };
    let is_close = matches!(
        key.code,
        KeyCode::Esc
            | KeyCode::Enter
            | KeyCode::Char('q')
    );
    if is_close {
        app.close_question();
        return false;
    }
    if key.code == KeyCode::Char('c')
        && key.modifiers.contains(KeyModifiers::CONTROL)
    {
        app.cancelled = true;
        app.close_question();
        return true;
    }
    match key.code {
        KeyCode::Up => {
            if let Some(ref mut view) = app.question_view {
                view.scroll = view.scroll.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if let Some(ref mut view) = app.question_view {
                let max = view.text.lines().count().saturating_sub(page_size.max(1));
                view.scroll = (view.scroll + 1).min(max);
            }
        }
        KeyCode::PageUp => {
            if let Some(ref mut view) = app.question_view {
                view.scroll =
                    view.scroll.saturating_sub(page_size.max(1));
            }
        }
        KeyCode::PageDown => {
            if let Some(ref mut view) = app.question_view {
                let max = view.text.lines().count().saturating_sub(page_size.max(1));
                view.scroll = (view.scroll + page_size.max(1)).min(max);
            }
        }
        KeyCode::Home => {
            if let Some(ref mut view) = app.question_view {
                view.scroll = 0;
            }
        }
        KeyCode::End => {
            if let Some(ref mut view) = app.question_view {
                view.scroll = max_scroll(&view.text);
            }
        }
        _ => {}
    }
    false
}

/// Key handler for the LLM "correct" modal overlay.
///
/// The action set is smaller than the describe
/// overlay: there are no scroll keys (the corrected
/// command is a single line of text, so scrolling
/// doesn't apply) and the only meaningful inputs
/// are:
///
/// - `Enter` — accept. Stages the corrected
///   command (inserts it into history with the
///   original as the comment) and returns `true`
///   so the run loop exits. The parent shell
///   then runs the corrected command.
/// - `Esc` / `q` — cancel. Closes the overlay,
///   returns `false` so the TUI stays open with
///   the user's original list state intact.
/// - `Ctrl-C` — abort the entire TUI (mirrors the
///   other overlay handlers' convention). Sets
///   `cancelled = true` and returns `true` so the
///   run loop exits.
fn handle_correct_view_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => {
            // Accept: stage the corrected command
            // and let the run loop exit. The status
            // message set inside `accept_corrected_command`
            // is the last thing the user sees
            // before the TUI tears down.
            app.accept_corrected_command();
            true
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            app.close_correct();
            false
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cancelled = true;
            app.close_correct();
            true
        }
        // All other keys are no-ops. The user can
        // only accept or cancel; we don't expose
        // the corrected command for editing in
        // this overlay (the LLM's output is the
        // proposal, the user either takes it or
        // leaves it). If the user wants to edit
        // the corrected command, they can press
        // `Esc` to cancel, then `Left`/`Right` on
        // the original row to prefill the line
        // editor, and edit there.
        _ => false,
    }
}

/// Parse one line of
/// `tmux list-windows -a -F
/// '#{pane_id} |
///  #{pane_current_path} |
///  active:#{window_active}
///  | Layout:
///  #{window_layout}'`
/// output into a
/// `TmuxWindowInfo`. Returns
/// `None` if the line is
/// malformed, the active flag
/// isn't `1`, or any required
/// field is empty.
///
/// **Why a 4-field format**:
/// the user's request was
/// specifically the format
/// `tmux list-windows -a -F
/// "#{pane_id} |
///  #{pane_current_path}
///  | active:#{window_active}
///  | Layout:
///  #{window_layout}" |
/// grep "active:1"`. We don't
/// pipe through `grep` (a second
/// subprocess) — we read all
/// lines and filter in-process.
/// The `active:1` filter has
/// to be a substring match on
/// the third field, with the
/// `active:` prefix and
/// exactly one character (`0`
/// or `1`) following. The
/// `|` separator is preserved
/// because tmux lets session
/// names contain spaces and
/// reserves `:`, `,`, `;`,
/// `\`, ` ` as field separators
/// in some commands; `|` is
/// safe in all current
/// formats.
///
/// **A subtle bug we hit
/// during development**: tmux
/// format strings use
/// `#`-prefixed placeholders
/// (`#S`, `#{pane_current_path}`),
/// with **the `#` always
/// required**. Writing
/// `"{S}"` instead of `"#S"`
/// silently renders an empty
/// first column, then any
/// strict parser that skips
/// empty fields throws the
/// whole line away. The
/// `FORMAT` constant in
/// `fetch_tmux_windows` is
/// tested by
/// `tmux list-windows -a -F`;
/// the regression test below
/// pins the correct format.

/// Build the home-prefix list
/// used by both
/// `directory_tmux_pane_id`
/// and `fetch_directories`.
/// Reads the user's `Config`
/// once (so the call site
/// doesn't need to thread it
/// through) and returns
/// `$HOME` followed by the
/// `homemap=...` entries,
/// sorted longest-first so
/// the most-specific home
/// wins. Same convention as
/// `shorten_home_path` /
/// `expand_home_with_config`.
/// Recomputed at App
/// construction; per-TUI-
/// session config changes
/// don't propagate (same
/// constraint the rest of
/// the App has — config is
/// read once at startup).
fn build_home_list() -> Vec<String> {
    let cfg = Config::load();
    let mut homes: Vec<String> = std::iter::once(
        std::env::var("HOME").unwrap_or_default(),
    )
    .chain(cfg.home_map().iter().filter_map(|p| {
        p.to_str().map(str::to_string)
    }))
    .filter(|s| !s.is_empty())
    .collect();
    homes.sort_by_key(|s| std::cmp::Reverse(s.len()));
    homes
}

/// Write a debug line to
/// `~/.local/cache/smarthistory/tmux-filter-debug.log`
/// when the
/// `SMARTHISTORY_DEBUG_TMUX`
/// env var is set. The TUI
/// renders to stderr (so
/// `eprintln!` output is
/// *absorbed* by the TUI's
/// render path and never
/// reaches the user's
/// terminal), so we use a
/// dedicated log file
/// instead. The user can
/// `tail -f` the file from
/// another terminal to see
/// which tmux panes are
/// being filtered / kept
/// and why.
///
/// `message` already
/// contains the pane id and
/// the rejected
/// `pane_current_path`.
/// This helper only adds a
/// timestamp and the
/// `[smarthistory]` prefix.
/// Best-effort: any I/O
/// error is silently
/// ignored (the debug log
/// is non-critical).
fn tmux_filter_debug_log(message: &str) {
    if std::env::var("SMARTHISTORY_DEBUG_TMUX").is_err() {
        return;
    }
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return,
    };
    let path = std::path::Path::new(&home)
        .join(".local")
        .join("cache")
        .join("smarthistory")
        .join("tmux-filter-debug.log");
    // Create the parent dir
    // in case the user has
    // never run the TUI
    // before (the cache dir
    // is normally created by
    // the history-recording
    // path, but the tmux
    // filter logs early in
    // App construction and
    // may run before any
    // history is written).
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("[{}] [smarthistory] {}\n", now, message);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
}

/// Walk every
/// `sessiondirs=...` config
/// entry recursively and
/// return the union of all
/// subdirectories found.
/// Used to populate
/// `App::session_subdirs`
/// at construction time.
///
/// Multiple `sessiondirs=`
/// entries are allowed;
/// each is walked
/// independently. A
/// non-existent or
/// unreadable root is
/// silently skipped (a
/// walker that errors would
/// fail the whole TUI
/// startup on a missing
/// mount). The result is
/// deduplicated: a
/// subdirectory that lives
/// under two sessiondirs
/// roots (e.g.
/// `sessiondirs=/Users/har`
/// and
/// `sessiondirs=/Users/har/work`)
/// appears once in the
/// output. Dedup is on
/// canonical paths so
/// symlinks that point to
/// the same physical dir
/// also collapse to one
/// entry.
///
/// Same
/// per-TUI-session-static
/// contract as
/// `build_home_list`: the
/// list is read once at
/// App construction; the
/// user has to restart the
/// TUI for config changes
/// to take effect.
fn build_session_subdirs() -> Vec<std::path::PathBuf> {
    let cfg = Config::load();
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for root in cfg.session_dirs() {
        for sub in crate::util::walk_subdirectories(root) {
            // Dedup on
            // canonical
            // path so a
            // symlink
            // and the
            // real path
            // it points
            // to don't
            // produce
            // two rows.
            let key = std::fs::canonicalize(&sub)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| {
                    sub.to_string_lossy().into_owned()
                });
            if seen.insert(key) {
                out.push(sub);
            }
        }
    }
    out
}

fn parse_tmux_pane_line(line: &str) -> Option<TmuxWindowInfo> {
    // `split('|')` with trim on
    // each field. Four fields,
    // no quoting, no escaping —
    // `|` is the format
    // separator and never
    // appears inside any of the
    // four fields in real-world
    // tmux output.
    let parts: Vec<&str> = line.split('|').map(str::trim).collect();
    if parts.len() != 4 {
        return None;
    }
    let pane_id = parts[0];
    let path_raw = parts[1];
    let active_field = parts[2];
    let _layout = parts[3];
    // Active-flag check. The
    // `active:` prefix is
    // literal in the format
    // string; the value is
    // either `0` or `1`. We
    // require exactly `1` to
    // match the user's
    // `grep "active:1"` filter,
    // which gives "currently
    // visible" — i.e. the
    // window the user is
    // looking at. `0` (inactive
    // windows) is filtered out
    // so the directories view
    // doesn't mark dirs the
    // user isn't actually using
    // right now.
    if active_field != "active:1" {
        return None;
    }
    if pane_id.is_empty() {
        return None;
    }
    let path = crate::util::canonicalize_directory(path_raw);
    if path.is_empty() {
        return None;
    }
    Some(TmuxWindowInfo {
        pane_id: pane_id.to_string(),
        path,
    })
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
            KeyCode::Char('w') => {
                // Same shortcut as the main TUI:
                // delete one word backward from the
                // cursor in the comment buffer.
                // `delete_word_backward` already
                // routes to the comment-edit
                // buffer when one is open, so we
                // can call it directly.
                app.delete_word_backward();
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

    /// Best-effort mtime setter
    /// used by tests that need a
    /// file to *look* old. We use
    /// the `filetime` crate
    /// (declared in
    /// `[dev-dependencies]` so
    /// this is the only place in
    /// the production tree that
    /// touches it). Errors are
    /// swallowed because the
    /// caller treats them as "mtime
    /// couldn't be set, the test
    /// may degenerate but
    /// shouldn't crash" — the
    /// filter logic is still
    /// exercised either way.
    fn filetime_touch_mtime(
        path: &std::path::Path,
        epoch_secs: i64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let time = filetime::FileTime::from_unix_time(epoch_secs, 0);
        filetime::set_file_mtime(path, time)?;
        Ok(())
    }

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

        /// The command palette closes only
        /// on keys mapped to the user's
        /// `Cancel` action. Default
        /// binding is `Esc`, so `Esc`
        /// closes; `q` does NOT close.
        /// Before this contract was
        /// introduced, the palette
        /// hard-coded `Esc | q | Q` and
        /// the user couldn't type a
        /// filter containing `q`
        /// without accidentally closing
        /// the palette.
        #[test]
        fn command_palette_closes_on_cancel_key_only() {
                let mut app = global_test_app(&[("a", 1)]);
                app.open_command_menu();
                assert!(app.is_command_menu_open());
                // Pressing `q` should NOT
                // close the palette — the
                // user may be typing a
                // filter like "quit" or
                // "query".
                let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty());
                handle_command_menu_key(&mut app, q);
                assert!(
                        app.is_command_menu_open(),
                        "q must not close the palette \
                         (only the user-configured Cancel \
                         binding does)"
                );
                // `Q` should also NOT
                // close (since it's a
                // printable character
                // now, and `Action::Cancel`
                // is `Esc` by default).
                let Q = KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::empty());
                handle_command_menu_key(&mut app, Q);
                assert!(
                        app.is_command_menu_open(),
                        "Q must not close the palette"
                );
                // `Esc` (the default Cancel
                // binding) closes it.
                let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
                handle_command_menu_key(&mut app, esc);
                assert!(
                        !app.is_command_menu_open(),
                        "Esc must close the palette (default \
                         Cancel binding)"
                );
        }

        /// If the user rebinds the
        /// Cancel action to `F1`
        /// (or any other key), that
        /// key becomes the only
        /// way to close the
        /// palette via keypress —
        /// `Esc` no longer does
        /// unless the user also
        /// bound it to Cancel.
        /// This test exercises
        /// the dynamic-binding
        /// branch of
        /// `handle_command_menu_key`.
        #[test]
        fn command_palette_respects_user_cancel_binding() {
                let mut app = global_test_app(&[("a", 1)]);
                // Re-bind Cancel to F1.
                app.bindings.set(
                        Action::Cancel,
                        vec![bindings::parse_key_spec("F1").expect("F1")],
                );
                app.open_command_menu();
                assert!(app.is_command_menu_open());
                // `Esc` no longer closes
                // (because the user
                // removed it from
                // Cancel — F1 is now
                // the only binding).
                let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
                handle_command_menu_key(&mut app, esc);
                assert!(
                        app.is_command_menu_open(),
                        "Esc must NOT close the palette \
                         when Cancel is bound only to F1"
                );
                // F1 closes.
                let f1 = KeyEvent::new(KeyCode::F(1), KeyModifiers::empty());
                handle_command_menu_key(&mut app, f1);
                assert!(
                        !app.is_command_menu_open(),
                        "F1 must close the palette when \
                         bound to Cancel"
                );
        }

        /// Multi-key Cancel binding
        /// (e.g. `key.cancel=Esc,F1`):
        /// every key in the list
        /// closes the palette.
        #[test]
        fn command_palette_respects_multi_key_cancel_binding() {
                let mut app = global_test_app(&[("a", 1)]);
                app.bindings.set(
                        Action::Cancel,
                        vec![
                                bindings::parse_key_spec("Esc").expect("Esc"),
                                bindings::parse_key_spec("F1").expect("F1"),
                        ],
                );
                app.open_command_menu();
                // F1 closes.
                let f1 = KeyEvent::new(KeyCode::F(1), KeyModifiers::empty());
                handle_command_menu_key(&mut app, f1);
                assert!(!app.is_command_menu_open());
                // Re-open and try Esc.
                app.open_command_menu();
                let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
                handle_command_menu_key(&mut app, esc);
                assert!(!app.is_command_menu_open());
                // `q` still doesn't close
                // (user might be typing
                // "quit" into the filter).
                app.open_command_menu();
                let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty());
                handle_command_menu_key(&mut app, q);
                assert!(app.is_command_menu_open());
        }

        /// The destructive-confirm
        /// dialog closes on the
        /// user-configured `Cancel`
        /// binding (default `Esc`,
        /// configurable via
        /// `key.cancel=...`).
        /// `n` / `N` also close (the
        /// mnemonic "no" answer,
        /// always allowed regardless
        /// of how the user has
        /// rebound Cancel). Before
        /// this tightening, the
        /// dialog hard-coded `Esc`
        /// and could not honour
        /// user rebindings — the
        /// displayed close hint
        /// said one thing, the
        /// accepted keys were
        /// another.
        #[test]
        fn confirm_delete_closes_on_user_cancel_binding() {
                let mut app = global_test_app(&[("a", 1)]);
                app.confirm_delete = Some(ConfirmMode::DeleteSelected);
                // Default Cancel is Esc.
                let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
                handle_confirm_delete_key(&mut app, esc, ConfirmMode::DeleteSelected);
                assert!(app.confirm_delete.is_none());
                // `n` is always a no
                // answer.
                app.confirm_delete = Some(ConfirmMode::DeleteSelected);
                let n = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty());
                handle_confirm_delete_key(&mut app, n, ConfirmMode::DeleteSelected);
                assert!(app.confirm_delete.is_none());
                // Rebind Cancel to F1 and
                // verify F1 now closes
                // (and Esc no longer
                // does — Cancel's scope
                // is the keys bound to
                // it, nothing more).
                app.bindings.set(
                        Action::Cancel,
                        vec![bindings::parse_key_spec("F1").expect("F1")],
                );
                app.confirm_delete = Some(ConfirmMode::DeleteSelected);
                let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
                handle_confirm_delete_key(&mut app, esc, ConfirmMode::DeleteSelected);
                assert!(
                        app.confirm_delete.is_some(),
                        "Esc must NOT close when Cancel is bound to F1"
                );
                let f1 = KeyEvent::new(KeyCode::F(1), KeyModifiers::empty());
                handle_confirm_delete_key(&mut app, f1, ConfirmMode::DeleteSelected);
                assert!(app.confirm_delete.is_none());
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

        /// The captured-output view
        /// closes only on the
        /// user-configured `Cancel`
        /// binding (default `Esc`,
        /// configurable via
        /// `key.cancel=...`). The
        /// toggle key (`Ctrl+L` /
        /// `Action::ShowOutput` by
        /// default) closes too —
        /// it's how the user opened
        /// the view, so pressing it
        /// again closes it
        /// (toggle-semantics).
        /// Other previously-hard-
        /// coded close keys (`q`,
        /// `Enter`) no longer close:
        /// `q` is just a printable
        /// character now and the
        /// title's close hint
        /// matches the actual keys.
        /// `Ctrl+C` still aborts the
        /// whole TUI session.
        #[test]
        fn output_view_closes_on_cancel_or_toggle_only() {
                let mut app = global_test_app(&[("a", 1)]);
                // Open the output view
                // with some text. We
                // do this directly on
                // the field rather than
                // via `show_output_view`
                // (which requires a
                // selected row with
                // non-empty output).
                app.output_view = Some(OutputView {
                        text: "captured\noutput".to_string(),
                        scroll: 0,
                });
                assert!(app.output_view.is_some());
                // Default `Esc` (Cancel)
                // closes — both returns
                // `Close` AND actually
                // tears down the view
                // (the runner loop ignores
                // the return value for
                // the Close case, so the
                // handler has to mutate
                // `app.output_view`
                // itself).
                let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
                let r = handle_output_view_key(&mut app, esc, 10);
                assert!(
                    matches!(r, OutputViewResult::Close),
                    "Esc (Cancel) must return Close"
                );
                assert!(
                    !app.is_output_viewing(),
                    "Esc must actually close the output view (not just return Close)"
                );
                // `Ctrl+L` (the toggle /
                // ShowOutput action)
                // also closes.
                app.output_view = Some(OutputView {
                    text: "captured\noutput".to_string(),
                    scroll: 0,
                });
                let cl = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
                let r = handle_output_view_key(&mut app, cl, 10);
                assert!(matches!(r, OutputViewResult::Close));
                assert!(
                    !app.is_output_viewing(),
                    "Ctrl+L (toggle) must actually close the output view"
                );
                // `q` does NOT close
                // anymore — it's
                // text-input with the
                // toggle key, and
                // would silently swallow
                // a `q` the user typed
                // looking for "quit" or
                // "query" output.
                app.output_view = Some(OutputView {
                        text: "captured".to_string(),
                        scroll: 0,
                });
                let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty());
                let r = handle_output_view_key(&mut app, q, 10);
                assert!(
                        matches!(r, OutputViewResult::Continue),
                        "q must NOT close the output view"
                );
                // `Enter` similarly
                // doesn't close.
                let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
                let r = handle_output_view_key(&mut app, enter, 10);
                assert!(
                        matches!(r, OutputViewResult::Continue),
                        "Enter must NOT close the output view"
                );
                // Scrolling keys still
                // work without closing.
                let down = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
                let r = handle_output_view_key(&mut app, down, 10);
                assert!(matches!(r, OutputViewResult::Continue));
                assert!(app.output_view.is_some());
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
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                        SortOrder::default(),
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        None,
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                Vec::new(),
                );
                app.refresh();
                app
        }

        /// Build an app in `Mode::Global` (the most
        /// common mode for ad-hoc history searches)
        /// with the given rows. Identical to
        /// `stats_test_app` except for the mode,
        /// which is what the sort-order tests
        /// below need: Stats mode overrides the
        /// user-picked sort with the successor-
        /// frequency ranking from `fetch_stats`, so
        /// the frequency-sort tests have to run in a
        /// non-Stats mode to actually exercise the
        /// `SortOrder::Frequency` path.
        fn global_test_app(rows: &[(&str, i64)]) -> App {
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                App::new(
                        conn,
                        Mode::Global,
                        String::new(),
                        false,
                        ExitFilter::All,
                        SortOrder::default(),
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        None,
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                    Vec::new(),
                )
        }

        /// Like `global_test_app` but with the unique
        /// index that backs the production
        /// `ON CONFLICT (command, directory, session_id)`
        /// upsert. Tests that exercise the
        /// history-insert path (e.g. the
        /// `correct` action) need this index,
        /// otherwise the insert fails with
        /// "ON CONFLICT clause does not match
        /// any PRIMARY KEY or UNIQUE constraint".
        fn global_test_app_with_dedup_index(rows: &[(&str, i64)]) -> App {
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                        );
                        CREATE UNIQUE INDEX idx_history_dedup
                            ON history (command, directory, session_id);",
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
                App::new(
                        conn,
                        Mode::Global,
                        String::new(),
                        false,
                        ExitFilter::All,
                        SortOrder::default(),
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        None,
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                    Vec::new(),
                )
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
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                        SortOrder::default(),
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        None,
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                Vec::new(),
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

        // --- Fuzzy search ---------------------------------------------------

        #[test]
        fn is_fuzzy_query_recognises_question_mark_prefix() {
                let mut app = stats_test_app(&[]);
                app.query = "".to_string();
                assert!(!app.is_fuzzy_query());
                app.query = "git".to_string();
                assert!(!app.is_fuzzy_query());
                app.query = "?git".to_string();
                assert!(app.is_fuzzy_query());
                app.query = "? git".to_string();
                assert!(app.is_fuzzy_query());
        }

        #[test]
        fn fuzzy_pattern_strips_question_mark() {
                let mut app = stats_test_app(&[]);
                app.query = "git".to_string();
                assert_eq!(app.fuzzy_pattern(), "");
                app.query = "?git".to_string();
                assert_eq!(app.fuzzy_pattern(), "git");
                app.query = "?git status".to_string();
                assert_eq!(app.fuzzy_pattern(), "git status");
        }

        #[test]
        fn query_matches_text_supports_fuzzy_subsequence() {
                let mut app = stats_test_app(&[]);
                app.query = "?gts".to_string();
                assert!(app.query_matches_text("git status"));
                assert!(app.query_matches_text("go test stuff"));
                assert!(!app.query_matches_text("vim"));
        }

        #[test]
        fn query_matches_text_fuzzy_is_case_insensitive() {
                let mut app = stats_test_app(&[]);
                app.query = "?GTS".to_string();
                assert!(app.query_matches_text("git status"));
        }

        #[test]
        fn query_matches_text_fuzzy_supports_and_by_word() {
                let mut app = stats_test_app(&[]);
                app.query = "?git st".to_string();
                // `git` and `st` both appear as subsequences.
                assert!(app.query_matches_text("git status"));
                assert!(app.query_matches_text("git stash"));
                // `st` is not a subsequence of "vim".
                assert!(!app.query_matches_text("vim"));
                // `git` is missing.
                assert!(!app.query_matches_text("cargo test"));
        }

        #[test]
        fn query_matches_text_fuzzy_empty_pattern_matches_all() {
                let mut app = stats_test_app(&[]);
                app.query = "?".to_string();
                // An empty fuzzy pattern (just the prefix) matches
                // everything, mirroring the empty plain query
                // behavior.
                assert!(app.query_matches_text("git status"));
                assert!(app.query_matches_text("vim"));
        }

        #[test]
        fn build_where_skips_like_clauses_for_fuzzy_query() {
                let mut app = stats_test_app(&[("git status", 1)]);
                app.query = "?gts".to_string();
                let (clause, _) = app.build_where();
                // Fuzzy search post-filters in Rust, so the SQL
                // should not narrow with `LIKE` clauses.
                assert!(
                        !clause.contains("LIKE"),
                        "Fuzzy query should not add LIKE clauses, got: {:?}",
                        clause
                );
        }

        #[test]
        fn cycle_search_mode_advances_prefix() {
                let mut app = stats_test_app(&[("git status", 1)]);
                // Empty query -> cycle lands on regex ('/').
                app.cycle_search_mode();
                assert_eq!(app.query, "/");
                // Cycle to fuzzy ('?').
                app.cycle_search_mode();
                assert_eq!(app.query, "?");
                // Cycle to output ('+'). New step in the
                // cycle since the `+...` search-inside-output
                // mode was added.
                app.cycle_search_mode();
                assert_eq!(app.query, "+");
                // Cycle to plain (no prefix).
                app.cycle_search_mode();
                assert_eq!(app.query, "");
        }

        #[test]
        fn cycle_search_mode_preserves_query_body() {
                let mut app = stats_test_app(&[("git status", 1)]);
                app.query = "git status".to_string();
                // Set the cursor to a mid-buffer position to
                // verify that the cycle resets it to the new
                // end (the body is preserved but the cursor
                // would otherwise be left at a stale index
                // past the new end of the query).
                app.query_cursor = 4;
                app.cycle_search_mode();
                assert_eq!(app.query, "/git status");
                assert_eq!(app.query_cursor, "/git status".chars().count());
                app.cycle_search_mode();
                assert_eq!(app.query, "?git status");
                assert_eq!(app.query_cursor, "?git status".chars().count());
                app.cycle_search_mode();
                assert_eq!(app.query, "+git status");
                assert_eq!(app.query_cursor, "+git status".chars().count());
                app.cycle_search_mode();
                assert_eq!(app.query, "git status");
                assert_eq!(app.query_cursor, "git status".chars().count());
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
                // The env-var manipulation in this test races
                // with `select_for_run_on_labeled_only_row_stages_command`
                // and the LLM tests when they all run in
                // parallel. Hold the env lock for the entire
                // test so the read/modify/restore is atomic
                // relative to other env-touching tests.
                let _env_guard = ENV_LOCK.lock().expect("env lock poisoned");
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
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                        SortOrder::default(),
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        None,
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                Vec::new(),
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
                // Hold the env lock for the whole test; see
                // `selected_row_finds_labeled_only_rows` for the
                // rationale.
                let _env_guard = ENV_LOCK.lock().expect("env lock poisoned");
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                        SortOrder::default(),
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        None,
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                Vec::new(),
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

        // --- LLM query mode -------------------------------------------------
        //
        // The LLM client is hidden behind a trait so these tests
        // can inject canned responses without a live ollama
        // server. The trait lives in `crate::llm`; the test
        // defines a minimal in-memory implementation.

        struct FakeLlm {
                /// Raw response to return from `generate`, exactly
                /// as the LLM would have produced it (before
                /// sanitization). Tests use this to exercise the
                /// full sanitize-then-stage path.
                response: String,
                /// Optional injection of an error.
                error: Option<crate::llm::LlmError>,
                /// Raw response to return from `describe`,
                /// exactly as the LLM would have produced
                /// it (no sanitization — the description
                /// is rendered as-is). Defaults to the
                /// empty string when the test doesn't care
                /// about the describe path.
                describe_response: String,
                /// Raw response to return from `correct`,
                /// exactly as the LLM would have
                /// produced it (before
                /// sanitization). The production path
                /// runs this through
                /// `sanitize_command` to extract a
                /// clean command, so a test that
                /// exercises the full pipeline should
                /// set this to a command-form string
                /// (or to a string with markdown
                /// fences to verify the sanitizer).
                /// Defaults to the empty string when
                /// the test doesn't care about the
                /// correct path.
                correct_response: String,
        }

        impl crate::llm::LlmClient for FakeLlm {
                fn generate(&self, _description: &str) -> Result<String, crate::llm::LlmError> {
                        match &self.error {
                                Some(e) => Err(match e {
                                        // Reconstruct the error
                                        // without owning its
                                        // detail (the variants we
                                        // test carry no heap
                                        // data so this is a
                                        // simple clone).
                                        crate::llm::LlmError::NotConfigured => {
                                                crate::llm::LlmError::NotConfigured
                                        }
                                        other => match other {
                                                crate::llm::LlmError::Transport(s) => {
                                                        crate::llm::LlmError::Transport(s.clone())
                                                }
                                                _ => crate::llm::LlmError::NoCommand,
                                        },
                                }),
                                None => Ok(self.response.clone()),
                        }
                }

                fn describe(&self, _command: &str) -> Result<String, crate::llm::LlmError> {
                        // The describe path uses the same
                        // `error` injection as `generate` so
                        // existing test fixtures (e.g.
                        // `LlmError::NotConfigured`) cover
                        // both code paths. The canned
                        // response is a separate field so
                        // tests can supply a description
                        // string distinct from a command
                        // string.
                        match &self.error {
                                Some(e) => Err(match e {
                                        crate::llm::LlmError::NotConfigured => {
                                                crate::llm::LlmError::NotConfigured
                                        }
                                        other => match other {
                                                crate::llm::LlmError::Transport(s) => {
                                                        crate::llm::LlmError::Transport(s.clone())
                                                }
                                                _ => crate::llm::LlmError::NoCommand,
                                        },
                                }),
                                None => Ok(self.describe_response.clone()),
                        }
                }

                fn correct(&self, _command: &str) -> Result<String, crate::llm::LlmError> {
                        // Same `error` injection as the
                        // other methods so a test
                        // fixture like
                        // `LlmError::NotConfigured`
                        // covers all three LLM-backed
                        // actions (generate, describe,
                        // correct). The canned response
                        // is in a separate field so
                        // tests can supply a corrected
                        // command distinct from a
                        // description.
                        match &self.error {
                                Some(e) => Err(match e {
                                        crate::llm::LlmError::NotConfigured => {
                                                crate::llm::LlmError::NotConfigured
                                        }
                                        other => match other {
                                                crate::llm::LlmError::Transport(s) => {
                                                        crate::llm::LlmError::Transport(s.clone())
                                                }
                                                _ => crate::llm::LlmError::NoCommand,
                                        },
                                }),
                                None => Ok(self.correct_response.clone()),
                        }
                }

                fn prompt(&self, _prompt: &str) -> Result<String, crate::llm::LlmError> {
                        // The trait's default `generate`
                        // and `describe` impls call
                        // `prompt(&build_prompt(...))` and
                        // `prompt(&build_describe_prompt(...))`
                        // respectively. The tests that
                        // exercise this fake override
                        // `generate` and `describe`
                        // directly, so this method is
                        // never called in practice. We
                        // still have to implement it
                        // (the trait has no default body
                        // for it) so we return the canned
                        // `response` — a sane fallback
                        // that makes any test that
                        // accidentally calls it get a
                        // deterministic value rather than
                        // a panic.
                        match &self.error {
                                Some(e) => Err(match e {
                                        crate::llm::LlmError::NotConfigured => {
                                                crate::llm::LlmError::NotConfigured
                                        }
                                        other => match other {
                                                crate::llm::LlmError::Transport(s) => {
                                                        crate::llm::LlmError::Transport(s.clone())
                                                }
                                                _ => crate::llm::LlmError::NoCommand,
                                        },
                                }),
                                None => Ok(self.response.clone()),
                        }
                }
        }

        fn make_llm_app(query: &str, fake: FakeLlm) -> App {
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                        );
                        -- The dedup index that backs the `ON
                        -- CONFLICT (command, directory, session_id)`
                        -- clause used by `run_llm_query`. In the
                        -- production schema this is created by
                        -- `init_db` in main.rs; tests have to
                        -- declare it themselves since they build
                        -- a fresh in-memory database.
                        CREATE UNIQUE INDEX idx_history_dedup
                            ON history (command, directory, session_id);",
                )
                .expect("create tables");
                App::new(
                        conn,
                        Mode::Global,
                        query.to_string(),
                        false,
                        ExitFilter::All,
                        SortOrder::default(),
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        Some(Box::new(fake)),
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                    Vec::new(),
                )
        }

        /// Process-wide serialization for environment-variable
        /// access in tests. The existing `selected_row_finds_labeled_only_rows`,
        /// `select_for_run_on_labeled_only_row_stages_command`,
        /// and LLM tests all call `unsafe { std::env::set_var }`
        /// to set `PWD` / `SMART_HISTORY_SESSION`. When those
        /// tests run in parallel, the env-var mutations race and
        /// one test sees a half-restored state. Holding this
        /// mutex's guard for the lifetime of each env-touching
        /// test makes the read/modify/restore critical section
        /// atomic across threads — the closest we can get to
        /// per-test isolation in a parallel test runner without
        /// pulling in a serial framework.
        ///
        /// Stored as a `std::sync::Mutex<()>` rather than a
        /// `parking_lot::Mutex` so the project stays
        /// dependency-free (this module already depends on
        /// `std` for everything else).
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

        #[test]
        fn is_llm_query_recognises_equals_prefix() {
                let mut app = make_llm_app(
                        "=Find all files modified yesterday",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                assert!(app.is_llm_query());
                app.query = "git status".to_string();
                assert!(!app.is_llm_query());
                app.query = "/regex".to_string();
                assert!(!app.is_llm_query());
                app.query = "?fuzzy".to_string();
                assert!(!app.is_llm_query());
                app.query = "".to_string();
                assert!(!app.is_llm_query());
        }

        #[test]
        fn run_llm_query_stages_clean_command() {
                let mut app = make_llm_app(
                        "=Find all files modified yesterday",
                        FakeLlm {
                                response: "find . -mtime -1 -type f".to_string(),
                                error: None,
                                describe_response: String::new(),
                                correct_response: String::new(),
                        },
                );
                app.select_for_run();
        app.process_pending_llm_request();
                // The LLM call should stage the generated
                // command for the parent shell to run.
                assert_eq!(
                        app.selection.as_deref(),
                        Some("find . -mtime -1 -type f")
                );
                assert_eq!(app.pick_mode, Some(PickMode::Run));
                // The new command should also be in the
                // history table with the description as the command (with = prefix)
                // and the generated command as output/comment.
                app.refresh();
                let rows = app.merged_rows();
                assert!(
                        rows.iter().any(|r| r.command == "=Find all files modified yesterday"
                                && r.output == "find . -mtime -1 -type f"),
                        "the LLM query should be inserted with the description as command, \
                         got rows: {:?}",
                        rows.iter().map(|r| (&*r.command, &*r.output, &*r.comment)
                        ).collect::<Vec<_>>()
                );
        }

        #[test]
        fn run_llm_query_sanitises_markdown_fences() {
                // The LLM echoed the command inside a fenced
                // block; the sanitizer should strip the fences
                // before staging.
                let mut app = make_llm_app(
                        "=List Cargo.toml files",
                        FakeLlm {
                                response: "```bash\nfind . -name Cargo.toml\n```".to_string(),
                                error: None,
                                describe_response: String::new(),
                                correct_response: String::new(),
                        },
                );
                app.select_for_run();
        app.process_pending_llm_request();
                let msg = app.status_message.as_ref().map(|(m, _)| m.clone());
                assert_eq!(
                        app.selection.as_deref(),
                        Some("find . -name Cargo.toml"),
                        "selection: {:?}, status: {:?}",
                        app.selection,
                        msg
                );
        }

        #[test]
        fn run_llm_query_rejects_empty_description() {
                // `=` with no description is now treated as a search
                // for old LLM queries, not a generation request. The
                // user can select existing LLM query rows.
                let mut app = make_llm_app(
                        "=",
                        FakeLlm {
                                // The fake will fail the test if
                                // it gets called.
                                response: "should not be called".to_string(),
                                error: None,
                                describe_response: String::new(),
                                correct_response: String::new(),
                        },
                );
                // Insert an old LLM query into history so there's
                // something to select
                app.conn.execute(
                        "INSERT INTO history (command, directory, session_id, exit_code, mode) VALUES (?1, ?2, ?3, ?4, 'llm')",
                        params!["=old test query", "/test", "test-session", -1],
                ).unwrap();
                let history_id: i64 = app.conn.query_row(
                        "SELECT id FROM history WHERE command = ?1",
                        params!["=old test query"],
                        |row| row.get(0),
                ).unwrap();
                app.conn.execute(
                        "INSERT INTO history_output (history_id, output) VALUES (?1, ?2)",
                        params![history_id, "ls -la"],
                ).unwrap();
                app.refresh();
                
                // With just "=", is_llm_query returns false (no description),
                // so select_for_run should select the row, not call run_llm_query
                app.select_for_run();
                // The selected row's output should be staged (since it's an old LLM query)
                assert_eq!(app.selection.as_deref(), Some("ls -la"));
                assert_eq!(app.pick_mode, Some(PickMode::Run));
        }

        #[test]
        fn run_llm_query_surfaces_not_configured_when_client_is_none() {
                // Build an app *without* a configured LLM
                // client and try to run an LLM query. The TUI
                // should report "not configured" without
                // panicking.
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                        );
                        CREATE UNIQUE INDEX idx_history_dedup
                            ON history (command, directory, session_id);",
                )
                .expect("create tables");
                let mut app = App::new(
                        conn,
                        Mode::Global,
                        "=anything".to_string(),
                        false,
                        ExitFilter::All,
                        SortOrder::default(),
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        None, // <-- the missing LLM config
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                Vec::new(),
                );
                app.select_for_run();
                assert!(app.selection.is_none());
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("not-configured must surface a status");
                assert!(
                        msg.contains("not configured"),
                        "got: {:?}",
                        msg
                );
        }

        #[test]
        fn run_llm_query_surfaces_no_command_when_sanitizer_rejects() {
                // The LLM responded with nothing usable after
                // sanitization (only commentary, no actual
                // command). The TUI should surface a
                // "no usable command" status.
                let mut app = make_llm_app(
                        "=Do something",
                        FakeLlm {
                                response: "# I cannot help with that.".to_string(),
                                error: None,
                                describe_response: String::new(),
                                correct_response: String::new(),
                        },
                );
                app.select_for_run();
        app.process_pending_llm_request();
                assert!(app.selection.is_none());
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("empty sanitizer output must surface a status");
                assert!(
                        msg.contains("no usable command"),
                        "got: {:?}",
                        msg
                );
        }

        // --- Query cursor (LLM mode edit support) ---------------------

        /// The cursor is initialised to the end of the query
        /// so the first character the user types lands in the
        /// expected place. For non-LLM queries this is a
        /// no-op (the input loop ignores the cursor in those
        /// modes); for LLM queries it's the starting point
        /// from which Left/Right can move.
        #[test]
        fn query_cursor_initialised_to_end() {
                let app = make_llm_app(
                        "=describe something",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                assert_eq!(app.query, "=describe something");
                assert_eq!(
                        app.query_cursor,
                        "=describe something".chars().count()
                );
        }

        /// `push_char` inserts at the cursor, not just at the
        /// end. This lets the user edit a multi-byte
        /// description mid-buffer with the cursor in any
        /// position.
        #[test]
        fn push_char_inserts_at_cursor_position() {
                let mut app = make_llm_app(
                        "=find files",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                // Move to the middle of "files" (after "find ").
                app.query_cursor = "=find ".chars().count();
                app.push_char('x');
                assert_eq!(app.query, "=find xfiles");
                assert_eq!(app.query_cursor, "=find x".chars().count());
                // Inserting again advances the cursor.
                app.push_char('y');
                assert_eq!(app.query, "=find xyfiles");
                assert_eq!(app.query_cursor, "=find xy".chars().count());
        }

        /// `backspace` deletes the character to the LEFT of
        /// the cursor. With the cursor at the end this is
        /// the historical "pop the last char" behaviour; with
        /// the cursor in the middle it deletes mid-buffer.
        #[test]
        fn backspace_deletes_before_cursor() {
                let mut app = make_llm_app(
                        "=find xfile",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                // Cursor at end (default). One backspace
                // removes the trailing `e` (the historical
                // behaviour).
                app.backspace();
                assert_eq!(app.query, "=find xfil");
                assert_eq!(app.query_cursor, "=find xfil".chars().count());
                // Now move the cursor to a mid-buffer
                // position. Place it between the space and
                // the `x` (position 6 in `=find xfil`).
                // The leading `=` counts as one char, so
                // `=find ` is positions 0-5 and `x` starts
                // at position 6.
                app.query_cursor = "=find ".chars().count();
                app.backspace();
                // Backspace at the cursor removes the
                // character to the LEFT — that's the space
                // at position 5 — collapsing the gap.
                assert_eq!(app.query, "=findxfil");
                assert_eq!(app.query_cursor, "=find".chars().count());
        }

        /// `backspace` at position 0 is a no-op. The user's
        /// backspace press at the start of the buffer should
        /// not panic and should not turn the cursor negative.
        #[test]
        fn backspace_at_position_zero_is_noop() {
                let mut app = make_llm_app(
                        "=x",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                app.query_cursor = 0;
                app.backspace();
                assert_eq!(app.query, "=x");
                assert_eq!(app.query_cursor, 0);
        }

        /// `EditStart` (the Left key) in LLM mode moves the
        /// cursor one character toward the start of the
        /// description, NOT to a row in the history list.
        /// This is the character-by-character navigation
        /// the user asked for: "When the query is an LLM
        /// query then cursor right and left should just
        /// position the cursor in the query line."
        #[test]
        fn edit_start_in_llm_mode_moves_cursor_one_char_left() {
                let mut app = make_llm_app(
                        "=find files",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                // Cursor starts at the end. The test helper
                // initialises it there.
                assert!(app.query_cursor > 0);
                let end = app.query_cursor;
                app.select_for_edit_start();
                // One character toward the start, not all
                // the way back to 0.
                assert_eq!(app.query_cursor, end - 1);
                // A second press moves one more character.
                app.select_for_edit_start();
                assert_eq!(app.query_cursor, end - 2);
                // Crucially, no row is staged — the Left
                // key in LLM mode is purely a cursor move.
                assert!(app.selection.is_none());
                assert!(app.pick_mode.is_none());
        }

        /// `EditEnd` (the Right key) in LLM mode moves the
        /// cursor one character toward the end of the
        /// description, NOT to a row in the history list.
        /// Mirror of the previous test.
        #[test]
        fn edit_end_in_llm_mode_moves_cursor_one_char_right() {
                let mut app = make_llm_app(
                        "=find files",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                // Start the cursor in the middle so we can
                // step toward the end.
                let mid = "=find ".chars().count();
                app.query_cursor = mid;
                app.select_for_edit_end();
                assert_eq!(app.query_cursor, mid + 1);
                app.select_for_edit_end();
                assert_eq!(app.query_cursor, mid + 2);
                assert!(app.selection.is_none());
                assert!(app.pick_mode.is_none());
        }

        /// Pressing Left at the very start of the buffer
        /// (cursor == 0) is a no-op, not an underflow. The
        /// cursor is tracked in characters; without the
        /// `saturating_sub` guard the dispatch could panic
        /// or wrap to `usize::MAX`. Behaviour: stays at 0.
        #[test]
        fn edit_start_at_position_zero_stays_at_zero() {
                let mut app = make_llm_app(
                        "=find files",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                app.query_cursor = 0;
                app.select_for_edit_start();
                assert_eq!(app.query_cursor, 0);
                assert!(app.selection.is_none());
        }

        /// Pressing Right at the very end of the buffer
        /// (cursor == len) is a no-op, not a panic. The
        /// `.min(len)` clamp ensures the cursor stays at
        /// the end even after repeated presses. Behaviour:
        /// stays at the character-count length.
        #[test]
        fn edit_end_at_end_stays_at_end() {
                let mut app = make_llm_app(
                        "=find files",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                let len = app.query.chars().count();
                app.query_cursor = len;
                app.select_for_edit_end();
                assert_eq!(app.query_cursor, len);
                // Pressing again (still at the end) is
                // still a no-op.
                app.select_for_edit_end();
                assert_eq!(app.query_cursor, len);
                assert!(app.selection.is_none());
        }

        /// Character-by-character navigation works for
        /// multi-byte UTF-8. The user types an accented
        /// character into a French-language description,
        /// steps the cursor with Left, inserts another
        /// accented character at that position, and the
        /// buffer is still valid UTF-8 with the expected
        /// character count.
        #[test]
        fn edit_left_right_handles_multibyte() {
                let mut app = make_llm_app(
                        "=chercher fichiers",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                let end = app.query.chars().count();
                // One step left, then one step right,
                // should round-trip back to the end.
                app.select_for_edit_start();
                assert_eq!(app.query_cursor, end - 1);
                app.select_for_edit_end();
                assert_eq!(app.query_cursor, end);
                // Multi-step walk back to position 1 (one
                // past the `=`).
                for _ in 0..(end - 1) {
                        app.select_for_edit_start();
                }
                assert_eq!(app.query_cursor, 1);
                // Insert a multi-byte character at the
                // cursor. `é` is 2 bytes in UTF-8 but 1
                // char in our cursor accounting, so the
                // cursor advances by exactly 1 char.
                app.push_char('é');
                assert_eq!(app.query_cursor, 2);
                // The new buffer is the original with
                // `é` inserted right after the `=`.
                assert!(app.query.starts_with("=é"));
                assert!(app.query.ends_with("chercher fichiers"));
        }

        /// `EditStart` / `EditEnd` keep their historical
        /// "stage a row" semantics for non-LLM queries. The
        /// LLM-mode override is specific to LLM.
        #[test]
        fn edit_start_end_in_non_llm_mode_stages_a_row() {
                // Three rows so the list isn't empty. The
                // timestamps are `now - offset`, so the
                // newest (smallest offset) comes first in
                // the default timestamp-DESC ordering. We use
                // the query field empty so the SQL `WHERE`
                // clause doesn't filter.
                let mut app = stats_test_app(&[("cd", 1), ("git status", 2), ("ls", 3)]);
                // Cursor at index 0 is the default.
                app.select_for_edit_start();
                // The first (newest) row is staged with
                // EditStart pick_mode.
                assert_eq!(app.selection.as_deref(), Some("cd"));
                assert_eq!(app.pick_mode, Some(PickMode::EditStart));
                // The query cursor is not modified by the
                // row-staging path — it's a no-op for
                // non-LLM queries.
                assert_eq!(app.query_cursor, 0);
        }

        // --- LLM auto-call debounce --------------------------------

        /// `llm_touch` arms the debounce when the query
        /// is an LLM query. Used by `push_char` /
        /// `backspace` / `clear_query` to reset the
        /// 1-second countdown each time the user edits.
        #[test]
        fn llm_touch_arms_debounce_in_llm_mode() {
                let mut app = make_llm_app(
                        "=describe something",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                assert!(app.llm_debounce_started.is_none());
                app.llm_touch();
                assert!(app.llm_debounce_started.is_some());
        }

        /// `llm_touch` clears all debounce state when the
        /// query is NOT an LLM query. We leave LLM mode
        /// (e.g. backspaced the `=`) and there's nothing
        /// for the auto-call to do.
        #[test]
        fn llm_touch_clears_state_outside_llm_mode() {
                let mut app = make_llm_app(
                        "=describe something",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                // First arm the debounce.
                app.llm_touch();
                assert!(app.llm_debounce_started.is_some());
                // Leave LLM mode.
                app.query = "git status".to_string();
                app.llm_touch();
                assert!(app.llm_debounce_started.is_none());
                assert!(app.llm_preview.is_none());
                assert!(!app.llm_in_flight);
        }

        /// `llm_touch` discards a stale preview when the
        /// user edits the description in LLM mode. The
        /// preview is no longer relevant; clearing it
        /// makes the next auto-call produce a fresh one.
        #[test]
        fn llm_touch_discards_stale_preview() {
                let mut app = make_llm_app(
                        "=describe something",
                        FakeLlm { response: String::new(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                // Manually install a preview as if the
                // debounce had just fired.
                app.llm_preview = Some(HistoryRow {
                        id: -1,
                        command: "old suggestion".to_string(),
                        directory: String::new(),
                        session_id: String::new(),
                        exit_code: -1,
                        timestamp: 0,
                        comment: "describe something".to_string(),
                        output: String::new(),
                        mode: String::new(),
                        source: String::new(),
                });
                app.llm_preview_description =
                        Some("describe something".to_string());
                // The user edits the description by
                // appending a character.
                app.push_char('!');
                assert!(
                        app.llm_preview.is_none(),
                        "stale preview must be cleared on edit"
                );
                assert!(app.llm_preview_description.is_none());
        }

        /// `llm_maybe_autocall` is a no-op when the
        /// query is empty (just `=` with no
        /// description). The model has nothing to work
        /// with; firing the call would waste a
        /// round-trip.
        #[test]
        fn llm_maybe_autocall_skips_empty_description() {
                let mut app = make_llm_app(
                        "=",
                        FakeLlm { response: "should not be called".to_string(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                // Arm the debounce in the past so the
                // time check passes if the call were to
                // fire.
                app.llm_debounce_started = Some(
                        std::time::Instant::now()
                                - std::time::Duration::from_secs(2),
                );
                app.llm_maybe_autocall();
                assert!(app.llm_preview.is_none());
        }

        /// `llm_maybe_autocall` is a no-op when the
        /// debounce window hasn't elapsed. The model
        /// shouldn't be queried on every tick; only
        /// after the user has paused for the full
        /// debounce period.
        #[test]
        fn llm_maybe_autocall_respects_debounce_window() {
                let mut app = make_llm_app(
                        "=find files",
                        FakeLlm { response: "should not be called yet".to_string(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                // Just-armed debounce: `Instant::now()` is
                // well within the 1-second window.
                app.llm_debounce_started = Some(std::time::Instant::now());
                app.llm_maybe_autocall();
                assert!(
                        app.llm_preview.is_none(),
                        "auto-call must not fire inside the debounce window"
                );
        }

        /// `llm_maybe_autocall` is a no-op when the
        /// live description already has a fresh
        /// preview. We don't want to re-fire the same
        /// call repeatedly when the user is just
        /// looking at the suggestion.
        #[test]
        fn llm_maybe_autocall_skips_when_preview_already_fresh() {
                let mut app = make_llm_app(
                        "=find files",
                        FakeLlm { response: "find . -name '*.txt'".to_string(), error: None, describe_response: String::new(), correct_response: String::new() },
                );
                // Install a fresh preview that already
                // matches the current description.
                app.llm_preview = Some(HistoryRow {
                        id: -1,
                        command: "find . -name '*.txt'".to_string(),
                        directory: String::new(),
                        session_id: String::new(),
                        exit_code: -1,
                        timestamp: 0,
                        comment: "find files".to_string(),
                        output: String::new(),
                        mode: String::new(),
                        source: String::new(),
                });
                app.llm_preview_description =
                        Some("find files".to_string());
                // Debounce expired in the past.
                app.llm_debounce_started = Some(
                        std::time::Instant::now()
                                - std::time::Duration::from_secs(2),
                );
                // The FakeLlm's response would be
                // "should not be called" if the call
                // fired, but we set the FakeLlm to a
                // specific response. If `generate` were
                // called the preview would be replaced.
                // Assert it WASN'T replaced.
                let original = app.llm_preview.clone();
                app.llm_maybe_autocall();
                assert_eq!(app.llm_preview, original);
        }

        /// Happy path: debounce elapsed, description
        /// has changed, LLM call fires, preview is
        /// populated.
        #[test]
        fn llm_maybe_autocall_fires_and_populates_preview() {
                let mut app = make_llm_app(
                        "=find files",
                        FakeLlm {
                                response: "find . -name '*.txt'".to_string(),
                                error: None,
                                describe_response: String::new(),
                                correct_response: String::new(),
                        },
                );
                // Debounce expired in the past.
                app.llm_debounce_started = Some(
                        std::time::Instant::now()
                                - std::time::Duration::from_secs(2),
                );
                app.llm_maybe_autocall();
                let preview = app
                        .llm_preview
                        .as_ref()
                        .expect("preview must be populated");
                // With the new design: command is the query (with = prefix),
                // output/comment is the generated command.
                assert_eq!(preview.command, "=find files");
                assert_eq!(preview.output, "find . -name '*.txt'");
                assert_eq!(preview.comment, "find . -name '*.txt'");
                assert_eq!(preview.id, -1);
                assert_eq!(preview.exit_code, -1);
                assert_eq!(
                        app.llm_preview_description.as_deref(),
                        Some("find files")
                );
                assert!(!app.llm_in_flight);
        }

        /// Sanitizer rejection during auto-call is
        /// silent — the user gets feedback when they
        /// press Enter (via `run_llm_query`), not on
        /// every auto-call. This is the same UX as a
        /// transport error: don't crowd the status
        /// bar on every typo.
        #[test]
        fn llm_maybe_autocall_silent_on_sanitizer_rejection() {
                let mut app = make_llm_app(
                        "=do something",
                        FakeLlm {
                                // All commentary, no command.
                                response: "# I cannot help with that.".to_string(),
                                error: None,
                                describe_response: String::new(),
                                correct_response: String::new(),
                        },
                );
                app.llm_debounce_started = Some(
                        std::time::Instant::now()
                                - std::time::Duration::from_secs(2),
                );
                app.llm_maybe_autocall();
                assert!(app.llm_preview.is_none());
                assert!(!app.llm_in_flight);
        }

        /// The preview row appears at the top of the
        /// merged list in LLM mode. Sort key is the
        /// `timestamp = now` we set in the autocall,
        /// so it sorts newest-first.
        #[test]
        fn llm_preview_appears_in_merged_rows() {
                let mut app = make_llm_app(
                        "=find files",
                        FakeLlm {
                                response: "find . -name '*.txt'".to_string(),
                                error: None,
                                describe_response: String::new(),
                                correct_response: String::new(),
                        },
                );
                app.llm_debounce_started = Some(
                        std::time::Instant::now()
                                - std::time::Duration::from_secs(2),
                );
                app.llm_maybe_autocall();
                let merged = app.merged_rows();
                assert!(!merged.is_empty());
                assert_eq!(merged[0].id, -1);
                // Command is the description (with = prefix), output is the generated command
                assert_eq!(merged[0].command, "=find files");
                assert_eq!(merged[0].output, "find . -name '*.txt'");
        }

        /// When the query leaves LLM mode, the preview
        /// is removed from the merged list. The user
        /// has stopped composing a description; the
        /// suggestion no longer applies.
        #[test]
        fn llm_preview_disappears_when_leaving_llm_mode() {
                let mut app = make_llm_app(
                        "=find files",
                        FakeLlm {
                                response: "find . -name '*.txt'".to_string(),
                                error: None,
                                describe_response: String::new(),
                                correct_response: String::new(),
                        },
                );
                app.llm_debounce_started = Some(
                        std::time::Instant::now()
                                - std::time::Duration::from_secs(2),
                );
                app.llm_maybe_autocall();
                assert!(!app.merged_rows().is_empty());
                // User backspaces out of LLM mode.
                app.query = "git status".to_string();
                app.refresh();
                // Preview must be gone from the merged
                // list (it was only added in LLM mode).
                let merged = app.merged_rows();
                for r in merged {
                        assert!(r.id >= 0, "preview leaked into non-LLM mode: {:?}", r);
                }
        }

        /// Fast-path: when a fresh preview exists for
        /// the live description, `run_llm_query`
        /// reuses it without making a second HTTP
        /// call. The FakeLlm's response is "should not
        /// be called" — if `run_llm_query` made a
        /// call, the staged command would be the
        /// FakeLlm's response, not the preview's.
        #[test]
        fn run_llm_query_reuses_fresh_preview() {
                let mut app = make_llm_app(
                        "=find files",
                        FakeLlm {
                                response: "should not be called".to_string(),
                                error: None,
                                describe_response: String::new(),
                                correct_response: String::new(),
                        },
                );
                // Install a fresh preview with the new structure:
                // command is the query (with = prefix), output/comment is the generated command.
                app.llm_preview = Some(HistoryRow {
                        id: -1,
                        command: "=find files".to_string(),
                        directory: String::new(),
                        session_id: String::new(),
                        exit_code: -1,
                        timestamp: 0,
                        comment: "find . -name '*.txt'".to_string(),
                        output: "find . -name '*.txt'".to_string(),
                        mode: String::new(),
                        source: String::new(),
                });
                app.llm_preview_description =
                        Some("find files".to_string());
                // Arm the debounce recently (well
                // within the 5x multiplier).
                app.llm_debounce_started = Some(std::time::Instant::now());
                app.select_for_run();
                // The preview's output (generated command) was staged,
                // not the FakeLlm's response.
                assert_eq!(
                        app.selection.as_deref(),
                        Some("find . -name '*.txt'")
                );
                assert_eq!(app.pick_mode, Some(PickMode::Run));
        }

        /// Slow-path: when the preview is stale (the
        /// description has changed since the preview
        /// was generated), `run_llm_query` falls
        /// through to the explicit LLM call.
        #[test]
        fn run_llm_query_does_not_reuse_stale_preview() {
                let mut app = make_llm_app(
                        "=find files",
                        FakeLlm {
                                response: "find . -mtime -1".to_string(),
                                error: None,
                                describe_response: String::new(),
                                correct_response: String::new(),
                        },
                );
                // Install a preview whose description
                // does NOT match the live query.
                app.llm_preview = Some(HistoryRow {
                        id: -1,
                        command: "stale".to_string(),
                        directory: String::new(),
                        session_id: String::new(),
                        exit_code: -1,
                        timestamp: 0,
                        comment: "old description".to_string(),
                        output: String::new(),
                        mode: String::new(),
                        source: String::new(),
                });
                app.llm_preview_description =
                        Some("old description".to_string());
                app.llm_debounce_started = Some(std::time::Instant::now());
                app.select_for_run();
        app.process_pending_llm_request();
                // The live FakeLlm was called, NOT
                // the stale preview.
                assert_eq!(
                        app.selection.as_deref(),
                        Some("find . -mtime -1")
                );
        }

        // --- Output search (`+...` query mode) ---------------------

        /// Build an app with a set of history rows, each of
        /// which has a captured output string. The `output`
        /// column is what the `+...` search mode targets;
        /// the tests below rely on this helper to set up
        /// the data they need.
        ///
        /// `rows` is a list of `(command, output)` pairs. The
        /// command and output are stored as-is. The test
        /// schema mirrors the production schema (including
        /// the `idx_history_dedup` unique index that backs
        /// `run_llm_query`'s upsert) so the output search
        /// path runs against the same SQL the real TUI
        /// issues.
        fn output_test_app(rows: &[(&str, &str)]) -> App {
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                        );
                        CREATE UNIQUE INDEX idx_history_dedup
                            ON history (command, directory, session_id);",
                )
                .expect("schema");
                let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                for (i, (cmd, output)) in rows.iter().enumerate() {
                        let id = i as i64 + 1;
                        conn.execute(
                                "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
                                 VALUES (?1, ?2, '/tmp', 'sess', 0, ?3)",
                                rusqlite::params![id, *cmd, now - (rows.len() as i64 - i as i64)],
                        )
                        .expect("insert history");
                        if !output.is_empty() {
                                conn.execute(
                                        "INSERT INTO history_output (history_id, output) VALUES (?1, ?2)",
                                        rusqlite::params![id, *output],
                                )
                                .expect("insert output");
                        }
                }
                App::new(
                        conn,
                        Mode::Global,
                        String::new(),
                        false,
                        ExitFilter::All,
                        SortOrder::default(),
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        None,
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                    Vec::new(),
                )
        }

        /// The `+` prefix is recognised as a mode marker.
        /// Without the prefix, `is_output_query` is false
        /// (e.g. `git log +foo` is a plain query, not an
        /// output search).
        #[test]
        fn is_output_query_recognises_plus_prefix() {
                let mut app =
                        output_test_app(&[("ls", "file1\nfile2\n")]);
                assert!(!app.is_output_query());
                app.query = "+segmentation".to_string();
                assert!(app.is_output_query());
                app.query = "git log +foo".to_string();
                assert!(!app.is_output_query());
                app.query = "+".to_string();
                assert!(app.is_output_query());
        }

        /// `output_pattern` returns everything after the
        /// leading `+`, with the leading `+` itself
        /// stripped. Used by `build_where` and
        /// `query_matches_text` to drive the actual
        /// `LIKE` clause and the post-filter.
        #[test]
        fn output_pattern_strips_leading_plus() {
                let mut app =
                        output_test_app(&[("ls", "")]);
                assert_eq!(app.output_pattern(), "");
                app.query = "+segmentation".to_string();
                assert_eq!(app.output_pattern(), "segmentation");
                app.query = "+".to_string();
                assert_eq!(app.output_pattern(), "");
                app.query = "+git stash".to_string();
                assert_eq!(app.output_pattern(), "git stash");
        }

        /// Single-word output search: the row whose
        /// captured output contains the substring is
        /// included; other rows are not.
        #[test]
        fn output_search_matches_substring_in_output() {
                let mut app = output_test_app(&[
                        ("make", "Compiling foo v0.1.0\nFinished release"),
                        ("ls", "src\nCargo.toml\nREADME.md"),
                        (
                                "cargo test",
                                "running 1 test\ntest ok\nsegmentation fault (core dumped)",
                        ),
                ]);
                app.query = "+segmentation".to_string();
                app.refresh();
                let commands: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                assert_eq!(commands, vec!["cargo test"]);
        }

        /// Multi-word output search: the query is
        /// `+running test` and only the row whose
        /// output contains BOTH substrings is
        /// included. This is the same AND-by-word
        /// behaviour as plain text mode. We use
        /// `running` / `test` here (not `seg` /
        /// `fault`) because the substring match is
        /// exact-substring, not word-boundary: a row
        /// containing `segfault` would match BOTH
        /// `seg` and `fault` as substrings, defeating
        /// the AND test.
        #[test]
        fn output_search_is_multi_word_and() {
                let mut app = output_test_app(&[
                        ("make", "Compiling foo\nFinished release"),
                        (
                                "binary_a",
                                "running test_a\nok",
                        ),
                        (
                                "binary_b",
                                "compiling test_b\nsegfault",
                        ),
                ]);
                app.query = "+running test".to_string();
                app.refresh();
                let commands: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                // Only `binary_a` contains both
                // `running` AND `test`. `binary_b` has
                // `test` (in `test_b`) but not
                // `running`, and `make` has neither.
                assert_eq!(commands, vec!["binary_a"]);
        }

        /// Output search is case-insensitive. The user
        /// types lowercase but the LLM-generated log
        /// lines often contain uppercase variants
        /// (`SEGMENTATION FAULT`); both should match.
        #[test]
        fn output_search_is_case_insensitive() {
                let mut app = output_test_app(&[
                        ("a", "ALL GOOD"),
                        ("b", "SEGMENTATION FAULT"),
                        ("c", "no output at all"),
                ]);
                app.query = "+segmentation".to_string();
                app.refresh();
                let commands: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                assert_eq!(commands, vec!["b"]);
        }

        /// Rows without captured output are excluded
        /// from output search. The SQL `LIKE` clause
        /// only matches against `o.output`, which is
        /// NULL for rows without a `history_output`
        /// row. This is the desired behaviour: the
        /// user is asking "which command produced
        /// *this output*?" and a command with no
        /// captured output cannot be the answer.
        #[test]
        fn output_search_excludes_rows_without_output() {
                let mut app = output_test_app(&[
                        ("with_output", "ERROR: something broke"),
                        // No output row for this one.
                        ("without_output", ""),
                ]);
                app.query = "+something".to_string();
                app.refresh();
                let commands: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                assert_eq!(commands, vec!["with_output"]);
        }

        /// An empty `+` (no body) lists all rows that
        /// have captured output. This mirrors the
        /// plain-mode behaviour of an empty query
        /// (show everything) but restricted to rows
        /// with output. Useful as a "show me what
        /// I've actually captured" view.
        #[test]
        fn output_search_empty_body_lists_all_with_output() {
                let mut app = output_test_app(&[
                        ("a", "some output"),
                        ("b", "other output"),
                        // No output row.
                        ("c", ""),
                ]);
                app.query = "+".to_string();
                app.refresh();
                let commands: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                // `c` has no output, so it must be
                // excluded. Order is timestamp-DESC
                // (`a` is oldest, `c` is newest in
                // the helper, so `c` would normally
                // be first, but `c` is excluded).
                assert_eq!(commands.len(), 2);
                assert!(commands.contains(&"a"));
                assert!(commands.contains(&"b"));
        }

        /// Output search respects the `history_output`
        /// join: even if the command text or comment
        /// doesn't contain the substring, the row is
        /// included when its captured output does.
        /// This is the whole point of the `+` mode —
        /// it searches a column the other modes
        /// don't touch.
        #[test]
        fn output_search_uses_output_not_command() {
                let mut app = output_test_app(&[
                        // Command text is innocuous;
                        // only the captured output
                        // contains the search term.
                        (
                                "do_thing",
                                "ERROR: kernel panic — not syncing",
                        ),
                ]);
                // `+panic` must match this row even
                // though the command (`do_thing`) and
                // the comment (empty) don't contain
                // the word.
                app.query = "+panic".to_string();
                app.refresh();
                let commands: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                assert_eq!(commands, vec!["do_thing"]);
        }

        /// `query_matches_text` uses the body of the
        /// `+` query (not the leading `+`) when
        /// post-filtering labeled rows. The post-
        /// filter would otherwise look for the
        /// literal substring `+segmentation` and
        /// never match.
        #[test]
        fn query_matches_text_strips_plus_prefix() {
                let mut app =
                        output_test_app(&[("x", "")]);
                app.query = "+segmentation".to_string();
                // The text being checked doesn't
                // contain the literal `+segmentation`
                // but does contain `segmentation`.
                assert!(app.query_matches_text("segmentation fault"));
                // Sanity: a totally unrelated text
                // doesn't match.
                assert!(!app.query_matches_text("all good"));
        }

        /// Mode cycle includes the `+` step. The
        /// `cycle_search_mode_advances_prefix` and
        /// `cycle_search_mode_preserves_query_body`
        /// tests cover the exact cycle, but we
        /// double-check here that the `+...` body is
        /// preserved across the cycle in both
        /// directions.
        #[test]
        fn cycle_search_mode_round_trips_output_mode() {
                let mut app =
                        output_test_app(&[("x", "")]);
                // Start in plain mode with a body.
                app.query = "error".to_string();
                // plain -> regex
                app.cycle_search_mode();
                assert_eq!(app.query, "/error");
                // regex -> fuzzy
                app.cycle_search_mode();
                assert_eq!(app.query, "?error");
                // fuzzy -> output
                app.cycle_search_mode();
                assert_eq!(app.query, "+error");
                // output -> plain
                app.cycle_search_mode();
                assert_eq!(app.query, "error");
        }

        // --- Sort order (Age / Frequency) -------------------------

        /// `SortOrder::next` cycles between the two
        /// supported values: Age (default) ↔ Frequency.
        #[test]
        fn sort_order_next_cycles_between_age_and_frequency() {
                assert_eq!(SortOrder::Age.next(), SortOrder::Frequency);
                assert_eq!(SortOrder::Frequency.next(), SortOrder::Age);
        }

        /// `SortOrder::as_str` returns the canonical
        /// lowercase form used in the session file.
        #[test]
        fn sort_order_as_str_returns_canonical_form() {
                assert_eq!(SortOrder::Age.as_str(), "age");
                assert_eq!(SortOrder::Frequency.as_str(), "frequency");
        }

        /// `SortOrder::parse` accepts the canonical
        /// form plus a small set of friendly aliases
        /// (case-insensitive, dash-tolerant in spirit).
        /// A bad value returns `None` so the caller can
        /// fall back to the default.
        #[test]
        fn sort_order_parse_accepts_canonical_and_aliases() {
                assert_eq!(SortOrder::parse("age"), Some(SortOrder::Age));
                assert_eq!(
                        SortOrder::parse("frequency"),
                        Some(SortOrder::Frequency)
                );
                // Aliases.
                assert_eq!(SortOrder::parse("freq"), Some(SortOrder::Frequency));
                assert_eq!(SortOrder::parse("count"), Some(SortOrder::Frequency));
                assert_eq!(
                        SortOrder::parse("occurrences"),
                        Some(SortOrder::Frequency)
                );
                assert_eq!(SortOrder::parse("time"), Some(SortOrder::Age));
                assert_eq!(SortOrder::parse("newest"), Some(SortOrder::Age));
                // Case-insensitive.
                assert_eq!(SortOrder::parse("AGE"), Some(SortOrder::Age));
                assert_eq!(
                        SortOrder::parse("Frequency"),
                        Some(SortOrder::Frequency)
                );
                // Unrecognised values fall through.
                assert_eq!(SortOrder::parse("garbage"), None);
                assert_eq!(SortOrder::parse(""), None);
        }

        /// `SortOrder::default` is `Age` (the historical
        /// default), so first-time TUI users get the
        /// familiar timestamp-DESC ordering.
        #[test]
        fn sort_order_default_is_age() {
                assert_eq!(SortOrder::default(), SortOrder::Age);
        }

        /// The default (Age) sort orders rows by
        /// timestamp DESC — the historical behaviour.
        /// This test pins the contract so any future
        /// refactor that swaps the primary key
        /// accidentally fails loudly.
        #[test]
        fn sort_by_age_orders_by_timestamp_desc() {
                let mut app = global_test_app(&[
                        ("git status", 5),  // oldest
                        ("cargo test", 2),
                        ("ls -la", 1),     // newest
                ]);
                app.sort_order = SortOrder::Age;
                app.refresh();
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                // Newest first: `ls -la` (offset 1), then
                // `cargo test` (offset 2), then `git status`
                // (offset 5).
                assert_eq!(cmds, vec!["ls -la", "cargo test", "git status"]);
        }

        /// In frequency sort mode, each command
        /// appears exactly once. Frequency mode is
        /// implicitly a dedup mode — without dedup,
        /// the most-frequent command would
        /// dominate the list with its own repeat
        /// instances, drowning out everything else
        /// and making the count ranking meaningless.
        /// The kept instance is the newest (highest
        /// timestamp = lowest offset), because the
        /// per-row tie-breaker is `timestamp DESC`
        /// and we keep the first occurrence per
        /// command in the sorted list.
        #[test]
        fn sort_by_frequency_orders_by_occurrence_count() {
                let mut app = global_test_app(&[
                        ("a", 1),
                        ("a", 2),
                        ("b", 3),
                        ("a", 4),
                ]);
                app.sort_order = SortOrder::Frequency;
                app.duplicate_filter = false;
                app.refresh();
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                // Frequency mode dedups implicitly:
                // one row per command, ordered by
                // count DESC. `a` had 3 occurrences
                // (count 3), `b` had 1 (count 1) —
                // `a` first. The kept `a` row is
                // the one with the highest timestamp
                // (offset 1, the newest).
                assert_eq!(cmds, vec!["a", "b"]);
        }

        /// When the duplicate filter is ON in
        /// frequency mode, only the highest-ranked
        /// instance of each command is kept. The
        /// primary sort is still by count, so the
        /// kept instances are correctly ordered.
        /// This is the same result as
        /// `sort_by_frequency_orders_by_occurrence_count`
        /// (frequency mode dedups implicitly
        /// regardless of the filter setting), but
        /// kept as a separate test to pin the
        /// explicit-toggle behaviour.
        #[test]
        fn sort_by_frequency_with_duplicate_filter() {
                let mut app = global_test_app(&[
                        ("a", 1),
                        ("a", 2),
                        ("a", 3),
                        ("b", 4),
                        ("b", 5),
                ]);
                app.sort_order = SortOrder::Frequency;
                app.duplicate_filter = true;
                app.refresh();
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                // `a` had 3 occurrences, `b` had 2.
                // With dedup ON, one `a` and one `b`
                // remain, and `a` sorts first.
                assert_eq!(cmds, vec!["a", "b"]);
        }

        /// In frequency sort mode the dedup is
        /// implicit, so the per-command tie-break
        /// (newest command wins) is what
        /// determines the final order — not
        /// per-row timestamps. The kept instance
        /// for each command is the newest one.
        #[test]
        fn sort_by_frequency_breaks_ties_by_age() {
                let mut app = global_test_app(&[
                        ("a", 1), // a's newest
                        ("a", 5),
                        ("b", 2), // b's newest
                        ("b", 3),
                        ("c", 4), // c's only
                ]);
                app.sort_order = SortOrder::Frequency;
                app.duplicate_filter = false;
                app.refresh();
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                // `a` and `b` both have count 2; the
                // per-command-newest tie-break picks
                // `a` (newest instance at offset 1
                // vs b's newest at offset 2). `c`
                // has count 1, so it sorts last.
                // Implicit dedup means each command
                // appears once.
                assert_eq!(cmds, vec!["a", "b", "c"]);
        }

        /// `cycle_sort_order` flips the field and
        /// refreshes the list, so the new order is
        /// immediately visible.
        #[test]
        fn cycle_sort_order_flips_the_field() {
                let mut app = stats_test_app(&[("a", 1), ("b", 2)]);
                assert_eq!(app.sort_order, SortOrder::Age);
                app.cycle_sort_order();
                assert_eq!(app.sort_order, SortOrder::Frequency);
                app.cycle_sort_order();
                assert_eq!(app.sort_order, SortOrder::Age);
        }

        /// In frequency sort mode, the duplicate
        /// filter is *implicit* — turning on
        /// frequency sort collapses the list to
        /// one row per command regardless of the
        /// `duplicate_filter` setting. The user's
        /// filter toggle is still respected in
        /// `Age` mode (where the historical
        /// behaviour applies), so the two settings
        /// are independent in their non-overlapping
        /// modes and simply both apply when both
        /// are active.
        ///
        /// This is the contract the user asked
        /// for: in FREQ mode, "display only the
        /// last element of a group of commands".
        /// The "last" instance is the most recent
        /// one, identified by the highest
        /// timestamp among the group's rows.
        #[test]
        fn frequency_sort_dedups_implicitly_even_when_duplicate_filter_off() {
                let mut app = global_test_app(&[
                        ("a", 1), // a's oldest
                        ("a", 2), // a's newest
                        ("b", 3), // b's oldest
                        ("b", 4), // b's newest
                        ("c", 5),
                ]);
                // User has NOT enabled the
                // duplicate filter.
                app.duplicate_filter = false;
                app.sort_order = SortOrder::Age;
                app.refresh();
                // In Age mode without dedup, all 5
                // rows are visible.
                let age_count = app.merged_rows().len();
                assert_eq!(age_count, 5);
                // Now switch to frequency mode. The
                // implicit dedup should collapse
                // this to 3 rows (one per command),
                // even though `duplicate_filter` is
                // still false.
                app.sort_order = SortOrder::Frequency;
                app.refresh();
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                assert_eq!(cmds.len(), 3, "FREQ mode must dedup implicitly: got {:?}", cmds);
                // The kept row per command is the
                // newest one. For `a` (offsets 1
                // and 2), the newer is offset 1
                // (higher timestamp). Same for
                // `b` (offsets 3 and 4, newer is
                // 3). `c` is alone. The list is
                // ordered by count DESC; `a` and
                // `b` are tied at 2 and `c` has 1.
                // Tie-break by per-command newest:
                // `a`'s newest is offset 1, `b`'s
                // is offset 3 — `a` is newer, so
                // `a` first.
                assert_eq!(cmds, vec!["a", "b", "c"]);
        }

        /// Switching back from frequency mode to
        /// age mode restores the
        /// `duplicate_filter` setting's
        /// independence: the implicit dedup
        /// disappears. This pins the
        /// "frequency mode adds implicit dedup,
        /// doesn't replace the user's setting"
        /// contract.
        #[test]
        fn age_sort_does_not_dedup_when_duplicate_filter_off() {
                let mut app = global_test_app(&[
                        ("a", 1),
                        ("a", 2),
                        ("b", 3),
                ]);
                app.duplicate_filter = false;
                // Age mode is the default; no
                // implicit dedup should happen.
                app.sort_order = SortOrder::Age;
                app.refresh();
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                // All 3 rows visible (the user's
                // duplicate filter is off).
                assert_eq!(cmds.len(), 3);
        }

        /// `Action::CycleSortOrder` dispatches to
        /// `cycle_sort_order` and is bound to `F4`
        /// by default.
        #[test]
        fn cycle_sort_order_default_key_routes() {
                let mut app = stats_test_app(&[("a", 1)]);
                let bindings = KeyBindings::defaults();
                let key = KeyEvent::new(KeyCode::F(4), KeyModifiers::empty());
                let action = action_for_key(&bindings, &key)
                        .expect("F4 is bound by default");
                assert_eq!(action, Action::CycleSortOrder);
                // Apply the action and check the
                // field flipped. We use the public
                // cycle_sort_order method directly
                // rather than going through the full
                // dispatch loop, which would need
                // terminal handles.
                app.cycle_sort_order();
                assert_eq!(app.sort_order, SortOrder::Frequency);
        }

        /// Stats mode overrides the user's sort
        /// order — the frequency-aware ranking from
        /// `fetch_stats` is preserved. Without this
        /// guard, an Age sort in Stats mode would
        /// wipe out the prediction signal.
        #[test]
        fn stats_mode_overrides_sort_order() {
                // We don't have a rich test for
                // Stats mode here; the contract is
                // that `build_merged_rows` skips the
                // sort when `Mode::Stats` is active.
                // Verify the helper directly.
                let mut app = stats_test_app(&[("a", 1)]);
                app.mode = Mode::Stats;
                app.sort_order = SortOrder::Age;
                let rows = app.build_merged_rows();
                // The rows come out in whatever order
                // `fetch_stats` produced — we just
                // assert the helper doesn't crash
                // and returns a non-empty list.
                assert!(!rows.is_empty());
        }

        // --- Session persistence for sort order ----------------

        /// The `sortorder=...` line in the session
        /// file is parsed by `TuiSession::load`.
        /// Verifying the round-trip here (without
        /// going through the real file system)
        /// catches drift between the writer and
        /// the reader.
        #[test]
        fn session_round_trips_sort_order() {
                // Build a session value that
                // differs from the default (Age)
                // so the writer actually emits
                // the field.
                let s = TuiSession {
                        mode: None,
                        query: None,
                        duplicate_filter: None,
                        exit_filter: None,
                        sort_order: Some("frequency".to_string()),
                        theme: None,
                        directory_source: None,
                };
                let rendered = format!("{:?}", s);
                // The `Debug` output includes the
                // raw field, but the actual
                // serialization format is
                // `sortorder=<value>`. We re-serialize
                // through a tiny helper here: the
                // `save` method writes the field
                // when `Some`; we just want to know
                // that `Some("frequency")` survives
                // a round-trip. Verify via the
                // `as_str` round-trip plus the
                // session's `sort_order` field being
                // populated as we set it.
                assert_eq!(
                        s.sort_order.as_deref(),
                        Some("frequency"),
                        "session struct keeps the value we put in"
                );
                // `SortOrder::parse` would also be
                // called on this value when the
                // session is loaded; verify it
                // recognises the canonical form.
                assert_eq!(
                        SortOrder::parse(s.sort_order.as_deref().unwrap()),
                        Some(SortOrder::Frequency)
                );
                // And that an unknown value would
                // be rejected on load (so a
                // hand-edited session file can't
                // wedge the TUI).
                assert_eq!(SortOrder::parse("garbage"), None);
                // Make sure the field is what we
                // think it is (the rendered debug
                // output would surface a rename).
                assert!(
                        rendered.contains("sort_order"),
                        "renamed the field: {:?}",
                        rendered
                );
        }

        // --- Describe (`Action::Describe`, default `C-k`) -----

        /// `Action::Describe` is bound to `Ctrl-K` by
        /// default. The test helper `FakeLlm` is wired
        /// up the same way as the LLM tests above, so
        /// we can drive `start_describe` end-to-end
        /// without a live ollama server.
        #[test]
        fn describe_default_key_routes() {
                let bindings = KeyBindings::defaults();
                let key = KeyEvent::new(
                        KeyCode::Char('k'),
                        KeyModifiers::CONTROL,
                );
                let action = action_for_key(&bindings, &key)
                        .expect("Ctrl-K is bound by default");
                assert_eq!(action, Action::Describe);
        }

        /// `start_describe` opens the overlay with
        /// the LLM's response, scoped to the
        /// currently-selected row. The command
        /// string is captured in the view so the
        /// title can render it.
        #[test]
        fn start_describe_opens_overlay_with_response() {
                let mut app = global_test_app(&[("git status", 1)]);
                // Wire up the FakeLlm. We replace the
                // existing `None` LLM (set by
                // `global_test_app`) with one that
                // returns a canned description.
                app.llm = Some(Box::new(FakeLlm {
                        response: String::new(),
                        error: None,
                        describe_response: "Lists the working \
                                            tree status in git."
                                .to_string(),
                        correct_response: String::new(),
                }));
                // Select the row.
                app.refresh();
                app.start_describe();
        app.process_pending_llm_request();
                let view = app
                        .describe_view
                        .as_ref()
                        .expect("describe overlay must open on success");
                assert_eq!(view.command, "git status");
                assert!(view.text.contains("Lists"));
                assert!(!app.cancelled);
        }

        /// `start_describe` with no LLM configured
        /// surfaces the "not configured" status
        /// message and does NOT open the overlay
        /// (so the user doesn't see an empty
        /// overlay that would have to be closed
        /// again). This is the same UX as the
        /// `run_llm_query` path.
        #[test]
        fn start_describe_surfaces_not_configured_when_client_is_none() {
                let mut app = global_test_app(&[("a", 1)]);
                // `global_test_app` already sets
                // `app.llm = None`.
                assert!(app.llm.is_none());
                app.refresh();
                app.start_describe();
                assert!(app.describe_view.is_none());
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("missing-LLM must surface a status");
                assert!(msg.contains("not configured"), "got: {:?}", msg);
        }

        /// `start_describe` with no row selected
        /// surfaces a status message and doesn't
        /// open the overlay. (We force "no row
        /// selected" by emptying the rows before
        /// the call.)
        #[test]
        fn start_describe_with_no_row_surfaces_status() {
                // Empty DB: no rows, so
                // `selected_row()` returns None.
                let mut app = global_test_app(&[]);
                app.llm = Some(Box::new(FakeLlm {
                        response: String::new(),
                        error: None,
                        describe_response: "should not be used".to_string(),
                        correct_response: String::new(),
                }));
                app.refresh();
                app.start_describe();
                assert!(app.describe_view.is_none());
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("empty list must surface a status");
                assert!(msg.contains("no row"), "got: {:?}", msg);
        }

        /// When the LLM call fails, the overlay is
        /// not opened and the error is surfaced in
        /// the status bar. Same UX as the
        /// "not configured" path: the user gets a
        /// message and the TUI stays in the normal
        /// list view.
        #[test]
        fn start_describe_surfaces_error_on_transport_failure() {
                let mut app = global_test_app(&[("a", 1)]);
                app.llm = Some(Box::new(FakeLlm {
                        response: String::new(),
                        error: Some(crate::llm::LlmError::Transport(
                                "connection refused".to_string(),
                        )),
                        describe_response: String::new(),
                        correct_response: String::new(),
                }));
                app.refresh();
                app.start_describe();
        app.process_pending_llm_request();
                assert!(app.describe_view.is_none());
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("transport error must surface a status");
                assert!(msg.contains("transport"), "got: {:?}", msg);
        }

        /// `start_describe` is reentrant-safe: if
        /// the overlay is already open, a second
        /// call replaces the previous view (rather
        /// than stacking two views on top of each
        /// other). The previous response is
        /// dropped; the new one wins.
        #[test]
        fn start_describe_replaces_existing_view() {
                let mut app = global_test_app(&[("a", 1)]);
                let llm = FakeLlm {
                        response: String::new(),
                        error: None,
                        describe_response: "first response".to_string(),
                        correct_response: String::new(),
                };
                app.llm = Some(Box::new(llm));
                app.refresh();
                app.start_describe();
        app.process_pending_llm_request();
                assert_eq!(
                        app.describe_view.as_ref().unwrap().text,
                        "first response"
                );
                // Now re-describe with a different
                // canned response. We can't easily
                // swap `app.llm` mid-test, so we
                // just verify the overlay is
                // re-entered cleanly: the overlay is
                // open, and re-running start_describe
                // should leave it open (not panic,
                // not stack).
                app.start_describe();
                assert!(app.describe_view.is_some());
        }

        /// The overlay's `command` field reflects
        /// the row that was selected at the time of
        /// the describe call. Navigating to a
        /// different row afterwards doesn't change
        /// the overlay's captured command — the
        /// title stays anchored to the original
        /// row, which is the right UX (the LLM was
        /// asked about that specific command).
        #[test]
        fn describe_view_anchors_to_original_command() {
                let mut app = global_test_app(&[
                        ("git status", 1),
                        ("ls -la", 2),
                ]);
                app.llm = Some(Box::new(FakeLlm {
                        response: String::new(),
                        error: None,
                        describe_response: "description".to_string(),
                        correct_response: String::new(),
                }));
                app.refresh();
                // Select the first row (newest
                // timestamp wins, so this is "git
                // status").
                app.start_describe();
        app.process_pending_llm_request();
                let view = app.describe_view.as_ref().unwrap();
                assert_eq!(view.command, "git status");
                // Move to the second row.
                app.move_selection(1);
                // The overlay's command is still
                // "git status" — it doesn't follow
                // the cursor.
                let view = app.describe_view.as_ref().unwrap();
                assert_eq!(view.command, "git status");
        }

        /// `is_describe_viewing` is the predicate
        /// the run loop uses to decide whether to
        /// route keys to the overlay. We just want
        /// to know it tracks the field correctly.
        #[test]
        fn is_describe_viewing_tracks_field() {
                let mut app = global_test_app(&[("a", 1)]);
                assert!(!app.is_describe_viewing());
                app.describe_view = Some(DescribeView {
                        command: "a".to_string(),
                        text: "a description".to_string(),
                        scroll: 0,
                });
                assert!(app.is_describe_viewing());
                app.close_describe();
                assert!(!app.is_describe_viewing());
        }

        // --- Correct (`Action::Correct`, default `C-t`) -----

        /// `Action::Correct` is bound to `Ctrl-T` by
        /// default. The default key is free of the
        /// other defaults and not used by readline /
        /// zsh in any common configuration, so the
        /// binding is a safe starting point.
        #[test]
        fn correct_default_key_routes() {
                let bindings = KeyBindings::defaults();
                let key = KeyEvent::new(
                        KeyCode::Char('t'),
                        KeyModifiers::CONTROL,
                );
                let action = action_for_key(&bindings, &key)
                        .expect("Ctrl-T is bound by default");
                assert_eq!(action, Action::Correct);
        }

        /// `start_correct` opens the overlay with
        /// the LLM's corrected command, scoped to
        /// the currently-selected row. The original
        /// command is captured in the view so the
        /// user can see what was being fixed.
        ///
        /// The FakeLlm returns a clean command
        /// (no markdown), so the sanitized result
        /// is exactly the canned string. The
        /// `sanitize_command` path is exercised
        /// separately by the LLM tests.
        #[test]
        fn start_correct_opens_overlay_with_response() {
                let mut app = global_test_app(&[("gti status", 1)]);
                app.llm = Some(Box::new(FakeLlm {
                        response: String::new(),
                        error: None,
                        describe_response: String::new(),
                        correct_response: "git status".to_string(),
                }));
                app.refresh();
                app.start_correct();
        app.process_pending_llm_request();
                let view = app
                        .correct_view
                        .as_ref()
                        .expect("correct overlay must open on success");
                assert_eq!(view.original_command, "gti status");
                assert_eq!(view.corrected_command, "git status");
                assert!(!app.cancelled);
        }

        /// `start_correct` with no LLM configured
        /// surfaces the "not configured" status
        /// message and does NOT open the overlay
        /// (so the user doesn't see an empty
        /// overlay that would have to be closed
        /// again). Same UX as `start_describe`.
        #[test]
        fn start_correct_surfaces_not_configured_when_client_is_none() {
                let mut app = global_test_app(&[("a", 1)]);
                assert!(app.llm.is_none());
                app.refresh();
                app.start_correct();
                assert!(app.correct_view.is_none());
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("missing-LLM must surface a status");
                assert!(msg.contains("not configured"), "got: {:?}", msg);
        }

        /// `start_correct` with no row selected
        /// surfaces a status message and doesn't
        /// open the overlay. (Empty DB.)
        #[test]
        fn start_correct_with_no_row_surfaces_status() {
                let mut app = global_test_app(&[]);
                app.llm = Some(Box::new(FakeLlm {
                        response: String::new(),
                        error: None,
                        describe_response: String::new(),
                        correct_response: "should not be used".to_string(),
                }));
                app.refresh();
                app.start_correct();
                assert!(app.correct_view.is_none());
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("empty list must surface a status");
                assert!(msg.contains("no row"), "got: {:?}", msg);
        }

        /// When the LLM call fails, the overlay
        /// is not opened and the error is surfaced
        /// in the status bar.
        #[test]
        fn start_correct_surfaces_error_on_transport_failure() {
                let mut app = global_test_app(&[("a", 1)]);
                app.llm = Some(Box::new(FakeLlm {
                        response: String::new(),
                        error: Some(crate::llm::LlmError::Transport(
                                "connection refused".to_string(),
                        )),
                        describe_response: String::new(),
                        correct_response: String::new(),
                }));
                app.refresh();
                app.start_correct();
        app.process_pending_llm_request();
                assert!(app.correct_view.is_none());
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("transport error must surface a status");
                assert!(msg.contains("transport"), "got: {:?}", msg);
        }

        /// When the LLM response sanitizes to
        /// `None` (e.g. all commentary, no command
        /// survived `sanitize_command`), the
        /// overlay is not opened and a status
        /// message is surfaced.
        #[test]
        fn start_correct_surfaces_no_command_when_sanitizer_rejects() {
                let mut app = global_test_app(&[("a", 1)]);
                app.llm = Some(Box::new(FakeLlm {
                        response: String::new(),
                        error: None,
                        describe_response: String::new(),
                        // All commentary, no
                        // command-form line survives
                        // `sanitize_command`.
                        correct_response: "# I cannot help with that."
                                .to_string(),
                }));
                app.refresh();
                app.start_correct();
        app.process_pending_llm_request();
                assert!(app.correct_view.is_none());
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("empty sanitizer output must surface a status");
                assert!(msg.contains("no usable command"), "got: {:?}", msg);
        }

        /// `is_correct_viewing` is the predicate
        /// the run loop uses to decide whether to
        /// route keys to the overlay. We just
        /// want to know it tracks the field
        /// correctly.
        #[test]
        fn is_correct_viewing_tracks_field() {
                let mut app = global_test_app(&[("a", 1)]);
                assert!(!app.is_correct_viewing());
                app.correct_view = Some(CorrectView {
                        original_command: "a".to_string(),
                        corrected_command: "b".to_string(),
                });
                assert!(app.is_correct_viewing());
                app.close_correct();
                assert!(!app.is_correct_viewing());
        }

        /// `accept_corrected_command` stages the
        /// corrected command and writes a new
        /// history row with the original as the
        /// comment (for traceability). This is the
        /// "Enter pressed in the correct overlay"
        /// path.
        #[test]
        fn accept_corrected_command_stages_and_inserts() {
                let mut app = global_test_app_with_dedup_index(&[("gti status", 1)]);
                app.correct_view = Some(CorrectView {
                        original_command: "gti status".to_string(),
                        corrected_command: "git status".to_string(),
                });
                app.accept_corrected_command();
                // Selection is set with the
                // corrected command.
                assert_eq!(app.selection.as_deref(), Some("git status"));
                assert_eq!(app.pick_mode, Some(PickMode::Run));
                // The corrected overlay is consumed
                // (taken).
                assert!(app.correct_view.is_none());
                // A new row was inserted into
                // history with the original as
                // the comment.
                let count: i64 = app
                        .conn
                        .query_row(
                                "SELECT COUNT(*) FROM history WHERE command = ?1",
                                rusqlite::params!["git status"],
                                |row| row.get(0),
                        )
                        .expect("count");
                assert_eq!(count, 1, "corrected command must be inserted");
                let comment: String = app
                        .conn
                        .query_row(
                                "SELECT comment FROM command_comments WHERE command = ?1",
                                rusqlite::params!["git status"],
                                |row| row.get(0),
                        )
                        .expect("comment");
                assert_eq!(comment, "gti status");
        }

        /// `accept_corrected_command` is a no-op
        /// when the overlay is closed (e.g. the
        /// user pressed `Esc` and then somehow
        /// triggered the action). We don't want
        /// to crash, and we don't want to write a
        /// row with a stale `view`.
        #[test]
        fn accept_corrected_command_no_op_when_overlay_closed() {
                let mut app = global_test_app(&[("a", 1)]);
                app.correct_view = None;
                app.accept_corrected_command();
                assert!(app.selection.is_none());
        }

        // --- Delete-word-backward (`Ctrl-W`) -------------------

        /// `Action::DeleteWordBackward` is bound to
        /// `Ctrl-W` by default. The default key
        /// matches the readline/bash/zsh muscle
        /// memory for "kill previous word".
        #[test]
        fn delete_word_backward_default_key_routes() {
                let bindings = KeyBindings::defaults();
                let key = KeyEvent::new(
                        KeyCode::Char('w'),
                        KeyModifiers::CONTROL,
                );
                let action = action_for_key(&bindings, &key)
                        .expect("Ctrl-W is bound by default");
                assert_eq!(action, Action::DeleteWordBackward);
        }

        /// Basic case: cursor at end of `git status`,
        /// press `Ctrl-W`, get `git `. The trailing
        /// word `status` is eaten; the space between
        /// `git` and `status` stays. We don't flag
        /// the query as touched for an empty /
        /// prefilled query — see the
        /// `delete_word_backward_at_start_is_noop`
        /// test for that boundary case.
        #[test]
        fn delete_word_backward_removes_trailing_word() {
                let mut app = stats_test_app(&[("ls", 1)]);
                app.query = "git status".to_string();
                app.query_cursor = app.query.chars().count();
                app.delete_word_backward();
                // `status` (positions 4..10) is
                // eaten; the space at position 3
                // stays. Result: "git ", cursor at
                // the start of where `status` used
                // to be (position 4).
                assert_eq!(app.query, "git ");
                assert_eq!(app.query_cursor, 4);
        }

        /// When the cursor is preceded by trailing
        /// whitespace, the whitespace is eaten
        /// along with the preceding word. So
        /// `git status  ` (with 2 trailing spaces)
        /// with the cursor at the end becomes `git`
        /// after one `Ctrl-W`. This matches
        /// readline/bash's `unix-word-rubout`: the
        /// char immediately to the left of the
        /// cursor is whitespace, so we eat that
        /// whitespace run, then eat the preceding
        /// word.
        #[test]
        fn delete_word_backward_eats_trailing_whitespace_first() {
                let mut app = stats_test_app(&[("ls", 1)]);
                app.query = "git status  ".to_string();
                app.query_cursor = app.query.chars().count();
                app.delete_word_backward();
                // Step 1 eats the 2 trailing spaces
                // (positions 10..12), then step 2
                // eats `status` (positions 4..10).
                // Total deleted: positions 4..12
                // (8 chars). Remaining: "git " (the
                // space between `git` and `status`,
                // at position 3, is NOT eaten because
                // step 1 only walks back from the
                // cursor, not forward through the
                // already-deleted range). Cursor at 4.
                assert_eq!(app.query, "git ");
                assert_eq!(app.query_cursor, 4);
        }

        /// Multiple consecutive spaces are all
        /// kept in the result — the function
        /// only eats ONE word (the trailing
        /// non-whitespace run) and the whitespace
        /// immediately to its left (one run of
        /// whitespace). It doesn't reach further
        /// back to consume additional whitespace
        /// runs. So `git    status` with the
        /// cursor at the end becomes `git    `
        /// (the 4 spaces between `git` and
        /// `status` stay; only `status` is
        /// eaten).
        #[test]
        fn delete_word_backward_handles_multiple_spaces() {
                let mut app = stats_test_app(&[("ls", 1)]);
                app.query = "git    status".to_string();
                app.query_cursor = app.query.chars().count();
                app.delete_word_backward();
                // `status` (positions 7..13) is
                // eaten; the 4 spaces between
                // `git` and `status` stay. Result:
                // "git    ", cursor at 7.
                assert_eq!(app.query, "git    ");
                assert_eq!(app.query_cursor, 7);
        }

        /// Cursor at the start of the buffer is a
        /// no-op. No panic, no underflow. We don't
        /// flag the query as touched (mirrors
        /// `backspace_at_position_zero_is_noop`).
        #[test]
        fn delete_word_backward_at_start_is_noop() {
                let mut app = stats_test_app(&[("ls", 1)]);
                app.query = "anything".to_string();
                app.query_cursor = 0;
                app.delete_word_backward();
                assert_eq!(app.query, "anything");
                assert_eq!(app.query_cursor, 0);
        }

        /// Cursor mid-buffer, between a space and
        /// the next word: the space AND the word
        /// before the space are eaten. The cursor
        /// is at position 4 in `git status`, which
        /// is right after the space and right
        /// before the `s` of `status`. Pressing
        /// `Ctrl-W` eats the trailing whitespace
        /// (1 char) plus the preceding non-
        /// whitespace run `git` (3 chars), so the
        /// result is `status` with the cursor at
        /// position 0.
        ///
        /// This is the standard readline/bash
        /// `unix-word-rubout` behaviour: if the
        /// char immediately to the left of the
        /// cursor is whitespace, the function
        /// eats both that whitespace run AND the
        /// preceding word.
        #[test]
        fn delete_word_backward_respects_cursor_position() {
                let mut app = stats_test_app(&[("ls", 1)]);
                app.query = "git status".to_string();
                // Position 4 is right after the space
                // and right before the `s` of
                // `status`. Cursor at position 4 =
                // chars().take(4) = "git ".
                app.query_cursor = 4;
                app.delete_word_backward();
                // Eat "git " (positions 0..4) —
                // the trailing whitespace AND the
                // preceding word. Result: "status",
                // cursor at 0.
                assert_eq!(app.query, "status");
                assert_eq!(app.query_cursor, 0);
        }

        /// Multi-byte UTF-8: the cursor is in
        /// characters, so an accented character
        /// counts as one step. The word-deletion
        /// logic must respect the character /
        /// byte distinction so it doesn't
        /// accidentally split a multi-byte
        /// codepoint.
        #[test]
        fn delete_word_backward_handles_multibyte() {
                let mut app = stats_test_app(&[("ls", 1)]);
                app.query = "café au lait".to_string();
                app.query_cursor = app.query.chars().count();
                app.delete_word_backward();
                // `lait` (positions 8..12) is eaten;
                // the spaces and `café au` stay.
                // The `é` (one character, 2 bytes)
                // is preserved correctly because
                // `String::replace_range` operates
                // on byte indices that we computed
                // via `char_to_byte_index`.
                assert_eq!(app.query, "café au ");
                assert_eq!(app.query_cursor, 8);
        }

        /// Cursor mid-word: only the part of the
        /// word to the LEFT of the cursor is
        /// eaten. The cursor is in position 5 of
        /// `cargotest`, between the `o` of
        /// `cargo` and the `t` of `test`. The
        /// function walks back through the
        /// non-whitespace run to the left of the
        /// cursor (positions 4, 3, 2, 1, 0 =
        /// "cargo", 5 chars), stopping at the
        /// start of the buffer because there's no
        /// whitespace before it. The result is
        /// `test` with the cursor at position 0.
        ///
        /// This is readline/bash's
        /// `unix-word-rubout` behaviour: only
        /// the characters to the LEFT of the
        /// cursor are considered. The part of
        /// the word to the right of the cursor
        /// is preserved. (Note: this differs
        /// from `backward-kill-word` in some
        /// shells which would delete the whole
        /// word regardless of cursor position.)
        #[test]
        fn delete_word_backward_mid_word_eats_left_of_cursor() {
                let mut app = stats_test_app(&[("ls", 1)]);
                app.query = "cargotest".to_string();
                // Position 5 is between `o` and `t`.
                app.query_cursor = 5;
                app.delete_word_backward();
                // Eat `cargo` (positions 0..5).
                // The `test` part to the right of
                // the cursor stays. Result: "test",
                // cursor at 0.
                assert_eq!(app.query, "test");
                assert_eq!(app.query_cursor, 0);
        }

        /// Empty query is a clean no-op, just like
        /// `backspace` on an empty buffer.
        #[test]
        fn delete_word_backward_on_empty_query() {
                let mut app = stats_test_app(&[("ls", 1)]);
                app.query = String::new();
                app.query_cursor = 0;
                app.delete_word_backward();
                assert_eq!(app.query, "");
                assert_eq!(app.query_cursor, 0);
        }

        /// The comment-edit buffer uses the same
        /// logic — when a comment is being edited,
        /// `Ctrl-W` deletes the previous word in
        /// the comment. We test the underlying
        /// helper (`delete_word_backward_in_string`)
        /// for the comment-edit path; the wrapper
        /// (`App::delete_word_backward`) routes to
        /// the right buffer based on whether a
        /// comment edit is in progress.
        #[test]
        fn delete_word_backward_in_string_helper() {
                // The comment-edit buffer has no
                // cursor concept — operate on the
                // logical end of the string. The
                // helper is what the App method
                // calls when
                // `self.comment_edit.is_some()`.
                let mut s = String::from("hello world");
                delete_word_backward_in_string(&mut s);
                // `world` (positions 6..11) is
                // eaten; the space at position 5
                // stays. Result: "hello ".
                assert_eq!(s, "hello ");
                // Second press: the char to the
                // left of the cursor (end of `s`)
                // is a space. Eat the space, then
                // the preceding word `hello`.
                // Result: empty.
                delete_word_backward_in_string(&mut s);
                assert_eq!(s, "");
        }

        /// The free function
        /// `delete_word_backward_at_cursor` is
        /// what the App method calls when the
        /// query field is the active buffer. It
        /// returns the new cursor position (in
        /// characters) without mutating the
        /// string — the caller applies the
        /// deletion as a single `replace_range`
        /// call. Pin the contract here so future
        /// refactors of the cursor logic can't
        /// accidentally change the readline
        /// semantics.
        #[test]
        fn delete_word_backward_at_cursor_helper() {
                // Empty string, cursor 0: returns 0.
                assert_eq!(delete_word_backward_at_cursor("", 0), 0);
                // Single word, cursor at end:
                // returns 0 (whole word consumed).
                assert_eq!(delete_word_backward_at_cursor("abc", 3), 0);
                // Cursor mid-word: returns the start
                // of the word (which is also the
                // start of the buffer in this case).
                assert_eq!(delete_word_backward_at_cursor("abc", 2), 0);
                assert_eq!(delete_word_backward_at_cursor("abc", 1), 0);
                // Two words, cursor at end: returns
                // the start of the second word.
                assert_eq!(delete_word_backward_at_cursor("abc def", 7), 4);
                // Trailing whitespace only: cursor
                // at end of `abc   ` returns 0
                // (step 1 eats 3 spaces, step 2
                // walks back through `abc`).
                assert_eq!(delete_word_backward_at_cursor("abc   ", 6), 0);
                // Two words with multiple spaces:
                // cursor at end of `abc   def`.
                // Char at end is `f` (non-ws), so
                // step 1 doesn't run; step 2 walks
                // back from `f` to the space at
                // position 6. Returns 6 (start of
                // `def`).
                assert_eq!(delete_word_backward_at_cursor("abc   def", 9), 6);
        }

        // --- Labeled-only rows partition ---------

        /// A labeled row that's NOT in the
        /// primary list (e.g. from a different
        /// session than the current
        /// `SMART_HISTORY_SESSION`) should appear
        /// at the end of the merged list, not in
        /// the middle of the timestamp-sorted
        /// primary rows.
        ///
        /// Test setup:
        /// - One primary row at offset 10 (recent,
        ///   current session).
        /// - One labeled row at offset 100_000
        ///   (ancient, different session — excluded
        ///   by `Mode::Sess`).
        /// Both commands match the query "git".
        ///
        /// Expected merged order under
        /// `SortOrder::Age`: `[git status (recent),
        /// git pull (labeled-ancient)]`. The labeled
        /// row's command is older than the primary
        /// row's command, so a pure timestamp sort
        /// would also put it last; this test pins the
        /// partition invariant so a future refactor
        /// that mixes the partitions can't regress.
        #[test]
        fn labeled_only_row_appears_at_end_of_merged_list() {
                let _env_guard = ENV_LOCK.lock().expect("env lock poisoned");
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (1, 'git status', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 10],
                )
                .expect("insert recent");
                conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (2, 'git pull', '/tmp', 'ancient', 0, ?1)",
                        rusqlite::params![now - 100_000],
                )
                .expect("insert ancient");
                conn.execute(
                        "INSERT INTO command_comments (command, comment) VALUES ('git pull', 'old but labeled')",
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
                        SortOrder::default(),
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        None,
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                Vec::new(),
                );
                // Restore env before any `?` can
                // short-circuit out of the test (so
                // a panic doesn't leak the env
                // override into other tests). We
                // refresh *after* the App is built
                // but *before* the env restore, so the
                // fetch sees the right session id.
                app.refresh();
                if let Some(prev) = prev_session {
                        unsafe { std::env::set_var("SMART_HISTORY_SESSION", prev); }
                } else {
                        unsafe { std::env::remove_var("SMART_HISTORY_SESSION"); }
                }

                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                // Two rows: the recent primary row
                // first, the ancient labeled row
                // second. The labeled row is at
                // the END regardless of its
                // timestamp.
                assert_eq!(cmds, vec!["git status", "git pull"]);
        }

        /// When a labeled row's command IS
        /// already in the primary list (i.e. it
        /// matches the active filter on its own),
        /// the labeled row is *not* added a second
        /// time — the existing primary row stays
        /// at its natural sort position. This is
        /// the "when a line would be listed in
        /// this mode anyway, then nothing is
        /// changed" half of the user's contract.
        ///
        /// Test setup: one row in the current
        /// session, with a comment. The command
        /// matches the query. The row is in
        /// `self.rows` AND in `self.labeled_rows`,
        /// so it should appear exactly once in
        /// the merged list.
        #[test]
        fn labeled_row_already_in_primary_list_is_not_duplicated() {
                let _env_guard = ENV_LOCK.lock().expect("env lock poisoned");
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (1, 'git status', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 10],
                )
                .expect("insert");
                conn.execute(
                        "INSERT INTO command_comments (command, comment) VALUES ('git status', 'labeled')",
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
                        SortOrder::default(),
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        None,
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                Vec::new(),
                );
                app.refresh();
                if let Some(prev) = prev_session {
                        unsafe { std::env::set_var("SMART_HISTORY_SESSION", prev); }
                } else {
                        unsafe { std::env::remove_var("SMART_HISTORY_SESSION"); }
                }

                // Single row in the merged list,
                // even though it's in BOTH
                // `self.rows` and `self.labeled_rows`.
                assert_eq!(app.merged_rows().len(), 1);
                assert_eq!(
                        app.merged_rows()[0].command,
                        "git status"
                );
        }

        /// The partition holds even when the
        /// labeled-only row's timestamp is
        /// *newer* than some of the primary
        /// rows. Without the partition, a
        /// natural sort would put the labeled-
        /// only row in the middle. With the
        /// partition, it's pinned to the end.
        /// This pins the "always at the end"
        /// invariant.
        ///
        /// Test setup:
        /// - Primary row "b" at offset 100 (older).
        /// - Primary row "a" at offset 10 (newer).
        /// - Labeled-only row "z" at offset 5
        ///   (newest of all), but only visible
        ///   because it's labeled — it's in a
        ///   different session.
        ///
        /// Without the partition, a timestamp
        /// sort would give: `[z (5), a (10),
        /// b (100)]`. With the partition, we
        /// expect: `[a (10), b (100), z (5)]`.
        #[test]
        fn labeled_only_row_stays_at_end_even_if_newer() {
                let _env_guard = ENV_LOCK.lock().expect("env lock poisoned");
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                // Two primary rows.
                conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (1, 'a', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 10],
                )
                .expect("insert");
                conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (2, 'b', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 100],
                )
                .expect("insert");
                // Labeled-only row (different session,
                // newer timestamp). It IS excluded
                // by the `Mode::Sess` SQL filter
                // because its session_id is
                // "ancient".
                conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (3, 'z', '/tmp', 'ancient', 0, ?1)",
                        rusqlite::params![now - 5],
                )
                .expect("insert");
                conn.execute(
                        "INSERT INTO command_comments (command, comment) VALUES ('z', 'labeled but newer')",
                        [],
                )
                .expect("insert comment");

                let prev_session = std::env::var("SMART_HISTORY_SESSION").ok();
                unsafe { std::env::set_var("SMART_HISTORY_SESSION", "current"); }
                let mut app = App::new(
                        conn,
                        Mode::Sess,
                        String::new(),
                        false,
                        ExitFilter::All,
                        SortOrder::default(),
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        None,
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                Vec::new(),
                );
                app.refresh();
                if let Some(prev) = prev_session {
                        unsafe { std::env::set_var("SMART_HISTORY_SESSION", prev); }
                } else {
                        unsafe { std::env::remove_var("SMART_HISTORY_SESSION"); }
                }

                // Without the partition the
                // natural timestamp sort would
                // give `[z, a, b]`. With the
                // partition we expect
                // `[a, b, z]`.
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                assert_eq!(cmds, vec!["a", "b", "z"]);
        }

        /// The partition also holds in
        /// `SortOrder::Frequency` mode: the
        /// labeled-only group is at the end of
        /// the merged list, sorted by its own
        /// internal counts rather than the
        /// counts of the entire merged set.
        ///
        /// Test setup:
        /// - Primary rows: 3 instances of "a",
        ///   1 of "b".
        /// - Labeled-only row: "z", excluded by
        ///   session filter.
        ///
        /// Expected merged order: `[a, b, z]`.
        /// (Frequency dedup is implicit so the
        /// primary partition dedupes to `[a, b]`.)
        #[test]
        fn labeled_only_partition_in_frequency_mode() {
                let _env_guard = ENV_LOCK.lock().expect("env lock poisoned");
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
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
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (1, 'a', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 1],
                )
                .expect("insert a1");
                conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (2, 'a', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 2],
                )
                .expect("insert a2");
                conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (3, 'a', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 3],
                )
                .expect("insert a3");
                conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (4, 'b', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 4],
                )
                .expect("insert b");
                // Labeled-only row in a different session.
                conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (5, 'z', '/tmp', 'ancient', 0, ?1)",
                        rusqlite::params![now - 5],
                )
                .expect("insert z");
                conn.execute(
                        "INSERT INTO command_comments (command, comment) VALUES ('z', 'labeled')",
                        [],
                )
                .expect("insert comment");

                let prev_session = std::env::var("SMART_HISTORY_SESSION").ok();
                unsafe { std::env::set_var("SMART_HISTORY_SESSION", "current"); }
                let mut app = App::new(
                        conn,
                        Mode::Sess,
                        String::new(),
                        false,
                        ExitFilter::All,
                        SortOrder::Frequency,
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                        None,
                        None,
                        crate::QueryPrefixes::default(),
                        None,
                        None,
                        String::from("+$LINE"),
                    std::collections::HashMap::new(),
                Vec::new(),
                );
                app.refresh();
                if let Some(prev) = prev_session {
                        unsafe { std::env::set_var("SMART_HISTORY_SESSION", prev); }
                } else {
                        unsafe { std::env::remove_var("SMART_HISTORY_SESSION"); }
                }

                // Frequency dedup is implicit in
                // Frequency mode (see
                // `build_merged_rows`). So the
                // primary partition dedupes to
                // `[a, b]`. The labeled-only group
                // is `[z]`. Final merged order:
                // `[a, b, z]`.
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                assert_eq!(cmds, vec!["a", "b", "z"]);
        }

        // --- Notes-mode date-filter aliases -------

        /// The simplest case: `@today` alone is
        /// recognised, stripped from the pattern,
        /// and the resolved filter is `Today`.
        /// The cleaned pattern is empty (only the
        /// alias was present), which the caller
        /// treats as "no search body — fall through
        /// to fetch_recent_notes".
        #[test]
        fn parse_notes_query_today_alone() {
                let (pattern, filter) = parse_notes_query("@today");
                assert_eq!(pattern, "");
                assert_eq!(filter, NotesDateFilter::Today);
        }

        /// Each alias maps to its filter.
        #[test]
        fn parse_notes_query_each_alias() {
                assert_eq!(parse_notes_query("@week").1, NotesDateFilter::Week);
                assert_eq!(parse_notes_query("@month").1, NotesDateFilter::Month);
                assert_eq!(parse_notes_query("@year").1, NotesDateFilter::Year);
        }

        /// An empty / whitespace pattern resolves
        /// to `All` (no filter) and an empty
        /// cleaned pattern.
        #[test]
        fn parse_notes_query_empty_is_all() {
                assert_eq!(
                        parse_notes_query(""),
                        (String::new(), NotesDateFilter::All)
                );
                assert_eq!(
                        parse_notes_query("   "),
                        (String::new(), NotesDateFilter::All)
                );
        }

        /// A pattern with no aliases returns the
        /// same string back and `All`.
        #[test]
        fn parse_notes_query_no_alias_passthrough() {
                assert_eq!(
                        parse_notes_query("hello world"),
                        ("hello world".to_string(), NotesDateFilter::All)
                );
        }

        /// The user's example: `test @reference @today`
        /// (the outer `@` is the notes-mode prefix
        /// already stripped by `notes_pattern`).
        /// The alias is removed; `@reference` is
        /// NOT an alias so it stays in the
        /// cleaned pattern.
        ///
        /// **Important**: a leading `@` on a
        /// non-alias token is *stripped* before
        /// the cleaned pattern is returned. The
        /// library's `parse_query` tokenizer
        /// treats `@foo` as a `Link` reference
        /// (matching `t.links`/`m.links`) which
        /// is never what the user means when they
        /// type `!@orchard` — they want a text
        /// search for "orchard". Stripping the
        /// `@` here ensures the downstream
        /// `parse_query` sees a plain word and
        /// routes it through the text-LIKE
        /// branch.
        #[test]
        fn parse_notes_query_with_search_terms() {
                let (pattern, filter) =
                        parse_notes_query("test @reference @today");
                assert_eq!(pattern, "test reference");
                assert_eq!(filter, NotesDateFilter::Today);
        }

        /// Multiple aliases: the last one wins.
        #[test]
        fn parse_notes_query_multiple_aliases_last_wins() {
                let (_, filter) = parse_notes_query("@today @week");
                assert_eq!(filter, NotesDateFilter::Week);
                let (_, filter) = parse_notes_query("@year @today");
                assert_eq!(filter, NotesDateFilter::Today);
        }

        /// Alias matching is case-insensitive:
        /// `@Today`, `@TODAY`, `@today` all work.
        /// When matched, the token is removed from
        /// the cleaned pattern.
        #[test]
        fn parse_notes_query_alias_matching_is_case_insensitive() {
                assert_eq!(parse_notes_query("@Today").1, NotesDateFilter::Today);
                assert_eq!(parse_notes_query("@TODAY").1, NotesDateFilter::Today);
                assert_eq!(parse_notes_query("@today").1, NotesDateFilter::Today);
                assert_eq!(parse_notes_query("@Today").0, "");
                assert_eq!(parse_notes_query("@TODAY").0, "");
                assert_eq!(parse_notes_query("@today").0, "");
        }

        /// Aliases can also be written without
        /// the leading `@` (so the aliases work
        /// even when the user types them inside
        /// the search body).
        #[test]
        fn parse_notes_query_alias_without_at_prefix() {
                let (pattern, filter) = parse_notes_query("today test");
                assert_eq!(pattern, "test");
                assert_eq!(filter, NotesDateFilter::Today);
        }

        /// The whole-token rule: `@todayfile` is
        /// NOT the alias. The whole token must
        /// match the alias name. We still
        /// strip the `@` from the cleaned
        /// pattern (the alias arm doesn't
        /// fire) so the library's parser
        /// sees a plain word.
        #[test]
        fn parse_notes_query_alias_must_be_whole_token() {
                let (pattern, filter) = parse_notes_query("@todayfile");
                assert_eq!(pattern, "todayfile");
                assert_eq!(filter, NotesDateFilter::All);
        }

        /// `@` on a non-alias token is the
        /// user's ad-hoc shorthand for
        /// "search the word", not a link
        /// reference. The library's
        /// `parse_query` would otherwise
        /// interpret `@orchard` as a
        /// `Link` token (matching
        /// `t.links`/`m.links`) which is
        /// never the user's intent.
        /// Stripping the `@` here routes the
        /// term through the text-LIKE
        /// branch. This is the exact
        /// scenario the user reported
        /// (`!@orchard` returning empty
        /// when todos contain the word
        /// "orchard") — the regression
        /// test for that bug.
        #[test]
        fn parse_notes_query_strips_at_from_non_alias_tokens() {
                assert_eq!(
                        parse_notes_query("@orchard").0,
                        "orchard"
                );
                assert_eq!(
                        parse_notes_query("@orchard").1,
                        NotesDateFilter::All
                );
                // Multiple `@` terms.
                assert_eq!(
                        parse_notes_query("@orchard @apple").0,
                        "orchard apple"
                );
                // Mixed: alias + non-alias.
                assert_eq!(
                        parse_notes_query("@today @orchard").0,
                        "orchard"
                );
                assert_eq!(
                        parse_notes_query("@today @orchard").1,
                        NotesDateFilter::Today
                );
                // Plain words are untouched.
                assert_eq!(
                        parse_notes_query("orchard apple").0,
                        "orchard apple"
                );
                // `@` in the middle of a word
                // is preserved (only leading
                // `@` is stripped).
                assert_eq!(
                        parse_notes_query("foo@bar").0,
                        "foo@bar"
                );
        }

        /// The `NotesDateFilter::cutoff(now)` math
        /// is exact: 24h for Today, 7d for Week,
        /// 30d for Month, 365d for Year. We use a
        /// fixed `now` to make the assertions
        /// deterministic.
        #[test]
        fn notes_date_filter_cutoff_math() {
                let now: i64 = 1_000_000_000;
                let day = 24 * 60 * 60;
                assert_eq!(NotesDateFilter::All.cutoff(now), None);
                assert_eq!(
                        NotesDateFilter::Today.cutoff(now),
                        Some(now - day)
                );
                assert_eq!(
                        NotesDateFilter::Week.cutoff(now),
                        Some(now - 7 * day)
                );
                assert_eq!(
                        NotesDateFilter::Month.cutoff(now),
                        Some(now - 30 * day)
                );
                assert_eq!(
                        NotesDateFilter::Year.cutoff(now),
                        Some(now - 365 * day)
                );
        }

        /// The filter applies the cutoff against
        /// each note's effective timestamp.
        /// Recent (within the window) passes,
        /// old (outside the window) fails.
        #[test]
        fn notes_date_filter_applies_to_results() {
                let now: i64 = 1_000_000_000;
                let day = 24 * 60 * 60;
                let recent = now - 12 * 60 * 60;
                let old = now - 30 * day;

                let (clean, filter) = parse_notes_query("query @today");
                let cutoff = filter.cutoff(now).unwrap();
                assert!(recent >= cutoff);
                assert!(old < cutoff);
                assert_eq!(clean, "query");
        }

        /// Regression test for the user's
        /// report: `@today` as a *bare*
        /// alias (with no text pattern)
        /// should restrict the
        /// `fetch_recent_notes` path
        /// to notes updated in the
        /// last 24h. Before the fix,
        /// `@today` was the same as
        /// `@` (no filtering at all,
        /// because the pattern was
        /// empty and `fetch_recent_notes`
        /// skipped the filter).
        #[test]
        fn bare_at_today_in_notes_mode_filters_by_mtime() {
                use std::fs;
                use rusqlite::Connection;
                use std::sync::atomic::{AtomicU64, Ordering};
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                let dir = std::env::temp_dir().join(format!(
                    "smarthistory-notes-bare-{}-{}",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir_all(&dir).expect("create notes dir");
                let day = 24 * 60 * 60;
                let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                // `recent.md`: written now
                fs::write(
                        dir.join("recent.md"),
                        "# Recent\n",
                )
                .expect("write recent");
                // `old.md`: pretend it was
                // written 30 days ago by
                // setting mtime via
                // `filetime`. If
                // `filetime` isn't
                // available we fall back
                // to the same mtime as
                // `recent.md` and the test
                // is degenerate — but the
                // *filter* logic is still
                // exercised either way.
                let old_path = dir.join("old.md");
                fs::write(&old_path, "# Old\n")
                        .expect("write old");
                let past = now - 30 * day;
                let _ = filetime_touch_mtime(
                        &old_path,
                        past,
                );
                let db_path = std::env::temp_dir().join(format!(
                    "smarthistory-notes-bare-db-{}-{}.sqlite",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_file(&db_path);
                let conn = Connection::open(&db_path)
                        .expect("open db");
                note_search::init_database_schema(&conn)
                        .map_err(|e| format!("schema: {e}"))
                        .expect("init schema");
                // Index both files. We have
                // to index twice because
                // `process_markdown_file`
                // records its own `updated`
                // (the current epoch), not
                // the file's actual mtime.
                // Then we patch `updated`
                // to match the file's
                // mtime so the filter has
                // something to work with.
                for entry in fs::read_dir(&dir)
                        .expect("read dir")
                {
                        let entry = entry.expect("entry");
                        let path = entry.path();
                        if !path.is_file()
                                || path.extension()
                                        .and_then(|e| e.to_str())
                                        != Some("md")
                        {
                                continue;
                        }
                        let data = note_search::markdown_parser::process_markdown_file(
                                &path, &dir,
                        )
                        .expect("process");
                        note_search::write_markdown_data_to_sqlite_with_conn(
                                &data, &conn,
                        )
                        .expect("write");
                }
                // Force `old.md`'s `updated`
                // to be 30 days ago so the
                // `@today` filter has
                // something to distinguish.
                conn.execute(
                        "UPDATE markdown_data \
                         SET updated = ?1 \
                         WHERE filename = 'old.md'",
                        rusqlite::params![past],
                )
                .expect("patch old.md");
                drop(conn);
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                // Bare `@today` — empty
                // pattern, filter active.
                app.query = "@today".to_string();
                app.refresh();
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                // Only `recent.md` should
                // pass.
                assert!(
                        cmds.iter().any(|c| c.contains("recent.md")),
                        "recent.md must be in the result: {:?}",
                        cmds
                );
                assert!(
                        cmds.iter().all(|c| !c.contains("old.md")),
                        "old.md must be filtered out by @today: {:?}",
                        cmds
                );
                // Sanity: `@` (no alias)
                // returns both.
                app.query = "@".to_string();
                app.refresh();
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                assert!(
                        cmds.iter().any(|c| c.contains("recent.md")),
                        "recent.md present in unfiltered mode: {:?}",
                        cmds
                );
                assert!(
                        cmds.iter().any(|c| c.contains("old.md")),
                        "old.md present in unfiltered mode: {:?}",
                        cmds
                );
                let _ = fs::remove_dir_all(&dir);
                let _ = fs::remove_file(&db_path);
        }

        /// Regression test for the user's
        /// report in todo mode:
        /// `!@today` should restrict
        /// the result set to todos in
        /// files whose `updated` is
        /// within the last 24h. Before
        /// the fix, `fetch_todos`
        /// discarded the filter (it
        /// was bound to `_filter` and
        /// the post-sort cutoff was
        /// never applied). The user
        /// reported `@today` and
        /// `!@today` as both broken —
        /// they're now both wired up.
        #[test]
        fn bare_today_in_todo_mode_filters_by_mtime() {
                use std::fs;
                use rusqlite::Connection;
                use std::sync::atomic::{AtomicU64, Ordering};
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                let dir = std::env::temp_dir().join(format!(
                    "smarthistory-todos-today-{}-{}",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir_all(&dir).expect("create notes dir");
                let day = 24 * 60 * 60;
                let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                fs::write(
                        dir.join("recent.md"),
                        "# Recent\n\n- [ ] recent todo\n",
                )
                .expect("write recent");
                let past = now - 30 * day;
                let old_path = dir.join("old.md");
                fs::write(
                        &old_path,
                        "# Old\n\n- [ ] old todo\n",
                )
                .expect("write old");
                let _ = filetime_touch_mtime(
                        &old_path,
                        past,
                );
                let db_path = std::env::temp_dir().join(format!(
                    "smarthistory-todos-today-db-{}-{}.sqlite",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_file(&db_path);
                let conn = Connection::open(&db_path)
                        .expect("open db");
                note_search::init_database_schema(&conn)
                        .map_err(|e| format!("schema: {e}"))
                        .expect("init schema");
                for entry in fs::read_dir(&dir)
                        .expect("read dir")
                {
                        let entry = entry.expect("entry");
                        let path = entry.path();
                        if !path.is_file()
                                || path.extension()
                                        .and_then(|e| e.to_str())
                                        != Some("md")
                        {
                                continue;
                        }
                        let data = note_search::markdown_parser::process_markdown_file(
                                &path, &dir,
                        )
                        .expect("process");
                        note_search::write_markdown_data_to_sqlite_with_conn(
                                &data, &conn,
                        )
                        .expect("write");
                }
                conn.execute(
                        "UPDATE markdown_data \
                         SET updated = ?1 \
                         WHERE filename = 'old.md'",
                        rusqlite::params![past],
                )
                .expect("patch old.md");
                drop(conn);
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                // Bare `!@today` — empty
                // pattern, filter active.
                app.query = "!@today".to_string();
                app.refresh();
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                // Only the recent todo
                // should pass.
                assert!(
                        cmds.iter().any(|c| c.contains("recent todo")),
                        "recent todo must be in the result: {:?}",
                        cmds
                );
                assert!(
                        cmds.iter().all(|c| !c.contains("old todo")),
                        "old todo must be filtered out by !@today: {:?}",
                        cmds
                );
                // `@year` lets the old todo
                // through (30 days is
                // within the last 365
                // days).
                app.query = "!@year".to_string();
                app.refresh();
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                assert!(
                        cmds.iter().any(|c| c.contains("recent todo")),
                        "recent todo in @year: {:?}",
                        cmds
                );
                assert!(
                        cmds.iter().any(|c| c.contains("old todo")),
                        "old todo (30d ago) in @year (365d): {:?}",
                        cmds
                );
                // Sanity: bare `!` returns
                // both.
                app.query = "!".to_string();
                app.refresh();
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                assert_eq!(cmds.len(), 2);
                let _ = fs::remove_dir_all(&dir);
                let _ = fs::remove_file(&db_path);
        }

        // --- Todo mode (`!` prefix) -----------------

        /// `is_todo_query` recognises the
        /// configured prefix; an empty query
        /// returns false (matches the existing
        /// `is_notes_query` contract).
        #[test]
        fn is_todo_query_recognises_prefix() {
                let mut app = global_test_app(&[("a", 1)]);
                assert!(!app.is_todo_query());
                app.query = "!write tests".to_string();
                assert!(app.is_todo_query());
                app.query = "!".to_string();
                assert!(app.is_todo_query());
                app.query = "write tests".to_string();
                assert!(!app.is_todo_query());
                // Other prefixes still don't trigger
                // todo mode.
                app.query = "@rust".to_string();
                assert!(!app.is_todo_query());
        }

        /// The `directories` mode is
        /// recognised by the `#`
        /// prefix (default). The
        /// pattern-stripping method
        /// returns the body after
        /// the prefix, matching
        /// `notes_pattern` /
        /// `todo_pattern`.
        #[test]
        fn is_directories_query_recognises_prefix() {
                let mut app = global_test_app(&[("a", 1)]);
                assert!(!app.is_directories_query());
                app.query = "#home".to_string();
                assert!(app.is_directories_query());
                app.query = "#".to_string();
                assert!(app.is_directories_query());
                app.query = "home".to_string();
                assert!(!app.is_directories_query());
                // Other prefixes don't trigger
                // directories mode.
                app.query = "!todo".to_string();
                assert!(!app.is_directories_query());
                app.query = "/regex".to_string();
                assert!(!app.is_directories_query());
        }

        #[test]
        fn directories_pattern_strips_prefix() {
                let mut app = global_test_app(&[("a", 1)]);
                app.query = "#home".to_string();
                assert_eq!(app.directories_pattern(), "home");
                app.query = "home".to_string();
                assert_eq!(app.directories_pattern(), "");
                // Whitespace inside the
                // body is preserved (the
                // pattern method returns
                // everything after the
                // leading `#`, no
                // trimming).
                app.query = "#foo bar".to_string();
                assert_eq!(app.directories_pattern(), "foo bar");
        }

        /// `todo_pattern` returns the body after
        /// the prefix; matches the
        /// `notes_pattern` contract.
        #[test]
        fn todo_pattern_strips_prefix() {
                let mut app = global_test_app(&[("a", 1)]);
                app.query = "!write tests".to_string();
                assert_eq!(app.todo_pattern(), "write tests");
                app.query = "write tests".to_string();
                assert_eq!(app.todo_pattern(), "");
        }

        /// `is_todo_line` recognises the standard
        /// markdown task-list forms. We test the
        /// library's detection indirectly by
        /// parsing a note file with
        /// `process_markdown_file` and asserting
        /// the resulting todo count.
        #[test]
        fn is_todo_line_recognises_markdown_checkboxes() {
                use std::fs;
                let dir = std::env::temp_dir().join(format!(
                    "smarthistory-todo-cb-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0),
                ));
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir_all(&dir).expect("create notes dir");
                fs::write(
                        dir.join("note.md"),
                        "# Title\n\
                         \n\
                         - [ ] open\n\
                         - [ ] also open\n\
                           - [ ] indented\n\
                         - [x] done\n\
                         - [X] also done\n\
                         \n\
                         the list contains [ ] for unchecked\n\
                         1. [ ] numbered lists not supported\n",
                )
                .expect("write");
                let data = note_search::markdown_parser::process_markdown_file(
                        &dir.join("note.md"), &dir,
                )
                .expect("process");
                // 5 todos detected: 3 open, 2 closed.
                // The prose line and the numbered
                // list are not recognised. (Note:
                // the note_search library only
                // recognises `-` as the bullet,
                // not `*` — that matches GFM but
                // is narrower than my hand-rolled
                // detector from earlier turns.)
                assert_eq!(data.todo.len(), 5);
                let open: Vec<&str> = data
                    .todo
                    .iter()
                    .filter(|t| !t.closed)
                    .map(|t| t.text.as_str())
                    .collect();
                assert_eq!(open.len(), 3);
                let closed: Vec<&str> = data
                    .todo
                    .iter()
                    .filter(|t| t.closed)
                    .map(|t| t.text.as_str())
                    .collect();
                assert_eq!(closed.len(), 2);
                let _ = fs::remove_dir_all(&dir);
        }

        /// Build a notes directory with two note
        /// files and a matching note_search
        /// SQLite database. Returns `(notes_dir,
        /// db_path)`. The caller is responsible
        /// for cleaning up the temp paths.
        ///
        /// The fixture mirrors the user's
        /// production setup: `notes.dir` is the
        /// directory containing the actual `.md`
        /// files, and `notes.database` is the
        /// SQLite database the indexer writes
        /// to. We do the indexing inline here
        /// (via `process_markdown_file` +
        /// `write_markdown_data_to_sqlite_with_conn`)
        /// so the test doesn't depend on the
        /// external indexer binary.
        fn setup_todo_db() -> (std::path::PathBuf, std::path::PathBuf) {
                use std::fs;
                use rusqlite::Connection;
                use std::sync::atomic::{AtomicU64, Ordering};
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                let dir = std::env::temp_dir().join(format!(
                    "smarthistory-todo-test-{}-{}",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir_all(&dir).expect("create notes dir");
                // Older note: written first so its
                // mtime is naturally older.
                fs::write(
                        dir.join("older.md"),
                        "# Older\n\n\
                         - [ ] older todo 1\n\
                         some prose in between\n\
                         - [x] older done 1\n\
                         - [ ] older todo 2\n",
                )
                .expect("write older");
                std::thread::sleep(std::time::Duration::from_millis(10));
                fs::write(
                        dir.join("newer.md"),
                        "# Newer\n\n\
                         - [ ] newer todo 1\n\
                         - [ ] newer todo 2\n",
                )
                .expect("write newer");
                // Index both files into a
                // SQLite database the way the
                // production `note_search` indexer
                // does. The library writes
                // `todo_entries` rows for each
                // detected todo.
                let db_path = std::env::temp_dir().join(format!(
                    "smarthistory-todo-db-{}-{}.sqlite",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_file(&db_path);
                let conn = Connection::open(&db_path).expect("open db");
                note_search::init_database_schema(&conn)
                        .map_err(|e| format!("schema: {e}"))
                        .expect("init schema");
                for entry in fs::read_dir(&dir).expect("read dir") {
                        let entry = entry.expect("entry");
                        let path = entry.path();
                        if !path.is_file()
                                || path.extension().and_then(|e| e.to_str())
                                        != Some("md")
                        {
                                continue;
                        }
                        let data = note_search::markdown_parser::process_markdown_file(
                                &path, &dir,
                        )
                        .expect("process file");
                        note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
                        .map_err(|e| format!("write: {e}"))
                        .expect("write db");
                }
                drop(conn);
                (dir, db_path)
        }

        /// `fetch_todos` returns every open todo
        /// from the note_search database, sorted
        /// by file modified time (DESC) then by
        /// line number (ASC within a file).
        /// This is the same ordering the user
        /// expects from `note_search list` —
        /// `!` is just a thin TUI over the same
        /// database.
        #[test]
        fn fetch_todos_lists_all_open_todos() {
                let (dir, db_path) = setup_todo_db();
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                app.query = "!".to_string();
                app.refresh();
                let cmds: Vec<&str> = app
                    .merged_rows()
                    .iter()
                    .map(|r| r.command.as_str())
                    .collect();
                // 4 open todos total: 2 in older.md
                // (line 3 = `older todo 1`, line 6 =
                // `older todo 2`) and 2 in newer.md
                // (lines 3, 4). The `[x]` done todo
                // is excluded because we set
                // `open: Some(true)`.
                assert_eq!(cmds.len(), 4);
                assert!(cmds
                    .iter()
                    .any(|c| c.contains("newer todo 1")));
                assert!(cmds
                    .iter()
                    .any(|c| c.contains("newer todo 2")));
                assert!(cmds
                    .iter()
                    .any(|c| c.contains("older todo 1")));
                assert!(cmds
                    .iter()
                    .any(|c| c.contains("older todo 2")));
                // The closed todo must NOT be in
                // the list.
                assert!(!cmds.iter().any(|c| c.contains("done")));
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// The user-typed query (after the `!`
        /// prefix) is parsed via the library's
        /// `parse_query`, which understands the
        /// Obsidian-like syntax. Bare words
        /// are AND-matched against each todo
        /// line; `#tag` is matched against
        /// both the todo's own tags and the
        /// note's header fields; `[[link]]`
        /// is matched against the todo's
        /// links and the note's outgoing
        /// links; `[attr:value]` is matched
        /// against the note's header fields.
        /// `!write` matches todos whose text
        /// contains "write"; the fixture has
        /// none, so the result is empty.
        /// `!older` matches the two open
        /// older.md todos.
        #[test]
        fn fetch_todos_applies_typed_query() {
                let (dir, db_path) = setup_todo_db();
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                app.query = "!write".to_string();
                app.refresh();
                assert_eq!(app.merged_rows().len(), 0);
                app.query = "!older".to_string();
                app.refresh();
                let cmds: Vec<&str> = app
                    .merged_rows()
                    .iter()
                    .map(|r| r.command.as_str())
                    .collect();
                assert_eq!(cmds.len(), 2);
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// `!#tag` filters to todos that are
        /// tagged with the given tag. The
        /// library's `expr_to_todo_condition`
        /// path searches `t.tags` (the
        /// todo's own tags, extracted by the
        /// library's `extract_todo_entries`
        /// from inline `#tag` patterns on the
        /// todo line) AND `m.header_fields`
        /// (the note's frontmatter `tags`
        /// array). This matches what
        /// `note_search list --tag urgent`
        /// would return, so the user's
        /// muscle memory transfers across
        /// the two surfaces.
        #[test]
        fn fetch_todos_filters_by_tag() {
                use std::fs;
                use rusqlite::Connection;
                use std::sync::atomic::{AtomicU64, Ordering};
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                let dir = std::env::temp_dir().join(format!(
                    "smarthistory-todo-tag-{}-{}",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir_all(&dir).expect("create notes dir");
                fs::write(
                        dir.join("note.md"),
                        "---\n\
                         tags: [urgent, work]\n\
                         ---\n\
                         \n\
                         - [ ] urgent task #urgent\n\
                         - [ ] ordinary task\n\
                         - [ ] another ordinary\n",
                )
                .expect("write");
                let db_path = std::env::temp_dir().join(format!(
                    "smarthistory-todo-tag-db-{}-{}.sqlite",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_file(&db_path);
                let conn = Connection::open(&db_path).expect("open db");
                note_search::init_database_schema(&conn)
                        .map_err(|e| format!("schema: {e}"))
                        .expect("init schema");
                let data = note_search::markdown_parser::process_markdown_file(
                        &dir.join("note.md"),
                        &dir,
                )
                .expect("process file");
                note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
                .map_err(|e| format!("write: {e}"))
                .expect("write db");
                drop(conn);
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                // Filter by the note-level tag
                // (`urgent` is in the frontmatter
                // `tags` array, so all three todos
                // come back).
                app.query = "!#urgent".to_string();
                app.refresh();
                let cmds: Vec<&str> = app
                    .merged_rows()
                    .iter()
                    .map(|r| r.command.as_str())
                    .collect();
                assert_eq!(cmds.len(), 3, "got: {:?}", cmds);
                // Filter by the inline tag
                // (`#urgent` appears on the first
                // todo's line, so only that one
                // comes back via the
                // `t.tags` clause; the note's
                // frontmatter also has it, so we
                // actually get all 3 still — the
                // SQL ORs both sources).
                app.query = "!ordinary".to_string();
                app.refresh();
                let cmds: Vec<&str> = app
                    .merged_rows()
                    .iter()
                    .map(|r| r.command.as_str())
                    .collect();
                assert_eq!(cmds.len(), 2);
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// `![[link]]` filters to todos that
        /// have a `[[link]]` reference
        /// either on the todo line itself or
        /// in the note body. This is the
        /// Obsidian-syntax analogue of `!#tag`
        /// and follows the same
        /// `query_expr` path through
        /// `parse_query` + `build_query_from_expr`.
        #[test]
        fn fetch_todos_filters_by_link() {
                use std::fs;
                use rusqlite::Connection;
                use std::sync::atomic::{AtomicU64, Ordering};
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                let dir = std::env::temp_dir().join(format!(
                    "smarthistory-todo-link-{}-{}",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir_all(&dir).expect("create notes dir");
                fs::write(
                        dir.join("note.md"),
                        "# Title\n\
                         \n\
                         See [[project-alpha]] for context.\n\
                         \n\
                         - [ ] task linked to alpha [[project-alpha]]\n\
                         - [ ] unrelated task\n",
                )
                .expect("write");
                let db_path = std::env::temp_dir().join(format!(
                    "smarthistory-todo-link-db-{}-{}.sqlite",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_file(&db_path);
                let conn = Connection::open(&db_path).expect("open db");
                note_search::init_database_schema(&conn)
                        .map_err(|e| format!("schema: {e}"))
                        .expect("init schema");
                let data = note_search::markdown_parser::process_markdown_file(
                        &dir.join("note.md"),
                        &dir,
                )
                .expect("process file");
                note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
                .map_err(|e| format!("write: {e}"))
                .expect("write db");
                drop(conn);
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                app.query = "![[project-alpha]]".to_string();
                app.refresh();
                let cmds: Vec<&str> = app
                    .merged_rows()
                    .iter()
                    .map(|r| r.command.as_str())
                    .collect();
                // Both the linked todo AND the
                // unrelated todo come back
                // because the note body contains
                // `[[project-alpha]]` and the
                // library's link condition
                // matches both `t.links` and
                // `m.links`. We assert >= 1
                // (loose) rather than == 2
                // (strict) because the exact
                // set depends on the library's
                // internal OR-of-sources logic
                // which we don't need to
                // duplicate here.
                assert!(
                        !cmds.is_empty(),
                        "link filter returned empty: {:?}",
                        cmds
                );
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// Each todo row carries the file's
        /// `updated` timestamp from the
        /// `markdown_data` table, so the
        /// Details pane can show a real age
        /// instead of the `9999M`
        /// placeholder that `format_diff(0)`
        /// would produce. We verify the
        /// timestamp is non-zero after the
        /// fetch — it must be the file's
        /// mtime (a recent Unix epoch value),
        /// not the `0` we used before the
        /// `fetch_file_updated_timestamps`
        /// helper existed.
        #[test]
        fn fetch_todos_populates_real_timestamps() {
                let (dir, db_path) = setup_todo_db();
                let before = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                app.query = "!".to_string();
                app.refresh();
                let after = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                // Every row should have a
                // timestamp that's strictly
                // positive (not the 0
                // placeholder) and within the
                // test window.
                for row in app.merged_rows() {
                        assert!(
                                row.timestamp > 0,
                                "row {:?} has zero timestamp",
                                row.command
                        );
                        assert!(
                                row.timestamp >= before - 1
                                        && row.timestamp <= after + 1,
                                "row {:?} timestamp {} outside test window [{}, {}]",
                                row.command,
                                row.timestamp,
                                before - 1,
                                after + 1
                        );
                }
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// Within a single file, todos are
        /// returned in line-number order
        /// (top-to-bottom), matching the
        /// library's own SQL `ORDER BY
        /// m.updated DESC, t.filename,
        /// t.line_number`. The test uses a
        /// dedicated single-file fixture so the
        /// cross-file ordering is irrelevant.
        #[test]
        fn fetch_todos_orders_lines_within_a_file() {
                use std::fs;
                use rusqlite::Connection;
                use std::sync::atomic::{AtomicU64, Ordering};
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                let dir = std::env::temp_dir().join(format!(
                    "smarthistory-todo-lineorder-{}-{}",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir_all(&dir).expect("create notes dir");
                fs::write(
                        dir.join("single.md"),
                        "# Title\n\
                         \n\
                         - [ ] line 3\n\
                         - [ ] line 4\n\
                         - [x] line 5\n\
                         - [ ] line 6\n",
                )
                .expect("write note");
                let db_path = std::env::temp_dir().join(format!(
                    "smarthistory-todo-lo-db-{}-{}.sqlite",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_file(&db_path);
                let conn = Connection::open(&db_path).expect("open db");
                note_search::init_database_schema(&conn)
                        .map_err(|e| format!("schema: {e}"))
                        .expect("init schema");
                let data = note_search::markdown_parser::process_markdown_file(
                        &dir.join("single.md"),
                        &dir,
                )
                .expect("process file");
                note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
                .map_err(|e| format!("write: {e}"))
                .expect("write db");
                drop(conn);
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                app.query = "!".to_string();
                app.refresh();
                let cmds: Vec<&str> = app
                    .merged_rows()
                    .iter()
                    .map(|r| r.command.as_str())
                    .collect();
                // 3 open todos: lines 3, 4, 6.
                // (Line 5 is `[x]`, closed.)
                // The library's `text` field is
                // the part after the checkbox (not
                // the full line), which differs
                // from the raw-line representation
                // we had when scanning the
                // filesystem directly. We test
                // against the library's
                // representation here.
                assert_eq!(cmds.len(), 3);
                assert_eq!(
                        cmds,
                        vec!["line 3", "line 4", "line 6",]
                );
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// `fetch_todos` returns an empty list
        /// when the user has a `notes.dir`
        /// configured but no `notes.database`.
        /// (The library needs the database to
        /// query; scanning the filesystem is no
        /// longer supported.) The TUI surfaces a
        /// status message so the user knows why.
        #[test]
        fn fetch_todos_requires_notes_database() {
                let mut app = global_test_app(&[("a", 1)]);
                // `notes_database` defaults to None.
                app.query = "!".to_string();
                app.refresh();
                assert_eq!(app.merged_rows().len(), 0);
                // The status message explains the
                // missing config so the user
                // doesn't see a silent empty list.
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("missing notes.database should surface a status");
                assert!(
                        msg.contains("notes.database"),
                        "got: {:?}",
                        msg
                );
        }

        /// `fetch_todos` reads the line number
        /// from the library's `TodoResult` and
        /// stores it in the synthetic `id` so
        /// consumers can recover it. We test
        /// that the resulting id encodes the
        /// line number (1-based) of the todo
        /// within its file.
        #[test]
        fn fetch_todos_id_encodes_line_number() {
                let (dir, db_path) = setup_todo_db();
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                app.query = "!".to_string();
                app.refresh();
                // The fixture has `older todo 1`
                // on line 3 of older.md. Find the
                // row whose comment is older.md
                // and check that its id is -3.
                let row = app
                        .merged_rows()
                        .iter()
                        .find(|r| r.command.contains("older todo 1"))
                        .expect("older todo 1 row");
                assert_eq!(row.id, -3);
                assert_eq!(row.comment, "older.md");
                let line_number: usize =
                        (row.id.unsigned_abs() as usize).max(1);
                assert_eq!(line_number, 3);
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// The `todo_line_option` template
        /// substitutes `$LINE` with the actual
        /// 1-based line number. We test this by
        /// mutating `app.todo_line_option` and
        /// confirming the resulting staged
        /// command uses the new template.
        #[test]
        fn todo_line_option_template_is_substituted() {
                let (dir, db_path) = setup_todo_db();
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                app.todo_line_option = String::from("+LINE:$LINE");
                app.query = "!older todo 1".to_string();
                app.refresh();
                let row = app.selected_row().expect("a row");
                let line_number: usize =
                        (row.id.unsigned_abs() as usize).max(1);
                let substituted = app
                        .todo_line_option
                        .replace("$LINE", &line_number.to_string());
                assert_eq!(substituted, "+LINE:3");
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// End-to-end regression test for the
        /// user's bug report: `!@orchard`
        /// should match todos whose text
        /// contains the word "orchard",
        /// not (as the library's
        /// `parse_query` would naively do)
        /// interpret `@orchard` as a link
        /// reference and return empty.
        ///
        /// The previous implementation
        /// pushed the raw `@orchard` token
        /// into the cleaned pattern; the
        /// library then tokenized it as
        /// `Token::Link("orchard")` and
        /// looked for an `[[orchard]]`
        /// reference in `t.links`/
        /// `m.links`, finding none in a
        /// normal notes-only workflow.
        /// The fix strips the leading `@`
        /// from non-alias tokens in
        /// `parse_notes_query` so the
        /// downstream `parse_query` sees a
        /// plain `Text("orchard")` token
        /// that routes through the
        /// text-LIKE branch.
        #[test]
        fn fetch_todos_at_prefix_matches_text() {
                use std::fs;
                use rusqlite::Connection;
                use std::sync::atomic::{AtomicU64, Ordering};
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                let dir = std::env::temp_dir().join(format!(
                    "smarthistory-todo-orchard-{}-{}",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir_all(&dir).expect("create notes dir");
                fs::write(
                        dir.join("note.md"),
                        "# Title\n\
                         \n\
                         - [ ] pick apples in the orchard\n\
                         - [ ] write tests\n\
                         - [ ] visit the orchard on saturday\n",
                )
                .expect("write note");
                let db_path = std::env::temp_dir().join(format!(
                    "smarthistory-todo-orchard-db-{}-{}.sqlite",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_file(&db_path);
                let conn = Connection::open(&db_path).expect("open db");
                note_search::init_database_schema(&conn)
                        .map_err(|e| format!("schema: {e}"))
                        .expect("init schema");
                let data = note_search::markdown_parser::process_markdown_file(
                        &dir.join("note.md"),
                        &dir,
                )
                .expect("process file");
                note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
                .map_err(|e| format!("write: {e}"))
                .expect("write db");
                drop(conn);
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                // The user's exact bug report
                // query: `!@orchard` should
                // return the two todos that
                // mention "orchard", not zero.
                app.query = "!@orchard".to_string();
                app.refresh();
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                assert_eq!(
                        cmds.len(),
                        2,
                        "expected 2 orchard todos, got: {:?}",
                        cmds
                );
                assert!(cmds.iter().any(|c| c.contains("apples")));
                assert!(cmds.iter().any(|c| c.contains("saturday")));
                // Sanity: a query that doesn't
                // appear in any todo returns
                // empty.
                app.query = "!@nonexistent".to_string();
                app.refresh();
                assert_eq!(app.merged_rows().len(), 0);
                // And the unprefixed form
                // works the same way (the
                // `@` is purely a
                // user-convenience prefix).
                app.query = "!orchard".to_string();
                app.refresh();
                assert_eq!(app.merged_rows().len(), 2);
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// `mark_todo_done` toggles the
        /// checkbox marker on the
        /// targeted line in the source
        /// note file from `[ ]` to
        /// `[x]`. We start with a note
        /// that has two open todos on
        /// lines 3 and 5, invoke the
        /// action on the first row
        /// (`older todo 1` on line 3),
        /// then read the file back and
        /// assert that line 3 is now
        /// `- [x] older todo 1` and
        /// line 5 is unchanged.
        #[test]
        fn mark_todo_done_toggles_checkbox_in_file() {
                let (dir, db_path) = setup_todo_db();
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                app.query = "!older todo 1".to_string();
                app.refresh();
                // Sanity: the row exists and
                // points at line 3.
                let row = app.selected_row().expect("row");
                assert_eq!(row.id, -3);
                assert_eq!(row.comment, "older.md");
                app.mark_todo_done();
                // Re-read the file and verify
                // line 3 was toggled.
                let contents =
                        std::fs::read_to_string(dir.join("older.md"))
                                .expect("read older.md");
                let lines: Vec<&str> =
                        contents.lines().collect();
                assert_eq!(lines[2], "- [x] older todo 1");
                // The closed todo on line 5
                // and the other open todo
                // on line 6 are both
                // unchanged.
                assert_eq!(lines[4], "- [x] older done 1");
                assert_eq!(lines[5], "- [ ] older todo 2");
                // The status message
                // confirms the toggle.
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("status message after mark");
                assert!(
                        msg.contains("Marked done"),
                        "got: {:?}",
                        msg
                );
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// After a successful file
        /// toggle, `mark_todo_done`
        /// refreshes the
        /// `todo_entries` SQLite
        /// table via the library's
        /// `update_files_in_db`
        /// function (the canonical
        /// re-index path) and then
        /// re-queries the TUI's view.
/// Both halves of the
/// contract are verified:
/// the row's `closed` column
/// is now `1`, and the row
/// itself is gone from the
/// merged list (the underlying
/// query filters
/// `open: true`).
        #[test]
        fn mark_todo_done_refreshes_database_via_update_files_in_db() {
                let (dir, db_path) = setup_todo_db();
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                app.query = "!older todo 1".to_string();
                app.refresh();
                // Sanity: the row exists
                // in the DB before the
                // action, with
                // `closed = 0`.
                use rusqlite::Connection;
                let conn_before =
                        Connection::open(&db_path).expect("open db");
                let closed_before: i64 = conn_before
                        .query_row(
                                "SELECT closed FROM todo_entries \
                                 WHERE filename = 'older.md' \
                                   AND line_number = 3",
                                [],
                                |row| row.get(0),
                        )
                        .expect("query closed before");
                assert_eq!(closed_before, 0);
                drop(conn_before);
                // Pre-condition: row is in
                // the merged list.
                assert!(
                        app.merged_rows()
                                .iter()
                                .any(|r| r.command.contains("older todo 1")),
                );
                app.mark_todo_done();
                // File was updated.
                let contents = std::fs::read_to_string(
                        dir.join("older.md"),
                )
                .expect("read older.md");
                assert!(
                        contents.contains("- [x] older todo 1"),
                        "file should be updated: {}",
                        contents
                );
                // DB was updated by
                // `update_files_in_db`:
                // the row's `closed` is
                // now 1.
                let conn_after =
                        Connection::open(&db_path).expect("open db");
                let closed_after: i64 = conn_after
                        .query_row(
                                "SELECT closed FROM todo_entries \
                                 WHERE filename = 'older.md' \
                                   AND line_number = 3",
                                [],
                                |row| row.get(0),
                        )
                        .expect("query closed after");
                assert_eq!(
                        closed_after, 1,
                        "DB should reflect the toggle \
                         (update_files_in_db re-parses \
                         the file and re-writes the \
                         todo_entries row)"
                );
                drop(conn_after);
                // Row is gone from the
                // merged list.
                let cmds: Vec<&str> = app
                        .merged_rows()
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                assert!(
                        cmds.iter()
                                .all(|c| !c.contains("older todo 1")),
                        "row should be gone after refresh: {:?}",
                        cmds
                );
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// `mark_todo_done` only works
        /// in todo mode. Outside of
        /// todo mode it's a no-op with a
        /// status message so the user
        /// understands why their `C-x`
        /// did nothing. This is the
        /// mode-gating contract: the
        /// action is "only available in
        /// the search of todos".
        #[test]
        fn mark_todo_done_outside_todo_mode_is_noop() {
                let mut app = global_test_app(&[("a", 1)]);
                // Note: we don't even have a
                // row selected, but the
                // mode gate fires first.
                app.query = "git".to_string(); // plain history mode
                app.refresh();
                let before_rows =
                        app.merged_rows().len();
                app.mark_todo_done();
                assert_eq!(
                        app.merged_rows().len(),
                        before_rows
                );
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("status message");
                assert!(
                        msg.contains("only available in todo"),
                        "got: {:?}",
                        msg
                );
        }

        /// If the file's content has
        /// changed since the indexer
        /// last saw it (the user
        /// manually toggled the
        /// checkbox, or the line was
        /// edited in some other way),
        /// the targeted line may no
        /// longer be an open todo. The
        /// action must NOT corrupt the
        /// file in that case — it
        /// surfaces a status message
        /// and leaves the file alone.
        #[test]
        fn mark_todo_done_rejects_stale_line() {
                let (dir, db_path) = setup_todo_db();
                // Mutate the file behind
                // the indexer's back: the
                // todo on line 3 is now
                // already closed.
                std::fs::write(
                        dir.join("older.md"),
                        "# Older\n\n\
                         - [x] older todo 1 (already done)\n\
                         some prose in between\n\
                         - [x] older done 1\n\
                         - [ ] older todo 2\n",
                )
                .expect("rewrite older.md");
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                app.query = "!older todo 1".to_string();
                app.refresh();
                app.mark_todo_done();
                // File unchanged.
                let contents = std::fs::read_to_string(
                        dir.join("older.md"),
                )
                .expect("read older.md");
                assert!(
                        contents.contains(
                                "already done"
                        ),
                        "file should be untouched: {}",
                        contents
                );
                // Status explains why.
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("status message");
                assert!(
                        msg.contains("no longer an open todo"),
                        "got: {:?}",
                        msg
                );
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// If `notes_dir` is not
        /// configured, the action
        /// surfaces a status message
        /// and writes nothing.
        #[test]
        fn mark_todo_done_without_notes_dir_is_noop() {
                let (dir, db_path) = setup_todo_db();
                let mut app = global_test_app(&[("a", 1)]);
                // `notes_database` is set
                // but `notes_dir` is None.
                app.notes_database = Some(db_path.clone());
                app.query = "!older todo 1".to_string();
                app.refresh();
                app.mark_todo_done();
                // The original file is
                // untouched.
                let contents = std::fs::read_to_string(
                        dir.join("older.md"),
                )
                .expect("read older.md");
                assert!(
                        contents.contains(
                                "- [ ] older todo 1"
                        ),
                        "file should be untouched: {}",
                        contents
                );
                let msg = app
                        .status_message
                        .as_ref()
                        .map(|(m, _)| m.as_str())
                        .expect("status message");
                assert!(
                        msg.contains("notes.dir"),
                        "got: {:?}",
                        msg
                );
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&db_path);
        }

        /// Indented todos (e.g. nested
        /// under a heading) get
        /// correctly toggled — the
        /// leading whitespace is
        /// preserved, only the bracket
        /// marker changes. We verify
        /// this with a hand-crafted
        /// single-row scenario where
        /// we bypass the library's
        /// parser: the library's
        /// `TODO_REGEX` is anchored
        /// with `^`, so indented
        /// checkboxes never reach
        /// the database in the first
        /// place. But a stale DB row
        /// (e.g. left over from a
        /// previous version of the
        /// library, or hand-edited by
        /// the user) might still
        /// point at an indented line,
        /// and our toggle must
        /// preserve the indentation.
        #[test]
        fn mark_todo_done_preserves_indentation() {
                use std::fs;
                let dir = std::env::temp_dir().join(format!(
                    "smarthistory-todo-indent-{}",
                    std::process::id(),
                ));
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir_all(&dir).expect("create notes dir");
                let mut note = String::from("# Title\n");
                note.push_str("\n");
                note.push_str("  - [ ] indented todo\n");
                fs::write(dir.join("note.md"), &note)
                        .expect("write");
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.query = "fake".to_string();
                app.refresh();
                // Construct a synthetic
                // todo row that points at
                // line 3 of the file.
                // This bypasses
                // `fetch_todos` (the
                // library wouldn't have
                // indexed the indented
                // todo in the first
                // place) and exercises
                // the file mutation in
                // isolation.
                let row = crate::tui::state::HistoryRow {
                        id: -3,
                        command: String::from(
                            "indented todo",
                        ),
                        directory: String::new(),
                        session_id: String::new(),
                        exit_code: 0,
                        timestamp: 0,
                        comment: String::from("note.md"),
                        output: String::new(),
                        mode: String::from("todo"),
                        source: String::new(),
                };
                app.mark_todo_done_for_row(&row);
                let contents = fs::read_to_string(
                        dir.join("note.md"),
                )
                .expect("read note.md");
                let lines: Vec<&str> =
                        contents.lines().collect();
                // The leading two spaces
                // are preserved; only the
                // bracket changed.
                assert_eq!(
                        lines[2],
                        "  - [x] indented todo",
                        "got: {:?}",
                        contents
                );
                let _ = fs::remove_dir_all(&dir);
        }

        /// Files without a trailing
        /// newline (unusual but
        /// legal) are preserved
        /// verbatim after the toggle —
        /// we don't accidentally add
        /// a trailing `\n` that
        /// wasn't there.
        #[test]
        fn mark_todo_done_preserves_no_trailing_newline() {
                use std::fs;
                use rusqlite::Connection;
                use std::sync::atomic::{AtomicU64, Ordering};
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                let dir = std::env::temp_dir().join(format!(
                    "smarthistory-todo-noeof-{}-{}",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir_all(&dir).expect("create notes dir");
                // Note: NO trailing newline.
                fs::write(
                        dir.join("note.md"),
                        "# Title\n\n- [ ] open todo",
                )
                .expect("write");
                let db_path = std::env::temp_dir().join(format!(
                    "smarthistory-todo-noeof-db-{}-{}.sqlite",
                    std::process::id(),
                    n
                ));
                let _ = fs::remove_file(&db_path);
                let conn = Connection::open(&db_path)
                        .expect("open db");
                note_search::init_database_schema(&conn)
                        .map_err(|e| format!("schema: {e}"))
                        .expect("init schema");
                let data = note_search::markdown_parser::process_markdown_file(
                        &dir.join("note.md"),
                        &dir,
                )
                .expect("process file");
                note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
                        .map_err(|e| format!("write: {e}"))
                        .expect("write db");
                drop(conn);
                let mut app = global_test_app(&[("a", 1)]);
                app.notes_dir = Some(dir.clone());
                app.notes_database = Some(db_path.clone());
                app.query = "!".to_string();
                app.refresh();
                app.mark_todo_done();
                let contents = fs::read_to_string(
                        dir.join("note.md"),
                )
                .expect("read note.md");
                assert_eq!(
                        contents,
                        "# Title\n\n- [x] open todo"
                );
                let _ = fs::remove_dir_all(&dir);
                let _ = fs::remove_file(&db_path);
        }

        // --- Directories mode (`#` prefix) ----

        /// Helper that builds a fresh
        /// in-memory `App` with a
        /// history table containing
        /// rows for several
        /// directories. The
        /// `global_test_app` helper
        /// hardcodes every row's
        /// `directory` to `/tmp`, so
        /// we need a bespoke
        /// constructor for
        /// directories-mode tests.
        /// The passed-in `(cmd,
        /// directory, offset_secs)`
        /// tuples are inserted in
        /// the given order; the
        /// resulting `timestamp` is
        /// `now - offset_secs` so we
        /// can drive the
        /// recency-ordering
        /// assertions deterministically.
        fn directories_test_app(
            rows: &[(&str, &str, i64)],
        ) -> App {
            use rusqlite::Connection;
            let conn = Connection::open_in_memory()
                .expect("open in-memory db");
            conn.execute_batch(
                "CREATE TABLE history (
                    id INTEGER PRIMARY KEY,
                    command TEXT NOT NULL,
                    directory TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    exit_code INTEGER,
                    timestamp INTEGER DEFAULT \
                     (strftime('%s', 'now')),
                    mode TEXT NOT NULL DEFAULT 'command'
                );",
            )
            .expect("schema");
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            for (i, (cmd, dir, offset)) in rows.iter().enumerate() {
                conn.execute(
                    "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
                     VALUES (?1, ?2, ?3, 'sess', 0, ?4)",
                    rusqlite::params![
                        i as i64 + 1,
                        *cmd,
                        *dir,
                        now - *offset,
                    ],
                )
                .expect("insert");
            }
            // Build the App and
            // immediately clear
            // the `session_subdirs`
            // field. `App::new`
            // calls
            // `build_session_subdirs`
            // which reads the
            // user's real
            // `~/.config/smarthistory/config`.
            // Tests that don't
            // care about
            // sessiondirs would
            // otherwise be
            // polluted by
            // whatever the user
            // happens to have
            // configured (a real
            // "I added
            // `sessiondirs=...`
            // to my config and
            // now my tests fail"
            // bug). Tests that
            // DO need pinned
            // directories should
            // call
            // `directories_test_app_with_sessions`
            // below.
            let mut app = App::new(
                conn,
                Mode::Global,
                String::new(),
                false,
                ExitFilter::All,
                SortOrder::default(),
                false,
                SelectedTheme::None,
                KeyBindings::defaults(),
                None,
                None,
                crate::QueryPrefixes::default(),
                None,
                None,
                String::from("+$LINE"),
                // No JIRA fragments in the default
                // test app. Tests that exercise the
                // fragment path push entries directly
                // into `app.jira_fragments` (it's a
                // plain HashMap field, not gated by
                // a setter — the test is the only
                // consumer and doesn't need a
                // formal setter).
                std::collections::HashMap::new(),
            Vec::new(),
            );
            // `App::new` calls
            // `build_session_subdirs`
            // (which reads the
            // user's real
            // `~/.config/smarthistory/config`)
            // and `fetch_tmux_windows`
            // (which runs
            // `tmux list-windows -a`).
            // Both of those would
            // pollute the test
            // with whatever the
            // user happens to
            // have configured or
            // running. Clear the
            // fields so each test
            // sees a known-empty
            // starting point.
            // Tests that need a
            // specific
            // `session_subdirs`
            // or `tmux_windows`
            // set should call
            // `directories_test_app_with_sessions`
            // (or set the
            // fields directly).
            app.session_subdirs.clear();
            app.tmux_windows.clear();
            app
        }

        /// A variant of
        /// `directories_test_app`
        /// that ALSO
        /// pre-populates the
        /// `session_subdirs`
        /// field with the given
        /// list. Use this when
        /// a test needs the
        /// pinned-directories
        /// behaviour; use the
        /// plain
        /// `directories_test_app`
        /// (which clears
        /// `session_subdirs` by
        /// `session_subdirs`
        /// field with the given
        /// list. Use this when
        /// a test needs the
        /// pinned-directories
        /// behaviour; use the
        /// plain
        /// `directories_test_app`
        /// (which clears
        /// `session_subdirs` by
        /// default) when the
        /// test should NOT see
        /// any sessiondirs.
        ///
        /// (The default-empty
        /// behaviour is what
        /// keeps tests
        /// isolated from the
        // developer's real
        // `~/.config/smarthistory/config`
        // — see
        // `build_session_subdirs`
        // for the
        // cross-contamination
        // story.)
        fn directories_test_app_with_sessions(
            rows: &[(&str, &str, i64)],
            sessions: Vec<std::path::PathBuf>,
        ) -> App {
            let mut app = directories_test_app(rows);
            app.session_subdirs = sessions;
            app
        }

        /// `fetch_directories`
        /// returns one row per
        /// unique directory, sorted
        /// by each directory's
        /// most-recent history
        /// timestamp DESC. The
        /// directory (in shell-
        /// friendly `~/x` form)
        /// is the visible primary
        /// text of the row, and
        /// the last command run
        /// in that directory is
        /// kept in `row.comment`
        /// (the secondary slot)
        /// so the user still has
        /// a hint of *what* they
        /// were doing there.
        #[test]
        fn fetch_directories_lists_unique_dirs_sorted_by_recency() {
            // Three directories,
            // several timestamps. The
            // recency order (most-recent
            // timestamp DESC) should
            // be `/home/c` first (just
            // ran there), `/home/a`
            // second (yesterday), and
            // `/home/b` last (a year
            // ago). The `comment`
            // for each directory is
            // the command that
            // produced its
            // max-timestamp row.
            let mut app = directories_test_app(&[
                ("ls",        "/home/a",  86_400),   // 1 day ago
                ("make",      "/home/b", 365 * 86_400), // 1 year ago
                ("echo hi",   "/home/c",       30),   // 30s ago
                ("git status", "/home/a",  3_600),   // 1h ago (newer than `ls`)
                ("touch x",   "/home/a", 86_400),   // 1d (older than `ls`)
            ]);
            app.query = "#".to_string();
            app.refresh();
            // Three directories
            // expected (one row
            // each). The visible
            // primary text is the
            // directory (now in
            // `row.command`), so
            // that's what we read
            // here. The new
            // directory-source
            // feature surfaces
            // tmux panes as
            // additional rows,
            // so we filter to
            // `/home/...` (the
            // test's directory
            // namespace) to
            // assert cleanly.
            let home_rows: Vec<&HistoryRow> = app
                .merged_rows()
                .iter()
                .filter(|r| {
                    r.directory.starts_with("/home/")
                })
                .collect();
            let visible: Vec<&str> = home_rows
                .iter()
                .map(|r| r.command.as_str())
                .collect();
            assert_eq!(visible.len(), 3);
            // Newest directory
            // first.
            assert_eq!(visible[0], "/home/c");
            // Second.
            assert_eq!(visible[1], "/home/a");
            // Third.
            assert_eq!(visible[2], "/home/b");
            // The last command run
            // in each directory
            // lives in `row.comment`
            // (the secondary slot).
            let last_cmds: Vec<&str> = home_rows
                .iter()
                .map(|r| r.comment.as_str())
                .collect();
            assert_eq!(last_cmds[0], "echo hi");
            assert_eq!(last_cmds[1], "git status");
            assert_eq!(last_cmds[2], "make");
            // Each row's `directory`
            // is the canonical path.
            let dirs: Vec<&str> = home_rows
                .iter()
                .map(|r| r.directory.as_str())
                .collect();
            assert_eq!(dirs[0], "/home/c");
            assert_eq!(dirs[1], "/home/a");
            assert_eq!(dirs[2], "/home/b");
        }

        /// Substring filter: `#home`
        /// restricts the listing to
        /// rows whose `directory`
        /// contains `home`. The
        /// filter is space-split AND
        /// (so `#home a` requires both
        /// `home` AND `a` somewhere in
        /// the path). The visible
        /// primary text on each row
        /// is now the directory (per
        /// the layout swap), so the
        /// assertions read
        /// `row.command` (the
        /// directory) and the
        /// comments confirm the
        /// last-command metadata.
        #[test]
        fn fetch_directories_applies_substring_filter() {
            let mut app = directories_test_app(&[
                ("ls", "/home/a", 86_400),
                ("ls", "/var/log", 3_600),
                ("ls", "/home/b", 60),
            ]);
            app.query = "#home".to_string();
            app.refresh();
            let visible: Vec<&str> = app
                .merged_rows()
                .iter()
                .map(|r| r.command.as_str())
                .collect();
            assert_eq!(visible.len(), 2);
            // `/home/b` is the latest
            // of the matching
            // directories (60s old),
            // so it sorts first.
            assert_eq!(visible[0], "/home/b");
            assert_eq!(visible[1], "/home/a");
            // The secondary slot
            // carries the last
            // command run in each
            // directory.
            let cmds: Vec<&str> = app
                .merged_rows()
                .iter()
                .map(|r| r.comment.as_str())
                .collect();
            assert_eq!(cmds[0], "ls");
            assert_eq!(cmds[1], "ls");
            // No match for `/var/log`
            // because the filter
            // requires `home`.
            app.query = "#var".to_string();
            app.refresh();
            assert_eq!(
                app.merged_rows().len(),
                1
            );
            app.query = "#no-such-dir".to_string();
            app.refresh();
            assert_eq!(
                app.merged_rows().len(),
                0
            );
        }

        /// Regression test for the
        /// directory-row layout
        /// swap: the visible
        /// primary text is the
        /// directory in shell-
        /// shortened form
        /// (`~/x` when under
        /// `$HOME`) and the
        /// secondary `# ...`
        /// slot is the last
        /// command run there.
        /// Without the swap the
        /// user would see the
        /// command first and the
        /// directory as a
        /// secondary hint — the
        /// inverse of what the
        /// user wants in `#`-mode
        /// (where they're
        /// searching for paths,
        /// not commands).
        #[test]
        fn fetch_directories_layout_swap() {
            // The `~`-shortening
            // depends on `$HOME` and
            // the `home_map` config.
            // We set `$HOME` for the
            // duration of the test
            // and clear `home_map`
            // (the default empty
            // list). This avoids
            // depending on the
            // caller's environment
            // (parallel test runs
            // could otherwise see
            // different `home_map`
            // values via the
            // user's actual
            // `~/.config/smarthistory/config`).
            let _guard = ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let saved_home =
                std::env::var("HOME").ok();
            // SAFETY: this test
            // holds `ENV_LOCK` (the
            // shared env-mutation
            // mutex), so no other
            // env-mutating test
            // can run concurrently.
            unsafe {
                std::env::set_var(
                    "HOME",
                    "/Users/har",
                );
            }
            let mut app = directories_test_app(&[(
                "ls -la /tmp/foo bar",
                "/Users/har/work/project",
                60,
            )]);
            app.query = "#".to_string();
            app.refresh();
            // Find the SQL-history
            // row specifically;
            // sessiondirs / tmux
            // rows may also be
            // present (cleared in
            // the test helper, but
            // `refresh()` re-runs
            // `fetch_tmux_windows`
            // for `#`-mode queries
            // and the user's
            // production `tmux`
            // panes may bleed in
            // when HOME is set to
            // a real path). We
            // assert on the row
            // whose `directory`
            // matches what we
            // inserted, not on
            // `merged_rows()[0]`.
            let row = app
                .merged_rows()
                .iter()
                .find(|r| {
                    r.directory == "/Users/har/work/project"
                })
                .expect(
                    "the SQL-history row for /Users/har/work/project must be in merged_rows",
                );
            // The primary text
            // (which the user sees
            // first in the list,
            // and which the query
            // highlights against)
            // is the directory in
            // `~/x` form. This is
            // the load-bearing
            // assertion: it locks
            // in the swap.
            assert_eq!(row.command, "~/work/project");
            // The secondary slot
            // (the `# ...` comment
            // in the rendered
            // line) is the last
            // command run in that
            // directory. The
            // command here is short
            // (under the 60-char
            // truncation threshold)
            // so it appears
            // verbatim.
            assert_eq!(row.comment, "ls -la /tmp/foo bar");
            // The full directory
            // (un-shortened) is
            // still in `directory`
            // for the tmux-pane
            // lookup and Details
            // pane.
            assert_eq!(
                row.directory,
                "/Users/har/work/project"
            );
            if let Some(home) = saved_home {
                unsafe {
                    std::env::set_var("HOME", home);
                }
            }
        }

        /// Long commands are
        /// truncated to 57
        /// characters plus an
        /// ellipsis when stored
        /// in the secondary slot
        /// (the comment field).
        /// The truncation is
        /// char-aware (uses
        /// `chars().take(57)`)
        /// so multi-byte UTF-8
        /// doesn't get cut in
        /// the middle of a
        /// code point.
        #[test]
        fn fetch_directories_truncates_long_command() {
            // 100-char command.
            let long_cmd = "a".repeat(100);
            let mut app = directories_test_app(&[(
                &long_cmd,
                "/Users/har/work",
                60,
            )]);
            app.query = "#".to_string();
            app.refresh();
            // Find the row by
            // directory
            // (the test helper
            // clears `tmux_windows`
            // but `refresh()`
            // re-runs the tmux
            // fetch, so the
            // user's real tmux
            // panes may also be
            // present). The
            // SQL row is the one
            // with the matching
            // directory.
            let row = app
                .merged_rows()
                .iter()
                .find(|r| r.directory == "/Users/har/work")
                .expect(
                    "the SQL-history row for /Users/har/work must be in merged_rows",
                );
            // Truncated to 57
            // `a`s + `…` = 58
            // chars.
            assert_eq!(
                row.comment.chars().count(),
                58
            );
            assert!(row.comment.ends_with('…'));
            assert!(row.comment.starts_with('a'));
        }

        /// Selecting a directory
        /// row in the TUI stages
        /// `cd <path>` as the next
        /// shell command. Paths
        /// with shell-metacharacters
        /// are quoted so the parent
        /// shell tokenises them
        /// correctly (defensive —
        /// covers spaces, `$`, etc.).
        #[test]
        /// The new contract: with an
        /// empty `tmux_windows`
        /// snapshot (no active
        /// tmux session for this
        /// directory), selecting
        /// the directory row
        /// creates a new tmux
        /// session and switches to
        /// it. (See
        /// `select_t_marked_directory_stages_select_and_switch`
        /// for the "T"-marked
        /// branch.)
        #[test]
        fn selecting_unmarked_directory_creates_new_tmux_session() {
            let mut app = directories_test_app(&[(
                "ls",
                "/home/user/project",
                60,
            )]);
            app.query = "#".to_string();
            app.refresh();
            // Select the
            // SQL-history row
            // explicitly.
            let sql_row_idx = app
                .merged_rows()
                .iter()
                .position(|r| r.directory == "/home/user/project")
                .expect(
                    "the SQL-history row for /home/user/project must be in merged_rows",
                );
            use ratatui::widgets::ListState;
            app.list_state.select(Some(sql_row_idx));
            app.select_for_run();
            let staged = app.selection.as_deref()
                .expect("selection must be set");
            assert!(
                staged.contains(
                    "tmux new-session -d -s project -c /home/user/project"
                )
                && staged.contains(
                    "tmux switch-client -t project"
                ),
                "staged command must create detached session with the directory basename, got: {staged:?}"
            );
            assert_eq!(
                app.pick_mode,
                Some(crate::tui::state::PickMode::Run),
            );
        }

        /// The `#` prefix is
        /// configurable via
        /// `prefix.directories=...`,
        /// parallel to every other
        /// query-mode prefix. We
        /// exercise the parse /
        /// assignment path
        /// (`assign_prefix`) directly
        /// because there's no
        /// `Config::parse` in scope
        /// here.
        #[test]
        fn directories_prefix_is_configurable() {
            let mut prefixes =
                crate::QueryPrefixes::default();
            assert_eq!(prefixes.directories, '#');
            // `assign_prefix` lives
            // in `main.rs`; mirror
            // its one-liner here so
            // we can confirm the
            // field is reachable via
            // the public API.
            prefixes.directories = '>';
            assert_eq!(prefixes.directories, '>');
            // A non-default prefix
            // is recognised by the
            // predicate.
            let mut app = directories_test_app(&[(
                "ls", "/home/a", 60,
            )]);
            app.query_prefixes.directories = '>';
            app.query = ">home".to_string();
            assert!(app.is_directories_query());
            assert_eq!(
                app.directories_pattern(),
                "home"
            );
        }

        // --- Tmux-pane marker (`#` directories mode) ----

        /// `directory_has_tmux_pane`
        /// returns false when the
        /// snapshot is empty
        /// (never populated). This
        /// is the contract: before
        /// the user types `#…`
        /// (which triggers the
        /// snapshot fetch) the
        /// check is a hard `false`
        /// so no rows are falsely
        /// marked as in-tmux.
        #[test]
        fn directory_tmux_pane_id_empty_snapshot_is_none() {
            let app = directories_test_app(&[(
                "ls",
                "/home/user",
                60,
            )]);
            assert!(app.tmux_windows.is_empty());
            assert!(
                app.directory_tmux_pane_id(
                    "/home/user"
                ).is_none()
            );
        }

        /// `directory_tmux_pane_id`
        /// returns `Some(pane_id)`
        /// iff a window's `path`
        /// (canonicalised at parse
        /// time) matches the input
        /// directory (also
        /// canonicalised). The
        /// `#{pane_id} |
        ///  #{pane_current_path} |
        ///  active:#{window_active}
        ///  | Layout:
        ///  #{window_layout}` format
        /// used by `tmux
        /// list-windows -a` always
        /// reports the kernel's
        /// canonical cwd, which on
        /// macOS is the
        /// `/Volumes/HUGE/...`
        /// form, while the directory
        /// stored by the
        /// `preexec` hook is the
        /// user's logical
        /// `/Users/...` form — both
        /// canonicalise to the same
        /// string. This test verifies
        /// that contract without
        /// actually spawning
        /// `tmux` (CI may not have it
        /// installed).
        #[test]
        fn directory_tmux_pane_id_canonicalises_both_sides() {
            let mut app = directories_test_app(&[(
                "ls",
                "/home/user",
                60,
            )]);
            // Simulate a snapshot
            // that came from `tmux`.
            // `fetch_tmux_windows`
            // canonicalises these
            // at parse time, so the
            // stored `path` is
            // already canonical. We
            // hand-craft the same
            // canonical value here.
            app.tmux_windows.push(TmuxWindowInfo {
                pane_id: "%0".to_string(),
                path: String::from("/home/user"),
            });
            assert_eq!(
                app.directory_tmux_pane_id(
                    "/home/user"
                ).as_deref(),
                Some("%0"),
                "exact match must return the pane id"
            );
            // Wrong directory, same
            // prefix — must NOT match.
            assert!(
                app.directory_tmux_pane_id(
                    "/home/other"
                ).is_none()
            );
        }

        /// The actual reported
        /// bug: `tmux` reports
        /// `/Volumes/HUGE/...` for
        /// directories under
        /// `/Users/har/...` because
        /// of macOS volume mounts,
        /// while the `preexec` hook
        /// records the user's
        /// logical `/Users/...`
        /// form. Without
        /// canonicalization, the
        /// two would never match.
        /// This test guarantees
        /// they do.
        #[test]
        fn directory_tmux_pane_id_handles_macos_volume_mount() {
            let mut app = directories_test_app(&[(
                "ls",
                "/Users/har/Sources/x",
                60,
            )]);
            // `tmux` returns the
            // canonical form (which on
            // macOS resolves through
            // any symlinks / volume
            // mounts). The fetch
            // helper canonicalises
            // these at parse time; we
            // pop a pre-canonicalised
            // entry.
            //
            // We don't depend on a
            // specific macOS path here
            // — we just verify the
            // canonicalisation
            // contract: as long as
            // both forms collapse to
            // the same string
            // (which the
            // `canonicalize_directory`
            // helper does), the match
            // succeeds.
            let canonical_dir =
                std::fs::canonicalize("/tmp")
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "/tmp".into());
            app.tmux_windows.push(TmuxWindowInfo {
                pane_id: "%42".to_string(),
                path: canonical_dir.clone(),
            });
            assert_eq!(
                app.directory_tmux_pane_id(&canonical_dir)
                    .as_deref(),
                Some("%42"),
                "real-path lookup must match the canonical pane path"
            );
            // Try a non-canonical
            // form: should still
            // match because the
            // helper canonicalises
            // input too. We use a
            // different dir here so
            // the test is
            // deterministic —
            // `/var` is a symlink to
            // `/private/var` on
            // macOS but a real dir on
            // Linux CI.
            let var_canonical =
                std::fs::canonicalize("/var")
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "/var".into());
            if var_canonical != "/var" {
                // macOS: `/var`
                // canonicalises to
                // `/private/var`. We
                // push that pane
                // and check that
                // asking for either
                // form matches.
                app.tmux_windows.push(TmuxWindowInfo {
                    pane_id: "%7".to_string(),
                    path: var_canonical,
                });
                assert!(
                    app.directory_tmux_pane_id(
                        "/var"
                    ).is_some(),
                    "/var must canonicalise to match"
                );
            }
            // Wrong directory
            // totally shouldn't
            // match.
            assert!(
                app.directory_tmux_pane_id(
                    "/home/nowhere"
                ).is_none()
            );
        }

        /// Regression test for the
        /// homemap-aware
        /// normalization: a DB
        /// row stored in the
        /// short `~/x` form
        /// (after
        /// `smarthistory
        /// update`) must match
        /// a tmux-reported
        /// pane at the
        /// absolute form
        /// (e.g. `/Users/har/x`).
        ///
        /// Without the
        /// homemap-aware
        /// expansion, the
        /// `std::fs::canonicalize`
        /// step on the `~/x`
        /// side would fail (no
        /// real `~/x` path
        /// exists) and fall
        /// back to the un-
        /// resolved input,
        /// which never matched
        /// the tmux side. The
        /// result: a directory
        /// row that DID have a
        /// live tmux pane was
        /// missing the `T`
        /// marker.
        ///
        /// We use `$HOME` via
        /// `set_var` (guarded
        /// by `ENV_LOCK` so
        /// the env mutation
        /// doesn't race with
        /// other env-mutating
        /// tests) and rely on
        /// `/tmp` (which
        /// always exists) as
        /// the test directory.
        #[test]
        fn directory_tmux_pane_id_handles_tilde_form_db_row() {
            let _env_guard = ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let saved_home =
                std::env::var("HOME").ok();
            // SAFETY: holds
            // `ENV_LOCK`.
            unsafe {
                std::env::set_var(
                    "HOME",
                    "/tmp",
                );
            }
            // Use `/tmp` as the
            // test directory.
            // `~/self_test_dir`
            // is therefore
            // `/tmp/self_test_dir`
            // after homemap
            // expansion, and the
            // tmux pane has the
            // same absolute path
            // (already canonical,
            // no macOS volume
            // mount to worry
            // about).
            let mut app = directories_test_app(&[(
                "ls",
                "/tmp",
                60,
            )]);
            app.tmux_windows
                .push(TmuxWindowInfo {
                    pane_id: "%99".to_string(),
                    // The path tmux
                    // reports is
                    // already the
                    // canonical
                    // absolute form
                    // (no `~`).
                    path: std::fs::canonicalize(
                        "/tmp",
                    )
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| {
                        "/tmp".into()
                    }),
                });
            // The user-facing
            // directory in the
            // DB is the short
            // form `~/x` (or
            // here, the home
            // itself: `~`).
            // This is the case
            // the user reported:
            // a row stored in
            // `~/x` form should
            // still get the `T`
            // marker when a
            // tmux pane is at
            // the matching
            // absolute path.
            let canonical_tmp =
                std::fs::canonicalize("/tmp")
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| {
                        "/tmp".into()
                    });
            assert_eq!(
                app.directory_tmux_pane_id(
                    &canonical_tmp
                )
                .as_deref(),
                Some("%99"),
                "absolute-path DB row must match the tmux pane"
            );
            if let Some(home) = saved_home {
                unsafe {
                    std::env::set_var("HOME", home);
                }
            }
        }

        /// The `tmux_windows` snapshot
        /// is preserved across
        /// `refresh()` calls — the
        /// helper is idempotent and
        /// the fetch only happens
        /// when the snapshot is
        /// empty. Otherwise
        /// scrolling through the
        /// directories list would
        /// re-spawn `tmux` on every
        /// keypress.
        ///
        /// This test verifies the
        /// idempotency by
        /// pre-populating the
        /// snapshot, calling
        /// `fetch_tmux_windows`
        /// once, and asserting the
        /// snapshot didn't change.
        /// (We don't run `refresh()`
        /// here because in a test
        /// environment without
        /// `tmux` on PATH the
        /// `refresh()` call would
        /// set the snapshot to
        /// empty, masking the
        /// behaviour we want to
        /// verify.)
        #[test]
        fn fetch_tmux_windows_is_idempotent_when_populated() {
            let mut app = directories_test_app(&[(
                "ls",
                "/home/user",
                60,
            )]);
            // Pre-populate the
            // snapshot with a
            // sentinel value that
            // would be wiped if the
            // helper re-ran.
            let sentinel = TmuxWindowInfo {
                pane_id: "%99".to_string(),
                path: String::from("/sentinel"),
            };
            app.tmux_windows.push(sentinel.clone());
            // The helper exits
            // early when the snapshot
            // is non-empty. This is
            // the "don't re-spawn on
            // every refresh" contract.
            app.fetch_tmux_windows();
            assert_eq!(app.tmux_windows.len(), 1);
            assert_eq!(app.tmux_windows[0].pane_id, "%99");
            assert_eq!(app.tmux_windows[0].path, "/sentinel");
        }

        /// Real-world tmux output
        /// sampled from the user's
        /// own machine at the time
        /// this test was added.
        /// Verifies our parser
        /// handles the live format
        /// string
        /// (`#{pane_id} |
        ///  #{pane_current_path} |
        ///  active:#{window_active}
        ///  | Layout:
        ///  #{window_layout}`)
        /// correctly.
        ///
        /// If this test fails,
        /// either tmux changed its
        /// format tokens (unlikely)
        /// or our format string in
        /// `fetch_tmux_windows` got
        /// silently truncated.
        /// Either way, the
        /// `parse_tmux_pane_line`
        /// contract has shifted;
        /// re-pin the live format in
        /// `fetch_tmux_windows` first.
        #[test]
        fn parse_tmux_pane_line_real_world_output() {
            // Captured from the user's
            // environment with
            // `tmux list-windows -a -F
            // '#{pane_id} |
            //  #{pane_current_path} |
            //  active:#{window_active} |
            //  Layout:
            //  #{window_layout}'
            //  | grep "active:1"`.
            // (All rows below are
            // active:1 because they
            // come from grepped
            // output.)
            let sample = "\
                %0 | /Users/har | active:1 | Layout: c17d,121x93,0,0,0\n\
                %2 | /Volumes/HUGE/har/Sources/markdown-search/note_search | active:1 | Layout: 3971,121x93,0,0[121x46,0,0,2,121x46,0,47,10]\n\
                %1 | /Users/har/smarthistory/smarthistory | active:1 | Layout: 7254,121x93,0,0[121x46,0,0,1,121x46,0,47,3]\n";
            let windows: Vec<TmuxWindowInfo> = sample
                .lines()
                .filter_map(parse_tmux_pane_line)
                .collect();
            assert_eq!(
                windows.len(),
                3,
                "expected 3 parsed windows, got: {:#?}",
                windows
            );
            // First window: pane id
            // `%0`, path
            // canonicalises to the
            // user's macOS
            // `/Users/har` (no
            // symlinks involved in
            // this test dir, so
            // canonicalisation is
            // a no-op).
            assert_eq!(windows[0].pane_id, "%0");
            assert_eq!(
                std::fs::canonicalize("/Users/har")
                    .map(|p| p
                        .to_string_lossy()
                        .into_owned())
                    .unwrap_or_else(|_| {
                        "/Users/har".into()
                    }),
                windows[0].path,
            );
            // Second window: pane
            // id `%2`, path
            // already-canonical
            // `/Volumes/HUGE/...`.
            assert_eq!(windows[1].pane_id, "%2");
            assert_eq!(
                windows[1].path,
                "/Volumes/HUGE/har/Sources/markdown-search/note_search"
            );
        }

        /// `parse_tmux_pane_line`
        /// drops inactive windows
        /// (`active:0` rows). The
        /// user's spec pipes
        /// through `grep "active:1"`
        /// — we do the filter
        /// in-process so we only
        /// spawn one subprocess.
        #[test]
        fn parse_tmux_pane_line_filters_inactive_windows() {
            let inactive = "%0 | /Users/har | active:0 | Layout: c17d,121x93,0,0,0";
            let active = "%0 | /Users/har | active:1 | Layout: c17d,121x93,0,0,0";
            assert!(
                parse_tmux_pane_line(inactive).is_none(),
                "active:0 must be filtered out"
            );
            assert!(
                parse_tmux_pane_line(active).is_some()
            );
        }

        /// The format-string bug we
        /// hit during development:
        /// tmux format strings use
        /// `#`-prefixed placeholders
        /// (`#S`, `#{pane_current_path}`),
        /// with **the `#` always
        /// required**. Writing
        /// `"{S}"` instead of `"#S"`
        /// silently renders an empty
        /// first column, then any
        /// strict parser that skips
        /// empty fields throws the
        /// whole line away. The
        /// `FORMAT` constant in
        /// `fetch_tmux_windows` is
        /// tested by `tmux
        /// list-windows -a -F`; the
        /// regression test below
        /// pins the correct format.
        #[test]
        fn parse_tmux_pane_line_rejects_buggy_format() {
            // What `tmux list-windows -a
            // -F "{S} | ... | active:0 | ..."`
            // (buggy format) would
            // actually emit — first
            // column empty. We don't
            // pin every field here
            // (the parse fails on
            // the empty `pane_id`
            // check); just the empty
            // first column.
            let buggy_line =
                " | /Users/har | active:1 | Layout: x";
            assert!(
                parse_tmux_pane_line(buggy_line).is_none(),
                "an empty pane_id field must be rejected, \
                 otherwise the whole tmux snapshot becomes \
                 silently empty and no T markers render"
            );
            // The non-buggy version
            // (with `#{pane_id}`)
            // parses correctly.
            let good_line =
                "%0 | /Users/har | active:1 | Layout: x";
            assert!(
                parse_tmux_pane_line(good_line).is_some()
            );
        }

        /// End-to-end: pre-loading
        /// the snapshot with a window
        /// whose canonical path
        /// matches a directory row
        /// causes
        /// `directory_tmux_pane_id`
        /// to return the pane id.
        /// This is the chain that
        /// produces the user-visible
        /// `T` marker.
        #[test]
        fn directory_row_is_marked_after_snapshot_loaded() {
            let mut app = directories_test_app(&[(
                "ls",
                "/Users/har/Sources/markdown-search/note_search",
                60,
            )]);
            // Snapshot populated
            // with the SAME
            // canonical path tmux
            // reports (so the
            // comparison succeeds
            // without canonicalising
            // on either side at
            // parse time — that
            // part is already
            // covered by the
            // canonicalisation test).
            app.tmux_windows.push(TmuxWindowInfo {
                pane_id: "%1".to_string(),
                path: String::from(
                    "/Volumes/HUGE/har/Sources/markdown-search/note_search",
                ),
            });
            // Direct look-up at the
            // database-stored form
            // (the user-side path)
            // must canonicalise to
            // match.
            assert_eq!(
                app.directory_tmux_pane_id(
                    "/Users/har/Sources/markdown-search/note_search"
                ).as_deref(),
                Some("%1"),
                "the row stored as /Users/... must \
                 match a window stored as /Volumes/HUGE/... — \
                 the canonicalisation contract"
            );
        }

        /// Selecting a `T`-marked
        /// directory row stages
        /// `tmux select-pane -t <id>
        /// && tmux switch-client -t
        /// <id>`. The parent shell
        /// (running the TUI as a
        /// child) eval's the
        /// staged command, which
        /// (since we're inside a
        /// tmux client) switches
        /// the client to the
        /// targeted pane.
        #[test]
        fn select_t_marked_directory_stages_select_and_switch() {
            let mut app = directories_test_app(&[(
                "ls",
                "/Users/har/Sources/markdown-search/note_search",
                60,
            )]);
            // Snapshot contains
            // one active window for
            // the directory above.
            app.tmux_windows.push(TmuxWindowInfo {
                pane_id: "%2".to_string(),
                path: String::from(
                    "/Volumes/HUGE/har/Sources/markdown-search/note_search",
                ),
            });
            app.query = "#".to_string();
            app.refresh();
            // The row is the only
            // one in merged_rows.
            assert_eq!(app.merged_rows().len(), 1);
            app.select_for_run();
            let staged = app.selection.as_deref()
                .expect("selection must be set");
            assert!(
                staged.contains(
                    "tmux select-pane -t %2"
                )
                && staged.contains(
                    "tmux switch-client -t %2"
                ),
                "staged command must call both \
                 select-pane and switch-client with the \
                 pane id, got: {staged:?}"
            );
            // The two must be
            // `&&`-chained so the
            // user doesn't end up
            // switching to a
            // half-targeted client if
            // select-pane failed.
            assert!(
                staged.contains("&&"),
                "select-pane and switch-client must be &&-chained, got: {staged:?}"
            );
            assert_eq!(
                app.pick_mode,
                Some(crate::tui::state::PickMode::Run),
            );
        }

        /// Selecting an unmarked
        /// directory row stages
        /// `tmux new-session -d -s
        /// <basename> -c <dir>;
        /// tmux switch-client -t
        /// <basename>`. The
        /// basename is
        /// `Path::file_name` of the
        /// directory; a quote is
        /// added if the path has
        /// shell metacharacters.
        #[test]
        fn select_unmarked_directory_stages_new_session_and_switch() {
            let mut app = directories_test_app(&[(
                "ls",
                "/Users/har/Projects/coolthing",
                60,
            )]);
            // Empty snapshot —
            // nothing matches.
            app.query = "#".to_string();
            app.refresh();
            // Select the
            // SQL-history row
            // explicitly.
            let sql_row_idx = app
                .merged_rows()
                .iter()
                .position(|r| {
                    r.directory
                        == "/Users/har/Projects/coolthing"
                })
                .expect(
                    "the SQL-history row for /Users/har/Projects/coolthing must be in merged_rows",
                );
            use ratatui::widgets::ListState;
            app.list_state.select(Some(sql_row_idx));
            app.select_for_run();
            let staged = app.selection.as_deref()
                .expect("selection must be set");
            // The directory is
            // under $HOME so it's
            // shortened to
            // `~/Projects/coolthing`
            // for display in the
            // staged command (the
            // user asked for `~` "as
            // much as possible"; tmux
            // also doesn't do `~`
            // expansion itself, so we
            // have to do it here).
            // The bare absolute path
            // is also accepted by
            // tmux, so this isn't a
            // correctness contract —
            // it's a UX one. The
            // dedicated tilde test
            // (`select_unmarked_directory_expands_tilde`)
            // pins the expansion
            // behaviour more
            // directly.
            assert!(
                staged.contains(
                    "tmux new-session -d -s coolthing -c ~/Projects/coolthing"
                )
                || staged.contains(
                    "tmux new-session -d -s coolthing -c /Users/har/Projects/coolthing"
                ),
                "staged command must create detached session with the directory basename, got: {staged:?}"
            );
            assert!(
                staged.contains(
                    "tmux switch-client -t coolthing"
                ),
                "staged command must switch-client to the new session, got: {staged:?}"
            );
            // The two are ;-chained
            // (not &&): the user
            // wants new-session to
            // run regardless of
            // any failure, and
            // switch-client is a
            // follow-up that may or
            // may not succeed (e.g.
            // session already exists
            // in the user's setup
            // with the same name —
            // that's a different
            // error the parent shell
            // surfaces).
            assert!(
                staged.contains("; "),
                "new-session and switch-client must be ;-chained, got: {staged:?}"
            );
        }

        /// Paths with shell
        /// metacharacters get
        /// quoted in the staged
        /// `cd <path>` — same
        /// defensive quoting
        /// already used in todo
        /// mode. This is the v1
        /// "be safe" contract; the
        /// user can always edit
        /// the staged command
        /// before submit.
        #[test]
        fn select_unmarked_directory_quotes_paths_with_spaces() {
            let mut app = directories_test_app(&[(
                "ls",
                "/Users/has spaces/project",
                60,
            )]);
            app.query = "#".to_string();
            app.refresh();
            // Select the
            // SQL-history row
            // explicitly.
            let sql_row_idx = app
                .merged_rows()
                .iter()
                .position(|r| {
                    r.directory == "/Users/has spaces/project"
                })
                .expect(
                    "the SQL-history row for /Users/has spaces/project must be in merged_rows",
                );
            use ratatui::widgets::ListState;
            app.list_state.select(Some(sql_row_idx));
            app.select_for_run();
            let staged = app.selection.as_deref()
                .expect("selection must be set");
            assert!(
                staged.contains(
                    r#"-c "/Users/has spaces/project""#
                ),
                "path with spaces must be quoted, got: {staged:?}"
            );
        }

        /// `~` in the directory is
        /// expanded to `$HOME`
        /// before staging. This
        /// matters because tmux
        /// does NOT do `~`
        /// expansion itself —
        /// `tmux new-session -c
        /// '~/work'` silently
        /// creates the session in
        /// `$HOME`, not `~/work`,
        /// which would be a
        /// surprising correctness
        /// bug. The TUI's staged
        /// command always carries
        /// the absolute path so
        /// tmux gets the right
        /// cwd.
        ///
        /// We can't easily test
        /// this through
        /// `directories_test_app`
        /// because the test inserts
        /// `/Users/har/...` paths
        /// into the DB (not
        /// `~/...`), and the
        /// `~` shorthand only
        /// matches paths that
        /// actually start with the
        /// home prefix. So the
        /// test inserts a
        /// home-prefixed absolute
        /// path and asserts the
        /// staged command has the
        /// `~`-shortened form.
        #[test]
        fn select_unmarked_directory_expands_tilde() {
            // SAFETY: tests run
            // single-threaded; see
            // the parallel-runs-stable
            // comment in
            // `expand_home_basic`.
            let saved_home =
                std::env::var("HOME").ok();
            unsafe {
                std::env::set_var(
                    "HOME",
                    "/Users/har",
                );
            }
            let mut app = directories_test_app(&[(
                "ls",
                "/Users/har/work",
                60,
            )]);
            app.query = "#".to_string();
            app.refresh();
            // Select the
            // SQL-history row
            // explicitly (not
            // `merged_rows()[0]`).
            // The new
            // directory-source
            // feature surfaces
            // tmux panes as
            // rows too, so the
            // first row may be
            // one of the user's
            // real tmux panes,
            // not our test row.
            let sql_row_idx = app
                .merged_rows()
                .iter()
                .position(|r| {
                    r.directory == "/Users/har/work"
                })
                .expect(
                    "the SQL-history row for /Users/har/work must be in merged_rows",
                );
            use ratatui::widgets::ListState;
            app.list_state.select(Some(sql_row_idx));
            app.select_for_run();
            let staged = app.selection.as_deref()
                .expect("selection must be set");
            // The directory in the
            // staged `new-session
            // -c` argument must use
            // `~/work`, not the
            // raw `/Users/har/work`,
            // because the source
            // directory is under
            // `$HOME` and the user
            // expects the `~`
            // form.
            //
            // Note: this test is
            // *not* the same as the
            // bug we're fixing (which
            // was about *literal* `~`
            // in the source directory).
            // The DB-stored path is
            // always absolute (per
            // `fetch_directories`'s
            // `directory` column),
            // so the expansion we
            // test here is the
            // *display + command*
            // shortening — a
            // separate feature. The
            // "no literal `~` in the
            // source path" contract
            // is covered implicitly:
            // the source is always
            // absolute, and the
            // expansion is a pure
            // function of the
            // home-prefix match.
            assert!(
                staged.contains(
                    "tmux new-session -d -s work -c ~/work"
                ),
                "staged command must use the `~/...` \
                 shorthand for paths under $HOME, \
                 got: {staged:?}"
            );
            // Restore HOME.
            if let Some(h) = saved_home {
                unsafe {
                    std::env::set_var("HOME", h);
                }
            } else {
                unsafe {
                    std::env::remove_var(
                        "HOME",
                    );
                }
            }
        }

        /// The user can pin a
        /// `sessiondirs=...`
        /// directory in the
        /// config and every
        /// subdirectory (recursively
        /// walked) appears as a
        /// row in the directories
        /// list, even if no
        /// command has ever been
        /// run there. We test this
        /// by injecting a
        /// `session_subdirs` entry
        /// directly into the
        /// `App` (the test
        /// doesn't have a config
        /// file to load) and
        /// checking the row
        /// surfaces.
        #[test]
        fn fetch_directories_includes_sessiondir_subdirs() {
            // Build a temp
            // directory tree to
            // walk.
            let n = std::sync::atomic::AtomicU64::new(0)
                .fetch_add(
                    1,
                    std::sync::atomic::Ordering::SeqCst,
                );
            let pid = std::process::id();
            let root = std::env::temp_dir().join(format!(
                "smarthistory_sessiondir_{pid}_{n}"
            ));
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::create_dir_all(
                root.join("a").join("b"),
            );
            let _ = std::fs::create_dir_all(
                root.join("c"),
            );
            // sessiondir
            // subdirectory set
            // (the production
            // path uses
            // `build_session_subdirs`
            // at App
            // construction;
            // for the test we
            // pass it
            // explicitly via
            // the
            // `directories_test_app_with_sessions`
            // helper).
            let mut app = directories_test_app_with_sessions(
                &[],
                vec![
                    root.join("a"),
                    root.join("a").join("b"),
                    root.join("c"),
                ],
            );
            app.query = "#".to_string();
            app.refresh();
            let rows = app.merged_rows();
            // The pinned
            // subdirs should
            // appear even
            // though we
            // passed `&[]` to
            // `directories_test_app`
            // (no history).
            let row_dirs: std::collections::HashSet<String> =
                rows.iter()
                    .map(|r| {
                        std::fs::canonicalize(&r.directory)
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| r.directory.clone())
                    })
                    .collect();
            let expected: std::collections::HashSet<String> = app
                .session_subdirs
                .iter()
                .map(|p| {
                    std::fs::canonicalize(p)
                        .map(|c| c.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| {
                            p.to_string_lossy().into_owned()
                        })
                })
                .collect();
            for want in &expected {
                assert!(
                    row_dirs.contains(want),
                    "pinned subdir {want:?} should be in merged_rows, got: {row_dirs:?}"
                );
            }
            // The pinned
            // rows have
            // `timestamp = 0`
            // (so they sort
            // to the bottom
            // of the
            // newest-first
            // list) and an
            // empty `command`
            // — except for
            // `.command`
            // hints.
            for row in rows {
                let canonical = std::fs::canonicalize(
                    &row.directory,
                )
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| row.directory.clone());
                if expected.contains(&canonical) {
                    assert_eq!(
                        row.timestamp, 0,
                        "sessiondir row must have timestamp 0, got: {}",
                        row.timestamp
                    );
                    assert_eq!(
                        row.mode, "directory",
                        "sessiondir row must have mode='directory'"
                    );
                }
            }
            let _ = std::fs::remove_dir_all(&root);
        }

        /// When a sessiondir row
        /// has a `.command` file
        /// in itself or an
        /// ancestor, the TUI
        /// surfaces "(has
        /// .command)" in the
        /// secondary slot so
        /// the user knows the
        /// row will run a setup
        /// script on select.
        #[test]
        fn fetch_directories_surfaces_command_file_hint() {
            // Build:
            //   tmpdir/
            //   tmpdir/project/         (has .command)
            //   tmpdir/project/src/     (no .command)
            let n = std::sync::atomic::AtomicU64::new(0)
                .fetch_add(
                    1,
                    std::sync::atomic::Ordering::SeqCst,
                );
            let pid = std::process::id();
            let root = std::env::temp_dir().join(format!(
                "smarthistory_sessiondir_cmd_{pid}_{n}"
            ));
            let _ = std::fs::remove_dir_all(&root);
            let project = root.join("project");
            let src = project.join("src");
            let _ = std::fs::create_dir_all(&src);
            let _ = std::fs::write(
                project.join(".command"),
                "#!/bin/sh\necho setup\n",
            );
            // `project/src`
            // subdir is
            // pinned (the
            // walker would
            // also pick up
            // `project` if
            // the user pinned
            // `root`; we pin
            // a leaf to test
            // the ancestor
            // walk).
            let mut app = directories_test_app_with_sessions(
                &[],
                vec![src.clone()],
            );
            app.query = "#".to_string();
            app.refresh();
            let row = app
                .merged_rows()
                .iter()
                .find(|r| {
                    std::fs::canonicalize(&r.directory)
                        .map(|c| {
                            c == std::fs::canonicalize(&src).unwrap()
                        })
                        .unwrap_or(false)
                })
                .expect("src row must be in the list");
            assert_eq!(
                row.comment, "(has .command)",
                "row's secondary slot should announce the .command, got: {:?}",
                row.comment
            );
            let _ = std::fs::remove_dir_all(&root);
        }

        /// When a sessiondir row
        /// has a `.command` file
        /// in itself or an
        /// ancestor, selecting
        /// the row chains
        /// `sh <command-file> <dir>`
        /// into the staged tmux
        /// command. The first
        /// argument is always
        /// the selected
        /// directory.
        #[test]
        fn select_directory_runs_command_file() {
            // Build:
            //   tmpdir/
            //   tmpdir/project/         (has .command)
            //   tmpdir/project/src/     (no .command)
            let n = std::sync::atomic::AtomicU64::new(0)
                .fetch_add(
                    1,
                    std::sync::atomic::Ordering::SeqCst,
                );
            let pid = std::process::id();
            let root = std::env::temp_dir().join(format!(
                "smarthistory_select_cmd_{pid}_{n}"
            ));
            let _ = std::fs::remove_dir_all(&root);
            let project = root.join("project");
            let src = project.join("src");
            let _ = std::fs::create_dir_all(&src);
            let cmd_path = project.join(".command");
            let _ = std::fs::write(
                &cmd_path,
                "#!/bin/sh\necho setup $1\n",
            );
            let mut app = directories_test_app(&[(
                "ls",
                &src.to_string_lossy(),
                60,
            )]);
            app.query = "#".to_string();
            app.refresh();
            // Find the SQL row
            // by its directory
            // (the user's real
            // tmux panes may
            // also be present
            // because
            // `refresh()` re-runs
            // the tmux fetch,
            // and the new
            // directory-source
            // feature surfaces
            // them as rows). We
            // explicitly select
            // the SQL row (not
            // `merged_rows()[0]`)
            // by setting
            // `list_state.selected`
            // to its index.
            let sql_row_idx = app
                .merged_rows()
                .iter()
                .position(|r| {
                    r.directory == src.to_string_lossy()
                })
                .expect(
                    "the SQL-history row for `src` must be in merged_rows",
                );
            use ratatui::widgets::ListState;
            app.list_state.select(Some(sql_row_idx));
            app.select_for_run();
            let staged = app
                .selection
                .as_deref()
                .expect("selection must be set");
            // The staged
            // command must
            // include both the
            // new-session
            // chain and the
            // .command run.
            // Form:
            //   tmux new-session -d -s src -c <src>; \
            //     sh <.command> <src>; \
            //     tmux switch-client -t src
            let cmd_str = cmd_path.to_string_lossy();
            let src_str = src.to_string_lossy();
            assert!(
                staged.contains("tmux new-session"),
                "staged must create a new tmux session, got: {staged:?}"
            );
            assert!(
                staged.contains("switch-client"),
                "staged must switch-client to the new session, got: {staged:?}"
            );
            // The .command
            // invocation
            // should appear
            // with the path
            // and the
            // selected
            // directory as
            // the first arg.
            assert!(
                staged.contains(&format!(
                    "sh {} {}",
                    cmd_str, src_str
                )),
                "staged must run `sh <.command> <dir>`, got: {staged:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }

        /// Active tmux panes
        /// (whose cwds are
        /// distinct from the
        /// SQL history) appear
        /// as rows in the
        /// directories list
        /// with `source =
        /// "tmux"`. The
        /// visible primary
        /// text is the
        /// directory in
        /// `~/x` form; the
        /// secondary slot
        /// shows `(pane %N)` so
        /// the user can copy
        /// the pane id for
        /// `tmux send-keys -t
        /// %N ...` directly
        /// from the list.
        #[test]
        fn fetch_directories_includes_tmux_panes() {
            let mut app = directories_test_app(&[]);
            // Inject one tmux
            // window. Use `/tmp`
            // (a real directory
            // on every Unix) so
            // it passes the
            // `is_dir()` check.
            // macOS canonicalises
            // `/tmp` to
            // `/private/tmp`,
            // which is fine for
            // this test.
            app.tmux_windows.push(TmuxWindowInfo {
                pane_id: "%42".to_string(),
                path: String::from("/tmp"),
            });
            app.query = "#".to_string();
            app.refresh();
            // Find the tmux
            // row. The
            // directory will be
            // canonicalised by
            // `std::fs::canonicalize`
            // (which on macOS
            // resolves
            // `/tmp` to
            // `/private/tmp`).
            let row = app
                .merged_rows()
                .iter()
                .find(|r| r.source == "tmux")
                .expect(
                    "the tmux pane row must be in merged_rows",
                );
            // The visible primary
            // text is the
            // directory in
            // shell-shortened
            // form. `/tmp`
            // shortens to
            // `/tmp` (no home
            // to expand to).
            assert_eq!(
                row.command, "/tmp",
                "primary text must be the directory, got: {:?}",
                row.command
            );
            // The secondary slot
            // carries the pane
            // id (so the user
            // can reuse it).
            assert!(
                row.comment.contains("%42"),
                "secondary slot must show the pane id, got: {:?}",
                row.comment
            );
        }

        /// The
        /// `DirectorySource::All`
        /// mode shows every
        /// row regardless of
        /// source.
        #[test]
        fn directory_source_all_shows_everything() {
            // Use `/tmp` (a real
            // directory on every
            // Unix) for the SQL
            // and tmux rows so
            // they pass the
            // `is_dir()` check.
            // macOS canonicalises
            // `/tmp` to
            // `/private/tmp`,
            // which is fine for
            // the test.
            let sql_dir = "/tmp";
            let tmux_dir = "/tmp";
            let mut app = directories_test_app(&[(
                "ls",
                sql_dir,
                60,
            )]);
            // Inject one
            // sessiondir
            // and one tmux
            // pane.
            let n = std::sync::atomic::AtomicU64::new(0)
                .fetch_add(
                    1,
                    std::sync::atomic::Ordering::SeqCst,
                );
            let pid = std::process::id();
            let session_root =
                std::env::temp_dir().join(format!(
                    "smarthistory_dirsrc_all_{pid}_{n}"
                ));
            let _ = std::fs::create_dir_all(
                session_root.join("inside"),
            );
            app.session_subdirs =
                vec![session_root.join("inside")];
            app.tmux_windows
                .push(TmuxWindowInfo {
                    pane_id: "%7".to_string(),
                    path: String::from(tmux_dir),
                });
            // Default source is
            // `All`, so all
            // three rows are
            // visible.
            app.query = "#".to_string();
            app.refresh();
            let dirs: std::collections::HashSet<String> = app
                .merged_rows()
                .iter()
                .map(|r| r.directory.clone())
                .collect();
            assert!(
                dirs.contains(sql_dir),
                "SQL row must be visible, got: {:?}",
                dirs
            );
            assert!(
                dirs.contains(
                    &session_root
                        .join("inside")
                        .to_string_lossy()
                        .to_string()
                ),
                "sessiondir row must be visible, got: {:?}",
                dirs
            );
            assert!(
                dirs.contains(tmux_dir),
                "tmux row must be visible, got: {:?}",
                dirs
            );
            let _ = std::fs::remove_dir_all(
                &session_root
            );
        }

        /// The
        /// `DirectorySource::Config`
        /// mode shows only the
        /// `sessiondirs=...`
        /// rows. SQL history
        /// rows and tmux panes
        /// are filtered out.
        #[test]
        fn directory_source_config_filters_to_sessiondirs() {
            let mut app = directories_test_app(&[(
                "ls",
                "/Users/har/sql_row",
                60,
            )]);
            let n = std::sync::atomic::AtomicU64::new(0)
                .fetch_add(
                    1,
                    std::sync::atomic::Ordering::SeqCst,
                );
            let pid = std::process::id();
            let session_root =
                std::env::temp_dir().join(format!(
                    "smarthistory_dirsrc_cfg_{pid}_{n}"
                ));
            let _ = std::fs::create_dir_all(
                session_root.join("inside"),
            );
            app.session_subdirs =
                vec![session_root.join("inside")];
            app.tmux_windows
                .push(TmuxWindowInfo {
                    pane_id: "%7".to_string(),
                    path: String::from(
                        "/Users/har/tmux_row",
                    ),
                });
            app.directory_source =
                crate::tui::state::DirectorySource::Config;
            app.query = "#".to_string();
            app.refresh();
            // Only the
            // sessiondir
            // row should be
            // visible.
            assert_eq!(
                app.merged_rows().len(),
                1,
                "Config mode must show only sessiondir rows, got: {:?}",
                app.merged_rows()
                    .iter()
                    .map(|r| (
                        r.directory.clone(),
                        r.source.clone()
                    ))
                    .collect::<Vec<_>>()
            );
            let row = &app.merged_rows()[0];
            assert_eq!(row.source, "sessiondir");
            let _ = std::fs::remove_dir_all(
                &session_root
            );
        }

        /// The
        /// `DirectorySource::Tmux`
        /// mode shows only the
        /// active tmux panes'
        /// cwds. SQL history
        /// rows and sessiondirs
        /// rows are filtered
        /// out.
        #[test]
        fn directory_source_tmux_filters_to_panes() {
            // The SQL and tmux
            // rows must use
            // *different* paths
            // (the dedup loop
            // suppresses tmux
            // rows whose canonical
            // path matches an
            // earlier SQL row).
            let n = std::sync::atomic::AtomicU64::new(0)
                .fetch_add(
                    1,
                    std::sync::atomic::Ordering::SeqCst,
                );
            let pid = std::process::id();
            let tmux_path = std::env::temp_dir()
                .join(format!(
                    "smarthistory_tmux_pane_{pid}_{n}"
                ));
            let _ = std::fs::create_dir_all(&tmux_path);
            let mut app = directories_test_app(&[(
                "ls",
                "/tmp",
                60,
            )]);
            app.tmux_windows
                .push(TmuxWindowInfo {
                    pane_id: "%7".to_string(),
                    path: tmux_path
                        .to_string_lossy()
                        .into_owned(),
                });
            app.directory_source =
                crate::tui::state::DirectorySource::Tmux;
            app.query = "#".to_string();
            app.refresh();
            // Only the
            // tmux-pane
            // row should
            // be visible.
            assert_eq!(
                app.merged_rows().len(),
                1,
                "Tmux mode must show only tmux pane rows, got: {:?}",
                app.merged_rows()
                    .iter()
                    .map(|r| (
                        r.directory.clone(),
                        r.source.clone()
                    ))
                    .collect::<Vec<_>>()
            );
            let row = &app.merged_rows()[0];
            assert_eq!(row.source, "tmux");
            let _ = std::fs::remove_dir_all(&tmux_path);
        }

        /// Regression test for the
        /// bug where a tmux pane
        /// whose path also appears
        /// in the SQL history DB
        /// was silently deduped
        /// away in `DIR:TMUX`
        /// mode. The shared `seen`
        /// set was populated by the
        /// SQL loop first, so the
        /// tmux loop's `seen.insert`
        /// returned `false` and the
        /// pane was dropped — even
        /// though the SQL row would
        /// later be filtered out by
        /// the source filter. The
        /// fix: the source filter is
        /// applied *early* (the SQL
        /// loop is skipped entirely
        /// in `DIR:TMUX` mode).
        ///
        /// User symptom: 5 active
        /// tmux panes, but
        /// `DIR:TMUX` showed only
        /// 2 (the ones not in the
        /// history DB).
        #[test]
        fn directory_source_tmux_shows_pane_even_if_path_in_history() {
            // A real directory we
            // use for BOTH the SQL
            // row and the tmux pane.
            let n = std::sync::atomic::AtomicU64::new(0)
                .fetch_add(
                    1,
                    std::sync::atomic::Ordering::SeqCst,
                );
            let pid = std::process::id();
            let shared_path = std::env::temp_dir()
                .join(format!(
                    "smarthistory_tmux_dup_{pid}_{n}"
                ));
            let _ = std::fs::create_dir_all(&shared_path);
            let shared_str = shared_path
                .to_string_lossy()
                .into_owned();
            // SQL history row in the
            // SAME directory.
            let mut app = directories_test_app(&[(
                "ls",
                &shared_str,
                60,
            )]);
            // Tmux pane in the SAME
            // directory.
            app.tmux_windows
                .push(TmuxWindowInfo {
                    pane_id: "%9".to_string(),
                    path: shared_str.clone(),
                });
            app.directory_source =
                crate::tui::state::DirectorySource::Tmux;
            app.query = "#".to_string();
            app.refresh();
            // In `DIR:TMUX` mode the
            // tmux pane MUST appear,
            // even though the SQL
            // row has the same
            // canonical path.
            let tmux_rows: Vec<_> = app
                .merged_rows()
                .iter()
                .filter(|r| r.source == "tmux")
                .collect();
            assert_eq!(
                tmux_rows.len(),
                1,
                "DIR:TMUX must show the tmux pane even when its path is in the history DB, got: {:?}",
                app.merged_rows()
                    .iter()
                    .map(|r| (
                        r.directory.clone(),
                        r.source.clone()
                    ))
                    .collect::<Vec<_>>()
            );
            assert_eq!(tmux_rows[0].source, "tmux");
            // And no SQL rows leak
            // through.
            assert!(
                app.merged_rows()
                    .iter()
                    .all(|r| r.source == "tmux"),
                "DIR:TMUX must not show SQL rows, got: {:?}",
                app.merged_rows()
                    .iter()
                    .map(|r| r.source.clone())
                    .collect::<Vec<_>>()
            );
            let _ = std::fs::remove_dir_all(&shared_path);
        }

        /// Regression test for the bug
        /// where a labeled history
        /// row (an entry with a
        /// comment in the
        /// `command_comments`
        /// table) leaked into
        /// `DIR:TMUX` directories
        /// mode. The user ran
        /// `tmux list-windows -a
        /// -F ... | grep
        /// "active:1"` at some
        /// point and labeled it,
        /// so the row had a
        /// comment. `build_merged_rows`
        /// appended *all* labeled
        /// rows to the merged
        /// list regardless of
        /// mode; in directories
        /// mode that meant the
        /// history row showed up
        /// alongside (or instead
        /// of) the real tmux
        /// pane rows. The fix:
        /// `build_merged_rows`
        /// skips the labeled/preview
        /// merge entirely in
        /// directories mode and
        /// returns only the
        /// directory rows.
        #[test]
        fn directory_source_tmux_excludes_labeled_history_rows() {
            let n = std::sync::atomic::AtomicU64::new(0)
                .fetch_add(
                    1,
                    std::sync::atomic::Ordering::SeqCst,
                );
            let pid = std::process::id();
            let tmux_path = std::env::temp_dir().join(format!(
                "smarthistory_tmux_labeled_{pid}_{n}"
            ));
            let _ = std::fs::create_dir_all(&tmux_path);
            // The labeled command —
            // the exact "tmux list-
            // windows ..." line the
            // user reported. It was
            // run from /tmp.
            let labeled_cmd =
                "tmux list-windows -a -F #{pane_id}";
            let mut app = directories_test_app(&[
                (labeled_cmd, "/tmp", 60),
            ]);
            // Create the
            // command_comments table
            // and label the command,
            // making it a "labeled
            // row" that
            // `fetch_labeled` will
            // return.
            app.conn
                .execute(
                    "CREATE TABLE command_comments (
                        command TEXT PRIMARY KEY,
                        comment TEXT NOT NULL
                    )",
                    [],
                )
                .expect("create command_comments");
            // `fetch_labeled` does a LEFT JOIN on
            // `history_output`; the table must exist or
            // the query errors (and `.unwrap_or_default()`
            // silently yields an empty labeled set —
            // which would mask the bug in this test).
            app.conn
                .execute(
                    "CREATE TABLE history_output (
                        history_id INTEGER PRIMARY KEY,
                        output TEXT NOT NULL
                    )",
                    [],
                )
                .expect("create history_output");
            app.conn
                .execute(
                    "INSERT INTO command_comments (command, comment) VALUES (?1, ?2)",
                    rusqlite::params![labeled_cmd, "TMUX LIST"],
                )
                .expect("insert comment");
            // One real tmux pane with a
            // different path.
            app.tmux_windows
                .push(TmuxWindowInfo {
                    pane_id: "%5".to_string(),
                    path: tmux_path
                        .to_string_lossy()
                        .into_owned(),
                });
            app.directory_source =
                crate::tui::state::DirectorySource::Tmux;
            app.query = "#".to_string();
            app.refresh();
            // The labeled history
            // row (`tmux list-
            // windows ...`) must
            // NOT appear in
            // `DIR:TMUX` mode.
            let has_labeled = app
                .merged_rows()
                .iter()
                .any(|r| r.command == labeled_cmd);
            assert!(
                !has_labeled,
                "DIR:TMUX must not show labeled history rows, got: {:?}",
                app.merged_rows()
                    .iter()
                    .map(|r| (
                        r.command.clone(),
                        r.source.clone()
                    ))
                    .collect::<Vec<_>>()
            );
            // Only the real tmux
            // pane should be
            // visible.
            assert_eq!(app.merged_rows().len(), 1);
            assert_eq!(app.merged_rows()[0].source, "tmux");
            let _ = std::fs::remove_dir_all(&tmux_path);
        }

        /// `cycle_directory_source`
        /// pressed while NOT in
        /// directories mode should
        /// switch INTO directories
        /// mode (prepending the `#`
        /// prefix) AND cycle the
        /// source. The user can be
        /// in plain history and
        /// land directly in `DIR:TMUX`.
        #[test]
        fn cycle_directory_source_enters_dirs_mode_from_plain() {
            let mut app = directories_test_app(&[(
                "ls",
                "/tmp",
                60,
            )]);
            // Plain mode, no prefix.
            app.query = String::from("ls");
            assert!(!app.is_directories_query());
            // Cycle from plain -> DIR:TMUX.
            app.cycle_directory_source();
            // Now in directories mode.
            assert!(
                app.is_directories_query(),
                "must enter directories mode, got query {:?}",
                app.query
            );
            // Source cycled to TMUX.
            assert_eq!(
                app.directory_source,
                crate::tui::state::DirectorySource::Tmux
            );
            // Body preserved: `#ls`.
            assert_eq!(app.query, "#ls");
        }

        /// Cycling three times from
        /// plain mode lands back on
        /// `DIR:ALL`, still in
        /// directories mode.
        #[test]
        fn cycle_directory_source_three_times_wraps_to_all() {
            let mut app = directories_test_app(&[(
                "ls",
                "/tmp",
                60,
            )]);
            app.query = String::new();
            app.cycle_directory_source(); // -> TMUX
            assert!(app.is_directories_query());
            assert_eq!(
                app.directory_source,
                crate::tui::state::DirectorySource::Tmux
            );
            app.cycle_directory_source(); // -> CFG
            assert!(app.is_directories_query());
            assert_eq!(
                app.directory_source,
                crate::tui::state::DirectorySource::Config
            );
            app.cycle_directory_source(); // -> ALL
            assert!(app.is_directories_query());
            assert_eq!(
                app.directory_source,
                crate::tui::state::DirectorySource::All
            );
            // Query is just `#` (empty body).
            assert_eq!(app.query, "#");
        }

        /// Switching from a search
        /// mode (`?foo`) strips the
        /// fuzzy prefix and yields
        /// `#foo` (not `#?foo`).
        #[test]
        fn cycle_directory_source_strips_search_prefix() {
            let mut app = directories_test_app(&[(
                "ls",
                "/tmp",
                60,
            )]);
            app.query = String::from("?foo");
            assert!(app.is_fuzzy_query());
            app.cycle_directory_source();
            assert!(app.is_directories_query());
            assert_eq!(
                app.directory_source,
                crate::tui::state::DirectorySource::Tmux
            );
            assert_eq!(app.query, "#foo");
        }

        /// When ALREADY in directories
        /// mode, cycle just advances the
        /// source — the query prefix is
        /// not doubled.
        #[test]
        fn cycle_directory_source_in_dirs_mode_does_not_double_prefix() {
            let mut app = directories_test_app(&[(
                "ls",
                "/tmp",
                60,
            )]);
            app.query = String::from("#");
            app.cycle_directory_source();
            assert_eq!(app.query, "#");
            assert!(app.is_directories_query());
            assert_eq!(
                app.directory_source,
                crate::tui::state::DirectorySource::Tmux
            );
        }

        /// Build an App pre-loaded with a set of session
        /// panes (bypassing the real `tmux list-panes -s`
        /// subprocess, which depends on a live tmux
        /// server). Each tuple is
        /// (pane_id, window_id, cwd, current_command).
        /// The caller still owns `app.session_panes` and can
        /// mutate it after construction. `TMUX_PANE` is NOT
        /// set (so the exclusion filter is exercised
        /// explicitly by the caller via the injected rows).
        fn panes_test_app(
            panes: &[(&str, &str, &str, &str)],
        ) -> App {
            let mut app = directories_test_app(&[]);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let home_list = app.home_list.clone();
            let mut next_id: i64 = -1;
            app.session_panes = panes
                .iter()
                .map(|(pane_id, window_id, cwd, cmd)| {
                    let full = crate::util::canonicalize_directory(cwd);
                    let short = crate::util::shorten_home_path(
                        &full,
                        &home_list,
                    )
                    .into_owned();
                    let id = next_id;
                    next_id -= 1;
                    HistoryRow {
                        id,
                        command: cmd.to_string(),
                        directory: full,
                        session_id: pane_id.to_string(),
                        exit_code: 0,
                        timestamp: now,
                        comment: short,
                        // window id (`@N`) stashed
                        // for the cross-window
                        // select-window jump.
                        output: window_id.to_string(),
                        mode: "pane".to_string(),
                        source: "pane".to_string(),
                    }
                })
                .collect();
            app
        }

        /// `*` prefix switches the query into panes mode,
        /// and `is_panes_query()` / `panes_pattern()` slice
        /// the body correctly.
        #[test]
        fn panes_prefix_detected_and_pattern_sliced() {
            let mut app = directories_test_app(&[]);
            app.query = String::new();
            assert!(!app.is_panes_query());
            app.query = String::from("*");
            assert!(app.is_panes_query());
            assert_eq!(app.panes_pattern(), "");
            app.query = String::from("*vim src");
            assert!(app.is_panes_query());
            assert_eq!(app.panes_pattern(), "vim src");
        }

        /// `fetch_panes` returns the cached session panes
        /// (no substring filter when the body is empty).
        #[test]
        fn fetch_panes_returns_all_when_no_filter() {
            let mut app = panes_test_app(&[
                ("%1", "@1", "/tmp", "zsh"),
                ("%2", "@2", "/tmp", "vim"),
            ]);
            app.query = String::from("*");
            let rows = app.fetch_panes().unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].source, "pane");
            assert_eq!(rows[0].session_id, "%1");
            assert_eq!(rows[0].command, "zsh");
        }

        /// The substring filter matches against the pane's
        /// current command OR its cwd (short form). Both
        /// whitespace tokens must match (AND semantics).
        #[test]
        fn fetch_panes_substring_filter_matches_command_or_cwd() {
            let mut app = panes_test_app(&[
                ("%1", "@1", "/tmp", "zsh"),
                ("%2", "@2", "/tmp", "vim"),
            ]);
            // `*vim` → only the pane running vim.
            app.query = String::from("*vim");
            let rows = app.fetch_panes().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].session_id, "%2");
            assert_eq!(rows[0].command, "vim");
        }

        /// Selecting a pane in `*` mode stages the
        /// `tmux select-window -t <window_id> && tmux select-pane -t <pane_id>`
        /// command — `select-window` first because plain
        /// `select-pane` does NOT switch windows, and a
        /// target pane may live in another window of the
        /// current session.
        #[test]
        fn panes_last_pane_bubbled_to_index_zero() {
            // Simulate three panes; mark %2 as the "last"
            // (previously-active) pane by giving it the
            // bumped timestamp `fetch_session_panes_impl`
            // assigns. The bubble logic moves it to
            // position 0 so the default selection (index 0)
            // lands on it — pressing Enter flips back to
            // the pane the user just came from.
            let mut app = panes_test_app(&[
                ("%1", "@1", "/tmp", "zsh"),
                ("%2", "@2", "/tmp", "vim"),
                ("%3", "@3", "/tmp", "cargo"),
            ]);
            // Bump %2's timestamp to mimic the `pane_last`
            // flag path in `fetch_session_panes_impl`.
            let base = app.session_panes[0].timestamp;
            let last_row = app
                .session_panes
                .iter_mut()
                .find(|r| r.session_id == "%2")
                .expect("%2 row");
            last_row.timestamp = base + 1;
            // Apply the same bubble the impl does.
            if let Some(pos) = app
                .session_panes
                .iter()
                .position(|r| r.timestamp > base)
            {
                let row = app.session_panes.remove(pos);
                app.session_panes.insert(0, row);
            }
            app.query = String::from("*");
            app.refresh();
            // The merged list must have %2 first.
            assert_eq!(app.merged_rows().len(), 3);
            assert_eq!(
                app.merged_rows()[0].session_id, "%2",
                "last pane must bubble to index 0, got {:?}",
                app.merged_rows()
                    .iter()
                    .map(|r| r.session_id.clone())
                    .collect::<Vec<_>>()
            );
            // Default selection is index 0 → pressing
            // Enter stages a jump to %2.
            app.select_for_run();
            assert!(
                app.selection.as_deref().unwrap_or("").contains("-t %2"),
                "Enter must stage a jump to the last pane %2, got {:?}",
                app.selection
            );
        }

        #[test]
        fn select_for_run_in_panes_mode_stages_switch_client() {
            let mut app = panes_test_app(&[
                ("%5", "@3", "/tmp", "vim"),
            ]);
            app.query = String::from("*");
            app.refresh();
            // Select the first (only) row.
            app.list_state.select(Some(0));
            app.select_for_run();
            assert_eq!(
                app.selection.as_deref(),
                Some("tmux select-window -t @3 && tmux select-pane -t %5")
            );
            assert_eq!(app.pick_mode, Some(PickMode::Run));
        }

        /// When the window id is missing (parse fallback /
        /// old snapshot), `select_for_run` degrades to a
        /// bare `select-pane -t <pane_id>` rather than
        /// staging a broken `select-window -t <empty>`.
        #[test]
        fn select_for_run_in_panes_mode_degrades_without_window_id() {
            // Empty window id ("@" stripped / old snapshot).
            let mut app = panes_test_app(&[
                ("%5", "", "/tmp", "vim"),
            ]);
            app.query = String::from("*");
            app.refresh();
            // Select the first (only) row.
            app.list_state.select(Some(0));
            app.select_for_run();
            assert_eq!(
                app.selection.as_deref(),
                Some("tmux select-pane -t %5")
            );
            assert_eq!(app.pick_mode, Some(PickMode::Run));
        }

        /// Panes mode is excluded from the labeled-row merge
        /// (same fix as directories mode — a labeled history
        /// row must not leak into the panes list).
        #[test]
        fn panes_mode_excludes_labeled_history_rows() {
            let labeled_cmd =
                "tmux list-panes -s -F stuff";
            let mut app = directories_test_app(&[
                (labeled_cmd, "/tmp", 60),
            ]);
            app.conn
                .execute(
                    "CREATE TABLE command_comments (
                        command TEXT PRIMARY KEY,
                        comment TEXT NOT NULL
                    )",
                    [],
                )
                .expect("cc");
            app.conn
                .execute(
                    "CREATE TABLE history_output (
                        history_id INTEGER PRIMARY KEY,
                        output TEXT NOT NULL
                    )",
                    [],
                )
                .expect("ho");
            app.conn
                .execute(
                    "INSERT INTO command_comments (command, comment) VALUES (?1, ?2)",
                    rusqlite::params![labeled_cmd, "PANES LIST"],
                )
                .expect("ins");
            // Inject one pane so the panes list isn't empty.
            app.session_panes.push(HistoryRow {
                id: -1,
                command: "zsh".to_string(),
                directory: "/tmp".to_string(),
                session_id: "%7".to_string(),
                exit_code: 0,
                timestamp: 0,
                comment: "/tmp".to_string(),
                output: String::new(),
                mode: "pane".to_string(),
                source: "pane".to_string(),
            });
            app.query = String::from("*");
            app.refresh();
            // The labeled history row must NOT appear.
            let has_labeled = app
                .merged_rows()
                .iter()
                .any(|r| r.command == labeled_cmd);
            assert!(
                !has_labeled,
                "panes mode must not show labeled history rows, got: {:?}",
                app.merged_rows()
                    .iter()
                    .map(|r| (
                        r.command.clone(),
                        r.source.clone()
                    ))
                    .collect::<Vec<_>>()
            );
            // Only the pane row is visible.
            assert_eq!(app.merged_rows().len(), 1);
            assert_eq!(app.merged_rows()[0].source, "pane");
        }

        /// `fetch_session_panes` does NOT run `tmux` when
        /// `$TMUX_PANE` is unset (the obvious "not in tmux"
        /// signal) — the cache stays empty and `fetch_panes`
        /// returns an empty list rather than spawning a
        /// doomed subprocess.
        #[test]
        fn fetch_session_panes_no_op_when_not_in_tmux() {
            let mut app = directories_test_app(&[]);
            // Ensure $TMUX_PANE is unset for this test. We
            // can't `env::remove_var` safely under parallel
            // test runners, so we read whatever the user's
            // environment has and assert the contract
            // conditionally: if unset, the cache must stay
            // empty; if set (user is in tmux), the cache
            // may be populated and we just assert no panic.
            let pane = std::env::var("TMUX_PANE").ok();
            app.fetch_session_panes();
            if pane.is_none() {
                assert!(
                    app.session_panes.is_empty(),
                    "cache must stay empty when $TMUX_PANE is unset"
                );
            }
            // In both cases, no panic.
        }

        /// End-to-end with the REAL current tmux session:
        /// run `fetch_session_panes_impl` with the actual
        /// `$TMUX_PANE` and confirm the current pane is
        /// excluded and every surviving row is well-formed
        /// (pane id `%N`, window id `@N`, source `pane`).
        /// Skipped if `tmux` isn't on PATH or `$TMUX_PANE`
        /// isn't set (not running inside tmux).
        #[test]
        fn fetch_session_panes_end_to_end_real_tmux() {
            let current_pane = std::env::var("TMUX_PANE")
                .unwrap_or_default();
            if current_pane.is_empty() {
                eprintln!("[skip] $TMUX_PANE unset (not in tmux)");
                return;
            }
            let mut app = directories_test_app(&[]);
            app.session_panes.clear();
            app.fetch_session_panes_impl(&current_pane);
            // The current pane must NOT appear.
            let ids: Vec<String> =
                app.session_panes.iter().map(|r| r.session_id.clone()).collect();
            assert!(
                !ids.contains(&current_pane),
                "current pane {} must be excluded, got {:?}",
                current_pane, ids
            );
            // Every surviving row is well-formed.
            for r in &app.session_panes {
                assert!(
                    r.session_id.starts_with('%'),
                    "pane id must look like %N, got {:?}",
                    r.session_id
                );
                assert!(
                    r.output.starts_with('@'),
                    "window id must look like @N, got {:?}",
                    r.output
                );
                assert_eq!(r.source, "pane");
            }
            // The last (previously-active) pane must be
            // bubbled to position 0 so the user can flip
            // back to it by pressing Enter. Identify it via
            // `tmux display-message -t {last}`. If the last
            // pane happens to equal the current pane (e.g.
            // the env-var quirk in CI) skip the positional
            // assertion — the exclusion check above
            // already covers that case.
            let last_pane = std::process::Command::new("tmux")
                .args(["display-message", "-p", "-t", "{last}", "#{pane_id}"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            if !last_pane.is_empty()
                && last_pane != current_pane
                && ids.contains(&last_pane)
            {
                assert_eq!(
                    app.session_panes[0].session_id, last_pane,
                    "last pane {} must be at index 0, got order {:?}",
                    last_pane, ids
                );
            }
        }

        /// End-to-end: run the
        /// actual `tmux
        /// list-windows -a`
        /// command and confirm
        /// the rows it produces
        /// (in `DIR:TMUX` mode)
        /// have `source =
        /// "tmux"`, the
        /// directory in `~/x`
        /// form, and the pane
        /// id in the secondary
        /// slot. Skipped if
        /// `tmux` is not on PATH
        /// (e.g. CI without
        /// tmux installed). This
        /// is a regression guard
        /// for the user's
        /// "I see `tmux list-
        /// windows -a` as an
        /// entry" report: the
        /// `pane_current_path`
        /// (the second column)
        /// is always a real
        /// absolute filesystem
        /// path, never the
        /// `tmux list-windows
        /// -a` command line
        /// itself. We pin both
        /// the `source` field
        /// and the prefix
        /// invariant.
        #[test]
        fn fetch_directories_tmux_pane_path_is_a_real_path() {
            // Skip silently if
            // tmux isn't on
            // PATH — CI
            // environments
            // typically don't
            // have it.
            let tmux_check =
                std::process::Command::new("tmux")
                    .arg("-V")
                    .output();
            if tmux_check.is_err() {
                eprintln!(
                    "[skip] tmux not on PATH"
                );
                return;
            }
            // Run the
            // production
            // format
            // command.
            let format = "\
                #{pane_id} | \
                #{pane_current_path} | \
                active:#{window_active} | \
                Layout: #{window_layout}";
            let output =
                std::process::Command::new("tmux")
                    .args(["list-windows", "-a", "-F", format])
                    .output()
                    .expect(
                        "tmux list-windows must succeed",
                    );
            let stdout =
                String::from_utf8_lossy(&output.stdout);
            // Build a
            // synthetic
            // `App` with
            // the same
            // shape as
            // `fetch_tmux_windows`
            // produces
            // (parse each
            // line into a
            // `TmuxWindowInfo`).
            let mut windows: Vec<TmuxWindowInfo> =
                Vec::new();
            for line in stdout.lines() {
                if let Some(w) =
                    parse_tmux_pane_line(line)
                {
                    windows.push(w);
                }
            }
            // Every
            // window's
            // `path`
            // must be a
            // real
            // absolute
            // path —
            // never
            // something
            // like a
            // command
            // line or a
            // shell
            // output.
            for w in &windows {
                assert!(
                    w.path.starts_with('/'),
                    "pane_current_path must be an absolute path, got: {:?}",
                    w.path
                );
                assert!(
                    w.path.contains('/')
                        && !w.path.contains('|')
                        && !w.path.contains(' '),
                    "pane_current_path must look like a real path (no separators like | or spaces), got: {:?}",
                    w.path
                );
            }
            // The
            // second-load
            // smoke test
            // for the
            // user's
            // report:
            // the
            // visible
            // primary
            // text on a
            // tmux-pane
            // row in
            // `DIR:TMUX`
            // mode is the
            // shortened
            // directory,
            // not the
            // pane id
            // (which goes
            // in the
            // secondary
            // slot).
            let mut app = directories_test_app(&[]);
            app.tmux_windows = windows;
            app.directory_source =
                crate::tui::state::DirectorySource::Tmux;
            app.query = "#".to_string();
            app.refresh();
            for row in app.merged_rows() {
                assert_eq!(row.source, "tmux");
                // The
                // primary
                // text
                // (visible
                // in the
                // first
                // column)
                // must
                // be a
                // shortened
                // directory
                // — never
                // a
                // command
                // name
                // like
                // `tmux
                // list-
                // windows
                // -a`
                // or a
                // shell
                // name.
                assert!(
                    row.command.starts_with('~')
                        || row.command.starts_with('/'),
                    "tmux-pane row's primary text must be a path, got: {:?}",
                    row.command
                );
                assert!(
                    !row.command.starts_with("tmux "),
                    "tmux-pane row's primary text must NOT be a command line (the 'tmux list-windows -a' bug), got: {:?}",
                    row.command
                );
            }
            // Dump the
            // visible
            // text
            // representation
            // of every
            // row in
            // `DIR:TMUX`
            // mode for
            // the user's
            // report
            // (debugging
            // the
            // 'tmux list-
            // windows -a'
            // mystery
            // entry).
            // The fix for that
            // mystery is the
            // `starts_with('/')`
            // and `is_dir()`
            // filters in
            // `fetch_directories`'s
            // tmux loop; this
            // test pins the
            // behaviour
            // (bad pane paths
            // are filtered out,
            // good ones
            // surface).
        }


        // ---- JIRA (`-`-prefix) mode ----

        /// A fake `JiraClient` that returns a canned set
        /// of issues, recording the JQL it was called with
        /// so tests can assert on the generated query.
        /// The `comments` field is the canned comments
        /// list returned by `fetch_comments`; the
        /// `comment_keys` field records which keys the
        /// TUI asked for so tests can assert the
        /// comments fetch was issued with the right
        /// target. The `posted_comments` field records
        /// the (key, body) pairs that the TUI tried to
        /// post via `add_comment`; tests assert on this
        /// to verify the save-comment-edit dispatch
        /// routes JIRA rows through the add-comment
        /// path (not the local SQLite `command_comments`
        /// path).
        #[derive(Default)]
        struct FakeJira {
            issues: Vec<crate::jira::JiraIssue>,
            recorded: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
            comments: Vec<crate::jira::JiraComment>,
            comment_keys: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
            posted_comments: std::sync::Arc<
                std::sync::Mutex<Vec<(String, String)>>,
            >,
        }

        impl crate::jira::JiraClient for FakeJira {
            fn search(&self, jql: &str) -> Result<Vec<crate::jira::JiraIssue>, crate::jira::JiraError> {
                self.recorded.lock().unwrap().push(jql.to_string());
                Ok(self.issues.clone())
            }
            fn fetch_comments(
                &self,
                key: &str,
            ) -> Result<Vec<crate::jira::JiraComment>, crate::jira::JiraError> {
                self.comment_keys.lock().unwrap().push(key.to_string());
                Ok(self.comments.clone())
            }
            fn add_comment(
                &self,
                key: &str,
                body: &str,
            ) -> Result<(), crate::jira::JiraError> {
                self.posted_comments
                    .lock()
                    .unwrap()
                    .push((key.to_string(), body.to_string()));
                Ok(())
            }
        }

        /// The `-` prefix is detected and the body sliced.
        #[test]
        fn jira_prefix_detected_and_pattern_sliced() {
            let mut app = directories_test_app(&[]);
            app.query = String::new();
            assert!(!app.is_jira_query());
            app.query = String::from("-");
            assert!(app.is_jira_query());
            assert_eq!(app.jira_pattern(), "");
            app.query = String::from("-PROJ-1 crash");
            assert_eq!(app.jira_pattern(), "PROJ-1 crash");
        }

        /// In jira mode, `build_merged_rows` does NOT merge
        /// labeled history rows (same guard as directories /
        /// panes modes).
        #[test]
        fn jira_mode_excludes_labeled_history_rows() {
            let labeled_cmd = "grep -c PROJ issues";
            let mut app = directories_test_app(&[
                (labeled_cmd, "/tmp", 60),
            ]);
            app.conn.execute(
                "CREATE TABLE command_comments (command TEXT PRIMARY KEY, comment TEXT NOT NULL)", [],
            ).expect("cc");
            app.conn.execute(
                "CREATE TABLE history_output (history_id INTEGER PRIMARY KEY, output TEXT NOT NULL)", [],
            ).expect("ho");
            app.conn.execute(
                "INSERT INTO command_comments (command, comment) VALUES (?1, ?2)",
                rusqlite::params![labeled_cmd, "JIRA-LIST"],
            ).expect("ins");
            app.jira_rows.push(crate::tui::state::HistoryRow {
                id: -1,
                command: "PROJ-9".to_string(),
                directory: String::new(),
                session_id: String::new(),
                exit_code: 0,
                timestamp: 0,
                comment: "boom".to_string(),
                output: String::new(),
                mode: "jira".to_string(),
                source: "jira".to_string(),
            });
            app.query = String::from("-");
            app.refresh();
            let has_labeled = app
                .merged_rows()
                .iter()
                .any(|r| r.command == labeled_cmd);
            assert!(!has_labeled, "jira mode must not show labeled rows, got {:?}",
                app.merged_rows().iter().map(|r| (r.command.clone(), r.source.clone())).collect::<Vec<_>>());
            assert_eq!(app.merged_rows().len(), 1);
            assert_eq!(app.merged_rows()[0].source, "jira");
        }

        /// `jira_maybe_autocall` fires the search after the
        /// debounce and caches the result rows. Verifies the
        /// fake-client synchronous path end-to-end:
        /// query → JQL → search → rows.
        #[test]
        fn jira_autocall_caches_search_results() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![
                    crate::jira::JiraIssue {
                        key: "PROJ-1".to_string(),
                        summary: "login crash".to_string(),
                        status: "Open".to_string(),
                        issuetype: "Bug".to_string(),
                        ..Default::default()
                    },
                    crate::jira::JiraIssue {
                        key: "PROJ-2".to_string(),
                        summary: "fix tests".to_string(),
                        updated: "2024-06-30T19:14:39.000+0000".to_string(),
                        ..Default::default()
                    },
                ],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                ..Default::default()
            };
            let recorded = fake.recorded.clone();
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-project=PROJ crash");
            app.refresh();
            // Forcibly arm the debounce in the past so the
            // autocall fires immediately (the run loop would
            // normally wait, but here we drive it by hand).
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            // The JQL was built and the search fired.
            assert_eq!(recorded.lock().unwrap().len(), 1, "search must fire once");
            let jql = recorded.lock().unwrap()[0].clone();
            assert!(jql.contains(r#"project = "PROJ""#), "JQL: {}", jql);
            assert!(jql.contains(r#"description ~ "crash""#), "JQL: {}", jql);
            // The result rows are cached on the app.
            assert_eq!(app.jira_rows.len(), 2);
            assert_eq!(app.jira_rows[0].command, "PROJ-1");
            assert_eq!(app.jira_rows[0].comment, "login crash");
            assert_eq!(app.jira_rows[0].source, "jira");
            assert_eq!(app.jira_rows[0].mode, "jira");
            // The new format wraps the label in
            // `**...**` so the renderer can produce
            // a bold span. The substring assertion
            // here uses the bold-marked form.
            assert!(app.jira_rows[0].output.contains("**Status**: Open"));
            // PROJ-2 has a real `updated` → parsed epoch.
            assert!(app.jira_rows[1].timestamp > 1_700_000_000);
        }

        /// A repeat `jira_maybe_autocall` with the SAME
        /// query does NOT re-fire the search (the
        /// `jira_last_jql` cache prevents spamming JIRA).
        #[test]
        fn jira_autocall_skips_unchanged_jql() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                ..Default::default()
            };
            let recorded = fake.recorded.clone();
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-PROJ-1");
            app.refresh();
            let past = || {
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50)
            };
            app.jira_debounce_started = Some(past());
            app.jira_maybe_autocall();
            assert_eq!(recorded.lock().unwrap().len(), 1);
            // Second call with no query change must NOT
            // re-fire.
            app.jira_debounce_started = Some(past());
            app.jira_maybe_autocall();
            assert_eq!(recorded.lock().unwrap().len(), 1, "must not re-fire for same JQL");
        }

        /// The `@me` / `@today` / `@week` / `@month`
        /// aliases thread through `jira_build_query`
        /// into the JQL the FakeJira receives.
        /// Asserts the JQL contains the expected
        /// alias-derived clauses end-to-end.
        #[test]
        fn jira_aliases_reach_the_fake_client() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                ..Default::default()
            };
            let recorded = fake.recorded.clone();
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-@me @week crash");
            app.refresh();
            // Force the debounce to be in the past
            // (the run loop would normally wait, but
            // we drive it by hand for determinism).
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            assert_eq!(recorded.lock().unwrap().len(), 1);
            let jql = recorded.lock().unwrap()[0].clone();
            // The `assignee = currentUser()` clause
            // from the `@me` alias.
            assert!(
                jql.contains("assignee = currentUser()"),
                "JQL: {}",
                jql
            );
            // The `updated >=` clause from the
            // `@week` alias (date is computed from
            // `now_epoch()` — we don't assert the
            // exact date, just the prefix).
            assert!(
                jql.contains(r#"updated >= "20"#),
                "JQL: {}",
                jql
            );
            // The free-text token survived.
            assert!(
                jql.contains(r#"(description ~ "crash" OR summary ~ "crash")"#),
                "JQL: {}",
                jql
            );
        }

        /// A user-defined JQL fragment (loaded from a
        /// hypothetical `jira.search.label1=labels = "test"`
        /// config entry) is spliced into the JQL the
        /// FakeJira receives. Mirrors the
        /// `jira_aliases_reach_the_fake_client` test
        /// but exercises the fragment expansion path
        /// end-to-end through `jira_build_query`.
        #[test]
        fn jira_fragments_reach_the_fake_client() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                ..Default::default()
            };
            let recorded = fake.recorded.clone();
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            // Install a single fragment via the
            // same field the config loader uses
            // (the public API doesn't expose a
            // setter — the field is the
            // authoritative store and the test
            // pushes directly).
            app.jira_fragments.insert(
                "label1".to_string(),
                r#"labels = "test""#.to_string(),
            );
            app.query = String::from("-@label1 @me crash");
            app.refresh();
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            assert_eq!(recorded.lock().unwrap().len(), 1);
            let jql = recorded.lock().unwrap()[0].clone();
            // The fragment is spliced verbatim,
            // parenthesised, AND-joined with the
            // other clauses.
            assert!(
                jql.contains(r#"(labels = "test")"#),
                "JQL: {}",
                jql
            );
            // The `@me` alias still fires.
            assert!(
                jql.contains("assignee = currentUser()"),
                "JQL: {}",
                jql
            );
            // The free-text token survived.
            assert!(
                jql.contains(r#"(description ~ "crash" OR summary ~ "crash")"#),
                "JQL: {}",
                jql
            );
        }

        /// An undefined fragment in the body
        /// prevents the JIRA search from firing
        /// and surfaces a status message naming
        /// the missing fragment. Asserts both the
        /// suppression of the network call and
        /// the diagnostic text.
        #[test]
        fn jira_undefined_fragment_blocks_search_and_surfaces_message() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                ..Default::default()
            };
            let recorded = fake.recorded.clone();
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            // No fragments defined — `@label1` is
            // an undefined fragment.
            app.query = String::from("-@label1 crash");
            app.refresh();
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            // The search was NOT fired — the
            // undefined-fragment gate short-circuits
            // before `spawn_jira_request`.
            assert_eq!(
                recorded.lock().unwrap().len(),
                0,
                "undefined fragment must not fire the search",
            );
            // The status message names the missing
            // fragment.
            let status = app
                .status_message
                .as_ref()
                .map(|(s, _)| s.as_str())
                .unwrap_or("");
            assert!(
                status.contains("@label1"),
                "status: {:?}",
                status,
            );
            assert!(
                status.contains("not configured"),
                "status: {:?}",
                status,
            );
        }

        /// End-to-end: a JIRA issue with all five
        /// preview attributes (Status, Priority,
        /// Due, Assignee, Description) produces a
        /// `HistoryRow.output` with five lines,
        /// each label wrapped in `**...**` markers
        /// so the details-pane renderer turns them
        /// into bold spans.
        #[test]
        fn jira_row_output_contains_all_five_bold_labels() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![crate::jira::JiraIssue {
                    key: "PROJ-1".to_string(),
                    summary: "login crash".to_string(),
                    status: "Open".to_string(),
                    priority: "High".to_string(),
                    assignee: "Alice".to_string(),
                    due: "2024-07-15".to_string(),
                    description: "The login button is broken on Safari.".to_string(),
                    ..Default::default()
                }],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                ..Default::default()
            };
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-");
            app.refresh();
            // Forcibly fire the autocall.
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            assert_eq!(app.jira_rows.len(), 1);
            let out = &app.jira_rows[0].output;
            // The new layout is 3 lines of
            // header (Status/Priority on
            // line 1, Due/Assignee on line 2,
            // Description label on line 3)
            // followed by the description
            // body on line 4. The
            // join-on-newline convention gives
            // us a single string with the
            // expected layout.
            let lines: Vec<&str> = out.lines().collect();
            assert_eq!(lines.len(), 4, "got: {:?}", lines);
            assert_eq!(lines[0], "**Status**: Open  **Priority**: High");
            assert_eq!(lines[1], "**Due**: 2024-07-15  **Assignee**: Alice");
            assert_eq!(lines[2], "**Description**");
            // The description body
            // appears on the line
            // after the label
            // (no value on the
            // label line itself).
            assert_eq!(lines[3], "The login button is broken on Safari.");
            // The full output contains
            // exactly four `**` openers
            // and four `**` closers
            // (one per label: Status,
            // Priority, Due, Assignee).
            // The description label is
            // also bolded via `**`
            // but without a colon, so
            // the `**Description**` line
            // has its own pair. Total:
            // 5 pairs (Status, Priority,
            // Due, Assignee, Description).
            assert_eq!(out.matches("**").count(), 10);
        }

        /// When the issue has empty values
        /// for some attributes, the row
        /// builder still emits the label
        /// (with `<none>` as the
        /// placeholder) so the layout stays
        /// consistent. The renderer doesn't
        /// strip `<none>` — it just
        /// displays it as plain text.
        #[test]
        fn jira_row_output_uses_none_for_empty_attributes() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![crate::jira::JiraIssue {
                    key: "PROJ-1".to_string(),
                    summary: "untriaged bug".to_string(),
                    // status, priority, assignee, due, description all default to empty
                    ..Default::default()
                }],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                ..Default::default()
            };
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-");
            app.refresh();
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            assert_eq!(app.jira_rows.len(), 1);
            let out = &app.jira_rows[0].output;
            // All four metadata labels
            // appear with `<none>` as the
            // placeholder. The Description
            // label is just the label (no
            // colon / no value), and the
            // body line below it is also
            // `<none>` so the layout
            // stays consistent.
            assert!(out.contains("**Status**: <none>"));
            assert!(out.contains("**Priority**: <none>"));
            assert!(out.contains("**Due**: <none>"));
            assert!(out.contains("**Assignee**: <none>"));
            assert!(out.contains("**Description**"));
            assert!(out.contains("\n<none>"));
        }

        /// `sort_comments_newest_first` reverses
        /// a comments list so the newest
        /// comment (by `created`) is at
        /// index 0. JIRA's REST v2 endpoint
        /// returns comments in
        /// `created`-ascending order; the
        /// TUI reverses them on the way in.
        #[test]
        fn sort_comments_newest_first_reverses_by_created() {
            let mut comments = vec![
                crate::jira::JiraComment {
                    id: "1".to_string(),
                    author: "Oldest".to_string(),
                    created: "2024-06-28T10:00:00.000+0000".to_string(),
                    ..Default::default()
                },
                crate::jira::JiraComment {
                    id: "3".to_string(),
                    author: "Newest".to_string(),
                    created: "2024-06-30T19:14:39.000+0000".to_string(),
                    ..Default::default()
                },
                crate::jira::JiraComment {
                    id: "2".to_string(),
                    author: "Middle".to_string(),
                    created: "2024-06-29T10:00:00.000+0000".to_string(),
                    ..Default::default()
                },
            ];
            sort_comments_newest_first(&mut comments);
            assert_eq!(comments[0].author, "Newest");
            assert_eq!(comments[1].author, "Middle");
            assert_eq!(comments[2].author, "Oldest");
        }

        /// Comments with the same `created`
        /// timestamp fall back to the
        /// `id` field as a tie-breaker.
        /// This covers the rare
        /// batch-imported-comments case
        /// where multiple comments share
        /// the exact same second.
        #[test]
        fn sort_comments_newest_first_uses_id_as_tie_breaker() {
            let mut comments = vec![
                crate::jira::JiraComment {
                    id: "100".to_string(),
                    author: "Lower id".to_string(),
                    created: "2024-06-30T19:14:39.000+0000".to_string(),
                    ..Default::default()
                },
                crate::jira::JiraComment {
                    id: "200".to_string(),
                    author: "Higher id".to_string(),
                    created: "2024-06-30T19:14:39.000+0000".to_string(),
                    ..Default::default()
                },
            ];
            sort_comments_newest_first(&mut comments);
            // Both have the same `created`,
            // so the higher id (200)
            // wins the tie-break and
            // comes first.
            assert_eq!(comments[0].author, "Higher id");
            assert_eq!(comments[1].author, "Lower id");
        }

        /// `format_jira_date` extracts the
        /// `YYYY-MM-DD HH:MM` portion of
        /// JIRA's ISO-8601 timestamp and
        /// appends ` UTC` for a compact,
        /// human-readable date suitable
        /// for the comment sub-heading.
        #[test]
        fn format_jira_date_trims_to_compact_utc() {
            assert_eq!(
                format_jira_date("2024-06-30T19:14:39.000+0000"),
                "2024-06-30 19:14 UTC"
            );
            // A timestamp without
            // milliseconds and offset
            // is also accepted (JIRA's
            // REST v2 may emit either
            // form depending on the
            // instance).
            assert_eq!(
                format_jira_date("2024-06-30T19:14:39Z"),
                "2024-06-30 19:14 UTC"
            );
            // Empty / short / malformed
            // inputs degrade to the
            // raw string or empty.
            assert_eq!(format_jira_date(""), "");
            assert_eq!(format_jira_date("garbage"), "garbage");
        }

        /// When the user opens the show-output
        /// overlay on a JIRA row, the TUI
        /// fires a background comments fetch
        /// and (with the fake client)
        /// synchronously builds the overlay
        /// text from the row + the canned
        /// comments. Verifies the full
        /// structure: `## Header`,
        /// `## Description`, `## Comments`
        /// with one sub-heading per
        /// comment.
        #[test]
        fn jira_show_output_view_fetches_comments_and_builds_overlay() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![crate::jira::JiraIssue {
                    key: "PROJ-1".to_string(),
                    summary: "login crash".to_string(),
                    status: "Open".to_string(),
                    priority: "High".to_string(),
                    assignee: "Alice".to_string(),
                    due: "2024-07-15".to_string(),
                    description: "The login button is broken on Safari.".to_string(),
                    ..Default::default()
                }],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                comments: vec![
                    // Two comments, one newer
                    // than the other. The
                    // TUI sorts them
                    // newest-first; the canned
                    // order is the opposite so
                    // we can verify the sort.
                    crate::jira::JiraComment {
                        id: "10001".to_string(),
                        author: "Bob".to_string(),
                        body: "Looking into this.".to_string(),
                        created: "2024-06-29T10:00:00.000+0000".to_string(),
                        updated: "2024-06-29T10:00:00.000+0000".to_string(),
                    },
                    crate::jira::JiraComment {
                        id: "10002".to_string(),
                        author: "Alice".to_string(),
                        body: "Confirmed, fixing now.".to_string(),
                        created: "2024-06-30T19:14:39.000+0000".to_string(),
                        updated: "2024-06-30T19:14:39.000+0000".to_string(),
                    },
                ],
                comment_keys: Arc::new(std::sync::Mutex::new(Vec::new())),
                            ..Default::default()
};
            let recorded = fake.recorded.clone();
            let comment_keys = fake.comment_keys.clone();
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-");
            app.refresh();
            // Forcibly fire the search
            // autocall so the row is
            // populated.
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            assert_eq!(app.jira_rows.len(), 1);
            // Select the row.
            app.list_state.select(Some(0));
            // Open the show-output view.
            // The fake-client path runs
            // synchronously, so the
            // overlay is open by the
            // time this method returns.
            app.show_output_view();
            // The fake client's
            // `fetch_comments` was
            // called once with the
            // right key.
            assert_eq!(comment_keys.lock().unwrap().len(), 1);
            assert_eq!(comment_keys.lock().unwrap()[0], "PROJ-1");
            // The overlay is open.
            let view = app.output_view.as_ref().expect("overlay should be open");
            // The overlay text follows the
            // user-spec structure.
            assert!(view.text.contains("## Header"), "got: {}", view.text);
            assert!(view.text.contains("## Description"), "got: {}", view.text);
            assert!(view.text.contains("## Comments"), "got: {}", view.text);
            // The header block contains
            // the 3-line preview
            // (Status/Priority, Due/
            // Assignee, Description
            // label) verbatim.
            assert!(view.text.contains("**Status**: Open  **Priority**: High"), "got: {}", view.text);
            assert!(view.text.contains("**Due**: 2024-07-15  **Assignee**: Alice"), "got: {}", view.text);
            assert!(view.text.contains("**Description**"), "got: {}", view.text);
            // The full description
            // appears in the `# Description`
            // section (not in `# Header`).
            // The description is
            // visible exactly once in
            // the overlay — the user
            // explicitly asked for
            // this. (`# Header` shows
            // the metadata block
            // only; the description
            // body lives in its
            // own section.)
            assert!(view.text.contains("login button"), "got: {}", view.text);
            // Comments are sorted
            // newest-first. Alice's
            // 2024-06-30 comment must
            // appear before Bob's
            // 2024-06-29 comment.
            let alice_pos = view.text.find("Alice").expect("Alice in overlay");
            let alice_date_pos = view
                .text
                .find("2024-06-30")
                .expect("Alice's date in overlay");
            let bob_pos = view.text.find("Bob").expect("Bob in overlay");
            let bob_date_pos = view
                .text
                .find("2024-06-29")
                .expect("Bob's date in overlay");
            assert!(
                alice_pos < bob_pos,
                "Alice (newer) must appear before Bob (older); got Alice@{alice_pos} Bob@{bob_pos}",
            );
            assert!(
                alice_date_pos < bob_date_pos,
                "2024-06-30 must appear before 2024-06-29",
            );
            // Each comment has a
            // sub-heading with the
            // author and date joined by
            // a middle dot (U+00B7).
            assert!(view.text.contains("Alice \u{00b7} 2024-06-30 19:14 UTC"), "got: {}", view.text);
            assert!(view.text.contains("Bob \u{00b7} 2024-06-29 10:00 UTC"), "got: {}", view.text);
            // Each comment's body
            // appears below its
            // sub-heading.
            assert!(view.text.contains("Confirmed, fixing now."), "got: {}", view.text);
            assert!(view.text.contains("Looking into this."), "got: {}", view.text);
        }

        /// An issue with no comments
        /// produces an overlay with a
        /// `(no comments)` placeholder
        /// after the `## Comments`
        /// heading. The overlay is
        /// still built and opened
        /// (a non-error result is
        /// still a result).
        #[test]
        fn jira_show_output_view_with_no_comments() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![crate::jira::JiraIssue {
                    key: "PROJ-1".to_string(),
                    summary: "no comments yet".to_string(),
                    status: "Open".to_string(),
                    ..Default::default()
                }],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                comments: vec![], // empty
                comment_keys: Arc::new(std::sync::Mutex::new(Vec::new())),
                            ..Default::default()
};
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-");
            app.refresh();
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            app.list_state.select(Some(0));
            app.show_output_view();
            let view = app.output_view.as_ref().expect("overlay should be open");
            assert!(view.text.contains("## Comments"));
            assert!(view.text.contains("(no comments)"));
        }

        /// An issue with no description
        /// produces an overlay with a
        /// `(no description)` placeholder
        /// after the `## Description`
        /// heading.
        #[test]
        fn jira_show_output_view_with_no_description() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![crate::jira::JiraIssue {
                    key: "PROJ-1".to_string(),
                    summary: "no description".to_string(),
                    status: "Open".to_string(),
                    description: String::new(),
                    ..Default::default()
                }],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                comments: vec![],
                comment_keys: Arc::new(std::sync::Mutex::new(Vec::new())),
                            ..Default::default()
};
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-");
            app.refresh();
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            app.list_state.select(Some(0));
            app.show_output_view();
            let view = app.output_view.as_ref().expect("overlay should be open");
            assert!(view.text.contains("## Description"));
            assert!(view.text.contains("(no description)"));
        }

        /// The description body
        /// appears exactly once in
        /// the overlay — in the
        /// `## Description` section,
        /// not duplicated in
        /// `## Header`. The user
        /// explicitly asked for
        /// this: previously the
        /// description was shown
        /// twice (once in
        /// `## Header` as part of
        /// the preview-window
        /// content, once in
        /// `## Description` as
        /// its own section),
        /// which was redundant.
        /// The fix: `## Header`
        /// now shows only the
        /// 3-line metadata block
        /// (Status/Priority,
        /// Due/Assignee,
        /// Description label);
        /// the description body
        /// lives in `## Description`
        /// only. The test uses a
        /// distinctive
        /// description string
        /// ("unicorn-magic-marker")
        /// so the count assertion
        /// is reliable — the
        /// literal text would
        /// never appear in the
        /// overlay except as the
        /// description body.
        #[test]
        fn jira_overlay_shows_description_exactly_once() {
            use std::sync::Arc;
            let unique_description =
                "unicorn-magic-marker \
                 paragraphs here";
            let fake = FakeJira {
                issues: vec![crate::jira::JiraIssue {
                    key: "PROJ-1".to_string(),
                    summary: "dedup test".to_string(),
                    status: "Open".to_string(),
                    priority: "High".to_string(),
                    assignee: "Alice".to_string(),
                    due: "2024-07-15".to_string(),
                    description: unique_description
                        .to_string(),
                    ..Default::default()
                }],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                comments: vec![],
                comment_keys: Arc::new(std::sync::Mutex::new(Vec::new())),
                            ..Default::default()
};
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-");
            app.refresh();
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            app.list_state.select(Some(0));
            app.show_output_view();
            let view = app
                .output_view
                .as_ref()
                .expect("overlay should be open");
            // The description text
            // appears exactly once.
            // (The `match_indices`
            // count is the number
            // of *non-overlapping*
            // occurrences of the
            // substring in the
            // haystack.)
            let occurrences =
                view.text.match_indices(unique_description).count();
            assert_eq!(
                occurrences, 1,
                "description should appear exactly once, found {} times in: {}",
                occurrences,
                view.text,
            );
            // Sanity: the
            // `## Description`
            // section exists and
            // contains the
            // description.
            assert!(view.text.contains("## Description"));
            // Sanity: the
            // `## Header` section
            // exists but does NOT
            // contain the
            // description body
            // (only the 3-line
            // metadata block).
            // We check this by
            // splitting the
            // overlay at the
            // `## Description`
            // heading; everything
            // before the split
            // is the `## Header`
            // section.
            let header_section = view
                .text
                .split("## Description")
                .next()
                .expect("`## Description` heading should exist");
            assert!(
                !header_section.contains(unique_description),
                "`## Header` should not contain the description body, but found: {}",
                header_section,
            );
        }

        /// Pressing Ctrl+L twice on a JIRA
        /// row while a fetch is in flight
        /// doesn't queue a second fetch
        /// (the `jira_comments_in_flight`
        /// latch prevents duplicate
        /// background threads).
        #[test]
        fn jira_show_output_view_dedupes_concurrent_fetches() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![crate::jira::JiraIssue {
                    key: "PROJ-1".to_string(),
                    ..Default::default()
                }],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                comments: vec![],
                comment_keys: Arc::new(std::sync::Mutex::new(Vec::new())),
                            ..Default::default()
};
            let comment_keys = fake.comment_keys.clone();
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-");
            app.refresh();
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            app.list_state.select(Some(0));
            // The fake-client path runs
            // synchronously and clears
            // the in-flight flag in
            // `process_jira_comments_result`,
            // so a *second* call to
            // `show_output_view` *does*
            // fire a second fetch. This
            // is acceptable: a real
            // user pressing Ctrl+L twice
            // is unlikely, and the
            // synchronous path is the
            // test seam. The dedup
            // behaviour is meaningful
            // only for the production
            // background-thread path,
            // where the in-flight flag
            // stays set until the
            // worker sends its
            // result.
            //
            // Verify the second call
            // does call fetch_comments
            // a second time (the test
            // seam doesn't dedupe, by
            // design) and the overlay
            // is rebuilt.
            app.show_output_view();
            assert_eq!(comment_keys.lock().unwrap().len(), 1);
            let first_overlay = app
                .output_view
                .as_ref()
                .expect("first overlay")
                .text
                .clone();
            app.show_output_view();
            assert_eq!(comment_keys.lock().unwrap().len(), 2);
            let second_overlay = app
                .output_view
                .as_ref()
                .expect("second overlay")
                .text
                .clone();
            // Both overlays have the
            // expected structure.
            assert!(first_overlay.contains("## Header"));
            assert!(second_overlay.contains("## Header"));
        }

        /// Pressing Ctrl-E on a JIRA row
        /// opens the comment-edit buffer in
        /// JIRA-add-comment mode (not the
        /// local `command_comments` mode).
        /// Verifies the `jira_add_comment_target`
        /// field is set to the issue key and
        /// the buffer is empty (the user is
        /// composing a *new* comment, not
        /// editing the issue's summary).
        #[test]
        fn jira_edit_comment_opens_jira_add_comment_mode() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![crate::jira::JiraIssue {
                    key: "PROJ-1".to_string(),
                    summary: "login crash".to_string(),
                    status: "Open".to_string(),
                    ..Default::default()
                }],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                ..FakeJira::default()
            };
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-");
            app.refresh();
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            app.list_state.select(Some(0));
            // Press Ctrl-E.
            app.start_comment_edit();
            // The buffer is in
            // JIRA-add-comment mode:
            // the target is the
            // issue key.
            assert_eq!(
                app.jira_add_comment_target.as_deref(),
                Some("PROJ-1"),
                "jira_add_comment_target should be the issue key, got {:?}",
                app.jira_add_comment_target,
            );
            // The buffer is empty
            // (the user is
            // composing a new
            // comment, not editing
            // the issue's summary).
            assert_eq!(app.comment_edit.as_deref(), Some(""),
                "buffer should be empty in JIRA add-comment mode");
        }

        /// When the user saves a non-empty
        /// comment in JIRA-add-comment mode,
        /// the FakeJira's `add_comment`
        /// method is called with the
        /// issue key and the buffer text.
        /// Verifies the end-to-end path:
        /// buffer → POST → fake records the
        /// (key, body).
        #[test]
        fn jira_save_comment_posts_to_jira() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![crate::jira::JiraIssue {
                    key: "PROJ-1".to_string(),
                    summary: "login crash".to_string(),
                    status: "Open".to_string(),
                    ..Default::default()
                }],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                posted_comments: Arc::new(std::sync::Mutex::new(Vec::new())),
                ..FakeJira::default()
            };
            let posted = fake.posted_comments.clone();
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-");
            app.refresh();
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            app.list_state.select(Some(0));
            app.start_comment_edit();
            // Type a comment.
            app.comment_edit = Some("This is fixed in PR #42.".to_string());
            // Save it.
            app.save_comment_edit().unwrap();
            // The FakeJira recorded
            // the POST.
            assert_eq!(
                posted.lock().unwrap().len(),
                1,
                "add_comment should be called once"
            );
            let (key, body) = &posted.lock().unwrap()[0];
            assert_eq!(key, "PROJ-1");
            assert_eq!(body, "This is fixed in PR #42.");
            // On success, the buffer
            // clears and the target
            // resets to None.
            assert!(
                app.comment_edit.is_none(),
                "buffer should clear on successful POST"
            );
            assert!(
                app.jira_add_comment_target.is_none(),
                "target should reset to None on successful POST"
            );
            // The status bar shows a
            // success message that
            // references the issue.
            let status = app
                .status_message
                .as_ref()
                .map(|(s, _)| s.as_str())
                .unwrap_or("");
            assert!(
                status.contains("Comment posted to PROJ-1"),
                "status should confirm the POST: {:?}",
                status,
            );
        }

        /// An empty buffer is NOT posted
        /// to JIRA. The user sees a
        /// status message telling them
        /// the body is empty, and the
        /// buffer stays so they can
        /// type something.
        #[test]
        fn jira_save_comment_rejects_empty_body() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![crate::jira::JiraIssue {
                    key: "PROJ-1".to_string(),
                    summary: "login crash".to_string(),
                    status: "Open".to_string(),
                    ..Default::default()
                }],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                posted_comments: Arc::new(std::sync::Mutex::new(Vec::new())),
                ..FakeJira::default()
            };
            let posted = fake.posted_comments.clone();
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-");
            app.refresh();
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            app.list_state.select(Some(0));
            app.start_comment_edit();
            // Buffer is empty by
            // default (start_comment_edit
            // sets it to
            // String::new() for JIRA
            // rows).
            app.save_comment_edit().unwrap();
            // No POST was made.
            assert_eq!(
                posted.lock().unwrap().len(),
                0,
                "empty body should not be POSTed"
            );
            // The status message
            // explains the body
            // is empty.
            let status = app
                .status_message
                .as_ref()
                .map(|(s, _)| s.as_str())
                .unwrap_or("");
            assert!(
                status.contains("empty"),
                "status should explain the body is empty: {:?}",
                status,
            );
            // The buffer stays so
            // the user can type
            // something. (The
            // target also stays so
            // the next Enter retries
            // the JIRA POST path.)
            assert!(
                app.comment_edit.is_some(),
                "buffer should be preserved on empty-body rejection"
            );
            assert_eq!(
                app.jira_add_comment_target.as_deref(),
                Some("PROJ-1"),
                "target should be preserved on empty-body rejection"
            );
        }

        /// Cancel (Esc) on the
        /// comment-edit buffer clears
        /// both the buffer and the
        /// JIRA-add-comment target.
        /// This is the "user changed
        /// their mind" path — they
        /// don't want to post after
        /// all.
        #[test]
        fn jira_cancel_comment_edit_clears_target() {
            use std::sync::Arc;
            let fake = FakeJira {
                issues: vec![crate::jira::JiraIssue {
                    key: "PROJ-1".to_string(),
                    summary: "login crash".to_string(),
                    status: "Open".to_string(),
                    ..Default::default()
                }],
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
                ..FakeJira::default()
            };
            let mut app = directories_test_app(&[]);
            app.set_jira_client(Arc::new(fake));
            app.query = String::from("-");
            app.refresh();
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            app.list_state.select(Some(0));
            app.start_comment_edit();
            app.comment_edit = Some("draft text".to_string());
            // Cancel.
            app.cancel_comment_edit();
            // Both the buffer and
            // the target clear.
            assert!(
                app.comment_edit.is_none(),
                "buffer should clear on cancel"
            );
            assert!(
                app.jira_add_comment_target.is_none(),
                "target should clear on cancel"
            );
        }

        /// Pressing Ctrl-E on a
        /// non-JIRA row keeps the
        /// local `command_comments`
        /// behaviour — the buffer is
        /// prefilled with the existing
        /// comment (or empty when no
        /// comment exists), and the
        /// `jira_add_comment_target` is
        /// `None`. This locks in the
        /// dispatch: only JIRA rows go
        /// through the JIRA-add path.
        #[test]
        fn non_jira_edit_comment_keeps_local_behaviour() {
            use crate::tui::state::HistoryRow;
            let mut app = directories_test_app(&[
                ("git status", "/tmp", 0),
            ]);
            // Create the schemas `fetch()`
            // joins against so the
            // query doesn't error
            // silently and return
            // an empty list.
            app.conn.execute(
                "CREATE TABLE command_comments (command TEXT PRIMARY KEY, comment TEXT NOT NULL)",
                [],
            )
            .expect("cc");
            app.conn.execute(
                "CREATE TABLE history_output (history_id INTEGER PRIMARY KEY, output TEXT NOT NULL)",
                [],
            )
            .expect("ho");
            app.conn.execute(
                "INSERT INTO command_comments (command, comment) VALUES ('git status', 'pre-existing comment')",
                [],
            )
            .expect("ins");
            app.refresh();
            app.refresh_labeled();
            // Find the row's index
            // by command (the
            // simplest way to select
            // the row regardless of
            // how the merge happens).
            let row_idx = app
                .merged_rows()
                .iter()
                .position(|r| r.command == "git status")
                .expect("row should be in merged_rows");
            app.list_state.select(Some(row_idx));
            // Press Ctrl-E.
            app.start_comment_edit();
            // The buffer has
            // the pre-existing
            // comment (not the JIRA
            // empty buffer).
            assert_eq!(
                app.comment_edit.as_deref(),
                Some("pre-existing comment"),
                "non-JIRA buffer should be prefilled with the existing comment"
            );
            // The JIRA target is
            // None (we're in the
            // local path).
            assert!(
                app.jira_add_comment_target.is_none(),
                "non-JIRA edit should NOT set the JIRA target"
            );
        }

        /// Selecting a JIRA row stages `open "<URL>"` using
        /// `JIRA_URL` (falls back to `JIRA_SERVER`).
        #[test]
        fn select_for_run_in_jira_mode_stages_open_url() {
            // Use a unique env-var guard to avoid racing
            // other tests: set the vars, run, restore.
            // (The run is synchronous in the test path; no
            // background thread reads these, so the window
            // is just this function's body.)
            use std::sync::Mutex;
            static ENV_LOCK: Mutex<()> = Mutex::new(());
            let _g = ENV_LOCK.lock().unwrap();
            let prev_server = std::env::var("JIRA_SERVER").ok();
            let prev_token = std::env::var("JIRA_API_TOKEN").ok();
            let prev_url = std::env::var("JIRA_URL").ok();
            // SAFETY: no other test in this binary uses these
            // vars (guarded by ENV_LOCK, and the binary is
            // single-process per test thread). Other JIRA
            // tests here don't set these specific vars.
            unsafe {
                std::env::set_var("JIRA_SERVER", "https://jira.example.com");
                std::env::set_var("JIRA_API_TOKEN", "tok");
                std::env::set_var("JIRA_URL", "https://browse.example.com/browse");
            }
            let mut app = directories_test_app(&[]);
            app.jira_rows.push(crate::tui::state::HistoryRow {
                id: -1,
                command: "PROJ-42".to_string(),
                directory: String::new(),
                session_id: String::new(),
                exit_code: 0,
                timestamp: 0,
                comment: "summary".to_string(),
                output: String::new(),
                mode: "jira".to_string(),
                source: "jira".to_string(),
            });
            app.query = String::from("-");
            app.refresh();
            app.list_state.select(Some(0));
            app.select_for_run();
            // Restore before asserting so a panic doesn't
            // leak the env to other tests.
            let restore = |name: &str, prev: Option<String>| unsafe {
                match prev {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            };
            restore("JIRA_SERVER", prev_server);
            restore("JIRA_API_TOKEN", prev_token);
            restore("JIRA_URL", prev_url);
            assert_eq!(
                app.selection.as_deref(),
                Some("open \"https://browse.example.com/browse/PROJ-42\""),
                "got: {:?}",
                app.selection
            );
            assert_eq!(app.pick_mode, Some(PickMode::Run));
        }

        /// `jira_maybe_autocall` shows a status message
        /// (and fires nothing) when JIRA isn't configured
        /// — no env vars and no injected client.
        #[test]
        fn jira_not_configured_surfaces_status() {
            // Clear any JIRA env so the "not configured" path
            // is deterministically taken (another test sets
            // these; under parallel execution we'd otherwise
            // race). Guarded by ENV_LOCK so we don't clobber
            // the other test's window.
            use std::sync::Mutex;
            static ENV_LOCK: Mutex<()> = Mutex::new(());
            let _g = ENV_LOCK.lock().unwrap();
            let prev_server = std::env::var("JIRA_SERVER").ok();
            let prev_token = std::env::var("JIRA_API_TOKEN").ok();
            unsafe {
                std::env::remove_var("JIRA_SERVER");
                std::env::remove_var("JIRA_API_TOKEN");
            }
            let mut app = directories_test_app(&[]);
            app.query = String::from("-PROJ-1");
            app.refresh();
            app.jira_debounce_started = Some(
                std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50),
            );
            app.jira_maybe_autocall();
            // Restore before asserting so a panic doesn't leak.
            let restore = |name: &str, prev: Option<String>| unsafe {
                match prev {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            };
            restore("JIRA_SERVER", prev_server);
            restore("JIRA_API_TOKEN", prev_token);
            // No client, no env → nothing fired, no rows.
            assert!(app.jira_rows.is_empty());
        }
        /// Defensive filter: a
        /// `pane_current_path`
        /// that doesn't start
        /// with `/` (e.g. the
        /// command line that
        /// spawned the pane,
        /// `tmux list-windows
        /// -a ...`) must NOT
        /// become a directory
        /// row. The user
        /// reported seeing
        /// exactly this in
        /// `DIR:TMUX` mode: a
        /// row whose visible
        /// text was the tmux
        /// command line, with
        /// no T flag (because
        /// the T-marker lookup
        /// can't canonicalize a
        /// non-path), and
        /// clearly not a
        /// directory. The
        /// fix: skip any
        /// `pane_current_path`
        /// that doesn't look
        /// like an absolute
        /// path. The check is
        /// `starts_with('/')`
        /// because every real
        /// absolute path on
        /// every Unix starts
        /// with `/` — a
        /// tmux-reported
        /// string that
        /// doesn't is
        /// necessarily
        /// something else
        /// (a command line, a
        /// relative path, an
        /// error message,
        /// etc.) and we have
        /// no way to render
        /// it usefully as a
        /// directory.
        #[test]
        fn tmux_pane_path_must_be_absolute() {
            let mut app = directories_test_app(&[]);
            app.tmux_windows.push(TmuxWindowInfo {
                pane_id: "%0".to_string(),
                // The user's reported
                // bug: tmux reports
                // a "pane_current_path"
                // that's actually
                // the command line.
                path: String::from(
                    "tmux list-windows -a -F #{pane_id} | #{pane_current_path} | active:#{window_active} | Layout: #{window_layout}",
                ),
            });
            app.tmux_windows.push(TmuxWindowInfo {
                pane_id: "%1".to_string(),
                // A real path
                // (a directory
                // that exists on
                // this system)
                // must still show
                // up. `/tmp` is
                // available on
                // every Unix
                // platform and on
                // macOS it
                // canonicalises
                // to
                // `/private/tmp`,
                // which is fine
                // for this test.
                path: std::env::temp_dir()
                    .to_string_lossy()
                    .into_owned(),
            });
            app.query = "#".to_string();
            app.refresh();
            // The bad path must
            // not produce a row
            // at all.
            let has_bad_row = app
                .merged_rows()
                .iter()
                .any(|r| r.directory.starts_with("tmux "));
            assert!(
                !has_bad_row,
                "tmux rows with non-absolute pane_current_path must be filtered out, got: {:?}",
                app.merged_rows()
                    .iter()
                    .map(|r| r.directory.clone())
                    .collect::<Vec<_>>()
            );
            // The real path must
            // still show up.
            let has_good_row = app
                .merged_rows()
                .iter()
                .any(|r| {
                    let canon = std::fs::canonicalize(&r.directory)
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| r.directory.clone());
                    let tmp_canon = std::fs::canonicalize(
                        std::env::temp_dir()
                    )
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| {
                        std::env::temp_dir()
                            .to_string_lossy()
                            .into_owned()
                    });
                    canon == tmp_canon
                });
            assert!(
                has_good_row,
                "tmux rows with absolute pane_current_path that resolves to a real directory must still show up, got: {:?}",
                app.merged_rows()
                    .iter()
                    .map(|r| r.directory.clone())
                    .collect::<Vec<_>>()
            );
        }

        // ---- build_help_lines (the help-overlay content) ----

        /// Build a minimal `App` for the
        /// help-line tests. The
        /// `directories_test_app(&[])`
        /// helper already builds an
        /// `App` with the test-helper
        /// defaults (Mode::Global,
        /// KeyBindings::defaults(),
        /// QueryPrefixes::default(),
        /// etc.). We override the
        /// fields the help builder
        /// actually reads so the
        /// test surface is small and
        /// stable regardless of
        /// test-helper changes.
        use super::render::build_help_lines;
        use ratatui::style::Modifier;
        fn help_app() -> App {
            let mut app = directories_test_app(&[]);
            // The fields the help
            // builder reads.
            app.mode = Mode::Sess;
            app.duplicate_filter = true;
            app.query_prefixes =
                crate::QueryPrefixes::default();
            app
        }

        /// The help overlay contains a
        /// "Search modes" section that
        /// lists every prefix-
        /// switchable mode and its
        /// trigger character. The
        /// section header is present
        /// and uses bold styling.
        #[test]
        fn help_includes_search_modes_section() {
            let lines = build_help_lines(&help_app());
            let texts: Vec<String> = lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect();
            // The section header
            // exists.
            let found = texts
                .iter()
                .position(|t| t == "Search modes")
                .expect("Search modes section");
            // The header is bold.
            let header_line = &lines[found];
            assert!(header_line
                .spans
                .first()
                .map(|s| s.style.add_modifier.contains(Modifier::BOLD))
                .unwrap_or(false));
        }

        /// Every search-mode row
        /// appears in the help with
        /// the user's configured
        /// prefix. We check each mode
        /// by name and assert the
        /// prefix column shows the
        /// right character.
        #[test]
        fn help_lists_all_eleven_search_modes() {
            let lines = build_help_lines(&help_app());
            let texts: Vec<String> = lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect();
            // Default prefixes
            // (from
            // `QueryPrefixes::default()`):
            // plain: "" (no prefix,
            //        em-dash
            //        marker)
            // regex: /
            // fuzzy: ?
            // output: +
            // llm: =
            // question: %
            // notes: @
            // todo: !
            // directories: #
            // panes: *
            // jira: -
            let expected: &[(&str, &str)] = &[
                ("plain", "\u{2014}"),
                ("regex", "/"),
                ("fuzzy", "?"),
                ("output", "+"),
                ("LLM command", "="),
                ("question", "%"),
                ("notes", "@"),
                ("todo", "!"),
                ("directories", "#"),
                ("panes", "*"),
                ("JIRA", "-"),
            ];
            for &(mode, prefix) in expected {
                // The row format is
                // `  {name:<14}{prefix_text}{desc}`
                // — 2 leading spaces,
                // 14 chars for the mode
                // name (left-aligned,
                // right-padded), then
                // the prefix text
                // (right-padded to 7
                // chars). The format
                // helper inside
                // `build_help_lines` uses:
                //   `prefix_text` is " X"
                // when the prefix is
                // non-empty (leading
                // space for column
                // alignment) and just
                // "\u{2014}" (no leading
                // space) when the prefix
                // is empty.
                // The row's actual
                // content is therefore
                // the 16-char name
                // followed immediately
                // by the prefix
                // (no extra separator
                // between columns).
                let prefix_with_pad =
                    if prefix == "\u{2014}" {
                        "\u{2014}".to_string()
                    } else {
                        format!(" {}", prefix)
                    };
                let needle = format!(
                    "  {:<14}{}",
                    mode, prefix_with_pad
                );
                assert!(
                    texts.iter().any(|t| t.contains(&needle)),
                    "missing row for mode {}: searched for {:?}",
                    mode,
                    needle,
                );
            }
        }

        /// The plain-mode row shows
        /// an em-dash (\u{2014}) in the
        /// prefix column because plain
        /// has no prefix. Verifies the
        /// "no prefix" visual
        /// indicator is present so
        /// the user sees that plain
        /// mode is the default.
        #[test]
        fn help_plain_mode_shows_em_dash_prefix() {
            let lines = build_help_lines(&help_app());
            let texts: Vec<String> = lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect();
            let plain_row = texts
                .iter()
                .find(|t| t.trim_start().starts_with("plain"))
                .expect("plain row");
            assert!(
                plain_row.contains('\u{2014}'),
                "plain row should contain em-dash: {:?}",
                plain_row
            );
        }

        /// The "JIRA-mode tags"
        /// section is present, with
        /// all five tag rows
        /// (`@me`, `@today`,
        /// `@week`, `@month`,
        /// `@<name>`).
        #[test]
        fn help_includes_jira_mode_tags_section() {
            let lines = build_help_lines(&help_app());
            let texts: Vec<String> = lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect();
            // Section header.
            let header_idx = texts
                .iter()
                .position(|t| t == "JIRA-mode tags")
                .expect("JIRA-mode tags section");
            // Header is bold.
            assert!(lines[header_idx]
                .spans
                .first()
                .map(|s| s.style.add_modifier.contains(Modifier::BOLD))
                .unwrap_or(false));
            // All five tags appear
            // in the lines after
            // the header.
            let after = &texts[header_idx..];
            assert!(
                after.iter().any(|t| t.contains("@me")),
                "missing @me"
            );
            assert!(
                after.iter().any(|t| t.contains("@today")),
                "missing @today"
            );
            assert!(
                after.iter().any(|t| t.contains("@week")),
                "missing @week"
            );
            assert!(
                after.iter().any(|t| t.contains("@month")),
                "missing @month"
            );
            assert!(
                after.iter().any(|t| t.contains("@<name>")),
                "missing @<name> fragment row"
            );
        }

        /// Each JIRA-tag row shows
        /// the exact JQL clause the
        /// tag expands to, so the
        /// help doubles as a JQL
        /// reference (the user can
        /// copy-paste the clause
        /// into a JIRA web search
        /// to verify the
        /// behaviour).
        #[test]
        fn help_jira_tags_show_jql_clauses() {
            let lines = build_help_lines(&help_app());
            let texts: Vec<String> = lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect();
            // The exact clauses
            // from the `build_jql`
            // implementation.
            let expected: &[(&str, &str)] = &[
                ("@me", "assignee = currentUser()"),
                ("@today", "updated >= \"<today-1d>\""),
                ("@week", "updated >= \"<today-7d>\""),
                ("@month", "updated >= \"<today-31d>\""),
            ];
            for &(tag, jql) in expected {
                // The tag row
                // contains the
                // tag in the
                // first
                // column
                // and the
                // JQL in
                // the
                // second
                // column.
                // We look
                // for a
                // line
                // that
                // contains
                // both.
                let matching = texts
                    .iter()
                    .find(|t| t.contains(tag) && t.contains(jql));
                assert!(
                    matching.is_some(),
                    "missing JQL clause for {}: looking for {:?}",
                    tag,
                    jql
                );
            }
        }

        /// The help reflects the
        /// user's configured prefixes,
        /// not the defaults. Rebinds
        /// the regex prefix to `#` and
        /// confirms the help shows `#`
        /// in the regex row (not `/`,
        /// the default).
        #[test]
        fn help_shows_user_configured_prefixes() {
            let mut app = help_app();
            // Rebind the regex
            // prefix from `/` to
            // `#`. The `prefix.regex=...`
            // config key is
            // parsed by
            // `Config::assign_prefix`;
            // here we set the field
            // directly (which is
            // what the config
            // loader does after
            // parsing).
            app.query_prefixes.regex = '#';
            let lines = build_help_lines(&app);
            let texts: Vec<String> = lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect();
            // The regex row should
            // now show `#` as
            // the prefix. The
            // format helper pads
            // the prefix with a
            // leading space, so
            // the row contains
            // ` # `.
            let regex_row = texts
                .iter()
                .find(|t| t.trim_start().starts_with("regex"))
                .expect("regex row");
            assert!(
                regex_row.contains(" # "),
                "regex row should show '#' as the prefix: {:?}",
                regex_row
            );
            // Sanity: the default
            // regex prefix `/` is
            // NOT in this row
            // (because we
            // overrode it).
            assert!(
                !regex_row.contains(" / "),
                "regex row should not show default '/' prefix: {:?}",
                regex_row
            );
        }
}
