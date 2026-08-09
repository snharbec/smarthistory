    use super::*;

    /// `format_diff` uses a calendar-month ladder before falling back
    /// to smaller units. Each test pins a specific scenario so a
    /// regression in the ordering or the unit suffix is caught.
    #[test]
    fn format_diff_seconds() {
        let five_sec_ago = chrono::Utc::now() - chrono::Duration::seconds(5);
        assert_eq!(format_diff(five_sec_ago.timestamp()), "5s");
    }

    #[test]
    fn format_diff_minutes() {
        let three_min_ago = chrono::Utc::now() - chrono::Duration::minutes(3);
        assert_eq!(format_diff(three_min_ago.timestamp()), "3m");
    }

    #[test]
    fn format_diff_hours() {
        let two_hours_ago = chrono::Utc::now() - chrono::Duration::hours(2);
        assert_eq!(format_diff(two_hours_ago.timestamp()), "2h");
    }

    #[test]
    fn format_diff_days() {
        let five_days_ago = chrono::Utc::now() - chrono::Duration::days(5);
        assert_eq!(format_diff(five_days_ago.timestamp()), "5d");
    }

    #[test]
    fn format_diff_zero_or_negative_is_na() {
        // 0 and negative timestamps are treated as missing data and
        // sort as the oldest possible entries (9999 months).
        assert_eq!(format_diff(0), "9999M");
        assert_eq!(format_diff(-1), "9999M");
    }

    /// `qualify_field` must reject any name outside the known
    /// `history`/`command_comments`/`history_output` columns rather
    /// than splicing it straight into the SQL text — an unvalidated
    /// name from `--fields` is a SQL injection primitive (a value
    /// containing a `--` comment marker could terminate the SELECT
    /// list and append arbitrary SQL).
    #[test]
    fn qualify_field_rejects_unknown_names() {
        assert_eq!(
            qualify_field("id FROM history h UNION SELECT sql FROM sqlite_master --"),
            "h.command"
        );
    }

    #[test]
    fn qualify_field_accepts_known_columns() {
        assert_eq!(qualify_field("command"), "h.command");
        assert_eq!(qualify_field("directory"), "h.directory");
        assert_eq!(qualify_field("session_id"), "h.session_id");
        assert_eq!(qualify_field("exit_code"), "h.exit_code");
        assert_eq!(qualify_field("timestamp"), "h.timestamp");
        assert_eq!(qualify_field("mode"), "h.mode");
        assert_eq!(qualify_field("id"), "h.id");
        assert_eq!(qualify_field("comment"), "c.comment");
        assert_eq!(qualify_field("output"), "o.output");
    }

    /// `migrate_history_comment_column` rebuilds the `history` table
    /// (rename-old / create-new / copy / drop-old) for databases
    /// carrying the legacy per-row `comment` column. The rebuilt
    /// table must come out of the migration with
    /// `idx_history_dedup` already in place — otherwise the first
    /// `INSERT ... ON CONFLICT (command, directory, session_id)`
    /// upsert issued against it (e.g. by `Commands::Add`, run by the
    /// zsh precmd hook on the very next prompt) fails because the
    /// `ON CONFLICT` target constraint doesn't exist yet.
    #[test]
    fn migrate_history_comment_column_recreates_dedup_index() {
        let conn = Connection::open_in_memory().unwrap();
        // Build the legacy schema by hand: a `history` table with
        // the old per-row `comment` column, no dedup index yet.
        conn.execute(
            "CREATE TABLE history (
                id INTEGER PRIMARY KEY,
                command TEXT NOT NULL,
                directory TEXT NOT NULL,
                session_id TEXT NOT NULL,
                exit_code INTEGER,
                timestamp INTEGER,
                comment TEXT
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE command_comments (command TEXT PRIMARY KEY, comment TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO history (command, directory, session_id, exit_code, timestamp, comment)
             VALUES ('ls', '/tmp', 'sess-1', 0, 1000, 'a comment')",
            [],
        )
        .unwrap();

        migrate_history_comment_column(&conn).unwrap();

        let has_index: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_history_dedup'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap();
        assert!(has_index, "idx_history_dedup must exist after migration");

        // The index existing is necessary but the real regression
        // test is that the upsert the precmd hook relies on actually
        // works against the rebuilt table.
        conn.execute(
            "INSERT INTO history (command, directory, session_id, exit_code, timestamp)
             VALUES ('ls', '/tmp', 'sess-1', 0, 2000)
             ON CONFLICT (command, directory, session_id)
             DO UPDATE SET exit_code = excluded.exit_code, timestamp = excluded.timestamp",
            [],
        )
        .expect("upsert must succeed against the rebuilt table");
    }

    /// Build the minimal schema `build_search_where_clause`'s SQL
    /// needs: `history` plus the `command_comments` table it LEFT
    /// JOINs (the comment column it optionally matches against).
    fn search_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE history (
                id INTEGER PRIMARY KEY,
                command TEXT NOT NULL,
                directory TEXT NOT NULL,
                session_id TEXT NOT NULL,
                exit_code INTEGER,
                timestamp INTEGER
            );
             CREATE TABLE command_comments (command TEXT PRIMARY KEY, comment TEXT NOT NULL);
             CREATE TABLE history_output (history_id INTEGER PRIMARY KEY, output TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO history (command, directory, session_id, exit_code, timestamp) VALUES
             ('ls -la', '/tmp', 's1', 0, 1),
             ('open \"http://paperless.fritz.box:8000/documents/2738/details\"', '/tmp', 's1', 0, 2),
             ('cargo build --release', '/tmp', 's1', 0, 3)",
            [],
        )
        .unwrap();
        conn
    }

    /// Reproduces the exact false positive reported against the live
    /// dropdown: a plain substring search for "ls" matches
    /// `open "http://.../details"` because it contains "ls" mid-word
    /// (inside "details"). `--prefix` (the `prefix_only` flag on
    /// `build_search_where_clause`) must exclude it.
    #[test]
    fn build_search_where_clause_substring_matches_unrelated_mid_word_hit() {
        let conn = search_test_db();
        let (where_clause, params) =
            build_search_where_clause(Some("ls"), None, false, None, false);
        let sql = format!("SELECT h.command{}", where_clause);
        let mut stmt = conn.prepare(&sql).unwrap();
        let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let matches: Vec<String> = stmt
            .query_map(&params_ref[..], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            matches,
            vec![
                "ls -la".to_string(),
                "open \"http://paperless.fritz.box:8000/documents/2738/details\"".to_string(),
            ],
            "default substring search reproduces the reported false positive: {:?}",
            matches
        );
    }

    #[test]
    fn build_search_where_clause_prefix_only_excludes_mid_word_hit() {
        let conn = search_test_db();
        let (where_clause, params) =
            build_search_where_clause(Some("ls"), None, false, None, true);
        let sql = format!("SELECT h.command{}", where_clause);
        let mut stmt = conn.prepare(&sql).unwrap();
        let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let matches: Vec<String> = stmt
            .query_map(&params_ref[..], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            matches,
            vec!["ls -la".to_string()],
            "prefix_only must exclude the mid-word 'ls' inside 'details', got: {:?}",
            matches
        );
    }

    #[test]
    fn build_search_where_clause_prefix_only_matches_actual_prefix() {
        let conn = search_test_db();
        let (where_clause, params) =
            build_search_where_clause(Some("open"), None, false, None, true);
        let sql = format!("SELECT h.command{}", where_clause);
        let mut stmt = conn.prepare(&sql).unwrap();
        let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let matches: Vec<String> = stmt
            .query_map(&params_ref[..], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            matches,
            vec!["open \"http://paperless.fritz.box:8000/documents/2738/details\"".to_string()],
            "a genuine prefix match must still be found, got: {:?}",
            matches
        );
    }

    /// `resolve_comment` (backing `smarthistory expand`, the zsh
    /// comment-expansion widget) must pick the most recently run
    /// command when two different commands share the exact same
    /// comment text.
    #[test]
    fn resolve_comment_picks_most_recent_when_comment_is_shared() {
        let conn = search_test_db();
        conn.execute_batch(
            "INSERT INTO command_comments (command, comment) VALUES
             ('ls -la', 'deploy'),
             ('cargo build --release', 'deploy');",
        )
        .unwrap();
        // `search_test_db` gives 'ls -la' timestamp 1 and
        // 'cargo build --release' timestamp 3 — the newer one.
        assert_eq!(
            resolve_comment(&conn, "deploy").unwrap(),
            Some("cargo build --release".to_string())
        );
    }

    #[test]
    fn resolve_comment_matches_case_insensitively() {
        let conn = search_test_db();
        conn.execute(
            "INSERT INTO command_comments (command, comment) VALUES ('ls -la', 'Deploy')",
            [],
        )
        .unwrap();
        assert_eq!(
            resolve_comment(&conn, "deploy").unwrap(),
            Some("ls -la".to_string())
        );
    }

    #[test]
    fn resolve_comment_no_match_returns_none() {
        let conn = search_test_db();
        assert_eq!(resolve_comment(&conn, "nope").unwrap(), None);
    }

    /// A comment that is only a substring of the typed word (or vice
    /// versa) must NOT match — `resolve_comment` is exact-match only,
    /// unlike the substring search `build_search_where_clause` does.
    #[test]
    fn resolve_comment_does_not_substring_match() {
        let conn = search_test_db();
        conn.execute(
            "INSERT INTO command_comments (command, comment) VALUES ('ls -la', 'deploy')",
            [],
        )
        .unwrap();
        assert_eq!(resolve_comment(&conn, "deploy-prod").unwrap(), None);
        assert_eq!(resolve_comment(&conn, "dep").unwrap(), None);
    }

    /// Build the minimal schema `import_history_rows` needs
    /// (`history` with its dedup index, plus the two side tables).
    fn import_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE history (
                id INTEGER PRIMARY KEY,
                command TEXT NOT NULL,
                directory TEXT NOT NULL,
                session_id TEXT NOT NULL,
                exit_code INTEGER,
                timestamp INTEGER,
                mode TEXT NOT NULL DEFAULT 'command'
            );
            CREATE UNIQUE INDEX idx_history_dedup ON history (command, directory, session_id);
            CREATE TABLE command_comments (command TEXT PRIMARY KEY, comment TEXT NOT NULL);
            CREATE TABLE history_output (
                history_id INTEGER PRIMARY KEY,
                output TEXT NOT NULL,
                captured_at INTEGER,
                FOREIGN KEY (history_id) REFERENCES history(id)
            );",
        )
        .unwrap();
        conn
    }

    fn export_row(command: &str, directory: &str, session_id: &str) -> HistoryExportRow {
        HistoryExportRow {
            id: None,
            command: command.to_string(),
            directory: directory.to_string(),
            session_id: session_id.to_string(),
            exit_code: 0,
            timestamp: 1000,
            mode: "command".to_string(),
            comment: None,
            output: None,
        }
    }

    /// A fresh row (no existing `(command, directory, session_id)`
    /// match) must count as imported, not updated.
    #[test]
    fn import_history_rows_counts_fresh_rows_as_imported() {
        let conn = import_test_db();
        let rows = vec![export_row("ls", "/tmp", "sess-1")];
        let (imported, updated) = import_history_rows(&conn, &rows).unwrap();
        assert_eq!((imported, updated), (1, 0));
    }

    /// Re-importing the same `(command, directory, session_id)` must
    /// count as updated, not imported — `INSERT ... ON CONFLICT DO
    /// UPDATE` reports the same changed-row count (1) for both
    /// cases, so `import_history_rows` must distinguish them via an
    /// existence check rather than trusting that count.
    #[test]
    fn import_history_rows_counts_reimported_rows_as_updated() {
        let conn = import_test_db();
        let rows = vec![export_row("ls", "/tmp", "sess-1")];
        import_history_rows(&conn, &rows).unwrap();

        let (imported, updated) = import_history_rows(&conn, &rows).unwrap();
        assert_eq!(
            (imported, updated),
            (0, 1),
            "re-importing the same row must report it as updated, not imported"
        );
    }

    #[test]
    fn import_history_rows_mixed_batch_counts_each_correctly() {
        let conn = import_test_db();
        import_history_rows(&conn, &[export_row("ls", "/tmp", "sess-1")]).unwrap();

        let (imported, updated) = import_history_rows(
            &conn,
            &[
                export_row("ls", "/tmp", "sess-1"),  // pre-existing -> updated
                export_row("pwd", "/tmp", "sess-1"), // new -> imported
            ],
        )
        .unwrap();
        assert_eq!((imported, updated), (1, 1));
    }

    #[test]
    fn format_base_leaf_dir() {
        assert_eq!(format_base("/Users/har/projects/notes"), "notes");
        assert_eq!(format_base("/tmp"), "tmp");
        assert_eq!(format_base("/"), "/");
    }

    #[test]
    fn format_base_empty_string() {
        // Path::file_name of "" returns None; the fallback returns the
        // input unchanged.
        assert_eq!(format_base(""), "");
    }

    #[test]
    fn highlight_empty_needle() {
        // An empty needle should not modify the haystack.
        assert_eq!(highlight("hello world", "", "[", "]"), "hello world");
    }

    #[test]
    fn highlight_wraps_all_occurrences() {
        assert_eq!(highlight("foo bar foo", "foo", "[", "]"), "[foo] bar [foo]");
    }

    #[test]
    fn highlight_no_occurrences() {
        // When the needle doesn't appear, the haystack is returned
        // unchanged.
        assert_eq!(highlight("hello world", "xyz", "[", "]"), "hello world");
    }

    #[test]
    fn highlight_empty_haystack() {
        assert_eq!(highlight("", "foo", "[", "]"), "");
    }

    #[test]
    fn highlight_at_start() {
        assert_eq!(highlight("foo bar", "foo", "[", "]"), "[foo] bar");
    }

    #[test]
    fn highlight_at_end() {
        assert_eq!(highlight("bar foo", "foo", "[", "]"), "bar [foo]");
    }

    #[test]
    fn generate_uuid_v4_format() {
        // The output must be 36 characters in the canonical
        // `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` form.
        let u = generate_uuid_v4();
        assert_eq!(u.len(), 36, "UUID has unexpected length: {u}");
        assert_eq!(u.as_bytes()[8], b'-');
        assert_eq!(u.as_bytes()[13], b'-');
        assert_eq!(u.as_bytes()[18], b'-');
        assert_eq!(u.as_bytes()[23], b'-');
        // The 13th hex char (index 14) is the version nibble; for
        // v4 it must be '4'.
        assert_eq!(
            u.as_bytes()[14],
            b'4',
            "UUID version nibble is not '4' in {u}"
        );
        // The 17th hex char (index 19) is the variant nibble; for
        // RFC 4122 it must be one of 8/9/a/b.
        let variant = u.as_bytes()[19];
        assert!(
            matches!(variant, b'8' | b'9' | b'a' | b'b'),
            "UUID variant nibble is invalid in {u}: {:?}",
            variant as char
        );
    }

    #[test]
    fn generate_uuid_v4_uniqueness() {
        // Two successive calls must return different UUIDs (the
        // counter + process start instant + wall clock provides more
        // than enough entropy for this to never collide).
        let u1 = generate_uuid_v4();
        let u2 = generate_uuid_v4();
        let u3 = generate_uuid_v4();
        assert_ne!(u1, u2);
        assert_ne!(u2, u3);
        assert_ne!(u1, u3);
    }

    fn write_temp_log(contents: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("smarthistory-tmux-test-{}.log", generate_uuid_v4()));
        std::fs::write(&path, contents).expect("write log");
        path
    }

    #[test]
    fn extract_tmux_output_uses_last_match() {
        let log = "some other line\necho first\necho first output\nrandom line\necho first again\nlast output\n";
        let path = write_temp_log(log);
        let out = extract_tmux_output("echo first again", &path, Some(MAX_OUTPUT_LINES))
            .expect("extract");
        assert!(out.contains("echo first again"));
        assert!(out.contains("last output"));
        assert!(!out.contains("echo first output"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_tmux_output_caps_at_twenty_lines() {
        let mut log = String::from("$ mycommand\n");
        for i in 0..30 {
            log.push_str(&format!("line {}\n", i));
        }
        let path = write_temp_log(&log);
        let out = extract_tmux_output("mycommand", &path, Some(MAX_OUTPUT_LINES)).expect("extract");
        let count = out.lines().count();
        // The slice includes the command line itself plus up to
        // MAX_OUTPUT_LINES following lines.
        assert_eq!(count, MAX_OUTPUT_LINES + 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_tmux_output_retries_until_match() {
        // Write a log without the command, then append the command
        // after a short delay. The retry loop should pick it up.
        let path = write_temp_log("initial content\nno match here\n");
        let path_clone = path.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(250));
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path_clone)
                .expect("open");
            writeln!(f, "before cmd").unwrap();
            writeln!(f, "$ delayedcmd").unwrap();
            writeln!(f, "output line 1").unwrap();
        });
        let out =
            extract_tmux_output("delayedcmd", &path, Some(MAX_OUTPUT_LINES)).expect("extract");
        handle.join().unwrap();
        assert!(out.contains("delayedcmd"));
        assert!(out.contains("output line 1"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_tmux_output_prefers_prompt_line_over_output() {
        // The command `echo ls` produces an output line that is
        // just `ls`. The search must prefer the prompt+command line
        // (`$ echo ls`) so that the captured slice starts at the
        // command, not at the output line.
        let log = "$ echo ls
ls
$ echo next
next output
";
        let path = write_temp_log(log);
        let out = extract_tmux_output("echo ls", &path, Some(MAX_OUTPUT_LINES)).expect("extract");
        // Must start with the prompt+command line, not the bare
        // output line.
        assert!(out.starts_with("$ echo ls"), "got: {out}");
        // The captured window should be at most 21 lines
        // (command + 20 following).
        assert!(out.lines().count() <= MAX_OUTPUT_LINES + 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_tmux_output_does_not_match_just_output_line() {
        // A bare output line that happens to equal the command text
        // must not be picked when a prompt+command line is also
        // present later. We rely on the end-of-line heuristic to
        // skip the bare output line.
        let log = "some output
ls
$ echo ls
real output
";
        let path = write_temp_log(log);
        let out = extract_tmux_output("echo ls", &path, Some(MAX_OUTPUT_LINES)).expect("extract");
        assert!(out.starts_with("$ echo ls"), "got: {out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_tmux_output_strips_ansi_before_matching() {
        // The prompt contains ANSI colour codes; the command line
        // after stripping is `$ ls -la`, which ends with the command.
        let log = "\x1b[32m$\x1b[0m ls -la
file1
file2
";
        let path = write_temp_log(log);
        let out = extract_tmux_output("ls -la", &path, Some(MAX_OUTPUT_LINES)).expect("extract");
        assert!(out.contains("ls -la"));
        assert!(out.contains("file1"));
        assert!(!out.contains("\x1b["), "ANSI should be stripped: {out}");
        std::fs::remove_file(&path).ok();
    }

    /// `extract_pane_output` is the source-agnostic core
    /// shared by `capture-tmux` (file) and
    /// `capture-herdr` (scrollback). It receives
    /// pre-stripped ANSI-clean lines and returns the
    /// command line + N following lines.
    #[test]
    fn extract_pane_output_finds_command_and_captures_output() {
        let lines: Vec<String> = vec![
            "some earlier output".to_string(),
            r#"$ echo hello"#.to_string(),
            "hello world".to_string(),
            r#"$ "#.to_string(),
        ];
        let out = extract_pane_output("echo hello", &lines, Some(20)).expect("extract");
        assert!(out.contains("echo hello"));
        assert!(out.contains("hello world"));
    }

    /// When `ALL` (None) is requested, the output
    /// runs until the next prompt boundary.
    #[test]
    fn extract_pane_output_unlimited_caps_at_next_prompt() {
        let lines: Vec<String> = vec![
            r#"$ ls"#.to_string(),
            "file1.txt".to_string(),
            "file2.txt".to_string(),
            r#"$ "#.to_string(),
            "next command".to_string(),
        ];
        let out = extract_pane_output("ls", &lines, None).expect("extract");
        assert!(out.contains("file1.txt"));
        assert!(out.contains("file2.txt"));
        // The prompt line and the next command
        // should NOT be included.
        assert!(!out.contains("next command"));
    }

    #[test]
    fn strip_ansi_removes_csi_and_osc() {
        let input = "before\x1b[32mgreen\x1b[0m after\x1b]0;title\x07end";
        let out = strip_ansi(input);
        assert_eq!(out, "beforegreen afterend");
    }

    #[test]
    fn strip_ansi_handles_bracketed_paste_prompt() {
        // Real-world zsh prompt with bracketed-paste markers, mode
        // switches and a BEL (from tab completion) interleaved with
        // the command line for `head README.md`. The BEL is stripped
        // along with all C0 control characters; the resulting line
        // ends with the actual command.
        let input = "har@arrakis.fritz.box in ~/smarthistory/smarthistory\x07\x1b[K\x1b[?1h\x1b=\x1b[?2004h\x1b[32mhead\x1b[39m \x1b[4mREADME.md\x1b[24m\x1b[?1l\x1b>\x1b[?2004l";
        let out = strip_ansi(input);
        // The BEL is removed and the prompt+command collapse together.
        assert_eq!(
            out,
            "har@arrakis.fritz.box in ~/smarthistory/smarthistoryhead README.md"
        );
        assert!(out.trim_end().ends_with("head README.md"));
    }

    #[test]
    fn first_token_strips_whitespace() {
        assert_eq!(first_token("ls -la"), "ls");
        assert_eq!(first_token("  vim"), "vim");
        assert_eq!(first_token("echo hello world"), "echo");
        assert_eq!(first_token(""), "");
    }

    #[test]
    fn config_default_contains_no_capture() {
        let cfg = Config::default();
        for cmd in DEFAULT_NO_CAPTURE {
            assert!(cfg.ignore_capture.contains(*cmd), "default {cmd} missing");
        }
    }

    #[test]
    fn config_parses_user_file() {
        // `$HOME` is process-global and `cargo test` runs every
        // test in the crate in one process, so this holds the
        // same lock `src/util.rs`'s and `src/tui/tests.rs`'s own
        // `$HOME`-mutating tests use — see
        // `crate::tui::tests::ENV_LOCK`'s doc comment.
        let _guard = crate::tui::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("smarthistory-test-{}", generate_uuid_v4()));
        let cfg_dir = dir.join(".config").join("smarthistory");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir");
        let cfg_path = cfg_dir.join("config");
        std::fs::write(
            &cfg_path,
            "# comment line

ignorecapture=mycustomcmd spaced
capturelines=40
capturelines.ps=ALL
tmuxpaneoutputdir=~/custom-tmux
",
        )
        .expect("write");
        let prev = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &dir);
        }
        let cfg = Config::load();
        match prev {
            Some(p) => unsafe {
                std::env::set_var("HOME", p);
            },
            None => unsafe {
                std::env::remove_var("HOME");
            },
        }
        // User override replaces the default ignore list.
        assert!(cfg.ignore_capture("mycustomcmd"));
        assert!(cfg.ignore_capture("spaced"));
        assert!(!cfg.ignore_capture("vim"));
        assert_eq!(cfg.default_capture_lines, Some(40));
        // Per-command override.
        assert_eq!(cfg.capture_lines_for("ps -ef"), None);
        assert_eq!(cfg.capture_lines_for("cat README"), Some(40));
        // tilde expansion for the path.
        let expected = dir.join("custom-tmux");
        assert_eq!(cfg.tmux_pane_output_dir, expected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `jira.search.<name>=<jql>` entries in the
    /// config file populate `Config::jira_fragments`.
    /// The user's example: `jira.search.label1=labels =
    /// "test"` should be addressable from the
    /// `-`-mode TUI search as `@label1`. Reserved
    /// names (`me`, `today`, `week`, `month`) are
    /// silently dropped to avoid shadowing the
    /// built-in aliases.
    #[test]
    fn config_parses_jira_search_fragments() {
        // We exercise `Config::parse` directly with
        // a string instead of round-tripping through
        // `Config::load`. The load path reads `$HOME`
        // to find the config file, and any test that
        // mutates `HOME` is racy against every other
        // test that reads it (cargo runs tests in
        // parallel; `std::env::set_var` is `unsafe`
        // in modern Rust precisely because of this).
        // Bypassing the env makes the test
        // self-contained without needing a mutex
        // that would have to be held by every
        // HOME-reading test in the binary.
        let mut cfg = Config::default();
        cfg.parse(
            "jira.search.label1=labels = \"test\"\n\
             jira.search.SPRINT=sprint = \"Sprint 42\"\n\
             jira.search.complex=priority = High AND labels = \"security\"\n\
             jira.search.me=assignee = \"alice\"\n\
             jira.search.=empty name is ignored\n\
             jira.search.bad name=spaces in name are ignored\n\
             jira.search.emptyvalue=\n",
        );
        let frags = cfg.jira_fragments();
        // The three valid fragments made it in
        // (lowercased keys — the loader
        // normalises the name to lowercase so
        // the parser lookup is a direct map
        // access).
        assert_eq!(
            frags.get("label1").map(String::as_str),
            Some(r#"labels = "test""#),
        );
        assert_eq!(
            frags.get("sprint").map(String::as_str),
            Some(r#"sprint = "Sprint 42""#),
        );
        assert_eq!(
            frags.get("complex").map(String::as_str),
            Some(r#"priority = High AND labels = "security""#),
        );
        // The reserved-name `me` was silently
        // dropped. The user can't shadow the
        // built-in `@me` alias.
        assert!(!frags.contains_key("me"));
        // Empty name (just the prefix, nothing
        // after the dot) was ignored.
        assert!(!frags.contains_key(""));
        // Name with a space isn't a valid
        // identifier (\w+ only) so it's dropped
        // silently.
        assert!(!frags.contains_key("bad name"));
        // Empty value: silently dropped. A
        // fragment with no JQL is worse than no
        // fragment at all.
        assert!(!frags.contains_key("emptyvalue"));
    }

    #[test]
    fn parse_capture_lines_handles_all_and_numbers() {
        assert_eq!(parse_capture_lines("ALL"), None);
        assert_eq!(parse_capture_lines("all"), None);
        assert_eq!(parse_capture_lines("20"), Some(20));
        assert_eq!(parse_capture_lines("  15  "), Some(15));
        assert_eq!(parse_capture_lines("not a number"), None);
    }

    /// `multiplexer=tmux` and
    /// `multiplexer=herdr` are
    /// the canonical config
    /// values. The loader is
    /// case-insensitive and
    /// unrecognised values are
    /// silently dropped so a
    /// typo can't disable
    /// directory switching.
    #[test]
    fn config_parses_multiplexer_key() {
        let mut cfg = Config::default();
        cfg.parse("multiplexer=tmux\n");
        assert_eq!(cfg.multiplexer(), crate::multiplexer::MultiplexerKind::Tmux);
        cfg.parse("multiplexer=herdr\n");
        assert_eq!(
            cfg.multiplexer(),
            crate::multiplexer::MultiplexerKind::Herdr
        );
        cfg.parse("multiplexer=HERDR\n");
        assert_eq!(
            cfg.multiplexer(),
            crate::multiplexer::MultiplexerKind::Herdr
        );
        // Unrecognised value:
        // the previous
        // value is
        // preserved (the
        // parser emits a
        // warning to
        // stderr but we
        // don't assert on
        // that here).
        // The default is
        // `Tmux`, so
        // starting from a
        // fresh `Config`
        // and feeding it
        // an invalid value
        // keeps the
        // default.
        let mut cfg = Config::default();
        cfg.parse("multiplexer=screen\n");
        assert_eq!(cfg.multiplexer(), crate::multiplexer::MultiplexerKind::Tmux);
    }

    /// Regression test for
    /// the "I have
    /// `sessiondirs=~/foo`
    /// in my config but no
    /// directories are
    /// added" bug: a
    /// literal `~` in a
    /// config value is a
    /// user-friendly
    /// shorthand for
    /// `$HOME`, but the
    /// config loader must
    /// actually expand it
    /// before passing the
    /// path to the
    /// filesystem walker.
    /// Without this
    /// expansion, the
    /// walker would see a
    /// path that doesn't
    /// exist
    /// (`std::path::Path::exists("~/x")`
    /// is always `false`)
    /// and silently skip
    /// the entry — the
    /// user's pinned
    /// directories would
    /// never appear in
    /// the `#`-mode list.
    /// The same expansion
    /// is already applied
    /// to `notes.database`
    /// and `notes.dir`; we
    /// add it here for
    /// `sessiondirs` so
    /// the user's mental
    /// model ("`~` works
    /// everywhere in the
    /// config") holds.
    #[test]
    fn sessiondirs_expands_tilde() {
        let home = std::env::var("HOME").expect("HOME must be set for this test");
        // Exercise the
        // production parse
        // path: feed the
        // config parser a
        // string with
        // `sessiondirs=~/work`
        // and verify the
        // result is the
        // expanded path, not
        // the literal `~/work`
        // (which was the
        // bug).
        let mut cfg = Config::default();
        cfg.parse("sessiondirs=~/work\n");
        assert_eq!(
            cfg.session_dirs().len(),
            1,
            "sessiondirs=~/work must produce exactly one entry"
        );
        let got = &cfg.session_dirs()[0];
        // The stored path
        // must be the
        // `$HOME`-relative
        // expansion, not the
        // literal `~/work`
        // (which is the bug
        // we're fixing).
        assert_ne!(
            got.to_string_lossy(),
            "~/work",
            "sessiondirs=~/work must not store the literal `~` (the bug we're fixing)"
        );
        assert_eq!(
            got.to_string_lossy(),
            format!("{}/work", home),
            "sessiondirs=~/work must expand to `$HOME/work`"
        );
        // And the resulting
        // path must be a
        // real (or at least
        // plausibly real)
        // path — i.e. it
        // would pass
        // `path.exists()` in
        // `walk_subdirectories`.
        // We don't *create*
        // the directory
        // here — we just
        // confirm the
        // expansion produced
        // something that
        // *could* exist on
        // disk.
    }

    #[test]
    fn project_row_escapes_multiline_command() {
        // A multiline command must be escaped to a single line so the
        // CLI output (one row per line) and the zsh widget's `(f)`
        // record splitter see exactly one match per row.
        let row_data = vec![(
            "command".to_string(),
            "for i in 1 2 3\ndo echo $i\ndone".to_string(),
        )];
        let fields = vec!["command".to_string()];
        // `AnsiMode::Off` matches the old `no_highlight=true`
        // behavior — the test only cares about escape-encoding, not
        // SGR markup.
        let out = project_row(&row_data, &fields, &[], None, AnsiMode::Off);
        assert_eq!(out.len(), 1);
        assert!(
            !out[0].contains('\n'),
            "escaped command still has a newline: {:?}",
            out[0]
        );
        assert_eq!(out[0], "for i in 1 2 3\\ndo echo $i\\ndone");
    }

    #[test]
    fn project_row_escapes_output_field() {
        // The `output` field is also escaped (it can contain newlines
        // from captured command output).
        let row_data = vec![("output".to_string(), "line1\nline2".to_string())];
        let fields = vec!["output".to_string()];
        let out = project_row(&row_data, &fields, &[], None, AnsiMode::Off);
        assert_eq!(out.len(), 1);
        assert!(
            !out[0].contains('\n'),
            "escaped output still has a newline: {:?}",
            out[0]
        );
        assert_eq!(out[0], "line1\\nline2");
    }

    #[test]
    fn project_row_ansi_off_emits_plain_cell() {
        // `AnsiMode::Off` must emit the cell with no decoration at
        // all — the legacy `no_highlight=true` behavior, plus the
        // explicit `--ansi=off` form.
        let row_data = vec![(
            "command".to_string(),
            "git status".to_string(),
        )];
        let fields = vec!["command".to_string()];
        let out = project_row(
            &row_data,
            &fields,
            &[],
            Some("git"),
            AnsiMode::Off,
        );
        assert_eq!(out, vec!["git status".to_string()]);
    }

    /// `Config::resolved_palette` falls back to the built-in
    /// defaults when no theme is selected and the user hasn't
    /// set any `tuicolor.*` overrides — the case the widget hits
    /// on a first-run install before the user has touched the
    /// config file.
    #[test]
    fn resolved_palette_defaults_when_no_theme() {
        use crate::tui::theme::ColorScheme;
        let cfg = Config::default();
        let p = cfg.resolved_palette(ColorScheme::Dark);
        // `accent` defaults to "cyan" per `TuiTheme::default()`;
        // no theme is selected, so the manual-config defaults
        // are the source of truth.
        let accent = p.iter().find(|(k, _)| *k == "accent").unwrap();
        assert_eq!(accent.1, "cyan");
        // `bg` defaults to "black"; `fg` to "white"; `selection`
        // to "blue". The exact slot set is intentionally stable
        // (the widget's parser depends on these names).
        assert_eq!(
            p.iter().find(|(k, _)| *k == "bg").unwrap().1,
            "black"
        );
        assert_eq!(
            p.iter().find(|(k, _)| *k == "fg").unwrap().1,
            "white"
        );
        assert_eq!(
            p.iter().find(|(k, _)| *k == "selection").unwrap().1,
            "blue"
        );
    }

    /// `tuicolor.<field>=` overrides win over the active theme's
    /// built-in default — the user always has the final say.
    #[test]
    fn resolved_palette_user_override_wins_over_theme() {
        use crate::tui::theme::ColorScheme;
        let mut cfg = Config::default();
        // Pick a built-in theme. Without an override, its
        // accent would be the theme's own accent (e.g.
        // Doom One's #ff0000-ish red). With the override,
        // `cyan` must win.
        cfg.parse("theme.dark=doom-one\ntuicolor.accent=cyan\n");
        let p = cfg.resolved_palette(ColorScheme::Dark);
        let accent = p.iter().find(|(k, _)| *k == "accent").unwrap();
        assert_eq!(
            accent.1, "cyan",
            "user `tuicolor.accent=cyan` must override the active theme"
        );
    }

    /// The resolution must produce all 14 `tuicolor.*` slots the
    /// widget parses, so a future widget addition can't silently
    /// get an empty SGR code by looking up an unknown key.
    #[test]
    fn resolved_palette_emits_all_14_slots() {
        use crate::tui::theme::ColorScheme;
        let cfg = Config::default();
        let p = cfg.resolved_palette(ColorScheme::Dark);
        let names: Vec<&str> = p.iter().map(|(k, _)| *k).collect();
        for required in [
            "bg", "fg", "accent", "success", "error", "warning", "dim",
            "highlight", "info", "selection", "badgefg", "listbg",
            "detailsbg", "inputbg", "statusbg",
        ] {
            assert!(
                names.contains(&required),
                "resolved_palette missing required slot `{required}`; \
                 widget would render with an empty SGR code"
            );
        }
    }

    /// `color_to_css` is the round-trip partner of
    /// `resolve_color`: a `Rgb(r, g, b)` from a built-in theme
    /// must come back as `#rrggbb`, and every standard ANSI
    /// variant must come back as the CSS name the user would
    /// have typed in `tuicolor.<field>=`.
    #[test]
    fn color_to_css_round_trips() {
        use crate::tui::theme::color_to_css;
        use ratatui::style::Color;
        assert_eq!(color_to_css(Color::Black), "black");
        assert_eq!(color_to_css(Color::LightRed), "lightred");
        assert_eq!(
            color_to_css(Color::Rgb(0xab, 0xcd, 0xef)),
            "#abcdef"
        );
        // `Color::Reset` round-trips to the literal "reset"
        // string (the widget's parser treats it as a no-op).
        assert_eq!(color_to_css(Color::Reset), "reset");
    }

    #[test]
    fn highlight_full_wraps_in_dim_and_bolds_match() {
        // The dedicated `Full`-mode helper wraps the whole cell in
        // dim, with the matched prefix upgraded to bold. The exact
        // emitted sequence is
        //   \x1b[2m  \x1b[0m\x1b[1mg\x1b[0m\x1b[2mit status\x1b[0m
        // for a needle of "g" in a cell of "  git status" — the dim
        // opens before the prefix-before, resets for the bold, then
        // re-opens dim for the suffix. The trailing dim-open has no
        // matching close; that's intentional, the caller (`project_row`)
        // emits a reset at end of line.
        let got = highlight_full("  git status", "g");
        assert_eq!(got, "\x1b[2m  \x1b[0m\x1b[1mg\x1b[0m\x1b[2mit status");
    }

    #[test]
    fn highlight_full_empty_needle_emits_plain() {
        // An empty needle means there's nothing to highlight, so the
        // cell is emitted as-is — same contract as the legacy
        // `highlight()` helper.
        assert_eq!(highlight_full("git status", ""), "git status");
    }

    #[test]
    fn highlight_full_multiple_occurrences_each_get_bold() {
        // Every match is bold-wrapped; the dim open is repeated for
        // each non-match segment so the cell is fully dim except for
        // the bolded matches.
        let got = highlight_full("foo bar foo", "foo");
        assert_eq!(
            got,
            "\x1b[2m\x1b[0m\x1b[1mfoo\x1b[0m\x1b[2m bar \x1b[0m\x1b[1mfoo\x1b[0m\x1b[2m"
        );
    }

    /// `Config::theme_for` returns the user-configured
    /// `theme.<scheme>=` value when set, with a fallback
    /// to the OTHER scheme's value. The fallback is
    /// symmetric: a user who only set `theme.dark=dracula`
    /// gets dracula on a light terminal too. This is the
    /// "one line, two schemes" opt-in shape — easier to
    /// write than setting both, and the common case
    /// (a user who runs the same theme in both their
    /// light and dark terminals) only needs one entry.
    #[test]
    fn theme_for_uses_active_scheme_first() {
        let mut cfg = Config::default();
        cfg.theme_light = Some("gruvbox-light".to_string());
        cfg.theme_dark = Some("dracula".to_string());
        assert_eq!(
            cfg.theme_for(crate::tui::theme::ColorScheme::Light),
            Some("gruvbox-light")
        );
        assert_eq!(
            cfg.theme_for(crate::tui::theme::ColorScheme::Dark),
            Some("dracula")
        );
    }

    /// When only the light slot is set, a dark terminal
    /// falls back to it (so a user who only set
    /// `theme.light=catppuccin-latte` gets the same
    /// theme in both their light AND dark terminals).
    #[test]
    fn theme_for_falls_back_to_other_scheme() {
        let mut cfg = Config::default();
        cfg.theme_light = Some("catppuccin-latte".to_string());
        // Dark slot unset; the light value is the fallback.
        assert_eq!(
            cfg.theme_for(crate::tui::theme::ColorScheme::Light),
            Some("catppuccin-latte")
        );
        assert_eq!(
            cfg.theme_for(crate::tui::theme::ColorScheme::Dark),
            Some("catppuccin-latte")
        );
    }

    /// When only the dark slot is set, a light terminal
    /// falls back to it (the inverse of the previous
    /// test).
    #[test]
    fn theme_for_falls_back_from_dark_to_light() {
        let mut cfg = Config::default();
        cfg.theme_dark = Some("nord".to_string());
        assert_eq!(
            cfg.theme_for(crate::tui::theme::ColorScheme::Light),
            Some("nord")
        );
        assert_eq!(
            cfg.theme_for(crate::tui::theme::ColorScheme::Dark),
            Some("nord")
        );
    }

    /// When neither slot is set, `theme_for` returns
    /// `None` — the TUI loader then falls back to the
    /// legacy `theme=` line in the session file, then to
    /// `SelectedTheme::None` (the manual `tuicolor.*`
    /// palette).
    #[test]
    fn theme_for_returns_none_when_unset() {
        let cfg = Config::default();
        assert!(cfg.theme_light.is_none());
        assert!(cfg.theme_dark.is_none());
        assert!(cfg.theme_for(crate::tui::theme::ColorScheme::Light).is_none());
        assert!(cfg.theme_for(crate::tui::theme::ColorScheme::Dark).is_none());
    }

    /// `session.<id>` entries defined in a SEPARATE source (the
    /// `sessions` file, in production) must show up exactly like
    /// entries defined inline in the main config file — this is
    /// the whole point of `parse_multi` treating multiple sources
    /// as if they were one concatenated file.
    #[test]
    fn parse_multi_merges_sessions_from_a_separate_source() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["duplicatefilter=off\n", "session.1 = \"Foo\"\nsession.1.dir = \"~/foo\"\n"]);
        let sessions = cfg.sessions();
        assert_eq!(sessions.len(), 1, "got: {:?}", sessions);
        assert_eq!(sessions[0].command, "Foo");
    }

    /// Same as above, but for `host.<id>` entries and a `hosts`
    /// file source.
    ///
    /// Doesn't assert an exact row count: `Config::parse` also
    /// merges the machine's real `~/.ssh/config` into `self.hosts`
    /// (auto-appending a synthetic entry per SSH `Host` block with
    /// no matching `host.<id>`), so a dev machine with its own SSH
    /// config produces extra rows here — that's real, correct
    /// behavior, not something this test should fight. Assert only
    /// that the entry from OUR source is present among whatever
    /// else got merged in.
    #[test]
    fn parse_multi_merges_hosts_from_a_separate_source() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["", "host.1 = \"Proxmox\"\nhost.1.host = \"pve-1\"\n", ""]);
        let hosts = cfg.hosts();
        assert!(
            hosts.iter().any(|r| r.command == "Proxmox"),
            "got: {:?}",
            hosts
        );
        assert!(
            cfg.host_defs().iter().any(|h| h.host == "pve-1"),
            "got: {:?}",
            cfg.host_defs()
        );
    }

    /// The three-source shape `load_tui` actually uses (main
    /// config, hosts file, sessions file) merges all three kinds
    /// of content in one pass, matching what a real split-file
    /// setup looks like. Same "don't fight the real SSH-config
    /// merge" caveat as `parse_multi_merges_hosts_from_a_separate_source`.
    #[test]
    fn parse_multi_merges_main_hosts_and_sessions_sources() {
        let mut cfg = Config::default();
        cfg.parse_multi(&[
            "prefix.jira=`\n",
            "host.1 = \"Proxmox\"\nhost.1.host = \"pve-1\"\n",
            "session.1 = \"Foo\"\n",
        ]);
        assert!(cfg.hosts().iter().any(|r| r.command == "Proxmox"));
        assert_eq!(cfg.sessions().len(), 1);
        assert_eq!(cfg.query_prefixes().jira, '`');
    }

    /// `resolve_pane_exec` (backing `smarthistory pane-exec`) must
    /// prefer a `session.<id>` match and return its `.exec` verbatim.
    #[test]
    fn resolve_pane_exec_matches_session_with_exec() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["session.1 = \"Proxmox\"\nsession.1.exec = \"tmux a\"\n"]);
        assert_eq!(
            resolve_pane_exec(&cfg, "Proxmox"),
            PaneExecTarget::Run("tmux a".to_string())
        );
    }

    /// A `session.<id>` match with no `.exec` set is a deliberate
    /// no-op, not an error and not a fall-through to a host match.
    #[test]
    fn resolve_pane_exec_matches_session_without_exec() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["session.1 = \"Proxmox\"\n"]);
        assert_eq!(
            resolve_pane_exec(&cfg, "Proxmox"),
            PaneExecTarget::NoExecConfigured
        );
    }

    /// A `host.<id>` match returns its `ssh` connection command —
    /// and must NOT include `.exec`, even when one is configured
    /// (see `resolve_pane_exec`'s doc comment for why: it's meant to
    /// be typed into the remote shell after connecting, not run as
    /// a local follow-up).
    #[test]
    fn resolve_pane_exec_matches_host_excludes_remote_exec() {
        let mut cfg = Config::default();
        cfg.parse_multi(&[
            "host.1 = \"Proxmox\"\nhost.1.host = \"pve-1\"\nhost.1.user = \"root\"\nhost.1.exec = \"tmux a\"\n",
        ]);
        assert_eq!(
            resolve_pane_exec(&cfg, "Proxmox"),
            PaneExecTarget::Run("ssh root@pve-1".to_string())
        );
    }

    /// herdr may show a host-created workspace as `host:<name>` if
    /// the user hasn't renamed it away from the auto-generated
    /// label — `resolve_pane_exec` must match that form too, same
    /// as `stage_pane_selection`'s own matcher.
    #[test]
    fn resolve_pane_exec_matches_host_prefixed_label() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["host.1 = \"Proxmox\"\nhost.1.host = \"pve-1\"\nhost.1.user = \"root\"\n"]);
        assert_eq!(
            resolve_pane_exec(&cfg, "host:Proxmox"),
            PaneExecTarget::Run("ssh root@pve-1".to_string())
        );
    }

    /// No configured session/host has this name — nothing to run.
    #[test]
    fn resolve_pane_exec_no_match_returns_not_found() {
        let cfg = Config::default();
        assert_eq!(
            resolve_pane_exec(&cfg, "some-unrelated-session-name"),
            PaneExecTarget::NotFound
        );
    }

    /// `HostDef::ssh_command` directly — port and identity flags are
    /// only included when actually set, matching the pre-extraction
    /// inline behavior (`host_row_in_panes_mode_ssh_argv_includes_port_and_identity`
    /// covers the same logic via the TUI staging path).
    #[test]
    fn host_def_ssh_command_includes_port_and_identity() {
        let host = crate::tui::state::HostDef {
            name: "Proxmox".to_string(),
            host: "pve-1".to_string(),
            hostname: String::new(),
            user: "root".to_string(),
            port: 2222,
            identity: "~/.ssh/id_prod".to_string(),
            dir: String::new(),
            exec: String::new(),
        };
        let cmd = host.ssh_command();
        assert!(cmd.starts_with("ssh -p 2222 -i "), "got: {cmd:?}");
        assert!(cmd.ends_with(" root@pve-1"), "got: {cmd:?}");
    }

    /// Default port (0 or 22) and no identity: no `-p`/`-i` flags.
    #[test]
    fn host_def_ssh_command_omits_default_port_and_empty_identity() {
        let host = crate::tui::state::HostDef {
            name: "Proxmox".to_string(),
            host: "pve-1".to_string(),
            hostname: String::new(),
            user: "root".to_string(),
            port: 22,
            identity: String::new(),
            dir: String::new(),
            exec: String::new(),
        };
        assert_eq!(host.ssh_command(), "ssh root@pve-1");
    }

    /// Regression guard for the bug `parse_multi` exists to avoid:
    /// finalization (applying the collected `key.*` overrides) must
    /// run exactly ONCE, after every source has contributed its
    /// lines — not once per source. Before this fix, a naive
    /// "call `parse()` once per file" approach would have applied
    /// `key_bindings_from_config` a second time with an EMPTY map
    /// (the `sessions` file has no `key.*` lines), silently
    /// resetting every key binding from the main config back to
    /// its default.
    #[test]
    fn parse_multi_key_bindings_survive_a_second_source_with_no_key_lines() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["key.cancel=C-x\n", "session.1 = \"Foo\"\n"]);
        let bindings = cfg.key_bindings();
        let (_, specs) = bindings
            .iter()
            .find(|(action, _)| *action == crate::tui::bindings::Action::Cancel)
            .expect("Cancel action should have bindings");
        assert!(
            specs.iter().any(|s| crate::tui::format_key_spec(*s) == "C-x"),
            "got: {:?}",
            specs.iter().map(|s| crate::tui::format_key_spec(*s)).collect::<Vec<_>>()
        );
    }

    /// "Later source wins" applies across sources the same way it
    /// already applies across lines within one file.
    #[test]
    fn parse_multi_later_source_wins_for_the_same_key() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["duplicatefilter=on\n", "duplicatefilter=off\n"]);
        assert!(!cfg.duplicate_filter);
    }

    // --- `prune-directories` CLI subcommand ------------------------

    /// `Config::session_directories` returns every `session.<id>`
    /// entry that has a `.dir` set, with the id preserved (needed by
    /// `prune-directories` to remove the right lines) and the
    /// directory expanded to an absolute path. An entry with no
    /// `.dir` is omitted — there's nothing to check for it.
    #[test]
    fn session_directories_returns_entries_with_dir_only() {
        let mut cfg = Config::default();
        cfg.parse_multi(&[
            "session.1 = \"Has dir\"\n\
             session.1.dir = /tmp/some-dir\n\
             session.2 = \"No dir\"\n",
        ]);
        let dirs = cfg.session_directories();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].0, "1");
        assert_eq!(dirs[0].1, "Has dir");
        assert_eq!(dirs[0].2, "/tmp/some-dir");
    }

    /// `remove_session_lines` deletes exactly the name/`.dir`/`.exec`
    /// lines for a given id and leaves every other id's lines (and
    /// non-`session.*` lines) untouched — including the
    /// `session.1` vs. `session.10` prefix-collision case.
    #[test]
    fn remove_session_lines_deletes_only_the_matching_id() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory-remove-session-lines-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("sessions");
        std::fs::write(
            &path,
            "session.1 = \"Keep\"\n\
             session.1.dir = /tmp/keep\n\
             \n\
             session.10 = \"Also keep (id collision check)\"\n\
             session.10.dir = /tmp/also-keep\n\
             \n\
             session.2 = \"Remove\"\n\
             session.2.dir = /tmp/remove\n\
             session.2.exec = nvim\n",
        )
        .expect("write");

        let ids: std::collections::HashSet<String> = ["2".to_string()].into_iter().collect();
        let removed = remove_session_lines(&path, &ids).expect("remove");
        assert_eq!(removed, 3, "name + .dir + .exec lines for session.2");

        let contents = std::fs::read_to_string(&path).expect("read back");
        assert!(contents.contains("session.1 = \"Keep\""));
        assert!(contents.contains("session.10 = \"Also keep"), "must not treat session.1 as a prefix of session.10");
        assert!(!contents.contains("session.2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file with no lines matching any of the given ids is left
    /// byte-for-byte untouched — `remove_session_lines` must not
    /// rewrite (and so not touch the mtime of) a file it has nothing
    /// to remove from.
    #[test]
    fn remove_session_lines_no_match_leaves_file_untouched() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory-remove-session-lines-noop-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("sessions");
        let original = "session.1 = \"Keep\"\nsession.1.dir = /tmp/keep\n";
        std::fs::write(&path, original).expect("write");

        let ids: std::collections::HashSet<String> = ["99".to_string()].into_iter().collect();
        let removed = remove_session_lines(&path, &ids).expect("remove");
        assert_eq!(removed, 0);
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing file is not an error — `prune-directories` calls
    /// this for both the main config file and the dedicated
    /// `sessions` file, and either (or both) may not exist.
    #[test]
    fn remove_session_lines_missing_file_returns_zero() {
        let path = std::env::temp_dir().join(format!(
            "smarthistory-remove-session-lines-missing-{}",
            std::process::id()
        ));
        let ids: std::collections::HashSet<String> = ["1".to_string()].into_iter().collect();
        assert_eq!(remove_session_lines(&path, &ids).expect("remove"), 0);
    }
