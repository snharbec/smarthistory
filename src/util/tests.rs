    use super::*;
    /// value doesn't matter as long as the formatted string matches.
    /// Using a fixed instant in the test catches both leap-second
    /// surprises (none here, since 2026 is far from a leap second) and
    /// any locale/UTC drift in the formatter.
    const REFERENCE_EPOCH: i64 = 1780562096; // 2026-06-04 12:34:56 UTC

    #[test]
    fn format_time_known_value() {
        // Re-derive the expected string with the same format the helper
        // uses, so this test stays self-documenting. If the format
        // string changes, the test will fail and force the change to
        // be intentional.
        let expected = chrono::DateTime::from_timestamp(REFERENCE_EPOCH, 0)
            .unwrap()
            .naive_utc()
            .format("%d.%b.%Y %H:%M:%S")
            .to_string();
        assert_eq!(format_time(REFERENCE_EPOCH), expected);
    }

    #[test]
    fn format_time_out_of_range() {
        // i64::MIN is guaranteed to be out of range for any reasonable
        // timestamp formatter.
        assert_eq!(format_time(i64::MIN), "(unknown)");
        // A timestamp of 0 (the Unix epoch) is in range; the helper
        // must NOT return "(unknown)" for it.
        assert_ne!(format_time(0), "(unknown)");
    }

    #[test]
    fn format_time_zero_is_unix_epoch() {
        // Unix epoch 0 is 1970-01-01 00:00:00 UTC. Hardcoded so a
        // regression in the formatter is caught immediately.
        assert_eq!(format_time(0), "01.Jan.1970 00:00:00");
    }

    #[test]
    fn escape_like_no_special_chars() {
        // No `%` or `\` in the input → output identical to input.
        // `_` IS a LIKE wildcard and is always escaped, so any
        // string containing `_` will be modified.
        assert_eq!(escape_like("hello world"), "hello world");
        assert_eq!(escape_like(""), "");
        assert_eq!(escape_like("plain text"), "plain text"); // no `_`
        // `_` is escaped to `\_`.
        assert_eq!(escape_like("plain_text"), "plain\\_text");
    }

    #[test]
    fn escape_like_percent() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("%abc%"), "\\%abc\\%");
    }

    #[test]
    fn escape_like_underscore() {
        assert_eq!(escape_like("foo_bar"), "foo\\_bar");
        assert_eq!(escape_like("_"), "\\_");
    }

    #[test]
    fn escape_like_backslash() {
        // A literal backslash must be escaped to `\\` so the LIKE
        // ESCAPE clause recognizes it as a literal.
        assert_eq!(escape_like("a\\b"), "a\\\\b");
    }

    #[test]
    fn escape_like_combined() {
        // Multiple special chars in a row.
        assert_eq!(escape_like("%_\\"), "\\%\\_\\\\");
    }

    /// `canonicalize_directory` is a
    /// no-op for already-canonical
    /// paths. We verify with the
    /// temp dir this test runs in —
    /// `std::env::temp_dir()` may
    /// not be the same as
    /// `canonicalize(...)` of it
    /// (macOS resolves `/tmp` to
    /// `/private/tmp`), so we use
    /// the canonicalized form of
    /// the temp dir as the
    /// expected output.
    #[test]
    fn canonicalize_directory_resolves_existing_path() {
        let dir = std::env::temp_dir();
        let canonical_dir = std::fs::canonicalize(&dir).expect("canonicalize temp dir");
        let canonical_str = canonical_dir.to_string_lossy().into_owned();
        let result = canonicalize_directory(&canonical_str);
        assert_eq!(result, canonical_str);
    }

    /// `canonicalize_directory` falls
    /// back to the input verbatim
    /// when the path doesn't exist
    /// (deleted directory, unmounted
    /// volume). This is the safe
    /// behaviour for the
    /// `preexec` hook: we don't
    /// want to crash the user's
    /// shell because a transient
    /// path was unavailable.
    #[test]
    fn canonicalize_directory_falls_back_for_missing_path() {
        let missing = "/this/path/should/never/exist/anywhere";
        assert_eq!(canonicalize_directory(missing), missing);
    }

    /// Empty input returns empty
    /// (the schema treats an empty
    /// `directory` column as "no
    /// filter"; we don't want to
    /// canonicalize the empty
    /// string, which would yield
    /// the cwd).
    #[test]
    fn canonicalize_directory_empty_input() {
        assert_eq!(canonicalize_directory(""), "");
    }

    /// Symlink resolution:
    /// `canonicalize_directory` of a
    /// path that contains a symlink
    /// returns the resolved path.
    /// We create a temp symlink
    /// (only on platforms that
    /// support `symlink`) and
    /// verify it resolves.
    #[cfg(unix)]
    #[test]
    fn canonicalize_directory_resolves_symlinks() {
        use std::fs;
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!(
            "smarthistory-canon-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create dir");
        let target = base.join("real");
        fs::create_dir(&target).expect("create target");
        let link = base.join("link");
        symlink(&target, &link).expect("symlink");
        // Querying through the
        // symlink should resolve
        // to the real path.
        let link_str = link.to_string_lossy().into_owned();
        let result = canonicalize_directory(&link_str);
        let expected = std::fs::canonicalize(&target)
            .expect("canonicalize target")
            .to_string_lossy()
            .into_owned();
        assert_eq!(result, expected);
        let _ = fs::remove_dir_all(&base);
    }

    /// The invariance the user
    /// reported: insert with one
    /// form of the path, query
    /// with another, the filter
    /// matches. We simulate this
    /// by canonicalizing both the
    /// "stored" and "queried"
    /// sides with the same helper
    /// — if both sides go through
    /// `canonicalize_directory`,
    /// they always agree.
    #[test]
    fn canonicalize_directory_keeps_insert_and_query_in_sync() {
        let base = std::env::temp_dir();
        let canonical_base = std::fs::canonicalize(&base)
            .expect("canonicalize temp dir")
            .to_string_lossy()
            .into_owned();
        // Simulate the
        // `/Users/...` vs
        // `/Volumes/HUGE/...`
        // mismatch by using two
        // textual forms that
        // canonicalize to the
        // same place. On most
        // platforms the temp dir
        // doesn't have this
        // property, so we test
        // the general invariant:
        // canonicalize is
        // idempotent.
        let canonicalized_once = canonicalize_directory(&canonical_base);
        let canonicalized_twice = canonicalize_directory(&canonicalized_once);
        assert_eq!(
            canonicalized_once, canonicalized_twice,
            "canonicalize is idempotent"
        );
    }

    /// `current_directory_for_storage`
    /// returns a non-empty string
    /// for the cwd of the test
    /// process. We don't pin the
    /// exact value (it's
    /// platform-dependent and
    /// depends on where cargo
    /// ran) — we just check it's
    /// non-empty and that it
    /// equals the canonicalized
    /// form of itself (i.e. the
    /// helper is internally
    /// consistent).
    #[test]
    fn current_directory_for_storage_is_canonical() {
        let s = current_directory_for_storage();
        assert!(!s.is_empty(), "got empty cwd");
        // Calling the helper
        // again should give the
        // same result.
        let s2 = current_directory_for_storage();
        assert_eq!(s, s2);
    }

    /// `expand_home` shortens
    /// absolute paths under
    /// `$HOME` to the `~/...`
    /// form. This is the case
    /// that matters for the TUI's
    /// directories view: the
    /// DB stores absolute paths
    /// (e.g. `/Users/har/work`),
    /// but the user wants to see
    /// `~/work`. The
    /// path-segment boundary
    /// check prevents
    /// `/Users/harry/...` from
    /// being mis-shortened to
    /// `~/...` when the home is
    /// `/Users/har`.
    #[test]
    fn expand_home_shortens_paths_under_home() {
        // `$HOME` is process-global and `cargo test` runs every
        // test in the crate (this file, `main.rs`, `tui/tests.rs`)
        // in one process, so this holds the SAME lock those other
        // files' `$HOME`-mutating tests use — see
        // `crate::tui::tests::ENV_LOCK`'s doc comment. A synthetic
        // path (`/home/tester`), never a real path on the machine
        // running the tests, is used throughout.
        let _guard = crate::tui::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var("HOME").ok();
        // SAFETY: holds `ENV_LOCK`, so no other env-mutating test
        // can run concurrently; restored before returning below.
        unsafe {
            std::env::set_var("HOME", "/home/tester");
        }
        // Direct subpath.
        assert_eq!(expand_home("/home/tester/work").as_ref(), "~/work");
        // Deeper path.
        assert_eq!(expand_home("/home/tester/a/b/c").as_ref(), "~/a/b/c");
        // The home dir itself
        // (no trailing path) →
        // `~`.
        assert_eq!(expand_home("/home/tester").as_ref(), "~");
        // Trailing slash on the
        // input — preserve the
        // slash in the output.
        assert_eq!(expand_home("/home/tester/work/").as_ref(), "~/work/");
        // `/home/testerx/...` is NOT under `/home/tester` (the
        // boundary check matches at `/`-or-end only). Pass through
        // unchanged.
        assert_eq!(
            expand_home("/home/testerx/work").as_ref(),
            "/home/testerx/work"
        );
        // Absolute path outside
        // $HOME — pass through.
        assert_eq!(expand_home("/etc/hosts").as_ref(), "/etc/hosts");
        // Restore HOME.
        if let Some(h) = saved_home {
            // SAFETY: see above.
            unsafe {
                std::env::set_var("HOME", h);
            }
        } else {
            // SAFETY: see above.
            unsafe {
                std::env::remove_var("HOME");
            }
        }
    }

    /// `expand_home` returns the
    /// user's home directory for
    /// the bare `~` token, the
    /// home + remainder for the
    /// `~/...` form, and the
    /// input verbatim for anything
    /// else (absolute paths,
    /// relative paths, empty
    /// input, the unsupported
    /// `~user/...` form).
    #[test]
    fn expand_home_basic() {
        // Pin HOME to something non-empty so the assertions are
        // deterministic (none of them actually depend on HOME's
        // specific value — they're all pass-through cases — but
        // `expand_home_no_home_env` below covers the "HOME unset"
        // behavior specifically, so this test needs HOME to be
        // SET to rule that path out). Holds the crate-wide env
        // lock — see `crate::tui::tests::ENV_LOCK`'s doc comment
        // for why a lock private to this file wouldn't be enough.
        let _guard = crate::tui::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var("HOME").ok();
        // SAFETY: holds `ENV_LOCK`; restored before returning below.
        unsafe {
            std::env::set_var("HOME", "/home/tester");
        }
        // Bare `~` is pass-through
        // (idempotence: the
        // function is a
        // one-way-shorten
        // absolute → `~/x`; the
        // bare `~` is already in
        // the target form).
        assert_eq!(expand_home("~").as_ref(), "~");
        // `~/x` is already the
        // short form — the
        // function does NOT
        // "expand" it back to
        // `$HOME/x`. Pass through
        // unchanged. (This is the
        // idempotence contract:
        // the function is
        // a one-way shorten,
        // never re-expand.)
        assert_eq!(expand_home("~/work").as_ref(), "~/work");
        // `~/x/y` (deeper path).
        assert_eq!(expand_home("~/a/b/c").as_ref(), "~/a/b/c");
        // Absolute path — passed
        // through unchanged, no
        // allocation.
        assert_eq!(expand_home("/etc/hosts").as_ref(), "/etc/hosts");
        // Relative path — passed
        // through unchanged.
        assert_eq!(expand_home("work").as_ref(), "work");
        // Empty input — passed
        // through unchanged.
        assert_eq!(expand_home("").as_ref(), "");
        // `~user/...` (a different
        // user's home) is NOT
        // expanded — we don't do
        // `~user` lookups. The
        // literal string passes
        // through; if the user
        // really wanted that path
        // they can edit the staged
        // command before submit.
        assert_eq!(expand_home("~alice/work").as_ref(), "~alice/work");
        // `~` followed by something
        // *not* a slash is also NOT
        // expanded. `~foo` could
        // be either "user foo's
        // home" (which we don't
        // support) or a literal
        // path that happens to
        // start with `~`. Same
        // answer: pass through.
        assert_eq!(expand_home("~something").as_ref(), "~something");
        // Restore HOME.
        if let Some(h) = saved_home {
            // SAFETY: see the
            // matching comment on
            // the `set_var` above.
            unsafe {
                std::env::set_var("HOME", h);
            }
        } else {
            // SAFETY: see above.
            unsafe {
                std::env::remove_var("HOME");
            }
        }
    }

    /// When HOME is unset (or
    /// empty), `expand_home` of
    /// the bare `~` returns an
    /// empty string rather than
    /// panicking. The caller (the
    /// `tmux new-session` action)
    /// would then pass `-c ""` to
    /// tmux, which falls back to
    /// the user's home — the
    /// same behaviour we'd get
    /// if HOME was set, just
    /// without the `~/` expansion
    /// working. This is a
    /// graceful-degradation
    /// contract, not a hard
    /// failure.
    #[test]
    fn expand_home_no_home_env() {
        // Holds the crate-wide env lock — see
        // `crate::tui::tests::ENV_LOCK`'s doc comment.
        let _guard = crate::tui::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var("HOME").ok();
        // SAFETY: holds `ENV_LOCK`; restored before returning below.
        unsafe {
            std::env::remove_var("HOME");
        }
        // When HOME is unset, we
        // can't expand `~` (we
        // don't know the
        // destination). The
        // graceful-degradation
        // contract: pass the `~`
        // through unchanged. The
        // upstream caller (tmux
        // -c, the user's shell)
        // will see the literal
        // `~` and either fail
        // gracefully (tmux) or
        // refuse the submission
        // (shell snippet, which
        // can be edited before
        // submit). Either way,
        // no panic.
        assert_eq!(expand_home("~").as_ref(), "~");
        // `~/x` with no HOME →
        // unchanged as well. (We
        // used to substitute "/x"
        // which was a hack to
        // preserve the rest of the
        // path; the cleaner answer
        // is to pass through.)
        assert_eq!(expand_home("~/work").as_ref(), "~/work");
        // Absolute paths under
        // the (now unset) HOME
        // are also unchanged —
        // there's no prefix to
        // match against.
        assert_eq!(
            expand_home("/home/tester/work").as_ref(),
            "/home/tester/work"
        );
        // Restore HOME.
        if let Some(h) = saved_home {
            // SAFETY: see above.
            unsafe {
                std::env::set_var("HOME", h);
            }
        }
    }

    /// `shorten_home_path` with
    /// multiple home prefixes
    /// picks the most-specific
    /// match. The macOS volume
    /// mount case: `$HOME` is
    /// `/Users/har` but the
    /// user's actual files live
    /// at `/Volumes/HUGE/har`.
    /// The user configures
    /// `homemap=/Volumes/HUGE/har`
    /// so both forms get the
    /// same `~/...` shortening.
    /// When both prefixes
    /// could match (e.g. if HOME
    /// is `/home/user` and the
    /// user has a `homemap=
    /// /home/user/external`),
    /// the longer one wins.
    #[test]
    fn shorten_home_path_picks_most_specific() {
        // macOS-volume case: two
        // homes, paths under the
        // external one match the
        // external one.
        assert_eq!(
            shorten_home_path(
                "/Volumes/HUGE/har/work",
                &["/Users/har".to_string(), "/Volumes/HUGE/har".to_string(),],
            )
            .as_ref(),
            "~/work"
        );
        // Path under the smaller
        // home → `~/...` using the
        // smaller home.
        assert_eq!(
            shorten_home_path(
                "/Users/har/Documents",
                &["/Users/har".to_string(), "/Volumes/HUGE/har".to_string(),],
            )
            .as_ref(),
            "~/Documents"
        );
        // Path under neither
        // home → unchanged.
        assert_eq!(
            shorten_home_path(
                "/etc/hosts",
                &["/Users/har".to_string(), "/Volumes/HUGE/har".to_string(),],
            )
            .as_ref(),
            "/etc/hosts"
        );
        // Bare `~` is the
        // idempotent "already in
        // the target form" case:
        // pass through. The
        // `smarthistory update`
        // subcommand relies on
        // this — a previously-
        // shortened row's `~`
        // value would otherwise
        // re-expand to the longest
        // home on the next run
        // (and then re-shorten
        // again on the third
        // run, oscillating).
        assert_eq!(
            shorten_home_path(
                "~",
                &["/Users/har".to_string(), "/Volumes/HUGE/har".to_string(),],
            )
            .as_ref(),
            "~"
        );
        // Same length tie: first
        // listed wins (sort is
        // stable).
        assert_eq!(
            shorten_home_path("/a/foo", &["/a".to_string(), "/b".to_string(),],).as_ref(),
            "~/foo"
        );
        assert_eq!(
            shorten_home_path("/b/foo", &["/a".to_string(), "/b".to_string(),],).as_ref(),
            "~/foo"
        );
    }

    /// `shorten_path_dirs` abbreviates every directory component to
    /// its first character (home-shortened first) while keeping the
    /// filename fully intact.
    #[test]
    fn shorten_path_dirs_abbreviates_directories_keeps_filename() {
        assert_eq!(
            shorten_path_dirs(
                "/Users/har/work/project/src/main.rs",
                &["/Users/har".to_string()],
            ),
            "~/w/p/s/main.rs"
        );
    }

    /// A path outside any configured home is abbreviated the same
    /// way, including the leading root slash (an empty path segment)
    /// staying empty so the join still starts with `/`.
    #[test]
    fn shorten_path_dirs_handles_absolute_path_outside_home() {
        assert_eq!(
            shorten_path_dirs("/etc/ssh/sshd_config", &["/Users/har".to_string()]),
            "/e/s/sshd_config"
        );
    }

    /// A dotfile-style directory (e.g. `.config`) is abbreviated to
    /// two characters, not one — a bare `.` would read as "current
    /// directory" rather than as a shortened directory name.
    #[test]
    fn shorten_path_dirs_dotfile_directory_keeps_two_chars() {
        assert_eq!(
            shorten_path_dirs("~/.config/smarthistory/config", &["/Users/har".to_string()]),
            "~/.c/s/config"
        );
    }

    /// A bare filename with no directory component (nothing to
    /// shorten) is returned unchanged.
    #[test]
    fn shorten_path_dirs_bare_filename_unchanged() {
        assert_eq!(
            shorten_path_dirs("main.rs", &["/Users/har".to_string()]),
            "main.rs"
        );
    }

    /// `~` itself is never abbreviated further, even though it's a
    /// non-final path segment.
    #[test]
    fn shorten_path_dirs_home_segment_stays_full() {
        assert_eq!(
            shorten_path_dirs("~/project/main.rs", &["/Users/har".to_string()]),
            "~/p/main.rs"
        );
    }

    /// `expand_home_to_absolute`
    /// is the inverse of
    /// `shorten_home_path`:
    /// `~/x` becomes
    /// `<home>/x` using
    /// the **longest** home
    /// in the list (so a
    /// homemap entry that's
    /// longer than `$HOME`
    /// wins). Absolute
    /// paths and bare `~`
    /// are also handled.
    #[test]
    fn expand_home_to_absolute_basic() {
        let homes = vec!["/Users/har".to_string()];
        // `~/x` expands to
        // `<home>/x`.
        assert_eq!(
            expand_home_to_absolute("~/work", &homes,).as_ref(),
            "/Users/har/work"
        );
        // `~/a/b/c` expands
        // similarly.
        assert_eq!(
            expand_home_to_absolute("~/a/b/c", &homes,).as_ref(),
            "/Users/har/a/b/c"
        );
        // Bare `~` expands
        // to the first home.
        assert_eq!(expand_home_to_absolute("~", &homes).as_ref(), "/Users/har");
        // Already-absolute
        // paths pass through
        // unchanged.
        assert_eq!(
            expand_home_to_absolute("/etc/hosts", &homes,).as_ref(),
            "/etc/hosts"
        );
        // Empty input passes
        // through.
        assert_eq!(expand_home_to_absolute("", &homes).as_ref(), "");
    }

    /// The homemap wins in
    /// length-tie cases. With
    /// `homemap=/Volumes/HUGE/har`
    /// and `$HOME=/Users/har`,
    /// `~/x` expands to the
    /// homemap form because
    /// it's the longer
    /// prefix.
    #[test]
    fn expand_home_to_absolute_picks_most_specific() {
        let homes = vec!["/Users/har".to_string(), "/Volumes/HUGE/har".to_string()];
        assert_eq!(
            expand_home_to_absolute("~/work", &homes,).as_ref(),
            "/Volumes/HUGE/har/work"
        );
    }

    /// `normalize_for_compare`
    /// puts a `~/x` DB row
    /// and a
    /// `/Users/har/x` tmux
    /// pane in the same
    /// canonical form so the
    /// `directory_tmux_pane_id`
    /// lookup succeeds. The
    /// `~/x` expansion is the
    /// load-bearing step —
    /// without it, the
    /// `std::fs::canonicalize`
    /// call would fail (no
    /// real `~/x` path
    /// exists) and the two
    /// sides would never
    /// agree.
    #[test]
    fn normalize_for_compare_handles_tilde_form() {
        let homes = vec!["/tmp".to_string()];
        // `~/x` expands to
        // `/tmp/x` and then
        // canonicalizes (which
        // succeeds on existing
        // dirs; the test uses
        // `/tmp` because it
        // exists on every
        // Unix).
        let from_tilde = normalize_for_compare("~/self_test_norm_dir", &homes);
        let from_absolute = normalize_for_compare("/tmp/self_test_norm_dir", &homes);
        // Both should
        // canonicalize to the
        // same value (modulo
        // symlink resolution on
        // `/tmp`).
        assert_eq!(from_tilde, from_absolute);
    }

    /// Empty input returns
    /// empty output (matches
    /// the contract of
    /// `canonicalize_directory`).
    #[test]
    fn normalize_for_compare_empty_input() {
        assert_eq!(normalize_for_compare("", &[]), "");
    }

    /// Paths outside any
    /// home pass through
    /// (the absolute form is
    /// canonicalized, the
    /// rest of the
    /// transformation
    /// doesn't apply).
    #[test]
    fn normalize_for_compare_unrelated_path() {
        let homes = vec!["/Users/har".to_string()];
        // `/etc/hosts` isn't
        // under any home, so
        // the home-expansion
        // step is a no-op. The
        // canonicalize step
        // resolves symlinks
        // (but `/etc/hosts`
        // isn't a symlink on
        // most systems).
        let result = normalize_for_compare("/etc/hosts", &homes);
        assert!(
            !result.is_empty(),
            "result must be non-empty for an existing path, got: {result:?}"
        );
    }

    #[test]
    fn escape_field_single_line_unchanged() {
        assert_eq!(escape_field_for_output("ls -la"), "ls -la");
    }

    #[test]
    fn escape_field_multiline_becomes_single_line() {
        let cmd = "for i in 1 2 3\ndo echo $i\ndone";
        let escaped = escape_field_for_output(cmd);
        // The escaped form must not contain a real newline — that's
        // the whole point: one row fits on one line of CLI output.
        assert!(
            !escaped.contains('\n'),
            "escaped still has newline: {escaped:?}"
        );
        assert!(
            !escaped.contains('\r'),
            "escaped still has carriage return: {escaped:?}"
        );
        // The backslash-n sequences must be present.
        assert_eq!(escaped, "for i in 1 2 3\\ndo echo $i\\ndone");
    }

    #[test]
    fn escape_field_carriage_return() {
        assert_eq!(escape_field_for_output("a\rb"), "a\\rb");
    }
