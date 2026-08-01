    use super::*;

    #[test]
    fn kind_parse_accepts_aliases() {
        assert_eq!(MultiplexerKind::parse("tmux"), Some(MultiplexerKind::Tmux));
        assert_eq!(MultiplexerKind::parse(""), Some(MultiplexerKind::Tmux));
        assert_eq!(MultiplexerKind::parse("TMUX"), Some(MultiplexerKind::Tmux));
        assert_eq!(
            MultiplexerKind::parse("herdr"),
            Some(MultiplexerKind::Herdr)
        );
        assert_eq!(
            MultiplexerKind::parse("HERDR"),
            Some(MultiplexerKind::Herdr)
        );
        assert_eq!(MultiplexerKind::parse("screen"), None);
    }

    #[test]
    fn kind_default_is_tmux() {
        assert_eq!(MultiplexerKind::default(), MultiplexerKind::Tmux);
    }

    #[test]
    fn kind_as_str_round_trips() {
        assert_eq!(MultiplexerKind::Tmux.as_str(), "tmux");
        assert_eq!(MultiplexerKind::Herdr.as_str(), "herdr");
    }

    #[test]
    fn tmux_list_windows_parses_active_only() {
        let raw = b"\
%1 | /Users/har/work | active:1 | Layout: ab12
%2 | /Users/har/notes | active:0 | Layout: cd34
%3 | /Users/har/notes/sub | active:1 | Layout: ef56
";
        let out = tmux_list_windows_parse(raw);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].pane_id, "%1");
        assert_eq!(out[0].path, "/Users/har/work");
        assert_eq!(out[1].pane_id, "%3");
    }

    #[test]
    fn tmux_list_panes_excludes_current() {
        let raw =
            b"%1 | @1 | /home | bash | 0\n%2 | @1 | /home | vim | 1\n%3 | @2 | /etc | sh | 0\n";
        let out = tmux_list_panes_parse(raw, "%1");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].pane_id, "%2");
        assert!(out[0].is_last);
        assert_eq!(out[1].pane_id, "%3");
        assert!(!out[1].is_last);
    }

    /// The 6-field form
    /// includes `session_name`
    /// (added so the `*`-mode
    /// row renderer can show a
    /// `[session-name]` badge
    /// on each pane row — the
    /// `*`-mode list now spans
    /// every session, so the
    /// badge is the only way to
    /// tell which session a pane
    /// belongs to). This test
    /// locks in the 6-field
    /// parsing path so a future
    /// format-string change
    /// (e.g. dropping
    /// `session_name`) would
    /// surface as a test
    /// failure rather than a
    /// silent regression.
    #[test]
    fn tmux_list_panes_extracts_session_name_in_six_field_form() {
        let raw = b"\
%1 | @1 | work | /Users/har/work | vim | 0
%2 | @1 | work | /Users/har/work | python | 1
%3 | @2 | debug | /var/log | tail | 0
";
        let out = tmux_list_panes_parse(raw, "%1");
        assert_eq!(out.len(), 2);
        // The session_name
        // from position 2
        // (a field that
        // doesn't appear
        // in the 5-field
        // form) lands in
        // `session_label`.
        assert_eq!(out[0].pane_id, "%2");
        assert_eq!(out[0].session_label, "work");
        assert_eq!(out[0].path, "/Users/har/work");
        assert_eq!(out[0].current_command, "python");
        assert!(out[0].is_last);
        assert_eq!(out[1].pane_id, "%3");
        assert_eq!(out[1].session_label, "debug");
        assert_eq!(out[1].path, "/var/log");
        assert_eq!(out[1].current_command, "tail");
        assert!(!out[1].is_last);
    }

    #[test]
    fn tmux_backend_focus_and_create_commands() {
        let b = TmuxBackend;
        assert_eq!(
            b.focus_command("%5").unwrap(),
            "tmux select-pane -t %5 && tmux switch-client -t %5"
        );
        assert!(b.focus_command("").is_none());
        let cmd = b
            .create_command(std::path::Path::new("/tmp/x"), "x")
            .unwrap();
        assert!(cmd.contains("tmux new-session -d -s x -c /tmp/x"));
        assert!(cmd.contains("tmux switch-client -t x"));
    }

    #[test]
    fn tmux_backend_quotes_paths_with_spaces() {
        let b = TmuxBackend;
        // Use a path that's
        // definitely not under
        // `$HOME` so the
        // `expand_home` call in
        // `create_command`
        // doesn't collapse the
        // leading `/` to `~` and
        // move the space to a
        // different spot.
        let cmd = b
            .create_command(std::path::Path::new("/var/tmp/My Work"), "work")
            .unwrap();
        assert!(cmd.contains("'/var/tmp/My Work'"), "got: {cmd}");
    }

    /// A directory name carrying a shell metacharacter must be
    /// single-quoted (via `shell_quote`), not double-quoted — POSIX
    /// double quotes still allow `$(...)`/backtick command
    /// substitution to run, which would execute arbitrary commands
    /// the moment the staged `tmux new-session -c ...` string is
    /// `eval`'d.
    #[test]
    fn tmux_backend_neutralizes_command_substitution_in_path() {
        let b = TmuxBackend;
        let cmd = b
            .create_command(std::path::Path::new("/var/tmp/foo$(touch pwned)bar"), "work")
            .unwrap();
        assert!(
            cmd.contains("'/var/tmp/foo$(touch pwned)bar'"),
            "got: {cmd}"
        );
        assert!(!cmd.contains("\"/var/tmp/foo$(touch pwned)bar\""));
    }

    #[test]
    fn tmux_send_in_pane_quotes_body() {
        let b = TmuxBackend;
        let cmd = b.send_in_pane_command("%3", "sh .command /tmp/x").unwrap();
        assert!(cmd.contains("tmux send-keys -t %3"));
        // shell_quote wraps the body
        // in single quotes, so the
        // space inside `.command
        // /tmp/x` survives intact.
        assert!(cmd.contains("'sh .command /tmp/x'"));
    }

    #[test]
    fn backend_for_tmux_is_tmux_backend() {
        let b = backend_for(MultiplexerKind::Tmux);
        assert_eq!(b.name(), "tmux");
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn backend_for_herdr_is_herdr_backend() {
        let b = backend_for(MultiplexerKind::Herdr);
        assert_eq!(b.name(), "herdr");
    }

    #[test]
    fn herdr_unavailable_only_when_feature_off() {
        if cfg!(feature = "herdr") {
            assert!(!MultiplexerKind::Herdr.is_herdr_unavailable());
        } else {
            assert!(MultiplexerKind::Herdr.is_herdr_unavailable());
        }
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_focus_command_emits_workspace_focus() {
        // The herdr backend's
        // focus command is a
        // single
        // `herdr workspace focus`
        // call (no
        // select-window /
        // select-pane pair —
        // herdr's public CLI
        // doesn't expose those
        // primitives; the
        // workspace-level
        // focus is enough).
        // The
        // `focus_command`
        // strips the
        // workspace-scoped
        // pane id's `:pN`
        // suffix because
        // `herdr workspace focus`
        // accepts a
        // workspace id,
        // not a pane id.
        let b = HerdrBackend;
        let cmd = b.focus_command("w1:p1").expect("non-empty pane id");
        assert_eq!(cmd, "herdr workspace focus w1 2>/dev/null");
        assert!(b.focus_command("").is_none());
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_create_command_uses_cwd_and_label() {
        let b = HerdrBackend;
        let cmd = b
            .create_command(std::path::Path::new("/var/tmp/build"), "build")
            .unwrap();
        assert!(cmd.contains("herdr workspace create"));
        assert!(cmd.contains("--cwd"));
        assert!(cmd.contains("/var/tmp/build"));
        assert!(cmd.contains("--label build"));
        // `--focus` must be
        // explicit so the
        // workspace is
        // auto-activated
        // after creation,
        // independent of
        // herdr's default
        // (which is
        // `--focus` today
        // but may change).
        assert!(cmd.contains("--focus"), "got: {cmd}");
        assert!(!cmd.contains("--no-focus"));
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_create_quotes_paths_with_spaces() {
        let b = HerdrBackend;
        let cmd = b
            .create_command(std::path::Path::new("/var/tmp/My Work"), "work")
            .unwrap();
        assert!(cmd.contains("'/var/tmp/My Work'"), "got: {cmd}");
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_send_in_pane_quotes_body() {
        let b = HerdrBackend;
        let cmd = b.send_in_pane_command("w1:p1", "sh .command /tmp").unwrap();
        assert!(cmd.starts_with("herdr pane send-text w1:p1"));
        // shell_quote wraps the
        // body in single quotes
        // so the space inside
        // `.command /tmp`
        // survives intact.
        assert!(cmd.contains("'sh .command /tmp'"));
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_pane_list_parses_per_pane_records() {
        // The herdr backend's
        // snapshot is built
        // from
        // `herdr pane list`
        // JSON. Each pane
        // becomes one
        // `ActiveContext` so
        // the T-marker
        // matching in
        // `directory_tmux_pane_id`
        // can find a
        // workspace for
        // directory the
        // user has an
        // active pane in.
        let json = serde_json::json!({
            "id": "cli:pane:list",
            "result": {
                "type": "pane_list",
                "panes": [
                    {
                        "pane_id": "wA:p1",
                        "workspace_id": "wA",
                        "cwd": "/Users/har",
                        "foreground_cwd": "/Users/har/work",
                        "agent": "pi"
                    },
                    {
                        "pane_id": "wB:p1",
                        "workspace_id": "wB",
                        "cwd": "/Users/har/other",
                        "foreground_cwd": "/Users/har/other",
                        "agent": ""
                    }
                ]
            }
        });
        let out = parse_herdr_pane_list(&json);
        assert_eq!(out.len(), 2);
        // `foreground_cwd`
        // wins over `cwd`
        // when present (the
        // pane's foreground
        // process changed
        // dir via `cd`).
        assert_eq!(out[0].cwd, "/Users/har/work");
        assert_eq!(out[0].workspace_id, "wA");
        assert_eq!(out[0].agent, "pi");
        // No
        // `foreground_cwd`
        // override — use
        // `cwd` verbatim.
        assert_eq!(out[1].cwd, "/Users/har/other");
        assert_eq!(out[1].agent, "");
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_pane_list_skips_empty_or_missing_cwd() {
        // Pane records
        // without a
        // resolvable cwd
        // (a brand-new
        // pane that hasn't
        // reported its
        // directory yet,
        // or a record
        // missing the
        // field) are
        // dropped from the
        // snapshot so the
        // T-marker logic
        // doesn't try to
        // match against an
        // empty path.
        let json = serde_json::json!({
            "id": "cli:pane:list",
            "result": {
                "type": "pane_list",
                "panes": [
                    {
                        "pane_id": "wA:p1",
                        "workspace_id": "wA",
                        "cwd": "",
                        "foreground_cwd": ""
                    },
                    {
                        "pane_id": "wA:p2",
                        "workspace_id": "wA"
                    }
                ]
            }
        });
        let out = parse_herdr_pane_list(&json);
        assert!(out.is_empty());
    }

    /// Regression test for the
    /// user-reported ask:
    /// show the workspace's
    /// human-readable label
    /// (e.g. `smarthistory`,
    /// `dir: Downloads`) instead
    /// of just the workspace id
    /// (`wB`) as the `#` workspace
    /// header row's primary text.
    /// `parse_workspace_labels`
    /// parses `herdr workspace list`'s
    /// JSON into a
    /// `workspace_id → label` map.
    /// The `snapshot_current_panes`
    /// code substitutes the
    /// resolved label into each
    /// `CurrentPaneInfo`'s
    /// `session_label`, so the
    /// renderer's `# {command}` text
    /// reads `smarthistory` rather
    /// than `wB`.
    #[cfg(feature = "herdr")]
    #[test]
    fn parse_workspace_labels_resolves_id_to_human_label() {
        let json = serde_json::json!({
            "id": "cli:workspace:list",
            "result": {
                "type": "workspace_list",
                "workspaces": [
                    {
                        "workspace_id": "wB",
                        "label": "smarthistory",
                        "number": 1,
                        "focused": true,
                        "pane_count": 3,
                        "tab_count": 2
                    },
                    {
                        "workspace_id": "wE",
                        "label": "dir: Downloads",
                        "number": 2,
                        "focused": false,
                        "pane_count": 2,
                        "tab_count": 1
                    }
                ]
            }
        });
        let labels = parse_workspace_labels(&json);
        assert_eq!(labels.len(), 2);
        assert_eq!(labels.get("wB").map(String::as_str), Some("smarthistory"));
        assert_eq!(labels.get("wE").map(String::as_str), Some("dir: Downloads"));
    }

    /// Workspaces with no
    /// `label` field (a
    /// brand-new herdr
    /// install that hasn't
    /// named the workspace
    /// yet, or older herdr
    /// versions that don't
    /// expose `label`) fall
    /// back to the bare id
    /// — keeps the `#` row's
    /// display non-empty
    /// rather than a blank
    /// header.
    #[cfg(feature = "herdr")]
    #[test]
    fn parse_workspace_labels_falls_back_to_id_when_label_missing() {
        let json = serde_json::json!({
            "result": {
                "panes": [],
                "workspaces": [
                    { "workspace_id": "wA" },
                    { "workspace_id": "wB", "label": "" }
                ]
            }
        });
        let labels = parse_workspace_labels(&json);
        assert_eq!(labels.len(), 2);
        // Missing `label` → fall
        // back to `workspace_id`.
        assert_eq!(labels.get("wA").map(String::as_str), Some("wA"));
        // Empty `label` → fall
        // back as well.
        assert_eq!(labels.get("wB").map(String::as_str), Some("wB"));
    }

    /// The popup-case fallback
    /// parser for
    /// `herdr_current_pane_id`.
    /// When the TUI is launched
    /// as a herdr popup, herdr
    /// may NOT pass
    /// `HERDR_PANE_ID` to the
    /// popup's process — the
    /// user's debug log shows
    /// `HERDR_PANE_ID=None` in
    /// that case. We fall
    /// back to
    /// `herdr pane current`,
    /// which returns the
    /// calling process's pane
    /// id. This test verifies
    /// the parser handles the
    /// canonical response
    /// shape.
    #[cfg(feature = "herdr")]
    #[test]
    fn parse_herdr_current_pane_resolves_canonical_response() {
        let json = serde_json::json!({
            "id": "cli:pane:current",
            "result": {
                "type": "pane_current",
                "pane": {
                    "pane_id": "w20:p27",
                    "workspace_id": "w20",
                    "tab_id": "w20:t3",
                    "focused": true,
                    "cwd": "/Users/har/smarthistory/smarthistory",
                    "foreground_cwd": "/Users/har/smarthistory/smarthistory",
                    "agent": "smarthistory-tui"
                }
            }
        });
        assert_eq!(
            parse_herdr_current_pane(&json),
            Some("w20:p27".to_string()),
            "the popup-case fallback must extract the pane id from result.pane.pane_id"
        );
    }

    /// Edge case: herdr has
    /// been observed to return
    /// an empty `pane_id`
    /// string for panes in
    /// transitional states
    /// (e.g. during a split).
    /// We MUST filter that
    /// out — falling back to
    /// the empty-string
    /// "current pane" would
    /// filter out EVERY row
    /// from the snapshot and
    /// reproduce the original
    /// "empty pane list" bug.
    /// The fallback resolver
    /// (`refresh_session_panes`)
    /// would then bail out
    /// when the empty string
    /// propagated, but it's
    /// cleaner for the parser
    /// to never produce it in
    /// the first place.
    #[cfg(feature = "herdr")]
    #[test]
    fn parse_herdr_current_pane_filters_empty_pane_id() {
        let json = serde_json::json!({
            "result": {
                "pane": {
                    "pane_id": "",
                    "workspace_id": "w20"
                }
            }
        });
        assert_eq!(
            parse_herdr_current_pane(&json),
            None,
            "empty pane_id must NOT be returned as the fallback current pane; \
             a None result causes the wrapper to bail out cleanly rather than \
             filtering every snapshot row"
        );
    }

    /// Malformed response
    /// shapes (missing
    /// `result`, missing
    /// `pane`, missing
    /// `pane_id`, non-string
    /// `pane_id`) all
    /// produce `None`. The
    /// wrapper treats `None`
    /// as "couldn't determine
    /// the current pane —
    /// bail out, the user
    /// isn't inside a
    /// multiplexer pane" and
    /// skips the snapshot
    /// fetch.
    #[cfg(feature = "herdr")]
    #[test]
    fn parse_herdr_current_pane_handles_malformed_responses() {
        // Missing `result` envelope.
        let json = serde_json::json!({
            "pane": { "pane_id": "wA:p1" }
        });
        assert_eq!(parse_herdr_current_pane(&json), None);
        // Missing `pane` field.
        let json = serde_json::json!({
            "result": { "type": "pane_current" }
        });
        assert_eq!(parse_herdr_current_pane(&json), None);
        // Missing `pane_id` field.
        let json = serde_json::json!({
            "result": { "pane": { "workspace_id": "wA" } }
        });
        assert_eq!(parse_herdr_current_pane(&json), None);
        // Non-string `pane_id`.
        let json = serde_json::json!({
            "result": { "pane": { "pane_id": 42 } }
        });
        assert_eq!(parse_herdr_current_pane(&json), None);
        // Empty object.
        let json = serde_json::json!({});
        assert_eq!(parse_herdr_current_pane(&json), None);
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_pane_list_handles_missing_result_envelope() {
        // A malformed
        // response (no
        // `result.panes`)
        // returns an empty
        // list rather than
        // panicking. This
        // is the
        // "silent failure"
        // path that keeps
        // the TUI from
        // crashing when
        // herdr's response
        // shape changes
        // between versions.
        let json = serde_json::json!({
            "id": "cli:pane:list"
        });
        let out = parse_herdr_pane_list(&json);
        assert!(out.is_empty());
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_snapshot_uses_pane_list_not_workspace_list() {
        // Regression: a
        // directory D that
        // is the cwd of an
        // existing herdr
        // pane must show
        // up in the
        // snapshot, so the
        // staging branches
        // to
        // `herdr workspace focus`
        // instead of
        // `herdr workspace create`.
        // This is the
        // user-reported bug
        // "A new workspace
        // is generated for
        // a directory which
        // is already part
        // of a workspace".
        let b = HerdrBackend;
        // We exercise the
        // parser directly
        // with a fixed
        // payload (we
        // can't easily mock
        // `herdr_run_json`
        // here — it shells
        // out) and assert
        // the resulting
        // rows match what
        // the TUI would
        // see.
        let json = serde_json::json!({
            "id": "cli:pane:list",
            "result": {
                "type": "pane_list",
                "panes": [
                    {
                        "pane_id": "wA:p1",
                        "workspace_id": "wA",
                        "cwd": "/Users/har/work",
                        "foreground_cwd": "/Users/har/work",
                        "agent": ""
                    }
                ]
            }
        });
        let rows = parse_herdr_pane_list(&json);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cwd, "/Users/har/work");
        assert_eq!(rows[0].workspace_id, "wA");
        // And
        // `focus_command`
        // must strip the
        // `:pN` suffix
        // before passing
        // to herdr.
        let staged = b.focus_command("wA:p1").expect("non-empty pane id");
        assert_eq!(staged, "herdr workspace focus wA 2>/dev/null");
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_focus_command_strips_pane_suffix() {
        // herdr's
        // `workspace focus`
        // accepts a
        // workspace id
        // (`wA`), not a
        // pane id
        // (`wA:p1`). The
        // snapshot rows
        // carry pane ids,
        // so the staging
        // must strip the
        // suffix.
        let b = HerdrBackend;
        assert_eq!(
            b.focus_command("wA:p1").unwrap(),
            "herdr workspace focus wA 2>/dev/null"
        );
        assert_eq!(
            b.focus_command("wB:p3").unwrap(),
            "herdr workspace focus wB 2>/dev/null"
        );
        // A bare workspace
        // id (no `:pN`
        // suffix) is
        // passed through
        // unchanged.
        assert_eq!(
            b.focus_command("wA").unwrap(),
            "herdr workspace focus wA 2>/dev/null"
        );
        // Empty / blank
        // inputs are
        // rejected so the
        // staging layer
        // doesn't produce
        // a malformed
        // command.
        assert!(b.focus_command("").is_none());
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_focus_session_emits_workspace_focus() {
        // Selecting
        // a workspace header
        // row (the user
        // picks the whole
        // workspace, not
        // a pane inside it)
        // stages
        // `herdr workspace focus <id>`.
        // The `session_label`
        // for herdr is the
        // workspace id
        // itself, so the
        // command is the
        // same as the
        // directories-mode
        // T-marker staging
        // (which uses
        // `focus_command` on
        // the workspace-scoped
        // pane id, stripping
        // the `:pN` suffix).
        let b = HerdrBackend;
        assert_eq!(
            b.focus_session("wA").unwrap(),
            "herdr workspace focus wA 2>/dev/null"
        );
        assert!(b.focus_session("").is_none());
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_focus_pane_uses_pane_zoom() {
        // Selecting a pane row stages
        // `herdr pane zoom <pane_id> && herdr pane zoom <pane_id> --off`.
        // The first `pane zoom` call focuses the EXACT pane
        // (across workspaces and tabs) and zooms it to fill
        // the tab. The second call (`--off`) un-zooms while
        // keeping the focus on that pane, so the user lands
        // on the right pane without a zoomed view.
        //
        // This replaces the old `workspace focus + tab focus`
        // approach, which only switched the workspace and tab
        // but left the pane-focus to whatever was last focused
        // in that tab.
        let b = HerdrBackend;
        let cmd = b.focus_pane("wA:p3", "wA:t2").expect("non-empty ids");
        assert_eq!(
            cmd,
            "herdr pane zoom wA:p3 2>/dev/null && herdr pane zoom wA:p3 --off 2>/dev/null"
        );
        // An empty `tab_id`
        // doesn't change the
        // behavior — `pane zoom`
        // resolves the workspace
        // and tab from the
        // pane_id itself.
        let cmd = b.focus_pane("wA:p3", "").expect("non-empty pane id");
        assert_eq!(
            cmd,
            "herdr pane zoom wA:p3 2>/dev/null && herdr pane zoom wA:p3 --off 2>/dev/null"
        );
        // An empty `pane_id`
        // is rejected.
        assert!(b.focus_pane("", "").is_none());
        // A bare workspace
        // id (no `:pN`)
        // still produces a
        // valid command —
        // `pane zoom` accepts
        // workspace ids too
        // (it will focus the
        // workspace's
        // focused-pane-by-
        // default).
        let cmd = b.focus_pane("wA", "wA:t1").expect("bare ws id");
        assert_eq!(
            cmd,
            "herdr pane zoom wA 2>/dev/null && herdr pane zoom wA --off 2>/dev/null"
        );
    }

    #[test]
    fn tmux_focus_session_uses_switch_client() {
        // Selecting a session
        // header row in the
        // `*` mode for a tmux
        // user stages
        // `tmux switch-client -t <session-name>`
        // which brings the
        // session's focused
        // window forward.
        let b = TmuxBackend;
        assert_eq!(b.focus_session("0").unwrap(), "tmux switch-client -t 0");
        assert_eq!(
            b.focus_session("my-session").unwrap(),
            "tmux switch-client -t my-session"
        );
        assert!(b.focus_session("").is_none());
    }

    #[test]
    fn tmux_focus_pane_reuses_focus_command() {
        // For tmux the per-pane
        // focus is the same
        // shape as the
        // directories-mode
        // T-marker focus:
        // `select-pane -t <pane_id> && switch-client -t <pane_id>`.
        // The `tab_id` (window
        // id `@N`) is ignored
        // because tmux's
        // `switch-client -t %N`
        // already switches the
        // window for you.
        let b = TmuxBackend;
        let cmd = b.focus_pane("%5", "@3").expect("non-empty pane id");
        assert_eq!(cmd, "tmux select-pane -t %5 && tmux switch-client -t %5");
        assert_eq!(
            b.focus_pane("%5", "").unwrap(),
            "tmux select-pane -t %5 && tmux switch-client -t %5"
        );
        assert!(b.focus_pane("", "").is_none());
    }

    /// Live integration test:
    /// runs the actual
    /// `herdr pane list` CLI
    /// parse path via
    /// `HerdrBackend::snapshot_current_panes`
    /// and asserts that the
    /// returned count
    /// is at least equal to
    /// (`herdr pane list`'s
    /// panes minus one for the
    /// current pane). This
    /// is the diagnostic
    /// for the user-reported
    /// bug where only some
    /// workspaces' panes
    /// showed up in the `*`
    /// mode list.
    ///
    /// Skipped when `HERDR_PANE_ID`
    /// is unset (the test
    /// suite isn't running
    /// inside a herdr pane)
    /// so CI doesn't fail
    /// when herdr isn't
    /// installed.
    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_backend_snapshot_current_panes_returns_all_workspaces() {
        let current_pane = std::env::var("HERDR_PANE_ID")
            .ok()
            .filter(|s| !s.is_empty());
        let Some(current_pane) = current_pane else {
            eprintln!("[skip] $HERDR_PANE_ID unset (not in herdr)");
            return;
        };
        // Use the same JSON the production code reads.
        let out = match std::process::Command::new("herdr")
            .args(["pane", "list"])
            .output()
        {
            Ok(o) => o,
            Err(_) => {
                eprintln!("[skip] `herdr` not on PATH");
                return;
            }
        };
        let json: serde_json::Value = match serde_json::from_slice(&out.stdout) {
            Ok(j) => j,
            Err(_) => {
                eprintln!("[skip] `herdr pane list` returned non-JSON output");
                return;
            }
        };
        let expected_count = json
            .get("result")
            .and_then(|r| r.get("panes"))
            .and_then(|p| p.as_array())
            .map(|ps| {
                ps.iter()
                    .filter(|p| {
                        p.get("pane_id")
                            .and_then(|v| v.as_str())
                            .map(|s| s != current_pane)
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);
        if expected_count == 0 {
            eprintln!("[skip] no non-current panes in `herdr pane list`");
            return;
        }
        // Run the backend's snapshot for the current pane.
        let b = HerdrBackend;
        let rows = b.snapshot_current_panes(&current_pane);
        eprintln!(
            "[debug] backend returned {} rows for current pane {:?} (expected {} from `herdr pane list`)",
            rows.len(),
            current_pane,
            expected_count
        );
        let mut workspaces_seen: Vec<String> = Vec::new();
        for r in &rows {
            if !workspaces_seen.contains(&r.session_label) {
                workspaces_seen.push(r.session_label.clone());
            }
            eprintln!(
                "[debug]   pane_id={:?} session_label={:?} cwd={:?} tab_id={:?}",
                r.pane_id, r.session_label, r.path, r.tab_id
            );
        }
        eprintln!(
            "[debug] workspaces represented in backend output: {:?}",
            workspaces_seen
        );
        // Every pane from `herdr pane list`
        // (excluding the current one)
        // must survive the JSON parse
        // path. This catches the case where
        // a single workspace's panes are
        // dropped (the user's bug).
        assert_eq!(
            rows.len(),
            expected_count,
            "backend snapshot returned {} rows but `herdr pane list` had {} (current pane {:?} excluded). \
             A mismatch means parse_herdr_pane_list is dropping some rows; \
             check the per-row debug output above.",
            rows.len(),
            expected_count,
            current_pane
        );
    }
