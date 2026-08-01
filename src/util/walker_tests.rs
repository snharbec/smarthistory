    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Build a unique temp
    /// directory and return
    /// its path. Auto-cleaned
    /// via a `Drop`-style
    /// `TempDir` wrapper
    /// (the test does its own
    /// `remove_dir_all` at
    /// the end of each
    /// function).
    fn unique_tempdir(label: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("smarthistory_walker_{label}_{pid}_{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// `walk_subdirectories`
    /// returns every
    /// subdirectory of a
    /// root, in stable
    /// sorted order. The
    /// root itself is not
    /// included.
    #[test]
    fn walk_subdirectories_lists_all_subs() {
        let root = unique_tempdir("walk_basic");
        // Create:
        //   root/
        //   root/a/
        //   root/a/b/
        //   root/a/c/
        //   root/d/
        let _ = std::fs::create_dir_all(root.join("a").join("b"));
        let _ = std::fs::create_dir_all(root.join("a").join("c"));
        let _ = std::fs::create_dir_all(root.join("d"));
        let _ = std::fs::write(root.join("a").join("file.txt"), "ignore me");
        let got = walk_subdirectories(&root);
        // The result should
        // contain all four
        // subdirectories. We
        // compare by canonical
        // path so symlinks
        // (e.g. on macOS where
        // `/tmp` is a symlink
        // to `/private/tmp`)
        // don't break the
        // assertion.
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        let names: std::collections::HashSet<String> = got
            .iter()
            .map(|p| {
                // Canonicalize
                // each path so
                // `/var/...` vs
                // `/private/var/...`
                // (a macOS
                // symlink) don't
                // break the
                // relative-path
                // comparison.
                let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
                canon
                    .strip_prefix(&canonical_root)
                    .map(|r| r.to_string_lossy().trim_start_matches('/').to_string())
                    .unwrap_or_else(|_| canon.to_string_lossy().into_owned())
            })
            .collect();
        assert!(names.contains("a"), "missing a, got: {names:?}");
        assert!(names.contains("a/b"), "missing a/b, got: {names:?}");
        assert!(names.contains("a/c"), "missing a/c, got: {names:?}");
        assert!(names.contains("d"), "missing d, got: {names:?}");
        // The root itself
        // should NOT be in the
        // list (the walker
        // returns subdirs, not
        // the root).
        assert!(
            !names.contains(""),
            "root path should not be in the result, got: {names:?}"
        );
        // The plain file must
        // NOT be in the list
        // (the walker filters
        // non-directories).
        assert!(
            !names.iter().any(|n| n.contains("file")),
            "files must not be in the result, got: {names:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A missing root
    /// returns an empty
    /// `Vec` (not an
    /// error). This is the
    /// "sessiondirs that
    /// don't exist are
    /// silently skipped"
    /// contract.
    #[test]
    fn walk_subdirectories_missing_root_is_empty() {
        let missing =
            std::env::temp_dir().join("smarthistory_walker_definitely_does_not_exist_xyz123");
        let _ = std::fs::remove_dir_all(&missing);
        let got = walk_subdirectories(&missing);
        assert!(got.is_empty());
    }

    /// `find_command_file`
    /// returns the
    /// `<dir>/.command`
    /// when one exists in
    /// the leaf directory.
    #[test]
    fn find_command_file_in_leaf() {
        let root = unique_tempdir("cmd_leaf");
        let dir = root.join("project");
        let _ = std::fs::create_dir_all(&dir);
        let cmd = dir.join(".command");
        let _ = std::fs::write(&cmd, "#!/bin/sh\necho hi\n");
        let found = find_command_file(&dir).expect("must find .command");
        // The canonical-path
        // comparison avoids
        // macOS `/tmp` vs
        // `/private/tmp`
        // surprises.
        let canonical_found = std::fs::canonicalize(&found).unwrap();
        let canonical_expected = std::fs::canonicalize(&cmd).unwrap();
        assert_eq!(canonical_found, canonical_expected);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// When the leaf has no
    /// `.command` but an
    /// ancestor does, the
    /// ancestor wins. The
    /// lookup walks up
    /// the tree until it
    /// finds one or hits
    /// the root.
    #[test]
    fn find_command_file_walks_up() {
        let root = unique_tempdir("cmd_walk");
        let project = root.join("project");
        let nested = project.join("src").join("lib");
        let _ = std::fs::create_dir_all(&nested);
        // Place the
        // `.command` at the
        // project level, NOT
        // in the leaf.
        let cmd = project.join(".command");
        let _ = std::fs::write(&cmd, "echo project-setup\n");
        let found = find_command_file(&nested).expect("must walk up to find .command");
        let canonical_found = std::fs::canonicalize(&found).unwrap();
        let canonical_expected = std::fs::canonicalize(&cmd).unwrap();
        assert_eq!(canonical_found, canonical_expected);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// When the leaf and
    /// the closest
    /// ancestor both have
    /// a `.command`, the
    /// **leaf** wins
    /// (closest-in-walk
    /// is
    /// first-match-wins).
    #[test]
    fn find_command_file_leaf_beats_ancestor() {
        let root = unique_tempdir("cmd_prefer");
        let project = root.join("project");
        let leaf = project.join("src");
        let _ = std::fs::create_dir_all(&leaf);
        // Place two files;
        // both leaves count.
        let ancestor_cmd = project.join(".command");
        let _ = std::fs::write(&ancestor_cmd, "echo ancestor\n");
        let leaf_cmd = leaf.join(".command");
        let _ = std::fs::write(&leaf_cmd, "echo leaf\n");
        let found = find_command_file(&leaf).expect("must find");
        let canonical_found = std::fs::canonicalize(&found).unwrap();
        let canonical_leaf = std::fs::canonicalize(&leaf_cmd).unwrap();
        assert_eq!(canonical_found, canonical_leaf);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// When no ancestor has
    /// a `.command`, the
    /// lookup returns
    /// `None`.
    #[test]
    fn find_command_file_none_returns_none() {
        let root = unique_tempdir("cmd_none");
        let nested = root.join("a").join("b").join("c");
        let _ = std::fs::create_dir_all(&nested);
        // No .command file
        // anywhere in the
        // tree.
        assert!(find_command_file(&nested).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `shell_quote`
    /// returns the input
    /// verbatim when it's
    /// already
    /// shell-clean
    /// (alphanumeric,
    /// `_`, `-`, `.`,
    /// `/`, `~`, `:`, `,`,
    /// `=`, `+`, `@`).
    #[test]
    fn shell_quote_clean_passes_through() {
        assert_eq!(shell_quote("ls"), "ls");
        assert_eq!(shell_quote("cargo-build"), "cargo-build");
        assert_eq!(shell_quote("a/b/c"), "a/b/c");
        assert_eq!(shell_quote("~/work"), "~/work");
        assert_eq!(shell_quote("key=value"), "key=value");
        assert_eq!(shell_quote("a,b"), "a,b");
    }

    /// Strings with spaces
    /// or shell
    /// metacharacters get
    /// wrapped in single
    /// quotes.
    #[test]
    fn shell_quote_dirty_gets_quoted() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("hello world"), "'hello world'");
        assert_eq!(shell_quote("a;b"), "'a;b'");
        assert_eq!(shell_quote("$VAR"), "'$VAR'");
    }

    /// Strings with single
    /// quotes get the
    /// standard POSIX
    /// escape (`'\''`):
    /// close, escape, reopen.
    #[test]
    fn shell_quote_escapes_inner_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
