//! `&` (CodeGraph symbol search) prefix mode.
//!
//! Searches the local `.codegraph/codegraph.db` index
//! by symbol name (FTS5) and lists matching
//! functions / methods / classes. The selected row's
//! details pane shows the source context plus the
//! symbol's callers and callees (edges with
//! `kind='calls'`). Selecting a row opens the file in
//! `$EDITOR` at `start_line`. When no `.codegraph/`
//! index exists the `$` (tags) mode falls back to this
//! index, so a repo without a `TAGS` file still has
//! symbol navigation as long as CodeGraph has indexed
//! it.
use crate::tui::App;
/// Whether the query is a CodeGraph symbol-search
/// request: the query starts with the codegraph
/// prefix (`&` by default). The body is matched
/// against symbol names in the local
/// `.codegraph/codegraph.db` index via FTS5.
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.codegraph;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The codegraph-search body, i.e. everything after
/// the leading `&` prefix. Empty string when not in
/// codegraph mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.codegraph;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Lazy-load the source-context preview for the
/// currently-selected codegraph row. Reads the
/// 50-line window around the symbol's `start_line`
/// from disk (cached in `App::tags_source_cache`
/// so multiple symbols in the same file share one
/// disk read), appends the callers / callees
/// overlay (each capped at 15 entries), and pipes
/// the result through `bat` with the
/// active theme's `--theme=light` / `--theme=dark`
/// flag. See the original
/// `App::ensure_selected_codegraph_context` doc
/// for the full rationale on the cap.
pub(crate) fn ensure_selected_context(app: &mut App) {
    if !matches(app) {
        return;
    }
    let Some(idx) = app.list_state.selected() else {
        return;
    };
    let (node_id, filepath, line_str, language) = match app.merged_rows.get(idx) {
        Some(r) if r.mode == "codegraph" && r.output.is_empty() => (
            r.codegraph_node_id.clone(),
            r.directory.clone(),
            r.session_id.clone(),
            r.source.strip_prefix("codegraph:").map(|s| s.to_string()),
        ),
        _ => return,
    };
    let line_number = line_str.parse::<usize>().unwrap_or(0);
    let mut context = crate::tui::read_source_context_with_cache(
        &filepath,
        line_number,
        &mut app.tags_source_cache,
    );
    // Append the callers / callees overlay. Each is
    // capped so a hub symbol with thousands of callers
    // doesn't blow up the details pane; the remaining
    // count is shown so the user knows the list was
    // truncated.
    if let Some(client) = app.codegraph_client.as_ref() {
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
        row.output = if let Some(lang) = language {
            crate::highlight::highlight_with_bat(&context, &lang).unwrap_or(context)
        } else {
            // No explicit `@lang`: let `bat` auto-detect
            // from the source file's extension via
            // `--file-name`.
            crate::highlight::highlight_with_bat_auto(&context, &filepath).unwrap_or(context)
        };
    }
}
