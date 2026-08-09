//! `/` (files) prefix mode.
//!
//! Lists every file in the current directory and
//! subdirectories, filtered by the typed pattern.
//! Selecting a row opens the file in `$EDITOR` (or the
//! configured per-extension command, via the SmartOpen
use crate::tui::mode::CheckReport;
/// key `Ctrl-]`).
use crate::tui::state::HistoryRow;
use crate::tui::App;
use anyhow::Result;

/// Whether the query is a files-view request:
/// the query starts with the files prefix (`/` by
/// default). The body (everything after `/`) is a
/// substring filter matched against each file's
/// path (relative to cwd).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.files;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// Health check for the files (`/`) mode. The
/// files mode has no external dependencies — it
/// just walks the local filesystem — so the
/// check verifies:
///
/// 1. The current working directory exists and
///    is readable.
/// 2. `walk_dir` returns at least one entry
///    (or the user is in a deliberately empty
///    directory, which is a `Warning`).
/// 3. The `files.ignore` config combines
///    with the built-in `DEFAULT_IGNORES`
///    without error.
///
/// The walk uses a real pattern (`*` /
///    everything) to exercise the same code
///    path the TUI uses.
pub(crate) fn check(_app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Files;

    // 1. CWD sanity.
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            return CheckReport::err(
                mode,
                format!("current working directory is unavailable: {e}"),
            );
        }
    };
    if !cwd.is_dir() {
        return CheckReport::err(mode, format!("cwd is not a directory: {}", cwd.display()));
    }

    // 2. Build the ignore set the same way the
    //    runtime does. We use the built-in
    //    `DEFAULT_IGNORES` plus any user
    //    additions from config; for the check
    //    we don't have an `App` context, so we
    //    just use the default set.
    let ignore = crate::files::IgnoreSet::new(&[]);

    // 3. Walk. The probe just runs `walk_dir` directly (unfiltered,
    //    same as the runtime's one-shot walk) rather than exercising
    //    the debounce/background-thread plumbing.
    let mut rows: Vec<crate::tui::state::HistoryRow> = Vec::new();
    let mut next_id: i64 = -1;
    crate::files::walk_dir(&cwd, &cwd, &ignore, &mut next_id, &mut rows);

    if rows.is_empty() {
        CheckReport::warn(
            mode,
            format!("walk_dir() returned 0 entries in {} (the directory is empty or every file is in the ignore list)", cwd.display()),
        )
    } else {
        CheckReport::ok(
            mode,
            format!(
                "walk_dir() returned {} entries in {} (showing up to 10)",
                rows.len().min(10),
                cwd.display()
            ),
        )
    }
}

/// The files-view body, i.e. everything after the
/// leading `/` prefix. Empty string when not in
/// files mode.
#[allow(dead_code)] // convention API; `App::files_pattern` delegates here
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.files;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Fetch the files-mode result set. The walk runs
/// on a background thread, exactly once per session (spawned by
/// `App::files_touch` → `crate::files::spawn_walk`; see the
/// module-level doc comment on `crate::files`). This function does
/// the actual per-keystroke work: it derives a filter (glob or
/// substring, same detection `App::spawn_files_walk` used to do
/// before every walk — see there for why glob detection applies
/// regardless of `file_picker_lock`) from the CURRENT query and
/// applies it in memory, via `crate::files::filter_rows`, against
/// the cached tree — no filesystem access. Plain `/` mode (and a
/// locked `--glob-complete` file picker) keeps only file rows —
/// directories are reachable via the directories (`#`) mode if the
/// user wants directory-level navigation, and showing them here
/// would clutter the list with rows that have no preview content. A
/// locked `--glob-complete-dir` DIRECTORY picker (see
/// `FilePickerKind::Directories`) inverts that: `walk_dir` already
/// tags every entry `mode == "file"` or `mode == "directory"`, so no
/// change to the walker itself is needed — only which of those two
/// kinds this function keeps.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    let want_dirs = app.is_directory_picker();
    let pattern = crate::files::FilesState::current_pattern(&app.query, app.query_prefixes.files);
    let mut words = pattern.split_whitespace();
    let first_word = words.next().unwrap_or("");
    let is_glob = first_word.contains(['*', '?', '[']);
    // Mirrors `App::spawn_files_walk`'s old per-walk filter
    // resolution, just run in memory on every keystroke instead of
    // gating a filesystem walk — see the module-level doc comment
    // on `crate::files` for why this moved here.
    let (root_suffix, spec) = if is_glob {
        let extra_tokens: Vec<String> = words.map(|w| w.to_lowercase()).collect();
        let (root_suffix, glob_pattern) = crate::files::split_glob_root(first_word);
        let glob_pattern = if glob_pattern.is_empty() { "*".to_string() } else { glob_pattern };
        match crate::files::glob_to_regex(&glob_pattern) {
            Ok(basename) => (root_suffix, crate::files::FilesFilterSpec::Glob { basename, extra_tokens }),
            Err(e) => {
                app.set_status_message(format!("invalid glob pattern {glob_pattern:?}: {e}"));
                return Ok(Vec::new());
            }
        }
    } else {
        let tokens: Vec<String> =
            pattern.split_whitespace().map(|w| w.to_lowercase()).collect();
        (String::new(), crate::files::FilesFilterSpec::Substring(tokens))
    };
    let Some(all_rows) = app.files_state.all_rows.as_ref() else {
        return Ok(Vec::new());
    };
    let filter = match &spec {
        crate::files::FilesFilterSpec::Substring(tokens) => crate::files::FilesFilter::Substring(tokens),
        crate::files::FilesFilterSpec::Glob { basename, extra_tokens } => {
            crate::files::FilesFilter::Glob { basename, extra_tokens }
        }
    };
    let mut rows = crate::files::filter_rows(all_rows, &root_suffix, &filter);
    rows.retain(|r| (r.mode == "directory") == want_dirs);
    crate::files::sort_rows_newest_modified_first(&mut rows);
    rows.truncate(1000);
    Ok(rows)
}

/// Lazy-load context for the currently-selected row into `output`
/// for preview in the output preview pane. Called from
/// `App::refresh()` on every selection change. A **file** row (the
/// absolute path is in `directory`, set during `walk_dir`) gets its
/// first 50 lines piped through `bat` for syntax highlighting (same
/// as tags / codegraph / notes / todo modes). A **directory** row —
/// only ever present in `merged_rows` for a locked
/// `--glob-complete-dir` picker (plain `/` mode's `fetch` filters
/// directories out entirely) — gets a plain listing of its immediate
/// children instead, via `list_directory_preview`, so `cd
/// proj*<TAB>` lets you see what's actually in a candidate directory
/// before committing to it. Both bail out (return without touching
/// `row.output`) once already populated for the current row — a
/// fresh row from a new `walk_dir` result always starts with
/// `output: String::new()`, so this is a correct "already loaded,
/// don't redo the I/O every refresh tick" guard, not a stale-cache
/// risk.
pub(crate) fn ensure_selected_context(app: &mut App) {
    if !matches(app) {
        return;
    }
    let Some(idx) = app.list_state.selected() else {
        return;
    };

    let (kind, target_path) = match app.merged_rows.get(idx) {
        Some(r) if r.mode == "file" => ("file", r.directory.clone()),
        Some(r) if r.mode == "directory" => ("directory", r.directory.clone()),
        _ => return,
    };
    if target_path.is_empty() {
        return;
    }
    if app
        .merged_rows
        .get(idx)
        .is_some_and(|r| !r.output.is_empty())
    {
        return; // already loaded for this row
    }

    let output = if kind == "directory" {
        let path = std::path::PathBuf::from(&target_path);
        if !path.is_dir() {
            return;
        }
        match list_directory_preview(&path) {
            Some(listing) => listing,
            None => return,
        }
    } else {
        let path = std::path::PathBuf::from(&target_path);
        if !path.is_file() {
            return;
        }
        // Read from the shared cache so files that appear in
        // tags / codegraph results aren't re-read.
        let content = {
            let cache: &mut std::collections::HashMap<std::path::PathBuf, String> =
                &mut app.tags_source_cache;
            if !cache.contains_key(&path) {
                match std::fs::read_to_string(&path) {
                    Ok(s) => {
                        cache.insert(path.clone(), s);
                    }
                    Err(_) => return,
                }
            }
            cache.get(&path).cloned().unwrap_or_default()
        };
        if content.is_empty() {
            return;
        }
        let preview: String = content
            .lines()
            .take(crate::tui::SOURCE_CONTEXT_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        crate::highlight::highlight_with_bat_auto(&preview, &target_path).unwrap_or(preview)
    };

    if let Some(row) = app.merged_rows.get_mut(idx)
        && row.output != output {
            row.output = output;
        }
}

/// List the immediate (non-recursive) children of `path`, one name
/// per line, directories suffixed with `/` (the familiar `ls -F`
/// convention) so the two kinds are visually distinguishable at a
/// glance. Hidden entries (leading `.`) are skipped, matching
/// `walk_dir`'s own hidden-entry convention. Sorted case-
/// insensitively, directories first (mirroring how a candidate `cd`
/// target's own subdirectories are usually the more relevant thing
/// to see at a glance), capped at `SOURCE_CONTEXT_LINES` entries so
/// a huge directory doesn't blow out the preview pane. Returns
/// `None` on a read error (permission denied, race with a delete,
/// etc.) or an empty directory — `ensure_selected_context` leaves
/// `row.output` untouched in that case, same fail-soft convention
/// the file-content path already uses for an unreadable/empty file.
fn list_directory_preview(path: &std::path::Path) -> Option<String> {
    let entries = std::fs::read_dir(path).ok()?;
    let mut names: Vec<(bool, String)> = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        if file_name.as_encoded_bytes().first() == Some(&b'.') {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        names.push((is_dir, file_name.to_string_lossy().into_owned()));
    }
    if names.is_empty() {
        return None;
    }
    names.sort_by(|a, b| {
        b.0.cmp(&a.0) // directories (true) before files (false)
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });
    let listing = names
        .into_iter()
        .take(crate::tui::SOURCE_CONTEXT_LINES)
        .map(|(is_dir, name)| if is_dir { format!("{name}/") } else { name })
        .collect::<Vec<_>>()
        .join("\n");
    Some(listing)
}

impl App {
    /// Whether the query is a files-view request:
    /// the query starts with the files prefix (`/` by
    /// default). The body (everything after `/`) is a
    /// substring filter matched against each file's
    /// path (relative to cwd).
    pub(crate) fn is_files_query(&self) -> bool {
        matches(self)
    }

    /// Spawn the one-shot background directory walk if we're in
    /// files mode (or a locked file/directory picker) and haven't
    /// walked yet this session. Called from `llm_touch` on every
    /// keystroke (same co-location pattern as `jira_touch`) AND
    /// from the run loop's idle tick (`files_maybe_autocall`, a
    /// thin alias kept for symmetry with the other modes'
    /// `*_maybe_autocall` idle-tick hooks) — both are safe to call
    /// repeatedly: `all_rows.is_some() || in_flight` makes every
    /// call after the first a no-op, so there's no debounce to
    /// arm/race here (see the module-level doc comment on
    /// `crate::files` for why the walk itself no longer needs one).
    pub(crate) fn files_touch(&mut self) {
        if !self.is_files_query() {
            return;
        }
        if self.files_state.all_rows.is_some() || self.files_state.in_flight {
            return;
        }
        self.spawn_files_walk();
    }

    /// Idle-tick backstop for `files_touch` — see its doc comment.
    /// Kept as a separate name so the run loop's per-mode
    /// `*_maybe_autocall()` dispatch list (`llm_maybe_autocall`,
    /// `jira_maybe_autocall`, etc.) stays uniform even though files
    /// mode no longer has a real debounce to drive.
    pub(crate) fn files_maybe_autocall(&mut self) {
        self.files_touch();
    }

    /// Spawn a background thread that walks the session's files-mode
    /// root ONCE (see `crate::files::spawn_walk` / the module-level
    /// doc comment on `crate::files`) and sends the result back over
    /// an mpsc channel. The run loop polls the receiver and calls
    /// `process_files_result` when the result arrives.
    pub(crate) fn spawn_files_walk(&mut self) {
        let ignore = crate::files::IgnoreSet::new(&self.files_ignores);
        // The walk root: `file_picker_lock.base_root` for a locked
        // `--glob-complete[-dir]` session, else `files_root`
        // (defaulting to `current_dir()`, overridable via
        // `smarthistory tui --root`). Fixed for the whole session —
        // unlike the old per-keystroke design, nothing here depends
        // on the typed pattern, since the walk is pattern-agnostic
        // (filtering happens per-keystroke in `fetch`, not here).
        let base_root = self
            .file_picker_lock
            .as_ref()
            .map(|l| l.base_root.clone())
            .unwrap_or_else(|| self.files_root.clone());
        let request = crate::files::spawn_walk(base_root, ignore);
        self.files_state.in_flight = true;
        self.files_state.request = Some(request);
        self.set_status_message("Searching files…".to_string());
    }

    /// Process the one-shot files-mode walk result that arrived from
    /// the background thread. Caches the full (unfiltered) tree in
    /// `self.files_state.all_rows` and refreshes the list — every
    /// keystroke after this filters the cached tree in memory (see
    /// `fetch`) instead of re-walking.
    pub(crate) fn process_files_result(
        &mut self,
        _request: crate::files::FilesRequest,
        rows: Vec<HistoryRow>,
    ) {
        self.files_state.in_flight = false;
        self.files_state.request = None;
        self.files_state.all_rows = Some(rows);
        self.refresh();
    }
}
