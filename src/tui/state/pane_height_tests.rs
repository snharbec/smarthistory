    use super::PaneHeight;

    #[test]
    fn default_is_the_historical_8_line_floor() {
        assert_eq!(PaneHeight::default(), PaneHeight::parse("8").unwrap());
    }

    #[test]
    fn increase_grows_by_exactly_one_line() {
        let h = PaneHeight::parse("10").unwrap().increase(100);
        assert_eq!(h, PaneHeight::parse("11").unwrap());
    }

    #[test]
    fn decrease_shrinks_by_exactly_one_line() {
        let h = PaneHeight::parse("10").unwrap().decrease();
        assert_eq!(h, PaneHeight::parse("9").unwrap());
    }

    /// `decrease` must never go below the one-line floor, no matter
    /// how many times it's called (the out-of-box default of 8 is not
    /// a floor — a 1-line pane is legitimate).
    #[test]
    fn decrease_shrinks_to_the_one_line_floor() {
        let mut h = PaneHeight::default();
        for _ in 0..20 {
            h = h.decrease();
        }
        assert_eq!(h, PaneHeight::parse("1").unwrap());
        // Seven decreases land exactly on the floor (8 → 1).
        let mut seven = PaneHeight::default();
        for _ in 0..7 {
            seven = seven.decrease();
        }
        assert_eq!(seven, PaneHeight::parse("1").unwrap());
    }

    /// `increase` must clamp to `max_for(page_size)` rather than
    /// growing without bound — otherwise a user holding F11 could
    /// shrink the history list to nothing (or push the layout into
    /// a degenerate state on a small terminal).
    #[test]
    fn increase_clamps_to_max_for_page_size() {
        // A 20-line terminal: max = 20 - 5 (chrome) - 3 (min list
        // rows) = 12.
        let mut h = PaneHeight::default();
        for _ in 0..50 {
            h = h.increase(20);
        }
        assert_eq!(h, PaneHeight::parse("12").unwrap());
    }

    /// On a terminal too short for the derived max to exceed the
    /// historical floor, `increase` must be a no-op rather than
    /// panicking or shrinking below `MIN`.
    #[test]
    fn increase_is_a_no_op_on_a_very_short_terminal() {
        let h = PaneHeight::default().increase(5);
        assert_eq!(h, PaneHeight::default());
    }

    /// `detail_row_height` clamps a persisted/CLI preference against
    /// the CURRENT terminal's max without mutating the stored value
    /// — so shrinking the terminal degrades the rendered height
    /// gracefully, and growing it back restores the original
    /// preference.
    #[test]
    fn detail_row_height_clamps_without_mutating_preference() {
        let tall_preference = PaneHeight::parse("40").unwrap();
        // Small terminal: rendered height is clamped down.
        assert!(tall_preference.detail_row_height(20) < 40);
        // The stored preference itself is untouched.
        assert_eq!(tall_preference, PaneHeight::parse("40").unwrap());
        // A big enough terminal renders the full preference.
        assert_eq!(tall_preference.detail_row_height(1000), 40);
    }

    #[test]
    fn parse_rejects_the_old_named_presets() {
        // The old `default` / `medium` / `tall` preset names are no
        // longer valid — `PaneHeight` is a plain line count now.
        assert_eq!(PaneHeight::parse("default"), None);
        assert_eq!(PaneHeight::parse("medium"), None);
        assert_eq!(PaneHeight::parse("tall"), None);
    }

    #[test]
    fn parse_clamps_a_zero_value_up_to_the_floor() {
        // 0 would make the pane invisible; clamp up to the 1-line
        // floor. Heights between the floor and the default (e.g. 3)
        // are honoured as-is now that the floor is 1, not 8.
        assert_eq!(PaneHeight::parse("0"), Some(PaneHeight::parse("1").unwrap()));
        assert_eq!(PaneHeight::parse("1"), Some(PaneHeight::parse("1").unwrap()));
        assert_eq!(PaneHeight::parse("3"), Some(PaneHeight::parse("3").unwrap()));
        assert_eq!(PaneHeight::parse("3").unwrap().detail_row_height(1000), 3);
    }

    #[test]
    fn parse_accepts_a_valid_line_count() {
        assert_eq!(
            PaneHeight::parse("14"),
            Some(PaneHeight::parse("14").unwrap())
        );
        assert_eq!(PaneHeight::parse("14").unwrap().detail_row_height(1000), 14);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(PaneHeight::parse("abc"), None);
        assert_eq!(PaneHeight::parse(""), None);
        assert_eq!(PaneHeight::parse("-5"), None);
    }
