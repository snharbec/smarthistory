    use super::PaneWidth;

    #[test]
    fn default_is_40_percent() {
        assert_eq!(PaneWidth::default(), PaneWidth::parse("40").unwrap());
        assert_eq!(PaneWidth::default().percent(), 40);
    }

    #[test]
    fn increase_grows_by_one_step() {
        let w = PaneWidth::default().increase();
        assert_eq!(w, PaneWidth(40 + PaneWidth::STEP));
    }

    #[test]
    fn decrease_shrinks_by_one_step() {
        let w = PaneWidth::default().decrease();
        assert_eq!(w, PaneWidth(40 - PaneWidth::STEP));
    }

    /// `increase` must clamp at `MAX` (80%) no matter how many times
    /// it's called — the list always keeps the complementary width.
    #[test]
    fn increase_clamps_to_max() {
        let mut w = PaneWidth::default();
        for _ in 0..100 {
            w = w.increase();
        }
        assert_eq!(w.percent(), PaneWidth::MAX);
        assert_eq!(w.list_percent(), 20);
    }

    /// `decrease` must clamp at `MIN` (20%).
    #[test]
    fn decrease_clamps_to_min() {
        let mut w = PaneWidth::default();
        for _ in 0..100 {
            w = w.decrease();
        }
        assert_eq!(w.percent(), PaneWidth::MIN);
        assert_eq!(w.list_percent(), 80);
    }

    #[test]
    fn list_percent_is_the_complement() {
        let w = PaneWidth::parse("35").unwrap();
        assert_eq!(w.percent(), 35);
        assert_eq!(w.list_percent(), 65);
    }

    #[test]
    fn parse_clamps_into_range() {
        // Below `MIN` clamps up; above `MAX` clamps down.
        assert_eq!(PaneWidth::parse("0").unwrap().percent(), PaneWidth::MIN);
        assert_eq!(PaneWidth::parse("10").unwrap().percent(), PaneWidth::MIN);
        assert_eq!(PaneWidth::parse("95").unwrap().percent(), PaneWidth::MAX);
        assert_eq!(PaneWidth::parse("50").unwrap().percent(), 50);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(PaneWidth::parse("abc"), None);
        assert_eq!(PaneWidth::parse(""), None);
        assert_eq!(PaneWidth::parse("-5"), None);
        assert_eq!(PaneWidth::parse("40%"), None);
    }

    #[test]
    fn label_and_as_str_round_trip() {
        let w = PaneWidth::parse("55").unwrap();
        assert_eq!(w.label(), "55%");
        assert_eq!(w.as_str(), "55");
        assert_eq!(PaneWidth::parse(&w.as_str()), Some(w));
    }
