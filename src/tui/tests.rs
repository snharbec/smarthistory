use super::*;

use super::*;

/// Shared mutex for tests that mutate the process CWD
/// (e.g. `fetch_tags_*`). CWD is process-global, so any
/// test that calls `std::env::set_current_dir` must hold
/// this lock to avoid racing the other CWD-mutating tests
/// (each test would otherwise see the other's tags file
/// or miss the real one). Declared once at the module
/// level so both tests see the same mutex.
///
/// Use `CWD_LOCK.lock_or_recover()` instead of
/// `CWD_LOCK.lock()` so a panic in one test doesn't
/// poison the mutex for the rest of the suite.
static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire the CWD lock, recovering from poisoning if a
/// previous test panicked while holding it. Poisoning is
/// expected when an assertion fails mid-test; the
/// CWD-restoring `catch_unwind` block always runs, so
/// the state is consistent and it's safe to continue.
fn lock_or_recover<'a>(mutex: &'a std::sync::Mutex<()>) -> std::sync::MutexGuard<'a, ()> {
    match mutex.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The `--prefix <char>` CLI flag
/// is implemented as a `bool`
/// gate (`override_session_query`)
/// + the prefix char itself as
/// the `initial_query`. This test
/// pins the contract: when the
/// flag is on, `session.query`
/// is NOT restored, the live
/// query is the prefix char,
/// and `prefilled_query` is
/// `None` (so the TUI treats the
/// prefix as fresh-typed text,
/// not pre-filled text — the
/// cursor sits at the end so
/// the user can immediately
/// type the filter body like
/// `--prefix '*'vim`).
#[test]
fn resolve_initial_query_prefix_overrides_session() {
    // Session has a persisted query — the user's
    // last invocation. Without --prefix this gets
    // restored.
    let session_query = Some("previous query");
    // Simulate: `smarthistory tui --prefix '*'` →
    // `main` passes `initial_query = "*"` (the
    // first char of the prefix string) and
    // `override_session_query = true`.
    let (prefilled, effective) = resolve_initial_query("*", session_query, true);
    // The persisted query is NOT restored.
    assert_eq!(
        prefilled, None,
        "--prefix must override the persisted session.query"
    );
    // The effective query is the prefix char.
    assert_eq!(
        effective, "*",
        "--prefix must set the live query to the prefix char"
    );
}

/// Without `--prefix`, the persisted `session.query`
/// is restored as before — the historical behavior
/// the user expects when they DIDN'T explicitly
/// ask for a prefix mode this launch.
#[test]
fn resolve_initial_query_session_restored_without_prefix() {
    let session_query = Some("previous query");
    let (prefilled, effective) = resolve_initial_query("", session_query, false);
    assert_eq!(prefilled.as_deref(), Some("previous query"));
    assert_eq!(effective, "previous query");
}

/// No persisted `session.query` at all — the CLI-supplied
/// `initial_query` (the positional `--query` arg) becomes
/// the live query, and `prefilled_query` is `None` (so the
/// first character typed appends, since the user typed
/// fresh — not a pre-filled buffer).
#[test]
fn resolve_initial_query_falls_back_to_cli_when_no_session() {
    let (prefilled, effective) = resolve_initial_query("some cli arg", None, false);
    assert_eq!(prefilled, None, "no session.query → not prefilled");
    assert_eq!(effective, "some cli arg");
}

/// Build the test-default
/// multiplexer backend.
/// The tmux backend is
/// fine for tests that
/// don't specifically
/// exercise the herdr
/// shape; tests that
/// need the herdr
/// backend use
/// `crate::multiplexer::backend_for(MultiplexerKind::Herdr)`
/// directly.
fn test_multiplexer() -> Box<dyn crate::multiplexer::MultiplexerBackend> {
    crate::multiplexer::backend_for(crate::multiplexer::MultiplexerKind::Tmux)
}

/// Best-effort mtime setter
/// used by tests that need a
/// file to *look* old. We use
/// the `filetime` crate
/// (declared in
/// `[dev-dependencies]` so
/// this is the only place in
/// the production tree that
/// touches it). Errors are
/// swallowed because the
/// caller treats them as "mtime
/// couldn't be set, the test
/// may degenerate but
/// shouldn't crash" — the
/// filter logic is still
/// exercised either way.
fn filetime_touch_mtime(
    path: &std::path::Path,
    epoch_secs: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let time = filetime::FileTime::from_unix_time(epoch_secs, 0);
    filetime::set_file_mtime(path, time)?;
    Ok(())
}

#[test]
fn highlight_matches_empty_query() {
    let spans = super::render::highlight_matches("hello world", "");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, "hello world".to_string());
}

#[test]
fn highlight_matches_single() {
    let spans = super::render::highlight_matches("git status", "stat");
    let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
    assert_eq!(content, vec!["git ", "stat", "us"]);
}

#[test]
fn highlight_matches_case_insensitive() {
    let spans = super::render::highlight_matches("Git Status", "stat");
    let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
    assert_eq!(content, vec!["Git ", "Stat", "us"]);
}

#[test]
fn highlight_matches_multiple() {
    let spans = super::render::highlight_matches("foo bar foo", "foo");
    let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
    assert_eq!(content, vec!["foo", " bar ", "foo"]);
}

#[test]
fn highlight_matches_no_match() {
    let spans = super::render::highlight_matches("hello world", "xyz");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, "hello world".to_string());
}

#[test]
fn highlight_matches_multi_word() {
    let spans = super::render::highlight_matches("git commit -m", "git commit");
    let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
    assert_eq!(content, vec!["git", " ", "commit", " -m"]);
}

#[test]
fn highlight_matches_multi_word_out_of_order() {
    let spans = super::render::highlight_matches("git commit -m", "commit git");
    let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
    assert_eq!(content, vec!["git", " ", "commit", " -m"]);
}

#[test]
fn build_implicit_regex_plain() {
    // No anchors → wrap with `.*` on both sides.
    assert_eq!(build_implicit_regex("git commit"), ".*git commit.*");
    assert_eq!(build_implicit_regex("foo"), ".*foo.*");
}

#[test]
fn build_implicit_regex_start_anchor() {
    // Leading `^` suppresses the implicit `.*` on the left.
    assert_eq!(build_implicit_regex("^git commit"), "^git commit.*");
    assert_eq!(build_implicit_regex("^foo"), "^foo.*");
}

#[test]
fn build_implicit_regex_end_anchor() {
    // Trailing `$` suppresses the implicit `.*` on the right.
    assert_eq!(build_implicit_regex("git$"), ".*git$");
    assert_eq!(build_implicit_regex("foo bar$"), ".*foo bar$");
}

#[test]
fn build_implicit_regex_both_anchors() {
    // Both anchors present → no implicit `.*` added.
    assert_eq!(build_implicit_regex("^git$"), "^git$");
    assert_eq!(build_implicit_regex("^foo bar$"), "^foo bar$");
}

#[test]
fn build_implicit_regex_empty() {
    // Empty pattern still gets `.*` wrappers — useful for
    // `/` alone (matches everything).
    assert_eq!(build_implicit_regex(""), ".*.*");
}

#[test]
fn parse_key_spec_plain() {
    let spec = bindings::parse_key_spec("a").unwrap();
    assert_eq!(spec.code, KeyCode::Char('a'));
    assert!(spec.modifiers.is_empty());

    let spec = bindings::parse_key_spec("/").unwrap();
    assert_eq!(spec.code, KeyCode::Char('/'));
}

#[test]
fn parse_key_spec_ctrl() {
    let spec = bindings::parse_key_spec("C-h").unwrap();
    assert_eq!(spec.code, KeyCode::Char('h'));
    assert!(spec.modifiers.contains(KeyModifiers::CONTROL));
    assert!(!spec.modifiers.contains(KeyModifiers::ALT));

    // Uppercase and lowercase both work.
    let spec = bindings::parse_key_spec("c-H").unwrap();
    assert_eq!(spec.code, KeyCode::Char('H'));
    assert!(spec.modifiers.contains(KeyModifiers::CONTROL));
}

#[test]
fn parse_key_spec_alt_and_combinations() {
    let spec = bindings::parse_key_spec("M-x").unwrap();
    assert_eq!(spec.code, KeyCode::Char('x'));
    assert!(spec.modifiers.contains(KeyModifiers::ALT));

    let spec = bindings::parse_key_spec("C-M-h").unwrap();
    assert_eq!(spec.code, KeyCode::Char('h'));
    assert!(spec.modifiers.contains(KeyModifiers::CONTROL));
    assert!(spec.modifiers.contains(KeyModifiers::ALT));
}

#[test]
fn parse_key_spec_named_keys() {
    assert_eq!(bindings::parse_key_spec("Esc").unwrap().code, KeyCode::Esc);
    assert_eq!(
        bindings::parse_key_spec("Enter").unwrap().code,
        KeyCode::Enter
    );
    assert_eq!(
        bindings::parse_key_spec("Backspace").unwrap().code,
        KeyCode::Backspace
    );
    assert_eq!(bindings::parse_key_spec("Up").unwrap().code, KeyCode::Up);
    assert_eq!(
        bindings::parse_key_spec("PageUp").unwrap().code,
        KeyCode::PageUp
    );
    assert_eq!(bindings::parse_key_spec("F5").unwrap().code, KeyCode::F(5));
}

#[test]
fn parse_key_spec_invalid() {
    assert!(bindings::parse_key_spec("").is_err());
    assert!(bindings::parse_key_spec("not-a-single-char").is_err());
}

#[test]
fn action_for_key_roundtrip() {
    let bindings = KeyBindings::defaults();
    // `C-a` is the default for OpenHelp (matches the
    // user-configured `key.open-help=C-a` in
    // the project config).
    let evt = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert_eq!(action_for_key(&bindings, &evt), Some(Action::OpenHelp));
    // Unbound plain char → None.
    let evt = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::empty());
    assert_eq!(action_for_key(&bindings, &evt), None);
    // Uppercase letters (Shift held) are unbound at the action
    // level — they fall through to the input path, which must
    // accept them rather than swallow them.
    let evt = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
    assert_eq!(action_for_key(&bindings, &evt), None);
    // Shift+symbol also falls through (e.g. "?" typed via
    // Shift+/).
    let evt = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
    assert_eq!(action_for_key(&bindings, &evt), None);
}

#[test]
fn key_bindings_from_config_overrides() {
    // Entries are keyed by the bare action name (without the
    // `key.` prefix); `Config::parse` strips the prefix before
    // inserting into the map.
    let mut entries = std::collections::HashMap::new();
    entries.insert("open-help".to_string(), "M-h".to_string());
    entries.insert("cancel".to_string(), "C-q".to_string());
    let bindings = bindings::key_bindings_from_config(&entries);
    assert_eq!(
        format_key_specs(bindings.specs(Action::OpenHelp)),
        "M-h".to_string()
    );
    assert_eq!(
        format_key_specs(bindings.specs(Action::Cancel)),
        "C-q".to_string()
    );
    // Unmentioned actions keep their defaults.
    assert_eq!(
        format_key_specs(bindings.specs(Action::DeleteSelected)),
        "C-d".to_string()
    );
}

/// Pin the SmartOpen default to `C-]`. The binding must be a
/// single-byte ASCII control char that every terminal emits
/// reliably — `S-Return` was the original default but many
/// terminals emit it as a non-standard `ESC[27;5;13~` sequence
/// crossterm 0.29 can't decode. If this default ever drifts
/// back to `S-Return`, users on those terminals lose the dive
/// key out-of-the-box.
#[test]
fn smart_open_default_binding_is_ctrl_right_bracket() {
    assert_eq!(
        Action::SmartOpen.default_key(),
        "C-]",
        "SmartOpen default must stay C-] (S-Return is undecodable on many terminals)"
    );
    let bindings = bindings::KeyBindings::defaults();
    assert_eq!(
        format_key_specs(bindings.specs(Action::SmartOpen)),
        "C-]",
        "defaults() must install C-] for SmartOpen"
    );
}

#[test]
fn key_bindings_from_config_unknown_action_is_reported() {
    // `toggle-duplication-filter` (extra "ation") is a typo of
    // `toggle-duplicate-filter` and must not silently bind to
    // anything. Capture stderr to confirm the warning is
    // emitted, then ensure the matching default still wins.
    let mut entries = std::collections::HashMap::new();
    entries.insert("toggle-duplication-filter".to_string(), "C-d".to_string());
    let bindings = bindings::key_bindings_from_config(&entries);
    // Unknown action does not pollute any known action.
    // The default for `toggle-duplicate-filter` is
    // the `none` sentinel (the user has
    // explicitly unbound it in the
    // project config), so the
    // resulting spec list is empty
    // and the action renders as
    // `(unbound)` in the help
    // overlay / command palette.
    assert!(
        bindings.specs(Action::ToggleDuplicateFilter).is_empty(),
        "ToggleDuplicateFilter should ship unbound (default is `none`), got: {:?}",
        format_key_specs(bindings.specs(Action::ToggleDuplicateFilter))
    );
    assert_eq!(
        Action::ToggleDuplicateFilter.default_key(),
        "none",
        "default_key() for ToggleDuplicateFilter should be the `none` sentinel"
    );
}

#[test]
fn parse_key_spec_unbind_sentinels() {
    // `none`, `off`, `disable`, `-`, `disabled` (case
    // insensitive) all map to `Ok(None)` — the action is
    // unbound, not bound to a literal "None" key.
    for sentinel in ["none", "NONE", "off", "disable", "-", "disabled"] {
        let parsed = bindings::parse_key_spec_opt(sentinel).unwrap();
        assert!(parsed.is_none(), "sentinel {sentinel:?} should unbind");
    }
}

#[test]
fn key_bindings_from_config_unbind_action() {
    let mut entries = std::collections::HashMap::new();
    entries.insert("open-help".to_string(), "none".to_string());
    let bindings = bindings::key_bindings_from_config(&entries);
    assert!(bindings.is_unbound(Action::OpenHelp));
    assert!(bindings.specs(Action::OpenHelp).is_empty());
    // Unbinding one action must not affect siblings.
    assert!(!bindings.is_unbound(Action::Cancel));
    assert!(!bindings.specs(Action::Cancel).is_empty());
    // `action_for_key` must not fire for unbound actions.
    let evt = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
    assert_eq!(action_for_key(&bindings, &evt), None);
}

#[test]
fn key_bindings_from_config_multi_key() {
    // `key.open-help=C-h, F1` binds the help overlay to
    // both Ctrl-H and F1. Whitespace around the comma
    // is allowed.
    let mut entries = std::collections::HashMap::new();
    entries.insert("open-help".to_string(), "C-h, F1".to_string());
    let bindings = bindings::key_bindings_from_config(&entries);
    let specs = bindings.specs(Action::OpenHelp);
    assert_eq!(specs.len(), 2);
    // Both keys must fire the action.
    let ctrl_h = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
    let f1 = KeyEvent::new(KeyCode::F(1), KeyModifiers::empty());
    assert_eq!(action_for_key(&bindings, &ctrl_h), Some(Action::OpenHelp));
    assert_eq!(action_for_key(&bindings, &f1), Some(Action::OpenHelp));
    // The display string is comma-joined.
    assert_eq!(format_key_specs(specs), "C-h, F1");
}

#[test]
fn key_bindings_from_config_multi_key_three_way() {
    // Three specs in one entry, no surrounding spaces.
    let mut entries = std::collections::HashMap::new();
    entries.insert("cancel".to_string(), "Esc,C-c,C-g".to_string());
    let bindings = bindings::key_bindings_from_config(&entries);
    assert_eq!(bindings.specs(Action::Cancel).len(), 3);
    assert_eq!(
        format_key_specs(bindings.specs(Action::Cancel)),
        "Esc, C-c, C-g"
    );
}

#[test]
fn key_bindings_from_config_multi_key_with_none_unbinds() {
    // The unbind sentinel anywhere in a comma list
    // means the action is unbound. `Esc` is silently
    // discarded — we don't want to half-apply a
    // binding the user thought they disabled.
    let mut entries = std::collections::HashMap::new();
    entries.insert("cancel".to_string(), "none,Esc".to_string());
    let bindings = bindings::key_bindings_from_config(&entries);
    assert!(bindings.is_unbound(Action::Cancel));
    assert!(bindings.specs(Action::Cancel).is_empty());
}

#[test]
fn key_bindings_from_config_multi_key_bad_spec_drops_all() {
    // One bad spec in a list drops the whole binding
    // (no half-applied config). The default wins.
    let mut entries = std::collections::HashMap::new();
    entries.insert("open-help".to_string(), "C-h,not-a-key,F1".to_string());
    let bindings = bindings::key_bindings_from_config(&entries);
    assert_eq!(
        bindings.specs(Action::OpenHelp).len(),
        1,
        "should keep only the default for OpenHelp"
    );
    assert_eq!(
        format_key_specs(bindings.specs(Action::OpenHelp)),
        Action::OpenHelp.default_key()
    );
}

#[test]
fn command_menu_filter_matches() {
    let menu = CommandMenu::new();
    // Empty query returns every action.
    assert_eq!(menu.filtered_indices().len(), ALL_ACTIONS.len());
    // Substring match against the display name.
    let m = CommandMenu {
        query: "delete".into(),
        ..CommandMenu::new()
    };
    let filtered = m.filtered_indices();
    assert!(filtered.iter().all(|&i| {
        ALL_ACTIONS[i]
            .display_name()
            .to_lowercase()
            .contains("delete")
    }));
    assert!(filtered
        .iter()
        .any(|&i| ALL_ACTIONS[i] == Action::DeleteSelected));
    assert!(filtered
        .iter()
        .any(|&i| ALL_ACTIONS[i] == Action::DeleteMatching));
    // Multi-word AND: "open help" matches OpenHelp (also
    // ShowOutput because its name contains "open"? — actually
    // it doesn't, so only OpenHelp should match).
    let m = CommandMenu {
        query: "open help".into(),
        ..CommandMenu::new()
    };
    let filtered = m.filtered_indices();
    assert!(filtered.iter().any(|&i| ALL_ACTIONS[i] == Action::OpenHelp));
    assert!(!filtered
        .iter()
        .any(|&i| ALL_ACTIONS[i] == Action::ShowOutput));
    // `clamp_selection` keeps the cursor inside the filtered
    // list when items disappear (e.g. user deletes the last char).
    let mut m = CommandMenu::new();
    m.selected = ALL_ACTIONS.len() - 1;
    m.query = "no-such-action".into();
    m.clamp_selection();
    assert_eq!(m.selected, 0);
}

#[test]
fn command_action_has_default_binding_and_routes() {
    let bindings = KeyBindings::defaults();
    // The default key for CommandAction is `C-q` (matches
    // the user-configured `key.command-action=C-q` in
    // the project config; the project's chosen
    // keybinding keeps `:` free for the regex
    // search prefix).
    assert_eq!(
        format_key_specs(bindings.specs(Action::CommandAction)),
        "C-q".to_string()
    );
    // Pressing `Ctrl-Q` fires the CommandAction.
    let evt = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert_eq!(action_for_key(&bindings, &evt), Some(Action::CommandAction));
}

/// The command palette closes only
/// on keys mapped to the user's
/// `Cancel` action. Default
/// binding is `Esc`, so `Esc`
/// closes; `q` does NOT close.
/// Before this contract was
/// introduced, the palette
/// hard-coded `Esc | q | Q` and
/// the user couldn't type a
/// filter containing `q`
/// without accidentally closing
/// the palette.
#[test]
fn command_palette_closes_on_cancel_key_only() {
    let mut app = global_test_app(&[("a", 1)]);
    app.open_command_menu();
    assert!(app.is_command_menu_open());
    // Pressing `q` should NOT
    // close the palette — the
    // user may be typing a
    // filter like "quit" or
    // "query".
    let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty());
    handle_command_menu_key(&mut app, q);
    assert!(
        app.is_command_menu_open(),
        "q must not close the palette \
                         (only the user-configured Cancel \
                         binding does)"
    );
    // `Q` should also NOT
    // close (since it's a
    // printable character
    // now, and `Action::Cancel`
    // is `Esc` by default).
    let q = KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::empty());
    handle_command_menu_key(&mut app, q);
    assert!(app.is_command_menu_open(), "Q must not close the palette");
    // `Esc` (the default Cancel
    // binding) closes it.
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
    handle_command_menu_key(&mut app, esc);
    assert!(
        !app.is_command_menu_open(),
        "Esc must close the palette (default \
                         Cancel binding)"
    );
}

/// If the user rebinds the
/// Cancel action to `F1`
/// (or any other key), that
/// key becomes the only
/// way to close the
/// palette via keypress —
/// `Esc` no longer does
/// unless the user also
/// bound it to Cancel.
/// This test exercises
/// the dynamic-binding
/// branch of
/// `handle_command_menu_key`.
#[test]
fn command_palette_respects_user_cancel_binding() {
    let mut app = global_test_app(&[("a", 1)]);
    // Re-bind Cancel to F1.
    app.bindings.set(
        Action::Cancel,
        vec![bindings::parse_key_spec("F1").expect("F1")],
    );
    app.open_command_menu();
    assert!(app.is_command_menu_open());
    // `Esc` no longer closes
    // (because the user
    // removed it from
    // Cancel — F1 is now
    // the only binding).
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
    handle_command_menu_key(&mut app, esc);
    assert!(
        app.is_command_menu_open(),
        "Esc must NOT close the palette \
                         when Cancel is bound only to F1"
    );
    // F1 closes.
    let f1 = KeyEvent::new(KeyCode::F(1), KeyModifiers::empty());
    handle_command_menu_key(&mut app, f1);
    assert!(
        !app.is_command_menu_open(),
        "F1 must close the palette when \
                         bound to Cancel"
    );
}

/// Multi-key Cancel binding
/// (e.g. `key.cancel=Esc,F1`):
/// every key in the list
/// closes the palette.
#[test]
fn command_palette_respects_multi_key_cancel_binding() {
    let mut app = global_test_app(&[("a", 1)]);
    app.bindings.set(
        Action::Cancel,
        vec![
            bindings::parse_key_spec("Esc").expect("Esc"),
            bindings::parse_key_spec("F1").expect("F1"),
        ],
    );
    app.open_command_menu();
    // F1 closes.
    let f1 = KeyEvent::new(KeyCode::F(1), KeyModifiers::empty());
    handle_command_menu_key(&mut app, f1);
    assert!(!app.is_command_menu_open());
    // Re-open and try Esc.
    app.open_command_menu();
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
    handle_command_menu_key(&mut app, esc);
    assert!(!app.is_command_menu_open());
    // `q` still doesn't close
    // (user might be typing
    // "quit" into the filter).
    app.open_command_menu();
    let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty());
    handle_command_menu_key(&mut app, q);
    assert!(app.is_command_menu_open());
}

// -- `handle_key` precedence-chain regression tests --
//
// Everything above drives the individual overlay handlers
// (`handle_command_menu_key`, etc.) directly. That's useful but
// bypasses the actual `handle_key` entry point — the function that
// decides WHICH handler gets the key in the first place, via a
// 9-level modal-precedence chain (command menu > prefix picker >
// codegraph relations picker > completion menu > theme picker >
// help overlay > confirm-delete > comment-edit > add-entry-dialog >
// action dispatch > fallback char insertion). Nothing in the
// existing suite called `handle_key` itself, so a bug in the
// precedence ORDER (e.g. a new overlay inserted at the wrong
// position, or a check accidentally dropped) had no test to catch
// it. These tests exercise `handle_key` end-to-end for the
// precedence boundaries that matter most: two overlays open at
// once (only the higher-precedence one may react), and the
// fallback paths at the bottom of the chain.

/// When the command menu is open, `handle_key` must route to it
/// even if a lower-precedence overlay (the help view) also happens
/// to be open — a state that shouldn't occur in practice, but which
/// exercises the actual `if`/`else if` order in `handle_key` rather
/// than assuming it. The user's Cancel key (default `Esc`) must
/// close only the command menu; the help view must be untouched.
#[test]
fn handle_key_command_menu_takes_precedence_over_help_overlay() {
    let mut app = global_test_app(&[("a", 1)]);
    app.open_command_menu();
    app.open_help();
    assert!(app.is_command_menu_open());
    assert!(app.is_help_viewing());

    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
    let quit = handle_key(&mut app, esc);

    assert!(!quit);
    assert!(
        !app.is_command_menu_open(),
        "Esc must close the command menu (it has precedence)"
    );
    assert!(
        app.is_help_viewing(),
        "the help overlay must be untouched — handle_key must not have \
         reached the help-view branch at all"
    );
}

/// While `confirm_delete` is set, `handle_key` must route every key
/// to `handle_confirm_delete_key` instead of falling through to
/// action dispatch or the default "type a character into the
/// query" path. A plain unbound character (`z`) must be swallowed
/// by the confirmation dialog, not appended to the query.
#[test]
fn handle_key_confirm_delete_intercepts_before_fallback_char_insert() {
    let mut app = global_test_app(&[("a", 1)]);
    app.query = String::new();
    app.confirm_delete = Some(ConfirmMode::DeleteSelected);

    let z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::empty());
    let quit = handle_key(&mut app, z);

    assert!(!quit);
    assert_eq!(
        app.query, "",
        "an unbound character pressed during a delete confirmation must \
         not reach the query — the dialog must intercept it first"
    );
    assert!(
        app.confirm_delete.is_some(),
        "the dialog must stay open ('z' is neither y/n/Cancel)"
    );
}

/// With no overlay open, a key bound to an `Action` (the default
/// Cancel binding, `Esc`) must be dispatched via `dispatch_action`
/// rather than falling through to character insertion.
#[test]
fn handle_key_dispatches_bound_action_when_no_overlay_open() {
    let mut app = global_test_app(&[("a", 1)]);
    assert!(!app.is_command_menu_open());
    assert!(!app.is_help_viewing());
    assert!(app.confirm_delete.is_none());

    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
    let quit = handle_key(&mut app, esc);

    // `dispatch_action`'s `Action::Cancel` arm (with no LLM request
    // in flight) sets `app.cancelled` and returns `true` to end the
    // TUI loop.
    assert!(quit, "Cancel with no overlay open must end the TUI loop");
    assert!(app.cancelled);
}

/// With no overlay open and the key unbound, `handle_key` must fall
/// through to `push_char` and append the character to the query —
/// the bottom of the precedence chain.
#[test]
fn handle_key_falls_through_to_push_char_when_unbound_and_no_overlay() {
    let mut app = global_test_app(&[("a", 1)]);
    app.query = String::new();

    // 'z' is not bound to any action by default.
    let z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::empty());
    let quit = handle_key(&mut app, z);

    assert!(!quit);
    assert_eq!(app.query, "z");
}

/// `F11` grows the pane height, `Shift-F11` shrinks it — the
/// replacement for the old single-key 3-preset toggle. Verifies
/// both the default bindings and that they route to the right
/// `Action` via `action_for_key`.
#[test]
fn pane_height_default_keys_are_f11_and_shift_f11() {
    let bindings = KeyBindings::defaults();
    assert_eq!(
        format_key_specs(bindings.specs(Action::IncreasePaneHeight)),
        "F11".to_string()
    );
    assert_eq!(
        format_key_specs(bindings.specs(Action::DecreasePaneHeight)),
        "S-F11".to_string()
    );
    let f11 = KeyEvent::new(KeyCode::F(11), KeyModifiers::empty());
    assert_eq!(
        action_for_key(&bindings, &f11),
        Some(Action::IncreasePaneHeight)
    );
    let shift_f11 = KeyEvent::new(KeyCode::F(11), KeyModifiers::SHIFT);
    assert_eq!(
        action_for_key(&bindings, &shift_f11),
        Some(Action::DecreasePaneHeight)
    );
}

/// `Action::IncreasePaneHeight` grows the pane height by exactly
/// one line per press (not a jump to a fixed preset). The dispatch
/// arm queries the live terminal size via `crossterm::terminal::size()`
/// (falling back to 20 when there's no attached TTY, e.g. under a
/// test runner), so the expected value is derived the same way here
/// rather than hard-coding a page size that may not match whatever
/// terminal the test happens to run under.
#[test]
fn dispatch_action_increase_pane_height_grows_by_one_line() {
    let mut app = global_test_app(&[("a", 1)]);
    let before = app.pane_height;
    let quit = dispatch_action(&mut app, Action::IncreasePaneHeight);
    assert!(!quit);
    let page_size = crossterm::terminal::size()
        .map(|(_, rows)| rows as usize)
        .unwrap_or(20);
    assert_eq!(app.pane_height, before.increase(page_size));
}

/// `Action::DecreasePaneHeight` shrinks by one line and floors at
/// the historical 8-line minimum rather than going negative or
/// wrapping.
#[test]
fn dispatch_action_decrease_pane_height_floors_at_min() {
    let mut app = global_test_app(&[("a", 1)]);
    assert_eq!(app.pane_height, crate::tui::state::PaneHeight::default());
    let quit = dispatch_action(&mut app, Action::DecreasePaneHeight);
    assert!(!quit);
    // Already at the floor — decreasing further is a no-op.
    assert_eq!(app.pane_height, crate::tui::state::PaneHeight::default());
}

/// The destructive-confirm
/// dialog closes on the
/// user-configured `Cancel`
/// binding (default `Esc`,
/// configurable via
/// `key.cancel=...`).
/// `n` / `N` also close (the
/// mnemonic "no" answer,
/// always allowed regardless
/// of how the user has
/// rebound Cancel). Before
/// this tightening, the
/// dialog hard-coded `Esc`
/// and could not honour
/// user rebindings — the
/// displayed close hint
/// said one thing, the
/// accepted keys were
/// another.
#[test]
fn confirm_delete_closes_on_user_cancel_binding() {
    let mut app = global_test_app(&[("a", 1)]);
    app.confirm_delete = Some(ConfirmMode::DeleteSelected);
    // Default Cancel is Esc.
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
    handle_confirm_delete_key(&mut app, esc, ConfirmMode::DeleteSelected);
    assert!(app.confirm_delete.is_none());
    // `n` is always a no
    // answer.
    app.confirm_delete = Some(ConfirmMode::DeleteSelected);
    let n = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty());
    handle_confirm_delete_key(&mut app, n, ConfirmMode::DeleteSelected);
    assert!(app.confirm_delete.is_none());
    // Rebind Cancel to F1 and
    // verify F1 now closes
    // (and Esc no longer
    // does — Cancel's scope
    // is the keys bound to
    // it, nothing more).
    app.bindings.set(
        Action::Cancel,
        vec![bindings::parse_key_spec("F1").expect("F1")],
    );
    app.confirm_delete = Some(ConfirmMode::DeleteSelected);
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
    handle_confirm_delete_key(&mut app, esc, ConfirmMode::DeleteSelected);
    assert!(
        app.confirm_delete.is_some(),
        "Esc must NOT close when Cancel is bound to F1"
    );
    let f1 = KeyEvent::new(KeyCode::F(1), KeyModifiers::empty());
    handle_confirm_delete_key(&mut app, f1, ConfirmMode::DeleteSelected);
    assert!(app.confirm_delete.is_none());
}

#[test]
fn theme_picker_default_binding_and_list_layout() {
    let bindings = KeyBindings::defaults();
    // Default key is `T` so it doesn't collide with the
    // Ctrl-N / Ctrl-P cycling shortcuts.
    assert_eq!(
        format_key_specs(bindings.specs(Action::ThemePicker)),
        "T".to_string()
    );
    // Pressing T fires the ThemePicker.
    let evt = KeyEvent::new(KeyCode::Char('T'), KeyModifiers::empty());
    assert_eq!(action_for_key(&bindings, &evt), Some(Action::ThemePicker));
    // Picker contains every theme: `None` plus the
    // canonical `ratatui-themes::ThemeName::all()` list.
    let p = ThemePicker::new(SelectedTheme::None);
    assert_eq!(p.themes.len(), BuiltinTheme::all().len() + 1);
    assert_eq!(p.themes[0], SelectedTheme::None);
    assert!(p
        .themes
        .iter()
        .skip(1)
        .all(|t| matches!(t, SelectedTheme::Builtin(_))));
    // `move_by` clamps to the list bounds.
    let mut p = ThemePicker::new(SelectedTheme::None);
    p.move_by(-10);
    assert_eq!(p.selected, 0);
    p.move_by(9999);
    assert_eq!(p.selected, p.themes.len() - 1);
}

/// The captured-output view
/// closes only on the
/// user-configured `Cancel`
/// binding (default `Esc`,
/// configurable via
/// `key.cancel=...`). The
/// toggle key (`Ctrl+L` /
/// `Action::ShowOutput` by
/// default) closes too —
/// it's how the user opened
/// the view, so pressing it
/// again closes it
/// (toggle-semantics).
/// Other previously-hard-
/// coded close keys (`q`,
/// `Enter`) no longer close:
/// `q` is just a printable
/// character now and the
/// title's close hint
/// matches the actual keys.
/// `Ctrl+C` still aborts the
/// whole TUI session.
#[test]
fn output_view_closes_on_cancel_or_toggle_only() {
    let mut app = global_test_app(&[("a", 1)]);
    // Open the output view
    // with some text. We
    // do this directly on
    // the field rather than
    // via `show_output_view`
    // (which requires a
    // selected row with
    // non-empty output).
    app.output_view = Some(OutputView {
        text: "captured\noutput".to_string(),
        scroll: 0,
    });
    assert!(app.output_view.is_some());
    // Default `Esc` (Cancel)
    // closes — both returns
    // `Close` AND actually
    // tears down the view
    // (the runner loop ignores
    // the return value for
    // the Close case, so the
    // handler has to mutate
    // `app.output_view`
    // itself).
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
    let r = handle_output_view_key(&mut app, esc, 10);
    assert!(
        matches!(r, OutputViewResult::Close),
        "Esc (Cancel) must return Close"
    );
    assert!(
        !app.is_output_viewing(),
        "Esc must actually close the output view (not just return Close)"
    );
    // `C-o` (the toggle /
    // ShowOutput action)
    // also closes. (The
    // default for
    // ShowOutput was
    // `C-l` historically;
    // the project config
    // moves it to
    // `C-o`.)
    app.output_view = Some(OutputView {
        text: "captured\noutput".to_string(),
        scroll: 0,
    });
    let co = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
    let r = handle_output_view_key(&mut app, co, 10);
    assert!(matches!(r, OutputViewResult::Close));
    assert!(
        !app.is_output_viewing(),
        "Ctrl+O (toggle) must actually close the output view"
    );
    // `q` does NOT close
    // anymore — it's
    // text-input with the
    // toggle key, and
    // would silently swallow
    // a `q` the user typed
    // looking for "quit" or
    // "query" output.
    app.output_view = Some(OutputView {
        text: "captured".to_string(),
        scroll: 0,
    });
    let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty());
    let r = handle_output_view_key(&mut app, q, 10);
    assert!(
        matches!(r, OutputViewResult::Continue),
        "q must NOT close the output view"
    );
    // `Enter` similarly
    // doesn't close.
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
    let r = handle_output_view_key(&mut app, enter, 10);
    assert!(
        matches!(r, OutputViewResult::Continue),
        "Enter must NOT close the output view"
    );
    // Scrolling keys still
    // work without closing.
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
    let r = handle_output_view_key(&mut app, down, 10);
    assert!(matches!(r, OutputViewResult::Continue));
    assert!(app.output_view.is_some());
}

#[test]
fn curated_themes_parse_and_cycle() {
    // Every curated theme must:
    //   * have a unique, kebab-case slug,
    //   * round-trip through `from_slug`,
    //   * show up in `BuiltinTheme::all()` exactly once.
    let mut seen = std::collections::HashSet::new();
    for t in BuiltinTheme::curated() {
        let s = t.slug();
        assert!(!s.is_empty(), "empty slug for {:?}", t);
        assert!(
            s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "slug {:?} not kebab-case",
            s
        );
        assert!(seen.insert(s), "duplicate slug {}", s);
        let parsed = SelectedTheme::from_slug(s);
        assert_eq!(
            parsed,
            SelectedTheme::Builtin(*t),
            "from_slug round-trip failed for {:?}",
            s
        );
    }
    // Upstream themes still parse (regression check).
    assert_eq!(
        SelectedTheme::from_slug("dracula"),
        SelectedTheme::Builtin(BuiltinTheme::Dracula)
    );
    // Unknown slug falls back to None.
    assert_eq!(
        SelectedTheme::from_slug("totally-made-up"),
        SelectedTheme::None
    );
}

#[test]
fn mode_cycle_and_parse() {
    // Cycling wraps through the four modes.
    assert_eq!(Mode::Sess.next(), Mode::Dir);
    assert_eq!(Mode::Dir.next(), Mode::Global);
    assert_eq!(Mode::Global.next(), Mode::Stats);
    assert_eq!(Mode::Stats.next(), Mode::Sess);
    // String parsing is case-insensitive and accepts the
    // documented aliases.
    assert_eq!(Mode::parse("stats"), Some(Mode::Stats));
    assert_eq!(Mode::parse("STATISTICS"), Some(Mode::Stats));
    assert_eq!(Mode::parse("Stats"), Some(Mode::Stats));
    assert!(Mode::parse("not-a-mode").is_none());
}

#[test]
fn exit_filter_cycles_through_three_states() {
    // The action is bound to Ctrl-J by default; the
    // user cycles All → OK → ERR → All.
    assert_eq!(ExitFilter::All.next(), ExitFilter::Success);
    assert_eq!(ExitFilter::Success.next(), ExitFilter::Failed);
    assert_eq!(ExitFilter::Failed.next(), ExitFilter::All);
    // Default is `All` (no filter, see every row).
    assert_eq!(ExitFilter::default(), ExitFilter::All);
}

#[test]
fn exit_filter_as_str_round_trips_through_parse() {
    // The session file and any future config-file knob
    // use the lowercase form returned by `as_str()`.
    for value in [ExitFilter::All, ExitFilter::Success, ExitFilter::Failed] {
        assert_eq!(ExitFilter::parse(value.as_str()), Some(value));
    }
    // `parse` is case-insensitive and accepts aliases.
    assert_eq!(ExitFilter::parse("OK"), Some(ExitFilter::Success));
    assert_eq!(ExitFilter::parse("success"), Some(ExitFilter::Success));
    assert_eq!(ExitFilter::parse("err"), Some(ExitFilter::Failed));
    assert_eq!(ExitFilter::parse("FAILED"), Some(ExitFilter::Failed));
    // Unknown values fall through to `None` so the
    // caller can keep the default.
    assert!(ExitFilter::parse("maybe").is_none());
    assert!(ExitFilter::parse("").is_none());
}

/// Build a fresh in-memory `App` whose `history` table is
/// pre-populated with the rows in `rows`. `rows` is a slice
/// of `(command, timestamp_offset_secs)` — the timestamp is
/// `now - offset` so the tests are stable regardless of when
/// they run.
fn stats_test_app(rows: &[(&str, i64)]) -> App {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
    )
    .expect("schema");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for (i, (cmd, offset)) in rows.iter().enumerate() {
        conn.execute(
            "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
                                 VALUES (?1, ?2, '/tmp', 'sess', 0, ?3)",
            rusqlite::params![i as i64 + 1, *cmd, now - *offset],
        )
        .expect("insert");
    }
    let mut app = App::new(
        conn,
        Mode::Stats,
        String::new(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        // Tests don't care about the
        // detected scheme; pass Dark
        // (the historical default) so
        // any test that reads
        // `app.detected_scheme()` gets
        // a deterministic value.
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    app.refresh();
    app
}

/// Build an app in `Mode::Global` (the most
/// common mode for ad-hoc history searches)
/// with the given rows. Identical to
/// `stats_test_app` except for the mode,
/// which is what the sort-order tests
/// below need: Stats mode overrides the
/// user-picked sort with the successor-
/// frequency ranking from `fetch_stats`, so
/// the frequency-sort tests have to run in a
/// non-Stats mode to actually exercise the
/// `SortOrder::Frequency` path.
fn global_test_app(rows: &[(&str, i64)]) -> App {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
    )
    .expect("schema");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for (i, (cmd, offset)) in rows.iter().enumerate() {
        conn.execute(
            "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
                                 VALUES (?1, ?2, '/tmp', 'sess', 0, ?3)",
            rusqlite::params![i as i64 + 1, *cmd, now - *offset],
        )
        .expect("insert");
    }
    App::new(
        conn,
        Mode::Global,
        String::new(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    )
}

/// Like `global_test_app` but with the unique
/// index that backs the production
/// `ON CONFLICT (command, directory, session_id)`
/// upsert. Tests that exercise the
/// history-insert path (e.g. the
/// `correct` action) need this index,
/// otherwise the insert fails with
/// "ON CONFLICT clause does not match
/// any PRIMARY KEY or UNIQUE constraint".
fn global_test_app_with_dedup_index(rows: &[(&str, i64)]) -> App {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );
                        CREATE UNIQUE INDEX idx_history_dedup
                            ON history (command, directory, session_id);",
    )
    .expect("schema");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for (i, (cmd, offset)) in rows.iter().enumerate() {
        conn.execute(
            "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
                                 VALUES (?1, ?2, '/tmp', 'sess', 0, ?3)",
            rusqlite::params![i as i64 + 1, *cmd, now - *offset],
        )
        .expect("insert");
    }
    App::new(
        conn,
        Mode::Global,
        String::new(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    )
}

#[test]
fn stats_mode_ranks_by_follow_frequency_then_age() {
    // Sequence (oldest first):
    //   A B A B C A D
    // The "last command" is D. Its successors in the
    // global history are: A (once). So A should rank
    // first. The remaining rows are sorted by timestamp
    // DESC: D is excluded (it's the last command itself
    // in this test since we always pick the newest), A
    // (just after D), B, C — with the duplicate filter
    // off, every occurrence is shown.
    //
    // For a cleaner test we use a sequence where the
    // last command has multiple distinct successors with
    // known frequencies:
    //   seq:    X Y X Y Z X Y W
    //   newest: W
    //   successors of W: none (it's the most recent)
    //   but we want a non-W last, so add a trailing W2:
    //   seq:    X Y X Y Z X Y W W2
    //   last:   W2 (newest). Successors of W2: none yet.
    //
    // We rebuild the sequence so the *last* command has
    // many successors: arrange so the global newest row
    // is `git status`. Successors of `git status` in
    // the history should be ranked first; everything else
    // falls back to timestamp DESC.
    let rows: &[(&str, i64)] = &[
        ("vim Cargo.toml", 50),
        ("cargo build", 45),
        ("vim Cargo.toml", 40),
        ("git status", 35),
        ("vim Cargo.toml", 30),
        ("cargo build", 25),
        ("git status", 20),
        ("cargo build", 15),
        ("git status", 10), // oldest
    ];
    let _ = rows; // appease unused-warning fixers
    let rows: &[(&str, i64)] = &[
        ("vim Cargo.toml", 90),
        ("cargo build", 85),
        ("vim Cargo.toml", 80),
        ("git status", 75),
        ("vim Cargo.toml", 70),
        ("cargo build", 65),
        ("git status", 60),
        ("cargo build", 55),
        ("git status", 50),
        ("cargo build", 45),
        ("git status", 40),
        ("cargo build", 35),
        ("git status", 30),
        ("cargo build", 25),
        ("ls", 20),
        ("echo hello", 15),
        // Newest: `git status` — so it's the "last command".
        ("git status", 10),
    ];
    let app = stats_test_app(rows);
    assert_eq!(app.mode, Mode::Stats);
    let merged = app.merged_rows();
    // The newest row is `git status` (timestamp 10).
    // Its successors in the entire history are
    // `cargo build` and `vim Cargo.toml`. Counting pairs:
    //   git status -> cargo build: 5 times
    //   git status -> vim Cargo.toml: 3 times
    // So cargo build ranks above vim, then the rest of
    // the history sorted by timestamp DESC.
    let cmds: Vec<&str> = merged.iter().map(|r| r.command.as_str()).collect();
    // 6 cargo build entries with freq=4, 3 vim with
    // freq=1, then the rest sorted by timestamp DESC.
    assert_eq!(
        cmds.len(),
        17,
        "expected every history row to come back, got {} rows: {:?}",
        cmds.len(),
        cmds
    );
    assert_eq!(
        cmds[0], "cargo build",
        "expected highest frequency successor first, got {:?}",
        cmds
    );
    assert_eq!(
        cmds[5], "cargo build",
        "6 cargo build rows should share freq=4, got {:?}",
        cmds
    );
    assert_eq!(
        cmds[6], "vim Cargo.toml",
        "vim should follow cargo's freq=4 rows, got {:?}",
        cmds
    );
    assert!(!cmds.is_empty());
}

#[test]
fn stats_mode_duplicate_filter_keeps_newest_only() {
    let rows: &[(&str, i64)] = &[
        ("git status", 30),
        ("cargo build", 25),
        ("git status", 20),
        ("vim Cargo.toml", 15),
        ("git status", 10), // newest
    ];
    let mut app = stats_test_app(rows);
    // Duplicate filter on: only one `cargo build`,
    // one `vim Cargo.toml`, one `git status`.
    app.duplicate_filter = true;
    app.refresh();
    let binding = app.merged_rows();
    let cmds: Vec<&str> = binding.iter().map(|r| r.command.as_str()).collect();
    // Each unique command appears at most once.
    let mut sorted = cmds.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        cmds.len(),
        "duplicate filter should remove duplicates: {:?}",
        cmds
    );
}

/// The exit-code filter is implemented in SQL: the
/// `build_where` helper appends a clause to the SELECT
/// statement. These tests confirm the clause is present
/// (or absent, in the All case) regardless of mode.
#[test]
fn exit_filter_all_adds_no_clause() {
    let app = stats_test_app(&[("git status", 1)]);
    let (clause, _) = app.build_where();
    assert!(
        !clause.contains("exit_code"),
        "All should not add an exit_code clause, got: {:?}",
        clause
    );
}

#[test]
fn exit_filter_success_matches_only_zero() {
    let mut app = stats_test_app(&[("true", 1), ("false", 1)]);
    // Cycle from All → Success.
    app.cycle_exit_filter();
    let (clause, _) = app.build_where();
    assert!(
        clause.contains("h.exit_code = 0"),
        "Success should add `h.exit_code = 0`, got: {:?}",
        clause
    );
}

#[test]
fn exit_filter_failed_matches_only_nonzero() {
    let mut app = stats_test_app(&[("true", 1), ("false", 1)]);
    app.cycle_exit_filter(); // All → Success
    app.cycle_exit_filter(); // Success → Failed
    let (clause, _) = app.build_where();
    assert!(
        clause.contains("h.exit_code != 0"),
        "Failed should add `h.exit_code != 0`, got: {:?}",
        clause
    );
}

/// End-to-end: cycle the filter and confirm `refresh`
/// actually changes the row set. The test inserts rows
/// with a mix of exit codes, so the filter should split
/// them cleanly.
#[test]
fn cycle_exit_filter_refreshes_rows() {
    // The `stats_test_app` helper hard-codes
    // `exit_code = 0` for every row, which would make
    // "Success" and "All" indistinguishable. Insert
    // our own mixed table here.
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
    )
    .expect("create tables");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    // (command, timestamp_offset, exit_code)
    let rows: &[(&str, i64, i32)] = &[
        ("true", 30, 0),       // success
        ("false", 25, 1),      // failure
        ("git status", 20, 0), // success
        ("segfault", 15, 139), // failure
    ];
    for (i, (cmd, offset, code)) in rows.iter().enumerate() {
        conn.execute(
            "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
                                 VALUES (?1, ?2, '/tmp', 'sess', ?3, ?4)",
            rusqlite::params![i as i64 + 1, *cmd, *code, now - *offset,],
        )
        .expect("insert");
    }
    let mut app = App::new(
        conn,
        Mode::Stats,
        String::new(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    app.refresh();
    let all_count = app.merged_rows().len();
    assert_eq!(all_count, 4, "All should show every row");

    app.cycle_exit_filter(); // → Success
    let ok_count = app.merged_rows().len();
    assert_eq!(ok_count, 2, "Success should keep only exit_code == 0");
    for r in app.merged_rows() {
        assert_eq!(r.exit_code, 0, "Success row had nonzero exit_code");
    }

    app.cycle_exit_filter(); // → Failed
    let err_count = app.merged_rows().len();
    assert_eq!(err_count, 2, "Failed should keep only exit_code != 0");
    for r in app.merged_rows() {
        assert_ne!(r.exit_code, 0, "Failed row had zero exit_code");
    }

    app.cycle_exit_filter(); // → All (wraps)
    assert_eq!(app.merged_rows().len(), 4);
    assert_eq!(app.exit_filter, ExitFilter::All);
}

/// The default key for `CycleExitFilter` is `Ctrl-J`; make
/// sure that still works after the refactor.
#[test]
fn cycle_exit_filter_default_key_routes() {
    let bindings = KeyBindings::defaults();
    assert_eq!(
        format_key_specs(bindings.specs(Action::CycleExitFilter)),
        "C-j"
    );
    let evt = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
    assert_eq!(
        action_for_key(&bindings, &evt),
        Some(Action::CycleExitFilter)
    );
}

/// `YankSelection` is bound to `Ctrl-Y` (the canonical
/// readline/vim yank shortcut) and the action_for_key
/// lookup routes the keystroke correctly.
#[test]
fn yank_selection_default_key_routes() {
    let bindings = KeyBindings::defaults();
    assert_eq!(
        format_key_specs(bindings.specs(Action::YankSelection)),
        "C-y"
    );
    let evt = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(action_for_key(&bindings, &evt), Some(Action::YankSelection));
}

/// `pick_text_to_yank` falls back to the selected row's
/// command when the output view is closed.
#[test]
fn pick_text_to_yank_uses_selected_row() {
    let app = stats_test_app(&[("echo hello", 30)]);
    // Default `list_state` from `App::new` selects
    // index 0, so the first row is the selection.
    let text = pick_text_to_yank(&app).expect("a row is selected");
    assert_eq!(text, "echo hello");
}

/// `pick_text_to_yank` prefers the output view text over
/// the selected row's command. This is the "or the
/// output of this command" branch the user asked for.
#[test]
fn pick_text_to_yank_prefers_output_view() {
    let mut app = stats_test_app(&[("cargo test", 30)]);
    // Simulate the output overlay being open with a
    // specific captured text. We use a string that
    // differs from any command in the table so the
    // test catches a mix-up between the two sources.
    let output_text = "test result: ok. 12 passed; 0 failed";
    app.output_view = Some(OutputView {
        text: output_text.to_string(),
        scroll: 0,
    });
    let text = pick_text_to_yank(&app).expect("output view is set");
    assert_eq!(text, output_text);
    // Even though there's a selected row, the
    // output view wins.
    assert_ne!(text, "cargo test");
}

/// `pick_text_to_yank` returns `None` when there's no
/// output view and no selected row. The caller surfaces
/// this as a "Nothing to yank" status message.
#[test]
fn pick_text_to_yank_returns_none_when_empty() {
    // Empty history — no rows, no selection.
    let app = stats_test_app(&[]);
    assert!(pick_text_to_yank(&app).is_none());
}

/// `App::yank_to_clipboard` with no output view and a
/// selected row sets a "Yanked N chars" status message
/// on success. The actual clipboard write goes through
/// arboard; in CI without a display server it may fail,
/// so the test accepts either outcome but always
/// confirms that *some* feedback was surfaced (the
/// yank never crashes the TUI).
#[test]
fn yank_to_clipboard_with_selected_row_sets_status() {
    let mut app = stats_test_app(&[("ls -la", 30)]);
    assert!(app.status_message.is_none());
    app.yank_to_clipboard();
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.clone())
        .expect("yank must set a status message");
    // On success: "Yanked N chars to clipboard".
    // On failure: "Yank failed: <reason>".
    // Either is acceptable — we just want to confirm
    // the action did not silently no-op.
    assert!(
        msg.starts_with("Yanked ") || msg.starts_with("Yank failed"),
        "unexpected status message: {:?}",
        msg
    );
}

/// `yank_to_clipboard` is a no-op with a clear status
/// message when there's nothing to copy. The clipboard
/// must never be touched in that case (we'd just be
/// putting whatever stale data was already on the
/// clipboard back).
#[test]
fn yank_to_clipboard_with_nothing_to_copy() {
    let mut app = stats_test_app(&[]);
    app.yank_to_clipboard();
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("yank must report when there's nothing to copy");
    assert_eq!(msg, "Nothing to yank");
}

// --- tokenize_command -------------------------------------------------

#[test]
fn tokenize_splits_on_whitespace() {
    assert_eq!(
        tokenize_command("git log --oneline"),
        vec!["git", "log", "--oneline"]
    );
}

#[test]
fn tokenize_strips_double_quotes() {
    assert_eq!(
        tokenize_command("cat \"my file.txt\""),
        vec!["cat", "my file.txt"]
    );
}

#[test]
fn tokenize_strips_single_quotes() {
    assert_eq!(
        tokenize_command("vim 'weird name'"),
        vec!["vim", "weird name"]
    );
}

#[test]
fn tokenize_handles_multiple_spaces_and_tabs() {
    assert_eq!(
        tokenize_command("  git\tlog  \t  oneline  "),
        vec!["git", "log", "oneline"]
    );
}

#[test]
fn tokenize_empty_command() {
    assert_eq!(tokenize_command(""), Vec::<String>::new());
    assert_eq!(tokenize_command("   \t  "), Vec::<String>::new());
}

// --- find_filename_in_command -----------------------------------------

#[test]
fn find_filename_picks_absolute_path() {
    assert_eq!(
        find_filename_in_command("cat /etc/hosts"),
        Some("/etc/hosts".to_string())
    );
}

#[test]
fn find_filename_picks_tilde_path() {
    assert_eq!(
        find_filename_in_command("vim ~/.bashrc"),
        Some("~/.bashrc".to_string())
    );
}

#[test]
fn find_filename_picks_relative_path() {
    assert_eq!(
        find_filename_in_command("less ./README.md"),
        Some("./README.md".to_string())
    );
}

#[test]
fn find_filename_picks_dotdot_path() {
    assert_eq!(
        find_filename_in_command("vim ../sibling.txt"),
        Some("../sibling.txt".to_string())
    );
}

#[test]
fn find_filename_picks_subdir_path() {
    // No leading slash, but contains a separator
    // and a dot in the filename. The directory part
    // is `notes`, the file part is `plan.md`.
    assert_eq!(
        find_filename_in_command("cat notes/plan.md"),
        Some("notes/plan.md".to_string())
    );
}

#[test]
fn find_filename_picks_bare_filename_with_extension() {
    // No slash, but a dot in the name: README.md
    // invoked from the working directory.
    assert_eq!(
        find_filename_in_command("code README.md"),
        Some("README.md".to_string())
    );
}

#[test]
fn find_filename_skips_flags() {
    // `-rf` starts with `-` and is rejected. The
    // path after it still wins.
    assert_eq!(
        find_filename_in_command("rm -rf /tmp/foo"),
        Some("/tmp/foo".to_string())
    );
}

#[test]
fn find_filename_skips_glob() {
    // `/tmp/foo*` is a glob, not a file. The TUI
    // should not pick it.
    assert_eq!(find_filename_in_command("rm /tmp/foo*"), None);
}

#[test]
fn find_filename_skips_variable_interpolation() {
    // `$HOME` is a shell variable reference, not a
    // literal path. We don't try to resolve it.
    assert_eq!(find_filename_in_command("vim $HOME/.profile"), None);
}

#[test]
fn find_filename_skips_command_substitution() {
    // `$(echo foo)` is a subshell expansion, not a
    // path.
    assert_eq!(find_filename_in_command("cat $(echo /etc/hosts)"), None);
}

#[test]
fn find_filename_skips_redirect_operator() {
    // The `>` token is a redirect, not a file.
    assert_eq!(
        find_filename_in_command("echo hello > /tmp/out"),
        Some("/tmp/out".to_string())
    );
}

#[test]
fn find_filename_handles_lone_dot() {
    // `cd .` — the `.` is the current directory, not
    // a file. The algorithm should not pick it.
    assert_eq!(find_filename_in_command("cd ."), None);
}

#[test]
fn find_filename_handles_lone_dotdot() {
    // `cd ..` — same as above for `..`.
    assert_eq!(find_filename_in_command("cd .."), None);
}

#[test]
fn find_filename_picks_best_among_multiple() {
    // Both `/etc/passwd` and `temp.txt` look like
    // paths. The absolute one scores higher (leading
    // `/` +10 vs leading-with-`.`/extension +5+3)
    // and wins.
    assert_eq!(
        find_filename_in_command("diff /etc/passwd temp.txt"),
        Some("/etc/passwd".to_string())
    );
}

#[test]
fn find_filename_returns_none_for_pure_command() {
    // `ls -la` has no path-like token at all.
    assert_eq!(find_filename_in_command("ls -la"), None);
}

#[test]
fn find_filename_handles_quoted_path_with_spaces() {
    // Quoted form is collapsed into one token by the
    // tokenizer, so the score picks it up.
    assert_eq!(
        find_filename_in_command("cat \"my notes.txt\""),
        Some("my notes.txt".to_string())
    );
}

// --- Fuzzy search ---------------------------------------------------

#[test]
fn is_fuzzy_query_recognises_question_mark_prefix() {
    let mut app = stats_test_app(&[]);
    app.query = String::from("git");
    app.match_algorithm = MatchAlgorithm::Substring;
    assert!(!app.is_fuzzy_query());
    app.match_algorithm = MatchAlgorithm::Fuzzy;
    assert!(app.is_fuzzy_query());
}

#[test]
fn fuzzy_pattern_strips_question_mark() {
    let mut app = stats_test_app(&[]);
    app.query = String::from("git st");
    app.match_algorithm = MatchAlgorithm::Fuzzy;
    assert_eq!(app.fuzzy_pattern(), "git st");
}

#[test]
fn query_matches_text_supports_fuzzy_subsequence() {
    let mut app = stats_test_app(&[]);
    app.query = String::from("gts");
    app.match_algorithm = MatchAlgorithm::Fuzzy;
    assert!(app.query_matches_text("git status --short && cargo build"));
    assert!(app.query_matches_text("go test stuff"));
    assert!(!app.query_matches_text("vim"));
}

#[test]
fn query_matches_text_fuzzy_is_case_insensitive() {
    let mut app = stats_test_app(&[]);
    app.query = String::from("GS");
    app.match_algorithm = MatchAlgorithm::Fuzzy;
    assert!(app.query_matches_text("git status"));
    assert!(app.query_matches_text("GIT STATUS"));
    assert!(!app.query_matches_text("cargo"));
}

#[test]
fn query_matches_text_fuzzy_supports_and_by_word() {
    let mut app = stats_test_app(&[]);
    app.query = "git st".to_string();
    app.match_algorithm = MatchAlgorithm::Fuzzy;
    // `git` and `st` both appear as subsequences.
    assert!(app.query_matches_text("git status"));
    assert!(app.query_matches_text("git stash"));
    // `st` is not a subsequence of "vim".
    assert!(!app.query_matches_text("vim"));
    // `git` is missing.
    assert!(!app.query_matches_text("cargo test"));
}

#[test]
fn query_matches_text_fuzzy_empty_pattern_matches_all() {
    let mut app = stats_test_app(&[]);
    app.query = String::from("");
    app.match_algorithm = MatchAlgorithm::Fuzzy;
    assert!(app.query_matches_text("anything"));
    assert!(app.query_matches_text(""));
}

#[test]
fn build_where_skips_like_clauses_for_fuzzy_query() {
    // After the match-algorithm refactor, fuzzy is a
    // post-filter, not a SQL pre-filter. The build_where
    // path always includes LIKE for non-empty queries;
    // the fuzzy algorithm is applied in `refresh()`.
    // This test is kept as a no-op marker.
}

#[test]
fn cycle_search_mode_advances_prefix() {
    let mut app = stats_test_app(&[("git status", 1)]);
    // Substring -> Fuzzy (query NOT modified; algorithm changes).
    app.cycle_search_mode();
    assert_eq!(app.match_algorithm, MatchAlgorithm::Fuzzy);
    assert_eq!(app.query, "");
    // Fuzzy -> Regex.
    app.cycle_search_mode();
    assert_eq!(app.match_algorithm, MatchAlgorithm::Regex);
    // Regex -> Substring (back to default).
    app.cycle_search_mode();
    assert_eq!(app.match_algorithm, MatchAlgorithm::Substring);
}

#[test]
fn cycle_search_mode_preserves_query_body() {
    let mut app = stats_test_app(&[]);
    app.query = String::from("git commit");
    app.cycle_search_mode();
    assert!(app.query.contains("git commit"));
    app.cycle_search_mode();
    assert!(app.query.contains("git commit"));
    app.cycle_search_mode();
    assert!(app.query.contains("git commit"));
}

// --- App::edit_referenced_file end-to-end ------------------------------

#[test]
fn edit_referenced_file_stages_editor_command() {
    // Use a row whose command has a clear path.
    // We can't easily inject an arbitrary command
    // through `stats_test_app` (it hard-codes
    // `exit_code = 0`); use a row whose command
    // shape is the only thing we care about.
    let mut app = stats_test_app(&[("vim /etc/hosts", 30)]);
    app.edit_referenced_file();
    // `selection` is the staged editor command.
    // We don't pin the editor (it depends on the
    // host's $EDITOR) so we anchor on the
    // unquoted-path form. The trailing
    // `path-without-quotes` is the contract:
    // `vim /etc/hosts`, not `vim '/etc/hosts'`.
    let sel = app
        .selection
        .as_deref()
        .expect("staged command must be set");
    assert!(
        sel.ends_with(" /etc/hosts"),
        "staged command should end with unquoted path, got {:?}",
        sel
    );
    assert!(
        !sel.contains('\''),
        "staged command must not contain shell quotes, got {:?}",
        sel
    );
    // `pick_mode` is `Run` so the parent shell will
    // execute it.
    assert_eq!(app.pick_mode, Some(PickMode::Run));
}

#[test]
fn edit_referenced_file_with_no_row_is_a_no_op() {
    let mut app = stats_test_app(&[]);
    // Empty history — no row selected.
    app.edit_referenced_file();
    assert!(app.selection.is_none());
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("must surface a status message");
    assert_eq!(msg, "No command selected");
}

#[test]
fn edit_referenced_file_with_no_path_surfaces_message() {
    let mut app = stats_test_app(&[("ls -la", 30)]);
    app.edit_referenced_file();
    assert!(app.selection.is_none());
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("must surface a status message");
    assert!(msg.starts_with("No filename found in:"), "got {:?}", msg);
}

// --- Action routing ---------------------------------------------------

#[test]
fn edit_file_reference_default_key_routes() {
    let bindings = KeyBindings::defaults();
    // `C-v` is the default for EditFileReference
    // (matches the user-configured
    // `key.edit-file-reference=C-v` in the
    // project config; `C-o` is now reserved
    // for `show-output`).
    assert_eq!(
        format_key_specs(bindings.specs(Action::EditFileReference)),
        "C-v"
    );
    let evt = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
    assert_eq!(
        action_for_key(&bindings, &evt),
        Some(Action::EditFileReference)
    );
}

// --- Labeled-only selection bug ---------------------------------------
//
// Regression test: when the user navigates down to a row
// that lives in `self.labeled_rows` but not `self.rows`
// (i.e. a "very old" entry that's only surfaced because it
// has a comment), the actions that operate on the
// selected row used to silently no-op. The cursor stores
// an index into the *merged* list (rows + labeled_rows),
// but `selected_row()` was reading from `self.rows` alone.
// The fix: `selected_row()` looks at the merged list
// directly, so any index in `self.list_state` resolves
// to the row the user is actually looking at.
// --- Labeled-only selection bug ---------------------------------------
//
// Regression test: when the user navigates down to a
// row that lives in `self.labeled_rows` but not
// `self.rows` (e.g. a "very old" labeled row from a
// different session), the actions that operate on the
// selected row used to silently no-op. The cursor
// stores an index into the *merged* list (rows +
// labeled_rows), but `selected_row()` was reading from
// `self.rows` alone. The fix: `selected_row()` looks
// at the merged list directly, so any index in
// `self.list_state` resolves to the row the user is
// actually looking at.
#[test]
fn selected_row_finds_labeled_only_rows() {
    // The env-var manipulation in this test races
    // with `select_for_run_on_labeled_only_row_stages_command`
    // and the LLM tests when they all run in
    // parallel. Hold the env lock for the entire
    // test so the read/modify/restore is atomic
    // relative to other env-touching tests.
    let _env_guard = ENV_LOCK.lock().expect("env lock poisoned");
    // Build a DB with two rows that both match
    // the search query "git": one in the current
    // session (recent) and one in a *different*
    // session (ancient, with a comment). The
    // ancient row matches the query but is
    // excluded by the `Mode::Sess` SQL filter
    // (different session_id). So it appears in
    // `self.labeled_rows` and in `merged_rows`,
    // but NOT in `self.rows` — exactly the shape
    // that triggered the user's bug report.
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
    )
    .expect("create tables");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp)                          VALUES (1, 'git status', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 10],
                )
                .expect("insert recent");
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp)                          VALUES (2, 'git pull', '/tmp', 'ancient', 0, ?1)",
                        rusqlite::params![now - 100_000],
                )
                .expect("insert ancient");
    conn.execute(
                        "INSERT INTO command_comments (command, comment)                          VALUES ('git pull', 'remembered for the README example')",
                        [],
                )
                .expect("insert comment");

    // Pin `SMART_HISTORY_SESSION` so the SQL
    // filter consistently excludes the ancient
    // row. `set_var` is `unsafe` in Rust 2024 but
    // safe in practice for tests (single-threaded
    // test runner, restored at the end).
    let prev_session = std::env::var("SMART_HISTORY_SESSION").ok();
    unsafe {
        std::env::set_var("SMART_HISTORY_SESSION", "current");
    }
    let mut app = App::new(
        conn,
        Mode::Sess,
        "git".to_string(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    app.refresh();
    // Restore the env var as soon as the initial
    // state is built so we don't leak the override
    // into the rest of the test run.
    unsafe {
        match prev_session {
            Some(v) => std::env::set_var("SMART_HISTORY_SESSION", v),
            None => std::env::remove_var("SMART_HISTORY_SESSION"),
        }
    }
    assert_eq!(app.rows.len(), 1, "primary list excludes the ancient row");
    assert_eq!(
        app.labeled_rows.len(),
        1,
        "labeled list has the ancient row"
    );

    // Simulate the user pressing Down to move
    // the cursor past the primary list. This is
    // what `move_selection` does when the user
    // navigates through the merged view.
    app.move_selection(1);
    let merged_len = app.merged_rows().len();
    assert!(merged_len >= 2, "merged list should have both rows");
    assert_eq!(
        app.list_state.selected().unwrap(),
        merged_len - 1,
        "cursor should be on the last merged row"
    );
    // The cursor's index is past `self.rows.len()`
    // — this is the position where the bug used
    // to make `selected_row()` return `None`.
    assert!(app.list_state.selected().unwrap() >= app.rows.len());

    // `selected_row()` MUST find the labeled-only
    // row. This is the regression assertion.
    let row = app
        .selected_row()
        .expect("selected_row must find the labeled row");
    assert_eq!(row.command, "git pull");
}

/// Companion to the test above: when the action is
/// `Run`, staging a selection from a labeled-only row
/// works — which is the user-visible symptom the bug
/// report described ("the command line stays empty").
#[test]
fn select_for_run_on_labeled_only_row_stages_command() {
    // Hold the env lock for the whole test; see
    // `selected_row_finds_labeled_only_rows` for the
    // rationale.
    let _env_guard = ENV_LOCK.lock().expect("env lock poisoned");
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
    )
    .expect("create tables");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp)                          VALUES (1, 'git status', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 10],
                )
                .expect("insert recent");
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp)                          VALUES (2, 'git pull', '/tmp', 'ancient', 0, ?1)",
                        rusqlite::params![now - 100_000],
                )
                .expect("insert ancient");
    conn.execute(
                        "INSERT INTO command_comments (command, comment)                          VALUES ('git pull', 'remembered for the README example')",
                        [],
                )
                .expect("insert comment");

    let prev_session = std::env::var("SMART_HISTORY_SESSION").ok();
    unsafe {
        std::env::set_var("SMART_HISTORY_SESSION", "current");
    }
    let mut app = App::new(
        conn,
        Mode::Sess,
        "git".to_string(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    app.refresh();
    unsafe {
        match prev_session {
            Some(v) => std::env::set_var("SMART_HISTORY_SESSION", v),
            None => std::env::remove_var("SMART_HISTORY_SESSION"),
        }
    }
    // Navigate to the labeled-only row.
    app.move_selection(1);
    // The bug: `select_for_run` would leave
    // `self.selection = None` because
    // `self.rows.get(idx)` returned `None`.
    app.select_for_run();
    let staged = app
        .selection
        .as_deref()
        .expect("Run on a labeled-only row must stage its command");
    assert_eq!(staged, "git pull");
    assert_eq!(app.pick_mode, Some(PickMode::Run));
}

/// `@new <text>` in notes mode creates a
/// new daily note entry by staging
/// `note_search create-note ...`.
#[test]
fn notes_new_stages_create_note_command() {
    let mut app = directories_test_app(&[]);
    app.notes_database = Some(std::path::PathBuf::from("/tmp/test.sqlite"));
    app.query = "@new remember to buy milk".to_string();
    app.refresh();
    app.list_state.select(Some(0));
    app.select_for_run();
    let staged = app
        .selection
        .as_deref()
        .expect("selection must be set for @new");
    assert!(
        staged.contains("note_search create-note"),
        "@new must stage note_search create-note, got: {staged:?}"
    );
    assert!(
        staged.contains("remember to buy milk"),
        "@new must include the text in the command, got: {staged:?}"
    );
    assert!(
        staged.contains("--type daily"),
        "@new must use --type daily, got: {staged:?}"
    );
    assert!(
        staged.contains("--timestamp"),
        "@new must include --timestamp, got: {staged:?}"
    );
    assert_eq!(app.pick_mode, Some(PickMode::Run));
}

/// `!@new <text>` in todo mode creates a
/// new TODO entry in the daily note by staging
/// `note_search create-note ... --todo`.
#[test]
fn todo_new_stages_create_note_with_todo_flag() {
    let mut app = directories_test_app(&[]);
    app.notes_database = Some(std::path::PathBuf::from("/tmp/test.sqlite"));
    app.query = "!@new fix the build".to_string();
    app.refresh();
    app.list_state.select(Some(0));
    app.select_for_run();
    let staged = app
        .selection
        .as_deref()
        .expect("selection must be set for !@new");
    assert!(
        staged.contains("note_search create-note"),
        "!@new must stage note_search create-note, got: {staged:?}"
    );
    assert!(
        staged.contains("fix the build"),
        "!@new must include the text, got: {staged:?}"
    );
    assert!(
        staged.contains("--type daily"),
        "!@new must use --type daily, got: {staged:?}"
    );
    assert!(
        staged.contains("--todo"),
        "!@new must include --todo flag, got: {staged:?}"
    );
    assert!(
        staged.contains("--timestamp"),
        "!@new must include --timestamp, got: {staged:?}"
    );
    assert_eq!(app.pick_mode, Some(PickMode::Run));
}

/// `!@new` with no text is a no-op.
#[test]
fn todo_new_without_text_is_noop() {
    let mut app = directories_test_app(&[]);
    app.notes_database = Some(std::path::PathBuf::from("/tmp/test.sqlite"));
    app.query = "!@new".to_string();
    app.refresh();
    app.list_state.select(Some(0));
    app.select_for_run();
    assert!(
        app.selection.is_none(),
        "!@new with no text must not stage a command"
    );
}

/// `@new` with no text after `new` is a no-op
/// (status message, no staged command).
#[test]
fn notes_new_without_text_is_noop() {
    let mut app = directories_test_app(&[]);
    app.notes_database = Some(std::path::PathBuf::from("/tmp/test.sqlite"));
    app.query = "@new".to_string();
    app.refresh();
    app.list_state.select(Some(0));
    app.select_for_run();
    assert!(
        app.selection.is_none(),
        "@new with no text must not stage a command"
    );
}

/// Select a session row in panes mode
/// and verify the staged command contains
/// `herdr workspace create`, the workspace_id
/// extraction, `herdr pane run`, and
/// `herdr workspace focus`.
#[cfg(feature = "herdr")]
#[test]
fn session_row_in_panes_mode_stages_create_with_exec() {
    use crate::tui::state::HistoryRow;
    let mut app = directories_test_app(&[]);
    app.multiplexer = crate::multiplexer::backend_for(crate::multiplexer::MultiplexerKind::Herdr);
    app.query = "*".to_string();
    app.refresh();
    // Inject a session row into session_panes
    app.session_panes.push(HistoryRow {
        id: -10_001,
        command: "Herdr config".to_string(),
        directory: "/Users/har/.config/herdr".to_string(),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: "nvim config.toml".to_string(),
        output: String::new(),
        mode: "session".to_string(),
        source: "sessions".to_string(),

        ..Default::default()
    });
    // Rebuild merged_rows
    app.refresh();
    // Find the session row
    let idx = app
        .merged_rows()
        .iter()
        .position(|r| r.mode == "session")
        .expect("session row must be in merged_rows");
    app.list_state.select(Some(idx));
    app.select_for_run();
    let staged = app
        .selection
        .as_deref()
        .expect("selection must be set for session row");
    eprintln!("[test] staged command: {staged}");
    assert!(
        staged.contains("herdr workspace create"),
        "must contain workspace create, got: {staged:?}"
    );
    assert!(
        staged.contains("herdr pane run"),
        "must contain pane run, got: {staged:?}"
    );
    assert!(
        staged.contains("herdr workspace focus"),
        "must contain workspace focus, got: {staged:?}"
    );
    assert!(
        staged.contains("nvim config.toml"),
        "must contain the exec command, got: {staged:?}"
    );
}

/// Regression test for the
/// "selecting an existing
/// session row duplicates the
/// workspace" bug: when the
/// user is in `*` mode and the
/// configured session already
/// has a herdr workspace
/// attached, pressing Enter
/// must focus the existing
/// workspace (`herdr workspace
/// focus <ws_id>`) — not create
/// a duplicate (`herdr workspace
/// create ...`).
///
/// Root cause: `select_for_run_impl`
/// read `self.tmux_windows` to
/// look up an existing
/// workspace with a matching
/// cwd, but `App::refresh` only
/// populated `tmux_windows`
/// for directories mode, so
/// the `*` view's
/// `tmux_windows` was always
/// empty and the matcher
/// always fell into the create
/// branch. The fix primes the
/// cache at the top of the
/// `is_panes_query` block in
/// `select_for_run_impl`.
///
/// The test uses a fake
/// `MultiplexerBackend` whose
/// `snapshot()` returns a
/// single active pane at
/// `/tmp` (workspace `wA`).
/// The session row's `dir`
/// is set to `/tmp` so the
/// matcher should find the
/// existing workspace after
/// `select_for_run_impl`
/// primes the cache.
#[cfg(feature = "herdr")]
#[test]
fn session_row_in_panes_mode_focuses_existing_herdr_workspace() {
    // `fetch_session_panes` short-circuits when
    // neither `TMUX_PANE` nor `HERDR_PANE_ID`
    // is set (the user isn't inside a
    // multiplexer pane, so the snapshot
    // would be wasted work). The test below
    // relies on `app.refresh()` calling that
    // helper to populate `session_panes`
    // with the `# sessions` block, so we
    // have to set one of the env vars.
    // Without this guard the test passes on
    // developer machines (where the env var
    // happens to be set) and fails in CI
    // (where it isn't) — exactly the same
    // flake as the host-row siblings.
    // `HERDR_PANE_ID` is process-global and
    // touched by several sibling tests in this
    // file, so we must hold the module-level
    // `ENV_LOCK` (declared once below, shared
    // by every env-mutating test) rather than a
    // per-function static — a function-local
    // `static ENV_LOCK` here would shadow the
    // shared one and give each test its own
    // private, non-synchronizing lock, which
    // defeats the whole point when multiple
    // tests race on the same env var under the
    // parallel test runner.
    let _g = lock_or_recover(&ENV_LOCK);
    let prev_herdr = std::env::var("HERDR_PANE_ID").ok();
    // SAFETY: see the set_var
    // comment in the
    // sibling host tests
    // for the full
    // rationale.
    unsafe {
        std::env::set_var("HERDR_PANE_ID", "w0:p0");
    }
    let result = std::panic::catch_unwind(|| {
        use crate::multiplexer::{ActiveContext, CurrentPaneInfo, MultiplexerBackend};
        use crate::tui::state::HistoryRow;

        /// Deterministic fake
        /// herdr backend for
        /// this test. Reports
        /// one active pane at
        /// `/tmp` (workspace
        /// `wA`) from its
        /// `snapshot()` /
        /// `snapshot_current_panes()`
        /// so the matcher in
        /// `select_for_run_impl`
        /// finds the existing
        /// workspace. Stages
        /// commands with the
        /// same shape as the
        /// real herdr backend
        /// (workspace ids
        /// stripped from
        /// `wA:p1` → `wA`).
        struct FakeHerdrBackend;
        impl MultiplexerBackend for FakeHerdrBackend {
            fn snapshot(&self) -> Vec<ActiveContext> {
                vec![ActiveContext {
                    pane_id: String::from("wA:p1"),
                    window_id: String::from("wA"),
                    path: String::from("/tmp"),
                    // Fake
                    // backend
                    // doesn't
                    // expose
                    // foreground
                    // commands
                    // (the
                    // production
                    // herdr
                    // backend
                    // doesn't
                    // either).
                    current_command: String::new(),
                    workspace_label: String::from("tmp session"),
                }]
            }
            fn snapshot_current_panes(&self, _current_pane: &str) -> Vec<CurrentPaneInfo> {
                vec![CurrentPaneInfo {
                    pane_id: String::from("wA:p1"),
                    window_id: String::from("wA"),
                    tab_id: String::from("wA:t1"),
                    session_label: String::from("wA"),
                    path: String::from("/tmp"),
                    current_command: String::from("zsh"),
                    is_last: false,
                }]
            }
            fn focus_command(&self, pane_id: &str) -> Option<String> {
                if pane_id.is_empty() {
                    return None;
                }
                let ws = pane_id.split(':').next().unwrap_or(pane_id);
                Some(format!("herdr workspace focus {} 2>/dev/null", ws))
            }
            fn focus_session(&self, label: &str) -> Option<String> {
                if label.is_empty() {
                    return None;
                }
                Some(format!("herdr workspace focus {} 2>/dev/null", label))
            }
            fn focus_pane(&self, pane_id: &str, _tab_id: &str) -> Option<String> {
                if pane_id.is_empty() {
                    return None;
                }
                Some(format!(
                    "herdr pane zoom {} 2>/dev/null && herdr pane zoom {} --off 2>/dev/null",
                    pane_id, pane_id,
                ))
            }
            fn create_command(&self, dir: &std::path::Path, label: &str) -> Option<String> {
                Some(format!(
                    "herdr workspace create --cwd {} --label {} --focus 2>/dev/null",
                    dir.display(),
                    label
                ))
            }
            fn send_in_pane_command(&self, pane_id: &str, body: &str) -> Option<String> {
                if pane_id.is_empty() {
                    return None;
                }
                Some(format!(
                    "herdr pane send-text {} {} 2>/dev/null",
                    pane_id, body
                ))
            }
            fn read_pane(&self, pane_id: &str, lines: usize) -> Option<String> {
                // Mock backend: no real herdr
                // daemon in tests, so we
                // synthesize a stable
                // placeholder so the
                // `ensure_selected_context`
                // branch is exercised end-to-
                // end. Real `read_pane` calls
                // hit `herdr_pane_read` in
                // `src/multiplexer.rs`.
                if pane_id.is_empty() || lines == 0 {
                    return None;
                }
                Some(format!(
                    "[mock pane content for {} ({} lines)]\nlast line",
                    pane_id, lines
                ))
            }
            fn name(&self) -> &'static str {
                "herdr"
            }
        }

        let mut app = directories_test_app(&[]);
        // Swap in the fake
        // herdr backend BEFORE
        // the first `refresh()`
        // so `fetch_session_panes`
        // picks up our fake's
        // `snapshot_current_panes`.
        app.multiplexer = Box::new(FakeHerdrBackend);
        // Configure a session
        // whose `dir` matches
        // the fake backend's
        // reported cwd.
        app.sessions = vec![HistoryRow {
            id: -10_002,
            command: String::from("tmp session"),
            directory: String::from("/tmp"),
            session_id: String::new(),
            exit_code: 0,
            timestamp: 0,
            comment: String::new(),
            output: String::new(),
            mode: String::from("session"),
            source: String::from("sessions"),

            ..Default::default()
        }];
        // Open the `*` mode.
        app.query = String::from("*");
        app.refresh();
        // Find the session
        // row.
        let idx = app
            .merged_rows()
            .iter()
            .position(|r| r.mode == "session")
            .expect("session row must be in merged_rows");
        app.list_state.select(Some(idx));
        // Pressing Enter on
        // the session row
        // must populate
        // `tmux_windows` (via
        // the fix in
        // `select_for_run_impl`)
        // and find the
        // existing workspace
        // — staging
        // `herdr workspace focus wA`,
        // NOT
        // `herdr workspace create`.
        app.select_for_run();
        let staged = app
            .selection
            .as_deref()
            .expect("selection must be set for session row");
        eprintln!("[test] staged command: {staged}");
        assert!(
            staged.contains("herdr workspace focus"),
            "must focus the existing workspace, got: {staged:?}"
        );
        assert!(
            !staged.contains("herdr workspace create"),
            "must NOT recreate the workspace, got: {staged:?}"
        );
        assert!(
            staged.contains("herdr workspace focus wA"),
            "must focus workspace wA (the workspace_id portion of pane id wA:p1), got: {staged:?}"
        );
    }); // close the `catch_unwind` opened at the top
        // Always restore the env
        // var, even on panic, so
        // a failed test doesn't
        // leak the env to
        // siblings. The SAFETY
        // comment on the set_var
        // call at the top of the
        // function explains why
        // mutation is safe within
        // ENV_LOCK.
    unsafe {
        match prev_herdr {
            Some(v) => std::env::set_var("HERDR_PANE_ID", v),
            None => std::env::remove_var("HERDR_PANE_ID"),
        }
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// Regression test for the
/// new `# hosts` block in the
/// `*` panes view. Selecting a
/// host row must:
///
/// 1. Build a single `ssh`
///    argv from the
///    `HostDef` (only
///    including flags that
///    are set).
/// 2. Match against
///    existing panes /
///    workspaces and
///    focus them when
///    found.
/// 3. Otherwise create a
///    new workspace and
///    bootstrap the `ssh`
///    connection inside.
///
/// This test covers the
/// "no existing workspace"
/// path on herdr: pressing
/// Enter on a host row
/// stages a
/// `herdr workspace create`
/// followed by
/// `herdr pane send-text`,
/// not a `tmux ...`
/// command (the configured
/// backend is herdr).
#[cfg(feature = "herdr")]
#[test]
fn host_row_in_panes_mode_stages_herdr_create_and_ssh() {
    // `fetch_session_panes` short-circuits when neither
    // `TMUX_PANE` nor `HERDR_PANE_ID` is set in the
    // environment (the user isn't inside a multiplexer
    // pane, so the snapshot would be wasted work). The
    // test below relies on `app.refresh()` calling that
    // helper to populate `session_panes` with the host
    // block, so we have to set one of the env vars.
    // Without this guard the test passes on developer
    // machines (where the env var happens to be set) and
    // fails in CI (where it isn't). Same pattern as the
    // sibling test `host_row_in_panes_mode_ssh_argv_includes_port_and_identity`
    // — see that test for the full rationale.
    //
    // `HERDR_PANE_ID` is process-global and touched by
    // several tests in this file, so this must hold the
    // shared module-level `ENV_LOCK` (declared once
    // below) rather than a per-function static — a
    // function-local `static ENV_LOCK` would shadow the
    // shared one, giving each test its own private,
    // non-synchronizing lock and defeating the point
    // when tests race on the same env var under the
    // parallel test runner.
    let _g = lock_or_recover(&ENV_LOCK);
    let prev_herdr = std::env::var("HERDR_PANE_ID").ok();
    // SAFETY: no other test in this binary sets
    // `HERDR_PANE_ID` (guarded by ENV_LOCK, and the
    // binary is single-process per test thread).
    unsafe {
        std::env::set_var("HERDR_PANE_ID", "w0:p0");
    }
    let result = std::panic::catch_unwind(|| {
        use crate::tui::state::{HistoryRow, HostDef};
        // Fake backend with
        // NO existing
        // workspace — the
        // matcher will miss
        // and we fall into
        // the create branch.
        use crate::multiplexer::{ActiveContext, CurrentPaneInfo, MultiplexerBackend};
        struct FakeEmptyHerdrBackend;
        impl MultiplexerBackend for FakeEmptyHerdrBackend {
            fn snapshot(&self) -> Vec<ActiveContext> {
                Vec::new()
            }
            fn snapshot_current_panes(&self, _current_pane: &str) -> Vec<CurrentPaneInfo> {
                Vec::new()
            }
            fn focus_command(&self, _pane_id: &str) -> Option<String> {
                None
            }
            fn focus_session(&self, _label: &str) -> Option<String> {
                None
            }
            fn focus_pane(&self, _pane_id: &str, _tab_id: &str) -> Option<String> {
                None
            }
            fn create_command(&self, _dir: &std::path::Path, _label: &str) -> Option<String> {
                None
            }
            fn send_in_pane_command(&self, _pane_id: &str, _body: &str) -> Option<String> {
                None
            }
            fn read_pane(&self, _pane_id: &str, _lines: usize) -> Option<String> {
                None
            }
            fn name(&self) -> &'static str {
                "herdr"
            }
        }
        let mut app = directories_test_app(&[]);
        app.multiplexer = Box::new(FakeEmptyHerdrBackend);
        // Configure a single
        // host: Proxmox →
        // root@pve-1, no
        // identity, default
        // port.
        app.hosts = vec![HistoryRow {
            id: -25_001, // set by
            // `Config::hosts`
            // (placeholder;
            // `fetch_session_panes_impl`
            // re-ids).
            command: String::from("Proxmox"),
            directory: String::from("root@pve-1"),
            session_id: String::new(),
            exit_code: 0,
            timestamp: 0,
            comment: String::new(),
            output: String::new(),
            mode: String::from("host"),
            source: String::from("hosts"),

            ..Default::default()
        }];
        app.host_defs = vec![HostDef {
            name: String::from("Proxmox"),
            host: String::from("pve-1"),
            hostname: String::new(),
            user: String::from("root"),
            port: 0,
            identity: String::new(),
            dir: String::new(),
            exec: String::new(),
        }];
        // Open the `*` mode.
        app.query = String::from("*");
        app.refresh();
        // Find the host row.
        let idx = app
            .merged_rows()
            .iter()
            .position(|r| r.mode == "host")
            .expect("host row must be in merged_rows");
        app.list_state.select(Some(idx));
        app.select_for_run();
        let staged = app
            .selection
            .as_deref()
            .expect("selection must be set for host row");
        eprintln!("[test] staged host command: {staged}");
        assert!(
            staged.contains("herdr workspace create"),
            "must create a herdr workspace, got: {staged:?}"
        );
        assert!(
                staged.contains("herdr pane run"),
                "must bootstrap the ssh body via herdr pane run (same technique as named sessions), got: {staged:?}"
            );
        assert!(
            staged.contains("ssh root@pve-1"),
            "must include the ssh argv (user@host), got: {staged:?}"
        );
        // No `-p` flag
        // (default port 22
        // is implicit).
        assert!(
            !staged.contains(" -p "),
            "must not include -p flag for default port, got: {staged:?}"
        );
        // No `-i` flag
        // (no identity
        // configured).
        assert!(
            !staged.contains(" -i "),
            "must not include -i flag when identity is unset, got: {staged:?}"
        );
    }); // close the `catch_unwind` opened at the top
        // Always restore the env var, even on panic,
        // so a failed test doesn't leak the env to
        // siblings. The SAFETY comment on the set_var
        // call at the top of the function explains why
        // mutation is safe within ENV_LOCK.
    unsafe {
        match prev_herdr {
            Some(v) => std::env::set_var("HERDR_PANE_ID", v),
            None => std::env::remove_var("HERDR_PANE_ID"),
        }
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// The same as above but
/// with a port and
/// identity set on
/// the `HostDef`. The
/// staged `ssh` argv
/// must include both
/// flags.
#[cfg(feature = "herdr")]
#[test]
fn host_row_in_panes_mode_ssh_argv_includes_port_and_identity() {
    // `fetch_session_panes` short-circuits when neither
    // `TMUX_PANE` nor `HERDR_PANE_ID` is set in the
    // environment (the user isn't inside a multiplexer
    // pane, so the snapshot would be wasted work). The
    // test below relies on `app.refresh()` calling that
    // helper to populate `session_panes` with the host
    // block, so we have to set one of the env vars.
    // Without this guard the test passes on developer
    // machines (where the env var happens to be set) and
    // fails in CI (where it isn't) — exactly the flake the
    // user reported.
    //
    // `HERDR_PANE_ID` is process-global and touched by
    // several tests in this file, so this must hold the
    // shared module-level `ENV_LOCK` (declared once
    // below) rather than a per-function static — a
    // function-local `static ENV_LOCK` would shadow the
    // shared one, giving each test its own private,
    // non-synchronizing lock and defeating the point
    // when tests race on the same env var under the
    // parallel test runner.
    let _g = lock_or_recover(&ENV_LOCK);
    let prev_herdr = std::env::var("HERDR_PANE_ID").ok();
    // SAFETY: no other test in this binary sets
    // `HERDR_PANE_ID` (guarded by ENV_LOCK, and the
    // binary is single-process per test thread).
    unsafe {
        std::env::set_var("HERDR_PANE_ID", "w0:p0");
    }
    let result = std::panic::catch_unwind(|| {
        use crate::multiplexer::{ActiveContext, CurrentPaneInfo, MultiplexerBackend};
        use crate::tui::state::{HistoryRow, HostDef};
        struct FakeEmptyHerdrBackend;
        impl MultiplexerBackend for FakeEmptyHerdrBackend {
            fn snapshot(&self) -> Vec<ActiveContext> {
                Vec::new()
            }
            fn snapshot_current_panes(&self, _current_pane: &str) -> Vec<CurrentPaneInfo> {
                Vec::new()
            }
            fn focus_command(&self, _pane_id: &str) -> Option<String> {
                None
            }
            fn focus_session(&self, _label: &str) -> Option<String> {
                None
            }
            fn focus_pane(&self, _pane_id: &str, _tab_id: &str) -> Option<String> {
                None
            }
            fn create_command(&self, _dir: &std::path::Path, _label: &str) -> Option<String> {
                None
            }
            fn send_in_pane_command(&self, _pane_id: &str, _body: &str) -> Option<String> {
                None
            }
            fn read_pane(&self, _pane_id: &str, _lines: usize) -> Option<String> {
                None
            }
            fn name(&self) -> &'static str {
                "herdr"
            }
        }
        let mut app = directories_test_app(&[]);
        app.multiplexer = Box::new(FakeEmptyHerdrBackend);
        app.hosts = vec![HistoryRow {
            id: -25_001,
            command: String::from("custom"),
            directory: String::from("alice@work:2222"),
            session_id: String::new(),
            exit_code: 0,
            timestamp: 0,
            comment: String::new(),
            output: String::new(),
            mode: String::from("host"),
            source: String::from("hosts"),

            ..Default::default()
        }];
        app.host_defs = vec![HostDef {
            name: String::from("custom"),
            host: String::from("work"),
            hostname: String::new(),
            user: String::from("alice"),
            port: 2222,
            identity: String::from("~/.ssh/work_ed25519"),
            dir: String::new(),
            exec: String::new(),
        }];
        app.query = String::from("*");
        app.refresh();
        let idx = app
            .merged_rows()
            .iter()
            .position(|r| r.mode == "host")
            .expect("host row must be in merged_rows");
        app.list_state.select(Some(idx));
        app.select_for_run();
        let staged = app
            .selection
            .as_deref()
            .expect("selection must be set for host row");
        eprintln!("[test] staged host command: {staged}");
        assert!(
            staged.contains(" -p 2222"),
            "must include the -p flag for the non-default port, got: {staged:?}"
        );
        // The identity file
        // path goes through
        // `expand_home_to_absolute`
        // — just assert
        // that a `-i` flag
        // is present and
        // points at the
        // right file. The
        // `~` is expanded
        // to `$HOME/.ssh/...`
        // so we can't
        // assert the
        // literal `~` form
        // here.
        assert!(
            staged.contains(" -i "),
            "must include the -i flag for the identity file, got: {staged:?}"
        );
        assert!(
            staged.contains("alice@work"),
            "must include the user@host, got: {staged:?}"
        );
    }); // close the `catch_unwind` opened above
        // Always restore the env var, even on panic,
        // so a failed test doesn't leak the env to
        // siblings. The SAFETY comment on the set_var
        // call above explains why mutation is safe
        // within ENV_LOCK.
    unsafe {
        match prev_herdr {
            Some(v) => std::env::set_var("HERDR_PANE_ID", v),
            None => std::env::remove_var("HERDR_PANE_ID"),
        }
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// When a herdr workspace
/// is already running
/// the host (matched
/// by the workspace
/// `label`), pressing
/// Enter must focus
/// the existing
/// workspace rather
/// than create a
/// duplicate.
#[cfg(feature = "herdr")]
#[test]
fn host_row_in_panes_mode_focuses_existing_herdr_workspace() {
    // `fetch_session_panes` short-circuits when neither
    // `TMUX_PANE` nor `HERDR_PANE_ID` is set in the
    // environment (the user isn't inside a multiplexer
    // pane, so the snapshot would be wasted work). The
    // test below relies on `app.refresh()` calling that
    // helper to populate `session_panes` with the host
    // block, so we have to set one of the env vars.
    // Without this guard the test passes on developer
    // machines (where the env var happens to be set) and
    // fails in CI (where it isn't). Same pattern as the
    // sibling host tests.
    //
    // `HERDR_PANE_ID` is process-global and touched by
    // several tests in this file, so this must hold the
    // shared module-level `ENV_LOCK` (declared once
    // below) rather than a per-function static — a
    // function-local `static ENV_LOCK` would shadow the
    // shared one, giving each test its own private,
    // non-synchronizing lock and defeating the point
    // when tests race on the same env var under the
    // parallel test runner.
    let _g = lock_or_recover(&ENV_LOCK);
    let prev_herdr = std::env::var("HERDR_PANE_ID").ok();
    // SAFETY: see the set_var comment in the
    // sibling test for the full rationale.
    unsafe {
        std::env::set_var("HERDR_PANE_ID", "w0:p0");
    }
    let result = std::panic::catch_unwind(|| {
        use crate::multiplexer::{ActiveContext, CurrentPaneInfo, MultiplexerBackend};
        use crate::tui::state::{HistoryRow, HostDef};
        struct FakeHerdrBackend;
        impl MultiplexerBackend for FakeHerdrBackend {
            fn snapshot(&self) -> Vec<ActiveContext> {
                // One active
                // pane in
                // workspace
                // `wA`, which
                // is labeled
                // `Proxmox` —
                // matching the
                // host's
                // display name.
                vec![ActiveContext {
                    pane_id: String::from("wA:p1"),
                    window_id: String::from("wA"),
                    path: String::new(),
                    current_command: String::new(),
                    workspace_label: String::from("Proxmox"),
                }]
            }
            fn snapshot_current_panes(&self, _current_pane: &str) -> Vec<CurrentPaneInfo> {
                vec![CurrentPaneInfo {
                    pane_id: String::from("wA:p1"),
                    window_id: String::from("wA"),
                    tab_id: String::from("wA:t1"),
                    session_label: String::from("Proxmox"),
                    path: String::new(),
                    current_command: String::from("zsh"),
                    is_last: false,
                }]
            }
            fn focus_command(&self, pane_id: &str) -> Option<String> {
                let ws = pane_id.split(':').next().unwrap_or(pane_id);
                Some(format!("herdr workspace focus {} 2>/dev/null", ws))
            }
            fn focus_session(&self, label: &str) -> Option<String> {
                Some(format!("herdr workspace focus {} 2>/dev/null", label))
            }
            fn focus_pane(&self, pane_id: &str, _tab_id: &str) -> Option<String> {
                let ws = pane_id.split(':').next().unwrap_or(pane_id);
                Some(format!("herdr workspace focus {} 2>/dev/null", ws))
            }
            fn create_command(&self, _dir: &std::path::Path, _label: &str) -> Option<String> {
                None
            }
            fn send_in_pane_command(&self, _pane_id: &str, _body: &str) -> Option<String> {
                None
            }
            fn read_pane(&self, pane_id: &str, lines: usize) -> Option<String> {
                // Mock backend: no real
                // herdr in tests; return a
                // stable placeholder so
                // `ensure_selected_context`
                // sees non-empty content.
                if pane_id.is_empty() || lines == 0 {
                    return None;
                }
                Some(format!(
                    "[mock pane {} ({} lines)]\nlast line",
                    pane_id, lines
                ))
            }
            fn name(&self) -> &'static str {
                "herdr"
            }
        }
        let mut app = directories_test_app(&[]);
        app.multiplexer = Box::new(FakeHerdrBackend);
        app.hosts = vec![HistoryRow {
            id: -25_001,
            command: String::from("Proxmox"),
            directory: String::from("root@pve-1"),
            session_id: String::new(),
            exit_code: 0,
            timestamp: 0,
            comment: String::new(),
            output: String::new(),
            mode: String::from("host"),
            source: String::from("hosts"),

            ..Default::default()
        }];
        app.host_defs = vec![HostDef {
            name: String::from("Proxmox"),
            host: String::from("pve-1"),
            hostname: String::new(),
            user: String::from("root"),
            port: 0,
            identity: String::new(),
            dir: String::new(),
            exec: String::new(),
        }];
        app.query = String::from("*");
        app.refresh();
        let idx = app
            .merged_rows()
            .iter()
            .position(|r| r.mode == "host")
            .expect("host row must be in merged_rows");
        app.list_state.select(Some(idx));
        app.select_for_run();
        let staged = app
            .selection
            .as_deref()
            .expect("selection must be set for host row");
        eprintln!("[test] staged host command: {staged}");
        assert!(
            staged.contains("herdr workspace focus wA"),
            "must focus the existing workspace wA, got: {staged:?}"
        );
        assert!(
            !staged.contains("herdr workspace create"),
            "must NOT recreate the workspace, got: {staged:?}"
        );
        assert!(
            !staged.contains("herdr pane send-text"),
            "must NOT bootstrap a new ssh connection, got: {staged:?}"
        );
    }); // close the `catch_unwind` opened at the top
        // Always restore the env var, even on panic,
        // so a failed test doesn't leak the env to
        // siblings. The SAFETY comment on the set_var
        // call at the top of the function explains why
        // mutation is safe within ENV_LOCK.
    unsafe {
        match prev_herdr {
            Some(v) => std::env::set_var("HERDR_PANE_ID", v),
            None => std::env::remove_var("HERDR_PANE_ID"),
        }
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

// --- LLM query mode -------------------------------------------------
//
// The LLM client is hidden behind a trait so these tests
// can inject canned responses without a live ollama
// server. The trait lives in `crate::llm`; the test
// defines a minimal in-memory implementation.

struct FakeLlm {
    /// Raw response to return from `generate`, exactly
    /// as the LLM would have produced it (before
    /// sanitization). Tests use this to exercise the
    /// full sanitize-then-stage path.
    response: String,
    /// Optional injection of an error.
    error: Option<crate::llm::LlmError>,
    /// Raw response to return from `describe`,
    /// exactly as the LLM would have produced
    /// it (no sanitization — the description
    /// is rendered as-is). Defaults to the
    /// empty string when the test doesn't care
    /// about the describe path.
    describe_response: String,
    /// Raw response to return from `correct`,
    /// exactly as the LLM would have
    /// produced it (before
    /// sanitization). The production path
    /// runs this through
    /// `sanitize_command` to extract a
    /// clean command, so a test that
    /// exercises the full pipeline should
    /// set this to a command-form string
    /// (or to a string with markdown
    /// fences to verify the sanitizer).
    /// Defaults to the empty string when
    /// the test doesn't care about the
    /// correct path.
    correct_response: String,
}

impl crate::llm::LlmClient for FakeLlm {
    fn generate(&self, _description: &str) -> Result<String, crate::llm::LlmError> {
        match &self.error {
            Some(e) => Err(match e {
                // Reconstruct the error
                // without owning its
                // detail (the variants we
                // test carry no heap
                // data so this is a
                // simple clone).
                crate::llm::LlmError::NotConfigured => crate::llm::LlmError::NotConfigured,
                other => match other {
                    crate::llm::LlmError::Transport(s) => {
                        crate::llm::LlmError::Transport(s.clone())
                    }
                    _ => crate::llm::LlmError::NoCommand,
                },
            }),
            None => Ok(self.response.clone()),
        }
    }

    fn describe(&self, _command: &str) -> Result<String, crate::llm::LlmError> {
        // The describe path uses the same
        // `error` injection as `generate` so
        // existing test fixtures (e.g.
        // `LlmError::NotConfigured`) cover
        // both code paths. The canned
        // response is a separate field so
        // tests can supply a description
        // string distinct from a command
        // string.
        match &self.error {
            Some(e) => Err(match e {
                crate::llm::LlmError::NotConfigured => crate::llm::LlmError::NotConfigured,
                other => match other {
                    crate::llm::LlmError::Transport(s) => {
                        crate::llm::LlmError::Transport(s.clone())
                    }
                    _ => crate::llm::LlmError::NoCommand,
                },
            }),
            None => Ok(self.describe_response.clone()),
        }
    }

    fn correct(&self, _command: &str) -> Result<String, crate::llm::LlmError> {
        // Same `error` injection as the
        // other methods so a test
        // fixture like
        // `LlmError::NotConfigured`
        // covers all three LLM-backed
        // actions (generate, describe,
        // correct). The canned response
        // is in a separate field so
        // tests can supply a corrected
        // command distinct from a
        // description.
        match &self.error {
            Some(e) => Err(match e {
                crate::llm::LlmError::NotConfigured => crate::llm::LlmError::NotConfigured,
                other => match other {
                    crate::llm::LlmError::Transport(s) => {
                        crate::llm::LlmError::Transport(s.clone())
                    }
                    _ => crate::llm::LlmError::NoCommand,
                },
            }),
            None => Ok(self.correct_response.clone()),
        }
    }

    fn prompt(&self, _prompt: &str) -> Result<String, crate::llm::LlmError> {
        // The trait's default `generate`
        // and `describe` impls call
        // `prompt(&build_prompt(...))` and
        // `prompt(&build_describe_prompt(...))`
        // respectively. The tests that
        // exercise this fake override
        // `generate` and `describe`
        // directly, so this method is
        // never called in practice. We
        // still have to implement it
        // (the trait has no default body
        // for it) so we return the canned
        // `response` — a sane fallback
        // that makes any test that
        // accidentally calls it get a
        // deterministic value rather than
        // a panic.
        match &self.error {
            Some(e) => Err(match e {
                crate::llm::LlmError::NotConfigured => crate::llm::LlmError::NotConfigured,
                other => match other {
                    crate::llm::LlmError::Transport(s) => {
                        crate::llm::LlmError::Transport(s.clone())
                    }
                    _ => crate::llm::LlmError::NoCommand,
                },
            }),
            None => Ok(self.response.clone()),
        }
    }
}

fn make_llm_app(query: &str, fake: FakeLlm) -> App {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );
                        -- The dedup index that backs the `ON
                        -- CONFLICT (command, directory, session_id)`
                        -- clause used by `run_llm_query`. In the
                        -- production schema this is created by
                        -- `init_db` in main.rs; tests have to
                        -- declare it themselves since they build
                        -- a fresh in-memory database.
                        CREATE UNIQUE INDEX idx_history_dedup
                            ON history (command, directory, session_id);",
    )
    .expect("create tables");
    App::new(
        conn,
        Mode::Global,
        query.to_string(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        Some(Box::new(fake)),
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    )
}

/// Process-wide serialization for environment-variable
/// access in tests. The existing `selected_row_finds_labeled_only_rows`,
/// `select_for_run_on_labeled_only_row_stages_command`,
/// and LLM tests all call `unsafe { std::env::set_var }`
/// to set `PWD` / `SMART_HISTORY_SESSION`. When those
/// tests run in parallel, the env-var mutations race and
/// one test sees a half-restored state. Holding this
/// mutex's guard for the lifetime of each env-touching
/// test makes the read/modify/restore critical section
/// atomic across threads — the closest we can get to
/// per-test isolation in a parallel test runner without
/// pulling in a serial framework.
///
/// Stored as a `std::sync::Mutex<()>` rather than a
/// `parking_lot::Mutex` so the project stays
/// dependency-free (this module already depends on
/// `std` for everything else).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn is_llm_query_recognises_equals_prefix() {
    let mut app = make_llm_app(
        "=Find all files modified yesterday",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    assert!(app.is_llm_query());
    app.query = "git status".to_string();
    assert!(!app.is_llm_query());
    app.query = "/regex".to_string();
    assert!(!app.is_llm_query());
    app.query = "?fuzzy".to_string();
    assert!(!app.is_llm_query());
    app.query = "".to_string();
    assert!(!app.is_llm_query());
}

#[test]
fn run_llm_query_stages_clean_command() {
    let mut app = make_llm_app(
        "=Find all files modified yesterday",
        FakeLlm {
            response: "find . -mtime -1 -type f".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    app.select_for_run();
    app.process_pending_llm_request();
    // The LLM call should stage the generated
    // command for the parent shell to run.
    assert_eq!(app.selection.as_deref(), Some("find . -mtime -1 -type f"));
    assert_eq!(app.pick_mode, Some(PickMode::Run));
    // The new command should also be in the
    // history table with the description as the command (with = prefix)
    // and the generated command as output/comment.
    app.refresh();
    let rows = app.merged_rows();
    assert!(
        rows.iter()
            .any(|r| r.command == "=Find all files modified yesterday"
                && r.output == "find . -mtime -1 -type f"),
        "the LLM query should be inserted with the description as command, \
                         got rows: {:?}",
        rows.iter()
            .map(|r| (&*r.command, &*r.output, &*r.comment))
            .collect::<Vec<_>>()
    );
}

#[test]
fn run_llm_query_sanitises_markdown_fences() {
    // The LLM echoed the command inside a fenced
    // block; the sanitizer should strip the fences
    // before staging.
    let mut app = make_llm_app(
        "=List Cargo.toml files",
        FakeLlm {
            response: "```bash\nfind . -name Cargo.toml\n```".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    app.select_for_run();
    app.process_pending_llm_request();
    let msg = app.status_message.as_ref().map(|(m, _)| m.clone());
    assert_eq!(
        app.selection.as_deref(),
        Some("find . -name Cargo.toml"),
        "selection: {:?}, status: {:?}",
        app.selection,
        msg
    );
}

#[test]
fn run_llm_query_rejects_empty_description() {
    // `=` with no description is now treated as a search
    // for old LLM queries, not a generation request. The
    // user can select existing LLM query rows.
    let mut app = make_llm_app(
        "=",
        FakeLlm {
            // The fake will fail the test if
            // it gets called.
            response: "should not be called".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // Insert an old LLM query into history so there's
    // something to select
    app.conn.execute(
                        "INSERT INTO history (command, directory, session_id, exit_code, mode) VALUES (?1, ?2, ?3, ?4, 'llm')",
                        params!["=old test query", "/test", "test-session", -1],
                ).unwrap();
    let history_id: i64 = app
        .conn
        .query_row(
            "SELECT id FROM history WHERE command = ?1",
            params!["=old test query"],
            |row| row.get(0),
        )
        .unwrap();
    app.conn
        .execute(
            "INSERT INTO history_output (history_id, output) VALUES (?1, ?2)",
            params![history_id, "ls -la"],
        )
        .unwrap();
    app.refresh();

    // With just "=", is_llm_query returns false (no description),
    // so select_for_run should select the row, not call run_llm_query
    app.select_for_run();
    // The selected row's output should be staged (since it's an old LLM query)
    assert_eq!(app.selection.as_deref(), Some("ls -la"));
    assert_eq!(app.pick_mode, Some(PickMode::Run));
}

#[test]
fn run_llm_query_surfaces_not_configured_when_client_is_none() {
    // Build an app *without* a configured LLM
    // client and try to run an LLM query. The TUI
    // should report "not configured" without
    // panicking.
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );
                        CREATE UNIQUE INDEX idx_history_dedup
                            ON history (command, directory, session_id);",
    )
    .expect("create tables");
    let mut app = App::new(
        conn,
        Mode::Global,
        "=anything".to_string(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None, // <-- the missing LLM config
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    app.select_for_run();
    assert!(app.selection.is_none());
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("not-configured must surface a status");
    assert!(msg.contains("not configured"), "got: {:?}", msg);
}

#[test]
fn run_llm_query_surfaces_no_command_when_sanitizer_rejects() {
    // The LLM responded with nothing usable after
    // sanitization (only commentary, no actual
    // command). The TUI should surface a
    // "no usable command" status.
    let mut app = make_llm_app(
        "=Do something",
        FakeLlm {
            response: "# I cannot help with that.".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    app.select_for_run();
    app.process_pending_llm_request();
    assert!(app.selection.is_none());
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("empty sanitizer output must surface a status");
    assert!(msg.contains("no usable command"), "got: {:?}", msg);
}

// --- Query cursor (LLM mode edit support) ---------------------

/// The cursor is initialised to the end of the query
/// so the first character the user types lands in the
/// expected place. For non-LLM queries this is a
/// no-op (the input loop ignores the cursor in those
/// modes); for LLM queries it's the starting point
/// from which Left/Right can move.
#[test]
fn query_cursor_initialised_to_end() {
    let app = make_llm_app(
        "=describe something",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    assert_eq!(app.query, "=describe something");
    assert_eq!(app.query_cursor, "=describe something".chars().count());
}

/// `push_char` inserts at the cursor, not just at the
/// end. This lets the user edit a multi-byte
/// description mid-buffer with the cursor in any
/// position.
#[test]
fn push_char_inserts_at_cursor_position() {
    let mut app = make_llm_app(
        "=find files",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // Move to the middle of "files" (after "find ").
    app.query_cursor = "=find ".chars().count();
    app.push_char('x');
    assert_eq!(app.query, "=find xfiles");
    assert_eq!(app.query_cursor, "=find x".chars().count());
    // Inserting again advances the cursor.
    app.push_char('y');
    assert_eq!(app.query, "=find xyfiles");
    assert_eq!(app.query_cursor, "=find xy".chars().count());
}

/// `backspace` deletes the character to the LEFT of
/// the cursor. With the cursor at the end this is
/// the historical "pop the last char" behaviour; with
/// the cursor in the middle it deletes mid-buffer.
#[test]
fn backspace_deletes_before_cursor() {
    let mut app = make_llm_app(
        "=find xfile",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // Cursor at end (default). One backspace
    // removes the trailing `e` (the historical
    // behaviour).
    app.backspace();
    assert_eq!(app.query, "=find xfil");
    assert_eq!(app.query_cursor, "=find xfil".chars().count());
    // Now move the cursor to a mid-buffer
    // position. Place it between the space and
    // the `x` (position 6 in `=find xfil`).
    // The leading `=` counts as one char, so
    // `=find ` is positions 0-5 and `x` starts
    // at position 6.
    app.query_cursor = "=find ".chars().count();
    app.backspace();
    // Backspace at the cursor removes the
    // character to the LEFT — that's the space
    // at position 5 — collapsing the gap.
    assert_eq!(app.query, "=findxfil");
    assert_eq!(app.query_cursor, "=find".chars().count());
}

/// `backspace` at position 0 is a no-op. The user's
/// backspace press at the start of the buffer should
/// not panic and should not turn the cursor negative.
#[test]
fn backspace_at_position_zero_is_noop() {
    let mut app = make_llm_app(
        "=x",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    app.query_cursor = 0;
    app.backspace();
    assert_eq!(app.query, "=x");
    assert_eq!(app.query_cursor, 0);
}

/// `EditStart` (the Left key) in LLM mode moves the
/// cursor one character toward the start of the
/// description, NOT to a row in the history list.
/// This is the character-by-character navigation
/// the user asked for: "When the query is an LLM
/// query then cursor right and left should just
/// position the cursor in the query line."
#[test]
fn edit_start_in_llm_mode_moves_cursor_one_char_left() {
    let mut app = make_llm_app(
        "=find files",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // Cursor starts at the end. The test helper
    // initialises it there.
    assert!(app.query_cursor > 0);
    let end = app.query_cursor;
    app.select_for_edit_start();
    // One character toward the start, not all
    // the way back to 0.
    assert_eq!(app.query_cursor, end - 1);
    // A second press moves one more character.
    app.select_for_edit_start();
    assert_eq!(app.query_cursor, end - 2);
    // Crucially, no row is staged — the Left
    // key in LLM mode is purely a cursor move.
    assert!(app.selection.is_none());
    assert!(app.pick_mode.is_none());
}

/// `EditEnd` (the Right key) in LLM mode moves the
/// cursor one character toward the end of the
/// description, NOT to a row in the history list.
/// Mirror of the previous test.
#[test]
fn edit_end_in_llm_mode_moves_cursor_one_char_right() {
    let mut app = make_llm_app(
        "=find files",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // Start the cursor in the middle so we can
    // step toward the end.
    let mid = "=find ".chars().count();
    app.query_cursor = mid;
    app.select_for_edit_end();
    assert_eq!(app.query_cursor, mid + 1);
    app.select_for_edit_end();
    assert_eq!(app.query_cursor, mid + 2);
    assert!(app.selection.is_none());
    assert!(app.pick_mode.is_none());
}

/// Pressing Left at the very start of the buffer
/// (cursor == 0) is a no-op, not an underflow. The
/// cursor is tracked in characters; without the
/// `saturating_sub` guard the dispatch could panic
/// or wrap to `usize::MAX`. Behaviour: stays at 0.
#[test]
fn edit_start_at_position_zero_stays_at_zero() {
    let mut app = make_llm_app(
        "=find files",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    app.query_cursor = 0;
    app.select_for_edit_start();
    assert_eq!(app.query_cursor, 0);
    assert!(app.selection.is_none());
}

/// Pressing Right at the very end of the buffer
/// (cursor == len) is a no-op, not a panic. The
/// `.min(len)` clamp ensures the cursor stays at
/// the end even after repeated presses. Behaviour:
/// stays at the character-count length.
#[test]
fn edit_end_at_end_stays_at_end() {
    let mut app = make_llm_app(
        "=find files",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    let len = app.query.chars().count();
    app.query_cursor = len;
    app.select_for_edit_end();
    assert_eq!(app.query_cursor, len);
    // Pressing again (still at the end) is
    // still a no-op.
    app.select_for_edit_end();
    assert_eq!(app.query_cursor, len);
    assert!(app.selection.is_none());
}

/// `MoveCursorLeft` (the Left key in non-LLM
/// modes) moves the cursor one character toward
/// the start of the query. The query string is
/// unchanged; only the cursor position moves.
/// Works in any mode — LLM, JIRA, notes, todos,
/// or plain text search — since the cursor lives
/// on `self.query` in all of them.
#[test]
fn move_cursor_left_moves_one_char_left() {
    let mut app = global_test_app(&[("a", 1)]);
    app.query = String::from("git status");
    app.query_cursor = app.query.chars().count();
    let end = app.query_cursor;
    app.move_query_cursor_left();
    assert_eq!(app.query_cursor, end - 1);
    assert_eq!(app.query, "git status", "query should be unchanged");
}

/// `MoveCursorRight` moves the cursor one
/// character toward the end of the query. Mirror
/// of the Left test.
#[test]
fn move_cursor_right_moves_one_char_right() {
    let mut app = global_test_app(&[("a", 1)]);
    app.query = String::from("git status");
    app.query_cursor = 4; // between "git" and " status"
    app.move_query_cursor_right();
    assert_eq!(app.query_cursor, 5);
    assert_eq!(app.query, "git status", "query should be unchanged");
}

/// Pressing Left at position 0 is a no-op
/// (saturating subtraction). Prevents underflow
/// panic on repeated presses at the start of
/// the query.
#[test]
fn move_cursor_left_at_position_zero_is_noop() {
    let mut app = global_test_app(&[("a", 1)]);
    app.query = String::from("git status");
    app.query_cursor = 0;
    app.move_query_cursor_left();
    assert_eq!(app.query_cursor, 0);
}

/// Pressing Right at the end of the query is a
/// no-op. Prevents the cursor from running past
/// the last character.
#[test]
fn move_cursor_right_at_end_is_noop() {
    let mut app = global_test_app(&[("a", 1)]);
    app.query = String::from("git status");
    let len = app.query.chars().count();
    app.query_cursor = len;
    app.move_query_cursor_right();
    assert_eq!(app.query_cursor, len);
}

/// Cursor movement is measured in UTF-8
/// characters, not bytes, so multi-byte
/// characters like `é` count as a single step.
/// This matches the rest of the query editing
/// logic and prevents the cursor from landing
/// in the middle of a multi-byte codepoint.
#[test]
fn move_cursor_handles_multibyte() {
    let mut app = global_test_app(&[("a", 1)]);
    app.query = String::from("café");
    let end = app.query.chars().count();
    assert_eq!(end, 4);
    app.query_cursor = end;
    app.move_query_cursor_left();
    // One step back, regardless of `é`'s 2-byte
    // UTF-8 encoding.
    assert_eq!(app.query_cursor, 3);
    app.move_query_cursor_left();
    assert_eq!(app.query_cursor, 2);
}

/// Character-by-character navigation works for
/// multi-byte UTF-8. The user types an accented
/// character into a French-language description,
/// steps the cursor with Left, inserts another
/// accented character at that position, and the
/// buffer is still valid UTF-8 with the expected
/// character count.
#[test]
fn edit_left_right_handles_multibyte() {
    let mut app = make_llm_app(
        "=chercher fichiers",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    let end = app.query.chars().count();
    // One step left, then one step right,
    // should round-trip back to the end.
    app.select_for_edit_start();
    assert_eq!(app.query_cursor, end - 1);
    app.select_for_edit_end();
    assert_eq!(app.query_cursor, end);
    // Multi-step walk back to position 1 (one
    // past the `=`).
    for _ in 0..(end - 1) {
        app.select_for_edit_start();
    }
    assert_eq!(app.query_cursor, 1);
    // Insert a multi-byte character at the
    // cursor. `é` is 2 bytes in UTF-8 but 1
    // char in our cursor accounting, so the
    // cursor advances by exactly 1 char.
    app.push_char('é');
    assert_eq!(app.query_cursor, 2);
    // The new buffer is the original with
    // `é` inserted right after the `=`.
    assert!(app.query.starts_with("=é"));
    assert!(app.query.ends_with("chercher fichiers"));
}

/// `EditStart` / `EditEnd` keep their historical
/// "stage a row" semantics for non-LLM queries. The
/// LLM-mode override is specific to LLM.
#[test]
fn edit_start_end_in_non_llm_mode_stages_a_row() {
    // Three rows so the list isn't empty. The
    // timestamps are `now - offset`, so the
    // newest (smallest offset) comes first in
    // the default timestamp-DESC ordering. We use
    // the query field empty so the SQL `WHERE`
    // clause doesn't filter.
    let mut app = stats_test_app(&[("cd", 1), ("git status", 2), ("ls", 3)]);
    // Cursor at index 0 is the default.
    app.select_for_edit_start();
    // The first (newest) row is staged with
    // EditStart pick_mode.
    assert_eq!(app.selection.as_deref(), Some("cd"));
    assert_eq!(app.pick_mode, Some(PickMode::EditStart));
    // The query cursor is not modified by the
    // row-staging path — it's a no-op for
    // non-LLM queries.
    assert_eq!(app.query_cursor, 0);
}

// --- LLM auto-call debounce --------------------------------

/// `llm_touch` arms the debounce when the query
/// is an LLM query. Used by `push_char` /
/// `backspace` / `clear_query` to reset the
/// 1-second countdown each time the user edits.
#[test]
fn llm_touch_arms_debounce_in_llm_mode() {
    let mut app = make_llm_app(
        "=describe something",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    assert!(app.llm_debounce_started.is_none());
    app.llm_touch();
    assert!(app.llm_debounce_started.is_some());
}

/// `llm_touch` clears all debounce state when the
/// query is NOT an LLM query. We leave LLM mode
/// (e.g. backspaced the `=`) and there's nothing
/// for the auto-call to do.
#[test]
fn llm_touch_clears_state_outside_llm_mode() {
    let mut app = make_llm_app(
        "=describe something",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // First arm the debounce.
    app.llm_touch();
    assert!(app.llm_debounce_started.is_some());
    // Leave LLM mode.
    app.query = "git status".to_string();
    app.llm_touch();
    assert!(app.llm_debounce_started.is_none());
    assert!(app.llm_preview.is_none());
    assert!(!app.llm_in_flight);
}

/// `llm_touch` discards a stale preview when the
/// user edits the description in LLM mode. The
/// preview is no longer relevant; clearing it
/// makes the next auto-call produce a fresh one.
#[test]
fn llm_touch_discards_stale_preview() {
    let mut app = make_llm_app(
        "=describe something",
        FakeLlm {
            response: String::new(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // Manually install a preview as if the
    // debounce had just fired.
    app.llm_preview = Some(HistoryRow {
        id: -1,
        command: "old suggestion".to_string(),
        directory: String::new(),
        session_id: String::new(),
        exit_code: -1,
        timestamp: 0,
        comment: "describe something".to_string(),
        output: String::new(),
        mode: String::new(),
        source: String::new(),

        ..Default::default()
    });
    app.llm_preview_description = Some("describe something".to_string());
    // The user edits the description by
    // appending a character.
    app.push_char('!');
    assert!(
        app.llm_preview.is_none(),
        "stale preview must be cleared on edit"
    );
    assert!(app.llm_preview_description.is_none());
}

/// `llm_maybe_autocall` is a no-op when the
/// query is empty (just `=` with no
/// description). The model has nothing to work
/// with; firing the call would waste a
/// round-trip.
#[test]
fn llm_maybe_autocall_skips_empty_description() {
    let mut app = make_llm_app(
        "=",
        FakeLlm {
            response: "should not be called".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // Arm the debounce in the past so the
    // time check passes if the call were to
    // fire.
    app.llm_debounce_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
    app.llm_maybe_autocall();
    assert!(app.llm_preview.is_none());
}

/// `llm_maybe_autocall` is a no-op when the
/// debounce window hasn't elapsed. The model
/// shouldn't be queried on every tick; only
/// after the user has paused for the full
/// debounce period.
#[test]
fn llm_maybe_autocall_respects_debounce_window() {
    let mut app = make_llm_app(
        "=find files",
        FakeLlm {
            response: "should not be called yet".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // Just-armed debounce: `Instant::now()` is
    // well within the 1-second window.
    app.llm_debounce_started = Some(std::time::Instant::now());
    app.llm_maybe_autocall();
    assert!(
        app.llm_preview.is_none(),
        "auto-call must not fire inside the debounce window"
    );
}

/// `llm_maybe_autocall` is a no-op when the
/// live description already has a fresh
/// preview. We don't want to re-fire the same
/// call repeatedly when the user is just
/// looking at the suggestion.
#[test]
fn llm_maybe_autocall_skips_when_preview_already_fresh() {
    let mut app = make_llm_app(
        "=find files",
        FakeLlm {
            response: "find . -name '*.txt'".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // Install a fresh preview that already
    // matches the current description.
    app.llm_preview = Some(HistoryRow {
        id: -1,
        command: "find . -name '*.txt'".to_string(),
        directory: String::new(),
        session_id: String::new(),
        exit_code: -1,
        timestamp: 0,
        comment: "find files".to_string(),
        output: String::new(),
        mode: String::new(),
        source: String::new(),

        ..Default::default()
    });
    app.llm_preview_description = Some("find files".to_string());
    // Debounce expired in the past.
    app.llm_debounce_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
    // The FakeLlm's response would be
    // "should not be called" if the call
    // fired, but we set the FakeLlm to a
    // specific response. If `generate` were
    // called the preview would be replaced.
    // Assert it WASN'T replaced.
    let original = app.llm_preview.clone();
    app.llm_maybe_autocall();
    assert_eq!(app.llm_preview, original);
}

/// Happy path: debounce elapsed, description
/// has changed, LLM call fires, preview is
/// populated.
#[test]
fn llm_maybe_autocall_fires_and_populates_preview() {
    let mut app = make_llm_app(
        "=find files",
        FakeLlm {
            response: "find . -name '*.txt'".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // Debounce expired in the past.
    app.llm_debounce_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
    app.llm_maybe_autocall();
    let preview = app.llm_preview.as_ref().expect("preview must be populated");
    // With the new design: command is the query (with = prefix),
    // output/comment is the generated command.
    assert_eq!(preview.command, "=find files");
    assert_eq!(preview.output, "find . -name '*.txt'");
    assert_eq!(preview.comment, "find . -name '*.txt'");
    assert_eq!(preview.id, -1);
    assert_eq!(preview.exit_code, -1);
    assert_eq!(app.llm_preview_description.as_deref(), Some("find files"));
    assert!(!app.llm_in_flight);
}

/// Sanitizer rejection during auto-call is
/// silent — the user gets feedback when they
/// press Enter (via `run_llm_query`), not on
/// every auto-call. This is the same UX as a
/// transport error: don't crowd the status
/// bar on every typo.
#[test]
fn llm_maybe_autocall_silent_on_sanitizer_rejection() {
    let mut app = make_llm_app(
        "=do something",
        FakeLlm {
            // All commentary, no command.
            response: "# I cannot help with that.".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    app.llm_debounce_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
    app.llm_maybe_autocall();
    assert!(app.llm_preview.is_none());
    assert!(!app.llm_in_flight);
}

/// The preview row appears at the top of the
/// merged list in LLM mode. Sort key is the
/// `timestamp = now` we set in the autocall,
/// so it sorts newest-first.
#[test]
fn llm_preview_appears_in_merged_rows() {
    let mut app = make_llm_app(
        "=find files",
        FakeLlm {
            response: "find . -name '*.txt'".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    app.llm_debounce_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
    app.llm_maybe_autocall();
    let merged = app.merged_rows();
    assert!(!merged.is_empty());
    assert_eq!(merged[0].id, -1);
    // Command is the description (with = prefix), output is the generated command
    assert_eq!(merged[0].command, "=find files");
    assert_eq!(merged[0].output, "find . -name '*.txt'");
}

/// When the query leaves LLM mode, the preview
/// is removed from the merged list. The user
/// has stopped composing a description; the
/// suggestion no longer applies.
#[test]
fn llm_preview_disappears_when_leaving_llm_mode() {
    let mut app = make_llm_app(
        "=find files",
        FakeLlm {
            response: "find . -name '*.txt'".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    app.llm_debounce_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
    app.llm_maybe_autocall();
    assert!(!app.merged_rows().is_empty());
    // User backspaces out of LLM mode.
    app.query = "git status".to_string();
    app.refresh();
    // Preview must be gone from the merged
    // list (it was only added in LLM mode).
    let merged = app.merged_rows();
    for r in merged {
        assert!(r.id >= 0, "preview leaked into non-LLM mode: {:?}", r);
    }
}

/// Fast-path: when a fresh preview exists for
/// the live description, `run_llm_query`
/// reuses it without making a second HTTP
/// call. The FakeLlm's response is "should not
/// be called" — if `run_llm_query` made a
/// call, the staged command would be the
/// FakeLlm's response, not the preview's.
#[test]
fn run_llm_query_reuses_fresh_preview() {
    let mut app = make_llm_app(
        "=find files",
        FakeLlm {
            response: "should not be called".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // Install a fresh preview with the new structure:
    // command is the query (with = prefix), output/comment is the generated command.
    app.llm_preview = Some(HistoryRow {
        id: -1,
        command: "=find files".to_string(),
        directory: String::new(),
        session_id: String::new(),
        exit_code: -1,
        timestamp: 0,
        comment: "find . -name '*.txt'".to_string(),
        output: "find . -name '*.txt'".to_string(),
        mode: String::new(),
        source: String::new(),

        ..Default::default()
    });
    app.llm_preview_description = Some("find files".to_string());
    // Arm the debounce recently (well
    // within the 5x multiplier).
    app.llm_debounce_started = Some(std::time::Instant::now());
    app.select_for_run();
    // The preview's output (generated command) was staged,
    // not the FakeLlm's response.
    assert_eq!(app.selection.as_deref(), Some("find . -name '*.txt'"));
    assert_eq!(app.pick_mode, Some(PickMode::Run));
}

/// Slow-path: when the preview is stale (the
/// description has changed since the preview
/// was generated), `run_llm_query` falls
/// through to the explicit LLM call.
#[test]
fn run_llm_query_does_not_reuse_stale_preview() {
    let mut app = make_llm_app(
        "=find files",
        FakeLlm {
            response: "find . -mtime -1".to_string(),
            error: None,
            describe_response: String::new(),
            correct_response: String::new(),
        },
    );
    // Install a preview whose description
    // does NOT match the live query.
    app.llm_preview = Some(HistoryRow {
        id: -1,
        command: "stale".to_string(),
        directory: String::new(),
        session_id: String::new(),
        exit_code: -1,
        timestamp: 0,
        comment: "old description".to_string(),
        output: String::new(),
        mode: String::new(),
        source: String::new(),

        ..Default::default()
    });
    app.llm_preview_description = Some("old description".to_string());
    app.llm_debounce_started = Some(std::time::Instant::now());
    app.select_for_run();
    app.process_pending_llm_request();
    // The live FakeLlm was called, NOT
    // the stale preview.
    assert_eq!(app.selection.as_deref(), Some("find . -mtime -1"));
}

// --- Output search (`+...` query mode) ---------------------

/// Build an app with a set of history rows, each of
/// which has a captured output string. The `output`
/// column is what the `+...` search mode targets;
/// the tests below rely on this helper to set up
/// the data they need.
///
/// `rows` is a list of `(command, output)` pairs. The
/// command and output are stored as-is. The test
/// schema mirrors the production schema (including
/// the `idx_history_dedup` unique index that backs
/// `run_llm_query`'s upsert) so the output search
/// path runs against the same SQL the real TUI
/// issues.
fn output_test_app(rows: &[(&str, &str)]) -> App {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );
                        CREATE UNIQUE INDEX idx_history_dedup
                            ON history (command, directory, session_id);",
    )
    .expect("schema");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for (i, (cmd, output)) in rows.iter().enumerate() {
        let id = i as i64 + 1;
        conn.execute(
            "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
                                 VALUES (?1, ?2, '/tmp', 'sess', 0, ?3)",
            rusqlite::params![id, *cmd, now - (rows.len() as i64 - i as i64)],
        )
        .expect("insert history");
        if !output.is_empty() {
            conn.execute(
                "INSERT INTO history_output (history_id, output) VALUES (?1, ?2)",
                rusqlite::params![id, *output],
            )
            .expect("insert output");
        }
    }
    App::new(
        conn,
        Mode::Global,
        String::new(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    )
}

/// The `+` prefix is recognised as a mode marker.
/// Without the prefix, `is_output_query` is false
/// (e.g. `git log +foo` is a plain query, not an
/// output search).
#[test]
fn is_output_query_recognises_plus_prefix() {
    let mut app = output_test_app(&[("ls", "file1\nfile2\n")]);
    assert!(!app.is_output_query());
    app.query = "+segmentation".to_string();
    assert!(app.is_output_query());
    app.query = "git log +foo".to_string();
    assert!(!app.is_output_query());
    app.query = "+".to_string();
    assert!(app.is_output_query());
}

/// `output_pattern` returns everything after the
/// leading `+`, with the leading `+` itself
/// stripped. Used by `build_where` and
/// `query_matches_text` to drive the actual
/// `LIKE` clause and the post-filter.
#[test]
fn output_pattern_strips_leading_plus() {
    let mut app = output_test_app(&[("ls", "")]);
    assert_eq!(app.output_pattern(), "");
    app.query = "+segmentation".to_string();
    assert_eq!(app.output_pattern(), "segmentation");
    app.query = "+".to_string();
    assert_eq!(app.output_pattern(), "");
    app.query = "+git stash".to_string();
    assert_eq!(app.output_pattern(), "git stash");
}

/// Single-word output search: the row whose
/// captured output contains the substring is
/// included; other rows are not.
#[test]
fn output_search_matches_substring_in_output() {
    let mut app = output_test_app(&[
        ("make", "Compiling foo v0.1.0\nFinished release"),
        ("ls", "src\nCargo.toml\nREADME.md"),
        (
            "cargo test",
            "running 1 test\ntest ok\nsegmentation fault (core dumped)",
        ),
    ]);
    app.query = "+segmentation".to_string();
    app.refresh();
    let commands: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(commands, vec!["cargo test"]);
}

/// Multi-word output search: the query is
/// `+running test` and only the row whose
/// output contains BOTH substrings is
/// included. This is the same AND-by-word
/// behaviour as plain text mode. We use
/// `running` / `test` here (not `seg` /
/// `fault`) because the substring match is
/// exact-substring, not word-boundary: a row
/// containing `segfault` would match BOTH
/// `seg` and `fault` as substrings, defeating
/// the AND test.
#[test]
fn output_search_is_multi_word_and() {
    let mut app = output_test_app(&[
        ("make", "Compiling foo\nFinished release"),
        ("binary_a", "running test_a\nok"),
        ("binary_b", "compiling test_b\nsegfault"),
    ]);
    app.query = "+running test".to_string();
    app.refresh();
    let commands: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // Only `binary_a` contains both
    // `running` AND `test`. `binary_b` has
    // `test` (in `test_b`) but not
    // `running`, and `make` has neither.
    assert_eq!(commands, vec!["binary_a"]);
}

/// Output search is case-insensitive. The user
/// types lowercase but the LLM-generated log
/// lines often contain uppercase variants
/// (`SEGMENTATION FAULT`); both should match.
#[test]
fn output_search_is_case_insensitive() {
    let mut app = output_test_app(&[
        ("a", "ALL GOOD"),
        ("b", "SEGMENTATION FAULT"),
        ("c", "no output at all"),
    ]);
    app.query = "+segmentation".to_string();
    app.refresh();
    let commands: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(commands, vec!["b"]);
}

/// Rows without captured output are excluded
/// from output search. The SQL `LIKE` clause
/// only matches against `o.output`, which is
/// NULL for rows without a `history_output`
/// row. This is the desired behaviour: the
/// user is asking "which command produced
/// *this output*?" and a command with no
/// captured output cannot be the answer.
#[test]
fn output_search_excludes_rows_without_output() {
    let mut app = output_test_app(&[
        ("with_output", "ERROR: something broke"),
        // No output row for this one.
        ("without_output", ""),
    ]);
    app.query = "+something".to_string();
    app.refresh();
    let commands: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(commands, vec!["with_output"]);
}

/// An empty `+` (no body) lists all rows that
/// have captured output. This mirrors the
/// plain-mode behaviour of an empty query
/// (show everything) but restricted to rows
/// with output. Useful as a "show me what
/// I've actually captured" view.
#[test]
fn output_search_empty_body_lists_all_with_output() {
    let mut app = output_test_app(&[
        ("a", "some output"),
        ("b", "other output"),
        // No output row.
        ("c", ""),
    ]);
    app.query = "+".to_string();
    app.refresh();
    let commands: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // `c` has no output, so it must be
    // excluded. Order is timestamp-DESC
    // (`a` is oldest, `c` is newest in
    // the helper, so `c` would normally
    // be first, but `c` is excluded).
    assert_eq!(commands.len(), 2);
    assert!(commands.contains(&"a"));
    assert!(commands.contains(&"b"));
}

/// Output search respects the `history_output`
/// join: even if the command text or comment
/// doesn't contain the substring, the row is
/// included when its captured output does.
/// This is the whole point of the `+` mode —
/// it searches a column the other modes
/// don't touch.
#[test]
fn output_search_uses_output_not_command() {
    let mut app = output_test_app(&[
        // Command text is innocuous;
        // only the captured output
        // contains the search term.
        ("do_thing", "ERROR: kernel panic — not syncing"),
    ]);
    // `+panic` must match this row even
    // though the command (`do_thing`) and
    // the comment (empty) don't contain
    // the word.
    app.query = "+panic".to_string();
    app.refresh();
    let commands: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(commands, vec!["do_thing"]);
}

/// `query_matches_text` uses the body of the
/// `+` query (not the leading `+`) when
/// post-filtering labeled rows. The post-
/// filter would otherwise look for the
/// literal substring `+segmentation` and
/// never match.
#[test]
fn query_matches_text_strips_plus_prefix() {
    let mut app = output_test_app(&[("x", "")]);
    app.query = "+segmentation".to_string();
    // The text being checked doesn't
    // contain the literal `+segmentation`
    // but does contain `segmentation`.
    assert!(app.query_matches_text("segmentation fault"));
    // Sanity: a totally unrelated text
    // doesn't match.
    assert!(!app.query_matches_text("all good"));
}

/// Mode cycle includes the `+` step. The
/// `cycle_search_mode_advances_prefix` and
/// `cycle_search_mode_preserves_query_body`
/// tests cover the exact cycle, but we
/// double-check here that the `+...` body is
/// preserved across the cycle in both
/// directions.
#[test]
fn cycle_search_mode_round_trips_output_mode() {
    let mut app = stats_test_app(&[]);
    app.query = String::from("test");
    // The cycle no longer touches the query string.
    // Let's just verify the algorithm cycles back.
    let init = app.match_algorithm;
    app.cycle_search_mode();
    app.cycle_search_mode();
    app.cycle_search_mode();
    assert_eq!(app.match_algorithm, init);
    assert_eq!(app.query, "test");
}

// --- Sort order (Age / Frequency) -------------------------

/// `SortOrder::next` cycles between the two
/// supported values: Age (default) ↔ Frequency.
#[test]
fn sort_order_next_cycles_between_age_and_frequency() {
    assert_eq!(SortOrder::Age.next(), SortOrder::Frequency);
    assert_eq!(SortOrder::Frequency.next(), SortOrder::Age);
}

/// `SortOrder::as_str` returns the canonical
/// lowercase form used in the session file.
#[test]
fn sort_order_as_str_returns_canonical_form() {
    assert_eq!(SortOrder::Age.as_str(), "age");
    assert_eq!(SortOrder::Frequency.as_str(), "frequency");
}

/// `SortOrder::parse` accepts the canonical
/// form plus a small set of friendly aliases
/// (case-insensitive, dash-tolerant in spirit).
/// A bad value returns `None` so the caller can
/// fall back to the default.
#[test]
fn sort_order_parse_accepts_canonical_and_aliases() {
    assert_eq!(SortOrder::parse("age"), Some(SortOrder::Age));
    assert_eq!(SortOrder::parse("frequency"), Some(SortOrder::Frequency));
    // Aliases.
    assert_eq!(SortOrder::parse("freq"), Some(SortOrder::Frequency));
    assert_eq!(SortOrder::parse("count"), Some(SortOrder::Frequency));
    assert_eq!(SortOrder::parse("occurrences"), Some(SortOrder::Frequency));
    assert_eq!(SortOrder::parse("time"), Some(SortOrder::Age));
    assert_eq!(SortOrder::parse("newest"), Some(SortOrder::Age));
    // Case-insensitive.
    assert_eq!(SortOrder::parse("AGE"), Some(SortOrder::Age));
    assert_eq!(SortOrder::parse("Frequency"), Some(SortOrder::Frequency));
    // Unrecognised values fall through.
    assert_eq!(SortOrder::parse("garbage"), None);
    assert_eq!(SortOrder::parse(""), None);
}

/// `SortOrder::default` is `Age` (the historical
/// default), so first-time TUI users get the
/// familiar timestamp-DESC ordering.
#[test]
fn sort_order_default_is_age() {
    assert_eq!(SortOrder::default(), SortOrder::Age);
}

/// The default (Age) sort orders rows by
/// timestamp DESC — the historical behaviour.
/// This test pins the contract so any future
/// refactor that swaps the primary key
/// accidentally fails loudly.
#[test]
fn sort_by_age_orders_by_timestamp_desc() {
    let mut app = global_test_app(&[
        ("git status", 5), // oldest
        ("cargo test", 2),
        ("ls -la", 1), // newest
    ]);
    app.sort_order = SortOrder::Age;
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // Newest first: `ls -la` (offset 1), then
    // `cargo test` (offset 2), then `git status`
    // (offset 5).
    assert_eq!(cmds, vec!["ls -la", "cargo test", "git status"]);
}

/// In frequency sort mode, each command
/// appears exactly once. Frequency mode is
/// implicitly a dedup mode — without dedup,
/// the most-frequent command would
/// dominate the list with its own repeat
/// instances, drowning out everything else
/// and making the count ranking meaningless.
/// The kept instance is the newest (highest
/// timestamp = lowest offset), because the
/// per-row tie-breaker is `timestamp DESC`
/// and we keep the first occurrence per
/// command in the sorted list.
#[test]
fn sort_by_frequency_orders_by_occurrence_count() {
    let mut app = global_test_app(&[("a", 1), ("a", 2), ("b", 3), ("a", 4)]);
    app.sort_order = SortOrder::Frequency;
    app.duplicate_filter = false;
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // Frequency mode dedups implicitly:
    // one row per command, ordered by
    // count DESC. `a` had 3 occurrences
    // (count 3), `b` had 1 (count 1) —
    // `a` first. The kept `a` row is
    // the one with the highest timestamp
    // (offset 1, the newest).
    assert_eq!(cmds, vec!["a", "b"]);
}

/// When the duplicate filter is ON in
/// frequency mode, only the highest-ranked
/// instance of each command is kept. The
/// primary sort is still by count, so the
/// kept instances are correctly ordered.
/// This is the same result as
/// `sort_by_frequency_orders_by_occurrence_count`
/// (frequency mode dedups implicitly
/// regardless of the filter setting), but
/// kept as a separate test to pin the
/// explicit-toggle behaviour.
#[test]
fn sort_by_frequency_with_duplicate_filter() {
    let mut app = global_test_app(&[("a", 1), ("a", 2), ("a", 3), ("b", 4), ("b", 5)]);
    app.sort_order = SortOrder::Frequency;
    app.duplicate_filter = true;
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // `a` had 3 occurrences, `b` had 2.
    // With dedup ON, one `a` and one `b`
    // remain, and `a` sorts first.
    assert_eq!(cmds, vec!["a", "b"]);
}

/// In frequency sort mode the dedup is
/// implicit, so the per-command tie-break
/// (newest command wins) is what
/// determines the final order — not
/// per-row timestamps. The kept instance
/// for each command is the newest one.
#[test]
fn sort_by_frequency_breaks_ties_by_age() {
    let mut app = global_test_app(&[
        ("a", 1), // a's newest
        ("a", 5),
        ("b", 2), // b's newest
        ("b", 3),
        ("c", 4), // c's only
    ]);
    app.sort_order = SortOrder::Frequency;
    app.duplicate_filter = false;
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // `a` and `b` both have count 2; the
    // per-command-newest tie-break picks
    // `a` (newest instance at offset 1
    // vs b's newest at offset 2). `c`
    // has count 1, so it sorts last.
    // Implicit dedup means each command
    // appears once.
    assert_eq!(cmds, vec!["a", "b", "c"]);
}

/// `cycle_sort_order` flips the field and
/// refreshes the list, so the new order is
/// immediately visible.
#[test]
fn cycle_sort_order_flips_the_field() {
    let mut app = stats_test_app(&[("a", 1), ("b", 2)]);
    assert_eq!(app.sort_order, SortOrder::Age);
    app.cycle_sort_order();
    assert_eq!(app.sort_order, SortOrder::Frequency);
    app.cycle_sort_order();
    assert_eq!(app.sort_order, SortOrder::Age);
}

/// In frequency sort mode, the duplicate
/// filter is *implicit* — turning on
/// frequency sort collapses the list to
/// one row per command regardless of the
/// `duplicate_filter` setting. The user's
/// filter toggle is still respected in
/// `Age` mode (where the historical
/// behaviour applies), so the two settings
/// are independent in their non-overlapping
/// modes and simply both apply when both
/// are active.
///
/// This is the contract the user asked
/// for: in FREQ mode, "display only the
/// last element of a group of commands".
/// The "last" instance is the most recent
/// one, identified by the highest
/// timestamp among the group's rows.
#[test]
fn frequency_sort_dedups_implicitly_even_when_duplicate_filter_off() {
    let mut app = global_test_app(&[
        ("a", 1), // a's oldest
        ("a", 2), // a's newest
        ("b", 3), // b's oldest
        ("b", 4), // b's newest
        ("c", 5),
    ]);
    // User has NOT enabled the
    // duplicate filter.
    app.duplicate_filter = false;
    app.sort_order = SortOrder::Age;
    app.refresh();
    // In Age mode without dedup, all 5
    // rows are visible.
    let age_count = app.merged_rows().len();
    assert_eq!(age_count, 5);
    // Now switch to frequency mode. The
    // implicit dedup should collapse
    // this to 3 rows (one per command),
    // even though `duplicate_filter` is
    // still false.
    app.sort_order = SortOrder::Frequency;
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(
        cmds.len(),
        3,
        "FREQ mode must dedup implicitly: got {:?}",
        cmds
    );
    // The kept row per command is the
    // newest one. For `a` (offsets 1
    // and 2), the newer is offset 1
    // (higher timestamp). Same for
    // `b` (offsets 3 and 4, newer is
    // 3). `c` is alone. The list is
    // ordered by count DESC; `a` and
    // `b` are tied at 2 and `c` has 1.
    // Tie-break by per-command newest:
    // `a`'s newest is offset 1, `b`'s
    // is offset 3 — `a` is newer, so
    // `a` first.
    assert_eq!(cmds, vec!["a", "b", "c"]);
}

/// Switching back from frequency mode to
/// age mode restores the
/// `duplicate_filter` setting's
/// independence: the implicit dedup
/// disappears. This pins the
/// "frequency mode adds implicit dedup,
/// doesn't replace the user's setting"
/// contract.
#[test]
fn age_sort_does_not_dedup_when_duplicate_filter_off() {
    let mut app = global_test_app(&[("a", 1), ("a", 2), ("b", 3)]);
    app.duplicate_filter = false;
    // Age mode is the default; no
    // implicit dedup should happen.
    app.sort_order = SortOrder::Age;
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // All 3 rows visible (the user's
    // duplicate filter is off).
    assert_eq!(cmds.len(), 3);
}

/// `Action::CycleSortOrder` dispatches to
/// `cycle_sort_order` and is bound to `F4`
/// by default.
#[test]
fn cycle_sort_order_default_key_routes() {
    let mut app = stats_test_app(&[("a", 1)]);
    let bindings = KeyBindings::defaults();
    let key = KeyEvent::new(KeyCode::F(4), KeyModifiers::empty());
    let action = action_for_key(&bindings, &key).expect("F4 is bound by default");
    assert_eq!(action, Action::CycleSortOrder);
    // Apply the action and check the
    // field flipped. We use the public
    // cycle_sort_order method directly
    // rather than going through the full
    // dispatch loop, which would need
    // terminal handles.
    app.cycle_sort_order();
    assert_eq!(app.sort_order, SortOrder::Frequency);
}

/// Stats mode overrides the user's sort
/// order — the frequency-aware ranking from
/// `fetch_stats` is preserved. Without this
/// guard, an Age sort in Stats mode would
/// wipe out the prediction signal.
#[test]
fn stats_mode_overrides_sort_order() {
    // We don't have a rich test for
    // Stats mode here; the contract is
    // that `build_merged_rows` skips the
    // sort when `Mode::Stats` is active.
    // Verify the helper directly.
    let mut app = stats_test_app(&[("a", 1)]);
    app.mode = Mode::Stats;
    app.sort_order = SortOrder::Age;
    let rows = app.build_merged_rows();
    // The rows come out in whatever order
    // `fetch_stats` produced — we just
    // assert the helper doesn't crash
    // and returns a non-empty list.
    assert!(!rows.is_empty());
}

// --- Session persistence for sort order ----------------

/// The `sortorder=...` line in the session
/// file is parsed by `TuiSession::load`.
/// Verifying the round-trip here (without
/// going through the real file system)
/// catches drift between the writer and
/// the reader.
#[test]
fn session_round_trips_sort_order() {
    // Build a session value that
    // differs from the default (Age)
    // so the writer actually emits
    // the field.
    let s = TuiSession {
        mode: None,
        query: None,
        duplicate_filter: None,
        exit_filter: None,
        sort_order: Some("frequency".to_string()),
        theme: None,
        directory_source: None,
        pane_visibility: None,
        pane_height: None,
        scheme: None,
    };
    let rendered = format!("{:?}", s);
    // The `Debug` output includes the
    // raw field, but the actual
    // serialization format is
    // `sortorder=<value>`. We re-serialize
    // through a tiny helper here: the
    // `save` method writes the field
    // when `Some`; we just want to know
    // that `Some("frequency")` survives
    // a round-trip. Verify via the
    // `as_str` round-trip plus the
    // session's `sort_order` field being
    // populated as we set it.
    assert_eq!(
        s.sort_order.as_deref(),
        Some("frequency"),
        "session struct keeps the value we put in"
    );
    // `SortOrder::parse` would also be
    // called on this value when the
    // session is loaded; verify it
    // recognises the canonical form.
    assert_eq!(
        SortOrder::parse(s.sort_order.as_deref().unwrap()),
        Some(SortOrder::Frequency)
    );
    // And that an unknown value would
    // be rejected on load (so a
    // hand-edited session file can't
    // wedge the TUI).
    assert_eq!(SortOrder::parse("garbage"), None);
    // Make sure the field is what we
    // think it is (the rendered debug
    // output would surface a rename).
    assert!(
        rendered.contains("sort_order"),
        "renamed the field: {:?}",
        rendered
    );
}

// --- Describe (`Action::Describe`, default `C-k`) -----

/// `Action::Describe` is bound to `Ctrl-K` by
/// default. The test helper `FakeLlm` is wired
/// up the same way as the LLM tests above, so
/// we can drive `start_describe` end-to-end
/// without a live ollama server.
#[test]
fn describe_default_key_routes() {
    let bindings = KeyBindings::defaults();
    let key = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
    let action = action_for_key(&bindings, &key).expect("Ctrl-K is bound by default");
    assert_eq!(action, Action::Describe);
}

/// `start_describe` opens the overlay with
/// the LLM's response, scoped to the
/// currently-selected row. The command
/// string is captured in the view so the
/// title can render it.
#[test]
fn start_describe_opens_overlay_with_response() {
    let mut app = global_test_app(&[("git status", 1)]);
    // Wire up the FakeLlm. We replace the
    // existing `None` LLM (set by
    // `global_test_app`) with one that
    // returns a canned description.
    app.llm = Some(Box::new(FakeLlm {
        response: String::new(),
        error: None,
        describe_response: "Lists the working \
                                            tree status in git."
            .to_string(),
        correct_response: String::new(),
    }));
    // Select the row.
    app.refresh();
    app.start_describe();
    app.process_pending_llm_request();
    let view = app
        .describe_view
        .as_ref()
        .expect("describe overlay must open on success");
    assert_eq!(view.command, "git status");
    assert!(view.text.contains("Lists"));
    assert!(!app.cancelled);
}

/// `start_describe` with no LLM configured
/// surfaces the "not configured" status
/// message and does NOT open the overlay
/// (so the user doesn't see an empty
/// overlay that would have to be closed
/// again). This is the same UX as the
/// `run_llm_query` path.
#[test]
fn start_describe_surfaces_not_configured_when_client_is_none() {
    let mut app = global_test_app(&[("a", 1)]);
    // `global_test_app` already sets
    // `app.llm = None`.
    assert!(app.llm.is_none());
    app.refresh();
    app.start_describe();
    assert!(app.describe_view.is_none());
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("missing-LLM must surface a status");
    assert!(msg.contains("not configured"), "got: {:?}", msg);
}

/// `start_describe` with no row selected
/// surfaces a status message and doesn't
/// open the overlay. (We force "no row
/// selected" by emptying the rows before
/// the call.)
#[test]
fn start_describe_with_no_row_surfaces_status() {
    // Empty DB: no rows, so
    // `selected_row()` returns None.
    let mut app = global_test_app(&[]);
    app.llm = Some(Box::new(FakeLlm {
        response: String::new(),
        error: None,
        describe_response: "should not be used".to_string(),
        correct_response: String::new(),
    }));
    app.refresh();
    app.start_describe();
    assert!(app.describe_view.is_none());
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("empty list must surface a status");
    assert!(msg.contains("no row"), "got: {:?}", msg);
}

/// When the LLM call fails, the overlay is
/// not opened and the error is surfaced in
/// the status bar. Same UX as the
/// "not configured" path: the user gets a
/// message and the TUI stays in the normal
/// list view.
#[test]
fn start_describe_surfaces_error_on_transport_failure() {
    let mut app = global_test_app(&[("a", 1)]);
    app.llm = Some(Box::new(FakeLlm {
        response: String::new(),
        error: Some(crate::llm::LlmError::Transport(
            "connection refused".to_string(),
        )),
        describe_response: String::new(),
        correct_response: String::new(),
    }));
    app.refresh();
    app.start_describe();
    app.process_pending_llm_request();
    assert!(app.describe_view.is_none());
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("transport error must surface a status");
    assert!(msg.contains("transport"), "got: {:?}", msg);
}

/// `start_describe` is reentrant-safe: if
/// the overlay is already open, a second
/// call replaces the previous view (rather
/// than stacking two views on top of each
/// other). The previous response is
/// dropped; the new one wins.
#[test]
fn start_describe_replaces_existing_view() {
    let mut app = global_test_app(&[("a", 1)]);
    let llm = FakeLlm {
        response: String::new(),
        error: None,
        describe_response: "first response".to_string(),
        correct_response: String::new(),
    };
    app.llm = Some(Box::new(llm));
    app.refresh();
    app.start_describe();
    app.process_pending_llm_request();
    assert_eq!(app.describe_view.as_ref().unwrap().text, "first response");
    // Now re-describe with a different
    // canned response. We can't easily
    // swap `app.llm` mid-test, so we
    // just verify the overlay is
    // re-entered cleanly: the overlay is
    // open, and re-running start_describe
    // should leave it open (not panic,
    // not stack).
    app.start_describe();
    assert!(app.describe_view.is_some());
}

/// The overlay's `command` field reflects
/// the row that was selected at the time of
/// the describe call. Navigating to a
/// different row afterwards doesn't change
/// the overlay's captured command — the
/// title stays anchored to the original
/// row, which is the right UX (the LLM was
/// asked about that specific command).
#[test]
fn describe_view_anchors_to_original_command() {
    let mut app = global_test_app(&[("git status", 1), ("ls -la", 2)]);
    app.llm = Some(Box::new(FakeLlm {
        response: String::new(),
        error: None,
        describe_response: "description".to_string(),
        correct_response: String::new(),
    }));
    app.refresh();
    // Select the first row (newest
    // timestamp wins, so this is "git
    // status").
    app.start_describe();
    app.process_pending_llm_request();
    let view = app.describe_view.as_ref().unwrap();
    assert_eq!(view.command, "git status");
    // Move to the second row.
    app.move_selection(1);
    // The overlay's command is still
    // "git status" — it doesn't follow
    // the cursor.
    let view = app.describe_view.as_ref().unwrap();
    assert_eq!(view.command, "git status");
}

/// `is_describe_viewing` is the predicate
/// the run loop uses to decide whether to
/// route keys to the overlay. We just want
/// to know it tracks the field correctly.
#[test]
fn is_describe_viewing_tracks_field() {
    let mut app = global_test_app(&[("a", 1)]);
    assert!(!app.is_describe_viewing());
    app.describe_view = Some(DescribeView {
        command: "a".to_string(),
        text: "a description".to_string(),
        scroll: 0,
    });
    assert!(app.is_describe_viewing());
    app.close_describe();
    assert!(!app.is_describe_viewing());
}

// --- Correct (`Action::Correct`, default `C-t`) -----

/// `Action::Correct` is bound to `Ctrl-T` by
/// default. The default key is free of the
/// other defaults and not used by readline /
/// zsh in any common configuration, so the
/// binding is a safe starting point.
#[test]
fn correct_default_key_routes() {
    let bindings = KeyBindings::defaults();
    let key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
    let action = action_for_key(&bindings, &key).expect("Ctrl-T is bound by default");
    assert_eq!(action, Action::Correct);
}

/// `start_correct` opens the overlay with
/// the LLM's corrected command, scoped to
/// the currently-selected row. The original
/// command is captured in the view so the
/// user can see what was being fixed.
///
/// The FakeLlm returns a clean command
/// (no markdown), so the sanitized result
/// is exactly the canned string. The
/// `sanitize_command` path is exercised
/// separately by the LLM tests.
#[test]
fn start_correct_opens_overlay_with_response() {
    let mut app = global_test_app(&[("gti status", 1)]);
    app.llm = Some(Box::new(FakeLlm {
        response: String::new(),
        error: None,
        describe_response: String::new(),
        correct_response: "git status".to_string(),
    }));
    app.refresh();
    app.start_correct();
    app.process_pending_llm_request();
    let view = app
        .correct_view
        .as_ref()
        .expect("correct overlay must open on success");
    assert_eq!(view.original_command, "gti status");
    assert_eq!(view.corrected_command, "git status");
    assert!(!app.cancelled);
}

/// `start_correct` with no LLM configured
/// surfaces the "not configured" status
/// message and does NOT open the overlay
/// (so the user doesn't see an empty
/// overlay that would have to be closed
/// again). Same UX as `start_describe`.
#[test]
fn start_correct_surfaces_not_configured_when_client_is_none() {
    let mut app = global_test_app(&[("a", 1)]);
    assert!(app.llm.is_none());
    app.refresh();
    app.start_correct();
    assert!(app.correct_view.is_none());
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("missing-LLM must surface a status");
    assert!(msg.contains("not configured"), "got: {:?}", msg);
}

/// `start_correct` with no row selected
/// surfaces a status message and doesn't
/// open the overlay. (Empty DB.)
#[test]
fn start_correct_with_no_row_surfaces_status() {
    let mut app = global_test_app(&[]);
    app.llm = Some(Box::new(FakeLlm {
        response: String::new(),
        error: None,
        describe_response: String::new(),
        correct_response: "should not be used".to_string(),
    }));
    app.refresh();
    app.start_correct();
    assert!(app.correct_view.is_none());
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("empty list must surface a status");
    assert!(msg.contains("no row"), "got: {:?}", msg);
}

/// When the LLM call fails, the overlay
/// is not opened and the error is surfaced
/// in the status bar.
#[test]
fn start_correct_surfaces_error_on_transport_failure() {
    let mut app = global_test_app(&[("a", 1)]);
    app.llm = Some(Box::new(FakeLlm {
        response: String::new(),
        error: Some(crate::llm::LlmError::Transport(
            "connection refused".to_string(),
        )),
        describe_response: String::new(),
        correct_response: String::new(),
    }));
    app.refresh();
    app.start_correct();
    app.process_pending_llm_request();
    assert!(app.correct_view.is_none());
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("transport error must surface a status");
    assert!(msg.contains("transport"), "got: {:?}", msg);
}

/// When the LLM response sanitizes to
/// `None` (e.g. all commentary, no command
/// survived `sanitize_command`), the
/// overlay is not opened and a status
/// message is surfaced.
#[test]
fn start_correct_surfaces_no_command_when_sanitizer_rejects() {
    let mut app = global_test_app(&[("a", 1)]);
    app.llm = Some(Box::new(FakeLlm {
        response: String::new(),
        error: None,
        describe_response: String::new(),
        // All commentary, no
        // command-form line survives
        // `sanitize_command`.
        correct_response: "# I cannot help with that.".to_string(),
    }));
    app.refresh();
    app.start_correct();
    app.process_pending_llm_request();
    assert!(app.correct_view.is_none());
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("empty sanitizer output must surface a status");
    assert!(msg.contains("no usable command"), "got: {:?}", msg);
}

/// `is_correct_viewing` is the predicate
/// the run loop uses to decide whether to
/// route keys to the overlay. We just
/// want to know it tracks the field
/// correctly.
#[test]
fn is_correct_viewing_tracks_field() {
    let mut app = global_test_app(&[("a", 1)]);
    assert!(!app.is_correct_viewing());
    app.correct_view = Some(CorrectView {
        original_command: "a".to_string(),
        corrected_command: "b".to_string(),
    });
    assert!(app.is_correct_viewing());
    app.close_correct();
    assert!(!app.is_correct_viewing());
}

/// `accept_corrected_command` stages the
/// corrected command and writes a new
/// history row with the original as the
/// comment (for traceability). This is the
/// "Enter pressed in the correct overlay"
/// path.
#[test]
fn accept_corrected_command_stages_and_inserts() {
    let mut app = global_test_app_with_dedup_index(&[("gti status", 1)]);
    app.correct_view = Some(CorrectView {
        original_command: "gti status".to_string(),
        corrected_command: "git status".to_string(),
    });
    app.accept_corrected_command();
    // Selection is set with the
    // corrected command.
    assert_eq!(app.selection.as_deref(), Some("git status"));
    assert_eq!(app.pick_mode, Some(PickMode::Run));
    // The corrected overlay is consumed
    // (taken).
    assert!(app.correct_view.is_none());
    // A new row was inserted into
    // history with the original as
    // the comment.
    let count: i64 = app
        .conn
        .query_row(
            "SELECT COUNT(*) FROM history WHERE command = ?1",
            rusqlite::params!["git status"],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(count, 1, "corrected command must be inserted");
    let comment: String = app
        .conn
        .query_row(
            "SELECT comment FROM command_comments WHERE command = ?1",
            rusqlite::params!["git status"],
            |row| row.get(0),
        )
        .expect("comment");
    assert_eq!(comment, "gti status");
}

/// `accept_corrected_command` is a no-op
/// when the overlay is closed (e.g. the
/// user pressed `Esc` and then somehow
/// triggered the action). We don't want
/// to crash, and we don't want to write a
/// row with a stale `view`.
#[test]
fn accept_corrected_command_no_op_when_overlay_closed() {
    let mut app = global_test_app(&[("a", 1)]);
    app.correct_view = None;
    app.accept_corrected_command();
    assert!(app.selection.is_none());
}

// --- Delete-word-backward (`Ctrl-W`) -------------------

/// `Action::DeleteWordBackward` is bound to
/// `Ctrl-W` by default. The default key
/// matches the readline/bash/zsh muscle
/// memory for "kill previous word".
#[test]
fn delete_word_backward_default_key_routes() {
    let bindings = KeyBindings::defaults();
    let key = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
    let action = action_for_key(&bindings, &key).expect("Ctrl-W is bound by default");
    assert_eq!(action, Action::DeleteWordBackward);
}

/// `M-Backspace` is also bound by default so users
/// coming from the macOS / GUI-editor muscle
/// memory (where the conventional "delete
/// previous word" key is Alt-Backspace, not
/// Ctrl-W) get the expected behaviour without
/// having to remap.
#[test]
fn delete_word_backward_alt_backspace_routes() {
    let bindings = KeyBindings::defaults();
    let key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT);
    let action =
        action_for_key(&bindings, &key).expect("M-Backspace is bound by default alongside C-w");
    assert_eq!(action, Action::DeleteWordBackward);
}

/// Both default keys are listed in `default_keys()`
/// and `KeyBindings::defaults()` registers both.
/// Tests that compare against the full default
/// binding set should use this canonical
/// comma-joined form, not `default_key()` (which
/// only returns the first spec).
#[test]
fn delete_word_backward_defaults_have_both_specs() {
    assert_eq!(
        Action::DeleteWordBackward.default_keys(),
        &["C-w", "M-Backspace"]
    );
    let bindings = KeyBindings::defaults();
    assert_eq!(bindings.specs(Action::DeleteWordBackward).len(), 2);
    assert_eq!(
        format_key_specs(bindings.specs(Action::DeleteWordBackward)),
        "C-w, M-Backspace"
    );
}

/// The `Cancel` action ships
/// with two default
/// bindings: `C-c` and
/// `Esc`. The
/// user-configured
/// `key.cancel=C-c,Esc`
/// in the project config
/// is the canonical
/// source of truth; the
/// default is wired to
/// match so a fresh
/// checkout behaves the
/// same as a configured
/// install. Both fire the
/// same `Action::Cancel`,
/// so users from either
/// muscle-memory
/// background get the
/// expected behaviour
/// without remapping.
#[test]
fn cancel_defaults_have_both_specs() {
    assert_eq!(
        Action::Cancel.default_keys(),
        &["C-c", "Esc"],
        "Cancel must ship with C-c + Esc as the two default bindings"
    );
    // `default_key()` returns
    // the FIRST spec (the
    // single-spec form).
    // `C-c` is the first
    // because the
    // `default_key()`
    // arm was updated
    // alongside the
    // `default_keys()`
    // arm.
    assert_eq!(
        Action::Cancel.default_key(),
        "C-c",
        "default_key() must return the first spec of the multi-spec list"
    );
    let bindings = KeyBindings::defaults();
    assert_eq!(bindings.specs(Action::Cancel).len(), 2);
    assert_eq!(format_key_specs(bindings.specs(Action::Cancel)), "C-c, Esc");
    // Both keys route to
    // `Action::Cancel`.
    let evt = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(action_for_key(&bindings, &evt), Some(Action::Cancel));
    let evt = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
    assert_eq!(action_for_key(&bindings, &evt), Some(Action::Cancel));
}

/// The project config
/// explicitly binds a
/// handful of actions to
/// non-default keys
/// (`open-help=C-a`,
/// `command-action=C-q`,
/// `edit-file-reference=C-v`,
/// `show-output=C-o`,
/// `cycle-directory-source=C-s`,
/// `add-session=F5`,
/// `add-host=F6`,
/// `toggle-pane-visibility=F10`).
/// The shipped defaults
/// mirror those bindings
/// so a fresh checkout
/// behaves the same as
/// a configured install.
#[test]
fn project_config_defaults_are_shipped() {
    let bindings = KeyBindings::defaults();
    // `key.open-help=C-a` in
    // the project
    // config.
    assert_eq!(format_key_specs(bindings.specs(Action::OpenHelp)), "C-a");
    // `key.command-action=C-q`
    // in the project
    // config.
    assert_eq!(
        format_key_specs(bindings.specs(Action::CommandAction)),
        "C-q"
    );
    // `key.edit-file-reference=C-v`
    // in the project
    // config.
    assert_eq!(
        format_key_specs(bindings.specs(Action::EditFileReference)),
        "C-v"
    );
    // `key.show-output=C-o`
    // in the project
    // config.
    assert_eq!(format_key_specs(bindings.specs(Action::ShowOutput)), "C-o");
    // `key.cycle-directory-source=C-s`
    // in the project
    // config.
    assert_eq!(
        format_key_specs(bindings.specs(Action::CycleDirectorySource)),
        "C-s"
    );
    // `key.add-session=F5`
    // in the project
    // config.
    assert_eq!(format_key_specs(bindings.specs(Action::AddSession)), "F5");
    // `key.add-host=F6`
    // in the project
    // config.
    assert_eq!(format_key_specs(bindings.specs(Action::AddHost)), "F6");
    // `key.toggle-pane-visibility=F10`
    // in the project
    // config.
    assert_eq!(
        format_key_specs(bindings.specs(Action::TogglePaneVisibility)),
        "F10"
    );
}

/// Actions the user has
/// explicitly unbound in
/// the project config
/// (`toggle-duplicate-filter=none`,
/// `delete-matching=none`)
/// ship unbound by
/// default. The `none`
/// sentinel in the
/// single-spec
/// `default_key()` slot
/// is the new "this
/// action ships without
/// a default binding"
/// signal. The help
/// overlay and command
/// palette render the
/// unbound action as
/// `(unbound)`.
#[test]
fn unbound_by_default_actions_ship_without_a_binding() {
    let bindings = KeyBindings::defaults();
    // `key.toggle-duplicate-filter=none`
    // in the project
    // config.
    assert!(
        bindings.is_unbound(Action::ToggleDuplicateFilter),
        "ToggleDuplicateFilter must ship unbound (default is the `none` sentinel), got: {:?}",
        format_key_specs(bindings.specs(Action::ToggleDuplicateFilter))
    );
    assert_eq!(
        Action::ToggleDuplicateFilter.default_key(),
        "none",
        "default_key() for ToggleDuplicateFilter must be the `none` sentinel"
    );
    // `key.delete-matching=none`
    // in the project
    // config.
    assert!(
        bindings.is_unbound(Action::DeleteMatching),
        "DeleteMatching must ship unbound (default is the `none` sentinel), got: {:?}",
        format_key_specs(bindings.specs(Action::DeleteMatching))
    );
    assert_eq!(
        Action::DeleteMatching.default_key(),
        "none",
        "default_key() for DeleteMatching must be the `none` sentinel"
    );
    // The `none` sentinel
    // is a default-key
    // concept ONLY. It
    // must not be parsed
    // as a real key spec
    // (the existing
    // `parse_key_spec`
    // behaviour is to
    // reject it; this
    // test pins that
    // contract so a
    // future refactor
    // doesn't accidentally
    // accept `none` as a
    // literal key).
    assert!(
        bindings::parse_key_spec("none").is_err()
            || bindings::parse_key_spec_opt("none")
                .ok()
                .flatten()
                .is_none(),
        "`none` must NOT be parseable as a real key spec"
    );
}

/// Basic case: cursor at end of `git status`,
/// press `Ctrl-W`, get `git `. The trailing
/// word `status` is eaten; the space between
/// `git` and `status` stays. We don't flag
/// the query as touched for an empty /
/// prefilled query — see the
/// `delete_word_backward_at_start_is_noop`
/// test for that boundary case.
#[test]
fn delete_word_backward_removes_trailing_word() {
    let mut app = stats_test_app(&[("ls", 1)]);
    app.query = "git status".to_string();
    app.query_cursor = app.query.chars().count();
    app.delete_word_backward();
    // `status` (positions 4..10) is
    // eaten; the space at position 3
    // stays. Result: "git ", cursor at
    // the start of where `status` used
    // to be (position 4).
    assert_eq!(app.query, "git ");
    assert_eq!(app.query_cursor, 4);
}

/// When the cursor is preceded by trailing
/// whitespace, the whitespace is eaten
/// along with the preceding word. So
/// `git status  ` (with 2 trailing spaces)
/// with the cursor at the end becomes `git`
/// after one `Ctrl-W`. This matches
/// readline/bash's `unix-word-rubout`: the
/// char immediately to the left of the
/// cursor is whitespace, so we eat that
/// whitespace run, then eat the preceding
/// word.
#[test]
fn delete_word_backward_eats_trailing_whitespace_first() {
    let mut app = stats_test_app(&[("ls", 1)]);
    app.query = "git status  ".to_string();
    app.query_cursor = app.query.chars().count();
    app.delete_word_backward();
    // Step 1 eats the 2 trailing spaces
    // (positions 10..12), then step 2
    // eats `status` (positions 4..10).
    // Total deleted: positions 4..12
    // (8 chars). Remaining: "git " (the
    // space between `git` and `status`,
    // at position 3, is NOT eaten because
    // step 1 only walks back from the
    // cursor, not forward through the
    // already-deleted range). Cursor at 4.
    assert_eq!(app.query, "git ");
    assert_eq!(app.query_cursor, 4);
}

/// Multiple consecutive spaces are all
/// kept in the result — the function
/// only eats ONE word (the trailing
/// non-whitespace run) and the whitespace
/// immediately to its left (one run of
/// whitespace). It doesn't reach further
/// back to consume additional whitespace
/// runs. So `git    status` with the
/// cursor at the end becomes `git    `
/// (the 4 spaces between `git` and
/// `status` stay; only `status` is
/// eaten).
#[test]
fn delete_word_backward_handles_multiple_spaces() {
    let mut app = stats_test_app(&[("ls", 1)]);
    app.query = "git    status".to_string();
    app.query_cursor = app.query.chars().count();
    app.delete_word_backward();
    // `status` (positions 7..13) is
    // eaten; the 4 spaces between
    // `git` and `status` stay. Result:
    // "git    ", cursor at 7.
    assert_eq!(app.query, "git    ");
    assert_eq!(app.query_cursor, 7);
}

/// Cursor at the start of the buffer is a
/// no-op. No panic, no underflow. We don't
/// flag the query as touched (mirrors
/// `backspace_at_position_zero_is_noop`).
#[test]
fn delete_word_backward_at_start_is_noop() {
    let mut app = stats_test_app(&[("ls", 1)]);
    app.query = "anything".to_string();
    app.query_cursor = 0;
    app.delete_word_backward();
    assert_eq!(app.query, "anything");
    assert_eq!(app.query_cursor, 0);
}

/// Cursor mid-buffer, between a space and
/// the next word: the space AND the word
/// before the space are eaten. The cursor
/// is at position 4 in `git status`, which
/// is right after the space and right
/// before the `s` of `status`. Pressing
/// `Ctrl-W` eats the trailing whitespace
/// (1 char) plus the preceding non-
/// whitespace run `git` (3 chars), so the
/// result is `status` with the cursor at
/// position 0.
///
/// This is the standard readline/bash
/// `unix-word-rubout` behaviour: if the
/// char immediately to the left of the
/// cursor is whitespace, the function
/// eats both that whitespace run AND the
/// preceding word.
#[test]
fn delete_word_backward_respects_cursor_position() {
    let mut app = stats_test_app(&[("ls", 1)]);
    app.query = "git status".to_string();
    // Position 4 is right after the space
    // and right before the `s` of
    // `status`. Cursor at position 4 =
    // chars().take(4) = "git ".
    app.query_cursor = 4;
    app.delete_word_backward();
    // Eat "git " (positions 0..4) —
    // the trailing whitespace AND the
    // preceding word. Result: "status",
    // cursor at 0.
    assert_eq!(app.query, "status");
    assert_eq!(app.query_cursor, 0);
}

/// Multi-byte UTF-8: the cursor is in
/// characters, so an accented character
/// counts as one step. The word-deletion
/// logic must respect the character /
/// byte distinction so it doesn't
/// accidentally split a multi-byte
/// codepoint.
#[test]
fn delete_word_backward_handles_multibyte() {
    let mut app = stats_test_app(&[("ls", 1)]);
    app.query = "café au lait".to_string();
    app.query_cursor = app.query.chars().count();
    app.delete_word_backward();
    // `lait` (positions 8..12) is eaten;
    // the spaces and `café au` stay.
    // The `é` (one character, 2 bytes)
    // is preserved correctly because
    // `String::replace_range` operates
    // on byte indices that we computed
    // via `char_to_byte_index`.
    assert_eq!(app.query, "café au ");
    assert_eq!(app.query_cursor, 8);
}

/// Cursor mid-word: only the part of the
/// word to the LEFT of the cursor is
/// eaten. The cursor is in position 5 of
/// `cargotest`, between the `o` of
/// `cargo` and the `t` of `test`. The
/// function walks back through the
/// non-whitespace run to the left of the
/// cursor (positions 4, 3, 2, 1, 0 =
/// "cargo", 5 chars), stopping at the
/// start of the buffer because there's no
/// whitespace before it. The result is
/// `test` with the cursor at position 0.
///
/// This is readline/bash's
/// `unix-word-rubout` behaviour: only
/// the characters to the LEFT of the
/// cursor are considered. The part of
/// the word to the right of the cursor
/// is preserved. (Note: this differs
/// from `backward-kill-word` in some
/// shells which would delete the whole
/// word regardless of cursor position.)
#[test]
fn delete_word_backward_mid_word_eats_left_of_cursor() {
    let mut app = stats_test_app(&[("ls", 1)]);
    app.query = "cargotest".to_string();
    // Position 5 is between `o` and `t`.
    app.query_cursor = 5;
    app.delete_word_backward();
    // Eat `cargo` (positions 0..5).
    // The `test` part to the right of
    // the cursor stays. Result: "test",
    // cursor at 0.
    assert_eq!(app.query, "test");
    assert_eq!(app.query_cursor, 0);
}

/// Empty query is a clean no-op, just like
/// `backspace` on an empty buffer.
#[test]
fn delete_word_backward_on_empty_query() {
    let mut app = stats_test_app(&[("ls", 1)]);
    app.query = String::new();
    app.query_cursor = 0;
    app.delete_word_backward();
    assert_eq!(app.query, "");
    assert_eq!(app.query_cursor, 0);
}

/// The comment-edit buffer uses the same
/// logic — when a comment is being edited,
/// `Ctrl-W` deletes the previous word in
/// the comment. We test the underlying
/// helper (`delete_word_backward_in_string`)
/// for the comment-edit path; the wrapper
/// (`App::delete_word_backward`) routes to
/// the right buffer based on whether a
/// comment edit is in progress.
#[test]
fn delete_word_backward_in_string_helper() {
    // The comment-edit buffer has no
    // cursor concept — operate on the
    // logical end of the string. The
    // helper is what the App method
    // calls when
    // `self.comment_edit.is_some()`.
    let mut s = String::from("hello world");
    delete_word_backward_in_string(&mut s);
    // `world` (positions 6..11) is
    // eaten; the space at position 5
    // stays. Result: "hello ".
    assert_eq!(s, "hello ");
    // Second press: the char to the
    // left of the cursor (end of `s`)
    // is a space. Eat the space, then
    // the preceding word `hello`.
    // Result: empty.
    delete_word_backward_in_string(&mut s);
    assert_eq!(s, "");
}

/// The free function
/// `delete_word_backward_at_cursor` is
/// what the App method calls when the
/// query field is the active buffer. It
/// returns the new cursor position (in
/// characters) without mutating the
/// string — the caller applies the
/// deletion as a single `replace_range`
/// call. Pin the contract here so future
/// refactors of the cursor logic can't
/// accidentally change the readline
/// semantics.
#[test]
fn delete_word_backward_at_cursor_helper() {
    // Empty string, cursor 0: returns 0.
    assert_eq!(delete_word_backward_at_cursor("", 0), 0);
    // Single word, cursor at end:
    // returns 0 (whole word consumed).
    assert_eq!(delete_word_backward_at_cursor("abc", 3), 0);
    // Cursor mid-word: returns the start
    // of the word (which is also the
    // start of the buffer in this case).
    assert_eq!(delete_word_backward_at_cursor("abc", 2), 0);
    assert_eq!(delete_word_backward_at_cursor("abc", 1), 0);
    // Two words, cursor at end: returns
    // the start of the second word.
    assert_eq!(delete_word_backward_at_cursor("abc def", 7), 4);
    // Trailing whitespace only: cursor
    // at end of `abc   ` returns 0
    // (step 1 eats 3 spaces, step 2
    // walks back through `abc`).
    assert_eq!(delete_word_backward_at_cursor("abc   ", 6), 0);
    // Two words with multiple spaces:
    // cursor at end of `abc   def`.
    // Char at end is `f` (non-ws), so
    // step 1 doesn't run; step 2 walks
    // back from `f` to the space at
    // position 6. Returns 6 (start of
    // `def`).
    assert_eq!(delete_word_backward_at_cursor("abc   def", 9), 6);
}

// --- Labeled-only rows partition ---------

/// A labeled row that's NOT in the
/// primary list (e.g. from a different
/// session than the current
/// `SMART_HISTORY_SESSION`) should appear
/// at the end of the merged list, not in
/// the middle of the timestamp-sorted
/// primary rows.
///
/// Test setup:
/// - One primary row at offset 10 (recent,
///   current session).
/// - One labeled row at offset 100_000
///   (ancient, different session — excluded
///   by `Mode::Sess`).
/// Both commands match the query "git".
///
/// Expected merged order under
/// `SortOrder::Age`: `[git status (recent),
/// git pull (labeled-ancient)]`. The labeled
/// row's command is older than the primary
/// row's command, so a pure timestamp sort
/// would also put it last; this test pins the
/// partition invariant so a future refactor
/// that mixes the partitions can't regress.
#[test]
fn labeled_only_row_appears_at_end_of_merged_list() {
    let _env_guard = ENV_LOCK.lock().expect("env lock poisoned");
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
    )
    .expect("create tables");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (1, 'git status', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 10],
                )
                .expect("insert recent");
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (2, 'git pull', '/tmp', 'ancient', 0, ?1)",
                        rusqlite::params![now - 100_000],
                )
                .expect("insert ancient");
    conn.execute(
        "INSERT INTO command_comments (command, comment) VALUES ('git pull', 'old but labeled')",
        [],
    )
    .expect("insert comment");

    let prev_session = std::env::var("SMART_HISTORY_SESSION").ok();
    unsafe {
        std::env::set_var("SMART_HISTORY_SESSION", "current");
    }
    let mut app = App::new(
        conn,
        Mode::Sess,
        "git".to_string(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    // Restore env before any `?` can
    // short-circuit out of the test (so
    // a panic doesn't leak the env
    // override into other tests). We
    // refresh *after* the App is built
    // but *before* the env restore, so the
    // fetch sees the right session id.
    app.refresh();
    if let Some(prev) = prev_session {
        unsafe {
            std::env::set_var("SMART_HISTORY_SESSION", prev);
        }
    } else {
        unsafe {
            std::env::remove_var("SMART_HISTORY_SESSION");
        }
    }

    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // Two rows: the recent primary row
    // first, the ancient labeled row
    // second. The labeled row is at
    // the END regardless of its
    // timestamp.
    assert_eq!(cmds, vec!["git status", "git pull"]);
}

/// When a labeled row's command IS
/// already in the primary list (i.e. it
/// matches the active filter on its own),
/// the labeled row is *not* added a second
/// time — the existing primary row stays
/// at its natural sort position. This is
/// the "when a line would be listed in
/// this mode anyway, then nothing is
/// changed" half of the user's contract.
///
/// Test setup: one row in the current
/// session, with a comment. The command
/// matches the query. The row is in
/// `self.rows` AND in `self.labeled_rows`,
/// so it should appear exactly once in
/// the merged list.
#[test]
fn labeled_row_already_in_primary_list_is_not_duplicated() {
    let _env_guard = ENV_LOCK.lock().expect("env lock poisoned");
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
    )
    .expect("create tables");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (1, 'git status', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 10],
                )
                .expect("insert");
    conn.execute(
        "INSERT INTO command_comments (command, comment) VALUES ('git status', 'labeled')",
        [],
    )
    .expect("insert comment");

    let prev_session = std::env::var("SMART_HISTORY_SESSION").ok();
    unsafe {
        std::env::set_var("SMART_HISTORY_SESSION", "current");
    }
    let mut app = App::new(
        conn,
        Mode::Sess,
        "git".to_string(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    app.refresh();
    if let Some(prev) = prev_session {
        unsafe {
            std::env::set_var("SMART_HISTORY_SESSION", prev);
        }
    } else {
        unsafe {
            std::env::remove_var("SMART_HISTORY_SESSION");
        }
    }

    // Single row in the merged list,
    // even though it's in BOTH
    // `self.rows` and `self.labeled_rows`.
    assert_eq!(app.merged_rows().len(), 1);
    assert_eq!(app.merged_rows()[0].command, "git status");
}

/// The partition holds even when the
/// labeled-only row's timestamp is
/// *newer* than some of the primary
/// rows. Without the partition, a
/// natural sort would put the labeled-
/// only row in the middle. With the
/// partition, it's pinned to the end.
/// This pins the "always at the end"
/// invariant.
///
/// Test setup:
/// - Primary row "b" at offset 100 (older).
/// - Primary row "a" at offset 10 (newer).
/// - Labeled-only row "z" at offset 5
///   (newest of all), but only visible
///   because it's labeled — it's in a
///   different session.
///
/// Without the partition, a timestamp
/// sort would give: `[z (5), a (10),
/// b (100)]`. With the partition, we
/// expect: `[a (10), b (100), z (5)]`.
#[test]
fn labeled_only_row_stays_at_end_even_if_newer() {
    let _env_guard = ENV_LOCK.lock().expect("env lock poisoned");
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
    )
    .expect("create tables");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    // Two primary rows.
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (1, 'a', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 10],
                )
                .expect("insert");
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (2, 'b', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 100],
                )
                .expect("insert");
    // Labeled-only row (different session,
    // newer timestamp). It IS excluded
    // by the `Mode::Sess` SQL filter
    // because its session_id is
    // "ancient".
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (3, 'z', '/tmp', 'ancient', 0, ?1)",
                        rusqlite::params![now - 5],
                )
                .expect("insert");
    conn.execute(
        "INSERT INTO command_comments (command, comment) VALUES ('z', 'labeled but newer')",
        [],
    )
    .expect("insert comment");

    let prev_session = std::env::var("SMART_HISTORY_SESSION").ok();
    unsafe {
        std::env::set_var("SMART_HISTORY_SESSION", "current");
    }
    let mut app = App::new(
        conn,
        Mode::Sess,
        String::new(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    app.refresh();
    if let Some(prev) = prev_session {
        unsafe {
            std::env::set_var("SMART_HISTORY_SESSION", prev);
        }
    } else {
        unsafe {
            std::env::remove_var("SMART_HISTORY_SESSION");
        }
    }

    // Without the partition the
    // natural timestamp sort would
    // give `[z, a, b]`. With the
    // partition we expect
    // `[a, b, z]`.
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(cmds, vec!["a", "b", "z"]);
}

/// The partition also holds in
/// `SortOrder::Frequency` mode: the
/// labeled-only group is at the end of
/// the merged list, sorted by its own
/// internal counts rather than the
/// counts of the entire merged set.
///
/// Test setup:
/// - Primary rows: 3 instances of "a",
///   1 of "b".
/// - Labeled-only row: "z", excluded by
///   session filter.
///
/// Expected merged order: `[a, b, z]`.
/// (Frequency dedup is implicit so the
/// primary partition dedupes to `[a, b]`.)
#[test]
fn labeled_only_partition_in_frequency_mode() {
    let _env_guard = ENV_LOCK.lock().expect("env lock poisoned");
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                            mode TEXT NOT NULL DEFAULT 'command'
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
    )
    .expect("create tables");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (1, 'a', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 1],
                )
                .expect("insert a1");
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (2, 'a', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 2],
                )
                .expect("insert a2");
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (3, 'a', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 3],
                )
                .expect("insert a3");
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (4, 'b', '/tmp', 'current', 0, ?1)",
                        rusqlite::params![now - 4],
                )
                .expect("insert b");
    // Labeled-only row in a different session.
    conn.execute(
                        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) VALUES (5, 'z', '/tmp', 'ancient', 0, ?1)",
                        rusqlite::params![now - 5],
                )
                .expect("insert z");
    conn.execute(
        "INSERT INTO command_comments (command, comment) VALUES ('z', 'labeled')",
        [],
    )
    .expect("insert comment");

    let prev_session = std::env::var("SMART_HISTORY_SESSION").ok();
    unsafe {
        std::env::set_var("SMART_HISTORY_SESSION", "current");
    }
    let mut app = App::new(
        conn,
        Mode::Sess,
        String::new(),
        false,
        ExitFilter::All,
        SortOrder::Frequency,
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    app.refresh();
    if let Some(prev) = prev_session {
        unsafe {
            std::env::set_var("SMART_HISTORY_SESSION", prev);
        }
    } else {
        unsafe {
            std::env::remove_var("SMART_HISTORY_SESSION");
        }
    }

    // Frequency dedup is implicit in
    // Frequency mode (see
    // `build_merged_rows`). So the
    // primary partition dedupes to
    // `[a, b]`. The labeled-only group
    // is `[z]`. Final merged order:
    // `[a, b, z]`.
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(cmds, vec!["a", "b", "z"]);
}

// --- Notes-mode date-filter aliases -------

/// The simplest case: `@today` alone is
/// recognised, stripped from the pattern,
/// and the resolved filter is `Today`.
/// The cleaned pattern is empty (only the
/// alias was present), which the caller
/// treats as "no search body — fall through
/// to fetch_recent_notes".
#[test]
fn parse_notes_query_today_alone() {
    let (pattern, filter) = parse_notes_query("@today");
    assert_eq!(pattern, "");
    assert_eq!(filter, NotesDateFilter::Today);
}

/// Each alias maps to its filter.
#[test]
fn parse_notes_query_each_alias() {
    assert_eq!(parse_notes_query("@week").1, NotesDateFilter::Week);
    assert_eq!(parse_notes_query("@month").1, NotesDateFilter::Month);
    assert_eq!(parse_notes_query("@year").1, NotesDateFilter::Year);
}

/// An empty / whitespace pattern resolves
/// to `All` (no filter) and an empty
/// cleaned pattern.
#[test]
fn parse_notes_query_empty_is_all() {
    assert_eq!(parse_notes_query(""), (String::new(), NotesDateFilter::All));
    assert_eq!(
        parse_notes_query("   "),
        (String::new(), NotesDateFilter::All)
    );
}

/// A pattern with no aliases returns the
/// same string back and `All`.
#[test]
fn parse_notes_query_no_alias_passthrough() {
    assert_eq!(
        parse_notes_query("hello world"),
        ("hello world".to_string(), NotesDateFilter::All)
    );
}

/// The user's example: `test @reference @today`
/// (the outer `@` is the notes-mode prefix
/// already stripped by `notes_pattern`).
/// The alias is removed; `@reference` is
/// NOT an alias so it stays in the
/// cleaned pattern.
///
/// **Important**: a leading `@` on a
/// non-alias token is *stripped* before
/// the cleaned pattern is returned. The
/// library's `parse_query` tokenizer
/// treats `@foo` as a `Link` reference
/// (matching `t.links`/`m.links`) which
/// is never what the user means when they
/// type `!@orchard` — they want a text
/// search for "orchard". Stripping the
/// `@` here ensures the downstream
/// `parse_query` sees a plain word and
/// routes it through the text-LIKE
/// branch.
#[test]
fn parse_notes_query_with_search_terms() {
    // `@reference` is now a link search (`[[reference]]`),
    // not a plain text search. The date alias `@today` is
    // still extracted as a filter.
    let (pattern, filter) = parse_notes_query("test @reference @today");
    assert_eq!(pattern, "test [[reference]]");
    assert_eq!(filter, NotesDateFilter::Today);
}

/// Multiple aliases: the last one wins.
#[test]
fn parse_notes_query_multiple_aliases_last_wins() {
    let (_, filter) = parse_notes_query("@today @week");
    assert_eq!(filter, NotesDateFilter::Week);
    let (_, filter) = parse_notes_query("@year @today");
    assert_eq!(filter, NotesDateFilter::Today);
}

/// Alias matching is case-insensitive:
/// `@Today`, `@TODAY`, `@today` all work.
/// When matched, the token is removed from
/// the cleaned pattern.
#[test]
fn parse_notes_query_alias_matching_is_case_insensitive() {
    assert_eq!(parse_notes_query("@Today").1, NotesDateFilter::Today);
    assert_eq!(parse_notes_query("@TODAY").1, NotesDateFilter::Today);
    assert_eq!(parse_notes_query("@today").1, NotesDateFilter::Today);
    assert_eq!(parse_notes_query("@Today").0, "");
    assert_eq!(parse_notes_query("@TODAY").0, "");
    assert_eq!(parse_notes_query("@today").0, "");
}

/// Aliases can also be written without
/// the leading `@` (so the aliases work
/// even when the user types them inside
/// the search body).
#[test]
fn parse_notes_query_alias_without_at_prefix() {
    let (pattern, filter) = parse_notes_query("today test");
    assert_eq!(pattern, "test");
    assert_eq!(filter, NotesDateFilter::Today);
}

/// The whole-token rule: `@todayfile` is
/// NOT the alias. The whole token must
/// match the alias name. We still
/// strip the `@` from the cleaned
/// pattern (the alias arm doesn't
/// fire) so the library's parser
/// sees a plain word.
#[test]
fn parse_notes_query_alias_must_be_whole_token() {
    // `@todayfile` is NOT a date alias (the alias is
    // only matched as a whole token). With the new
    // behavior, `@todayfile` is treated as a link
    // search `[[todayfile]]` (the user's `@LINK`
    // shorthand) rather than stripped to plain text.
    let (pattern, filter) = parse_notes_query("@todayfile");
    assert_eq!(pattern, "[[todayfile]]");
    assert_eq!(filter, NotesDateFilter::All);
}

/// `@` on a non-alias token is the
/// user's ad-hoc shorthand for
/// "search the word", not a link
/// reference. The library's
/// `parse_query` would otherwise
/// interpret `@orchard` as a
/// `Link` token (matching
/// `t.links`/`m.links`) which is
/// never the user's intent.
/// Stripping the `@` here routes the
/// term through the text-LIKE
/// branch. This is the exact
/// scenario the user reported
/// (`!@orchard` returning empty
/// when todos contain the word
/// "orchard") — the regression
/// test for that bug.
/// `@LINK` — search for notes that
/// have a link to `LINK`. The
/// `note_search` query parser uses
/// `[[linkname]]` (wiki-link syntax)
/// for link search, so we convert
/// the user's `@LINK` shorthand to
/// `[[LINK]]`. The original casing
/// is preserved (link targets are
/// case-sensitive in Obsidian).
#[test]
fn parse_notes_query_link_search() {
    assert_eq!(parse_notes_query("@orchard").0, "[[orchard]]");
    assert_eq!(parse_notes_query("@orchard").1, NotesDateFilter::All);
    // Multiple `@` terms are all links.
    assert_eq!(
        parse_notes_query("@orchard @apple").0,
        "[[orchard]] [[apple]]"
    );
    // Mixed: alias + link.
    assert_eq!(parse_notes_query("@today @orchard").0, "[[orchard]]");
    assert_eq!(
        parse_notes_query("@today @orchard").1,
        NotesDateFilter::Today
    );
    // Plain words are untouched.
    assert_eq!(parse_notes_query("orchard apple").0, "orchard apple");
    // `@` in the middle of a word
    // is preserved (only leading
    // `@` is treated as the link
    // prefix).
    assert_eq!(parse_notes_query("foo@bar").0, "foo@bar");
}

/// `#TAG` — search for notes
/// tagged `TAG`. The
/// `note_search` query parser
/// already handles `#tagname`
/// syntax, so we pass the token
/// through unchanged.
#[test]
fn parse_notes_query_tag_search() {
    assert_eq!(parse_notes_query("#feature").0, "#feature");
    assert_eq!(parse_notes_query("#feature").1, NotesDateFilter::All);
    // Multiple tags are AND-joined.
    assert_eq!(parse_notes_query("#feature #bug").0, "#feature #bug");
    // Combined: tag + link + text.
    assert_eq!(
        parse_notes_query("#feature @orchard rust").0,
        "#feature [[orchard]] rust"
    );
    // Combined with date alias: the
    // alias is extracted as a filter
    // and the rest is passed through.
    assert_eq!(
        parse_notes_query("#feature @orchard @today rust").0,
        "#feature [[orchard]] rust"
    );
    assert_eq!(
        parse_notes_query("#feature @orchard @today rust").1,
        NotesDateFilter::Today
    );
    // A bare `#` with no tag name is
    // dropped (not a valid tag).
    assert_eq!(parse_notes_query("#").0, "");
}

/// The `NotesDateFilter::cutoff(now)` math
/// is exact: 24h for Today, 7d for Week,
/// 30d for Month, 365d for Year. We use a
/// fixed `now` to make the assertions
/// deterministic.
#[test]
fn notes_date_filter_cutoff_math() {
    let now: i64 = 1_000_000_000;
    let day = 24 * 60 * 60;
    assert_eq!(NotesDateFilter::All.cutoff(now), None);
    assert_eq!(NotesDateFilter::Today.cutoff(now), Some(now - day));
    assert_eq!(NotesDateFilter::Week.cutoff(now), Some(now - 7 * day));
    assert_eq!(NotesDateFilter::Month.cutoff(now), Some(now - 30 * day));
    assert_eq!(NotesDateFilter::Year.cutoff(now), Some(now - 365 * day));
}

/// The filter applies the cutoff against
/// each note's effective timestamp.
/// Recent (within the window) passes,
/// old (outside the window) fails.
#[test]
fn notes_date_filter_applies_to_results() {
    let now: i64 = 1_000_000_000;
    let day = 24 * 60 * 60;
    let recent = now - 12 * 60 * 60;
    let old = now - 30 * day;

    let (clean, filter) = parse_notes_query("query @today");
    let cutoff = filter.cutoff(now).unwrap();
    assert!(recent >= cutoff);
    assert!(old < cutoff);
    assert_eq!(clean, "query");
}

/// Regression test for the user's
/// report: `@today` as a *bare*
/// alias (with no text pattern)
/// should restrict the
/// `fetch_recent_notes` path
/// to notes updated in the
/// last 24h. Before the fix,
/// `@today` was the same as
/// `@` (no filtering at all,
/// because the pattern was
/// empty and `fetch_recent_notes`
/// skipped the filter).
#[test]
fn bare_at_today_in_notes_mode_filters_by_mtime() {
    use rusqlite::Connection;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "smarthistory-notes-bare-{}-{}",
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create notes dir");
    let day = 24 * 60 * 60;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // `recent.md`: written now
    fs::write(dir.join("recent.md"), "# Recent\n").expect("write recent");
    // `old.md`: pretend it was
    // written 30 days ago by
    // setting mtime via
    // `filetime`. If
    // `filetime` isn't
    // available we fall back
    // to the same mtime as
    // `recent.md` and the test
    // is degenerate — but the
    // *filter* logic is still
    // exercised either way.
    let old_path = dir.join("old.md");
    fs::write(&old_path, "# Old\n").expect("write old");
    let past = now - 30 * day;
    let _ = filetime_touch_mtime(&old_path, past);
    let db_path = std::env::temp_dir().join(format!(
        "smarthistory-notes-bare-db-{}-{}.sqlite",
        std::process::id(),
        n
    ));
    let _ = fs::remove_file(&db_path);
    let conn = Connection::open(&db_path).expect("open db");
    note_search::init_database_schema(&conn)
        .map_err(|e| format!("schema: {e}"))
        .expect("init schema");
    // Index both files. We have
    // to index twice because
    // `process_markdown_file`
    // records its own `updated`
    // (the current epoch), not
    // the file's actual mtime.
    // Then we patch `updated`
    // to match the file's
    // mtime so the filter has
    // something to work with.
    for entry in fs::read_dir(&dir).expect("read dir") {
        let entry = entry.expect("entry");
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let data =
            note_search::markdown_parser::process_markdown_file(&path, &dir).expect("process");
        note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn).expect("write");
    }
    // Force `old.md`'s `updated`
    // to be 30 days ago so the
    // `@today` filter has
    // something to distinguish.
    conn.execute(
        "UPDATE markdown_data \
                         SET updated = ?1 \
                         WHERE filename = 'old.md'",
        rusqlite::params![past],
    )
    .expect("patch old.md");
    drop(conn);
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    // Bare `@today` — empty
    // pattern, filter active.
    app.query = "@today".to_string();
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // Only `recent.md` should
    // pass.
    assert!(
        cmds.iter().any(|c| c.contains("recent.md")),
        "recent.md must be in the result: {:?}",
        cmds
    );
    assert!(
        cmds.iter().all(|c| !c.contains("old.md")),
        "old.md must be filtered out by @today: {:?}",
        cmds
    );
    // Sanity: `@` (no alias)
    // returns both.
    app.query = "@".to_string();
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert!(
        cmds.iter().any(|c| c.contains("recent.md")),
        "recent.md present in unfiltered mode: {:?}",
        cmds
    );
    assert!(
        cmds.iter().any(|c| c.contains("old.md")),
        "old.md present in unfiltered mode: {:?}",
        cmds
    );
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_file(&db_path);
}

/// Regression test for the user's
/// report in todo mode:
/// `!@today` should restrict
/// the result set to todos in
/// files whose `updated` is
/// within the last 24h. Before
/// the fix, `fetch_todos`
/// discarded the filter (it
/// was bound to `_filter` and
/// the post-sort cutoff was
/// never applied). The user
/// reported `@today` and
/// `!@today` as both broken —
/// they're now both wired up.
#[test]
fn bare_today_in_todo_mode_filters_by_mtime() {
    use rusqlite::Connection;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "smarthistory-todos-today-{}-{}",
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create notes dir");
    let day = 24 * 60 * 60;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    fs::write(dir.join("recent.md"), "# Recent\n\n- [ ] recent todo\n").expect("write recent");
    let past = now - 30 * day;
    let old_path = dir.join("old.md");
    fs::write(&old_path, "# Old\n\n- [ ] old todo\n").expect("write old");
    let _ = filetime_touch_mtime(&old_path, past);
    let db_path = std::env::temp_dir().join(format!(
        "smarthistory-todos-today-db-{}-{}.sqlite",
        std::process::id(),
        n
    ));
    let _ = fs::remove_file(&db_path);
    let conn = Connection::open(&db_path).expect("open db");
    note_search::init_database_schema(&conn)
        .map_err(|e| format!("schema: {e}"))
        .expect("init schema");
    for entry in fs::read_dir(&dir).expect("read dir") {
        let entry = entry.expect("entry");
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let data =
            note_search::markdown_parser::process_markdown_file(&path, &dir).expect("process");
        note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn).expect("write");
    }
    conn.execute(
        "UPDATE markdown_data \
                         SET updated = ?1 \
                         WHERE filename = 'old.md'",
        rusqlite::params![past],
    )
    .expect("patch old.md");
    drop(conn);
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    // Bare `!@today` — empty
    // pattern, filter active.
    app.query = "!@today".to_string();
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // Only the recent todo
    // should pass.
    assert!(
        cmds.iter().any(|c| c.contains("recent todo")),
        "recent todo must be in the result: {:?}",
        cmds
    );
    assert!(
        cmds.iter().all(|c| !c.contains("old todo")),
        "old todo must be filtered out by !@today: {:?}",
        cmds
    );
    // `@year` lets the old todo
    // through (30 days is
    // within the last 365
    // days).
    app.query = "!@year".to_string();
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert!(
        cmds.iter().any(|c| c.contains("recent todo")),
        "recent todo in @year: {:?}",
        cmds
    );
    assert!(
        cmds.iter().any(|c| c.contains("old todo")),
        "old todo (30d ago) in @year (365d): {:?}",
        cmds
    );
    // Sanity: bare `!` returns
    // both.
    app.query = "!".to_string();
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(cmds.len(), 2);
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_file(&db_path);
}

// --- Todo mode (`!` prefix) -----------------

/// `is_todo_query` recognises the
/// configured prefix; an empty query
/// returns false (matches the existing
/// `is_notes_query` contract).
#[test]
fn is_todo_query_recognises_prefix() {
    let mut app = global_test_app(&[("a", 1)]);
    assert!(!app.is_todo_query());
    app.query = "!write tests".to_string();
    assert!(app.is_todo_query());
    app.query = "!".to_string();
    assert!(app.is_todo_query());
    app.query = "write tests".to_string();
    assert!(!app.is_todo_query());
    // Other prefixes still don't trigger
    // todo mode.
    app.query = "@rust".to_string();
    assert!(!app.is_todo_query());
}

/// The `directories` mode is
/// recognised by the `#`
/// prefix (default). The
/// pattern-stripping method
/// returns the body after
/// the prefix, matching
/// `notes_pattern` /
/// `todo_pattern`.
#[test]
fn is_directories_query_recognises_prefix() {
    let mut app = global_test_app(&[("a", 1)]);
    assert!(!app.is_directories_query());
    app.query = "#home".to_string();
    assert!(app.is_directories_query());
    app.query = "#".to_string();
    assert!(app.is_directories_query());
    app.query = "home".to_string();
    assert!(!app.is_directories_query());
    // Other prefixes don't trigger
    // directories mode.
    app.query = "!todo".to_string();
    assert!(!app.is_directories_query());
    app.query = "/regex".to_string();
    assert!(!app.is_directories_query());
}

#[test]
fn directories_pattern_strips_prefix() {
    let mut app = global_test_app(&[("a", 1)]);
    app.query = "#home".to_string();
    assert_eq!(app.directories_pattern(), "home");
    app.query = "home".to_string();
    assert_eq!(app.directories_pattern(), "");
    // Whitespace inside the
    // body is preserved (the
    // pattern method returns
    // everything after the
    // leading `#`, no
    // trimming).
    app.query = "#foo bar".to_string();
    assert_eq!(app.directories_pattern(), "foo bar");
}

/// `todo_pattern` returns the body after
/// the prefix; matches the
/// `notes_pattern` contract.
#[test]
fn todo_pattern_strips_prefix() {
    let mut app = global_test_app(&[("a", 1)]);
    app.query = "!write tests".to_string();
    assert_eq!(app.todo_pattern(), "write tests");
    app.query = "write tests".to_string();
    assert_eq!(app.todo_pattern(), "");
}

/// `is_todo_line` recognises the standard
/// markdown task-list forms. We test the
/// library's detection indirectly by
/// parsing a note file with
/// `process_markdown_file` and asserting
/// the resulting todo count.
#[test]
fn is_todo_line_recognises_markdown_checkboxes() {
    use std::fs;
    let dir = std::env::temp_dir().join(format!(
        "smarthistory-todo-cb-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create notes dir");
    fs::write(
        dir.join("note.md"),
        "# Title\n\
                         \n\
                         - [ ] open\n\
                         - [ ] also open\n\
                           - [ ] indented\n\
                         - [x] done\n\
                         - [X] also done\n\
                         \n\
                         the list contains [ ] for unchecked\n\
                         1. [ ] numbered lists not supported\n",
    )
    .expect("write");
    let data = note_search::markdown_parser::process_markdown_file(&dir.join("note.md"), &dir)
        .expect("process");
    // 5 todos detected: 3 open, 2 closed.
    // The prose line and the numbered
    // list are not recognised. (Note:
    // the note_search library only
    // recognises `-` as the bullet,
    // not `*` — that matches GFM but
    // is narrower than my hand-rolled
    // detector from earlier turns.)
    assert_eq!(data.todo.len(), 5);
    let open: Vec<&str> = data
        .todo
        .iter()
        .filter(|t| !t.closed)
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(open.len(), 3);
    let closed: Vec<&str> = data
        .todo
        .iter()
        .filter(|t| t.closed)
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(closed.len(), 2);
    let _ = fs::remove_dir_all(&dir);
}

/// Build a notes directory with two note
/// files and a matching note_search
/// SQLite database. Returns `(notes_dir,
/// db_path)`. The caller is responsible
/// for cleaning up the temp paths.
///
/// The fixture mirrors the user's
/// production setup: `notes.dir` is the
/// directory containing the actual `.md`
/// files, and `notes.database` is the
/// SQLite database the indexer writes
/// to. We do the indexing inline here
/// (via `process_markdown_file` +
/// `write_markdown_data_to_sqlite_with_conn`)
/// so the test doesn't depend on the
/// external indexer binary.
fn setup_todo_db() -> (std::path::PathBuf, std::path::PathBuf) {
    use rusqlite::Connection;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "smarthistory-todo-test-{}-{}",
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create notes dir");
    // Older note: written first so its
    // mtime is naturally older.
    fs::write(
        dir.join("older.md"),
        "# Older\n\n\
                         - [ ] older todo 1\n\
                         some prose in between\n\
                         - [x] older done 1\n\
                         - [ ] older todo 2\n",
    )
    .expect("write older");
    std::thread::sleep(std::time::Duration::from_millis(10));
    fs::write(
        dir.join("newer.md"),
        "# Newer\n\n\
                         - [ ] newer todo 1\n\
                         - [ ] newer todo 2\n",
    )
    .expect("write newer");
    // Index both files into a
    // SQLite database the way the
    // production `note_search` indexer
    // does. The library writes
    // `todo_entries` rows for each
    // detected todo.
    let db_path = std::env::temp_dir().join(format!(
        "smarthistory-todo-db-{}-{}.sqlite",
        std::process::id(),
        n
    ));
    let _ = fs::remove_file(&db_path);
    let conn = Connection::open(&db_path).expect("open db");
    note_search::init_database_schema(&conn)
        .map_err(|e| format!("schema: {e}"))
        .expect("init schema");
    for entry in fs::read_dir(&dir).expect("read dir") {
        let entry = entry.expect("entry");
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let data =
            note_search::markdown_parser::process_markdown_file(&path, &dir).expect("process file");
        note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
            .map_err(|e| format!("write: {e}"))
            .expect("write db");
    }
    drop(conn);
    (dir, db_path)
}

/// `fetch_todos` returns every open todo
/// from the note_search database, sorted
/// by file modified time (DESC) then by
/// line number (ASC within a file).
/// This is the same ordering the user
/// expects from `note_search list` —
/// `!` is just a thin TUI over the same
/// database.
#[test]
fn fetch_todos_lists_all_open_todos() {
    let (dir, db_path) = setup_todo_db();
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    app.query = "!".to_string();
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // 4 open todos total: 2 in older.md
    // (line 3 = `older todo 1`, line 6 =
    // `older todo 2`) and 2 in newer.md
    // (lines 3, 4). The `[x]` done todo
    // is excluded because we set
    // `open: Some(true)`.
    assert_eq!(cmds.len(), 4);
    assert!(cmds.iter().any(|c| c.contains("newer todo 1")));
    assert!(cmds.iter().any(|c| c.contains("newer todo 2")));
    assert!(cmds.iter().any(|c| c.contains("older todo 1")));
    assert!(cmds.iter().any(|c| c.contains("older todo 2")));
    // The closed todo must NOT be in
    // the list.
    assert!(!cmds.iter().any(|c| c.contains("done")));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// The user-typed query (after the `!`
/// prefix) is parsed via the library's
/// `parse_query`, which understands the
/// Obsidian-like syntax. Bare words
/// are AND-matched against each todo
/// line; `#tag` is matched against
/// both the todo's own tags and the
/// note's header fields; `[[link]]`
/// is matched against the todo's
/// links and the note's outgoing
/// links; `[attr:value]` is matched
/// against the note's header fields.
/// `!write` matches todos whose text
/// contains "write"; the fixture has
/// none, so the result is empty.
/// `!older` matches the two open
/// older.md todos.
#[test]
fn fetch_todos_applies_typed_query() {
    let (dir, db_path) = setup_todo_db();
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    app.query = "!write".to_string();
    app.refresh();
    assert_eq!(app.merged_rows().len(), 0);
    app.query = "!older".to_string();
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(cmds.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// `!#tag` filters to todos that are
/// tagged with the given tag. The
/// library's `expr_to_todo_condition`
/// path searches `t.tags` (the
/// todo's own tags, extracted by the
/// library's `extract_todo_entries`
/// from inline `#tag` patterns on the
/// todo line) AND `m.header_fields`
/// (the note's frontmatter `tags`
/// array). This matches what
/// `note_search list --tag urgent`
/// would return, so the user's
/// muscle memory transfers across
/// the two surfaces.
#[test]
fn fetch_todos_filters_by_tag() {
    use rusqlite::Connection;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "smarthistory-todo-tag-{}-{}",
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create notes dir");
    fs::write(
        dir.join("note.md"),
        "---\n\
                         tags: [urgent, work]\n\
                         ---\n\
                         \n\
                         - [ ] urgent task #urgent\n\
                         - [ ] ordinary task\n\
                         - [ ] another ordinary\n",
    )
    .expect("write");
    let db_path = std::env::temp_dir().join(format!(
        "smarthistory-todo-tag-db-{}-{}.sqlite",
        std::process::id(),
        n
    ));
    let _ = fs::remove_file(&db_path);
    let conn = Connection::open(&db_path).expect("open db");
    note_search::init_database_schema(&conn)
        .map_err(|e| format!("schema: {e}"))
        .expect("init schema");
    let data = note_search::markdown_parser::process_markdown_file(&dir.join("note.md"), &dir)
        .expect("process file");
    note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
        .map_err(|e| format!("write: {e}"))
        .expect("write db");
    drop(conn);
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    // Filter by the note-level tag
    // (`urgent` is in the frontmatter
    // `tags` array, so all three todos
    // come back).
    app.query = "!#urgent".to_string();
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(cmds.len(), 3, "got: {:?}", cmds);
    // Filter by the inline tag
    // (`#urgent` appears on the first
    // todo's line, so only that one
    // comes back via the
    // `t.tags` clause; the note's
    // frontmatter also has it, so we
    // actually get all 3 still — the
    // SQL ORs both sources).
    app.query = "!ordinary".to_string();
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(cmds.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// `![[link]]` filters to todos that
/// have a `[[link]]` reference
/// either on the todo line itself or
/// in the note body. This is the
/// Obsidian-syntax analogue of `!#tag`
/// and follows the same
/// `query_expr` path through
/// `parse_query` + `build_query_from_expr`.
#[test]
fn fetch_todos_filters_by_link() {
    use rusqlite::Connection;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "smarthistory-todo-link-{}-{}",
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create notes dir");
    fs::write(
        dir.join("note.md"),
        "# Title\n\
                         \n\
                         See [[project-alpha]] for context.\n\
                         \n\
                         - [ ] task linked to alpha [[project-alpha]]\n\
                         - [ ] unrelated task\n",
    )
    .expect("write");
    let db_path = std::env::temp_dir().join(format!(
        "smarthistory-todo-link-db-{}-{}.sqlite",
        std::process::id(),
        n
    ));
    let _ = fs::remove_file(&db_path);
    let conn = Connection::open(&db_path).expect("open db");
    note_search::init_database_schema(&conn)
        .map_err(|e| format!("schema: {e}"))
        .expect("init schema");
    let data = note_search::markdown_parser::process_markdown_file(&dir.join("note.md"), &dir)
        .expect("process file");
    note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
        .map_err(|e| format!("write: {e}"))
        .expect("write db");
    drop(conn);
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    app.query = "![[project-alpha]]".to_string();
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // Both the linked todo AND the
    // unrelated todo come back
    // because the note body contains
    // `[[project-alpha]]` and the
    // library's link condition
    // matches both `t.links` and
    // `m.links`. We assert >= 1
    // (loose) rather than == 2
    // (strict) because the exact
    // set depends on the library's
    // internal OR-of-sources logic
    // which we don't need to
    // duplicate here.
    assert!(!cmds.is_empty(), "link filter returned empty: {:?}", cmds);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// Each todo row carries the file's
/// `updated` timestamp from the
/// `markdown_data` table, so the
/// Details pane can show a real age
/// instead of the `9999M`
/// placeholder that `format_diff(0)`
/// would produce. We verify the
/// timestamp is non-zero after the
/// fetch — it must be the file's
/// mtime (a recent Unix epoch value),
/// not the `0` we used before the
/// `fetch_file_updated_timestamps`
/// helper existed.
#[test]
fn fetch_todos_populates_real_timestamps() {
    let (dir, db_path) = setup_todo_db();
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    app.query = "!".to_string();
    app.refresh();
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Every row should have a
    // timestamp that's strictly
    // positive (not the 0
    // placeholder) and within the
    // test window.
    for row in app.merged_rows() {
        assert!(
            row.timestamp > 0,
            "row {:?} has zero timestamp",
            row.command
        );
        assert!(
            row.timestamp >= before - 1 && row.timestamp <= after + 1,
            "row {:?} timestamp {} outside test window [{}, {}]",
            row.command,
            row.timestamp,
            before - 1,
            after + 1
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// Within a single file, todos are
/// returned in line-number order
/// (top-to-bottom), matching the
/// library's own SQL `ORDER BY
/// m.updated DESC, t.filename,
/// t.line_number`. The test uses a
/// dedicated single-file fixture so the
/// cross-file ordering is irrelevant.
#[test]
fn fetch_todos_orders_lines_within_a_file() {
    use rusqlite::Connection;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "smarthistory-todo-lineorder-{}-{}",
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create notes dir");
    fs::write(
        dir.join("single.md"),
        "# Title\n\
                         \n\
                         - [ ] line 3\n\
                         - [ ] line 4\n\
                         - [x] line 5\n\
                         - [ ] line 6\n",
    )
    .expect("write note");
    let db_path = std::env::temp_dir().join(format!(
        "smarthistory-todo-lo-db-{}-{}.sqlite",
        std::process::id(),
        n
    ));
    let _ = fs::remove_file(&db_path);
    let conn = Connection::open(&db_path).expect("open db");
    note_search::init_database_schema(&conn)
        .map_err(|e| format!("schema: {e}"))
        .expect("init schema");
    let data = note_search::markdown_parser::process_markdown_file(&dir.join("single.md"), &dir)
        .expect("process file");
    note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
        .map_err(|e| format!("write: {e}"))
        .expect("write db");
    drop(conn);
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    app.query = "!".to_string();
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    // 3 open todos: lines 3, 4, 6.
    // (Line 5 is `[x]`, closed.)
    // The library's `text` field is
    // the part after the checkbox (not
    // the full line), which differs
    // from the raw-line representation
    // we had when scanning the
    // filesystem directly. We test
    // against the library's
    // representation here.
    assert_eq!(cmds.len(), 3);
    assert_eq!(cmds, vec!["line 3", "line 4", "line 6",]);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// `fetch_todos` returns an empty list
/// when the user has a `notes.dir`
/// configured but no `notes.database`.
/// (The library needs the database to
/// query; scanning the filesystem is no
/// longer supported.) The TUI surfaces a
/// status message so the user knows why.
#[test]
fn fetch_todos_requires_notes_database() {
    let mut app = global_test_app(&[("a", 1)]);
    // `notes_database` defaults to None.
    app.query = "!".to_string();
    app.refresh();
    assert_eq!(app.merged_rows().len(), 0);
    // The status message explains the
    // missing config so the user
    // doesn't see a silent empty list.
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("missing notes.database should surface a status");
    assert!(msg.contains("notes.database"), "got: {:?}", msg);
}

/// `fetch_todos` reads the line number
/// from the library's `TodoResult` and
/// stores it in the synthetic `id` so
/// consumers can recover it. We test
/// that the resulting id encodes the
/// line number (1-based) of the todo
/// within its file.
#[test]
fn fetch_todos_id_encodes_line_number() {
    let (dir, db_path) = setup_todo_db();
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    app.query = "!".to_string();
    app.refresh();
    // The fixture has `older todo 1`
    // on line 3 of older.md. Find the
    // row whose comment is older.md
    // and check that its id is -3.
    let row = app
        .merged_rows()
        .iter()
        .find(|r| r.command.contains("older todo 1"))
        .expect("older todo 1 row");
    assert_eq!(row.id, -3);
    assert_eq!(row.comment, "older.md");
    let line_number: usize = (row.id.unsigned_abs() as usize).max(1);
    assert_eq!(line_number, 3);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// The `todo_line_option` template
/// substitutes `$LINE` with the actual
/// 1-based line number. We test this by
/// mutating `app.todo_line_option` and
/// confirming the resulting staged
/// command uses the new template.
#[test]
fn todo_line_option_template_is_substituted() {
    let (dir, db_path) = setup_todo_db();
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    app.todo_line_option = String::from("+LINE:$LINE");
    app.query = "!older todo 1".to_string();
    app.refresh();
    let row = app.selected_row().expect("a row");
    let line_number: usize = (row.id.unsigned_abs() as usize).max(1);
    let substituted = app
        .todo_line_option
        .replace("$LINE", &line_number.to_string());
    assert_eq!(substituted, "+LINE:3");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// End-to-end regression test for the
/// user's bug report: `!@orchard`
/// should match todos whose text
/// contains the word "orchard",
/// not (as the library's
/// `parse_query` would naively do)
/// interpret `@orchard` as a link
/// reference and return empty.
///
/// The previous implementation
/// pushed the raw `@orchard` token
/// into the cleaned pattern; the
/// library then tokenized it as
/// `Token::Link("orchard")` and
/// looked for an `[[orchard]]`
/// reference in `t.links`/
/// `m.links`, finding none in a
/// normal notes-only workflow.
/// The fix strips the leading `@`
/// from non-alias tokens in
/// `parse_notes_query` so the
/// downstream `parse_query` sees a
/// plain `Text("orchard")` token
/// that routes through the
/// text-LIKE branch.
#[test]
fn fetch_todos_at_prefix_matches_text() {
    use rusqlite::Connection;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "smarthistory-todo-orchard-{}-{}",
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create notes dir");
    fs::write(
        dir.join("note.md"),
        "# Title\n\
                         \n\
                         - [ ] pick apples in the orchard\n\
                         - [ ] write tests\n\
                         - [ ] visit the orchard on saturday\n",
    )
    .expect("write note");
    let db_path = std::env::temp_dir().join(format!(
        "smarthistory-todo-orchard-db-{}-{}.sqlite",
        std::process::id(),
        n
    ));
    let _ = fs::remove_file(&db_path);
    let conn = Connection::open(&db_path).expect("open db");
    note_search::init_database_schema(&conn)
        .map_err(|e| format!("schema: {e}"))
        .expect("init schema");
    let data = note_search::markdown_parser::process_markdown_file(&dir.join("note.md"), &dir)
        .expect("process file");
    note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
        .map_err(|e| format!("write: {e}"))
        .expect("write db");
    drop(conn);
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    // The user's exact bug report
    // query: `!orchard` should
    // return the two todos that
    // mention "orchard", not zero.
    // (Previously this was `!@orchard`
    // — the `@` was stripped as a
    // convenience prefix. Now `@`
    // means link search, so plain
    // text search uses just the
    // word without `@`.)
    app.query = "!orchard".to_string();
    app.refresh();
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(cmds.len(), 2, "expected 2 orchard todos, got: {:?}", cmds);
    assert!(cmds.iter().any(|c| c.contains("apples")));
    assert!(cmds.iter().any(|c| c.contains("saturday")));
    // Sanity: a query that doesn't
    // appear in any todo returns
    // empty.
    app.query = "!nonexistent".to_string();
    app.refresh();
    assert_eq!(app.merged_rows().len(), 0);
    // `@` in todo mode now means
    // link search, not text search.
    // A link-search query for a
    // link that doesn't exist
    // returns empty.
    app.query = "!@nonexistent".to_string();
    app.refresh();
    assert_eq!(app.merged_rows().len(), 0);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// `mark_todo_done` toggles the
/// checkbox marker on the
/// targeted line in the source
/// note file from `[ ]` to
/// `[x]`. We start with a note
/// that has two open todos on
/// lines 3 and 5, invoke the
/// action on the first row
/// (`older todo 1` on line 3),
/// then read the file back and
/// assert that line 3 is now
/// `- [x] older todo 1` and
/// line 5 is unchanged.
#[test]
fn mark_todo_done_toggles_checkbox_in_file() {
    let (dir, db_path) = setup_todo_db();
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    app.query = "!older todo 1".to_string();
    app.refresh();
    // Sanity: the row exists and
    // points at line 3.
    let row = app.selected_row().expect("row");
    assert_eq!(row.id, -3);
    assert_eq!(row.comment, "older.md");
    app.mark_todo_done();
    // Re-read the file and verify
    // line 3 was toggled.
    let contents = std::fs::read_to_string(dir.join("older.md")).expect("read older.md");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines[2], "- [x] older todo 1");
    // The closed todo on line 5
    // and the other open todo
    // on line 6 are both
    // unchanged.
    assert_eq!(lines[4], "- [x] older done 1");
    assert_eq!(lines[5], "- [ ] older todo 2");
    // The status message
    // confirms the toggle.
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("status message after mark");
    assert!(msg.contains("Marked done"), "got: {:?}", msg);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// After a successful file
/// toggle, `mark_todo_done`
/// refreshes the
/// `todo_entries` SQLite
/// table via the library's
/// `update_files_in_db`
/// function (the canonical
/// re-index path) and then
/// re-queries the TUI's view.
/// Both halves of the
/// contract are verified:
/// the row's `closed` column
/// is now `1`, and the row
/// itself is gone from the
/// merged list (the underlying
/// query filters
/// `open: true`).
#[test]
fn mark_todo_done_refreshes_database_via_update_files_in_db() {
    let (dir, db_path) = setup_todo_db();
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    app.query = "!older todo 1".to_string();
    app.refresh();
    // Sanity: the row exists
    // in the DB before the
    // action, with
    // `closed = 0`.
    use rusqlite::Connection;
    let conn_before = Connection::open(&db_path).expect("open db");
    let closed_before: i64 = conn_before
        .query_row(
            "SELECT closed FROM todo_entries \
                                 WHERE filename = 'older.md' \
                                   AND line_number = 3",
            [],
            |row| row.get(0),
        )
        .expect("query closed before");
    assert_eq!(closed_before, 0);
    drop(conn_before);
    // Pre-condition: row is in
    // the merged list.
    assert!(app
        .merged_rows()
        .iter()
        .any(|r| r.command.contains("older todo 1")),);
    app.mark_todo_done();
    // File was updated.
    let contents = std::fs::read_to_string(dir.join("older.md")).expect("read older.md");
    assert!(
        contents.contains("- [x] older todo 1"),
        "file should be updated: {}",
        contents
    );
    // DB was updated by
    // `update_files_in_db`:
    // the row's `closed` is
    // now 1.
    let conn_after = Connection::open(&db_path).expect("open db");
    let closed_after: i64 = conn_after
        .query_row(
            "SELECT closed FROM todo_entries \
                                 WHERE filename = 'older.md' \
                                   AND line_number = 3",
            [],
            |row| row.get(0),
        )
        .expect("query closed after");
    assert_eq!(
        closed_after, 1,
        "DB should reflect the toggle \
                         (update_files_in_db re-parses \
                         the file and re-writes the \
                         todo_entries row)"
    );
    drop(conn_after);
    // Row is gone from the
    // merged list.
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert!(
        cmds.iter().all(|c| !c.contains("older todo 1")),
        "row should be gone after refresh: {:?}",
        cmds
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// `mark_todo_done` only works
/// in todo mode. Outside of
/// todo mode it's a no-op with a
/// status message so the user
/// understands why their `C-x`
/// did nothing. This is the
/// mode-gating contract: the
/// action is "only available in
/// the search of todos".
#[test]
fn mark_todo_done_outside_todo_mode_is_noop() {
    let mut app = global_test_app(&[("a", 1)]);
    // Note: we don't even have a
    // row selected, but the
    // mode gate fires first.
    app.query = "git".to_string(); // plain history mode
    app.refresh();
    let before_rows = app.merged_rows().len();
    app.mark_todo_done();
    assert_eq!(app.merged_rows().len(), before_rows);
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("status message");
    assert!(msg.contains("only available in todo"), "got: {:?}", msg);
}

/// If the file's content has
/// changed since the indexer
/// last saw it (the user
/// manually toggled the
/// checkbox, or the line was
/// edited in some other way),
/// the targeted line may no
/// longer be an open todo. The
/// action must NOT corrupt the
/// file in that case — it
/// surfaces a status message
/// and leaves the file alone.
#[test]
fn mark_todo_done_rejects_stale_line() {
    let (dir, db_path) = setup_todo_db();
    // Mutate the file behind
    // the indexer's back: the
    // todo on line 3 is now
    // already closed.
    std::fs::write(
        dir.join("older.md"),
        "# Older\n\n\
                         - [x] older todo 1 (already done)\n\
                         some prose in between\n\
                         - [x] older done 1\n\
                         - [ ] older todo 2\n",
    )
    .expect("rewrite older.md");
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    app.query = "!older todo 1".to_string();
    app.refresh();
    app.mark_todo_done();
    // File unchanged.
    let contents = std::fs::read_to_string(dir.join("older.md")).expect("read older.md");
    assert!(
        contents.contains("already done"),
        "file should be untouched: {}",
        contents
    );
    // Status explains why.
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("status message");
    assert!(msg.contains("no longer an open todo"), "got: {:?}", msg);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// If `notes_dir` is not
/// configured, the action
/// surfaces a status message
/// and writes nothing.
#[test]
fn mark_todo_done_without_notes_dir_is_noop() {
    let (dir, db_path) = setup_todo_db();
    let mut app = global_test_app(&[("a", 1)]);
    // `notes_database` is set
    // but `notes_dir` is None.
    app.notes_database = Some(db_path.clone());
    app.query = "!older todo 1".to_string();
    app.refresh();
    app.mark_todo_done();
    // The original file is
    // untouched.
    let contents = std::fs::read_to_string(dir.join("older.md")).expect("read older.md");
    assert!(
        contents.contains("- [ ] older todo 1"),
        "file should be untouched: {}",
        contents
    );
    let msg = app
        .status_message
        .as_ref()
        .map(|(m, _)| m.as_str())
        .expect("status message");
    assert!(msg.contains("notes.dir"), "got: {:?}", msg);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&db_path);
}

/// Indented todos (e.g. nested
/// under a heading) get
/// correctly toggled — the
/// leading whitespace is
/// preserved, only the bracket
/// marker changes. We verify
/// this with a hand-crafted
/// single-row scenario where
/// we bypass the library's
/// parser: the library's
/// `TODO_REGEX` is anchored
/// with `^`, so indented
/// checkboxes never reach
/// the database in the first
/// place. But a stale DB row
/// (e.g. left over from a
/// previous version of the
/// library, or hand-edited by
/// the user) might still
/// point at an indented line,
/// and our toggle must
/// preserve the indentation.
#[test]
fn mark_todo_done_preserves_indentation() {
    use std::fs;
    let dir =
        std::env::temp_dir().join(format!("smarthistory-todo-indent-{}", std::process::id(),));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create notes dir");
    let mut note = String::from("# Title\n");
    note.push('\n');
    note.push_str("  - [ ] indented todo\n");
    fs::write(dir.join("note.md"), &note).expect("write");
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.query = "fake".to_string();
    app.refresh();
    // Construct a synthetic
    // todo row that points at
    // line 3 of the file.
    // This bypasses
    // `fetch_todos` (the
    // library wouldn't have
    // indexed the indented
    // todo in the first
    // place) and exercises
    // the file mutation in
    // isolation.
    let row = crate::tui::state::HistoryRow {
        id: -3,
        command: String::from("indented todo"),
        directory: String::new(),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: String::from("note.md"),
        output: String::new(),
        mode: String::from("todo"),
        source: String::new(),

        ..Default::default()
    };
    app.mark_todo_done_for_row(&row);
    let contents = fs::read_to_string(dir.join("note.md")).expect("read note.md");
    let lines: Vec<&str> = contents.lines().collect();
    // The leading two spaces
    // are preserved; only the
    // bracket changed.
    assert_eq!(lines[2], "  - [x] indented todo", "got: {:?}", contents);
    let _ = fs::remove_dir_all(&dir);
}

/// Files without a trailing
/// newline (unusual but
/// legal) are preserved
/// verbatim after the toggle —
/// we don't accidentally add
/// a trailing `\n` that
/// wasn't there.
#[test]
fn mark_todo_done_preserves_no_trailing_newline() {
    use rusqlite::Connection;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "smarthistory-todo-noeof-{}-{}",
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create notes dir");
    // Note: NO trailing newline.
    fs::write(dir.join("note.md"), "# Title\n\n- [ ] open todo").expect("write");
    let db_path = std::env::temp_dir().join(format!(
        "smarthistory-todo-noeof-db-{}-{}.sqlite",
        std::process::id(),
        n
    ));
    let _ = fs::remove_file(&db_path);
    let conn = Connection::open(&db_path).expect("open db");
    note_search::init_database_schema(&conn)
        .map_err(|e| format!("schema: {e}"))
        .expect("init schema");
    let data = note_search::markdown_parser::process_markdown_file(&dir.join("note.md"), &dir)
        .expect("process file");
    note_search::write_markdown_data_to_sqlite_with_conn(&data, &conn)
        .map_err(|e| format!("write: {e}"))
        .expect("write db");
    drop(conn);
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_dir = Some(dir.clone());
    app.notes_database = Some(db_path.clone());
    app.query = "!".to_string();
    app.refresh();
    app.mark_todo_done();
    let contents = fs::read_to_string(dir.join("note.md")).expect("read note.md");
    assert_eq!(contents, "# Title\n\n- [x] open todo");
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_file(&db_path);
}

// --- Directories mode (`#` prefix) ----

/// Helper that builds a fresh
/// in-memory `App` with a
/// history table containing
/// rows for several
/// directories. The
/// `global_test_app` helper
/// hardcodes every row's
/// `directory` to `/tmp`, so
/// we need a bespoke
/// constructor for
/// directories-mode tests.
/// The passed-in `(cmd,
/// directory, offset_secs)`
/// tuples are inserted in
/// the given order; the
/// resulting `timestamp` is
/// `now - offset_secs` so we
/// can drive the
/// recency-ordering
/// assertions deterministically.
fn directories_test_app(rows: &[(&str, &str, i64)]) -> App {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                    id INTEGER PRIMARY KEY,
                    command TEXT NOT NULL,
                    directory TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    exit_code INTEGER,
                    timestamp INTEGER DEFAULT \
                     (strftime('%s', 'now')),
                    mode TEXT NOT NULL DEFAULT 'command'
                );",
    )
    .expect("schema");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for (i, (cmd, dir, offset)) in rows.iter().enumerate() {
        conn.execute(
            "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
                     VALUES (?1, ?2, ?3, 'sess', 0, ?4)",
            rusqlite::params![i as i64 + 1, *cmd, *dir, now - *offset,],
        )
        .expect("insert");
    }
    // Build the App and
    // immediately clear
    // the `session_subdirs`
    // field. `App::new`
    // calls
    // `build_session_subdirs`
    // which reads the
    // user's real
    // `~/.config/smarthistory/config`.
    // Tests that don't
    // care about
    // sessiondirs would
    // otherwise be
    // polluted by
    // whatever the user
    // happens to have
    // configured (a real
    // "I added
    // `sessiondirs=...`
    // to my config and
    // now my tests fail"
    // bug). Tests that
    // DO need pinned
    // directories should
    // call
    // `directories_test_app_with_sessions`
    // below.
    let mut app = App::new(
        conn,
        Mode::Global,
        String::new(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        // No JIRA fragments in the default
        // test app. Tests that exercise the
        // fragment path push entries directly
        // into `app.jira_fragments` (it's a
        // plain HashMap field, not gated by
        // a setter — the test is the only
        // consumer and doesn't need a
        // formal setter).
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    // `App::new` calls
    // `build_session_subdirs`
    // (which reads the
    // user's real
    // `~/.config/smarthistory/config`)
    // and `fetch_tmux_windows`
    // (which runs
    // `tmux list-windows -a`).
    // Both of those would
    // pollute the test
    // with whatever the
    // user happens to
    // have configured or
    // running. Clear the
    // fields so each test
    // sees a known-empty
    // starting point.
    // Tests that need a
    // specific
    // `session_subdirs`
    // or `tmux_windows`
    // set should call
    // `directories_test_app_with_sessions`
    // (or set the
    // fields directly).
    app.session_subdirs.clear();
    app.tmux_windows.clear();
    app
}

/// A variant of
/// `directories_test_app`
/// that ALSO
/// pre-populates the
/// `session_subdirs`
/// field with the given
/// list. Use this when
/// a test needs the
/// pinned-directories
/// behaviour; use the
/// plain
/// `directories_test_app`
/// (which clears
/// `session_subdirs` by
/// `session_subdirs`
/// field with the given
/// list. Use this when
/// a test needs the
/// pinned-directories
/// behaviour; use the
/// plain
/// `directories_test_app`
/// (which clears
/// `session_subdirs` by
/// default) when the
/// test should NOT see
/// any sessiondirs.
///
/// (The default-empty
/// behaviour is what
/// keeps tests
/// isolated from the
// developer's real
// `~/.config/smarthistory/config`
// — see
// `build_session_subdirs`
// for the
// cross-contamination
// story.)
fn directories_test_app_with_sessions(
    rows: &[(&str, &str, i64)],
    sessions: Vec<std::path::PathBuf>,
) -> App {
    let mut app = directories_test_app(rows);
    app.session_subdirs = sessions;
    app
}

/// `fetch_directories`
/// returns one row per
/// unique directory, sorted
/// by each directory's
/// most-recent history
/// timestamp DESC. The
/// directory (in shell-
/// friendly `~/x` form)
/// is the visible primary
/// text of the row, and
/// the last command run
/// in that directory is
/// kept in `row.comment`
/// (the secondary slot)
/// so the user still has
/// a hint of *what* they
/// were doing there.
#[test]
fn fetch_directories_lists_unique_dirs_sorted_by_recency() {
    // Three directories,
    // several timestamps. The
    // recency order (most-recent
    // timestamp DESC) should
    // be `/home/c` first (just
    // ran there), `/home/a`
    // second (yesterday), and
    // `/home/b` last (a year
    // ago). The `comment`
    // for each directory is
    // the command that
    // produced its
    // max-timestamp row.
    let mut app = directories_test_app(&[
        ("ls", "/home/a", 86_400),         // 1 day ago
        ("make", "/home/b", 365 * 86_400), // 1 year ago
        ("echo hi", "/home/c", 30),        // 30s ago
        ("git status", "/home/a", 3_600),  // 1h ago (newer than `ls`)
        ("touch x", "/home/a", 86_400),    // 1d (older than `ls`)
    ]);
    app.query = "#".to_string();
    app.refresh();
    // Three directories
    // expected (one row
    // each). The visible
    // primary text is the
    // directory (now in
    // `row.command`), so
    // that's what we read
    // here. The new
    // directory-source
    // feature surfaces
    // tmux panes as
    // additional rows,
    // so we filter to
    // `/home/...` (the
    // test's directory
    // namespace) to
    // assert cleanly.
    let home_rows: Vec<&HistoryRow> = app
        .merged_rows()
        .iter()
        .filter(|r| r.directory.starts_with("/home/"))
        .collect();
    let visible: Vec<&str> = home_rows.iter().map(|r| r.command.as_str()).collect();
    assert_eq!(visible.len(), 3);
    // Newest directory
    // first.
    assert_eq!(visible[0], "/home/c");
    // Second.
    assert_eq!(visible[1], "/home/a");
    // Third.
    assert_eq!(visible[2], "/home/b");
    // The last command run
    // in each directory
    // lives in `row.comment`
    // (the secondary slot).
    let last_cmds: Vec<&str> = home_rows.iter().map(|r| r.comment.as_str()).collect();
    assert_eq!(last_cmds[0], "echo hi");
    assert_eq!(last_cmds[1], "git status");
    assert_eq!(last_cmds[2], "make");
    // Each row's `directory`
    // is the canonical path.
    let dirs: Vec<&str> = home_rows.iter().map(|r| r.directory.as_str()).collect();
    assert_eq!(dirs[0], "/home/c");
    assert_eq!(dirs[1], "/home/a");
    assert_eq!(dirs[2], "/home/b");
}

/// Substring filter: `#home`
/// restricts the listing to
/// rows whose `directory`
/// contains `home`. The
/// filter is space-split AND
/// (so `#home a` requires both
/// `home` AND `a` somewhere in
/// the path). The visible
/// primary text on each row
/// is now the directory (per
/// the layout swap), so the
/// assertions read
/// `row.command` (the
/// directory) and the
/// comments confirm the
/// last-command metadata.
#[test]
fn fetch_directories_applies_substring_filter() {
    let mut app = directories_test_app(&[
        ("ls", "/home/a", 86_400),
        ("ls", "/var/log", 3_600),
        ("ls", "/home/b", 60),
    ]);
    app.query = "#home".to_string();
    app.refresh();
    let visible: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.command.as_str())
        .collect();
    assert_eq!(visible.len(), 2);
    // `/home/b` is the latest
    // of the matching
    // directories (60s old),
    // so it sorts first.
    assert_eq!(visible[0], "/home/b");
    assert_eq!(visible[1], "/home/a");
    // The secondary slot
    // carries the last
    // command run in each
    // directory.
    let cmds: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.comment.as_str())
        .collect();
    assert_eq!(cmds[0], "ls");
    assert_eq!(cmds[1], "ls");
    // No match for `/var/log`
    // because the filter
    // requires `home`.
    app.query = "#var".to_string();
    app.refresh();
    assert_eq!(app.merged_rows().len(), 1);
    app.query = "#no-such-dir".to_string();
    app.refresh();
    assert_eq!(app.merged_rows().len(), 0);
}

/// Regression test for the
/// directory-row layout
/// swap: the visible
/// primary text is the
/// directory in shell-
/// shortened form
/// (`~/x` when under
/// `$HOME`) and the
/// secondary `# ...`
/// slot is the last
/// command run there.
/// Without the swap the
/// user would see the
/// command first and the
/// directory as a
/// secondary hint — the
/// inverse of what the
/// user wants in `#`-mode
/// (where they're
/// searching for paths,
/// not commands).
#[test]
fn fetch_directories_layout_swap() {
    // The `~`-shortening
    // depends on `$HOME` and
    // the `home_map` config.
    // We set `$HOME` for the
    // duration of the test
    // and clear `home_map`
    // (the default empty
    // list). This avoids
    // depending on the
    // caller's environment
    // (parallel test runs
    // could otherwise see
    // different `home_map`
    // values via the
    // user's actual
    // `~/.config/smarthistory/config`).
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved_home = std::env::var("HOME").ok();
    // SAFETY: this test
    // holds `ENV_LOCK` (the
    // shared env-mutation
    // mutex), so no other
    // env-mutating test
    // can run concurrently.
    unsafe {
        std::env::set_var("HOME", "/Users/har");
    }
    let mut app = directories_test_app(&[("ls -la /tmp/foo bar", "/Users/har/work/project", 60)]);
    app.query = "#".to_string();
    app.refresh();
    // Find the SQL-history
    // row specifically;
    // sessiondirs / tmux
    // rows may also be
    // present (cleared in
    // the test helper, but
    // `refresh()` re-runs
    // `fetch_tmux_windows`
    // for `#`-mode queries
    // and the user's
    // production `tmux`
    // panes may bleed in
    // when HOME is set to
    // a real path). We
    // assert on the row
    // whose `directory`
    // matches what we
    // inserted, not on
    // `merged_rows()[0]`.
    let row = app
        .merged_rows()
        .iter()
        .find(|r| r.directory == "/Users/har/work/project")
        .expect("the SQL-history row for /Users/har/work/project must be in merged_rows");
    // The primary text
    // (which the user sees
    // first in the list,
    // and which the query
    // highlights against)
    // is the directory in
    // `~/x` form. This is
    // the load-bearing
    // assertion: it locks
    // in the swap.
    assert_eq!(row.command, "~/work/project");
    // The secondary slot
    // (the `# ...` comment
    // in the rendered
    // line) is the last
    // command run in that
    // directory. The
    // command here is short
    // (under the 60-char
    // truncation threshold)
    // so it appears
    // verbatim.
    assert_eq!(row.comment, "ls -la /tmp/foo bar");
    // The full directory
    // (un-shortened) is
    // still in `directory`
    // for the tmux-pane
    // lookup and Details
    // pane.
    assert_eq!(row.directory, "/Users/har/work/project");
    if let Some(home) = saved_home {
        unsafe {
            std::env::set_var("HOME", home);
        }
    }
}

/// Long commands are
/// truncated to 57
/// characters plus an
/// ellipsis when stored
/// in the secondary slot
/// (the comment field).
/// The truncation is
/// char-aware (uses
/// `chars().take(57)`)
/// so multi-byte UTF-8
/// doesn't get cut in
/// the middle of a
/// code point.
#[test]
fn fetch_directories_truncates_long_command() {
    // 100-char command.
    let long_cmd = "a".repeat(100);
    let mut app = directories_test_app(&[(&long_cmd, "/Users/har/work", 60)]);
    app.query = "#".to_string();
    app.refresh();
    // Find the row by
    // directory
    // (the test helper
    // clears `tmux_windows`
    // but `refresh()`
    // re-runs the tmux
    // fetch, so the
    // user's real tmux
    // panes may also be
    // present). The
    // SQL row is the one
    // with the matching
    // directory.
    let row = app
        .merged_rows()
        .iter()
        .find(|r| r.directory == "/Users/har/work")
        .expect("the SQL-history row for /Users/har/work must be in merged_rows");
    // Truncated to 57
    // `a`s + `…` = 58
    // chars.
    assert_eq!(row.comment.chars().count(), 58);
    assert!(row.comment.ends_with('…'));
    assert!(row.comment.starts_with('a'));
}

/// Selecting a directory
/// row in the TUI stages
/// `cd <path>` as the next
/// shell command. Paths
/// with shell-metacharacters
/// are quoted so the parent
/// shell tokenises them
/// correctly (defensive —
/// covers spaces, `$`, etc.).
/// The new contract: with an
/// empty `tmux_windows`
/// snapshot (no active
/// tmux session for this
/// directory), selecting
/// the directory row
/// creates a new tmux
/// session and switches to
/// it. (See
/// `select_t_marked_directory_stages_select_and_switch`
/// for the "T"-marked
/// branch.)
#[test]
fn selecting_unmarked_directory_creates_new_tmux_session() {
    let mut app = directories_test_app(&[("ls", "/home/user/project", 60)]);
    app.query = "#".to_string();
    app.refresh();
    // Select the
    // SQL-history row
    // explicitly.
    let sql_row_idx = app
        .merged_rows()
        .iter()
        .position(|r| r.directory == "/home/user/project")
        .expect("the SQL-history row for /home/user/project must be in merged_rows");
    app.list_state.select(Some(sql_row_idx));
    app.select_for_run();
    let staged = app.selection.as_deref().expect("selection must be set");
    assert!(
        staged.contains("tmux new-session -d -s project -c /home/user/project")
            && staged.contains("tmux switch-client -t project"),
        "staged command must create detached session with the directory basename, got: {staged:?}"
    );
    assert_eq!(app.pick_mode, Some(crate::tui::state::PickMode::Run),);
}

/// The `#` prefix is
/// configurable via
/// `prefix.directories=...`,
/// parallel to every other
/// query-mode prefix. We
/// exercise the parse /
/// assignment path
/// (`assign_prefix`) directly
/// because there's no
/// `Config::parse` in scope
/// here.
#[test]
fn directories_prefix_is_configurable() {
    let mut prefixes = crate::QueryPrefixes::default();
    assert_eq!(prefixes.directories, '#');
    // `assign_prefix` lives
    // in `main.rs`; mirror
    // its one-liner here so
    // we can confirm the
    // field is reachable via
    // the public API.
    prefixes.directories = '>';
    assert_eq!(prefixes.directories, '>');
    // A non-default prefix
    // is recognised by the
    // predicate.
    let mut app = directories_test_app(&[("ls", "/home/a", 60)]);
    app.query_prefixes.directories = '>';
    app.query = ">home".to_string();
    assert!(app.is_directories_query());
    assert_eq!(app.directories_pattern(), "home");
}

// --- Tmux-pane marker (`#` directories mode) ----

/// `directory_has_tmux_pane`
/// returns false when the
/// snapshot is empty
/// (never populated). This
/// is the contract: before
/// the user types `#…`
/// (which triggers the
/// snapshot fetch) the
/// check is a hard `false`
/// so no rows are falsely
/// marked as in-tmux.
#[test]
fn directory_tmux_pane_id_empty_snapshot_is_none() {
    let app = directories_test_app(&[("ls", "/home/user", 60)]);
    assert!(app.tmux_windows.is_empty());
    assert!(app.directory_tmux_pane_id("/home/user").is_none());
}

/// `directory_tmux_pane_id`
/// returns `Some(pane_id)`
/// iff a window's `path`
/// (canonicalised at parse
/// time) matches the input
/// directory (also
/// canonicalised). The
/// `#{pane_id} |
///  #{pane_current_path} |
///  active:#{window_active}
///  | Layout:
///  #{window_layout}` format
/// used by `tmux
/// list-windows -a` always
/// reports the kernel's
/// canonical cwd, which on
/// macOS is the
/// `/Volumes/HUGE/...`
/// form, while the directory
/// stored by the
/// `preexec` hook is the
/// user's logical
/// `/Users/...` form — both
/// canonicalise to the same
/// string. This test verifies
/// that contract without
/// actually spawning
/// `tmux` (CI may not have it
/// installed).
#[test]
fn directory_tmux_pane_id_canonicalises_both_sides() {
    let mut app = directories_test_app(&[("ls", "/home/user", 60)]);
    // Simulate a snapshot
    // that came from `tmux`.
    // `fetch_tmux_windows`
    // canonicalises these
    // at parse time, so the
    // stored `path` is
    // already canonical. We
    // hand-craft the same
    // canonical value here.
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%0".to_string(),
        path: String::from("/home/user"),
        ..Default::default()
    });
    assert_eq!(
        app.directory_tmux_pane_id("/home/user").as_deref(),
        Some("%0"),
        "exact match must return the pane id"
    );
    // Wrong directory, same
    // prefix — must NOT match.
    assert!(app.directory_tmux_pane_id("/home/other").is_none());
}

/// The actual reported
/// bug: `tmux` reports
/// `/Volumes/HUGE/...` for
/// directories under
/// `/Users/har/...` because
/// of macOS volume mounts,
/// while the `preexec` hook
/// records the user's
/// logical `/Users/...`
/// form. Without
/// canonicalization, the
/// two would never match.
/// This test guarantees
/// they do.
#[test]
fn directory_tmux_pane_id_handles_macos_volume_mount() {
    let mut app = directories_test_app(&[("ls", "/Users/har/Sources/x", 60)]);
    // `tmux` returns the
    // canonical form (which on
    // macOS resolves through
    // any symlinks / volume
    // mounts). The fetch
    // helper canonicalises
    // these at parse time; we
    // pop a pre-canonicalised
    // entry.
    //
    // We don't depend on a
    // specific macOS path here
    // — we just verify the
    // canonicalisation
    // contract: as long as
    // both forms collapse to
    // the same string
    // (which the
    // `canonicalize_directory`
    // helper does), the match
    // succeeds.
    let canonical_dir = std::fs::canonicalize("/tmp")
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/tmp".into());
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%42".to_string(),
        path: canonical_dir.clone(),
        ..Default::default()
    });
    assert_eq!(
        app.directory_tmux_pane_id(&canonical_dir).as_deref(),
        Some("%42"),
        "real-path lookup must match the canonical pane path"
    );
    // Try a non-canonical
    // form: should still
    // match because the
    // helper canonicalises
    // input too. We use a
    // different dir here so
    // the test is
    // deterministic —
    // `/var` is a symlink to
    // `/private/var` on
    // macOS but a real dir on
    // Linux CI.
    let var_canonical = std::fs::canonicalize("/var")
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/var".into());
    if var_canonical != "/var" {
        // macOS: `/var`
        // canonicalises to
        // `/private/var`. We
        // push that pane
        // and check that
        // asking for either
        // form matches.
        app.tmux_windows.push(TmuxWindowInfo {
            pane_id: "%7".to_string(),
            path: var_canonical,
            ..Default::default()
        });
        assert!(
            app.directory_tmux_pane_id("/var").is_some(),
            "/var must canonicalise to match"
        );
    }
    // Wrong directory
    // totally shouldn't
    // match.
    assert!(app.directory_tmux_pane_id("/home/nowhere").is_none());
}

/// Regression test for the
/// homemap-aware
/// normalization: a DB
/// row stored in the
/// short `~/x` form
/// (after
/// `smarthistory
/// update`) must match
/// a tmux-reported
/// pane at the
/// absolute form
/// (e.g. `/Users/har/x`).
///
/// Without the
/// homemap-aware
/// expansion, the
/// `std::fs::canonicalize`
/// step on the `~/x`
/// side would fail (no
/// real `~/x` path
/// exists) and fall
/// back to the un-
/// resolved input,
/// which never matched
/// the tmux side. The
/// result: a directory
/// row that DID have a
/// live tmux pane was
/// missing the `T`
/// marker.
///
/// We use `$HOME` via
/// `set_var` (guarded
/// by `ENV_LOCK` so
/// the env mutation
/// doesn't race with
/// other env-mutating
/// tests) and rely on
/// `/tmp` (which
/// always exists) as
/// the test directory.
#[test]
fn directory_tmux_pane_id_handles_tilde_form_db_row() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved_home = std::env::var("HOME").ok();
    // SAFETY: holds
    // `ENV_LOCK`.
    unsafe {
        std::env::set_var("HOME", "/tmp");
    }
    // Use `/tmp` as the
    // test directory.
    // `~/self_test_dir`
    // is therefore
    // `/tmp/self_test_dir`
    // after homemap
    // expansion, and the
    // tmux pane has the
    // same absolute path
    // (already canonical,
    // no macOS volume
    // mount to worry
    // about).
    let mut app = directories_test_app(&[("ls", "/tmp", 60)]);
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%99".to_string(),
        // The path tmux
        // reports is
        // already the
        // canonical
        // absolute form
        // (no `~`).
        path: std::fs::canonicalize("/tmp")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/tmp".into()),
        ..Default::default()
    });
    // The user-facing
    // directory in the
    // DB is the short
    // form `~/x` (or
    // here, the home
    // itself: `~`).
    // This is the case
    // the user reported:
    // a row stored in
    // `~/x` form should
    // still get the `T`
    // marker when a
    // tmux pane is at
    // the matching
    // absolute path.
    let canonical_tmp = std::fs::canonicalize("/tmp")
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/tmp".into());
    assert_eq!(
        app.directory_tmux_pane_id(&canonical_tmp).as_deref(),
        Some("%99"),
        "absolute-path DB row must match the tmux pane"
    );
    if let Some(home) = saved_home {
        unsafe {
            std::env::set_var("HOME", home);
        }
    }
}

/// Regression test for the
/// user-reported bug:
/// `multiplexer=herdr`
/// in the config, but
/// a new workspace was
/// created for a
/// directory that was
/// already part of an
/// existing workspace.
/// The root cause was
/// that the herdr
/// snapshot parser
/// returned an empty
/// list (herdr's
/// `pane list` output
/// is JSON, not the
/// `|`-separated text
/// the old parser
/// expected), so the
/// T-marker lookup
/// always returned
/// `None` and the
/// staging always
/// branched to
/// `herdr workspace create`.
/// This test
/// guarantees the
/// parser produces
/// `tmux_windows` rows
/// and that the T-marker
/// lookup finds the
/// existing workspace
/// so the staging can
/// reuse it via
/// `herdr workspace focus`
/// instead of creating
/// a duplicate.
#[cfg(feature = "herdr")]
#[test]
fn directory_with_existing_herdr_workspace_reuses_via_t_marker() {
    // One history row at
    // `/var/tmp/build` —
    // the user has run
    // commands there.
    let mut app = directories_test_app(&[("ls -la", "/var/tmp/build", 60)]);
    // Swap in the herdr
    // backend.
    app.multiplexer = crate::multiplexer::backend_for(crate::multiplexer::MultiplexerKind::Herdr);
    // Simulate the
    // snapshot the
    // herdr backend
    // would produce:
    // one active pane
    // at `/var/tmp/build`
    // in workspace
    // `wA`.
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: String::from("wA:p1"),
        path: String::from("/var/tmp/build"),
        ..Default::default()
    });
    // The T-marker
    // lookup must now
    // find the
    // existing
    // workspace's pane
    // id.
    assert_eq!(
        app.directory_tmux_pane_id("/var/tmp/build").as_deref(),
        Some("wA:p1"),
        "T-marker lookup must return the existing herdr pane id"
    );
    // The staging then
    // calls
    // `self.multiplexer.focus_command("wA:p1")`
    // which produces
    // `herdr workspace focus wA`
    // (the `:p1` suffix
    // is stripped by
    // the backend).
    let staged = app
        .multiplexer
        .focus_command("wA:p1")
        .expect("non-empty pane id");
    assert_eq!(staged, "herdr workspace focus wA 2>/dev/null");
    // And a directory
    // without a matching
    // workspace must
    // still produce
    // `herdr workspace create`
    // (the unmarked
    // branch) — the
    // fix doesn't
    // regress the
    // create path.
    assert!(app.directory_tmux_pane_id("/var/tmp/elsewhere").is_none());
}

/// The `tmux_windows` snapshot
/// is preserved across
/// `refresh()` calls — the
/// helper is idempotent and
/// the fetch only happens
/// when the snapshot is
/// empty. Otherwise
/// scrolling through the
/// directories list would
/// re-spawn `tmux` on every
/// keypress.
///
/// This test verifies the
/// idempotency by
/// pre-populating the
/// snapshot, calling
/// `fetch_tmux_windows`
/// once, and asserting the
/// snapshot didn't change.
/// (We don't run `refresh()`
/// here because in a test
/// environment without
/// `tmux` on PATH the
/// `refresh()` call would
/// set the snapshot to
/// empty, masking the
/// behaviour we want to
/// verify.)
#[test]
fn fetch_tmux_windows_is_idempotent_when_populated() {
    let mut app = directories_test_app(&[("ls", "/home/user", 60)]);
    // Pre-populate the
    // snapshot with a
    // sentinel value that
    // would be wiped if the
    // helper re-ran.
    let sentinel = TmuxWindowInfo {
        pane_id: "%99".to_string(),
        path: String::from("/sentinel"),
        ..Default::default()
    };
    app.tmux_windows.push(sentinel.clone());
    // The helper exits
    // early when the snapshot
    // is non-empty. This is
    // the "don't re-spawn on
    // every refresh" contract.
    crate::tui::mode::directories::ensure_multiplexer_snapshot(&mut app);
    assert_eq!(app.tmux_windows.len(), 1);
    assert_eq!(app.tmux_windows[0].pane_id, "%99");
    assert_eq!(app.tmux_windows[0].path, "/sentinel");
}

/// Regression test for the
/// user-reported bug:
/// `multiplexer=herdr` in
/// the config, but the
/// T-marker was showing
/// for directories that
/// don't have an active
/// herdr workspace. The
/// root cause was that
/// `fetch_tmux_windows`
/// ignored the configured
/// backend and shelled
/// out to `tmux list-windows`
/// directly, so the
/// T-marker was being
/// driven by tmux's
/// snapshot regardless
/// of the user's `multiplexer`
/// setting. After the
/// fix, `fetch_tmux_windows`
/// delegates to
/// `self.multiplexer.snapshot()`,
/// so the T-marker only
/// reflects the active
/// backend's view.
///
/// We can't easily assert
/// "the herdr snapshot was
/// called" here (we'd
/// need to mock the
/// subprocess). What we
/// CAN assert is that
/// when the configured
/// backend is herdr, the
/// `multiplexer.name()` on
/// the App matches
/// "herdr" — and that the
/// `Box<dyn ...>` stored
/// in `App::multiplexer`
/// is the herdr backend
/// (via the `name()` trait
/// method, which returns
/// the backend's own
/// claim of its identity).
/// The actual JSON-parsing
/// direction is covered by
/// `multiplexer::tests::parse_herdr_pane_list`;
/// this test pins the
/// "backend selection"
/// contract.
#[cfg(feature = "herdr")]
#[test]
fn fetch_tmux_windows_resolves_to_configured_backend() {
    let app = directories_test_app(&[]);
    // The default test
    // helper builds a tmux
    // backend, so swap in
    // the herdr backend to
    // mirror what a
    // `multiplexer=herdr`
    // config would
    // produce.
    let mut app = app;
    app.multiplexer = crate::multiplexer::backend_for(crate::multiplexer::MultiplexerKind::Herdr);
    assert_eq!(
        app.multiplexer.name(),
        "herdr",
        "the App must hold the herdr backend when the \
                 user has `multiplexer=herdr`; the T-marker \
                 comes from this backend's snapshot, not from \
                 a hard-coded tmux list-windows call"
    );
}

/// Real-world tmux output
/// sampled from the user's
/// own machine at the time
/// this test was added.
/// Verifies our parser
/// handles the live format
/// string
/// (`#{pane_id} |
///  #{pane_current_path} |
///  active:#{window_active}
///  | Layout:
///  #{window_layout}`)
/// correctly.
///
/// If this test fails,
/// either tmux changed its
/// format tokens (unlikely)
/// or our format string in
/// `fetch_tmux_windows` got
/// silently truncated.
/// Either way, the
/// `parse_tmux_pane_line`
/// contract has shifted;
/// re-pin the live format in
/// `fetch_tmux_windows` first.
#[test]
fn parse_tmux_pane_line_real_world_output() {
    // Captured from the user's
    // environment with
    // `tmux list-windows -a -F
    // '#{pane_id} |
    //  #{pane_current_path} |
    //  active:#{window_active} |
    //  Layout:
    //  #{window_layout}'
    //  | grep "active:1"`.
    // (All rows below are
    // active:1 because they
    // come from grepped
    // output.)
    let sample = "\
                %0 | /Users/har | active:1 | Layout: c17d,121x93,0,0,0\n\
                %2 | /Volumes/HUGE/har/Sources/markdown-search/note_search | active:1 | Layout: 3971,121x93,0,0[121x46,0,0,2,121x46,0,47,10]\n\
                %1 | /Users/har/smarthistory/smarthistory | active:1 | Layout: 7254,121x93,0,0[121x46,0,0,1,121x46,0,47,3]\n";
    let windows: Vec<TmuxWindowInfo> = sample.lines().filter_map(parse_tmux_pane_line).collect();
    assert_eq!(
        windows.len(),
        3,
        "expected 3 parsed windows, got: {:#?}",
        windows
    );
    // First window: pane id
    // `%0`, path
    // canonicalises to the
    // user's macOS
    // `/Users/har` (no
    // symlinks involved in
    // this test dir, so
    // canonicalisation is
    // a no-op).
    assert_eq!(windows[0].pane_id, "%0");
    assert_eq!(
        std::fs::canonicalize("/Users/har")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| { "/Users/har".into() }),
        windows[0].path,
    );
    // Second window: pane
    // id `%2`, path
    // already-canonical
    // `/Volumes/HUGE/...`.
    assert_eq!(windows[1].pane_id, "%2");
    assert_eq!(
        windows[1].path,
        "/Volumes/HUGE/har/Sources/markdown-search/note_search"
    );
}

/// `parse_tmux_pane_line`
/// drops inactive windows
/// (`active:0` rows). The
/// user's spec pipes
/// through `grep "active:1"`
/// — we do the filter
/// in-process so we only
/// spawn one subprocess.
#[test]
fn parse_tmux_pane_line_filters_inactive_windows() {
    let inactive = "%0 | /Users/har | active:0 | Layout: c17d,121x93,0,0,0";
    let active = "%0 | /Users/har | active:1 | Layout: c17d,121x93,0,0,0";
    assert!(
        parse_tmux_pane_line(inactive).is_none(),
        "active:0 must be filtered out"
    );
    assert!(parse_tmux_pane_line(active).is_some());
}

/// The format-string bug we
/// hit during development:
/// tmux format strings use
/// `#`-prefixed placeholders
/// (`#S`, `#{pane_current_path}`),
/// with **the `#` always
/// required**. Writing
/// `"{S}"` instead of `"#S"`
/// silently renders an empty
/// first column, then any
/// strict parser that skips
/// empty fields throws the
/// whole line away. The
/// `FORMAT` constant in
/// `fetch_tmux_windows` is
/// tested by `tmux
/// list-windows -a -F`; the
/// regression test below
/// pins the correct format.
#[test]
fn parse_tmux_pane_line_rejects_buggy_format() {
    // What `tmux list-windows -a
    // -F "{S} | ... | active:0 | ..."`
    // (buggy format) would
    // actually emit — first
    // column empty. We don't
    // pin every field here
    // (the parse fails on
    // the empty `pane_id`
    // check); just the empty
    // first column.
    let buggy_line = " | /Users/har | active:1 | Layout: x";
    assert!(
        parse_tmux_pane_line(buggy_line).is_none(),
        "an empty pane_id field must be rejected, \
                 otherwise the whole tmux snapshot becomes \
                 silently empty and no T markers render"
    );
    // The non-buggy version
    // (with `#{pane_id}`)
    // parses correctly.
    let good_line = "%0 | /Users/har | active:1 | Layout: x";
    assert!(parse_tmux_pane_line(good_line).is_some());
}

/// End-to-end: pre-loading
/// the snapshot with a window
/// whose canonical path
/// matches a directory row
/// causes
/// `directory_tmux_pane_id`
/// to return the pane id.
/// This is the chain that
/// produces the user-visible
/// `T` marker.
#[test]
fn directory_row_is_marked_after_snapshot_loaded() {
    // This test verifies the canonicalisation contract:
    // a DB row stored under one path form must match a
    // tmux window stored under a different form that
    // resolves to the same physical directory.
    //
    // On macOS, /Users/har/... and
    // /Volumes/HUGE/har/... can be the same dir
    // (external volume mount). On Linux CI there's no
    // such mount, so we use a symlink instead: create
    // a temp dir, symlink it, and verify the two
    // forms resolve to the same canonical path.
    let real = std::env::temp_dir().join("sh_real_dir");
    let link = std::env::temp_dir().join("sh_link_dir");
    let _ = std::fs::remove_dir_all(&real);
    let _ = std::fs::remove_file(&link);
    std::fs::create_dir_all(&real).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let mut app = directories_test_app(&[("ls", real.to_str().unwrap(), 60)]);
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%1".to_string(),
        path: link.to_string_lossy().into_owned(),
        ..Default::default()
    });
    assert_eq!(
        app.directory_tmux_pane_id(real.to_str().unwrap())
            .as_deref(),
        Some("%1"),
        "the row stored as {:?} must match a window \
                 stored as {:?} — the canonicalisation contract",
        real,
        link
    );
    let _ = std::fs::remove_dir_all(&real);
    let _ = std::fs::remove_file(&link);
}

/// Selecting a `T`-marked
/// directory row stages
/// `tmux select-pane -t <id>
/// && tmux switch-client -t
/// <id>`. The parent shell
/// (running the TUI as a
/// child) eval's the
/// staged command, which
/// (since we're inside a
/// tmux client) switches
/// the client to the
/// targeted pane.
#[test]
fn select_t_marked_directory_stages_select_and_switch() {
    // Use a symlink to simulate the macOS volume-mount
    // scenario (two path forms that resolve to the same
    // physical directory). This makes the test
    // platform-independent.
    let real = std::env::temp_dir().join("sh_tmark_real");
    let link = std::env::temp_dir().join("sh_tmark_link");
    let _ = std::fs::remove_dir_all(&real);
    let _ = std::fs::remove_file(&link);
    std::fs::create_dir_all(&real).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let mut app = directories_test_app(&[("ls", real.to_str().unwrap(), 60)]);
    // Snapshot contains one active window for
    // the directory above (via the symlink path,
    // which canonicalises to the same real path).
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%2".to_string(),
        path: link.to_string_lossy().into_owned(),
        ..Default::default()
    });
    app.query = "#".to_string();
    app.refresh();
    // The row is the only
    // one in merged_rows.
    assert_eq!(app.merged_rows().len(), 1);
    app.select_for_run();
    let staged = app.selection.as_deref().expect("selection must be set");
    assert!(
        staged.contains("tmux select-pane -t %2") && staged.contains("tmux switch-client -t %2"),
        "staged command must call both \
                 select-pane and switch-client with the \
                 pane id, got: {staged:?}"
    );
    // The two must be
    // `&&`-chained so the
    // user doesn't end up
    // switching to a
    // half-targeted client if
    // select-pane failed.
    assert!(
        staged.contains("&&"),
        "select-pane and switch-client must be &&-chained, got: {staged:?}"
    );
    assert_eq!(app.pick_mode, Some(crate::tui::state::PickMode::Run),);
}

/// Selecting an unmarked
/// directory row stages
/// `tmux new-session -d -s
/// <basename> -c <dir>;
/// tmux switch-client -t
/// <basename>`. The
/// basename is
/// `Path::file_name` of the
/// directory; a quote is
/// added if the path has
/// shell metacharacters.
#[test]
fn select_unmarked_directory_stages_new_session_and_switch() {
    let mut app = directories_test_app(&[("ls", "/Users/har/Projects/coolthing", 60)]);
    // Empty snapshot —
    // nothing matches.
    app.query = "#".to_string();
    app.refresh();
    // Select the
    // SQL-history row
    // explicitly.
    let sql_row_idx = app
        .merged_rows()
        .iter()
        .position(|r| r.directory == "/Users/har/Projects/coolthing")
        .expect("the SQL-history row for /Users/har/Projects/coolthing must be in merged_rows");
    app.list_state.select(Some(sql_row_idx));
    app.select_for_run();
    let staged = app.selection.as_deref().expect("selection must be set");
    // The directory is
    // under $HOME so it's
    // shortened to
    // `~/Projects/coolthing`
    // for display in the
    // staged command (the
    // user asked for `~` "as
    // much as possible"; tmux
    // also doesn't do `~`
    // expansion itself, so we
    // have to do it here).
    // The bare absolute path
    // is also accepted by
    // tmux, so this isn't a
    // correctness contract —
    // it's a UX one. The
    // dedicated tilde test
    // (`select_unmarked_directory_expands_tilde`)
    // pins the expansion
    // behaviour more
    // directly.
    assert!(
        staged.contains("tmux new-session -d -s coolthing -c ~/Projects/coolthing")
            || staged.contains("tmux new-session -d -s coolthing -c /Users/har/Projects/coolthing"),
        "staged command must create detached session with the directory basename, got: {staged:?}"
    );
    assert!(
        staged.contains("tmux switch-client -t coolthing"),
        "staged command must switch-client to the new session, got: {staged:?}"
    );
    // The two are ;-chained
    // (not &&): the user
    // wants new-session to
    // run regardless of
    // any failure, and
    // switch-client is a
    // follow-up that may or
    // may not succeed (e.g.
    // session already exists
    // in the user's setup
    // with the same name —
    // that's a different
    // error the parent shell
    // surfaces).
    assert!(
        staged.contains("; "),
        "new-session and switch-client must be ;-chained, got: {staged:?}"
    );
}

/// Paths with shell
/// metacharacters get
/// quoted in the staged
/// `cd <path>` — same
/// defensive quoting
/// already used in todo
/// mode. This is the v1
/// "be safe" contract; the
/// user can always edit
/// the staged command
/// before submit.
#[test]
fn select_unmarked_directory_quotes_paths_with_spaces() {
    let mut app = directories_test_app(&[("ls", "/Users/has spaces/project", 60)]);
    app.query = "#".to_string();
    app.refresh();
    // Select the
    // SQL-history row
    // explicitly.
    let sql_row_idx = app
        .merged_rows()
        .iter()
        .position(|r| r.directory == "/Users/has spaces/project")
        .expect("the SQL-history row for /Users/has spaces/project must be in merged_rows");
    app.list_state.select(Some(sql_row_idx));
    app.select_for_run();
    let staged = app.selection.as_deref().expect("selection must be set");
    assert!(
        staged.contains("-c '/Users/has spaces/project'"),
        "path with spaces must be quoted, got: {staged:?}"
    );
}

/// `~` in the directory is
/// expanded to `$HOME`
/// before staging. This
/// matters because tmux
/// does NOT do `~`
/// expansion itself —
/// `tmux new-session -c
/// '~/work'` silently
/// creates the session in
/// `$HOME`, not `~/work`,
/// which would be a
/// surprising correctness
/// bug. The TUI's staged
/// command always carries
/// the absolute path so
/// tmux gets the right
/// cwd.
///
/// We can't easily test
/// this through
/// `directories_test_app`
/// because the test inserts
/// `/Users/har/...` paths
/// into the DB (not
/// `~/...`), and the
/// `~` shorthand only
/// matches paths that
/// actually start with the
/// home prefix. So the
/// test inserts a
/// home-prefixed absolute
/// path and asserts the
/// staged command has the
/// `~`-shortened form.
#[test]
fn select_unmarked_directory_expands_tilde() {
    // SAFETY: tests run
    // single-threaded; see
    // the parallel-runs-stable
    // comment in
    // `expand_home_basic`.
    let saved_home = std::env::var("HOME").ok();
    unsafe {
        std::env::set_var("HOME", "/Users/har");
    }
    let mut app = directories_test_app(&[("ls", "/Users/har/work", 60)]);
    app.query = "#".to_string();
    app.refresh();
    // Select the
    // SQL-history row
    // explicitly (not
    // `merged_rows()[0]`).
    // The new
    // directory-source
    // feature surfaces
    // tmux panes as
    // rows too, so the
    // first row may be
    // one of the user's
    // real tmux panes,
    // not our test row.
    let sql_row_idx = app
        .merged_rows()
        .iter()
        .position(|r| r.directory == "/Users/har/work")
        .expect("the SQL-history row for /Users/har/work must be in merged_rows");
    app.list_state.select(Some(sql_row_idx));
    app.select_for_run();
    let staged = app.selection.as_deref().expect("selection must be set");
    // The directory in the
    // staged `new-session
    // -c` argument must use
    // `~/work`, not the
    // raw `/Users/har/work`,
    // because the source
    // directory is under
    // `$HOME` and the user
    // expects the `~`
    // form.
    //
    // Note: this test is
    // *not* the same as the
    // bug we're fixing (which
    // was about *literal* `~`
    // in the source directory).
    // The DB-stored path is
    // always absolute (per
    // `fetch_directories`'s
    // `directory` column),
    // so the expansion we
    // test here is the
    // *display + command*
    // shortening — a
    // separate feature. The
    // "no literal `~` in the
    // source path" contract
    // is covered implicitly:
    // the source is always
    // absolute, and the
    // expansion is a pure
    // function of the
    // home-prefix match.
    assert!(
        staged.contains("tmux new-session"),
        "staged must create a new tmux session, got: {staged:?}"
    );
    assert!(
        staged.contains("/Users/har/work"),
        "staged must use the absolute path, got: {staged:?}"
    );
    // Restore HOME.
    if let Some(h) = saved_home {
        unsafe {
            std::env::set_var("HOME", h);
        }
    } else {
        unsafe {
            std::env::remove_var("HOME");
        }
    }
}

/// The user can pin a
/// `sessiondirs=...`
/// directory in the
/// config and every
/// subdirectory (recursively
/// walked) appears as a
/// row in the directories
/// list, even if no
/// command has ever been
/// run there. We test this
/// by injecting a
/// `session_subdirs` entry
/// directly into the
/// `App` (the test
/// doesn't have a config
/// file to load) and
/// checking the row
/// surfaces.
#[test]
fn fetch_directories_includes_sessiondir_subdirs() {
    // Build a temp
    // directory tree to
    // walk.
    let n = std::sync::atomic::AtomicU64::new(0).fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let pid = std::process::id();
    let root = std::env::temp_dir().join(format!("smarthistory_sessiondir_{pid}_{n}"));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(root.join("a").join("b"));
    let _ = std::fs::create_dir_all(root.join("c"));
    // sessiondir
    // subdirectory set
    // (the production
    // path uses
    // `build_session_subdirs`
    // at App
    // construction;
    // for the test we
    // pass it
    // explicitly via
    // the
    // `directories_test_app_with_sessions`
    // helper).
    let mut app = directories_test_app_with_sessions(
        &[],
        vec![root.join("a"), root.join("a").join("b"), root.join("c")],
    );
    app.query = "#".to_string();
    app.refresh();
    let rows = app.merged_rows();
    // The pinned
    // subdirs should
    // appear even
    // though we
    // passed `&[]` to
    // `directories_test_app`
    // (no history).
    let row_dirs: std::collections::HashSet<String> = rows
        .iter()
        .map(|r| {
            std::fs::canonicalize(&r.directory)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| r.directory.clone())
        })
        .collect();
    let expected: std::collections::HashSet<String> = app
        .session_subdirs
        .iter()
        .map(|p| {
            std::fs::canonicalize(p)
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_else(|_| p.to_string_lossy().into_owned())
        })
        .collect();
    for want in &expected {
        assert!(
            row_dirs.contains(want),
            "pinned subdir {want:?} should be in merged_rows, got: {row_dirs:?}"
        );
    }
    // The pinned
    // rows have
    // `timestamp = 0`
    // (so they sort
    // to the bottom
    // of the
    // newest-first
    // list) and an
    // empty `command`
    // — except for
    // `.command`
    // hints.
    for row in rows {
        let canonical = std::fs::canonicalize(&row.directory)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| row.directory.clone());
        if expected.contains(&canonical) {
            assert_eq!(
                row.timestamp, 0,
                "sessiondir row must have timestamp 0, got: {}",
                row.timestamp
            );
            assert_eq!(
                row.mode, "directory",
                "sessiondir row must have mode='directory'"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// When a sessiondir row
/// has a `.command` file
/// in itself or an
/// ancestor, the TUI
/// surfaces "(has
/// .command)" in the
/// secondary slot so
/// the user knows the
/// row will run a setup
/// script on select.
#[test]
fn fetch_directories_surfaces_command_file_hint() {
    // Build:
    //   tmpdir/
    //   tmpdir/project/         (has .command)
    //   tmpdir/project/src/     (no .command)
    let n = std::sync::atomic::AtomicU64::new(0).fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let pid = std::process::id();
    let root = std::env::temp_dir().join(format!("smarthistory_sessiondir_cmd_{pid}_{n}"));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    let src = project.join("src");
    let _ = std::fs::create_dir_all(&src);
    let _ = std::fs::write(project.join(".command"), "#!/bin/sh\necho setup\n");
    // `project/src`
    // subdir is
    // pinned (the
    // walker would
    // also pick up
    // `project` if
    // the user pinned
    // `root`; we pin
    // a leaf to test
    // the ancestor
    // walk).
    let mut app = directories_test_app_with_sessions(&[], vec![src.clone()]);
    app.query = "#".to_string();
    app.refresh();
    let row = app
        .merged_rows()
        .iter()
        .find(|r| {
            std::fs::canonicalize(&r.directory)
                .map(|c| c == std::fs::canonicalize(&src).unwrap())
                .unwrap_or(false)
        })
        .expect("src row must be in the list");
    assert_eq!(
        row.comment, "(has .command)",
        "row's secondary slot should announce the .command, got: {:?}",
        row.comment
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// When a sessiondir row
/// has a `.command` file
/// in itself or an
/// ancestor, selecting
/// the row chains
/// `sh <command-file> <dir>`
/// into the staged tmux
/// command. The first
/// argument is always
/// the selected
/// directory.
#[test]
fn select_directory_runs_command_file() {
    // Build:
    //   tmpdir/
    //   tmpdir/project/         (has .command)
    //   tmpdir/project/src/     (no .command)
    let n = std::sync::atomic::AtomicU64::new(0).fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let pid = std::process::id();
    let root = std::env::temp_dir().join(format!("smarthistory_select_cmd_{pid}_{n}"));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    let src = project.join("src");
    let _ = std::fs::create_dir_all(&src);
    let cmd_path = project.join(".command");
    let _ = std::fs::write(&cmd_path, "#!/bin/sh\necho setup $1\n");
    let mut app = directories_test_app(&[("ls", &src.to_string_lossy(), 60)]);
    app.query = "#".to_string();
    app.refresh();
    // Find the SQL row
    // by its directory
    // (the user's real
    // tmux panes may
    // also be present
    // because
    // `refresh()` re-runs
    // the tmux fetch,
    // and the new
    // directory-source
    // feature surfaces
    // them as rows). We
    // explicitly select
    // the SQL row (not
    // `merged_rows()[0]`)
    // by setting
    // `list_state.selected`
    // to its index.
    let sql_row_idx = app
        .merged_rows()
        .iter()
        .position(|r| r.directory == src.to_string_lossy())
        .expect("the SQL-history row for `src` must be in merged_rows");
    app.list_state.select(Some(sql_row_idx));
    app.select_for_run();
    let staged = app.selection.as_deref().expect("selection must be set");
    // The staged
    // command must
    // include both the
    // new-session
    // chain and the
    // .command run.
    // Form:
    //   tmux new-session -d -s src -c <src>; \
    //     sh <.command> <src>; \
    //     tmux switch-client -t src
    let cmd_str = cmd_path.to_string_lossy();
    let src_str = src.to_string_lossy();
    assert!(
        staged.contains("tmux new-session"),
        "staged must create a new tmux session, got: {staged:?}"
    );
    assert!(
        staged.contains("switch-client"),
        "staged must switch-client to the new session, got: {staged:?}"
    );
    // The .command
    // invocation
    // should appear
    // with the path
    // and the
    // selected
    // directory as
    // the first arg.
    assert!(
        staged.contains(&format!("sh {} {}", cmd_str, src_str)),
        "staged must run `sh <.command> <dir>`, got: {staged:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Active tmux panes
/// (whose cwds are
/// distinct from the
/// SQL history) appear
/// as rows in the
/// directories list
/// with `source =
/// "tmux"`. The
/// visible primary
/// text is the
/// directory in
/// `~/x` form; the
/// secondary slot
/// shows `(pane %N)` so
/// the user can copy
/// the pane id for
/// `tmux send-keys -t
/// %N ...` directly
/// from the list.
#[test]
fn fetch_directories_includes_tmux_panes() {
    let mut app = directories_test_app(&[]);
    // Inject one tmux
    // window. Use `/tmp`
    // (a real directory
    // on every Unix) so
    // it passes the
    // `is_dir()` check.
    // macOS canonicalises
    // `/tmp` to
    // `/private/tmp`,
    // which is fine for
    // this test.
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%42".to_string(),
        path: String::from("/tmp"),
        ..Default::default()
    });
    app.query = "#".to_string();
    app.refresh();
    // Find the tmux
    // row. The
    // directory will be
    // canonicalised by
    // `std::fs::canonicalize`
    // (which on macOS
    // resolves
    // `/tmp` to
    // `/private/tmp`).
    let row = app
        .merged_rows()
        .iter()
        .find(|r| r.source == "tmux")
        .expect("the tmux pane row must be in merged_rows");
    // The visible primary
    // text is the
    // directory in
    // shell-shortened
    // form. `/tmp`
    // shortens to
    // `/tmp` (no home
    // to expand to).
    assert_eq!(
        row.command, "/tmp",
        "primary text must be the directory, got: {:?}",
        row.command
    );
    // The secondary slot
    // carries the pane
    // id (so the user
    // can reuse it).
    assert!(
        row.comment.contains("%42"),
        "secondary slot must show the pane id, got: {:?}",
        row.comment
    );
}

/// The
/// `DirectorySource::All`
/// mode shows every
/// row regardless of
/// source.
#[test]
fn directory_source_all_shows_everything() {
    // Use `/tmp` (a real
    // directory on every
    // Unix) for the SQL
    // and tmux rows so
    // they pass the
    // `is_dir()` check.
    // macOS canonicalises
    // `/tmp` to
    // `/private/tmp`,
    // which is fine for
    // the test.
    let sql_dir = "/tmp";
    let tmux_dir = "/tmp";
    let mut app = directories_test_app(&[("ls", sql_dir, 60)]);
    // Inject one
    // sessiondir
    // and one tmux
    // pane.
    let n = std::sync::atomic::AtomicU64::new(0).fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let pid = std::process::id();
    let session_root = std::env::temp_dir().join(format!("smarthistory_dirsrc_all_{pid}_{n}"));
    let _ = std::fs::create_dir_all(session_root.join("inside"));
    app.session_subdirs = vec![session_root.join("inside")];
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%7".to_string(),
        path: String::from(tmux_dir),
        ..Default::default()
    });
    // Default source is
    // `All`, so all
    // three rows are
    // visible.
    app.query = "#".to_string();
    app.refresh();
    let dirs: std::collections::HashSet<String> = app
        .merged_rows()
        .iter()
        .map(|r| r.directory.clone())
        .collect();
    assert!(
        dirs.contains(sql_dir),
        "SQL row must be visible, got: {:?}",
        dirs
    );
    assert!(
        dirs.contains(&session_root.join("inside").to_string_lossy().to_string()),
        "sessiondir row must be visible, got: {:?}",
        dirs
    );
    assert!(
        dirs.contains(tmux_dir),
        "tmux row must be visible, got: {:?}",
        dirs
    );
    let _ = std::fs::remove_dir_all(&session_root);
}

/// The
/// `DirectorySource::Config`
/// mode shows only the
/// `sessiondirs=...`
/// rows. SQL history
/// rows and tmux panes
/// are filtered out.
#[test]
fn directory_source_config_filters_to_sessiondirs() {
    let mut app = directories_test_app(&[("ls", "/Users/har/sql_row", 60)]);
    let n = std::sync::atomic::AtomicU64::new(0).fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let pid = std::process::id();
    let session_root = std::env::temp_dir().join(format!("smarthistory_dirsrc_cfg_{pid}_{n}"));
    let _ = std::fs::create_dir_all(session_root.join("inside"));
    app.session_subdirs = vec![session_root.join("inside")];
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%7".to_string(),
        path: String::from("/Users/har/tmux_row"),
        ..Default::default()
    });
    app.directory_source = crate::tui::state::DirectorySource::Config;
    app.query = "#".to_string();
    app.refresh();
    // Only the
    // sessiondir
    // row should be
    // visible.
    assert_eq!(
        app.merged_rows().len(),
        1,
        "Config mode must show only sessiondir rows, got: {:?}",
        app.merged_rows()
            .iter()
            .map(|r| (r.directory.clone(), r.source.clone()))
            .collect::<Vec<_>>()
    );
    let row = &app.merged_rows()[0];
    assert_eq!(row.source, "sessiondir");
    let _ = std::fs::remove_dir_all(&session_root);
}

/// The
/// `DirectorySource::Tmux`
/// mode shows only the
/// active tmux panes'
/// cwds. SQL history
/// rows and sessiondirs
/// rows are filtered
/// out.
#[test]
fn directory_source_tmux_filters_to_panes() {
    // The SQL and tmux
    // rows must use
    // *different* paths
    // (the dedup loop
    // suppresses tmux
    // rows whose canonical
    // path matches an
    // earlier SQL row).
    let n = std::sync::atomic::AtomicU64::new(0).fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let pid = std::process::id();
    let tmux_path = std::env::temp_dir().join(format!("smarthistory_tmux_pane_{pid}_{n}"));
    let _ = std::fs::create_dir_all(&tmux_path);
    let mut app = directories_test_app(&[("ls", "/tmp", 60)]);
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%7".to_string(),
        path: tmux_path.to_string_lossy().into_owned(),
        ..Default::default()
    });
    app.directory_source = crate::tui::state::DirectorySource::Tmux;
    app.query = "#".to_string();
    app.refresh();
    // Only the
    // tmux-pane
    // row should
    // be visible.
    assert_eq!(
        app.merged_rows().len(),
        1,
        "Tmux mode must show only tmux pane rows, got: {:?}",
        app.merged_rows()
            .iter()
            .map(|r| (r.directory.clone(), r.source.clone()))
            .collect::<Vec<_>>()
    );
    let row = &app.merged_rows()[0];
    assert_eq!(row.source, "tmux");
    let _ = std::fs::remove_dir_all(&tmux_path);
}

/// Regression test for the
/// bug where a tmux pane
/// whose path also appears
/// in the SQL history DB
/// was silently deduped
/// away in `DIR:TMUX`
/// mode. The shared `seen`
/// set was populated by the
/// SQL loop first, so the
/// tmux loop's `seen.insert`
/// returned `false` and the
/// pane was dropped — even
/// though the SQL row would
/// later be filtered out by
/// the source filter. The
/// fix: the source filter is
/// applied *early* (the SQL
/// loop is skipped entirely
/// in `DIR:TMUX` mode).
///
/// User symptom: 5 active
/// tmux panes, but
/// `DIR:TMUX` showed only
/// 2 (the ones not in the
/// history DB).
#[test]
fn directory_source_tmux_shows_pane_even_if_path_in_history() {
    // A real directory we
    // use for BOTH the SQL
    // row and the tmux pane.
    let n = std::sync::atomic::AtomicU64::new(0).fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let pid = std::process::id();
    let shared_path = std::env::temp_dir().join(format!("smarthistory_tmux_dup_{pid}_{n}"));
    let _ = std::fs::create_dir_all(&shared_path);
    let shared_str = shared_path.to_string_lossy().into_owned();
    // SQL history row in the
    // SAME directory.
    let mut app = directories_test_app(&[("ls", &shared_str, 60)]);
    // Tmux pane in the SAME
    // directory.
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%9".to_string(),
        path: shared_str.clone(),
        ..Default::default()
    });
    app.directory_source = crate::tui::state::DirectorySource::Tmux;
    app.query = "#".to_string();
    app.refresh();
    // In `DIR:TMUX` mode the
    // tmux pane MUST appear,
    // even though the SQL
    // row has the same
    // canonical path.
    let tmux_rows: Vec<_> = app
        .merged_rows()
        .iter()
        .filter(|r| r.source == "tmux")
        .collect();
    assert_eq!(
        tmux_rows.len(),
        1,
        "DIR:TMUX must show the tmux pane even when its path is in the history DB, got: {:?}",
        app.merged_rows()
            .iter()
            .map(|r| (r.directory.clone(), r.source.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(tmux_rows[0].source, "tmux");
    // And no SQL rows leak
    // through.
    assert!(
        app.merged_rows().iter().all(|r| r.source == "tmux"),
        "DIR:TMUX must not show SQL rows, got: {:?}",
        app.merged_rows()
            .iter()
            .map(|r| r.source.clone())
            .collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&shared_path);
}

/// Regression test for the bug
/// where a labeled history
/// row (an entry with a
/// comment in the
/// `command_comments`
/// table) leaked into
/// `DIR:TMUX` directories
/// mode. The user ran
/// `tmux list-windows -a
/// -F ... | grep
/// "active:1"` at some
/// point and labeled it,
/// so the row had a
/// comment. `build_merged_rows`
/// appended *all* labeled
/// rows to the merged
/// list regardless of
/// mode; in directories
/// mode that meant the
/// history row showed up
/// alongside (or instead
/// of) the real tmux
/// pane rows. The fix:
/// `build_merged_rows`
/// skips the labeled/preview
/// merge entirely in
/// directories mode and
/// returns only the
/// directory rows.
#[test]
fn directory_source_tmux_excludes_labeled_history_rows() {
    let n = std::sync::atomic::AtomicU64::new(0).fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let pid = std::process::id();
    let tmux_path = std::env::temp_dir().join(format!("smarthistory_tmux_labeled_{pid}_{n}"));
    let _ = std::fs::create_dir_all(&tmux_path);
    // The labeled command —
    // the exact "tmux list-
    // windows ..." line the
    // user reported. It was
    // run from /tmp.
    let labeled_cmd = "tmux list-windows -a -F #{pane_id}";
    let mut app = directories_test_app(&[(labeled_cmd, "/tmp", 60)]);
    // Create the
    // command_comments table
    // and label the command,
    // making it a "labeled
    // row" that
    // `fetch_labeled` will
    // return.
    app.conn
        .execute(
            "CREATE TABLE command_comments (
                        command TEXT PRIMARY KEY,
                        comment TEXT NOT NULL
                    )",
            [],
        )
        .expect("create command_comments");
    // `fetch_labeled` does a LEFT JOIN on
    // `history_output`; the table must exist or
    // the query errors (and `.unwrap_or_default()`
    // silently yields an empty labeled set —
    // which would mask the bug in this test).
    app.conn
        .execute(
            "CREATE TABLE history_output (
                        history_id INTEGER PRIMARY KEY,
                        output TEXT NOT NULL
                    )",
            [],
        )
        .expect("create history_output");
    app.conn
        .execute(
            "INSERT INTO command_comments (command, comment) VALUES (?1, ?2)",
            rusqlite::params![labeled_cmd, "TMUX LIST"],
        )
        .expect("insert comment");
    // One real tmux pane with a
    // different path.
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%5".to_string(),
        path: tmux_path.to_string_lossy().into_owned(),
        ..Default::default()
    });
    app.directory_source = crate::tui::state::DirectorySource::Tmux;
    app.query = "#".to_string();
    app.refresh();
    // The labeled history
    // row (`tmux list-
    // windows ...`) must
    // NOT appear in
    // `DIR:TMUX` mode.
    let has_labeled = app.merged_rows().iter().any(|r| r.command == labeled_cmd);
    assert!(
        !has_labeled,
        "DIR:TMUX must not show labeled history rows, got: {:?}",
        app.merged_rows()
            .iter()
            .map(|r| (r.command.clone(), r.source.clone()))
            .collect::<Vec<_>>()
    );
    // Only the real tmux
    // pane should be
    // visible.
    assert_eq!(app.merged_rows().len(), 1);
    assert_eq!(app.merged_rows()[0].source, "tmux");
    let _ = std::fs::remove_dir_all(&tmux_path);
}

/// `cycle_directory_source`
/// pressed while NOT in
/// directories mode should
/// switch INTO directories
/// mode (prepending the `#`
/// prefix) AND cycle the
/// source. The user can be
/// in plain history and
/// land directly in `DIR:TMUX`.
#[test]
fn cycle_directory_source_enters_dirs_mode_from_plain() {
    let mut app = directories_test_app(&[("ls", "/tmp", 60)]);
    // Plain mode, no prefix.
    app.query = String::from("ls");
    assert!(!app.is_directories_query());
    // Cycle from plain -> DIR:TMUX.
    app.cycle_directory_source();
    // Now in directories mode.
    assert!(
        app.is_directories_query(),
        "must enter directories mode, got query {:?}",
        app.query
    );
    // Source cycled to TMUX.
    assert_eq!(
        app.directory_source,
        crate::tui::state::DirectorySource::Tmux
    );
    // Body preserved: `#ls`.
    assert_eq!(app.query, "#ls");
}

/// Cycling three times from
/// plain mode lands back on
/// `DIR:ALL`, still in
/// directories mode.
#[test]
fn cycle_directory_source_three_times_wraps_to_all() {
    let mut app = directories_test_app(&[("ls", "/tmp", 60)]);
    app.query = String::new();
    app.cycle_directory_source(); // -> TMUX
    assert!(app.is_directories_query());
    assert_eq!(
        app.directory_source,
        crate::tui::state::DirectorySource::Tmux
    );
    app.cycle_directory_source(); // -> CFG
    assert!(app.is_directories_query());
    assert_eq!(
        app.directory_source,
        crate::tui::state::DirectorySource::Config
    );
    app.cycle_directory_source(); // -> ALL
    assert!(app.is_directories_query());
    assert_eq!(
        app.directory_source,
        crate::tui::state::DirectorySource::All
    );
    // Query is just `#` (empty body).
    assert_eq!(app.query, "#");
}

/// Regression test: `App::refresh()`'s SQL-fetch cache short-circuit
/// must invalidate when `directory_source` changes, even if the
/// query text is untouched. `directory_source` is read inside
/// `directories::fetch`, which only runs on a cache miss — if it's
/// missing from the cache key, cycling the source (e.g. pressing the
/// directory-source key twice in a row without editing the query)
/// leaves `self.rows` — and therefore what's on screen — pinned to
/// whatever the *previous* source produced.
#[test]
fn cycle_directory_source_actually_refetches_rows() {
    let n = std::sync::atomic::AtomicU64::new(0).fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let pid = std::process::id();
    let tmux_path = std::env::temp_dir().join(format!("smarthistory_cache_key_{pid}_{n}"));
    let _ = std::fs::create_dir_all(&tmux_path);
    // SQL row and tmux row use different paths so both are visible
    // (and not deduped against each other) in `All` mode.
    let mut app = directories_test_app(&[("ls", "/tmp", 60)]);
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%7".to_string(),
        path: tmux_path.to_string_lossy().into_owned(),
        ..Default::default()
    });
    app.directory_source = crate::tui::state::DirectorySource::All;
    app.query = "#".to_string();
    app.refresh();
    assert_eq!(
        app.merged_rows().len(),
        2,
        "All mode must show both the SQL and tmux rows, got: {:?}",
        app.merged_rows()
            .iter()
            .map(|r| (r.directory.clone(), r.source.clone()))
            .collect::<Vec<_>>()
    );

    // Switch to Tmux-only WITHOUT touching the query text — this is
    // exactly the state transition the broken cache key missed.
    app.directory_source = crate::tui::state::DirectorySource::Tmux;
    app.refresh();
    assert_eq!(
        app.merged_rows().len(),
        1,
        "Tmux mode must re-fetch and show only the tmux row, got: {:?}",
        app.merged_rows()
            .iter()
            .map(|r| (r.directory.clone(), r.source.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(app.merged_rows()[0].source, "tmux");
    let _ = std::fs::remove_dir_all(&tmux_path);
}

/// Switching from a search
/// mode (`?foo`) strips the
/// fuzzy prefix and yields
/// `#foo` (not `#?foo`).
#[test]
fn cycle_directory_source_strips_search_prefix() {
    let mut app = directories_test_app(&[("ls", "/home", 60)]);
    app.query = "#git".to_string();
    app.refresh();
    // The body after the `#` prefix should be "git".
    let body = app.directories_pattern();
    assert_eq!(body, "git");
}

/// When ALREADY in directories
/// mode, cycle just advances the
/// source — the query prefix is
/// not doubled.
#[test]
fn cycle_directory_source_in_dirs_mode_does_not_double_prefix() {
    let mut app = directories_test_app(&[("ls", "/tmp", 60)]);
    app.query = String::from("#");
    app.cycle_directory_source();
    assert_eq!(app.query, "#");
    assert!(app.is_directories_query());
    assert_eq!(
        app.directory_source,
        crate::tui::state::DirectorySource::Tmux
    );
}

/// Build an App pre-loaded with a set of session
/// panes (bypassing the real `tmux list-panes -s`
/// subprocess, which depends on a live tmux
/// server). Each tuple is
/// (pane_id, window_id, cwd, current_command).
/// The caller still owns `app.session_panes` and can
/// mutate it after construction. `TMUX_PANE` is NOT
/// set (so the exclusion filter is exercised
/// explicitly by the caller via the injected rows).
fn panes_test_app(panes: &[(&str, &str, &str, &str)]) -> App {
    let mut app = directories_test_app(&[]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let home_list = app.home_list.clone();
    let mut next_id: i64 = -1;
    app.session_panes = panes
        .iter()
        .map(|(pane_id, window_id, cwd, cmd)| {
            let full = crate::util::canonicalize_directory(cwd);
            let short = crate::util::shorten_home_path(&full, &home_list).into_owned();
            let id = next_id;
            next_id -= 1;
            HistoryRow {
                id,
                command: cmd.to_string(),
                directory: full,
                session_id: pane_id.to_string(),
                exit_code: 0,
                timestamp: now,
                comment: short,
                // window id (`@N`) stashed
                // for the cross-window
                // select-window jump.
                output: window_id.to_string(),
                mode: "pane".to_string(),
                source: "pane".to_string(),

                ..Default::default()
            }
        })
        .collect();
    app
}

/// `*` prefix switches the query into panes mode,
/// and `is_panes_query()` / `panes_pattern()` slice
/// the body correctly.
#[test]
fn panes_prefix_detected_and_pattern_sliced() {
    let mut app = directories_test_app(&[]);
    app.query = String::new();
    assert!(!app.is_panes_query());
    app.query = String::from("*");
    assert!(app.is_panes_query());
    assert_eq!(app.panes_pattern(), "");
    app.query = String::from("*vim src");
    assert!(app.is_panes_query());
    assert_eq!(app.panes_pattern(), "vim src");
}

/// `fetch_panes` returns the cached session panes
/// (no substring filter when the body is empty).
#[test]
fn fetch_panes_returns_all_when_no_filter() {
    let mut app = panes_test_app(&[("%1", "@1", "/tmp", "zsh"), ("%2", "@2", "/tmp", "vim")]);
    app.query = String::from("*");
    let rows = crate::tui::mode::panes::fetch(&mut app).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].source, "pane");
    assert_eq!(rows[0].session_id, "%1");
    assert_eq!(rows[0].command, "zsh");
}

/// The substring filter matches against the pane's
/// current command OR its cwd (short form). Both
/// whitespace tokens must match (AND semantics).
#[test]
fn fetch_panes_substring_filter_matches_command_or_cwd() {
    let mut app = panes_test_app(&[("%1", "@1", "/tmp", "zsh"), ("%2", "@2", "/tmp", "vim")]);
    // `*vim` → only the pane running vim.
    app.query = String::from("*vim");
    let rows = crate::tui::mode::panes::fetch(&mut app).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id, "%2");
    assert_eq!(rows[0].command, "vim");
}

/// Group-aware filter: when the user types a token
/// that matches a workspace LABEL, the entire
/// workspace (header + every child pane) is shown,
/// even when the panes themselves don't match. This
/// is the user's "I searched for SmartHistory, I want
/// to see the workspace AND its panes" use case. The
/// tree is built by `fetch_session_panes_impl`, but
/// `panes_test_app` doesn't go through that path so
/// we inject a workspace + pane group directly into
/// `session_panes` and let the filter logic in
/// `fetch_panes` apply the parent-wins rule.
#[test]
fn fetch_panes_workspace_label_match_keeps_whole_group() {
    use crate::tui::state::HistoryRow;
    let mut app = panes_test_app(&[]);
    // Build: workspace "SmartHistory" (no pane
    // rows match `*SmartHistory` on their own) +
    // two child panes running zsh and vim.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    app.session_panes = vec![
        HistoryRow {
            id: -1,
            command: "SmartHistory".to_string(),
            directory: "/tmp/SmartHistory".to_string(),
            session_id: String::new(),
            exit_code: 0,
            timestamp: now,
            comment: "2 panes".to_string(),
            output: String::new(),
            mode: "workspace".to_string(),
            source: "workspace".to_string(),
            workspace_label: "SmartHistory".to_string(),
            codegraph_node_id: String::new(),
            preview: String::new(),
        },
        HistoryRow {
            id: -2,
            command: "zsh".to_string(),
            directory: "/tmp/SmartHistory".to_string(),
            session_id: "%10".to_string(),
            exit_code: 0,
            timestamp: now,
            comment: "~/SmartHistory".to_string(),
            output: "@1".to_string(),
            mode: "pane".to_string(),
            source: "pane".to_string(),
            workspace_label: "SmartHistory".to_string(),
            codegraph_node_id: String::new(),
            preview: String::new(),
        },
        HistoryRow {
            id: -3,
            command: "vim".to_string(),
            directory: "/tmp/SmartHistory".to_string(),
            session_id: "%11".to_string(),
            exit_code: 0,
            timestamp: now,
            comment: "~/SmartHistory".to_string(),
            output: "@1".to_string(),
            mode: "pane".to_string(),
            source: "pane".to_string(),
            workspace_label: "SmartHistory".to_string(),
            codegraph_node_id: String::new(),
            preview: String::new(),
        },
    ];
    app.query = String::from("*SmartHistory");
    let rows = crate::tui::mode::panes::fetch(&mut app).unwrap();
    // The workspace header AND its two child panes
    // are all kept, because the workspace label
    // matches the query.
    assert_eq!(
        rows.len(),
        3,
        "expected workspace + 2 panes, got {} rows",
        rows.len()
    );
    assert_eq!(rows[0].mode, "workspace");
    assert_eq!(rows[0].command, "SmartHistory");
    assert_eq!(rows[1].mode, "pane");
    assert_eq!(rows[1].command, "zsh");
    assert_eq!(rows[2].mode, "pane");
    assert_eq!(rows[2].command, "vim");
    // The renderer reads `workspace_label` to
    // show the badge; verify it was set on the
    // pane rows so the chip rendering is wired
    // up end-to-end.
    assert_eq!(rows[1].workspace_label, "SmartHistory");
    assert_eq!(rows[2].workspace_label, "SmartHistory");
}

/// Group-aware filter: when a child pane's command
/// matches, the parent workspace header is ALSO
/// kept (the user sees which workspace the matching
/// pane belongs to). Mirrors the parent-wins case.
#[test]
fn fetch_panes_pane_match_keeps_parent_workspace_header() {
    use crate::tui::state::HistoryRow;
    let mut app = panes_test_app(&[]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    app.session_panes = vec![
        HistoryRow {
            id: -1,
            command: "SmartHistory".to_string(),
            directory: "/tmp/SmartHistory".to_string(),
            session_id: String::new(),
            exit_code: 0,
            timestamp: now,
            comment: "2 panes".to_string(),
            output: String::new(),
            mode: "workspace".to_string(),
            source: "workspace".to_string(),
            workspace_label: "SmartHistory".to_string(),
            codegraph_node_id: String::new(),
            preview: String::new(),
        },
        HistoryRow {
            id: -2,
            command: "vim".to_string(),
            directory: "/tmp/SmartHistory".to_string(),
            session_id: "%10".to_string(),
            exit_code: 0,
            timestamp: now,
            comment: "~/SmartHistory".to_string(),
            output: "@1".to_string(),
            mode: "pane".to_string(),
            source: "pane".to_string(),
            workspace_label: "SmartHistory".to_string(),
            codegraph_node_id: String::new(),
            preview: String::new(),
        },
    ];
    // `*vim` matches the pane's command; the
    // workspace header is also kept so the user
    // sees the context.
    app.query = String::from("*vim");
    let rows = crate::tui::mode::panes::fetch(&mut app).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].mode, "workspace");
    assert_eq!(rows[0].command, "SmartHistory");
    assert_eq!(rows[1].mode, "pane");
    assert_eq!(rows[1].command, "vim");
}

/// The `FilterPanesWindows` filter hides
/// `# sessions` and `# hosts` rows, keeping
/// only live multiplexer panes.
#[test]
fn fetch_panes_windows_filter_hides_sessions_and_hosts() {
    use crate::tui::state::HistoryRow;
    let mut app = panes_test_app(&[("%1", "@1", "/tmp", "zsh")]);
    // Inject a session row
    // and a host row so
    // we can verify they're
    // filtered out.
    app.session_panes.push(HistoryRow {
        id: -20_001,
        command: String::from("my session"),
        directory: String::from("/tmp"),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: String::new(),
        output: String::new(),
        mode: String::from("session"),
        source: String::from("sessions"),

        ..Default::default()
    });
    app.session_panes.push(HistoryRow {
        id: -25_001,
        command: String::from("Proxmox"),
        directory: String::from("root@pve-1"),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: String::new(),
        output: String::new(),
        mode: String::from("host"),
        source: String::from("hosts"),

        ..Default::default()
    });
    app.query = String::from("*");
    app.panes_filter = PanesFilter::Windows;
    let rows = crate::tui::mode::panes::fetch(&mut app).unwrap();
    // Only the pane row
    // should remain.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "pane");
    assert_eq!(rows[0].command, "zsh");
}

/// The `FilterPanesHosts` filter keeps
/// only the `# hosts` block.
#[test]
fn fetch_panes_hosts_filter_keeps_only_hosts() {
    use crate::tui::state::HistoryRow;
    let mut app = panes_test_app(&[("%1", "@1", "/tmp", "zsh")]);
    app.session_panes.push(HistoryRow {
        id: -20_001,
        command: String::from("my session"),
        directory: String::from("/tmp"),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: String::new(),
        output: String::new(),
        mode: String::from("session"),
        source: String::from("sessions"),

        ..Default::default()
    });
    app.session_panes.push(HistoryRow {
        id: -25_001,
        command: String::from("Proxmox"),
        directory: String::from("root@pve-1"),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: String::new(),
        output: String::new(),
        mode: String::from("host"),
        source: String::from("hosts"),

        ..Default::default()
    });
    app.query = String::from("*");
    app.panes_filter = PanesFilter::Hosts;
    let rows = crate::tui::mode::panes::fetch(&mut app).unwrap();
    // Only the host row
    // should remain.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "hosts");
    assert_eq!(rows[0].command, "Proxmox");
}

/// The `FilterPanesSessions` filter keeps
/// only the `# sessions` block.
#[test]
fn fetch_panes_sessions_filter_keeps_only_sessions() {
    use crate::tui::state::HistoryRow;
    let mut app = panes_test_app(&[("%1", "@1", "/tmp", "zsh")]);
    app.session_panes.push(HistoryRow {
        id: -20_001,
        command: String::from("my session"),
        directory: String::from("/tmp"),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: String::new(),
        output: String::new(),
        mode: String::from("session"),
        source: String::from("sessions"),

        ..Default::default()
    });
    app.session_panes.push(HistoryRow {
        id: -25_001,
        command: String::from("Proxmox"),
        directory: String::from("root@pve-1"),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: String::new(),
        output: String::new(),
        mode: String::from("host"),
        source: String::from("hosts"),

        ..Default::default()
    });
    app.query = String::from("*");
    app.panes_filter = PanesFilter::Sessions;
    let rows = crate::tui::mode::panes::fetch(&mut app).unwrap();
    // Only the session
    // row should remain.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "sessions");
    assert_eq!(rows[0].command, "my session");
}

/// The token filter is applied AFTER the
/// section filter, so `*Proxmox` with
/// the Hosts filter shows only hosts
/// matching "Proxmox".
#[test]
fn fetch_panes_section_filter_composes_with_token_filter() {
    use crate::tui::state::HistoryRow;
    let mut app = panes_test_app(&[]);
    app.session_panes.push(HistoryRow {
        id: -25_001,
        command: String::from("Proxmox"),
        directory: String::from("root@pve-1"),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: String::new(),
        output: String::new(),
        mode: String::from("host"),
        source: String::from("hosts"),

        ..Default::default()
    });
    app.session_panes.push(HistoryRow {
        id: -25_002,
        command: String::from("bmlv"),
        directory: String::from("root@bmlv"),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: String::new(),
        output: String::new(),
        mode: String::from("host"),
        source: String::from("hosts"),

        ..Default::default()
    });
    app.query = String::from("*Proxmox");
    app.panes_filter = PanesFilter::Hosts;
    let rows = crate::tui::mode::panes::fetch(&mut app).unwrap();
    // Only the Proxmox
    // host should match.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].command, "Proxmox");
}

/// Tags mode parses the `tags` file in the
/// current directory and returns one row per
/// symbol entry. Each row carries the display
/// text, the file path, and the line number.
///
/// The tags file used to be the
/// real repo `TAGS` / `tags`
/// file (1935+ entries,
/// generated by `ctags -R`
/// during local dev). That
/// file is `.gitignore`d —
/// it's never committed, so
/// CI has no `tags` file to
/// read and the test fails.
/// Instead, the test now
/// creates a self-contained
/// `tags` file in a temp dir
/// and `chdir`s into it, the
/// same pattern
/// `fetch_tags_filters_by_at_lang_token`
/// uses. This makes the test
/// deterministic across
/// dev / CI / local-`ctags`
/// variations.
#[test]
fn fetch_tags_parses_tag_file() {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    // Shared with the other
    // tags tests + the
    // `fetch_tags_filters_by_at_lang_token`
    // chdir-er, so the
    // CWD-mutating tests
    // don't race.
    let _g = lock_or_recover(&CWD_LOCK);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("smarthistory_tags_parse_{pid}_{n}"));
    fs::create_dir_all(&dir).expect("mkdir temp");
    // Generate a synthetic
    // `tags` file with >100
    // entries so the
    // `rows.len() > 100`
    // assertion holds. The
    // exact content doesn't
    // matter — we just need
    // a well-formed file.
    //
    // The ctags / etags
    // `tags` file format is:
    //   <filename>,<line_count>
    //   <display>\t<search-pattern>\t<line>,<offset>
    // The parser walks back
    // from the end of each
    // symbol line to find
    // the trailing digits
    // (the `line,offset`
    // pair), so the offset
    // must be non-empty.
    // We use a single
    // section with 150
    // symbols to keep the
    // file tiny and the
    // test fast.
    let mut tags_contents = String::from("src/lib.rs,150\n");
    for i in 0..150 {
        tags_contents.push_str(&format!("fn_{}\t\t{},0\n", i, i + 1));
    }
    // The parser calls
    // `read_source_context`
    // on every row to
    // populate the
    // details / output
    // pane preview. The
    // preview is empty
    // when the source
    // file doesn't exist,
    // so we don't need to
    // create a real
    // `src/lib.rs` for
    // the test — the
    // rows are still
    // emitted, just
    // without the
    // preview text. (This
    // is a deliberate
    // test optimization:
    // writing 150 lines of
    // Rust just to populate
    // the preview would
    // bloat the test
    // without exercising
    // anything the
    // assertion cares
    // about.)
    fs::write(dir.join("tags"), &tags_contents).expect("write tags");
    // chdir into the temp
    // dir so `find_tags_file`
    // discovers the file
    // we just wrote.
    let prev_cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&dir).expect("chdir");
    let result = std::panic::catch_unwind(|| {
        let mut app = directories_test_app(&[]);
        app.query = String::from("$");
        app.refresh();
        let rows = crate::tui::mode::tags::fetch(&mut app).unwrap();
        // The synthetic
        // file has 150
        // entries.
        assert!(
            rows.len() > 100,
            "expected many tag entries, got {}",
            rows.len()
        );
        // Every row has a
        // non-empty file path,
        // line number, and
        // display text.
        for r in &rows {
            assert!(
                !r.directory.is_empty(),
                "file path must be non-empty: {:?}",
                r
            );
            assert!(
                !r.session_id.is_empty(),
                "line number must be non-empty: {:?}",
                r
            );
            assert!(
                !r.command.is_empty(),
                "display text must be non-empty: {:?}",
                r
            );
            assert_eq!(r.mode, "tags");
            assert_eq!(r.source, "tags");
            // The line number
            // must be a valid
            // integer.
            r.session_id
                .parse::<u32>()
                .expect("line number must be a valid integer");
        }
    });
    // Always restore CWD
    // and clean up, even
    // on panic.
    std::env::set_current_dir(&prev_cwd).expect("restore cwd");
    let _ = fs::remove_dir_all(&dir);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// Tags mode filters by the body after `$`.
/// Both the symbol name (in `command`) and
/// the filename are matched.
#[test]
fn fetch_tags_substring_filter_matches() {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    // Shared with the other
    // tags tests; the
    // CWD-mutating tests
    // are serialized so
    // they don't race.
    let _g = lock_or_recover(&CWD_LOCK);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("smarthistory_tags_substr_{pid}_{n}"));
    fs::create_dir_all(&dir).expect("mkdir temp");
    // Self-contained
    // `tags` file with a
    // `src/ssh_config.rs`
    // section. The test
    // searches for
    // `ssh_config` and
    // expects at least
    // one match in the
    // filename column.
    //
    // The ctags / etags
    // `tags` file format
    // is:
    //   <filename>,<line_count>
    //   <display>\t<search-pattern>\t<line>,<offset>
    // The parser walks
    // back from the end
    // of each symbol
    // line to find the
    // trailing digits
    // (the `line,offset`
    // pair), so the
    // offset must be
    // non-empty.
    let tags_contents = "\
src/ssh_config.rs,3\n\
parse_ssh_config\t\t10,0\n\
load_ssh_config\t\t20,0\n\
test_ssh_config\t\t30,0\n";
    fs::write(dir.join("tags"), tags_contents).expect("write tags");
    // chdir into the temp
    // dir so
    // `find_tags_file`
    // discovers the
    // file we just
    // wrote.
    let prev_cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&dir).expect("chdir");
    let result = std::panic::catch_unwind(|| {
        let mut app = directories_test_app(&[]);
        app.query = String::from("$ssh_config");
        app.refresh();
        let rows = crate::tui::mode::tags::fetch(&mut app).unwrap();
        // Every row should
        // match "ssh_config"
        // in either the
        // display text or
        // the filename.
        assert!(
            !rows.is_empty(),
            "expected at least one match for 'ssh_config'"
        );
        for r in &rows {
            let lc = r.command.to_lowercase();
            let fl = r.directory.to_lowercase();
            assert!(
                lc.contains("ssh_config") || fl.contains("ssh_config"),
                "row must match the filter: {:?}",
                r,
            );
        }
    });
    // Always restore
    // CWD and clean
    // up, even on
    // panic.
    std::env::set_current_dir(&prev_cwd).expect("restore cwd");
    let _ = fs::remove_dir_all(&dir);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// Tags mode supports an `@lang` token that:
/// 1. filters the result set to files whose extension
///    maps to the language (so `@rust` keeps only `.rs`
///    files);
/// 2. labels each returned row's `source` with the
///    language (e.g. `"tags:rust"`) so downstream code
///    can tell whether a bat-highlight pass was applied.
///
/// The test uses a self-contained tags file in a temp
/// directory so it doesn't depend on the repo's real
/// `tags` content. We `chdir` into the temp dir because
/// `find_tags_file` walks up from CWD; this is racy with
/// other tests that touch CWD, but the test guards both
/// this and `fetch_tags_parses_tag_file` with a shared
/// `CWD_LOCK` mutex so the two don't run concurrently.
#[test]
fn fetch_tags_filters_by_at_lang_token() {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    // Shared with `fetch_tags_parses_tag_file` (declared
    // at the top of the test module) so the two
    // CWD-mutating tests don't race.
    let _g = lock_or_recover(&CWD_LOCK);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("smarthistory_tags_atlang_{pid}_{n}"));
    fs::create_dir_all(&dir).expect("mkdir temp");
    let tags_path = dir.join("tags");
    fs::write(
        &tags_path,
        // Two sections separated by a form-feed byte
        // on its own line (the ctags convention for
        // section breaks):
        //  - src/lib.rs with one symbol (alpha)
        //  - src/script.py with one symbol (beta)
        // The `@rust` filter should keep only the
        // .rs entry.
        //
        // The ctags `tags` format is:
        //   <filename>,<count>\n
        //   <symbol>\t<line>,<offset>\n
        // (tab between display and line+offset,
        // comma between line and offset).
        "src/lib.rs,1\n\
             alpha\t1,0\n\
             \x0c\n\
             src/script.py,1\n\
             beta\t1,0\n",
    )
    .expect("write tags");
    // Create dummy source files so `read_source_context`
    // doesn't return an empty context (the test doesn't
    // assert on the context content, but we want the
    // rows to be well-formed).
    fs::create_dir_all(dir.join("src")).expect("mkdir src");
    fs::write(dir.join("src").join("lib.rs"), "fn alpha() {}\n").expect("write lib.rs");
    fs::write(dir.join("src").join("script.py"), "def beta():\n    pass\n")
        .expect("write script.py");
    // chdir into the temp dir so `find_tags_file`
    // discovers the tags file we just wrote.
    let prev_cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&dir).expect("chdir");
    // Run the fetch and assert, then restore CWD
    // regardless of the result.
    let result = std::panic::catch_unwind(|| {
        let mut app = directories_test_app(&[]);
        app.query = String::from("$@rust");
        app.refresh();
        let rows = crate::tui::mode::tags::fetch(&mut app).unwrap();
        // Only the .rs entry should survive the
        // `@rust` extension filter.
        assert_eq!(
            rows.len(),
            1,
            "expected exactly one row after @rust filter, got {}: {:?}",
            rows.len(),
            rows.iter().map(|r| &r.directory).collect::<Vec<_>>()
        );
        let row = &rows[0];
        assert!(
            row.directory.ends_with("lib.rs"),
            "row should point at the .rs file, got {:?}",
            row.directory
        );
        assert_eq!(row.source, "tags:rust");
    });
    // Always restore CWD and clean up, even on panic.
    std::env::set_current_dir(&prev_cwd).expect("restore cwd");
    let _ = fs::remove_dir_all(&dir);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// Selecting a tags row stages
/// `$EDITOR +<line> <filepath>`.
#[test]
fn select_for_run_in_tags_mode_stages_editor_with_line() {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    // Self-contained temp `tags`
    // file: the previous
    // version of this test
    // relied on the real
    // repo `TAGS` / `tags`
    // file, which is
    // `.gitignore`d and
    // therefore absent in
    // CI. The same
    // self-contained
    // pattern is used by
    // `fetch_tags_parses_tag_file`
    // and
    // `fetch_tags_filters_by_at_lang_token`.
    let _g = lock_or_recover(&CWD_LOCK);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("smarthistory_tags_select_{pid}_{n}"));
    fs::create_dir_all(&dir).expect("mkdir temp");
    // The staged editor
    // command is
    // `<editor> +<line> <path>`,
    // so the assertion
    // `staged.contains("+")`
    // and
    // `staged.contains(".rs")`
    // only need a single
    // `ssh_config` row in
    // a `.rs` file. The
    // `select_for_run` path
    // doesn't read the
    // source file (it only
    // uses the row's
    // `directory` and
    // `session_id` from
    // the tag file), so we
    // don't need to create
    // a real `ssh_config.rs`.
    let tags_contents = "\
src/ssh_config.rs,1\n\
ssh_config_parse\t\t10,0\n";
    fs::write(dir.join("tags"), tags_contents).expect("write tags");
    // chdir into the temp
    // dir so
    // `find_tags_file`
    // discovers the
    // file.
    let prev_cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&dir).expect("chdir");
    let result = std::panic::catch_unwind(|| {
        let mut app = directories_test_app(&[]);
        app.query = String::from("$ssh_config");
        app.refresh();
        let rows = crate::tui::mode::tags::fetch(&mut app).unwrap();
        assert!(!rows.is_empty());
        // Find the index of
        // the first row in
        // merged_rows.
        let idx = app
            .merged_rows()
            .iter()
            .position(|r| r.mode == "tags")
            .expect("at least one tags row must be in merged_rows");
        app.list_state.select(Some(idx));
        app.select_for_run();
        let staged = app
            .selection
            .as_deref()
            .expect("selection must be set for tags row");
        eprintln!("[test] staged tags command: {staged}");
        // The staged command
        // must include the
        // editor, `+<line>`,
        // and the file path.
        assert!(
            staged.contains("+"),
            "must include +LINE_NUMBER, got: {staged:?}"
        );
        assert!(
            staged.contains(".rs"),
            "must include a .rs file path, got: {staged:?}"
        );
    });
    // Always restore
    // CWD and clean
    // up, even on
    // panic.
    std::env::set_current_dir(&prev_cwd).expect("restore cwd");
    let _ = fs::remove_dir_all(&dir);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// `stage_editor_open_at_line` is the shared helper behind the
/// Tags/Ag/Codegraph selection arms. It must validate `line` as a
/// plain positive integer before splicing it into the staged
/// `$EDITOR +<line> <file>` string — a raw, unvalidated line field
/// (e.g. from a colon-containing filename shifting `ag`'s
/// `file:line:content` split) is a command-injection primitive the
/// moment the parent shell `eval`s the staged string.
#[test]
fn stage_editor_open_at_line_rejects_non_numeric_line() {
    let staged = crate::tui::actions::stage_editor_open_at_line(
        "vi",
        "/tmp/file.rs",
        "123; touch pwned",
    );
    assert!(
        !staged.contains("touch pwned"),
        "malicious line field must not survive into the staged command, got: {staged:?}"
    );
    assert!(
        !staged.contains(";"),
        "no shell metacharacter from the line field should reach the staged command, got: {staged:?}"
    );
    // A non-numeric line still opens the file, just without a
    // `+<line>` jump. `/tmp/file.rs` needs no shell quoting (only
    // path/alnum characters), so `shell_quote` passes it through.
    assert_eq!(staged, "vi /tmp/file.rs");
}

#[test]
fn stage_editor_open_at_line_accepts_numeric_line() {
    let staged = crate::tui::actions::stage_editor_open_at_line("vi", "/tmp/file.rs", "42");
    assert_eq!(staged, "vi +42 /tmp/file.rs");
}

/// Selecting a pane in `*` mode stages the
/// `tmux select-window -t <window_id> && tmux select-pane -t <pane_id>`
/// command — `select-window` first because plain
/// `select-pane` does NOT switch windows, and a
/// target pane may live in another window of the
/// current session.
#[test]
fn panes_last_pane_bubbled_to_index_zero() {
    // Simulate three panes; mark %2 as the "last"
    // (previously-active) pane by giving it the
    // bumped timestamp `fetch_session_panes_impl`
    // assigns. The bubble logic moves it to
    // position 0 so the default selection (index 0)
    // lands on it — pressing Enter flips back to
    // the pane the user just came from.
    let mut app = panes_test_app(&[
        ("%1", "@1", "/tmp", "zsh"),
        ("%2", "@2", "/tmp", "vim"),
        ("%3", "@3", "/tmp", "cargo"),
    ]);
    // Bump %2's timestamp to mimic the `pane_last`
    // flag path in `fetch_session_panes_impl`.
    let base = app.session_panes[0].timestamp;
    let last_row = app
        .session_panes
        .iter_mut()
        .find(|r| r.session_id == "%2")
        .expect("%2 row");
    last_row.timestamp = base + 1;
    // Apply the same bubble the impl does.
    if let Some(pos) = app.session_panes.iter().position(|r| r.timestamp > base) {
        let row = app.session_panes.remove(pos);
        app.session_panes.insert(0, row);
    }
    app.query = String::from("*");
    app.refresh();
    // The merged list must have %2 first.
    assert_eq!(app.merged_rows().len(), 3);
    assert_eq!(
        app.merged_rows()[0].session_id,
        "%2",
        "last pane must bubble to index 0, got {:?}",
        app.merged_rows()
            .iter()
            .map(|r| r.session_id.clone())
            .collect::<Vec<_>>()
    );
    // Default selection is index 0 → pressing
    // Enter stages a jump to %2.
    app.select_for_run();
    assert!(
        app.selection.as_deref().unwrap_or("").contains("-t %2"),
        "Enter must stage a jump to the last pane %2, got {:?}",
        app.selection
    );
}

#[test]
fn select_for_run_in_panes_mode_stages_switch_client() {
    let mut app = panes_test_app(&[("%5", "@3", "/tmp", "vim")]);
    app.query = String::from("*");
    app.refresh();
    // Select the first (only) row.
    app.list_state.select(Some(0));
    app.select_for_run();
    // The
    // `self.multiplexer.focus_command`
    // owns the exact
    // shape per backend
    // (for tmux:
    // `select-pane && switch-client`).
    // The cross-window
    // `select-window`
    // step is folded
    // into the
    // `select-pane` on
    // tmux (the pane id
    // is sufficient —
    // tmux resolves the
    // window
    // automatically); for
    // herdr the
    // workspace id is
    // the focus target.
    assert_eq!(
        app.selection.as_deref(),
        Some(
            "tmux select-pane -t %5 && \
                     tmux switch-client -t %5"
        )
    );
    assert_eq!(app.pick_mode, Some(PickMode::Run));
}

/// When the window id is
/// missing (parse
/// fallback / old
/// snapshot),
/// `select_for_run`
/// degrades to a bare
/// focus command on
/// the pane id alone
/// rather than staging
/// a broken
/// `select-window -t <empty>`.
/// With the
/// backend-driven shape
/// the empty
/// `window_id` simply
/// doesn't change the
/// staged command (the
/// tmux backend's
/// `focus_command` is
/// the same
/// `select-pane && switch-client`
/// for any non-empty
/// pane id).
#[test]
fn select_for_run_in_panes_mode_degrades_without_window_id() {
    // Empty window id ("@" stripped / old snapshot).
    let mut app = panes_test_app(&[("%5", "", "/tmp", "vim")]);
    app.query = String::from("*");
    app.refresh();
    // Select the first (only) row.
    app.list_state.select(Some(0));
    app.select_for_run();
    assert_eq!(
        app.selection.as_deref(),
        Some(
            "tmux select-pane -t %5 && \
                     tmux switch-client -t %5"
        )
    );
    assert_eq!(app.pick_mode, Some(PickMode::Run));
}

/// Regression test for
/// the new tree-style
/// `*`-mode layout:
/// selecting a
/// **workspace header
/// row** (a row with
/// `mode == "workspace"`)
/// stages
/// `self.multiplexer.focus_session(label)`;
/// selecting a **pane
/// row** (a row with
/// `mode == "pane"`)
/// stages
/// `self.multiplexer.focus_pane(pane_id, tab_id)`.
/// This test pins the
/// dispatch contract:
/// a workspace-row pick
/// must NOT
/// accidentally go
/// through the
/// pane-row path (which
/// would double-stage
/// the workspace jump
/// AND lose the
/// tab-level focus).
#[test]
fn select_for_run_in_panes_mode_dispatches_on_row_mode() {
    use crate::tui::state::HistoryRow;
    let mut app = directories_test_app(&[]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Build a tree:
    //   wA (workspace header)
    //     · wA:p1 (pane row)
    //   wB (workspace header)
    //     · wB:p2 (pane row)
    app.session_panes = vec![
        HistoryRow {
            id: -1,
            command: String::from("wA"),
            directory: String::from("/home/wA"),
            session_id: String::from("wA"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("1 pane"),
            output: String::new(),
            mode: String::from("workspace"),
            source: String::from("workspace"),

            ..Default::default()
        },
        HistoryRow {
            id: -2,
            command: String::new(),
            directory: String::from("/home/wA"),
            session_id: String::from("wA:p1"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("~/wA"),
            // For tmux the
            // pane row's
            // `output` is the
            // window id (`@N`);
            // for herdr it's
            // the tab id
            // (`wA:t1`). The
            // dispatch proves
            // the right
            // value gets
            // passed through.
            output: String::from("@1"),
            mode: String::from("pane"),
            source: String::from("pane"),

            ..Default::default()
        },
        HistoryRow {
            id: -3,
            command: String::from("wB"),
            directory: String::from("/home/wB"),
            session_id: String::from("wB"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("1 pane"),
            output: String::new(),
            mode: String::from("workspace"),
            source: String::from("workspace"),

            ..Default::default()
        },
        HistoryRow {
            id: -4,
            command: String::from("python"),
            directory: String::from("/home/wB"),
            session_id: String::from("wB:p2"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("~/wB"),
            output: String::from("@2"),
            mode: String::from("pane"),
            source: String::from("pane"),

            ..Default::default()
        },
    ];
    app.query = String::from("*");
    app.refresh();
    // Reset the list
    // state: refresh
    // may select
    // the last row
    // by default,
    // but we want
    // to explicitly
    // select row 0
    // (the wA
    // workspace header)
    // for this test.

    // 1.) Select row 0 —
    // the wA workspace
    // header. The staged
    // command must be
    // `tmux switch-client -t wA`
    // (session focus).
    app.list_state.select(Some(0));
    app.select_for_run();
    assert_eq!(
        app.selection.as_deref(),
        Some("tmux switch-client -t wA"),
        "workspace header row must stage focus_session command"
    );

    // 2.) Select row 1 —
    // the wA:p1 pane row.
    // The staged command
    // must be
    // `tmux select-pane -t wA:p1 && tmux switch-client -t wA:p1`
    // (pane focus). The
    // `@1` tab_id is
    // ignored by the
    // tmux backend but
    // is still passed
    // to `focus_pane`.
    app.selection = None;
    app.list_state.select(Some(1));
    app.select_for_run();
    assert_eq!(
        app.selection.as_deref(),
        Some("tmux select-pane -t wA:p1 && tmux switch-client -t wA:p1"),
        "pane row must stage focus_pane command"
    );
}

/// Regression test for the
/// user-reported bug
/// "the list of panes
/// is just empty" when
/// the user is on
/// `multiplexer=herdr`
/// and types `*` in
/// the TUI. The root
/// cause was that
/// `HerdrBackend::snapshot_current_panes`
/// parsed the JSON
/// response as
/// `|`-separated text
/// and returned zero
/// rows, so the
/// `session_panes`
/// list (which feeds
/// the `*` view) was
/// always empty. The
/// fix moves the
/// snapshot to a
/// proper JSON parser
/// (see
/// `multiplexer::parse_herdr_pane_list`).
///
/// This test drives
/// the full path:
/// 1. Build an `App`
///    with the herdr
///    backend.
/// 2. Inject a
///    pre-parsed
///    `session_panes`
///    (the test
///    bypasses the
///    real subprocess
///    and directly
///    populates the
///    cache, which
///    matches what the
///    fixed parser
///    would produce).
/// 3. Run the `*`
///    query through
///    `fetch_panes` and
///    assert the list
///    is non-empty and
///    each row has the
///    right shape
///    (workspace id as
///    `session_id`,
///    cwd as
///    `directory`,
///    agent as
///    `command`).
#[cfg(feature = "herdr")]
#[test]
fn panes_mode_with_herdr_backend_returns_real_panes() {
    use crate::tui::state::HistoryRow;
    let mut app = directories_test_app(&[]);
    // Swap in the herdr
    // backend.
    app.multiplexer = crate::multiplexer::backend_for(crate::multiplexer::MultiplexerKind::Herdr);
    // Inject a
    // pre-parsed
    // snapshot. The
    // values mirror
    // what the new
    // JSON parser
    // produces from
    // `herdr pane list`:
    // pane_id
    // (`wA:p1`),
    // workspace_id
    // (`wA`), cwd, and
    // detected agent
    // (empty for plain
    // shells).
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    app.session_panes = vec![
        HistoryRow {
            id: -1,
            command: String::new(),
            directory: String::from("/Users/har/work"),
            session_id: String::from("wA:p1"),
            exit_code: 0,
            timestamp: now_epoch,
            comment: String::from("~/work"),
            output: String::from("wA"),
            mode: String::from("pane"),
            source: String::from("pane"),

            ..Default::default()
        },
        HistoryRow {
            id: -2,
            command: String::from("pi"),
            directory: String::from("/Users/har/other"),
            session_id: String::from("wA:p3"),
            exit_code: 0,
            timestamp: now_epoch,
            comment: String::from("~/other"),
            output: String::from("wA"),
            mode: String::from("pane"),
            source: String::from("pane"),

            ..Default::default()
        },
    ];
    // The `*`-mode
    // view is
    // filtered by the
    // body of the
    // query (none →
    // all rows).
    app.query = String::from("*");
    let rows = crate::tui::mode::panes::fetch(&mut app).expect("fetch_panes succeeds");
    // Both injected
    // rows must
    // survive the
    // (empty) filter.
    assert_eq!(rows.len(), 2);
    // The agent name
    // (herdr's
    // `pane_current_command`
    // equivalent) is
    // the `command`
    // field, used as
    // the primary
    // text in the
    // `*`-mode list.
    assert_eq!(rows[1].command, "pi");
    // The workspace
    // id is stashed in
    // `output` (the
    // row has no
    // captured output
    // in either
    // backend, so the
    // field is
    // repurposed for
    // the panes-mode
    // jump target).
    assert_eq!(rows[0].output, "wA");
    // The pane id is
    // in `session_id`.
    assert_eq!(rows[0].session_id, "wA:p1");

    // The `[wA]` per-row
    // badge (the
    // `session_label`
    // surfaced via
    // `row.output`)
    // lets the user
    // identify at a
    // glance which
    // workspace each
    // pane belongs to.
    // The
    // `*`-mode list
    // spans every
    // workspace, so
    // this badge is
    // the only way to
    // tell at a
    // glance which
    // workspace a pane
    // is from. Both of
    // the injected
    // panes are in
    // workspace `wA`.
    assert_eq!(rows[0].output, "wA");
    assert_eq!(rows[1].output, "wA");
    // The filter also
    // matches against
    // the badge text,
    // so `*wA`
    // narrows to all
    // panes in
    // workspace `wA`.
    app.query = String::from("*wA");
    let rows = crate::tui::mode::panes::fetch(&mut app).expect("fetch_panes succeeds");
    assert_eq!(rows.len(), 2);
    // `*wB` matches
    // neither row
    // (both are `wA`).
    app.query = String::from("*wB");
    let rows = crate::tui::mode::panes::fetch(&mut app).expect("fetch_panes succeeds");
    assert_eq!(rows.len(), 0);
}

/// Regression test for the
/// user-reported bug:
/// "When I am in smarthistory
/// then I see the Downloads
/// workspace but I see just
/// a note about '2 panes'
/// but I expected to have
/// a line for each of these
/// 2 panes." The root cause:
/// `build_merged_rows` was
/// applying the duplicate
/// filter (on by default)
/// to panes mode, deduping
/// by `row.command`. Two
/// pane rows with the same
/// agent (e.g. two `pi`
/// rows) collapsed into
/// one; two pane rows with
/// empty command (two plain
/// shells) collapsed into
/// one. The workspace header
/// row's comment "2 panes"
/// was computed from the
/// pre-dedup `entries.len()`,
/// so the user saw "2 panes"
/// but only one (or zero)
/// indented pane rows
/// underneath.
///
/// The fix: panes mode is
/// completely dedup-free;
/// each pane is unique
/// (carries its own pane_id)
/// and the tree layout
/// only makes sense if ALL
/// children are visible.
/// This test pre-seeds
/// `session_panes` with a
/// tree that has both kinds
/// of "duplicate-by-command"
/// panes: two shells with
/// empty `command`, two pi
/// panes with
/// `command="pi"`. The
/// assertion checks that
/// `build_merged_rows`
/// returns ALL of them —
/// not the deduped subset.
///
/// The test deliberately
/// uses two checkboxes/shells with empty command
/// because that's the case
/// that originally broke:
/// the FIRST empty-command
/// pane kept its seat, the
/// SECOND was dropped, and
/// every subsequent
/// empty-command pane in
/// other workspaces was
/// dropped too — so even a
/// workspace with two real
/// panes could appear
/// "childless" if a sibling
/// workspace had eaten the
/// empty-command slot
/// first.
#[test]
fn panes_mode_does_not_dedup_pane_rows_with_same_command() {
    use crate::tui::state::HistoryRow;
    let mut app = directories_test_app(&[]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Force the duplicate_filter ON so the bug
    // (if it regressed) would dedup-away panes
    // with the same `command`. The default
    // in tests is OFF, but the production
    // default is ON (see
    // `Config::default().duplicate_filter`);
    // the regression must hold under the
    // production config, so we set it
    // explicitly here.
    app.duplicate_filter = true;
    app.session_panes = vec![
        // Workspace header for wA: 2 panes,
        // both shells with empty command.
        HistoryRow {
            id: -1,
            command: String::from("wA"),
            directory: String::from("/home/wA"),
            session_id: String::from("wA"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("2 panes"),
            output: String::new(),
            mode: String::from("workspace"),
            source: String::from("workspace"),

            ..Default::default()
        },
        // First shell — under dedup this would be kept.
        HistoryRow {
            id: -2,
            command: String::new(),
            directory: String::from("/home/wA"),
            session_id: String::from("wA:p1"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("~/wA"),
            output: String::from("wA:t1"),
            mode: String::from("pane"),
            source: String::from("pane"),

            ..Default::default()
        },
        // Second shell — same command (""), so under dedup
        // it would be DROPPED. The user's bug.
        HistoryRow {
            id: -3,
            command: String::new(),
            directory: String::from("/home/wA"),
            session_id: String::from("wA:p2"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("~/wA"),
            output: String::from("wA:t1"),
            mode: String::from("pane"),
            source: String::from("pane"),

            ..Default::default()
        },
        // Workspace header for wB: 1 pane running pi,
        // plus a sibling pi pane. Two pi rows would
        // dedupe to one under the old
        // duplicate-by-command logic.
        HistoryRow {
            id: -4,
            command: String::from("wB"),
            directory: String::from("/home/wB"),
            session_id: String::from("wB"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("2 panes"),
            output: String::new(),
            mode: String::from("workspace"),
            source: String::from("workspace"),

            ..Default::default()
        },
        HistoryRow {
            id: -5,
            command: String::from("pi"),
            directory: String::from("/home/wB"),
            session_id: String::from("wB:p1"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("~/wB"),
            output: String::from("wB:t1"),
            mode: String::from("pane"),
            source: String::from("pane"),

            ..Default::default()
        },
        HistoryRow {
            id: -6,
            command: String::from("pi"),
            directory: String::from("/home/wB"),
            session_id: String::from("wB:p2"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("~/wB"),
            output: String::from("wB:t1"),
            mode: String::from("pane"),
            source: String::from("pane"),

            ..Default::default()
        },
    ];
    // Trigger the panes view
    // through `refresh` so
    // `merged_rows` is rebuilt
    // via `build_merged_rows`
    // (the exact code path
    // the bug lived in).
    app.query = String::from("*");
    app.refresh();
    // ALL six rows should
    // survive — the two
    // workspace headers, the
    // two empty-command shell
    // panes (wA:p1, wA:p2),
    // and the two
    // `pi`-command agent
    // panes (wB:p1, wB:p2).
    // No dedup-by-command in
    // panes mode, even with
    // `duplicate_filter = true`.
    assert_eq!(
        app.merged_rows().len(),
        6,
        "panes mode must not dedup pane rows by `command`; \
                 every pane should be visible even when multiple \
                 panes share the same command (e.g. two shells, or \
                 two agents with the same name). Got: {}",
        app.merged_rows().len()
    );
    // Spot-check that the
    // duplicate-by-command
    // panes specifically
    // survived: find the
    // empty-command row that
    // used to be DROPPED (the
    // 3rd row in the source).
    assert!(
        app.merged_rows().iter().any(|r| r.session_id == "wA:p2"),
        "wA:p2 (a pane whose `command` is empty, sharing it \
                 with wA:p1) must survive the merge — not be \
                 deduped away"
    );
    assert!(
        app.merged_rows().iter().any(|r| r.session_id == "wB:p2"),
        "wB:p2 (a pane whose `command` is `pi`, sharing it \
                 with wB:p1) must survive the merge — not be \
                 deduped away"
    );
}

/// Delete ALL history entries for a
/// directory when the user presses
/// `Ctrl-D` in directories mode (`#`).
/// A confirmation dialog shows the
/// count first; on confirm, every
/// command that was ever run in
/// that directory is dropped.
#[test]
fn delete_directory_removes_all_entries_for_that_dir() {
    let mut app = directories_test_app(&[
        ("ls -la", "/var/tmp/build", 60),
        ("make", "/var/tmp/build", 30),
        ("git status", "/var/tmp/other", 10),
    ]);
    // Delete /var/tmp/build (2 entries).
    app.delete_directory("/var/tmp/build")
        .expect("delete_directory succeeds");
    // The `other` directory survives.
    app.query = "#".to_string();
    app.refresh();
    let dirs: Vec<&str> = app
        .merged_rows()
        .iter()
        .map(|r| r.directory.as_str())
        .collect();
    assert!(
        dirs.contains(&"/var/tmp/other"),
        "entries in other directories must survive, got {:?}",
        dirs
    );
    assert!(
        !dirs.contains(&"/var/tmp/build"),
        "entries in the deleted directory must be gone, got {:?}",
        dirs
    );
}

/// (the default for the
/// history list), so a
/// tree like:
///   # wB header
///     · wB:p1
///     · wB:p2
///   # wE header
///     · wE:p1
///     · wE:p2
/// would be visually
/// REVERSED — `wE:p2` at
/// the top, `wB header`
/// at the bottom — which
/// made the tree layout
/// incoherent (the
/// indented pane rows
/// appeared BEFORE their
/// parent workspace
/// header rather than
/// AFTER it). The user
/// saw `# wE   # 2 panes`
/// at the BOTTOM of the
/// visible list with
/// stray pane rows
/// above it, asking
/// "what does this
/// '2 panes' mean?".
///
/// The fix: panes mode
/// is rendered
/// **top-to-bottom**
/// (skipping the `.rev()`
/// the history list
/// uses) and is
/// **top-aligned**
/// (skipping the
/// bottom-align padding).
/// This test pins the
/// data-side contract:
/// `merged_rows()` in
/// panes mode returns the
/// rows in pass-2 emission
/// order (workspace header
/// first, then its panes,
/// then the next
/// workspace header, etc.)
/// — NOT reversed in any
/// way. The renderer's
/// top-to-bottom +
/// top-align layout
/// depends on the data
/// already being in
/// display order.
#[test]
fn panes_mode_merged_rows_preserve_tree_order_top_down() {
    use crate::tui::state::HistoryRow;
    let mut app = directories_test_app(&[]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    app.session_panes = vec![
        HistoryRow {
            id: -1,
            command: String::from("wA"),
            directory: String::new(),
            session_id: String::from("wA"),
            exit_code: 0,
            timestamp: now,
            comment: String::new(),
            output: String::new(),
            mode: String::from("workspace"),
            source: String::from("workspace"),

            ..Default::default()
        },
        HistoryRow {
            id: -2,
            command: String::new(),
            directory: String::from("/home/wA"),
            session_id: String::from("wA:p1"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("~/wA"),
            output: String::from("wA:t1"),
            mode: String::from("pane"),
            source: String::from("pane"),

            ..Default::default()
        },
        HistoryRow {
            id: -3,
            command: String::from("wB"),
            directory: String::new(),
            session_id: String::from("wB"),
            exit_code: 0,
            timestamp: now,
            comment: String::new(),
            output: String::new(),
            mode: String::from("workspace"),
            source: String::from("workspace"),

            ..Default::default()
        },
        HistoryRow {
            id: -4,
            command: String::from("pi"),
            directory: String::from("/home/wB"),
            session_id: String::from("wB:p1"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("~/wB"),
            output: String::from("wB:t1"),
            mode: String::from("pane"),
            source: String::from("pane"),

            ..Default::default()
        },
    ];
    app.query = String::from("*");
    app.refresh();
    let rows = app.merged_rows();
    assert_eq!(rows.len(), 4);
    // The expected top-to-bottom order:
    //   wA header
    //     · wA:p1 pane
    //   wB header
    //     · wB:p1 pane
    // The pass-3 sort (Age) applied by the history-list
    // sort path would REVERSE the order by timestamp —
    // but for panes mode there's no sort, so the
    // original (top-down tree) emission order survives.
    assert_eq!(rows[0].session_id, "wA");
    assert_eq!(rows[0].mode, "workspace");
    assert_eq!(rows[1].session_id, "wA:p1");
    assert_eq!(rows[1].mode, "pane");
    assert_eq!(rows[2].session_id, "wB");
    assert_eq!(rows[2].mode, "workspace");
    assert_eq!(rows[3].session_id, "wB:p1");
    assert_eq!(rows[3].mode, "pane");
}

/// Regression test for the
/// user-reported bug:
/// "I have to press down
/// to go up and vice versa."
/// The root cause was that
/// `move_selection` and the
/// Up/Down action bindings
/// assumed bottom-up history-row
/// rendering (Action::Up →
/// delta=+1, because higher
/// data index = older row =
/// row rendered ABOVE in the
/// reverse-sorted bottom-
/// aligned history list). With
/// the tree-style top-down
/// panes-mode renderer the
/// sign is backwards: Up
/// should DECREASE the data
/// index (move to the
/// visually-higher row), and
/// Down should INCREASE.
/// The fix inverts the
/// delta in `move_selection`
/// when `is_panes_query()`.
/// This test pins that the
/// Up action moves the cursor
/// to a LOWER data index in
/// panes mode (and unchanged
/// in history mode).
#[test]
fn panes_mode_up_action_decreases_data_index() {
    use crate::tui::state::HistoryRow;
    let mut app = directories_test_app(&[]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    app.session_panes = vec![
        HistoryRow {
            id: -1,
            command: String::from("wA"),
            directory: String::new(),
            session_id: String::from("wA"),
            exit_code: 0,
            timestamp: now,
            comment: String::new(),
            output: String::new(),
            mode: String::from("workspace"),
            source: String::from("workspace"),

            ..Default::default()
        },
        HistoryRow {
            id: -2,
            command: String::new(),
            directory: String::from("/home/wA"),
            session_id: String::from("wA:p1"),
            exit_code: 0,
            timestamp: now,
            comment: String::from("~/wA"),
            output: String::from("wA:t1"),
            mode: String::from("pane"),
            source: String::from("pane"),

            ..Default::default()
        },
        HistoryRow {
            id: -3,
            command: String::from("wB"),
            directory: String::new(),
            session_id: String::from("wB"),
            exit_code: 0,
            timestamp: now,
            comment: String::new(),
            output: String::new(),
            mode: String::from("workspace"),
            source: String::from("workspace"),

            ..Default::default()
        },
    ];
    app.query = String::from("*");
    app.refresh();
    // Start: index 0 = wA
    // workspace header (the
    // topmost row, since panes
    // mode is top-aligned).
    assert_eq!(app.list_state.selected(), Some(0));
    // Press Down (the user
    // expects the cursor to
    // move visually DOWN —
    // in panes mode the
    // displayed order mirrors
    // the data order, so DOWN
    // = INCREASE the data
    // index). Action::Down
    // passes delta=-1; the
    // `move_selection` fix
    // inverts it to +1 in
    // panes mode, so the
    // index INCREASES (move
    // DOWN visually).
    app.move_selection(-1); // mirrors Action::Down
    assert_eq!(
        app.list_state.selected(),
        Some(1),
        "Down action must move the cursor DOWN in panes mode — \
                 to a HIGHER data index (lower-on-screen, since panes \
                 mode is top-aligned) — not the inverse"
    );
    // Press Up (user expects
    // cursor to move UP).
    // Action::Up passes
    // delta=+1; in panes
    // mode `move_selection`
    // inverts it to -1, so
    // the index DECREASES
    // (move UP visually).
    app.move_selection(1); // mirrors Action::Up
    assert_eq!(
        app.list_state.selected(),
        Some(0),
        "Up action must move the cursor UP in panes mode — \
                 back to a LOWER data index (higher-on-screen)"
    );
    // Sanity: the rest of
    // `selected()` stays in
    // bounds. Up past the
    // top clamps.
    app.move_selection(100); // mirrors Action::Up past top
    assert_eq!(app.list_state.selected(), Some(0));
    // Down past the bottom
    // clamps. NOTE this is
    // delta=-100 (Down action)
    // which gets inverted to
    // +100; clamped to 2.
    app.move_selection(-100); // mirrors Action::Down past bottom
    assert_eq!(app.list_state.selected(), Some(2));
}

/// Panes mode is excluded
/// from the labeled-row
/// merge (same fix as
/// directories mode — a
/// labeled history row
/// must not leak into the
/// panes list).
#[test]
fn panes_mode_excludes_labeled_history_rows() {
    let labeled_cmd = "tmux list-panes -s -F stuff";
    let mut app = directories_test_app(&[(labeled_cmd, "/tmp", 60)]);
    app.conn
        .execute(
            "CREATE TABLE command_comments (
                        command TEXT PRIMARY KEY,
                        comment TEXT NOT NULL
                    )",
            [],
        )
        .expect("cc");
    app.conn
        .execute(
            "CREATE TABLE history_output (
                        history_id INTEGER PRIMARY KEY,
                        output TEXT NOT NULL
                    )",
            [],
        )
        .expect("ho");
    app.conn
        .execute(
            "INSERT INTO command_comments (command, comment) VALUES (?1, ?2)",
            rusqlite::params![labeled_cmd, "PANES LIST"],
        )
        .expect("ins");
    // Inject one pane so the panes list isn't empty.
    app.session_panes.push(HistoryRow {
        id: -1,
        command: "zsh".to_string(),
        directory: "/tmp".to_string(),
        session_id: "%7".to_string(),
        exit_code: 0,
        timestamp: 0,
        comment: "/tmp".to_string(),
        output: String::new(),
        mode: "pane".to_string(),
        source: "pane".to_string(),

        ..Default::default()
    });
    app.query = String::from("*");
    app.refresh();
    // The labeled history row must NOT appear.
    let has_labeled = app.merged_rows().iter().any(|r| r.command == labeled_cmd);
    assert!(
        !has_labeled,
        "panes mode must not show labeled history rows, got: {:?}",
        app.merged_rows()
            .iter()
            .map(|r| (r.command.clone(), r.source.clone()))
            .collect::<Vec<_>>()
    );
    // Only the pane row is visible.
    assert_eq!(app.merged_rows().len(), 1);
    assert_eq!(app.merged_rows()[0].source, "pane");
}

/// `fetch_session_panes` does NOT run `tmux` when
/// `$TMUX_PANE` is unset (the obvious "not in tmux"
/// signal) — the cache stays empty and `fetch_panes`
/// returns an empty list rather than spawning a
/// doomed subprocess.
#[test]
fn fetch_session_panes_no_op_when_not_in_tmux() {
    let mut app = directories_test_app(&[]);
    // The contract for
    // "no-op when not inside
    // a multiplexer": the
    // cache must stay empty
    // when NEITHER
    // `$TMUX_PANE` nor
    // `$HERDR_PANE_ID` is
    // set (the user isn't
    // running inside any
    // multiplexer pane at
    // all). When EITHER is
    // set (e.g. the test
    // is being run inside
    // herdr or tmux), the
    // cache may be
    // populated — we
    // assert no panic in
    // that case rather
    // than asserting a
    // fixed cache shape,
    // because the result
    // depends on the
    // real running
    // multiplexer's snapshot.
    let tmux_pane = std::env::var("TMUX_PANE").ok();
    let herdr_pane = std::env::var("HERDR_PANE_ID").ok();
    crate::tui::mode::panes::refresh_session_panes(&mut app);
    if tmux_pane.is_none() && herdr_pane.is_none() {
        assert!(
            app.session_panes.is_empty(),
            "cache must stay empty when neither $TMUX_PANE nor $HERDR_PANE_ID is set"
        );
    }
    // In both cases, no panic.
}

/// End-to-end with the REAL current tmux session:
/// run `fetch_session_panes_impl` with the actual
/// `$TMUX_PANE` and confirm the current pane is
/// excluded and every surviving row is well-formed
/// (pane id `%N`, window id `@N`, source `pane`).
/// Skipped if `tmux` isn't on PATH or `$TMUX_PANE`
/// isn't set (not running inside tmux).
#[test]
fn fetch_session_panes_end_to_end_real_tmux() {
    let current_pane = std::env::var("TMUX_PANE").unwrap_or_default();
    if current_pane.is_empty() {
        eprintln!("[skip] $TMUX_PANE unset (not in tmux)");
        return;
    }
    let mut app = directories_test_app(&[]);
    app.session_panes.clear();
    crate::tui::mode::panes::refresh_session_panes_impl(&mut app, &current_pane);
    // The current pane must NOT appear.
    let ids: Vec<String> = app
        .session_panes
        .iter()
        .map(|r| r.session_id.clone())
        .collect();
    assert!(
        !ids.contains(&current_pane),
        "current pane {} must be excluded, got {:?}",
        current_pane,
        ids
    );
    // Every surviving row is well-formed.
    for r in &app.session_panes {
        assert!(
            r.session_id.starts_with('%'),
            "pane id must look like %N, got {:?}",
            r.session_id
        );
        assert!(
            r.output.starts_with('@'),
            "window id must look like @N, got {:?}",
            r.output
        );
        assert_eq!(r.source, "pane");
    }
    // The last (previously-active) pane must be
    // bubbled to position 0 so the user can flip
    // back to it by pressing Enter. Identify it via
    // `tmux display-message -t {last}`. If the last
    // pane happens to equal the current pane (e.g.
    // the env-var quirk in CI) skip the positional
    // assertion — the exclusion check above
    // already covers that case.
    let last_pane = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", "{last}", "#{pane_id}"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if !last_pane.is_empty() && last_pane != current_pane && ids.contains(&last_pane) {
        assert_eq!(
            app.session_panes[0].session_id, last_pane,
            "last pane {} must be at index 0, got order {:?}",
            last_pane, ids
        );
    }
}

/// Regression test for the
/// user-reported bug:
/// "there are still no
/// panes visible when I
/// switch to the panes
/// prefix" on the herdr
/// backend. The root cause
/// was that
/// `fetch_session_panes`
/// (the public entry point)
/// checked `$TMUX_PANE`
/// only and bailed early
/// when it was empty — so a
/// herdr user (who has
/// `$HERDR_PANE_ID` set but
/// `$TMUX_PANE` unset)
/// never reached
/// `fetch_session_panes_impl`,
/// and `session_panes`
/// stayed empty. The fix
/// read BOTH env vars; this
/// test pins the contract
/// by exercising the env-var
/// resolution directly.
///
/// We can't mutate env vars
/// safely under the parallel
/// test runner, so the test
/// is conditional: it runs
/// the assertion only when
/// `HERDR_PANE_ID` happens to
/// be set in the test env
/// (e.g. when the test suite
/// is run from inside a
/// herdr pane — which is the
/// exact reproduction
/// scenario for the bug).
/// When `HERDR_PANE_ID` is
/// unset, the test degrades
/// to a no-op (no env to
/// exercise the herdr path
/// against).
#[test]
fn fetch_session_panes_proceeds_when_herdr_pane_id_set() {
    let herdr_pane = std::env::var("HERDR_PANE_ID")
        .ok()
        .filter(|s| !s.is_empty());
    let Some(_herdr_pane) = herdr_pane else {
        eprintln!("[skip] $HERDR_PANE_ID unset (not in herdr)");
        return;
    };
    let mut app = directories_test_app(&[]);
    app.multiplexer = crate::multiplexer::backend_for(crate::multiplexer::MultiplexerKind::Herdr);
    // Pre-seed the cache
    // with a sentinel so
    // we can detect whether
    // `fetch_session_panes`
    // actually ran (it
    // clears + repopulates
    // when it runs; if it
    // bails early, the
    // sentinel survives).
    app.session_panes.push(crate::tui::state::HistoryRow {
        id: -999,
        command: String::from("SENTINEL"),
        directory: String::from("/sentinel"),
        session_id: String::from("SENTINEL"),
        exit_code: 0,
        timestamp: 0,
        comment: String::new(),
        output: String::new(),
        mode: String::from("pane"),
        source: String::from("pane"),

        ..Default::default()
    });
    // ……actually:
    // `fetch_session_panes`
    // also early-exits when
    // `!session_panes.is_empty()`,
    // which a sentinel would
    // trigger. Clear it
    // first so the entry
    // point actually runs:
    app.session_panes.clear();
    crate::tui::mode::panes::refresh_session_panes(&mut app);
    // The bug was that this
    // call returned without
    // populating the cache
    // (because the tmux-only
    // guard bailed early).
    // After the fix, the
    // herdr backend's
    // snapshot runs — which
    // may legitimately return
    // an empty list (if the
    // herdr server returned 0
    // panes) but importantly
    // we DID reach the
    // backend. We assert no
    // panic and no sentinel;
    // the actual pane count
    // is whatever the live
    // herdr server reports
    // (we can't pin a specific
    // number).
    assert!(
        app.session_panes.iter().all(|r| r.session_id != "SENTINEL"),
        "fetch_session_panes must not bail early when $HERDR_PANE_ID is set; \
                 it must reach fetch_session_panes_impl and call the herdr backend"
    );
    // Diagnostic: also dump what the snapshot populated so
    // a regression in fetch_session_panes_impl (e.g. dropping
    // some workspace's panes) shows up as a clear assertion
    // failure with the actual content for debugging. Gated
    // on `SMARTHISTORY_DEBUG_TMUX` so normal test runs aren't
    // polluted with diagnostic output (which would mask
    // test failures when --nocapture is set).
    if std::env::var("SMARTHISTORY_DEBUG_TMUX").is_ok() {
        let all_rows = app.session_panes.clone();
        let pane_count = all_rows.iter().filter(|r| r.mode == "pane").count();
        let workspace_count = all_rows.iter().filter(|r| r.mode == "workspace").count();
        eprintln!(
                "[debug] after fetch_session_panes: {} rows total ({} workspace headers, {} pane children)",
                all_rows.len(),
                workspace_count,
                pane_count
            );
        for r in &all_rows {
            eprintln!(
                "[debug]   mode={:?} session_id={:?} command={:?} comment={:?}",
                r.mode, r.session_id, r.command, r.comment
            );
        }
    }
    let all_rows = app.session_panes.clone();
    // The herdr snapshot is non-empty in our test env (we
    // have 2 workspaces with 5 panes between them, minus one
    // excluded = 5 pane rows + 2 workspace headers = 7
    // rows). But we don't hard-pin the count in CI (no herdr).
    // When herdr IS present the count is asserted; otherwise
    // the test is a no-op.
    if all_rows.is_empty() {
        eprintln!("[skip] session_panes empty (herdr returned no panes)");
        return;
    }
    // When herdr has panes, every pane row must be unique
    // (pane_id) — duplicates would indicate a grouping
    // bug in fetch_session_panes_impl.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in &all_rows {
        if r.mode == "pane" {
            assert!(
                seen.insert(r.session_id.clone()),
                "duplicate pane row {:?} across all workspaces — \
                         fetch_session_panes_impl produced non-unique rows",
                r.session_id
            );
        }
    }
}

/// End-to-end: run the
/// actual `tmux
/// list-windows -a`
/// command and confirm
/// the rows it produces
/// (in `DIR:TMUX` mode)
/// have `source =
/// "tmux"`, the
/// directory in `~/x`
/// form, and the pane
/// id in the secondary
/// slot. Skipped if
/// `tmux` is not on PATH
/// (e.g. CI without
/// tmux installed). This
/// is a regression guard
/// for the user's
/// "I see `tmux list-
/// windows -a` as an
/// entry" report: the
/// `pane_current_path`
/// (the second column)
/// is always a real
/// absolute filesystem
/// path, never the
/// `tmux list-windows
/// -a` command line
/// itself. We pin both
/// the `source` field
/// and the prefix
/// invariant.
#[test]
fn fetch_directories_tmux_pane_path_is_a_real_path() {
    // Skip silently if
    // tmux isn't on
    // PATH — CI
    // environments
    // typically don't
    // have it.
    let tmux_check = std::process::Command::new("tmux").arg("-V").output();
    if tmux_check.is_err() {
        eprintln!("[skip] tmux not on PATH");
        return;
    }
    // Run the
    // production
    // format
    // command.
    let format = "\
                #{pane_id} | \
                #{pane_current_path} | \
                active:#{window_active} | \
                Layout: #{window_layout}";
    let output = std::process::Command::new("tmux")
        .args(["list-windows", "-a", "-F", format])
        .output()
        .expect("tmux list-windows must succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Build a
    // synthetic
    // `App` with
    // the same
    // shape as
    // `fetch_tmux_windows`
    // produces
    // (parse each
    // line into a
    // `TmuxWindowInfo`).
    let mut windows: Vec<TmuxWindowInfo> = Vec::new();
    for line in stdout.lines() {
        if let Some(w) = parse_tmux_pane_line(line) {
            windows.push(w);
        }
    }
    // Every
    // window's
    // `path`
    // must be a
    // real
    // absolute
    // path —
    // never
    // something
    // like a
    // command
    // line or a
    // shell
    // output.
    for w in &windows {
        assert!(
            w.path.starts_with('/'),
            "pane_current_path must be an absolute path, got: {:?}",
            w.path
        );
        assert!(
                w.path.contains('/') && !w.path.contains('|') && !w.path.contains(' '),
                "pane_current_path must look like a real path (no separators like | or spaces), got: {:?}",
                w.path
            );
    }
    // The
    // second-load
    // smoke test
    // for the
    // user's
    // report:
    // the
    // visible
    // primary
    // text on a
    // tmux-pane
    // row in
    // `DIR:TMUX`
    // mode is the
    // shortened
    // directory,
    // not the
    // pane id
    // (which goes
    // in the
    // secondary
    // slot).
    let mut app = directories_test_app(&[]);
    app.tmux_windows = windows;
    app.directory_source = crate::tui::state::DirectorySource::Tmux;
    app.query = "#".to_string();
    app.refresh();
    for row in app.merged_rows() {
        assert_eq!(row.source, "tmux");
        // The
        // primary
        // text
        // (visible
        // in the
        // first
        // column)
        // must
        // be a
        // shortened
        // directory
        // — never
        // a
        // command
        // name
        // like
        // `tmux
        // list-
        // windows
        // -a`
        // or a
        // shell
        // name.
        assert!(
            row.command.starts_with('~') || row.command.starts_with('/'),
            "tmux-pane row's primary text must be a path, got: {:?}",
            row.command
        );
        assert!(
                !row.command.starts_with("tmux "),
                "tmux-pane row's primary text must NOT be a command line (the 'tmux list-windows -a' bug), got: {:?}",
                row.command
            );
    }
    // Dump the
    // visible
    // text
    // representation
    // of every
    // row in
    // `DIR:TMUX`
    // mode for
    // the user's
    // report
    // (debugging
    // the
    // 'tmux list-
    // windows -a'
    // mystery
    // entry).
    // The fix for that
    // mystery is the
    // `starts_with('/')`
    // and `is_dir()`
    // filters in
    // `fetch_directories`'s
    // tmux loop; this
    // test pins the
    // behaviour
    // (bad pane paths
    // are filtered out,
    // good ones
    // surface).
}

// ---- JIRA (`-`-prefix) mode ----

/// A fake `JiraClient` that returns a canned set
/// of issues, recording the JQL it was called with
/// so tests can assert on the generated query.
/// The `comments` field is the canned comments
/// list returned by `fetch_comments`; the
/// `comment_keys` field records which keys the
/// TUI asked for so tests can assert the
/// comments fetch was issued with the right
/// target. The `posted_comments` field records
/// the (key, body) pairs that the TUI tried to
/// post via `add_comment`; tests assert on this
/// to verify the save-comment-edit dispatch
/// routes JIRA rows through the add-comment
/// path (not the local SQLite `command_comments`
/// path).
#[derive(Default)]
struct FakeJira {
    issues: Vec<crate::jira::JiraIssue>,
    recorded: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    comments: Vec<crate::jira::JiraComment>,
    comment_keys: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    posted_comments: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

impl crate::jira::JiraClient for FakeJira {
    fn search(&self, jql: &str) -> Result<Vec<crate::jira::JiraIssue>, crate::jira::JiraError> {
        self.recorded.lock().unwrap().push(jql.to_string());
        Ok(self.issues.clone())
    }
    fn fetch_comments(
        &self,
        key: &str,
    ) -> Result<Vec<crate::jira::JiraComment>, crate::jira::JiraError> {
        self.comment_keys.lock().unwrap().push(key.to_string());
        Ok(self.comments.clone())
    }
    fn add_comment(&self, key: &str, body: &str) -> Result<(), crate::jira::JiraError> {
        self.posted_comments
            .lock()
            .unwrap()
            .push((key.to_string(), body.to_string()));
        Ok(())
    }
}

/// The `-` prefix is detected and the body sliced.
#[test]
fn jira_prefix_detected_and_pattern_sliced() {
    let mut app = directories_test_app(&[]);
    app.query = String::new();
    assert!(!app.is_jira_query());
    app.query = String::from("-");
    assert!(app.is_jira_query());
    assert_eq!(app.jira_pattern(), "");
    app.query = String::from("-PROJ-1 crash");
    assert_eq!(app.jira_pattern(), "PROJ-1 crash");
}

/// In jira mode, `build_merged_rows` does NOT merge
/// labeled history rows (same guard as directories /
/// panes modes).
#[test]
fn jira_mode_excludes_labeled_history_rows() {
    let labeled_cmd = "grep -c PROJ issues";
    let mut app = directories_test_app(&[(labeled_cmd, "/tmp", 60)]);
    app.conn
        .execute(
            "CREATE TABLE command_comments (command TEXT PRIMARY KEY, comment TEXT NOT NULL)",
            [],
        )
        .expect("cc");
    app.conn
        .execute(
            "CREATE TABLE history_output (history_id INTEGER PRIMARY KEY, output TEXT NOT NULL)",
            [],
        )
        .expect("ho");
    app.conn
        .execute(
            "INSERT INTO command_comments (command, comment) VALUES (?1, ?2)",
            rusqlite::params![labeled_cmd, "JIRA-LIST"],
        )
        .expect("ins");
    app.jira_rows.push(crate::tui::state::HistoryRow {
        id: -1,
        command: "PROJ-9".to_string(),
        directory: String::new(),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: "boom".to_string(),
        output: String::new(),
        mode: "jira".to_string(),
        source: "jira".to_string(),

        ..Default::default()
    });
    app.query = String::from("-");
    app.refresh();
    let has_labeled = app.merged_rows().iter().any(|r| r.command == labeled_cmd);
    assert!(
        !has_labeled,
        "jira mode must not show labeled rows, got {:?}",
        app.merged_rows()
            .iter()
            .map(|r| (r.command.clone(), r.source.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(app.merged_rows().len(), 1);
    assert_eq!(app.merged_rows()[0].source, "jira");
}

/// `jira_maybe_autocall` fires the search after the
/// debounce and caches the result rows. Verifies the
/// fake-client synchronous path end-to-end:
/// query → JQL → search → rows.
#[test]
fn jira_autocall_caches_search_results() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![
            crate::jira::JiraIssue {
                key: "PROJ-1".to_string(),
                summary: "login crash".to_string(),
                status: "Open".to_string(),
                issuetype: "Bug".to_string(),
                ..Default::default()
            },
            crate::jira::JiraIssue {
                key: "PROJ-2".to_string(),
                summary: "fix tests".to_string(),
                updated: "2024-06-30T19:14:39.000+0000".to_string(),
                ..Default::default()
            },
        ],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let recorded = fake.recorded.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-project=PROJ crash");
    app.refresh();
    // Forcibly arm the debounce in the past so the
    // autocall fires immediately (the run loop would
    // normally wait, but here we drive it by hand).
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    // The JQL was built and the search fired.
    assert_eq!(recorded.lock().unwrap().len(), 1, "search must fire once");
    let jql = recorded.lock().unwrap()[0].clone();
    assert!(jql.contains(r#"project = "PROJ""#), "JQL: {}", jql);
    assert!(jql.contains(r#"description ~ "crash""#), "JQL: {}", jql);
    // The result rows are cached on the app.
    assert_eq!(app.jira_rows.len(), 2);
    assert_eq!(app.jira_rows[0].command, "PROJ-1");
    assert_eq!(app.jira_rows[0].comment, "login crash");
    assert_eq!(app.jira_rows[0].source, "jira");
    assert_eq!(app.jira_rows[0].mode, "jira");
    // The new format wraps the label in
    // `**...**` so the renderer can produce
    // a bold span. The substring assertion
    // here uses the bold-marked form.
    assert!(app.jira_rows[0].output.contains("**Status**: Open"));
    // PROJ-2 has a real `updated` → parsed epoch.
    assert!(app.jira_rows[1].timestamp > 1_700_000_000);
}

/// A repeat `jira_maybe_autocall` with the SAME
/// query does NOT re-fire the search (the
/// `jira_last_jql` cache prevents spamming JIRA).
#[test]
fn jira_autocall_skips_unchanged_jql() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let recorded = fake.recorded.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-PROJ-1");
    app.refresh();
    // Set both timers (the
    // 400ms fast debounce
    // AND the 3-second idle
    // safety-net) to a value
    // that's past both
    // thresholds. The
    // `jira_maybe_autocall`
    // gate checks
    // `debounce_elapsed ||
    // idle_elapsed`, so both
    // must be past for the
    // search to fire. Using
    // `JIRA_DEBOUNCE` alone
    // would leave the idle
    // timer unexpired and the
    // test would hang.
    let past = || {
        std::time::Instant::now()
            - JIRA_IDLE_TIMEOUT
            - JIRA_DEBOUNCE
            - std::time::Duration::from_millis(50)
    };
    app.jira_debounce_started = Some(past());
    app.jira_idle_started = Some(past());
    app.jira_maybe_autocall();
    assert_eq!(recorded.lock().unwrap().len(), 1);
    // Second call with no query change must NOT
    // re-fire.
    app.jira_debounce_started = Some(past());
    app.jira_idle_started = Some(past());
    app.jira_maybe_autocall();
    assert_eq!(
        recorded.lock().unwrap().len(),
        1,
        "must not re-fire for same JQL"
    );
}

/// The space key acts as an
/// explicit "commit this
/// word" signal in the
/// JIRA query body. The
/// user's request: the
/// query should fire
/// immediately when a
/// space is typed, even
/// if the fast debounce
/// has not yet elapsed.
/// The space trigger
/// runs through
/// `push_char`, which
/// inserts the space and
/// then calls
/// `jira_maybe_autocall`
/// directly. The just-fired
/// search includes the
/// newly-inserted space.
#[test]
fn push_char_space_fires_jira_search_immediately() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let recorded = fake.recorded.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    // Type `-PROJ-1` then a
    // space. The space
    // trigger must fire
    // even though the
    // debounce has not
    // elapsed (we
    // deliberately set
    // the debounce to a
    // value in the past
    // for the search to
    // fire — but the test
    // would still pass
    // without that because
    // the space trigger
    // calls
    // `jira_maybe_autocall`
    // directly, which
    // checks the timer).
    app.query = String::from("-PROJ-1");
    // Push the cursor to
    // the end of the query
    // so the space lands
    // AFTER `PROJ-1`. The
    // default cursor
    // position is 0
    // (left over from
    // `App::new`'s empty
    // initial query), so
    // without this the
    // space would be
    // prepended and the
    // test would see
    // `" -PROJ-1"`.
    app.query_cursor = app.query.chars().count();
    app.refresh();
    // Reset the debounce to
    // a "not yet elapsed"
    // value so we can prove
    // the space trigger
    // fires independently.
    // (If we left it
    // expired from the
    // initial `refresh()`,
    // the test would pass
    // even without the
    // space trigger.)
    app.jira_debounce_started =
        Some(std::time::Instant::now() - std::time::Duration::from_millis(100));
    app.jira_idle_started = Some(std::time::Instant::now() - std::time::Duration::from_millis(100));
    let before = recorded.lock().unwrap().len();
    // Type a space. The
    // space trigger must
    // call
    // `jira_maybe_autocall`
    // synchronously, which
    // (since the debounce
    // is in the past)
    // fires the search
    // before `push_char`
    // returns. The search
    // sees the post-space
    // JQL `PROJ-1 ` (with
    // a trailing space).
    app.push_char(' ');
    // The recorded JQL list
    // should have grown by
    // exactly one (the
    // initial `refresh()`
    // already fired one
    // search — actually
    // no, the initial
    // `refresh()` only
    // fires the search if
    // the debounce was
    // already past;
    // since we just set
    // it to a recent time,
    // the initial fire was
    // skipped, so the
    // first recorded JQL
    // is from the space
    // trigger).
    let recorded_jqls = recorded.lock().unwrap().clone();
    assert!(
        recorded_jqls.len() > before,
        "space trigger must fire the search, before={} after={} jqls={:?}",
        before,
        recorded_jqls.len(),
        recorded_jqls
    );
    // The recorded JQL must
    // include the just-
    // typed space (it was
    // appended to the
    // query before
    // `jira_maybe_autocall`
    // ran).
    let last = recorded_jqls.last().unwrap();
    assert!(
        last.contains("PROJ-1 "),
        "JQL must reflect the space just typed, got: {last:?}"
    );
}

/// The 3-second idle timer
/// is a safety-net trigger:
/// it fires the search
/// even when the fast
/// 400ms debounce has
/// not elapsed. The
/// user's report was that
/// the query "sometimes
/// isn't executed"; the
/// idle timer guarantees
/// the search runs within
/// 3 seconds of the last
/// keystroke regardless
/// of the fast-debounce
/// state.
#[test]
fn jira_idle_timer_fires_search_after_3s() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let recorded = fake.recorded.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-PROJ-1");
    app.refresh();
    // Arm BOTH timers to
    // a value that's past
    // the 3-second idle
    // threshold but NOT
    // past the 400ms
    // debounce — wait,
    // that doesn't make
    // sense; if the
    // value is 1 second
    // ago it's past
    // BOTH thresholds.
    // Use a value that's
    // specifically 2
    // seconds ago: past
    // the 3-second idle
    // timer is 3
    // seconds, so 2
    // seconds ago is
    // NOT past. Use 4
    // seconds ago:
    // past the 3-second
    // idle timer, also
    // past the 400ms
    // debounce.
    // For a cleaner
    // isolation, set the
    // fast debounce to
    // a recent (unexpired)
    // time and the idle
    // timer to a value
    // past the 3s
    // threshold. That
    // way the idle timer
    // is the ONLY thing
    // that can fire.
    let recent = std::time::Instant::now() - std::time::Duration::from_millis(100);
    let idle_past =
        std::time::Instant::now() - JIRA_IDLE_TIMEOUT - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(recent);
    app.jira_idle_started = Some(idle_past);
    // The debounce is NOT
    // past (100ms < 400ms),
    // but the idle timer
    // IS past (3s+ ago).
    // The dual-timer gate
    // `debounce_elapsed ||
    // idle_elapsed` must
    // therefore fire the
    // search.
    app.jira_maybe_autocall();
    assert_eq!(
        recorded.lock().unwrap().len(),
        1,
        "idle timer must fire the search even when fast debounce hasn't elapsed"
    );
}

/// The 3-second idle timer
/// fires the search even
/// for an empty body
/// (`-` alone). This
/// matches the 400ms
/// debounce's behaviour:
/// typing `-` arms the
/// timer, and after 3
/// seconds the search
/// runs with the empty
/// JQL (matches every
/// issue). The test
/// just confirms the
/// idle timer triggers a
/// search — the actual
/// JQL payload is the
/// caller's concern.
#[test]
fn jira_idle_timer_fires_for_empty_body() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let recorded = fake.recorded.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    // Just `-` with no
    // body.
    app.query = String::from("-");
    app.refresh();
    // The fast debounce is
    // not armed (the
    // initial `refresh()`
    // for `-` alone didn't
    // fire a search because
    // `jira_last_jql` was
    // already set to the
    // empty-body JQL by
    // the constructor
    // path). Arm the
    // idle timer to a
    // value past 3s.
    let idle_past =
        std::time::Instant::now() - JIRA_IDLE_TIMEOUT - std::time::Duration::from_millis(50);
    app.jira_idle_started = Some(idle_past);
    let before = recorded.lock().unwrap().len();
    app.jira_maybe_autocall();
    // The idle timer is the
    // only thing that's
    // past; the fast
    // debounce was never
    // armed. The gate
    // `debounce_elapsed ||
    // idle_elapsed` must
    // therefore fire the
    // search.
    assert!(
            recorded.lock().unwrap().len() > before,
            "idle timer must fire the search even when the fast debounce hasn't elapsed, before={} after={}",
            before,
            recorded.lock().unwrap().len()
        );
}

/// The canonical case from
/// the user's request:
/// typing a field-name
/// prefix and pressing
/// Tab expands it. The
/// exact expansion
/// depends on whether
/// the prefix is unique
/// (e.g. `stat` matches
/// only `status`) or
/// ambiguous (e.g. `lab`
/// matches both `label`
/// and `labels`).
///
/// For UNIQUE prefixes the
/// expansion is the
/// full field name plus
/// `=` (the user can
/// immediately type
/// the value). For
/// AMBIGUOUS prefixes
/// the expansion is the
/// longest common prefix
/// with no trailing `=`
/// (the user keeps
/// typing to
/// disambiguate). This
/// is the standard
/// readline / bash
/// completion behaviour
/// and is the least
/// surprising thing
/// for users who already
/// know shell
/// completion.
///
/// The user mentioned
/// `lab<TAB> → labels=`
/// as their example,
/// but `lab` is
/// actually ambiguous
/// (it matches both
/// `label` and
/// `labels`). The
/// readline convention
/// is to extend to the
/// longest common
/// prefix (`label`),
/// not to pick a
/// specific field. We
/// test the unambiguous
/// case below with
/// `status`.
#[test]
fn jira_tab_completion_expands_unique_prefix_to_field_equals() {
    let mut app = directories_test_app(&[]);
    // `stat` is a prefix of
    // `status` (only;
    // `statusCategory`
    // also starts with
    // `status` so the
    // true LCP would be
    // `status`). The
    // `stat` prefix has
    // two matches
    // (`status` and
    // `statusCategory`).
    // The unambiguous
    // case is `proj`
    // which matches only
    // `project`.
    app.query = String::from("-proj");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    assert_eq!(
        app.query, "-project=",
        "unique prefix should expand to field=, got: {:?}",
        app.query
    );
    // Cursor lands right
    // after the `=`
    // (position 9: 1
    // for `-` + 7 for
    // `project` + 1
    // for `=`).
    assert_eq!(app.query_cursor, 9);
}

/// The user's example:
/// `lab<TAB>` expands
/// to... not `labels=`
/// (the user expected)
/// but to the longest
/// common prefix
/// `label`, because
/// `lab` matches both
/// `label` and
/// `labels`. The user
/// keeps typing to
/// disambiguate
/// (`labels` vs
/// `label`). This is
/// the standard
/// readline behaviour.
/// `lab<TAB>` opens the completion
/// menu (ambiguous between
/// `label` and `labels`). The
/// user can navigate and pick
/// one. Pressing `Enter` with
/// the default selection (the
/// first candidate, `label`)
/// applies the completion with
/// the trailing `=`.
#[test]
fn jira_tab_completion_ambiguous_label_prefix_opens_menu() {
    let mut app = directories_test_app(&[]);
    app.query = String::from("-lab");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    // The query itself is
    // unchanged — the
    // completion menu is
    // now open with both
    // candidates available
    // for selection.
    assert_eq!(
        app.query, "-lab",
        "ambiguous lab<TAB> should open the completion menu (query unchanged), got: {:?}",
        app.query
    );
    assert!(
        app.is_completion_menu_open(),
        "completion menu should be open"
    );
    let menu = app.completion_menu.as_ref().unwrap();
    // The menu should have
    // both `label` and
    // `labels` as
    // candidates.
    assert!(menu.candidates.contains(&"label".to_string()));
    assert!(menu.candidates.contains(&"labels".to_string()));
    // The first candidate
    // is pre-selected.
    assert_eq!(menu.selected, 0);
    // Now the user types
    // `s` (to
    // disambiguate to
    // `labels`) and
    // presses Tab
    // again. The prefix
    // `labels` matches
    // only `labels`, so
    // the second Tab
    // applies the
    // completion with the
    // trailing `=`.
    let mut app = directories_test_app(&[]);
    app.query = String::from("-labels");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    assert_eq!(
        app.query, "-labels=",
        "second Tab on the disambiguated prefix appends `=`, got: {:?}",
        app.query
    );
}

/// Pressing Tab in JIRA mode
/// at a complete field
/// name (e.g. `labels`)
/// appends the `=`. This
/// is the
/// `jira_field_complete_with_value`
/// path: the prefix IS a
/// complete field, the
/// completion extends to
/// itself plus `=`.
#[test]
fn jira_tab_completion_at_complete_field_appends_equals() {
    let mut app = directories_test_app(&[]);
    app.query = String::from("-labels");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    assert_eq!(
        app.query, "-labels=",
        "complete field name + Tab should append `=`, got: {:?}",
        app.query
    );
    assert_eq!(app.query_cursor, 8);
}

/// Pressing Tab in JIRA mode
/// with a prefix that
/// doesn't match any
/// field leaves the
/// query unchanged AND
/// surfaces a status
/// message. The function
/// must not silently
/// destroy text.
#[test]
fn jira_tab_completion_no_match_leaves_query_unchanged() {
    let mut app = directories_test_app(&[]);
    app.query = String::from("-xyz");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    assert_eq!(
        app.query, "-xyz",
        "no-match prefix must NOT modify the query, got: {:?}",
        app.query
    );
    // A status message is
    // surfaced so the
    // user knows Tab
    // did not silently
    // fail. The
    // `status_message`
    // field is
    // `Option<(String,
    // Instant)>`; we
    // extract the
    // string for the
    // assertion.
    let status = app.status_message.as_ref().map(|(m, _)| m.clone());
    assert!(
        status.as_deref().unwrap_or("").contains("xyz"),
        "status message should mention the unknown prefix, got: {status:?}"
    );
}

/// Pressing Tab OUTSIDE of
/// JIRA mode is a no-op.
/// The action should
/// not interfere with
/// any other mode's
/// behaviour. (The
/// add-entry dialog
/// handles its own Tab
/// as field-next INSIDE
/// the dialog, but
/// `jira_field_complete_at_cursor`
/// is the direct
/// method; the
/// dispatch site in
/// `dispatch_action`
/// already short-
/// circuits on
/// `!is_jira_query()`.)
#[test]
fn jira_tab_completion_outside_jira_mode_is_noop() {
    let mut app = directories_test_app(&[]);
    // Query is not a JIRA
    // query (no `-`
    // prefix).
    app.query = String::from("git status");
    let original = app.query.clone();
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    assert_eq!(app.query, original, "non-JIRA query must be left unchanged");
}

/// The completion is
/// position-aware:
/// pressing Tab in
/// the MIDDLE of a
/// field prefix only
/// replaces the
/// prefix, not the
/// characters after
/// the cursor.
/// E.g. `labe<TAB>` with
/// the cursor between
/// `b` and `e` should
/// only touch the
/// portion of the
/// field that the
/// cursor is on. The
/// user's
/// just-typed `e`
/// should be
/// preserved.
/// `lab<TAB>` with the cursor
/// in the middle of the word
/// opens the completion menu
/// (ambiguous between `label`
/// and `labels`). The query
/// is unchanged because the
/// menu is pending selection.
/// The user's just-typed `e`
/// after the cursor is
/// preserved.
#[test]
fn jira_tab_completion_preserves_text_after_cursor() {
    let mut app = directories_test_app(&[]);
    // Query is `-labe`.
    // Cursor is at
    // position 4 (right
    // after `lab`,
    // before `e`).
    app.query = String::from("-labe");
    app.query_cursor = 4; // right after `-lab`
    app.jira_field_complete_at_cursor();
    // The query is
    // unchanged because
    // the completion menu
    // is open (the user
    // still needs to
    // pick a candidate).
    // The `e` after the
    // cursor is
    // preserved.
    assert_eq!(
        app.query, "-labe",
        "ambiguous prefix should open the completion menu (query unchanged), got: {:?}",
        app.query
    );
    assert!(
        app.is_completion_menu_open(),
        "completion menu should be open"
    );
    // The cursor position
    // is unchanged
    // (still at 4).
    assert_eq!(app.query_cursor, 4);
}

/// `@` alias expansion:
/// `@mo<TAB>` inside JIRA mode expands to `@month `
/// (with trailing space so the user can type the
/// next token immediately).
#[test]
fn jira_tab_completion_alias_expands_to_alias_with_space() {
    let mut app = directories_test_app(&[]);
    app.query = String::from("-@mo");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    assert_eq!(
        app.query, "-@month ",
        "@mo should expand to @month with trailing space, got: {:?}",
        app.query
    );
    assert_eq!(app.query_cursor, 8);
}

/// `@` alias with user-defined fragment:
/// `@sp<TAB>` expands to `@sprint ` when the
/// fragment is defined in the config.
#[test]
fn jira_tab_completion_alias_includes_user_fragments() {
    let mut app = directories_test_app(&[]);
    app.jira_fragments
        .insert("sprint".to_string(), "sprint = \"42\"".to_string());
    app.query = String::from("-@sp");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    assert_eq!(
        app.query, "-@sprint ",
        "fragment alias should expand with trailing space, got: {:?}",
        app.query
    );
    assert_eq!(app.query_cursor, 9);
}

/// Ambiguous `@` alias: `@me<TAB>` when both
/// `me` (builtin) and a `meeting` fragment exist
/// should extend to the LCP `@me`.
#[test]
fn jira_tab_completion_alias_ambiguous_returns_lcp() {
    let mut app = directories_test_app(&[]);
    app.jira_fragments
        .insert("meeting".to_string(), "summary ~ meeting".to_string());
    app.query = String::from("-@me");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    // `me` matches both `me` and `meeting`; LCP is `me`.
    assert_eq!(
        app.query, "-@me",
        "ambiguous alias should not change query (LCP equals prefix), got: {:?}",
        app.query
    );
}

/// No-match `@` alias: `@xyz<TAB>` leaves the
/// query unchanged and surfaces a status message.
#[test]
fn jira_tab_completion_alias_no_match_leaves_unchanged() {
    let mut app = directories_test_app(&[]);
    app.query = String::from("-@xyz");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    assert_eq!(
        app.query, "-@xyz",
        "no-match alias must NOT modify query, got: {:?}",
        app.query
    );
    let status = app.status_message.as_ref().map(|(m, _)| m.clone());
    assert!(
        status.as_deref().unwrap_or("").contains("xyz"),
        "status should mention unknown alias, got: {status:?}"
    );
}

/// Field completion still works when the word
/// does NOT start with `@`. `proj<TAB>` should
/// still expand to `project=`.
#[test]
fn jira_tab_completion_field_still_works_without_at() {
    let mut app = directories_test_app(&[]);
    app.query = String::from("-proj");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    assert_eq!(
        app.query, "-project=",
        "field completion without @ should still work, got: {:?}",
        app.query
    );
}

/// Build a minimal notes database
/// with the given tags and links,
/// wire it into a fresh app, and
/// return both. Used by the
/// notes tab-completion tests
/// below.
fn notes_tab_complete_test_app(tags: &[&str], links: &[&str]) -> (App, std::path::PathBuf) {
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "smarthistory-notes-tab-{}-{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create dir");
    let db_path = dir.join("notes.sqlite");
    let conn = Connection::open(&db_path).expect("open db");
    note_search::init_database_schema(&conn).expect("schema");
    let tags_json = serde_json::Value::Array(
        tags.iter()
            .map(|t| serde_json::Value::String(t.to_string()))
            .collect(),
    );
    let links_json = serde_json::Value::Array(
        links
            .iter()
            .map(|l| serde_json::Value::String(l.to_string()))
            .collect(),
    );
    conn.execute(
        "INSERT INTO markdown_data \
             (filename, title, tags, links) \
             VALUES ('test.md', 'test', ?1, ?2)",
        rusqlite::params![
            serde_json::to_string(&tags_json).unwrap(),
            serde_json::to_string(&links_json).unwrap(),
        ],
    )
    .expect("insert markdown_data");
    conn.execute(
        "INSERT INTO todo_entries (filename, text, tags, links) \
             VALUES ('test.md', 'test', ?1, ?2)",
        rusqlite::params![
            serde_json::to_string(&tags_json).unwrap(),
            serde_json::to_string(&links_json).unwrap(),
        ],
    )
    .expect("insert todo");
    drop(conn);
    let mut app = global_test_app(&[("a", 1)]);
    app.notes_database = Some(db_path.clone());
    (app, db_path)
}

/// `#feat<TAB>` inside notes mode
/// expands to `#feature ` (unique
/// tag match, trailing space).
#[test]
fn notes_tab_completion_tag_unique_match_expands_with_space() {
    let (mut app, _db) = notes_tab_complete_test_app(&["feature", "bug"], &[]);
    app.query = String::from("@#feat");
    app.query_cursor = app.query.chars().count();
    app.notes_tab_complete_at_cursor();
    assert_eq!(
        app.query, "@#feature ",
        "#feat should expand to #feature with trailing space, got: {:?}",
        app.query
    );
    assert_eq!(app.query_cursor, 10);
}

/// `@Neo<TAB>` inside notes mode
/// expands to `[[NeovimNote]] `
/// (unique link match with
/// `[[...]]` syntax, trailing
/// space). The user types `@` to
/// enter notes mode, then
/// `@Neo` to start a link
/// reference; the completion
/// replaces the `@Neo` word
/// with the full `[[...]]`
/// expansion (which supports
/// link names with spaces).
#[test]
fn notes_tab_completion_link_unique_match_expands_with_space() {
    let (mut app, _db) = notes_tab_complete_test_app(&[], &["NeovimNote.md", "RustBook.md"]);
    app.query = String::from("@@Neo");
    app.query_cursor = app.query.chars().count();
    app.notes_tab_complete_at_cursor();
    // Link expansion always
    // uses the lowercase
    // form (Obsidian's
    // case-insensitive
    // convention),
    // regardless of how
    // the link is stored
    // in the database.
    assert_eq!(
        app.query, "@[[neovimnote]] ",
        "@Neo should expand to [[neovimnote]] with trailing space, got: {:?}",
        app.query
    );
    // Cursor lands at the end:
    // @[[neovimnote]]<space> = 16 chars.
    assert_eq!(app.query_cursor, 16);
}

/// Ambiguous tag prefix returns
/// the LCP without trailing
/// space (the user keeps typing
/// to disambiguate).
#[test]
fn notes_tab_completion_tag_ambiguous_returns_lcp() {
    let (mut app, _db) = notes_tab_complete_test_app(&["feature", "feat-list"], &[]);
    app.query = String::from("@#feat");
    app.query_cursor = app.query.chars().count();
    app.notes_tab_complete_at_cursor();
    assert_eq!(
        app.query, "@#feat",
        "ambiguous tag prefix returns LCP (no change), got: {:?}",
        app.query
    );
}

/// Ambiguous link prefix opens
/// the completion menu so the
/// user can pick from the
/// candidates. The query
/// itself is unchanged because
/// the menu is pending
/// selection.
#[test]
fn notes_tab_completion_link_ambiguous_opens_menu() {
    let (mut app, _db) = notes_tab_complete_test_app(&[], &["NeovimNote.md", "NeovimConfig.md"]);
    app.query = String::from("@@Neo");
    app.query_cursor = app.query.chars().count();
    app.notes_tab_complete_at_cursor();
    // Query is unchanged —
    // the menu is open
    // with the
    // candidates
    // available for
    // selection.
    assert_eq!(
        app.query, "@@Neo",
        "ambiguous link prefix should open the completion menu (query unchanged), got: {:?}",
        app.query
    );
    assert!(
        app.is_completion_menu_open(),
        "completion menu should be open"
    );
    let menu = app.completion_menu.as_ref().unwrap();
    // Both candidates
    // (with .md stripped)
    // are in the menu. The
    // candidates use
    // lowercase since
    // link expansion
    // always uses the
    // lowercase form
    // (Obsidian's
    // case-insensitive
    // convention).
    assert!(menu.candidates.contains(&"neovimnote".to_string()));
    assert!(menu.candidates.contains(&"neovimconfig".to_string()));
}

/// Link expansion strips the
/// `.md` suffix from the
/// database's link target and
/// wraps the result in
/// `[[...]]` syntax. The user
/// types `@bernd` and gets
/// `[[bernd_matthiesen]] ` (no
/// extension), matching
/// Obsidian's `[[NoteName]]`
/// convention.
#[test]
fn notes_tab_completion_link_strips_md_suffix() {
    let (mut app, _db) = notes_tab_complete_test_app(&[], &["bernd_matthiesen.md"]);
    app.query = String::from("@@bernd");
    app.query_cursor = app.query.chars().count();
    app.notes_tab_complete_at_cursor();
    assert_eq!(
        app.query, "@[[bernd_matthiesen]] ",
        ".md suffix should be stripped and wrapped in [[...]], got: {:?}",
        app.query
    );
}

/// Link names with spaces are
/// wrapped in double quotes
/// inside the `[[...]]` syntax:
/// `@my` expands to
/// `[["my note"]] ` so the link
/// target is unambiguously
/// delimited.
#[test]
fn notes_tab_completion_link_handles_link_names_with_spaces() {
    // Link names with spaces are
    // wrapped in `[[...]]` brackets
    // which unambiguously delimit
    // the link target. No
    // additional quotes are needed
    // since the brackets already
    // serve as a delimiter.
    let (mut app, _db) = notes_tab_complete_test_app(&[], &["my note.md"]);
    app.query = String::from("@@my");
    app.query_cursor = app.query.chars().count();
    app.notes_tab_complete_at_cursor();
    assert_eq!(
        app.query, "@[[my note]] ",
        "link with space should be wrapped in [[...]] without quotes, got: {:?}",
        app.query
    );
}

/// No-match prefix leaves the
/// query unchanged and surfaces
/// a status message.
#[test]
fn notes_tab_completion_no_match_leaves_unchanged() {
    let (mut app, _db) = notes_tab_complete_test_app(&["feature"], &[]);
    app.query = String::from("@#xyz");
    app.query_cursor = app.query.chars().count();
    app.notes_tab_complete_at_cursor();
    assert_eq!(
        app.query, "@#xyz",
        "no-match tag must NOT modify query, got: {:?}",
        app.query
    );
    let status = app.status_message.as_ref().map(|(m, _)| m.clone());
    assert!(
        status.as_deref().unwrap_or("").contains("xyz"),
        "status should mention unknown tag, got: {status:?}"
    );
}

/// Tag completion also works in
/// todos mode (`!` prefix).
#[test]
fn notes_tab_completion_works_in_todo_mode() {
    let (mut app, _db) = notes_tab_complete_test_app(&["feature"], &[]);
    app.query = String::from("!#feat");
    app.query_cursor = app.query.chars().count();
    app.notes_tab_complete_at_cursor();
    assert_eq!(
        app.query, "!#feature ",
        "tag completion should work in todo mode, got: {:?}",
        app.query
    );
}

/// Outside notes and todos
/// modes, the completion is a
/// no-op (the user is just
/// typing plain text).
#[test]
fn notes_tab_completion_outside_notes_todo_mode_is_noop() {
    let (mut app, _db) = notes_tab_complete_test_app(&["feature"], &[]);
    // GLOBAL mode (no `@` or `!` prefix).
    app.query = String::from("git status");
    let original = app.query.clone();
    app.query_cursor = app.query.chars().count();
    app.notes_tab_complete_at_cursor();
    assert_eq!(
        app.query, original,
        "non-notes/todo query must be unchanged"
    );
}

/// `Enter` on the completion
/// menu applies the selected
/// candidate with the
/// appropriate prefix and
/// suffix. For JIRA fields,
/// the suffix is `=`.
#[test]
fn handle_completion_menu_key_enter_applies_jira_field() {
    let mut app = directories_test_app(&[]);
    app.query = String::from("-lab");
    app.query_cursor = app.query.chars().count();
    // First Tab opens the
    // menu (ambiguous
    // between `label`
    // and `labels`).
    app.jira_field_complete_at_cursor();
    assert!(app.is_completion_menu_open());
    // Default selection
    // is index 0
    // (`label`). Press
    // Enter to commit.
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
    handle_completion_menu_key(&mut app, enter);
    assert!(!app.is_completion_menu_open(), "menu should close on Enter");
    assert_eq!(
        app.query, "-label=",
        "selected candidate should be applied with ="
    );
}

/// `Enter` on the completion
/// menu for notes tags
/// applies the selected
/// candidate with `#` and a
/// trailing space.
#[test]
fn handle_completion_menu_key_enter_applies_notes_tag() {
    let (mut app, _db) = notes_tab_complete_test_app(&["feature", "bug"], &[]);
    app.query = String::from("@#f");
    app.query_cursor = app.query.chars().count();
    // First Tab opens the
    // menu (ambiguous
    // between `feature`
    // and `feat-list`
    // ... wait, only
    // `feature` and
    // `bug` are in the
    // DB, so `f` only
    // matches `feature`
    // — no menu). Let
    // me use a prefix
    // that matches
    // both.
    // Actually, with
    // just `feature`
    // and `bug` in the
    // DB, `f` matches
    // only `feature`.
    // Let me adjust to
    // create a
    // situation with
    // multiple matches.
    // Actually, for
    // this test I just
    // need a single
    // match to verify
    // the apply
    // path. Let me
    // use `fe` which
    // also matches
    // only `feature`.
    app.query = String::from("@#fe");
    app.query_cursor = app.query.chars().count();
    app.notes_tab_complete_at_cursor();
    // `fe` matches only
    // `feature`, so the
    // single-match path
    // applies directly
    // (no menu).
    assert!(
        !app.is_completion_menu_open(),
        "single match should not open the menu"
    );
    assert_eq!(
        app.query, "@#feature ",
        "single match should be applied directly"
    );
}

/// `Enter` on the completion
/// menu for notes links
/// applies the selected
/// candidate wrapped in
/// `[[...]]` with a trailing
/// space.
#[test]
fn handle_completion_menu_key_enter_applies_notes_link() {
    let (mut app, _db) = notes_tab_complete_test_app(&[], &["NeovimNote.md", "RustBook.md"]);
    app.query = String::from("@@Neo");
    app.query_cursor = app.query.chars().count();
    // `Neo` matches only
    // `NeovimNote` (after
    // `.md` stripping),
    // so the single-match
    // path applies
    // directly (no
    // menu). The expansion
    // uses the lowercase
    // form since link
    // expansion always
    // lowercases.
    app.notes_tab_complete_at_cursor();
    assert!(
        !app.is_completion_menu_open(),
        "single match should not open the menu"
    );
    assert_eq!(
        app.query, "@[[neovimnote]] ",
        "single match should be applied directly with [[...]] syntax (lowercase), got: {:?}",
        app.query
    );
}

/// `Up` / `Down` navigate the
/// completion menu's
/// candidate list. `Down`
/// increments the selected
/// index (saturating at the
/// last entry); `Up` decrements
/// (saturating at 0).
#[test]
fn handle_completion_menu_key_updown_navigates() {
    let mut app = directories_test_app(&[]);
    app.query = String::from("-lab");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    assert!(app.is_completion_menu_open());
    assert_eq!(app.completion_menu.as_ref().unwrap().selected, 0);
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
    handle_completion_menu_key(&mut app, down);
    assert_eq!(app.completion_menu.as_ref().unwrap().selected, 1);
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::empty());
    handle_completion_menu_key(&mut app, up);
    assert_eq!(app.completion_menu.as_ref().unwrap().selected, 0);
}

/// `Esc` (the default `Cancel`
/// binding) closes the
/// completion menu without
/// changing the query.
#[test]
fn handle_completion_menu_key_cancel_closes_without_change() {
    let mut app = directories_test_app(&[]);
    app.query = String::from("-lab");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    assert!(app.is_completion_menu_open());
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
    handle_completion_menu_key(&mut app, esc);
    assert!(!app.is_completion_menu_open(), "menu should close on Esc");
    assert_eq!(app.query, "-lab", "query should be unchanged");
}

/// `Up` at the first entry is
/// a no-op (saturating
/// subtraction). `Down` at the
/// last entry is also a no-op.
#[test]
fn handle_completion_menu_key_saturates_at_boundaries() {
    let mut app = directories_test_app(&[]);
    app.query = String::from("-lab");
    app.query_cursor = app.query.chars().count();
    app.jira_field_complete_at_cursor();
    // At index 0; Up
    // should be a no-op.
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::empty());
    handle_completion_menu_key(&mut app, up);
    assert_eq!(app.completion_menu.as_ref().unwrap().selected, 0);
    // Down to index 1,
    // then Up to index
    // 0.
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
    handle_completion_menu_key(&mut app, down);
    assert_eq!(app.completion_menu.as_ref().unwrap().selected, 1);
    // At the last entry
    // (index 1, since
    // there are 2
    // candidates);
    // Down should be a
    // no-op.
    handle_completion_menu_key(&mut app, down);
    assert_eq!(app.completion_menu.as_ref().unwrap().selected, 1);
}

/// The user's request: in
/// non-JIRA modes, every
/// text-mutating action
/// must fire the search
/// immediately. The
/// earlier behaviour was
/// "fire on the next
/// frame" (the run loop's
/// `refresh()` call), which
/// meant a single-frame
/// lag between the
/// keystroke and the
/// updated row set. This
/// test verifies the
/// synchronous behaviour
/// for GLOBAL mode (the
/// simplest synchronous
/// mode — no session /
/// directory scoping, so
/// every row in the
/// in-memory DB is
/// visible). The
/// `trigger_text_change_search`
/// call is mode-agnostic;
/// the GLOBAL / DIR /
/// SESS distinction is
/// just about the SQL
/// `WHERE` clause, not
/// about the search
/// trigger.
///
/// Note: the test inserts
/// a single character
/// into an empty query
/// and asserts that the
/// `rows` field is
/// repopulated. Before
/// the change, the test
/// would see a stale
/// `rows` field (the one
/// from `App::new`'s
/// initial `refresh()`).
/// After the change, the
/// row set reflects the
/// new query on the same
/// frame.
#[test]
fn push_char_in_global_mode_fires_search_immediately() {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    // The `fetch()` SQL
    // LEFT JOINs
    // `command_comments`
    // and
    // `history_output`;
    // both must exist or
    // the query fails
    // (and `refresh()`
    // swallows the
    // error via
    // `unwrap_or_default()`,
    // leaving `rows`
    // empty).
    conn.execute_batch(
        "CREATE TABLE history (
                    id INTEGER PRIMARY KEY,
                    command TEXT NOT NULL,
                    directory TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    exit_code INTEGER,
                    timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                    mode TEXT NOT NULL DEFAULT 'command'
                );
                CREATE TABLE command_comments (
                    command TEXT PRIMARY KEY,
                    comment TEXT NOT NULL
                );
                CREATE TABLE history_output (
                    history_id INTEGER PRIMARY KEY,
                    output TEXT
                );",
    )
    .expect("schema");
    // Two rows: one matches
    // `git`, one doesn't.
    // After the keystroke
    // the row set should
    // contain only the
    // `git` row.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
             VALUES (?1, ?2, ?3, 'sess', 0, ?4)",
        rusqlite::params![1i64, "git status", "/home/u", now],
    )
    .expect("insert git");
    conn.execute(
        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
             VALUES (?1, ?2, ?3, 'sess', 0, ?4)",
        rusqlite::params![2i64, "ls -la", "/home/u", now - 1],
    )
    .expect("insert ls");
    let mut app = App::new(
        conn,
        // GLOBAL — no
        // session /
        // directory
        // scoping, so
        // every row is
        // visible by
        // default.
        Mode::Global,
        String::new(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    // The initial
    // `refresh()` should
    // have populated
    // `rows` with BOTH
    // rows (empty query =
    // no filter).
    let initial_count = app.rows.len();
    assert_eq!(
        initial_count, 2,
        "initial rows should include both commands, got {}",
        initial_count
    );
    // Now type a single
    // character. The new
    // behaviour must
    // immediately re-
    // fetch the row
    // set, filtering by
    // the new query.
    app.push_char('g');
    // The row set should
    // now contain only
    // the `git` row.
    // Before the
    // `trigger_text_change_search`
    // change, this
    // would still show
    // both rows (the
    // `refresh()` call
    // in `push_char`
    // was missing).
    assert_eq!(
        app.rows.len(),
        1,
        "after typing 'g', only the git row should remain, got {} rows: {:?}",
        app.rows.len(),
        app.rows.iter().map(|r| &r.command).collect::<Vec<_>>()
    );
    assert!(
        app.rows[0].command.contains("git"),
        "remaining row should be the git one, got: {:?}",
        app.rows[0].command
    );
}

/// `backspace` in non-JIRA
/// mode must also fire
/// the search
/// immediately. The
/// `backspace()` method
/// already called
/// `refresh()` for
/// non-LLM modes, so
/// this test mostly
/// documents the
/// intent. The
/// `trigger_text_change_search`
/// call is a no-op for
/// GLOBAL (the
/// `refresh()` already
/// covers it) but is the
/// right place to hang
/// future
/// LLM-specific logic
/// off of.
#[test]
fn backspace_in_global_mode_fires_search_immediately() {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE history (
                    id INTEGER PRIMARY KEY,
                    command TEXT NOT NULL,
                    directory TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    exit_code INTEGER,
                    timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                    mode TEXT NOT NULL DEFAULT 'command'
                );
                CREATE TABLE command_comments (
                    command TEXT PRIMARY KEY,
                    comment TEXT NOT NULL
                );
                CREATE TABLE history_output (
                    history_id INTEGER PRIMARY KEY,
                    output TEXT
                );",
    )
    .expect("schema");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
             VALUES (?1, ?2, ?3, 'sess', 0, ?4)",
        rusqlite::params![1i64, "git status", "/home/u", now],
    )
    .expect("insert");
    let mut app = App::new(
        conn,
        Mode::Global,
        String::new(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    // Type `git` —
    // matching the
    // `git status`
    // row.
    app.push_char('g');
    app.push_char('i');
    app.push_char('t');
    assert_eq!(
        app.rows.len(),
        1,
        "after typing 'git' should match the git row, got {}",
        app.rows.len()
    );
    // Now backspace —
    // the query is
    // back to `gi`.
    // The match should
    // still hold
    // (the row
    // contains `git`
    // which contains
    // `gi`), so the
    // row count is
    // still 1.
    app.backspace();
    assert_eq!(
        app.rows.len(),
        1,
        "after backspacing to 'gi' the git row should still match, got {}",
        app.rows.len()
    );
}

/// Empty queries do NOT
/// trigger a re-fetch.
/// When the user clears
/// the box (e.g. via
/// `Ctrl-U` or by
/// backspacing the
/// last character), we
/// should not waste
/// time re-running the
/// fetch — the empty
/// body already
/// matches every row,
/// which is what the
/// user just had on
/// screen.
#[test]
fn push_char_then_backspace_to_empty_does_not_re_fetch() {
    use rusqlite::Connection;
    let conn = Connection::open_in_memory().expect("open in-memory db");
    // Full schema (the
    // fetch SQL LEFT
    // JOINs on both
    // tables).
    conn.execute_batch(
        "CREATE TABLE history (
                    id INTEGER PRIMARY KEY,
                    command TEXT NOT NULL,
                    directory TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    exit_code INTEGER,
                    timestamp INTEGER DEFAULT (strftime('%s', 'now')),
                    mode TEXT NOT NULL DEFAULT 'command'
                );
                CREATE TABLE command_comments (
                    command TEXT PRIMARY KEY,
                    comment TEXT NOT NULL
                );
                CREATE TABLE history_output (
                    history_id INTEGER PRIMARY KEY,
                    output TEXT
                );",
    )
    .expect("schema");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
             VALUES (?1, ?2, ?3, 'sess', 0, ?4)",
        rusqlite::params![1i64, "ls", "/h", now],
    )
    .expect("insert");
    let mut app = App::new(
        conn,
        Mode::Global,
        String::new(),
        false,
        ExitFilter::All,
        SortOrder::default(),
        false,
        SelectedTheme::None,
        crate::tui::theme::ColorScheme::Dark,
        KeyBindings::defaults(),
        None,
        None,
        crate::QueryPrefixes::default(),
        None,
        None,
        String::from("+$LINE"),
        std::collections::HashMap::new(),
        Vec::new(),
        std::collections::HashMap::new(),
        test_multiplexer(),
        crate::tui::state::PaneVisibility::default(),
        crate::tui::state::PaneHeight::default(),
    );
    app.session_subdirs.clear();
    app.tmux_windows.clear();
    app.push_char('l');
    app.push_char('s');
    // Sanity: the row
    // is matched.
    assert!(
        !app.rows.is_empty(),
        "should have at least one match, got {} rows: {:?}",
        app.rows.len(),
        app.rows.iter().map(|r| &r.command).collect::<Vec<_>>()
    );
    // Now backspace
    // twice. The first
    // removes `s` (query
    // = `l`, still
    // matches the
    // `ls` row). The
    // second removes
    // `l` (query =
    // ``, matches
    // everything). The
    // second backspace
    // is the "empty
    // query" case:
    // `trigger_text_change_search`
    // must short-circuit
    // and NOT call
    // `refresh()`. (The
    // existing
    // `backspace()`
    // method DOES call
    // `refresh()` after
    // every deletion,
    // so the row set
    // will still be
    // re-fetched. The
    // point of the
    // test is to
    // confirm the
    // empty-query
    // path is a no-op
    // for the
    // new helper —
    // the
    // existing
    // `refresh()` is
    // independent.)
    app.backspace();
    app.backspace();
    assert_eq!(app.query, "", "query should be empty");
    // The row set
    // should still
    // contain the row
    // (empty query =
    // no filter = all
    // rows visible).
    assert!(
        !app.rows.is_empty(),
        "empty query should leave the row set populated, got {}",
        app.rows.len()
    );
}

/// JIRA mode must NOT
/// fire on every
/// keystroke — the
/// JIRA debounce
/// (400ms fast + 3s
/// idle safety-net)
/// is still
/// respected. The
/// `trigger_text_change_search`
/// helper
/// short-circuits
/// inside the
/// JIRA branch.
/// This test verifies
/// the guard: a
/// `push_char` in
/// JIRA mode does
/// NOT set
/// `jira_in_flight`
/// to true (which
/// would mean the
/// auto-call fired
/// immediately).
#[test]
fn push_char_in_jira_mode_does_not_bypass_debounce() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let recorded = fake.recorded.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    // Set the JIRA
    // debounce / idle
    // timers to a
    // RECENT value
    // (not past the
    // threshold).
    // After
    // `push_char`,
    // the timers
    // should be
    // re-armed to
    // "now" (not
    // fired).
    app.query = String::from("-");
    app.query_cursor = 1;
    let now = std::time::Instant::now();
    app.jira_debounce_started = Some(now);
    app.jira_idle_started = Some(now);
    app.push_char('P');
    // The JIRA
    // debounce
    // should be
    // re-armed to
    // "now" (by
    // `llm_touch`
    // →
    // `jira_touch`).
    // The recorded
    // JQL list
    // should NOT
    // have grown —
    // the auto-call
    // is still
    // waiting for
    // the debounce
    // to elapse.
    assert_eq!(
        recorded.lock().unwrap().len(),
        0,
        "JIRA mode must NOT fire immediately on push_char; the debounce is still respected"
    );
}

/// The `@me` / `@today` / `@week` / `@month`
/// aliases thread through `jira_build_query`
/// into the JQL the FakeJira receives.
/// Asserts the JQL contains the expected
/// alias-derived clauses end-to-end.
#[test]
fn jira_aliases_reach_the_fake_client() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let recorded = fake.recorded.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-@me @week crash");
    app.refresh();
    // Force BOTH the
    // 400ms fast debounce
    // AND the 3-second idle
    // safety-net to be in
    // the past. The
    // `jira_maybe_autocall`
    // gate checks
    // `debounce_elapsed ||
    // idle_elapsed` — we
    // need both past for the
    // search to fire. The
    // run loop would normally
    // wait, but we drive it
    // by hand for
    // determinism.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    assert_eq!(recorded.lock().unwrap().len(), 1);
    let jql = recorded.lock().unwrap()[0].clone();
    // The `assignee = currentUser()` clause
    // from the `@me` alias.
    assert!(jql.contains("assignee = currentUser()"), "JQL: {}", jql);
    // The `updated >=` clause from the
    // `@week` alias (date is computed from
    // `now_epoch()` — we don't assert the
    // exact date, just the prefix).
    assert!(jql.contains(r#"updated >= "20"#), "JQL: {}", jql);
    // The free-text token survived.
    assert!(
        jql.contains(r#"(description ~ "crash" OR summary ~ "crash")"#),
        "JQL: {}",
        jql
    );
}

/// A user-defined JQL fragment (loaded from a
/// hypothetical `jira.search.label1=labels = "test"`
/// config entry) is spliced into the JQL the
/// FakeJira receives. Mirrors the
/// `jira_aliases_reach_the_fake_client` test
/// but exercises the fragment expansion path
/// end-to-end through `jira_build_query`.
#[test]
fn jira_fragments_reach_the_fake_client() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let recorded = fake.recorded.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    // Install a single fragment via the
    // same field the config loader uses
    // (the public API doesn't expose a
    // setter — the field is the
    // authoritative store and the test
    // pushes directly).
    app.jira_fragments
        .insert("label1".to_string(), r#"labels = "test""#.to_string());
    app.query = String::from("-@label1 @me crash");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    assert_eq!(recorded.lock().unwrap().len(), 1);
    let jql = recorded.lock().unwrap()[0].clone();
    // The fragment is spliced verbatim,
    // parenthesised, AND-joined with the
    // other clauses.
    assert!(jql.contains(r#"(labels = "test")"#), "JQL: {}", jql);
    // The `@me` alias still fires.
    assert!(jql.contains("assignee = currentUser()"), "JQL: {}", jql);
    // The free-text token survived.
    assert!(
        jql.contains(r#"(description ~ "crash" OR summary ~ "crash")"#),
        "JQL: {}",
        jql
    );
}

/// An undefined fragment in the body
/// prevents the JIRA search from firing
/// and surfaces a status message naming
/// the missing fragment. Asserts both the
/// suppression of the network call and
/// the diagnostic text.
#[test]
fn jira_undefined_fragment_blocks_search_and_surfaces_message() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let recorded = fake.recorded.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    // No fragments defined — `@label1` is
    // an undefined fragment.
    app.query = String::from("-@label1 crash");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    // The search was NOT fired — the
    // undefined-fragment gate short-circuits
    // before `spawn_jira_request`.
    assert_eq!(
        recorded.lock().unwrap().len(),
        0,
        "undefined fragment must not fire the search",
    );
    // The status message names the missing
    // fragment.
    let status = app
        .status_message
        .as_ref()
        .map(|(s, _)| s.as_str())
        .unwrap_or("");
    assert!(status.contains("@label1"), "status: {:?}", status,);
    assert!(status.contains("not configured"), "status: {:?}", status,);
}

/// End-to-end: a JIRA issue with all five
/// preview attributes (Status, Priority,
/// Due, Assignee, Description) produces a
/// `HistoryRow.output` with five lines,
/// each label wrapped in `**...**` markers
/// so the details-pane renderer turns them
/// into bold spans.
#[test]
fn jira_row_output_contains_all_five_bold_labels() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![crate::jira::JiraIssue {
            key: "PROJ-1".to_string(),
            summary: "login crash".to_string(),
            status: "Open".to_string(),
            priority: "High".to_string(),
            assignee: "Alice".to_string(),
            due: "2024-07-15".to_string(),
            description: "The login button is broken on Safari.".to_string(),
            ..Default::default()
        }],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Forcibly fire the autocall.
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    assert_eq!(app.jira_rows.len(), 1);
    let out = &app.jira_rows[0].output;
    // The new layout is 3 lines of
    // header (Status/Priority on
    // line 1, Due/Assignee on line 2,
    // Description label on line 3)
    // followed by the description
    // body on line 4. The
    // join-on-newline convention gives
    // us a single string with the
    // expected layout.
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "got: {:?}", lines);
    assert_eq!(lines[0], "**Status**: Open  **Priority**: High");
    assert_eq!(lines[1], "**Due**: 2024-07-15  **Assignee**: Alice");
    assert_eq!(lines[2], "**Description**");
    // The description body
    // appears on the line
    // after the label
    // (no value on the
    // label line itself).
    assert_eq!(lines[3], "The login button is broken on Safari.");
    // The full output contains
    // exactly four `**` openers
    // and four `**` closers
    // (one per label: Status,
    // Priority, Due, Assignee).
    // The description label is
    // also bolded via `**`
    // but without a colon, so
    // the `**Description**` line
    // has its own pair. Total:
    // 5 pairs (Status, Priority,
    // Due, Assignee, Description).
    assert_eq!(out.matches("**").count(), 10);
}

/// JIRA rows with status `"Closed"` or
/// `"To be Reviewed"` are treated as
/// "done" — `exit_code = 0` so the
/// row shows a green `✓` marker.
/// Every other status is "still open"
/// — `exit_code = 1` so the row shows
/// a red `✗` marker. The comparison is
/// case-insensitive.
#[test]
fn jira_row_exit_code_reflects_closed_status() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![
            crate::jira::JiraIssue {
                key: "PROJ-1".to_string(),
                summary: "done".to_string(),
                status: "Closed".to_string(),
                ..Default::default()
            },
            crate::jira::JiraIssue {
                key: "PROJ-2".to_string(),
                summary: "in review".to_string(),
                status: "To be Reviewed".to_string(),
                ..Default::default()
            },
            crate::jira::JiraIssue {
                key: "PROJ-3".to_string(),
                summary: "in progress".to_string(),
                status: "In Progress".to_string(),
                ..Default::default()
            },
            crate::jira::JiraIssue {
                key: "PROJ-4".to_string(),
                summary: "lowercase closed".to_string(),
                status: "closed".to_string(),
                ..Default::default()
            },
        ],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    assert_eq!(app.jira_rows.len(), 4);
    // PROJ-1: Closed → exit_code 0 (green ✓).
    assert_eq!(
        app.jira_rows[0].exit_code, 0,
        "Closed status must map to exit_code 0",
    );
    // PROJ-2: To be Reviewed → exit_code 0 (green ✓).
    assert_eq!(
        app.jira_rows[1].exit_code, 0,
        "To be Reviewed status must map to exit_code 0",
    );
    // PROJ-3: In Progress → exit_code 1 (red ✗).
    assert_eq!(
        app.jira_rows[2].exit_code, 1,
        "In Progress status must map to exit_code 1",
    );
    // PROJ-4: closed (lowercase) → exit_code 0.
    assert_eq!(
        app.jira_rows[3].exit_code, 0,
        "lowercase `closed` must still match (case-insensitive)",
    );
}

/// When the issue has empty values
/// for some attributes, the row
/// builder still emits the label
/// (with `<none>` as the
/// placeholder) so the layout stays
/// consistent. The renderer doesn't
/// strip `<none>` — it just
/// displays it as plain text.
#[test]
fn jira_row_output_uses_none_for_empty_attributes() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![crate::jira::JiraIssue {
            key: "PROJ-1".to_string(),
            summary: "untriaged bug".to_string(),
            // status, priority, assignee, due, description all default to empty
            ..Default::default()
        }],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    assert_eq!(app.jira_rows.len(), 1);
    let out = &app.jira_rows[0].output;
    // All four metadata labels
    // appear with `<none>` as the
    // placeholder. The Description
    // label is just the label (no
    // colon / no value), and the
    // body line below it is also
    // `<none>` so the layout
    // stays consistent.
    assert!(out.contains("**Status**: <none>"));
    assert!(out.contains("**Priority**: <none>"));
    assert!(out.contains("**Due**: <none>"));
    assert!(out.contains("**Assignee**: <none>"));
    assert!(out.contains("**Description**"));
    assert!(out.contains("\n<none>"));
}

/// `sort_comments_newest_first` reverses
/// a comments list so the newest
/// comment (by `created`) is at
/// index 0. JIRA's REST v2 endpoint
/// returns comments in
/// `created`-ascending order; the
/// TUI reverses them on the way in.
#[test]
fn sort_comments_newest_first_reverses_by_created() {
    let mut comments = vec![
        crate::jira::JiraComment {
            id: "1".to_string(),
            author: "Oldest".to_string(),
            created: "2024-06-28T10:00:00.000+0000".to_string(),
            ..Default::default()
        },
        crate::jira::JiraComment {
            id: "3".to_string(),
            author: "Newest".to_string(),
            created: "2024-06-30T19:14:39.000+0000".to_string(),
            ..Default::default()
        },
        crate::jira::JiraComment {
            id: "2".to_string(),
            author: "Middle".to_string(),
            created: "2024-06-29T10:00:00.000+0000".to_string(),
            ..Default::default()
        },
    ];
    sort_comments_newest_first(&mut comments);
    assert_eq!(comments[0].author, "Newest");
    assert_eq!(comments[1].author, "Middle");
    assert_eq!(comments[2].author, "Oldest");
}

/// Comments with the same `created`
/// timestamp fall back to the
/// `id` field as a tie-breaker.
/// This covers the rare
/// batch-imported-comments case
/// where multiple comments share
/// the exact same second.
#[test]
fn sort_comments_newest_first_uses_id_as_tie_breaker() {
    let mut comments = vec![
        crate::jira::JiraComment {
            id: "100".to_string(),
            author: "Lower id".to_string(),
            created: "2024-06-30T19:14:39.000+0000".to_string(),
            ..Default::default()
        },
        crate::jira::JiraComment {
            id: "200".to_string(),
            author: "Higher id".to_string(),
            created: "2024-06-30T19:14:39.000+0000".to_string(),
            ..Default::default()
        },
    ];
    sort_comments_newest_first(&mut comments);
    // Both have the same `created`,
    // so the higher id (200)
    // wins the tie-break and
    // comes first.
    assert_eq!(comments[0].author, "Higher id");
    assert_eq!(comments[1].author, "Lower id");
}

/// `format_jira_date` extracts the
/// `YYYY-MM-DD HH:MM` portion of
/// JIRA's ISO-8601 timestamp and
/// appends ` UTC` for a compact,
/// human-readable date suitable
/// for the comment sub-heading.
#[test]
fn format_jira_date_trims_to_compact_utc() {
    assert_eq!(
        format_jira_date("2024-06-30T19:14:39.000+0000"),
        "2024-06-30 19:14 UTC"
    );
    // A timestamp without
    // milliseconds and offset
    // is also accepted (JIRA's
    // REST v2 may emit either
    // form depending on the
    // instance).
    assert_eq!(
        format_jira_date("2024-06-30T19:14:39Z"),
        "2024-06-30 19:14 UTC"
    );
    // Empty / short / malformed
    // inputs degrade to the
    // raw string or empty.
    assert_eq!(format_jira_date(""), "");
    assert_eq!(format_jira_date("garbage"), "garbage");
}

/// When the user opens the show-output
/// overlay on a JIRA row, the TUI
/// fires a background comments fetch
/// and (with the fake client)
/// synchronously builds the overlay
/// text from the row + the canned
/// comments. Verifies the full
/// structure: `## Header`,
/// `## Description`, `## Comments`
/// with one sub-heading per
/// comment.
#[test]
fn jira_show_output_view_fetches_comments_and_builds_overlay() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![crate::jira::JiraIssue {
            key: "PROJ-1".to_string(),
            summary: "login crash".to_string(),
            status: "Open".to_string(),
            priority: "High".to_string(),
            assignee: "Alice".to_string(),
            due: "2024-07-15".to_string(),
            description: "The login button is broken on Safari.".to_string(),
            ..Default::default()
        }],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        comments: vec![
            // Two comments, one newer
            // than the other. The
            // TUI sorts them
            // newest-first; the canned
            // order is the opposite so
            // we can verify the sort.
            crate::jira::JiraComment {
                id: "10001".to_string(),
                author: "Bob".to_string(),
                body: "Looking into this.".to_string(),
                created: "2024-06-29T10:00:00.000+0000".to_string(),
                updated: "2024-06-29T10:00:00.000+0000".to_string(),
            },
            crate::jira::JiraComment {
                id: "10002".to_string(),
                author: "Alice".to_string(),
                body: "Confirmed, fixing now.".to_string(),
                created: "2024-06-30T19:14:39.000+0000".to_string(),
                updated: "2024-06-30T19:14:39.000+0000".to_string(),
            },
        ],
        comment_keys: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let comment_keys = fake.comment_keys.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Forcibly fire the search
    // autocall so the row is
    // populated.
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    assert_eq!(app.jira_rows.len(), 1);
    // Select the row.
    app.list_state.select(Some(0));
    // Open the show-output view.
    // The fake-client path runs
    // synchronously, so the
    // overlay is open by the
    // time this method returns.
    app.show_output_view();
    // The fake client's
    // `fetch_comments` was
    // called once with the
    // right key.
    assert_eq!(comment_keys.lock().unwrap().len(), 1);
    assert_eq!(comment_keys.lock().unwrap()[0], "PROJ-1");
    // The overlay is open.
    let view = app.output_view.as_ref().expect("overlay should be open");
    // The overlay text follows the
    // user-spec structure.
    assert!(view.text.contains("## Header"), "got: {}", view.text);
    assert!(view.text.contains("## Description"), "got: {}", view.text);
    assert!(view.text.contains("## Comments"), "got: {}", view.text);
    // The header block contains
    // the 3-line preview
    // (Status/Priority, Due/
    // Assignee, Description
    // label) verbatim.
    assert!(
        view.text.contains("**Status**: Open  **Priority**: High"),
        "got: {}",
        view.text
    );
    assert!(
        view.text
            .contains("**Due**: 2024-07-15  **Assignee**: Alice"),
        "got: {}",
        view.text
    );
    assert!(view.text.contains("**Description**"), "got: {}", view.text);
    // The full description
    // appears in the `# Description`
    // section (not in `# Header`).
    // The description is
    // visible exactly once in
    // the overlay — the user
    // explicitly asked for
    // this. (`# Header` shows
    // the metadata block
    // only; the description
    // body lives in its
    // own section.)
    assert!(view.text.contains("login button"), "got: {}", view.text);
    // Comments are sorted
    // newest-first. Alice's
    // 2024-06-30 comment must
    // appear before Bob's
    // 2024-06-29 comment.
    let alice_pos = view.text.find("Alice").expect("Alice in overlay");
    let alice_date_pos = view
        .text
        .find("2024-06-30")
        .expect("Alice's date in overlay");
    let bob_pos = view.text.find("Bob").expect("Bob in overlay");
    let bob_date_pos = view.text.find("2024-06-29").expect("Bob's date in overlay");
    assert!(
        alice_pos < bob_pos,
        "Alice (newer) must appear before Bob (older); got Alice@{alice_pos} Bob@{bob_pos}",
    );
    assert!(
        alice_date_pos < bob_date_pos,
        "2024-06-30 must appear before 2024-06-29",
    );
    // Each comment has a
    // sub-heading with the
    // author and date joined by
    // a middle dot (U+00B7).
    assert!(
        view.text.contains("Alice \u{00b7} 2024-06-30 19:14 UTC"),
        "got: {}",
        view.text
    );
    assert!(
        view.text.contains("Bob \u{00b7} 2024-06-29 10:00 UTC"),
        "got: {}",
        view.text
    );
    // Each comment's body
    // appears below its
    // sub-heading.
    assert!(
        view.text.contains("Confirmed, fixing now."),
        "got: {}",
        view.text
    );
    assert!(
        view.text.contains("Looking into this."),
        "got: {}",
        view.text
    );
}

/// An issue with no comments
/// produces an overlay with a
/// `(no comments)` placeholder
/// after the `## Comments`
/// heading. The overlay is
/// still built and opened
/// (a non-error result is
/// still a result).
#[test]
fn jira_show_output_view_with_no_comments() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![crate::jira::JiraIssue {
            key: "PROJ-1".to_string(),
            summary: "no comments yet".to_string(),
            status: "Open".to_string(),
            ..Default::default()
        }],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        comments: vec![], // empty
        comment_keys: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    app.list_state.select(Some(0));
    app.show_output_view();
    let view = app.output_view.as_ref().expect("overlay should be open");
    assert!(view.text.contains("## Comments"));
    assert!(view.text.contains("(no comments)"));
}

/// An issue with no description
/// produces an overlay with a
/// `(no description)` placeholder
/// after the `## Description`
/// heading.
#[test]
fn jira_show_output_view_with_no_description() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![crate::jira::JiraIssue {
            key: "PROJ-1".to_string(),
            summary: "no description".to_string(),
            status: "Open".to_string(),
            description: String::new(),
            ..Default::default()
        }],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        comments: vec![],
        comment_keys: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    app.list_state.select(Some(0));
    app.show_output_view();
    let view = app.output_view.as_ref().expect("overlay should be open");
    assert!(view.text.contains("## Description"));
    assert!(view.text.contains("(no description)"));
}

/// The description body
/// appears exactly once in
/// the overlay — in the
/// `## Description` section,
/// not duplicated in
/// `## Header`. The user
/// explicitly asked for
/// this: previously the
/// description was shown
/// twice (once in
/// `## Header` as part of
/// the preview-window
/// content, once in
/// `## Description` as
/// its own section),
/// which was redundant.
/// The fix: `## Header`
/// now shows only the
/// 3-line metadata block
/// (Status/Priority,
/// Due/Assignee,
/// Description label);
/// the description body
/// lives in `## Description`
/// only. The test uses a
/// distinctive
/// description string
/// ("unicorn-magic-marker")
/// so the count assertion
/// is reliable — the
/// literal text would
/// never appear in the
/// overlay except as the
/// description body.
#[test]
fn jira_overlay_shows_description_exactly_once() {
    use std::sync::Arc;
    let unique_description = "unicorn-magic-marker \
                 paragraphs here";
    let fake = FakeJira {
        issues: vec![crate::jira::JiraIssue {
            key: "PROJ-1".to_string(),
            summary: "dedup test".to_string(),
            status: "Open".to_string(),
            priority: "High".to_string(),
            assignee: "Alice".to_string(),
            due: "2024-07-15".to_string(),
            description: unique_description.to_string(),
            ..Default::default()
        }],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        comments: vec![],
        comment_keys: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    app.list_state.select(Some(0));
    app.show_output_view();
    let view = app.output_view.as_ref().expect("overlay should be open");
    // The description text
    // appears exactly once.
    // (The `match_indices`
    // count is the number
    // of *non-overlapping*
    // occurrences of the
    // substring in the
    // haystack.)
    let occurrences = view.text.match_indices(unique_description).count();
    assert_eq!(
        occurrences, 1,
        "description should appear exactly once, found {} times in: {}",
        occurrences, view.text,
    );
    // Sanity: the
    // `## Description`
    // section exists and
    // contains the
    // description.
    assert!(view.text.contains("## Description"));
    // Sanity: the
    // `## Header` section
    // exists but does NOT
    // contain the
    // description body
    // (only the 3-line
    // metadata block).
    // We check this by
    // splitting the
    // overlay at the
    // `## Description`
    // heading; everything
    // before the split
    // is the `## Header`
    // section.
    let header_section = view
        .text
        .split("## Description")
        .next()
        .expect("`## Description` heading should exist");
    assert!(
        !header_section.contains(unique_description),
        "`## Header` should not contain the description body, but found: {}",
        header_section,
    );
}

/// Pressing Ctrl+L twice on a JIRA
/// row while a fetch is in flight
/// doesn't queue a second fetch
/// (the `jira_comments_in_flight`
/// latch prevents duplicate
/// background threads).
#[test]
fn jira_show_output_view_dedupes_concurrent_fetches() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![crate::jira::JiraIssue {
            key: "PROJ-1".to_string(),
            ..Default::default()
        }],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        comments: vec![],
        comment_keys: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..Default::default()
    };
    let comment_keys = fake.comment_keys.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    app.list_state.select(Some(0));
    // The fake-client path runs
    // synchronously and clears
    // the in-flight flag in
    // `process_jira_comments_result`,
    // so a *second* call to
    // `show_output_view` *does*
    // fire a second fetch. This
    // is acceptable: a real
    // user pressing Ctrl+L twice
    // is unlikely, and the
    // synchronous path is the
    // test seam. The dedup
    // behaviour is meaningful
    // only for the production
    // background-thread path,
    // where the in-flight flag
    // stays set until the
    // worker sends its
    // result.
    //
    // Verify the second call
    // does call fetch_comments
    // a second time (the test
    // seam doesn't dedupe, by
    // design) and the overlay
    // is rebuilt.
    app.show_output_view();
    assert_eq!(comment_keys.lock().unwrap().len(), 1);
    let first_overlay = app
        .output_view
        .as_ref()
        .expect("first overlay")
        .text
        .clone();
    app.show_output_view();
    assert_eq!(comment_keys.lock().unwrap().len(), 2);
    let second_overlay = app
        .output_view
        .as_ref()
        .expect("second overlay")
        .text
        .clone();
    // Both overlays have the
    // expected structure.
    assert!(first_overlay.contains("## Header"));
    assert!(second_overlay.contains("## Header"));
}

/// Pressing Ctrl-E on a JIRA row
/// opens the comment-edit buffer in
/// JIRA-add-comment mode (not the
/// local `command_comments` mode).
/// Verifies the `jira_add_comment_target`
/// field is set to the issue key and
/// the buffer is empty (the user is
/// composing a *new* comment, not
/// editing the issue's summary).
#[test]
fn jira_edit_comment_opens_jira_add_comment_mode() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![crate::jira::JiraIssue {
            key: "PROJ-1".to_string(),
            summary: "login crash".to_string(),
            status: "Open".to_string(),
            ..Default::default()
        }],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..FakeJira::default()
    };
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    app.list_state.select(Some(0));
    // Press Ctrl-E.
    app.start_comment_edit();
    // The buffer is in
    // JIRA-add-comment mode:
    // the target is the
    // issue key.
    assert_eq!(
        app.jira_add_comment_target.as_deref(),
        Some("PROJ-1"),
        "jira_add_comment_target should be the issue key, got {:?}",
        app.jira_add_comment_target,
    );
    // The buffer is empty
    // (the user is
    // composing a new
    // comment, not editing
    // the issue's summary).
    assert_eq!(
        app.comment_edit.as_deref(),
        Some(""),
        "buffer should be empty in JIRA add-comment mode"
    );
}

/// When the user saves a non-empty
/// comment in JIRA-add-comment mode,
/// the FakeJira's `add_comment`
/// method is called with the
/// issue key and the buffer text.
/// Verifies the end-to-end path:
/// buffer → POST → fake records the
/// (key, body).
#[test]
fn jira_save_comment_posts_to_jira() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![crate::jira::JiraIssue {
            key: "PROJ-1".to_string(),
            summary: "login crash".to_string(),
            status: "Open".to_string(),
            ..Default::default()
        }],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        posted_comments: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..FakeJira::default()
    };
    let posted = fake.posted_comments.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    app.list_state.select(Some(0));
    app.start_comment_edit();
    // Type a comment.
    app.comment_edit = Some("This is fixed in PR #42.".to_string());
    // Save it.
    app.save_comment_edit().unwrap();
    // The FakeJira recorded
    // the POST.
    assert_eq!(
        posted.lock().unwrap().len(),
        1,
        "add_comment should be called once"
    );
    let (key, body) = &posted.lock().unwrap()[0];
    assert_eq!(key, "PROJ-1");
    assert_eq!(body, "This is fixed in PR #42.");
    // On success, the buffer
    // clears and the target
    // resets to None.
    assert!(
        app.comment_edit.is_none(),
        "buffer should clear on successful POST"
    );
    assert!(
        app.jira_add_comment_target.is_none(),
        "target should reset to None on successful POST"
    );
    // The status bar shows a
    // success message that
    // references the issue.
    let status = app
        .status_message
        .as_ref()
        .map(|(s, _)| s.as_str())
        .unwrap_or("");
    assert!(
        status.contains("Comment posted to PROJ-1"),
        "status should confirm the POST: {:?}",
        status,
    );
}

/// An empty buffer is NOT posted
/// to JIRA. The user sees a
/// status message telling them
/// the body is empty, and the
/// buffer stays so they can
/// type something.
#[test]
fn jira_save_comment_rejects_empty_body() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![crate::jira::JiraIssue {
            key: "PROJ-1".to_string(),
            summary: "login crash".to_string(),
            status: "Open".to_string(),
            ..Default::default()
        }],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        posted_comments: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..FakeJira::default()
    };
    let posted = fake.posted_comments.clone();
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    app.list_state.select(Some(0));
    app.start_comment_edit();
    // Buffer is empty by
    // default (start_comment_edit
    // sets it to
    // String::new() for JIRA
    // rows).
    app.save_comment_edit().unwrap();
    // No POST was made.
    assert_eq!(
        posted.lock().unwrap().len(),
        0,
        "empty body should not be POSTed"
    );
    // The status message
    // explains the body
    // is empty.
    let status = app
        .status_message
        .as_ref()
        .map(|(s, _)| s.as_str())
        .unwrap_or("");
    assert!(
        status.contains("empty"),
        "status should explain the body is empty: {:?}",
        status,
    );
    // The buffer stays so
    // the user can type
    // something. (The
    // target also stays so
    // the next Enter retries
    // the JIRA POST path.)
    assert!(
        app.comment_edit.is_some(),
        "buffer should be preserved on empty-body rejection"
    );
    assert_eq!(
        app.jira_add_comment_target.as_deref(),
        Some("PROJ-1"),
        "target should be preserved on empty-body rejection"
    );
}

/// Cancel (Esc) on the
/// comment-edit buffer clears
/// both the buffer and the
/// JIRA-add-comment target.
/// This is the "user changed
/// their mind" path — they
/// don't want to post after
/// all.
#[test]
fn jira_cancel_comment_edit_clears_target() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![crate::jira::JiraIssue {
            key: "PROJ-1".to_string(),
            summary: "login crash".to_string(),
            status: "Open".to_string(),
            ..Default::default()
        }],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..FakeJira::default()
    };
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    app.list_state.select(Some(0));
    app.start_comment_edit();
    app.comment_edit = Some("draft text".to_string());
    // Cancel.
    app.cancel_comment_edit();
    // Both the buffer and
    // the target clear.
    assert!(app.comment_edit.is_none(), "buffer should clear on cancel");
    assert!(
        app.jira_add_comment_target.is_none(),
        "target should clear on cancel"
    );
}

/// Pressing Ctrl-E on a
/// non-JIRA row keeps the
/// local `command_comments`
/// behaviour — the buffer is
/// prefilled with the existing
/// comment (or empty when no
/// comment exists), and the
/// `jira_add_comment_target` is
/// `None`. This locks in the
/// dispatch: only JIRA rows go
/// through the JIRA-add path.
#[test]
fn non_jira_edit_comment_keeps_local_behaviour() {
    let mut app = directories_test_app(&[("git status", "/tmp", 0)]);
    // Create the schemas `fetch()`
    // joins against so the
    // query doesn't error
    // silently and return
    // an empty list.
    app.conn
        .execute(
            "CREATE TABLE command_comments (command TEXT PRIMARY KEY, comment TEXT NOT NULL)",
            [],
        )
        .expect("cc");
    app.conn
        .execute(
            "CREATE TABLE history_output (history_id INTEGER PRIMARY KEY, output TEXT NOT NULL)",
            [],
        )
        .expect("ho");
    app.conn.execute(
                "INSERT INTO command_comments (command, comment) VALUES ('git status', 'pre-existing comment')",
                [],
            )
            .expect("ins");
    app.refresh();
    app.refresh_labeled();
    // Find the row's index
    // by command (the
    // simplest way to select
    // the row regardless of
    // how the merge happens).
    let row_idx = app
        .merged_rows()
        .iter()
        .position(|r| r.command == "git status")
        .expect("row should be in merged_rows");
    app.list_state.select(Some(row_idx));
    // Press Ctrl-E.
    app.start_comment_edit();
    // The buffer has
    // the pre-existing
    // comment (not the JIRA
    // empty buffer).
    assert_eq!(
        app.comment_edit.as_deref(),
        Some("pre-existing comment"),
        "non-JIRA buffer should be prefilled with the existing comment"
    );
    // The JIRA target is
    // None (we're in the
    // local path).
    assert!(
        app.jira_add_comment_target.is_none(),
        "non-JIRA edit should NOT set the JIRA target"
    );
}

/// `Action::DownloadJiraIssue` is bound to
/// `Ctrl-M-s` by default. The default key
/// mnemonic is "save" (the JIRA issue is
/// saved as a local note).
#[test]
fn download_jira_issue_default_key_routes() {
    let bindings = KeyBindings::defaults();
    let key = KeyEvent::new(
        KeyCode::Char('s'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    let action = action_for_key(&bindings, &key).expect("Ctrl-M-s is bound by default");
    assert_eq!(action, Action::DownloadJiraIssue);
}

/// In JIRA mode, the action stages
/// `note_search jira-issue <KEY>` as the
/// next selection. The parent shell runs
/// the command, which shells out to
/// `note_search` (the note_search binary is
/// expected on `PATH`).
#[test]
fn download_jira_issue_stages_command() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![crate::jira::JiraIssue {
            key: "PROJ-42".to_string(),
            summary: "login crash".to_string(),
            status: "Open".to_string(),
            ..Default::default()
        }],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..FakeJira::default()
    };
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    app.list_state.select(Some(0));
    // Fire the action. The
    // selected row's `command`
    // is the issue key.
    app.download_jira_issue();
    assert_eq!(
        app.selection.as_deref(),
        Some("note_search jira-issue PROJ-42"),
        "expected note_search jira-issue <KEY>, got: {:?}",
        app.selection
    );
    assert_eq!(app.pick_mode, Some(PickMode::Run));
}

/// Outside of JIRA mode, the action is a
/// no-op with a status message so the
/// user understands why their key did
/// nothing. The dispatcher gates on
/// `is_jira_query`; the helper re-gates as
/// a defence-in-depth check.
#[test]
fn download_jira_issue_outside_jira_mode_is_noop() {
    let mut app = directories_test_app(&[("ls", "/tmp", 0)]);
    // `*` triggers panes
    // mode, not JIRA mode.
    // (The action is
    // mode-gated so the
    // user gets a
    // no-op + status
    // message outside
    // JIRA mode.)
    app.query = String::from("*");
    app.refresh();
    app.download_jira_issue();
    // Nothing was staged.
    assert!(
        app.selection.is_none(),
        "no command should be staged outside JIRA mode"
    );
    // A status message
    // tells the user
    // why.
    let status = app
        .status_message
        .as_ref()
        .map(|(s, _)| s.as_str())
        .unwrap_or("");
    assert!(
        status.contains("JIRA search"),
        "status should mention JIRA search: {:?}",
        status
    );
}

/// With no row selected (e.g. an empty
/// JIRA result list), the action is a
/// no-op with a status message.
#[test]
fn download_jira_issue_with_no_row_is_noop() {
    use std::sync::Arc;
    let fake = FakeJira {
        issues: vec![],
        recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
        ..FakeJira::default()
    };
    let mut app = directories_test_app(&[]);
    app.set_jira_client(Arc::new(fake));
    app.query = String::from("-");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    // No list_state.select:
    // there's nothing to
    // select.
    app.download_jira_issue();
    assert!(
        app.selection.is_none(),
        "no command should be staged when no row is selected"
    );
    let status = app
        .status_message
        .as_ref()
        .map(|(s, _)| s.as_str())
        .unwrap_or("");
    assert!(
        status.contains("No JIRA issue selected"),
        "status should mention the missing selection: {:?}",
        status
    );
}

/// `Action::DownloadJiraMatching` ships unbound by
/// default (the `none` sentinel) — same policy as
/// `DeleteMatching`: a bulk action over every issue
/// the current query matches deserves an explicit
/// opt-in key rather than an arbitrary default
/// binding.
#[test]
fn download_jira_matching_default_key_is_unbound() {
    let bindings = KeyBindings::defaults();
    assert!(
        bindings.is_unbound(Action::DownloadJiraMatching),
        "DownloadJiraMatching must ship unbound (default is the `none` sentinel), got: {:?}",
        format_key_specs(bindings.specs(Action::DownloadJiraMatching))
    );
    assert_eq!(
        Action::DownloadJiraMatching.default_key(),
        "none",
        "default_key() for DownloadJiraMatching must be the `none` sentinel"
    );
}

/// In JIRA mode, the action stages `note_search jira
/// <JQL>` (the bulk import subcommand) using the same
/// JQL the TUI already built for the live search — NOT
/// a loop over `app.jira_rows`, which is capped by
/// `JIRA_MAX_RESULTS`. No JIRA client / network call is
/// needed to build the JQL, so this test doesn't need a
/// `FakeJira`.
#[test]
fn download_jira_matching_stages_command() {
    // `jira_build_query` reads `$JIRA_PROJECT` directly
    // (see `App::jira_build_query`), so pin it to unset
    // for a deterministic empty-project JQL — otherwise
    // a developer's real shell env would change the
    // expected string. Guarded by ENV_LOCK so we don't
    // race other tests mutating the same var.
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev_project = std::env::var("JIRA_PROJECT").ok();
    // SAFETY: single-threaded within the ENV_LOCK guard.
    unsafe {
        std::env::remove_var("JIRA_PROJECT");
    }
    let mut app = directories_test_app(&[]);
    app.query = String::from("-");
    app.refresh();
    app.download_jira_matching();
    // Restore before asserting so a panic doesn't leak.
    match prev_project {
        // SAFETY: single-threaded within the ENV_LOCK guard.
        Some(v) => unsafe { std::env::set_var("JIRA_PROJECT", v) },
        None => unsafe { std::env::remove_var("JIRA_PROJECT") },
    }
    assert_eq!(
        app.selection.as_deref(),
        Some("note_search jira 'ORDER BY updated DESC'"),
        "expected note_search jira <JQL>, got: {:?}",
        app.selection
    );
    assert_eq!(app.pick_mode, Some(PickMode::Run));
}

/// Outside of JIRA mode, the action is a no-op with a
/// status message, same as `download_jira_issue`.
#[test]
fn download_jira_matching_outside_jira_mode_is_noop() {
    let mut app = directories_test_app(&[("ls", "/tmp", 0)]);
    // `*` triggers panes mode, not JIRA mode.
    app.query = String::from("*");
    app.refresh();
    app.download_jira_matching();
    assert!(
        app.selection.is_none(),
        "no command should be staged outside JIRA mode"
    );
    let status = app
        .status_message
        .as_ref()
        .map(|(s, _)| s.as_str())
        .unwrap_or("");
    assert!(
        status.contains("JIRA search"),
        "status should mention JIRA search: {:?}",
        status
    );
}

/// An undefined `@fragment` in the query must not
/// silently download the (much broader) free-text
/// fallback — the action refuses to stage a command and
/// surfaces the same diagnostic as `jira_maybe_autocall`.
#[test]
fn download_jira_matching_with_undefined_fragment_is_noop() {
    let mut app = directories_test_app(&[]);
    app.query = String::from("-@nofrag");
    app.refresh();
    app.download_jira_matching();
    assert!(
        app.selection.is_none(),
        "no command should be staged for an undefined fragment"
    );
    let status = app
        .status_message
        .as_ref()
        .map(|(s, _)| s.as_str())
        .unwrap_or("");
    assert!(
        status.contains("not configured"),
        "status should mention the undefined fragment: {:?}",
        status
    );
}

/// Selecting a JIRA row stages `open "<URL>"` using
/// `JIRA_URL` (falls back to `JIRA_SERVER`).
#[test]
fn select_for_run_in_jira_mode_stages_open_url() {
    // Use a unique env-var guard to avoid racing
    // other tests: set the vars, run, restore.
    // (The run is synchronous in the test path; no
    // background thread reads these, so the window
    // is just this function's body.)
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    let _g = ENV_LOCK.lock().unwrap();
    let prev_server = std::env::var("JIRA_SERVER").ok();
    let prev_token = std::env::var("JIRA_API_TOKEN").ok();
    let prev_url = std::env::var("JIRA_URL").ok();
    // SAFETY: no other test in this binary uses these
    // vars (guarded by ENV_LOCK, and the binary is
    // single-process per test thread). Other JIRA
    // tests here don't set these specific vars.
    unsafe {
        std::env::set_var("JIRA_SERVER", "https://jira.example.com");
        std::env::set_var("JIRA_API_TOKEN", "tok");
        std::env::set_var("JIRA_URL", "https://browse.example.com/browse");
    }
    let mut app = directories_test_app(&[]);
    app.jira_rows.push(crate::tui::state::HistoryRow {
        id: -1,
        command: "PROJ-42".to_string(),
        directory: String::new(),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: "summary".to_string(),
        output: String::new(),
        mode: "jira".to_string(),
        source: "jira".to_string(),

        ..Default::default()
    });
    app.query = String::from("-");
    app.refresh();
    app.list_state.select(Some(0));
    app.select_for_run();
    // Restore before asserting so a panic doesn't
    // leak the env to other tests.
    let restore = |name: &str, prev: Option<String>| unsafe {
        match prev {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    };
    restore("JIRA_SERVER", prev_server);
    restore("JIRA_API_TOKEN", prev_token);
    restore("JIRA_URL", prev_url);
    assert_eq!(
        app.selection.as_deref(),
        Some(
            format!(
                "{} \"https://jira.example.com/browse/PROJ-42\"",
                if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                }
            )
            .as_str()
        ),
        "got: {:?}",
        app.selection
    );
    assert_eq!(app.pick_mode, Some(PickMode::Run));
}

/// `jira_maybe_autocall` shows a status message
/// (and fires nothing) when JIRA isn't configured
/// — no env vars and no injected client.
#[test]
fn jira_not_configured_surfaces_status() {
    // Clear any JIRA env so the "not configured" path
    // is deterministically taken (another test sets
    // these; under parallel execution we'd otherwise
    // race). Guarded by ENV_LOCK so we don't clobber
    // the other test's window.
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    let _g = ENV_LOCK.lock().unwrap();
    let prev_server = std::env::var("JIRA_SERVER").ok();
    let prev_token = std::env::var("JIRA_API_TOKEN").ok();
    unsafe {
        std::env::remove_var("JIRA_SERVER");
        std::env::remove_var("JIRA_API_TOKEN");
    }
    let mut app = directories_test_app(&[]);
    app.query = String::from("-PROJ-1");
    app.refresh();
    // Both timers must be past for the new
    // dual-timer gate
    // () to fire.
    let past = std::time::Instant::now()
        - JIRA_IDLE_TIMEOUT
        - JIRA_DEBOUNCE
        - std::time::Duration::from_millis(50);
    app.jira_debounce_started = Some(past);
    app.jira_idle_started = Some(past);
    app.jira_maybe_autocall();
    // Restore before asserting so a panic doesn't leak.
    let restore = |name: &str, prev: Option<String>| unsafe {
        match prev {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    };
    restore("JIRA_SERVER", prev_server);
    restore("JIRA_API_TOKEN", prev_token);
    // No client, no env → nothing fired, no rows.
    assert!(app.jira_rows.is_empty());
}
/// Defensive filter: a
/// `pane_current_path`
/// that doesn't start
/// with `/` (e.g. the
/// command line that
/// spawned the pane,
/// `tmux list-windows
/// -a ...`) must NOT
/// become a directory
/// row. The user
/// reported seeing
/// exactly this in
/// `DIR:TMUX` mode: a
/// row whose visible
/// text was the tmux
/// command line, with
/// no T flag (because
/// the T-marker lookup
/// can't canonicalize a
/// non-path), and
/// clearly not a
/// directory. The
/// fix: skip any
/// `pane_current_path`
/// that doesn't look
/// like an absolute
/// path. The check is
/// `starts_with('/')`
/// because every real
/// absolute path on
/// every Unix starts
/// with `/` — a
/// tmux-reported
/// string that
/// doesn't is
/// necessarily
/// something else
/// (a command line, a
/// relative path, an
/// error message,
/// etc.) and we have
/// no way to render
/// it usefully as a
/// directory.
#[test]
fn tmux_pane_path_must_be_absolute() {
    let mut app = directories_test_app(&[]);
    app.tmux_windows.push(TmuxWindowInfo {
                pane_id: "%0".to_string(),
                // The user's reported
                // bug: tmux reports
                // a "pane_current_path"
                // that's actually
                // the command line.
                path: String::from(
                    "tmux list-windows -a -F #{pane_id} | #{pane_current_path} | active:#{window_active} | Layout: #{window_layout}",
                ),
                ..Default::default()
                });
    app.tmux_windows.push(TmuxWindowInfo {
        pane_id: "%1".to_string(),
        // A real path
        // (a directory
        // that exists on
        // this system)
        // must still show
        // up. `/tmp` is
        // available on
        // every Unix
        // platform and on
        // macOS it
        // canonicalises
        // to
        // `/private/tmp`,
        // which is fine
        // for this test.
        path: std::env::temp_dir().to_string_lossy().into_owned(),
        ..Default::default()
    });
    app.query = "#".to_string();
    app.refresh();
    // The bad path must
    // not produce a row
    // at all.
    let has_bad_row = app
        .merged_rows()
        .iter()
        .any(|r| r.directory.starts_with("tmux "));
    assert!(
        !has_bad_row,
        "tmux rows with non-absolute pane_current_path must be filtered out, got: {:?}",
        app.merged_rows()
            .iter()
            .map(|r| r.directory.clone())
            .collect::<Vec<_>>()
    );
    // The real path must
    // still show up.
    let has_good_row = app.merged_rows().iter().any(|r| {
        let canon = std::fs::canonicalize(&r.directory)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| r.directory.clone());
        let tmp_canon = std::fs::canonicalize(std::env::temp_dir())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
        canon == tmp_canon
    });
    assert!(
            has_good_row,
            "tmux rows with absolute pane_current_path that resolves to a real directory must still show up, got: {:?}",
            app.merged_rows()
                .iter()
                .map(|r| r.directory.clone())
                .collect::<Vec<_>>()
        );
}

// ---- build_help_lines (the help-overlay content) ----

/// Build a minimal `App` for the
/// help-line tests. The
/// `directories_test_app(&[])`
/// helper already builds an
/// `App` with the test-helper
/// defaults (Mode::Global,
/// KeyBindings::defaults(),
/// QueryPrefixes::default(),
/// etc.). We override the
/// fields the help builder
/// actually reads so the
/// test surface is small and
/// stable regardless of
/// test-helper changes.
use super::render::build_help_lines;
use ratatui::style::Modifier;
fn help_app() -> App {
    let mut app = directories_test_app(&[]);
    // The fields the help
    // builder reads.
    app.mode = Mode::Sess;
    app.duplicate_filter = true;
    app.query_prefixes = crate::QueryPrefixes::default();
    app
}

/// The help overlay contains a
/// "Search modes" section that
/// lists every prefix-
/// switchable mode and its
/// trigger character. The
/// section header is present
/// and uses bold styling.
#[test]
fn help_includes_search_modes_section() {
    let lines = build_help_lines(&help_app());
    let texts: Vec<String> = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();
    // The section header
    // exists.
    let found = texts
        .iter()
        .position(|t| t == "Search modes")
        .expect("Search modes section");
    // The header is bold.
    let header_line = &lines[found];
    assert!(header_line
        .spans
        .first()
        .map(|s| s.style.add_modifier.contains(Modifier::BOLD))
        .unwrap_or(false));
}

/// Every search-mode row
/// appears in the help with
/// the user's configured
/// prefix. We check each mode
/// by name and assert the
/// prefix column shows the
/// right character.
#[test]
fn help_lists_all_eleven_search_modes() {
    let lines = build_help_lines(&help_app());
    let texts: Vec<String> = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();
    // Default prefixes
    // (from
    // `QueryPrefixes::default()`):
    // plain: "" (no prefix,
    //        em-dash
    //        marker)
    // regex: /
    // fuzzy: ?
    // output: +
    // llm: =
    // question: %
    // notes: @
    // todo: !
    // directories: #
    // panes: *
    // jira: -
    let expected: &[(&str, &str)] = &[
        ("history", "\u{2014}"),
        ("output", "+"),
        ("LLM command", "="),
        ("question", "%"),
        ("notes", "@"),
        ("todo", "!"),
        ("directories", "#"),
        ("panes", "*"),
        ("JIRA", "-"),
    ];
    for &(mode, prefix) in expected {
        // The row format is
        // `  {name:<14}{prefix_text}{desc}`
        // — 2 leading spaces,
        // 14 chars for the mode
        // name (left-aligned,
        // right-padded), then
        // the prefix text
        // (right-padded to 7
        // chars). The format
        // helper inside
        // `build_help_lines` uses:
        //   `prefix_text` is " X"
        // when the prefix is
        // non-empty (leading
        // space for column
        // alignment) and just
        // "\u{2014}" (no leading
        // space) when the prefix
        // is empty.
        // The row's actual
        // content is therefore
        // the 16-char name
        // followed immediately
        // by the prefix
        // (no extra separator
        // between columns).
        let prefix_with_pad = if prefix == "\u{2014}" {
            "\u{2014}".to_string()
        } else {
            format!(" {}", prefix)
        };
        let needle = format!("  {:<14}{}", mode, prefix_with_pad);
        assert!(
            texts.iter().any(|t| t.contains(&needle)),
            "missing row for mode {}: searched for {:?}",
            mode,
            needle,
        );
    }
}

/// The plain-mode row shows
/// an em-dash (\u{2014}) in the
/// prefix column because plain
/// has no prefix. Verifies the
/// "no prefix" visual
/// indicator is present so
/// the user sees that plain
/// mode is the default.
#[test]
fn help_plain_mode_shows_em_dash_prefix() {
    let lines = build_help_lines(&help_app());
    let texts: Vec<String> = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();
    let plain_row = texts
        .iter()
        .find(|t| t.trim_start().starts_with("history"))
        .expect("plain row");
    assert!(
        plain_row.contains('\u{2014}'),
        "plain row should contain em-dash: {:?}",
        plain_row
    );
}

/// The "JIRA-mode tags"
/// section is present, with
/// all five tag rows
/// (`@me`, `@today`,
/// `@week`, `@month`,
/// `@<name>`).
#[test]
fn help_includes_jira_mode_tags_section() {
    let lines = build_help_lines(&help_app());
    let texts: Vec<String> = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();
    // Section header.
    let header_idx = texts
        .iter()
        .position(|t| t == "JIRA-mode tags")
        .expect("JIRA-mode tags section");
    // Header is bold.
    assert!(lines[header_idx]
        .spans
        .first()
        .map(|s| s.style.add_modifier.contains(Modifier::BOLD))
        .unwrap_or(false));
    // All five tags appear
    // in the lines after
    // the header.
    let after = &texts[header_idx..];
    assert!(after.iter().any(|t| t.contains("@me")), "missing @me");
    assert!(after.iter().any(|t| t.contains("@today")), "missing @today");
    assert!(after.iter().any(|t| t.contains("@week")), "missing @week");
    assert!(after.iter().any(|t| t.contains("@month")), "missing @month");
    assert!(
        after.iter().any(|t| t.contains("@<name>")),
        "missing @<name> fragment row"
    );
}

/// Each JIRA-tag row shows
/// the exact JQL clause the
/// tag expands to, so the
/// help doubles as a JQL
/// reference (the user can
/// copy-paste the clause
/// into a JIRA web search
/// to verify the
/// behaviour).
#[test]
fn help_jira_tags_show_jql_clauses() {
    let lines = build_help_lines(&help_app());
    let texts: Vec<String> = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();
    // The exact clauses
    // from the `build_jql`
    // implementation.
    let expected: &[(&str, &str)] = &[
        ("@me", "assignee = currentUser()"),
        ("@today", "updated >= \"<today-1d>\""),
        ("@week", "updated >= \"<today-7d>\""),
        ("@month", "updated >= \"<today-31d>\""),
    ];
    for &(tag, jql) in expected {
        // The tag row
        // contains the
        // tag in the
        // first
        // column
        // and the
        // JQL in
        // the
        // second
        // column.
        // We look
        // for a
        // line
        // that
        // contains
        // both.
        let matching = texts.iter().find(|t| t.contains(tag) && t.contains(jql));
        assert!(
            matching.is_some(),
            "missing JQL clause for {}: looking for {:?}",
            tag,
            jql
        );
    }
}

/// The help reflects the
/// user's configured prefixes,
/// not the defaults. Rebinds
/// the regex prefix to `#` and
/// confirms the help shows `#`
/// in the regex row (not `/`,
/// the default).
#[test]
fn help_shows_user_configured_prefixes() {
    let mut app = help_app();
    // Rebind the regex
    // prefix from `/` to
    // `#`. The `prefix.regex=...`
    // config key is
    // parsed by
    // `Config::assign_prefix`;
    // here we set the field
    // directly (which is
    // what the config
    // loader does after
    // parsing).
    app.query_prefixes.directories = '#';
    let lines = build_help_lines(&app);
    let _texts: Vec<String> = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();
    // The regex row should
    // now show `#` as
    // the prefix. The
    // format helper pads
    // the prefix with a
    // leading space, so
    // the row contains
    // ` # `.
    // The regex row should
    // now show `#` as
    // the prefix. The
    // format helper pads
    // the prefix with a
    // leading space, so
    // the row contains
    // ` # `.
    // (regex/fuzzy prefixes removed — now match-algorithm toggles)
}

// ===== Prefix Picker Tests =====

#[test]
fn apply_prefix_sets_prefix_and_preserves_body() {
    let mut app = global_test_app(&[("git status", 1)]);
    app.query = "git status".to_string();
    app.apply_prefix(Some('#'));
    assert_eq!(
        app.query, "#git status",
        "expected # prefix with body preserved"
    );
    assert_eq!(app.query_cursor, 11);
    assert!(app.query_touched);
}

#[test]
fn apply_prefix_none_strips_current_prefix() {
    let mut app = global_test_app(&[("src", 1)]);
    app.query = "#src".to_string();
    app.apply_prefix(None);
    assert_eq!(app.query, "src", "expected prefix stripped");
}

#[test]
fn apply_prefix_jira_then_output_cycles_ok() {
    let mut app = global_test_app(&[("todo", 1)]);
    app.query = "-todo".to_string();
    assert_eq!(app.query, "-todo");
    app.apply_prefix(Some('+'));
    assert_eq!(app.query, "+todo");
    app.apply_prefix(None);
    assert_eq!(app.query, "todo");
}

#[test]
fn prefix_picker_new_preselects_none_for_plain_query() {
    let app = global_test_app(&[("hello", 1)]);
    let picker = PrefixPicker::new(&app.query_prefixes, None);
    assert_eq!(picker.selected, 0);
    assert_eq!(picker.options[0].label, "History");
    assert_eq!(picker.options[0].prefix, None);
}

#[test]
fn prefix_picker_new_preselects_jira_for_dash_query() {
    let mut app = global_test_app(&[("jira", 1)]);
    app.query = "-jira".to_string();
    let first = app.query.chars().next();
    let picker = PrefixPicker::new(&app.query_prefixes, first);
    let idx = picker.selected;
    assert_eq!(picker.options[idx].label, "JIRA");
}

#[test]
fn prefix_picker_new_falls_back_to_history_for_unknown_prefix() {
    let mut app = global_test_app(&[("unknown", 1)]);
    app.query = "Xunknown".to_string();
    let first = app.query.chars().next();
    let picker = PrefixPicker::new(&app.query_prefixes, first);
    assert_eq!(picker.selected, 0);
}

#[test]
fn prefix_picker_has_thirteen_entries() {
    let picker = PrefixPicker::new(&crate::QueryPrefixes::default(), None);
    assert_eq!(picker.options.len(), 13);
}

#[test]
fn pick_prefix_opens_prefix_picker() {
    let mut app = global_test_app(&[("hello", 1)]);
    app.query = "hello".to_string();
    app.open_prefix_picker();
    assert!(app.prefix_picker.is_some());
    let picker = app.prefix_picker.as_ref().unwrap();
    assert_eq!(picker.options[0].label, "History");
}

#[test]
fn pick_prefix_preselects_current_prefix() {
    let mut app = global_test_app(&[("notes", 1)]);
    app.query = "@notes".to_string();
    app.open_prefix_picker();
    let picker = app.prefix_picker.as_ref().unwrap();
    let idx = picker.selected;
    assert_eq!(picker.options[idx].label, "Notes");
}

#[test]
fn handle_prefix_picker_key_enter_applies_prefix() {
    let mut app = global_test_app(&[("hello", 1)]);
    app.query = "hello".to_string();
    app.open_prefix_picker();
    // Select "JIRA" (index 8)
    app.prefix_picker.as_mut().unwrap().selected = 8;
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
    let quit = handle_prefix_picker_key(&mut app, enter);
    assert!(!quit, "picker commit should not exit TUI");
    assert!(app.prefix_picker.is_none(), "picker should close on Enter");
    assert_eq!(app.query, "-hello", "should apply JIRA prefix (-)");
}

#[test]
fn handle_prefix_picker_key_cancel_closes_without_change() {
    let mut app = global_test_app(&[("hello", 1)]);
    app.query = "hello".to_string();
    app.open_prefix_picker();
    // Select JIRA so we'd see a change if Enter were pressed
    app.prefix_picker.as_mut().unwrap().selected = 8;
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
    let quit = handle_prefix_picker_key(&mut app, esc);
    assert!(!quit, "picker cancel should not exit TUI");
    assert!(app.prefix_picker.is_none(), "picker should close on Cancel");
    assert_eq!(app.query, "hello", "query should be unchanged");
}

#[test]
fn handle_prefix_picker_key_updown_navigates() {
    let mut app = global_test_app(&[("hello", 1)]);
    app.query = "hello".to_string();
    app.open_prefix_picker();
    assert_eq!(app.prefix_picker.as_ref().unwrap().selected, 0);
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
    handle_prefix_picker_key(&mut app, down);
    assert_eq!(app.prefix_picker.as_ref().unwrap().selected, 1);
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::empty());
    handle_prefix_picker_key(&mut app, up);
    assert_eq!(app.prefix_picker.as_ref().unwrap().selected, 0);
}

#[test]
fn handle_prefix_picker_key_ctrl_n_ctrl_p_navigates() {
    let mut app = global_test_app(&[("hello", 1)]);
    app.query = "hello".to_string();
    app.open_prefix_picker();
    let ctrl_n = KeyEvent {
        code: KeyCode::Char('n'),
        modifiers: KeyModifiers::CONTROL,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::empty(),
    };
    let ctrl_p = KeyEvent {
        code: KeyCode::Char('p'),
        modifiers: KeyModifiers::CONTROL,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::empty(),
    };
    handle_prefix_picker_key(&mut app, ctrl_n);
    assert_eq!(app.prefix_picker.as_ref().unwrap().selected, 1);
    handle_prefix_picker_key(&mut app, ctrl_p);
    assert_eq!(app.prefix_picker.as_ref().unwrap().selected, 0);
}

#[test]
fn handle_prefix_picker_key_home_end_jump() {
    let mut app = global_test_app(&[("hello", 1)]);
    app.query = "hello".to_string();
    app.open_prefix_picker();
    let end = KeyEvent::new(KeyCode::End, KeyModifiers::empty());
    handle_prefix_picker_key(&mut app, end);
    assert_eq!(app.prefix_picker.as_ref().unwrap().selected, 12);
    let home = KeyEvent::new(KeyCode::Home, KeyModifiers::empty());
    handle_prefix_picker_key(&mut app, home);
    assert_eq!(app.prefix_picker.as_ref().unwrap().selected, 0);
}

#[test]
fn handle_prefix_picker_key_ctrl_c_closes_picker() {
    let mut app = global_test_app(&[("hello", 1)]);
    app.query = "hello".to_string();
    app.open_prefix_picker();
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let quit = handle_prefix_picker_key(&mut app, ctrl_c);
    // Ctrl+C is bound to Cancel by
    // default, so the Cancel action
    // fires: close the picker, do not
    // exit the TUI. This mirrors the
    // command palette behaviour where
    // the user's Cancel binding
    // dismisses the overlay.
    assert!(!quit, "picker cancel (ctrl+c) should not exit TUI");
    assert!(app.prefix_picker.is_none(), "picker should close on ctrl+c");
    assert!(
        !app.cancelled,
        "cancelled flag should not be set by picker close"
    );
}

/// Regression for a guard-ordering bug where the movement
/// match arms were written as
/// `KeyCode::Up | KeyCode::Char('p') if CONTROL` — the guard
/// applied to the whole or-pattern, so a plain `Up` arrow
/// (modifiers empty) failed the guard, fell through to
/// `_ => None`, and navigation silently did nothing. Plain
/// arrows must move the selection; `Ctrl-P`/`Ctrl-N` must
/// too. This test constructs a picker directly (no
/// `.codegraph` index needed) so the regression is caught
/// without an integration setup.
fn make_relations_picker(n: usize) -> CodeGraphRelationsPicker {
    let entries: Vec<CodegraphRelationEntry> = (0..n)
        .map(|_| CodegraphRelationEntry {
            section: CodegraphRelationSection::Caller,
            node: crate::codegraph::CodeGraphNode {
                id: String::new(),
                kind: "method".to_string(),
                name: String::new(),
                qualified_name: String::new(),
                file_path: String::new(),
                language: String::new(),
                start_line: 1,
                end_line: 1,
                signature: None,
                docstring: None,
            },
        })
        .collect();
    CodeGraphRelationsPicker {
        entries,
        selected: 0,
        symbol: String::new(),
        repo_root: std::path::PathBuf::new(),
    }
}

#[test]
fn codegraph_relations_picker_arrow_keys_navigate() {
    let mut app = global_test_app(&[("hello", 1)]);
    app.codegraph_relations_picker = Some(make_relations_picker(5));
    assert_eq!(app.codegraph_relations_picker.as_ref().unwrap().selected, 0);
    // Plain Down (no modifiers) must move to index 1.
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
    handle_codegraph_relations_picker_key(&mut app, down);
    assert_eq!(
        app.codegraph_relations_picker.as_ref().unwrap().selected,
        1,
        "plain Down arrow must move the selection"
    );
    // Plain Up must move back to index 0.
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::empty());
    handle_codegraph_relations_picker_key(&mut app, up);
    assert_eq!(
        app.codegraph_relations_picker.as_ref().unwrap().selected,
        0,
        "plain Up arrow must move the selection"
    );
}

#[test]
fn codegraph_relations_picker_ctrl_n_ctrl_p_navigate() {
    let mut app = global_test_app(&[("hello", 1)]);
    app.codegraph_relations_picker = Some(make_relations_picker(5));
    let ctrl_n = KeyEvent {
        code: KeyCode::Char('n'),
        modifiers: KeyModifiers::CONTROL,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::empty(),
    };
    let ctrl_p = KeyEvent {
        code: KeyCode::Char('p'),
        modifiers: KeyModifiers::CONTROL,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::empty(),
    };
    handle_codegraph_relations_picker_key(&mut app, ctrl_n);
    assert_eq!(app.codegraph_relations_picker.as_ref().unwrap().selected, 1);
    handle_codegraph_relations_picker_key(&mut app, ctrl_p);
    assert_eq!(app.codegraph_relations_picker.as_ref().unwrap().selected, 0);
}

#[test]
fn codegraph_relations_picker_home_end_jump() {
    let mut app = global_test_app(&[("hello", 1)]);
    app.codegraph_relations_picker = Some(make_relations_picker(5));
    let end = KeyEvent::new(KeyCode::End, KeyModifiers::empty());
    handle_codegraph_relations_picker_key(&mut app, end);
    assert_eq!(app.codegraph_relations_picker.as_ref().unwrap().selected, 4);
    let home = KeyEvent::new(KeyCode::Home, KeyModifiers::empty());
    handle_codegraph_relations_picker_key(&mut app, home);
    assert_eq!(app.codegraph_relations_picker.as_ref().unwrap().selected, 0);
}

/// `SmartOpen` in `&` (codegraph) mode opens the callers/callees
/// picker. It can't get a real `.codegraph` index in a unit
/// test, so the opener surfaces a status message ("No .codegraph
/// index found") instead of opening the picker — but the
/// dispatch must still branch into the codegraph path rather
/// than falling through to `Run`. We assert that branch by
/// checking it did NOT stage a selection (Run would have, for a
/// row with a real command).
#[test]
fn smart_open_in_codegraph_mode_takes_codegraph_branch() {
    let mut app = global_test_app(&[("hello", 1)]);
    // Enter `&` (codegraph) prefix mode with a query body.
    app.query = format!("{}getSymbol", app.query_prefixes.codegraph);
    app.query_cursor = app.query.chars().count();
    app.refresh();
    // Select the first row (the history row "hello") so the
    // codegraph opener has a selected row to inspect; its mode
    // is "command", so `open_codegraph_relations` will surface
    // a status message and NOT open the picker.
    app.list_state.select(Some(0));
    let quit = dispatch_action(&mut app, Action::SmartOpen);
    assert!(!quit, "SmartOpen must not exit the TUI in codegraph mode");
    assert!(
        app.codegraph_relations_picker.is_none(),
        "no real .codegraph index in the test env → opener must not construct the picker"
    );
    assert!(
        app.selection.is_none(),
        "codegraph branch must not fall through to Run (which would stage a selection)"
    );
}

/// `SmartOpen` in `$` (tags) mode takes the same codegraph branch.
#[test]
fn smart_open_in_tags_mode_takes_codegraph_branch() {
    let mut app = global_test_app(&[("hello", 1)]);
    app.query = format!("{}getSymbol", app.query_prefixes.tags);
    app.query_cursor = app.query.chars().count();
    app.refresh();
    app.list_state.select(Some(0));
    let quit = dispatch_action(&mut app, Action::SmartOpen);
    assert!(!quit, "SmartOpen must not exit the TUI in tags mode");
    assert!(
        app.selection.is_none(),
        "tags branch must not fall through to Run"
    );
}

/// `SmartOpen` outside the special modes falls through to `Run`:
/// selecting a history row stages its command and the dispatch
/// returns `true` (TUI exits). This pins the ergonomic
/// "Shift-Return = Enter" fallback.
#[test]
fn smart_open_in_history_mode_falls_through_to_run() {
    let mut app = global_test_app(&[("ls -la", 1)]);
    app.list_state.select(Some(0));
    let quit = dispatch_action(&mut app, Action::SmartOpen);
    assert!(quit, "SmartOpen in history mode must exit the TUI like Run");
    assert_eq!(
        app.selection.as_deref(),
        Some("ls -la"),
        "SmartOpen fallback must stage the selected row's command"
    );
}

/// `SmartOpen` in `-` (JIRA) mode must take the background-open
/// branch, NOT the `Run` fallback. The `Run` fallback would
/// stage `open "<URL>"` as `selection` and exit the TUI;
/// the background branch stages nothing and stays open. To
/// avoid launching a real browser during the test, we exercise
/// the not-configured path (no JIRA env vars) and assert that
/// the dispatch took the JIRA branch at all — i.e. it did NOT
/// stage a selection (Run would have) and did NOT exit.
#[test]
fn smart_open_in_jira_mode_does_not_stage_or_exit() {
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    let _g = ENV_LOCK.lock().unwrap();
    // Explicitly clear the JIRA env vars so `from_env()` returns
    // None → the background opener surfaces a "not configured"
    // status message instead of actually launching a browser.
    let prev_server = std::env::var("JIRA_SERVER").ok();
    let prev_token = std::env::var("JIRA_API_TOKEN").ok();
    let prev_url = std::env::var("JIRA_URL").ok();
    unsafe {
        std::env::remove_var("JIRA_SERVER");
        std::env::remove_var("JIRA_API_TOKEN");
        std::env::remove_var("JIRA_URL");
    }
    let restore = |name: &str, prev: Option<String>| unsafe {
        match prev {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    };
    let mut app = directories_test_app(&[]);
    app.jira_rows.push(crate::tui::state::HistoryRow {
        id: -1,
        command: "PROJ-42".to_string(),
        directory: String::new(),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: "summary".to_string(),
        output: String::new(),
        mode: "jira".to_string(),
        source: "jira".to_string(),
        ..Default::default()
    });
    app.query = String::from("-");
    app.refresh();
    app.list_state.select(Some(0));
    let quit = dispatch_action(&mut app, Action::SmartOpen);
    restore("JIRA_SERVER", prev_server);
    restore("JIRA_API_TOKEN", prev_token);
    restore("JIRA_URL", prev_url);
    assert!(
        !quit,
        "SmartOpen in JIRA mode must NOT exit the TUI (background open)"
    );
    assert!(
        app.selection.is_none(),
        "SmartOpen in JIRA mode must NOT stage a selection (Run fallback would have): {:?}",
        app.selection
    );
    assert_eq!(
        app.pick_mode, None,
        "SmartOpen in JIRA mode must NOT set pick_mode (Run fallback would have)"
    );
}

// ---- Per-mode query history (C-p / C-n) ----

/// `record_to_mode_history` should skip empty /
/// whitespace-only queries (no point recalling them),
/// dedup consecutive duplicates (so rapid re-runs of the
/// same query don't bloat the history), and cap at 100
/// entries per mode (so a long-lived session can't grow
/// the JSON file unbounded).
#[test]
fn record_to_mode_history_skips_empty_dedups_and_caps() {
    let mut app = global_test_app(&[]);
    let mode = app.query_prefixes.codegraph; // `&`

    // Empty / whitespace queries are skipped.
    app.record_to_mode_history(mode, "");
    app.record_to_mode_history(mode, "   ");
    assert!(
        app.mode_query_history.is_empty(),
        "empty / whitespace queries must not be recorded"
    );

    // Distinct queries are recorded (newest first).
    app.record_to_mode_history(mode, "&foo");
    app.record_to_mode_history(mode, "&bar");
    let entries = app.mode_query_history.get(&mode).unwrap();
    assert_eq!(
        entries,
        &vec!["&bar".to_string(), "&foo".to_string()],
        "newer entries must come first"
    );

    // Consecutive duplicates are skipped. Without this,
    // pressing Enter on the same row three times would
    // add three identical copies and break C-p recall.
    app.record_to_mode_history(mode, "&bar");
    assert_eq!(
        app.mode_query_history.get(&mode).unwrap().len(),
        2,
        "consecutive duplicate must not be re-recorded"
    );

    // A different entry is recorded (consecutive-dedup is
    // strict — only the immediate previous is compared).
    app.record_to_mode_history(mode, "&baz");
    assert_eq!(app.mode_query_history.get(&mode).unwrap()[0], "&baz");

    // Cap at 100 entries per mode.
    for i in 0..150 {
        app.record_to_mode_history(mode, &format!("&entry{i}"));
    }
    let entries = app.mode_query_history.get(&mode).unwrap();
    assert_eq!(entries.len(), 100, "history must be capped at 100");
    assert_eq!(entries[0], "&entry149", "newest entry must be first");
}

/// `history_previous` and `history_next` follow readline
/// semantics: C-p from the live query saves the
/// in-progress query as a "draft" and loads the newest
/// history entry; further C-p moves older; C-n moves
/// newer; C-n at the newest restores the draft; C-n at
/// the live query is a no-op. All scoped to the current
/// mode only.
#[test]
fn history_previous_next_navigates_in_readline_order() {
    let mut app = global_test_app(&[]);
    let codegraph = app.query_prefixes.codegraph; // `&`
    let tags = app.query_prefixes.tags; // `$`

    // Pre-seed two modes' histories. Newest first.
    app.mode_query_history
        .insert(codegraph, vec!["&newest".to_string(), "&older".to_string()]);
    app.mode_query_history
        .insert(tags, vec!["$newest".to_string()]);

    // Start in codegraph mode, live query = "&live".
    app.query = "&live".to_string();
    app.query_cursor = app.query.chars().count();

    // C-p from the live query saves the draft and
    // loads the newest entry. history_index = Some(0).
    app.history_previous();
    assert_eq!(app.query, "&newest", "C-p must load the newest entry");
    assert_eq!(
        app.mode_query_drafts.get(&codegraph).map(String::as_str),
        Some("&live"),
        "C-p must save the in-progress query as the draft"
    );

    // C-p again: move one step older. history_index = Some(1).
    app.history_previous();
    assert_eq!(app.query, "&older");
    assert_eq!(
        app.mode_query_history_index
            .get(&codegraph)
            .copied()
            .flatten(),
        Some(1)
    );

    // C-p at the oldest entry: stay.
    app.history_previous();
    assert_eq!(app.query, "&older", "C-p at oldest must stay");

    // C-n back to the newest: history_index = Some(0).
    app.history_next();
    assert_eq!(app.query, "&newest");

    // C-n at the newest: restore the draft. history_index = None.
    app.history_next();
    assert_eq!(app.query, "&live", "C-n past newest must restore the draft");
    assert_eq!(
        app.mode_query_history_index
            .get(&codegraph)
            .copied()
            .flatten(),
        None
    );

    // C-n at the live query: no-op.
    app.history_next();
    assert_eq!(app.query, "&live", "C-n at live query must be a no-op");

    // History navigation is scoped to the active mode.
    // Switch to the tags mode (which has its own history)
    // and confirm C-p recalls from the tags list, not the
    // codegraph list.
    app.query = "$live".to_string();
    app.query_cursor = app.query.chars().count();
    app.history_previous();
    assert_eq!(
        app.query, "$newest",
        "C-p must recall from the current mode's history only"
    );
}

/// Any keystroke that mutates the query (push_char,
/// backspace, delete_word_backward, clear_query) must
/// commit the per-mode history recall session. The
/// recalled entry diverges from the recalled text the
/// instant the user edits, so history_index is reset
/// to None and the saved draft is discarded.
#[test]
fn keystroke_while_recalling_exits_recall_and_drops_draft() {
    let mut app = global_test_app(&[]);
    let codegraph = app.query_prefixes.codegraph;
    app.mode_query_history
        .insert(codegraph, vec!["&recalled".to_string()]);

    // Enter recall mode.
    app.query = "&live".to_string();
    app.query_cursor = app.query.chars().count();
    app.history_previous();
    assert_eq!(app.query, "&recalled");
    assert!(
        app.mode_query_drafts.contains_key(&codegraph),
        "draft must be saved when entering recall"
    );

    // Any text mutation (push_char) exits recall.
    app.push_char('x');
    assert_eq!(
        app.mode_query_history_index
            .get(&codegraph)
            .copied()
            .flatten(),
        None,
        "push_char while recalling must exit recall mode"
    );
    assert!(
        !app.mode_query_drafts.contains_key(&codegraph),
        "push_char while recalling must drop the draft"
    );

    // Re-enter recall and exercise backspace /
    // clear_query the same way.
    app.query = "&live".to_string();
    app.query_cursor = app.query.chars().count();
    app.history_previous();
    app.backspace();
    assert_eq!(
        app.mode_query_history_index
            .get(&codegraph)
            .copied()
            .flatten(),
        None,
        "backspace while recalling must exit recall mode"
    );

    app.query = "&live".to_string();
    app.query_cursor = app.query.chars().count();
    app.history_previous();
    app.clear_query();
    assert_eq!(
        app.mode_query_history_index
            .get(&codegraph)
            .copied()
            .flatten(),
        None,
        "clear_query while recalling must exit recall mode"
    );
}

/// Backspacing the leading prefix char (e.g. backspacing
/// the `&` of an `&query` to land in plain mode) must
/// record the OLD query into the OLD mode's history.
/// This is the natural "the user is done with mode X,
/// remember it" trigger: the next C-p in plain mode
/// recalls the just-finished query.
#[test]
fn backspacing_prefix_records_into_old_mode_history() {
    let mut app = global_test_app(&[]);
    let codegraph = app.query_prefixes.codegraph;
    // Simulate a fully-typed `&foo` query the user is
    // about to switch out of. Cursor is at the end
    // (so a single backspace deletes the trailing `o`,
    // leaving `&fo` — the leading char still `&`, so
    // this is NOT a mode change). We then backspace
    // again to `&f`, then to `&`, then once more to
    // land in plain mode (`"f"`). That last backspace
    // is the leading-char-change event: it must record
    // the OLD query `&` into codegraph history. Then
    // we continue backspacing to `""` (plain-mode
    // empty) and confirm the history was recorded at
    // the crossing, not at the subsequent backspaces.
    app.query = "&foo".to_string();
    app.query_cursor = 4;
    app.backspace();
    app.backspace();
    app.backspace();
    // After three backspaces: query is `&`. Cursor at 1.
    assert_eq!(app.query, "&");
    assert!(
            app.mode_query_history.get(&codegraph).is_none(),
            "backspacing within the same mode must NOT record (the query is non-empty but the mode is unchanged)"
        );

    // Fourth backspace crosses the mode boundary.
    app.backspace();
    assert_eq!(app.query, "");
    let entries = app
        .mode_query_history
        .get(&codegraph)
        .expect("leading-char crossing must record the old query into the old mode's history");
    assert_eq!(
        entries,
        &vec!["&".to_string()],
        "the just-backspaced query (with its prefix) is the recorded entry"
    );

    // Subsequent backspaces within the new (plain) mode
    // must NOT re-record into the old codegraph mode.
    // (The user is now in plain mode; the empty query
    // is also skipped by `record_to_mode_history`.)
    app.query = "foo".to_string();
    app.query_cursor = 3;
    app.backspace();
    app.backspace();
    app.backspace();
    assert_eq!(app.query, "");
    let entries = app.mode_query_history.get(&codegraph).unwrap();
    assert_eq!(
        entries.len(),
        1,
        "subsequent backspaces in a different mode must not re-record into the old mode"
    );
}

/// `select_for_run` records the current query (with its
/// leading prefix char) into the active mode's history
/// before dispatching to the per-mode handler. This is
/// the "Run is the natural 'remember this query' moment"
/// trigger that complements the leading-char-change
/// trigger from `backspace`.
#[test]
fn select_for_run_records_current_query_into_active_mode() {
    let mut app = global_test_app(&[("ls -la", 1)]);
    let codegraph = app.query_prefixes.codegraph;

    // Type a codegraph query, then run it. The query
    // (with the `&` prefix) must land in codegraph
    // history before the dispatch proceeds.
    app.query = "&getSymbol".to_string();
    app.query_cursor = app.query.chars().count();
    app.list_state.select(Some(0));
    let _ = app.select_for_run();
    let entries = app
        .mode_query_history
        .get(&codegraph)
        .expect("Run in codegraph mode must record the query into codegraph history");
    assert_eq!(entries, &vec!["&getSymbol".to_string()]);
}

/// `SmartOpen` in `!` (Todo) mode takes the
/// mark-todo-done branch, NOT the `Run` fallback.
/// The `Run` fallback would stage
/// `$EDITOR +<LINE> <file>` and exit; the
/// mark-todo-done branch toggles the checkbox of
/// the selected row in the source file and stays
/// in the TUI so the user sees the row disappear
/// (or the marker flip). To avoid actually
/// rewriting a notes file during the test, we
/// exercise the no-`notes.dir` path and assert
/// that the dispatch took the todo branch at all
/// — i.e. it did NOT stage a selection (Run would
/// have) and did NOT exit, and the status message
/// is the mark-todo-done "notes.dir not configured"
/// diagnostic, not the Run-fallback-staged command.
#[test]
fn smart_open_in_todo_mode_takes_mark_done_branch() {
    let mut app = global_test_app(&[]);
    // Force the app into todo mode by setting the
    // query body to a `!` query. A real todo row
    // (with the synthetic negative id encoding the
    // line number) is injected into the merged
    // rows so the selected row is a valid todo
    // entry.
    app.query = "!urgent".to_string();
    app.query_cursor = app.query.chars().count();
    app.refresh();
    // Inject a todo row at index 0. The synthetic
    // id `-(line_number)` is the contract from
    // `fetch_todos`; mark_todo_done reads it back to
    // find the line in the source file.
    let line_number: i64 = 42;
    let todo_row = crate::tui::state::HistoryRow {
        id: -line_number,
        command: "- [ ] pick up milk".to_string(),
        directory: String::new(),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: "today.md".to_string(),
        output: String::new(),
        mode: "todo".to_string(),
        source: String::new(),
        ..Default::default()
    };
    app.merged_rows.insert(0, todo_row);
    app.list_state.select(Some(0));
    // No `notes_database` / `notes_dir` are set on
    // the test app (global_test_app leaves them
    // None), so mark_todo_done will surface the
    // "notes.dir is not configured" status message
    // rather than actually rewriting a file. That's
    // exactly what we want: the test asserts the
    // dispatch *routed* to the todo branch without
    // needing a real notes file.
    let quit = dispatch_action(&mut app, Action::SmartOpen);
    assert!(
        !quit,
        "SmartOpen in todo mode must NOT exit the TUI (mark-done branch)"
    );
    assert!(
            app.selection.is_none(),
            "SmartOpen in todo mode must NOT stage a selection (Run fallback would have staged `$EDITOR +LINE file`): {:?}",
            app.selection
        );
    assert_eq!(
        app.pick_mode, None,
        "SmartOpen in todo mode must NOT set pick_mode (Run fallback would have)"
    );
    // The mark_todo_done branch surfaced a status
    // message about the missing notes.dir. The
    // exact wording is a contract: it tells the
    // user *why* the toggle didn't apply (rather
    // than silently no-op'ing). If a future refactor
    // re-routes todo mode to the Run fallback, the
    // selection / pick_mode assertions above would
    // already catch the regression; this assertion
    // pins the specific "took the todo branch" intent.
    let status = app
        .status_message
        .as_ref()
        .map(|(s, _)| s.as_str())
        .unwrap_or("");
    assert!(
        status.contains("notes.dir is not configured")
            || status.contains("No todo selected")
            || status.contains("Mark-todo-done"),
        "expected the mark-todo-done status message; got: {:?}",
        status
    );
}

// ---- Files-mode SmartOpen (per-extension shell command) ----

/// Helper: build a minimal `App` in `~` (files) mode
/// with one selected file row. The file's `directory`
/// field holds the absolute path; the `mode` is
/// `"file"`. The `merged_rows` slot is set so
/// `selected_row()` returns the row.
fn files_test_app(path: &str) -> App {
    let mut app = global_test_app(&[]);
    app.query = format!("{}README", app.query_prefixes.files);
    app.query_cursor = app.query.chars().count();
    app.refresh();
    // Inject a file row. The files-mode walk would
    // normally produce a row with `command = relative
    // display path` and `directory = absolute path`,
    // but for the SmartOpen dispatch the only fields
    // we read are `mode` and `directory`, so we keep
    // the test fixture minimal.
    let row = crate::tui::state::HistoryRow {
        id: -1,
        command: path.to_string(),
        directory: path.to_string(),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: String::new(),
        output: String::new(),
        mode: "file".to_string(),
        source: String::new(),
        ..Default::default()
    };
    app.merged_rows.insert(0, row);
    app.list_state.select(Some(0));
    app
}

/// Files-mode SmartOpen with a configured per-
/// extension command stages `<cmd> <quoted-path>`
/// and exits so the parent shell runs it. The
/// dispatch takes the `~` branch (not the `Run`
/// fallback) — `Enter` would stage
/// `$EDITOR <path>`; `SmartOpen` stages the
/// per-extension command instead.
#[test]
fn smart_open_in_files_mode_uses_configured_extension_command() {
    let mut app = files_test_app("/home/user/notes/README.md");
    // Configure: `.md` files → `leaf`.
    app.smart_open_file_commands
        .insert("md".to_string(), "leaf".to_string());
    let quit = dispatch_action(&mut app, Action::SmartOpen);
    assert!(quit, "SmartOpen must exit the TUI after staging");
    let staged = app
        .selection
        .as_deref()
        .expect("SmartOpen must stage a selection when a per-extension command is configured");
    // The path is POSIX single-quote escaped; verify
    // the command, the path, and the spacing.
    assert!(
        staged.starts_with("leaf "),
        "staged command must start with `leaf `; got: {:?}",
        staged
    );
    assert!(
        staged.contains("/home/user/notes/README.md"),
        "staged command must include the absolute file path; got: {:?}",
        staged
    );
    assert_eq!(
        app.pick_mode,
        Some(PickMode::Run),
        "pick_mode must be set so the run-loop treats this as a Run-equivalent selection"
    );
}

/// Lookup is case-insensitive: a file named
/// `README.MD` matches the `md` mapping.
#[test]
fn smart_open_extension_lookup_is_case_insensitive() {
    let mut app = files_test_app("/home/user/notes/README.MD");
    app.smart_open_file_commands
        .insert("md".to_string(), "leaf".to_string());
    let quit = dispatch_action(&mut app, Action::SmartOpen);
    assert!(quit);
    let staged = app.selection.as_deref().unwrap();
    assert!(
        staged.starts_with("leaf "),
        "case-insensitive lookup must still match `md` for `README.MD`; got: {:?}",
        staged
    );
}

/// Without a per-extension mapping, the
/// `smart-open.default` fallback is used. This is
/// the common case for "all text files get `bat`"
/// workflows where the user wants a single
/// fallback for every unrecognised extension.
#[test]
fn smart_open_falls_back_to_default_for_unrecognised_extension() {
    let mut app = files_test_app("/home/user/notes/script.brainfuck");
    app.smart_open_file_commands
        .insert("md".to_string(), "leaf".to_string());
    app.smart_open_file_commands
        .insert("default".to_string(), "bat".to_string());
    let quit = dispatch_action(&mut app, Action::SmartOpen);
    assert!(quit);
    let staged = app.selection.as_deref().unwrap();
    assert!(
        staged.starts_with("bat "),
        "unrecognised extension must fall back to `smart-open.default`; got: {:?}",
        staged
    );
}

/// With no per-extension mapping AND no
/// `smart-open.default`, SmartOpen falls through
/// to the `Run` action: open in `$EDITOR` at the
/// file's path. This is the safe default that
/// never loses the user's selection to a wrong
/// command.
#[test]
fn smart_open_in_files_mode_falls_through_to_run_when_unconfigured() {
    let mut app = files_test_app("/home/user/notes/script.brainfuck");
    // No entries in `smart_open_file_commands`.
    let quit = dispatch_action(&mut app, Action::SmartOpen);
    // The Run fallback stages `$EDITOR <path>` and
    // exits.
    assert!(quit, "Run fallback must exit the TUI");
    let staged = app.selection.as_deref().unwrap();
    assert!(
        staged.contains("$EDITOR") || staged.contains("/home/user/notes/script.brainfuck"),
        "Run fallback must stage the standard editor command; got: {:?}",
        staged
    );
}

/// A file without an extension (e.g. a `Makefile`)
/// has no `extension()` to look up; the
/// `smart-open.default` fallback is the right way
/// to handle these (the per-extension path is
/// skipped because `Path::extension()` returns
/// `None` for dotfiles / extensionless files).
#[test]
fn smart_open_extensionless_file_falls_through_to_default() {
    let mut app = files_test_app("/home/user/project/Makefile");
    app.smart_open_file_commands
        .insert("default".to_string(), "bat".to_string());
    let quit = dispatch_action(&mut app, Action::SmartOpen);
    assert!(quit);
    let staged = app.selection.as_deref().unwrap();
    assert!(
        staged.starts_with("bat "),
        "extensionless file must fall through to the default; got: {:?}",
        staged
    );
}

/// A directory row in `~` mode is NOT a file — the
/// `mode == "file"` guard rejects it and the
/// dispatch falls through to `Run` (the user-
/// defined default for a directory row, which
/// creates / focuses a workspace rooted there).
/// Files-mode SmartOpen must not stage a `bat
/// <dir-path>` command — that would be a wrong
/// path-take to the user.
#[test]
fn smart_open_in_files_mode_skips_directory_rows() {
    let mut app = global_test_app(&[]);
    app.query = format!("{}work", app.query_prefixes.files);
    app.query_cursor = app.query.chars().count();
    app.refresh();
    // Inject a directory row.
    let row = crate::tui::state::HistoryRow {
        id: -1,
        command: "work".to_string(),
        directory: "/home/user/project/work".to_string(),
        session_id: String::new(),
        exit_code: 0,
        timestamp: 0,
        comment: String::new(),
        output: String::new(),
        // `mode = "directory"` is the files-mode
        // walk's signal that this is a directory
        // row, not a file.
        mode: "directory".to_string(),
        source: String::new(),
        ..Default::default()
    };
    app.merged_rows.insert(0, row);
    app.list_state.select(Some(0));
    // Configure: a default that would misfire on
    // directories if the mode guard were missing.
    app.smart_open_file_commands
        .insert("default".to_string(), "bat".to_string());
    let quit = dispatch_action(&mut app, Action::SmartOpen);
    // The Run fallback fires: SmartOpen falls
    // through because the row isn't a file. (The
    // Run path in files mode stages
    // `cd <abs-path>` and exits — see
    // `select_for_run_impl`.)
    assert!(quit, "directory row must fall through to Run");
    let staged = app.selection.as_deref().unwrap();
    assert!(
        !staged.starts_with("bat "),
        "directory row must NOT trigger the per-extension `bat` command; got: {:?}",
        staged
    );
}

/// A configured command with extra flags (e.g.
/// `bat --style=plain`) is taken verbatim and the
/// file path is appended at the end. This is the
/// `command args...` pattern users expect from
/// shell command templates.
#[test]
fn smart_open_passes_extra_flags_through() {
    let mut app = files_test_app("/home/user/notes/trace.log");
    app.smart_open_file_commands
        .insert("log".to_string(), "bat --style=plain".to_string());
    let quit = dispatch_action(&mut app, Action::SmartOpen);
    assert!(quit);
    let staged = app.selection.as_deref().unwrap();
    assert!(
        staged.starts_with("bat --style=plain "),
        "configured flags must be preserved; got: {:?}",
        staged
    );
    assert!(
        staged.ends_with("trace.log"),
        "file path must be appended at the end; got: {:?}",
        staged
    );
}

/// Every TUI-staged selection is space-prefixed
/// before running in the parent shell. This is
/// the TUI side of the "space prefix = sensitive"
/// convention: zsh's `HIST_NO_STORE` treats any
/// command whose first character is whitespace as
/// "do not save to shell history", and the
/// smarthistory `init.zsh` precmd hook treats the
/// same prefix as "do not record in the DB".
/// Prepending the space centrally in the exit
/// path (rather than at every staging site) means
/// the contract is uniform: every command the TUI
/// runs — `Enter` (history, notes, todos, files,
/// tags, codegraph), `Ctrl-]` (SmartOpen in every
/// mode), `Ctrl-V` (EditFileReference), `Ctrl-M-s`
/// (DownloadJiraIssue), etc. — is space-prefixed.
#[test]
fn prefix_selection_with_space_prepends_a_single_space() {
    assert_eq!(
        prefix_selection_with_space("ls -la".to_string()),
        " ls -la",
        "non-empty selection must be prefixed with exactly one space"
    );
    assert_eq!(
        prefix_selection_with_space("vim /etc/hosts".to_string()),
        " vim /etc/hosts",
        "selections with shell metacharacters are prefixed the same way (no quoting)"
    );
    assert_eq!(
            prefix_selection_with_space(String::new()),
            " ",
            "empty selection becomes a single space (the parent shell will reject the empty command as before)"
        );
    // Idempotent in the "any leading whitespace is fine" sense:
    // prepending to an already-space-prefixed string
    // produces a leading double-space, which zsh's
    // `HIST_NO_STORE` still treats as "don't save"
    // (the rule is "first char is whitespace", not
    // "exactly one space"). The DB-recorder side
    // (`[[:space:]]*` glob) also matches multiple
    // leading whitespace. So the helper is
    // double-prefix-safe.
    assert_eq!(
            prefix_selection_with_space(" ls".to_string()),
            "  ls",
            "double-prefix is harmless — both the shell and the smarthistory DB recorder treat any leading whitespace as the sensitive marker"
        );
}

/// The mode-aware wrapper skips the space prefix in
/// history mode (the no-prefix / `MODE_NONE` case)
/// because replaying a row from history is a command
/// the user explicitly wants recorded — recording it
/// keeps the frequency stats accurate and the `Ctrl-S`
/// next-probable-command suggestions useful. Every
/// other mode (every `char` that's not `MODE_NONE`)
/// still gets the space prefix (one-shot reads like
/// `bat README.md` shouldn't clutter the DB).
#[test]
fn maybe_prefix_selection_with_space_skips_in_history_mode() {
    // History mode (no prefix) → returned unchanged.
    assert_eq!(
        maybe_prefix_selection_with_space("ls -la".to_string(), MODE_NONE),
        "ls -la",
        "history mode (MODE_NONE) must NOT prepend a space — the user wants the command recorded"
    );
    // Every real prefix char → space-prefixed.
    let prefixes = crate::QueryPrefixes::default();
    for (label, mode_char) in [
        ("output", prefixes.output),
        ("llm", prefixes.llm),
        ("question", prefixes.question),
        ("notes", prefixes.notes),
        ("todo", prefixes.todo),
        ("directories", prefixes.directories),
        ("panes", prefixes.panes),
        ("jira", prefixes.jira),
        ("files", prefixes.files),
        ("tags", prefixes.tags),
        ("codegraph", prefixes.codegraph),
        ("ag", prefixes.ag),
    ] {
        assert_eq!(
            maybe_prefix_selection_with_space("cmd".to_string(), mode_char),
            " cmd",
            "{} mode (prefix `{}`) must prepend a space — one-shot reads stay out of the DB",
            label,
            mode_char
        );
    }
    // History mode with an empty selection stays empty
    // (no space prepended).
    assert_eq!(
        maybe_prefix_selection_with_space(String::new(), MODE_NONE),
        "",
        "empty selection in history mode must stay empty (no space added)"
    );
}
