    use super::*;
    use std::io::Write;

    /// End-to-end regression test of the REAL background-thread path
    /// (`spawn_walk` + its `std::thread::spawn` closure + the mpsc
    /// channel), not just the synchronous `walk_dir` call other
    /// tests in this file exercise directly. `spawn_walk` itself is
    /// pattern-agnostic now (see the module-level doc comment) — it
    /// walks EVERYTHING once; filtering (`filter_rows`, tested
    /// separately below) happens afterward, in memory. This is the
    /// exact walk `App::spawn_files_walk` drives, once per session;
    /// a bug here would otherwise only surface through manual
    /// interactive testing.
    #[test]
    fn spawn_walk_finds_every_file_via_real_background_thread() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_spawn_walk_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("readme.md"), "x").unwrap();
        std::fs::write(dir.join("notes.md"), "y").unwrap();
        std::fs::write(dir.join("other.txt"), "z").unwrap();

        let ignore = IgnoreSet::new(&[]);
        let request = spawn_walk(dir.clone(), ignore);
        let result = request
            .receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("walk did not complete within 5s (hang or panic)");
        assert_eq!(
            result.len(),
            3,
            "expected all 3 files (walk is unfiltered), got {:?}",
            result.iter().map(|r| &r.command).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &ignore, &mut next_id, &mut rows);
        // The nested file should be in the (unfiltered)
        // result, proving recursion reaches
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
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &ignore, &mut next_id, &mut rows);
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
    fn filter_rows_finds_match_through_non_matching_ancestor_directory() {
        // The bug we fixed earlier: `~main.rs` must still find
        // `src/main.rs` even though the intermediate `src/` entry
        // itself doesn't match the filter. `walk_dir` collects
        // both unconditionally now (it doesn't filter at all), so
        // this is really testing that `filter_rows` correctly
        // excludes the non-matching `src` row while keeping the
        // matching `src/main.rs` row.
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walk_test_{}_{}",
            std::process::id(),
            "recurse"
        ));
        let src = dir.join("src");
        let _ = std::fs::create_dir_all(&src);
        std::fs::write(src.join("main.rs"), "x").unwrap();
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &ignore, &mut next_id, &mut rows);
        let tokens: Vec<String> = vec!["main.rs".into()];
        let filtered = filter_rows(&rows, "", &FilesFilter::Substring(&tokens));
        assert!(
            filtered.iter().any(|r| r.command == "src/main.rs"),
            "expected `src/main.rs`, got {:?}",
            filtered.iter().map(|r| &r.command).collect::<Vec<_>>()
        );
        assert!(
            !filtered.iter().any(|r| r.command == "src"),
            "the non-matching `src` directory row must be excluded"
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
        walk_dir(&dir, &dir, &ignore, &mut next_id, &mut rows);

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

    // --- glob_to_regex ---

    #[test]
    fn glob_to_regex_star_matches_anything() {
        let re = glob_to_regex("a*").unwrap();
        assert!(re.is_match("apple.txt"));
        assert!(re.is_match("a"));
        assert!(!re.is_match("banana"));
    }

    #[test]
    fn glob_to_regex_double_star_collapses_to_single_wildcard() {
        // Deliberate departure from real glob semantics (see the
        // function's doc comment) — basename-only matching means
        // `**` and `*` are indistinguishable.
        let single = glob_to_regex("*.rs").unwrap();
        let double = glob_to_regex("**.rs").unwrap();
        assert_eq!(single.as_str(), double.as_str());
        assert!(double.is_match("main.rs"));
    }

    #[test]
    fn glob_to_regex_question_mark_matches_single_char() {
        let re = glob_to_regex("a?c").unwrap();
        assert!(re.is_match("abc"));
        assert!(!re.is_match("ac"));
        assert!(!re.is_match("abbc"));
    }

    #[test]
    fn glob_to_regex_bracket_class_matches_set() {
        let re = glob_to_regex("file[0-9].txt").unwrap();
        assert!(re.is_match("file1.txt"));
        assert!(re.is_match("file9.txt"));
        assert!(!re.is_match("filea.txt"));
    }

    #[test]
    fn glob_to_regex_negated_bracket_class() {
        let re = glob_to_regex("file[!0-9].txt").unwrap();
        assert!(re.is_match("filea.txt"));
        assert!(!re.is_match("file1.txt"));
    }

    #[test]
    fn glob_to_regex_is_case_insensitive() {
        let re = glob_to_regex("readme*").unwrap();
        assert!(re.is_match("README.md"));
        assert!(re.is_match("ReadMe.txt"));
    }

    #[test]
    fn glob_to_regex_escapes_literal_metacharacters() {
        // A literal `.` in the pattern must NOT act as regex "any
        // character" — `a.b*` should not match `axb` (missing the
        // literal dot).
        let re = glob_to_regex("a.b*").unwrap();
        assert!(re.is_match("a.bc"));
        assert!(!re.is_match("axbc"));
        // `+` is a regex metacharacter too; must be treated literally.
        let re2 = glob_to_regex("a+b").unwrap();
        assert!(re2.is_match("a+b"));
        assert!(!re2.is_match("aab"));
    }

    #[test]
    fn glob_to_regex_is_fully_anchored() {
        // Full-match semantics: a pattern with no wildcards only
        // matches the exact basename, not a substring of it.
        let re = glob_to_regex("main.rs").unwrap();
        assert!(re.is_match("main.rs"));
        assert!(!re.is_match("main.rs.bak"));
        assert!(!re.is_match("not_main.rs"));
    }

    // --- split_glob_root ---

    #[test]
    fn split_glob_root_literal_prefix() {
        let (root, pattern) = split_glob_root("foo/bar/a*");
        assert_eq!(root, "foo/bar");
        assert_eq!(pattern, "a*");
    }

    #[test]
    fn split_glob_root_no_slash() {
        let (root, pattern) = split_glob_root("a*");
        assert_eq!(root, "");
        assert_eq!(pattern, "a*");
    }

    #[test]
    fn split_glob_root_single_directory() {
        let (root, pattern) = split_glob_root("foo/*");
        assert_eq!(root, "foo");
        assert_eq!(pattern, "*");
    }

    #[test]
    fn split_glob_root_globby_leading_segment_falls_back_to_final_segment_only() {
        // `**/*.rs` — the leading `**` segment is itself globby, so
        // root-scoping is skipped entirely (root stays at the base
        // root) and only the final segment becomes the pattern.
        let (root, pattern) = split_glob_root("**/*.rs");
        assert_eq!(root, "");
        assert_eq!(pattern, "*.rs");

        let (root2, pattern2) = split_glob_root("src/*/test.rs");
        assert_eq!(root2, "");
        assert_eq!(pattern2, "test.rs");
    }

    // --- FilesFilter::Glob via walk_dir (recursive basename matching) ---

    /// The glob filter matches recursively against basenames only —
    /// per the feature's explicit "fzf-style, not literal single-
    /// level glob semantics" design: a pattern with no `/` still
    /// finds arbitrarily deeply nested files. Exercised via
    /// `filter_rows` (in-memory, post-walk) now, not `walk_dir`
    /// itself — see the module-level doc comment.
    #[test]
    fn filter_rows_glob_filter_matches_recursively_by_basename() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walk_glob_test_{}_{}",
            std::process::id(),
            "recursive"
        ));
        let nested = dir.join("a").join("b");
        let _ = std::fs::create_dir_all(&nested);
        std::fs::write(nested.join("apple.txt"), "x").unwrap();
        std::fs::write(dir.join("banana.txt"), "x").unwrap();
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &ignore, &mut next_id, &mut rows);
        let re = glob_to_regex("a*").unwrap();
        let filtered = filter_rows(&rows, "", &FilesFilter::Glob { basename: &re, extra_tokens: &[] });
        assert!(
            filtered.iter().any(|r| r.command == "a/b/apple.txt"),
            "expected a/b/apple.txt (matches basename glob a*) in {:?}",
            filtered.iter().map(|r| &r.command).collect::<Vec<_>>()
        );
        assert!(
            !filtered.iter().any(|r| r.command == "banana.txt"),
            "banana.txt's basename doesn't match a*, must be excluded"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Extra whitespace-separated words after the glob narrow the
    /// match further, AND-combined against the relative display
    /// path — e.g. `*.md jira` matches every markdown file whose
    /// path contains "jira", not just its basename.
    #[test]
    fn filter_rows_glob_filter_with_extra_substring_tokens() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walk_glob_extra_test_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("jira-notes.md"), "x").unwrap();
        std::fs::write(dir.join("readme.md"), "x").unwrap();
        std::fs::write(dir.join("jira-summary.txt"), "x").unwrap();
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &ignore, &mut next_id, &mut rows);
        let re = glob_to_regex("*.md").unwrap();
        let extra_tokens = vec!["jira".to_string()];
        let filtered = filter_rows(
            &rows,
            "",
            &FilesFilter::Glob { basename: &re, extra_tokens: &extra_tokens },
        );
        let names: Vec<&str> = filtered.iter().map(|r| r.command.as_str()).collect();
        assert_eq!(
            names,
            vec!["jira-notes.md"],
            "expected only jira-notes.md (matches *.md AND contains \"jira\"), got {names:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An empty extra-tokens list is the same as no additional
    /// filter — every basename match passes through, matching
    /// `Substring`'s own empty-tokens convention.
    #[test]
    fn filter_rows_glob_filter_empty_extra_tokens_matches_everything_the_glob_matches() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walk_glob_no_extra_test_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("a.md"), "x").unwrap();
        std::fs::write(dir.join("b.md"), "x").unwrap();
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &ignore, &mut next_id, &mut rows);
        let re = glob_to_regex("*.md").unwrap();
        let filtered = filter_rows(&rows, "", &FilesFilter::Glob { basename: &re, extra_tokens: &[] });
        assert_eq!(filtered.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression guard: moving the filter check out of `walk_dir`
    /// and into `filter_rows` must not change
    /// `FilesFilter::Substring`'s existing AND-of-tokens behavior.
    #[test]
    fn filter_rows_substring_filter_unchanged_by_refactor() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walk_substr_test_{}_{}",
            std::process::id(),
            "regression"
        ));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("apple.txt"), "x").unwrap();
        std::fs::write(dir.join("banana.txt"), "x").unwrap();
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &ignore, &mut next_id, &mut rows);
        let tokens: Vec<String> = vec!["apple".into()];
        let filtered = filter_rows(&rows, "", &FilesFilter::Substring(&tokens));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].command, "apple.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `filter_rows`'s `root_suffix` scoping: rows outside it are
    /// excluded, and surviving rows' `command` is rewritten relative
    /// to it — matching how `walk_dir` used to behave when it was
    /// itself scoped to the narrower root (before the walk-once
    /// refactor moved root-scoping into the post-walk filter).
    #[test]
    fn filter_rows_root_suffix_scopes_and_trims_display_path() {
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_filter_root_suffix_test_{}",
            std::process::id()
        ));
        let foo_bar = dir.join("foo").join("bar");
        let _ = std::fs::create_dir_all(&foo_bar);
        std::fs::write(foo_bar.join("banana.txt"), "x").unwrap();
        std::fs::write(dir.join("sibling.txt"), "x").unwrap();
        let mut rows = Vec::new();
        let mut next_id: i64 = -1;
        let ignore = IgnoreSet::new(&[]);
        walk_dir(&dir, &dir, &ignore, &mut next_id, &mut rows);
        let tokens: Vec<String> = Vec::new();
        let filtered = filter_rows(&rows, "foo/bar", &FilesFilter::Substring(&tokens));
        let names: Vec<&str> = filtered.iter().map(|r| r.command.as_str()).collect();
        assert_eq!(
            names,
            vec!["banana.txt"],
            "expected only the scoped file, displayed relative to the root_suffix, got {names:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
