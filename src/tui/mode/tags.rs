//! `$` (tags) prefix mode.
//!
//! Lists every symbol defined in a universal tag file
//! (`tags`) in the current directory, filtered by the
//! typed pattern. When no `tags` file exists, falls back
//! to the local `.codegraph/codegraph.db` FTS5 index
//! (see [`crate::tui::mode::codegraph`]).
use crate::tui::state::HistoryRow;
use crate::tui::App;
use crate::tui::mode::CheckReport;
use anyhow::Result;

/// Whether the query is a tags-search request:
/// the query starts with the tags prefix (`$` by
/// default). The body is matched against the
/// symbol names AND the source-line text from the
/// `tags` file in the current directory.
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.tags;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The tags-search body, i.e. everything after the
/// leading `$` prefix. Empty string when not in
/// tags mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.tags;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Health check for the tags (`$`) mode. Verifies:
///
/// 1. A `tags` / `TAGS` file is reachable from
///    the current working directory (we walk
///    upward, mirroring how editors discover
///    tag files).
/// 2. When found, the file is readable and
///    parses without error (we read the first
///    few entries to confirm the format).
/// 3. When NOT found, check the CodeGraph
///    fallback: if `.codegraph/codegraph.db`
///    exists and is a valid FTS5 sqlite
///    database, the fallback will work; if
///    neither is available, the mode can't
///    list any symbols (Error).
///
/// The dig-down order is: tags-file presence →
/// tags-file readability → tags-file parse
/// sanity → CodeGraph fallback (only when no
/// tags file).
#[allow(unused_variables)] // `app` kept for symmetry with the other `check` signatures; not used here because the tags check is CWD-relative.
pub(crate) fn check(app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Tags;
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    // 1. Walk upward from CWD looking for a
    //    `tags` / `TAGS` file. The
    //    `find_tags_file` helper does the
    //    walking.
    let tags_path = crate::tui::find_tags_file();
    if tags_path.is_file() {
        // Tags file found. Verify readability
        // and parse sanity.
        let read_result = std::fs::read_to_string(&tags_path);
        let (read_ok, section_count) = match read_result {
            Ok(s) => {
                // Parse sanity: count
                // `\x0c` section
                // separators (a
                // universal-tags file
                // has one per file).
                // A file with zero
                // separators is
                // suspicious (empty
                // file, or not a
                // universal-tags file
                // — ctags has other
                // output formats).
                (true, s.matches('\x0c').count())
            }
            Err(_e) => (false, 0_usize),
        };
        if !read_ok {
            // `read_ok` is false precisely
            // because `read_result` is `Err`;
            // pattern-match to recover the
            // error.
            let err_msg = std::fs::read_to_string(&tags_path)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown error".to_string());
            return CheckReport::err(
                mode,
                format!("tags file found at {} but is unreadable: {}", tags_path.display(), err_msg),
            );
        }
        if section_count == 0 {
            return CheckReport::warn(
                mode,
                format!("tags file at {} has 0 entries (or is in a non-universal-tags format)", tags_path.display()),
            );
        }
        return CheckReport::ok(
            mode,
            format!("tags file at {} parsed ({} sections, {} bytes)", tags_path.display(), section_count, std::fs::metadata(&tags_path).map(|m| m.len()).unwrap_or(0)),
        );
    }

    // 2. No tags file: check the CodeGraph
    //    fallback. The fallback is what the
    //    `mode::tags::fetch` function uses when
    //    no TAGS file is reachable; if it's
    //    also unavailable, the mode returns an
    //    empty list.
    match crate::codegraph::find_codegraph_db() {
        Some(codegraph_path) if codegraph_path.is_file() => {
            // Probe the DB: must be a valid
            // sqlite, must have a `nodes`
            // table.
            let conn = match rusqlite::Connection::open_with_flags(
                &codegraph_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            ) {
                Ok(c) => c,
                Err(e) => {
                    return CheckReport::err(
                        mode,
                        format!(
                            "no tags file found, and CodeGraph fallback at {} is not a valid sqlite DB: {e}",
                            codegraph_path.display()
                        ),
                    );
                }
            };
            let present: Result<i64, _> = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='nodes'",
                [],
                |row| row.get(0),
            );
            match present {
                Ok(n) if n > 0 => CheckReport::ok(
                    mode,
                    format!(
                        "no tags file in {} or any parent; CodeGraph fallback at {} is available",
                        cwd.display(),
                        codegraph_path.display()
                    ),
                ),
                Ok(_) => CheckReport::err(
                    mode,
                    format!(
                        "no tags file found, and CodeGraph fallback at {} has no `nodes` table (incomplete index)",
                        codegraph_path.display()
                    ),
                ),
                Err(e) => CheckReport::err(
                    mode,
                    format!(
                        "no tags file found, and CodeGraph fallback at {} could not be probed: {e}",
                        codegraph_path.display()
                    ),
                ),
            }
        }
        _ => CheckReport::err(
            mode,
            format!(
                "no tags file in {} or any parent, and no CodeGraph index at .codegraph/codegraph.db; the `$` mode has no symbol source to query",
                cwd.display()
            ),
        ),
    }
}

/// Fetch the tags-mode result set.
///
/// Steps:
/// 1. Search for a `tags` file in the current
///    directory, then walk upward through parent
///    directories (mirroring how vim / nvim discover
///    tag files). The first `tags` found is used.
/// 2. If no tag file exists anywhere up the tree,
///    fall back to the CodeGraph index via
///    `App::fetch_tags_via_codegraph`. The fallback
///    rows are tagged with `mode: "tags"` so the
///    existing tags dispatch (open at line, `@lang`
///    filter, `ensure_selected_context`) work
///    unchanged.
/// 3. Parse the TAGS file: section header
///    (`<filename>,<line_count>`) on the line after
///    a `\x0c` separator, then one symbol line per
///    entry (`<display><line>,<offset>`). Working
///    backward from the end splits the line into
///    `display` / `line_number` / `byte_offset`.
/// 4. Apply the `@lang` extension filter (if any)
///    and the token filter (AND-combined
///    whitespace-separated tokens, case-sensitive
///    when the body has any uppercase).
/// 5. Build `HistoryRow`s with a synthetic negative
///    `id` (matching the codegraph-mode convention),
///    the absolute path in `directory`, the line
///    number in `session_id`, and the source file's
///    basename in `comment`. The source context is
///    loaded lazily on selection by
///    `ensure_selected_context`.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    // Search for a `tags` file in the current
    // directory, then walk upward through parent
    // directories until one is found (or we hit the
    // filesystem root). This mirrors how editors
    // like vim/nvim discover tag files: the first
    // `tags` file found (closest to the cwd) is the
    // one that's used. The file paths inside the tag
    // file are relative to the directory containing
    // the tag file, so we resolve them against that
    // directory (not the cwd) to produce correct
    // absolute paths.
    //
    // If no tag file is found anywhere up the tree,
    // fall back to the CodeGraph index
    // (`.codegraph/codegraph.db`) when one exists.
    // The fallback rows are tagged with `mode:
    // "tags"` so the existing tags dispatch (open at
    // line, `@lang` filter,
    // `ensure_selected_context`) all work
    // unchanged — the user gets symbol navigation
    // via CodeGraph data with the `$` UX they
    // already know.
    let tags_path = crate::tui::find_tags_file();
    if !tags_path.is_file() {
        return fetch_via_codegraph(app);
    }
    let tags_dir = tags_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let contents = match std::fs::read_to_string(&tags_path) {
        Ok(s) => s,
        Err(_) => return Ok(Vec::new()),
    };
    let pattern = pattern(app).trim();
    let case_sensitive = app.is_case_sensitive();

    // Use the shared classifier so `$`, `,` (ag),
    // and any future content modes share one set of
    // rules. The first `@lang` token (if any) drives
    // both the extension filter and the bat
    // syntax-highlight; later `@lang` tokens are
    // accepted by the parser but ignored here
    // (consistent with ag mode using the first one).
    let parsed = crate::highlight::parse_query_tokens(pattern);
    let lang_filter: Option<&str> = parsed.languages.first().map(String::as_str);
    // Build the extension set for the language
    // filter (if any). An unknown language — e.g.
    // `@cobol` — yields `None`, in which case we
    // still apply the bat highlighting (bat will
    // fall back to its own extension-based
    // detection, gracefully degrading), but we skip
    // the extension filter so the user sees a
    // (possibly empty) result rather than a silent
    // zero.
    let allowed_exts: Option<Vec<String>> = lang_filter
        .and_then(crate::highlight::extensions_for_language)
        .map(|exts| exts.iter().map(|s| s.to_string()).collect());

    let tokens: Vec<String> = parsed
        .terms
        .into_iter()
        .map(|t| if case_sensitive { t } else { t.to_lowercase() })
        .collect();
    let mut rows: Vec<HistoryRow> = Vec::new();
    let mut next_id: i64 = -1;
    let mut current_file: String = String::new();
    let mut in_section = false;
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for line in contents.lines() {
        if line == "\x0c" || line.is_empty() {
            in_section = false;
            continue;
        }
        if !in_section {
            // Section header: <filename>,<line_count>
            if let Some(idx) = line.find(',') {
                current_file = line[..idx].to_string();
            } else {
                current_file = line.to_string();
            }
            in_section = true;
            continue;
        }
        // Symbol line: <display><line>,<offset>
        // Parse from the end: last comma → offset,
        // digits before it → line, rest → display.
        let Some(comma_idx) = line.rfind(',') else {
            continue;
        };
        let offset_str = &line[comma_idx + 1..];
        if offset_str.is_empty() || !offset_str.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let rest = &line[..comma_idx];
        // Walk back from the end of `rest` to find
        // where the line-number digits start.
        let mut i = rest.len();
        while i > 0 && rest.as_bytes()[i - 1].is_ascii_digit() {
            i -= 1;
        }
        if i == rest.len() {
            // No digits found — malformed line.
            continue;
        }
        let line_number = &rest[i..];
        let display = &rest[..i];
        if display.is_empty() || line_number.is_empty() {
            continue;
        }
        // Build the absolute file path. File paths
        // in the tag file are relative to the
        // directory containing the tag file (not
        // necessarily the cwd), so we resolve
        // against `tags_dir`.
        let filepath = if std::path::Path::new(&current_file).is_absolute() {
            current_file.clone()
        } else {
            tags_dir.join(&current_file).to_string_lossy().into_owned()
        };
        // Apply the `@lang` extension filter, if
        // any. Extension comparison is
        // case-insensitive (extensions are
        // lowercased before lookup, so `.RS` matches
        // `@rust` the same as `.rs`).
        if let Some(ref exts) = allowed_exts {
            let file_ext = std::path::Path::new(&current_file)
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase());
            let ext_ok = file_ext
                .as_deref()
                .map(|e| exts.iter().any(|a| a == e))
                .unwrap_or(false);
            if !ext_ok {
                continue;
            }
        }
        // Apply the token filter.
        if !tokens.is_empty() {
            let (display_check, file_check) = if case_sensitive {
                (display.to_string(), current_file.clone())
            } else {
                (display.to_lowercase(), current_file.to_lowercase())
            };
            let all_match = tokens.iter().all(|tok| {
                display_check.contains(tok) || file_check.contains(tok)
            });
            if !all_match {
                continue;
            }
        }
        // Source context is loaded lazily when the
        // row is selected rather than read eagerly
        // here. A large TAGS file can reference many
        // symbols in the same source files, and
        // reading every source file once per symbol
        // made opening tags mode take tens of
        // seconds on large repositories. The
        // selected row's `output` is populated on
        // demand by `ensure_selected_context` using
        // the `tags_source_cache`.
        // Tag the row's `source` with the language
        // so the chips / source label match the
        // ag-mode convention (`"tags"` when no
        // language, `"tags:<lang>"` when one was
        // supplied). This also lets the status bar
        // surface the active language in a future
        // iteration.
        let source = match lang_filter {
            Some(lang) => format!("tags:{}", lang),
            None => "tags".to_string(),
        };
        rows.push(HistoryRow {
            id: next_id,
            command: display.to_string(),
            directory: filepath,
            session_id: line_number.to_string(),
            exit_code: 0,
            timestamp: now_epoch,
            comment: std::path::Path::new(&current_file)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            output: String::new(),
            mode: "tags".to_string(),
            source,
            ..Default::default()
        });
        next_id -= 1;
    }
    Ok(rows)
}

/// `$` (tags) fallback when no `tags` / `TAGS` file
/// exists. Queries the CodeGraph index instead and
/// returns rows tagged with `mode: "tags"` so the
/// existing tags dispatch (open at line, `@lang`
/// filter, `ensure_selected_context`) work
/// unchanged. The CodeGraph node id is stashed in
/// `codegraph_node_id` so `ensure_selected_context`
/// can append the callers/callees overlay for these
/// rows too.
fn fetch_via_codegraph(app: &mut App) -> Result<Vec<HistoryRow>> {
    let pattern = pattern(app).trim();
    let parsed = crate::highlight::parse_query_tokens(pattern);
    let lang_filter: Option<&str> = parsed.languages.first().map(String::as_str);
    let fts_pattern = parsed.terms.join(" ");
    if app.codegraph_client.is_none() {
        app.codegraph_client = crate::codegraph::CodeGraphClient::open();
    }
    let Some(client) = app.codegraph_client.as_ref() else {
        return Ok(Vec::new());
    };
    // Empty query with no tag file: list nothing.
    // The user can type a symbol fragment to start
    // the FTS search.
    if fts_pattern.trim().is_empty() {
        return Ok(Vec::new());
    }
    let nodes = client.search(&fts_pattern, lang_filter, 500);
    let repo_root = client.repo_root();
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let source = match lang_filter {
        Some(lang) => format!("tags:{}", lang),
        None => "tags".to_string(),
    };
    let mut rows: Vec<HistoryRow> = Vec::with_capacity(nodes.len());
    let mut next_id: i64 = -1;
    for n in &nodes {
        let abs = n.abs_path(repo_root);
        rows.push(HistoryRow {
            id: next_id,
            command: if n.qualified_name.is_empty() {
                n.name.clone()
            } else {
                n.qualified_name.clone()
            },
            directory: abs.to_string_lossy().into_owned(),
            session_id: n.start_line.to_string(),
            exit_code: 0,
            timestamp: now_epoch,
            comment: format!("{} · {}: {}", n.kind, n.file_path, n.start_line),
            output: String::new(),
            mode: "tags".to_string(),
            source: source.clone(),
            codegraph_node_id: n.id.clone(),
            ..Default::default()
        });
        next_id -= 1;
    }
    Ok(rows)
}

/// Lazy-load the source-context preview for the
/// currently-selected `$`-mode row. Reads the
/// 50-line window around the symbol's `start_line`
/// from disk (cached in `App::tags_source_cache`),
/// appends the callers / callees overlay if the
/// row came from the CodeGraph fallback
/// (`fetch_tags_via_codegraph`), and pipes the
/// result through `bat` with the active theme's
/// `--theme=light` / `--theme=dark` flag. See the
/// original `App::ensure_selected_tag_context`
/// doc for the full rationale on the cap.
pub(crate) fn ensure_selected_context(app: &mut App) {
    if !matches(app) {
        return;
    }
    let Some(idx) = app.list_state.selected() else {
        return;
    };
    let row_ref = match app.merged_rows.get(idx) {
        Some(r) => r,
        None => return,
    };
    if row_ref.mode != "tags" || !row_ref.output.is_empty() {
        return;
    }
    let line_number = row_ref.session_id.parse::<usize>().unwrap_or(0);
    let filepath = row_ref.directory.clone();
    let lang = crate::highlight::parse_query_tokens(app.tags_pattern().trim())
        .languages
        .first()
        .cloned();
    let context =
        crate::tui::read_source_context_with_cache(&filepath, line_number, &mut app.tags_source_cache);
    // When this tags row came from the CodeGraph
    // fallback (`fetch_tags_via_codegraph`), the
    // symbolic node id is stashed in
    // `codegraph_node_id`. Append the callers /
    // callees overlay so the `$` fallback gets the
    // same rich context the dedicated `&` mode shows.
    let mut context = context;
    let node_id = row_ref.codegraph_node_id.clone();
    if !node_id.is_empty()
        && let Some(client) = app.codegraph_client.as_ref()
    {
        let callers = client.callers(&node_id, 15);
        let callees = client.callees(&node_id, 15);
        if !callers.is_empty() || !callees.is_empty() {
            if !context.is_empty() {
                context.push('\n');
            }
            context.push_str("── callers ──\n");
            for c in &callers {
                context.push_str(&format!(
                    "  {}  @{}:{}\n",
                    c.qualified_name, c.file_path, c.start_line
                ));
            }
            context.push_str("── callees ──\n");
            for c in &callees {
                context.push_str(&format!(
                    "  {}  @{}:{}\n",
                    c.qualified_name, c.file_path, c.start_line
                ));
            }
        }
    }
    if let Some(row) = app.merged_rows.get_mut(idx) {
        row.output = if let Some(lang) = lang {
            crate::highlight::highlight_with_bat(&context, &lang).unwrap_or(context)
        } else {
            // No explicit `@lang`: let `bat` auto-detect
            // from the source file's extension via
            // `--file-name`.
            crate::highlight::highlight_with_bat_auto(&context, &filepath).unwrap_or(context)
        };
    }
}
