//! `smarthistory serve`: an HTTP dashboard for the time-tracking data
//! `smarthistory project report` already computes, as a JSON API plus
//! an embedded single-page web UI.
//!
//! **No authentication.** This binds to `127.0.0.1` by default —
//! loopback only — specifically because there's no auth layer at
//! all; anyone who can reach the port can read every project's
//! tracked directories, commands, notes, and visited URLs for any
//! day. `--host`/`serve.host` can widen the bind for LAN access, but
//! that's an explicit, documented opt-in, never the default (see
//! `docs/server.md`).
//!
//! All data comes from a fresh, short-lived **read-only**
//! (`SQLITE_OPEN_READ_ONLY`) connection opened per request — the
//! dashboard never writes, so there's no shared-connection locking to
//! reason about; SQLite handles concurrent readers natively.
//!
//! Routes:
//! - `GET /api/report?day=<YYYY-MM-DD|today|yesterday>&project=<slug>&min_duration=<secs>`
//!   — one day's [`crate::DayReport`] (all projects, or one when
//!   `project` is given). Same data `ProjectAction::Report` prints as
//!   text — both call [`crate::build_day_report`].
//! - `GET /api/history?project=<slug>&days=<N>` — the trailing `N`
//!   days' (default 7) active-time totals for one project
//!   ([`crate::project_history`]), for the dashboard's search view.
//! - `GET /api/projects` — every known project slug
//!   ([`crate::all_project_slugs`]), for the search box.
//! - Anything else (`/`, `/day/2026-08-16`, `/project/acme`, a hard
//!   refresh on any client-side route, …) falls back to the embedded
//!   `dashboard.html` shell unconditionally. The page's own JS reads
//!   `location.pathname` to decide what to render and drives further
//!   navigation via `history.pushState`/`popstate` — so a direct link
//!   or a browser refresh on a deep route still works, and back/forward
//!   navigate for real.

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

/// The embedded dashboard SPA — HTML/CSS/JS in one file, baked into
/// the binary at compile time (`include_str!`). No build step, no
/// CDN fetch, no separate assets to ship: the compiled `smarthistory`
/// binary is the entire dashboard.
const DASHBOARD_HTML: &str = include_str!("server/dashboard.html");

/// Wrap any error as a `500` JSON response — the handlers below only
/// ever fail on a DB/query error, not on malformed input (query
/// params all have sane defaults), so one blanket mapping is enough.
struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.0.to_string() });
        (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        AppError(err.into())
    }
}

/// Open a fresh, read-only connection to the same database every CLI
/// command uses (`crate::get_db_path`) — never a shared/mutable
/// connection, since every dashboard request is read-only.
///
/// `smarthistory serve` is meant to run alongside `smarthistory
/// daemon` and every shell's own `smarthistory add` writes — a
/// dashboard request racing the exact instant one of those holds a
/// write lock would otherwise fail immediately with `SQLITE_BUSY`
/// instead of just waiting the (sub-millisecond, single-statement)
/// moment out, so this sets the same busy_timeout the daemon's own
/// connection uses.
fn open_readonly_conn() -> anyhow::Result<Connection> {
    let path = crate::get_db_path();
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

/// Run `f` on tokio's blocking-thread pool instead of the async
/// runtime's own worker thread. Every handler below is really a
/// synchronous CLI-style call (SQLite + filesystem I/O), not
/// `async`-native work — and `build_day_report` can, when JIRA is
/// configured, build and drop a `reqwest::blocking::Client` to
/// resolve a JIRA-linked website visit. `reqwest::blocking` spins up
/// its own mini tokio runtime internally; doing that from a worker
/// thread that's already inside `axum::serve`'s runtime panics
/// ("Cannot drop a runtime in a context where blocking is not
/// allowed") the instant a viewed day has a JIRA-linked visit.
/// `spawn_blocking` runs `f` on a dedicated thread outside any async
/// context, sidestepping that entirely — and, as a side effect, keeps
/// the DB/filesystem work off the runtime's worker threads too.
async fn spawn_blocking_result<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(f).await?
}

#[derive(Debug, Deserialize)]
struct ReportQuery {
    day: Option<String>,
    project: Option<String>,
    min_duration: Option<i64>,
}

async fn report_handler(Query(q): Query<ReportQuery>) -> Result<Json<serde_json::Value>, AppError> {
    let report = spawn_blocking_result(move || {
        let conn = open_readonly_conn()?;
        let cfg = crate::Config::load();
        let (range_start, range_end, date) = crate::parse_project_report_day(&q.day)?;
        let report = crate::build_day_report(
            &conn,
            &cfg,
            range_start,
            range_end,
            &date,
            q.project.as_deref(),
            q.min_duration.unwrap_or(0),
        )?;
        Ok(serde_json::to_value(report)?)
    })
    .await?;
    Ok(Json(report))
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    project: String,
    days: Option<u32>,
}

async fn history_handler(
    Query(q): Query<HistoryQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let history = spawn_blocking_result(move || {
        let conn = open_readonly_conn()?;
        let cfg = crate::Config::load();
        let days = q.days.unwrap_or(7).clamp(1, 366);
        let history = crate::project_history(&conn, &cfg, &q.project, days)?;
        Ok(serde_json::to_value(history)?)
    })
    .await?;
    Ok(Json(history))
}

async fn projects_handler() -> Result<Json<serde_json::Value>, AppError> {
    let slugs = spawn_blocking_result(|| {
        let conn = open_readonly_conn()?;
        let cfg = crate::Config::load();
        let slugs = crate::all_project_slugs(&conn, &cfg)?;
        Ok(serde_json::to_value(slugs)?)
    })
    .await?;
    Ok(Json(slugs))
}

async fn dashboard_handler() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

fn build_router() -> Router {
    Router::new()
        .route("/api/report", get(report_handler))
        .route("/api/history", get(history_handler))
        .route("/api/projects", get(projects_handler))
        .fallback(get(dashboard_handler))
}

/// Bind and serve until interrupted. `host`/`port` are already
/// resolved (CLI flag, falling back to `serve.host`/`serve.port`,
/// falling back to `127.0.0.1`/`4590` — see `Commands::Serve`'s
/// dispatch in `main.rs`) by the time this is called.
pub fn run(host: &str, port: u16) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let addr = format!("{host}:{port}");
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| anyhow::anyhow!("failed to bind {addr}: {e}"))?;
        eprintln!("smarthistory serve: listening on http://{addr}");
        if host != "127.0.0.1" && host != "localhost" {
            eprintln!(
                "smarthistory serve: WARNING — bound to {host}, not loopback-only. This \
                 dashboard has no authentication; anyone who can reach this address can read \
                 every tracked project's directories, commands, notes, and visited URLs."
            );
        }
        axum::serve(listener, build_router())
            .await
            .map_err(|e| anyhow::anyhow!("server error: {e}"))
    })
}

#[cfg(test)]
mod tests;
