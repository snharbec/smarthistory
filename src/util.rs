//! Shared formatting helpers used by both the CLI (`main.rs`) and the
//! TUI (`tui.rs`). Keeping them in one place avoids drift when the
//! format string or the "N/A" sentinel changes.

/// Format a Unix epoch (seconds) as "dd.Mon.YYYY HH:MM:SS" in UTC, e.g.
/// "03.Jun.2026 17:43:01". Returns "N/A" if the value is out of range.
pub fn format_time(epoch: i64) -> String {
    match chrono::DateTime::from_timestamp(epoch, 0) {
        Some(dt) => dt.naive_utc().format("%d.%b.%Y %H:%M:%S").to_string(),
        None => "N/A".to_string(),
    }
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
        assert_eq!(format_time(i64::MIN), "N/A");
        // A timestamp of 0 (the Unix epoch) is in range; the helper
        // must NOT return "N/A" for it.
        assert_ne!(format_time(0), "N/A");
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
}
