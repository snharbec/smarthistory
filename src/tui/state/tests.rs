    use super::HistoryRow;
    use super::PanesFilter;

    /// The configured-sessions panes filter is displayed as
    /// "DIRECTORIES" (renamed from "SESSIONS" — see
    /// `configured_sections_into` in `panes.rs`), while the enum
    /// variant itself stays `Sessions` for backward compatibility
    /// with existing `key.filter-panes-sessions=...` bindings.
    #[test]
    fn panes_filter_sessions_label_is_directories() {
        assert_eq!(PanesFilter::Sessions.label(), "DIRECTORIES");
    }

    /// `parse` accepts both the legacy "sessions"/"session" spelling
    /// and the new "directories"/"directory"/"dir"/"dirs" aliases —
    /// all resolve to the same `PanesFilter::Sessions` variant.
    #[test]
    fn panes_filter_parse_accepts_directories_aliases() {
        for s in ["sessions", "session", "directories", "directory", "dir", "dirs", "DIR"] {
            assert_eq!(
                PanesFilter::parse(s),
                Some(PanesFilter::Sessions),
                "expected {s:?} to parse as Sessions"
            );
        }
    }

    /// A real history row (positive
    /// `id`, `exit_code == 0`) is
    /// not an LLM preview.
    #[test]
    fn is_llm_preview_real_history_row_is_false() {
        let row = HistoryRow {
            id: 42,
            command: "ls -la".to_string(),
            directory: String::new(),
            session_id: String::new(),
            exit_code: 0,
            timestamp: 1_000_000,
            comment: String::new(),
            output: String::new(),
            mode: "command".to_string(),
            source: String::new(),
            ..Default::default()
        };
        assert!(!row.is_llm_preview());
    }

    /// A history row that failed
    /// (positive `id`,
    /// `exit_code != 0`) is not
    /// an LLM preview either —
    /// the user actually ran it.
    #[test]
    fn is_llm_preview_failed_command_is_false() {
        let row = HistoryRow {
            id: 100,
            command: "false".to_string(),
            directory: String::new(),
            session_id: String::new(),
            exit_code: 1,
            timestamp: 1_000_000,
            comment: String::new(),
            output: String::new(),
            mode: "command".to_string(),
            source: String::new(),
            ..Default::default()
        };
        assert!(!row.is_llm_preview());
    }

    /// A todo row has a negative
    /// `id` (encoding the 1-based
    /// line number as
    /// `id = -(line_number)`) and
    /// `exit_code == 0`. It is
    /// emphatically NOT an LLM
    /// preview — checking
    /// `id < 0` instead of
    /// `exit_code == -1` was the
    /// exact bug that made every
    /// todo row show a `[LLM]`
    /// marker in the age column.
    /// This test is the regression
    /// guard.
    #[test]
    fn is_llm_preview_todo_row_is_false() {
        let row = HistoryRow {
            id: -42, // line 42 of the source note
            command: "pick apples in the orchard".to_string(),
            directory: String::new(),
            session_id: String::new(),
            exit_code: 0,
            timestamp: 1_000_000,
            comment: "note.md".to_string(),
            output: String::new(),
            mode: "todo".to_string(),
            source: String::new(),
            ..Default::default()
        };
        assert!(
            !row.is_llm_preview(),
            "todo row must NOT be classified as LLM preview \
             (negative id encodes the line number, not a preview)"
        );
    }

    /// The synthetic LLM preview
    /// row has `exit_code == -1`
    /// (the "never executed"
    /// sentinel) and a negative
    /// `id` (typically `-1`). Both
    /// signals together are the
    /// canonical fingerprint of an
    /// LLM preview; the predicate
    /// keys on the `exit_code`
    /// sentinel because it's the
    /// load-bearing distinction
    /// (other row types may also
    /// use negative ids).
    #[test]
    fn is_llm_preview_llm_preview_row_is_true() {
        let row = HistoryRow {
            id: -1,
            command: "find . -name '*.rs' -newer foo".to_string(),
            directory: String::new(),
            session_id: String::new(),
            exit_code: -1, // never executed sentinel
            timestamp: 0,
            comment: "find rust files newer than foo".to_string(),
            output: String::new(),
            mode: String::new(),
            source: String::new(),
            ..Default::default()
        };
        assert!(row.is_llm_preview());
    }

    /// A question-mode row has
    /// `exit_code == 0` (the
    /// question was answered
    /// successfully by ollama) and
    /// is not an LLM preview in
    /// the `=...`-style sense.
    /// The render path uses
    /// `is_llm_preview()` to decide
    /// whether to draw a `[LLM]`
    /// tag, and we don't want
    /// questions to pick that up.
    #[test]
    fn is_llm_preview_question_row_is_false() {
        let row = HistoryRow {
            id: 7,
            command: "what is the capital of france?".to_string(),
            directory: String::new(),
            session_id: String::new(),
            exit_code: 0,
            timestamp: 1_000_000,
            comment: String::new(),
            output: "Paris".to_string(),
            mode: "question".to_string(),
            source: String::new(),
            ..Default::default()
        };
        assert!(!row.is_llm_preview());
    }
