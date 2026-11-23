//! Source directories: directories containing `.git` or `build.xml`,
//! recorded when the shell changes into them and searched by the
//! `Source` TUI mode.

use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

/// One row of the `sources` table. Doubles as the export/import format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceRecord {
    pub(crate) path: String,
    pub(crate) is_git: bool,
    pub(crate) first_seen: i64,
    pub(crate) last_visited: i64,
    pub(crate) visit_count: i64,
}

/// What [`classify_source`] found for a directory. `path` can differ
/// from the inspected directory: a linked worktree resolves to its main
/// repository so each repository is listed once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClassifiedSource {
    pub(crate) path: String,
    pub(crate) is_git: bool,
}

pub(crate) fn ensure_sources_table(conn: &Connection) -> anyhow::Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sources (
            path TEXT PRIMARY KEY,
            is_git INTEGER NOT NULL,
            first_seen INTEGER NOT NULL,
            last_visited INTEGER NOT NULL,
            visit_count INTEGER NOT NULL DEFAULT 1
        )",
        [],
    )?;
    Ok(())
}

/// Record one visit to a source directory.
pub(crate) fn upsert_source(
    conn: &Connection,
    path: &str,
    is_git: bool,
    now: i64,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO sources (path, is_git, first_seen, last_visited, visit_count)
         VALUES (?1, ?2, ?3, ?3, 1)
         ON CONFLICT (path) DO UPDATE SET
            is_git = excluded.is_git,
            last_visited = MAX(last_visited, excluded.last_visited),
            visit_count = visit_count + 1",
        params![path, is_git, now],
    )?;
    Ok(())
}

/// Every recorded source, most recently visited first.
pub(crate) fn list_sources(conn: &Connection) -> anyhow::Result<Vec<SourceRecord>> {
    let mut stmt = conn.prepare(
        "SELECT path, is_git, first_seen, last_visited, visit_count
         FROM sources ORDER BY last_visited DESC, path ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SourceRecord {
                path: row.get(0)?,
                is_git: row.get(1)?,
                first_seen: row.get(2)?,
                last_visited: row.get(3)?,
                visit_count: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Remove a source. Returns whether a row existed.
pub(crate) fn delete_source(conn: &Connection, path: &str) -> anyhow::Result<bool> {
    Ok(conn.execute("DELETE FROM sources WHERE path = ?1", params![path])? > 0)
}

/// Insert `record`, or merge it into an existing row for the same path.
fn merge_record(conn: &Connection, record: &SourceRecord) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO sources (path, is_git, first_seen, last_visited, visit_count)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (path) DO UPDATE SET
            is_git = MAX(is_git, excluded.is_git),
            first_seen = MIN(first_seen, excluded.first_seen),
            last_visited = MAX(last_visited, excluded.last_visited),
            visit_count = MAX(visit_count, excluded.visit_count)",
        params![
            record.path,
            record.is_git,
            record.first_seen,
            record.last_visited,
            record.visit_count
        ],
    )?;
    Ok(())
}

/// Merge exported rows. Importing the same file twice changes nothing.
pub(crate) fn import_sources(conn: &Connection, records: &[SourceRecord]) -> anyhow::Result<usize> {
    let tx = conn.unchecked_transaction()?;
    for record in records {
        merge_record(&tx, record)?;
    }
    tx.commit()?;
    Ok(records.len())
}

/// Rewrite stored paths through `rewrite`, merging rows that collapse
/// onto the same path. Returns the number of rows rewritten.
pub(crate) fn rewrite_source_paths(
    conn: &Connection,
    rewrite: impl Fn(&str) -> String,
) -> anyhow::Result<usize> {
    let records = list_sources(conn)?;
    let tx = conn.unchecked_transaction()?;
    let mut rewritten = 0;
    for record in records {
        let new_path = rewrite(&record.path);
        if new_path == record.path {
            continue;
        }
        tx.execute("DELETE FROM sources WHERE path = ?1", params![record.path])?;
        merge_record(&tx, &SourceRecord { path: new_path, ..record })?;
        rewritten += 1;
    }
    tx.commit()?;
    Ok(rewritten)
}

/// Decide whether `dir` is a source directory. Uses `symlink_metadata`
/// and never spawns `git`. It can block on a dead mount, so call it only
/// from the detached `record-source` process or a background thread.
pub(crate) fn classify_source(dir: &Path) -> Option<ClassifiedSource> {
    let dot_git = dir.join(".git");
    if let Ok(meta) = std::fs::symlink_metadata(&dot_git) {
        if meta.is_file()
            && let Some(main_repo) = linked_worktree_main_repo(dir, &dot_git)
        {
            return Some(ClassifiedSource {
                path: main_repo.to_string_lossy().into_owned(),
                is_git: true,
            });
        }
        if meta.is_file() || meta.is_dir() {
            return Some(ClassifiedSource {
                path: dir.to_string_lossy().into_owned(),
                is_git: true,
            });
        }
    }
    let build_xml = std::fs::symlink_metadata(dir.join("build.xml")).ok()?;
    build_xml.is_file().then(|| ClassifiedSource {
        path: dir.to_string_lossy().into_owned(),
        is_git: false,
    })
}

/// The main repository of the linked worktree at `dir`, or `None` for
/// anything else. Git writes a `commondir` file only into a linked
/// worktree's admin directory, which distinguishes it from a submodule,
/// whose `.git` file also holds a `gitdir:` pointer.
fn linked_worktree_main_repo(dir: &Path, dot_git: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(dot_git).ok()?;
    let gitdir = contents.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let gitdir = normalize_lexically(&dir.join(gitdir));
    let commondir = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = normalize_lexically(&gitdir.join(commondir.trim()));
    // A bare repository's common dir is the repository itself, and it
    // is the path `git -C` needs later.
    if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().map(Path::to_path_buf)
    } else {
        Some(common)
    }
}

/// Resolve `.` and `..` without touching the filesystem.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// What to do to open a directory in the multiplexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionTarget {
    /// Focus the existing session with this id.
    Reuse(String),
    /// Create a session with this name.
    Create(String),
}

/// A name safe to use as a tmux target: tmux treats `.` and `:` as
/// target separators and a leading `=` as an exact-match marker.
pub(crate) fn sanitize_session_name(base: &str) -> String {
    let name: String = base
        .chars()
        .enumerate()
        .map(|(i, c)| if c == '.' || c == ':' || (i == 0 && c == '=') { '-' } else { c })
        .collect();
    if name.is_empty() { "source".to_string() } else { name }
}

/// Pick the session for `dir`: `base`, then `base-2`, `base-3`, and so
/// on. The first candidate rooted at `dir` is reused, and the first
/// unused name is created. `dir` and session roots must already be in
/// the same canonical form.
pub(crate) fn resolve_session_target(
    base: &str,
    dir: &str,
    sessions: &[crate::multiplexer::MuxSession],
) -> SessionTarget {
    let base = sanitize_session_name(base);
    for n in 1.. {
        let candidate = if n == 1 { base.clone() } else { format!("{base}-{n}") };
        let mut named = sessions.iter().filter(|s| s.name == candidate).peekable();
        if named.peek().is_none() {
            return SessionTarget::Create(candidate);
        }
        if let Some(session) = named.find(|s| s.roots.iter().any(|r| r == dir)) {
            return SessionTarget::Reuse(session.id.clone());
        }
    }
    unreachable!("the session list is finite, so an unused name exists")
}

/// Entry point of the hidden `record-source` command. Opens the database
/// only for actual source directories, so an ordinary `cd` costs just
/// two `lstat` calls.
pub(crate) fn record_current_directory() -> anyhow::Result<()> {
    let cwd = crate::util::current_directory_for_storage();
    if cwd.is_empty() {
        return Ok(());
    }
    let Some(source) = classify_source(Path::new(&cwd)) else {
        return Ok(());
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let conn = crate::init_db()?;
    upsert_source(&conn, &source.path, source.is_git, now)
}

#[cfg(test)]
mod tests;
