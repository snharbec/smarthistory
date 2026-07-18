//! `$` (tags) prefix mode.
//!
//! Lists every symbol defined in a universal tag file
//! (`tags`) in the current directory, filtered by the
//! typed pattern. When no `tags` file exists, falls back
//! to the local `.codegraph/codegraph.db` FTS5 index
//! (see [`crate::tui::mode::codegraph`]).
use crate::tui::App;

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
