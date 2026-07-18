#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::enum_variant_names)]
#![allow(clippy::empty_line_after_doc_comments)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::ptr_arg)]
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::CrosstermBackend, widgets::ListState};
use rusqlite::{Connection, params};
use std::time::Duration;

pub mod actions;
pub mod bindings;
pub mod mode;
pub mod render;
pub mod state;
pub mod labeled;

pub mod stats;

pub mod theme;

use crate::Config;
use crate::QueryPrefixes;
use crate::jira::JiraClient;
use crate::llm::LlmClient;
use crate::util::{format_diff, format_time};
use regex::Regex;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

pub use bindings::{
    ALL_ACTIONS, Action, KeyBindings, action_for_key, format_key_spec, format_key_specs,
};
pub use state::{
    AddEntryDialog, AddEntryKind, ExitFilter, HistoryRow, HostDef, MatchAlgorithm, Mode,
    PanesFilter, PickMode, SortOrder, TmuxWindowInfo, exit_code,
};
pub use theme::{BuiltinTheme, SelectedTheme, ThemePicker, install_palette};

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
    /// Which detail panes were
    /// visible at the end of
    /// the last session
    /// (`both` / `details` /
    /// `output`). `None`
    /// means "no preference"
    /// and falls back to
    /// `PaneVisibility::Both`.
    /// Unrecognised values
    /// are silently dropped.
    pane_visibility: Option<String>,
    /// Which detail-pane height was
    /// active at the end of the last
    /// session (`default` /
    /// `tall`). `None` means "no
    /// preference" and falls back to
    /// `PaneHeight::Default`.
    pane_height: Option<String>,
}

/// All theme choices available in the TUI. The first entry, `None`,
/// represents the "no theme" mode where the manually-configured
/// `tuicolor.*` settings from `~/.config/smarthistory/config` are
/// used. Every other entry corresponds to a built-in theme —
/// see `BuiltinTheme` for the full list (upstream `ratatui-themes`
/// plus a small set of hand-curated themes shipped with this
// crate).
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
        let Some(path) = Self::path() else {
            return Self::default();
        };
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
                "duplicatefilter" => s.duplicate_filter = Some(crate::util::parse_bool(value, true)),
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
                "directorysource"
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
                    => {
                        s.directory_source =
                            Some(value.to_string());
                    }
                "panevisibility"
                    // Same pattern
                    // as the other
                    // session fields:
                    // only accept
                    // values that
                    // `PaneVisibility::parse`
                    // recognises
                    // (lowercase
                    // `both` /
                    // `details` /
                    // `output`).
                    if crate::tui::state::PaneVisibility::parse(
                        value,
                    )
                    .is_some()
                    => {
                        s.pane_visibility =
                            Some(value.to_string());
                    }
                "paneheight"
                    // Same pattern as
                    // `pane_visibility`: only
                    // accept values that
                    // `PaneHeight::parse`
                    // recognises (`default` /
                    // `tall` plus aliases).
                    if crate::tui::state::PaneHeight::parse(
                        value,
                    )
                    .is_some()
                    => {
                        s.pane_height =
                            Some(value.to_string());
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
            out.push_str(&format!(
                "duplicatefilter={}\n",
                if d { "on" } else { "off" }
            ));
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
        if let Some(ref pv) = self.pane_visibility {
            out.push_str(&format!("panevisibility={}\n", pv));
        }
        if let Some(ref ph) = self.pane_height {
            out.push_str(&format!("paneheight={}\n", ph));
        }
        if let Err(e) = std::fs::write(&path, out) {
            eprintln!("warning: failed to persist TUI session: {}", e);
        }
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
pub(crate) enum NotesDateFilter {
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
        // Date-alias path. The user types
        // `@today` (or `today`); both
        // should be recognised as the
        // date alias. We strip a leading
        // `@` so the alias is matched on
        // the bare keyword. Date aliases
        // are extracted as a filter
        // rather than passed through to
        // the search query (the
        // `note_search` library doesn't
        // know about them — we apply the
        // cutoff post-query in
        // `fetch_notes`).
        let candidate = token.strip_prefix('@').unwrap_or(token);
        match candidate.to_ascii_lowercase().as_str() {
            "today" => {
                filter = NotesDateFilter::Today;
                continue;
            }
            "week" => {
                filter = NotesDateFilter::Week;
                continue;
            }
            "month" => {
                filter = NotesDateFilter::Month;
                continue;
            }
            "year" => {
                filter = NotesDateFilter::Year;
                continue;
            }
            _ => {}
        }
        // `#TAG` — search for notes
        // tagged `TAG`. The
        // `note_search` query parser
        // already handles `#tagname`
        // syntax, so we pass the token
        // through unchanged. This lets
        // the user combine tag and text
        // search: `#feature rust` finds
        // notes tagged `feature` that
        // also mention `rust`.
        if let Some(tag) = token.strip_prefix('#') {
            if !tag.is_empty() {
                cleaned_tokens.push(format!("#{}", tag));
            }
            continue;
        }
        // `@LINK` — search for notes
        // that have a link to `LINK`.
        // The `note_search` query parser
        // uses `[[linkname]]` (wiki-link
        // syntax) for link search, so
        // we convert the user's `@LINK`
        // shorthand to `[[LINK]]`. The
        // link name preserves the
        // user's original casing
        // (link targets are
        // case-sensitive in Obsidian).
        if let Some(link) = token.strip_prefix('@') {
            if !link.is_empty() {
                cleaned_tokens.push(format!("[[{}]]", link));
            }
            continue;
        }
        // Plain text: push the token
        // verbatim. We no longer strip
        // `@` here because the date-
        // alias path above already
        // consumed the four known
        // aliases; any remaining `@foo`
        // is the user's intent for
        // `@foo` (e.g. searching for
        // the literal word `@foo` in
        // note text).
        cleaned_tokens.push(token.to_string());
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
        '*', '?', '[', ']', '{', '}', ';', '|', '&', '<', '>', '(', ')', '`', '$', '=', '\'', '"',
        '\\', '!', '#',
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
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {}", e))?;
    cb.set_text(text.to_string())
        .map_err(|e| format!("write failed: {}", e))?;
    Ok(())
}

/// A high-level action that the TUI can take in response to a key
/// press. Action names appear in the user-facing config file as
// `key.<action>=<key-spec>`, e.g. `key.help=C-h`.
// (The enum itself lives in `bindings::Action`.)

pub(crate) struct App {
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
    /// When `Some`, the prefix-picker overlay is open. The
    /// picker is the `Action::PickPrefix` counterpart
    /// to the command palette: a centred list of every
    /// configured prefix mode that the user can
    /// navigate with Up/Down and commit with Enter.
    /// Closing on `Cancel` (`Esc` / `Ctrl-C`) leaves
    /// the query unchanged. The picker pre-selects
    /// the row matching the current query's leading
    /// char (or the "no prefix" row for a bare
    /// text query), so Enter with no navigation
    /// is a no-op.
    prefix_picker: Option<PrefixPicker>,
    /// CodeGraph callers/callees overlay picker. Opened by the
    /// `CodegraphRelations` action (`C-r` by default) from a
    /// `&` / `$` (codegraph-backed) row. `None` when closed.
    codegraph_relations_picker: Option<CodeGraphRelationsPicker>,
    /// When `Some`, the theme-picker overlay is open. Navigating
    /// the list applies the selected theme live; `Enter` commits,
    /// `Esc` reverts to the original.
    theme_picker: Option<ThemePicker>,
    /// When `Some`, the tab-completion
    /// menu is open. Shown when the
    /// user presses `Tab` and the
    /// completion has multiple
    /// matches (ambiguous prefix).
    /// The user can navigate the
    /// candidates with `Up`/`Down`
    /// (or `Ctrl-N`/`Ctrl-P`) and
    /// apply the selected one with
    /// `Enter`. `Esc` / `Cancel`
    /// closes the menu without
    /// changing the query. The menu
    /// remembers the original
    /// prefix range so applying a
    /// candidate replaces exactly
    /// the word the user typed (not
    /// the whole query).
    completion_menu: Option<CompletionMenu>,
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
    /// The active match algorithm, toggled by
    /// `Action::CycleMatchAlgorithm` (default
    /// key `C-f`). Applies to ALL prefix modes
    /// (history, directories, panes, notes,
    /// todos, files, output) — wherever
    /// `query_matches_text` is consulted.
    /// JIRA (`-` mode) is exempt because it
    /// parses its own JQL syntax.
    ///
    /// Defaults to `Substring` (the
    /// historical plain-text behavior). The
    /// cycle is Substring → Fuzzy → Regex
    /// → Substring.
    match_algorithm: MatchAlgorithm,
    /// Which detail panes are visible. Cycled
    /// between BOTH → Details → Output
    /// Preview → BOTH with `F6`
    /// (configurable).
    pane_visibility: crate::tui::state::PaneVisibility,
    /// Which detail-pane height preset is
    /// active (`Default` 8 lines,
    /// `Tall` ~70% of the list area).
    /// Toggled by `Action::TogglePaneHeight`
    /// and persisted in the session file.
    pane_height: crate::tui::state::PaneHeight,
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
    /// Terminal multiplexer
    /// backend (tmux or
    /// herdr) that owns
    /// the snapshot and
    /// the focus / create
    /// / send-in-pane
    /// staging. Built once
    /// in `App::new` from
    /// `Config::multiplexer()`.
    /// `Box<dyn ...>` because
    /// the concrete backend
    /// type depends on the
    /// `herdr` Cargo feature
    /// and we want a single
    /// struct shape regardless
    /// of which is compiled
    /// in.
    multiplexer: Box<dyn crate::multiplexer::MultiplexerBackend>,
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
    /// Named sessions parsed from
    /// Named sessions from the config file
    /// (`session.<id>=...`, `session.<id>.dir=...`).
    /// Each entry has a display name, a directory,
    /// and optionally a startup command. Populated
    /// at construction from `Config::sessions()`;
    /// appended to the panes view by
    /// `fetch_session_panes_impl`.
    sessions: Vec<HistoryRow>,
    /// Host entries parsed from the
    /// config file (`host.<id>=...`,
    /// `host.<id>.host=...`) merged
    /// with `~/.ssh/config` entries.
    /// Each entry has a display name
    /// and a connection target.
    /// Populated at construction from
    /// `Config::hosts()`; appended to
    /// the panes view by
    /// `fetch_session_panes_impl` as
    /// a `# hosts` section after the
    /// existing `# sessions` section.
    hosts: Vec<HistoryRow>,
    /// The full [`HostDef`] entries
    /// in the same order as
    /// [`App::hosts`]. Used by the
    /// staging layer to read the
    /// real hostname, identity,
    /// port, and exec — fields
    /// the projected `HistoryRow`
    /// doesn't carry. Index-aligned
    /// with `hosts`.
    host_defs: Vec<HostDef>,
    /// The "add session / host"
    /// dialog state. `None`
    /// when the dialog is
    /// closed. Opened by the
    /// `AddSession` / `AddHost`
    /// actions (`C-1` /
    /// `C-2`); the dialog
    /// handles its own input
    /// until the user commits
    /// (Enter) or cancels
    /// (Esc).
    add_entry_dialog: Option<AddEntryDialog>,
    /// Filter for the `*`-mode panes view.
    /// When set to a non-`All` value,
    /// `fetch_panes` hides rows whose
    /// `source` doesn't match the
    /// filter. Toggled by the
    /// `FilterPanesWindows` /
    /// `FilterPanesHosts` /
    /// `FilterPanesSessions` actions
    /// (`F7` / `F8` / `F9`); pressing
    /// the active filter's key again
    /// resets to `All`. The current
    /// value is shown as a chip in the
    /// mode strip so the user can see
    /// at a glance which section is
    /// filtered.
    panes_filter: PanesFilter,
    /// Background request for pane cmdlines (see
    /// `PaneCmdlineRequest`). Spawned after the
    /// panes snapshot is populated so the cmdline
    /// appears asynchronously without blocking
    /// the first render.
    pane_cmdlines_request: Option<PaneCmdlineRequest>,
    /// Monotonic counter incremented on every
    /// `fetch_session_panes_impl` call. Acts as a
    /// snapshot id so a stale background cmdline
    /// lookup (from an old snapshot) is detected
    /// and discarded.
    panes_snapshot_id: u64,

    /// Memoization cache for `pane::ensure_selected_context`:
    /// for each pane id we've recently read, the
    /// `Instant` we read it. Reads within a short
    /// window are skipped to avoid an IPC round-trip
    /// to `herdr` on every keystroke (the TUI run
    /// loop calls `ensure_selected_context` from
    /// every action dispatch, which can be many
    /// events per second). `None` until the first
    /// read; populated lazily. Cleared at TUI
    /// exit only — pane content does change over
    /// time, so the cache TTL is short (~750ms)
    /// rather than "until process exit".
    pane_preview_cache: Option<std::collections::HashMap<String, std::time::Instant>>,
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
    /// Secondary idle timer for JIRA search-as-you-type.
    /// Also armed by `jira_touch` on every keystroke
    /// (in lock-step with `jira_debounce_started`).
    /// Unlike the 400ms debounce, the idle timer
    /// fires only after
    /// [`JIRA_IDLE_TIMEOUT`] (3 seconds) of no
    /// input — it's the safety-net trigger that
    /// guarantees the query runs even when the
    /// fast debounce fails to fire (e.g. the user
    /// keeps typing slowly, or the run loop is
    /// temporarily blocked). Cleared when a search
    /// fires or the mode is left.
    jira_idle_started: Option<std::time::Instant>,
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
    /// Per-extension shell commands invoked by
    /// `Action::SmartOpen` in `~` (files) mode.
    /// Loaded from the config file's
    /// `smart-open.<ext>=<cmd>` lines. The TUI's
    /// `~`-mode SmartOpen dispatch looks up the
    /// selected file's extension (lowercase, no
    /// leading `.`) in this map; the matched
    /// command is staged with the file path
    /// appended. The reserved key `default` is
    /// the fallback for any extension without an
    /// explicit mapping. See
    /// [`crate::Config::smart_open_file_commands`]
    /// for the matching / fallback semantics and
    /// parsing rules.
    smart_open_file_commands: std::collections::HashMap<String, String>,
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

    /// Source-file contents cache for tags
    /// (`$`) mode. The TAGS file can be
    /// very large (hundreds of thousands
    /// of symbols) and many symbols point
    /// back at the same source file. This
    /// cache makes the preview context
    /// load from disk only once per file
    /// per TUI session instead of once
    /// per symbol, which was the dominant
    /// cost of opening tags mode.
    tags_source_cache: std::collections::HashMap<std::path::PathBuf, String>,

    /// Lazily-opened read-only client over the local
    /// `.codegraph/codegraph.db` index. `None` until
    /// the first `&` (codegraph) query — or until the
    /// `$` (tags) fallback discovers no `TAGS` file —
    /// at which point we try to open it once and cache
    /// the connection for the rest of the session. A
    /// missing index leaves this `None` so `&` mode is
    /// a clean no-op in repos without CodeGraph.
    codegraph_client: Option<crate::codegraph::CodeGraphClient>,

    /// Per-mode input query history. Keyed by the
    /// leading `char` of the query (the prefix char,
    /// e.g. `&` for codegraph mode, or `MODE_NONE`
    /// for plain no-prefix history). Each value is
    /// the list of past queries the user typed in
    /// that mode, **newest first**. Persisted across
    /// TUI sessions to `<db_dir>/query_history.json`
    /// so the recall state survives restarts.
    mode_query_history:
        std::collections::HashMap<char, Vec<String>>,

    /// Per-mode in-progress "draft" query saved when
    /// the user starts history recall (C-p from the
    /// live query). Restored on C-n past the newest
    /// entry so the user can resume typing where they
    /// left off. Session-local — not persisted, so
    /// fresh TUI sessions always start with the live
    /// query and no draft.
    mode_query_drafts: std::collections::HashMap<char, String>,

    /// Per-mode recall position. `None` means "at the
    /// live query" (the current `self.query` IS the
    /// user's in-progress text); `Some(0)` means "at
    /// the newest history entry"; `Some(N-1)` means
    /// "at the oldest". Session-local — not persisted.
    mode_query_history_index: std::collections::HashMap<char, Option<usize>>,
    /// Cache key for the SQL `fetch()` short-circuit: the
    /// `(query, mode, exit_filter, match_algorithm)` tuple from
    /// the last successful fetch. When this matches the
    /// current state, `refresh()` skips re-querying the DB.
    last_fetch_key: Option<(String, Mode, ExitFilter, MatchAlgorithm)>,

    /// Aggregated files-mode state:
    /// debounce timer, in-flight walk
    /// request, last walked pattern,
    /// and cached rows. The full
    /// state machine lives in
    /// `src/files.rs::FilesState` so
    /// the four interrelated fields
    /// stay in one struct.
    files_state: crate::files::FilesState,

    /// Aggregated ag-mode state:
    /// debounce timer, in-flight search
    /// request, last searched pattern,
    /// and cached rows.
    ag_state: crate::ag::AgState,

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

/// Secondary safety-net timeout for the JIRA
/// search-as-you-type. The 400ms `JIRA_DEBOUNCE`
/// handles the fast-typo case (fires shortly after the
/// user pauses). The 3-second `JIRA_IDLE_TIMEOUT` is
/// a fallback for the cases where the fast debounce
/// doesn't fire — e.g. the user keeps typing slowly
/// for more than 3 seconds, or the run loop is
/// temporarily blocked on background work so the
/// tick that would normally fire the 400ms debounce
/// never runs. The user's report was that the query
/// "sometimes isn't executed"; this idle timeout
/// guarantees the query fires within 3 seconds of the
/// last keystroke regardless of what the fast debounce
/// does.
///
/// The space key (`' '`) has its own explicit-fire
/// path in `push_char` — it commits the current
/// query immediately so the user can type
/// `<word> <next-word>` and see results after the
/// first word is complete, without waiting for the
/// debounce. The space trigger fires before either
/// the fast debounce or the idle timeout.
const JIRA_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Sentinel for the plain (no-prefix) mode in the per-mode
/// query-history maps. Real prefix chars (`+`, `=`, `%`, `@`,
/// `!`, `#`, `*`, `~`, `$`, `&`, `,`, `-`) are all printable;
/// `'\0'` can't be one, so it uniquely identifies "the query
/// has no leading prefix char" without colliding with a real
/// mode. Used as the `char` key in
/// [`App::mode_query_history`] / `mode_query_drafts` /
/// `mode_query_history_index` for the plain-mode slot.
const MODE_NONE: char = '\0';

/// The mode char (or `MODE_NONE` for plain) of a given query,
/// decided by the leading `char`. An empty query returns
/// `MODE_NONE` so the plain-history slot is the default
/// destination for empty / pre-mode queries (e.g. before the
/// user has typed their first prefix char).
///
/// Centralising this here (rather than replicating the
/// prefix-character table inside `App`) keeps the mode
/// detection consistent with the per-mode history bookkeeping
/// and the existing `is_<mode>_query` predicates, and makes
/// it straightforward to add a new prefix by extending
/// `QueryPrefixes` (the function reads the configured
/// prefixes, not a hard-coded list).
fn query_mode_char(query: &str, prefixes: &crate::QueryPrefixes) -> char {
    let Some(c) = query.chars().next() else {
        return MODE_NONE;
    };
    let known: [char; 12] = [
        prefixes.output,
        prefixes.llm,
        prefixes.question,
        prefixes.notes,
        prefixes.todo,
        prefixes.directories,
        prefixes.panes,
        prefixes.jira,
        prefixes.files,
        prefixes.tags,
        prefixes.codegraph,
        prefixes.ag,
    ];
    if known.contains(&c) {
        c
    } else {
        MODE_NONE
    }
}

impl App {
    /// True if the active match algorithm is Regex.
    fn is_regex_query(&self) -> bool {
        self.match_algorithm == MatchAlgorithm::Regex
    }

    /// True if the active match algorithm is Fuzzy.
    fn is_fuzzy_query(&self) -> bool {
        self.match_algorithm == MatchAlgorithm::Fuzzy
    }

    /// True if the current query is an output-content search
    /// (prefixed with configured output prefix).
    fn is_output_query(&self) -> bool {
        crate::tui::mode::output::matches(self)
    }

    /// True if the current query is an LLM command-generation
    /// request (prefixed with configured LLM prefix).
    /// Only returns true if there's actual description text after
    /// the prefix (not just the prefix alone or with only whitespace).
    fn is_llm_query(&self) -> bool {
        crate::tui::mode::llm::matches(self)
    }

    /// True if the current query is a general question
    /// request (prefixed with configured question prefix).
    /// Only returns true if there's actual question text after
    /// the prefix (not just the prefix alone or with only whitespace).
    fn is_question_query(&self) -> bool {
        crate::tui::mode::question::matches(self)
    }

    /// The regex pattern. Returns the query body (the
    /// text after any prefix-mode char like `#` or `*`,
    /// or the full query if there's no prefix mode).
    #[allow(dead_code)]
    fn regex_pattern(&self) -> &str {
        self.search_body()
    }

    /// The fuzzy pattern. Returns the query body.
    #[allow(dead_code)]
    fn fuzzy_pattern(&self) -> &str {
        self.search_body()
    }

    /// The search body: the part of the query that's
    /// actually used for matching. This is the full query
    /// with the leading prefix-mode character stripped
    /// (e.g. `#ls -la` → `ls -la`, `*vim` → `vim`).
    /// For the default history mode (no prefix), it's the
    /// full query. For output mode (`+...`), it's the text
    /// after the `+`. For LLM/question modes, it's the text
    /// after the `=`/`%`. This is the canonical body that
    /// `query_matches_text` and `recompile_regex` operate
    /// on regardless of the active match algorithm.
    fn search_body(&self) -> &str {
        if self.query.is_empty() {
            return "";
        }
        let p = &self.query_prefixes;
        let c = self.query.chars().next().unwrap_or('\0');
        // Strip one prefix char if the query starts with a
        // known prefix-mode character. The match algorithm
        // (substring/fuzzy/regex) operates on what's LEFT.
        if c == p.output
            || c == p.llm
            || c == p.question
            || c == p.notes
            || c == p.todo
            || c == p.directories
            || c == p.panes
            || c == p.jira
            || c == p.files
            || c == p.tags
            || c == p.ag
        {
            &self.query[c.len_utf8()..]
        } else {
            self.query.as_str()
        }
    }

    /// Whether the current query body (the search text
    /// after stripping the prefix character) contains
    /// any uppercase ASCII characters. When `true`, the
    /// search should be case-sensitive; when `false`, the
    /// search should be case-insensitive (the historical
    /// default).
    ///
    /// This is a heuristic: the user types `git` →
    /// case-insensitive (matches `GIT`, `Git`, etc.);
    /// the user types `Git` → case-sensitive (matches only
    /// `Git`, not `git` or `GIT`). The rule is intuirive
    /// and matches what most IDE / editor search toggles
    /// (the "smart case" mode) do.
    ///
    /// The check looks at the body AFTER prefix stripping
    /// so prefix characters like `+` or `=` (which are
    /// not uppercase) don't interfere.
    fn is_case_sensitive(&self) -> bool {
        self.search_body().chars().any(|c| c.is_ascii_uppercase())
    }

    /// The output-search body, i.e. everything after the
    /// leading output prefix.
    fn output_pattern(&self) -> &str {
        crate::tui::mode::output::pattern(self)
    }

    /// The LLM query body, i.e. everything after the
    /// leading LLM prefix.
    fn llm_pattern(&self) -> &str {
        crate::tui::mode::llm::pattern(self)
    }

    /// The question body, i.e. everything after the
    /// leading question prefix.
    fn question_pattern(&self) -> &str {
        crate::tui::mode::question::pattern(self)
    }

    /// True if the current query is a note search request
    /// (prefixed with the configured notes prefix, default `@`).
    fn is_notes_query(&self) -> bool {
        crate::tui::mode::notes::matches(self)
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
        crate::tui::mode::todo::matches(self)
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
        crate::tui::mode::directories::matches(self)
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
        crate::tui::mode::directories::pattern(self)
    }

    /// Whether the query is a session-panes request:
    /// the query starts with the panes prefix (`*` by
    /// default). The body (everything after `*`) is a
    /// substring filter matched against each pane's
    /// current command and cwd.
    fn is_panes_query(&self) -> bool {
        crate::tui::mode::panes::matches(self)
    }

    /// The session-panes filter body, i.e. everything
    /// after the leading `*` prefix. Empty when not in
    /// panes mode.
    fn panes_pattern(&self) -> &str {
        crate::tui::mode::panes::pattern(self)
    }

    /// Whether the query is a files-view request:
    /// the query starts with the files prefix (`~` by
    /// default). The body (everything after `~`) is a
    /// substring filter matched against each file's
    /// path (relative to cwd).
    fn is_files_query(&self) -> bool {
        crate::tui::mode::files::matches(self)
    }

    /// Whether the query is a tags-search request:
    /// the query starts with the tags prefix (`$` by
    /// default). The body is matched against the
    /// symbol names AND the source-line text from the
    /// `tags` file in the current directory.
    fn is_tags_query(&self) -> bool {
        crate::tui::mode::tags::matches(self)
    }

    /// The tags-search body, i.e. everything after the
    /// leading `$` prefix. Empty string when not in
    /// tags mode.
    fn tags_pattern(&self) -> &str {
        crate::tui::mode::tags::pattern(self)
    }

    /// Whether the query is an ag content-search request:
    /// the query starts with the ag prefix (`,` by
    /// default). The body is split into search terms
    /// and file-pattern globs (tokens containing `*`).
    fn is_ag_query(&self) -> bool {
        crate::tui::mode::ag::matches(self)
    }

    /// The ag-search body, i.e. everything after the
    /// leading ag prefix. Empty string when not in
    /// ag mode.
    #[allow(dead_code)]
    fn ag_pattern(&self) -> &str {
        crate::tui::mode::ag::pattern(self)
    }

    /// Whether the query is a CodeGraph symbol-search
    /// request: the query starts with the codegraph
    /// prefix (`&` by default). The body is matched
    /// against symbol names in the local
    /// `.codegraph/codegraph.db` index via FTS5.
    fn is_codegraph_query(&self) -> bool {
        crate::tui::mode::codegraph::matches(self)
    }

    // `codegraph_pattern` was inlined into
    // `crate::tui::mode::codegraph::fetch` — the only
    // caller is now the per-mode free function, so the
    // 1-line shim is removed. The `App::codegraph_pattern`
    // method body was identical to the per-mode
    // `pattern()` function; the call-site change
    // was a rename from `self.codegraph_pattern()`
    // to `crate::tui::mode::codegraph::pattern(self)`.

    /// The note search body, i.e. everything after the
    /// leading notes prefix.
    fn notes_pattern(&self) -> &str {
        crate::tui::mode::notes::pattern(self)
    }

    /// Whether the query is a JIRA issue-search request:
    /// the query starts with the jira prefix (`-` by
    /// default). The body is parsed into a JQL query by
    /// `crate::jira::build_jql` (issue keys,
    /// `field=value` constraints, free text).
    fn is_jira_query(&self) -> bool {
        crate::tui::mode::jira::matches(self)
    }

    /// The JIRA search body, i.e. everything after the
    /// leading `-` prefix. Empty string when not in jira
    /// mode.
    fn jira_pattern(&self) -> &str {
        crate::tui::mode::jira::pattern(self)
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
        std::fs::read_to_string(&path).unwrap_or_default()
    }

    // `fetch_todos` was extracted to
    // `crate::tui::mode::todo::fetch` (the note_search
    // `search_todos` query + per-row file-mtime enrichment
    // + date-filter post-sort for the `!` mode).
    // The two `App` fields it writes
    // (`notes_date_filter`, status messages via
    // `set_status_message`) are read back by the
    // renderer / status bar; the per-mode free
    // function mutates `app.notes_date_filter` and
    // calls `app.set_status_message` directly so the
    // existing field accessors continue to work.


    // `fetch_file_updated_timestamps` was extracted to
    // `crate::tui::mode::notes::fetch_file_updated_timestamps`
    // (a free function — it has no `self` access — used
    // by `todo::fetch` to populate the per-row `updated`
    // timestamp).


    // `fetch_directories` was extracted to
    // `crate::tui::mode::directories::fetch` (the SQL
    // `SELECT` over unique directories + sessiondirs
    // subdirs + tmux/herdr pane cwd's + substring /
    // fuzzy / regex token filter + canonicalization
    // for the `#` mode). The function is large
    // (700+ lines) but is a single coherent
    // pipeline, so it moves as a unit.


    // `fetch_session_panes` was extracted to
    // `crate::tui::mode::panes::refresh_session_panes`
    // (the lazy multiplexer snapshot that populates
    // `app.session_panes` once per TUI session; called
    // by `panes::fetch` before applying the
    // panes-filter and token filter).


    // `fetch_session_panes_impl` was extracted
    // alongside `fetch_session_panes` (the test-injectable
    // variant that takes `current_pane` directly, used
    // by `refresh_session_panes` and by the panes-mode
    // tests in `panes::tests`).


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
    /// Spawn a background thread to look up each
    /// pane's running-process command line via
    /// `multiplexer::herdr_pane_cmdline`. The
    /// thread sends `(pane_id, cmdline)` pairs
    /// over a channel; the run loop polls the
    /// channel in `process_pane_cmdlines`.
    ///
    /// Only spawned for the herdr backend (the
    /// tmux backend already has the
    /// `current_command` in its snapshot via
    /// `#{pane_current_command}`; no extra
    /// lookup is needed there).
    ///
    /// Cancels any previous in-flight request
    /// before spawning — its results would be
    /// stale (from a previous snapshot).
    ///
    /// `snapshot_id` is stashed on the request
    /// so the App can detect stale results on
    /// receipt (a snapshot that has been
    /// superseded by a newer one between spawn
    /// and receipt).
    fn spawn_pane_cmdlines(&mut self, snapshot_id: u64) {
        // Cancel any previous in-flight request.
        if let Some(prev) = self.pane_cmdlines_request.take() {
            prev.cancelled.store(true, Ordering::Relaxed);
        }
        // Only the herdr backend needs the
        // background lookup. The tmux
        // backend's snapshot already carries
        // `current_command` via
        // `#{pane_current_command}`.
        if self.multiplexer.name() != "herdr" {
            return;
        }
        // Collect the pane ids to look up
        // (only `mode == "pane"` rows —
        // workspace headers, sessions, and
        // hosts have no running process).
        let pane_ids: Vec<String> = self
            .session_panes
            .iter()
            .filter(|r| r.mode == "pane")
            .map(|r| r.session_id.clone())
            .collect();
        if pane_ids.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_clone = cancelled.clone();
        std::thread::spawn(move || {
            for pane_id in &pane_ids {
                if cancelled_clone.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(cmdline) = crate::multiplexer::herdr_pane_cmdline(pane_id) {
                    if cancelled_clone.load(Ordering::Relaxed) {
                        return;
                    }
                    let _ = tx.send((pane_id.clone(), cmdline));
                }
            }
        });
        self.pane_cmdlines_request = Some(PaneCmdlineRequest {
            receiver: rx,
            cancelled,
            snapshot_id,
        });
    }

    /// Drain the background pane-cmdline channel
    /// and patch the results into
    /// `self.session_panes`. Called from the
    /// run loop tick every ~100ms. Each received
    /// `(pane_id, cmdline)` pair updates the
    /// matching pane row's `command` field in
    /// place (combining the agent name and the
    /// cmdline — same logic the synchronous path
    /// used before this was moved to a
    /// background thread).
    ///
    /// Stale results (from a snapshot that has
    /// been superseded by a newer one) are
    /// discarded — the `snapshot_id` check at
    /// the top of the function guards against
    /// overwriting the new snapshot with old
    /// data.
    ///
    /// When the channel is closed (the thread
    /// finished), the request is taken out of
    /// `pane_cmdlines_request` so the next poll
    /// is a no-op.
    fn process_pane_cmdlines(&mut self) {
        // Lazy spawn: if no lookup is in flight AND
        // we have pane rows AND the backend is herdr,
        // spawn one now. This fires once, after the
        // run loop has settled (the multiple
        // `fetch_session_panes_impl` calls during
        // init have all happened), so the snapshot
        // id at spawn time matches the current
        // snapshot — the request survives long
        // enough to deliver results.
        if self.pane_cmdlines_request.is_none()
            && self.multiplexer.name() == "herdr"
            && self.is_panes_query()
            && self.session_panes.iter().any(|r| r.mode == "pane")
        {
            self.spawn_pane_cmdlines(self.panes_snapshot_id);
        }
        let Some(request) = self.pane_cmdlines_request.as_ref() else {
            return;
        };
        // Discard stale results from a
        // superseded snapshot.
        if request.snapshot_id != self.panes_snapshot_id {
            if let Some(req) = self.pane_cmdlines_request.take() {
                req.cancelled.store(true, Ordering::Relaxed);
            }
            return;
        }
        // Drain everything that's ready.
        let mut updates: Vec<(String, String)> = Vec::new();
        loop {
            match request.receiver.try_recv() {
                Ok(pair) => updates.push(pair),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Thread finished — take the
                    // request so we stop polling.
                    self.pane_cmdlines_request = None;
                    break;
                }
            }
        }
        if updates.is_empty() {
            return;
        }
        // Patch each update into the matching
        // pane row. The `command` field is what
        // the renderer shows as the row's
        // primary text.
        for (pane_id, cmdline) in &updates {
            if let Some(row) = self
                .session_panes
                .iter_mut()
                .find(|r| r.mode == "pane" && &r.session_id == pane_id)
            {
                // Combine the agent name
                // (if any) with the
                // cmdline (if any). When
                // the agent and the
                // cmdline's first token
                // match (e.g.
                // agent="pi" and
                // argv0="pi"), show
                // only the cmdline
                // (avoids `pi pi`).
                let agent = row.command.clone();
                let combined = if agent.is_empty() {
                    cmdline.clone()
                } else {
                    let cmd_first = cmdline.split_whitespace().next().unwrap_or("");
                    if cmd_first.eq_ignore_ascii_case(&agent) {
                        cmdline.clone()
                    } else {
                        format!("{} {}", agent, cmdline)
                    }
                };
                row.command = combined;
            }
        }
        // Re-fetch the panes so the patched
        // `session_panes` values flow into
        // `self.rows` (which `build_merged_rows`
        // reads in panes mode), then rebuild
        // the merged list so the next render
        // picks up the updated `command` fields.
        // Only do this if we're actually in
        // panes mode — otherwise the rebuild
        // is wasted work (and could thrash
        // the main view).
        //
        // The new `self.rows` from `fetch()` has
        // empty `preview` fields (the fetch
        // itself doesn't load any pane
        // content). The previous `self.rows`
        // may have populated `preview` values
        // (written by `panes::ensure_selected_context`
        // when the user selected a row, or by
        // the user re-selecting after a previous
        // read). Without preservation, the
        // rebuild would wipe the previews and
        // the user would see the row's
        // `tab_id` (`row.output`) for one frame
        // (until the next `ensure_selected_context`
        // re-populated) — the "toggling" bug
        // reported alongside the cmdline
        // background-thread feature.
        //
        // Snapshot the old previews by pane_id
        // before the re-fetch, then copy them
        // back onto the matching new rows
        // afterward. Workspace and session rows
        // don't carry previews so they're
        // skipped (their pane_id is empty in
        // our snapshot map, so a `Some("")`
        // entry would be a no-op anyway).
        if self.is_panes_query() {
            let mut old_previews: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for row in &self.rows {
                if row.mode == "pane" && !row.preview.is_empty() {
                    old_previews.insert(row.session_id.clone(), row.preview.clone());
                }
            }
            self.rows = self.fetch().unwrap_or_default();
            // Restore the previews onto the
            // matching new rows.
            for row in self.rows.iter_mut() {
                if row.mode == "pane"
                    && let Some(preview) = old_previews.get(&row.session_id)
                {
                    row.preview = preview.clone();
                }
            }
            self.merged_rows = self.build_merged_rows();
        }
    }

    // `fetch_panes` was extracted to
    // `crate::tui::mode::panes::fetch` (151 lines, the multiplexer
    // snapshot / panes-filter / token-filter / group-aware filter
    // pipeline). The dispatch in `App::fetch` calls the per-mode
    // function via `ModeKind::Panes`.


    // `fetch_files` was extracted to
    // `crate::tui::mode::files::fetch` (the
    // cached-rows clone is one line; the
    // interesting logic is the background walk
    // that `files_maybe_autocall` →
    // `spawn_files_walk` → `process_files_result`
    // manages).

    // `fetch_tags` was extracted to
    // `crate::tui::mode::tags::fetch` (the TAGS-file parser +
    // `@lang` filter + token filter for the `$` mode).


    // `ensure_selected_tag_context` was extracted to
    // `crate::tui::mode::tags::ensure_selected_context` (the
    // source-context + CodeGraph-fallback callers/callees
    // overlay for the selected `$`-mode row). The dispatch
    // in the call sites uses the per-mode function directly.

    // `fetch_tags_via_codegraph` was extracted to
    // `crate::tui::mode::tags::fetch_via_codegraph` (the `$`-mode
    // fallback when no TAGS file exists: queries the CodeGraph
    // index and tags the rows with `mode: "tags"` so the
    // existing tags dispatch works unchanged).


    // `fetch_codegraph` was extracted to
    // `crate::tui::mode::codegraph::fetch` (the FTS5 search + result
    // shaping for the `&` mode).


    // `ensure_selected_codegraph_context` was extracted to
    // `crate::tui::mode::codegraph::ensure_selected_context`
    // (the source-context + callers/callees overlay
    // for the selected `&`-mode row). The dispatch
    // in the call sites uses the per-mode function
    // directly.

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
        let db_normalized = crate::util::normalize_for_compare(dir, &self.home_list);
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
                let tmux_normalized = crate::util::normalize_for_compare(&w.path, &self.home_list);
                tmux_normalized == db_normalized
            })
            .map(|w| w.pane_id.clone())
    }

    // `fetch_tmux_windows` was extracted to
    // `crate::tui::mode::directories::ensure_multiplexer_snapshot`
    // (the lazy population of `app.tmux_windows` from the
    // configured multiplexer backend; called once per
    // directories-mode entry).


    // `fetch_notes` was extracted to
    // `crate::tui::mode::notes::fetch` (the note_search
    // database query + date-filter / token-filter for the
    // `@` mode). The two `App` fields it writes
    // (`notes_date_filter`, `notes_query_error`) are read
    // back by the renderer; the per-mode free function
    // mutates `app.notes_date_filter` directly so the
    // existing field accessors continue to work.


    // `fetch_recent_notes_with_filter` was extracted to
    // `crate::tui::mode::notes::fetch_recent_with_filter`
    // (the no-pattern-all-notes path that the `fetch`
    // dispatch takes when the user types a bare date
    // alias like `@today`).


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
        if self.match_algorithm != MatchAlgorithm::Regex {
            self.query_regex = None;
            return;
        }
        let pattern = build_implicit_regex(self.search_body());
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

    /// Cycle the match algorithm:
    /// Substring → Fuzzy → Regex → Substring.
    /// The query string is NOT modified — the algorithm
    /// is a separate top-level state, not encoded in the
    /// query prefix. This is the key difference from the
    /// old `cycle_search_mode` which swapped the leading
    /// char. `C-f` (default) triggers this.
    ///
    /// JIRA mode (`-` prefix) is exempt — the JQL parser
    /// doesn't benefit from a match-algorithm overlay, and
    /// the user's `@alias` / `field=value` syntax is its
    /// own query language. We still allow the cycle in JIRA
    /// mode (the algorithm is just ignored), so the user's
    /// setting is preserved when they switch back.
    fn cycle_match_algorithm(&mut self) {
        self.match_algorithm = self.match_algorithm.next();
        self.recompile_regex();
        self.refresh();
        self.llm_touch();
    }

    /// Backward-compatibility shim: old callers (tests,
    /// action handlers) used `cycle_search_mode` to toggle
    /// via F3. We now cycle the algorithm instead. The
    /// query string is not modified (no prefix swap).
    fn cycle_search_mode(&mut self) {
        self.cycle_match_algorithm();
    }

    /// Cycle to the next prefix mode. The cycle
    /// order is:
    ///
    /// 1. **No prefix** (history) — the
    ///    default text-search mode.
    /// 2. `+` (output)
    /// 3. `=` (LLM)
    /// 4. `%` (question)
    /// 5. `@` (notes)
    /// 6. `!` (todo)
    /// 7. `#` (directories)
    /// 8. `*` (panes)
    /// 9. `-` (JIRA)
    /// 10. `~` (files)
    /// 11. `$` (tags)
    /// 12. `,` (ag)
    /// 13. **No prefix** (history) — wrap
    ///    back to the start.
    ///
    /// The body of the query (everything
    /// after the current prefix) is
    /// preserved verbatim. So `git status`
    /// → `+git status` →
    /// `=git status` → ...; the user
    /// only changes the leading char
    /// (or removes it on the wrap).
    ///
    /// Custom prefix chars (configured
    /// via `prefix.<mode>=<char>` in the
    /// config file) are honoured: the
    /// cycle walks the *fields* of
    /// `QueryPrefixes`, not the literal
    /// default chars, so a user who has
    /// rebound `prefix.jira=X` still
    /// cycles through their `X` in
    /// the JIRA slot.
    ///
    /// After the cycle, the run-loop
    /// tick sees the new prefix and
    /// fires the per-mode search on
    /// the next iteration (the LLM /
    /// JIRA / files / ag / ag debounces
    /// are all armed by the
    /// `llm_touch()` call below).
    /// The synchronous modes (history,
    /// output, directories, panes,
    /// notes, todos) get an immediate
    /// `refresh()` so the row set is
    /// up-to-date on the same frame.
    ///
    /// Outside of any prefixable state
    /// (e.g. inside the comment
    /// editor or the add-entry
    /// dialog) the action is a
    /// no-op so the key doesn't
    /// interfere with anything
    /// else. The dispatch site
    /// re-checks the state to keep
    /// the contract clear.
    /// Apply a new prefix to the query. The body
    /// (everything after the current prefix, or
    /// the whole query if there is no prefix) is
    /// preserved verbatim. Used by the prefix
    /// picker to commit the selected mode and
    /// by any future caller that needs to swap
    /// the leading char.
    fn apply_prefix(&mut self, new_prefix: Option<char>) {
        // Determine the body:
        // if the query starts with a known
        // prefix char, drop it; otherwise the
        // entire query IS the body.
        let first = self.query.chars().next();
        let has_prefix = first.is_some_and(|c| {
            let prefixes = &self.query_prefixes;
            c == prefixes.output
                || c == prefixes.llm
                || c == prefixes.question
                || c == prefixes.notes
                || c == prefixes.todo
                || c == prefixes.directories
                || c == prefixes.panes
                || c == prefixes.jira
                || c == prefixes.files
                || c == prefixes.tags
                || c == prefixes.ag
        });
        let body = if has_prefix {
            self.query.chars().skip(1).collect::<String>()
        } else {
            self.query.clone()
        };
        self.query = match new_prefix {
            Some(c) => format!("{}{}", c, body),
            None => body,
        };
        self.query_cursor = self.query.chars().count();
        self.query_touched = true;
        self.recompile_regex();
        self.llm_touch();
        self.refresh();
        let label = match new_prefix {
            Some(c) => format!("`{}`", c),
            None => "history (no prefix)".to_string(),
        };
        self.set_status_message(format!("prefix: {}", label));
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
        // Same co-location for the ag-mode
        // search debounce. `ag_touch` is a
        // no-op outside `,` mode.
        self.ag_touch();
    }

    /// Fire the per-mode search immediately on a
    /// text change. The user reported that the
    /// JIRA search "sometimes isn't executed";
    /// for non-JIRA modes, the corresponding
    /// complaint is "the search lags my
    /// typing". This helper is the
    /// non-JIRA counterpart to the JIRA
    /// debounce/idle/space-trigger paths: every
    /// text-mutating action (push_char,
    /// backspace, delete_word_backward, tab
    /// completion, etc.) calls into here so the
    /// visible result list updates on the same
    /// frame as the keystroke.
    ///
    /// Behaviour by mode:
    ///
    /// - **JIRA (`-`)**: no-op. The JIRA
    ///   mode has its own dual-timer debounce
    ///   (400ms fast + 3s idle safety-net)
    ///   and the space trigger. Those
    ///   paths are driven by
    ///   `jira_touch()` and the
    ///   space-key code in
    ///   `push_char()`; mixing in a
    ///   per-keystroke fire would
    ///   defeat the debounce and
    ///   re-introduce the JIRA-server
    ///   spam the debounce was
    ///   designed to prevent.
    ///
    /// - **LLM (`=`)**: force-fire the
    ///   LLM auto-call. The
    ///   `llm_touch()` call from
    ///   `push_char` arms the 1s
    ///   debounce; we override
    ///   that here so the call
    ///   fires on the same frame
    ///   as the keystroke. The
    ///   user has typed a
    ///   description; they want
    ///   to see a preview now,
    ///   not after 1s of typing
    ///   latency. (The 1s
    ///   debounce is still in
    ///   place for the cases
    ///   where the user is
    ///   mid-edit and the call
    ///   is already in flight
    ///   — `llm_in_flight` short-
    ///   circuits the auto-call
    ///   path until the
    ///   in-flight call
    ///   completes.)
    ///
    /// - **All other modes** (SESS,
    ///   DIR, GLOBAL, STATS,
    ///   panes `*`, directories
    ///   `#`, symbols `$`,
    ///   todos `!`, notes `@`,
    ///   tags `$`, ag `,`,
    ///   files `~`): call
    ///   `self.refresh()` so the
    ///   row set is re-fetched
    ///   immediately. The
    ///   fetch is a synchronous
    ///   SQL query (or in the
    ///   case of files/ag, an
    ///   in-process walk) —
    ///   fast enough that a
    ///   per-keystroke fire is
    ///   well within the TUI's
    ///   frame budget. Empty
    ///   queries bail out
    ///   without firing (no
    ///   point re-fetching
    ///   the same all-rows
    ///   result set the user
    ///   just had before
    ///   they cleared the
    ///   box).
    ///
    /// Note: this method is
    /// intentionally a no-op when
    /// the comment editor or the
    /// add-entry dialog is open
    /// (those manage their own
    /// text and have their own
    /// refresh paths). The caller
    /// — `push_char` /
    /// `backspace` etc. —
    /// already short-circuits
    /// to the comment buffer
    /// in that case, so by the
    /// time we get here the
    /// query is the live
    /// field.
    fn trigger_text_change_search(&mut self) {
        // JIRA has its own
        // debounce/idle/space
        // machinery. But on the
        // FIRST entry into JIRA
        // mode (no search has
        // ever fired AND the query
        // body is empty — the user
        // just typed `-`), fire
        // immediately so the user
        // sees a results list the
        // moment they enter `-`
        // mode, not after the 400ms
        // debounce. Subsequent
        // keystrokes (which add a
        // non-empty body) use the
        // normal debounce.
        if self.is_jira_query() {
            if self.jira_last_jql.is_none()
                && self.jira_pattern().trim().is_empty()
            {
                let past = std::time::Instant::now()
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50);
                self.jira_debounce_started = Some(past);
                self.jira_idle_started = Some(past);
                self.jira_maybe_autocall();
            }
            return;
        }
        // No query -> no search
        // (an empty body in
        // most modes just
        // shows the all-rows
        // list, which is
        // already the visible
        // state).
        if self.query.is_empty() {
            return;
        }
        if self.is_llm_query() {
            // The user typed (or
            // backspaced)
            // something in the
            // LLM description.
            // Force-fire the
            // auto-call now
            // rather than
            // waiting for the
            // 1s debounce.
            //
            // `llm_maybe_autocall`
            // checks its own
            // guards (client
            // configured, not
            // in-flight, has a
            // description); the
            // only thing we
            // need to override
            // is the debounce
            // window. Setting
            // the debounce to
            // a value past
            // `LLM_DEBOUNCE`
            // makes the gate
            // see "elapsed"
            // and proceed.
            self.llm_debounce_started = Some(
                std::time::Instant::now() - LLM_DEBOUNCE - std::time::Duration::from_millis(50),
            );
            self.llm_maybe_autocall();
            return;
        }
        // Synchronous modes
        // (SESS, DIR, GLOBAL,
        // STATS, panes,
        // directories,
        // symbols, todos,
        // notes, tags, ag,
        // files): refresh
        // immediately.
        //
        // `refresh()` is a
        // no-op on the
        // result set when the
        // query hasn't
        // changed in a way
        // that affects the
        // fetch; calling it
        // on every keystroke
        // is therefore
        // cheap (the SQL
        // query plan is
        // cached) and
        // gives the user
        // instant feedback.
        self.refresh();
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
    fn spawn_llm_request(&mut self, request_type: LlmRequestType, prompt: String) {
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
                self.set_status_message(crate::llm::LlmError::NotConfigured.to_string());
            }
            return;
        }

        let Some(ref cfg) = self.llm_config else {
            self.set_status_message(crate::llm::LlmError::NotConfigured.to_string());
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

            ..Default::default()
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
        let body = self.search_body();
        if body.is_empty() {
            return true;
        }
        let case_sensitive = self.is_case_sensitive();
        match self.match_algorithm {
            MatchAlgorithm::Regex => {
                if let Some(ref re) = self.query_regex {
                    return re.is_match(text);
                }
                // Regex mode but no valid compiled regex yet — treat
                // the body as a literal pattern so the user sees at
                // least the matches that contain it.
                if case_sensitive {
                    text.contains(body)
                } else {
                    text.to_lowercase().contains(&body.to_lowercase())
                }
            }
            MatchAlgorithm::Fuzzy => {
                // Fuzzy search: every whitespace-separated word in the
                // body must be a fuzzy subsequence of the text.
                if case_sensitive {
                    body.split_whitespace().all(|term| fuzzy_match(term, text))
                } else {
                    // Case-insensitive: lower-case both sides
                    // before the subsequence check.
                    let lc_text = text.to_lowercase();
                    body.split_whitespace()
                        .all(|term| fuzzy_match(&term.to_lowercase(), &lc_text))
                }
            }
            MatchAlgorithm::Substring => {
                // Plain text: every whitespace-separated word must
                // appear. Case-sensitive when the body has
                // uppercase, case-insensitive otherwise.
                if case_sensitive {
                    text.split_whitespace().all(|_| true) // handled below
                        && body
                            .split_whitespace()
                            .all(|w| text.contains(w))
                } else {
                    let lower = text.to_lowercase();
                    body.split_whitespace()
                        .all(|w| lower.contains(&w.to_lowercase()))
                }
            }
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfirmMode {
    DeleteSelected,
    DeleteMatching,
    /// Delete ALL history entries in the selected
    /// directory (directory mode only). The count is
    /// pre-computed when the dialog opens so the
    /// confirmation message can show it.
    DeleteDirectory {
        directory: String,
        count: usize,
    },
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
    Generate { description: String },
    /// A `Ctrl-K` describe request.
    Describe { command: String },
    /// A `Ctrl-T` correct request.
    Correct { original_command: String },
    /// A `%...` general question request.
    Question { question: String },
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

/// An in-flight pane-cmdline lookup request.
/// Spawned by `App::spawn_pane_cmdlines` when
/// the `*`-mode panes view is populated. The
/// background thread calls
/// `multiplexer::herdr_pane_cmdline` for each
/// pane in the snapshot and streams results
/// back over the channel; the run loop polls
/// them in `process_pane_cmdlines`.
///
/// The result is `Vec<(pane_id, cmdline)>` —
/// one entry per pane whose cmdline was
/// successfully looked up. Panes whose lookup
/// fails (or whose `process-info` returns no
/// foreground process) are simply absent
/// from the result; the row keeps its initial
/// agent-name display.
///
/// The `snapshot_id` tracks which snapshot
/// the request belongs to, so a stale result
/// from a superseded snapshot (the user
/// refreshed the panes view while a previous
/// lookup was in flight) is discarded rather
/// than overwriting the new snapshot.
struct PaneCmdlineRequest {
    receiver: mpsc::Receiver<(String, String)>,
    cancelled: Arc<AtomicBool>,
    /// Monotonic counter incremented on each
    /// `fetch_session_panes_impl` call. The
    /// spawned thread stashes the counter at
    /// spawn time; the App checks it on
    /// receipt to detect stale results.
    snapshot_id: u64,
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
    receiver: mpsc::Receiver<Result<Vec<crate::jira::JiraComment>, crate::jira::JiraError>>,
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
#[allow(dead_code)]
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
    let header_block: Vec<&str> = all_lines.by_ref().take(3).collect();
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
                words.iter().all(|w| name.contains(w) || key.contains(w))
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

/// The prefix picker — a centred list of every
/// configured prefix mode. Modelled on
/// [`CommandMenu`] but smaller: there's no
/// filter query (the list has 12 entries),
/// every entry is a `(label, prefix_char,
/// description)` triple, and the user
/// navigates with Up/Down (or `j`/`k` /
/// `Ctrl-N` / `Ctrl-P`) and commits with
/// Enter. The picker pre-selects the row
/// matching the current query's leading
/// char (or the "no prefix" row for a
/// bare text query), so Enter with no
/// navigation is a no-op.
struct PrefixPicker {
    /// The full ordered list of prefix
    /// options. `None` represents the
    /// "no prefix" (history) entry at
    /// the top of the picker. The
    /// remaining entries carry the
    /// literal `char` from the user's
    /// `QueryPrefixes` config (so
    /// `prefix.jira=X` rebinds show up
    /// as `X` here, not `-`).
    options: Vec<PrefixOption>,
    /// Index into `self.options` of the
    /// currently-highlighted entry.
    /// Clamped to `0..options.len()`
    /// whenever the picker is opened so
    /// the user can never navigate past
    /// the last entry.
    selected: usize,
}

/// One row in the prefix picker. The
/// `prefix` is `None` for the "no
/// prefix" (history) entry at the top
/// of the list; every other entry
/// carries the literal `char` the user
/// would type to enter that mode.
#[derive(Clone, Copy)]
struct PrefixOption {
    /// `None` = the "no prefix"
    /// (history) entry. `Some(c)` =
    /// the literal prefix char the
    /// user would type.
    prefix: Option<char>,
    /// Short human-readable label
    /// for the row, e.g.
    /// "Output", "LLM command",
    /// "JIRA search". The renderer
    /// uses this for the left
    /// column.
    label: &'static str,
    /// One-line description
    /// shown in the second
    /// column. Helps the user
    /// remember which prefix
    /// does what without
    /// flipping to the help
    /// overlay.
    description: &'static str,
}

/// Section a CodeGraph relations-picker entry belongs to. The
/// picker renders two sections in one flat navigable list:
/// callers (who calls the selected symbol) and callees (what the
/// selected symbol calls). Each section gets a header row.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CodegraphRelationSection {
    Caller,
    Callee,
}

impl CodegraphRelationSection {
    fn header(self) -> &'static str {
        match self {
            CodegraphRelationSection::Caller => "── callers ──",
            CodegraphRelationSection::Callee => "── callees ──",
        }
    }
}

/// One navigable entry in the CodeGraph relations picker. The
/// `section` tag lets the renderer insert a section header row
/// when the section changes between consecutive entries; the
/// `node` carries the symbol's qualified name, file path, and
/// line for both display and the Enter action (open in $EDITOR).
struct CodegraphRelationEntry {
    section: CodegraphRelationSection,
    node: crate::codegraph::CodeGraphNode,
}

/// Overlay picker for CodeGraph callers / callees. Opened by the
/// `CodegraphRelations` action (`C-r` by default) when the
/// selected `&` / `$` row carries a CodeGraph node id. Up/Down
/// navigate, Enter opens the highlighted relation's source file
/// in `$EDITOR +LINE path` (and exits the TUI so the parent shell
/// runs it, mirroring the main list's tags/codegraph selection),
/// Esc / Cancel closes without opening anything.
struct CodeGraphRelationsPicker {
    /// Flat list with section tags; the renderer emits a header row
    /// whenever an entry's section differs from the previous one's.
    /// Callers come first, then callees.
    entries: Vec<CodegraphRelationEntry>,
    /// Index into `entries` of the highlighted row. Header rows
    /// are not entries — they're synthesized at render time — so
    /// this index always points at a real, selectable node.
    selected: usize,
    /// The symbol whose relations are shown, for the title bar.
    symbol: String,
    /// The repo root (the directory containing `.codegraph/`),
    /// captured at open time so the Enter action can resolve a
    /// relation's relative `file_path` to an absolute editor-
    /// openable path without re-touching the CodeGraph client.
    repo_root: std::path::PathBuf,
}

impl PrefixPicker {
    /// Build the picker from the user's
    /// configured `QueryPrefixes`. The
    /// order matches the
    /// `QueryPrefixes` field-declaration
    /// order, with a "no prefix" entry
    /// at the top so the user can
    /// always cycle back to a plain
    /// text search with one Up press.
    ///
    /// `current_prefix` is the leading
    /// char of the user's current
    /// query (or `None` for a bare
    /// text query). The picker
    /// pre-selects the matching row
    /// (or the "no prefix" row if
    /// the leading char isn't one
    /// of the configured prefixes).
    fn new(prefixes: &QueryPrefixes, current_prefix: Option<char>) -> Self {
        let options = vec![
            PrefixOption {
                prefix: None,
                label: "History",
                description: "search shell history (no prefix)",
            },
            PrefixOption {
                prefix: Some(prefixes.output),
                label: "Output",
                description: "search captured command output",
            },
            PrefixOption {
                prefix: Some(prefixes.llm),
                label: "LLM command",
                description: "ask the LLM to generate a shell command",
            },
            PrefixOption {
                prefix: Some(prefixes.question),
                label: "Question",
                description: "ask the LLM a short factual question",
            },
            PrefixOption {
                prefix: Some(prefixes.notes),
                label: "Notes",
                description: "search the note_search SQLite database",
            },
            PrefixOption {
                prefix: Some(prefixes.todo),
                label: "Todos",
                description: "list open markdown todo items",
            },
            PrefixOption {
                prefix: Some(prefixes.directories),
                label: "Directories",
                description: "list every directory in the global history",
            },
            PrefixOption {
                prefix: Some(prefixes.panes),
                label: "Panes",
                description: "list every pane across tmux / herdr sessions",
            },
            PrefixOption {
                prefix: Some(prefixes.jira),
                label: "JIRA",
                description: "search JIRA issues via the REST API",
            },
            PrefixOption {
                prefix: Some(prefixes.files),
                label: "Files",
                description: "list every file under the current directory",
            },
            PrefixOption {
                prefix: Some(prefixes.tags),
                label: "Tags",
                description: "list every symbol in the local ctags `tags` file",
            },
            PrefixOption {
                prefix: Some(prefixes.codegraph),
                label: "CodeGraph",
                description: "search symbols in the local .codegraph index (callers/callees)",
            },
            PrefixOption {
                prefix: Some(prefixes.ag),
                label: "ag search",
                description: "search file contents with `ag` (The Silver Searcher)",
            },
        ];
        // Pre-select the row
        // matching the current
        // prefix. If the current
        // prefix is unknown (e.g.
        // the user typed a
        // custom rebind that
        // isn't a real `QueryPrefixes`
        // field, or the query
        // is empty) fall back to
        // the "no prefix" row at
        // index 0.
        let selected = current_prefix
            .and_then(|c| options.iter().position(|o| o.prefix == Some(c)))
            .unwrap_or(0);
        PrefixPicker { options, selected }
    }

    /// Highlighted entry, or `None` if the
    /// list is empty (defensive — should
    /// never happen because the
    /// `PrefixPicker::new` constructor
    /// always populates the list).
    fn selected(&self) -> Option<&PrefixOption> {
        self.options.get(self.selected)
    }
}

impl CodeGraphRelationsPicker {
    /// The highlighted entry, or `None` when the list is empty
    /// (defensive — the opener only constructs the picker when at
    /// least one caller or callee exists).
    fn selected(&self) -> Option<&CodegraphRelationEntry> {
        self.entries.get(self.selected)
    }
}

/// The completion menu — a popup that
/// shows all matching tab-completion
/// candidates when the user presses
/// `Tab` and the completion is
/// ambiguous (multiple matches).
/// Modelled on `CommandMenu` /
/// `ThemePicker` / `PrefixPicker` so
/// muscle memory transfers across
/// overlays. Unlike those pickers,
/// this one does NOT filter as the
/// user types — it's a fixed list of
/// candidates collected at the moment
/// the menu opened. The user navigates
/// with `Up`/`Down` (or `Ctrl-N`/
/// `Ctrl-P` / `j`/`k`), commits with
/// `Enter`, and dismisses with `Esc` or
/// the user's `Cancel` binding.
struct CompletionMenu {
    /// The full list of candidates
    /// (the raw match names, e.g.
    /// `"NeovimNote"` for a link, or
    /// `"assignee"` for a JIRA
    /// field). The completion menu
    /// applies the appropriate
    /// prefix (`#` / `@` / `[[...]]`)
    /// and suffix (` ` / `=`) when the
    /// user commits a selection.
    candidates: Vec<String>,
    /// Index into `candidates` of
    /// the currently-highlighted
    /// entry. Clamped to
    /// `0..candidates.len()`
    /// whenever the menu is opened.
    selected: usize,
    /// The byte range in `self.query`
    /// that the original prefix
    /// occupied. When the user
    /// commits a candidate, the menu
    /// replaces this range with the
    /// formatted completion (the
    /// raw match name + the
    /// appropriate prefix and
    /// suffix). Stored as
    /// `(start_byte, end_byte)` so the
    /// replacement is exact
    /// regardless of how many
    /// characters the prefix spans.
    replace_start_byte: usize,
    /// End byte of the original
    /// prefix in `self.query`.
    replace_end_byte: usize,
    /// Start character index of the
    /// original prefix in
    /// `self.query`. Used to
    /// position the cursor after the
    /// replacement.
    replace_start_char: usize,
    /// What kind of completion this
    /// is. Determines how the
    /// selected candidate is
    /// formatted when applied:
    /// `JiraField` adds `=`,
    /// `JiraAlias` / `NotesTag` add
    /// ` `, `NotesLink` wraps in
    /// `[[...]]` (with quotes for
    /// spaced names).
    kind: CompletionKind,
}

/// What kind of completion the menu
/// represents. The kind determines how
/// the selected candidate is formatted
/// when the user commits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionKind {
    /// JIRA field name (e.g.
    /// `assignee`). The selected
    /// candidate is applied with a
    /// trailing `=` so the user
    /// can immediately type the
    /// value.
    JiraField,
    /// JIRA `@` alias or fragment
    /// (e.g. `me`, `today`, or a
    /// user-defined `jira.search.*`
    /// name). Applied with a
    /// leading `@` and trailing
    /// space.
    JiraAlias,
    /// Note tag (e.g. `feature`).
    /// Applied with a leading `#`
    /// and trailing space.
    NotesTag,
    /// Note wiki-link (e.g.
    /// `NeovimNote`). Applied
    /// wrapped in `[[...]]` with a
    /// trailing space. Link names
    /// containing a space are
    /// additionally wrapped in
    /// double quotes inside the
    /// brackets.
    NotesLink,
}

impl CompletionMenu {
    /// Build the menu from a list of
    /// candidates. The first entry
    /// is pre-selected so Enter
    /// with no navigation picks the
    /// top match (the same
    /// behaviour as the command
    /// palette and theme picker).
    fn new(
        candidates: Vec<String>,
        replace_start_byte: usize,
        replace_end_byte: usize,
        replace_start_char: usize,
        kind: CompletionKind,
    ) -> Self {
        CompletionMenu {
            candidates,
            selected: 0,
            replace_start_byte,
            replace_end_byte,
            replace_start_char,
            kind,
        }
    }

    /// Highlighted candidate, or
    /// `None` if the list is empty
    /// (defensive — should never
    /// happen because we only
    /// open the menu when there
    /// are 2+ candidates).
    fn selected(&self) -> Option<&str> {
        self.candidates.get(self.selected).map(|s| s.as_str())
    }

    /// Format the selected candidate
    /// for insertion into the
    /// query. The formatting matches
    /// the single-match behaviour of
    /// the corresponding completion
    /// function: JIRA fields get a
    /// trailing `=`, aliases and
    /// tags get a leading prefix
    /// char and trailing space,
    /// links get wrapped in
    /// `[[...]]` and a trailing
    /// space. The brackets
    /// unambiguously delimit the
    /// link target even when it
    /// contains a space, so no
    /// additional quoting is
    /// needed.
    fn format_selected(&self) -> String {
        let Some(name) = self.selected() else {
            return String::new();
        };
        match self.kind {
            CompletionKind::JiraField => format!("{}=", name),
            CompletionKind::JiraAlias => format!("@{} ", name),
            CompletionKind::NotesTag => format!("#{} ", name),
            CompletionKind::NotesLink => format!("[[{}]] ", name),
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
        smart_open_file_commands: std::collections::HashMap<String, String>,
        multiplexer: Box<dyn crate::multiplexer::MultiplexerBackend>,
        pane_visibility: crate::tui::state::PaneVisibility,
        pane_height: crate::tui::state::PaneHeight,
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
            prefix_picker: None,
            codegraph_relations_picker: None,
            theme_picker: None,
            completion_menu: None,
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
            match_algorithm: MatchAlgorithm::default(),
            pane_visibility,
            pane_height,
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
            sessions: Vec::new(),
            hosts: Vec::new(),
            host_defs: Vec::new(),
            add_entry_dialog: None,
            panes_filter: PanesFilter::default(),
            pane_cmdlines_request: None,
            panes_snapshot_id: 0,
            // Lazy pane-preview memoization: maps pane_id
            // → `Instant` of last read. The map itself
            // is `Option` so we don't pay the
            // allocation until the first `*`-mode
            // preview is actually requested.
            pane_preview_cache: None,
            // Multiplexer backend
            // (tmux / herdr).
            // The staging layer
            // calls into this
            // for snapshot and
            // focus / create /
            // send-in-pane
            // commands. The
            // concrete backend
            // is selected by
            // `run_tui_to_stdout`
            // from
            // `Config::multiplexer()`.
            multiplexer,
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
            directory_source: crate::tui::state::DirectorySource::All,
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
            ag_state: crate::ag::AgState::new(),
            files_state: crate::files::FilesState::new(),
            files_ignores,
            jira_rows: Vec::new(),
            jira_request: None,
            jira_in_flight: false,
            jira_debounce_started: None,
            jira_idle_started: None,
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
            smart_open_file_commands,
            tags_source_cache: std::collections::HashMap::new(),
            codegraph_client: None,
            mode_query_history: std::collections::HashMap::new(),
            mode_query_drafts: std::collections::HashMap::new(),
            mode_query_history_index: std::collections::HashMap::new(),
            last_fetch_key: None,
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
        // Both timers (fast debounce and idle safety-net)
        // are armed in lock-step so the bookkeeping
        // stays consistent regardless of which one fires
        // first.
        if app.is_jira_query() {
            // Set the debounce to the past so
            // `jira_maybe_autocall` fires the
            // initial search on the first frame
            // rather than waiting `JIRA_DEBOUNCE`.
            // The user expects to see a results list
            // the moment they enter `-` mode, not
            // after 400ms of quiet staring.
            //
            // NB: `spawn_jira_request` sets
            // `jira_last_jql` when the search
            // actually fires. We must NOT set it
            // here — the old code did
            // `app.jira_last_jql = Some(jql)` before
            // the search fired, which tripped the
            // "already have results for this JQL"
            // guard in `jira_maybe_autocall` and
            // the first search NEVER fired.
            let past = std::time::Instant::now()
                - JIRA_DEBOUNCE
                - std::time::Duration::from_millis(50);
            app.jira_debounce_started = Some(past);
            app.jira_idle_started = Some(past);
            app.jira_maybe_autocall();
            // Eagerly build the JQL so the input
            // border title shows it on the first
            // frame. `spawn_jira_request` will
            // overwrite this with the same value
            // when the search actually fires.
            let jql = app.jira_build_query();
            if app.jira_last_jql.is_none() {
                // `jira_maybe_autocall` may have
                // already set this (when the search
                // fired synchronously via the test
                // client path). Only set it when
                // it's still `None` (the search is
                // running in the background and
                // hasn't completed yet — the JQL
                // for display is the same).
                app.jira_last_jql = Some(jql);
            }
        }
        // If the restored query is an ag query, arm the
        // debounce and fire the search immediately so the
        // user sees results on the first frame rather than
        // an empty list. This mirrors the JIRA path above.
        if app.is_ag_query() {
            app.ag_state.debounce_started = Some(std::time::Instant::now());
            app.ag_maybe_autocall();
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
        // Clear the fetch cache so the first user-triggered
        // `refresh()` always re-queries (tests insert rows
        // after `App::new` and expect them to appear).
        app.last_fetch_key = None;
        app
    }

    /// Re-query the database with the current mode + query.
    /// After re-querying, land on the newest match (index 0 in the
    /// merged list, which is the bottom of the bottom-aligned render).
    /// When the query is a regex, post-filter the SQL results using
    /// `query_matches_text` so the regex can match anywhere in the
    /// command or comment text.
    fn refresh(&mut self) {
        // Short-circuit: if the query text, mode, match
        // algorithm, exit filter, sort order, and directory
        // source are all unchanged since the last `fetch()`, the
        // SQL rows are still valid and there's no point
        // re-querying. This saves one SQLite `prepare()` +
        // `query_map()` round-trip on every keystroke that
        // doesn't actually change the query (e.g. cursor
        // movement, overlay open/close, comment edits).
        //
        // The cache key is a simple tuple of everything that
        // affects the SQL result: the query text, the scope
        // mode, the exit filter, the sort order, and the match
        // algorithm (Regex/Fuzzy skip the SQL filter, so the
        // result differs). We don't include the
        // `duplicate_filter` because that's applied in
        // `build_merged_rows`, not in `fetch()`.
        let cache_key = (
            self.query.clone(),
            self.mode,
            self.exit_filter,
            self.match_algorithm,
        );
        if self.last_fetch_key.as_ref() == Some(&cache_key) {
            // Still need to rebuild the merged rows (the
            // duplicate filter may have been toggled).
            self.merged_rows = self.build_merged_rows();
            // Re-select row 0 if the list is non-empty.
            let n = self.merged_rows.len();
            if n == 0 {
                self.list_state.select(None);
            } else if self.list_state.selected().is_none() {
                self.list_state.select(Some(0));
            }
            // Still need to load the lazy context.
            crate::tui::mode::tags::ensure_selected_context(self);
            crate::tui::mode::codegraph::ensure_selected_context(self);
            crate::tui::mode::notes::ensure_selected_context(self);
            crate::tui::mode::todo::ensure_selected_context(self);
            crate::tui::mode::files::ensure_selected_context(self);
            crate::tui::mode::panes::ensure_selected_context(self);
            return;
        }
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
            crate::tui::mode::directories::ensure_multiplexer_snapshot(self);
        }
        // Same one-shot cache priming for the
        // `*`-prefix panes view: populate the
        // session-panes snapshot before `fetch()`
        // reads it, so the first frame after the
        // user types `*` already shows the list.
        if self.is_panes_query() {
            crate::tui::mode::panes::refresh_session_panes(self);
        }
        self.rows = self.fetch().unwrap_or_default();
        if self.match_algorithm != MatchAlgorithm::Substring
            && !self.is_ag_query()
            && !self.is_codegraph_query()
        {
            // Two-phase borrow: copy the rows out, then post-filter.
            // Avoids the borrow checker complaining about
            // simultaneously borrowing `self.rows` and `self`.
            let body = self.search_body().to_string();
            let regex = self.query_regex.clone();
            let is_regex = self.match_algorithm == MatchAlgorithm::Regex;
            let is_fuzzy = self.match_algorithm == MatchAlgorithm::Fuzzy;
            let case_sensitive = self.is_case_sensitive();
            self.rows.retain(|r| {
                if body.is_empty() {
                    true
                } else if is_regex {
                    if let Some(ref re) = regex {
                        re.is_match(&r.command) || re.is_match(&r.comment)
                    } else {
                        // No valid regex yet (in-progress typo) — fall
                        // back to a literal substring match.
                        // Respect the case-sensitivity heuristic:
                        // if the body has uppercase, match
                        // case-sensitively; otherwise,
                        // case-insensitively.
                        if case_sensitive {
                            r.command.contains(&body) || r.comment.contains(&body)
                        } else {
                            r.command.to_lowercase().contains(&body.to_lowercase())
                                || r.comment.to_lowercase().contains(&body.to_lowercase())
                        }
                    }
                } else if is_fuzzy {
                    body.split_whitespace()
                        .all(|term| fuzzy_match(term, &r.command) || fuzzy_match(term, &r.comment))
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
        // Load the source context for the selected tags row
        // lazily now that the selection is known. Keeping this
        // out of `fetch_tags` avoids reading every source file
        // once per symbol when a large TAGS file is loaded.
        crate::tui::mode::tags::ensure_selected_context(self);
        // Same lazy load for `&` (codegraph) rows: the row's
        // `output` carries source context + callers/callees,
        // loaded only for the row under the cursor.
        crate::tui::mode::codegraph::ensure_selected_context(self);
        // Lazy-load the first 50 lines of the selected note
        // file for `@` (notes) and `!` (todo) modes. Piped
        // through `bat` for syntax highlighting.
        crate::tui::mode::notes::ensure_selected_context(self);
        crate::tui::mode::todo::ensure_selected_context(self);
        // Lazy-load the first 50 lines of the selected file
        // for `~` (files) mode. Directory rows are skipped.
        crate::tui::mode::files::ensure_selected_context(self);
        // Lazy-load the last 50 visible lines of the
        // selected herdr pane for `*` (panes) mode.
        crate::tui::mode::panes::ensure_selected_context(self);
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
        // Panes mode (`*`) is its own
        // case: each row is a unique
        // object (a real pane with its
        // own pane_id, or a workspace
        // header with its own
        // workspace id). The user
        // explicitly asked to see
        // **every** pane in the
        // tree layout — deduping by
        // `command` (the agent name or
        // empty for plain shells) would
        // collapse multiple real panes
        // into a single visible row,
        // which would defeat the entire
        // point of the tree. The
        // duplicate filter is
        // meaningful for the directory
        // list (where the same directory
        // appears in many history rows
        // and collapses to a single
        // entry) but NOT for panes mode.
        // The Frequency sort is also
        // not meaningful here — we keep
        // the snapshot's natural
        // (workspace-grouped, last-pane-bubbled)
        // order.
        if self.is_panes_query() {
            return self.rows.clone();
        }
        // Directories / JIRA / files
        // modes are completely
        // different views that must NOT
        // interleave labeled history
        // rows. The dedup-by-command
        // (collapsed directories with
        // the same path; collapsed
        // JIRA issues with the same key;
        // collapsed file rows with the
        // same path) is still meaningful
        // here and is gate-checked by
        // `duplicate_filter` (default
        // on).
        if crate::tui::mode::active_mode(self).dedup_eligible() {
            let mut merged = self.rows.clone();
            if self.duplicate_filter || self.sort_order == SortOrder::Frequency {
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                merged.retain(|r| seen.insert(r.command.clone()));
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
        let existing_ids: std::collections::HashSet<i64> = main_part.iter().map(|r| r.id).collect();
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
                    let in_output = self.is_output_query() && self.query_matches_text(&row.output);
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
                partition.sort_by_key(|b| std::cmp::Reverse(b.timestamp));
            }
            SortOrder::Frequency => {
                let mut counts: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                let mut newest: std::collections::HashMap<String, i64> =
                    std::collections::HashMap::new();
                for r in partition.iter() {
                    *counts.entry(r.command.clone()).or_insert(0) += 1;
                    let n = newest.entry(r.command.clone()).or_insert(i64::MIN);
                    if r.timestamp > *n {
                        *n = r.timestamp;
                    }
                }
                partition.sort_by(|a, b| {
                    let ca = counts.get(&a.command).copied().unwrap_or(0);
                    let cb = counts.get(&b.command).copied().unwrap_or(0);
                    let na = newest.get(&a.command).copied().unwrap_or(i64::MIN);
                    let nb = newest.get(&b.command).copied().unwrap_or(i64::MIN);
                    // Primary: count DESC.
                    // Secondary: per-command newest
                    // timestamp DESC. Tertiary:
                    // per-row timestamp DESC
                    // (newer instances of the same
                    // command come first).
                    cb.cmp(&ca)
                        .then_with(|| nb.cmp(&na))
                        .then_with(|| b.timestamp.cmp(&a.timestamp))
                });
            }
        }
    }

    fn fetch(&mut self) -> Result<Vec<HistoryRow>> {
        if matches!(self.mode, Mode::Stats) {
            return crate::tui::stats::fetch(self);
        }
        // The per-mode fetch dispatch. The modes are
        // mutually exclusive (the first char of the
        // query determines the active mode) so a flat
        // `match` on `ModeKind` is the canonical form;
        // each per-mode `fetch_*` orchestration is
        // free to read / mutate any `App` state it
        // needs because the match arm is the only
        // borrow of `self` in that arm. The history /
        // no-prefix fall-through runs the SQL
        // `SELECT` below.
        match crate::tui::mode::active_mode(self) {
            crate::tui::mode::ModeKind::Todo => return crate::tui::mode::todo::fetch(self),
            crate::tui::mode::ModeKind::Notes => return crate::tui::mode::notes::fetch(self),
            crate::tui::mode::ModeKind::Directories => return crate::tui::mode::directories::fetch(self),
            crate::tui::mode::ModeKind::Panes => return crate::tui::mode::panes::fetch(self),
            crate::tui::mode::ModeKind::Jira => return crate::tui::mode::jira::fetch(self),
            crate::tui::mode::ModeKind::Files => return crate::tui::mode::files::fetch(self),
            crate::tui::mode::ModeKind::Tags => return crate::tui::mode::tags::fetch(self),
            crate::tui::mode::ModeKind::Codegraph => return crate::tui::mode::codegraph::fetch(self),
            crate::tui::mode::ModeKind::Ag => return crate::tui::mode::ag::fetch(self),
            // Output, LLM, Question, History: all
            // fall through to the SQL `SELECT` below.
            _ => {}
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
        let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
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

                    ..Default::default()
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        // Record the cache key so the next `refresh()` can
        // short-circuit when the query hasn't changed.
        self.last_fetch_key = Some((
            self.query.clone(),
            self.mode,
            self.exit_filter,
            self.match_algorithm,
        ));
        Ok(rows)
    }

    // `fetch_stats` was extracted to
    // `crate::tui::stats::fetch` (the successor-frequency
    // SQL query for `Mode::Stats` — not a prefix mode but
    // a scope mode, so the per-prefix `ModeKind` dispatch
    // doesn't apply; the caller branches on `Mode::Stats`
    // before the per-mode match in `App::fetch`).


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
        if !self.query.is_empty() && !self.is_regex_query() && !self.is_fuzzy_query() {
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
                let case_sensitive = self.is_case_sensitive();
                for word in self.query.split_whitespace() {
                    let escaped = if case_sensitive {
                        // GLOB uses `*` and `?`
                        // as wildcards; escape
                        // them so the user's
                        // literal text is
                        // matched.
                        crate::util::escape_glob(word)
                    } else {
                        crate::util::escape_like(word)
                    };
                    if case_sensitive {
                        clause.push_str(" AND (h.command GLOB ? OR c.comment GLOB ?)");
                    } else {
                        clause.push_str(
                            " AND (h.command LIKE ? ESCAPE '\\' OR c.comment LIKE ? ESCAPE '\\')",
                        );
                    }
                    if case_sensitive {
                        params.push(Box::new(format!("*{}*", escaped)));
                        params.push(Box::new(format!("*{}*", escaped)));
                    } else {
                        params.push(Box::new(format!("%{}%", escaped)));
                        params.push(Box::new(format!("%{}%", escaped)));
                    }
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
                    let canonical = crate::util::canonicalize_directory(&pwd);
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
        self.directory_source = self.directory_source.next();
        self.refresh();
    }

    /// Toggle the `*`-mode panes filter.
    /// If `target` is already active,
    /// resets to `All` (toggle off).
    /// Otherwise sets the filter to
    /// `target`. After changing the
    /// filter, refreshes the list so
    /// the rows update immediately.
    ///
    /// No-op (with a status message)
    /// when not in panes (`*`) mode —
    /// the filter only applies to the
    /// panes view, so firing it
    /// elsewhere would surprise the
    /// user.
    fn toggle_panes_filter(&mut self, target: PanesFilter) {
        if !self.is_panes_query() {
            self.set_status_message(
                "PanES filter is only available in panes mode (type `*`)".to_string(),
            );
            return;
        }
        if self.panes_filter == target {
            // Same key pressed again — reset.
            self.panes_filter = PanesFilter::All;
            self.set_status_message("panes filter: all".to_string());
        } else {
            self.panes_filter = target;
            self.set_status_message(format!("panes filter: {}", target.label().to_lowercase(),));
        }
        // Reset the selection to the
        // first row so the cursor
        // doesn't land on a row
        // that's now filtered out.
        self.list_state.select(Some(0));
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
        let prefixes = [p.output, p.llm, p.question, p.notes, p.todo, p.directories];
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
        let mut s = String::with_capacity(body.len() + p.directories.len_utf8());
        s.push(p.directories);
        s.push_str(&body);
        self.query = s;
        self.recompile_regex();
        self.query_cursor = self.query.chars().count();
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
        // For the history list the data
        // is stored newest-first and
        // rendered bottom-to-top with
        // bottom-alignment. Pressing Up
        // (a positive delta) moves to a
        // higher index = older timestamp
        // = a row that renders ABOVE the
        // cursor — so up visually moves
        // up. For panes mode the list is
        // rendered top-to-bottom with
        // top-alignment (data index 0 =
        // top of the list) — the
        // history convention's "up =
        // index +1" makes the cursor go
        // DOWN visually. Flip the sign
        // for panes mode so the cursor's
        // up/down matches the displayed
        // order. PageUp / PageDown are
        // the same — they pass a larger
        // delta but the visual direction
        // contract is the same.
        let delta = if self.is_panes_query() { -delta } else { delta };
        let cur = self.list_state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, merged_len as isize - 1) as usize;
        self.list_state.select(Some(next));
        // The selected row changed; load its preview context on
        // demand for the active mode without re-fetching the
        // whole list.
        crate::tui::mode::tags::ensure_selected_context(self);
        crate::tui::mode::codegraph::ensure_selected_context(self);
        crate::tui::mode::notes::ensure_selected_context(self);
        crate::tui::mode::todo::ensure_selected_context(self);
        crate::tui::mode::files::ensure_selected_context(self);
        crate::tui::mode::panes::ensure_selected_context(self);
    }

    fn select_for_run(&mut self) {
        // Record the current query (with its leading prefix
        // char) into the active mode's history before
        // dispatching. Run is the natural "the user is done
        // with this query, remember it" moment: the in-memory
        // history is persisted to disk at TUI exit, so the
        // entry survives across sessions. Empty queries
        // are skipped inside `record_to_mode_history`.
        //
        // We extract the mode char + query up front so the
        // immutable borrow of `self` (needed to read
        // `self.query` and compute the mode) is released
        // before the `&mut self` call into
        // `record_to_mode_history`. (Same borrow-ordering
        // pattern as `open_codegraph_relations`: the
        // borrow checker disallows the chained immutable/
        // mutable borrow of the same `self`.)
        let (mode_char, query_snapshot) = (self.current_mode_char(), self.query.clone());
        self.record_to_mode_history(mode_char, &query_snapshot);
        self.select_for_run_dispatch()
    }

    /// Dispatcher: routes to the per-mode handler based
    /// on the current query prefix. Each handler is a
    /// separate method so the code is easier to navigate.
    fn select_for_run_dispatch(&mut self) {
        if self.is_llm_query() {
            self.run_llm_query();
            return;
        }
        if self.is_question_query() {
            self.run_question_query();
            return;
        }
        if self.is_todo_query() {
            self.select_for_run_impl();
            return;
        }
        if self.is_notes_query() {
            self.select_for_run_impl();
            return;
        }
        if self.is_files_query() {
            self.select_for_run_impl();
            return;
        }
        if self.is_tags_query() {
            self.select_for_run_impl();
            return;
        }
        if self.is_codegraph_query() {
            self.select_for_run_impl();
            return;
        }
        if self.is_directories_query() {
            self.select_for_run_impl();
            return;
        }
        if self.is_panes_query() {
            self.select_for_run_impl();
            return;
        }
        if self.is_jira_query() {
            self.select_for_run_impl();
            return;
        }
        if self.is_ag_query() {
            self.select_for_run_impl();
            return;
        }
        // Default: history mode.
        if let Some(row) = self.selected_row() {
            if row.mode == "llm" && !row.output.is_empty() {
                self.selection = Some(row.output.clone());
                self.pick_mode = Some(PickMode::Run);
            } else if row.mode == "question" && !row.output.is_empty() {
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
    fn process_llm_result(
        &mut self,
        request: LlmRequest,
        result: Result<String, crate::llm::LlmError>,
    ) {
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
        if let Some(request) = self.llm_request.take()
            && let Ok(result) = request.receiver.recv()
        {
            self.process_llm_result(request, result);
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
            LlmRequestType::Generate {
                description: description.to_string(),
            },
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
            crate::util::canonicalize_directory(&std::env::var("PWD").unwrap_or_default());
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
            // The idle timer fires in
            // lock-step with the
            // fast debounce. Both
            // are armed on every
            // keystroke; the run
            // loop tick fires
            // whichever is due
            // first (400ms for the
            // fast debounce, 3s
            // for the idle timer).
            // See [`JIRA_IDLE_TIMEOUT`]
            // for the safety-net
            // rationale.
            self.jira_idle_started = Some(std::time::Instant::now());
        } else {
            self.jira_debounce_started = None;
            self.jira_idle_started = None;
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
        let pattern =
            crate::files::FilesState::current_pattern(&self.query, self.query_prefixes.files);
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
    fn process_files_result(&mut self, request: crate::files::FilesRequest, rows: Vec<HistoryRow>) {
        self.files_state.in_flight = false;
        self.files_state.request = None;
        // Only accept if this result
        // matches the current pattern
        // (the user may have typed
        // more characters while the
        // walk was running).
        let current =
            crate::files::FilesState::current_pattern(&self.query, self.query_prefixes.files);
        if current == request.pattern {
            self.files_state.rows = rows;
            self.refresh();
        }
    }

    // ---- AG (`,`-prefix) content search ----

    /// Return cached ag search results.
    // `fetch_ag` was extracted to
    // `crate::tui::mode::ag::fetch` (a one-line
    // cached-rows clone; the interesting logic
    // is the background `ag` process that
    // `ag_touch` → `crate::ag::spawn_ag_search` →
    // `process_ag_result` manages).

    /// Arm the ag-mode debounce. Mirrors `files_touch`.
    fn ag_touch(&mut self) {
        if self.is_ag_query() {
            self.ag_state.debounce_started = Some(std::time::Instant::now());
            if let Some(request) = self.ag_state.request.take() {
                request.cancelled.store(true, Ordering::Relaxed);
            }
            self.ag_state.in_flight = false;
        } else {
            self.ag_state.debounce_started = None;
            self.ag_state.in_flight = false;
            self.ag_state.request = None;
            self.ag_state.last_pattern = None;
        }
    }

    /// Check whether the ag-mode debounce has elapsed
    /// and spawn a background search if so.
    fn ag_maybe_autocall(&mut self) {
        if !self.is_ag_query() {
            return;
        }
        if self.ag_state.in_flight {
            return;
        }
        let Some(started) = self.ag_state.debounce_started else {
            return;
        };
        if started.elapsed() < crate::ag::AG_DEBOUNCE {
            return;
        }
        let pattern = crate::ag::AgState::current_pattern(&self.query, self.query_prefixes.ag);
        if self.ag_state.has_results_for(&pattern) {
            return;
        }
        self.ag_state.last_pattern = Some(pattern.clone());
        self.spawn_ag_search(pattern);
    }

    /// Spawn a background thread that runs `ag` and
    /// parses the results.
    fn spawn_ag_search(&mut self, pattern: String) {
        let request = crate::ag::spawn_ag_search(pattern);
        self.ag_state.in_flight = true;
        self.ag_state.request = Some(request);
        self.set_status_message("Searching with ag…".to_string());
    }

    /// Process an ag-mode search result from the
    /// background thread.
    fn process_ag_result(&mut self, request: crate::ag::AgRequest, rows: Vec<HistoryRow>) {
        self.ag_state.in_flight = false;
        self.ag_state.request = None;
        let current = crate::ag::AgState::current_pattern(&self.query, self.query_prefixes.ag);
        if current == request.pattern {
            self.ag_state.rows = rows;
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
    ///
    /// Two timers can trigger a fire:
    /// 1. The 400ms fast debounce
    ///    ([`JIRA_DEBOUNCE`]) — handles the
    ///    fast-typo case (user
    ///    pauses briefly after
    ///    typing).
    /// 2. The 3-second idle
    ///    timer
    ///    ([`JIRA_IDLE_TIMEOUT`])
    ///    — safety-net trigger
    ///    that guarantees the
    ///    query fires within 3
    ///    seconds of the last
    ///    keystroke regardless
    ///    of whether the fast
    ///    debounce ever elapses
    ///    (e.g. the user keeps
    ///    typing slowly, or
    ///    the run loop is
    ///    temporarily blocked).
    /// The space key
    /// (`' '`) has its own
    /// explicit-fire path in
    /// `push_char` that fires
    /// immediately, before
    /// either timer.
    fn jira_maybe_autocall(&mut self) {
        if !self.is_jira_query() {
            return;
        }
        if self.jira_in_flight {
            return;
        }
        // The fast debounce and
        // the idle timer are
        // armed in lock-step by
        // `jira_touch`. Either
        // can fire the search
        // when its respective
        // window elapses.
        let debounce_elapsed = self
            .jira_debounce_started
            .map(|started| started.elapsed() >= JIRA_DEBOUNCE)
            .unwrap_or(false);
        let idle_elapsed = self
            .jira_idle_started
            .map(|started| started.elapsed() >= JIRA_IDLE_TIMEOUT)
            .unwrap_or(false);
        if !debounce_elapsed && !idle_elapsed {
            return;
        }
        // "Configured" means either real env config OR an
        // injected test client. If neither, surface a
        // one-shot status message and disarm.
        let configured =
            self.jira_client.is_some() || crate::jira::JiraConfig::from_env().is_some();
        if !configured {
            if self.jira_last_jql.is_some() || self.jira_rows.is_empty() {
                self.set_status_message(crate::jira::JiraError::NotConfigured.to_string());
            }
            self.jira_debounce_started = None;
            self.jira_idle_started = None;
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
            if self.jira_last_undefined_message.as_ref() != Some(&self.jira_undefined_fragments) {
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
                self.jira_last_undefined_message = Some(self.jira_undefined_fragments.clone());
            }
            self.jira_debounce_started = None;
            self.jira_idle_started = None;
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
            self.jira_last_jql = Some(jql.clone());
            // Clear both timers now that
            // the search is in flight.
            // `jira_maybe_autocall`
            // early-returns on
            // `jira_in_flight` so the
            // timers are not strictly
            // required to be `None`,
            // but clearing them keeps
            // the bookkeeping
            // consistent and avoids a
            // stale timer firing on
            // the next iteration if
            // the in-flight guard is
            // ever bypassed (e.g. by
            // a future fast-path).
            self.jira_debounce_started = None;
            self.jira_idle_started = None;
            self.process_jira_result(request, result);
            return;
        }
        let Some(config) = crate::jira::JiraConfig::from_env() else {
            self.set_status_message(crate::jira::JiraError::NotConfigured.to_string());
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
        // Same bookkeeping
        // clear as the test-client
        // path above.
        self.jira_debounce_started = None;
        self.jira_idle_started = None;
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
                        details.push(format!("**Due**: {}  **Assignee**: {}", due, assignee));
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
                        // Map the JIRA workflow status to an exit-code
                        // sentinel so the row's `✓`/`✗` marker reflects
                        // whether the issue is closed or still open.
                        // `Closed` and `To be Reviewed` are treated as
                        // "done" (exit_code = 0 → green ✓); every other
                        // status is "still open" (exit_code = 1 → red ✗).
                        // The comparison is case-insensitive so a JIRA
                        // instance that lowercases its workflow names
                        // (e.g. `closed`) still matches.
                        let status_lower = issue.status.to_ascii_lowercase();
                        let jira_exit_code =
                            if status_lower == "closed" || status_lower == "to be reviewed" {
                                0
                            } else {
                                1
                            };
                        crate::tui::state::HistoryRow {
                            id,
                            command: issue.key,
                            directory: String::new(),
                            session_id: String::new(),
                            exit_code: jira_exit_code,
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

                            ..Default::default()
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
    // `fetch_jira` was extracted to
    // `crate::tui::mode::jira::fetch` (a one-line
    // cached-rows clone; the live fetch happens in
    // the background via `jira_maybe_autocall` →
    // `crate::jira::spawn_jira_search` →
    // `process_jira_result`).

    /// Install a JIRA client for tests (a fake). When set,
    /// searches run synchronously on the calling thread via
    /// this client instead of spawning a background HTTP
    /// thread, so the search-render path is deterministic.
    #[cfg(test)]
    fn set_jira_client(&mut self, client: std::sync::Arc<dyn crate::jira::JiraClient>) {
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
            // doesn't accidentally end up as a prefix). Space is
            // excepted — the user expects to be able to add a space
            // and keep typing a multi-word refinement of the cached
            // query.
            if self.query_prefilled && !self.query_touched && c != ' ' {
                self.query.clear();
                // Reset the cursor to the (now-empty) end so
                // the new character lands at position 0.
                self.query_cursor = 0;
            }
            // Snapshot the query before the mutation. We use
            // it for two things: (a) commit the per-mode history
            // recall session — any keystroke that mutates the
            // query exits recall mode so the user's edits
            // become the live query; (b) record the OLD query
            // into the OLD mode's history if the leading
            // prefix char is about to change (rare, but
            // happens in LLM mode when the user inserts at
            // position 0, overwriting the leading `=`).
            let old_query = self.query.clone();
            self.history_exit_recall();
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
            // Leading-char change detection. In non-LLM
            // modes the cursor is always at the end, so
            // `push_char` never changes the leading char; the
            // check is a no-op there. In LLM mode the user
            // can move the cursor anywhere, so inserting at
            // position 0 replaces the leading `=` with a new
            // character — when that happens, record the OLD
            // LLM query into LLM mode's history.
            if query_mode_char(&old_query, &self.query_prefixes)
                != query_mode_char(&self.query, &self.query_prefixes)
            {
                self.on_query_mode_change(&old_query);
            }
            self.recompile_regex();
            // Re-arm the LLM auto-call debounce (or clear
            // the preview if we just left LLM mode by
            // backspacing the `=`). The user's last
            // edit time is the new debounce anchor.
            self.llm_touch();
            // Non-JIRA modes: fire the
            // search immediately on
            // the keystroke. The user
            // reported that JIRA
            // "sometimes isn't
            // executed"; the
            // corresponding complaint
            // for the in-process
            // search modes is "the
            // list lags my typing".
            // For LLM mode this
            // bypasses the 1s
            // debounce; for
            // synchronous modes it
            // calls `refresh()`
            // directly. (JIRA mode
            // bails inside
            // `trigger_text_change_search`
            // — the JIRA-specific
            // timers handle it.)
            self.trigger_text_change_search();
            // Space-trigger for the JIRA
            // search-as-you-type: when
            // the user types a space
            // inside the JIRA query
            // body, fire the search
            // immediately rather
            // than waiting for the
            // 400ms debounce or the
            // 3-second idle timer.
            // This matches IDE
            // autocomplete
            // conventions (a
            // space commits the
            // current token and
            // commits the query
            // to a search) and
            // gives the user a
            // "I'm done with this
            // word" signal.
            //
            // The implementation
            // temporarily forces
            // both timers to a
            // past value so the
            // dual-timer gate
            // (`debounce_elapsed ||
            // idle_elapsed`) is
            // satisfied. The
            // `llm_touch()` call
            // above re-armed the
            // timers to "now" (so
            // neither is elapsed);
            // we override that
            // here for the duration
            // of the space-trigger
            // call only. This keeps
            // the regular
            // debounce/idle-timer
            // accounting intact
            // (the next keystroke
            // re-arms both
            // timers); the space
            // trigger is a
            // one-shot
            // "fire now" override
            // that doesn't affect
            // the user's normal
            // typing cadence.
            if c == ' ' && self.is_jira_query() {
                let past = std::time::Instant::now()
                    - JIRA_IDLE_TIMEOUT
                    - JIRA_DEBOUNCE
                    - std::time::Duration::from_millis(50);
                self.jira_debounce_started = Some(past);
                self.jira_idle_started = Some(past);
                self.jira_maybe_autocall();
            }
        }
    }

    /// Move the cursor one
    /// character to the left
    /// inside the search query.
    /// The cursor is measured in
    /// UTF-8 characters (matching
    /// the rest of the query
    /// editing logic), so
    /// multi-byte characters are
    /// stepped over as single
    /// units. Saturates at
    /// position 0 so pressing
    /// Left at the very start of
    /// the query is a no-op. The
    /// query string is unchanged;
    /// only the cursor position
    /// moves. In comment-edit mode
    /// the cursor lives on the
    /// edit buffer, not on
    /// `self.query`, so the action
    /// is a no-op there (the
    /// comment editor handles its
    /// own Left/Right).
    fn move_query_cursor_left(&mut self) {
        if self.comment_edit.is_some() {
            return;
        }
        if self.query_cursor > 0 {
            self.query_cursor -= 1;
        }
    }

    /// Move the cursor one
    /// character to the right
    /// inside the search query.
    /// Saturates at the end of
    /// the query so pressing
    /// Right past the last
    /// character is a no-op.
    /// See `move_query_cursor_left`
    /// for the comment-edit branch
    /// rationale.
    fn move_query_cursor_right(&mut self) {
        if self.comment_edit.is_some() {
            return;
        }
        let len = self.query.chars().count();
        if self.query_cursor < len {
            self.query_cursor += 1;
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
                // Snapshot the query BEFORE the mutation so we
                // can (a) record it into its old mode's history
                // if the leading prefix char is about to change,
                // and (b) commit the per-mode history recall
                // session if the user is currently navigating
                // with C-p / C-n. Any keystroke that mutates
                // the query should commit the recall (the
                // user's edits become the "live" query).
                let old_query = self.query.clone();
                self.history_exit_recall();
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
                // If the leading char changed (e.g. the user
                // backspaced through their prefix `&` and is
                // now in plain mode, or backspaced the entire
                // query down to a different prefix char), record
                // the OLD query into the OLD mode's history
                // and reset the new mode's recall state. The
                // `on_query_mode_change` helper handles both
                // sides: it records `old_query` and clears the
                // recall state for the mode we're now in.
                if query_mode_char(&old_query, &self.query_prefixes)
                    != query_mode_char(&self.query, &self.query_prefixes)
                {
                    self.on_query_mode_change(&old_query);
                }
                self.recompile_regex();
                self.refresh();
                // Mirror of `push_char`: re-arm the LLM
                // debounce (or clear preview state if we
                // just backspaced out of LLM mode).
                self.llm_touch();
                // Fire the per-mode search
                // immediately on the
                // deletion. Same
                // rationale as
                // `push_char`:
                // non-JIRA modes
                // should reflect
                // the user's edit
                // on the same
                // frame.
                self.trigger_text_change_search();
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
    /// Tab-completion of a JQL field name at the
    /// current cursor position. The user's example:
    /// typing `lab<TAB>` inside the JIRA query
    /// expands to `labels=` (cursor right after the
    /// `=`). Behaviour:
    ///
    /// 1. Find the field-name token immediately
    ///    before the cursor — the run of
    ///    word-characters (`[A-Za-z0-9_]`) that
    ///    starts at the previous whitespace
    ///    boundary and ends at the cursor.
    /// 2. Call `jira::jira_field_complete_with_value`
    ///    on the prefix.
    /// 3. If the prefix has no matches, do
    ///    nothing (and surface a soft status
    ///    message so the user knows Tab did
    ///    not silently fail).
    /// 4. If the prefix is a complete field name
    ///    (e.g. the user typed `labels<TAB>` and
    ///    the field is already fully typed), append
    ///    a `=` and move the cursor past it. This
    ///    means a second Tab always advances the
    ///    user from "I typed the field" to "I'm
    ///    ready to type the value".
    /// 5. If the prefix is the start of multiple
    ///    fields (e.g. `lab` matches both `label`
    ///    and `labels`), the prefix is extended
    ///    to the LONGEST COMMON PREFIX and the
    ///    cursor lands after it. The user keeps
    ///    typing to disambiguate.
    /// 6. If the prefix matches exactly one
    ///    field, the token is replaced with
    ///    the full field name + `=` and the
    ///    cursor lands right after the `=`.
    ///
    /// The function also handles the edge cases:
    /// - Cursor at the start of the query
    ///   (no prefix): no-op.
    /// - Cursor mid-field (e.g. `lab|els` with
    ///   the cursor between `b` and `e`): the
    ///   prefix is everything from the
    ///   previous whitespace to the cursor
    ///   (`lab`), and the function replaces
    ///   just that prefix. The `els` after the
    ///   cursor is preserved.
    /// - The cursor is in a value position
    ///   (e.g. after `=`, e.g.
    ///   `labels=|foo`): the prefix-to-complete
    ///   is the empty string immediately after
    ///   `=`, which is a no-match case, so the
    ///   function does nothing. (The completion
    ///   is intentionally not bidirectional:
    ///   the field name lives to the LEFT of
    ///   the `=`; the value lives to the RIGHT
    ///   and is left alone. If the user wants
    ///   to edit a value, they can backspace
    ///   and re-type, which is the expected
    ///   readline convention.)
    fn jira_field_complete_at_cursor(&mut self) {
        // The JIRA prefix is `-` by
        // default (a single `char`).
        // The completion operates
        // on the body (the text
        // after the prefix), not
        // on the prefix itself.
        // We also re-check
        // `is_jira_query` here so
        // the function is safe to
        // call from tests and from
        // any future caller — the
        // dispatch site already
        // checks, but defence in
        // depth.
        if !self.is_jira_query() {
            return;
        }
        // `QueryPrefixes::jira` is a
        // single `char`, so the
        // prefix length is always
        // 1.
        let prefix_len: usize = 1;
        // `query_cursor` is in
        // characters and points to
        // the position where the
        // next character would be
        // inserted (i.e. one past
        // the last character if the
        // cursor is at the end).
        if self.query_cursor < prefix_len {
            // Cursor is on or before
            // the JIRA prefix itself
            // (e.g. the user is in
            // the middle of the `-`
            // prefix). There's no
            // field name to
            // complete.
            return;
        }
        // Walk left from the cursor
        // (in character indices),
        // stopping at the first
        // character that is NOT a
        // JQL field-name character
        // (alphanumeric or
        // underscore). The
        // completion target is
        // everything in
        // `self.query[prefix_len..cursor]`
        // back to the start of the
        // current word.
        let mut start_char = self.query_cursor;
        while start_char > prefix_len {
            let prev = start_char - 1;
            let ch = self.query[char_to_byte_index(&self.query, prev)
                ..char_to_byte_index(&self.query, start_char)]
                .chars()
                .next()
                .expect("non-empty slice between char indices");
            if ch == '_' || ch.is_ascii_alphanumeric() {
                start_char = prev;
            } else {
                break;
            }
        }
        let start_byte = char_to_byte_index(&self.query, start_char);
        let cursor_byte = char_to_byte_index(&self.query, self.query_cursor);
        let prefix_str = &self.query[start_byte..cursor_byte];
        if prefix_str.is_empty() {
            // No field-name characters
            // before the cursor —
            // the user pressed Tab
            // right after whitespace
            // or at the start of a
            // value. Surface a
            // status message and
            // bail.
            self.set_status_message("jira-field-complete: no field name to expand".to_string());
            return;
        }
        // Detect `@` alias / fragment
        // completion. If the
        // character immediately
        // before the alphanumeric
        // word is `@`, the user is
        // typing an alias like
        // `@me` or `@today`. We
        // include the `@` in the
        // replacement range and
        // route to the alias
        // completion path (built-in
        // aliases + user-defined
        // fragments from
        // `jira.search.<name>=...`
        // config entries).
        let is_alias = start_char > prefix_len
            && self.query.as_bytes()[char_to_byte_index(&self.query, start_char - 1)] == b'@';
        // For alias completions, the `@`
        // is part of the replacement range.
        let (replace_start_byte, replace_start_char) = if is_alias {
            let at_char = start_char - 1;
            let at_byte = char_to_byte_index(&self.query, at_char);
            (at_byte, at_char)
        } else {
            (start_byte, start_char)
        };
        let (completion, _kind) = if is_alias {
            let alias_prefix = prefix_str; // alphanumeric part after @
            // Check for multiple
            // matches first. If the
            // alias completion is
            // ambiguous, open the
            // completion menu so
            // the user can pick
            // from the candidates
            // rather than just
            // extending to the
            // LCP.
            let matches = crate::jira::jira_alias_matches(alias_prefix, &self.jira_fragments);
            if matches.len() >= 2 {
                // Open the
                // completion
                // menu. The
                // user picks
                // a candidate
                // and the
                // menu
                // applies it
                // (prepending
                // `@` and
                // adding a
                // trailing
                // space).
                self.open_completion_menu(
                    matches,
                    replace_start_byte,
                    cursor_byte,
                    replace_start_char,
                    CompletionKind::JiraAlias,
                );
                return;
            }
            let result = match crate::jira::jira_alias_complete_with_space(
                alias_prefix,
                &self.jira_fragments,
            ) {
                Some(c) => c,
                None => {
                    self.set_status_message(format!(
                        "jira-alias-complete: no alias starts with `{}`",
                        alias_prefix
                    ));
                    return;
                }
            };
            // Include the `@` in the
            // replacement: the result
            // is just the alias name
            // (e.g. `"me "`), so we
            // prepend `@`.
            let mut full = String::from("@");
            full.push_str(&result);
            (full, "alias")
        } else {
            // Check for multiple
            // matches first. If the
            // field completion is
            // ambiguous, open the
            // completion menu so
            // the user can pick
            // from the candidates
            // rather than just
            // extending to the
            // LCP.
            let matches = crate::jira::jira_field_matches(prefix_str);
            if matches.len() >= 2 {
                // Open the
                // completion
                // menu. The
                // user picks
                // a candidate
                // and the
                // menu
                // applies it
                // with a
                // trailing
                // `=`.
                self.open_completion_menu(
                    matches,
                    replace_start_byte,
                    cursor_byte,
                    replace_start_char,
                    CompletionKind::JiraField,
                );
                return;
            }
            let result = match crate::jira::jira_field_complete_with_value(prefix_str) {
                Some(c) => c,
                None => {
                    // No field starts with
                    // the prefix. Don't
                    // silently destroy
                    // text; surface a
                    // status message.
                    self.set_status_message(format!(
                        "jira-field-complete: no JIRA field starts with `{}`",
                        prefix_str
                    ));
                    return;
                }
            };
            (result, "field")
        };
        // Replace the prefix with the
        // completion string and move
        // the cursor to the end.
        self.query
            .replace_range(replace_start_byte..cursor_byte, &completion);
        let completion_chars = completion.chars().count();
        self.query_cursor = replace_start_char + completion_chars;
        // Re-arm the debounce/idle
        // timers so the JIRA
        // search fires on the
        // expanded query.
        // (Same effect as a
        // normal keystroke.)
        self.llm_touch();
        // Recompile the regex (if
        // any) for the new
        // query body.
        self.recompile_regex();
        // Refresh the row set so
        // the search-as-you-type
        // shows the new result
        // count immediately
        // (without waiting for
        // the debounce).
        self.refresh();
        // If the expansion has a
        // trailing `=`, the user
        // is now ready to type
        // the value. A status
        // message like
        // "expanded labels=" is
        // a useful confirmation
        // (and is the same
        // verbosity as the rest
        // of the TUI's status
        // messages). If the
        // expansion is the
        // longest-common-prefix
        // (no trailing `=`), we
        // don't surface a
        // status message
        // because it would
        // flash too often
        // during disambiguation
        // and the user can see
        // the change in the
        // query line anyway.
        if completion.ends_with('=') {
            self.set_status_message(format!("expanded {}", completion));
        }
    }

    /// Tab-completion for notes (`@`) and todos (`!`)
    /// modes. The completion targets are tags (the
    /// `#TAG` token) and wiki-link targets (the
    /// `@LINK` token). Both completion lists come
    /// from the `note_search` database via
    /// `note_search::commands::metadata::get_unique_values`.
    ///
    /// Behaviour:
    /// - `#feat<TAB>` → `#feature ` (unique tag
    ///   match, trailing space)
    /// - `#f<TAB>` → `#feature` (LCP when multiple
    ///   tags share the prefix)
    /// - `@Neo<TAB>` → `@NeovimNote ` (unique link
    ///   match)
    /// - `@xyz<TAB>` → no-op + status message (no
    ///   match)
    /// - Outside notes/todos mode, or when the word
    ///   before the cursor doesn't start with `#`
    ///   or `@`, the function is a no-op so the
    ///   `Tab` key doesn't interfere with any other
    ///   mode.
    fn notes_tab_complete_at_cursor(&mut self) {
        // Only active in notes (`@`) or
        // todos (`!`) mode. The notes
        // and todos prefixes are both
        // single chars (the `query_prefixes.notes`
        // and `query_prefixes.todo` fields).
        let prefix_len: usize = 1;
        if self.query_cursor < prefix_len {
            return;
        }
        // Check the query's leading char.
        // The first char after the
        // prefix must be `#` (tag) or
        // `@` (link) for the completion
        // to be meaningful; otherwise
        // the user is just typing plain
        // text and Tab should not
        // interfere.
        let first = self
            .query
            .chars()
            .next()
            .expect("query_cursor >= 1 implies non-empty");
        if first != self.query_prefixes.notes && first != self.query_prefixes.todo {
            return;
        }
        // Walk left from the cursor
        // (character indices), stopping
        // at the first character that
        // is NOT alphanumeric or
        // underscore. Same walk as
        // JIRA field completion, but
        // applied to the notes/todos
        // body (after the single-char
        // prefix).
        let mut start_char = self.query_cursor;
        while start_char > prefix_len {
            let prev = start_char - 1;
            let ch = self.query[char_to_byte_index(&self.query, prev)
                ..char_to_byte_index(&self.query, start_char)]
                .chars()
                .next()
                .expect("non-empty slice between char indices");
            if ch == '_' || ch.is_ascii_alphanumeric() {
                start_char = prev;
            } else {
                break;
            }
        }
        // After the walk, `start_char`
        // points to the first
        // alphanumeric character of
        // the word. If the character
        // immediately before is `#`
        // (tag prefix) or `@` (link
        // prefix), include it in the
        // word by decrementing
        // `start_char`. This handles
        // `#feat` and `@Neo` where the
        // walk would otherwise stop
        // before the prefix char.
        if start_char > prefix_len {
            let prev_char_byte = char_to_byte_index(&self.query, start_char - 1);
            let curr_char_byte = char_to_byte_index(&self.query, start_char);
            let prev_ch = self.query[prev_char_byte..curr_char_byte]
                .chars()
                .next()
                .expect("non-empty slice between char indices");
            if prev_ch == '#' || prev_ch == '@' {
                start_char -= 1;
            }
        }
        let start_byte = char_to_byte_index(&self.query, start_char);
        let cursor_byte = char_to_byte_index(&self.query, self.query_cursor);
        let word = &self.query[start_byte..cursor_byte];
        if word.is_empty() {
            self.set_status_message(
                "notes-tab-complete: no tag or link name to expand".to_string(),
            );
            return;
        }
        // Determine whether the word
        // is a tag (starts with `#`)
        // or a link (starts with `@`).
        // The prefix char of the word
        // is the char at
        // `start_char` in the query
        // (or one position back if the
        // word is just `#` or `@`
        // with no alphanumeric
        // characters).
        let first_char_of_word = self.query[char_to_byte_index(&self.query, start_char)
            ..char_to_byte_index(&self.query, start_char + 1)]
            .chars()
            .next()
            .expect("start_char < cursor_byte");
        let Some(db_path) = self.notes_database.clone() else {
            // No notes database
            // configured; the
            // completion is a
            // no-op. The user
            // needs `notes.database`
            // in their config
            // for tag/link
            // completion to work.
            self.set_status_message(
                "notes-tab-complete: notes.database is not configured".to_string(),
            );
            return;
        };
        let (name_prefix, completion, kind) = match first_char_of_word {
            '#' => {
                // Tag completion: strip
                // the leading `#` and
                // query the DB for tags
                // starting with the
                // remainder. The
                // completion result from
                // `notes_tag_complete` is
                // the bare tag name
                // (e.g. `"feature "`); we
                // prepend `#` so the
                // replacement includes
                // the tag prefix.
                let name = &word[1..];
                // Check for multiple
                // matches first. If
                // the tag completion
                // is ambiguous, open
                // the completion menu
                // so the user can
                // pick from the
                // candidates rather
                // than just extending
                // to the LCP.
                let matches = crate::jira::notes_tag_matches(&db_path, name);
                if matches.len() >= 2 {
                    // Open the
                    // completion
                    // menu. The
                    // user
                    // picks a
                    // candidate
                    // and the
                    // menu
                    // applies
                    // it with
                    // a
                    // leading
                    // `#` and
                    // a
                    // trailing
                    // space.
                    self.open_completion_menu(
                        matches,
                        start_byte,
                        cursor_byte,
                        start_char,
                        CompletionKind::NotesTag,
                    );
                    return;
                }
                let result = crate::jira::notes_tag_complete(&db_path, name);
                let result = match result {
                    Some(r) => r,
                    None => {
                        self.set_status_message(format!(
                            "notes-tab-complete: no tag starts with `#{}`",
                            name
                        ));
                        return;
                    }
                };
                // Prepend `#` to the
                // completion so the
                // replacement includes
                // the tag prefix.
                let mut with_hash = String::from("#");
                with_hash.push_str(&result);
                (name.to_string(), with_hash, "tag")
            }
            '@' => {
                // Link completion: strip
                // the leading `@` and
                // query the DB for links
                // starting with the
                // remainder. The
                // completion result from
                // `notes_link_complete`
                // is the full `[[...]]`
                // expansion (e.g.
                // `[[NeovimNote]] ` or
                // `[["my note"]] ` for
                // links with spaces),
                // including the brackets
                // and a trailing space.
                // We use the result as-is
                // since the user typed
                // `@` to trigger the
                // completion but the
                // expansion uses the
                // `[[...]]` syntax (which
                // supports link names
                // with spaces, unlike
                // the `@` syntax in
                // `note_search`).
                let name = &word[1..];
                // Check for multiple
                // matches first. If
                // the link completion
                // is ambiguous, open
                // the completion menu
                // so the user can
                // pick from the
                // candidates rather
                // than just extending
                // to the LCP.
                let matches = crate::jira::notes_link_matches(&db_path, name);
                if matches.len() >= 2 {
                    // Open the
                    // completion
                    // menu. The
                    // user
                    // picks a
                    // candidate
                    // and the
                    // menu
                    // applies
                    // it
                    // wrapped
                    // in
                    // `[[...]]`
                    // with a
                    // trailing
                    // space.
                    self.open_completion_menu(
                        matches,
                        start_byte,
                        cursor_byte,
                        start_char,
                        CompletionKind::NotesLink,
                    );
                    return;
                }
                let result = crate::jira::notes_link_complete(&db_path, name);
                let result = match result {
                    Some(r) => r,
                    None => {
                        self.set_status_message(format!(
                            "notes-tab-complete: no link starts with `@{}`",
                            name
                        ));
                        return;
                    }
                };
                (name.to_string(), result, "link")
            }
            _ => {
                // Plain text: no
                // completion. The user
                // is just typing a
                // word; Tab should
                // not interfere.
                return;
            }
        };
        let _ = name_prefix; // silence unused warning; kept for future diagnostics
        // The completion string already
        // includes the leading `#` or
        // `@` and the trailing space
        // (when unique). Replace the
        // word at `start_byte` with
        // the completion.
        self.query
            .replace_range(start_byte..cursor_byte, &completion);
        let completion_chars = completion.chars().count();
        self.query_cursor = start_char + completion_chars;
        // Re-arm the debounce/idle
        // timers and refresh so the
        // new query fires its search
        // immediately. This mirrors
        // the JIRA path.
        self.llm_touch();
        self.recompile_regex();
        self.refresh();
        // Surface a status message
        // for the unique-match case
        // (the user can see the
        // expanded query in the
        // input line, but a
        // confirmation is useful
        // for the tag/link case
        // where the result has
        // many characters). We
        // don't surface a message
        // for the LCP case (no
        // trailing space) because
        // that fires every time
        // the user presses Tab to
        // disambiguate, and the
        // flash would be
        // distracting.
        if completion.ends_with(' ') {
            self.set_status_message(format!("expanded {} `{}`", kind, completion.trim_end()));
        }
    }

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
        // Snapshot the query before the mutation so
        // we can record the OLD query into the OLD
        // mode's history if the leading prefix char
        // is about to change, and commit any
        // in-progress per-mode history recall
        // session. Same pattern as `backspace`.
        let old_query = self.query.clone();
        self.history_exit_recall();
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
        // Leading-char change detection (same as
        // `backspace`): if the deletion crossed the
        // mode boundary (e.g. C-w deleted the prefix
        // char), record the OLD query into the OLD
        // mode's history.
        if query_mode_char(&old_query, &self.query_prefixes)
            != query_mode_char(&self.query, &self.query_prefixes)
        {
            self.on_query_mode_change(&old_query);
        }
        self.recompile_regex();
        self.refresh();
        self.llm_touch();
        // Fire the per-mode search
        // immediately on the
        // deletion. Same
        // rationale as
        // `push_char` /
        // `backspace`:
        // non-JIRA modes
        // should reflect
        // the user's edit
        // on the same
        // frame.
        self.trigger_text_change_search();
    }

    fn clear_query(&mut self) {
        if let Some(ref mut buf) = self.comment_edit {
            buf.clear();
        } else {
            // Clear-input is a user edit. Exit any
            // in-progress per-mode history recall (the
            // user's typing intent is "blank slate", not
            // "edit the recalled entry"), but do NOT
            // record the cleared query to history —
            // empty queries are skipped by
            // `record_to_mode_history` anyway, and
            // recording just before clearing would
            // capture the about-to-be-discarded text.
            self.history_exit_recall();
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
            // Fire the per-mode search
            // (no-op for the
            // empty query: the
            // empty check at the
            // top of
            // `trigger_text_change_search`
            // bails before
            // reaching
            // `refresh()` /
            // `llm_maybe_autocall`).
            // Calling it here is
            // cheap and keeps
            // the call sites
            // uniform — every
            // text-mutating
            // path is wired.
            self.trigger_text_change_search();
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
        let selection = self
            .selected_row()
            .map(|r| (r.command.clone(), r.mode.clone(), r.comment.clone()));
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
            let body = self.comment_edit.clone().unwrap_or_default();
            // An empty body is a
            // user error: don't
            // POST an empty
            // comment. Surface a
            // status message and
            // keep the buffer open
            // so the user can
            // type something.
            if body.trim().is_empty() {
                self.set_status_message("JIRA add-comment: body is empty".to_string());
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
                self.process_jira_add_comment_result(request, key, result);
                return Ok(());
            }
            // Production path:
            // spawn a background
            // thread.
            let Some(config) = crate::jira::JiraConfig::from_env() else {
                self.set_status_message(crate::jira::JiraError::NotConfigured.to_string());
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
                let client = crate::jira::RestJiraClient::new(config);
                let result = client.add_comment(&key_for_thread, &body_for_thread);
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
            self.set_status_message(format!("JIRA add-comment to {} cancelled", key));
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
                self.set_status_message(format!("Comment posted to {}", key));
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
                self.set_status_message(format!("JIRA add-comment to {} failed: {}", key, e));
            }
        }
    }

    fn show_output_view(&mut self) {
        // For all lazy-context modes, the selected row's
        // output is populated lazily. Make sure it's loaded
        // before opening the overlay.
        crate::tui::mode::tags::ensure_selected_context(self);
        crate::tui::mode::codegraph::ensure_selected_context(self);
        crate::tui::mode::notes::ensure_selected_context(self);
        crate::tui::mode::todo::ensure_selected_context(self);
        crate::tui::mode::files::ensure_selected_context(self);
        crate::tui::mode::panes::ensure_selected_context(self);
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
        let selection = self
            .selected_row()
            .map(|r| {
                // For `preview_only` modes (pane / workspace /
                // session), the row's `output` is metadata
                // (the pane's `tab_id`, an empty string for
                // headers, etc.), not actual content. Use the
                // `preview` field instead so the overlay
                // shows the real pane content rather than the
                // tab_id. For every other mode the historical
                // `output` is the right field (it carries
                // either the captured stdout of a history
                // command, the source-context + callers /
                // callees overlay for symbols, or the JIRA
                // description body).
                let is_preview_only = matches!(
                    r.mode.as_str(),
                    "pane" | "workspace" | "session"
                );
                let text = if is_preview_only && !r.preview.is_empty() {
                    r.preview.clone()
                } else {
                    r.output.clone()
                };
                (r.command.clone(), r.mode.clone(), text)
            });
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
            self.set_status_message(crate::jira::JiraError::NotConfigured.to_string());
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
                self.output_view = Some(OutputView { text, scroll: 0 });
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
            self.set_status_message("Describe: no row selected".to_string());
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
        if row.mode == "directory" && row.comment.is_empty() {
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
            self.set_status_message(crate::llm::LlmError::NotConfigured.to_string());
            return;
        }

        let prompt = crate::llm::build_describe_prompt(&command);
        self.spawn_llm_request(LlmRequestType::Describe { command }, prompt);
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
            self.set_status_message("Correct: no row selected".to_string());
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
        if row.mode == "directory" && row.comment.is_empty() {
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
            self.set_status_message(crate::llm::LlmError::NotConfigured.to_string());
            return;
        }

        let prompt = crate::llm::build_correct_prompt(&original_command);
        self.spawn_llm_request(LlmRequestType::Correct { original_command }, prompt);
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
            crate::util::canonicalize_directory(&std::env::var("PWD").unwrap_or_default());
        let session_id = std::env::var("SMART_HISTORY_SESSION").unwrap_or_default();
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
            self.set_status_message(format!("Correct: history insert failed: {}", e));
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
            self.set_status_message(
                "Question: provide a question after the question prefix".to_string(),
            );
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
            LlmRequestType::Question {
                question: question_owned,
            },
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
            crate::util::canonicalize_directory(&std::env::var("PWD").unwrap_or_default());
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

    /// Download the currently-selected JIRA issue as a
    /// local markdown note via
    /// `note_search jira-issue <KEY>`.
    ///
    /// The action is intended to be
    /// invoked only from the JIRA
    /// search mode (`-...`) where the
    /// selected row's `command` field
    /// carries the issue key (e.g.
    /// `PROJ-42`). The dispatcher
    /// already gates on
    /// `is_jira_query`; the helper
    /// itself also re-checks the mode
    /// so a stray test or future caller
    /// can't stage the wrong command
    /// from outside JIRA mode.
    ///
    /// The staged command is the bare
    /// `note_search jira-issue <KEY>`
    /// shell line — no path, no flags.
    /// `note_search` writes the
    /// markdown into the `notes.dir`
    /// configured in the same config
    /// file, picking its own filename
    /// from the issue summary. The
    /// TUI exits so the parent shell
    /// runs the command, which in
    /// turn shells out to the
    /// `note_search` binary on `PATH`.
    ///
    /// On any no-op (no row selected,
    /// empty key, not in JIRA mode)
    /// a status message is surfaced
    /// and `selection` stays `None`,
    /// so the TUI remains open and
    /// the user can react to the
    /// feedback.
    fn download_jira_issue(&mut self) {
        // Re-gate here so a stray
        // caller can't stage the
        // wrong command from outside
        // JIRA mode. The dispatcher
        // already gates on this, but
        // the helper defends against
        // future refactors that might
        // call it from a different
        // code path (e.g. a future
        // command-palette entry that
        // calls into the helper
        // directly).
        if !self.is_jira_query() {
            self.set_status_message(
                "Download-JIRA-issue is only available in JIRA search (type `-`)".to_string(),
            );
            return;
        }
        let Some(row) = self.selected_row().cloned() else {
            self.set_status_message("No JIRA issue selected".to_string());
            return;
        };
        // The issue key lives in the
        // row's `command` field (the
        // JIRA search stores it
        // there so the column is
        // visible in the row's
        // primary slot). Empty
        // command is a no-op — a
        // freshly-inserted row that
        // hasn't been populated yet
        // could in theory have one,
        // and staging `note_search
        // jira-issue ""` would be a
        // confusing surprise.
        let key = row.command.trim().to_string();
        if key.is_empty() {
            self.set_status_message("Selected JIRA row has no issue key".to_string());
            return;
        }
        // The bare shell line.
        // `shell_quote` keeps the
        // staged command safe even if
        // the key happens to contain
        // shell metacharacters (it
        // shouldn't — JIRA keys are
        // `^[A-Z]+-\d+$` — but
        // defence in depth costs
        // nothing here).
        let staged = format!("note_search jira-issue {}", crate::util::shell_quote(&key));
        self.selection = Some(staged);
        self.pick_mode = Some(PickMode::Run);
    }

    /// Open the selected JIRA issue's browse URL in the system
    /// browser **in the background** — the same action as pressing
    /// `Enter` on the row (`select_for_run_impl`), but spawned as a
    /// detached child process so the TUI stays open. Used by the
    /// [`Action::SmartOpen`] dive key in `-` (JIRA) mode.
    ///
    /// The opener is `open` on macOS and `xdg-open` on other
    /// Unixes (matching `select_for_run_impl`). The process is
    /// spawned on a short-lived thread that calls `.status()`
    /// (blocking wait + reap) so the child doesn't become a
    /// zombie; the TUI never blocks on the browser-launch call.
    fn open_jira_in_background(&mut self) {
        // Same gates as `select_for_run_impl`'s JIRA branch.
        if !self.is_jira_query() {
            self.set_status_message(
                "JIRA open is only available in JIRA search (type `-`)".to_string(),
            );
            return;
        }
        let key: String = match self.selected_row() {
            Some(r) => r.command.clone(),
            None => {
                self.set_status_message("No JIRA issue selected".to_string());
                return;
            }
        };
        let key = key.trim().to_string();
        if key.is_empty() {
            self.set_status_message("Selected JIRA row has no issue key".to_string());
            return;
        }
        let Some(cfg) = crate::jira::JiraConfig::from_env() else {
            self.set_status_message(crate::jira::JiraError::NotConfigured.to_string());
            return;
        };
        let url = cfg.browse_url(&key);
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        }
        .to_string();
        // Spawn a thread that runs the opener and reaps the child.
        // The TUI thread never blocks on the browser launch.
        std::thread::spawn(move || {
            let _ = std::process::Command::new(&opener).arg(&url).status();
        });
        self.set_status_message(format!("Opened {} in browser", key));
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
                self.set_status_message("Selected row is not a todo entry".to_string());
                return;
            }
        };
        if row.comment.is_empty() {
            self.set_status_message("Selected todo has no source filename".to_string());
            return;
        }
        let Some(ref notes_dir) = self.notes_dir else {
            self.set_status_message("Cannot mark done: notes.dir is not configured".to_string());
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
                self.set_status_message(format!("Cannot read {}: {}", row.comment, e));
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
            self.set_status_message(format!("Cannot write {}: {}", row.comment, e));
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
        if let (Some(dir), Some(db)) = (notes_dir_for_db.as_ref(), notes_db_for_db.as_ref()) {
            use rusqlite::Connection;
            match Connection::open(db) {
                Ok(conn) => {
                    if let Err(e) = note_search::update_files_in_db(&[filename_for_db], dir, &conn)
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
        self.set_status_message(format!("Marked done: {}:{}", row.comment, line_number));
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

    /// File-type-aware file open for the `~` (files)
    /// mode's `SmartOpen` dispatch. Returns `Some((staged
    /// command, exit))` if the selected file's
    /// extension (lowercase, no leading `.`) maps to a
    /// configured `smart-open.<ext>` entry (or the
    /// `smart-open.default` fallback). Returns
    /// `None` if no row is selected, the row isn't a
    /// file (it's a directory or some other non-file
    /// mode), the file has no extension, or no mapping
    /// is configured for any extension — in which
    /// case the dispatch falls back to the default
    /// `Run` action (open in `$EDITOR`).
    ///
    /// The command is the user-configured shell
    /// command with the file's absolute path
    /// appended (POSIX single-quote escaped) so
    /// paths with spaces / shell metacharacters
    /// can't break the staged command. The
    /// `exit = true` signals the dispatch site to
    /// terminate the TUI (the parent shell then
    /// runs the staged command).
    fn smart_open_for_file(&mut self) -> Option<(String, bool)> {
        // Re-gate on `~` mode so a
        // stray caller can't stage
        // a `bat README.md` (etc.)
        // from a different mode.
        if !self.is_files_query() {
            return None;
        }
        let Some(row) = self.selected_row() else {
            // No row selected —
            // surface a soft
            // diagnostic rather than
            // staging a wrong
            // command. The dispatch
            // site falls through to
            // `select_for_run` if
            // we return `None`, but
            // `select_for_run` is
            // also a no-op on an
            // empty list (it
            // surfaces "no row
            // selected" itself). To
            // avoid two status
            // messages, we just
            // fall through.
            return None;
        };
        // Only files trigger the
        // extension lookup —
        // directories fall through
        // to the default
        // (cd / workspace
        // create).
        if row.mode != "file" {
            return None;
        }
        // Extract the extension.
        // `Path::extension()` returns
        // the part after the last
        // `.` of the file name, or
        // `None` for dotfiles
        // (which the files-mode
        // walk skips by default
        // anyway) and files with
        // no extension.
        let ext = std::path::Path::new(&row.directory)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        // Look up the command:
        // 1. exact extension
        //    match (case-
        //    insensitive) if
        //    the file has one,
        // 2. `default` fallback
        //    otherwise.
        //
        // A file without an extension
        // (e.g. a `Makefile`,
        // `LICENSE`) is matched
        // against `default` only,
        // not the empty-string
        // mapping. The user who
        // wants "all extensionless
        // files use `bat`" writes
        // `smart-open.default=bat`
        // — they don't need to
        // add a separate empty-key
        // mapping.
        let cmd = ext
            .as_ref()
            .and_then(|e| self.smart_open_file_commands.get(e))
            .or_else(|| self.smart_open_file_commands.get("default"))
            .cloned();
        let Some(cmd) = cmd else {
            // No mapping for this
            // extension and no
            // `default` fallback —
            // fall through to
            // `Run` (open in
            // editor).
            return None;
        };
        // Stage the command with
        // the file's absolute path
        // appended. The command
        // is taken verbatim (the
        // user owns the formatting
        // — `bat --style=plain`
        // works as expected) and
        // the path is POSIX-
        // single-quote escaped so
        // paths with spaces,
        // quotes, or backslashes
        // can't break the staged
        // command.
        let quoted = crate::util::shell_quote(&row.directory);
        let staged = format!("{} {}", cmd.trim(), quoted);
        // Set pick_mode so the run-
        // loop treats this as a
        // normal `Run`-equivalent
        // selection (the parent's
        // exit code maps through).
        self.selection = Some(staged.clone());
        self.pick_mode = Some(PickMode::Run);
        Some((staged, true))
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

    fn is_prefix_picker_open(&self) -> bool {
        self.prefix_picker.is_some()
    }

    fn open_prefix_picker(&mut self) {
        let first = self.query.chars().next();
        let current = first.and_then(|c| {
            let p = &self.query_prefixes;
            let known = [
                p.output,
                p.llm,
                p.question,
                p.notes,
                p.todo,
                p.directories,
                p.panes,
                p.jira,
                p.files,
                p.tags,
                p.ag,
            ];
            known.contains(&c).then_some(c)
        });
        self.prefix_picker = Some(PrefixPicker::new(&self.query_prefixes, current));
    }

    fn close_prefix_picker(&mut self) {
        self.prefix_picker = None;
    }

    /// Whether the CodeGraph relations picker overlay is currently open.
    fn is_codegraph_relations_picker_open(&self) -> bool {
        self.codegraph_relations_picker.is_some()
    }

    /// Open the CodeGraph callers/callees picker for the currently
    /// selected `&` / `$` (codegraph-backed) row. The picker lists
    /// the symbol's callers (who calls it) followed by its callees
    /// (what it calls) as one navigable list with section headers;
    /// Enter on a relation opens its source file in `$EDITOR` at
    /// `start_line` (mirroring the main list's selection), Esc
    /// closes the overlay.
    ///
    /// Only rows carrying a `codegraph_node_id` can open the
    /// picker — i.e. `&`-mode rows and `$`-mode rows produced by
    /// the CodeGraph fallback when no `TAGS` file exists. A
    /// regular tags row (from a real `tags` file) or any non-
    /// tags/codegraph row surfaces a status message instead of
    /// opening the picker, so the key is a clean no-op (rather
    /// than a confusing empty overlay) outside the supported modes.
    fn open_codegraph_relations(&mut self) {
        // Need a selected row. Copy the fields we need out of the row
        // so the immutable borrow of `self` (via `selected_row`) is
        // released before we assign `self.codegraph_client` below —
        // holding the row borrow across the lazy client-open would
        // clash with the `&mut self` needed to populate it.
        let (node_id, symbol) = match self.selected_row() {
            None => {
                self.set_status_message("No row selected".to_string());
                return;
            }
            Some(row) => {
                // Only meaningful for codegraph /
                // tags(codegraph-fallback) rows.
                if row.mode != "codegraph" && row.mode != "tags" {
                    self.set_status_message(
                        "Callers/callees are available only in & / $ codegraph mode"
                            .to_string(),
                    );
                    return;
                }
                if row.codegraph_node_id.is_empty() {
                    // A `$` row from a real `tags` file has no
                    // CodeGraph node id — there's no `edges` row
                    // to query.
                    self.set_status_message(
                        "No CodeGraph node for this row (tags file has no codegraph id)"
                            .to_string(),
                    );
                    return;
                }
                let sym = if row.command.is_empty() {
                    "(symbol)".to_string()
                } else {
                    row.command.clone()
                };
                (row.codegraph_node_id.clone(), sym)
            }
        };
        // Ensure the read-only client is open (the `&` mode opens
        // it lazily; the `$` fallback does too).
        if self.codegraph_client.is_none() {
            self.codegraph_client = crate::codegraph::CodeGraphClient::open();
        }
        let Some(client) = self.codegraph_client.as_ref() else {
            self.set_status_message("No .codegraph/index found".to_string());
            return;
        };
        let repo_root = client.repo_root().to_path_buf();
        let callers = client.callers(&node_id, 50);
        let callees = client.callees(&node_id, 50);
        if callers.is_empty() && callees.is_empty() {
            self.set_status_message("No callers or callees recorded for this symbol".to_string());
            return;
        }
        let entries: Vec<CodegraphRelationEntry> = callers
            .iter()
            .map(|n| CodegraphRelationEntry {
                section: CodegraphRelationSection::Caller,
                node: n.clone(),
            })
            .chain(callees.iter().map(|n| CodegraphRelationEntry {
                section: CodegraphRelationSection::Callee,
                node: n.clone(),
            }))
            .collect();
        self.codegraph_relations_picker = Some(CodeGraphRelationsPicker {
            entries,
            selected: 0,
            symbol,
            // stash repo_root on the picker? it's used by Enter to
            // resolve the relation's relative file_path to an
            // absolute editor-openable path.
            repo_root,
        });
    }

    fn close_codegraph_relations_picker(&mut self) {
        self.codegraph_relations_picker = None;
    }

    /// The mode char of the current `self.query` (or
    /// `MODE_NONE` for plain no-prefix). The leading char is
    /// the mode identity — every prefix mode owns a
    /// `Vec<String>` of past queries in
    /// [`App::mode_query_history`], and the plain history lives
    /// under the `MODE_NONE` key. The empty query is treated
    /// as plain (no mode) so the "no history yet" case doesn't
    /// accidentally claim a mode.
    fn current_mode_char(&self) -> char {
        query_mode_char(&self.query, &self.query_prefixes)
    }

    /// Record `query` into the history of the given mode char.
    /// The query is recorded as-is (preserving its leading
    /// prefix char) so recalling it later puts the user back
    /// in the same mode. Empty / whitespace-only queries are
    /// skipped (no point recalling them). Consecutive
    /// duplicates are skipped so rapid re-runs of the same
    /// command don't bloat the history. The history is capped
    /// at 100 entries per mode; older entries are dropped
    /// from the tail (oldest end).
    ///
    /// Called from three places:
    /// 1. **Mode transition** (the leading char of the query
    ///    changed via backspace or push_char) — the OLD query
    ///    is recorded to the OLD mode's history via
    ///    [`App::on_query_mode_change`].
    /// 2. **Run** (the user picked a row and is exiting) — the
    ///    final query (with its prefix) is recorded to the
    ///    current mode's history.
    /// 3. **Tests** directly.
    fn record_to_mode_history(&mut self, mode_char: char, query: &str) {
        if query.trim().is_empty() {
            return;
        }
        let entry = self.mode_query_history.entry(mode_char).or_default();
        // Consecutive dedup: skip if the query is identical
        // to the most recent entry. Without this, the user
        // picking the same row three times in a row would
        // add three identical copies to the history and
        // the C-p recall would feel broken (cycling through
        // duplicates to get to the actual previous query).
        if entry.first().map(String::as_str) == Some(query) {
            return;
        }
        entry.insert(0, query.to_string());
        // Cap at 100 entries per mode so a long-lived TUI
        // session can't grow the JSON file unbounded.
        const MAX: usize = 100;
        if entry.len() > MAX {
            entry.truncate(MAX);
        }
    }

    /// Call when the leading char of `self.query` is about to
    /// change (e.g. user backspaced the prefix and is now in
    /// plain mode, or typed a new prefix over the leading
    /// char in LLM mode). Records the OLD query (with its
    /// previous mode) into the old mode's history, and
    /// resets the recall state for the new mode so the new
    /// mode starts at "live" (no draft, no recalled entry).
    ///
    /// The OLD query is passed in by the caller because by the
    /// time the leading char has actually changed, the OLD
    /// query is gone from `self.query` (it was mutated to
    /// the new form). Callers compute the OLD query before
    /// the mutation, then call this helper after.
    fn on_query_mode_change(&mut self, old_query: &str) {
        let old_mode = query_mode_char(old_query, &self.query_prefixes);
        self.record_to_mode_history(old_mode, old_query);
        // Reset the NEW mode's recall state. The user just
        // switched modes; their prior recall session (in the
        // old mode) is implicitly dropped. The new mode
        // starts at "live" so the first C-p recalls the new
        // mode's own history, not a leftover from the old
        // mode.
        let new_mode = self.current_mode_char();
        self.mode_query_history_index.insert(new_mode, None);
        self.mode_query_drafts.remove(&new_mode);
    }

    /// Exit recall mode (if active) and discard the saved
    /// draft. Called by `push_char`, `backspace`, and
    /// `clear_query`: any keystroke that mutates the query
    /// commits the recall session, so the user's edits become
    /// the "live" query and the next C-p starts a fresh
    /// recall cycle.
    fn history_exit_recall(&mut self) {
        let mode = self.current_mode_char();
        if self
            .mode_query_history_index
            .get(&mode)
            .copied()
            .flatten()
            .is_some()
        {
            self.mode_query_history_index.insert(mode, None);
            // The draft was the user's pre-recall
            // in-progress text. They've now edited the
            // recalled entry (or the live query) and
            // diverged from it, so the draft is no
            // longer relevant. Dropping it here means a
            // later C-n past the newest history entry
            // lands on an empty query (rather than
            // restoring a stale draft the user
            // intentionally diverged from).
            self.mode_query_drafts.remove(&mode);
        }
    }

    /// Move to the previous (older) entry in the current
    /// mode's history. Readline `previous-history`
    /// semantics:
    /// - From the live query (history_index = None): save
    ///   the in-progress query as a "draft" and load the
    ///   newest history entry.
    /// - From a recalled entry: move one step older
    ///   (index + 1, clamped at the oldest).
    /// - At the oldest entry: stay.
    /// - No history for this mode: no-op.
    fn history_previous(&mut self) {
        let mode = self.current_mode_char();
        let n = self
            .mode_query_history
            .get(&mode)
            .map(|v| v.len())
            .unwrap_or(0);
        if n == 0 {
            return;
        }
        let idx = self
            .mode_query_history_index
            .get(&mode)
            .copied()
            .flatten();
        match idx {
            None => {
                // Save the current in-progress query as
                // the draft for this mode (only the first
                // time we enter recall — re-recalling after
                // C-n back to draft is a no-op for the
                // draft, which `history_exit_recall`
                // would have already cleared).
                if !self.mode_query_drafts.contains_key(&mode) {
                    self.mode_query_drafts.insert(mode, self.query.clone());
                }
                let entry = self
                    .mode_query_history
                    .get(&mode)
                    .and_then(|v| v.first().cloned());
                if let Some(q) = entry {
                    self.query = q;
                    self.query_cursor = self.query.chars().count();
                    self.query_touched = true;
                    self.recompile_regex();
                    self.mode_query_history_index.insert(mode, Some(0));
                    self.refresh();
                }
            }
            Some(i) => {
                if i + 1 < n {
                    let next_i = i + 1;
                    let entry = self
                        .mode_query_history
                        .get(&mode)
                        .and_then(|v| v.get(next_i).cloned());
                    if let Some(q) = entry {
                        self.query = q;
                        self.query_cursor = self.query.chars().count();
                        self.query_touched = true;
                        self.recompile_regex();
                        self.mode_query_history_index.insert(mode, Some(next_i));
                        self.refresh();
                    }
                }
                // else: at oldest, stay
            }
        }
    }

    /// Move to the next (newer) entry in the current mode's
    /// history. Readline `next-history` semantics:
    /// - From the live query (history_index = None): no-op.
    /// - From the newest entry (index = 0): restore the
    ///   saved draft (or empty if no draft) and exit recall.
    /// - From a recalled entry: move one step newer
    ///   (index - 1).
    fn history_next(&mut self) {
        let mode = self.current_mode_char();
        let Some(i) = self
            .mode_query_history_index
            .get(&mode)
            .copied()
            .flatten()
        else {
            return; // already at the live query
        };
        if i == 0 {
            // Restore the draft and exit recall mode. The
            // draft is the in-progress query the user had
            // before they started recalling; clearing the
            // `mode_query_history_index` means subsequent
            // C-p starts a fresh recall cycle.
            let draft = self.mode_query_drafts.remove(&mode);
            self.query = draft.unwrap_or_default();
            self.query_cursor = self.query.chars().count();
            self.query_touched = true;
            self.recompile_regex();
            self.mode_query_history_index.insert(mode, None);
            self.refresh();
        } else {
            let next_i = i - 1;
            let entry = self
                .mode_query_history
                .get(&mode)
                .and_then(|v| v.get(next_i).cloned());
            if let Some(q) = entry {
                self.query = q;
                self.query_cursor = self.query.chars().count();
                self.query_touched = true;
                self.recompile_regex();
                self.mode_query_history_index.insert(mode, Some(next_i));
                self.refresh();
            }
        }
    }

    /// True if the user is currently recalling a history
    /// entry in the active mode (i.e. the live `self.query`
    /// was loaded by C-p / C-n, not typed). Used by the
    /// status bar to show "N/M" or similar so the user
    /// knows they're in recall mode. Currently unconsumed;
    /// retained for the future status-bar integration. Marked
    /// `#[allow(dead_code)]` so the public-but-unused method
    /// doesn't trip the unused-warning lint.
    #[allow(dead_code)]
    fn history_is_recalling(&self) -> bool {
        self.mode_query_history_index
            .get(&self.current_mode_char())
            .copied()
            .flatten()
            .is_some()
    }

    /// Load the per-mode query history from
    /// `<db_dir>/query_history.json` if it exists. Called
    /// once at TUI start (from `run_tui_to_stdout`) so the
    /// user's recall state survives across sessions.
    /// Missing / malformed / unreadable files are silently
    /// treated as "no history" — a corrupt sidecar must
    /// never block the TUI from launching.
    fn load_mode_history_from_disk(&mut self) {
        let Some(path) = self.mode_history_path() else {
            return;
        };
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return;
        };
        match serde_json::from_str::<std::collections::HashMap<char, Vec<String>>>(&contents) {
            Ok(map) => {
                self.mode_query_history = map;
            }
            Err(_) => {
                // Corrupt file (e.g. hand-edited, partial
                // write, schema mismatch). Drop it and
                // start fresh so the user isn't stuck in a
                // broken state. Future writes overwrite
                // the bad file.
                self.mode_query_history.clear();
            }
        }
    }

    /// Persist the current per-mode query history to
    /// `<db_dir>/query_history.json`. Called at TUI exit
    /// (from `run_tui_to_stdout`, near the session file
    /// save). Sessions/drafts/history_index are NOT
    /// persisted — only the history vectors — so the user
    /// always starts the next session in a clean
    /// "live query" state.
    fn persist_mode_history_to_disk(&self) {
        let Some(path) = self.mode_history_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&self.mode_query_history) {
            Ok(s) => {
                let _ = std::fs::write(&path, s);
            }
            Err(_) => {
                // Serialization failed (shouldn't happen
                // for `HashMap<char, Vec<String>>`); skip
                // the write. The user's history is lost
                // for this session, but the TUI continues
                // to work.
            }
        }
    }

    /// Resolve `<db_dir>/query_history.json`. Returns
    /// `None` if `HOME` is unset (so the path is
    /// unresolvable). The directory is shared with the
    /// smarthistory database (`~/.local/cache/smarthistory/`).
    fn mode_history_path(&self) -> Option<std::path::PathBuf> {
        let home = std::env::var("HOME").ok()?;
        Some(
            std::path::PathBuf::from(home)
                .join(".local")
                .join("cache")
                .join("smarthistory")
                .join("query_history.json"),
        )
    }


    /// True when the tab-completion
    /// menu is open. The completion
    /// menu is a sibling of the
    /// command palette, theme
    /// picker, and prefix picker —
    /// it sits above the help
    /// overlay and below the
    /// command palette in the
    /// input-handler hierarchy.
    fn is_completion_menu_open(&self) -> bool {
        self.completion_menu.is_some()
    }

    /// Open the completion menu with
    /// the given candidates. Called
    /// from the tab-completion path
    /// when the completion is
    /// ambiguous (2+ matches). The
    /// caller supplies the byte
    /// range in `self.query` that
    /// the original prefix occupied
    /// (so the menu knows exactly
    /// what to replace when the
    /// user commits a candidate)
    /// and the kind of completion
    /// (so the menu formats the
    /// selected candidate with the
    /// right prefix / suffix).
    fn open_completion_menu(
        &mut self,
        candidates: Vec<String>,
        replace_start_byte: usize,
        replace_end_byte: usize,
        replace_start_char: usize,
        kind: CompletionKind,
    ) {
        self.completion_menu = Some(CompletionMenu::new(
            candidates,
            replace_start_byte,
            replace_end_byte,
            replace_start_char,
            kind,
        ));
    }

    /// Close the completion menu
    /// without applying a candidate.
    /// Called when the user presses
    /// `Esc` or the `Cancel` binding
    /// while the menu is open. The
    /// query is left exactly as it
    /// was when the menu opened.
    fn close_completion_menu(&mut self) {
        self.completion_menu = None;
    }

    fn is_theme_picker_open(&self) -> bool {
        self.theme_picker.is_some()
    }

    fn is_add_entry_dialog_open(&self) -> bool {
        self.add_entry_dialog.is_some()
    }

    fn close_add_entry_dialog(&mut self) {
        self.add_entry_dialog = None;
    }

    fn open_theme_picker(&mut self) {
        self.theme_picker = Some(ThemePicker::new(self.theme));
    }

    /// Open the "add session /
    /// host" dialog. The
    /// dialog is pre-filled
    /// from the currently
    /// selected row's
    /// `directory` (used as
    /// the Dir / Host field)
    /// and `command` (shown in
    /// the dialog title as a
    /// reminder of which
    /// history row the entry
    /// is being created from).
    ///
    /// The action is a no-op
    /// (with a status
    /// message) when no row
    /// is selected, when the
    /// selected row has no
    /// `directory`, or when
    /// the config file can't
    /// be located. A second
    /// invocation while the
    /// dialog is already open
    /// is also a no-op (the
    /// existing dialog stays
    /// on screen).
    fn open_add_entry_dialog(&mut self, kind: AddEntryKind) {
        // Already-open
        // dialog: keep the
        // existing one
        // (re-entering would
        // surprise the user by
        // resetting their
        // typing).
        if self.add_entry_dialog.is_some() {
            return;
        }
        let Some(row) = self.selected_row() else {
            self.set_status_message("no row selected — move to a history row first".to_string());
            return;
        };
        let directory = row.directory.clone();
        if directory.is_empty() {
            self.set_status_message("selected row has no directory — nothing to add".to_string());
            return;
        }
        // Sanity-check the
        // config file: it
        // must be locatable,
        // otherwise we can't
        // write the new entry.
        // The lookup is the
        // same one
        // `Config::load` uses.
        if crate::config_path().is_none() {
            self.set_status_message(
                "no config file found — set $XDG_CONFIG_HOME/smarthistory/config \
                 or ~/.config/smarthistory/config"
                    .to_string(),
            );
            return;
        }
        self.add_entry_dialog = Some(AddEntryDialog::new(kind, directory, row.command.clone()));
        self.set_status_message(match kind {
            AddEntryKind::Session => "add session: type a name, then Tab".to_string(),
            AddEntryKind::Host => "add host: type a name, then Tab".to_string(),
        });
    }

    /// Commit the add-entry
    /// dialog: validate the
    /// fields, write the new
    /// entry to the config
    /// file, reload the
    /// in-memory session /
    /// host list, refresh the
    /// panes view, and close
    /// the dialog.
    ///
    /// On validation failure
    /// (e.g. empty Name), the
    /// dialog stays open and
    /// the error is surfaced
    /// in the dialog's own
    /// `error` field (shown
    /// inline in the dialog
    /// title).
    fn commit_add_entry_dialog(&mut self) {
        // Validate first, copying
        // out the first failing
        // field's name so we can
        // release the borrow of
        // `self.add_entry_dialog`
        // before mutating
        // `self.status_message`.
        let failing_name: Option<String> = self.add_entry_dialog.as_ref().and_then(|d| {
            d.fields
                .iter()
                .find(|f| f.required && f.value.trim().is_empty())
                .map(|f| f.name.to_string())
        });
        if let Some(name) = failing_name {
            self.set_status_message(format!("`{}` is required", name));
            if let Some(d) = self.add_entry_dialog.as_mut() {
                d.error = Some(format!("`{}` is required", name));
            }
            return;
        }
        // Hand off to the
        // write helper. On
        // success, the
        // helper closes the
        // dialog and reloads
        // the in-memory lists.
        let result = self.write_new_entry_to_config();
        if let Err(e) = result {
            self.set_status_message(format!("add-entry: {}", e));
            if let Some(d) = self.add_entry_dialog.as_mut() {
                d.error = Some(e);
            }
        }
    }

    /// Write the dialog's
    /// contents as a new
    /// `session.<id>` or
    /// `host.<id>` line in
    /// `~/.config/smarthistory/config`.
    /// On success, reloads
    /// the config and refreshes
    /// the panes view so the
    /// new row appears.
    ///
    /// Returns the new id on
    /// success, or an error
    /// string the caller can
    /// surface to the user.
    fn write_new_entry_to_config(&mut self) -> Result<usize, String> {
        // Snapshot the dialog
        // up-front so the
        // immutable borrow of
        // `self.add_entry_dialog`
        // is released before
        // we start mutating
        // `self` below.
        let (kind, fields) = {
            let d = self
                .add_entry_dialog
                .as_ref()
                .ok_or_else(|| "no dialog open".to_string())?;
            (d.kind, d.fields.clone())
        };
        let config_path = crate::config_path().ok_or_else(|| "no config file path".to_string())?;
        // Build the
        // config-file lines.
        // Each non-empty
        // field becomes a
        // line:
        //
        // session.3 = "Proxmox"
        // session.3.dir = "~/foo"
        // session.3.exec = "nvim"
        //
        // The Name field
        // (config_suffix = "")
        // becomes the first
        // line; sub-fields
        // follow. Empty
        // sub-fields are
        // skipped (no
        // `session.3.dir =`
        // lines for unset
        // values).
        let name = fields
            .first()
            .map(|f| f.value.trim().to_string())
            .ok_or_else(|| "dialog has no name field".to_string())?;
        if name.is_empty() {
            return Err("`Name` is required".to_string());
        }
        // Read the existing
        // config so we can
        // find the next
        // available id.
        let contents = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("failed to read {}: {}", config_path.display(), e,))?;
        let prefix = match kind {
            AddEntryKind::Session => "session",
            AddEntryKind::Host => "host",
        };
        let new_id = crate::tui::state::next_config_index(&contents, prefix)
            .ok_or_else(|| format!("no free {} id (existing ids are exhausted)", prefix,))?;
        // Build the new
        // lines. The Name
        // field's value is
        // always written
        // (the Name field is
        // required). Other
        // fields are skipped
        // when empty so the
        // config file stays
        // terse.
        let mut lines: Vec<String> = Vec::new();
        for field in &fields {
            let value = field.value.trim();
            if value.is_empty() {
                continue;
            }
            let line = if field.config_suffix.is_empty() {
                // Name field.
                format!("{}.{} = {:?}", prefix, new_id, value)
            } else {
                format!("{}.{}{} = {:?}", prefix, new_id, field.config_suffix, value,)
            };
            lines.push(line);
        }
        // Append to the
        // config file. Use
        // a trailing newline
        // if the file
        // doesn't end in
        // one, and a blank
        // line separator
        // between existing
        // content and the
        // new block for
        // readability.
        let mut new_contents = contents.clone();
        if !new_contents.ends_with('\n') {
            new_contents.push('\n');
        }
        new_contents.push('\n');
        for line in &lines {
            new_contents.push_str(line);
            new_contents.push('\n');
        }
        // Atomic write:
        // write to a temp
        // file in the same
        // directory, then
        // rename over the
        // original. This
        // avoids leaving a
        // half-written
        // config if the
        // process is killed
        // mid-write.
        let tmp_path = config_path.with_extension("tmp");
        std::fs::write(&tmp_path, new_contents.as_bytes())
            .map_err(|e| format!("failed to write {}: {}", tmp_path.display(), e,))?;
        std::fs::rename(&tmp_path, &config_path).map_err(|e| {
            format!(
                "failed to rename {} to {}: {}",
                tmp_path.display(),
                config_path.display(),
                e,
            )
        })?;
        // Reload the config
        // and update the
        // in-memory session /
        // host lists. The
        // simplest path is
        // to re-run
        // `Config::load()` —
        // the file is read
        // once at startup,
        // so this is the
        // first time we
        // refresh after a
        // write. The reload
        // also re-merges the
        // SSH config for
        // host entries, so
        // a user adding a
        // new host whose
        // alias matches an
        // existing SSH
        // config block
        // gets the
        // auto-filled
        // defaults
        // (Hostname, User,
        // etc.) on the very
        // next refresh.
        let new_cfg = crate::Config::load();
        self.sessions = new_cfg.sessions();
        self.hosts = new_cfg.hosts();
        self.host_defs = new_cfg.host_defs();
        // The panes view
        // was populated
        // from
        // `self.session_panes`
        // BEFORE we updated
        // sessions / hosts.
        // Clear and
        // re-refresh so the
        // `# sessions` and
        // `# hosts` blocks
        // pick up the new
        // entries.
        self.session_panes.clear();
        self.refresh();
        // Close the dialog.
        self.add_entry_dialog = None;
        let kind_label = match kind {
            AddEntryKind::Session => "session",
            AddEntryKind::Host => "host",
        };
        self.set_status_message(format!("added {} {:?} (id={})", kind_label, name, new_id,));
        Ok(new_id)
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
        self.labeled_rows = crate::tui::labeled::fetch(self).unwrap_or_default();
        if self.labeled_rows.is_empty() {
            self.labeled_list_state.select(None);
        } else {
            self.labeled_list_state.select(Some(0));
        }
    }

    // `fetch_labeled` was extracted to
    // `crate::tui::labeled::fetch` (the SQL query that
    // returns every history row that has a comment —
    // used to populate the labeled-rows partition that
    // `build_merged_rows` mixes in alongside the primary
    // fetch).


    fn delete_selected(&mut self) -> Result<()> {
        // Delete ALL history items with the same command text
        // (not just the one row). The user's intent when pressing
        // Ctrl-D on a history entry is "remove this command from
        // my history" — keeping duplicates with the same text
        // would be confusing. We also delete the command's
        // comment (if any) and its captured output rows (the
        // `history_output` table is `ON DELETE CASCADE` in
        // schema but SQLite doesn't enable FK enforcement by
        // default, so we do the cleanup explicitly).
        if let Some(row) = self.selected_row() {
            let command = row.command.clone();
            // Delete the comment for this command (if any).
            // The `command_comments` table has one row per
            // unique command text, so this removes the comment
            // globally — the user's intent is to remove the
            // command entirely, not leave an orphaned comment.
            self.conn.execute(
                "DELETE FROM command_comments WHERE command = ?1",
                params![&command],
            )?;
            // Delete captured output for all history rows
            // with this command (explicit cascade — SQLite's
            // `ON DELETE CASCADE` is not active without
            // `PRAGMA foreign_keys = ON`).
            self.conn.execute(
                "DELETE FROM history_output WHERE history_id IN \
                 (SELECT id FROM history WHERE command = ?1)",
                params![&command],
            )?;
            // Delete all history rows with the same command text.
            self.conn
                .execute("DELETE FROM history WHERE command = ?1", params![&command])?;
            self.refresh();
            self.refresh_labeled();
        }
        self.confirm_delete = None;
        Ok(())
    }

    fn delete_matching(&mut self) -> Result<()> {
        let (where_clause, params) = self.build_where();
        let sql = format!(
            "DELETE FROM history WHERE id IN (SELECT h.id FROM history h LEFT JOIN command_comments c ON h.command = c.command{})",
            where_clause
        );
        let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        self.conn.execute(&sql, &params_ref[..])?;
        self.refresh();
        self.refresh_labeled();
        self.confirm_delete = None;
        Ok(())
    }

    /// Delete ALL history entries whose `directory` column
    /// matches `dir`. Used by the directory-mode (`#`)
    /// delete flow: pressing `Ctrl-D` on a directory row
    /// prompts the user to confirm, then drops every
    /// command that was ever run in that directory.
    fn delete_directory(&mut self, dir: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM history WHERE directory = ?1", params![dir])?;
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
///
/// Resolve the TUI's initial query at startup. The precedences
/// are:
///   1. If `override_session_query` is true (the new `--prefix
///      <char>` CLI flag was given), the persisted `session_query`
///      is NOT restored — the user explicitly asked to start in a
///      particular prefix mode this launch. The returned
///      `effective_query` is the `initial_query` (which `main`
///      pre-fills with the prefix character when `--prefix` is
///      present), and `prefilled_query` is `None` (so the TUI's
///      `query_prefilled` flag is `false`, the user typed fresh
///      text and the cursor sits at the end).
///   2. Otherwise, if the session file has a persisted query,
///      it takes precedence — the user's last query is restored.
///      `effective_query` is the persisted value, and
///      `prefilled_query` is the persisted value (so the TUI
///      treats it as pre-filled text rather than fresh input,
///      and the first character typed replaces the pre-filled
///      buffer instead of appending).
///   3. Otherwise, the CLI-supplied `initial_query` (`--query`) is
///      used as-is, `prefilled_query` is `None`.
///
/// Returns `(prefilled_query, effective_query)` so `run_tui_to_stdout`
/// can pass both fields into `App::new` (the `query_prefilled` flag is
/// `prefilled_query.is_some()`).
///
/// The helper is extracted out of `run_tui_to_stdout` so its precedence
/// contract can be unit-tested directly (the surrounding `run_tui_to_stdout`
/// body is hard to test: it does terminal setup and TTY interaction).
fn resolve_initial_query(
    initial_query: &str,
    session_query: Option<&str>,
    override_session_query: bool,
) -> (Option<String>, String) {
    if override_session_query {
        return (None, initial_query.to_string());
    }
    match session_query {
        Some(q) => (Some(q.to_string()), q.to_string()),
        None => (None, initial_query.to_string()),
    }
}

/// Find a `tags` file by walking upward from the
/// current directory. Returns the first `tags` file
/// found (closest to the cwd), or a path pointing
/// to `tags` in the current directory if none is
/// found (the `read_to_string` call in `fetch_tags`
/// will then fail with a file-not-found error and
/// return an empty list — the historical behavior).
///
/// Walk: cwd → parent → parent → … until either a
/// `tags` file is found or we reach the filesystem
/// root (a directory whose parent is itself).
fn find_tags_file() -> std::path::PathBuf {
    let mut dir = std::env::current_dir().unwrap_or_default();
    loop {
        // Check for both lowercase `tags` and
        // uppercase `TAGS` — different ctags
        // invocations produce different
        // filenames (e.g. `ctags -R` writes
        // `tags`, while `etags` / `ctags -e`
        // writes `TAGS`). The lowercase form is
        // checked first (it's the more common
        // convention).
        let lower = dir.join("tags");
        if lower.is_file() {
            return lower;
        }
        let upper = dir.join("TAGS");
        if upper.is_file() {
            return upper;
        }
        match dir.parent() {
            Some(parent) if parent != dir => {
                dir = parent.to_path_buf();
            }
            _ => break,
        }
    }
    // No tags file found — return the default path
    // so the error message in `fetch_tags` is
    // consistent ("file not found" rather than
    // "no tags file searched").
    std::path::PathBuf::from("tags")
}

/// Read up to 5 lines of source context around
/// `line_number` (2 before, the line itself, 2
/// after) from the given file. Returns the
/// context as a newline-joined string; the
/// match line is prefixed with `>> ` so the
/// user can spot it at a glance in the details
/// pane. Returns the empty string on any error
/// (file not found, line number out of range,
/// etc.) — `fetch_tags` treats the context as
/// best-effort.
/// Read up to 5 lines of context around `line_number` in `filepath`,
/// returning a formatted string with line numbers. The target line is
/// marked with `>>` for visual distinction. Used by both tags mode
/// and ag mode to populate the details-pane preview.
/// The number of source-context lines loaded around a selected
/// symbol (`tags` / `codegraph` modes). 25 before, the match
/// line, and 24 after — i.e. 50 lines — give the user a full
/// function-body-or-file view in the `Ctrl-O` overlay while the
/// inline preview pane still shows whatever fits its height.
/// The `>>` marker on the match line stays aligned across the
/// half-window regardless of how much before/after context exists
/// toward the file boundaries.
pub const SOURCE_CONTEXT_LINES: usize = 50;

pub fn read_source_context(filepath: &str, line_number: usize) -> String {
    if line_number == 0 {
        return String::new();
    }
    let contents = match std::fs::read_to_string(filepath) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let lines: Vec<&str> = contents.lines().collect();
    // line_number is 1-based; convert to 0-based.
    let target = line_number.saturating_sub(1);
    if target >= lines.len() {
        return String::new();
    }
    let half = SOURCE_CONTEXT_LINES / 2;
    let start = target.saturating_sub(half);
    let end = (target + half).min(lines.len());
    let mut out: Vec<String> = Vec::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let absolute = start + i;
        if absolute == target {
            out.push(format!(">> {:>5}  {}", line_number, line));
        } else {
            out.push(format!("   {:>5}  {}", absolute + 1, line));
        }
    }
    out.join("\n")
}

/// Like [`read_source_context`], but caches the entire file
/// contents in `cache` so repeated lookups in the same file
/// (common for tags mode, where many symbols live in one
/// source file) only hit the disk once per TUI session.
pub fn read_source_context_with_cache(
    filepath: &str,
    line_number: usize,
    cache: &mut std::collections::HashMap<std::path::PathBuf, String>,
) -> String {
    if line_number == 0 {
        return String::new();
    }
    let path = std::path::PathBuf::from(filepath);
    if !cache.contains_key(&path) {
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                cache.insert(path.clone(), contents);
            }
            Err(_) => return String::new(),
        }
    }
    let contents = match cache.get(&path) {
        Some(s) => s,
        None => return String::new(),
    };
    let lines: Vec<&str> = contents.lines().collect();
    let target = line_number.saturating_sub(1);
    if target >= lines.len() {
        return String::new();
    }
    let half = SOURCE_CONTEXT_LINES / 2;
    let start = target.saturating_sub(half);
    let end = (target + half).min(lines.len());
    let mut out: Vec<String> = Vec::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let absolute = start + i;
        if absolute == target {
            out.push(format!(">> {:>5}  {}", line_number, line));
        } else {
            out.push(format!("   {:>5}  {}", absolute + 1, line));
        }
    }
    out.join("\n")
}

/// Prepend a single space to a TUI-staged selection
/// before it runs in the parent shell. Zsh's
/// `HIST_NO_STORE` (default-on) treats any command
/// whose first character is whitespace as "do not
/// save to the shell history" — the canonical
/// convention for "this command shouldn't be
/// persisted". The TUI honours the same convention
/// by prepending a space to every staged selection, so
/// the user's shell history doesn't accumulate
/// `bat README.md` (etc.) entries that the TUI
/// picked. The smarthistory DB also honours the same
/// convention (see `_smarthistory_precmd` in
/// `init.zsh`) — a space-prefixed command is
/// sensitive (a credential, a destructive op, a
/// private URL) and must not be persisted in either
/// place.
///
/// The space is prepended unconditionally (even on
/// already-space-prefixed commands — the user might
/// have set the selection themselves; the double-space
/// is harmless). An empty selection becomes `" "`,
/// which the parent shell will reject as an empty
/// command — the same as before this helper existed;
/// we're not changing the contract, just normalising
/// the format.
fn prefix_selection_with_space(sel: String) -> String {
    format!(" {}", sel)
}

/// Mode-aware wrapper around [`prefix_selection_with_space`].
/// Returns the selection unchanged when `mode_char` is
/// [`MODE_NONE`] (the plain no-prefix history mode);
/// otherwise prepends a single space so the parent
/// shell treats the command as "do not save to shell
/// history" (zsh `HIST_NO_STORE`) and the smarthistory
/// `init.zsh` precmd hook skips the DB write.
///
/// The history-mode exception is the user's
/// explicit request: replaying a row from history is a
/// command they *want* recorded — recording it keeps
/// the frequency stats accurate (so `Ctrl-S` next-
/// probable-command suggestions stay useful) and lets
/// the same command surface in future searches. Every
/// other mode (`+`, `=`, `%`, `@`, `!`, `#`, `*`, `-`,
/// `~`, `$`, `&`, `,`) stages a one-shot read (`bat
/// README.md`, `note_search edit-note <id>`, `open
/// <jira-url>`, etc.) that the user typically doesn't
/// want cluttering the DB — the space prefix keeps the
/// DB focused on commands worth recalling.
///
/// `mode_char` is the result of [`query_mode_char`] on
/// the current `app.query`; the caller computes it once
/// before taking the selection so the borrow of `app`
/// is released by the time `app.selection.take()` runs.
fn maybe_prefix_selection_with_space(sel: String, mode_char: char) -> String {
    if mode_char == MODE_NONE {
        sel
    } else {
        prefix_selection_with_space(sel)
    }
}

/// Run `smarthistory tui check` — the prefix-mode health
/// check. Builds a minimal `App` (same config / DB /
/// multiplexer setup as the TUI startup, but no terminal
/// rendering), runs the per-mode checks via
/// [`crate::tui::mode::run_all_checks`], and prints a
/// human-readable report.
///
/// When `prefix` is `Some`, only that prefix mode is
/// checked. When `None`, every prefix mode is checked.
///
/// Exit code: 0 if all checks pass, 1 if any `Warning`,
/// 2 if any `Error`.
pub fn run_tui_check(prefix: Option<String>, _exec: bool) -> Result<()> {
    use crate::tui::mode::{CheckStatus, ModeKind};

    let conn = crate::init_db()?;
    let app_cfg = Config::load();
    let bindings = app_cfg.key_bindings().clone();
    let query_prefixes = app_cfg.query_prefixes().clone();
    let notes_database = app_cfg.notes_database().map(|p| p.to_path_buf());
    let notes_dir = app_cfg.notes_dir().map(|p| p.to_path_buf());
    let llm_config = app_cfg.llm().cloned();
    let multiplexer = crate::multiplexer::backend_for(app_cfg.multiplexer());
    // No theme rendering in check mode, but install
    // the default palette anyway so `check` functions
    // that read colours don't panic on a missing
    // thread-local.
    crate::tui::theme::install_palette(crate::tui::theme::SelectedTheme::None);

    // Resolve which mode to check.
    let only: Option<ModeKind> = prefix.as_deref().and_then(|c| {
        let c = c.chars().next()?;
        // Match against the configured prefix chars,
        // not the defaults — so `--prefix @` works even
        // after the user remapped `prefix.notes=.`.
        match c {
            _ if c == query_prefixes.notes => Some(ModeKind::Notes),
            _ if c == query_prefixes.todo => Some(ModeKind::Todo),
            _ if c == query_prefixes.tags => Some(ModeKind::Tags),
            _ if c == query_prefixes.codegraph => Some(ModeKind::Codegraph),
            _ if c == query_prefixes.files => Some(ModeKind::Files),
            _ if c == query_prefixes.ag => Some(ModeKind::Ag),
            _ if c == query_prefixes.llm => Some(ModeKind::Llm),
            _ if c == query_prefixes.question => Some(ModeKind::Question),
            _ if c == query_prefixes.output => Some(ModeKind::Output),
            _ if c == query_prefixes.directories => Some(ModeKind::Directories),
            _ if c == query_prefixes.panes => Some(ModeKind::Panes),
            _ if c == query_prefixes.jira => Some(ModeKind::Jira),
            _ => None,
        }
    });
    if prefix.is_some() && only.is_none() {
        anyhow::bail!(
            "unknown prefix {:?}; expected one of the configured prefix characters",
            prefix
        );
    }

    let app = App::new(
        conn,
        Mode::Global,
        String::new(),
        true,                  // duplicate_filter
        ExitFilter::default(),
        SortOrder::default(),
        false,                 // query_prefilled
        crate::tui::theme::SelectedTheme::None,
        bindings,
        None,                  // llm client
        llm_config,
        query_prefixes,
        notes_database,
        notes_dir,
        app_cfg.todo_line_option().to_string(),
        app_cfg.jira_fragments().clone(),
        app_cfg.files_ignores().to_vec(),
        app_cfg.smart_open_file_commands().clone(),
        multiplexer,
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );

    let reports = crate::tui::mode::run_all_checks(&app, only);
    print_check_report(&reports);

    // Exit code: 0 = all ok, 1 = warnings, 2 = errors.
    let any_error = reports.iter().any(|r| r.worst_status() == CheckStatus::Error);
    let any_warning = reports.iter().any(|r| r.worst_status() == CheckStatus::Warning);
    if any_error {
        std::process::exit(2);
    } else if any_warning {
        std::process::exit(1);
    }
    Ok(())
}

/// Print a human-readable check report.
fn print_check_report(reports: &[crate::tui::mode::CheckReport]) {
    use crate::tui::mode::CheckStatus;

    println!("smarthistory prefix-mode health check");
    println!("======================================");
    println!();

    for report in reports {
        let (icon, _label) = match report.worst_status() {
            CheckStatus::Ok => ("✓", "ok"),
            CheckStatus::Warning => ("⚠", "WARN"),
            CheckStatus::Error => ("✗", "FAIL"),
        };
        println!("  {} {} — {}", icon, report.mode.display_name(), report.message);
        for detail in &report.details {
            let (dicon, _) = match detail.worst_status() {
                CheckStatus::Ok => ("  ✓", ""),
                CheckStatus::Warning => ("  ⚠", ""),
                CheckStatus::Error => ("  ✗", ""),
            };
            println!("      {} {}", dicon, detail.message);
        }
    }

    println!();
    let errors = reports.iter().filter(|r| r.worst_status() == CheckStatus::Error).count();
    let warnings = reports.iter().filter(|r| r.worst_status() == CheckStatus::Warning).count();
    let oks = reports.iter().filter(|r| r.worst_status() == CheckStatus::Ok).count();
    println!("  {} ok, {} warning(s), {} error(s)", oks, warnings, errors);
}

pub fn run_tui_to_stdout(
    initial_mode: String,
    initial_query: String,
    conn: Connection,
    llm: Option<Box<dyn crate::llm::LlmClient>>,
    llm_config: Option<crate::llm::LlmConfig>,
    override_session_query: bool,
    override_pane_visibility: Option<&str>,
    override_panes_filter: Option<&str>,
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
    let duplicate_filter = session.duplicate_filter.unwrap_or(app_cfg.duplicate_filter);
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
    //
    // `override_session_query` is set by the new `--prefix <char>`
    // CLI flag — the user explicitly asked to start the TUI in a
    // particular prefix mode this launch, so the persisted
    // `session.query` is NOT restored (the user's intent takes
    // final precedence, exactly as they specified). The CLI flag
    // resolves to `initial_query = "<prefix-char>"` in `main`, so
    // setting `prefilled_query = None` here makes the TUI start
    // with that prefix as the live query — the first frame already
    // shows the chosen view.
    let (prefilled_query, effective_query) = resolve_initial_query(
        &initial_query,
        session.query.as_deref(),
        override_session_query,
    );
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
    let initial_pane_visibility = session
        .pane_visibility
        .as_deref()
        .and_then(crate::tui::state::PaneVisibility::parse)
        .unwrap_or_default();
    let initial_pane_height = session
        .pane_height
        .as_deref()
        .and_then(crate::tui::state::PaneHeight::parse)
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
        app_cfg.smart_open_file_commands().clone(),
        crate::multiplexer::backend_for(app_cfg.multiplexer()),
        initial_pane_visibility,
        initial_pane_height,
    );
    // than the one we initialized with, honor it.
    if session.duplicate_filter.is_some() && session.duplicate_filter != Some(duplicate_filter) {
        app.duplicate_filter = session.duplicate_filter.unwrap_or(true);
    }
    // Load named sessions from the config file
    // (`session.<id>=...`, `session.<id>.dir=...`).
    app.sessions = app_cfg.sessions();
    if !app.sessions.is_empty() {
        // App::new calls refresh() which populates
        // session_panes BEFORE we set sessions
        // (because App::new initializes it to empty).
        // Clearing forces a re-fetch so the session
        // entries are appended on the next pass.
        app.session_panes.clear();
        app.refresh();
    }
    // Load hosts from the config file
    // (`host.<id>=...`, `host.<id>.host=...`, etc.)
    // merged with `~/.ssh/config` entries.
    app.hosts = app_cfg.hosts();
    app.host_defs = app_cfg.host_defs();
    if !app.hosts.is_empty() {
        // Same one-shot pattern as `sessions`:
        // the `*` view's `session_panes` was
        // populated by `App::new`'s first
        // `refresh()` before we knew about the
        // hosts. Clearing and re-refreshing
        // appends the `# hosts` block.
        app.session_panes.clear();
        app.refresh();
    }

    // Apply CLI overrides for pane
    // visibility and panes-filter.
    // The CLI flags take precedence
    // over the session file and the
    // defaults.
    if let Some(v) = override_pane_visibility
        && let Some(pv) = crate::tui::state::PaneVisibility::parse(v)
    {
        app.pane_visibility = pv;
    }
    if let Some(f) = override_panes_filter
        && let Some(pf) = crate::tui::state::PanesFilter::parse(f)
    {
        app.panes_filter = pf;
    }

    // Load the per-mode query history from
    // `<db_dir>/query_history.json` (if it exists). Done
    // AFTER all `App::new` / config-merge / CLI-override
    // steps so the loaded entries are the final state the
    // user will navigate. A missing or corrupt file is a
    // no-op (handled inside `load_mode_history_from_disk`)
    // so a partial write from a previous crash never
    // blocks the TUI from launching.
    app.load_mode_history_from_disk();

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
    // Compute the active mode char BEFORE taking the
    // selection so the immutable borrow of `app.query` /
    // `app.query_prefixes` is released before the
    // `&mut self`-style `app.selection.take()` call.
    // `MODE_NONE` (`'\0'`) means the no-prefix history
    // mode — the only case where we DON'T prepend a space
    // (see `maybe_prefix_selection_with_space`).
    let mode_char = query_mode_char(&app.query, &app.query_prefixes);
    let selection = if app.cancelled {
        None
    } else if let Some(sel) = app.selection.take() {
        let pm = app.pick_mode.unwrap_or(PickMode::Run).exit_code();
        // Mode-aware space prefixing. In history mode
        // (no leading prefix char) the selection is
        // returned unchanged so the command IS recorded
        // in the smarthistory DB (it's a history replay —
        // recording it keeps the frequency stats and
        // `Ctrl-S` next-probable-command suggestions
        // accurate). Every other mode prepends a single
        // space (zsh `HIST_NO_STORE` convention) so the
        // command stays out of both the shell history
        // and the smarthistory DB. See
        // [`maybe_prefix_selection_with_space`] for the
        // full rationale.
        Some((maybe_prefix_selection_with_space(sel, mode_char), pm))
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
        directory_source: if app.directory_source == crate::tui::state::DirectorySource::All {
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
        // Persist only when the user has changed the pane
        // visibility away from the default (`Both`). Same
        // policy as the other session fields.
        pane_visibility: if app.pane_visibility == crate::tui::state::PaneVisibility::Both {
            None
        } else {
            Some(app.pane_visibility.as_str().to_string())
        },
        pane_height: if app.pane_height == crate::tui::state::PaneHeight::Default {
            None
        } else {
            Some(app.pane_height.as_str().to_string())
        },
    };
    session.save();

    // Persist the per-mode query history to
    // `<db_dir>/query_history.json`. Done AFTER
    // `session.save()` so a write failure on one doesn't
    // block the other. Only the history vectors are
    // persisted — drafts and recall positions are
    // session-local and reset on the next launch.
    app.persist_mode_history_to_disk();

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
        // Tags mode loads source context lazily. Make sure
        // the row under the cursor has its preview before
        // each draw so the details/output pane never stays
        // empty just because selection changed through a
        // path we didn't instrument explicitly.
        crate::tui::mode::tags::ensure_selected_context(app);
        crate::tui::mode::codegraph::ensure_selected_context(app);
        crate::tui::mode::notes::ensure_selected_context(app);
        crate::tui::mode::todo::ensure_selected_context(app);
        crate::tui::mode::files::ensure_selected_context(app);
        crate::tui::mode::panes::ensure_selected_context(app);
        if let Err(e) = terminal.draw(|f| render::ui(f, app)) {
            return Err(anyhow::anyhow!("terminal draw failed: {}", e));
        }

        // Check for LLM result from background thread.
        if let Some(request) = app.llm_request.as_ref()
            && let Ok(result) = request.receiver.try_recv()
        {
            // Take ownership of the request before processing.
            if let Some(request) = app.llm_request.take() {
                app.process_llm_result(request, result);
            }
        }

        // Check for JIRA result from background thread
        // (mirrors the LLM poll above).
        if let Some(request) = app.jira_request.as_ref()
            && let Ok(result) = request.receiver.try_recv()
            && let Some(request) = app.jira_request.take()
        {
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
            && let Some(request) = app.files_state.request.take() {
                app.process_files_result(request, result);
            }

        // Check for ag-mode search result
        // from background thread. Mirrors the
        // files-mode poll above.
        if let Some(request) = app.ag_state.request.as_ref()
            && let Ok(result) = request.receiver.try_recv()
            && let Some(request) = app.ag_state.request.take() {
                app.process_ag_result(request, result);
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
            if let Some(request) = app.jira_comments_request.take() {
                let row = app.jira_rows.iter().find(|r| r.command == key).cloned();
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
                    app.set_status_message("JIRA row no longer available for comments".to_string());
                }
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
            if let Some(request) = app.jira_add_comment_request.take() {
                app.process_jira_add_comment_result(request, key, result);
            }
        }

        // Drain the background pane-cmdline lookup
        // (only in the herdr backend). Each ready
        // `(pane_id, cmdline)` pair patches the
        // matching pane row's `command` field in
        // place and triggers a merged-rows
        // rebuild so the next draw reflects
        // the cmdline (e.g. `nvim config.toml`,
        // `ssh har@host`) instead of just the
        // agent name.
        //
        // The poll is unconditional (not gated on
        // `is_panes_query`) so a lookup that was
        // in flight when the user left `*` mode
        // is still drained — the stale-result
        // guard in `process_pane_cmdlines` drops
        // anything that doesn't match the
        // current snapshot.
        app.process_pane_cmdlines();

        // Drive the various debounce timers (LLM / JIRA / files / ag
        // auto-calls) on the no-input path.
        // crossterm 0.29 can return `Err` from `event::poll` / `event::read`
        // for sequences it doesn't recognise — notably the malformed
        // Shift+Return encoding `ESC[27;5;13~` some terminals emit
        // (first param `27` isn't in crossterm's legacy `~`-terminated
        // special-key table, so the parser raises
        // `could_not_parse_event_error`). `?` would propagate that out
        // and the TUI would exit; instead we surface a status message
        // so the user can see their key wasn't decodable and rebind via
        // `key.<action>=<spec>` in the config file.
        let poll_result = crossterm::event::poll(Duration::from_millis(100));
        match poll_result {
            Ok(false) => {
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
                // Same debounce drive for ag-mode
                // searches: spawns the background
                // ag search after `AG_DEBOUNCE` of
                // quiet typing in `,` mode.
                app.ag_maybe_autocall();
                continue;
            }
            Ok(true) => {}
            Err(e) => {
                app.set_status_message(format!("input poll error: {e}"));
                continue;
            }
        }
        let event = match event::read() {
            Ok(ev) => ev,
            Err(e) => {
                app.set_status_message(format!(
                    "unrecognised key sequence (crossterm parse error: {e}); rebind via key.<action>=<spec> in config"
                ));
                continue;
            }
        };
        let Event::Key(key) = event else {
            continue;
        };

        // If an LLM request is in flight, check if this is a
        // cancel key. If so, cancel the request without leaving
        // the TUI.
        if app.llm_request.is_some()
            && let Some(action) = action_for_key(&app.bindings, &key)
            && matches!(action, Action::Cancel)
        {
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
            && matches!(action, Action::Cancel)
        {
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
            && matches!(action, Action::Cancel)
        {
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
            && matches!(action, Action::Cancel)
        {
            if let Some(request) = app.jira_add_comment_request.take() {
                request.cancelled.store(true, Ordering::Relaxed);
            }
            app.jira_add_comment_in_flight = false;
            app.set_status_message("JIRA add-comment cancelled".to_string());
            continue;
        }

        // Same cancel handling for an in-flight
        // ag search. Pressing Esc sets the
        // cancelled flag on the worker thread.
        if app.ag_state.request.is_some()
            && let Some(action) = action_for_key(&app.bindings, &key)
            && matches!(action, Action::Cancel)
        {
            if let Some(request) = app.ag_state.request.take() {
                request.cancelled.store(true, Ordering::Relaxed);
            }
            app.ag_state.in_flight = false;
            app.set_status_message("ag search cancelled".to_string());
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

    // The prefix picker is a
    // sibling to the command
    // palette: it also sits
    // above the help overlay so
    // the user can dismiss it
    // with their `Cancel`
    // binding without
    // accidentally scrolling
    // the help text underneath.
    if app.is_prefix_picker_open() {
        return handle_prefix_picker_key(app, key);
    }

    // The CodeGraph relations picker is a sibling overlay: it
    // also sits above the help overlay so the user can dismiss
    // it with `Cancel` without scrolling the help text underneath.
    if app.is_codegraph_relations_picker_open() {
        return handle_codegraph_relations_picker_key(app, key);
    }

    // The completion menu is a
    // sibling of the command
    // palette and prefix picker:
    // it also sits above the help
    // overlay so the user can
    // dismiss it with their
    // `Cancel` binding without
    // accidentally scrolling the
    // help text underneath. The
    // menu is shown when the user
    // presses `Tab` and the
    // completion is ambiguous
    // (multiple matches). The
    // user can navigate the
    // candidates and pick one
    // with `Enter`, or dismiss
    // with `Esc` / `Cancel` to
    // keep typing.
    if app.is_completion_menu_open() {
        return handle_completion_menu_key(app, key);
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
    if let Some(ref mode) = app.confirm_delete {
        return handle_confirm_delete_key(app, key, mode.clone());
    }

    // When editing a comment, most keys go to the comment buffer.
    if app.is_comment_editing() {
        return handle_comment_edit_key(app, key);
    }

    // The add-session / add-host
    // dialog takes precedence
    // over the action dispatch
    // so printable characters
    // type into the focused
    // field rather than into
    // the search query. Enter
    // commits (or surfaces a
    // validation error), Tab
    // / Shift+Tab move between
    // fields, Esc cancels.
    if app.is_add_entry_dialog_open() {
        return handle_add_entry_dialog_key(app, key);
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
        && let KeyCode::Char(c) = key.code
    {
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
            // In directories mode, deleting a row means
            // deleting ALL history entries in that directory.
            // Show a special confirmation dialog with the count.
            if app.is_directories_query() {
                if let Some(row) = app.selected_row() {
                    let dir = row.directory.clone();
                    let count: usize = app
                        .conn
                        .query_row(
                            "SELECT COUNT(*) FROM history WHERE directory = ?1",
                            params![&dir],
                            |row| row.get::<_, i64>(0),
                        )
                        .map(|n| n as usize)
                        .unwrap_or(0);
                    app.confirm_delete = Some(ConfirmMode::DeleteDirectory {
                        directory: dir,
                        count,
                    });
                }
            } else {
                app.confirm_delete = Some(ConfirmMode::DeleteSelected);
            }
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
        Action::DownloadJiraIssue => {
            // Downloading a JIRA issue as a
            // note is only meaningful inside
            // the JIRA search mode (`-...`)
            // where the selected row's
            // `command` field carries the
            // issue key (e.g. `PROJ-42`).
            // Outside of JIRA mode the
            // action is a no-op with a
            // status message so the user
            // understands why their key
            // did nothing — the `Ctrl-M-s`
            // key fires regardless of mode
            // (so it's a discoverable key
            // binding) but the *effect* is
            // gated. The helper itself
            // re-checks the mode so a stray
            // test or future caller can't
            // stage the wrong command from
            // outside JIRA mode.
            if !app.is_jira_query() {
                app.set_status_message(
                    "Download-JIRA-issue is only available in JIRA search (type `-`)".to_string(),
                );
                return false;
            }
            app.download_jira_issue();
            // Stay in the TUI if no row
            // was selected / the key was
            // empty so the user sees the
            // status message; exit
            // (returning `true`) once a
            // command is staged so the
            // parent shell can run it.
            app.selection.is_some()
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
        Action::MoveCursorLeft => {
            // Move the cursor one
            // character to the left
            // inside the search query.
            // Saturates at position
            // 0 so pressing Left at
            // the very start of the
            // query is a no-op. The
            // query string is
            // unchanged; only the
            // cursor position moves.
            app.move_query_cursor_left();
            false
        }
        Action::MoveCursorRight => {
            // Move the cursor one
            // character to the right
            // inside the search query.
            // Saturates at the end of
            // the query so pressing
            // Right past the last
            // character is a no-op.
            app.move_query_cursor_right();
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
        Action::AddSession => {
            // Open the add-entry
            // dialog in "session"
            // mode. The dialog
            // itself handles the
            // no-row case (status
            // message, no
            // `add_entry_dialog`
            // set).
            app.open_add_entry_dialog(crate::tui::state::AddEntryKind::Session);
            false
        }
        Action::AddHost => {
            // Same, but in "host"
            // mode. The Host field
            // is pre-filled with
            // the selected row's
            // directory basename.
            app.open_add_entry_dialog(crate::tui::state::AddEntryKind::Host);
            false
        }
        Action::FilterPanesWindows => {
            app.toggle_panes_filter(PanesFilter::Windows);
            false
        }
        Action::FilterPanesHosts => {
            app.toggle_panes_filter(PanesFilter::Hosts);
            false
        }
        Action::FilterPanesSessions => {
            app.toggle_panes_filter(PanesFilter::Sessions);
            false
        }
        Action::TogglePaneVisibility => {
            // Cycle through: BOTH → Details → OutputPreview → BOTH.
            app.pane_visibility = app.pane_visibility.next();
            app.set_status_message(format!("Pane layout: {}", app.pane_visibility.label()));
            false
        }
        Action::TogglePaneHeight => {
            // Toggle between `Default` (8 lines, ~50% of the
            // list area) and `Tall` (~70% of the list area).
            // Persisted in the session file so the user's
            // choice carries over to the next TUI startup.
            app.pane_height = app.pane_height.toggle();
            app.set_status_message(format!("Pane height: {}", app.pane_height.label()));
            false
        }
        Action::JiraFieldComplete => {
            // Tab-completion of JQL field
            // names inside the JIRA
            // search mode AND tag / link
            // names inside the notes
            // (`@`) and todos (`!`)
            // modes. The add-entry
            // dialog handles its own
            // Tab as field-next INSIDE
            // the dialog, so the two
            // paths never collide.
            if app.is_jira_query() {
                app.jira_field_complete_at_cursor();
            } else if app.is_notes_query() || app.is_todo_query() {
                app.notes_tab_complete_at_cursor();
            }
            // Outside all three
            // modes, Tab is a
            // no-op.
            false
        }
        Action::PickPrefix => {
            // Open the prefix picker.
            // The user gets a centred
            // list of every configured
            // prefix mode and can
            // choose one with
            // Up/Down + Enter. The
            // picker pre-selects the
            // row matching the
            // current query's leading
            // char (or the "no
            // prefix" row).
            //
            // Outside of the
            // prefixable state
            // (e.g. inside the
            // comment editor or
            // the add-entry
            // dialog) the action
            // is a no-op so the
            // key doesn't
            // interfere with
            // anything else.
            if app.comment_edit.is_some() || app.add_entry_dialog.is_some() {
                return false;
            }
            app.open_prefix_picker();
            false
        }
        Action::CodegraphRelations => {
            // Open the callers/callees picker for the selected
            // `&` / `$` (codegraph-backed) row. A no-op (with a
            // status message) outside codegraph / tags mode, when
            // no row is selected, or when the selected row has no
            // CodeGraph node id. The picker takes over key
            // routing until the user picks a relation (Enter →
            // open file + exit) or cancels (Esc → close overlay).
            app.open_codegraph_relations();
            false
        }
        Action::SmartOpen => {
            // Context-aware "dive" key (default C-]):
            // adapt to the active prefix mode. The
            // per-mode behaviour is dispatched through
            // the `ModeKind` enum so adding a new mode
            // only requires one new match arm here
            // (the if/else version grew to 5 arms
            // before this refactor and would have kept
            // growing).
            match crate::tui::mode::active_mode(app) {
                crate::tui::mode::ModeKind::Codegraph | crate::tui::mode::ModeKind::Tags => {
                    // In `&` / `$` (codegraph-backed) symbol
                    // mode, open the callers/callees picker
                    // (same as `Action::CodegraphRelations`).
                    app.open_codegraph_relations();
                    false
                }
                crate::tui::mode::ModeKind::Jira => {
                    // In `-` (JIRA) mode, open the
                    // selected issue's browse URL in
                    // the system browser **in the
                    // background** (same as pressing
                    // Enter, but spawned detached so
                    // the TUI stays open).
                    app.open_jira_in_background();
                    false
                }
                crate::tui::mode::ModeKind::Todo => {
                    // In `!` (Todo) mode, toggle the
                    // checkbox of the selected todo
                    // (same as `Action::MarkTodoDone`
                    // / `Ctrl-X`) — a "smart"
                    // companion to the existing "open
                    // the file at the line"
                    // behaviour of `Enter` (which the
                    // fallback also handles).
                    // `mark_todo_done` is no-op-safe
                    // (it surfaces a status message
                    // rather than staging a
                    // selection when no row is
                    // selected, or when the
                    // selection is not a todo row,
                    // or when `notes.dir` is unset),
                    // so dispatching directly is
                    // safe. Returning `false` (don't
                    // exit the TUI) matches the
                    // other SmartOpen branches'
                    // semantics — the user stays in
                    // the todo list to see the
                    // result (row removed / marker
                    // flipped) and can keep
                    // navigating.
                    app.mark_todo_done();
                    false
                }
                crate::tui::mode::ModeKind::Files => {
                    // File-type-aware open. Looks up
                    // the selected file's extension
                    // in `app.smart_open_file_commands`
                    // (populated from
                    // `smart-open.<ext>=<cmd>` config
                    // lines) and stages the configured
                    // command with the file path
                    // appended. Returns
                    // `Some((staged, true))` on a
                    // match (we exit so the parent
                    // shell runs the staged command)
                    // or `None` on no match / no row
                    // / non-file (the caller falls
                    // through to the `Run` action —
                    // open in `$EDITOR`).
                    let (exited, quit) = match app.smart_open_for_file() {
                        Some((_, quit)) => (true, quit),
                        None => (false, false),
                    };
                    if !exited {
                        app.select_for_run();
                        app.selection.is_some()
                    } else {
                        quit
                    }
                }
                // Everywhere else, fall through to
                // the normal `Run` action (select
                // row / open editor / fire LLM),
                // so the key is an ergonomic Enter
                // replacement across all modes.
                _ => {
                    app.select_for_run();
                    app.selection.is_some()
                }
            }
        }
        Action::PreviousHistory => {
            // Per-mode input-history recall (readline
            // `previous-history`). No-op outside the query
            // input state (comment edit, add-entry dialog,
            // overlays, help view, etc. all have their own
            // key handling routed earlier in `handle_key`,
            // so we only land here on the bare-query path).
            app.history_previous();
            false
        }
        Action::NextHistory => {
            // Mirror of PreviousHistory. `app.history_next()`
            // restores the saved draft when navigating past
            // the newest entry (i.e. back to "live" mode).
            app.history_next();
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
    let is_cancel_key = action_for_key(&app.bindings, &key) == Some(Action::Cancel);
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            match &mode {
                ConfirmMode::DeleteSelected => {
                    let _ = app.delete_selected();
                }
                ConfirmMode::DeleteMatching => {
                    let _ = app.delete_matching();
                }
                ConfirmMode::DeleteDirectory { directory, .. } => {
                    let dir = directory.clone();
                    let _ = app.delete_directory(&dir);
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
    if action_for_key(&app.bindings, &key) == Some(Action::Cancel) {
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
/// Key handler for the tab-completion menu.
/// `Up` / `Down` / `Ctrl-N` / `Ctrl-P`
/// navigate the candidate list; `Enter`
/// commits the selected candidate
/// (replaces the original prefix with
/// the formatted completion); the user's
/// `Cancel` binding (e.g. `Esc` or
/// `Ctrl-C`) closes the menu without
/// changing the query.
fn handle_completion_menu_key(app: &mut App, key: KeyEvent) -> bool {
    // Dismiss on the user's `Cancel`
    // binding.
    if action_for_key(&app.bindings, &key) == Some(Action::Cancel) {
        app.close_completion_menu();
        return false;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.cancelled = true;
        app.close_completion_menu();
        return true;
    }

    match key.code {
        KeyCode::Enter => {
            // Commit the selected
            // candidate. Read the
            // menu fields, then close
            // it and apply the
            // replacement.
            let (replace_start_byte, replace_end_byte, replace_start_char, formatted) = {
                let Some(menu) = app.completion_menu.as_ref() else {
                    return false;
                };
                let formatted = menu.format_selected();
                (
                    menu.replace_start_byte,
                    menu.replace_end_byte,
                    menu.replace_start_char,
                    formatted,
                )
            };
            app.close_completion_menu();
            if !formatted.is_empty() {
                // Replace the
                // original prefix
                // range with the
                // formatted
                // completion. The
                // byte indices are
                // exact so the
                // replacement is
                // O(1) regardless
                // of how long the
                // prefix was.
                app.query
                    .replace_range(replace_start_byte..replace_end_byte, &formatted);
                let formatted_chars = formatted.chars().count();
                app.query_cursor = replace_start_char + formatted_chars;
                // Re-arm the search
                // debounce and
                // refresh so the
                // new query fires
                // its search
                // immediately.
                // Mirrors the
                // single-match
                // tab-completion
                // path.
                app.llm_touch();
                app.recompile_regex();
                app.refresh();
            }
            false
        }
        KeyCode::Up => {
            if let Some(menu) = app.completion_menu.as_mut()
                && menu.selected > 0
            {
                menu.selected -= 1;
            }
            false
        }
        KeyCode::Down => {
            if let Some(menu) = app.completion_menu.as_mut() {
                let n = menu.candidates.len();
                if n > 0 && menu.selected + 1 < n {
                    menu.selected += 1;
                }
            }
            false
        }
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(menu) = app.completion_menu.as_mut() {
                let n = menu.candidates.len();
                if n > 0 && menu.selected + 1 < n {
                    menu.selected += 1;
                }
            }
            false
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(menu) = app.completion_menu.as_mut()
                && menu.selected > 0
            {
                menu.selected -= 1;
            }
            false
        }
        KeyCode::Char('j') => {
            if let Some(menu) = app.completion_menu.as_mut() {
                let n = menu.candidates.len();
                if n > 0 && menu.selected + 1 < n {
                    menu.selected += 1;
                }
            }
            false
        }
        KeyCode::Char('k') => {
            if let Some(menu) = app.completion_menu.as_mut()
                && menu.selected > 0
            {
                menu.selected -= 1;
            }
            false
        }
        KeyCode::Home => {
            if let Some(menu) = app.completion_menu.as_mut() {
                menu.selected = 0;
            }
            false
        }
        KeyCode::End => {
            if let Some(menu) = app.completion_menu.as_mut() {
                let n = menu.candidates.len();
                if n > 0 {
                    menu.selected = n - 1;
                }
            }
            false
        }
        _ => false,
    }
}

/// Key handler for the prefix picker. Up/Down
/// (and `j`/`k` / `Ctrl-N` / `Ctrl-P`) move
/// the selection; Enter commits (applies the
/// selected prefix); the user's `Cancel`
/// binding (e.g. Esc or Ctrl-C) dismisses the
/// picker without changing the query.
fn handle_prefix_picker_key(app: &mut App, key: KeyEvent) -> bool {
    // Dismiss on the user's `Cancel` binding.
    if action_for_key(&app.bindings, &key) == Some(Action::Cancel) {
        app.close_prefix_picker();
        return false;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.cancelled = true;
        app.close_prefix_picker();
        return true;
    }

    // Capture a mutable borrow of the picker once.
    let picker = match app.prefix_picker.as_mut() {
        Some(p) => p,
        None => return false,
    };

    match key.code {
        KeyCode::Enter => {
            if let Some(opt) = picker.selected().copied() {
                app.close_prefix_picker();
                app.apply_prefix(opt.prefix);
            } else {
                app.close_prefix_picker();
            }
            false
        }
        KeyCode::Up => {
            if picker.selected > 0 {
                picker.selected -= 1;
            }
            false
        }
        KeyCode::Down => {
            let n = picker.options.len();
            if n > 0 && picker.selected + 1 < n {
                picker.selected += 1;
            }
            false
        }
        KeyCode::PageUp => {
            picker.selected = picker.selected.saturating_sub(5);
            if picker.options.is_empty() {
                picker.selected = 0;
            } else if picker.selected >= picker.options.len() {
                picker.selected = picker.options.len() - 1;
            }
            false
        }
        KeyCode::PageDown => {
            let n = picker.options.len();
            picker.selected = (picker.selected + 5).min(n.saturating_sub(1));
            false
        }
        KeyCode::Home => {
            picker.selected = 0;
            false
        }
        KeyCode::End => {
            let n = picker.options.len();
            if n > 0 {
                picker.selected = n - 1;
            }
            false
        }
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let n = picker.options.len();
            if n > 0 && picker.selected + 1 < n {
                picker.selected += 1;
            }
            false
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if picker.selected > 0 {
                picker.selected -= 1;
            }
            false
        }
        _ => false,
    }
}

/// Key handler for the CodeGraph relations picker. Up/Down (and
/// `Ctrl-N`/`Ctrl-P`) move the selection past section headers;
/// `PageUp`/`PageDown`/`Home`/`End` jump; Enter opens the
/// highlighted relation's source file in `$EDITOR +LINE path`
/// and exits the TUI (mirroring the main list's tags/codegraph
/// selection); the user's `Cancel` binding (Esc / Ctrl-C)
/// dismisses the picker without opening anything.
fn handle_codegraph_relations_picker_key(app: &mut App, key: KeyEvent) -> bool {
    // Dismiss on the user's `Cancel` binding.
    if action_for_key(&app.bindings, &key) == Some(Action::Cancel) {
        app.close_codegraph_relations_picker();
        return false;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.cancelled = true;
        app.close_codegraph_relations_picker();
        return true;
    }

    // Movement keys only need the index; do them with a short
    // mutable borrow of the picker.
    let n = match app.codegraph_relations_picker.as_ref() {
        Some(p) => p.entries.len(),
        None => return false,
    };
    let move_delta = match key.code {
        // Plain arrow keys have no modifiers, so the guard must
        // NOT apply to them — splitting the arm keeps `Up`/`Down`
        // (the primary navigation) working while `Ctrl-P`/`Ctrl-N`
        // stay a separate guarded arm. (Combining them as
        // `KeyCode::Up | KeyCode::Char('p') if CONTROL` would make
        // the guard apply to the whole or-pattern, swallowing plain
        // `Up`.)
        KeyCode::Up => Some(-1isize),
        KeyCode::Down => Some(1isize),
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(-1isize),
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(1isize),
        KeyCode::PageUp => Some(-5isize),
        KeyCode::PageDown => Some(5isize),
        KeyCode::Home => {
            if let Some(p) = app.codegraph_relations_picker.as_mut() {
                p.selected = 0;
            }
            return false;
        }
        KeyCode::End => {
            if let Some(p) = app.codegraph_relations_picker.as_mut() {
                p.selected = n.saturating_sub(1);
            }
            return false;
        }
        _ => None,
    };
    if let Some(delta) = move_delta {
        if let Some(p) = app.codegraph_relations_picker.as_mut() {
            let next = (p.selected as isize + delta).clamp(0, n.saturating_sub(1) as isize) as usize;
            p.selected = next;
        }
        return false;
    }

    // Enter: open the highlighted relation's source file. Copy
    // the fields out of the picker (so the borrow is released
    // before we stage the selection), close the picker, and stage
    // `$EDITOR +LINE path` exactly like selecting a codegraph row
    // in the main list. Returning `true` exits the TUI so the
    // parent shell runs the editor command.
    if key.code == KeyCode::Enter {
        let picked = app
            .codegraph_relations_picker
            .as_ref()
            .and_then(|p| p.selected().map(|e| (e.node.clone(), p.repo_root.clone())));
        if let Some((node, repo_root)) = picked {
            app.close_codegraph_relations_picker();
            let editor = std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "vi".to_string());
            let abs = node.abs_path(&repo_root);
            let quoted = crate::util::shell_quote(&abs.to_string_lossy());
            app.selection = Some(format!("{} +{} {}", editor, node.start_line, quoted));
            app.pick_mode = Some(PickMode::Run);
            return true;
        }
        // Nothing selected (empty list — shouldn't happen since
        // the opener guards against it); just close.
        app.close_codegraph_relations_picker();
        return false;
    }
    false
}

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

    // Backspace removes the last character from the search
    // query; the filtered list widens accordingly.
    if key.code == KeyCode::Backspace {
        if let Some(picker) = app.theme_picker.as_mut() {
            picker.backspace();
            app.theme = picker.current();
            install_palette(app.theme);
        }
        return false;
    }

    // Printable characters extend the search query. The
    // filtered list narrows live, matching the command-
    // palette UX.
    if !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && let KeyCode::Char(c) = key.code
    {
        if let Some(picker) = app.theme_picker.as_mut() {
            picker.push_char(c);
            app.theme = picker.current();
            install_palette(app.theme);
        }
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
                picker.selected = picker.filtered().len().saturating_sub(1);
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
        && let Some(picker) = app.theme_picker.as_mut()
    {
        picker.move_by(delta);
        app.theme = picker.current();
        install_palette(app.theme);
    }
    false
}

/// Key handler used while viewing captured output. Returns a result
/// describing what the run loop should do next.
fn handle_output_view_key(app: &mut App, key: KeyEvent, page_size: usize) -> OutputViewResult {
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
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(ref mut view) = app.output_view {
                view.scroll = view.scroll.saturating_sub(1);
            }
            OutputViewResult::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(ref mut view) = app.output_view {
                let max = max_scroll(&view.text);
                view.scroll = (view.scroll + 1).min(max);
            }
            OutputViewResult::Continue
        }
        KeyCode::PageUp | KeyCode::Char('K') => {
            if let Some(ref mut view) = app.output_view {
                view.scroll = view.scroll.saturating_sub(page_size.max(1));
            }
            OutputViewResult::Continue
        }
        KeyCode::PageDown | KeyCode::Char('J') => {
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
fn handle_describe_view_key(app: &mut App, key: KeyEvent, page_size: usize) -> bool {
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
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
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
                view.scroll = view.scroll.saturating_sub(page_size.max(1));
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
fn handle_question_view_key(app: &mut App, key: KeyEvent, page_size: usize) -> bool {
    let max_scroll = |text: &str| -> usize {
        let total = text.lines().count();
        total.saturating_sub(page_size.max(1))
    };
    let is_close = matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q'));
    if is_close {
        app.close_question();
        return false;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
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
                view.scroll = view.scroll.saturating_sub(page_size.max(1));
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
    let mut homes: Vec<String> = std::iter::once(std::env::var("HOME").unwrap_or_default())
        .chain(
            cfg.home_map()
                .iter()
                .filter_map(|p| p.to_str().map(str::to_string)),
        )
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
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
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
                .unwrap_or_else(|_| sub.to_string_lossy().into_owned());
            if seen.insert(key) {
                out.push(sub);
            }
        }
    }
    out
}

#[allow(dead_code)]
fn parse_tmux_pane_line(line: &str) -> Option<TmuxWindowInfo> {
    // The directory / panes
    // TUI no longer parses
    // raw `tmux list-windows`
    // output here — the
    // configured backend
    // (tmux or herdr) owns
    // the snapshot and the
    // parsing. This helper
    // is retained for unit
    // tests (the
    // `parse_tmux_pane_line_*`
    // tests in the test
    // module still exercise
    // the parsing logic) and
    // as documentation of the
    // tmux format.
    //
    // `split('|')` with trim
    // on each field. Four
    // fields, no quoting, no
    // escaping — `|` is the
    // format separator and
    // never appears inside
    // any of the four fields
    // in real-world tmux
    // output.
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
        // The legacy 4-field
        // format doesn't carry
        // the foreground
        // command or the
        // session name, so
        // both are empty.
        // Real-world snapshots
        // (populated by
        // `fetch_tmux_windows`
        // via the configured
        // backend) carry both
        // — the legacy
        // helper is only
        // exercised by
        // `parse_tmux_pane_line_*`
        // tests.
        current_command: String::new(),
        workspace_label: String::new(),
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

/// Input handling for the
/// add-session / add-host
/// dialog. The dialog is
/// a multi-field text
/// editor with Tab/Shift+Tab
/// to move between fields,
/// Enter to commit, Esc to
/// cancel, and the standard
/// printable-character /
/// Backspace / Left / Right
/// edits inside each field.
///
/// Always returns `false`
/// (the TUI doesn't exit
/// when the dialog
/// commits — the dialog
/// closes itself, and the
/// user is left looking at
/// the refreshed panes
/// view with a status
/// message confirming
/// the new entry).
fn handle_add_entry_dialog_key(app: &mut App, key: KeyEvent) -> bool {
    // The cancel / cancel-buffer
    // shortcuts mirror the
    // comment-edit dialog so
    // the muscle memory is
    // identical: Ctrl-C
    // aborts the whole TUI;
    // Esc just closes the
    // dialog.
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => {
                app.cancelled = true;
                return true;
            }
            // Ctrl-U clears the
            // focused field. Same
            // shortcut as the main
            // search query.
            KeyCode::Char('u') => {
                if let Some(d) = app.add_entry_dialog.as_mut()
                    && let Some(field) = d.fields.get_mut(d.focused)
                {
                    field.value.clear();
                    field.cursor = 0;
                    d.error = None;
                }
                return false;
            }
            // Ctrl-W deletes one
            // word backward in the
            // focused field.
            KeyCode::Char('w') => {
                if let Some(d) = app.add_entry_dialog.as_mut()
                    && let Some(field) = d.fields.get_mut(d.focused)
                {
                    delete_field_word_backward(field);
                    d.error = None;
                }
                return false;
            }
            _ => return false,
        }
    }

    match key.code {
        KeyCode::Esc => {
            // Cancel: close the
            // dialog without
            // writing anything to
            // the config file.
            app.close_add_entry_dialog();
            app.set_status_message("add-entry: cancelled".to_string());
            false
        }
        KeyCode::Enter => {
            // Commit: validate the
            // required fields, then
            // write the entry. The
            // commit method closes
            // the dialog on success
            // and keeps it open (with
            // an error) on failure.
            app.commit_add_entry_dialog();
            false
        }
        KeyCode::Tab => {
            // Next field. Shift+Tab
            // (KeyCode::BackTab)
            // goes the other way.
            if let Some(d) = app.add_entry_dialog.as_mut() {
                d.focus_next();
                d.error = None;
            }
            false
        }
        KeyCode::BackTab => {
            if let Some(d) = app.add_entry_dialog.as_mut() {
                d.focus_prev();
                d.error = None;
            }
            false
        }
        KeyCode::Backspace => {
            if let Some(d) = app.add_entry_dialog.as_mut()
                && let Some(field) = d.fields.get_mut(d.focused)
                && field.cursor > 0
            {
                // Delete the
                // character
                // before the
                // cursor. The
                // cursor is in
                // characters
                // (matching the
                // main query
                // editor), so
                // we walk left
                // one char
                // from the
                // byte index
                // that maps
                // to
                // `cursor - 1`
                // characters.
                let byte_idx = char_to_byte_idx(&field.value, field.cursor - 1);
                // Find the
                // next
                // char
                // boundary
                // after
                // `byte_idx`
                // (i.e. the
                // char
                // that
                // occupies
                // positions
                // `cursor - 1`
                // ).
                if let Some(next) = field.value[byte_idx..].chars().next() {
                    field
                        .value
                        .replace_range(byte_idx..byte_idx + next.len_utf8(), "");
                }
                field.cursor -= 1;
                d.error = None;
            }
            false
        }
        KeyCode::Left => {
            if let Some(d) = app.add_entry_dialog.as_mut()
                && let Some(field) = d.fields.get_mut(d.focused)
                && field.cursor > 0
            {
                field.cursor -= 1;
            }
            false
        }
        KeyCode::Right => {
            if let Some(d) = app.add_entry_dialog.as_mut()
                && let Some(field) = d.fields.get_mut(d.focused)
                && field.cursor < field.value.chars().count()
            {
                field.cursor += 1;
            }
            false
        }
        KeyCode::Home => {
            if let Some(d) = app.add_entry_dialog.as_mut()
                && let Some(field) = d.fields.get_mut(d.focused)
            {
                field.cursor = 0;
            }
            false
        }
        KeyCode::End => {
            if let Some(d) = app.add_entry_dialog.as_mut()
                && let Some(field) = d.fields.get_mut(d.focused)
            {
                field.cursor = field.value.chars().count();
            }
            false
        }
        KeyCode::Char(c) => {
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
                && let Some(d) = app.add_entry_dialog.as_mut()
                && let Some(field) = d.fields.get_mut(d.focused)
            {
                // Insert `c` at
                // the cursor.
                // The cursor
                // is in
                // characters,
                // so convert
                // to a byte
                // index
                // first.
                let byte_idx = char_to_byte_idx(&field.value, field.cursor);
                field.value.insert(byte_idx, c);
                field.cursor += 1;
                d.error = None;
            }
            false
        }
        _ => false,
    }
}

/// Convert a character
/// index in `s` to the
/// corresponding byte
/// index. Returns `s.len()`
/// when `idx` is at or past
/// the end of `s`. Used by
/// the dialog's per-field
/// cursor to translate
/// between the character-
/// oriented cursor (which
/// is what the user
/// perceives) and the
/// byte-oriented `String`
/// API.
fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or_else(|| s.len())
}

/// Delete one word backward
/// in a dialog field's
/// value (the readline /
/// bash `Ctrl-W` semantics).
/// First eats trailing
/// whitespace immediately
/// before the cursor, then
/// walks left through the
/// preceding run of
/// non-whitespace characters
/// and removes them.
fn delete_field_word_backward(field: &mut crate::tui::state::DialogField) {
    if field.cursor == 0 {
        return;
    }
    let chars: Vec<char> = field.value.chars().collect();
    let mut new_cursor = field.cursor;
    // Phase 1: skip
    // trailing
    // whitespace
    // immediately
    // before the
    // cursor.
    while new_cursor > 0 && chars[new_cursor - 1].is_whitespace() {
        new_cursor -= 1;
    }
    // Phase 2: walk
    // left through
    // the
    // non-whitespace
    // run.
    while new_cursor > 0 && !chars[new_cursor - 1].is_whitespace() {
        new_cursor -= 1;
    }
    // Convert the
    // new cursor
    // position to a
    // byte index
    // and splice out
    // everything
    // from there to
    // the old
    // cursor.
    let old_byte = char_to_byte_idx(&field.value, field.cursor);
    let new_byte = char_to_byte_idx(&field.value, new_cursor);
    field.value.replace_range(new_byte..old_byte, "");
    field.cursor = new_cursor;
}

#[cfg(test)]
#[path = "tui/tests.rs"]
mod tests;
