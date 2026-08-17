    use super::{extract_ssh_target, AddEntryDialog, AddEntryKind};

    /// Session dialog has 3
    /// fields (Name, Dir,
    /// Exec); Name is
    /// required, Dir is
    /// pre-filled from the
    /// source directory, Exec
    /// is optional.
    #[test]
    fn session_dialog_fields() {
        let d = AddEntryDialog::new(
            AddEntryKind::Session,
            "/home/user/project".to_string(),
            "make test".to_string(),
        );
        assert_eq!(d.kind, AddEntryKind::Session);
        assert_eq!(d.fields.len(), 3);
        assert_eq!(d.fields[0].name, "Name");
        assert!(d.fields[0].required);
        assert_eq!(d.fields[1].name, "Dir");
        assert!(!d.fields[1].required);
        // The Dir field is
        // pre-filled with
        // the source
        // directory.
        assert_eq!(d.fields[1].value, "/home/user/project");
        // The cursor lands
        // at the end of
        // the pre-filled
        // value.
        assert_eq!(d.fields[1].cursor, "/home/user/project".chars().count());
        assert_eq!(d.fields[2].name, "Exec");
        assert!(!d.fields[2].required);
    }

    /// Host dialog has 7
    /// fields (Name, Host,
    /// Hostname, User, Port,
    /// Identity, Exec); Name
    /// and Host are required;
    /// Host is pre-filled with
    /// the directory basename.
    #[test]
    fn host_dialog_fields() {
        let d = AddEntryDialog::new(
            AddEntryKind::Host,
            "/home/user/.config/herdr".to_string(),
            String::new(),
        );
        assert_eq!(d.kind, AddEntryKind::Host);
        assert_eq!(d.fields.len(), 7);
        assert_eq!(d.fields[0].name, "Name");
        assert!(d.fields[0].required);
        assert_eq!(d.fields[1].name, "Host");
        assert!(d.fields[1].required);
        // The Host field is
        // pre-filled with
        // the basename of
        // the source
        // directory.
        assert_eq!(d.fields[1].value, "herdr");
        assert_eq!(d.fields[2].name, "Hostname");
        assert!(!d.fields[2].required);
        assert_eq!(d.fields[3].name, "User");
        assert!(!d.fields[3].required);
        assert_eq!(d.fields[4].name, "Port");
        assert!(!d.fields[4].required);
        assert_eq!(d.fields[5].name, "Identity");
        assert!(!d.fields[5].required);
        assert_eq!(d.fields[6].name, "Exec");
        assert!(!d.fields[6].required);
    }

    /// Host dialog with a
    /// path that has no
    /// basename component
    /// (e.g. just "/") falls
    /// back to the full path
    /// for the Host pre-fill
    /// (rather than crashing
    /// on the missing
    /// basename).
    #[test]
    fn host_dialog_root_path_falls_back_to_full_path() {
        let d = AddEntryDialog::new(AddEntryKind::Host, "/".to_string(), String::new());
        // `/` has no
        // basename; the
        // fallback is the
        // full path.
        assert_eq!(d.fields[1].value, "/");
    }

    /// focus_next wraps from
    /// the last field back to
    /// the first.
    #[test]
    fn focus_next_wraps() {
        let mut d = AddEntryDialog::new(AddEntryKind::Session, "/tmp".to_string(), String::new());
        assert_eq!(d.focused, 0);
        d.focus_next();
        assert_eq!(d.focused, 1);
        d.focus_next();
        assert_eq!(d.focused, 2);
        d.focus_next();
        // Wrap to 0.
        assert_eq!(d.focused, 0);
    }

    /// focus_prev wraps from
    /// the first field back to
    /// the last.
    #[test]
    fn focus_prev_wraps() {
        let mut d = AddEntryDialog::new(AddEntryKind::Session, "/tmp".to_string(), String::new());
        assert_eq!(d.focused, 0);
        d.focus_prev();
        // Wrap to 2 (the
        // last field).
        assert_eq!(d.focused, 2);
    }

    /// The source directory
    /// and command are kept
    /// verbatim in the
    /// dialog's
    /// `source_directory` /
    /// `source_command`
    /// fields, which the
    /// renderer shows as a
    /// "from: <cmd> in <dir>"
    /// hint.
    #[test]
    fn source_fields_preserved() {
        let d = AddEntryDialog::new(
            AddEntryKind::Host,
            "/home/user/proj".to_string(),
            "cargo build --release".to_string(),
        );
        assert_eq!(d.source_directory, "/home/user/proj");
        assert_eq!(d.source_command, "cargo build --release");
    }

    // --- extract_ssh_target -------------------------------------------

    #[test]
    fn extract_ssh_target_finds_user_and_ipv4_host() {
        assert_eq!(
            extract_ssh_target("ssh root@122.1.1.40"),
            Some((Some("root".to_string()), "122.1.1.40".to_string()))
        );
    }

    #[test]
    fn extract_ssh_target_finds_bare_ipv4_host_without_user() {
        assert_eq!(
            extract_ssh_target("ssh 122.1.1.40"),
            Some((None, "122.1.1.40".to_string()))
        );
    }

    #[test]
    fn extract_ssh_target_finds_dotted_hostname() {
        assert_eq!(
            extract_ssh_target("ssh alice@pve-1.local"),
            Some((Some("alice".to_string()), "pve-1.local".to_string()))
        );
    }

    #[test]
    fn extract_ssh_target_skips_flags_and_their_values() {
        assert_eq!(
            extract_ssh_target("ssh -p 2222 -i ~/.ssh/id_ed25519 root@122.1.1.40"),
            Some((Some("root".to_string()), "122.1.1.40".to_string()))
        );
    }

    /// `ssh`/`sftp`/`mosh` take a bare `[user@]host` (no `:` marker
    /// needed); `scp`/`rsync` only recognize their colon-suffixed
    /// remote argument — see `extract_ssh_target_scp_ignores_local_path_
    /// that_looks_hostname_shaped` for why.
    #[test]
    fn extract_ssh_target_recognizes_ssh_sftp_mosh() {
        for program in ["ssh", "sftp", "mosh"] {
            let cmd = format!("{program} root@122.1.1.40");
            assert_eq!(
                extract_ssh_target(&cmd),
                Some((Some("root".to_string()), "122.1.1.40".to_string())),
                "program: {program}"
            );
        }
    }

    #[test]
    fn extract_ssh_target_recognizes_scp_and_rsync_remote_argument() {
        for program in ["scp", "rsync"] {
            // No preceding local-path argument here -- that false
            // positive is covered separately, below.
            let cmd = format!("{program} root@122.1.1.40:/remote/path");
            assert_eq!(
                extract_ssh_target(&cmd),
                Some((Some("root".to_string()), "122.1.1.40".to_string())),
                "program: {program}"
            );
        }
    }

    /// The real bug this test caught during development: `scp`/
    /// `rsync` take a LOCAL path as their first positional argument,
    /// and a local path can look exactly as "hostname-shaped" as a
    /// real host (`file.txt` parses the same as `pve-1.local` under
    /// a naive dotted-hostname pattern). Without the colon
    /// requirement, `scp file.txt root@122.1.1.40:/path` would
    /// wrongly extract `file.txt` as the host instead of the real
    /// target.
    #[test]
    fn extract_ssh_target_scp_ignores_local_path_that_looks_hostname_shaped() {
        assert_eq!(
            extract_ssh_target("scp file.txt root@122.1.1.40:/remote/path"),
            Some((Some("root".to_string()), "122.1.1.40".to_string()))
        );
    }

    /// A path-prefixed program name (`/usr/bin/ssh`) is still
    /// recognized -- the leading directory is stripped before the
    /// program-name check.
    #[test]
    fn extract_ssh_target_strips_program_path_prefix() {
        assert_eq!(
            extract_ssh_target("/usr/bin/ssh root@122.1.1.40"),
            Some((Some("root".to_string()), "122.1.1.40".to_string()))
        );
    }

    /// A non-remote-connection command is never scanned, even if it
    /// contains an `@`-shaped word -- avoids false positives like
    /// `git commit --author user@host`.
    #[test]
    fn extract_ssh_target_ignores_non_ssh_commands() {
        assert_eq!(
            extract_ssh_target("git commit --author user@example.com -m fix"),
            None
        );
        assert_eq!(extract_ssh_target("cargo build --release"), None);
    }

    #[test]
    fn extract_ssh_target_returns_none_for_empty_command() {
        assert_eq!(extract_ssh_target(""), None);
    }

    /// A bare single-label hostname (no dot) is indistinguishable
    /// from any other word without deeper flag-aware parsing, so
    /// it's deliberately not recognized -- left to the existing
    /// directory-basename fallback instead.
    #[test]
    fn extract_ssh_target_does_not_match_bare_unqualified_hostname() {
        assert_eq!(extract_ssh_target("ssh myserver"), None);
    }

    // --- Host dialog pre-fill from an SSH target ------------------------

    #[test]
    fn host_dialog_prefills_host_and_user_from_ssh_command() {
        let d = AddEntryDialog::new(
            AddEntryKind::Host,
            "/home/user/unrelated-dir".to_string(),
            "ssh root@122.1.1.40".to_string(),
        );
        assert_eq!(d.fields[1].name, "Host");
        assert_eq!(d.fields[1].value, "122.1.1.40");
        assert_eq!(d.fields[3].name, "User");
        assert_eq!(d.fields[3].value, "root");
        // The cursor lands at the end of the pre-filled value, same
        // as every other pre-filled field.
        assert_eq!(d.fields[3].cursor, "root".chars().count());
    }

    /// No SSH-shaped command: Host still falls back to the directory
    /// basename (pre-existing behavior, unchanged) and User stays
    /// empty rather than being pre-filled with something wrong.
    #[test]
    fn host_dialog_falls_back_to_directory_basename_for_non_ssh_command() {
        let d = AddEntryDialog::new(
            AddEntryKind::Host,
            "/home/user/.config/herdr".to_string(),
            "cargo build --release".to_string(),
        );
        assert_eq!(d.fields[1].value, "herdr");
        assert_eq!(d.fields[3].value, "");
    }
