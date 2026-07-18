//! `#` (directories) prefix mode.
//!
//! The directories view lists every unique directory
//! that's been used in the global history, sorted by
//! the most-recent history row's timestamp DESC, with
//! each directory's most-recently-executed
//! command surfaced for context. Selecting a row
/// stages a `cd <path>` command.
use crate::tui::state::{HistoryRow, MatchAlgorithm};
use crate::tui::App;
use anyhow::Result;

///
/// True if the user typed the
/// `directories` prefix
/// (default `#`).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.directories;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The directories-search
/// body, i.e. everything
/// after the leading `#
/// prefix. Used to filter
/// the listed directories by
/// path substring. Empty
/// when not in directories
/// mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.directories;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Lazy-populate `app.tmux_windows` from the
/// configured multiplexer backend. The snapshot is
/// per-TUI-session (refreshing it would mean
/// re-spawning the multiplexer for every keystroke
/// the user makes while in directories mode, which
/// is wasteful). A future "refresh" key binding
/// could re-invoke this when the user wants
/// freshness.
///
/// The field is named `tmux_windows` for historical
/// reasons — it's now populated by whichever
/// backend is configured. The conversion is a
/// plain field-by-field copy from
/// `ActiveContext` (the `MultiplexerBackend::snapshot`
/// return type) to `TmuxWindowInfo` (the `App`'s
/// internal cache type).
pub(crate) fn ensure_multiplexer_snapshot(app: &mut App) {
    if !app.tmux_windows.is_empty() {
        return;
    }
    let tmux_debug = std::env::var("SMARTHISTORY_DEBUG_TMUX").is_ok();
    let rows = app.multiplexer.snapshot();
    if tmux_debug {
        crate::tui::tmux_filter_debug_log(&format!(
            "multiplexer snapshot: {} rows (backend={})",
            rows.len(),
            app.multiplexer.name()
        ));
    }
    app.tmux_windows = rows
        .into_iter()
        .map(|r| crate::tui::TmuxWindowInfo {
            pane_id: r.pane_id,
            path: r.path,
            current_command: r.current_command,
            workspace_label: r.workspace_label,
        })
        .collect();
}

/// List every unique directory
/// that has been used in the
/// global history, sorted by
/// each directory's most-
/// recent history row's
/// timestamp DESC. Each row
/// also surfaces that
/// directory's most-recently-
/// executed command so the
/// user has context for "what
/// was I doing in there?" The
/// typed query (after the
/// prefix) is treated as a
/// space-separated AND-filter
/// against the directory path,
/// same contract as the
/// other query modes.
///
/// The "recency" sort is
/// server-side: the SQL uses
/// an aggregate `MAX(timestamp)`
/// over each `directory`
/// group and orders by it
/// DESC, so a directory the
/// user visited yesterday
/// beats one visited last
/// week even if both have many
/// history rows.
///
/// Output shape: reuses
/// `HistoryRow` so the rest of
/// the TUI (highlighting,
/// detail pane, key dispatch)
/// keeps working without a new
/// parallel rendering path.
/// The `command` field carries
/// the directory's latest
/// command (so the list rows
/// show a useful one-line
/// summary); `directory`
/// carries the absolute path
/// (used by the action layer
/// to stage the `cd`
/// command); `timestamp`
/// carries the directory's
/// `MAX(timestamp)`; `id` is
/// a synthetic negative
/// `(directory_index)` so we
/// don't collide with real
/// history ids.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    let filter = app.directories_pattern().trim();
    // Build the SQL once, with
    // a single optional
    // `LIKE` filter per
    // whitespace-split token
    // (AND-matched). Empty
    // pattern means "no
    // filter". Parameter
    // positions are computed
    // along the way so
    // rusqlite binds them in
    // the same order as the
    // `?` placeholders.
    let filter_tokens: Vec<&str> = filter
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .collect();
    // When the match algorithm is not Substring, skip the
    // SQL LIKE pre-filter entirely. The LIKE uses substring
    // matching which would exclude rows that a regex or fuzzy
    // search SHOULD match. Instead, fetch without a SQL filter
    // and let the `refresh()` post-filter (which branches on
    // `match_algorithm`) apply the correct matching strategy.
    let use_sql_like = app.match_algorithm == MatchAlgorithm::Substring;
    let mut sql = String::from(
        "SELECT h.directory, \
                    h.command, \
                    latest.max_ts \
             FROM history h \
             INNER JOIN ( \
                 SELECT directory, \
                        MAX(timestamp) AS max_ts \
                 FROM history \
                 WHERE directory != '' \
                 GROUP BY directory \
             ) latest \
               ON h.directory = latest.directory \
              AND h.timestamp = latest.max_ts \
             WHERE h.directory != ''",
    );
    if use_sql_like && !filter_tokens.is_empty() {
        sql.push_str(" AND (");
        for (i, _tok) in filter_tokens.iter().enumerate() {
            if i > 0 {
                sql.push_str(" AND ");
            }
            sql.push_str("h.directory LIKE ? ESCAPE '\\'");
        }
        sql.push(')');
    }
    // Tie-break: same-timestamp
    // directories sort by
    // directory ASC for stable
    // output. We then
    // canonicalise the
    // directory in code so
    // `/Users/har/foo` and
    // `/Volumes/HUGE/har/foo`
    // collapse to the same
    // group (matching the
    // DIR-mode filter logic
    // elsewhere — see
    // `canonicalize_directory`).
    sql.push_str(
        " GROUP BY h.directory \
             ORDER BY latest.max_ts DESC, h.directory ASC \
             LIMIT 1000",
    );
    let mut stmt = app.conn.prepare(&sql)?;
    // Build owned parameter
    // strings so the lifetime
    // requirements of
    // `params_ref` are satisfied
    // without needing to box-
    // leak. Each token becomes
    // a `%token%` substring
    // for `LIKE`. Empty tokens
    // are skipped so an
    // accidental double-space
    // doesn't blow up the
    // bind count.
    let filter_tokens: Vec<&str> = filter
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .collect();
    let owned_params: Vec<String> = filter_tokens
        .iter()
        .map(|tok| format!("%{}%", crate::util::escape_like(tok)))
        .collect();
    let params_ref: Vec<&dyn rusqlite::ToSql> = owned_params
        .iter()
        .map(|s| s as &dyn rusqlite::ToSql)
        .collect();
    let raw_rows = stmt.query_map(params_ref.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    // Use the cached
    // home-prefix list
    // (computed once at App
    // construction; see
    // `build_home_list`) so
    // we don't re-read
    // `~/.config/smarthistory/config`
    // on every
    // `fetch_directories`
    // call. The list
    // already has `$HOME`
    // first and homemap
    // entries after, so
    // `shorten_home_path`
    // does the right thing.
    let home_list = app.home_list.clone();
    // Deduplicate on canonical
    // path: a directory may
    // appear under multiple
    // forms (e.g. `/Users/har/x`
    // and `/Volumes/HUGE/har/x`)
    // because of macOS volume
    // mounts. The first
    // occurrence (which is the
    // newest, since we sort by
    // max_ts DESC) wins.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut rows: Vec<HistoryRow> = Vec::new();
    let mut next_id: i64 = -1;
    // The directory-source
    // filter is applied
    // *early*, not just at
    // the end. If we let
    // the SQL loop (or the
    // sessiondir loop)
    // populate the shared
    // `seen` set first, a
    // tmux pane whose path
    // also appears in
    // history would be
    // silently deduped away
    // — so in `DIR:TMUX`
    // mode the user would
    // only see the tmux
    // panes whose paths
    // they had *never*
    // visited (exact bug
    // reported: of 5 active
    // panes, only 2 showed,
    // the ones not in the
    // history DB). Skip the
    // irrelevant loops
    // entirely instead.
    let want_sql = matches!(
        app.directory_source,
        crate::tui::state::DirectorySource::All | crate::tui::state::DirectorySource::Config
    );
    let want_sessiondirs = matches!(
        app.directory_source,
        crate::tui::state::DirectorySource::All | crate::tui::state::DirectorySource::Config
    );
    let want_tmux = matches!(
        app.directory_source,
        crate::tui::state::DirectorySource::All | crate::tui::state::DirectorySource::Tmux
    );
    if !want_sql {
        crate::tui::tmux_filter_debug_log("skipping SQL loop (directory_source != All/Config)");
    }
    for raw in raw_rows {
        if !want_sql {
            break;
        }
        let (directory, command, ts) = raw?;
        let canonical = crate::util::canonicalize_directory(&directory);
        if !seen.insert(canonical.clone()) {
            if std::env::var("SMARTHISTORY_DEBUG_TMUX").is_ok() {
                crate::tui::tmux_filter_debug_log(&format!(
                    "SQL row deduped (dup canonical {:?}): {:?}",
                    canonical, directory
                ));
            }
            continue;
        }
        // The visible list line
        // shows the **directory**
        // as the primary text
        // and the last command
        // as the secondary text
        // (the inverse of how
        // normal history rows
        // are laid out). We
        // achieve that by
        // storing the directory
        // in `command` (so the
        // existing
        // `highlight_matches(
        //   &row.command, ...)`
        // path applies
        // unchanged) and the
        // last command in
        // `comment` (so the
        // existing `# ...`
        // secondary-slot
        // rendering picks it
        // up). The `directory`
        // field still holds
        // the full absolute
        // path because the
        // tmux-pane lookup
        // (`directory_tmux_pane_id`)
        // canonicalises against
        // it.
        //
        // The directory in
        // `command` is the
        // shell-friendly `~/x`
        // form (matching the
        // user's typing
        // convention) so the
        // query highlighting
        // shows matches in the
        // short form they're
        // used to.
        let short_dir = crate::util::shorten_home_path(&directory, &home_list).into_owned();
        // The command in
        // `comment` is
        // truncated because
        // the secondary slot
        // is narrow. The user
        // can still see the
        // full command in
        // the Details pane.
        let short_cmd = if command.is_empty() {
            String::new()
        } else if command.chars().count() > 60 {
            let truncated: String = command.chars().take(57).collect();
            format!("{}…", truncated)
        } else {
            command.clone()
        };
        // Synthetic row. `id`
        // is negative to avoid
        // colliding with real
        // history ids (same
        // convention as todo
        // rows).
        let id = next_id;
        next_id -= 1;
        rows.push(HistoryRow {
            id,
            command: short_dir,
            directory,
            session_id: String::new(),
            exit_code: 0,
            timestamp: ts,
            comment: short_cmd,
            output: String::new(),
            mode: "directory".to_string(),
            source: "history".to_string(),

            ..Default::default()
        });
    }
    // Augment with the user's
    // `sessiondirs=...` entries.
    // Every subdirectory of
    // every configured root
    // becomes a row, even if
    // the user has never run
    // a command there.
    //
    // Rows added by this loop
    // get `timestamp = 0` so
    // they sort to the bottom
    // of the list (the
    // history-driven rows
    // have real recent
    // timestamps and surface
    // first). The user can
    // still type `#<name>` to
    // filter to one of these
    // pinned rows.
    //
    // Dedup is via the same
    // `seen` set the SQL loop
    // used: a subdirectory
    // that *also* has history
    // (and thus already
    // surfaced via SQL)
    // won't appear twice. The
    // history row wins
    // (newer timestamp) and
    // carries the last
    // command; the
    // sessiondirs row is
    // suppressed.
    //
    // The secondary
    // (`comment`) slot is
    // empty for these rows,
    // unless the directory
    // (or an ancestor) has a
    // `.command` file — in
    // which case we surface
    // "has .command" so the
    // user knows the row
    // will run a setup
    // script on select.
    if !want_sessiondirs {
        crate::tui::tmux_filter_debug_log(
            "skipping sessiondir loop (directory_source != All/Config)",
        );
    }
    for sub in &app.session_subdirs {
        if !want_sessiondirs {
            break;
        }
        let canonical = crate::util::canonicalize_directory(&sub.to_string_lossy());
        if !seen.insert(canonical.clone()) {
            continue;
        }
        let directory_str = sub.to_string_lossy().into_owned();
        // Apply the same
        // substring filter
        // the SQL fetch
        // applied, so the
        // sessiondirs rows
        // are visible only
        // when they match
        // the user's typed
        // pattern. The SQL
        // `LIKE` uses the
        // raw `directory`
        // (e.g.
        // `/Volumes/HUGE/har/foo`),
        // and the user types
        // a pattern that
        // matches against
        // that form (because
        // the visible list
        // shows the shortened
        // form, but the
        // filtering is on the
        // raw form). For
        // consistency, we
        // also filter on the
        // raw form here, so
        // `#home` matches
        // both a sessiondir at
        // `~/work` (raw
        // `/Users/har/work`)
        // and an SQL row at
        // `/Users/har/home`.
        if !filter_tokens.is_empty()
            && !filter_tokens
                .iter()
                .all(|tok| directory_str.to_lowercase().contains(&tok.to_lowercase()))
        {
            continue;
        }
        // Surface a hint when
        // the row has a
        // `.command` file
        // (either in the
        // directory itself or
        // in an ancestor). The
        // user can see at a
        // glance "this row
        // will run a setup
        // script".
        let has_command =
            crate::util::find_command_file(std::path::Path::new(&directory_str)).is_some();
        let short_dir = crate::util::shorten_home_path(&directory_str, &home_list).into_owned();
        let hint = if has_command {
            String::from("(has .command)")
        } else {
            String::new()
        };
        let id = next_id;
        next_id -= 1;
        rows.push(HistoryRow {
            id,
            command: short_dir,
            directory: directory_str,
            session_id: String::new(),
            exit_code: 0,
            // `0` = unix epoch.
            // The list is sorted
            // by timestamp DESC
            // (most-recent first)
            // elsewhere, so
            // epoch-zero rows
            // land at the bottom
            // of the list. The
            // user types a
            // pattern to filter
            // to one of these.
            timestamp: 0,
            comment: hint,
            output: String::new(),
            mode: "directory".to_string(),
            source: "sessiondir".to_string(),

            ..Default::default()
        });
    }
    // Add rows for the
    // cwds of every
    // active tmux pane.
    // These appear in
    // the list even
    // when the user has
    // never run a
    // command in the
    // directory (e.g.
    // a session they
    // started months
    // ago, or a session
    // attached to a
    // project that
    // doesn't yet have
    // history).
    //
    // The `T` marker
    // (drawn in
    // `render_row`)
    // already shows
    // which directories
    // are active in
    // tmux; this
    // augmented list
    // makes the same
    // information
    // available as
    // filterable rows
    // for the `TMUX`
    // directory source
    // (so the user can
    // list "every
    // directory I'm
    // currently active
    // in" without
    // scrolling past
    // their pinned
    // projects or the
    // global history).
    //
    // Each unique
    // `pane_current_path`
    // becomes one row.
    // We dedup against
    // `seen` so a
    // directory that's
    // already in the
    // history (and so
    // already got a
    // row from the SQL
    // loop) doesn't get
    // a duplicate from
    // the tmux side. The
    // history row wins
    // (newer timestamp)
    // and carries the
    // last command; the
    // tmux row is
    // suppressed.
    //
    // Sort order: by
    // `pane_id`
    // (deterministic
    // since tmux
    // returns panes in
    // a stable order).
    // We don't have a
    // meaningful
    // timestamp for a
    // tmux pane
    // (the pane itself
    // doesn't expose
    // one), so we
    // use the current
    // epoch for all
    // tmux rows; the
    // user can still
    // type a pattern to
    // filter to one.
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if !want_tmux {
        crate::tui::tmux_filter_debug_log("skipping tmux loop (directory_source != All/Tmux)");
    }
    for window in &app.tmux_windows {
        if !want_tmux {
            break;
        }
        // Defensive filter: a
        // `pane_current_path`
        // that doesn't start
        // with `/` is not a
        // real absolute
        // filesystem path.
        // Tmux normally
        // reports only real
        // paths, but a
        // custom tmux config
        // or a wrapper could
        // produce something
        // like the command
        // line that spawned
        // the pane
        // (`tmux list-windows
        // -a ...`). Showing
        // such a "path" as a
        // directory row is
        // wrong: the row
        // wouldn't be a
        // directory, the
        // T-marker lookup
        // would fail (no
        // matching pane), and
        // the visible primary
        // text would be a
        // shell command —
        // confusing. The user
        // reported exactly
        // this: a `DIR:TMUX`
        // entry whose text
        // was the tmux
        // command line, with
        // no T flag. The
        // fix: skip any
        // `pane_current_path`
        // that doesn't look
        // like an absolute
        // path.
        if !window.path.starts_with('/') {
            crate::tui::tmux_filter_debug_log(&format!(
                "filtered tmux pane %{}: pane_current_path {:?} does not start with `/`",
                window.pane_id, window.path
            ));
            continue;
        }
        // Also require the
        // path to actually
        // resolve to a
        // directory on disk.
        // A real tmux pane's
        // cwd is a directory
        // that exists; a
        // non-path or a path
        // to a non-existent
        // file shouldn't
        // surface. Without
        // this, a tmux pane
        // whose cwd was
        // deleted while the
        // TUI is running
        // would still show
        // as a row, but the
        // user couldn't
        // actually jump to
        // it. The check is
        // best-effort: a
        // race just means
        // the row disappears
        // on the next
        // refresh, which is
        // the right behaviour
        // anyway.
        if !std::path::Path::new(&window.path).is_dir() {
            crate::tui::tmux_filter_debug_log(&format!(
                "filtered tmux pane %{}: pane_current_path {:?} is not a directory",
                window.pane_id, window.path
            ));
            continue;
        }
        let canonical = crate::util::canonicalize_directory(&window.path);
        if !seen.insert(canonical.clone()) {
            crate::tui::tmux_filter_debug_log(&format!(
                "tmux pane %{} deduped (dup canonical {:?}, eaten by an earlier loop): {:?}",
                window.pane_id, canonical, window.path
            ));
            continue;
        }
        // Same substring
        // filter as the SQL
        // and sessiondirs
        // loops above. The
        // tmux-reported path
        // is the raw absolute
        // form, so filter on
        // it directly.
        if !filter_tokens.is_empty()
            && !filter_tokens
                .iter()
                .all(|tok| window.path.to_lowercase().contains(&tok.to_lowercase()))
        {
            continue;
        }
        let short_dir = crate::util::shorten_home_path(&window.path, &home_list).into_owned();
        // Build a
        // synthetic
        // command
        // field for
        // the
        // secondary
        // slot: the
        // pane id.
        // The user
        // can copy
        // / reuse
        // it
        // (e.g. as
        // the
        // `-t`
        // argument
        // to a
        // custom
        // tmux
        // command)
        // directly
        // from the
        // list.
        let pane_hint = format!("(pane {})", window.pane_id);
        let id = next_id;
        next_id -= 1;
        crate::tui::tmux_filter_debug_log(&format!(
            "kept tmux pane %{}: pane_current_path {:?} (source=tmux)",
            window.pane_id, window.path
        ));
        rows.push(HistoryRow {
            id,
            command: short_dir,
            directory: window.path.clone(),
            session_id: String::new(),
            exit_code: 0,
            timestamp: now_epoch,
            comment: pane_hint,
            output: String::new(),
            mode: "directory".to_string(),
            source: "tmux".to_string(),

            ..Default::default()
        });
    }
    // Apply the
    // directory-source
    // filter. The
    // `ALL` mode is a
    // no-op; the
    // `TMUX` and
    // `CONFIG` modes
    // drop rows whose
    // `source` doesn't
    // match.
    let rows: Vec<HistoryRow> = match app.directory_source {
        crate::tui::state::DirectorySource::All => rows,
        crate::tui::state::DirectorySource::Tmux => {
            rows.into_iter().filter(|r| r.source == "tmux").collect()
        }
        crate::tui::state::DirectorySource::Config => rows
            .into_iter()
            .filter(|r| r.source == "sessiondir")
            .collect(),
    };
    Ok(rows)
}
