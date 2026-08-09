    use super::unique_config_slug;

    #[test]
    fn empty_contents_slugifies_the_name() {
        assert_eq!(unique_config_slug("", "session", "Proxmox"), "proxmox");
    }

    #[test]
    fn unrelated_contents_slugifies_the_name() {
        let s = "\
multiplexer = tmux
capturelines = 20
";
        assert_eq!(unique_config_slug(s, "session", "Proxmox"), "proxmox");
    }

    #[test]
    fn colliding_slug_gets_a_numeric_suffix() {
        let s = "session.proxmox = \"Proxmox\"\nsession.proxmox.dir = \"~/foo\"\n";
        assert_eq!(unique_config_slug(s, "session", "Proxmox"), "proxmox-2");
    }

    #[test]
    fn legacy_numeric_ids_still_count_as_collisions() {
        // A pre-slug config with `session.1` must not let a new
        // entry that happens to slugify to "1" (an all-digit name)
        // silently clobber it.
        let s = "session.1 = \"a\"\n";
        assert_eq!(unique_config_slug(s, "session", "1"), "1-2");
    }

    #[test]
    fn subfields_do_not_double_count_as_separate_keys() {
        // `session.proxmox.dir`/`.exec` are sub-fields of the same
        // `proxmox` key, not separate entries — the collision check
        // still only sees "proxmox" once.
        let s = "\
session.proxmox = \"a\"
session.proxmox.dir = \"~/a\"
session.proxmox.exec = \"cmd\"
";
        assert_eq!(unique_config_slug(s, "session", "Proxmox"), "proxmox-2");
    }

    #[test]
    fn prefix_overlap_does_not_confuse() {
        // `sessiondirs=...` starts with the literal string `session`
        // but is NOT a `session.<key>` entry. The strip_prefix
        // requires the dot after `session`, so this line is
        // correctly ignored and doesn't collide with "proxmox".
        let s = "\
sessiondirs = ~/projects
session.proxmox = \"a\"
";
        assert_eq!(unique_config_slug(s, "session", "Other"), "other");
    }

    #[test]
    fn host_prefix_works_independently() {
        let s = "session.proxmox = \"a\"\n";
        // The host collision check is independent of the session
        // one — "proxmox" is free under "host" even though it's
        // taken under "session".
        assert_eq!(unique_config_slug(s, "session", "Proxmox"), "proxmox-2");
        assert_eq!(unique_config_slug(s, "host", "Proxmox"), "proxmox");
    }

    #[test]
    fn subfield_only_entry_still_counts_as_a_collision() {
        // Even without a bare `host.proxmox = "..."` line, a
        // sub-field-only entry (`host.proxmox.user`) still reserves
        // the "proxmox" key.
        let s = "\
host.proxmox.user = \"root\"
";
        assert_eq!(unique_config_slug(s, "host", "Proxmox"), "proxmox-2");
    }

    #[test]
    fn empty_or_all_emoji_name_falls_back_to_the_prefix() {
        assert_eq!(unique_config_slug("", "session", ""), "session");
        assert_eq!(unique_config_slug("", "host", "\u{1F4BE}"), "host");
    }

    #[test]
    fn name_with_spaces_and_case_is_slugified() {
        assert_eq!(
            unique_config_slug("", "session", "My Cool Project"),
            "my-cool-project"
        );
    }
