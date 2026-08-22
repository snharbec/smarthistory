    use super::*;

    /// Fixed "now" used by every `build_jql` test.
    /// Choosing a non-zero value that maps to a known
    /// UTC date (2024-06-30 19:14:39) makes the
    /// date-cutoff strings the alias tests assert
    /// against stable and reproducible across runs.
    /// This is the same instant the `updated_to_epoch`
    /// test uses, so a single epoch underpins both
    /// layers of the test suite.
    const TEST_NOW_EPOCH: i64 = 1_719_774_879;

    /// Empty fragment map shared by every test that
    /// doesn't care about the fragment feature. The
    /// `build_jql` signature takes a fragments map;
    /// passing an empty one is the "no user-defined
    /// fragments" default and is the most common
    /// shape for existing tests.
    fn empty_fragments() -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }

    /// Convenience wrapper that discards the
    /// undefined-fragments return value. Existing
    /// tests that don't exercise the fragment
    /// feature use this so the body of every test
    /// (including its `assert_eq!` against an
    /// exact JQL string) keeps the simple form
    /// it had before `build_jql` started returning
    /// a tuple. Tests that DO care about undefined
    /// fragments call `build_jql` directly.
    fn call_jql(
        body: &str,
        default_project: Option<&str>,
        now_epoch: i64,
        fragments: &std::collections::HashMap<String, String>,
    ) -> String {
        build_jql(body, default_project, now_epoch, fragments).0
    }

    // ---- build_jql ----

    #[test]
    fn build_jql_empty_body_uses_default_project() {
        assert_eq!(
            call_jql("", Some("PROJ"), TEST_NOW_EPOCH, &empty_fragments()),
            r#"project = "PROJ" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_empty_body_no_project_is_global_recent() {
        assert_eq!(
            call_jql("", None, TEST_NOW_EPOCH, &empty_fragments()),
            "ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_single_issue_key() {
        assert_eq!(
            call_jql("PROJ-123", None, TEST_NOW_EPOCH, &empty_fragments()),
            "key = PROJ-123 ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_multiple_issue_keys_collapse_into_in() {
        assert_eq!(
            call_jql("PROJ-1 PROJ-2", None, TEST_NOW_EPOCH, &empty_fragments()),
            "key in (PROJ-1, PROJ-2) ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_field_value_quoted() {
        assert_eq!(
            call_jql("project=PROJ", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"project = "PROJ" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_empty_field_value() {
        // `project=` (empty value) is a valid token by the
        // `\w+=\S*` classifier; value is the empty string.
        assert_eq!(
            call_jql("assignee=", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"assignee = "" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_free_text_searches_description_or_summary() {
        assert_eq!(
            call_jql("login", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"(description ~ "login" OR summary ~ "login") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_combines_all_three_groups_with_and() {
        // Project (explicit or default) always comes
        // first, then aliases, then keys, then other
        // field-values, then free text.
        assert_eq!(
            call_jql(
                "PROJ-123 project=PROJ crash",
                None,
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"project = "PROJ" AND key = PROJ-123 AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_multiple_free_text_tokens_are_anded() {
        assert_eq!(
            call_jql("login crash", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"(description ~ "login" OR summary ~ "login") AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_escapes_quotes_and_backslashes_in_text() {
        // A free-text token containing `"` and `\` must be
        // escaped so it's a valid JQL string literal.
        let jql = call_jql(r#"a"b\c"#, None, TEST_NOW_EPOCH, &empty_fragments());
        assert!(jql.contains(r#"description ~ "a\"b\\c""#), "{}", jql);
    }

    #[test]
    fn build_jql_whitespace_only_falls_back_to_default() {
        assert_eq!(
            call_jql("   ", Some("PROJ"), TEST_NOW_EPOCH, &empty_fragments()),
            r#"project = "PROJ" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_strips_user_supplied_order_by() {
        // Users may persist a session query that includes
        // the ORDER BY clause. `build_jql` always appends
        // its own `ORDER BY updated DESC`, so the user-
        // supplied clause must be stripped before parsing
        // to avoid double-counting and mis-tokenization.
        assert_eq!(
            call_jql(
                "crash ORDER BY updated DESC",
                None,
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"(description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
        // Case-insensitive match.
        assert_eq!(
            call_jql(
                "crash order by updated DESC",
                None,
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"(description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
        // With project scope.
        assert_eq!(
            call_jql(
                "crash ORDER BY updated DESC",
                Some("PROJ"),
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"project = "PROJ" AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_explicit_project_overrides_default() {
        // The full ordering policy:
        //   1. project (explicit > default)
        //   2. @me / date aliases
        //   3. fragments
        //   4. issue keys
        //   5. other field-value pairs
        //   6. free-text tokens
        //
        // Below we mirror the user's specification table
        // with ENG as the default project.

        // Empty body with a default project.
        assert_eq!(
            call_jql("", Some("ENG"), TEST_NOW_EPOCH, &empty_fragments()),
            r#"project = "ENG" ORDER BY updated DESC"#,
        );

        // Single free-text token.
        assert_eq!(
            call_jql("test1", Some("ENG"), TEST_NOW_EPOCH, &empty_fragments()),
            r#"project = "ENG" AND (description ~ "test1" OR summary ~ "test1") ORDER BY updated DESC"#,
        );

        // Multiple free-text tokens are ANDed.
        assert_eq!(
            call_jql(
                "test1 test2",
                Some("ENG"),
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"project = "ENG" AND (description ~ "test1" OR summary ~ "test1") AND (description ~ "test2" OR summary ~ "test2") ORDER BY updated DESC"#,
        );

        // Explicit `project=...` overrides the default
        // project and appears first.
        assert_eq!(
            call_jql(
                "project=RMS",
                Some("ENG"),
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"project = "RMS" ORDER BY updated DESC"#,
        );

        // Free text + explicit project.
        assert_eq!(
            call_jql(
                "test1 project=RMS",
                Some("ENG"),
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"project = "RMS" AND (description ~ "test1" OR summary ~ "test1") ORDER BY updated DESC"#,
        );

        // @me alias + explicit project.
        assert_eq!(
            call_jql(
                "@me project=RMS",
                Some("ENG"),
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"project = "RMS" AND assignee = currentUser() ORDER BY updated DESC"#,
        );

        // @me + explicit project + free text.
        assert_eq!(
            call_jql(
                "@me project=RMS test1",
                Some("ENG"),
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"project = "RMS" AND assignee = currentUser() AND (description ~ "test1" OR summary ~ "test1") ORDER BY updated DESC"#,
        );
    }

    #[test]
    fn build_jql_free_text_with_default_project_is_scoped() {
        // When a default project is configured, free-text
        // searches must NOT leak results from other projects.
        assert_eq!(
            call_jql("crash", Some("PROJ"), TEST_NOW_EPOCH, &empty_fragments()),
            r#"project = "PROJ" AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_issue_key_ignores_default_project() {
        // A bare issue key is globally unique — adding
        // a project scope would cause a mismatch if the
        // key belongs to a different project.
        assert_eq!(
            call_jql("PROJ-123", Some("ENG"), TEST_NOW_EPOCH, &empty_fragments()),
            "key = PROJ-123 ORDER BY updated DESC",
        );
    }

    #[test]
    fn build_jql_issue_key_with_default_project_is_scoped() {
        // Only when the query ALSO contains free text,
        // field-values, aliases, or fragments does the
        // project scope apply.
        assert_eq!(
            call_jql(
                "PROJ-123 crash",
                Some("PROJ"),
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"project = "PROJ" AND key = PROJ-123 AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_issue_key_only_uppercase() {
        // Only uppercase project keys with digits after
        // a hyphen match the issue-key pattern.
        assert_eq!(
            call_jql("ENG-123", None, TEST_NOW_EPOCH, &empty_fragments()),
            "key = ENG-123 ORDER BY updated DESC",
        );

        // Lowercase is treated as free text.
        assert_eq!(
            call_jql("eng-123", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"(description ~ "eng-123" OR summary ~ "eng-123") ORDER BY updated DESC"#,
        );

        // Mixed case is free text.
        assert_eq!(
            call_jql("Eng-123", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"(description ~ "Eng-123" OR summary ~ "Eng-123") ORDER BY updated DESC"#,
        );

        // No hyphen is free text.
        assert_eq!(
            call_jql("ENG123", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"(description ~ "ENG123" OR summary ~ "ENG123") ORDER BY updated DESC"#,
        );
    }

    #[test]
    fn build_jql_field_value_with_default_project_is_scoped() {
        assert_eq!(
            call_jql(
                "assignee=alice",
                Some("PROJ"),
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"project = "PROJ" AND assignee = "alice" ORDER BY updated DESC"#
        );
    }

    // ---- build_jql: aliases (@me, @today, @week, @month) ----
    //
    // The expected date strings below are computed from
    // TEST_NOW_EPOCH (= 2024-06-30 19:14:39 UTC):
    //   @today  -> 2024-06-29 (today - 1 day)
    //   @week   -> 2024-06-23 (today - 7 days)
    //   @month  -> 2024-05-30 (today - 31 days)
    // If TEST_NOW_EPOCH changes, update these literals
    // in lock-step.

    #[test]
    fn build_jql_at_me_becomes_current_user() {
        assert_eq!(
            call_jql("@me", None, TEST_NOW_EPOCH, &empty_fragments()),
            "assignee = currentUser() ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_at_me_no_at_prefix_also_works() {
        // The notes-mode parser convention: both `@me`
        // and the bare `me` keyword work.
        assert_eq!(
            call_jql("me", None, TEST_NOW_EPOCH, &empty_fragments()),
            "assignee = currentUser() ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_at_today_uses_yesterday_date() {
        assert_eq!(
            call_jql("@today", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"updated >= "2024-06-29" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_at_week_uses_today_minus_7() {
        assert_eq!(
            call_jql("@week", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"updated >= "2024-06-23" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_at_month_uses_today_minus_31() {
        // Spec: `@month` looks back 31 days (vs the
        // notes-mode precedent of 30). The constant
        // JIRA_ALIAS_MONTH_DAYS owns the policy.
        assert_eq!(
            call_jql("@month", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"updated >= "2024-05-30" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_aliases_are_case_insensitive() {
        // @Today, @TODAY, @tOdAy all match.
        assert_eq!(
            call_jql("@TODAY", None, TEST_NOW_EPOCH, &empty_fragments()),
            call_jql("@today", None, TEST_NOW_EPOCH, &empty_fragments()),
        );
        assert_eq!(
            call_jql("@Me", None, TEST_NOW_EPOCH, &empty_fragments()),
            call_jql("@me", None, TEST_NOW_EPOCH, &empty_fragments()),
        );
    }

    #[test]
    fn build_jql_aliases_stripped_from_body() {
        // After alias resolution, the body must be
        // empty of alias tokens — they don't fall
        // through to free text. A typo'd alias
        // (e.g. `@tody`) would still fall through;
        // see `build_jql_unknown_alias_falls_through`.
        let jql = call_jql("@me @today crash", None, TEST_NOW_EPOCH, &empty_fragments());
        assert!(!jql.contains("@me"));
        assert!(!jql.contains("@today"));
        // The free-text token survives.
        assert!(jql.contains(r#"(description ~ "crash" OR summary ~ "crash")"#));
    }

    #[test]
    fn build_jql_unknown_alias_falls_through_to_free_text() {
        // A token like `@tody` isn't a recognised
        // alias. It is NOT stripped of its leading
        // `@` — the user's literal text is preserved
        // in the JQL. This is a deliberate departure
        // from the notes-mode parser, which DOES
        // strip leading `@` from unknown tokens to
        // route them past the note_search library's
        // link-tokenizer. JIRA mode has no upstream
        // tokenizer to satisfy, so we keep the user's
        // text verbatim: a free-text search for
        // `@tody` is a different query from `tody`.
        let jql = call_jql("@tody", None, TEST_NOW_EPOCH, &empty_fragments());
        assert_eq!(
            jql,
            r#"(description ~ "@tody" OR summary ~ "@tody") ORDER BY updated DESC"#
        );
        // No alias fired.
        assert!(!jql.contains("updated >="));
        assert!(!jql.contains("currentUser"));
    }

    #[test]
    fn build_jql_email_like_tokens_are_not_aliases() {
        // `email@today` must NOT be treated as the
        // `@today` alias. The parser only strips a
        // leading `@`; the bare keyword must be the
        // whole token. So `email@today` stays
        // intact and falls through to free text.
        let jql = call_jql("user@today", None, TEST_NOW_EPOCH, &empty_fragments());
        // No `updated >=` clause (the alias didn't fire).
        assert!(!jql.contains("updated >="));
        // The token survives verbatim as free text.
        assert!(jql.contains("user@today"));
    }

    #[test]
    fn build_jql_compound_alias_tokens_are_not_aliases() {
        // `@todayfile` is a single token that
        // doesn't equal `today` (whole-word match).
        // Falls through to free text.
        let jql = call_jql("@todayfile", None, TEST_NOW_EPOCH, &empty_fragments());
        assert!(!jql.contains("updated >="));
        assert!(jql.contains("@todayfile"));
    }

    #[test]
    fn build_jql_date_aliases_last_one_wins() {
        // `@today @week` resolves to @week (last write
        // wins). Same convention as notes mode.
        assert_eq!(
            call_jql("@today @week", None, TEST_NOW_EPOCH, &empty_fragments()),
            call_jql("@week", None, TEST_NOW_EPOCH, &empty_fragments()),
        );
        assert_eq!(
            call_jql(
                "@week @today @month",
                None,
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            call_jql("@month", None, TEST_NOW_EPOCH, &empty_fragments()),
        );
    }

    #[test]
    fn build_jql_at_me_combines_with_date_alias() {
        // `@me` and `@week` are orthogonal; both
        // clauses should appear, AND-joined.
        // Ordering: project (none) -> @me ->
        // @date -> ...; the test asserts the
        // exact JQL string.
        assert_eq!(
            call_jql("@me @week", None, TEST_NOW_EPOCH, &empty_fragments()),
            r#"assignee = currentUser() AND updated >= "2024-06-23" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_all_four_aliases_together() {
        assert_eq!(
            call_jql(
                "@me @today @week @month",
                None,
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            // @month is the resolved date filter
            // (last-one-wins); @me is the assignee.
            r#"assignee = currentUser() AND updated >= "2024-05-30" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_alias_with_default_project() {
        // The default-project clause is always
        // prepended, before the alias clauses.
        assert_eq!(
            call_jql(
                "@me @week",
                Some("PROJ"),
                TEST_NOW_EPOCH,
                &empty_fragments()
            ),
            r#"project = "PROJ" AND assignee = currentUser() AND updated >= "2024-06-23" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_alias_with_field_value() {
        // A regular `field=value` clause coexists
        // with the alias clauses; ordering: project
        // (none) -> @me -> @date -> keys -> kvs -> text.
        assert_eq!(
            call_jql(
                "@me @week status=Open",
                None,
                TEST_NOW_EPOCH,
                &empty_fragments(),
            ),
            r#"assignee = currentUser() AND updated >= "2024-06-23" AND status = "Open" ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_alias_with_issue_key() {
        // `@me PROJ-1` -> only PROJ-1, but only
        // when the user is the assignee. Useful
        // for "is this issue mine?".
        assert_eq!(
            call_jql("@me PROJ-1", None, TEST_NOW_EPOCH, &empty_fragments()),
            "assignee = currentUser() AND key = PROJ-1 ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_jql_alias_with_free_text() {
        assert_eq!(
            call_jql("@me @week crash", None, TEST_NOW_EPOCH, &empty_fragments(),),
            r#"assignee = currentUser() AND updated >= "2024-06-23" AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    // ---- build_jql: fragments (jira.search.<name>=<jql>) ----
    //
    // Fragments are user-defined JQL snippets loaded
    // from the config file. They are spliced into the
    // query verbatim (no JQL-quoting) when the body
    // contains `@<name>`. Each fragment is wrapped in
    // parens so internal `AND` / `OR` doesn't break
    // the top-level AND-join.
    //
    // The expected JQL strings in these tests assume
    // the fragment map below (`fragments_with_labels`).
    // Tests that exercise undefined fragments use
    // `&empty_fragments()` to assert the error path.

    /// Fragment map shared by every "happy path"
    /// test below. Three entries of varying
    /// complexity:
    /// - `label1`: a single JQL clause with an
    ///   equals sign (the user's example).
    /// - `sprint`: a single clause, different
    ///   operator, to confirm the parser doesn't
    ///   special-case `=`.
    /// - `complex`: contains an internal `AND`
    ///   so the paren-wrapping is observable in
    ///   the resulting JQL.
    fn fragments_with_labels() -> std::collections::HashMap<String, String> {
        let mut m = std::collections::HashMap::new();
        m.insert("label1".to_string(), r#"labels = "test""#.to_string());
        m.insert("sprint".to_string(), "sprint = \"Sprint 42\"".to_string());
        m.insert(
            "complex".to_string(),
            r#"priority = High AND labels = "security""#.to_string(),
        );
        m
    }

    #[test]
    fn build_jql_simple_fragment_substituted() {
        // The user's example: `jira.search.label1=labels = "test"`,
        // invoked as `@label1` in the body.
        let (jql, undefined) = build_jql("@label1", None, TEST_NOW_EPOCH, &fragments_with_labels());
        assert_eq!(jql, r#"(labels = "test") ORDER BY updated DESC"#);
        // A recognised fragment never appears in the
        // undefined list (that's the whole point of
        // looking it up before recording).
        assert!(undefined.is_empty());
    }

    #[test]
    fn build_jql_fragment_case_insensitive_lookup() {
        // `@Label1` and `@LABEL1` both resolve to
        // the same fragment — the parser lowercases
        // the lookup key.
        let (jql_upper, _) = build_jql("@LABEL1", None, TEST_NOW_EPOCH, &fragments_with_labels());
        let (jql_lower, _) = build_jql("@label1", None, TEST_NOW_EPOCH, &fragments_with_labels());
        assert_eq!(jql_upper, jql_lower);
    }

    #[test]
    fn build_jql_fragment_combines_with_aliases() {
        // `@label1 @me` -> both the fragment and
        // the `@me` alias appear, AND-joined.
        let (jql, undefined) = build_jql(
            "@label1 @me",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert_eq!(
            jql,
            r#"assignee = currentUser() AND (labels = "test") ORDER BY updated DESC"#
        );
        assert!(undefined.is_empty());
    }

    #[test]
    fn build_jql_fragment_with_project_default() {
        // Project clause prepended, then fragment.
        let (jql, _) = build_jql(
            "@label1",
            Some("PROJ"),
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert_eq!(
            jql,
            r#"project = "PROJ" AND (labels = "test") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_fragment_combines_with_field_value_and_text() {
        // `@label1 status=Open crash` -> fragment,
        // then field=value, then free text.
        let (jql, _) = build_jql(
            "@label1 status=Open crash",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert_eq!(
            jql,
            r#"(labels = "test") AND status = "Open" AND (description ~ "crash" OR summary ~ "crash") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_complex_fragment_preserves_internal_and() {
        // The fragment value contains an internal
        // `AND`. The paren-wrap is what keeps the
        // top-level AND-join well-formed.
        let (jql, _) = build_jql("@complex", None, TEST_NOW_EPOCH, &fragments_with_labels());
        assert_eq!(
            jql,
            r#"(priority = High AND labels = "security") ORDER BY updated DESC"#
        );
    }

    #[test]
    fn build_jql_multiple_fragments_in_order() {
        // Two fragments, typed in order, appear in
        // that order. Order matters when a fragment
        // is asymmetrically selective (e.g. one
        // fragment filters by sprint, another by
        // assignee).
        let (jql, _) = build_jql(
            "@label1 @sprint",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert_eq!(
            jql,
            r#"(labels = "test") AND (sprint = "Sprint 42") ORDER BY updated DESC"#
        );
    }

    /// Unprefixed tokens (e.g. `kramfors` when
    /// `jira.search.kramfors=...` is defined) are
    /// treated as plain text — they do NOT expand
    /// the fragment. The `@` is a deliberate
    /// invocation. The user reported this as a
    /// bug: typing `-kramfors` (no `@`) in JIRA
    /// mode was silently expanding the `kramfors`
    /// fragment even when the user just wanted a
    /// free-text search. With this fix, the user
    /// must type `-@kramfors` to expand the
    /// fragment; `-kramfors` searches the
    /// description / summary for the literal
    /// string.
    #[test]
    fn build_jql_unprefixed_fragment_name_is_free_text() {
        // `label1` is defined as a fragment
        // (labels = "test") in `fragments_with_labels`.
        // Typing it without `@` must NOT expand the
        // fragment — it must fall through to the
        // free-text classifier.
        let (jql, undefined) = build_jql(
            "label1",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        // The JQL is a free-text search for the
        // literal `label1`, NOT the fragment's JQL.
        assert!(jql.contains(r#"(description ~ "label1" OR summary ~ "label1")"#));
        assert!(
            !jql.contains(r#"labels = "test""#),
            "unprefixed `label1` must NOT expand the fragment; got: {}",
            jql
        );
        // No undefined fragment: the token was
        // handled as free text, not as a
        // fragment-name lookup that failed.
        assert!(
            undefined.is_empty(),
            "unprefixed `label1` is free text, not an unknown fragment; got: {:?}",
            undefined
        );
    }

    /// The same query, prefixed with `@`, expands
    /// the fragment. This is the contract: `@`
    /// is a deliberate invocation, unprefixed is
    /// free text. The two behaviours co-exist
    /// cleanly in one parser.
    #[test]
    fn build_jql_prefixed_fragment_name_expands() {
        let (jql, undefined) = build_jql(
            "@label1",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert!(jql.contains(r#"(labels = "test")"#));
        assert!(undefined.is_empty());
    }

    /// Mixed query: `@label1` expands the
    /// fragment; the bare `label1` (no `@`)
    /// elsewhere in the body is free text. The
    /// two are AND-joined in the JQL.
    #[test]
    fn build_jql_mixed_prefixed_and_unprefixed_fragment_name() {
        let (jql, undefined) = build_jql(
            "@label1 label1",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        // The fragment expansion appears in the
        // JQL.
        assert!(
            jql.contains(r#"(labels = "test")"#),
            "expected fragment expansion for `@label1`; got: {}",
            jql
        );
        // The unprefixed token is in the free-text
        // path (description / summary).
        assert!(
            jql.contains(r#"(description ~ "label1" OR summary ~ "label1")"#),
            "expected free-text for unprefixed `label1`; got: {}",
            jql
        );
        // No undefined diagnostics: the
        // unprefixed token is free text, not a
        // typo.
        assert!(
            undefined.is_empty(),
            "unprefixed `label1` is free text, not a typo; got: {:?}",
            undefined
        );
    }

    /// The built-in aliases (`me`, `today`, `week`,
    /// `month`) keep their permissive matching —
    /// typing `me` (no `@`) still triggers the
    /// `@me` alias. The asymmetry with
    /// user-defined fragments is deliberate: the
    /// built-ins are short common words where
    /// requiring `@` would be friction, while
    /// user-defined patterns are typically
    /// project words (labels, epics) that would
    /// collide with free-text searches if
    /// expanded unprefixed.
    #[test]
    fn build_jql_built_in_aliases_still_work_without_at() {
        // `me` (no `@`) still sets `me_alias`.
        let (jql, undefined) = build_jql(
            "me",
            None,
            TEST_NOW_EPOCH,
            &empty_fragments(),
        );
        assert!(jql.contains("assignee = currentUser()"));
        assert!(undefined.is_empty());
    }

    #[test]
    fn build_jql_undefined_fragment_recorded_in_list() {
        // A `@<name>` token that isn't in the map
        // is recorded in the second return value.
        // The JQL still falls through to free text
        // (so the function never produces a parse
        // error) — the caller decides whether to
        // fire the search anyway.
        let (jql, undefined) = build_jql("@nosuch", None, TEST_NOW_EPOCH, &fragments_with_labels());
        // The token survives verbatim (with the `@`)
        // in the free-text path.
        assert!(jql.contains(r#"(description ~ "@nosuch" OR summary ~ "@nosuch")"#));
        // The undefined list has the bare name
        // (without the `@`), in the user's casing.
        assert_eq!(undefined, vec!["nosuch".to_string()]);
    }

    #[test]
    fn build_jql_undefined_fragment_dedupes() {
        // Repeating the same undefined fragment in
        // the body reports it once, not N times.
        let (_, undefined) = build_jql(
            "@nosuch @nosuch @nosuch",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert_eq!(undefined, vec!["nosuch".to_string()]);
    }

    #[test]
    fn build_jql_undefined_fragments_preserve_first_appearance_order() {
        let (_, undefined) = build_jql(
            "@first @second @first @third @second",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert_eq!(
            undefined,
            vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string(),
            ],
        );
    }

    #[test]
    fn build_jql_built_in_alias_not_marked_undefined() {
        // `@me`, `@today`, `@week`, `@month` are
        // built-in aliases. Typing them without a
        // matching config entry is NOT a typo —
        // they're a valid query on their own.
        // The undefined list must stay empty.
        let (_, undefined) = build_jql("@me @today", None, TEST_NOW_EPOCH, &empty_fragments());
        assert!(undefined.is_empty(), "got {:?}", undefined);
    }

    #[test]
    fn build_jql_mix_defined_and_undefined_fragments() {
        // One defined fragment, one undefined.
        // The JQL has the defined fragment spliced
        // and the undefined one as free text. The
        // undefined list has only the missing one.
        let (jql, undefined) = build_jql(
            "@label1 @nosuch",
            None,
            TEST_NOW_EPOCH,
            &fragments_with_labels(),
        );
        assert!(jql.contains(r#"(labels = "test")"#));
        assert!(jql.contains(r#"(description ~ "@nosuch" OR summary ~ "@nosuch")"#));
        assert_eq!(undefined, vec!["nosuch".to_string()]);
    }

    #[test]
    fn build_jql_empty_fragments_map_does_not_error() {
        // The default case: no user-defined
        // fragments. Every `@`-prefixed token in
        // the body is either a built-in alias or
        // falls through to free text.
        let (jql, undefined) = build_jql("@label1 @me", None, TEST_NOW_EPOCH, &empty_fragments());
        // `@label1` falls through to free text.
        assert!(jql.contains(r#"(description ~ "@label1" OR summary ~ "@label1")"#));
        // `@me` produces its clause.
        assert!(jql.contains("assignee = currentUser()"));
        // `label1` is reported as undefined.
        assert_eq!(undefined, vec!["label1".to_string()]);
    }

    #[test]
    fn build_jql_fragment_alone_in_body_omits_project() {
        // Without a body at all the empty-body
        // branch fires (server-wide or
        // project-scoped). With just `@label1` the
        // parser DOES run (the body is non-empty)
        // and produces `(labels = "test") ...`
        // without a project clause.
        let (jql, _) = build_jql("@label1", None, TEST_NOW_EPOCH, &fragments_with_labels());
        assert!(!jql.contains("project = "));
        assert!(jql.starts_with("(labels = "));
    }

    #[test]
    fn build_jql_empty_body_with_fragments_is_project_or_global() {
        // `-` alone (empty body) is the
        // "all aliases" / "no body" path; fragments
        // defined in the config are NOT
        // auto-included in an empty body. The
        // user has to type the fragment name
        // explicitly. This matches the other
        // built-in aliases: `-` alone shows
        // everything; `-@me` shows just the
        // user's tickets.
        let (jql_no_proj, _) = build_jql("", None, TEST_NOW_EPOCH, &fragments_with_labels());
        assert_eq!(jql_no_proj, "ORDER BY updated DESC");

        let (jql_with_proj, _) =
            build_jql("", Some("PROJ"), TEST_NOW_EPOCH, &fragments_with_labels());
        assert_eq!(jql_with_proj, r#"project = "PROJ" ORDER BY updated DESC"#);
    }

    #[test]
    fn build_jql_undefined_fragment_after_alias_does_not_clobber() {
        // An alias fires correctly even when
        // there's also an undefined fragment in
        // the body.
        let (jql, undefined) = build_jql("@me @nosuch", None, TEST_NOW_EPOCH, &empty_fragments());
        assert!(jql.contains("assignee = currentUser()"));
        assert!(jql.contains(r#"(description ~ "@nosuch" OR summary ~ "@nosuch")"#));
        assert_eq!(undefined, vec!["nosuch".to_string()]);
    }

    // ---- escape_jql_string ----

    #[test]
    fn escape_jql_string_quotes_plain() {
        assert_eq!(escape_jql_string("hello"), r#""hello""#);
    }

    #[test]
    fn escape_jql_string_escapes_backslash_and_quote() {
        assert_eq!(escape_jql_string(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    // ---- updated_to_epoch ----

    #[test]
    fn updated_to_epoch_parses_jira_offset() {
        // JIRA's `+0000` form (no colon).
        let e = updated_to_epoch("2024-06-30T19:14:39.000+0000");
        assert!(e > 1_700_000_000, "epoch should be in 2024+, got {}", e);
    }

    #[test]
    fn updated_to_epoch_parses_rfc3339() {
        let e = updated_to_epoch("2024-06-30T19:14:39.000+00:00");
        assert!(e > 1_700_000_000);
    }

    #[test]
    fn updated_to_epoch_empty_is_zero() {
        assert_eq!(updated_to_epoch(""), 0);
    }

    #[test]
    fn updated_to_epoch_garbage_is_zero() {
        assert_eq!(updated_to_epoch("not a date"), 0);
    }

    // ---- JiraConfig::browse_url ----

    #[test]
    fn browse_url_uses_server_with_browse_path() {
        let cfg = JiraConfig {
            server: "https://jira.internal".to_string(),
            token: "tok".to_string(),
            url: "https://jira.company.com/browse/".to_string(),
            project: None,
            max_results: 5,
            certificate_path: None,
            certificate_password: None,
            ca_certificate_path: None,
            available_projects: Vec::new(),
            available_issue_types: Vec::new(),
            clone_fields: Vec::new(),
        };
        assert_eq!(
            cfg.browse_url("PROJ-123"),
            "https://jira.internal/browse/PROJ-123"
        );
    }

    #[test]
    fn browse_url_default_uses_server_when_url_unset() {
        // Constructed directly (from_env would fall back too,
        // but that depends on the live environment).
        let cfg = JiraConfig {
            server: "https://jira".to_string(),
            token: "t".to_string(),
            url: "https://jira".to_string(),
            project: None,
            max_results: 5,
            certificate_path: None,
            certificate_password: None,
            ca_certificate_path: None,
            available_projects: Vec::new(),
            available_issue_types: Vec::new(),
            clone_fields: Vec::new(),
        };
        assert_eq!(cfg.browse_url("X-1"), "https://jira/browse/X-1");
    }

    // ---- parse_comma_list / resolve_available_projects / resolve_available_issue_types ----

    #[test]
    fn parse_comma_list_trims_and_drops_empties() {
        assert_eq!(
            parse_comma_list(" Epic, Task ,, Bug"),
            vec!["Epic".to_string(), "Task".to_string(), "Bug".to_string()]
        );
    }

    #[test]
    fn parse_comma_list_empty_string_is_empty_vec() {
        assert_eq!(parse_comma_list(""), Vec::<String>::new());
    }

    /// The create-JIRA-issue dialog's Project selector's list. When
    /// `JIRA_AVAILABLE_PROJECTS` is set (and non-empty after
    /// parsing), it wins outright — `JIRA_PROJECT` is not merged in.
    #[test]
    fn resolve_available_projects_prefers_available_projects_env() {
        assert_eq!(
            resolve_available_projects(Some("ENG, OPS"), Some("DEFAULT")),
            vec!["ENG".to_string(), "OPS".to_string()]
        );
    }

    /// Unlike `available_issue_types`, there's no universal default
    /// project list — falling back to the single already-configured
    /// `JIRA_PROJECT` value is the closest thing to a sensible
    /// default.
    #[test]
    fn resolve_available_projects_falls_back_to_single_project_when_unset() {
        assert_eq!(
            resolve_available_projects(None, Some("ENG")),
            vec!["ENG".to_string()]
        );
    }

    /// An explicitly-set but empty/whitespace-only
    /// `JIRA_AVAILABLE_PROJECTS` falls back the same way an unset one
    /// does -- "on" with nothing parseable in it shouldn't produce
    /// an unselectable empty dialog when a plain `JIRA_PROJECT` is
    /// available as a fallback.
    #[test]
    fn resolve_available_projects_falls_back_when_env_parses_empty() {
        assert_eq!(
            resolve_available_projects(Some("  ,  ,"), Some("ENG")),
            vec!["ENG".to_string()]
        );
    }

    /// With neither env var set, there's genuinely nothing to
    /// select from -- an empty `Vec`, not a panic or a made-up
    /// default (project keys are installation-specific, unlike
    /// issue types).
    #[test]
    fn resolve_available_projects_empty_when_neither_env_set() {
        assert_eq!(resolve_available_projects(None, None), Vec::<String>::new());
    }

    #[test]
    fn resolve_available_issue_types_uses_env_when_set() {
        assert_eq!(
            resolve_available_issue_types(Some("Bug, Task")),
            vec!["Bug".to_string(), "Task".to_string()]
        );
    }

    /// Unlike projects, issue types DO have a sensible universal
    /// default -- JIRA's own standard set -- so an unset (or
    /// empty-after-parsing) env var falls back to that instead of an
    /// empty list.
    #[test]
    fn resolve_available_issue_types_defaults_when_unset() {
        assert_eq!(
            resolve_available_issue_types(None),
            vec![
                "Epic".to_string(),
                "Initiative".to_string(),
                "Story".to_string(),
                "Task".to_string(),
                "Bug".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_available_issue_types_defaults_when_env_parses_empty() {
        assert_eq!(
            resolve_available_issue_types(Some(" , ,")),
            resolve_available_issue_types(None)
        );
    }

    // ---- cf_bracket_field_id / resolve_clone_fields ----

    #[test]
    fn cf_bracket_field_id_parses_valid_bracket() {
        assert_eq!(cf_bracket_field_id("cf[11601]"), Some("customfield_11601".to_string()));
    }

    #[test]
    fn cf_bracket_field_id_rejects_non_digit_contents() {
        assert_eq!(cf_bracket_field_id("cf[abc]"), None);
    }

    #[test]
    fn cf_bracket_field_id_rejects_empty_brackets() {
        assert_eq!(cf_bracket_field_id("cf[]"), None);
    }

    #[test]
    fn cf_bracket_field_id_rejects_missing_prefix_or_suffix() {
        assert_eq!(cf_bracket_field_id("11601]"), None);
        assert_eq!(cf_bracket_field_id("cf[11601"), None);
        assert_eq!(cf_bracket_field_id("summary"), None);
    }

    #[test]
    fn resolve_clone_fields_parses_comma_separated_list() {
        assert_eq!(
            resolve_clone_fields(Some("cf[11601],cf[10050]")),
            vec!["customfield_11601".to_string(), "customfield_10050".to_string()]
        );
    }

    /// A malformed entry (not a valid `cf[<digits>]`) is dropped, not a
    /// startup error — same "garbled config can't wedge the app" policy
    /// every other env var in `JiraConfig` follows.
    #[test]
    fn resolve_clone_fields_drops_malformed_entries() {
        assert_eq!(
            resolve_clone_fields(Some("cf[11601], notacustomfield, cf[abc]")),
            vec!["customfield_11601".to_string()]
        );
    }

    #[test]
    fn resolve_clone_fields_unset_is_empty() {
        assert_eq!(resolve_clone_fields(None), Vec::<String>::new());
    }

    // ---- extract_custom_field_value ----

    #[test]
    fn extract_custom_field_value_plain_string() {
        assert_eq!(extract_custom_field_value(&serde_json::json!("Team ComS")), "Team ComS");
    }

    #[test]
    fn extract_custom_field_value_null_is_empty() {
        assert_eq!(extract_custom_field_value(&serde_json::Value::Null), "");
    }

    #[test]
    fn extract_custom_field_value_object_with_value_key() {
        assert_eq!(
            extract_custom_field_value(&serde_json::json!({"value": "High", "id": "1"})),
            "High"
        );
    }

    #[test]
    fn extract_custom_field_value_object_with_name_key() {
        assert_eq!(
            extract_custom_field_value(&serde_json::json!({"name": "Alice", "id": "2"})),
            "Alice"
        );
    }

    /// An object shape with neither `"value"` nor `"name"` falls back to
    /// the raw JSON rather than silently losing the data.
    #[test]
    fn extract_custom_field_value_unrecognized_object_falls_back_to_raw_json() {
        let v = serde_json::json!({"foo": "bar"});
        assert_eq!(extract_custom_field_value(&v), v.to_string());
    }

    #[test]
    fn extract_custom_field_value_number_falls_back_to_raw_json() {
        assert_eq!(extract_custom_field_value(&serde_json::json!(42)), "42");
    }

    // ---- JSON parsing ----

    #[test]
    fn parse_search_response_minimal() {
        let json = r#"{"issues":[{"key":"PROJ-1","fields":{"summary":"s"}}]}"#;
        let parsed: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.issues.len(), 1);
        assert_eq!(parsed.issues[0].key, "PROJ-1");
        let issue = JiraIssue::from(ApiIssue {
            key: parsed.issues[0].key.clone(),
            fields: parsed.issues[0].fields.clone(),
        });
        assert_eq!(issue.summary, "s");
        assert_eq!(issue.status, ""); // absent → empty
    }

    #[test]
    fn parse_search_response_full_fields() {
        let json = r#"{"issues":[{"key":"PROJ-2","fields":{
            "summary":"boom","status":{"name":"Done"},
            "issuetype":{"name":"Bug"},"priority":{"name":"High"},
            "assignee":{"name":"Alice"},
            "updated":"2024-06-30T19:14:39.000+0000",
            "duedate":"2024-07-15",
            "description":{"type":"doc","version":1,"content":[
                {"type":"paragraph","content":[
                    {"type":"text","text":"Hello "},
                    {"type":"text","text":"world."}
                ]}
            ]}
        }}]}"#;
        let parsed: SearchResponse = serde_json::from_str(json).unwrap();
        let issue = JiraIssue::from(parsed.issues.into_iter().next().unwrap());
        assert_eq!(issue.key, "PROJ-2");
        assert_eq!(issue.status, "Done");
        assert_eq!(issue.issuetype, "Bug");
        assert_eq!(issue.priority, "High");
        assert_eq!(issue.assignee, "Alice");
        assert!(updated_to_epoch(&issue.updated) > 0);
        // Due date flows through verbatim.
        assert_eq!(issue.due, "2024-07-15");
        // The ADF description is walked and
        // flattened to plain text.
        assert_eq!(issue.description, "Hello world.");
    }

    /// The `duedate` and `description` fields are
    /// optional on every issue. Both `null` and
    /// absent must degrade to empty strings —
    /// the From impl wraps both in
    /// `unwrap_or_default()` and the description
    /// extractor returns empty for null/missing.
    #[test]
    fn parse_search_response_due_and_description_optional() {
        let json = r#"{"issues":[
            {"key":"PROJ-A","fields":{"summary":"no due"}},
            {"key":"PROJ-B","fields":{"summary":"null due",
              "duedate":null,"description":null}},
            {"key":"PROJ-C","fields":{"summary":"empty desc",
              "description":{}}}
        ]}"#;
        let parsed: SearchResponse = serde_json::from_str(json).unwrap();
        let issues: Vec<JiraIssue> = parsed.issues.into_iter().map(JiraIssue::from).collect();
        // Absent `duedate` and `description` →
        // empty strings.
        assert_eq!(issues[0].due, "");
        assert_eq!(issues[0].description, "");
        // Explicit `null` for both → empty strings.
        assert_eq!(issues[1].due, "");
        assert_eq!(issues[1].description, "");
        // An empty `description` object (no `type`
        // and no `content`) → empty string.
        assert_eq!(issues[2].description, "");
    }

    /// Some JIRA events / webhooks / older versions
    /// return issues with an empty or missing `fields`
    /// object. Must not fail the search.
    #[test]
    fn parse_search_response_empty_fields_dont_fail() {
        let json = r#"{"issues":[{"key":"PROJ-A","fields":{}},{"key":"PROJ-B","fields":{"summary":"ok"}}]}"#;
        let parsed: SearchResponse = serde_json::from_str(json).unwrap();
        let issues: Vec<JiraIssue> = parsed.issues.into_iter().map(JiraIssue::from).collect();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].key, "PROJ-A");
        assert_eq!(issues[0].summary, "");
        assert_eq!(issues[0].status, "");
        assert_eq!(issues[1].summary, "ok");
    }

    // ---- comment response parsing ----

    /// A typical comment response with two
    /// comments. Both have authors, full ADF
    /// bodies, and ISO-8601 timestamps.
    #[test]
    fn parse_comments_response_full() {
        let json = r#"{
            "startAt": 0,
            "maxResults": 100,
            "total": 2,
            "comments": [
                {
                    "id": "10001",
                    "author": {"name": "Alice"},
                    "body": {"type":"doc","version":1,"content":[
                        {"type":"paragraph","content":[
                            {"type":"text","text":"First comment."}
                        ]}
                    ]},
                    "created": "2024-06-30T19:14:39.000+0000",
                    "updated": "2024-06-30T19:14:39.000+0000"
                },
                {
                    "id": "10002",
                    "author": {"name": "Bob"},
                    "body": {"type":"doc","version":1,"content":[
                        {"type":"paragraph","content":[
                            {"type":"text","text":"Second."}
                        ]}
                    ]},
                    "created": "2024-06-29T10:00:00.000+0000",
                    "updated": "2024-06-29T10:00:00.000+0000"
                }
            ]
        }"#;
        let parsed: CommentsResponse = serde_json::from_str(json).unwrap();
        let comments: Vec<JiraComment> =
            parsed.comments.into_iter().map(JiraComment::from).collect();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].id, "10001");
        assert_eq!(comments[0].author, "Alice");
        assert_eq!(comments[0].body, "First comment.");
        assert_eq!(comments[0].created, "2024-06-30T19:14:39.000+0000");
        assert_eq!(comments[0].updated, "2024-06-30T19:14:39.000+0000");
        assert_eq!(comments[1].author, "Bob");
        assert_eq!(comments[1].body, "Second.");
    }

    /// An empty `comments` array (the
    /// common case for issues with no
    /// comments) is parsed cleanly — the
    /// response shape tolerates the empty list
    /// because of the `#[serde(default)]` on
    /// the `comments` field.
    #[test]
    fn parse_comments_response_empty_list() {
        let json = r#"{"comments":[]}"#;
        let parsed: CommentsResponse = serde_json::from_str(json).unwrap();
        let comments: Vec<JiraComment> =
            parsed.comments.into_iter().map(JiraComment::from).collect();
        assert!(comments.is_empty());
    }

    /// `null` and missing optional fields
    /// (`id`, `author`, `body`, `created`,
    /// `updated`) must all degrade to empty
    /// strings, not fail the parse. JIRA's
    /// real responses frequently have system
    /// comments with most of these fields
    /// `null`.
    #[test]
    fn parse_comments_response_null_and_missing_fields() {
        let json = r#"{"comments":[
            {"id":null,"author":null,"body":null,"created":null,"updated":null},
            {"id":"10002","body":{}}
        ]}"#;
        let parsed: CommentsResponse = serde_json::from_str(json).unwrap();
        let comments: Vec<JiraComment> =
            parsed.comments.into_iter().map(JiraComment::from).collect();
        assert_eq!(comments.len(), 2);
        // First comment: every field is
        // either `null` or absent; all
        // degrade to empty strings.
        assert_eq!(comments[0].id, "");
        assert_eq!(comments[0].author, "");
        assert_eq!(comments[0].body, "");
        assert_eq!(comments[0].created, "");
        assert_eq!(comments[0].updated, "");
        // Second comment: `created`
        // missing — `updated` falls back
        // to `created` (which is empty).
        assert_eq!(comments[1].id, "10002");
        assert_eq!(comments[1].author, "");
        // An empty `body` object → empty
        // string (the extractor returns
        // empty for an object with no
        // `type` and no `content`).
        assert_eq!(comments[1].body, "");
        assert_eq!(comments[1].created, "");
        assert_eq!(comments[1].updated, "");
    }

    /// `author.name` can be `null` (rare
    /// but possible for system comments).
    /// The author field degrades to an empty
    /// string rather than failing the parse.
    #[test]
    fn parse_comments_response_author_name_null() {
        let json = r#"{"comments":[
            {"id":"1","author":{"name":null},"body":null,"created":"2024-06-30T00:00:00.000+0000"}
        ]}"#;
        let parsed: CommentsResponse = serde_json::from_str(json).unwrap();
        let comments: Vec<JiraComment> =
            parsed.comments.into_iter().map(JiraComment::from).collect();
        assert_eq!(comments[0].author, "");
    }

    // ---- extract_adf_text ----

    /// ADF helper: parse a JSON snippet into a
    /// `serde_json::Value` for the extractor. Lets
    /// the tests below use a compact inline
    /// notation without re-typing the
    /// `serde_json::from_str` boilerplate.
    fn adf(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("valid ADF JSON")
    }

    #[test]
    fn adf_empty_doc() {
        // A document with no `content` returns
        // an empty string. Common case for an
        // issue that was created with no
        // description.
        assert_eq!(extract_adf_text(&adf(r#"{"type":"doc","version":1}"#)), "");
    }

    #[test]
    fn adf_single_paragraph() {
        // The most common shape: a doc with one
        // paragraph containing one text node.
        assert_eq!(
            extract_adf_text(&adf(r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"Hello world."}
                    ]}
                ]}"#)),
            "Hello world."
        );
    }

    #[test]
    fn adf_multiple_paragraphs_joined_with_newline() {
        // Two paragraphs become one string with a
        // newline separator. (Earlier designs
        // folded to a single space, but the new
        // multi-line preview / overlay layout
        // wants the paragraph boundaries to
        // survive so the description body can
        // span multiple rendered lines.)
        assert_eq!(
            extract_adf_text(&adf(r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"First."}
                    ]},
                    {"type":"paragraph","content":[
                        {"type":"text","text":"Second."}
                    ]}
                ]}"#)),
            "First.\nSecond."
        );
    }

    #[test]
    fn adf_text_split_across_nodes_is_concatenated() {
        // JIRA often splits a single visual
        // sentence into multiple `text` nodes
        // (formatting marks between them). The
        // extractor concatenates them in order.
        assert_eq!(
            extract_adf_text(&adf(r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"The "},
                        {"type":"text","text":"quick "},
                        {"type":"text","text":"fox."}
                    ]}
                ]}"#)),
            "The quick fox."
        );
    }

    #[test]
    fn adf_mention_uses_attrs_text() {
        // A `@user` mention carries
        // `attrs.text` like `"@alice"`. The
        // extractor prefers that over a
        // hand-rolled `@`-prefix concatenation.
        assert_eq!(
            extract_adf_text(&adf(r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"Hi "},
                        {"type":"mention","attrs":{
                            "id":"5","text":"@alice","displayName":"Alice"
                        }},
                        {"type":"text","text":" please review."}
                    ]}
                ]}"#)),
            "Hi @alice please review."
        );
    }

    #[test]
    fn adf_mention_falls_back_to_display_name() {
        // No `attrs.text` — fall back to
        // `attrs.displayName` with an `@`
        // prefix. Common for Jira Service
        // Management customers whose mention
        // shape omits the `text` shorthand.
        assert_eq!(
            extract_adf_text(&adf(r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"mention","attrs":{
                            "id":"5","displayName":"Alice"
                        }}
                    ]}
                ]}"#)),
            "@Alice"
        );
    }

    #[test]
    fn adf_link_uses_child_text() {
        // A link with child text nodes renders
        // the child text. The href is silently
        // dropped (we have no way to render it
        // inline in a single-line preview
        // without breaking the line on
        // long URLs).
        assert_eq!(
            extract_adf_text(&adf(r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"See "},
                        {"type":"link","attrs":{
                            "href":"https://example.com/wonky/url"
                        },"content":[
                            {"type":"text","text":"docs"}
                        ]},
                        {"type":"text","text":" for more."}
                    ]}
                ]}"#)),
            "See docs for more."
        );
    }

    #[test]
    fn adf_link_falls_back_to_href_when_no_children() {
        // A bare link with no child text nodes
        // renders the href. Useful when an
        // author pasted a URL with no
        // descriptive text.
        assert_eq!(
            extract_adf_text(&adf(r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"link","attrs":{
                            "href":"https://example.com/wonky/url"
                        }}
                    ]}
                ]}"#)),
            "https://example.com/wonky/url"
        );
    }

    #[test]
    fn adf_emoji_renders_short_name() {
        // Emoji nodes carry `:smile:` style
        // short names in `attrs.shortName`. We
        // render them literally so the user
        // gets a hint that an emoji was in the
        // original.
        assert_eq!(
            extract_adf_text(&adf(r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"Hello "},
                        {"type":"emoji","attrs":{
                            "shortName":":wave:","id":"1f44b"
                        }}
                    ]}
                ]}"#)),
            "Hello :wave:"
        );
    }

    #[test]
    fn adf_hard_break_becomes_newline() {
        // A `hardBreak` inside a paragraph is a
        // soft line break — rendered as a real
        // newline so the author's line structure
        // survives. (Earlier designs folded to
        // a space, but the new multi-line
        // layout wants the line breaks to
        // survive.)
        assert_eq!(
            extract_adf_text(&adf(r#"{"type":"doc","version":1,"content":[
                    {"type":"paragraph","content":[
                        {"type":"text","text":"line one"},
                        {"type":"hardBreak"},
                        {"type":"text","text":"line two"}
                    ]}
                ]}"#)),
            "line one\nline two"
        );
    }

    #[test]
    fn adf_bullet_list_items_joined() {
        // A bullet list flattens to multiple
        // lines, one per item. (The list-item
        // contains a paragraph, and paragraphs
        // are now separated by newlines in the
        // extractor output. Earlier designs
        // folded to spaces; the new multi-line
        // layout wants each item on its own
        // line in the rendered preview / overlay.)
        assert_eq!(
            extract_adf_text(&adf(r#"{"type":"doc","version":1,"content":[
                    {"type":"bulletList","content":[
                        {"type":"listItem","content":[
                            {"type":"paragraph","content":[
                                {"type":"text","text":"first"}
                            ]}
                        ]},
                        {"type":"listItem","content":[
                            {"type":"paragraph","content":[
                                {"type":"text","text":"second"}
                            ]}
                        ]}
                    ]}
                ]}"#)),
            "first\nsecond"
        );
    }

    #[test]
    fn adf_plain_string_fallback() {
        // Some JIRA installations or custom
        // apps may return a flat string instead
        // of ADF. The extractor returns the
        // string verbatim.
        assert_eq!(
            extract_adf_text(&adf(r#""just a string""#)),
            "just a string"
        );
    }

    #[test]
    fn adf_null_and_bool_fall_through_to_empty() {
        // Defensive: a `null` or boolean at the
        // top level should render as empty
        // rather than panic.
        assert_eq!(extract_adf_text(&adf("null")), "");
    }

    #[test]
    fn adf_keeps_full_description_no_truncation() {
        // The new design keeps the FULL
        // description text in `JiraIssue`
        // (no character cap) and lets the
        // preview renderer / overlay do their
        // own line-budget truncation. A
        // 500-character description flows
        // through to the issue unchanged.
        let text = "a".repeat(500);
        let json_str = format!(
            r#"{{"type":"doc","version":1,"content":[
                {{"type":"paragraph","content":[
                    {{"type":"text","text":"{}"}}
                ]}}
            ]}}"#,
            text
        );
        let issue: JiraIssue = serde_json::from_str::<SearchResponse>(&format!(
            r#"{{"issues":[{{"key":"P-1","fields":{{"description":{}}}}}]}}"#,
            json_str
        ))
        .unwrap()
        .issues
        .into_iter()
        .next()
        .unwrap()
        .into();
        // No trailing `…` — the text is
        // preserved verbatim.
        assert!(!issue.description.ends_with('…'));
        // The full 500 characters are present.
        assert_eq!(issue.description.chars().count(), 500);
        for c in issue.description.chars() {
            assert_eq!(c, 'a');
        }
    }

    // `jira_field_complete` / `jira_field_complete_with_value`
    // — the JQL field-name tab-completion
    // helpers. These are the core
    // completion logic; the TUI layer
    // (in `src/tui.rs`) is a thin
    // wrapper that finds the
    // field-name prefix at the
    // cursor and calls these.
    //
    // The tests cover the three
    // outcomes the function can
    // produce (single-match
    // expansion, multi-match
    // longest-common-prefix,
    // no-match) and the edge
    // cases (empty prefix,
    // case-insensitivity, exact
    // match against a system
    // field).

    /// Exact match: `lab` matches
    /// `label` and `labels` (two
    /// matches), so the function
    /// returns the longest common
    /// prefix `label` (the point
    /// at which the two fields
    /// diverge) with no trailing
    /// `=`. The user keeps
    /// typing to disambiguate.
    #[test]
    fn jira_field_complete_ambiguous_returns_longest_common_prefix() {
        // `lab` is a prefix of
        // both `label` and
        // `labels`. The
        // longest common
        // prefix of those two
        // is `label` (they
        // agree on `label`;
        // `labels` continues
        // with `s` while
        // `label` ends).
        // The completion
        // algorithm must
        // extend the prefix
        // to that point so
        // the user makes
        // forward progress.
        assert_eq!(
            jira_field_complete("lab").as_deref(),
            Some("label"),
            "ambiguous prefix extends to the common prefix (just before the divergence)"
        );
    }

    /// Single match: `ass` matches
    /// `assignee` only. The
    /// completion returns the
    /// full field name with a
    /// trailing `=` so the user
    /// can immediately type the
    /// value.
    #[test]
    fn jira_field_complete_with_value_single_match_appends_equals() {
        assert_eq!(
            jira_field_complete_with_value("ass").as_deref(),
            Some("assignee="),
            "single match expands to full field name + `=`"
        );
    }

    /// No match: `xy` matches
    /// nothing. The function
    /// returns `None` (caller
    /// surfaces a status
    /// message and leaves the
    /// query unchanged).
    #[test]
    fn jira_field_complete_no_match_returns_none() {
        assert_eq!(jira_field_complete("xy"), None);
        assert_eq!(jira_field_complete_with_value("xy"), None);
    }

    /// Empty prefix: the function
    /// returns `None` rather
    /// than expanding to
    /// everything. The user
    /// might press Tab in
    /// unusual states (cursor
    /// at the start of the
    /// query, cursor right
    /// after a value `=`),
    /// and we don't want to
    /// silently destroy their
    /// context.
    #[test]
    fn jira_field_complete_empty_prefix_returns_none() {
        assert_eq!(jira_field_complete(""), None);
        assert_eq!(jira_field_complete_with_value(""), None);
    }

    /// Case-insensitive: `LAB`
    /// matches the same fields
    /// as `lab`. The completion
    /// preserves the CANONICAL
    /// casing from the
    /// `JIRA_FIELDS` table
    /// (`label` and `labels`),
    /// not the user's input
    /// casing.
    #[test]
    fn jira_field_complete_is_case_insensitive() {
        // The canonical casing is
        // mixed-case (`label` /
        // `labels`), so the
        // returned completion
        // has the canonical
        // casing regardless of
        // the input casing.
        // `LAB` matches both
        // `label` and `labels`,
        // so the result is
        // the longest common
        // prefix `label`
        // (canonical-cased).
        assert_eq!(
            jira_field_complete("LAB").as_deref(),
            Some("label"),
            "case-insensitive match returns canonical-cased completion"
        );
        assert_eq!(
            jira_field_complete("Lab").as_deref(),
            Some("label"),
            "mixed-case input returns canonical-cased completion"
        );
        // The same logic applies
        // to the
        // `_with_value`
        // variant: `ASS`
        // matches only
        // `assignee`, so the
        // result is
        // `assignee=`
        // (canonical-cased).
        assert_eq!(
            jira_field_complete_with_value("ASS").as_deref(),
            Some("assignee="),
            "case-insensitive single match returns canonical-cased `field=`"
        );
    }

    /// Exact full-field-name match:
    /// the user typed `labels`
    /// (a complete field) and
    /// pressed Tab. The
    /// completion extends to
    /// the same field name
    /// (no-op for the field
    /// part) and appends `=`
    /// so the user can type
    /// the value.
    #[test]
    fn jira_field_complete_with_value_full_field_name_appends_equals() {
        // `labels` is a complete
        // system field. The
        // completion is
        // `labels=` (the field
        // name itself plus
        // `=`).
        assert_eq!(
            jira_field_complete_with_value("labels").as_deref(),
            Some("labels="),
            "complete field name expands to itself + `=`"
        );
    }

    /// Single-character prefix:
    /// `s` is a prefix of
    /// multiple fields
    /// (`status`,
    /// `statusCategory`,
    /// `sprint`, `summary`,
    /// `storyPoints`,
    /// `statusCategory`, …).
    /// The longest common
    /// prefix of those is
    /// just `s`, so the
    /// function returns
    /// `s` (no `=`, the user
    /// keeps typing).
    #[test]
    fn jira_field_complete_very_ambiguous_returns_shortest_common() {
        // `s` matches too many
        // fields to be
        // useful; the
        // function returns
        // `s` (no
        // progress).
        let r = jira_field_complete("s");
        assert_eq!(r.as_deref(), Some("s"));
        // The `_with_value`
        // variant must NOT
        // append `=` in the
        // ambiguous case —
        // we'd be guessing
        // which field the
        // user is heading
        // toward.
        assert_eq!(
            jira_field_complete_with_value("s").as_deref(),
            Some("s"),
            "ambiguous prefix in `_with_value` does NOT append `=`"
        );
    }

    // ---- jira_alias_complete ----

    #[test]
    fn jira_alias_complete_single_match_returns_name() {
        let fragments = empty_fragments();
        // `mo` matches only `month`.
        assert_eq!(
            jira_alias_complete("mo", &fragments).as_deref(),
            Some("month"),
            "`mo` matches only `month`"
        );
    }

    #[test]
    fn jira_alias_complete_with_space_appends_trailing_space() {
        let fragments = empty_fragments();
        // `mo` matches only `month`.
        assert_eq!(
            jira_alias_complete_with_space("mo", &fragments).as_deref(),
            Some("month "),
            "single match gets trailing space"
        );
    }

    #[test]
    fn jira_alias_complete_ambiguous_returns_longest_common_prefix() {
        let fragments = empty_fragments();
        // `m` matches both `me` and `month`;
        // longest common prefix is `m`.
        assert_eq!(
            jira_alias_complete("m", &fragments).as_deref(),
            Some("m"),
            "`m` matches `me` and `month`; LCP is `m`"
        );
        // `mo` matches only `month`.
        assert_eq!(
            jira_alias_complete_with_space("mo", &fragments).as_deref(),
            Some("month "),
            "`mo` matches only `month`"
        );
    }

    #[test]
    fn jira_alias_complete_no_match_returns_none() {
        let fragments = empty_fragments();
        assert_eq!(jira_alias_complete("xyz", &fragments), None);
        assert_eq!(jira_alias_complete_with_space("xyz", &fragments), None);
    }

    #[test]
    fn jira_alias_complete_empty_prefix_returns_none() {
        let fragments = empty_fragments();
        assert_eq!(jira_alias_complete("", &fragments), None);
        assert_eq!(jira_alias_complete_with_space("", &fragments), None);
    }

    #[test]
    fn jira_alias_complete_is_case_insensitive() {
        let fragments = empty_fragments();
        // `mo` matches `month` regardless of case.
        assert_eq!(
            jira_alias_complete("MO", &fragments).as_deref(),
            Some("month"),
            "uppercase prefix matches lowercase alias"
        );
        assert_eq!(
            jira_alias_complete_with_space("TO", &fragments).as_deref(),
            Some("today "),
            "mixed-case prefix matches"
        );
    }

    #[test]
    fn jira_alias_complete_includes_user_fragments() {
        let mut fragments = std::collections::HashMap::new();
        fragments.insert("sprint".to_string(), "sprint = \"42\"".to_string());
        fragments.insert("blocked".to_string(), "resolution = Unresolved".to_string());
        // `sp` matches `sprint` (fragment) and `status` is not an alias.
        assert_eq!(
            jira_alias_complete_with_space("sp", &fragments).as_deref(),
            Some("sprint "),
            "fragment `sprint` is included in completion"
        );
        // `b` matches `blocked` (fragment).
        assert_eq!(
            jira_alias_complete_with_space("b", &fragments).as_deref(),
            Some("blocked "),
            "fragment `blocked` is included"
        );
        // `bl` also matches only `blocked`.
        assert_eq!(
            jira_alias_complete_with_space("bl", &fragments).as_deref(),
            Some("blocked "),
        );
    }

    #[test]
    fn jira_alias_complete_fragments_and_builtins_together() {
        let mut fragments = std::collections::HashMap::new();
        fragments.insert("meeting".to_string(), "summary ~ meeting".to_string());
        // `me` matches BOTH `me` (built-in) and `meeting` (fragment).
        // Longest common prefix is `me`.
        assert_eq!(
            jira_alias_complete("me", &fragments).as_deref(),
            Some("me"),
            "ambiguous between builtin and fragment returns LCP"
        );
        // `mee` matches only `meeting`.
        assert_eq!(
            jira_alias_complete_with_space("mee", &fragments).as_deref(),
            Some("meeting "),
            "disambiguated prefix matches fragment only"
        );
    }

    // ---- notes_tag_complete / notes_link_complete ----

    /// Build a note_search database with the given
    /// tag / link names and return the path. The
    /// tags and links are inserted directly into the
    /// `todo_entries` and `markdown_data` tables
    /// (the tables `get_unique_values` reads from for
    /// tags and links). One row per tag/link is
    /// enough to populate the unique-value set.
    fn make_notes_db_with_tags_and_links(
        tags: &[&str],
        links: &[&str],
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        use rusqlite::Connection;
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "smarthistory-completetest-{}-{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");
        let db_path = dir.join("notes.sqlite");
        // Write a real markdown file with `#tag` / `[[link]]`
        // tokens in the body and index it through
        // `process_markdown_file` + `write_markdown_data_to_sqlite_with_conn`
        // (the real indexer's write path), rather than a raw
        // `INSERT INTO markdown_data (...) VALUES (...)`
        // bypass. `notes_tag_complete`/`notes_link_complete`
        // read from the `note_tags`/`note_links` junction
        // tables via `note_search::commands::metadata::get_unique_values`,
        // which are only populated by that write path — the
        // markdown_data/todo_entries JSON columns a hand-rolled
        // INSERT can fill in are not what completion reads.
        let tag_tokens: String = tags.iter().map(|t| format!("#{} ", t)).collect();
        let link_tokens: String = links.iter().map(|l| format!("[[{}]] ", l)).collect();
        let body = format!("---\ntitle: test\n---\n\n{}{}\n", tag_tokens, link_tokens);
        std::fs::write(dir.join("test.md"), &body).expect("write test.md");

        let conn = Connection::open(&db_path).expect("open db");
        note_search::init_database_schema(&conn).expect("schema");
        let data = note_search::markdown_parser::process_markdown_file(&dir.join("test.md"), &dir)
            .expect("process file");
        note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
            .map_err(|e| format!("write: {e}"))
            .expect("write db");
        drop(conn);
        (dir, db_path)
    }

    #[test]
    fn notes_tag_complete_unique_match_returns_name_with_space() {
        let (_dir, db) = make_notes_db_with_tags_and_links(&["feature", "bug", "urgent"], &[]);
        assert_eq!(
            notes_tag_complete(&db, "feat").as_deref(),
            Some("feature "),
            "unique tag match gets trailing space"
        );
    }

    #[test]
    fn notes_tag_complete_ambiguous_returns_lcp() {
        let (_dir, db) = make_notes_db_with_tags_and_links(&["feature", "feat-list"], &[]);
        // `feat` matches both `feature` and `feat-list`.
        // LCP is `feat`.
        assert_eq!(
            notes_tag_complete(&db, "feat").as_deref(),
            Some("feat"),
            "ambiguous tag prefix returns LCP without trailing space"
        );
    }

    #[test]
    fn notes_tag_complete_no_match_returns_none() {
        let (_dir, db) = make_notes_db_with_tags_and_links(&["feature"], &[]);
        assert_eq!(notes_tag_complete(&db, "xyz"), None);
    }

    #[test]
    fn notes_tag_complete_empty_prefix_returns_none() {
        let (_dir, db) = make_notes_db_with_tags_and_links(&["feature"], &[]);
        assert_eq!(notes_tag_complete(&db, ""), None);
    }

    #[test]
    fn notes_tag_complete_is_case_insensitive() {
        let (_dir, db) = make_notes_db_with_tags_and_links(&["Feature"], &[]);
        assert_eq!(
            notes_tag_complete(&db, "feat").as_deref(),
            Some("Feature "),
            "case-insensitive prefix matches tag (preserves canonical casing)"
        );
    }

    #[test]
    fn notes_link_complete_unique_match_returns_name_with_space() {
        // Link targets are
        // case-insensitive in
        // Obsidian — the
        // expansion always uses
        // the lowercase form
        // regardless of how the
        // link is stored in the
        // database.
        let (_dir, db) = make_notes_db_with_tags_and_links(&[], &["NeovimNote.md", "RustBook.md"]);
        assert_eq!(
            notes_link_complete(&db, "Neo").as_deref(),
            Some("[[neovimnote]] "),
            "unique link match gets [[...]] syntax and .md suffix is stripped (lowercase)"
        );
    }

    #[test]
    fn notes_link_complete_ambiguous_returns_lcp() {
        let (_dir, db) =
            make_notes_db_with_tags_and_links(&[], &["NeovimNote.md", "NeovimConfig.md"]);
        // `Neo` matches both; LCP
        // is `Neovim`. The LCP is
        // computed case-insensitively
        // and returned in lowercase
        // (Obsidian convention).
        assert_eq!(
            notes_link_complete(&db, "Neo").as_deref(),
            Some("[[neovim]]"),
            "ambiguous link prefix returns [[...]] LCP (lowercase)"
        );
    }

    #[test]
    fn notes_link_complete_no_match_returns_none() {
        let (_dir, db) = make_notes_db_with_tags_and_links(&[], &["NeovimNote.md"]);
        assert_eq!(notes_link_complete(&db, "xyz"), None);
    }

    #[test]
    fn notes_link_complete_is_case_insensitive() {
        // Link targets are
        // case-insensitive in
        // Obsidian — the
        // expansion always uses
        // the lowercase form
        // regardless of how the
        // link is stored in the
        // database.
        let (_dir, db) = make_notes_db_with_tags_and_links(&[], &["NeovimNote.md"]);
        assert_eq!(
            notes_link_complete(&db, "neo").as_deref(),
            Some("[[neovimnote]] "),
            "case-insensitive prefix matches link (lowercase expansion)"
        );
    }

    #[test]
    fn notes_link_complete_strips_md_suffix() {
        // The database stores link targets
        // with their `.md` extension. The
        // completion should strip it so the
        // user gets the bare note name
        // (matching Obsidian's
        // `[[NoteName]]` convention).
        let (_dir, db) = make_notes_db_with_tags_and_links(&[], &["bernd_matthiesen.md"]);
        assert_eq!(
            notes_link_complete(&db, "bernd").as_deref(),
            Some("[[bernd_matthiesen]] "),
            ".md suffix should be stripped from link expansion"
        );
    }

    #[test]
    fn notes_link_complete_preserves_non_md_extensions() {
        // Non-`.md` extensions (e.g. `.org`,
        // `.txt`) are left intact since the
        // user might have indexed
        // non-markdown notes and those
        // names are the actual reference
        // targets.
        let (_dir, db) = make_notes_db_with_tags_and_links(&[], &["todo_list.org"]);
        assert_eq!(
            notes_link_complete(&db, "todo").as_deref(),
            Some("[[todo_list.org]] "),
            "non-.md extensions should be preserved"
        );
    }

    #[test]
    fn notes_link_complete_handles_link_names_with_spaces() {
        // Link names with spaces are
        // wrapped in `[[...]]` brackets
        // which unambiguously delimit
        // the link target. The brackets
        // already serve as a delimiter,
        // so no additional quoting is
        // needed: `[[my note]]` is
        // already a single link
        // reference as far as the
        // `note_search` tokenizer is
        // concerned.
        let (_dir, db) = make_notes_db_with_tags_and_links(&[], &["my note.md"]);
        assert_eq!(
            notes_link_complete(&db, "my").as_deref(),
            Some("[[my note]] "),
            "link with space should be wrapped in [[...]] without additional quotes"
        );
    }

    /// Link targets in Obsidian
    /// are case-insensitive. When
    /// the database contains
    /// multiple casings of the
    /// same link (e.g. `Project.md`
    /// and `project.md`), the
    /// completion should
    /// deduplicate by lowercased
    /// form and return just the
    /// lowercase version. This
    /// prevents the menu from
    /// opening for the trivial
    /// case where matches only
    /// differ by case.
    #[test]
    fn notes_link_matches_deduplicates_by_case() {
        let (_dir, db) =
            make_notes_db_with_tags_and_links(&[], &["Project.md", "project.md", "PROJECT.md"]);
        let matches = notes_link_matches(&db, "proj");
        // All three casings
        // collapse to one
        // lowercase match.
        assert_eq!(matches, vec!["project".to_string()]);
    }

    /// `notes_link_complete` also
    /// returns the lowercase
    /// version when the database
    /// has multiple casings of
    /// the same link.
    #[test]
    fn notes_link_complete_lowercases_duplicate_casings() {
        let (_dir, db) = make_notes_db_with_tags_and_links(&[], &["Project.md", "project.md"]);
        assert_eq!(
            notes_link_complete(&db, "proj").as_deref(),
            Some("[[project]] "),
            "duplicate casings should collapse to lowercase"
        );
    }

    /// Genuinely different links
    /// (not just case variants)
    /// still trigger the menu.
    /// `Project` and `project`
    /// collapse to one match, but
    /// `Project` and `Projects` are
    /// different and should both
    /// appear.
    #[test]
    fn notes_link_matches_keeps_genuinely_different_links() {
        let (_dir, db) =
            make_notes_db_with_tags_and_links(&[], &["Project.md", "project.md", "Projects.md"]);
        let matches = notes_link_matches(&db, "proj");
        // `Project` and
        // `project` collapse to
        // one (lowercase
        // `project`), but
        // `Projects` is genuinely
        // different (lowercase
        // `projects`). So we get
        // two matches.
        assert_eq!(matches.len(), 2);
        assert!(matches.contains(&"project".to_string()));
        assert!(matches.contains(&"projects".to_string()));
    }

    // --- Time tracking: extract_issue_key / labels_for_issue ------------

    #[test]
    fn extract_issue_key_finds_key_in_browse_url() {
        assert_eq!(
            extract_issue_key("https://jira.example.com/browse/PROJ-123"),
            Some("PROJ-123")
        );
    }

    #[test]
    fn extract_issue_key_finds_key_in_shell_command() {
        assert_eq!(
            extract_issue_key(r#"open "https://jira.example.com/browse/AB-42""#),
            Some("AB-42")
        );
    }

    #[test]
    fn extract_issue_key_returns_none_for_plain_text() {
        assert_eq!(extract_issue_key("just some free text, no ticket here"), None);
    }

    #[test]
    fn extract_issue_key_returns_none_for_lowercase_key_like_text() {
        // The anchored `key_re` in `build_jql` is also
        // case-sensitive (JIRA keys are always uppercase); this
        // confirms `extract_issue_key` shares that constraint rather
        // than loosely matching a lowercase look-alike.
        assert_eq!(extract_issue_key("proj-123"), None);
    }

    #[test]
    fn extract_issue_key_requires_a_digit_suffix() {
        assert_eq!(extract_issue_key("PROJ-"), None);
        assert_eq!(extract_issue_key("PROJ-abc"), None);
    }

    #[test]
    fn extract_issue_key_finds_first_match_when_multiple() {
        assert_eq!(extract_issue_key("PROJ-1 and PROJ-2"), Some("PROJ-1"));
    }

    /// A `JiraClient` fake for `labels_for_issue`'s cache tests.
    /// Records how many times `search` was actually called, so the
    /// cache-hit test can assert the REST round-trip is skipped on
    /// the second lookup.
    struct FakeLabelClient {
        labels: Vec<String>,
        calls: std::sync::atomic::AtomicU32,
    }

    impl JiraClient for FakeLabelClient {
        fn search(&self, _jql: &str) -> Result<Vec<JiraIssue>, JiraError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![JiraIssue {
                key: "PROJ-1".to_string(),
                labels: self.labels.clone(),
                ..Default::default()
            }])
        }
        fn fetch_comments(&self, _key: &str) -> Result<Vec<JiraComment>, JiraError> {
            Ok(Vec::new())
        }
        fn add_comment(&self, _key: &str, _body: &str) -> Result<(), JiraError> {
            Ok(())
        }
        fn create_issue(
            &self,
            _project: &str,
            _issuetype: &str,
            _summary: &str,
            _description: &str,
            _labels: &[String],
            _custom_fields: &[(String, String)],
        ) -> Result<String, JiraError> {
            Ok("PROJ-1".to_string())
        }
        fn link_issues(
            &self,
            _inward_key: &str,
            _outward_key: &str,
            _link_type: &str,
        ) -> Result<(), JiraError> {
            Ok(())
        }
        fn fetch_custom_fields(
            &self,
            _key: &str,
            _field_ids: &[String],
        ) -> Result<Vec<(String, String)>, JiraError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn labels_for_issue_fetches_and_caches_on_miss() {
        let client = FakeLabelClient {
            labels: vec!["acme".to_string(), "urgent".to_string()],
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let mut cache = std::collections::HashMap::new();
        let labels = labels_for_issue(&client, "PROJ-1", &mut cache);
        assert_eq!(labels, vec!["acme".to_string(), "urgent".to_string()]);
        assert_eq!(client.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(cache.contains_key("PROJ-1"));
    }

    #[test]
    fn labels_for_issue_reuses_cache_without_a_second_search() {
        let client = FakeLabelClient {
            labels: vec!["acme".to_string()],
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let mut cache = std::collections::HashMap::new();
        let _ = labels_for_issue(&client, "PROJ-1", &mut cache);
        let labels = labels_for_issue(&client, "PROJ-1", &mut cache);
        assert_eq!(labels, vec!["acme".to_string()]);
        assert_eq!(client.calls.load(std::sync::atomic::Ordering::SeqCst), 1, "second lookup must hit the cache, not the client");
    }

    /// A search error (network failure, issue not found, etc.)
    /// degrades to "no labels" rather than propagating — a label
    /// lookup failure falls through to the next resolution tier, it
    /// doesn't hard-fail the whole report/mode.
    struct FailingClient;

    impl JiraClient for FailingClient {
        fn search(&self, _jql: &str) -> Result<Vec<JiraIssue>, JiraError> {
            Err(JiraError::Http("boom".to_string()))
        }
        fn fetch_comments(&self, _key: &str) -> Result<Vec<JiraComment>, JiraError> {
            Ok(Vec::new())
        }
        fn add_comment(&self, _key: &str, _body: &str) -> Result<(), JiraError> {
            Ok(())
        }
        fn create_issue(
            &self,
            _project: &str,
            _issuetype: &str,
            _summary: &str,
            _description: &str,
            _labels: &[String],
            _custom_fields: &[(String, String)],
        ) -> Result<String, JiraError> {
            Err(JiraError::Http("boom".to_string()))
        }
        fn link_issues(
            &self,
            _inward_key: &str,
            _outward_key: &str,
            _link_type: &str,
        ) -> Result<(), JiraError> {
            Err(JiraError::Http("boom".to_string()))
        }
        fn fetch_custom_fields(
            &self,
            _key: &str,
            _field_ids: &[String],
        ) -> Result<Vec<(String, String)>, JiraError> {
            Err(JiraError::Http("boom".to_string()))
        }
    }

    #[test]
    fn labels_for_issue_degrades_to_empty_on_search_error() {
        let mut cache = std::collections::HashMap::new();
        let labels = labels_for_issue(&FailingClient, "PROJ-1", &mut cache);
        assert!(labels.is_empty());
        // The empty result is still cached — a failing lookup
        // shouldn't retry the network on every subsequent reference
        // to the same key within one report/session.
        assert_eq!(cache.get("PROJ-1"), Some(&Vec::new()));
    }

    /// `shared_label_cache_snapshot`/`merge_shared_label_cache`: the
    /// process-wide cache `build_day_report` seeds/refills its local
    /// `label_cache` from on every `smarthistory serve` request, so a
    /// day's issue-label lookups only hit JIRA once across the
    /// server's whole lifetime (not once per HTTP request). Uses a
    /// key unlikely to collide with any other test sharing this
    /// process-global cache.
    #[test]
    fn shared_label_cache_round_trips_across_snapshot_and_merge() {
        let key = "SHARED-CACHE-TEST-1";
        let mut local = shared_label_cache_snapshot();
        assert!(
            !local.contains_key(key),
            "test key must not already be present from a prior run"
        );
        local.insert(key.to_string(), vec!["from-merge".to_string()]);
        merge_shared_label_cache(&local);

        let refreshed = shared_label_cache_snapshot();
        assert_eq!(refreshed.get(key), Some(&vec!["from-merge".to_string()]));
    }

    /// `merge_shared_label_cache` must never clobber an existing
    /// shared entry with a stale one from a call that started before
    /// some other call already refreshed it — first-write-wins per
    /// key, matching `labels_for_issue`'s own "insert on miss" idiom.
    #[test]
    fn shared_label_cache_merge_does_not_overwrite_existing_entry() {
        let key = "SHARED-CACHE-TEST-2";
        let mut first = std::collections::HashMap::new();
        first.insert(key.to_string(), vec!["original".to_string()]);
        merge_shared_label_cache(&first);

        let mut stale = std::collections::HashMap::new();
        stale.insert(key.to_string(), vec!["stale".to_string()]);
        merge_shared_label_cache(&stale);

        let snapshot = shared_label_cache_snapshot();
        assert_eq!(snapshot.get(key), Some(&vec!["original".to_string()]));
    }
