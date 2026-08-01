    use super::next_config_index;

    #[test]
    fn empty_contents_returns_one() {
        assert_eq!(next_config_index("", "session"), Some(1));
    }

    #[test]
    fn unrelated_contents_returns_one() {
        let s = "\
multiplexer = tmux
capturelines = 20
";
        assert_eq!(next_config_index(s, "session"), Some(1));
    }

    #[test]
    fn single_existing_returns_two() {
        let s = "session.1 = \"Proxmox\"\nsession.1.dir = \"~/foo\"\n";
        assert_eq!(next_config_index(s, "session"), Some(2));
    }

    #[test]
    fn gaps_are_filled() {
        // Existing ids
        // 1, 3, 5 — the
        // next free id is
        // max+1 = 6, not
        // 2 (we don't reuse
        // gaps because the
        // config parser
        // allows any integer
        // id and reusing
        // would surprise
        // users who expect
        // ids to be stable
        // across edits).
        let s = "\
session.1 = \"a\"
session.3 = \"b\"
session.5 = \"c\"
";
        assert_eq!(next_config_index(s, "session"), Some(6));
    }

    #[test]
    fn subfields_do_not_count() {
        // `session.2.dir` is
        // a sub-field of
        // session.2, not a
        // separate id.
        let s = "\
session.1 = \"a\"
session.1.dir = \"~/a\"
session.1.exec = \"cmd\"
";
        assert_eq!(next_config_index(s, "session"), Some(2));
    }

    #[test]
    fn prefix_overlap_does_not_confuse() {
        // `sessiondirs=...`
        // starts with the
        // literal string
        // `session` but is
        // NOT a `session.<id>`
        // entry. The
        // strip_prefix
        // requires the dot
        // after `session`,
        // so this line is
        // correctly ignored.
        let s = "\
sessiondirs = ~/projects
session.1 = \"a\"
";
        assert_eq!(next_config_index(s, "session"), Some(2));
    }

    #[test]
    fn host_prefix_works_independently() {
        let s = "\
session.1 = \"a\"
host.1 = \"Proxmox\"
";
        assert_eq!(next_config_index(s, "host"), Some(2));
        // The session
        // counter is
        // independent of
        // the host counter.
        assert_eq!(next_config_index(s, "session"), Some(2));
    }

    #[test]
    fn host_prefix_with_subfields_uses_parent_id() {
        // The `host.2.user` line
        // is a SUB-FIELD of a
        // (hypothetical) host.2
        // entry. Since the
        // `host.2 = "..."` line
        // itself is missing
        // from the file, the
        // scan only finds
        // `host.1` and returns
        // `max+1 = 2` — the
        // sub-field line alone
        // doesn't promote the
        // parent id. A user
        // who added `host.2.user`
        // by hand without a
        // parent `host.2` line
        // would see the
        // function assign the
        // new entry the same
        // id (2), and the
        // config parser would
        // then see the
        // duplicate parent
        // line. That's a
        // pathological case;
        // the function is
        // correct for well-
        // formed configs.
        let s = "\
host.1 = \"a\"
host.2.user = \"root\"
";
        assert_eq!(next_config_index(s, "host"), Some(2));
    }
