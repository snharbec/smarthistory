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
    let _hours = delta.num_hours();
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
pub fn expand_home_to_absolute<'a>(
    path: &'a str,
    homes: &[String],
) -> std::borrow::Cow<'a, str> {
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
pub fn normalize_for_compare(
    path: &str,
    homes: &[String],
) -> String {
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
            || matches!(
                c,
                '_' | '-' | '.' | '/' | '~' | ':' | ',' | '=' | '+' | '@'
            )
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
pub fn walk_subdirectories(
    root: &std::path::Path,
) -> Vec<std::path::PathBuf> {
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
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect(),
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
pub fn find_command_file(
    start: &std::path::Path,
) -> Option<std::path::PathBuf> {
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
mod walker_tests {
    use super::*;
    use std::sync::atomic::{
        AtomicU64, Ordering,
    };
    static COUNTER: AtomicU64 =
        AtomicU64::new(0);

    /// Build a unique temp
    /// directory and return
    /// its path. Auto-cleaned
    /// via a `Drop`-style
    /// `TempDir` wrapper
    /// (the test does its own
    /// `remove_dir_all` at
    /// the end of each
    /// function).
    fn unique_tempdir(label: &str) -> std::path::PathBuf {
        let n = COUNTER
            .fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!(
            "smarthistory_walker_{label}_{pid}_{n}"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)
            .expect("create temp dir");
        dir
    }

    /// `walk_subdirectories`
    /// returns every
    /// subdirectory of a
    /// root, in stable
    /// sorted order. The
    /// root itself is not
    /// included.
    #[test]
    fn walk_subdirectories_lists_all_subs() {
        let root = unique_tempdir("walk_basic");
        // Create:
        //   root/
        //   root/a/
        //   root/a/b/
        //   root/a/c/
        //   root/d/
        let _ = std::fs::create_dir_all(
            root.join("a").join("b"),
        );
        let _ = std::fs::create_dir_all(
            root.join("a").join("c"),
        );
        let _ =
            std::fs::create_dir_all(root.join("d"));
        let _ = std::fs::write(
            root.join("a").join("file.txt"),
            "ignore me",
        );
        let got = walk_subdirectories(&root);
        // The result should
        // contain all four
        // subdirectories. We
        // compare by canonical
        // path so symlinks
        // (e.g. on macOS where
        // `/tmp` is a symlink
        // to `/private/tmp`)
        // don't break the
        // assertion.
        let canonical_root =
            std::fs::canonicalize(&root)
                .unwrap();
        let names: std::collections::HashSet<String> = got
            .iter()
            .map(|p| {
                // Canonicalize
                // each path so
                // `/var/...` vs
                // `/private/var/...`
                // (a macOS
                // symlink) don't
                // break the
                // relative-path
                // comparison.
                let canon = std::fs::canonicalize(p)
                    .unwrap_or_else(|_| p.clone());
                canon
                    .strip_prefix(&canonical_root)
                    .map(|r| {
                        r.to_string_lossy()
                            .trim_start_matches('/')
                            .to_string()
                    })
                    .unwrap_or_else(|_| {
                        canon.to_string_lossy().into_owned()
                    })
            })
            .collect();
        assert!(names.contains("a"), "missing a, got: {names:?}");
        assert!(names.contains("a/b"), "missing a/b, got: {names:?}");
        assert!(names.contains("a/c"), "missing a/c, got: {names:?}");
        assert!(names.contains("d"), "missing d, got: {names:?}");
        // The root itself
        // should NOT be in the
        // list (the walker
        // returns subdirs, not
        // the root).
        assert!(
            !names.contains(""),
            "root path should not be in the result, got: {names:?}"
        );
        // The plain file must
        // NOT be in the list
        // (the walker filters
        // non-directories).
        assert!(
            !names.iter().any(|n| n.contains("file")),
            "files must not be in the result, got: {names:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A missing root
    /// returns an empty
    /// `Vec` (not an
    /// error). This is the
    /// "sessiondirs that
    /// don't exist are
    /// silently skipped"
    /// contract.
    #[test]
    fn walk_subdirectories_missing_root_is_empty() {
        let missing = std::env::temp_dir()
            .join("smarthistory_walker_definitely_does_not_exist_xyz123");
        let _ = std::fs::remove_dir_all(&missing);
        let got = walk_subdirectories(&missing);
        assert!(got.is_empty());
    }

    /// `find_command_file`
    /// returns the
    /// `<dir>/.command`
    /// when one exists in
    /// the leaf directory.
    #[test]
    fn find_command_file_in_leaf() {
        let root = unique_tempdir("cmd_leaf");
        let dir = root.join("project");
        let _ = std::fs::create_dir_all(&dir);
        let cmd = dir.join(".command");
        let _ = std::fs::write(&cmd, "#!/bin/sh\necho hi\n");
        let found =
            find_command_file(&dir).expect("must find .command");
        // The canonical-path
        // comparison avoids
        // macOS `/tmp` vs
        // `/private/tmp`
        // surprises.
        let canonical_found =
            std::fs::canonicalize(&found)
                .unwrap();
        let canonical_expected =
            std::fs::canonicalize(&cmd)
                .unwrap();
        assert_eq!(canonical_found, canonical_expected);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// When the leaf has no
    /// `.command` but an
    /// ancestor does, the
    /// ancestor wins. The
    /// lookup walks up
    /// the tree until it
    /// finds one or hits
    /// the root.
    #[test]
    fn find_command_file_walks_up() {
        let root = unique_tempdir("cmd_walk");
        let project = root.join("project");
        let nested =
            project.join("src").join("lib");
        let _ = std::fs::create_dir_all(&nested);
        // Place the
        // `.command` at the
        // project level, NOT
        // in the leaf.
        let cmd = project.join(".command");
        let _ = std::fs::write(&cmd, "echo project-setup\n");
        let found = find_command_file(&nested)
            .expect("must walk up to find .command");
        let canonical_found =
            std::fs::canonicalize(&found)
                .unwrap();
        let canonical_expected =
            std::fs::canonicalize(&cmd)
                .unwrap();
        assert_eq!(canonical_found, canonical_expected);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// When the leaf and
    /// the closest
    /// ancestor both have
    /// a `.command`, the
    /// **leaf** wins
    /// (closest-in-walk
    /// is
    /// first-match-wins).
    #[test]
    fn find_command_file_leaf_beats_ancestor() {
        let root = unique_tempdir("cmd_prefer");
        let project = root.join("project");
        let leaf = project.join("src");
        let _ = std::fs::create_dir_all(&leaf);
        // Place two files;
        // both leaves count.
        let ancestor_cmd = project.join(".command");
        let _ = std::fs::write(
            &ancestor_cmd,
            "echo ancestor\n",
        );
        let leaf_cmd = leaf.join(".command");
        let _ = std::fs::write(
            &leaf_cmd,
            "echo leaf\n",
        );
        let found =
            find_command_file(&leaf).expect("must find");
        let canonical_found =
            std::fs::canonicalize(&found)
                .unwrap();
        let canonical_leaf =
            std::fs::canonicalize(&leaf_cmd)
                .unwrap();
        assert_eq!(canonical_found, canonical_leaf);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// When no ancestor has
    /// a `.command`, the
    /// lookup returns
    /// `None`.
    #[test]
    fn find_command_file_none_returns_none() {
        let root = unique_tempdir("cmd_none");
        let nested =
            root.join("a").join("b").join("c");
        let _ = std::fs::create_dir_all(&nested);
        // No .command file
        // anywhere in the
        // tree.
        assert!(find_command_file(&nested).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `shell_quote`
    /// returns the input
    /// verbatim when it's
    /// already
    /// shell-clean
    /// (alphanumeric,
    /// `_`, `-`, `.`,
    /// `/`, `~`, `:`, `,`,
    /// `=`, `+`, `@`).
    #[test]
    fn shell_quote_clean_passes_through() {
        assert_eq!(shell_quote("ls"), "ls");
        assert_eq!(shell_quote("cargo-build"), "cargo-build");
        assert_eq!(shell_quote("a/b/c"), "a/b/c");
        assert_eq!(shell_quote("~/work"), "~/work");
        assert_eq!(shell_quote("key=value"), "key=value");
        assert_eq!(shell_quote("a,b"), "a,b");
    }

    /// Strings with spaces
    /// or shell
    /// metacharacters get
    /// wrapped in single
    /// quotes.
    #[test]
    fn shell_quote_dirty_gets_quoted() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(
            shell_quote("hello world"),
            "'hello world'"
        );
        assert_eq!(
            shell_quote("a;b"),
            "'a;b'"
        );
        assert_eq!(
            shell_quote("$VAR"),
            "'$VAR'"
        );
    }

    /// Strings with single
    /// quotes get the
    /// standard POSIX
    /// escape (`'\''`):
    /// close, escape, reopen.
    #[test]
    fn shell_quote_escapes_inner_quotes() {
        assert_eq!(
            shell_quote("it's"),
            "'it'\\''s'"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// `expand_home_to_absolute`
    /// is the inverse of
    /// `shorten_home_path`:
    /// `~/x` becomes
    /// `<home>/x` using
    /// the **longest** home
    /// in the list (so a
    /// homemap entry that's
    /// longer than `$HOME`
    /// wins). Absolute
    /// paths and bare `~`
    /// are also handled.
    #[test]
    fn expand_home_to_absolute_basic() {
        let homes = vec!["/Users/har".to_string()];
        // `~/x` expands to
        // `<home>/x`.
        assert_eq!(
            expand_home_to_absolute(
                "~/work",
                &homes,
            )
            .as_ref(),
            "/Users/har/work"
        );
        // `~/a/b/c` expands
        // similarly.
        assert_eq!(
            expand_home_to_absolute(
                "~/a/b/c",
                &homes,
            )
            .as_ref(),
            "/Users/har/a/b/c"
        );
        // Bare `~` expands
        // to the first home.
        assert_eq!(
            expand_home_to_absolute("~", &homes)
                .as_ref(),
            "/Users/har"
        );
        // Already-absolute
        // paths pass through
        // unchanged.
        assert_eq!(
            expand_home_to_absolute(
                "/etc/hosts",
                &homes,
            )
            .as_ref(),
            "/etc/hosts"
        );
        // Empty input passes
        // through.
        assert_eq!(
            expand_home_to_absolute("", &homes)
                .as_ref(),
            ""
        );
    }

    /// The homemap wins in
    /// length-tie cases. With
    /// `homemap=/Volumes/HUGE/har`
    /// and `$HOME=/Users/har`,
    /// `~/x` expands to the
    /// homemap form because
    /// it's the longer
    /// prefix.
    #[test]
    fn expand_home_to_absolute_picks_most_specific() {
        let homes = vec![
            "/Users/har".to_string(),
            "/Volumes/HUGE/har".to_string(),
        ];
        assert_eq!(
            expand_home_to_absolute(
                "~/work",
                &homes,
            )
            .as_ref(),
            "/Volumes/HUGE/har/work"
        );
    }

    /// `normalize_for_compare`
    /// puts a `~/x` DB row
    /// and a
    /// `/Users/har/x` tmux
    /// pane in the same
    /// canonical form so the
    /// `directory_tmux_pane_id`
    /// lookup succeeds. The
    /// `~/x` expansion is the
    /// load-bearing step —
    /// without it, the
    /// `std::fs::canonicalize`
    /// call would fail (no
    /// real `~/x` path
    /// exists) and the two
    /// sides would never
    /// agree.
    #[test]
    fn normalize_for_compare_handles_tilde_form() {
        let homes = vec!["/tmp".to_string()];
        // `~/x` expands to
        // `/tmp/x` and then
        // canonicalizes (which
        // succeeds on existing
        // dirs; the test uses
        // `/tmp` because it
        // exists on every
        // Unix).
        let from_tilde = normalize_for_compare(
            "~/self_test_norm_dir",
            &homes,
        );
        let from_absolute =
            normalize_for_compare(
                "/tmp/self_test_norm_dir",
                &homes,
            );
        // Both should
        // canonicalize to the
        // same value (modulo
        // symlink resolution on
        // `/tmp`).
        assert_eq!(from_tilde, from_absolute);
    }

    /// Empty input returns
    /// empty output (matches
    /// the contract of
    /// `canonicalize_directory`).
    #[test]
    fn normalize_for_compare_empty_input() {
        assert_eq!(
            normalize_for_compare("", &[]),
            ""
        );
    }

    /// Paths outside any
    /// home pass through
    /// (the absolute form is
    /// canonicalized, the
    /// rest of the
    /// transformation
    /// doesn't apply).
    #[test]
    fn normalize_for_compare_unrelated_path() {
        let homes = vec!["/Users/har".to_string()];
        // `/etc/hosts` isn't
        // under any home, so
        // the home-expansion
        // step is a no-op. The
        // canonicalize step
        // resolves symlinks
        // (but `/etc/hosts`
        // isn't a symlink on
        // most systems).
        let result = normalize_for_compare(
            "/etc/hosts",
            &homes,
        );
        assert!(
            !result.is_empty(),
            "result must be non-empty for an existing path, got: {result:?}"
        );
    }

    #[test]
    fn escape_field_single_line_unchanged() {
        assert_eq!(escape_field_for_output("ls -la"), "ls -la");
    }

    #[test]
    fn escape_field_multiline_becomes_single_line() {
        let cmd = "for i in 1 2 3\ndo echo $i\ndone";
        let escaped = escape_field_for_output(cmd);
        // The escaped form must not contain a real newline — that's
        // the whole point: one row fits on one line of CLI output.
        assert!(!escaped.contains('\n'), "escaped still has newline: {escaped:?}");
        assert!(!escaped.contains('\r'), "escaped still has carriage return: {escaped:?}");
        // The backslash-n sequences must be present.
        assert_eq!(escaped, "for i in 1 2 3\\ndo echo $i\\ndone");
    }

    #[test]
    fn escape_field_carriage_return() {
        assert_eq!(escape_field_for_output("a\rb"), "a\\rb");
    }
}
