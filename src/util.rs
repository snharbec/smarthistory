//! Shared formatting helpers used by both the CLI (`main.rs`) and the
//! TUI (`tui.rs`). Keeping them in one place avoids drift when the
//! format string or the "N/A" sentinel changes.

use chrono::Datelike;

/// Parse a boolean config/session value. Accepts "on", "true", "1",
/// "yes" (case-insensitive) as true; "off", "false", "0", "no" as
/// false. Anything else falls back to `default` rather than failing.
pub fn parse_bool(s: &str, default: bool) -> bool {
    match s.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => true,
        "off" | "false" | "0" | "no" => false,
        _ => default,
    }
}

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

/// Human-readable file size. Ladder:
///   < 1 KiB  -> "N B"
///   < 1 MiB  -> "N.N KiB"
///   else     -> "N.N MiB"
/// Negative or zero returns "0 B". The
/// caller is expected to have already
/// handled directories (which have
/// empty size strings).
pub fn format_size(len: u64) -> String {
    if len < 1024 {
        format!("{} B", len)
    } else if len < 1024 * 1024 {
        format!("{:.1} KiB", len as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", len as f64 / (1024.0 * 1024.0))
    }
}

/// Escape newlines (and carriage
/// returns) in a field value for
/// safe line-based output. The CLI
/// prints one row per line; fields
/// like `command` and `output` can
/// contain newlines which would
/// otherwise split a single row into
/// multiple lines (and break the
/// zsh-widget's `(f)`-parameter
/// record splitter). The zsh widget
/// reverses the escape in shell
/// before assigning to `BUFFER`.
///
/// The escape sequences chosen
/// (`\n` and `\r`) are the standard
/// C-style backslash escapes. They
/// are unambiguous because zsh's
/// shell parser never produces literal
/// `\` + `n` / `r` in a command
/// typed at the prompt.
pub fn escape_field_for_output(s: &str) -> String {
    s.replace('\n', "\\n").replace('\r', "\\r")
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
    // A raw `year*12 + month` difference overcounts by one whenever
    // `now`'s day-of-month hasn't yet reached `then`'s: e.g. Aug 1
    // minus Jul 27 (5 days ago) gives month=8 - month=7 = 1, even
    // though a full calendar month hasn't actually elapsed. Subtract
    // 1 in that case — the same "hasn't had its monthiversary yet"
    // adjustment used for age-in-years calculations — so a small
    // gap that merely crosses a month boundary still falls through
    // to the day/hour/minute/second ladder below instead of being
    // misreported as "1M".
    let mut months = (now.year() - then.year()) * 12 + (now.month() as i32 - then.month() as i32);
    if now.day() < then.day() {
        months -= 1;
    }
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

/// Escape `*`, `?`, and `[` in a
/// GLOB pattern so the user's
/// literal text is matched.
/// SQLite's `GLOB` operator uses
/// `*` (any sequence), `?` (any
/// single char), and `[...]` (char
/// class) as wildcards — these
/// must be escaped (by wrapping
/// with `[...]`) so the user's
/// query text is treated
/// literally.
pub fn escape_glob(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '*' || ch == '?' || ch == '[' {
            out.push('[');
            out.push(ch);
            out.push(']');
        } else {
            out.push(ch);
        }
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
pub fn shorten_home_path<'a>(path: &'a str, homes: &[String]) -> std::borrow::Cow<'a, str> {
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
        if let Some(rest) = path
            .strip_prefix(*home)
            .filter(|r| r.is_empty() || r.starts_with('/'))
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

/// Shorten every directory component of `path` down to its first
/// character (two characters for a dotfile-style directory, e.g.
/// `.config` -> `.c`, so it doesn't collapse to a bare `.` and read
/// as "current directory") while leaving the FINAL component (the
/// filename) fully intact — the classic shell-prompt path-shortening
/// convention (`~/w/p/src/main.rs` for
/// `~/work/project/src/main.rs`). `homes` is passed straight through
/// to [`shorten_home_path`] first, so an absolute path under any
/// configured home collapses to `~/...` before the per-component
/// abbreviation runs; `~` itself is left as `~`, never abbreviated
/// further. Used by ag (`,`) mode to show a content match's file
/// path up front, as compactly as possible, without ever truncating
/// the filename the user actually needs to recognize.
///
/// A path with no directory component (a bare filename, or an
/// already-shortened one-segment string) is returned unchanged.
pub fn shorten_path_dirs(path: &str, homes: &[String]) -> String {
    let home_shortened = shorten_home_path(path, homes);
    let mut parts: Vec<&str> = home_shortened.split('/').collect();
    let Some(filename) = parts.pop() else {
        return home_shortened.into_owned();
    };
    let mut out: Vec<String> = parts
        .into_iter()
        .map(|segment| {
            if segment.is_empty() || segment == "~" {
                // Empty means a leading `/` (root) or a doubled
                // separator — preserve as-is so the join below still
                // produces a leading slash. `~` is already as short
                // as it gets.
                segment.to_string()
            } else if segment.starts_with('.') && segment.len() > 1 {
                segment.chars().take(2).collect()
            } else {
                segment.chars().take(1).collect()
            }
        })
        .collect();
    out.push(filename.to_string());
    out.join("/")
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
    let mut homes: Vec<String> = Vec::with_capacity(home_map.len() + 1);
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
            && !s.is_empty()
        {
            homes.push(s.to_string());
        }
    }
    shorten_home_path(path, &homes)
}

/// Expand a leading `~/x`
/// using the home list.
/// This is the *opposite*
/// of `shorten_home_path`:
/// the function takes a
/// path that may be in
/// short form (`~/x`) or
/// already absolute
/// (`/Users/har/x`,
/// `/Volumes/HUGE/har/x`),
/// and returns the
/// absolute form using
/// the **longest** home
/// in the list (so
/// `/Volumes/HUGE/har/x`
/// wins over
/// `/Users/har/x` when
/// the homemap is set).
///
/// Used by
/// `normalize_for_compare`
/// to put both the
/// DB-side and the
/// tmux-side paths in
/// the same absolute
/// form before
/// canonicalization. The
/// previous behaviour
/// was to call
/// `shorten_home_path`
/// (which is idempotent
/// in the short
/// direction) and then
/// `canonicalize_directory`,
/// but that left DB
/// rows in `~/x` form
/// unresolved on the
/// canonicalize step
/// (no real path
/// `~/x` exists), so
/// the tmux lookup
/// never matched.
///
/// `~/x` (no path) is
/// expanded to the
/// first home. Absolute
/// paths are returned
/// unchanged. Other
/// inputs (relative
/// paths, paths outside
/// any home) are
/// returned verbatim.
pub fn expand_home_to_absolute<'a>(path: &'a str, homes: &[String]) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    if path.is_empty() {
        return Cow::Borrowed(path);
    }
    // `~/` expands to the
    // longest home in the
    // sorted list (most-
    // specific wins).
    if let Some(rest) = path.strip_prefix("~/") {
        // Sort homes longest-
        // first to match the
        // convention used by
        // `shorten_home_path`.
        let mut sorted: Vec<&str> = homes
            .iter()
            .filter(|h| !h.is_empty())
            .map(String::as_str)
            .collect();
        sorted.sort_by_key(|s| std::cmp::Reverse(s.len()));
        if let Some(home) = sorted.first() {
            return Cow::Owned(format!("{}/{}", home, rest));
        }
        return Cow::Borrowed(path);
    }
    // Bare `~` expands to
    // the longest home.
    if path == "~" {
        let mut sorted: Vec<&str> = homes
            .iter()
            .filter(|h| !h.is_empty())
            .map(String::as_str)
            .collect();
        sorted.sort_by_key(|s| std::cmp::Reverse(s.len()));
        if let Some(home) = sorted.first() {
            return Cow::Owned(home.to_string());
        }
        return Cow::Borrowed(path);
    }
    // Already absolute —
    // pass through. The
    // caller will
    // `canonicalize_directory`
    // it next, which
    // handles macOS
    // volume mounts.
    Cow::Borrowed(path)
}

/// Normalize a path for
/// equivalence comparisons
/// across different sources
/// (DB rows vs tmux-reported
/// panes). The transformation
/// is:
/// 1. Expand a leading `~/`
///    using the home list
///    (so the DB's
///    `~/Sources/foo` becomes
///    `/Users/har/Sources/foo`
///    or the homemap form).
///    This step is what was
///    missing before: a
///    `~/x` DB row would
///    fail
///    `std::fs::canonicalize`
///    (the path doesn't
///    exist as `~/x`) and
///    fall back to the
///    un-resolved input,
///    which never matches
///    the tmux side.
/// 2. Run
///    `std::fs::canonicalize`
///    to resolve any macOS
///    volume mounts (so
///    `/Users/har/x` and
///    `/Volumes/HUGE/har/x`
///    collapse to the same
///    physical path on the
///    user's setup).
/// 3. If canonicalize fails
///    (e.g. the directory
///    was unmounted between
///    insert and query),
///    return the home-
///    expanded form
///    verbatim so the
///    comparison still has
///    a string to compare.
///
/// Two paths that refer to
/// the same physical
/// directory always
/// normalize to the same
/// string, so this is
/// safe to use as a key in
/// `tmux_windows.iter().find(...)`.
pub fn normalize_for_compare(path: &str, homes: &[String]) -> String {
    if path.is_empty() {
        return String::new();
    }
    // Step 1: expand a
    // leading `~/` if the
    // path uses the short
    // form. We do this with
    // `expand_home_to_absolute`
    // so a
    // `homemap=/Volumes/HUGE/har`
    // config still wins in
    // length-tie cases. The
    // helper returns
    // `Cow<'_, str>` so the
    // allocation is avoided
    // for paths that don't
    // need expansion (e.g.
    // tmux-reported absolute
    // paths).
    let expanded = expand_home_to_absolute(path, homes);
    // Step 2: resolve any
    // macOS volume mounts /
    // symlinks. tmux reports
    // a real absolute path
    // so this typically
    // succeeds.
    let canonical = canonicalize_directory(&expanded);
    if canonical.is_empty() {
        // Canonicalize failed
        // AND the input was
        // empty (the
        // canonicalize helper
        // returns empty on
        // empty input).
        return String::new();
    }
    canonical
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

/// Quote a string for use as a
/// single argument to a shell
/// command. The result is a
/// `String` suitable for
/// splicing into a `tmux
/// send-keys` payload or any
/// other context that runs
/// the result through a shell
/// later.
///
/// The rules are POSIX-shell
/// compatible:
///
/// - Empty input becomes
///   `''` (otherwise the
///   argument would
///   disappear entirely).
/// - A string that is
///   already "shell-clean"
///   (alphanumeric, `_`,
///   `-`, `.`, `/`, `~`,
///   `:`, `,`, `=`, `+`,
///   `@`) is returned
///   verbatim — no
///   allocation, no
///   allocation, no
///   overhead in the
///   common case.
/// - Otherwise, wrap in
///   single quotes and
///   replace every
///   internal `'` with
///   `'\''` (the standard
///   "close-quote, escape,
///   reopen" pattern).
///
/// Used by the directory
/// `.command` chain in
/// `select_for_run` to wrap
/// the script body before
/// passing it to `tmux
/// send-keys` (which would
/// otherwise mis-interpret
/// spaces, semicolons, etc.
/// as keystrokes).
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '_' | '-' | '.' | '/' | '~' | ':' | ',' | '=' | '+' | '@')
    }) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            // POSIX: close the
            // current quoted
            // string, emit an
            // escaped single
            // quote, reopen.
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Recursively walk `root` and
/// return every directory
/// found underneath it
/// (excluding `root` itself).
///
/// The walk is depth-first,
/// post-order: a parent's
/// directories come before
/// the children's, so the
/// returned list is in a
/// stable, predictable order
/// that matches what the
/// user would type if they
/// ran `find <root> -type d
/// -mindepth 1`.
///
/// The walk skips:
///
/// - **Non-directory entries**
///   (regular files, symlinks
///   to files, etc.). We
///   only return directories.
/// - **Symlinks** that point
///   back to a parent (loops).
///   The walk tracks a
///   "seen canonical paths"
///   set so a symlink loop
///   can't spin forever.
///   Symlinks to *other*
///   directories are
///   followed (and
///   canonicalised) so a
///   symlinked project tree
///   shows up like a real
///   one. (This matches
///   `find -type d -L`.)
/// - **Permission errors.**
///   A directory the user
///   can't read is silently
///   skipped — the walk
///   continues into the
///   rest of the tree.
///   Better to under-report
///   than to crash the TUI
///   on startup.
///
/// The function never panics
/// or returns an `Err`: a
/// missing root returns an
/// empty `Vec`, matching the
/// "sessiondirs that don't
/// exist are silently
/// skipped" contract
/// (see `Config::session_dirs`).
pub fn walk_subdirectories(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    use std::collections::HashSet;
    use std::path::PathBuf;

    let mut out: Vec<PathBuf> = Vec::new();
    // `seen` tracks canonical
    // paths so a symlink loop
    // (e.g. `a -> b` and
    // `b -> a`) doesn't
    // recurse forever. The
    // set is intentionally
    // unbounded: a real
    // directory tree is
    // typically <10k entries,
    // which is well within
    // memory budget for a
    // single TUI startup.
    let mut seen: HashSet<String> = HashSet::new();
    walk_subdir_recurse(root, &mut out, &mut seen);
    out
}

fn walk_subdir_recurse(
    dir: &std::path::Path,
    out: &mut Vec<std::path::PathBuf>,
    seen: &mut std::collections::HashSet<String>,
) {
    // If we can't canonicalise
    // the directory (e.g.
    // permission denied,
    // symlink loop, missing
    // dir between two
    // `read_dir` calls), skip
    // it silently. The
    // walker's contract is
    // "best effort, never
    // panic".
    let canonical = match std::fs::canonicalize(dir) {
        Ok(c) => c,
        Err(_) => return,
    };
    let canonical_str = canonical.to_string_lossy().into_owned();
    if !seen.insert(canonical_str) {
        // Already visited (a
        // symlink brought us
        // back to an earlier
        // node). Skip the
        // recurse to avoid an
        // infinite loop.
        return;
    }
    // `read_dir` returns an
    // iterator that yields
    // entries in
    // implementation-defined
    // order. Sort by path for
    // stable output (matches
    // `find` on most
    // filesystems).
    let mut entries: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => return,
    };
    entries.sort();
    for entry in entries {
        // Only follow real
        // directories. A
        // symlink that points
        // to a directory will
        // be canonicalised by
        // the recursive call's
        // own `canonicalize`,
        // so we don't need to
        // resolve it here.
        match std::fs::metadata(&entry) {
            Ok(md) if md.is_dir() => {
                // Skip hidden
                // directories by
                // default? No — the
                // user might have
                // legitimate
                // hidden
                // subdirectories
                // (e.g. `.claude`,
                // `.config`). We
                // include them.
                // The cost is a
                // slightly longer
                // list, which the
                // user can filter
                // with the `#`
                // query.
                out.push(entry.clone());
                walk_subdir_recurse(&entry, out, seen);
            }
            _ => {
                // Not a
                // directory (file,
                // symlink to file,
                // socket, etc.).
                // Skip.
            }
        }
    }
}

/// Find a `.command` file in
/// the ancestor chain
/// starting at `start`. The
/// first match wins. Returns
/// `Some(path)` if `start`
/// itself has a `.command`
/// (or any ancestor up to
/// the filesystem root).
/// Returns `None` if no
/// ancestor has a
/// `.command`.
///
/// "First match wins" means
/// the closest one in the
/// walk: if both
/// `/a/.command` and
/// `/a/b/.command` exist
/// and the user picks
/// `/a/b/c`, we return
/// `/a/b/.command`. This
/// is the standard
/// "project-overrides-
/// workspace" convention
/// used by similar tools
/// (e.g. the `.envrc` /
/// `.env.local` pattern).
///
/// Symlinks are not
/// resolved (the comparison
/// is on the *path* as
/// given). This is the
/// right behaviour: the
/// user types a real
/// directory path, and the
/// `.command` lookup should
/// follow the same path
/// the user sees, not the
/// canonicalised one.
pub fn find_command_file(start: &std::path::Path) -> Option<std::path::PathBuf> {
    // Start at the leaf
    // (the directory the user
    // picked) and walk up. If
    // even the leaf doesn't
    // have a `.command`, try
    // the parent, then the
    // grandparent, and so on.
    let mut current: Option<&std::path::Path> = Some(start);
    while let Some(dir) = current {
        let candidate = dir.join(".command");
        if candidate.is_file() {
            return Some(candidate);
        }
        // `parent()` returns
        // `None` for the root
        // (or for relative paths
        // with no parent
        // component). The
        // `unwrap_or` keeps the
        // walk bounded at the
        // filesystem root.
        current = dir.parent();
    }
    None
}

/// Test helpers for the
/// `walk_subdirectories` /
/// `find_command_file` /
/// `shell_quote` regression
/// suite. The tests need a
/// sandboxed directory tree
/// they can build and
/// dispose of cleanly,
/// without polluting the
/// real filesystem. We use
/// a tempdir under
/// `std::env::temp_dir()`
/// with a per-test
/// counter + process-id
/// suffix to avoid
/// collisions when
/// `cargo test` runs in
/// parallel.
#[cfg(test)]
mod walker_tests;

#[cfg(test)]
mod tests;
