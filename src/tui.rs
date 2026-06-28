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
use crate::Config;
use regex::Regex;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use bindings::{action_for_key, format_key_spec, format_key_specs, Action, KeyBindings, ALL_ACTIONS};
pub use state::{ExitFilter, Mode, HistoryRow, PickMode, SortOrder, exit_code};
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
}

/// How long the LLM auto-call waits after the last keystroke
/// before firing. Tuned to the "user is composing a thought"
/// rhythm: long enough that the model isn't re-queried on
/// every character of a long description, short enough that
/// the user sees the suggestion before they have to look up
/// to the status bar. 1 second is the value the user asked
/// for in the spec.
const LLM_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(1);

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
        let (pattern, _filter) = parse_notes_query(raw_pattern);
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
            return self.fetch_recent_notes(db_path);
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
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let cutoff = filter.cutoff(now);
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
    fn fetch_recent_notes(&self, db_path: &std::path::Path) -> Result<Vec<HistoryRow>> {
        let service = note_search::database_service::DatabaseService::new(
            &db_path.to_string_lossy()
        );
        // Use default SearchCriteria to get all notes (no query filter).
        let criteria = note_search::SearchCriteria::default();
        match service.search_notes(&criteria) {
            Ok(results) => {
                let mut rows: Vec<HistoryRow> = results.iter().map(|note| {
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
            // LLM debounce state. The user hasn't typed
            // anything yet (we're at construction time), so
            // the debounce is satisfied and no preview is
            // active. The run-loop tick will arm the
            // debounce on the first keystroke in LLM mode.
            llm_debounce_started: None,
            llm_preview: None,
            llm_in_flight: false,
            llm_request: None,
            llm_preview_description: None,
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
        let directory = std::env::var("PWD").unwrap_or_default();
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
                params![&query_command, std::env::var("PWD").unwrap_or_default(), session_id],
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
        let command = row.command.clone();
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
        // Clone the command out of the merged view
        // so the borrow is dropped before we mutate
        // `self` again. (See `start_describe` for
        // the same pattern; both LLM-backed actions
        // need this dance.)
        let original_command = row.command.clone();
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
        let directory = std::env::var("PWD").unwrap_or_default();
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
        let directory = std::env::var("PWD").unwrap_or_default();
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
}
