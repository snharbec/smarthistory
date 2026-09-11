    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn file_mtime_reads_real_file_mtime() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_ag_test_{}_{}",
            std::process::id(),
            "mtime"
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("touched.txt");
        std::fs::write(&path, "x").unwrap();
        let expected = std::fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(file_mtime(&path.to_string_lossy()), expected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_mtime_missing_file_returns_zero() {
        assert_eq!(file_mtime("/nonexistent/path/that/does/not/exist"), 0);
    }

    /// `sort_rows_newest_modified_first` is `run_ag`'s ordering,
    /// extracted so it can be tested directly without a real
    /// filesystem walk. Newest `timestamp` first.
    #[test]
    fn sort_rows_newest_modified_first_orders_by_timestamp_desc() {
        let mut rows = vec![
            HistoryRow {
                command: "old match".to_string(),
                timestamp: 100,
                ..Default::default()
            },
            HistoryRow {
                command: "newest match".to_string(),
                timestamp: 300,
                ..Default::default()
            },
            HistoryRow {
                command: "middle match".to_string(),
                timestamp: 200,
                ..Default::default()
            },
        ];
        sort_rows_newest_modified_first(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r.command.as_str()).collect();
        assert_eq!(order, vec!["newest match", "middle match", "old match"]);
    }

    /// Multiple matches within the SAME file share that file's
    /// mtime, so they're tied on the sort key — the stable sort
    /// must preserve their original (line-number ascending) order
    /// rather than shuffling them.
    #[test]
    fn sort_rows_newest_modified_first_is_stable_for_same_file_matches() {
        let mut rows = vec![
            HistoryRow {
                command: "line 5 match".to_string(),
                session_id: "5".to_string(),
                timestamp: 100,
                ..Default::default()
            },
            HistoryRow {
                command: "line 10 match".to_string(),
                session_id: "10".to_string(),
                timestamp: 100,
                ..Default::default()
            },
            HistoryRow {
                command: "line 20 match".to_string(),
                session_id: "20".to_string(),
                timestamp: 100,
                ..Default::default()
            },
        ];
        sort_rows_newest_modified_first(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(order, vec!["5", "10", "20"]);
    }

    // --- End-to-end `run_ag` tests -------------------------------------
    //
    // Feasible/safe now (unlike with the old `ag`-subprocess design)
    // because `run_ag` takes an explicit `root: &Path` instead of
    // implicitly searching `.` — no `std::env::set_current_dir` needed,
    // so nothing here interferes with other tests running in parallel
    // threads.

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_ag_e2e_{}_{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn no_cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn run_ag_finds_a_basic_match() {
        let dir = temp_dir("basic");
        std::fs::write(dir.join("readme.txt"), "hello NEEDLE world\n").unwrap();

        let rows = run_ag("NEEDLE", &dir, &no_cancel());

        assert_eq!(rows.len(), 1, "expected exactly one match, got {rows:?}");
        assert_eq!(rows[0].command, "hello NEEDLE world");
        assert_eq!(rows[0].directory, dir.join("readme.txt").to_string_lossy());
        assert_eq!(rows[0].session_id, "1");
        assert_eq!(rows[0].comment, "readme.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `.gitignore` is only honored inside an actual git repository —
    /// same as real `git`, and a deliberate divergence from `ag`'s
    /// more lenient "any `.gitignore`-looking file, repo or not"
    /// behavior (see the module-level doc comment) — so this test
    /// must `git init` the temp dir for the exclusion to take effect.
    #[test]
    fn run_ag_respects_gitignore_inside_a_git_repo() {
        let dir = temp_dir("gitignore");
        std::fs::create_dir_all(dir.join("ignored_sub")).unwrap();
        std::fs::write(dir.join("ignored_sub").join("secret.txt"), "NEEDLE hidden\n").unwrap();
        std::fs::write(dir.join("visible.txt"), "NEEDLE visible\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "ignored_sub/\n").unwrap();
        let init = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["init", "-q"])
            .output();
        if init.map(|o| o.status.success()).unwrap_or(false) {
            let rows = run_ag("NEEDLE", &dir, &no_cancel());
            let files: Vec<&str> = rows.iter().map(|r| r.comment.as_str()).collect();
            assert_eq!(files, vec!["visible.txt"], "expected only visible.txt, got {files:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_ag_includes_hidden_dotfiles() {
        let dir = temp_dir("hidden");
        std::fs::write(dir.join(".env"), "NEEDLE=1\n").unwrap();

        let rows = run_ag("NEEDLE", &dir, &no_cancel());

        assert_eq!(rows.len(), 1, "expected the dotfile's match, got {rows:?}");
        assert_eq!(rows[0].comment, ".env");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_ag_skips_binary_files() {
        let dir = temp_dir("binary");
        let mut binary_content = vec![0u8; 8];
        binary_content.extend_from_slice(b"NEEDLE");
        std::fs::write(dir.join("blob.bin"), &binary_content).unwrap();

        let rows = run_ag("NEEDLE", &dir, &no_cancel());

        assert!(rows.is_empty(), "expected the binary file to be skipped, got {rows:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_ag_glob_token_restricts_to_matching_files() {
        let dir = temp_dir("glob");
        std::fs::write(dir.join("match.rs"), "NEEDLE in rust\n").unwrap();
        std::fs::write(dir.join("match.py"), "NEEDLE in python\n").unwrap();

        let rows = run_ag("NEEDLE *.rs", &dir, &no_cancel());

        let files: Vec<&str> = rows.iter().map(|r| r.comment.as_str()).collect();
        assert_eq!(files, vec!["match.rs"], "expected only the .rs file, got {files:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The important regression guard: `@lang` must restrict WHICH
    /// FILES ARE SEARCHED, not just pick a highlight language — this
    /// matches how `ag --rust` behaved (see the module-level doc
    /// comment). A `.py` file containing the term must never surface
    /// when the query says `@rust`.
    #[test]
    fn run_ag_language_token_restricts_walk_not_just_highlight() {
        let dir = temp_dir("lang");
        std::fs::write(dir.join("match.rs"), "NEEDLE in rust\n").unwrap();
        std::fs::write(dir.join("match.py"), "NEEDLE in python\n").unwrap();

        let rows = run_ag("NEEDLE @rust", &dir, &no_cancel());

        let files: Vec<&str> = rows.iter().map(|r| r.comment.as_str()).collect();
        assert_eq!(
            files,
            vec!["match.rs"],
            "the .py match must be excluded by @rust, not just left unhighlighted, got {files:?}"
        );
        assert_eq!(rows[0].source, "ag:rust");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_ag_cancellation_yields_empty_result() {
        let dir = temp_dir("cancel");
        std::fs::write(dir.join("readme.txt"), "NEEDLE\n").unwrap();
        let cancelled = Arc::new(AtomicBool::new(true));

        let rows = run_ag("NEEDLE", &dir, &cancelled);

        assert!(rows.is_empty(), "a cancelled search must yield no rows, got {rows:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_ag_sorts_newest_modified_file_first_end_to_end() {
        let dir = temp_dir("sort");
        let old = dir.join("old.txt");
        let new = dir.join("new.txt");
        std::fs::write(&old, "NEEDLE old\n").unwrap();
        std::fs::write(&new, "NEEDLE new\n").unwrap();
        filetime::set_file_mtime(&old, filetime::FileTime::from_unix_time(1_000_000, 0)).unwrap();
        filetime::set_file_mtime(&new, filetime::FileTime::from_unix_time(2_000_000, 0)).unwrap();

        let rows = run_ag("NEEDLE", &dir, &no_cancel());

        let files: Vec<&str> = rows.iter().map(|r| r.comment.as_str()).collect();
        assert_eq!(files, vec!["new.txt", "old.txt"], "expected newest-mtime-first, got {files:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_ag_post_filter_requires_every_extra_term() {
        let dir = temp_dir("postfilter");
        std::fs::write(dir.join("both.txt"), "NEEDLE and extra\n").unwrap();
        std::fs::write(dir.join("only_needle.txt"), "NEEDLE alone\n").unwrap();

        let rows = run_ag("NEEDLE extra", &dir, &no_cancel());

        let files: Vec<&str> = rows.iter().map(|r| r.comment.as_str()).collect();
        assert_eq!(files, vec!["both.txt"], "expected only the line containing both terms, got {files:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
