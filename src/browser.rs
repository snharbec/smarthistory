//! `^` (browser bookmarks + history) prefix mode.
//!
//! Reads bookmarks and visited-URL history straight from the
//! on-disk profile files of locally-installed browsers (Chrome,
//! Firefox, Safari) — no browser extension, no running-browser IPC.
//! All sources are merged into one result list; each row's `command`
//! text is prefixed with the literal word `bookmark` or `history`
//! (see [`browser_entry_to_row`]) so the user can narrow the view
//! by typing that word, e.g. `^bookmark rust` or `^history github`.
//!
//! Selecting a row stages `open "<url>"` (macOS) / `xdg-open
//! "<url>"` (other Unixes) — see `stage_browser_selection` in
//! `src/tui/actions.rs`.
//!
//! ## Sources
//!
//! - **Chrome**: `<profile>/Bookmarks` (JSON) and `<profile>/History`
//!   (SQLite, table `urls`). Profile defaults to `Default` under the
//!   platform's Chrome user-data dir; override with
//!   `browser.<id>.type=chrome` + `browser.<id>.profile=<path>` in
//!   the config file.
//! - **Firefox**: `<profile>/places.sqlite` (SQLite; `moz_bookmarks`
//!   joined with `moz_places` for bookmarks, `moz_places` alone for
//!   history). Profile defaults to the first `*.default*` profile
//!   found under the platform's Firefox profiles dir; override with
//!   `browser.<id>.type=firefox` + `browser.<id>.profile=<path>`.
//! - **Safari** (macOS only): `<profile>/Bookmarks.plist` (a binary
//!   or XML property list, parsed via the `plist` crate — Safari
//!   has no separate "profile" concept, so `<profile>` is just
//!   `~/Library/Safari`) and `<profile>/History.db` (SQLite,
//!   `history_visits` joined with `history_items`). Bookmark rows
//!   carry no reliable date (the plist format has no stable
//!   "date added" field for a plain bookmark, only for Reading List
//!   items), so Safari bookmarks always sort as the oldest rows.
//!
//! Every SQLite read goes through [`copy_sqlite_snapshot`], which
//! copies the main db file plus its `-wal` / `-shm` sidecars to a
//! scratch directory first — the browser's own db file is typically
//! held open (and often locked) by a running browser process, and
//! this is the same approach every other "read a live browser's
//! history" tool uses.
//!
//! When no `browser.<id>.*` entries are configured, [`resolve_configured`]
//! auto-detects Chrome / Firefox / Safari at their platform-default
//! install locations (only including a browser whose profile
//! directory actually exists) — mirrors `crate::jira::JiraConfig::
//! from_env`'s "config is optional, resolved fresh at point of use"
//! convention rather than being threaded through `App::new` as a
//! constructor parameter (see that function's doc comment for why
//! fetching config fresh, instead of storing it on `App`, is the
//! established pattern for a mode whose config can't be
//! deterministically injected into every test that builds an
//! `App`).

use crate::tui::state::HistoryRow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// How long the browser-mode search waits after the last keystroke
/// before spawning the background read. Matches the files-mode walk
/// debounce (400 ms) — both are local disk reads, not network calls.
pub const BROWSER_DEBOUNCE: Duration = Duration::from_millis(400);

/// Which browser a [`BrowserSource`] reads from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserKind {
    Chrome,
    Firefox,
    Safari,
}

impl BrowserKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BrowserKind::Chrome => "chrome",
            BrowserKind::Firefox => "firefox",
            BrowserKind::Safari => "safari",
        }
    }

    /// Parse a `browser.<id>.type=` value (case-insensitive).
    /// Returns `None` for anything else.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "chrome" => Some(BrowserKind::Chrome),
            "firefox" => Some(BrowserKind::Firefox),
            "safari" => Some(BrowserKind::Safari),
            _ => None,
        }
    }
}

/// One configured (or auto-detected) browser to read bookmarks +
/// history from. `profile` is the browser-specific profile
/// directory: for Chrome, the directory directly containing
/// `Bookmarks` / `History`; for Firefox, the directory directly
/// containing `places.sqlite`; for Safari, `~/Library/Safari`
/// itself (Safari has no separate profile concept).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSource {
    pub kind: BrowserKind,
    pub profile: PathBuf,
}

/// The platform-default Chrome profile directory, or `None` on a
/// platform this module doesn't special-case (Windows).
fn default_chrome_profile(home: &str) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(PathBuf::from(home).join("Library/Application Support/Google/Chrome/Default"))
    } else if cfg!(target_os = "linux") {
        Some(PathBuf::from(home).join(".config/google-chrome/Default"))
    } else {
        None
    }
}

/// The platform-default Firefox profiles ROOT (the directory
/// containing one subdirectory per profile, e.g.
/// `xxxxxxxx.default-release`). `None` on an unsupported platform.
fn default_firefox_profiles_root(home: &str) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(PathBuf::from(home).join("Library/Application Support/Firefox/Profiles"))
    } else if cfg!(target_os = "linux") {
        Some(PathBuf::from(home).join(".mozilla/firefox"))
    } else {
        None
    }
}

/// Pick the best Firefox profile directory under `root`: prefers a
/// `*.default-release` subdirectory, falls back to the first
/// `*.default*` match, and returns `None` if `root` doesn't exist or
/// has no matching entry. Firefox names the profile actually used
/// by default installs with a random-looking prefix (e.g.
/// `abcd1234.default-release`), so an exact path can't be hardcoded
/// — this mirrors what every "read Firefox history" tool does.
fn pick_firefox_profile(root: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".default-release") || name.ends_with(".default") || name.contains(".default") {
            candidates.push(entry.path());
        }
    }
    candidates.sort_by_key(|p| {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // Prefer "default-release" over any other ".default*" variant.
        if name.ends_with(".default-release") { 0 } else { 1 }
    });
    candidates.into_iter().next()
}

/// The platform-default Safari profile directory — just
/// `~/Library/Safari` itself, since Safari (unlike Chrome/Firefox)
/// has no separate multi-profile concept; `Bookmarks.plist` and
/// `History.db` live directly under it. `None` on any non-macOS
/// platform (Safari doesn't exist there).
fn default_safari_profile(home: &str) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(PathBuf::from(home).join("Library/Safari"))
    } else {
        None
    }
}

/// The platform-default profile location for a given browser
/// kind, regardless of whether it exists on disk. Used to resolve
/// a `browser.<id>.type=...` config entry that has no `profile`
/// override (see `Config::browser_sources` in `src/main.rs`) —
/// unlike [`BrowserSource::autodetect`], this doesn't filter out
/// missing directories, since an explicit `browser.<id>.type=...`
/// entry is the user asserting that browser is installed; a
/// missing profile at read time surfaces as "no rows from this
/// source" rather than silently vanishing from the config.
pub fn default_profile_for(kind: BrowserKind) -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    match kind {
        BrowserKind::Chrome => default_chrome_profile(&home),
        BrowserKind::Firefox => {
            default_firefox_profiles_root(&home).and_then(|root| pick_firefox_profile(&root))
        }
        BrowserKind::Safari => default_safari_profile(&home),
    }
}

impl BrowserSource {
    /// Auto-detect every locally-installed, supported browser at
    /// its platform-default profile location. A browser is
    /// included only when its profile directory actually exists on
    /// disk (so a machine without Chrome installed doesn't get a
    /// dead source that fails every read). Used by
    /// [`resolve_configured`] when the user hasn't set any
    /// `browser.<id>.*` config entries.
    pub fn autodetect() -> Vec<BrowserSource> {
        let home = std::env::var("HOME").unwrap_or_default();
        let mut sources = Vec::new();
        if let Some(profile) = default_chrome_profile(&home)
            && profile.is_dir()
        {
            sources.push(BrowserSource {
                kind: BrowserKind::Chrome,
                profile,
            });
        }
        if let Some(root) = default_firefox_profiles_root(&home)
            && let Some(profile) = pick_firefox_profile(&root)
        {
            sources.push(BrowserSource {
                kind: BrowserKind::Firefox,
                profile,
            });
        }
        if let Some(profile) = default_safari_profile(&home)
            && profile.is_dir()
        {
            sources.push(BrowserSource {
                kind: BrowserKind::Safari,
                profile,
            });
        }
        sources
    }

    /// The one file whose readability is the best single proxy for
    /// "will this source actually yield rows" — used by
    /// `crate::tui::mode::browser::check` to probe past a
    /// misleadingly-optimistic `profile.is_dir()` check.
    ///
    /// This matters concretely for Safari on macOS: `~/Library/
    /// Safari` itself passes a plain `is_dir()`/`stat` check (TCC,
    /// macOS's privacy-protection layer, doesn't hide the
    /// directory's existence), but actually **opening**
    /// `Bookmarks.plist` or `History.db` fails with "Operation not
    /// permitted" unless the process has been granted Full Disk
    /// Access in System Settings → Privacy & Security. Attempting a
    /// real file open is the only way to surface that distinction
    /// before the user hits an empty, silently-failed result list.
    pub fn primary_file(&self) -> PathBuf {
        match self.kind {
            BrowserKind::Chrome => self.profile.join("Bookmarks"),
            BrowserKind::Firefox => self.profile.join("places.sqlite"),
            BrowserKind::Safari => self.profile.join("Bookmarks.plist"),
        }
    }
}

/// Resolve the browsers to read from: the user's configured
/// `browser.<id>.type` / `browser.<id>.profile` entries (see
/// `Config::browser_sources` in `src/main.rs`), or — when none are
/// configured — [`BrowserSource::autodetect`]. Called fresh at
/// every point of use (debounce fire, health check), the same
/// "no config stored on `App`" convention `crate::jira::JiraConfig::
/// from_env` uses; see this module's doc comment for why.
pub fn resolve_configured() -> Vec<BrowserSource> {
    let configured = crate::Config::load().browser_sources();
    if !configured.is_empty() {
        configured
    } else {
        BrowserSource::autodetect()
    }
}

/// Seconds between the WebKit/Chrome epoch (1601-01-01) and the
/// Unix epoch (1970-01-01). Chrome's `last_visit_time` / bookmark
/// `date_added` are microseconds since the WebKit epoch.
const WEBKIT_EPOCH_OFFSET_SECS: i64 = 11_644_473_600;

fn webkit_micros_to_unix_secs(webkit_micros: i64) -> i64 {
    if webkit_micros <= 0 {
        return 0;
    }
    webkit_micros / 1_000_000 - WEBKIT_EPOCH_OFFSET_SECS
}

/// Firefox's `last_visit_date` / `dateAdded` (`PRTime`) are already
/// microseconds since the Unix epoch.
fn firefox_micros_to_unix_secs(firefox_micros: i64) -> i64 {
    if firefox_micros <= 0 {
        return 0;
    }
    firefox_micros / 1_000_000
}

/// Seconds between the Mac absolute time epoch (2001-01-01) and the
/// Unix epoch (1970-01-01). Safari's `history_visits.visit_time` is
/// a `CFAbsoluteTime` / Core Data timestamp: seconds (as a
/// floating-point value) since the Mac epoch.
const MAC_ABSOLUTE_EPOCH_OFFSET_SECS: i64 = 978_307_200;

fn mac_absolute_time_to_unix_secs(mac_time: f64) -> i64 {
    if mac_time <= 0.0 {
        return 0;
    }
    mac_time as i64 + MAC_ABSOLUTE_EPOCH_OFFSET_SECS
}

/// One bookmark or history entry read from a browser, before it's
/// converted into a `HistoryRow`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserEntry {
    /// `"bookmark"` or `"history"` — spliced into `HistoryRow::command`
    /// verbatim so the user can filter by typing the word.
    tag: &'static str,
    title: String,
    url: String,
    timestamp: i64,
    kind: BrowserKind,
}

/// Copy a SQLite database file (plus its `-wal` / `-shm` sidecars,
/// when present) into a fresh scratch directory under
/// `std::env::temp_dir()`, and return the path to the copy. Browser
/// history/places databases are usually held open (and often in WAL
/// mode) by a running browser process; reading a live copy directly
/// can hit a transient lock, so every read in this module goes
/// through a snapshot copy first, matching how other "read a live
/// browser's history" tools do it. Returns `None` on any I/O
/// failure (missing file, permission denied) — callers treat that
/// as "this source has nothing to offer" rather than propagating an
/// error, since a browser that isn't installed / has no profile yet
/// is a normal, common case, not a bug.
fn copy_sqlite_snapshot(src: &Path) -> Option<PathBuf> {
    if !src.is_file() {
        return None;
    }
    let dir = std::env::temp_dir().join(format!(
        "smarthistory-browser-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).ok()?;
    let dest = dir.join(src.file_name()?);
    std::fs::copy(src, &dest).ok()?;
    for suffix in ["-wal", "-shm"] {
        let sidecar_src = append_to_filename(src, suffix);
        if sidecar_src.is_file() {
            let sidecar_dest = append_to_filename(&dest, suffix);
            let _ = std::fs::copy(&sidecar_src, &sidecar_dest);
        }
    }
    Some(dest)
}

/// Append `suffix` to a path's final component, e.g.
/// `append_to_filename("/a/History", "-wal") == "/a/History-wal"`.
fn append_to_filename(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Open a read-only SQLite connection to `path`. Used on the
/// snapshot copy produced by `copy_sqlite_snapshot`, so read-only
/// is a safety net rather than the mechanism that avoids the lock.
fn open_readonly(path: &Path) -> Option<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

/// Read Chrome's `<profile>/Bookmarks` JSON file. Walks every root
/// (`bookmark_bar`, `other`, `synced`) recursively; folders are
/// descended into but not emitted as rows themselves.
fn read_chrome_bookmarks(profile: &Path) -> Vec<BrowserEntry> {
    let path = profile.join("Bookmarks");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    if let Some(roots) = json.get("roots").and_then(|r| r.as_object()) {
        for root in roots.values() {
            walk_chrome_bookmark_node(root, &mut entries);
        }
    }
    entries
}

fn walk_chrome_bookmark_node(node: &serde_json::Value, out: &mut Vec<BrowserEntry>) {
    let node_type = node.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if node_type == "url" {
        let title = node.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        let url = node.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
        if url.is_empty() {
            return;
        }
        let date_added = node
            .get("date_added")
            .and_then(|d| d.as_str())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        out.push(BrowserEntry {
            tag: "bookmark",
            title,
            url,
            timestamp: webkit_micros_to_unix_secs(date_added),
            kind: BrowserKind::Chrome,
        });
    } else if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
        for child in children {
            walk_chrome_bookmark_node(child, out);
        }
    }
}

/// Read Chrome's `<profile>/History` SQLite database's `urls`
/// table. Capped at 5000 rows (newest-visited first) — a Chrome
/// profile can accumulate hundreds of thousands of history rows,
/// and the browser-mode list has no use for more than a recent
/// window of them.
fn read_chrome_history(profile: &Path) -> Vec<BrowserEntry> {
    let Some(snapshot) = copy_sqlite_snapshot(&profile.join("History")) else {
        return Vec::new();
    };
    let result = (|| -> rusqlite::Result<Vec<BrowserEntry>> {
        let conn = open_readonly(&snapshot).ok_or(rusqlite::Error::InvalidPath(snapshot.clone()))?;
        let mut stmt = conn.prepare(
            "SELECT title, url, last_visit_time FROM urls \
             WHERE last_visit_time > 0 ORDER BY last_visit_time DESC LIMIT 5000",
        )?;
        let rows = stmt.query_map([], |row| {
            let title: String = row.get(0)?;
            let url: String = row.get(1)?;
            let last_visit_time: i64 = row.get(2)?;
            Ok(BrowserEntry {
                tag: "history",
                title,
                url,
                timestamp: webkit_micros_to_unix_secs(last_visit_time),
                kind: BrowserKind::Chrome,
            })
        })?;
        rows.collect()
    })();
    cleanup_snapshot_dir(&snapshot);
    result.unwrap_or_default()
}

/// Read a Firefox `places.sqlite` file's bookmarks
/// (`moz_bookmarks` joined with `moz_places`, `type = 1` i.e.
/// `TYPE_BOOKMARK`) and history (`moz_places` rows with a non-null
/// `last_visit_date`). Both queries run against the same snapshot
/// copy since Firefox stores bookmarks and history in one database.
fn read_firefox_places(profile: &Path) -> Vec<BrowserEntry> {
    let Some(snapshot) = copy_sqlite_snapshot(&profile.join("places.sqlite")) else {
        return Vec::new();
    };
    let result = (|| -> rusqlite::Result<Vec<BrowserEntry>> {
        let conn = open_readonly(&snapshot).ok_or(rusqlite::Error::InvalidPath(snapshot.clone()))?;
        let mut entries = Vec::new();

        let mut bookmark_stmt = conn.prepare(
            "SELECT COALESCE(b.title, ''), p.url, COALESCE(b.dateAdded, 0) \
             FROM moz_bookmarks b JOIN moz_places p ON b.fk = p.id \
             WHERE b.type = 1 ORDER BY b.dateAdded DESC LIMIT 5000",
        )?;
        let bookmark_rows = bookmark_stmt.query_map([], |row| {
            let title: String = row.get(0)?;
            let url: String = row.get(1)?;
            let date_added: i64 = row.get(2)?;
            Ok(BrowserEntry {
                tag: "bookmark",
                title,
                url,
                timestamp: firefox_micros_to_unix_secs(date_added),
                kind: BrowserKind::Firefox,
            })
        })?;
        for row in bookmark_rows {
            entries.push(row?);
        }

        let mut history_stmt = conn.prepare(
            "SELECT COALESCE(title, ''), url, last_visit_date FROM moz_places \
             WHERE last_visit_date IS NOT NULL ORDER BY last_visit_date DESC LIMIT 5000",
        )?;
        let history_rows = history_stmt.query_map([], |row| {
            let title: String = row.get(0)?;
            let url: String = row.get(1)?;
            let last_visit_date: i64 = row.get(2)?;
            Ok(BrowserEntry {
                tag: "history",
                title,
                url,
                timestamp: firefox_micros_to_unix_secs(last_visit_date),
                kind: BrowserKind::Firefox,
            })
        })?;
        for row in history_rows {
            entries.push(row?);
        }

        Ok(entries)
    })();
    cleanup_snapshot_dir(&snapshot);
    result.unwrap_or_default()
}

/// Read Safari's `<profile>/Bookmarks.plist`. The file is either a
/// binary or XML property list — the `plist` crate auto-detects the
/// format from the file's magic bytes, so no format flag is needed.
/// Walks the tree recursively: a `WebBookmarkTypeList` node is a
/// folder (descended into via its `Children` array, not emitted as
/// a row); a `WebBookmarkTypeLeaf` node is a bookmark (`URLString`
/// + `URIDictionary.title`).
///
/// Unlike Chrome's bookmarks JSON (which has a `date_added` field
/// on every leaf), Safari's plist has no reliable "date added" for
/// a plain bookmark — only Reading List entries carry a date, under
/// a differently-shaped `ReadingList` sub-dictionary this function
/// doesn't parse. Every Safari bookmark row therefore gets
/// `timestamp: 0` (see this module's doc comment) and sorts as the
/// oldest rows in the merged, newest-first list.
fn read_safari_bookmarks(profile: &Path) -> Vec<BrowserEntry> {
    let path = profile.join("Bookmarks.plist");
    let Ok(value) = plist::Value::from_file(&path) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    walk_safari_bookmark_node(&value, &mut entries);
    entries
}

fn walk_safari_bookmark_node(node: &plist::Value, out: &mut Vec<BrowserEntry>) {
    let Some(dict) = node.as_dictionary() else {
        return;
    };
    let node_type = dict
        .get("WebBookmarkType")
        .and_then(|v| v.as_string())
        .unwrap_or("");
    if node_type == "WebBookmarkTypeLeaf" {
        let url = dict
            .get("URLString")
            .and_then(|v| v.as_string())
            .unwrap_or("")
            .to_string();
        if url.is_empty() {
            return;
        }
        let title = dict
            .get("URIDictionary")
            .and_then(|v| v.as_dictionary())
            .and_then(|d| d.get("title"))
            .and_then(|v| v.as_string())
            .unwrap_or("")
            .to_string();
        out.push(BrowserEntry {
            tag: "bookmark",
            title,
            url,
            timestamp: 0,
            kind: BrowserKind::Safari,
        });
    } else if let Some(children) = dict.get("Children").and_then(|v| v.as_array()) {
        for child in children {
            walk_safari_bookmark_node(child, out);
        }
    }
}

/// Read Safari's `<profile>/History.db` SQLite database:
/// `history_visits` (one row per visit, carrying the title and
/// timestamp of that specific visit) joined with `history_items`
/// (one row per unique URL). Capped at 5000 rows, same as the
/// Chrome / Firefox history reads.
fn read_safari_history(profile: &Path) -> Vec<BrowserEntry> {
    let Some(snapshot) = copy_sqlite_snapshot(&profile.join("History.db")) else {
        return Vec::new();
    };
    let result = (|| -> rusqlite::Result<Vec<BrowserEntry>> {
        let conn = open_readonly(&snapshot).ok_or(rusqlite::Error::InvalidPath(snapshot.clone()))?;
        let mut stmt = conn.prepare(
            "SELECT COALESCE(hv.title, ''), hi.url, hv.visit_time \
             FROM history_visits hv JOIN history_items hi ON hv.history_item = hi.id \
             ORDER BY hv.visit_time DESC LIMIT 5000",
        )?;
        let rows = stmt.query_map([], |row| {
            let title: String = row.get(0)?;
            let url: String = row.get(1)?;
            let visit_time: f64 = row.get(2)?;
            Ok(BrowserEntry {
                tag: "history",
                title,
                url,
                timestamp: mac_absolute_time_to_unix_secs(visit_time),
                kind: BrowserKind::Safari,
            })
        })?;
        rows.collect()
    })();
    cleanup_snapshot_dir(&snapshot);
    result.unwrap_or_default()
}

/// Best-effort removal of the scratch directory a snapshot copy
/// was written into. Failures are ignored — a leftover temp file
/// is harmless clutter, not a correctness issue.
fn cleanup_snapshot_dir(snapshot_path: &Path) {
    if let Some(dir) = snapshot_path.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Read every entry (bookmarks + history) from one configured
/// source.
fn read_source(source: &BrowserSource) -> Vec<BrowserEntry> {
    match source.kind {
        BrowserKind::Chrome => {
            let mut entries = read_chrome_bookmarks(&source.profile);
            entries.extend(read_chrome_history(&source.profile));
            entries
        }
        BrowserKind::Firefox => read_firefox_places(&source.profile),
        BrowserKind::Safari => {
            let mut entries = read_safari_bookmarks(&source.profile);
            entries.extend(read_safari_history(&source.profile));
            entries
        }
    }
}

/// Convert one entry into a `HistoryRow`. `command` is `"<tag>
/// <title-or-url>"` — the literal tag word lets the user narrow the
/// merged list to just bookmarks or just history by typing
/// `bookmark` / `history` as a search term. `comment` carries the
/// raw URL (the secondary column the render layer already shows for
/// every mode); `directory` carries the source browser's name
/// (`chrome` / `firefox`), not rendered by default but available
/// for a future per-browser filter. `id` is a synthetic
/// monotonically-decreasing negative id, same convention as
/// `files::walk_dir` / the todo mode's line numbers.
fn browser_entry_to_row(entry: BrowserEntry, next_id: &mut i64) -> HistoryRow {
    let display_title = if entry.title.is_empty() {
        entry.url.clone()
    } else {
        entry.title.clone()
    };
    let id = *next_id;
    *next_id -= 1;
    HistoryRow {
        id,
        command: format!("{} {}", entry.tag, display_title),
        directory: entry.kind.as_str().to_string(),
        session_id: String::new(),
        exit_code: 0,
        timestamp: entry.timestamp,
        comment: entry.url,
        output: String::new(),
        mode: "browser".to_string(),
        source: entry.tag.to_string(),
        ..Default::default()
    }
}

/// An in-flight browser-mode fetch. Mirrors `files::FilesRequest` /
/// `paperless::PaperlessRequest`.
pub struct BrowserRequest {
    pub receiver: mpsc::Receiver<Vec<HistoryRow>>,
    pub cancelled: Arc<AtomicBool>,
    pub pattern: String,
}

/// Aggregated browser-mode state, owned by `App`. Mirrors
/// `files::FilesState` — see that struct's doc comment.
pub struct BrowserState {
    pub debounce_started: Option<std::time::Instant>,
    pub last_pattern: Option<String>,
    pub in_flight: bool,
    pub request: Option<BrowserRequest>,
    pub rows: Vec<HistoryRow>,
}

impl BrowserState {
    pub fn new() -> Self {
        BrowserState {
            debounce_started: None,
            last_pattern: None,
            in_flight: false,
            request: None,
            rows: Vec::new(),
        }
    }

    /// The canonical pattern for "is this the same search we just
    /// ran?" comparisons — the query body after the leading prefix
    /// char, trimmed.
    pub fn current_pattern(query: &str, prefix: char) -> String {
        let body = if query.starts_with(prefix) {
            &query[prefix.len_utf8()..]
        } else {
            query
        };
        body.trim().to_string()
    }

    pub fn has_results_for(&self, pattern: &str) -> bool {
        self.last_pattern.as_deref() == Some(pattern)
    }
}

impl Default for BrowserState {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn a background thread that reads every configured / auto-
/// detected browser source, filters by `pattern` (every whitespace-
/// separated word must appear, case-insensitively, in the row's
/// `"<tag> <title> <url>"` text — same AND-by-word convention as
/// `files::walk_dir`), sorts newest-first, and sends the result over
/// the returned request's channel.
pub fn spawn_fetch(sources: Vec<BrowserSource>, pattern: String) -> BrowserRequest {
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let tokens: Vec<String> = pattern
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();
    let pattern_clone = pattern.clone();
    std::thread::spawn(move || {
        let mut all_entries: Vec<BrowserEntry> = Vec::new();
        for source in &sources {
            all_entries.extend(read_source(source));
        }
        let mut next_id: i64 = -1;
        let mut rows: Vec<HistoryRow> = all_entries
            .into_iter()
            .filter(|e| {
                if tokens.is_empty() {
                    return true;
                }
                let haystack = format!("{} {} {}", e.tag, e.title, e.url).to_lowercase();
                tokens.iter().all(|tok| haystack.contains(tok.as_str()))
            })
            .map(|e| browser_entry_to_row(e, &mut next_id))
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.timestamp));
        rows.truncate(2000);
        if !cancelled_clone.load(Ordering::Relaxed) {
            let _ = tx.send(rows);
        }
    });
    BrowserRequest {
        receiver: rx,
        cancelled,
        pattern: pattern_clone,
    }
}

#[cfg(test)]
mod tests {
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
}
