    use super::*;
    use std::io::Write;

    #[test]
    fn ignore_set_dedupes_and_case_sensitive() {
        let s = IgnoreSet::new(&[
            "target".to_string(),
            "node_modules".to_string(),
            "Target".to_string(),
        ]);
        // The set contains built-ins plus 3 user entries
        // (one duplicate, one new).
        assert!(s.contains(std::ffi::OsStr::new("target")));
        assert!(s.contains(std::ffi::OsStr::new("node_modules")));
        assert!(s.contains(std::ffi::OsStr::new("Target")));
        // Built-ins are present too.
        assert!(s.contains(std::ffi::OsStr::new(".git")));
        assert!(s.contains(std::ffi::OsStr::new("__pycache__")));
    }

    #[test]
    fn ignore_set_rejects_unrelated_names() {
        let s = IgnoreSet::new(&[]);
        assert!(!s.contains(std::ffi::OsStr::new("src")));
        assert!(!s.contains(std::ffi::OsStr::new("Cargo.toml")));
        assert!(!s.contains(std::ffi::OsStr::new("README.md")));
    }

    #[test]
    fn read_preview_bytes_handles_small_text() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_files_test_{}_{}",
            std::process::id(),
            "small_text"
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hello.txt");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"Hello, world!\nLine 2\n").unwrap();
        drop(f);
        let preview = read_preview_bytes(&path).unwrap();
        assert_eq!(preview, "Hello, world!\nLine 2\n");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn read_preview_bytes_returns_none_for_binary() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_files_test_{}_{}",
            std::process::id(),
            "binary"
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("blob.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        // 4 KiB of mostly-zero data with a NUL byte —
        // the NUL triggers the binary heuristic.
        f.write_all(&[0u8; 1024]).unwrap();
        f.write_all(b"AB").unwrap();
        f.write_all(&[0u8; 1024]).unwrap();
        drop(f);
        assert!(read_preview_bytes(&path).is_none());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn read_preview_bytes_returns_none_for_missing_file() {
        let path = Path::new("/nonexistent/path/that/does/not/exist");
        assert!(read_preview_bytes(path).is_none());
    }

    #[test]
    fn read_preview_bytes_caps_at_4kb() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_files_test_{}_{}",
            std::process::id(),
            "large"
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("big.txt");
        let mut f = std::fs::File::create(&path).unwrap();
        // 8 KiB of repeating "abcd\n" — bounded read
        // should return ≤ 4 KiB.
        let chunk = "abcd\n".repeat(200); // 1000 bytes
        for _ in 0..9 {
            f.write_all(chunk.as_bytes()).unwrap();
        }
        drop(f);
        let preview = read_preview_bytes(&path).unwrap();
        assert!(preview.len() <= 4096);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn walk_dir_finds_nested_file() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walk_test_{}_{}",
            std::process::id(),
            "nested"
        ));
        let nested = dir.join("a").join("b");
        let _ = std::fs::create_dir_all(&nested);
        let path = nested.join("target.txt");
        std::fs::write(&path, "x").unwrap();
        let tokens: Vec<String> = vec!["target.txt".into()];
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &tokens, &ignore, &mut next_id, &mut rows);
        // The file should be in the result. The
        // intermediate `a/` and `a/b/` directories
        // should NOT match the filter but should NOT
        // prevent the recursion from reaching
        // `a/b/target.txt`.
        assert!(
            rows.iter().any(|r| r.command == "a/b/target.txt"),
            "expected `a/b/target.txt` in {:?}",
            rows.iter().map(|r| &r.command).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_dir_skips_artifact_dirs() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walk_test_{}_{}",
            std::process::id(),
            "ignore"
        ));
        let target = dir.join("target");
        let _ = std::fs::create_dir_all(&target);
        std::fs::write(target.join("artifact.txt"), "x").unwrap();
        let tokens: Vec<String> = vec![];
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &tokens, &ignore, &mut next_id, &mut rows);
        // The `target/` directory itself should be
        // skipped at the entry level, and so should
        // its `artifact.txt` child.
        assert!(
            !rows.iter().any(|r| r.command.contains("target")),
            "expected `target/` to be skipped, got {:?}",
            rows.iter().map(|r| &r.command).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_dir_recurses_through_non_matching_directories() {
        // The bug we fixed earlier: `~main.rs` must
        // still find `src/main.rs` even though `src/`
        // doesn't match.
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walk_test_{}_{}",
            std::process::id(),
            "recurse"
        ));
        let src = dir.join("src");
        let _ = std::fs::create_dir_all(&src);
        std::fs::write(src.join("main.rs"), "x").unwrap();
        let tokens: Vec<String> = vec!["main.rs".into()];
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &tokens, &ignore, &mut next_id, &mut rows);
        // The intermediate `src/` does NOT match the
        // filter but we should still recurse and find
        // `src/main.rs`.
        assert!(
            rows.iter().any(|r| r.command == "src/main.rs"),
            "expected `src/main.rs`, got {:?}",
            rows.iter().map(|r| &r.command).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_dir_sets_row_timestamp_to_file_mtime() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walk_test_{}_{}",
            std::process::id(),
            "mtime"
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("touched.txt");
        std::fs::write(&path, "x").unwrap();
        let expected_mtime = std::fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &[], &ignore, &mut next_id, &mut rows);

        let row = rows
            .iter()
            .find(|r| r.command == "touched.txt")
            .expect("touched.txt row");
        assert_eq!(
            row.timestamp, expected_mtime,
            "row.timestamp must be the file's real mtime, not 0"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `sort_rows_newest_modified_first` is `spawn_walk`'s ordering,
    /// extracted so it can be tested directly without spawning a
    /// thread. Newest `timestamp` first; ties fall back to path
    /// order for a deterministic display.
    #[test]
    fn sort_rows_newest_modified_first_orders_by_timestamp_desc() {
        let mut rows = vec![
            HistoryRow {
                command: "old.txt".to_string(),
                timestamp: 100,
                ..Default::default()
            },
            HistoryRow {
                command: "newest.txt".to_string(),
                timestamp: 300,
                ..Default::default()
            },
            HistoryRow {
                command: "middle.txt".to_string(),
                timestamp: 200,
                ..Default::default()
            },
        ];
        sort_rows_newest_modified_first(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r.command.as_str()).collect();
        assert_eq!(order, vec!["newest.txt", "middle.txt", "old.txt"]);
    }

    /// Equal (or missing, both `0`) timestamps fall back to path
    /// order, so the display is deterministic rather than depending
    /// on filesystem read_dir order.
    #[test]
    fn sort_rows_newest_modified_first_ties_break_by_path() {
        let mut rows = vec![
            HistoryRow {
                command: "zeta.txt".to_string(),
                timestamp: 0,
                ..Default::default()
            },
            HistoryRow {
                command: "alpha.txt".to_string(),
                timestamp: 0,
                ..Default::default()
            },
        ];
        sort_rows_newest_modified_first(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r.command.as_str()).collect();
        assert_eq!(order, vec!["alpha.txt", "zeta.txt"]);
    }
