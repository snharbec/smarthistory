//! `)` (pass password manager) prefix mode.
//!
//! Lists every entry in the local `pass` password store (the
//! standard Unix password manager). Entries are discovered by
//! walking `$PASSWORD_STORE_DIR` (defaults to `~/.password-store`)
//! for `*.gpg` files and stripping the directory prefix and
//! `.gpg` extension to derive the entry name. The list is filtered
//! by the typed body (case-insensitive AND-match across space-
//! separated tokens, same contract as every other mode).
//!
//! Selecting a row stages `pass show --clip <entry>` as the shell
//! command. The parent shell runs it; `pass` copies the first line
//! (the password) to the clipboard and clears it after 45 seconds.
//! No GPG passphrase handling is needed here — `pass` does it itself.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

/// Resolve the password store root: `$PASSWORD_STORE_DIR` if set,
/// otherwise `~/.password-store`.
fn password_store_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("PASSWORD_STORE_DIR") {
        std::path::PathBuf::from(dir)
    } else {
        let home = std::env::var("HOME").unwrap_or_default();
        std::path::PathBuf::from(home).join(".password-store")
    }
}

/// True if the user typed the `pass` prefix (default `)`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.pass;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The pass-mode body, i.e. everything after the leading `)` prefix.
/// Empty when not in pass mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.pass;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Health check for the pass (`)`) mode: verifies the password store
/// directory exists and reports how many entries it contains.
pub(crate) fn check(_app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Pass;

    let store = password_store_dir();
    if !store.is_dir() {
        return CheckReport::err(
            mode,
            format!(
                "password store not found at {} (set $PASSWORD_STORE_DIR or run `pass init`)",
                store.display()
            ),
        );
    }

    let entries = list_entries(&store);
    if entries.is_empty() {
        CheckReport::warn(
            mode,
            format!("password store at {} is empty", store.display()),
        )
    } else {
        CheckReport::ok(
            mode,
            format!(
                "password store at {} has {} entries",
                store.display(),
                entries.len()
            ),
        )
    }
}

/// Walk the password store and return every entry name (the relative
/// path without the `.gpg` extension), sorted alphabetically.
/// Dot-files / dot-directories (`.git`, `.gpg-id`, etc.) are skipped.
fn list_entries(store: &std::path::Path) -> Vec<String> {
    let mut entries = Vec::new();
    walk(store, store, &mut entries);
    entries.sort();
    entries
}

fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            if let Some(name) = rel.to_str() {
                if let Some(entry_name) = name.strip_suffix(".gpg") {
                    out.push(entry_name.to_string());
                }
            }
        }
    }
}

/// List every entry in the pass store, filtered by the typed body
/// (space-separated AND-match, case-insensitive).
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    let filter = app.pass_pattern().trim().to_string();
    let filter_tokens: Vec<String> = filter
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .collect();
    let store = password_store_dir();
    let entries = list_entries(&store);
    Ok(build_rows(entries, &filter_tokens))
}

/// Pure row-building step, separated from `fetch` so tests can
/// exercise it with synthetic entry lists without touching the
/// real password store.
pub(crate) fn build_rows(entries: Vec<String>, filter_tokens: &[String]) -> Vec<HistoryRow> {
    let total = entries.len() as i64;
    entries
        .into_iter()
        .enumerate()
        .filter(|(_, entry)| {
            let lower = entry.to_lowercase();
            filter_tokens.is_empty() || filter_tokens.iter().all(|tok| lower.contains(tok.as_str()))
        })
        .map(|(idx, entry)| HistoryRow {
            id: -(idx as i64) - 1,
            command: entry.clone(),
            directory: entry,
            session_id: String::new(),
            exit_code: 0,
            timestamp: total - idx as i64,
            comment: String::new(),
            output: String::new(),
            mode: "pass".to_string(),
            source: "pass".to_string(),
            ..Default::default()
        })
        .collect()
}

impl App {
    /// True if the user typed the `pass` prefix (default `)`).
    pub(crate) fn is_pass_query(&self) -> bool {
        matches(self)
    }

    /// The pass-mode body, i.e. everything after the leading `)` prefix.
    /// Empty when not in pass mode.
    pub(crate) fn pass_pattern(&self) -> &str {
        pattern(self)
    }
}
