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

    fn index_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap()
    }

    #[test]
    fn ensure_history_performance_indexes_creates_all_three() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE history (
                id INTEGER PRIMARY KEY,
                command TEXT NOT NULL,
                directory TEXT NOT NULL,
                session_id TEXT NOT NULL,
                exit_code INTEGER,
                timestamp INTEGER,
                mode TEXT NOT NULL DEFAULT 'command'
            )",
            [],
        )
        .unwrap();

        ensure_history_performance_indexes(&conn).unwrap();

        assert!(index_exists(&conn, "idx_history_timestamp"));
        assert!(index_exists(&conn, "idx_history_session_ts"));
        assert!(index_exists(&conn, "idx_history_directory_ts"));
    }

    /// The actual regression this guards against: at the time these
    /// indexes were added, `migrate_history_comment_column`
    /// rebuilds `history` from scratch (see
    /// `migrate_history_comment_column_recreates_dedup_index` above)
    /// and only knows to recreate `idx_history_dedup`. Calling
    /// `ensure_history_performance_indexes` AFTER that migration —
    /// exactly the order `init_db` uses — must still leave all
    /// three indexes in place on the rebuilt table.
    #[test]
    fn history_performance_indexes_survive_comment_column_migration() {
        let conn = Connection::open_in_memory().unwrap();
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

        migrate_history_comment_column(&conn).unwrap();
        ensure_history_performance_indexes(&conn).unwrap();

        assert!(index_exists(&conn, "idx_history_timestamp"));
        assert!(index_exists(&conn, "idx_history_session_ts"));
        assert!(index_exists(&conn, "idx_history_directory_ts"));
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
    fn config_default_contains_file_view_commands() {
        let cfg = Config::default();
        for cmd in DEFAULT_FILE_VIEW_COMMANDS {
            assert!(cfg.is_file_view_command(cmd), "default {cmd} missing");
        }
        assert!(!cfg.is_file_view_command("cat"), "cat is not a default file-view command");
    }

    #[test]
    fn fileviewcommands_config_replaces_the_default_set() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["fileviewcommands = cat bat\n"]);
        assert!(cfg.is_file_view_command("cat something"));
        assert!(cfg.is_file_view_command("bat something"));
        assert!(!cfg.is_file_view_command("less something"), "less was not re-added to the custom list");
    }

    #[test]
    fn first_non_flag_argument_skips_leading_flags() {
        assert_eq!(first_non_flag_argument("tail -f app.log"), Some("app.log"));
        assert_eq!(first_non_flag_argument("less -N config.yaml"), Some("config.yaml"));
        assert_eq!(first_non_flag_argument("less file.txt"), Some("file.txt"));
        assert_eq!(first_non_flag_argument("less"), None);
        assert_eq!(first_non_flag_argument("less -N -x"), None, "only flag arguments");
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

    #[test]
    fn format_ask_output_no_color_is_plain_text() {
        let (answer, suggestions) = format_ask_output(
            "It failed because of a permission error.",
            &["chmod +x foo.sh".to_string()],
            false,
        );
        assert_eq!(
            answer,
            "LLM Answer\nIt failed because of a permission error."
        );
        assert_eq!(suggestions, vec!["1) chmod +x foo.sh".to_string()]);
        assert!(!answer.contains('\x1b'));
        assert!(!suggestions[0].contains('\x1b'));
    }

    #[test]
    fn format_ask_output_header_starts_on_its_own_line() {
        // The header must be followed by a newline before the
        // answer text, not inline on the same line -- the whole
        // point of the "LLM Answer\n<answer>" shape.
        let (answer, _suggestions) = format_ask_output("It lists files.", &[], false);
        let mut lines = answer.lines();
        assert_eq!(lines.next(), Some("LLM Answer"));
        assert_eq!(lines.next(), Some("It lists files."));
    }

    #[test]
    fn format_ask_output_color_wraps_header_and_indices() {
        let (answer, suggestions) =
            format_ask_output("It lists files.", &["ls -la".to_string()], true);
        assert!(answer.starts_with("\x1b[1;35mLLM Answer\x1b[0m\n"));
        assert!(answer.ends_with("It lists files."));
        assert!(suggestions[0].contains("\x1b[1;36m1)\x1b[0m"));
        assert!(suggestions[0].ends_with("ls -la"));
    }

    #[test]
    fn format_thinking_message_no_color_is_plain_text() {
        let msg = format_thinking_message(false);
        assert_eq!(msg, "Thinking…");
        assert!(!msg.contains('\x1b'));
    }

    #[test]
    fn format_thinking_message_color_is_dim() {
        let msg = format_thinking_message(true);
        assert_eq!(msg, "\x1b[2mThinking…\x1b[0m");
    }

    #[test]
    fn clear_thinking_message_color_erases_line_in_place() {
        assert_eq!(clear_thinking_message(true), "\r\x1b[2K");
    }

    #[test]
    fn clear_thinking_message_no_color_is_just_a_newline() {
        assert_eq!(clear_thinking_message(false), "\n");
    }

    #[test]
    fn format_ask_output_numbers_multiple_suggestions_in_order() {
        let suggestions = vec!["git stash".to_string(), "git stash pop".to_string()];
        let (_answer, lines) = format_ask_output("Try one of these.", &suggestions, false);
        assert_eq!(
            lines,
            vec!["1) git stash".to_string(), "2) git stash pop".to_string()]
        );
    }

    #[test]
    fn format_ask_output_no_suggestions_is_empty() {
        let (_answer, lines) = format_ask_output("Just a fact, no command.", &[], false);
        assert!(lines.is_empty());
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

    // --- Time tracking: project resolution + session lifecycle -----

    /// The most specific (longest) matching `project.<slug>.dir`
    /// wins over a broader ancestor's binding — a sub-project nested
    /// inside a monorepo's own binding should resolve to itself, not
    /// the monorepo.
    #[test]
    fn resolve_project_dir_prefers_longest_match() {
        let mut cfg = Config::default();
        cfg.parse_multi(&[
            "project.monorepo.dir = /tmp/work\n\
             project.subproj.dir = /tmp/work/subproj\n",
        ]);
        assert_eq!(
            resolve_project_dir(&cfg, "/tmp/work/subproj/src"),
            Some("subproj".to_string())
        );
        assert_eq!(
            resolve_project_dir(&cfg, "/tmp/work/other"),
            Some("monorepo".to_string())
        );
    }

    #[test]
    fn resolve_project_dir_no_match_returns_none() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["project.demo.dir = /tmp/demo\n"]);
        assert_eq!(resolve_project_dir(&cfg, "/tmp/unrelated"), None);
    }

    /// A directory that is EXACTLY a configured project's dir (not
    /// just a descendant of it) still matches — the prefix check
    /// must not require a trailing path segment.
    #[test]
    fn resolve_project_dir_matches_exact_directory() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["project.demo.dir = /tmp/demo\n"]);
        assert_eq!(
            resolve_project_dir(&cfg, "/tmp/demo"),
            Some("demo".to_string())
        );
        // A sibling directory with the same prefix as a STRING (but
        // not as a path) must not falsely match.
        assert_eq!(resolve_project_dir(&cfg, "/tmp/demo-other"), None);
    }

    /// A marker file's first non-blank line is the slug, found from
    /// a directory several levels below it.
    #[test]
    fn find_project_marker_finds_file_n_levels_up() {
        let root = std::env::temp_dir().join(format!(
            "smarthistory-marker-test-{}",
            generate_uuid_v4()
        ));
        let nested = root.join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(root.join(".smarthistory-project"), "\n  demo-slug  \n").expect("write");
        assert_eq!(find_project_marker(&nested), Some("demo-slug".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_project_marker_no_file_returns_none() {
        let root = std::env::temp_dir().join(format!(
            "smarthistory-marker-none-test-{}",
            generate_uuid_v4()
        ));
        std::fs::create_dir_all(&root).expect("mkdir");
        assert_eq!(find_project_marker(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Build an in-memory `Connection` with just the `history` +
    /// `project_sessions` columns `switch_project` reads/writes —
    /// test fixtures don't inherit `init_db`'s schema automatically.
    fn project_lifecycle_test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE history (id INTEGER PRIMARY KEY, timestamp INTEGER);
             CREATE TABLE project_sessions (
                 id INTEGER PRIMARY KEY,
                 project_slug TEXT NOT NULL,
                 start_ts INTEGER NOT NULL,
                 end_ts INTEGER,
                 end_reason TEXT
             );",
        )
        .expect("schema");
        conn
    }

    #[test]
    fn switch_project_opens_new_session_when_none_open() {
        let conn = project_lifecycle_test_conn();
        switch_project(&conn, Some("demo"), 1000, 1800, None).expect("switch");
        let (slug, start_ts, end_ts): (String, i64, Option<i64>) = conn
            .query_row(
                "SELECT project_slug, start_ts, end_ts FROM project_sessions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("row");
        assert_eq!(slug, "demo");
        assert_eq!(start_ts, 1000);
        assert_eq!(end_ts, None);
    }

    /// A resolved project different from the currently-open one
    /// closes it IMMEDIATELY (not after the idle threshold) with
    /// `end_reason = 'directory_change'` by default, and opens a
    /// fresh session for the new project.
    #[test]
    fn switch_project_closes_on_directory_change_and_opens_new() {
        let conn = project_lifecycle_test_conn();
        switch_project(&conn, Some("demo"), 1000, 1800, None).expect("switch");
        switch_project(&conn, Some("other"), 1050, 1800, None).expect("switch");

        let mut stmt = conn
            .prepare("SELECT project_slug, start_ts, end_ts, end_reason FROM project_sessions ORDER BY id")
            .expect("prepare");
        let rows: Vec<(String, i64, Option<i64>, Option<String>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ("demo".to_string(), 1000, Some(1050), Some("directory_change".to_string())));
        assert_eq!(rows[1].0, "other");
        assert_eq!(rows[1].2, None, "the new session must still be open");
    }

    /// The same project, with a command observed WITHIN the idle
    /// window, stays open — `switch_project` is a no-op.
    #[test]
    fn switch_project_stays_open_when_same_project_and_not_idle() {
        let conn = project_lifecycle_test_conn();
        switch_project(&conn, Some("demo"), 1000, 1800, None).expect("switch");
        conn.execute(
            "INSERT INTO history (timestamp) VALUES (1100)",
            [],
        )
        .expect("insert");
        switch_project(&conn, Some("demo"), 1200, 1800, None).expect("switch");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM project_sessions", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 1, "must not open a second session for the same project");
        let end_ts: Option<i64> = conn
            .query_row("SELECT end_ts FROM project_sessions", [], |r| r.get(0))
            .expect("row");
        assert_eq!(end_ts, None, "must still be open");
    }

    /// The same project, with the gap since the last observed
    /// command exceeding the idle threshold, closes with
    /// `end_reason = 'idle'` and `end_ts` BACKDATED to
    /// `last_activity + idle_threshold` — not the wall-clock `now`
    /// this function happened to run at.
    #[test]
    fn switch_project_closes_on_idle_gap_with_backdated_end_ts() {
        let conn = project_lifecycle_test_conn();
        switch_project(&conn, Some("demo"), 1000, 100, None).expect("switch");
        conn.execute("INSERT INTO history (timestamp) VALUES (1010)", [])
            .expect("insert");
        // Gap since last activity (1010) exceeds the 100s idle
        // threshold by the time this runs at 1500.
        switch_project(&conn, Some("demo"), 1500, 100, None).expect("switch");

        let (end_ts, end_reason): (Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT end_ts, end_reason FROM project_sessions WHERE project_slug = 'demo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("row");
        assert_eq!(end_reason, Some("idle".to_string()));
        assert_eq!(end_ts, Some(1110), "backdated to last_activity(1010) + idle_threshold(100), not now(1500)");
    }

    /// A session with NO commands observed yet (freshly opened, no
    /// `history` row landed in it) still idles out correctly relative
    /// to its own `start_ts`, not an earlier unrelated command.
    #[test]
    fn switch_project_idles_out_from_start_ts_when_no_activity_recorded() {
        let conn = project_lifecycle_test_conn();
        switch_project(&conn, Some("demo"), 1000, 100, None).expect("switch");
        switch_project(&conn, Some("demo"), 1200, 100, None).expect("switch");

        let (end_ts, end_reason): (Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT end_ts, end_reason FROM project_sessions WHERE project_slug = 'demo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("row");
        assert_eq!(end_reason, Some("idle".to_string()));
        assert_eq!(end_ts, Some(1100), "backdated to start_ts(1000) + idle_threshold(100)");
    }

    /// `smarthistory project select`'s explicit switch uses
    /// `forced_reason = Some("switch")`, overriding the default
    /// `"directory_change"` even though the underlying trigger
    /// (a project mismatch) is structurally identical.
    #[test]
    fn switch_project_forced_reason_used_for_explicit_switch() {
        let conn = project_lifecycle_test_conn();
        switch_project(&conn, Some("demo"), 1000, 1800, None).expect("switch");
        switch_project(&conn, Some("other"), 1050, 1800, Some("switch")).expect("switch");

        let end_reason: Option<String> = conn
            .query_row(
                "SELECT end_reason FROM project_sessions WHERE project_slug = 'demo'",
                [],
                |r| r.get(0),
            )
            .expect("row");
        assert_eq!(end_reason, Some("switch".to_string()));
    }

    /// A resolved project of `None` (moved to an untracked directory)
    /// closes the open session without opening a replacement.
    #[test]
    fn switch_project_none_closes_without_opening_new() {
        let conn = project_lifecycle_test_conn();
        switch_project(&conn, Some("demo"), 1000, 1800, None).expect("switch");
        switch_project(&conn, None, 1050, 1800, None).expect("switch");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM project_sessions", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 1, "no new session opened for an unresolved directory");
        let end_ts: Option<i64> = conn
            .query_row("SELECT end_ts FROM project_sessions", [], |r| r.get(0))
            .expect("row");
        assert_eq!(end_ts, Some(1050));
    }

    // --- Time tracking: resolve_current_project (`project current`) ----

    /// Fixture for `resolve_current_project`: needs `project_current`
    /// on top of `project_lifecycle_test_conn`'s schema (the marker-
    /// file tier is exercised separately by the
    /// `find_project_marker_*` tests — no filesystem fixture needed
    /// here since these tests use a `pwd` with no marker file).
    fn resolve_current_project_test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE project_current (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 project_slug TEXT NOT NULL,
                 set_ts INTEGER NOT NULL
             );
             CREATE TABLE project_pause (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 paused_slug TEXT,
                 paused_at INTEGER NOT NULL
             );",
        )
        .expect("schema");
        conn
    }

    #[test]
    fn resolve_current_project_prefers_dir_match_over_project_current() {
        let conn = resolve_current_project_test_conn();
        conn.execute(
            "INSERT INTO project_current (id, project_slug, set_ts) VALUES (1, 'other', 1000)",
            [],
        )
        .expect("insert");
        let mut cfg = Config::default();
        cfg.parse_multi(&["project.demo.dir = /tmp/work\n"]);
        assert_eq!(
            resolve_current_project(&conn, &cfg, "/tmp/work/subdir").unwrap(),
            Some("demo".to_string()),
            "a directory match must win over the explicit project_current fallback"
        );
    }

    #[test]
    fn resolve_current_project_falls_back_to_project_current_when_no_dir_match() {
        let conn = resolve_current_project_test_conn();
        conn.execute(
            "INSERT INTO project_current (id, project_slug, set_ts) VALUES (1, 'other', 1000)",
            [],
        )
        .expect("insert");
        let cfg = Config::default();
        assert_eq!(
            resolve_current_project(&conn, &cfg, "/tmp/unrelated").unwrap(),
            Some("other".to_string())
        );
    }

    #[test]
    fn resolve_current_project_returns_none_when_nothing_resolves() {
        let conn = resolve_current_project_test_conn();
        let cfg = Config::default();
        assert_eq!(resolve_current_project(&conn, &cfg, "/tmp/unrelated").unwrap(), None);
    }

    // --- Time tracking: `project pause` ---------------------------------

    #[test]
    fn is_project_tracking_paused_reflects_project_pause_row() {
        let conn = resolve_current_project_test_conn();
        assert!(!is_project_tracking_paused(&conn).unwrap());
        conn.execute(
            "INSERT INTO project_pause (id, paused_slug, paused_at) VALUES (1, 'demo', 1000)",
            [],
        )
        .expect("insert");
        assert!(is_project_tracking_paused(&conn).unwrap());
    }

    /// The whole point of pausing: even a directory bound to a
    /// project (which would normally win outright — see
    /// `resolve_current_project_prefers_dir_match_over_project_current`)
    /// must resolve to `None` while paused.
    #[test]
    fn resolve_current_project_ignores_directory_and_explicit_selection_while_paused() {
        let conn = resolve_current_project_test_conn();
        conn.execute(
            "INSERT INTO project_current (id, project_slug, set_ts) VALUES (1, 'other', 1000)",
            [],
        )
        .expect("insert");
        conn.execute(
            "INSERT INTO project_pause (id, paused_slug, paused_at) VALUES (1, 'demo', 1000)",
            [],
        )
        .expect("insert");
        let mut cfg = Config::default();
        cfg.parse_multi(&["project.demo.dir = /tmp/work\n"]);
        assert_eq!(resolve_current_project(&conn, &cfg, "/tmp/work/subdir").unwrap(), None);
        assert_eq!(resolve_current_project(&conn, &cfg, "/tmp/unrelated").unwrap(), None);
    }

    // --- Time tracking: report Commands-table grouping ("Nx" counter) --

    fn command_row(command: &str, directory: &str, timestamp: i64, active_secs: i64) -> ReportCommandRow {
        ReportCommandRow {
            command: command.to_string(),
            directory: directory.to_string(),
            project_slug: None,
            timestamp,
            active_secs,
        }
    }

    #[test]
    fn group_command_rows_collapses_same_command_and_directory_across_sessions() {
        // `history`'s own dedup upsert (`idx_history_dedup`) already
        // collapses repeats within one shell session, so this
        // fixture models what a report actually sees: the same
        // command in the same directory at three different
        // timestamps (three different panes/sessions during the
        // day) — three separate `ReportCommandRow`s, not one.
        let a = command_row("git status", "/work", 1000, 2);
        let b = command_row("git status", "/work", 2000, 3);
        let c = command_row("git status", "/work", 3000, 1);
        let rows = vec![&a, &b, &c];
        let groups = group_command_rows(&rows);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 3);
        assert_eq!(groups[0].total_secs, 6, "durations must sum, not just count occurrences");
        assert_eq!(groups[0].command, "git status");
    }

    #[test]
    fn group_command_rows_keeps_different_commands_and_directories_separate() {
        let a = command_row("git status", "/work", 1000, 2);
        let b = command_row("cargo build", "/work", 2000, 3);
        let c = command_row("git status", "/other", 3000, 1);
        let rows = vec![&a, &b, &c];
        let groups = group_command_rows(&rows);
        assert_eq!(groups.len(), 3, "same command in a different directory is a different group");
        assert!(groups.iter().all(|g| g.count == 1));
    }

    #[test]
    fn group_command_rows_preserves_first_appearance_order() {
        let a = command_row("zzz", "/work", 1000, 1);
        let b = command_row("aaa", "/work", 2000, 1);
        let c = command_row("zzz", "/work", 3000, 1);
        let rows = vec![&a, &b, &c];
        let groups = group_command_rows(&rows);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].command, "zzz", "first-seen command stays first, not alphabetized");
        assert_eq!(groups[1].command, "aaa");
    }

    #[test]
    fn escape_md_table_cell_escapes_pipes_and_strips_newlines() {
        assert_eq!(escape_md_table_cell("grep foo | wc -l"), "grep foo \\| wc -l");
        assert_eq!(escape_md_table_cell("line1\nline2"), "line1 line2");
        assert_eq!(escape_md_table_cell("plain text"), "plain text");
    }

    // --- Time tracking: project report ------------------------------

    #[test]
    fn parse_project_report_day_defaults_to_today_spanning_exactly_one_day() {
        let (start, end, date) = parse_project_report_day(&None).expect("parse");
        assert_eq!(date, chrono::Local::now().date_naive());
        assert_eq!(end - start, 86400, "a calendar day is exactly 24h wide");
    }

    #[test]
    fn parse_project_report_day_parses_explicit_date() {
        let (_start, _end, date) =
            parse_project_report_day(&Some("2024-01-15".to_string())).expect("parse");
        assert_eq!(date, chrono::NaiveDate::from_ymd_opt(2024, 1, 15).unwrap());
    }

    #[test]
    fn parse_project_report_day_yesterday_is_one_day_before_today() {
        let (_s1, _e1, today) = parse_project_report_day(&None).expect("parse");
        let (_s2, _e2, yesterday) =
            parse_project_report_day(&Some("yesterday".to_string())).expect("parse");
        assert_eq!(yesterday, today - chrono::Duration::days(1));
    }

    #[test]
    fn parse_project_report_day_rejects_garbage() {
        assert!(parse_project_report_day(&Some("not-a-date".to_string())).is_err());
    }

    #[test]
    fn format_duration_secs_picks_the_coarsest_useful_unit() {
        assert_eq!(format_duration_secs(0), "0s");
        assert_eq!(format_duration_secs(45), "45s");
        assert_eq!(format_duration_secs(90), "1m30s");
        assert_eq!(format_duration_secs(3600), "1h00m");
        assert_eq!(format_duration_secs(3665), "1h01m");
    }

    /// Fixture matching the subset of `history`'s real schema
    /// `report_command_rows` reads, plus `project_sessions` — a
    /// superset of `project_lifecycle_test_conn`'s history table
    /// (adds `command`/`directory`/`session_id`/`mode`, needed for
    /// the report's per-command duration query).
    fn report_test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE history (
                 id INTEGER PRIMARY KEY,
                 command TEXT NOT NULL,
                 directory TEXT NOT NULL,
                 session_id TEXT NOT NULL,
                 exit_code INTEGER,
                 timestamp INTEGER NOT NULL,
                 mode TEXT NOT NULL DEFAULT 'command'
             );
             CREATE TABLE project_sessions (
                 id INTEGER PRIMARY KEY,
                 project_slug TEXT NOT NULL,
                 start_ts INTEGER NOT NULL,
                 end_ts INTEGER,
                 end_reason TEXT
             );",
        )
        .expect("schema");
        conn
    }

    fn insert_history(conn: &Connection, command: &str, directory: &str, session_id: &str, ts: i64) {
        conn.execute(
            "INSERT INTO history (command, directory, session_id, timestamp) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![command, directory, session_id, ts],
        )
        .expect("insert history");
    }

    /// The core correctness case from the spec: a command's derived
    /// duration is `min(gap to the next event, idle_threshold)`, and
    /// that gap is computed *within its own session/pane* — a long
    /// gap in one pane must not inflate a command's duration in a
    /// different, concurrently-active pane.
    #[test]
    fn report_command_rows_caps_duration_and_partitions_by_session() {
        let conn = report_test_conn();
        conn.execute(
            "INSERT INTO project_sessions (project_slug, start_ts, end_ts) VALUES ('demo', 1000, 2000)",
            [],
        )
        .expect("insert session");
        // paneA: a 600s gap between its two commands.
        insert_history(&conn, "build", "/repo", "paneA", 1000);
        insert_history(&conn, "test", "/repo", "paneA", 1600);
        // paneB: a single command with no successor in its own
        // partition — falls back to the session's end_ts (2000).
        insert_history(&conn, "watch", "/repo", "paneB", 1100);

        let rows = report_command_rows(&conn, 500, 2500, 300).expect("query");
        assert_eq!(rows.len(), 3);
        for r in &rows {
            assert_eq!(r.project_slug.as_deref(), Some("demo"));
            assert_eq!(
                r.active_secs, 300,
                "command {:?}: gap capped at the 300s idle threshold regardless of which pane produced it",
                r.command
            );
        }
    }

    #[test]
    fn report_command_rows_leaves_project_slug_none_outside_any_session() {
        let conn = report_test_conn();
        conn.execute(
            "INSERT INTO project_sessions (project_slug, start_ts, end_ts) VALUES ('demo', 1000, 2000)",
            [],
        )
        .expect("insert session");
        insert_history(&conn, "later", "/other", "paneC", 3000);

        let rows = report_command_rows(&conn, 2500, 3500, 300).expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].project_slug, None, "3000 falls after the session's end_ts=2000");
    }

    #[test]
    fn project_sessions_in_range_clamps_still_open_session_to_now() {
        let conn = report_test_conn();
        conn.execute(
            "INSERT INTO project_sessions (project_slug, start_ts, end_ts) VALUES ('demo', 1000, NULL)",
            [],
        )
        .expect("insert session");

        let sessions = project_sessions_in_range(&conn, 0, 5000, 1500).expect("query");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].effective_end, 1500, "an open session clamps to `now`, not the range end");
        assert!(sessions[0].still_open);
    }

    #[test]
    fn project_sessions_in_range_excludes_sessions_entirely_outside_the_window() {
        let conn = report_test_conn();
        conn.execute(
            "INSERT INTO project_sessions (project_slug, start_ts, end_ts) VALUES ('demo', 1000, 1100)",
            [],
        )
        .expect("insert session");

        let sessions = project_sessions_in_range(&conn, 2000, 3000, 3500).expect("query");
        assert!(sessions.is_empty());
    }

    // --- Time tracking: jiralabel config + resolve_project_by_label -----

    #[test]
    fn jiralabel_config_parses_and_resolves() {
        let mut cfg = Config::default();
        cfg.parse_multi(&[
            "jiralabel.acme.match = acme-corp\n\
             jiralabel.beta.match = beta-team\n",
        ]);
        assert_eq!(
            resolve_project_by_label(&cfg, &["acme-corp".to_string(), "urgent".to_string()]),
            Some("acme".to_string())
        );
        assert_eq!(
            resolve_project_by_label(&cfg, &["beta-team".to_string()]),
            Some("beta".to_string())
        );
        assert_eq!(resolve_project_by_label(&cfg, &["unrelated".to_string()]), None);
        assert_eq!(resolve_project_by_label(&cfg, &[]), None);
    }

    /// When a ticket carries labels matching more than one
    /// configured project, the earliest-declared `jiralabel.<slug>.match`
    /// wins — same "first in file order" tie-break `session.<key>`/
    /// `host.<key>` use elsewhere.
    #[test]
    fn jiralabel_resolution_prefers_first_declared_on_multi_match() {
        let mut cfg = Config::default();
        cfg.parse_multi(&[
            "jiralabel.acme.match = shared-label\n\
             jiralabel.beta.match = other-label\n",
        ]);
        assert_eq!(
            resolve_project_by_label(&cfg, &["other-label".to_string(), "shared-label".to_string()]),
            Some("acme".to_string()),
            "acme was declared first in the config, regardless of label order in the ticket"
        );
    }

    #[test]
    fn jiralabel_match_is_case_sensitive() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["jiralabel.acme.match = Acme-Corp\n"]);
        assert_eq!(
            resolve_project_by_label(&cfg, &["acme-corp".to_string()]),
            None,
            "JIRA labels are case-sensitive; a lowercase mismatch should not match"
        );
        assert_eq!(
            resolve_project_by_label(&cfg, &["Acme-Corp".to_string()]),
            Some("acme".to_string())
        );
    }

    // --- Time tracking: weburl/weburlgroup + 3-tier website resolution --

    #[test]
    fn url_host_and_path_strips_scheme_query_and_fragment() {
        assert_eq!(
            url_host_and_path("https://example.com/path?x=1#frag"),
            "example.com/path"
        );
        assert_eq!(url_host_and_path("no-scheme.example.com/x"), "no-scheme.example.com/x");
    }

    #[test]
    fn weburl_config_parses_and_resolves() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["weburl.acme.match = acme.example.com\n"]);
        assert_eq!(
            resolve_project_by_weburl(&cfg, "https://acme.example.com/docs?x=1"),
            Some("acme".to_string())
        );
        assert_eq!(resolve_project_by_weburl(&cfg, "https://unrelated.com/x"), None);
    }

    #[test]
    fn weburlgroup_config_parses_and_clusters_independently_of_assignment() {
        let mut cfg = Config::default();
        cfg.parse_multi(&[
            "weburlgroup.jira.match = /browse/\n\
             weburlgroup.jira.label = JIRA tickets\n",
        ]);
        assert_eq!(
            cluster_label_for_url(&cfg, "https://jira.example.com/browse/PROJ-1"),
            Some("JIRA tickets".to_string())
        );
        assert_eq!(cluster_label_for_url(&cfg, "https://unrelated.com/x"), None);
        // Clustering has nothing to do with project assignment — no
        // `weburl`/`jiralabel` entries configured here at all, so
        // assignment must stay `None` even though the URL clusters.
        assert_eq!(resolve_project_by_weburl(&cfg, "https://jira.example.com/browse/PROJ-1"), None);
    }

    /// A `JiraClient` fake for `resolve_project_for_website_visit`'s
    /// tier tests. Always returns the same fixed label set,
    /// regardless of the JQL query — these tests only care about
    /// resolution priority, not JQL construction (already covered by
    /// `jira::tests::labels_for_issue_*`).
    struct FixedLabelClient {
        labels: Vec<String>,
    }

    impl crate::jira::JiraClient for FixedLabelClient {
        fn search(&self, _jql: &str) -> Result<Vec<crate::jira::JiraIssue>, crate::jira::JiraError> {
            Ok(vec![crate::jira::JiraIssue {
                key: "PROJ-1".to_string(),
                labels: self.labels.clone(),
                ..Default::default()
            }])
        }
        fn fetch_comments(&self, _key: &str) -> Result<Vec<crate::jira::JiraComment>, crate::jira::JiraError> {
            Ok(Vec::new())
        }
        fn add_comment(&self, _key: &str, _body: &str) -> Result<(), crate::jira::JiraError> {
            Ok(())
        }
    }

    #[test]
    fn resolve_project_for_website_visit_prefers_jira_label_over_weburl() {
        let mut cfg = Config::default();
        cfg.parse_multi(&[
            "jiralabel.acme.match = acme-label\n\
             weburl.beta.match = jira.example.com\n",
        ]);
        let client = FixedLabelClient {
            labels: vec!["acme-label".to_string()],
        };
        let mut cache = std::collections::HashMap::new();
        let slug = resolve_project_for_website_visit(
            &cfg,
            Some(&client),
            &mut cache,
            "https://jira.example.com/browse/PROJ-1",
            1000,
            &[],
        );
        assert_eq!(
            slug,
            Some("acme".to_string()),
            "the ticket's own label must win even though the domain also matches a weburl override for a different project"
        );
    }

    #[test]
    fn resolve_project_for_website_visit_falls_back_to_weburl_without_jira_client() {
        let mut cfg = Config::default();
        cfg.parse_multi(&["weburl.beta.match = docs.example.com\n"]);
        let mut cache = std::collections::HashMap::new();
        let slug = resolve_project_for_website_visit(
            &cfg,
            None,
            &mut cache,
            "https://docs.example.com/guide",
            1000,
            &[],
        );
        assert_eq!(slug, Some("beta".to_string()));
    }

    #[test]
    fn resolve_project_for_website_visit_falls_back_to_time_based_session() {
        let cfg = Config::default();
        let mut cache = std::collections::HashMap::new();
        let sessions = vec![ProjectSessionInterval {
            slug: "gamma".to_string(),
            start_ts: 500,
            effective_end: 1500,
            still_open: false,
        }];
        let slug = resolve_project_for_website_visit(
            &cfg,
            None,
            &mut cache,
            "https://unrelated.example.com/x",
            1000,
            &sessions,
        );
        assert_eq!(slug, Some("gamma".to_string()));
    }

    /// Regression: a still-open session's `effective_end` is only a
    /// clamp-to-`now` for display purposes, not a real boundary — a
    /// visit timestamped in the same wall-clock second the report
    /// runs (`timestamp == effective_end`) must still fall inside an
    /// open session's window. A strict `timestamp < effective_end`
    /// check would wrongly exclude it and misfile the visit as
    /// "untracked".
    #[test]
    fn resolve_project_for_website_visit_matches_open_session_at_exact_now_boundary() {
        let cfg = Config::default();
        let mut cache = std::collections::HashMap::new();
        let sessions = vec![ProjectSessionInterval {
            slug: "acme".to_string(),
            start_ts: 500,
            effective_end: 1000,
            still_open: true,
        }];
        let slug = resolve_project_for_website_visit(
            &cfg,
            None,
            &mut cache,
            "https://unrelated.example.com/x",
            1000,
            &sessions,
        );
        assert_eq!(slug, Some("acme".to_string()));
    }

    #[test]
    fn resolve_project_for_website_visit_returns_none_when_nothing_matches() {
        let cfg = Config::default();
        let mut cache = std::collections::HashMap::new();
        let slug = resolve_project_for_website_visit(
            &cfg,
            None,
            &mut cache,
            "https://unrelated.example.com/x",
            1000,
            &[],
        );
        assert_eq!(slug, None);
    }

    // --- Time tracking: website host-clustering, dedup, markdown links --

    #[test]
    fn url_host_strips_scheme_www_port_userinfo_and_path() {
        assert_eq!(url_host("https://www.github.com/org/repo"), "github.com");
        assert_eq!(url_host("https://github.com:8443/org/repo"), "github.com");
        assert_eq!(url_host("https://user:pass@github.com/org/repo"), "github.com");
        assert_eq!(url_host("github.com/org/repo?x=1#frag"), "github.com");
    }

    #[test]
    fn extract_quoted_url_pulls_url_from_staged_open_command() {
        assert_eq!(
            extract_quoted_url(r#"open "https://jira.example.com/browse/PROJ-1""#),
            Some("https://jira.example.com/browse/PROJ-1")
        );
        assert_eq!(
            extract_quoted_url(r#"xdg-open "https://jira.example.com/browse/PROJ-1""#),
            Some("https://jira.example.com/browse/PROJ-1")
        );
        assert_eq!(extract_quoted_url("no quotes here"), None);
    }

    #[test]
    fn group_website_links_clusters_and_dedupes_by_url() {
        let links = vec![
            WebsiteLink {
                cluster: "github.com".to_string(),
                title: "Repo".to_string(),
                url: "https://github.com/org/repo".to_string(),
            },
            // Same URL visited twice — must collapse to one entry,
            // keeping the first title seen.
            WebsiteLink {
                cluster: "github.com".to_string(),
                title: "Repo (again)".to_string(),
                url: "https://github.com/org/repo".to_string(),
            },
            WebsiteLink {
                cluster: "github.com".to_string(),
                title: "Issue #4".to_string(),
                url: "https://github.com/org/repo/issues/4".to_string(),
            },
            WebsiteLink {
                cluster: "JIRA tickets".to_string(),
                title: "BETA-42".to_string(),
                url: "https://jira.example.com/browse/BETA-42".to_string(),
            },
        ];
        let grouped = group_website_links(&links);
        assert_eq!(grouped.len(), 2, "two distinct clusters");

        // Clusters sort in plain byte order (uppercase before
        // lowercase, same as every other BTreeMap-sorted list in
        // this report), so "JIRA tickets" comes before "github.com".
        let (cluster, jira_links) = &grouped[0];
        assert_eq!(*cluster, "JIRA tickets");
        assert_eq!(jira_links.len(), 1);

        let (cluster2, github_links) = &grouped[1];
        assert_eq!(*cluster2, "github.com");
        assert_eq!(github_links.len(), 2, "the duplicate URL must collapse to one entry");
        assert!(
            github_links.contains(&("https://github.com/org/repo", "Repo")),
            "first-seen title wins on a duplicate URL: {:?}",
            github_links
        );
    }

    #[test]
    fn escape_md_link_text_neutralizes_bracket_and_paren_syntax() {
        assert_eq!(escape_md_link_text("[urgent] fix"), "(urgent) fix");
        assert_eq!(escape_md_link_text("plain title"), "plain title");
    }

    #[test]
    fn note_basename_strips_extension_only() {
        assert_eq!(note_basename("Standup.md"), "Standup");
        assert_eq!(note_basename("2026-08-14-notes.md"), "2026-08-14-notes");
        assert_eq!(note_basename("no-extension"), "no-extension");
    }

    // --- Time tracking: file-tracking events (`smarthistory file ...`) --

    fn file_events_test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE file_events (
                 id INTEGER PRIMARY KEY,
                 path TEXT NOT NULL,
                 event_kind TEXT NOT NULL,
                 project_slug TEXT,
                 timestamp INTEGER NOT NULL
             );",
        )
        .expect("schema");
        conn
    }

    fn insert_file_event(conn: &Connection, path: &str, kind: &str, slug: Option<&str>, ts: i64) {
        conn.execute(
            "INSERT INTO file_events (path, event_kind, project_slug, timestamp) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![path, kind, slug, ts],
        )
        .expect("insert file_event");
    }

    #[test]
    fn report_file_events_groups_by_project_and_kind() {
        let conn = file_events_test_conn();
        insert_file_event(&conn, "/work/acme/src/main.rs", "viewed", Some("acme"), 1000);
        insert_file_event(&conn, "/work/acme/src/main.rs", "modified", Some("acme"), 1100);
        insert_file_event(&conn, "/work/acme/README.md", "created", Some("acme"), 1200);
        insert_file_event(&conn, "/tmp/scratch.txt", "viewed", None, 1300);

        let by_slug = report_file_events(&conn, 500, 2000).expect("query");
        assert_eq!(by_slug.len(), 2, "two distinct project buckets: acme and untracked");

        let acme = by_slug.get(&Some("acme".to_string())).expect("acme bucket");
        assert_eq!(acme.viewed.get("/work/acme/src/main.rs"), Some(&1));
        assert_eq!(acme.modified.get("/work/acme/src/main.rs"), Some(&1));
        assert_eq!(acme.created.get("/work/acme/README.md"), Some(&1));
        assert!(!acme.modified.contains_key("/work/acme/README.md"));

        let untracked = by_slug.get(&None).expect("untracked bucket");
        assert_eq!(untracked.viewed.get("/tmp/scratch.txt"), Some(&1));
    }

    #[test]
    fn report_file_events_dedupes_by_path_with_occurrence_count() {
        let conn = file_events_test_conn();
        insert_file_event(&conn, "/work/acme/src/main.rs", "viewed", Some("acme"), 1000);
        insert_file_event(&conn, "/work/acme/src/main.rs", "viewed", Some("acme"), 1100);
        insert_file_event(&conn, "/work/acme/src/main.rs", "viewed", Some("acme"), 1200);

        let by_slug = report_file_events(&conn, 500, 2000).expect("query");
        let acme = by_slug.get(&Some("acme".to_string())).expect("acme bucket");
        assert_eq!(
            acme.viewed.get("/work/acme/src/main.rs"),
            Some(&3),
            "three viewed events for the same path must collapse to one entry with count 3"
        );
    }

    #[test]
    fn report_file_events_respects_day_range() {
        let conn = file_events_test_conn();
        insert_file_event(&conn, "/work/acme/old.rs", "viewed", Some("acme"), 100);
        insert_file_event(&conn, "/work/acme/in-range.rs", "viewed", Some("acme"), 1000);
        insert_file_event(&conn, "/work/acme/future.rs", "viewed", Some("acme"), 5000);

        let by_slug = report_file_events(&conn, 500, 2000).expect("query");
        let acme = by_slug.get(&Some("acme".to_string())).expect("acme bucket");
        assert_eq!(acme.viewed.len(), 1);
        assert!(acme.viewed.contains_key("/work/acme/in-range.rs"));
    }

    // --- Time tracking: `config check` validation -----------------------

    /// Write a config file under a fresh fake `$HOME` and run
    /// `validate_config()` against it. Holds `ENV_LOCK` for the
    /// duration since `$HOME` is process-global — same convention
    /// `config_parses_user_file` above uses.
    fn validate_project_config(body: &str) -> ConfigReport {
        let _guard = crate::tui::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        validate_project_config_locked(body)
    }

    /// The guts of `validate_project_config`, minus the lock
    /// acquisition — for callers that need to hold `ENV_LOCK` across
    /// *more* than just this function (e.g. also mutating
    /// `JIRA_SERVER`/`JIRA_API_TOKEN`, which `validate_config`'s
    /// `jiralabel.*` check reads). `Mutex` isn't reentrant, so a
    /// caller that already holds the lock must call this directly,
    /// never `validate_project_config` (which would deadlock trying
    /// to lock it again on the same thread).
    fn validate_project_config_locked(body: &str) -> ConfigReport {
        let dir = std::env::temp_dir().join(format!("smarthistory-test-{}", generate_uuid_v4()));
        let cfg_dir = dir.join(".config").join("smarthistory");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir");
        std::fs::write(cfg_dir.join("config"), body).expect("write");
        let prev_home = std::env::var("HOME").ok();
        // `Config::notes_database()` also honors `NOTE_SEARCH_DATABASE`
        // (see `Config::load`'s parse loop) — a real developer
        // environment commonly has this set to a real, possibly
        // large notes vault, which would make the project/note
        // cross-check in `validate_config` query real data (slow,
        // and non-deterministic across machines) instead of this
        // test's isolated fixture. Cleared for the duration.
        let prev_notes_db = std::env::var("NOTE_SEARCH_DATABASE").ok();
        unsafe {
            std::env::set_var("HOME", &dir);
            std::env::remove_var("NOTE_SEARCH_DATABASE");
        }
        let report = validate_config();
        match prev_home {
            Some(p) => unsafe {
                std::env::set_var("HOME", p);
            },
            None => unsafe {
                std::env::remove_var("HOME");
            },
        }
        match prev_notes_db {
            Some(v) => unsafe {
                std::env::set_var("NOTE_SEARCH_DATABASE", v);
            },
            None => unsafe {
                std::env::remove_var("NOTE_SEARCH_DATABASE");
            },
        }
        let _ = std::fs::remove_dir_all(&dir);
        report
    }

    #[test]
    fn validate_config_flags_non_numeric_idlethreshold_as_error() {
        let report = validate_project_config("project.idlethreshold = notanumber\n");
        assert!(report.has_errors());
        assert!(
            report
                .issues()
                .iter()
                .any(|i| i.category == "project" && i.message.contains("idlethreshold")),
            "issues: {:?}",
            report.issues()
        );
    }

    #[test]
    fn validate_config_flags_non_positive_idlethreshold_as_error() {
        let report = validate_project_config("project.idlethreshold = 0\n");
        assert!(report.has_errors());
    }

    #[test]
    fn validate_config_accepts_valid_idlethreshold() {
        let report = validate_project_config("project.idlethreshold = 900\n");
        assert!(
            !report
                .issues()
                .iter()
                .any(|i| i.category == "project" && i.message.contains("idlethreshold")),
            "issues: {:?}",
            report.issues()
        );
    }

    #[test]
    fn validate_config_warns_on_jiralabel_without_jira_credentials() {
        // Hold `ENV_LOCK` for the JIRA env mutation too, not just
        // `validate_project_config_locked`'s own $HOME/NOTE_SEARCH_DATABASE
        // handling — otherwise a concurrently-running test (under real
        // parallel `cargo test`, not just `--test-threads=1`) can set
        // JIRA_SERVER/JIRA_API_TOKEN between this test clearing them
        // and `validate_config` reading them, making the expected
        // warning silently disappear. Calls the `_locked` variant
        // directly (not `validate_project_config`, which would try to
        // acquire this same non-reentrant lock again and deadlock).
        let _guard = crate::tui::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_server = std::env::var("JIRA_SERVER").ok();
        let prev_token = std::env::var("JIRA_API_TOKEN").ok();
        unsafe {
            std::env::remove_var("JIRA_SERVER");
            std::env::remove_var("JIRA_API_TOKEN");
        }
        let report = validate_project_config_locked("jiralabel.acme.match = acme-label\n");
        match prev_server {
            Some(v) => unsafe { std::env::set_var("JIRA_SERVER", v) },
            None => unsafe { std::env::remove_var("JIRA_SERVER") },
        }
        match prev_token {
            Some(v) => unsafe { std::env::set_var("JIRA_API_TOKEN", v) },
            None => unsafe { std::env::remove_var("JIRA_API_TOKEN") },
        }
        assert!(
            report
                .issues()
                .iter()
                .any(|i| i.category == "jiralabel"),
            "issues: {:?}",
            report.issues()
        );
    }

    #[test]
    fn validate_config_no_jiralabel_warning_without_any_jiralabel_entries() {
        // See the previous test's comment: holds `ENV_LOCK` across
        // both the JIRA env mutation and the `_locked` validation
        // call, so no concurrently-running test can interleave.
        let _guard = crate::tui::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_server = std::env::var("JIRA_SERVER").ok();
        let prev_token = std::env::var("JIRA_API_TOKEN").ok();
        unsafe {
            std::env::remove_var("JIRA_SERVER");
            std::env::remove_var("JIRA_API_TOKEN");
        }
        let report = validate_project_config_locked("project.idlethreshold = 900\n");
        match prev_server {
            Some(v) => unsafe { std::env::set_var("JIRA_SERVER", v) },
            None => unsafe { std::env::remove_var("JIRA_SERVER") },
        }
        match prev_token {
            Some(v) => unsafe { std::env::set_var("JIRA_API_TOKEN", v) },
            None => unsafe { std::env::remove_var("JIRA_API_TOKEN") },
        }
        assert!(
            !report.issues().iter().any(|i| i.category == "jiralabel"),
            "no jiralabel.* configured, so there's nothing to warn about: {:?}",
            report.issues()
        );
    }
