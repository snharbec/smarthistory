//! AG-mode content search.
//!
//! Runs `ag` (The Silver Searcher) in the current directory
//! on a background thread, parses the results, and returns
//! rows the TUI can render. The pattern mirrors the files-
//! mode walker (`src/files.rs`) and the JIRA search path
//! (`src/jira.rs`): a background thread does the actual work,
//! an mpsc channel reports results, and an `Arc<AtomicBool>`
//! cancellation flag lets the run loop abort stale searches.
//!
//! ## Search semantics
//!
//! The query body is split on whitespace via the shared
//! [`crate::highlight::parse_query_tokens`] helper:
//!
//! - **Search terms** (no prefix): the first becomes ag's pattern.
//! - **Glob tokens** (`*`): converted to regex and passed via `-G`.
//! - **Language tokens** (`@rust`): stripped of `@` and passed as `--rust`.
//!
//! Examples:
//!   `,result @rust`     -> `ag ... --rust "result"`
//!   `,tui *.rs @rust`   -> `ag ... -G '.*\.rs$' --rust "tui"`

use crate::highlight::{highlight_with_bat, parse_query_tokens};
use crate::tui::read_source_context;
use crate::tui::state::HistoryRow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// How long the ag-mode debounce waits after the last
/// keystroke before spawning the background search.
/// Same value as JIRA and files modes (400 ms).
pub const AG_DEBOUNCE: Duration = Duration::from_millis(400);

/// An in-flight ag search. The background thread sends
/// results over `receiver`; the run loop polls it.
/// `cancelled` lets the run loop abort a stale search.
pub struct AgRequest {
    pub receiver: mpsc::Receiver<Vec<HistoryRow>>,
    pub cancelled: Arc<AtomicBool>,
    /// The pattern that was being searched for.
    pub pattern: String,
}

/// Aggregated ag-mode state. Held by the TUI App.
pub struct AgState {
    /// Debounce timer, armed on every keystroke in ag mode.
    pub debounce_started: Option<std::time::Instant>,
    /// Last successfully searched pattern. Prevents re-querying
    /// when the pattern hasn't changed.
    pub last_pattern: Option<String>,
    /// Whether a search is currently in flight.
    pub in_flight: bool,
    /// In-flight request (background thread).
    pub request: Option<AgRequest>,
    /// Cached results of the most recent search.
    pub rows: Vec<HistoryRow>,
}

impl AgState {
    pub fn new() -> Self {
        AgState {
            debounce_started: None,
            last_pattern: None,
            in_flight: false,
            request: None,
            rows: Vec::new(),
        }
    }

    /// Extract the body after the prefix character.
    pub fn current_pattern(query: &str, prefix: char) -> String {
        let body = if query.starts_with(prefix) {
            &query[prefix.len_utf8()..]
        } else {
            query
        };
        body.trim().to_string()
    }

    pub fn has_results_for(&self, pattern: &str) -> bool {
        self.last_pattern.as_deref() == Some(pattern)
    }
}

impl Default for AgState {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn a background thread that runs `ag`, parses the
/// output, and sends the result rows back.
///
/// The `ag` binary must be on PATH. If it is not, or if
/// it exits non-zero, an empty result set is returned
/// (the TUI will show an empty list).
pub fn spawn_ag_search(pattern: String) -> AgRequest {
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let pattern_for_thread = pattern.clone();

    std::thread::spawn(move || {
        let rows = run_ag(&pattern_for_thread);
        if !cancelled_clone.load(Ordering::Relaxed) {
            let _ = tx.send(rows);
        }
    });

    AgRequest {
        receiver: rx,
        cancelled,
        pattern,
    }
}

/// Convert a shell-style glob pattern to a PCRE regex for `ag -G`.
///
/// `ag -G` expects a regex that matches against the full file path.
/// The user types shell-style globs (e.g. `*.rs`), so we convert:
///   - `*`  -> `.*`  (glob wildcard -> regex wildcard)
///   - `.`  -> `\.`  (literal dot)
///   - other regex metacharacters are escaped too
///   - `$` is appended to anchor at end-of-path
///
/// Examples:
///   *.rs      -> .*\.rs$
///   bla*.txt  -> bla.*\.txt$
fn glob_to_ag_regex(glob: &str) -> String {
    let mut regex = String::new();
    for c in glob.chars() {
        match c {
            '*' => regex.push_str(".*"),
            // Escape regex metacharacters so they are treated literally.
            '.' | '+' | '?' | '[' | ']' | '(' | ')' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                regex.push('\\');
                regex.push(c);
            }
            _ => regex.push(c),
        }
    }
    regex.push('$');
    regex
}

/// Build and run the `ag` command for the given user pattern.
fn run_ag(pattern: &str) -> Vec<HistoryRow> {
    // If the pattern is empty, return nothing.
    if pattern.is_empty() {
        return Vec::new();
    }

    // Split into search terms, file-pattern globs, and `@lang`
    // language flags via the shared classifier. See
    // `crate::highlight::parse_query_tokens` for the rules.
    let tokens = parse_query_tokens(pattern);

    // If there are no search terms at all (only globs and/or
    // languages), we have nothing to search for.
    if tokens.terms.is_empty() {
        return Vec::new();
    }

    // First term is the primary pattern given to ag.
    // Remaining terms are post-filtered.
    let primary = tokens.terms[0].clone();
    let post_filter = &tokens.terms[1..];

    // Build the ag command.
    let mut cmd = std::process::Command::new("ag");
    cmd.arg("--nocolor").arg("--nogroup").arg("--hidden");

    // Language flags (@rust -> --rust).
    for lang in &tokens.languages {
        cmd.arg(format!("--{}", lang));
    }

    // File-pattern filters.
    // Convert shell-style globs to regex patterns for `ag -G`.
    // `ag -G` takes a PCRE regex that matches against the full
    // file path. Examples:
    //   *.rs      -> .*\.rs$
    //   bla*.txt  -> bla.*\.txt$
    for g in &tokens.globs {
        let regex = glob_to_ag_regex(g);
        cmd.arg("-G").arg(regex);
    }

    // The search pattern.
    cmd.arg(primary);

    // Current directory.
    cmd.arg(".");

    let output = match cmd.output() {
        Ok(o) => o,
        Err(_) => return Vec::new(), // ag not on PATH, or other error.
    };

    if !output.status.success() {
        // Exit code 1 means "no matches" —
        // that's a valid, empty result.
        // Anything else (e.g. 2 = error) also
        // yields an empty result.
        return Vec::new();
    }

    let stdout = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut rows: Vec<HistoryRow> = Vec::new();
    let mut next_id: i64 = -1;

    // Use the first explicit language for bat syntax highlighting.
    // If multiple languages were specified we use only the first
    // to avoid guessing per-file; bat auto-detects from extension
    // when no language is given, but here we prefer the user's
    // explicit choice.
    let bat_lang = tokens.languages.first().map(String::as_str);
    let mut bat_count = 0usize;
    const BAT_MAX: usize = 50;

    for line in stdout.lines() {
        // Format: file:line_number:matched_content
        // (or file:line_number:column:matched_content when --column is used,
        // but we avoid --column for broader compatibility).
        let mut parts = line.splitn(3, ':');
        let file = match parts.next() {
            Some(f) if !f.is_empty() => f,
            _ => continue,
        };
        let line_num = match parts.next() {
            Some(n) => n,
            _ => continue,
        };
        let content = match parts.next() {
            Some(c) => c,
            _ => continue,
        };

        // Post-filter: every remaining search term must appear
        // in the matched line (case-insensitive).
        if !post_filter.is_empty() {
            let content_lower = content.to_lowercase();
            if !post_filter
                .iter()
                .all(|t| content_lower.contains(&t.to_lowercase()))
            {
                continue;
            }
        }

        // Build absolute path.
        let abs_path = if std::path::Path::new(file).is_absolute() {
            file.to_string()
        } else {
            cwd.join(file).to_string_lossy().into_owned()
        };

        // Read 5 lines of context around the match (2 before,
        // the match line, 2 after) so the details pane shows
        // the surrounding code — same pattern as tags mode.
        let line_number = line_num.parse::<usize>().unwrap_or(0);
        let context = read_source_context(&abs_path, line_number);

        // If a language was specified, pipe the context through
        // `bat` for syntax highlighting. We cap the number of
        // bat calls to keep the background thread responsive.
        let output = if let Some(lang) = bat_lang {
            if bat_count < BAT_MAX {
                bat_count += 1;
                highlight_with_bat(&context, lang).unwrap_or(context)
            } else {
                context
            }
        } else {
            context
        };

        let basename = std::path::Path::new(file)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.to_string());

        let source = if let Some(lang) = bat_lang {
            format!("ag:{}", lang)
        } else {
            "ag".to_string()
        };

        rows.push(HistoryRow {
            id: next_id,
            command: content.trim_start().to_string(),
            directory: abs_path,
            session_id: line_num.to_string(),
            exit_code: 0,
            timestamp: 0,
            comment: basename,
            output,
            mode: "ag".to_string(),
            source,
            ..Default::default()
        });
        next_id -= 1;

        // Cap results to keep the UI responsive.
        if rows.len() >= 1000 {
            break;
        }
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_to_regex_star_rs() {
        assert_eq!(glob_to_ag_regex("*.rs"), r".*\.rs$");
    }

    #[test]
    fn glob_to_regex_bla_star_txt() {
        assert_eq!(glob_to_ag_regex("bla*.txt"), r"bla.*\.txt$");
    }

    #[test]
    fn glob_to_regex_all_files() {
        assert_eq!(glob_to_ag_regex("*"), r".*$");
    }

    #[test]
    fn glob_to_regex_escapes_dot() {
        assert_eq!(glob_to_ag_regex("*.min.js"), r".*\.min\.js$");
    }

    #[test]
    fn glob_to_regex_escapes_plus() {
        assert_eq!(glob_to_ag_regex("file*.c++"), r"file.*\.c\+\+$");
    }

    #[test]
    fn glob_to_regex_no_star_is_literal() {
        assert_eq!(glob_to_ag_regex("Makefile"), r"Makefile$");
    }
}
