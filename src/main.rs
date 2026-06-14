mod tui;
mod util;

use chrono::Datelike;
use clap::Parser;
use rusqlite::{params, Connection};
use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Process start instant, captured on first use. Mixed into UUID generation so
/// distinct invocations of the binary produce distinct IDs even when the wall
/// clock and counter alone would collide (e.g. fast successive calls).
fn process_start_instant() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// Returns a UUID v4 string (e.g. "f47ac10b-58cc-4372-a567-0e02b2c3d479").
///
/// Entropy sources (no /dev/urandom, no OS RNG, no uuidgen):
///   - wall-clock nanoseconds since UNIX_EPOCH
///   - monotonic time since process start
///   - the process PID
///   - a process-lifetime atomic counter
///
/// All four are mixed through a splitmix64-style hash to fill 16 bytes,
/// and the version/variant bits are set per RFC 4122.
fn generate_uuid_v4() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);

    let wall_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mono_nanos = process_start_instant().elapsed().as_nanos() as u64;
    let pid = process::id() as u64;

    // splitmix64: x ^= x >> 30; x = x.wrapping_mul(0xbf58476d1ce4e5b9); x ^= x >> 27; ...
    fn splitmix64(mut x: u64) -> u64 {
        x ^= x >> 30;
        x = x.wrapping_mul(0xbf58476d1ce4e5b9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94d049bb133111eb);
        x ^= x >> 31;
        x
    }

    let lo = splitmix64(wall_nanos ^ n);
    let hi = splitmix64(mono_nanos ^ pid ^ n.rotate_left(17));

    let mut b = [0u8; 16];
    b[0..8].copy_from_slice(&lo.to_le_bytes());
    b[8..16].copy_from_slice(&hi.to_le_bytes());

    // RFC 4122 v4 bits
    b[6] = (b[6] & 0x0f) | 0x40;
    // RFC 4122 variant bits
    b[8] = (b[8] & 0x3f) | 0x80;

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
    )
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Smart history: ZSH-style persistent command history in SQLite",
    long_about = "Smart history: ZSH-style persistent command history in SQLite.\n\n\
                  Available field names for --fields (search, select, list):\n  \
                  raw columns:    id, command, directory, session_id, exit_code, timestamp\n  \
                  derived fields: time (formatted timestamp), diff (age, e.g. \"2h\", \"5M\"), base (leaf directory)\n\n\
                  The default field is `command`. Derived fields are computed in\n\
                  Rust from the raw `timestamp` and `directory` columns."
)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    Add {
        command: String,
        #[arg(short, long)]
        exit_code: i32,
    },
    Search {
        #[arg(index = 1)]
        query: Option<String>,
        #[arg(short, long)]
        directory: Option<String>,
        /// When set, restrict results to the current $SMART_HISTORY_SESSION.
        #[arg(short, long)]
        session: bool,
        #[arg(long)]
        exit_code: Option<String>,
        /// Comma-separated list of columns to return. Available: command,
        /// directory, session_id, exit_code, timestamp, id, time, diff, base.
        /// May also be passed multiple times: -f command -f directory.
        #[arg(short, long, value_delimiter = ',')]
        fields: Option<Vec<String>>,
        /// Maximum number of rows to return. Default 100. Use 0 for no limit.
        #[arg(short, long)]
        limit: Option<usize>,
        /// Disable the bracket / ANSI-bold highlight around the search
        /// substring in the `command` field. Used by the line-editor
        /// widget so the chosen command is inserted verbatim.
        #[arg(long)]
        no_highlight: bool,
    },
    Select {
        #[arg(index = 1)]
        query: Option<String>,
        #[arg(short, long)]
        directory: Option<String>,
        /// When set, restrict results to the current $SMART_HISTORY_SESSION.
        #[arg(short, long)]
        session: bool,
        #[arg(long)]
        exit_code: Option<String>,
        /// Comma-separated list of columns to return. Available: command,
        /// directory, session_id, exit_code, timestamp, id, time, diff, base.
        /// May also be passed multiple times: -f command -f directory.
        #[arg(short, long, value_delimiter = ',')]
        fields: Option<Vec<String>>,
        /// Maximum number of rows to return. Default 1000. Use 0 for no limit.
        #[arg(short, long)]
        limit: Option<usize>,
        /// Disable the bracket / ANSI-bold highlight around the search
        /// substring in the `command` field.
        #[arg(long)]
        no_highlight: bool,
    },
    Tui {
        #[arg(short, long)]
        mode: Option<String>,
        #[arg(index = 1)]
        query: Option<String>,
    },
    ImportAtuin,
    List {
        /// Comma-separated list of columns to return. Available: command,
        /// directory, session_id, exit_code, timestamp, id, time, diff, base.
        /// May also be passed multiple times: -f command -f directory.
        #[arg(short, long, value_delimiter = ',')]
        fields: Option<Vec<String>>,
        #[arg(short, long)]
        table: bool,
    },
    Init {
        shell: String,
    },
    /// Delete entries matching the given filter. With no filter, deletes
    /// every entry in the database. Prompts for confirmation unless
    /// --force is passed.
    Clean {
        #[arg(index = 1)]
        query: Option<String>,
        #[arg(short, long)]
        directory: Option<String>,
        /// When set, restrict deletion to the current $SMART_HISTORY_SESSION.
        #[arg(short, long)]
        session: bool,
        #[arg(long)]
        exit_code: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(short, long)]
        force: bool,
    },
    /// Return the most probable next commands that follow the given
    /// command in the global history, ordered by frequency (then
    /// lexicographically for ties). Used by the Ctrl-S line-editor
    /// widget to suggest likely next steps.
    Next {
        /// The command whose successors to look up.
        command: String,
        /// Maximum number of candidates to return. Default 5.
        #[arg(short, long)]
        limit: Option<usize>,
    },
}

fn get_db_path() -> PathBuf {
    let home = env::var("HOME").expect("HOME not set");
    PathBuf::from(home)
        .join(".local")
        .join("cache")
        .join("smarthistory")
        .join("smarthistory.db")
}

/// Reserved field names that are computed in Rust from the raw columns
/// Decode a single column to a String, trying TEXT then INTEGER.
fn cell_to_string(row: &rusqlite::Row, i: usize) -> String {
    if let Ok(s) = row.get::<_, String>(i) {
        s
    } else if let Ok(t) = row.get::<_, i64>(i) {
        t.to_string()
    } else {
        "N/A".to_string()
    }
}

/// Format a single output row, given the raw column names (in the order
/// they appear in `row_data`) and the user-requested `fields` (which may
/// include derived names). The output preserves the user's field order.
/// Wrap occurrences of `needle` in `haystack` with the given markers.
/// Case-sensitive. Returns the (possibly multi-segment) concatenation.
fn highlight(haystack: &str, needle: &str, open: &str, close: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let mut out = String::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(pos) = rest.find(needle) {
        out.push_str(&rest[..pos]);
        out.push_str(open);
        out.push_str(needle);
        out.push_str(close);
        rest = &rest[pos + needle.len()..];
    }
    out.push_str(rest);
    out
}

/// Escape the SQLite `LIKE` wildcards (`%` and `_`) in a user-supplied
/// search string. (Implementation in `crate::util`; kept as a
/// re-export so existing call sites compile unchanged.)
use crate::util::escape_like;

/// True if stdout is connected to a terminal (so we can emit ANSI escapes
/// without polluting piped output).
fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// Build the highlight markers. Bracket markers are always used; ANSI bold
/// is added on top when stdout is a TTY.
fn highlight_markers() -> (&'static str, &'static str) {
    if stdout_is_tty() {
        ("\x1b[1m", "\x1b[0m") // bold on, reset
    } else {
        ("[", "]")
    }
}

fn project_row(
    row_data: &[(String, String)],
    fields: &[String],
    derived: &[String],
    query: Option<&str>,
    no_highlight: bool,
) -> Vec<String> {
    let (open, close) = if no_highlight {
        ("", "")
    } else {
        highlight_markers()
    };
    let mut out = Vec::with_capacity(fields.len());
    for f in fields {
        if derived.contains(f) {
            out.push(compute_derived(f, row_data));
        } else if let Some((_, v)) = row_data.iter().find(|(k, _)| k.as_str() == f) {
            let cell = v.clone();
            // Highlight the search string only in the `command` field.
            // Other fields (directory, base, etc.) won't contain it because
            // the SQL WHERE filters on `command LIKE ?`.
            if f == "command" {
                if !no_highlight {
                    if let Some(q) = query {
                        out.push(highlight(&cell, q, open, close));
                    } else {
                        out.push(cell);
                    }
                } else {
                    out.push(cell);
                }
            } else {
                out.push(cell);
            }
        } else {
            out.push("N/A".to_string());
        }
    }
    out
}

/// Split a user-supplied field list into (raw_columns, derived_set).
/// `raw_columns` are the columns to fetch from SQLite (the raw table
/// columns the derived fields depend on are auto-included). `derived_set`
/// is the set of derived field names the user asked for, in user order.
fn split_fields(fields: &[String]) -> (Vec<String>, Vec<String>) {
    let mut raw: Vec<String> = Vec::new();
    let mut derived: Vec<String> = Vec::new();
    let mut have_timestamp = false;
    let mut have_directory = false;
    for f in fields {
        if DERIVED_FIELDS.contains(&f.as_str()) {
            if !derived.contains(f) {
                derived.push(f.clone());
            }
            if (f == "time" || f == "diff") && !have_timestamp {
                raw.push("timestamp".to_string());
                have_timestamp = true;
            }
            if f == "base" && !have_directory {
                raw.push("directory".to_string());
                have_directory = true;
            }
        } else {
            if !raw.contains(f) {
                raw.push(f.clone());
            }
        }
    }
    (raw, derived)
}

/// Reserved field names that are computed in Rust from the raw columns
/// (`timestamp`, `directory`) rather than read directly from the table.
const DERIVED_FIELDS: &[&str] = &["time", "diff", "base"];

/// Produce the value for a single derived field, given the raw row.
/// `raw_row` is the (raw_field, value) pairs in the order of the SQL select.
fn compute_derived(name: &str, raw_row: &[(String, String)]) -> String {
    match name {
        "time" => raw_row
            .iter()
            .find(|(k, _)| k.as_str() == "timestamp")
            .and_then(|(_, v)| v.parse::<i64>().ok())
            .map(format_time)
            .unwrap_or_else(|| "N/A".to_string()),
        "diff" => raw_row
            .iter()
            .find(|(k, _)| k.as_str() == "timestamp")
            .and_then(|(_, v)| v.parse::<i64>().ok())
            .map(format_diff)
            .unwrap_or_else(|| "N/A".to_string()),
        "base" => raw_row
            .iter()
            .find(|(k, _)| k.as_str() == "directory")
            .map(|(_, v)| format_base(v))
            .unwrap_or_else(|| "N/A".to_string()),
        _ => "N/A".to_string(),
    }
}

/// Format a Unix epoch (seconds) as "dd.Mon.YYYY HH:MM:SS" in UTC, e.g.
/// "03.Jun.2026 17:43:01". Returns "N/A" if the value is out of range.
/// (Implementation in `crate::util`; kept as a re-export so existing
/// call sites compile unchanged.)
use crate::util::format_time;

/// Human-readable difference between `epoch` and now, using the largest
/// non-zero unit. Ladder (with short unit suffixes):
///   month  -> "1M", 2M, ...
///   day    -> "1d", 2d, ...
///   hour   -> "1h", 2h, ...
///   minute -> "1m", 2m, ...
///   second -> "1s", 2s, ...
/// Returns "N/A" for non-positive or out-of-range timestamps.
fn format_diff(epoch: i64) -> String {
    let now = chrono::Utc::now().naive_utc();
    let Some(then) = chrono::DateTime::from_timestamp(epoch, 0).map(|dt| dt.naive_utc()) else {
        return "N/A".to_string();
    };
    if epoch <= 0 {
        return "N/A".to_string();
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

/// Leaf directory name of a stored path. For "/Users/har/projects/foo"
/// returns "foo". For "/" or empty strings returns the input unchanged.
fn format_base(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

/// Field names whose values should be right-aligned (padded with leading
/// spaces) so columns line up. Both are numeric/short-code and padding
/// does not corrupt the visible content.
const PADDED_FIELDS: &[&str] = &["timestamp", "diff"];

/// Right-pad each cell in every row to the maximum width of its column,
/// but only for field names in PADDED_FIELDS. Other fields (text like
/// `command`, `directory`, `base`) are returned as-is so no leading
/// whitespace is introduced into the actual data.
fn pad_rows(rows: &[Vec<String>], fields: &[String]) -> Vec<Vec<String>> {
    let mut widths: Vec<usize> = vec![0; fields.len()];
    for r in rows {
        for (i, cell) in r.iter().enumerate() {
            if i < fields.len()
                && PADDED_FIELDS.contains(&fields[i].as_str())
                && cell.chars().count() > widths[i]
            {
                widths[i] = cell.chars().count();
            }
        }
    }
    rows.iter()
        .map(|r| {
            r.iter()
                .enumerate()
                .map(|(i, cell)| {
                    if i < fields.len() && PADDED_FIELDS.contains(&fields[i].as_str()) {
                        let w = widths[i];
                        let pad = w.saturating_sub(cell.chars().count());
                        format!("{}{}", " ".repeat(pad), cell)
                    } else {
                        cell.clone()
                    }
                })
                .collect()
        })
        .collect()
}

/// Read a single line from stdin and return true if it starts with "y" or
/// "Y" (after trimming). Anything else (including EOF) returns false.
/// Used for destructive-action confirmations.
fn confirm(prompt: &str) -> bool {
    eprint!("{}", prompt);
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) => false, // EOF
        Ok(_) => {
            let trimmed = line.trim();
            trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes")
        }
        Err(_) => false,
    }
}

/// Append ` ORDER BY timestamp DESC [LIMIT n]` to `sql`. A `limit` of 0
/// means "no limit" and the `LIMIT` clause is omitted. The newest
/// entries come first so the line-editor widget's first Up/Down press
/// shows the most recent command in scope.
fn append_order_and_limit(sql: &mut String, limit: usize) {
    sql.push_str(" ORDER BY timestamp DESC");
    if limit > 0 {
        sql.push_str(&format!(" LIMIT {}", limit));
    }
}

/// Build the shared `WHERE 1=1 [AND ...]` clause and its bound parameters
/// for the history filter (`query`, `directory`, `session`, `exit_code`).
/// Returns the clause (including the leading ` WHERE `) and the params in
/// order. The session filter reads `$SMART_HISTORY_SESSION` at call time
/// and emits a warning to stderr if the flag was passed but the env var
/// is unset/empty. `query` is wrapped in `%...%` for `LIKE`; the others
/// are matched exactly. `exit_code` accepts "OK" (=0) or "ERROR" (!=0);
/// any other value is ignored.
fn build_where_clause(
    query: Option<&str>,
    directory: Option<String>,
    session_flag: bool,
    exit_code: Option<&str>,
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut clause = String::from(" WHERE 1=1");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(q) = query {
        // Escape LIKE wildcards in the user's query so `100%` matches
        // a literal `100%` rather than anything starting with `100`.
        // The ESCAPE clause tells SQLite to use `\` as the escape
        // character (default is no escape).
        clause.push_str(" AND command LIKE ? ESCAPE '\\'");
        params.push(Box::new(format!("%{}%", escape_like(q))));
    }
    if let Some(dir) = directory {
        clause.push_str(" AND directory = ?");
        params.push(Box::new(dir));
    }
    if session_flag {
        match env::var("SMART_HISTORY_SESSION") {
            Ok(s) if !s.is_empty() => {
                clause.push_str(" AND session_id = ?");
                params.push(Box::new(s));
            }
            _ => eprintln!(
                "warning: --session requested but SMART_HISTORY_SESSION is not set; ignoring"
            ),
        }
    }
    if let Some(ec) = exit_code {
        if ec == "OK" {
            clause.push_str(" AND exit_code = 0");
        } else if ec == "ERROR" {
            clause.push_str(" AND exit_code != 0");
        }
    }
    (clause, params)
}

fn init_db() -> anyhow::Result<Connection> {
    let path = get_db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(&path)?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS history (
            id INTEGER PRIMARY KEY,
            command TEXT NOT NULL,
            directory TEXT NOT NULL,
            session_id TEXT NOT NULL,
            exit_code INTEGER,
            timestamp INTEGER DEFAULT (strftime('%s', 'now'))
        )",
        [],
    )?;
    // A unique index on (command, directory, session_id) lets the
    // `Add` arm use `INSERT ... ON CONFLICT DO UPDATE` for atomic
    // upsert. The IF NOT EXISTS makes this safe for both new and
    // existing databases (the upgrade is a no-op when the index
    // already exists).
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_history_dedup
         ON history (command, directory, session_id)",
        [],
    )?;
    Ok(conn)
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let conn = init_db()?;

    match args.command {
        Commands::Add { command, exit_code } => {
            let pwd = env::current_dir()?.to_string_lossy().into_owned();
            let session_id =
                env::var("SMART_HISTORY_SESSION").unwrap_or_else(|_| "default".to_string());

            // Atomic upsert: if (command, directory, session_id) already
            // exists, refresh its timestamp and exit_code; otherwise
            // insert a new row. The unique index idx_history_dedup is
            // the conflict target.
            conn.execute(
                "INSERT INTO history (command, directory, session_id, exit_code)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (command, directory, session_id) DO UPDATE
                 SET timestamp = (strftime('%s', 'now')),
                     exit_code = excluded.exit_code",
                params![command, pwd, session_id, exit_code],
            )?;
        }
        Commands::Search {
            query,
            directory,
            session,
            exit_code,
            fields,
            limit,
            no_highlight,
        } => {
            let selected_fields = fields.unwrap_or_else(|| vec!["command".to_string()]);
            let (raw_fields, derived) = split_fields(&selected_fields);
            let mut sql = format!("SELECT {} FROM history", raw_fields.join(", "));

            let query_ref = query.as_deref();
            let (where_clause, params) =
                build_where_clause(query_ref, directory, session, exit_code.as_deref());
            sql.push_str(&where_clause);

            append_order_and_limit(&mut sql, limit.unwrap_or(100));

            let raw_names: Vec<String> = raw_fields.clone();
            let mut stmt = conn.prepare(&sql)?;
            let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

            let rows = stmt.query_map(&params_ref[..], move |row| -> Result<Vec<(String, String)>, rusqlite::Error> {
                let row_data: Vec<(String, String)> = raw_names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| (name.clone(), cell_to_string(row, i)))
                    .collect();
                Ok(row_data)
            })?;

            let mut out_rows: Vec<Vec<String>> = Vec::new();
            for row in rows {
                let raw_row = row?;
                out_rows.push(project_row(&raw_row, &selected_fields, &derived, query_ref, no_highlight));
            }
            for out in pad_rows(&out_rows, &selected_fields) {
                println!("{}", out.join("  "));
            }
        }
        Commands::Select {
            query,
            directory,
            session,
            exit_code,
            fields,
            limit,
            no_highlight,
        } => {
            let selected_fields = fields.unwrap_or_else(|| vec!["command".to_string()]);
            let (raw_fields, derived) = split_fields(&selected_fields);
            let mut sql = format!("SELECT {} FROM history", raw_fields.join(", "));

            let query_ref = query.as_deref();
            let (where_clause, params) =
                build_where_clause(query_ref, directory, session, exit_code.as_deref());
            sql.push_str(&where_clause);

            append_order_and_limit(&mut sql, limit.unwrap_or(1000));

            let raw_names: Vec<String> = raw_fields.clone();
            let mut stmt = conn.prepare(&sql)?;
            let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

            let rows = stmt.query_map(&params_ref[..], move |row| {
                let row_data: Vec<(String, String)> = raw_names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| (name.clone(), cell_to_string(row, i)))
                    .collect();
                Ok(row_data)
            })?;

            let mut out_rows: Vec<Vec<String>> = Vec::new();
            for row in rows {
                let raw_row = row?;
                out_rows.push(project_row(&raw_row, &selected_fields, &derived, query_ref, no_highlight));
            }
            for out in pad_rows(&out_rows, &selected_fields) {
                println!("{}", out.join("  "));
            }
        }
        Commands::Clean {
            query,
            directory,
            session,
            exit_code,
            force,
        } => {
            // Build the same WHERE clause Search/Select use, then issue a
            // COUNT first and a DELETE second. The COUNT drives the
            // confirmation message; the DELETE uses the same params so
            // the matched set is identical.
            let (where_clause, params) = build_where_clause(
                query.as_deref(),
                directory,
                session,
                exit_code.as_deref(),
            );
            let count_sql = format!("SELECT COUNT(*) FROM history{}", where_clause);
            let params_ref: Vec<&dyn rusqlite::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();

            let n: i64 = {
                let mut stmt = conn.prepare(&count_sql)?;
                stmt.query_row(&params_ref[..], |row| row.get::<_, i64>(0))?
            };

            if n == 0 {
                println!("No entries match the filter; nothing to delete.");
                return Ok(());
            }

            if !force
                && !confirm(&format!(
                    "Delete {} entr{} matching the filter? [y/N] ",
                    n,
                    if n == 1 { "y" } else { "ies" }
                ))
            {
                println!("Aborted.");
                return Ok(());
            }

            let delete_sql = format!("DELETE FROM history{}", where_clause);
            let deleted = conn.execute(&delete_sql, &params_ref[..])?;
            println!("Deleted {} entr{}.", deleted, if deleted == 1 { "y" } else { "ies" });
        }
        Commands::Init { shell } => {
            if shell != "zsh" {
                anyhow::bail!(
                    "unsupported shell: {}. Only 'zsh' is currently supported.",
                    shell
                );
            }
            let session_id = generate_uuid_v4();
            let snippet = include_str!("init.zsh");
            println!("{}", snippet.replace("{session_id}", &session_id));
        }
        Commands::ImportAtuin => {
            let atuin_db =
                PathBuf::from(env::var("HOME").unwrap()).join(".local/share/atuin/history.db");
            if !atuin_db.exists() {
                anyhow::bail!("Atuin database not found at {:?}", atuin_db);
            }

            let atuin_conn = Connection::open(atuin_db)?;
            let mut stmt =
                atuin_conn.prepare("SELECT command, cwd, session, exit, timestamp FROM history")?;
            let history_iter = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i32>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?;

            let mut count = 0;
            for entry in history_iter {
                let (command, cwd, session, exit, timestamp) = entry?;
                conn.execute(
                    "INSERT INTO history (command, directory, session_id, exit_code, timestamp) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![command, cwd, session, exit, timestamp],
                )?;
                count += 1;
            }
            println!("Imported {} entries from Atuin.", count);
        }
        Commands::List { fields, table } => {
            let selected_fields = fields.unwrap_or_else(|| vec!["command".to_string()]);
            let (raw_fields, derived) = split_fields(&selected_fields);
            let sql = format!("SELECT {} FROM history ORDER BY timestamp DESC", raw_fields.join(", "));

            let mut stmt = conn.prepare(&sql)?;

            let raw_names: Vec<String> = raw_fields.clone();
            let rows = stmt.query_map([], move |row| -> Result<Vec<(String, String)>, rusqlite::Error> {
                let row_data: Vec<(String, String)> = raw_names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| (name.clone(), cell_to_string(row, i)))
                    .collect();
                Ok(row_data)
            })?;

            let mut out_rows: Vec<Vec<String>> = Vec::new();
            for row in rows {
                let raw_row = row?;
                out_rows.push(project_row(&raw_row, &selected_fields, &derived, None, false));
            }
            let out_rows = pad_rows(&out_rows, &selected_fields);
            if table {
                // Right-pad only the PADDED_FIELDS in the header so the
                // column widths match the data rows.
                let pad_widths: Vec<usize> = selected_fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let data_w = out_rows
                            .iter()
                            .map(|r| r.get(i).map(|c| c.chars().count()).unwrap_or(0))
                            .max()
                            .unwrap_or(0);
                        if PADDED_FIELDS.contains(&f.as_str()) {
                            data_w.max(f.chars().count())
                        } else {
                            f.chars().count()
                        }
                    })
                    .collect();
                let header: Vec<String> = selected_fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let pad = pad_widths[i].saturating_sub(f.chars().count());
                        format!("{}{}", " ".repeat(pad), f)
                    })
                    .collect();
                println!("{}", header.join("  |  "));
                println!("{}", "-".repeat(selected_fields.len() * 15));
                for out in &out_rows {
                    println!("{}", out.join("  |  "));
                }
            } else {
                for out in &out_rows {
                    println!("{}", out.join("  "));
                }
            }
        }
        Commands::Next { command, limit } => {
            // Find the most frequent commands that follow `command`
            // in the global history. Uses SQLite's LEAD() window
            // function to pair each row with its immediate successor
            // by timestamp, then groups by the successor and counts.
            let limit = limit.unwrap_or(5);
            let sql = "
                WITH pairs AS (
                    SELECT
                        command,
                        LEAD(command) OVER (ORDER BY timestamp ASC, id ASC) AS next_cmd
                    FROM history
                )
                SELECT next_cmd, COUNT(*) AS freq
                FROM pairs
                WHERE command = ?1 AND next_cmd IS NOT NULL
                GROUP BY next_cmd
                ORDER BY freq DESC, next_cmd ASC
                LIMIT ?2
            ";
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(params![command, limit], |row| {
                let next: String = row.get(0)?;
                let freq: i64 = row.get(1)?;
                Ok((next, freq))
            })?;
            for r in rows {
                let (next, freq) = r?;
                println!("{}\t{}", freq, next);
            }
        }
        Commands::Tui { mode, query } => {
            let initial_mode = mode.unwrap_or_else(|| "SESS".to_string());
            let initial_query = query.unwrap_or_else(|| "".to_string());
            match tui::run_tui_to_stdout(initial_mode, initial_query, conn)? {
                Some((command, pick_mode)) => {
                    // Print the chosen command. The pick_mode tells
                    // the parent what to do with it.
                    println!("{}", command);
                    std::process::exit(pick_mode);
                }
                None => std::process::exit(tui::exit_code::CANCEL),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `format_diff` uses a calendar-month ladder before falling back
    /// to smaller units. Each test pins a specific scenario so a
    /// regression in the ordering or the unit suffix is caught.
    #[test]
    fn format_diff_seconds() {
        let five_sec_ago = chrono::Utc::now() - chrono::Duration::seconds(5);
        assert_eq!(format_diff(five_sec_ago.timestamp()), "5s");
    }

    #[test]
    fn format_diff_minutes() {
        let three_min_ago = chrono::Utc::now() - chrono::Duration::minutes(3);
        assert_eq!(format_diff(three_min_ago.timestamp()), "3m");
    }

    #[test]
    fn format_diff_hours() {
        let two_hours_ago = chrono::Utc::now() - chrono::Duration::hours(2);
        assert_eq!(format_diff(two_hours_ago.timestamp()), "2h");
    }

    #[test]
    fn format_diff_days() {
        let five_days_ago = chrono::Utc::now() - chrono::Duration::days(5);
        assert_eq!(format_diff(five_days_ago.timestamp()), "5d");
    }

    #[test]
    fn format_diff_zero_or_negative_is_na() {
        // 0 and negative timestamps are treated as missing data.
        assert_eq!(format_diff(0), "N/A");
        assert_eq!(format_diff(-1), "N/A");
    }

    #[test]
    fn format_base_leaf_dir() {
        assert_eq!(format_base("/Users/har/projects/notes"), "notes");
        assert_eq!(format_base("/tmp"), "tmp");
        assert_eq!(format_base("/"), "/");
    }

    #[test]
    fn format_base_empty_string() {
        // Path::file_name of "" returns None; the fallback returns the
        // input unchanged.
        assert_eq!(format_base(""), "");
    }

    #[test]
    fn highlight_empty_needle() {
        // An empty needle should not modify the haystack.
        assert_eq!(highlight("hello world", "", "[", "]"), "hello world");
    }

    #[test]
    fn highlight_wraps_all_occurrences() {
        assert_eq!(
            highlight("foo bar foo", "foo", "[", "]"),
            "[foo] bar [foo]"
        );
    }

    #[test]
    fn highlight_no_occurrences() {
        // When the needle doesn't appear, the haystack is returned
        // unchanged.
        assert_eq!(
            highlight("hello world", "xyz", "[", "]"),
            "hello world"
        );
    }

    #[test]
    fn highlight_empty_haystack() {
        assert_eq!(highlight("", "foo", "[", "]"), "");
    }

    #[test]
    fn highlight_at_start() {
        assert_eq!(highlight("foo bar", "foo", "[", "]"), "[foo] bar");
    }

    #[test]
    fn highlight_at_end() {
        assert_eq!(highlight("bar foo", "foo", "[", "]"), "bar [foo]");
    }

    #[test]
    fn generate_uuid_v4_format() {
        // The output must be 36 characters in the canonical
        // `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` form.
        let u = generate_uuid_v4();
        assert_eq!(u.len(), 36, "UUID has unexpected length: {u}");
        assert_eq!(u.as_bytes()[8], b'-');
        assert_eq!(u.as_bytes()[13], b'-');
        assert_eq!(u.as_bytes()[18], b'-');
        assert_eq!(u.as_bytes()[23], b'-');
        // The 13th hex char (index 14) is the version nibble; for
        // v4 it must be '4'.
        assert_eq!(
            u.as_bytes()[14],
            b'4',
            "UUID version nibble is not '4' in {u}"
        );
        // The 17th hex char (index 19) is the variant nibble; for
        // RFC 4122 it must be one of 8/9/a/b.
        let variant = u.as_bytes()[19];
        assert!(
            matches!(variant, b'8' | b'9' | b'a' | b'b'),
            "UUID variant nibble is invalid in {u}: {:?}",
            variant as char
        );
    }

    #[test]
    fn generate_uuid_v4_uniqueness() {
        // Two successive calls must return different UUIDs (the
        // counter + process start instant + wall clock provides more
        // than enough entropy for this to never collide).
        let u1 = generate_uuid_v4();
        let u2 = generate_uuid_v4();
        let u3 = generate_uuid_v4();
        assert_ne!(u1, u2);
        assert_ne!(u2, u3);
        assert_ne!(u1, u3);
    }
}
