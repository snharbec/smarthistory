//! `*` (session panes) prefix mode.
//!
//! Lists every pane in the *current* multiplexer context
//! (tmux session or herdr workspace), excluding the pane
//! the TUI is running in (read from `$TMUX_PANE`).
//! Selecting a row stages a `select-pane` / `switch-client`
//! command (or the herdr equivalent) and exits the TUI.
use crate::tui::state::{HistoryRow, MatchAlgorithm, PanesFilter};
use crate::tui::mode::CheckReport;
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

/// Health check for the panes (`*`) mode. The mode
/// reads a snapshot from the configured multiplexer
/// backend (tmux or herdr), so the check verifies:
///
/// 1. The multiplexer backend is configured
///    (`multiplexer=tmux|herdr` in the config or
///    `SMARTHISTORY_MULTIPLEXER` env var).
/// 2. The user is running inside a multiplexer
///    session (`$TMUX` or `$HERDR_PANE_ID` is set).
/// 3. The backend's `snapshot_current_panes` returns
///    without error (a cheap round-trip to
///    `tmux list-panes -s` or `herdr pane list`).
pub(crate) fn check(app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Panes;

    // 1. Backend is configured.
    let backend = app.multiplexer.name();
    let in_tmux = std::env::var("TMUX").is_ok();
    let in_herdr = std::env::var("HERDR_PANE_ID").is_ok();
    let current_pane_env = if in_tmux {
        std::env::var("TMUX_PANE").ok()
    } else if in_herdr {
        std::env::var("HERDR_PANE_ID").ok()
    } else {
        None
    };

    // 2. Inside a multiplexer session?
    if !in_tmux && !in_herdr {
        return CheckReport::warn(
            mode,
            format!(
                "multiplexer backend is `{backend}` but you are not inside a multiplexer session ($TMUX / $HERDR_PANE_ID both unset); the `*` mode will show an empty list"
            ),
        );
    }

    // 3. Snapshot round-trip.
    let Some(ref current_pane) = current_pane_env else {
        return CheckReport::warn(
            mode,
            format!(
                "inside a `{backend}` session but the pane-id env var ($TMUX_PANE / $HERDR_PANE_ID) is not set; the `*` mode cannot exclude your own pane"
            ),
        );
    };
    let panes = app.multiplexer.snapshot_current_panes(current_pane);
    if panes.is_empty() {
        CheckReport::warn(
            mode,
            format!("`{backend}` backend returned 0 panes (you may be the only pane in this session)"),
        )
    } else {
        CheckReport::ok(
            mode,
            format!("`{backend}` backend returned {} pane(s)", panes.len()),
        )
    }
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
    crate::tui::mode::panes::refresh_session_panes(app);
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


    /// Populate `app.session_panes` from
    /// `tmux list-panes -s` (the *current*
    /// session only — `-s` limits to the
    /// session the TUI is running in, unlike
    /// `-a` which walks every session). The
    /// current pane (`$TMUX_PANE`) is excluded
    /// so the user never sees the pane they're
    /// in. Idempotent — runs at most once per
    /// TUI session; subsequent calls return
    /// immediately (the pane set doesn't
    /// change while the TUI is the foreground
    /// process). Failure modes are silent
    /// (same contract as `fetch_tmux_windows`):
    /// `tmux` not on PATH, not in a tmux
    /// session, or the subprocess hangs past
    /// `TMUX_PANE_PROBE_TIMEOUT_MS` → the
    /// cache stays empty and the user sees an
    /// empty list.
    ///
    /// Each pane becomes a `HistoryRow`:
    /// - `command` (primary text) = the
    ///   pane's current command
    ///   (`#{pane_current_command}`, e.g.
    ///   `zsh`, `vim`, `cargo`).
    /// - `comment` (secondary text) = the
    ///   pane's cwd shortened to `~/x`.
    /// - `directory` = the full canonical cwd.
    /// - `session_id` = the pane id (`%N`),
    ///   used as the `select-pane -t` target.
    /// - `output` = the pane's global window
    ///   id (`@N`), used as the
    ///   `select-window -t` target so the
    ///   jump works even when the pane is
    ///   in a different window than the
    ///   current one (plain `select-pane`
    ///   does NOT switch windows).
    /// - `source` = `"pane"`.
    /// - `id` = synthetic decreasing negative.
    pub(crate) fn refresh_session_panes(app: &mut App) {
        if !app.session_panes.is_empty() {
            return;
        }
        // The pane id the TUI is
        // running in. tmux sets
        // `$TMUX_PANE` for every
        // pane; herdr sets
        // `$HERDR_PANE_ID` for
        // every pane. Either is
        // a valid exclude-target
        // (jumping to ourselves
        // would be a no-op). We
        // bail early only when
        // NEITHER is set — that
        // means the user isn't
        // running inside a
        // multiplexer pane at
        // all (so there are no
        // sibling panes to jump
        // to and the snapshot
        // would be wasted work).
        //
        // The previous code
        // checked `$TMUX_PANE`
        // only, which silently
        // zeroed the panes list
        // for herdr users (they
        // have `HERDR_PANE_ID`
        // set but not `TMUX_PANE`),
        // surfacing as the
        // user-reported bug
        // "there are no panes
        // visible when I switch
        // to the panes prefix".
        let current_pane = std::env::var("TMUX_PANE")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("HERDR_PANE_ID")
                    .ok()
                    .filter(|s| !s.is_empty())
            });
        let current_pane = match current_pane {
            Some(p) => p,
            // Neither env var set —
            // the user isn't inside
            // a multiplexer pane.
            // Bail rather than
            // spawn a snapshot that
            // would have nothing
            // useful to return.
            None => return,
        };
        crate::tui::mode::panes::refresh_session_panes_impl(app, &current_pane);
    }

    /// The implementation of `fetch_session_panes`,
    /// separated so tests can inject the "current
    /// pane" id directly (env-var mutation is
    /// `unsafe` since Rust 1.66 and is racy under
    /// the parallel test runner). `current_pane`
    /// is the pane id to EXCLUDE from the list
    /// (the one the TUI is running in). Reads
    /// `list-panes -s` and caches the parsed
    /// panes into `app.session_panes`.
    pub(crate) fn refresh_session_panes_impl(app: &mut App, current_pane: &str) {
        // Delegate the snapshot
        // to the configured backend's
        // `snapshot_current_panes`. The
        // backend returns one row per
        // pane the user can switch to
        // (every pane across every
        // session / workspace, excluding
        // the one the TUI is running in).
        // The backend's
        // `CurrentPaneInfo` carries a
        // `session_label` (tmux: the
        // session name; herdr: the
        // workspace id) and a
        // `tab_id` (the parent window /
        // tab the pane lives in).
        //
        // The display layout the user
        // asked for is a **tree**:
        // one "header" row per
        // session / workspace, with
        // its panes indented
        // underneath. So we group the
        // backend rows by
        // `session_label` (preserving
        // first-seen order) and emit
        // a `workspace` row, then its
        // `pane` rows, for each group.
        //
        // The pane the TUI is running
        // in (passed as `current_pane`
        // by the caller; for herdr
        // this is `HERDR_PANE_ID` like
        // `wB:p1`) is excluded — the
        // user never sees a "switch to
        // myself" row.
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let backend_rows = app.multiplexer.snapshot_current_panes(current_pane);

        // First pass: build a
        // (session_label → Vec<pane_record>)
        // map preserving first-seen
        // order. Panes with no resolvable
        // absolute path are dropped (same
        // defensive filter as the
        // directories-mode fetch).
        // The `is_last` flag is carried
        // through so we can bubble the
        // containing workspace to the
        // top of the list afterwards.
        use std::collections::BTreeMap;
        let mut order: Vec<String> = Vec::new();
        let mut grouped: BTreeMap<
            String,
            Vec<(crate::multiplexer::CurrentPaneInfo, String, String, i64)>,
        > = BTreeMap::new();
        // Decreasing synthetic ids so
        // the rows sort consistently
        // under any timestamp-DESC sort.
        let mut next_id: i64 = -1;
        let mut last_workspace: Option<String> = None;
        for pr in backend_rows {
            if std::env::var("SMARTHISTORY_DEBUG_TMUX").is_ok() {
                eprintln!(
                    "[debug] pass 1: considering pr.pane_id={:?} session_label={:?} cwd={:?}",
                    pr.pane_id, pr.session_label, pr.path
                );
            }
            if pr.pane_id.is_empty() || pr.pane_id == current_pane {
                if std::env::var("SMARTHISTORY_DEBUG_TMUX").is_ok() {
                    eprintln!(
                        "[debug]   DROPPED: empty pane_id or matches current_pane={:?}",
                        current_pane
                    );
                }
                continue;
            }
            let path_raw = pr.path.clone();
            if path_raw.is_empty() {
                if std::env::var("SMARTHISTORY_DEBUG_TMUX").is_ok() {
                    eprintln!("[debug]   DROPPED: path_raw empty");
                }
                continue;
            }
            // herdr sometimes reports shell-shortened `~/x` paths.
            // We expand them to absolute form here. NOTE: this uses
            // `expand_home_to_absolute`, NOT `expand_home` —
            // `expand_home` is misnamed and actually calls
            // `shorten_home_path` (which goes the OTHER direction,
            // absolute → `~/x`). The previous code used `expand_home`
            // and silently shortened paths like
            // `/Users/har/smarthistory/smarthistory` to
            // `~/smarthistory/smarthistory`, which then failed the
            // `starts_with('/')` check below and got dropped.
            // That was the user's bug: only some workspaces' panes
            // showed up in the `*` mode list because the others'
            // cwds got shortened (and dropped) here.
            let abs_path =
                crate::util::expand_home_to_absolute(&path_raw, &app.home_list).into_owned();
            if !abs_path.starts_with('/') {
                if std::env::var("SMARTHISTORY_DEBUG_TMUX").is_ok() {
                    eprintln!(
                        "[debug]   DROPPED: abs_path={:?} doesn't start with '/'",
                        abs_path
                    );
                }
                continue;
            }
            let full_path = crate::util::canonicalize_directory(&abs_path);
            if full_path.is_empty() {
                if std::env::var("SMARTHISTORY_DEBUG_TMUX").is_ok() {
                    eprintln!(
                        "[debug]   DROPPED: canonicalize_directory({:?}) returned empty",
                        abs_path
                    );
                }
                continue;
            }
            let short_dir =
                crate::util::shorten_home_path(&full_path, &app.home_list).into_owned();
            let id = next_id;
            next_id -= 1;
            let label = pr.session_label.clone();
            if !grouped.contains_key(&label) {
                order.push(label.clone());
            }
            // The last pane gets a
            // slightly newer timestamp
            // so it sorts first within
            // its group AND signals
            // "bring this workspace to
            // the top of the list".
            if pr.is_last {
                last_workspace = Some(label.clone());
            }
            grouped
                .entry(label)
                .or_default()
                .push((pr.clone(), short_dir, full_path, id));
        }

        // Second pass: emit
        // (workspace_header, then
        // its pane children) for
        // each group, in
        // first-seen order. The
        // workspace containing the
        // "last" pane is emitted
        // first so pressing Enter
        // on the default selection
        // flips back to where the
        // user just was.
        if let Some(ref last_ws) = last_workspace
            && let Some(pos) = order.iter().position(|l| l == last_ws)
            && pos > 0
        {
            let l = order.remove(pos);
            order.insert(0, l);
        }

        let mut panes: Vec<HistoryRow> = Vec::new();
        if std::env::var("SMARTHISTORY_DEBUG_TMUX").is_ok() {
            eprintln!(
                "[debug] pass 2: order={:?}, grouped.keys={:?}",
                order,
                grouped.keys().collect::<Vec<_>>()
            );
            for (k, v) in &grouped {
                eprintln!("[debug]   grouped[{:?}] has {} entries", k, v.len());
            }
        }
        for label in &order {
            let entries = grouped.get(label).cloned().unwrap_or_default();
            if entries.is_empty() {
                continue;
            }
            // Bubble any
            // `is_last` pane to
            // the front of this
            // workspace's pane
            // list so the user
            // can flip back to it
            // immediately.
            let mut entries = entries;
            if let Some(pos) = entries.iter().position(|(pr, _, _, _)| pr.is_last)
                && pos > 0
            {
                let item = entries.remove(pos);
                entries.insert(0, item);
            }

            // The workspace header
            // row. `command` is the
            // session/workspace label
            // itself (what the user
            // sees as the row's
            // primary text);
            // `session_id` is the
            // label too (passed to
            // `focus_session` on
            // selection). The pane
            // count + agent summary
            // goes in `comment` as
            // a secondary hint.
            let agent_count = entries
                .iter()
                .filter(|(pr, _, _, _)| !pr.current_command.is_empty())
                .count();
            let summary = format!(
                "{} pane{}{}, ",
                entries.len(),
                if entries.len() == 1 { "" } else { "s" },
                if agent_count > 0 {
                    format!(
                        ", {} agent{}",
                        agent_count,
                        if agent_count == 1 { "" } else { "s" }
                    )
                } else {
                    String::new()
                }
            );
            panes.push(HistoryRow {
                id: next_id,
                command: label.clone(),
                directory: entries
                    .first()
                    .map(|(_, _, full, _)| full.clone())
                    .unwrap_or_default(),
                // The workspace ID (`wB`, `wE`,
                // etc.) — used as the focus
                // target by `select_for_run`'s
                // workspace-row branch
                // (`app.multiplexer.focus_session(session_id)`).
                // The DISPLAY text the
                // user sees (e.g. "smarthistory",
                // "dir: Downloads") is in
                // `command` and is the backend's
                // `session_label` (resolved from
                // `herdr workspace list`'s `label`
                // field). Keep these separate:
                // `session_id` is what herdr's
                // `workspace focus` accepts,
                // `command` is what the user
                // recognizes.
                session_id: entries
                    .first()
                    .map(|(pr, _, _, _)| pr.window_id.clone())
                    .unwrap_or_default(),
                exit_code: 0,
                timestamp: now_epoch,
                comment: summary,
                output: String::new(),
                mode: "workspace".to_string(),
                source: "workspace".to_string(),

                ..Default::default()
            });
            next_id -= 1;
            // Then the pane rows.
            // Each is indented in the
            // renderer (we drop the
            // `[label]` badge since
            // the workspace header
            // above already identifies
            // it). `tab_id` is stashed
            // in `output` so
            // `select_for_run`'s
            // pane-row branch can pass
            // it to `focus_pane`.
            for (pr, short_dir, full_path, id) in entries {
                panes.push(HistoryRow {
                    id,
                    command: pr.current_command.clone(),
                    directory: full_path,
                    session_id: pr.pane_id.clone(),
                    exit_code: 0,
                    timestamp: now_epoch,
                    comment: short_dir,
                    output: pr.tab_id.clone(),
                    mode: "pane".to_string(),
                    source: "pane".to_string(),

                    ..Default::default()
                });
            }
        }
        // Diagnostic: dump
        // the row count +
        // structure right
        // before we commit
        // to `session_panes`.
        // The grouping logic
        // (BTreeMap + first-seen
        // order) is subtle and
        // a regression here
        // would silently drop
        // whole workspaces.
        // The eprintln is
        // gated on the
        // `SMARTHISTORY_DEBUG_TMUX`
        // env var (same flag
        // the existing tmux-
        // filter debug logs
        // watch) so it doesn't
        // run in production
        // for users who don't
        // want noise.
        if std::env::var("SMARTHISTORY_DEBUG_TMUX").is_ok() {
            let ws_count = panes.iter().filter(|r| r.mode == "workspace").count();
            let pane_count = panes.iter().filter(|r| r.mode == "pane").count();
            eprintln!(
                "[debug] fetch_session_panes_impl: emitting {} rows ({} workspace headers, {} pane children)",
                panes.len(),
                ws_count,
                pane_count
            );
            for r in &panes {
                eprintln!(
                    "[debug]   mode={:?} session_id={:?} command={:?} comment={:?}",
                    r.mode, r.session_id, r.command, r.comment
                );
            }
        }
        app.session_panes = panes;
        // Append named sessions as additional
        // workspace + pane rows, grouped under
        // a `# sessions` header.
        if !app.sessions.is_empty() {
            app.session_panes.push(HistoryRow {
                id: -20_000,
                command: "sessions".to_string(),
                directory: String::new(),
                session_id: "sessions".to_string(),
                exit_code: 0,
                timestamp: 0,
                comment: "configured sessions".to_string(),
                output: String::new(),
                mode: "workspace".to_string(),
                source: "sessions".to_string(),

                ..Default::default()
            });
            let mut next_session_id: i64 = -20_000;
            for s in &app.sessions {
                next_session_id -= 1;
                app.session_panes.push(HistoryRow {
                    id: next_session_id,
                    command: s.command.clone(),
                    directory: s.directory.clone(),
                    session_id: s.command.clone(),
                    exit_code: 0,
                    timestamp: 0,
                    comment: s.comment.clone(),
                    output: String::new(),
                    mode: "session".to_string(),
                    source: "sessions".to_string(),

                    ..Default::default()
                });
            }
        }
        // Append hosts as additional
        // rows, grouped under a
        // `# hosts` header. Each
        // host row carries the
        // display name in `command`
        // and a `user@host:port`
        // connection string in
        // `directory` (the staging
        // layer reads both for the
        // matcher and the
        // connection-argv
        // construction).
        if !app.hosts.is_empty() {
            app.session_panes.push(HistoryRow {
                id: -25_000,
                command: "hosts".to_string(),
                directory: String::new(),
                session_id: "hosts".to_string(),
                exit_code: 0,
                timestamp: 0,
                comment: "configured hosts".to_string(),
                output: String::new(),
                mode: "workspace".to_string(),
                source: "hosts".to_string(),

                ..Default::default()
            });
            let mut next_host_id: i64 = -25_000;
            for h in &app.hosts {
                next_host_id -= 1;
                // The synthetic
                // `id` is
                // negative
                // (consistent
                // with
                // `# sessions`)
                // and stable
                // across
                // refreshes, so
                // the staging
                // layer can
                // index
                // `app.hosts`
                // by
                // `-(row.id) -
                // 25_000 - 1`
                // (the
                // accessor in
                // `Config::hosts`
                // uses the
                // same
                // `id - 30_000`
                // scheme,
                // shifted by
                // 5_000 to keep
                // the two
                // ranges
                // disjoint).
                app.session_panes.push(HistoryRow {
                    id: next_host_id,
                    command: h.command.clone(),
                    directory: h.directory.clone(),
                    session_id: String::new(),
                    exit_code: 0,
                    timestamp: 0,
                    comment: h.comment.clone(),
                    output: String::new(),
                    mode: "host".to_string(),
                    source: "hosts".to_string(),

                    ..Default::default()
                });
            }
        }
        if std::env::var("SMARTHISTORY_DEBUG_TMUX").is_ok() {
            eprintln!(
                "[debug] sessions count: {}, session_panes total: {}",
                app.sessions.len(),
                app.session_panes.len()
            );
        }
        // Bump the snapshot id and spawn an
        // asynchronous cmdline lookup for every
        // pane row in the new snapshot. The
        // background thread is the herdr path's
        // way of getting the running process's
        // command line (`nvim config.toml`,
        // `ssh har@host`, …) without blocking
        // the first render — the panes view is
        // shown immediately with the agent name,
        // and the cmdline is patched in later
        // when the lookup completes (see
        // `process_pane_cmdlines`).
        //
        // We also cancel any in-flight lookup
        // from a previous snapshot — its results
        // would be stale (the panes may have
        // changed since) and could overwrite the
        // new snapshot's rows.
        app.panes_snapshot_id = app.panes_snapshot_id.wrapping_add(1);
        // The background cmdlines lookup is spawned lazily
        // from `process_pane_cmdlines` (called on every
        // run-loop tick), NOT here. This avoids the
        // spawn-and-immediately-cancel pattern that
        // happened when `fetch_session_panes_impl` was
        // called multiple times in quick succession during
        // TUI initialization (sessions / hosts population
        // triggers several refreshes, each bumping the
        // snapshot id). The lazy spawn fires once,
        // after the run loop settles, and the snapshot
        // id at that point matches the current snapshot.
    }

/// Lazy-load the last 50 lines of the selected herdr pane
/// into `row.preview` for the output preview pane. Called
/// from `App::refresh()` and `App::move_selection` on every
/// selection change so the preview updates immediately when
/// the user navigates the `*` panes list.
///
/// Behavior by row kind:
/// - `mode == "pane"`: read the pane's last 50 visible
///   lines via `app.multiplexer.read_pane(pane_id, 50)`.
///   The pane id is in `row.session_id`. On success, the
///   text is stored verbatim in `row.preview`; on failure
///   (tmux backend, daemon down, `pane_not_found`, or
///   timeout), the row is left with an empty preview so
///   the renderer shows the standard "no preview
///   available" placeholder.
/// - `mode == "workspace"`: workspaces are group headers
///   with no pane content of their own; preview stays
///   empty.
/// - `mode == "session"`: configured sessions are
///   external commands (not live panes); preview stays
///   empty.
///
/// The function is cheap to call repeatedly: the
/// `read_pane` call is gated on `row.preview.is_empty()`
/// so a row that already has its preview doesn't trigger
/// a second `herdr` IPC round-trip on subsequent
/// selections. The cache is per-row, not per-pane-id —
/// re-selecting a pane after selecting something else
/// will re-read (the visible content of the pane may
/// have changed in the meantime).
pub(crate) fn ensure_selected_context(app: &mut App) {
    if !matches(app) {
        return;
    }
    let Some(idx) = app.list_state.selected() else {
        return;
    };

    // Read the row's kind and pane id up front so the
    // immutable borrow is released before the
    // `&mut app.multiplexer.read_pane` and the
    // `&mut app.merged_rows` borrow.
    let (kind, pane_id) = match app.merged_rows.get(idx) {
        Some(r) if r.mode == "pane" => ("pane", r.session_id.clone()),
        Some(r) if r.mode == "workspace" => ("workspace", String::new()),
        Some(r) if r.mode == "session" => ("session", String::new()),
        _ => return, // host rows or other modes
    };

    if kind != "pane" || pane_id.is_empty() {
        return;
    }

    // Read the current preview state so we can
    // short-circuit. We re-read on every selection even
    // if the row was previously read (the pane content
    // may have changed). To avoid the IPC round-trip on
    // every keystroke, we keep a tiny memoization: the
    // last pane id we read + the time we read it, and
    // skip re-reading within a 750ms window. The same
    // `pane_id` will refresh the preview every
    // selection, so the user still sees fresh content
    // when navigating.
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    let cache_key = pane_id.clone();
    let now = Instant::now();
    let cached_at: Option<Instant> = app
        .pane_preview_cache
        .as_ref()
        .and_then(|m| m.get(&cache_key).copied());
    let fresh = cached_at
        .map(|t| now.duration_since(t) < Duration::from_millis(750))
        .unwrap_or(false);

    if !fresh {
        let preview_text = app.multiplexer.read_pane(&pane_id, 50);
        // Update the memoization timestamp even on
        // failure so a transient daemon blip doesn't
        // trigger a tight retry loop on every
        // keystroke.
        let cache = app
            .pane_preview_cache
            .get_or_insert_with(HashMap::new);
        cache.insert(cache_key, now);
        // For panes mode, `build_merged_rows` is a
        // straight clone of `self.rows` (no dedup, no
        // labeled injection — see the panes-mode
        // comment in `build_merged_rows`). So the
        // row index is identical between `self.rows`
        // and `self.merged_rows`. We MUST write the
        // preview to BOTH so a subsequent
        // `build_merged_rows` rebuild doesn't wipe
        // the preview (the typical case: a pane
        // cmdline background-thread update arrives
        // and `process_pane_cmdlines` calls
        // `self.rows = self.fetch()` + `self.merged_rows
        // = self.build_merged_rows()`; the new
        // `self.rows` has empty previews, and the
        // rebuild clones them into `merged_rows`).
        //
        // Writing to both indexes keeps the preview
        // alive across cmdline updates and across
        // every other rebuild path. The earlier code
        // only wrote to `merged_rows` and so the
        // preview appeared to "toggle": it was
        // visible for one frame after a selection
        // change (the write beat the next
        // `process_pane_cmdlines` tick), then was
        // wiped by the next rebuild, then
        // re-populated by the next
        // `ensure_selected_context` call, ad
        // infinitum.
        if let Some(text) = preview_text {
            let preview: String = text
                .lines()
                .take(crate::tui::SOURCE_CONTEXT_LINES)
                .collect::<Vec<_>>()
                .join("\n");
            if let Some(row) = app.rows.get_mut(idx)
                && row.preview != preview {
                    row.preview = preview.clone();
                }
            if let Some(row) = app.merged_rows.get_mut(idx)
                && row.preview != preview {
                    row.preview = preview;
                }
        } else {
            // Empty / unavailable: keep any existing
            // preview on both copies (so a transient
            // failure doesn't blank a successful
            // read). Don't write empty strings —
            // they'd clobber a previously-good read.
        }
    }
}
