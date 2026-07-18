//! `*` (session panes) prefix mode.
//!
//! Lists every pane in the *current* multiplexer context
//! (tmux session or herdr workspace), excluding the pane
//! the TUI is running in (read from `$TMUX_PANE`).
//! Selecting a row stages a `select-pane` / `switch-client`
//! command (or the herdr equivalent) and exits the TUI.
use crate::tui::state::{HistoryRow, MatchAlgorithm, PanesFilter};
use crate::tui::App;
use anyhow::Result;

/// Whether the query is a session-panes request:
/// the query starts with the panes prefix (`*` by
/// default). The body (everything after `*`) is a
/// substring filter matched against each pane's
/// current command and cwd.
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.panes;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The session-panes filter body, i.e. everything
/// after the leading `*` prefix. Empty when not in
/// panes mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.panes;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Fetch the panes-mode result set.
///
/// Steps:
/// 1. `App::fetch_session_panes` (called first
///    to refresh the multiplexer snapshot) — this
///    populates `app.session_panes` with the
///    linearised tree of workspaces + panes + the
///    hosts / sessions sections. The actual
///    multiplexer work
///    (`MultiplexerBackend::snapshot_current_panes`)
///    lives in `crate::multiplexer`.
/// 2. Apply the panes-filter (F7/F8/F9) BEFORE
///    the token filter so the user can narrow
///    within the filtered section.
/// 3. Apply the Substring / Fuzzy / Regex token
///    filter. Group-aware: a workspace header is
///    kept if any child pane matches, so searching
///    for a pane command still surfaces the parent
///    workspace header (and vice versa).
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    app.fetch_session_panes();
    // Apply the panes-filter
    // (toggled by F7 / F8 /
    // F9) BEFORE the token
    // filter so the user can
    // narrow within the
    // filtered section (e.g.
    // `*vim` with the Windows
    // filter on shows only
    // live panes running vim).
    //
    // The filter is on the
    // row's `source` field:
    //   - `Windows` keeps
    //     `pane` + `workspace`
    //     (live multiplexer).
    //   - `Hosts` keeps `hosts`.
    //   - `Sessions` keeps
    //     `sessions`.
    //   - `All` keeps everything.
    let section_rows: Vec<HistoryRow> = if app.panes_filter.is_default() {
        app.session_panes.clone()
    } else {
        app.session_panes
            .iter()
            .filter(|r| match app.panes_filter {
                PanesFilter::All => true,
                PanesFilter::Windows => r.source == "pane" || r.source == "workspace",
                PanesFilter::Hosts => r.source == "hosts",
                PanesFilter::Sessions => r.source == "sessions",
            })
            .cloned()
            .collect()
    };
    let filter = app.panes_pattern().trim();
    let case_sensitive = app.is_case_sensitive();
    let tokens: Vec<String> = filter
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| {
            if case_sensitive {
                t.to_string()
            } else {
                t.to_lowercase()
            }
        })
        .collect();
    if tokens.is_empty() {
        return Ok(section_rows);
    }
    // Per-row match predicate. Used for both
    // the Substring fast path and the Fuzzy /
    // Regex delegating path.
    let row_matches = |r: &HistoryRow| -> bool {
        // When the match algorithm is Substring,
        // use the fast inline AND-by-token check.
        // When it's Fuzzy or Regex, delegate to
        // `query_matches_text` so the active
        // algorithm is honored.
        if app.match_algorithm != MatchAlgorithm::Substring {
            return app.query_matches_text(&r.command)
                || app.query_matches_text(&r.comment)
                || (!r.output.is_empty() && app.query_matches_text(&r.output));
        }
        if case_sensitive {
            tokens.iter().all(|tok| {
                r.command.contains(tok) || r.comment.contains(tok) || r.output.contains(tok)
            })
        } else {
            let cmd_lc = r.command.to_lowercase();
            let dir_lc = r.comment.to_lowercase();
            let tab_lc = r.output.to_lowercase();
            tokens
                .iter()
                .all(|tok| cmd_lc.contains(tok) || dir_lc.contains(tok) || tab_lc.contains(tok))
        }
    };
    // Group-aware filter: the panes-mode rows
    // are already laid out as a linearised
    // tree (`workspace_header, pane, pane, …,
    // workspace_header, pane, …`) by
    // `fetch_session_panes_impl`. Each group is
    // "one workspace header followed by its
    // zero-or-more child pane rows". A group
    // matches if ANY row in the group matches
    // (workspace-label match OR any-child-pane
    // match), in which case the WHOLE group is
    // emitted. This is what the user asked
    // for: "I searched for `SmartHistory`, I
    // want to see the workspace AND its panes".
    // Hosts (`source == "hosts"`) and sessions
    // (`source == "sessions"`) are standalone
    // rows (no children) and use the legacy
    // per-row filter.
    let mut out: Vec<HistoryRow> = Vec::new();
    let mut idx = 0;
    while idx < section_rows.len() {
        let row = &section_rows[idx];
        if row.source == "workspace" {
            // Collect the contiguous group:
            // this `workspace` header plus every
            // immediately following row whose
            // `source` is `"pane"`. Rows after
            // the first non-`pane` row start a
            // new group.
            let group_start = idx;
            let mut group_end = idx + 1;
            while group_end < section_rows.len() && section_rows[group_end].source == "pane" {
                group_end += 1;
            }
            let group = &section_rows[group_start..group_end];
            // Group matches if any row in it
            // matches. This is the
            // parent-wins-and-child-wins semantic:
            // typing the workspace label keeps
            // the whole workspace; typing a pane
            // command keeps that pane AND its
            // parent workspace header.
            if group.iter().any(row_matches) {
                out.extend_from_slice(group);
            }
            idx = group_end;
        } else {
            // Standalone row (hosts, sessions,
            // or a stray pane that lost its
            // header for any reason): per-row
            // filter.
            if row_matches(row) {
                out.push(row.clone());
            }
            idx += 1;
        }
    }
    Ok(out)
}
