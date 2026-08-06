    use super::*;

    #[test]
    fn glob_to_regex_star_rs() {
        assert_eq!(glob_to_ag_regex("*.rs"), r".*\.rs$");
    }

    #[test]
    fn glob_to_regex_bla_star_txt() {
        assert_eq!(glob_to_ag_regex("bla*.txt"), r"bla.*\.txt$");
    }

    #[test]
    fn glob_to_regex_all_files() {
        assert_eq!(glob_to_ag_regex("*"), r".*$");
    }

    #[test]
    fn glob_to_regex_escapes_dot() {
        assert_eq!(glob_to_ag_regex("*.min.js"), r".*\.min\.js$");
    }

    #[test]
    fn glob_to_regex_escapes_plus() {
        assert_eq!(glob_to_ag_regex("file*.c++"), r"file.*\.c\+\+$");
    }

    #[test]
    fn glob_to_regex_no_star_is_literal() {
        assert_eq!(glob_to_ag_regex("Makefile"), r"Makefile$");
    }

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
    /// extracted so it can be tested directly without spawning the
    /// real `ag` binary. Newest `timestamp` first.
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
