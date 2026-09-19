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
    fn herdr_focus_pane_uses_socket_pane_focus() {
        // Selecting a pane row stages a call to smarthistory's own
        // `herdr focus-pane` helper, which wraps the socket API's
        // `pane.focus` method.
        //
        // This replaces a previous implementation that staged
        // `herdr pane zoom <pane_id> && herdr pane zoom <pane_id>
        // --off`. That used a zoom toggle as a focus primitive, and
        // it silently did nothing whenever the target pane was the
        // only pane in its tab: `pane.zoom` then reports
        // `reason: "single_pane"` and leaves focus where it was, so
        // the user stayed in the pane they started from. There is no
        // CLI replacement either — `herdr pane focus` is directional
        // (`--direction` required), `herdr tab focus` is tab-scoped,
        // and `herdr agent focus` takes only agent rows — so the
        // socket method is the only way to hit a specific pane id.
        let b = HerdrBackend;
        let cmd = b.focus_pane("wA:p3", "wA:t2").expect("non-empty ids");
        assert_eq!(cmd, "smarthistory herdr focus-pane wA:p3");
        // The `tab_id` is unused (`pane.focus` derives the workspace
        // and tab from the pane id), so an empty one changes nothing.
        let cmd = b.focus_pane("wA:p3", "").expect("non-empty pane id");
        assert_eq!(cmd, "smarthistory herdr focus-pane wA:p3");
        // An empty `pane_id` is rejected — there's nothing to focus,
        // and `pane.focus` would error on it anyway.
        assert!(b.focus_pane("", "").is_none());
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_socket_path_prefers_env_then_named_session_then_default() {
        // The staged `smarthistory herdr focus-pane` call resolves
        // its socket in the user's shell, where `HERDR_SOCKET_PATH`
        // is set by herdr for every managed pane. This pins the
        // documented fallback order for the cases where it isn't:
        // named session socket, then the default under the herdr
        // config dir (honouring `XDG_CONFIG_HOME`).
        //
        // The resolution is exercised through the pure helper so the
        // test never mutates the process environment — several other
        // tests spawn real `herdr`/`tmux` subprocesses, which would
        // inherit any temporary `HERDR_SOCKET_PATH` set here.
        let resolve = |socket: Option<&str>, session: Option<&str>| {
            resolve_herdr_socket_path(socket, session, Some("/tmp/xdg"), Some("/tmp/home"))
        };
        // 1. An explicit path always wins.
        assert_eq!(
            resolve(Some("/tmp/explicit.sock"), Some("named")).unwrap(),
            std::path::PathBuf::from("/tmp/explicit.sock")
        );
        // 2. Without it, a named session gets its own socket.
        assert_eq!(
            resolve(None, Some("named")).unwrap(),
            std::path::PathBuf::from("/tmp/xdg/herdr/sessions/named/herdr.sock")
        );
        // 3. Without a session name, the default session socket.
        assert_eq!(
            resolve(None, None).unwrap(),
            std::path::PathBuf::from("/tmp/xdg/herdr/herdr.sock")
        );
        // 4. With no `XDG_CONFIG_HOME`, fall back to `$HOME/.config`.
        assert_eq!(
            resolve_herdr_socket_path(None, None, None, Some("/tmp/home")).unwrap(),
            std::path::PathBuf::from("/tmp/home/.config/herdr/herdr.sock")
        );
        // Empty values are treated as unset rather than producing
        // nonsense paths like `/herdr/sessions//herdr.sock`.
        assert_eq!(
            resolve_herdr_socket_path(Some(""), Some(""), Some(""), Some("/tmp/home")).unwrap(),
            std::path::PathBuf::from("/tmp/home/.config/herdr/herdr.sock")
        );
        // With neither a socket path nor any way to find a config
        // dir, there is nothing to connect to.
        assert!(resolve_herdr_socket_path(None, None, None, None).is_none());
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_focus_pane_rejects_empty_and_unusable_socket() {
        // An empty pane id is rejected before any I/O — there is
        // nothing to focus.
        assert!(herdr_focus_pane("").is_err());
        // No resolvable socket path is an error, not a silent no-op.
        let err = herdr_focus_pane_at("wA:p1", None).expect_err("no socket path must fail");
        assert!(
            err.contains("could not resolve"),
            "expected a 'could not resolve' diagnostic, got: {err}"
        );
        // A socket path that doesn't exist must produce an error (so
        // the staged command exits non-zero and the user sees a
        // message) rather than silently appearing to succeed. The
        // path is injected rather than set through the environment,
        // so this can't race with other tests that spawn real
        // `herdr`/`tmux` subprocesses.
        let missing = std::path::PathBuf::from("/tmp/definitely-not-a-real-herdr.sock");
        assert!(!missing.exists(), "test fixture path must not exist");
        let err = herdr_focus_pane_at("wA:p1", Some(missing)).expect_err("missing socket must fail");
        assert!(
            err.contains("does not exist"),
            "expected a 'does not exist' diagnostic, got: {err}"
        );
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_focus_pane_succeeds_against_a_live_socket() {
        // End-to-end proof of the transport, not just its error
        // paths: stand up a real Unix socket that answers
        // `pane.focus` the way herdr does, and confirm the helper
        // reports success — and that the request actually reached
        // the server with the right method and pane id.
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!(
            "smarthistory_herdr_sock_{}_{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("herdr.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            // Reply in herdr's documented `pane_info` shape.
            let pane_id = req["params"]["pane_id"].as_str().unwrap().to_string();
            let reply = serde_json::json!({
                "id": req["id"],
                "result": {
                    "type": "pane_info",
                    "pane": { "pane_id": pane_id, "focused": true }
                }
            });
            stream.write_all(format!("{reply}\n").as_bytes()).unwrap();
            stream.flush().unwrap();
            // Hand the raw request back so the test can assert on it.
            line
        });

        herdr_focus_pane_at("wA:p1", Some(sock.clone())).expect("live socket must succeed");

        let raw_request = handle.join().unwrap();
        let req: serde_json::Value = serde_json::from_str(&raw_request).unwrap();
        assert_eq!(
            req["method"], "pane.focus",
            "must call the pane.focus method, got request {raw_request}"
        );
        assert_eq!(
            req["params"]["pane_id"], "wA:p1",
            "must ask for the pane the user selected, got request {raw_request}"
        );

        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "herdr")]
    #[test]
    fn herdr_focus_pane_reports_error_envelope_as_failure() {
        // herdr answers an unknown pane with an `error` envelope
        // (`pane_not_found`) rather than a `result`. That must NOT
        // be treated as a successful focus.
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!(
            "smarthistory_herdr_err_{}_{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("herdr.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let reply = serde_json::json!({
                "id": "smarthistory",
                "error": {
                    "code": "pane_not_found",
                    "message": "pane wZZ:p9 not found"
                }
            });
            stream.write_all(format!("{reply}\n").as_bytes()).unwrap();
            stream.flush().unwrap();
        });

        let err = herdr_focus_pane_at("wZZ:p9", Some(sock.clone()))
            .expect_err("an error envelope must not count as success");
        assert!(
            err.contains("wZZ:p9"),
            "diagnostic should name the pane, got: {err}"
        );
        handle.join().unwrap();

        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir_all(&dir);
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
