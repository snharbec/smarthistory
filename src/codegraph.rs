//! CodeGraph index access.
//!
//! Reads the local `.codegraph/codegraph.db` SQLite index produced by the
//! CodeGraph tooling. The index stores source-code symbol nodes (functions,
//! methods, classes, …) plus relationship edges (`calls`, `contains`,
//! `references`, …) and a FTS5 table over node names/qualified-names/
//! docstrings/signatures for fast symbol search.
//!
//! This module is intentionally self-contained: it owns a single read-only
//! `rusqlite::Connection` and exposes three queries the TUI needs:
//!
//! * [`CodeGraphClient::search`] — FTS symbol search by name.
//! * [`CodeGraphClient::callers`] — who calls a given node (edges with
//!   `kind='calls'` and `target=<id>`).
//! * [`CodeGraphClient::callees`] — what a given node calls (edges with
//!   `kind='calls'` and `source=<id>`).
//!
//! The connection is opened read-only and cached on the [`App`] so repeated
//! keystrokes in `&` mode reuse one fd instead of reopening the database.

use std::path::{Path, PathBuf};

use rusqlite::{params, OpenFlags};

/// A single symbol node from the CodeGraph index. Only the fields the TUI
/// actually renders are loaded.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CodeGraphNode {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub language: String,
    pub start_line: i64,
    pub end_line: i64,
    pub signature: Option<String>,
    pub docstring: Option<String>,
}

impl CodeGraphNode {
    /// Absolute path of the source file inside `repo_root`. The `file_path`
    /// stored in the index is relative to the repo root (the directory
    /// containing `.codegraph/`), so callers join it with the repo root to
    /// produce an editor-openable path.
    pub fn abs_path(&self, repo_root: &Path) -> PathBuf {
        let p = Path::new(&self.file_path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            repo_root.join(&self.file_path)
        }
    }
}

/// A read-only client over a `.codegraph/codegraph.db` index.
pub struct CodeGraphClient {
    conn: rusqlite::Connection,
    /// The repo root (the directory that *contains* `.codegraph/`).
    /// Used to resolve the relative `file_path` stored in the index into
    /// an absolute path the editor can open.
    repo_root: PathBuf,
}

impl CodeGraphClient {
    /// Open the codegraph index located in `.codegraph/codegraph.db` under
    /// the current working directory or any ancestor. Returns `None` when
    /// no index exists — the caller falls back to an empty result list so
    /// `&` mode is a no-op in repos without a `.codegraph/` directory.
    pub fn open() -> Option<Self> {
        let db_path = find_codegraph_db()?;
        let repo_root = db_path
            .parent()
            .and_then(|p| p.parent()) // strip `.codegraph/codegraph.db`
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        // Read-only so we never mutate the index a background indexer may
        // also be writing to. `SQLITE_OPEN_READ_ONLY` avoids taking a write
        // lock and tolerates concurrent indexer runs.
        let conn = rusqlite::Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )
        .ok()?;
        // `query_only` is belt-and-suspenders: even if some future code path
        // tried a write through this connection, SQLite refuses it.
        let _ = conn.pragma_update(None, "query_only", true);
        Some(CodeGraphClient { conn, repo_root })
    }

    /// The repo root this index was opened for.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// FTS symbol search. `pattern` is split on whitespace into tokens; each
    /// token becomes an FTS5 prefix term (`tok*`) and all tokens are ANDed
    /// (matching the substring semantics of the `tags` / `ag` modes).
    /// `language_filter`, when `Some`, restricts results to that language
    /// (case-insensitive match on the `nodes.language` column). An empty
    /// `pattern` returns an empty list — listing every symbol in a 350k-node
    /// index is useless in the TUI.
    pub fn search(
        &self,
        pattern: &str,
        language_filter: Option<&str>,
        limit: i64,
    ) -> Vec<CodeGraphNode> {
        // Sanitize each whitespace token into a safe FTS5 prefix term.
        // Only identifier-ish characters are kept; anything else would
        // break FTS5's query parser. An empty token list means "no
        // search" — return empty rather than listing the whole index.
        let mut fts_terms: Vec<String> = Vec::new();
        for tok in pattern.split_whitespace() {
            let cleaned: String = tok
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if cleaned.is_empty() {
                continue;
            }
            // FTS5 prefix: `tok*` matches any token starting with `tok`.
            // Quoting the cleaned token defends against reserved words
            // (AND/OR/NOT/NEAR) being interpreted as operators.
            fts_terms.push(format!("\"{}\"*", cleaned));
        }
        if fts_terms.is_empty() {
            return Vec::new();
        }
        let fts_query = fts_terms.join(" ");
        let sql = if language_filter.is_some() {
            "SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path, \
             n.language, n.start_line, n.end_line, n.signature, n.docstring \
             FROM nodes_fts f \
             JOIN nodes n ON n.rowid = f.rowid \
             WHERE nodes_fts MATCH ?1 AND n.language = ?2 COLLATE NOCASE \
             ORDER BY n.kind ASC, n.name ASC \
             LIMIT ?3"
        } else {
            "SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path, \
             n.language, n.start_line, n.end_line, n.signature, n.docstring \
             FROM nodes_fts f \
             JOIN nodes n ON n.rowid = f.rowid \
             WHERE nodes_fts MATCH ?1 \
             ORDER BY n.kind ASC, n.name ASC \
             LIMIT ?2"
        };
        let mut stmt = match self.conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let map = |row: &rusqlite::Row| -> rusqlite::Result<CodeGraphNode> {
            Ok(CodeGraphNode {
                id: row.get(0)?,
                kind: row.get(1)?,
                name: row.get(2)?,
                qualified_name: row.get(3)?,
                file_path: row.get(4)?,
                language: row.get(5)?,
                start_line: row.get(6)?,
                end_line: row.get(7)?,
                signature: row.get(8)?,
                docstring: row.get(9)?,
            })
        };

        if let Some(lang) = language_filter {
            match stmt.query_map(params![fts_query, lang, limit], map) {
                Ok(iter) => iter.filter_map(Result::ok).collect(),
                Err(_) => Vec::new(),
            }
        } else {
            match stmt.query_map(params![fts_query, limit], map) {
                Ok(iter) => iter.filter_map(Result::ok).collect(),
                Err(_) => Vec::new(),
            }
        }
    }

    /// Resolve the nodes that *call* `node_id` (edges with `kind='calls'`
    /// and `target=<node_id>`; the caller is the `source` node of each
    /// such edge). Returns up to `limit` callers, sorted by file then
    /// line for a stable, readable order.
    pub fn callers(&self, node_id: &str, limit: i64) -> Vec<CodeGraphNode> {
        // relation(edge_side, node_id): the JOIN is on `edge_side`
        // (the side we return — the caller is the `source` node),
        // and the filter is on the other side (`target` == node_id).
        self.relation("source", node_id, limit)
    }

    /// Resolve the nodes that `node_id` *calls* (edges with `kind='calls'`
    /// and `source=<node_id>`; the callee is the `target` node).
    pub fn callees(&self, node_id: &str, limit: i64) -> Vec<CodeGraphNode> {
        self.relation("target", node_id, limit)
    }

    fn relation(&self, edge_side: &str, node_id: &str, limit: i64) -> Vec<CodeGraphNode> {
        // edge_side is a static literal ("source" or "target"); it's safe to
        // interpolate into the SQL string here.
        let sql = format!(
            "SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path, \
             n.language, n.start_line, n.end_line, n.signature, n.docstring \
             FROM edges e \
             JOIN nodes n ON n.id = e.{side} \
             WHERE e.kind = 'calls' AND e.{other} = ?1 \
             ORDER BY n.file_path ASC, n.start_line ASC \
             LIMIT ?2",
            side = edge_side,
            other = if edge_side == "source" {
                "target"
            } else {
                "source"
            }
        );
        let Ok(mut stmt) = self.conn.prepare(&sql) else {
            return Vec::new();
        };
        let rows = stmt.query_map(params![node_id, limit], |row| {
            Ok(CodeGraphNode {
                id: row.get(0)?,
                kind: row.get(1)?,
                name: row.get(2)?,
                qualified_name: row.get(3)?,
                file_path: row.get(4)?,
                language: row.get(5)?,
                start_line: row.get(6)?,
                end_line: row.get(7)?,
                signature: row.get(8)?,
                docstring: row.get(9)?,
            })
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.filter_map(Result::ok).collect()
    }

    /// Load a single node by id (used when the selected row came from the
    /// tags-mode fallback and we still want its callers/callees overlay).
    #[allow(dead_code)]
    pub fn node_by_id(&self, node_id: &str) -> Option<CodeGraphNode> {
        let sql = "SELECT id, kind, name, qualified_name, file_path, language, \
             start_line, end_line, signature, docstring FROM nodes WHERE id = ?1";
        self.conn
            .prepare(sql)
            .ok()?
            .query_row(params![node_id], |row| {
                Ok(CodeGraphNode {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    name: row.get(2)?,
                    qualified_name: row.get(3)?,
                    file_path: row.get(4)?,
                    language: row.get(5)?,
                    start_line: row.get(6)?,
                    end_line: row.get(7)?,
                    signature: row.get(8)?,
                    docstring: row.get(9)?,
                })
            })
            .ok()
    }
}

/// Walk from the current directory upward looking for
/// `.codegraph/codegraph.db`. The first match wins (closest to the cwd),
/// mirroring how [`find_tags_file`] discovers tag files.
pub fn find_codegraph_db() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".codegraph").join("codegraph.db");
        if candidate.is_file() {
            return Some(candidate);
        }
        match dir.parent() {
            Some(parent) if parent != dir => {
                dir = parent.to_path_buf();
            }
            _ => break,
        }
    }
    None
}
