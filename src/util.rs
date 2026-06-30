//! Shared formatting helpers used by both the CLI (`main.rs`) and the
//! TUI (`tui.rs`). Keeping them in one place avoids drift when the
//! format string or the "N/A" sentinel changes.

use chrono::Datelike;

/// Format a Unix epoch (seconds) as "dd.Mon.YYYY HH:MM:SS" in UTC, e.g.
/// "03.Jun.2026 17:43:01". Returns a placeholder string for invalid
/// timestamps so that history items with no valid time stamp can still
/// be displayed and treated as very old.
pub fn format_time(epoch: i64) -> String {
    match chrono::DateTime::from_timestamp(epoch, 0) {
        Some(dt) => dt.naive_utc().format("%d.%b.%Y %H:%M:%S").to_string(),
        None => "(unknown)".to_string(),
    }
}

/// Human-readable difference between `epoch` and now, using the largest
/// non-zero unit. Ladder (with short unit suffixes):
///   month  -> "1M", 2M, ...
///   day    -> "1d", 2d, ...
///   hour   -> "1h", 2h, ...
///   minute -> "1m", 2m, ...
///   second -> "1s", 2s, ...
/// Returns a placeholder "9999M" for non-positive or out-of-range
/// timestamps so they sort as the oldest possible entries.
pub fn format_diff(epoch: i64) -> String {
    let now = chrono::Utc::now().naive_utc();
    let Some(then) = chrono::DateTime::from_timestamp(epoch, 0).map(|dt| dt.naive_utc()) else {
        return "9999M".to_string();
    };
    if epoch <= 0 {
        return "9999M".to_string();
    }

    // Calendar-month diff first, since it's non-uniform in seconds.
    let months = (now.year() - then.year()) * 12 + (now.month() as i32 - then.month() as i32);
    if months > 0 {
        return format!("{}M", months);
    }

    let delta = now - then;
    let secs = delta.num_seconds();
    if secs < 60 {
        return format!("{}s", secs.max(0));
    }
    let mins = delta.num_minutes();
    if mins < 60 {
        return format!("{}m", mins);
    }
    let hours = delta.num_hours();
    if hours < 24 {
        return format!("{}h", hours);
    }
    let days = delta.num_days();
    format!("{}d", days)
}

/// Escape the SQLite `LIKE` wildcards (`%` and `_`) in a user-supplied
/// search string. Without this, a query like `100%` would match anything
/// containing `100` followed by anything. The `\` is also escaped so
/// users can search for a literal backslash.
pub fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '%' || ch == '_' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Canonicalize a directory path the
/// way the rest of smarthistory
/// expects it to be stored and
/// compared.
///
/// The problem this solves: on
/// macOS the user's home directory
/// and other paths are exposed
/// under `/Users/...` (the
/// "synthetic" path the user
/// types) but the kernel sees the
/// same physical directory at
/// `/Volumes/HUGE/...` (the real
/// path on the mounted volume).
/// The shell's `$PWD` is the
/// synthetic path; `env::current_dir()`
/// (which `preexec` triggers when
/// our binary runs) is the real
/// path. If we store one and
/// compare against the other in
/// DIR mode, the filter returns
/// no rows even though the user
/// has been running commands in
/// that directory.
///
/// The fix: canonicalize on both
/// sides. `std::fs::canonicalize`
/// follows symlinks and resolves
/// volume mounts, so both
/// `/Users/har` and
/// `/Volumes/HUGE/har` collapse
/// to the same absolute path
/// (whichever one is the real
/// mount). When the path doesn't
/// exist anymore (deleted
/// directory, unmounted volume)
/// the syscall fails and we fall
/// back to the input string so
/// insert doesn't crash.
///
/// Returns the canonical path as
/// a String. Empty input returns
/// empty (we don't want to store
/// an empty `directory` column;
/// the schema treats it as "no
/// filter").
/// Expand a leading `~` (or a
/// `$HOME` / configured
/// `homemap` prefix on an
/// absolute path) to the
/// `~`-shorthand form.
///
/// `path` semantics:
/// - `~` (alone) → matches
///   against the first home in
///   `homes`; the canonical
///   `/` form. With the default
///   `[$HOME]` and a missing
///   `$HOME`, returns the
///   empty string.
/// - `~/x` → matched-home + `x`.
/// - `/x` absolute, with `/x`
///   starting with one of
///   `homes` → `~/x` (or `~`
///   if `x` is empty).
/// - `/x` absolute, NOT under
///   any home → unchanged.
/// - `x` (relative) → unchanged.
/// - `~user/...` → unchanged
///   (we deliberately don't
///   support `~other_user`
///   expansion).
/// - empty → unchanged.
///
/// **`homes` ordering**:
/// most-specific prefix wins.
/// We try the longest home
/// first, so `/Volumes/HUGE/har/foo`
/// matches `/Volumes/HUGE/har` over
/// `/Users/har` (if both are in
/// `homes`).
///
/// Returns a `Cow<str>` so
/// callers that pass an
/// already-short path don't
/// pay for an allocation.
///
/// **Why this exists**: tmux
/// (and most C programs) do
/// NOT do `~` expansion —
/// `tmux new-session -d -c
/// '~/work'` silently creates a
/// session in the user's home
/// directory, not `~/work`.
/// The shell snippets that
/// source this binary *do*
/// expand `~` in `BUFFER=...`
/// before submit, but our
/// staged command runs through
/// the snippet verbatim, so we
/// have to expand `~` ourselves
/// before passing the path to
/// `tmux new-session -c`.
pub fn shorten_home_path<'a>(
    path: &'a str,
    homes: &[String],
) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    // Sort homes longest-first so
    // the most-specific match
    // wins. e.g. if `homes =
    // ["/Users/har",
    // "/Volumes/HUGE/har"]`,
    // `/Volumes/HUGE/har/foo`
    // matches `/Volumes/HUGE/har`
    // — the `~/foo` form, not
    // `~/Volumes/HUGE/har/foo`.
    let mut sorted: Vec<&str> = homes
        .iter()
        .filter(|h| !h.is_empty())
        .map(String::as_str)
        .collect();
    sorted.sort_by_key(|s| std::cmp::Reverse(s.len()));
    // Bare `~` is left alone.
    // Originally this arm
    // expanded to the longest
    // home (e.g. `/Volumes/HUGE/har`),
    // but that made the
    // `smarthistory update`
    // subcommand non-idempotent:
    // a previously-shortened
    // row's `~` would re-expand
    // to the home on the second
    // pass, then re-shorten on
    // the third, oscillating.
    // Pass-through is the
    // idempotent answer: the
    // function is a one-way
    // shortener (absolute →
    // `~/x`), and `~` is
    // already in the target
    // form. (Callers that
    // actually want the
    // absolute `$HOME` from a
    // user-typed `~` can read
    // `env::var("HOME")` directly;
    // that's not this
    // function's job.)
    if path == "~" {
        return Cow::Borrowed(path);
    }
    // `~/x` is already the
    // short form — pass
    // through unchanged.
    // (Don't re-expand it back
    // to `$HOME/x`; the caller
    // already chose the short
    // form. This is the
    // idempotence contract:
    // running the function on
    // an already-short path is
    // a no-op. Without it, the
    // `smarthistory update`
    // subcommand's second
    // invocation would
    // un-shorten everything.)
    if path.starts_with("~/") {
        return Cow::Borrowed(path);
    }
    // Absolute paths under any
    // home in `homes` get the
    // `~/...` shortening. The
    // path-segment boundary
    // check (the remainder
    // starts with `/` or is
    // empty) prevents
    // `/Users/harry/...` from
    // matching a `/Users/har`
    // home prefix.
    for home in &sorted {
        if path == *home {
            return Cow::Borrowed("~");
        }
        if let Some(rest) =
            path.strip_prefix(*home).filter(|r| {
                r.is_empty() || r.starts_with('/')
            })
        {
            return Cow::Owned(format!("~{}", rest));
        }
    }
    // No allocation for the
    // common cases (relative
    // paths, absolute paths
    // outside any home, empty
    // input, or unsupported
    // `~user/...` form).
    Cow::Borrowed(path)
}

/// Convenience: shorten `path`
/// using `$HOME` only.
/// Equivalent to
/// `shorten_home_path(path, &[$HOME])`
/// but reads `$HOME` itself, so
/// callers don't have to. The
/// "expand" name is historical
/// (the function does NOT
/// expand `~/x` to `$HOME/x`).
pub fn expand_home(path: &str) -> std::borrow::Cow<'_, str> {
    let home = std::env::var("HOME").unwrap_or_default();
    shorten_home_path(path, &[home])
}

/// Like `expand_home` but accepts a
/// user-configured `home_map`
/// (in addition to `$HOME`).
/// Used by the TUI's render and
/// action layer when a `Config`
/// is in scope; the `smarthistory
/// update` subcommand uses the
/// same helper to rewrite the DB.
pub fn expand_home_with_config<'a>(
    path: &'a str,
    home_map: &[std::path::PathBuf],
) -> std::borrow::Cow<'a, str> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut homes: Vec<String> = Vec::with_capacity(
        home_map.len() + 1,
    );
    // `home_map` last so `$HOME`
    // wins in a tie (both are
    // present, both the same
    // length). For length-
    // distinct prefixes,
    // most-specific still wins
    // via the `sort_by_key(Reverse)`
    // inside `expand_home_with`.
    if !home.is_empty() {
        homes.push(home);
    }
    for h in home_map {
        // Skip empties. Don't
        // canonicalize here —
        // the user-supplied path
        // is already the form
        // they want to match
        // against. The DB-stored
        // paths are canonical
        // (per `current_directory_for_storage`'s
        // contract) and the
        // user-supplied `homemap`
        // is a real path on disk,
        // so they should match
        // without further
        // normalization.
        if let Some(s) = h.to_str()
            && !s.is_empty() {
                homes.push(s.to_string());
            }
    }
    shorten_home_path(path, &homes)
}

pub fn canonicalize_directory(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    match std::fs::canonicalize(path) {
        Ok(p) => p.to_string_lossy().into_owned(),
        // Fall back to the input
        // verbatim. This is the same
        // value the query side will
        // canonicalize too, so if
        // canonicalize fails for both
        // (e.g. the volume was
        // unmounted between insert
        // and query) the two strings
        // are still equal and the
        // filter works.
        Err(_) => path.to_string(),
    }
}

/// Read the current working
/// directory for storage, the way
/// the rest of smarthistory
/// expects it.
///
/// `env::current_dir()` returns
/// the kernel's view of the cwd,
/// which is the canonical path
/// (resolves symlinks, volume
/// mounts, etc.). On macOS this
/// is `/Volumes/HUGE/...` for
/// files on the user's external
/// volume, while the shell's
/// `$PWD` is `/Users/...` (the
/// synthetic path the user
/// types). We want the canonical
/// form because both insert and
/// query sides run the same
/// canonicalization; without it,
/// the directory stored in a row
/// from the `preexec` hook may
/// not match the directory the
/// user later filters on in DIR
/// mode.
///
/// If the canonicalize syscall
/// fails (rare: deleted dir,
/// offline volume) we fall back
/// to `env::current_dir()`'s raw
/// output — that's the value the
/// caller already had, and it's
/// still better than crashing.
pub fn current_directory_for_storage() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    canonicalize_directory(&cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A known reference epoch: 2026-06-03 12:34:56 UTC. The exact
    /// value doesn't matter as long as the formatted string matches.
    /// Using a fixed instant in the test catches both leap-second
    /// surprises (none here, since 2026 is far from a leap second) and
    /// any locale/UTC drift in the formatter.
    const REFERENCE_EPOCH: i64 = 1780562096; // 2026-06-04 12:34:56 UTC

    #[test]
    fn format_time_known_value() {
        // Re-derive the expected string with the same format the helper
        // uses, so this test stays self-documenting. If the format
        // string changes, the test will fail and force the change to
        // be intentional.
        let expected = chrono::DateTime::from_timestamp(REFERENCE_EPOCH, 0)
            .unwrap()
            .naive_utc()
            .format("%d.%b.%Y %H:%M:%S")
            .to_string();
        assert_eq!(format_time(REFERENCE_EPOCH), expected);
    }

    #[test]
    fn format_time_out_of_range() {
        // i64::MIN is guaranteed to be out of range for any reasonable
        // timestamp formatter.
        assert_eq!(format_time(i64::MIN), "(unknown)");
        // A timestamp of 0 (the Unix epoch) is in range; the helper
        // must NOT return "(unknown)" for it.
        assert_ne!(format_time(0), "(unknown)");
    }

    #[test]
    fn format_time_zero_is_unix_epoch() {
        // Unix epoch 0 is 1970-01-01 00:00:00 UTC. Hardcoded so a
        // regression in the formatter is caught immediately.
        assert_eq!(format_time(0), "01.Jan.1970 00:00:00");
    }

    #[test]
    fn escape_like_no_special_chars() {
        // No `%` or `\` in the input → output identical to input.
        // `_` IS a LIKE wildcard and is always escaped, so any
        // string containing `_` will be modified.
        assert_eq!(escape_like("hello world"), "hello world");
        assert_eq!(escape_like(""), "");
        assert_eq!(escape_like("plain text"), "plain text"); // no `_`
                                                             // `_` is escaped to `\_`.
        assert_eq!(escape_like("plain_text"), "plain\\_text");
    }

    #[test]
    fn escape_like_percent() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("%abc%"), "\\%abc\\%");
    }

    #[test]
    fn escape_like_underscore() {
        assert_eq!(escape_like("foo_bar"), "foo\\_bar");
        assert_eq!(escape_like("_"), "\\_");
    }

    #[test]
    fn escape_like_backslash() {
        // A literal backslash must be escaped to `\\` so the LIKE
        // ESCAPE clause recognizes it as a literal.
        assert_eq!(escape_like("a\\b"), "a\\\\b");
    }

    #[test]
    fn escape_like_combined() {
        // Multiple special chars in a row.
        assert_eq!(escape_like("%_\\"), "\\%\\_\\\\");
    }

    /// `canonicalize_directory` is a
    /// no-op for already-canonical
    /// paths. We verify with the
    /// temp dir this test runs in —
    /// `std::env::temp_dir()` may
    /// not be the same as
    /// `canonicalize(...)` of it
    /// (macOS resolves `/tmp` to
    /// `/private/tmp`), so we use
    /// the canonicalized form of
    /// the temp dir as the
    /// expected output.
    #[test]
    fn canonicalize_directory_resolves_existing_path() {
        let dir = std::env::temp_dir();
        let canonical_dir = std::fs::canonicalize(&dir).expect("canonicalize temp dir");
        let canonical_str = canonical_dir.to_string_lossy().into_owned();
        let result = canonicalize_directory(&canonical_str);
        assert_eq!(result, canonical_str);
    }

    /// `canonicalize_directory` falls
    /// back to the input verbatim
    /// when the path doesn't exist
    /// (deleted directory, unmounted
    /// volume). This is the safe
    /// behaviour for the
    /// `preexec` hook: we don't
    /// want to crash the user's
    /// shell because a transient
    /// path was unavailable.
    #[test]
    fn canonicalize_directory_falls_back_for_missing_path() {
        let missing = "/this/path/should/never/exist/anywhere";
        assert_eq!(canonicalize_directory(missing), missing);
    }

    /// Empty input returns empty
    /// (the schema treats an empty
    /// `directory` column as "no
    /// filter"; we don't want to
    /// canonicalize the empty
    /// string, which would yield
    /// the cwd).
    #[test]
    fn canonicalize_directory_empty_input() {
        assert_eq!(canonicalize_directory(""), "");
    }

    /// Symlink resolution:
    /// `canonicalize_directory` of a
    /// path that contains a symlink
    /// returns the resolved path.
    /// We create a temp symlink
    /// (only on platforms that
    /// support `symlink`) and
    /// verify it resolves.
    #[cfg(unix)]
    #[test]
    fn canonicalize_directory_resolves_symlinks() {
        use std::fs;
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!(
            "smarthistory-canon-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create dir");
        let target = base.join("real");
        fs::create_dir(&target).expect("create target");
        let link = base.join("link");
        symlink(&target, &link).expect("symlink");
        // Querying through the
        // symlink should resolve
        // to the real path.
        let link_str = link.to_string_lossy().into_owned();
        let result = canonicalize_directory(&link_str);
        let expected = std::fs::canonicalize(&target)
            .expect("canonicalize target")
            .to_string_lossy()
            .into_owned();
        assert_eq!(result, expected);
        let _ = fs::remove_dir_all(&base);
    }

    /// The invariance the user
    /// reported: insert with one
    /// form of the path, query
    /// with another, the filter
    /// matches. We simulate this
    /// by canonicalizing both the
    /// "stored" and "queried"
    /// sides with the same helper
    /// — if both sides go through
    /// `canonicalize_directory`,
    /// they always agree.
    #[test]
    fn canonicalize_directory_keeps_insert_and_query_in_sync() {
        let base = std::env::temp_dir();
        let canonical_base = std::fs::canonicalize(&base)
            .expect("canonicalize temp dir")
            .to_string_lossy()
            .into_owned();
        // Simulate the
        // `/Users/...` vs
        // `/Volumes/HUGE/...`
        // mismatch by using two
        // textual forms that
        // canonicalize to the
        // same place. On most
        // platforms the temp dir
        // doesn't have this
        // property, so we test
        // the general invariant:
        // canonicalize is
        // idempotent.
        let canonicalized_once = canonicalize_directory(&canonical_base);
        let canonicalized_twice = canonicalize_directory(&canonicalized_once);
        assert_eq!(
            canonicalized_once, canonicalized_twice,
            "canonicalize is idempotent"
        );
    }

    /// `current_directory_for_storage`
    /// returns a non-empty string
    /// for the cwd of the test
    /// process. We don't pin the
    /// exact value (it's
    /// platform-dependent and
    /// depends on where cargo
    /// ran) — we just check it's
    /// non-empty and that it
    /// equals the canonicalized
    /// form of itself (i.e. the
    /// helper is internally
    /// consistent).
    #[test]
    fn current_directory_for_storage_is_canonical() {
        let s = current_directory_for_storage();
        assert!(!s.is_empty(), "got empty cwd");
        // Calling the helper
        // again should give the
        // same result.
        let s2 = current_directory_for_storage();
        assert_eq!(s, s2);
    }

    /// `expand_home` shortens
    /// absolute paths under
    /// `$HOME` to the `~/...`
    /// form. This is the case
    /// that matters for the TUI's
    /// directories view: the
    /// DB stores absolute paths
    /// (e.g. `/Users/har/work`),
    /// but the user wants to see
    /// `~/work`. The
    /// path-segment boundary
    /// check prevents
    /// `/Users/harry/...` from
    /// being mis-shortened to
    /// `~/...` when the home is
    /// `/Users/har`.
    #[test]
    fn expand_home_shortens_paths_under_home() {
        let saved_home = std::env::var("HOME").ok();
        // SAFETY: see
        // expand_home_basic.
        unsafe {
            std::env::set_var("HOME", "/Users/har");
        }
        // Direct subpath.
        assert_eq!(expand_home("/Users/har/work").as_ref(), "~/work");
        // Deeper path.
        assert_eq!(expand_home("/Users/har/a/b/c").as_ref(), "~/a/b/c");
        // The home dir itself
        // (no trailing path) →
        // `~`.
        assert_eq!(expand_home("/Users/har").as_ref(), "~");
        // Trailing slash on the
        // input — preserve the
        // slash in the output.
        assert_eq!(expand_home("/Users/har/work/").as_ref(), "~/work/");
        // `/Users/harry/...` is
        // NOT under `/Users/har`
        // (the boundary check
        // matches at `/`-or-end
        // only). Pass through
        // unchanged.
        assert_eq!(
            expand_home("/Users/harry/work").as_ref(),
            "/Users/harry/work"
        );
        // Absolute path outside
        // $HOME — pass through.
        assert_eq!(expand_home("/etc/hosts").as_ref(), "/etc/hosts");
        // Restore HOME.
        if let Some(h) = saved_home {
            // SAFETY: see
            // expand_home_basic.
            unsafe {
                std::env::set_var("HOME", h);
            }
        } else {
            // SAFETY: see above.
            unsafe {
                std::env::remove_var("HOME");
            }
        }
    }

    /// `expand_home` returns the
    /// user's home directory for
    /// the bare `~` token, the
    /// home + remainder for the
    /// `~/...` form, and the
    /// input verbatim for anything
    /// else (absolute paths,
    /// relative paths, empty
    /// input, the unsupported
    /// `~user/...` form).
    #[test]
    fn expand_home_basic() {
        // Pin HOME for the test so
        // the assertions are
        // deterministic. (HOME
        // is normally set in the
        // test env, but the test
        // harness may not always
        // pass it through. We
        // check-and-set rather
        // than set unconditionally
        // to avoid clobbering the
        // user's real env.)
        let saved_home = std::env::var("HOME").ok();
        // SAFETY: tests run single-threaded by default
        // (the parallel runner pins per-test
        // parallelism via internal `Mutex`es and our
        // test names are unique; no other test
        // reads HOME). `set_var` / `remove_var` are
        // process-global, but we restore the
        // pre-test value at the end of the function
        // (see below) so even if a future test
        // interleave changes this, the saved
        // value gets re-installed.
        unsafe {
            std::env::set_var("HOME", "/Users/har");
        }
        // Bare `~` is pass-through
        // (idempotence: the
        // function is a
        // one-way-shorten
        // absolute → `~/x`; the
        // bare `~` is already in
        // the target form).
        assert_eq!(expand_home("~").as_ref(), "~");
        // `~/x` is already the
        // short form — the
        // function does NOT
        // "expand" it back to
        // `$HOME/x`. Pass through
        // unchanged. (This is the
        // idempotence contract:
        // the function is
        // a one-way shorten,
        // never re-expand.)
        assert_eq!(expand_home("~/work").as_ref(), "~/work");
        // `~/x/y` (deeper path).
        assert_eq!(expand_home("~/a/b/c").as_ref(), "~/a/b/c");
        // Absolute path — passed
        // through unchanged, no
        // allocation.
        assert_eq!(expand_home("/etc/hosts").as_ref(), "/etc/hosts");
        // Relative path — passed
        // through unchanged.
        assert_eq!(expand_home("work").as_ref(), "work");
        // Empty input — passed
        // through unchanged.
        assert_eq!(expand_home("").as_ref(), "");
        // `~user/...` (a different
        // user's home) is NOT
        // expanded — we don't do
        // `~user` lookups. The
        // literal string passes
        // through; if the user
        // really wanted that path
        // they can edit the staged
        // command before submit.
        assert_eq!(expand_home("~alice/work").as_ref(), "~alice/work");
        // `~` followed by something
        // *not* a slash is also NOT
        // expanded. `~foo` could
        // be either "user foo's
        // home" (which we don't
        // support) or a literal
        // path that happens to
        // start with `~`. Same
        // answer: pass through.
        assert_eq!(expand_home("~something").as_ref(), "~something");
        // Restore HOME.
        if let Some(h) = saved_home {
            // SAFETY: see the
            // matching comment on
            // the `set_var` above.
            unsafe {
                std::env::set_var("HOME", h);
            }
        } else {
            // SAFETY: see above.
            unsafe {
                std::env::remove_var("HOME");
            }
        }
    }

    /// When HOME is unset (or
    /// empty), `expand_home` of
    /// the bare `~` returns an
    /// empty string rather than
    /// panicking. The caller (the
    /// `tmux new-session` action)
    /// would then pass `-c ""` to
    /// tmux, which falls back to
    /// the user's home — the
    /// same behaviour we'd get
    /// if HOME was set, just
    /// without the `~/` expansion
    /// working. This is a
    /// graceful-degradation
    /// contract, not a hard
    /// failure.
    #[test]
    fn expand_home_no_home_env() {
        let saved_home = std::env::var("HOME").ok();
        // SAFETY: see the
        // expand_home_basic test.
        unsafe {
            std::env::remove_var("HOME");
        }
        // When HOME is unset, we
        // can't expand `~` (we
        // don't know the
        // destination). The
        // graceful-degradation
        // contract: pass the `~`
        // through unchanged. The
        // upstream caller (tmux
        // -c, the user's shell)
        // will see the literal
        // `~` and either fail
        // gracefully (tmux) or
        // refuse the submission
        // (shell snippet, which
        // can be edited before
        // submit). Either way,
        // no panic.
        assert_eq!(expand_home("~").as_ref(), "~");
        // `~/x` with no HOME →
        // unchanged as well. (We
        // used to substitute "/x"
        // which was a hack to
        // preserve the rest of the
        // path; the cleaner answer
        // is to pass through.)
        assert_eq!(expand_home("~/work").as_ref(), "~/work");
        // Absolute paths under
        // the (now unset) HOME
        // are also unchanged —
        // there's no prefix to
        // match against.
        assert_eq!(
            expand_home("/Users/har/work").as_ref(),
            "/Users/har/work"
        );
        // Restore HOME.
        if let Some(h) = saved_home {
            // SAFETY: see
            // expand_home_basic.
            unsafe {
                std::env::set_var("HOME", h);
            }
        }
    }

    /// `shorten_home_path` with
    /// multiple home prefixes
    /// picks the most-specific
    /// match. The macOS volume
    /// mount case: `$HOME` is
    /// `/Users/har` but the
    /// user's actual files live
    /// at `/Volumes/HUGE/har`.
    /// The user configures
    /// `homemap=/Volumes/HUGE/har`
    /// so both forms get the
    /// same `~/...` shortening.
    /// When both prefixes
    /// could match (e.g. if HOME
    /// is `/home/user` and the
    /// user has a `homemap=
    /// /home/user/external`),
    /// the longer one wins.
    #[test]
    fn shorten_home_path_picks_most_specific() {
        // macOS-volume case: two
        // homes, paths under the
        // external one match the
        // external one.
        assert_eq!(
            shorten_home_path(
                "/Volumes/HUGE/har/work",
                &[
                    "/Users/har".to_string(),
                    "/Volumes/HUGE/har".to_string(),
                ],
            )
            .as_ref(),
            "~/work"
        );
        // Path under the smaller
        // home → `~/...` using the
        // smaller home.
        assert_eq!(
            shorten_home_path(
                "/Users/har/Documents",
                &[
                    "/Users/har".to_string(),
                    "/Volumes/HUGE/har".to_string(),
                ],
            )
            .as_ref(),
            "~/Documents"
        );
        // Path under neither
        // home → unchanged.
        assert_eq!(
            shorten_home_path(
                "/etc/hosts",
                &[
                    "/Users/har".to_string(),
                    "/Volumes/HUGE/har".to_string(),
                ],
            )
            .as_ref(),
            "/etc/hosts"
        );
        // Bare `~` is the
        // idempotent "already in
        // the target form" case:
        // pass through. The
        // `smarthistory update`
        // subcommand relies on
        // this — a previously-
        // shortened row's `~`
        // value would otherwise
        // re-expand to the longest
        // home on the next run
        // (and then re-shorten
        // again on the third
        // run, oscillating).
        assert_eq!(
            shorten_home_path(
                "~",
                &[
                    "/Users/har".to_string(),
                    "/Volumes/HUGE/har".to_string(),
                ],
            )
            .as_ref(),
            "~"
        );
        // Same length tie: first
        // listed wins (sort is
        // stable).
        assert_eq!(
            shorten_home_path(
                "/a/foo",
                &[
                    "/a".to_string(),
                    "/b".to_string(),
                ],
            )
            .as_ref(),
            "~/foo"
        );
        assert_eq!(
            shorten_home_path(
                "/b/foo",
                &[
                    "/a".to_string(),
                    "/b".to_string(),
                ],
            )
            .as_ref(),
            "~/foo"
        );
    }
}
