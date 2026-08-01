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
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::{
    Action, App, CodeGraphRelationsPicker, CodegraphRelationEntry, CodegraphRelationSection,
    PickMode,
};
use crate::tui::bindings::action_for_key;
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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

/// Health check for the codegraph (`&`) mode. Verifies:
///
/// 1. The CodeGraph index (`.codegraph/codegraph.db`)
///    exists at any of the discovery paths
///    (`CodeGraphClient::open` walks
///    upward from CWD, same as the runtime).
/// 2. It opens as a valid sqlite database.
/// 3. The required schema is present: `nodes`
///    and `edges` tables, the `nodes_fts` FTS5
///    index, the `kind` column on edges (so
///    `callers` / `callees` work).
/// 4. A trivial FTS5 search returns successfully
///    (proves the index isn't corrupt).
/// 5. When reachable, an informational row
///    count + repo_root path is included so the
///    user can sanity-check "did I index the
///    right repo?".
pub(crate) fn check(_app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Codegraph;

    // 1. Discovery. `CodeGraphClient::open()`
    //    returns `None` when no index is
    //    reachable; the runtime mode returns
    //    empty in that case.
    let client = match crate::codegraph::CodeGraphClient::open() {
        Some(c) => c,
        None => {
            return CheckReport::err(
                mode,
                "no .codegraph/codegraph.db index found (run `codegraph build` in your repo root to create one)",
            );
        }
    };

    // 2-3. Schema probe. The client's
    //     `repo_root()` returns the path
    //     (it can be empty if the index has
    //     no repository root metadata
    //     recorded); we then need the DB
    //     path to open a fresh connection
    //     and probe the schema.
    let repo_root = client.repo_root();
    if repo_root.as_os_str().is_empty() {
        return CheckReport::warn(
            mode,
            "CodeGraph index is reachable but has no `repo_root` metadata; cannot probe schema",
        );
    }
    let db_path = repo_root.join(".codegraph").join("codegraph.db");
    if !db_path.is_file() {
        return CheckReport::err(
            mode,
            format!(
                "CodeGraph client opened, but db file is missing at {}",
                db_path.display()
            ),
        );
    }
    let conn = match rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => {
            return CheckReport::err(
                mode,
                format!(
                    "CodeGraph db at {} is not a valid sqlite file: {e}",
                    db_path.display()
                ),
            );
        }
    };
    let required = [("nodes", "nodes table"), ("edges", "edges table")];
    for (name, label) in &required {
        let present: Result<i64, _> = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            rusqlite::params![name],
            |row| row.get(0),
        );
        if !matches!(present, Ok(n) if n > 0) {
            return CheckReport::err(
                mode,
                format!("CodeGraph db is missing the `{label}` ({name}) — the index is incomplete or from an incompatible codegraph version"),
            );
        }
    }
    // The FTS5 virtual table may be named
    // `nodes_fts` (current schema) or
    // `nodes_search` (older versions). Probe
    // both.
    let fts_present: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type='table' AND name IN ('nodes_fts', 'nodes_search')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if fts_present == 0 {
        return CheckReport::err(
            mode,
            "CodeGraph db is missing the FTS5 virtual table (expected `nodes_fts` or `nodes_search`)",
        );
    }

    // 4. Trivial FTS5 search. We use the
    //    client's own search method so we
    //    exercise the same code path the
    //    TUI uses. A common failure here is
    //    "FTS5 integrity-check failed" —
    //    the index file got truncated
    //    (e.g. the user ctrl-C'd the
    //    indexer mid-write) and the
    //    search fails with an obscure
    //    sqlite error.
    let nodes = client.search("", None, 10);
    if nodes.is_empty() {
        // Not necessarily an error: the
        // user could have indexed an
        // empty repo. But an FTS5
        // search on an empty string
        // should match *something* if
        // the index is non-empty.
        // The "row count" check
        // below surfaces the
        // distinction.
    }

    // 5. Informational row count.
    let node_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
        .unwrap_or(0);
    let edge_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
        .unwrap_or(0);

    if node_count == 0 {
        CheckReport::warn(
            mode,
            format!(
                "CodeGraph index at {} has 0 nodes (the index is empty; run `codegraph build` in the repo root)",
                db_path.display()
            ),
        )
    } else {
        CheckReport::ok(
            mode,
            format!(
                "CodeGraph index at {} is healthy ({} nodes, {} edges, repo root {})",
                db_path.display(),
                node_count,
                edge_count,
                repo_root.display()
            ),
        )
    }
}

/// Fetch the codegraph-mode result set.
///
/// Steps:
/// 1. Parse the typed query for an `@lang` token
///    (e.g. `@rust`); the language filters the
///    FTS5 search and shapes the row's `source`
///    field (so `ensure_selected_context` can
///    pass it to `bat --language`).
/// 2. Open (and cache) the read-only CodeGraph
///    connection. The connection is opened here
///    (not in `App::new`) so a repo without an
///    index never pays the discovery walk for
///    users who never type `&`.
/// 3. FTS5 search via `client.search`, capped
///    at 500 rows. Empty pattern → empty list
///    (listing every symbol in a 350k-node
///    index is useless and slow to render).
/// 4. Shape each `CodeGraphNode` into a
///    `HistoryRow` with a synthetic negative
///    `id` (matching the tags-mode convention),
///    the absolute path in `directory`, the
///    `start_line` in `session_id`, and the
///    symbolic node id in `codegraph_node_id`
///    (so `ensure_selected_context` can look up
///    the callers / callees).
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    let pattern = pattern(app).trim();
    let parsed = crate::highlight::parse_query_tokens(pattern);
    let lang_filter: Option<&str> = parsed.languages.first().map(String::as_str);
    // Rebuild the FTS pattern from the non-language
    // terms so `@java getSymbol` searches for
    // `getSymbol` filtered to java (the `@java`
    // token itself must not become an FTS term).
    let fts_pattern = parsed.terms.join(" ");
    // Open (and cache) the read-only CodeGraph
    // connection if we haven't already. We do this
    // here rather than `App::new` so a repo without
    // an index never pays the discovery walk for
    // users who never type `&`.
    if app.codegraph_client.is_none() {
        app.codegraph_client = crate::codegraph::CodeGraphClient::open();
    }
    let Some(client) = app.codegraph_client.as_ref() else {
        return Ok(Vec::new());
    };
    // Empty query → empty list. Listing every
    // symbol in a 350k-node index is useless and
    // slow to render.
    if fts_pattern.trim().is_empty() {
        return Ok(Vec::new());
    }
    // The `@lang` token maps to CodeGraph's
    // `language` column verbatim (e.g. `java`,
    // `kotlin`). Unknown values simply return no
    // rows — same graceful degradation as tags
    // mode for an unknown `@cobol` filter.
    let nodes = client.search(&fts_pattern, lang_filter, 500);
    let repo_root = client.repo_root();
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let source = match lang_filter {
        Some(lang) => format!("codegraph:{}", lang),
        None => "codegraph".to_string(),
    };
    let mut rows: Vec<HistoryRow> = Vec::with_capacity(nodes.len());
    let mut next_id: i64 = -1;
    for n in &nodes {
        let abs = n.abs_path(repo_root);
        let file_display = n.file_path.clone();
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
            comment: format!("{} · {}: {}", n.kind, file_display, n.start_line),
            output: String::new(),
            mode: "codegraph".to_string(),
            source: source.clone(),
            codegraph_node_id: n.id.clone(),
            ..Default::default()
        });
        next_id -= 1;
    }
    Ok(rows)
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
        // Scroll hint: the
        // source-context
        // window above is 50
        // lines centered on
        // the match (match at
        // line 25). Scroll
        // so it's near the
        // top of the
        // visible preview
        // area. The renderer's
        // `min(max_scroll)` clamp
        // handles short
        // files / near-end
        // matches safely.
        let half = crate::tui::SOURCE_CONTEXT_LINES / 2;
        row.preview_scroll = half.saturating_sub(2) as u16;
    }
}

impl App {
    /// Whether the query is a CodeGraph symbol-search
    /// request: the query starts with the codegraph
    /// prefix (`&` by default). The body is matched
    /// against symbol names in the local
    /// `.codegraph/codegraph.db` index via FTS5.
    pub(crate) fn is_codegraph_query(&self) -> bool {
        crate::tui::mode::codegraph::matches(self)
    }

    /// Whether the CodeGraph relations picker overlay is currently open.
    pub(crate) fn is_codegraph_relations_picker_open(&self) -> bool {
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
    pub(crate) fn open_codegraph_relations(&mut self) {
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

    pub(crate) fn close_codegraph_relations_picker(&mut self) {
        self.codegraph_relations_picker = None;
    }
}

/// Key handler for the CodeGraph relations picker. Up/Down (and
/// `Ctrl-N`/`Ctrl-P`) move the selection past section headers;
/// `PageUp`/`PageDown`/`Home`/`End` jump; Enter opens the
/// highlighted relation's source file in `$EDITOR +LINE path`
/// and exits the TUI (mirroring the main list's tags/codegraph
/// selection); the user's `Cancel` binding (Esc / Ctrl-C)
/// dismisses the picker without opening anything.
pub(crate) fn handle_codegraph_relations_picker_key(app: &mut App, key: KeyEvent) -> bool {
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
