    use super::*;

    #[test]
    fn browser_kind_parse_accepts_known_names_case_insensitively() {
        assert_eq!(BrowserKind::parse("chrome"), Some(BrowserKind::Chrome));
        assert_eq!(BrowserKind::parse("Chrome"), Some(BrowserKind::Chrome));
        assert_eq!(BrowserKind::parse("FIREFOX"), Some(BrowserKind::Firefox));
        assert_eq!(BrowserKind::parse("Safari"), Some(BrowserKind::Safari));
        assert_eq!(BrowserKind::parse("SAFARI"), Some(BrowserKind::Safari));
    }

    #[test]
    fn browser_kind_parse_rejects_garbage() {
        assert_eq!(BrowserKind::parse("edge"), None);
        assert_eq!(BrowserKind::parse(""), None);
    }

    #[test]
    fn webkit_epoch_conversion_matches_known_value() {
        // 2024-01-15T10:30:00Z in WebKit micros, cross-checked
        // against the same instant used in
        // `paperless::tests::parse_iso8601_epoch_parses_rfc3339`
        // (1705314600 unix seconds).
        let unix_secs = 1705314600i64;
        let webkit_micros = (unix_secs + WEBKIT_EPOCH_OFFSET_SECS) * 1_000_000;
        assert_eq!(webkit_micros_to_unix_secs(webkit_micros), unix_secs);
    }

    #[test]
    fn webkit_epoch_conversion_zero_or_negative_is_zero() {
        assert_eq!(webkit_micros_to_unix_secs(0), 0);
        assert_eq!(webkit_micros_to_unix_secs(-5), 0);
    }

    #[test]
    fn firefox_epoch_conversion_is_plain_microseconds() {
        let unix_secs = 1705314600i64;
        assert_eq!(firefox_micros_to_unix_secs(unix_secs * 1_000_000), unix_secs);
        assert_eq!(firefox_micros_to_unix_secs(0), 0);
    }

    #[test]
    fn browser_entry_to_row_uses_tag_prefixed_command() {
        let entry = BrowserEntry {
            tag: "bookmark",
            title: "Rust Lang".to_string(),
            url: "https://rust-lang.org".to_string(),
            timestamp: 1705314600,
            kind: BrowserKind::Chrome,
        };
        let mut next_id = -1i64;
        let row = browser_entry_to_row(entry, &mut next_id);
        assert_eq!(row.command, "bookmark Rust Lang");
        assert_eq!(row.comment, "https://rust-lang.org");
        assert_eq!(row.directory, "chrome");
        assert_eq!(row.mode, "browser");
        assert_eq!(row.id, -1);
    }

    #[test]
    fn browser_entry_to_row_falls_back_to_url_when_title_empty() {
        let entry = BrowserEntry {
            tag: "history",
            title: String::new(),
            url: "https://example.com/page".to_string(),
            timestamp: 0,
            kind: BrowserKind::Firefox,
        };
        let mut next_id = -1i64;
        let row = browser_entry_to_row(entry, &mut next_id);
        assert_eq!(row.command, "history https://example.com/page");
    }

    #[test]
    fn browser_entry_to_row_ids_decrement() {
        let mut next_id = -1i64;
        let a = browser_entry_to_row(
            BrowserEntry {
                tag: "history",
                title: "A".to_string(),
                url: "https://a.example".to_string(),
                timestamp: 1,
                kind: BrowserKind::Chrome,
            },
            &mut next_id,
        );
        let b = browser_entry_to_row(
            BrowserEntry {
                tag: "history",
                title: "B".to_string(),
                url: "https://b.example".to_string(),
                timestamp: 2,
                kind: BrowserKind::Chrome,
            },
            &mut next_id,
        );
        assert_eq!(a.id, -1);
        assert_eq!(b.id, -2);
    }

    #[test]
    fn walk_chrome_bookmark_node_recurses_into_folders() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "type": "folder",
                "name": "Bookmarks bar",
                "children": [
                    {
                        "type": "url",
                        "name": "Example",
                        "url": "https://example.com",
                        "date_added": "13350000000000000"
                    },
                    {
                        "type": "folder",
                        "name": "Nested",
                        "children": [
                            {
                                "type": "url",
                                "name": "Nested link",
                                "url": "https://nested.example",
                                "date_added": "0"
                            }
                        ]
                    }
                ]
            }"#,
        )
        .unwrap();
        let mut out = Vec::new();
        walk_chrome_bookmark_node(&json, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|e| e.url == "https://example.com"));
        assert!(out.iter().any(|e| e.url == "https://nested.example"));
        assert!(out.iter().all(|e| e.tag == "bookmark"));
    }

    #[test]
    fn walk_chrome_bookmark_node_skips_urlless_entries() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"type": "url", "name": "Broken", "date_added": "0"}"#,
        )
        .unwrap();
        let mut out = Vec::new();
        walk_chrome_bookmark_node(&json, &mut out);
        assert!(out.is_empty());
    }

    fn safari_leaf(url: &str, title: &str) -> plist::Value {
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "WebBookmarkType".to_string(),
            plist::Value::String("WebBookmarkTypeLeaf".to_string()),
        );
        dict.insert("URLString".to_string(), plist::Value::String(url.to_string()));
        let mut uri_dict = plist::Dictionary::new();
        uri_dict.insert("title".to_string(), plist::Value::String(title.to_string()));
        dict.insert("URIDictionary".to_string(), plist::Value::Dictionary(uri_dict));
        plist::Value::Dictionary(dict)
    }

    fn safari_folder(children: Vec<plist::Value>) -> plist::Value {
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "WebBookmarkType".to_string(),
            plist::Value::String("WebBookmarkTypeList".to_string()),
        );
        dict.insert("Children".to_string(), plist::Value::Array(children));
        plist::Value::Dictionary(dict)
    }

    #[test]
    fn walk_safari_bookmark_node_recurses_into_folders() {
        let root = safari_folder(vec![
            safari_leaf("https://example.com", "Example"),
            safari_folder(vec![safari_leaf("https://nested.example", "Nested link")]),
        ]);
        let mut out = Vec::new();
        walk_safari_bookmark_node(&root, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|e| e.url == "https://example.com" && e.title == "Example"));
        assert!(out.iter().any(|e| e.url == "https://nested.example"));
        assert!(out.iter().all(|e| e.tag == "bookmark" && e.kind == BrowserKind::Safari));
    }

    #[test]
    fn walk_safari_bookmark_node_skips_urlless_leaf() {
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "WebBookmarkType".to_string(),
            plist::Value::String("WebBookmarkTypeLeaf".to_string()),
        );
        let leaf = plist::Value::Dictionary(dict);
        let mut out = Vec::new();
        walk_safari_bookmark_node(&leaf, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn mac_absolute_time_conversion_matches_known_value() {
        let unix_secs = 1705314600i64;
        let mac_time = (unix_secs - MAC_ABSOLUTE_EPOCH_OFFSET_SECS) as f64;
        assert_eq!(mac_absolute_time_to_unix_secs(mac_time), unix_secs);
    }

    #[test]
    fn mac_absolute_time_conversion_zero_or_negative_is_zero() {
        assert_eq!(mac_absolute_time_to_unix_secs(0.0), 0);
        assert_eq!(mac_absolute_time_to_unix_secs(-1.0), 0);
    }

    #[test]
    fn primary_file_matches_each_kind() {
        let profile = PathBuf::from("/tmp/some-profile");
        assert_eq!(
            BrowserSource { kind: BrowserKind::Chrome, profile: profile.clone() }.primary_file(),
            profile.join("Bookmarks")
        );
        assert_eq!(
            BrowserSource { kind: BrowserKind::Firefox, profile: profile.clone() }.primary_file(),
            profile.join("places.sqlite")
        );
        assert_eq!(
            BrowserSource { kind: BrowserKind::Safari, profile: profile.clone() }.primary_file(),
            profile.join("Bookmarks.plist")
        );
    }

    #[test]
    fn autodetect_skips_missing_profiles() {
        // On a machine (or CI sandbox) without Chrome / Firefox
        // installed at the default location, autodetect must
        // return an empty list rather than a source pointing at a
        // nonexistent directory (every downstream read already
        // handles a missing file gracefully, but there's no reason
        // to carry a dead source around).
        let sources = BrowserSource::autodetect();
        for s in &sources {
            assert!(
                s.profile.is_dir(),
                "autodetect returned a source with a nonexistent profile dir: {:?}",
                s
            );
        }
    }

    #[test]
    fn current_pattern_strips_prefix_and_trims() {
        assert_eq!(BrowserState::current_pattern("^bookmark rust", '^'), "bookmark rust");
        assert_eq!(BrowserState::current_pattern("^  ", '^'), "");
        assert_eq!(BrowserState::current_pattern("plain", '^'), "plain");
    }

    #[test]
    fn spawn_fetch_with_no_sources_yields_empty_rows() {
        let request = spawn_fetch(Vec::new(), String::new());
        let rows = request.receiver.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(rows.is_empty());
    }
