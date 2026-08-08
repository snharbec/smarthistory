    use super::ModeKind;

    /// `ModeKind::Todo` uses "Todo" (singular) to
    /// match the existing `display_name` value
    /// ("todo"). The list is the "todo mode",
    /// not "the todos mode" — the title is the
    /// mode label, not a count noun. Tests
    /// below assert the display_name ↔
    /// list_title round-trip is exact.
    #[test]
    fn list_title_is_title_case_and_non_empty() {
        for kind in [
            ModeKind::History,
            ModeKind::Output,
            ModeKind::Llm,
            ModeKind::Question,
            ModeKind::Notes,
            ModeKind::Todo,
            ModeKind::Directories,
            ModeKind::Panes,
            ModeKind::Files,
            ModeKind::Tags,
            ModeKind::Ag,
            ModeKind::Codegraph,
            ModeKind::Jira,
            ModeKind::Segments,
            ModeKind::Similar,
            ModeKind::Paperless,
            ModeKind::Browser,
            ModeKind::Processes,
        ] {
            let title = kind.list_title();
            assert!(!title.is_empty(), "{:?} returned empty title", kind);
            assert_eq!(
                title,
                title.trim(),
                "{:?} title has leading / trailing whitespace",
                kind
            );
            // The FIRST character must be an
            // uppercase ASCII letter (or a
            // non-ASCII capital). The test
            // allows the rest of the title to
            // be lowercase — "History" is a
            // valid title even though it
            // contains lowercase letters
            // after the leading capital.
            let first = title.chars().next().expect("non-empty");
            assert!(
                first.is_ascii_uppercase() || first.is_uppercase(),
                "{:?} title {:?} doesn't start with an uppercase letter",
                kind,
                title
            );
        }
    }

    /// `History` keeps the historical label so the
    /// existing UX is preserved. This is a
    /// regression test for the "don't break
    /// muscle memory" requirement.
    #[test]
    fn history_mode_title_is_history() {
        assert_eq!(ModeKind::History.list_title(), "History");
    }

    /// Every non-history mode has a title that
    /// is DISTINCT from "History" — the user's
    /// whole point of asking for this change was
    /// to see the active mode in the title, not
    /// always "History". This guards against a
    /// future edit accidentally regressing one
    /// variant to the default.
    #[test]
    fn non_history_modes_have_distinct_titles() {
        let others = [
            ModeKind::Output,
            ModeKind::Llm,
            ModeKind::Question,
            ModeKind::Notes,
            ModeKind::Todo,
            ModeKind::Directories,
            ModeKind::Panes,
            ModeKind::Files,
            ModeKind::Tags,
            ModeKind::Ag,
            ModeKind::Codegraph,
            ModeKind::Jira,
            ModeKind::Paperless,
            ModeKind::Browser,
            ModeKind::Processes,
        ];
        for kind in others {
            let title = kind.list_title();
            assert_ne!(
                title, "History",
                "{:?} should have a distinct title, not 'History'",
                kind
            );
        }
    }

    /// `ModeKind::display_name` (the existing
    /// lowercase helper used by the mode strip /
    /// help overlay) is independent of
    /// `list_title`. They serve different
    /// surfaces: the strip is a compact
    /// single-line label, the title is a
    /// user-facing border label. They don't
    /// have to be related in form, but for the
    /// common single-word modes (Notes / Files /
    /// Tags / etc.) the lowercase of the title
    /// should equal the display name. The
    /// multi-word titles (Output search / LLM
    /// command / ag search) are excluded from
    /// the round-trip check.
    #[test]
    fn display_name_agrees_for_single_word_modes() {
        let single_word_kinds = [
            ModeKind::History,
            ModeKind::Notes,
            ModeKind::Todo,
            ModeKind::Files,
            ModeKind::Tags,
            ModeKind::Jira,
            ModeKind::Paperless,
            ModeKind::Browser,
            ModeKind::Processes,
        ];
        for kind in single_word_kinds {
            let title = kind.list_title();
            let display = kind.display_name();
            assert_eq!(
                display,
                title.to_ascii_lowercase(),
                "{:?} display_name ({:?}) should equal the lowercase of list_title ({:?})",
                kind,
                display,
                title
            );
        }
    }
