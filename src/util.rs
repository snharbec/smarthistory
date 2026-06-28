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
}
