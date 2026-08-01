    use super::*;

    #[test]
    fn parse_pattern_plain_words_become_title_contains() {
        let f = parse_pattern("invoice");
        assert_eq!(f.title_contains, "invoice");
        assert_eq!(f.tag_exact, None);
        assert_eq!(f.correspondent_exact, None);
    }

    #[test]
    fn parse_pattern_tag_token() {
        let f = parse_pattern("#work");
        assert_eq!(f.tag_exact.as_deref(), Some("work"));
        assert_eq!(f.title_contains, "");
    }

    #[test]
    fn parse_pattern_author_token() {
        let f = parse_pattern("@acme");
        assert_eq!(f.correspondent_exact.as_deref(), Some("acme"));
        assert_eq!(f.title_contains, "");
    }

    #[test]
    fn parse_pattern_mixed_tokens() {
        let f = parse_pattern("invoice #work @acme");
        assert_eq!(f.title_contains, "invoice");
        assert_eq!(f.tag_exact.as_deref(), Some("work"));
        assert_eq!(f.correspondent_exact.as_deref(), Some("acme"));
    }

    #[test]
    fn parse_pattern_multiple_title_words_join_with_space_in_order() {
        // `title__icontains` is a single substring lookup, so
        // multiple bare words become ONE joined phrase rather
        // than independently-ANDed substrings — see
        // `parse_pattern`'s doc comment.
        let f = parse_pattern("annual report");
        assert_eq!(f.title_contains, "annual report");
    }

    #[test]
    fn parse_pattern_repeated_tag_token_last_one_wins() {
        // `tags__name__iexact` takes one value; Django can't AND
        // two repeated GET params for the same field.
        let f = parse_pattern("#work #urgent");
        assert_eq!(f.tag_exact.as_deref(), Some("urgent"));
    }

    #[test]
    fn parse_pattern_drops_empty_tag_and_author_tokens() {
        let f = parse_pattern("# @ invoice");
        assert_eq!(f.tag_exact, None);
        assert_eq!(f.correspondent_exact, None);
        assert_eq!(f.title_contains, "invoice");
    }

    #[test]
    fn parse_pattern_empty_pattern_is_all_unset() {
        let f = parse_pattern("");
        assert_eq!(f, PaperlessFilters::default());
        let f = parse_pattern("   ");
        assert_eq!(f, PaperlessFilters::default());
    }

    #[test]
    fn filter_query_params_title_only() {
        let filters = PaperlessFilters {
            title_contains: "invoice".to_string(),
            tag_exact: None,
            correspondent_exact: None,
        };
        assert_eq!(
            filter_query_params(&filters),
            vec![("page_size", "100"), ("title__icontains", "invoice")]
        );
    }

    #[test]
    fn filter_query_params_all_three_set() {
        let filters = PaperlessFilters {
            title_contains: "annual report".to_string(),
            tag_exact: Some("work".to_string()),
            correspondent_exact: Some("acme".to_string()),
        };
        assert_eq!(
            filter_query_params(&filters),
            vec![
                ("page_size", "100"),
                ("title__icontains", "annual report"),
                ("tags__name__iexact", "work"),
                ("correspondent__name__iexact", "acme"),
            ]
        );
    }

    #[test]
    fn filter_query_params_empty_filters_only_page_size() {
        assert_eq!(
            filter_query_params(&PaperlessFilters::default()),
            vec![("page_size", "100")]
        );
    }

    /// End-to-end regression test for the actual wire request:
    /// builds a real `reqwest::blocking::Client` request (via
    /// `.build()`, no network I/O) and asserts on the exact URL,
    /// so a future change to `filter_query_params` or the
    /// underlying reqwest version can't silently reintroduce a
    /// broken query string without a test failure. This mirrors
    /// the manual diagnostic that found the fix in the first
    /// place — `*` is sent literally, `:` gets percent-encoded
    /// (both round-trip correctly through Django's URL decoding).
    #[test]
    fn search_request_url_matches_expected_wire_format() {
        let filters = PaperlessFilters {
            title_contains: "invoice".to_string(),
            tag_exact: Some("work".to_string()),
            correspondent_exact: None,
        };
        let params = filter_query_params(&filters);
        let client = reqwest::blocking::Client::new();
        let request = client
            .get("http://paperless.example.com/api/documents/")
            .header("Authorization", "Token secret")
            .query(&params)
            .build()
            .expect("request should build without sending");
        assert_eq!(
            request.url().as_str(),
            "http://paperless.example.com/api/documents/?page_size=100&title__icontains=invoice&tags__name__iexact=work"
        );
    }

    #[test]
    fn document_to_row_negates_id() {
        let doc = PaperlessDocument {
            id: 42,
            title: "Annual report".to_string(),
            correspondent: String::new(),
            tags: Vec::new(),
            created: String::new(),
            added: String::new(),
            content: String::new(),
        };
        let row = document_to_row(doc);
        assert_eq!(row.id, -42);
        assert_eq!(row.command, "Annual report");
    }

    #[test]
    fn document_to_row_builds_comment_from_correspondent_and_tags() {
        let doc = PaperlessDocument {
            id: 1,
            title: "Invoice".to_string(),
            correspondent: "Acme Corp".to_string(),
            tags: vec!["work".to_string(), "2024".to_string()],
            created: String::new(),
            added: String::new(),
            content: String::new(),
        };
        let row = document_to_row(doc);
        assert_eq!(row.comment, "Acme Corp · #work #2024");
    }

    #[test]
    fn document_to_row_comment_empty_when_no_metadata() {
        let doc = PaperlessDocument {
            id: 1,
            title: "Invoice".to_string(),
            correspondent: String::new(),
            tags: Vec::new(),
            created: String::new(),
            added: String::new(),
            content: String::new(),
        };
        let row = document_to_row(doc);
        assert_eq!(row.comment, "");
    }

    #[test]
    fn document_url_appends_details_path() {
        let cfg = PaperlessConfig {
            url: "https://paperless.example.com".to_string(),
            token: "secret".to_string(),
        };
        assert_eq!(
            cfg.document_url(42),
            "https://paperless.example.com/documents/42/details"
        );
    }

    #[test]
    fn parse_iso8601_epoch_parses_rfc3339() {
        assert_eq!(parse_iso8601_epoch("2024-01-15T10:30:00+00:00"), 1705314600);
    }

    #[test]
    fn parse_iso8601_epoch_empty_is_zero() {
        assert_eq!(parse_iso8601_epoch(""), 0);
    }

    #[test]
    fn document_to_row_uses_added_not_created_for_timestamp() {
        // A document whose nominal `created` date is old but was
        // only just scanned/inserted (`added`) should sort as
        // recent, not as 9 years old.
        let doc = PaperlessDocument {
            id: 1,
            title: "Old invoice".to_string(),
            correspondent: String::new(),
            tags: Vec::new(),
            created: "2015-01-01T00:00:00+00:00".to_string(),
            added: "2024-01-15T10:30:00+00:00".to_string(),
            content: String::new(),
        };
        let row = document_to_row(doc);
        assert_eq!(row.timestamp, 1705314600);
    }
