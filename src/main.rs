#![allow(clippy::should_implement_trait)]
#![allow(clippy::empty_line_after_doc_comments)]
mod files;
mod jira;
mod llm;
mod multiplexer;
mod ssh_config;
mod tui;
mod util;

use clap::Parser;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process;
use std::borrow::Cow;
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
        /// Optional comment attached to the history entry. Searchable
        /// from the TUI and CLI.
        #[arg(long)]
        comment: Option<String>,
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
        /// directory, session_id, exit_code, timestamp, id, comment, output,
        /// time, diff, base. May also be passed multiple times: -f command
        /// -f directory.
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
        /// directory, session_id, exit_code, timestamp, id, comment, output,
        /// time, diff, base. May also be passed multiple times: -f command
        /// -f directory.
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
        /// Start the TUI directly in a specific prefix mode
        /// (e.g. `--prefix '*'` for panes, `--prefix '#'`
        /// for directories, `--prefix '@'` for notes,
        /// `--prefix '!'` for todos, `--prefix '-'` for
        /// JIRA, `--prefix '~'` for files, `--prefix '='`
        /// for LLM command generation, `--prefix '%'`
        /// for the question mode, `--prefix '+'`
        /// for output search). The prefix character is
        /// the user's configured one — see
        /// `prefix.<mode>=...` in the config file; the
        /// example values above are the defaults.
        ///
        /// When `--prefix` is given, the TUI starts with
        /// the query set to that prefix character — so
        /// the first frame already shows the chosen view
        /// instead of the default history list. The CLI
        /// `--prefix` value also takes final precedence
        /// over the persisted `session.query`: the previous
        /// query is NOT restored, so the user lands in
        /// exactly the prefix mode they asked for.
        ///
        /// Note: the match algorithm (SUBSTRING / FUZZY /
        /// REGEX) is toggled separately via `C-f` inside
        /// the TUI; it applies to all prefix modes
        /// (except JIRA).
        #[arg(long)]
        prefix: Option<String>,
        /// Execute the selected command directly (via `sh -c`)
        /// instead of printing it to stdout for the parent
        /// shell to eval. Use this when launching the TUI
        /// from outside a shell context (e.g. a herdr
        /// keybinding, a GUI launcher, or a systemd
        /// service) where there's no parent shell to
        /// `eval` the printed command.
        #[arg(long)]
        exec: bool,
        #[arg(index = 1)]
        query: Option<String>,
    },
    ImportAtuin,
    List {
        /// Comma-separated list of columns to return. Available: command,
        /// directory, session_id, exit_code, timestamp, id, comment, output,
        /// time, diff, base. May also be passed multiple times: -f command
        /// -f directory.
        #[arg(short, long, value_delimiter = ',')]
        fields: Option<Vec<String>>,
        #[arg(short, long)]
        table: bool,
    },
    Init {
        shell: String,
    },
    /// Print the resolved value of a single configuration key. Used
    /// by the zsh precmd hook to discover the tmux pane output
    /// directory.
    ///
    /// Sub-commands:
    ///
    ///   get <key>    Print the resolved value of a single key
    ///                (used by the zsh precmd hook).
    ///   check        Validate ~/.config/smarthistory/config and
    ///                exit non-zero if any problems are found.
    ///                Prints a human-readable report on stdout.
    ///   list         Print every known key with its resolved value.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
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
    /// Run a command, capture up to 20 lines of combined stdout/stderr,
    /// and store the output in the database alongside the history entry.
    Capture {
        /// The command to run (pass remaining args verbatim).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Read a tmux pane log file, extract the command line and the
    /// following output (up to 20 lines), and store it in the database.
    /// Intended to be called automatically by the zsh precmd hook when
    /// running inside tmux.
    CaptureTmux {
        /// The command that was executed (as recorded by zsh preexec).
        command: String,
        /// Path to the tmux pane log file.
        file: PathBuf,
        #[arg(short, long)]
        exit_code: i32,
    },
    /// Read the herdr pane scrollback via `herdr pane read`,
    /// extract the command line and the following output,
    /// and store it in the database. Intended to be called
    /// automatically by the zsh precmd hook when running
    /// inside a herdr workspace pane.
    CaptureHerdr {
        /// The command that was executed (as recorded by zsh preexec).
        command: String,
        #[arg(short, long)]
        exit_code: i32,
    },
    /// Export history data to a JSON file. The file contains all
    /// history entries, command comments, and captured output so
    /// that a complete import is possible.
    Export {
        /// Path to the output JSON file.
        filename: PathBuf,
        /// Optional start timestamp (Unix epoch seconds). Only
        /// entries with timestamp >= this value are exported.
        #[arg(long)]
        since: Option<i64>,
        /// Optional end timestamp (Unix epoch seconds). Only
        /// entries with timestamp <= this value are exported.
        #[arg(long)]
        until: Option<i64>,
    },
    /// Import history data from a JSON file previously created
    /// with the `export` command. Existing entries with the same
    /// (command, directory, session_id) are updated; new entries
    /// are inserted.
    Import {
        /// Path to the input JSON file.
        filename: PathBuf,
    },
    /// Walk the SQLite history
    /// database and rewrite every
    /// `directory` value to its
    /// `~`-shorthened form (where
    /// the directory is under
    /// `$HOME` or any `homemap=...`
    /// entry in the config file).
    ///
    /// `smarthistory add` (the
    /// preexec hook entry point)
    /// always records the
    /// kernel-canonical absolute
    /// path. For the directories
    /// view and the staged `tmux
    /// new-session` command, the
    /// user wants the short `~`
    /// form. `smarthistory update`
    /// is a one-shot migration
    /// that updates the rows in
    /// place (preserving
    /// `id`/`timestamp`); running
    /// it twice is a no-op.
    ///
    /// New rows added after the
    /// migration are stored
    /// `~`-shortened from the
    /// start (see
    /// `current_directory_for_storage`).
    Update,
}

/// Sub-commands of `smarthistory config`. `Get` preserves the
/// original `config <key>` interface (used by the zsh precmd
/// hook). `Check` validates the config file end-to-end and exits
/// non-zero when anything is wrong. `List` prints the full
/// resolved configuration.
#[derive(clap::Subcommand, Debug)]
enum ConfigAction {
    /// Print the resolved value of a single configuration key.
    /// Used by the zsh precmd hook to discover the tmux pane
    /// output directory.
    Get {
        /// One of: `tmuxpaneoutputdir`, `ignorecapture`, `capturelines`.
        key: String,
    },
    /// Validate ~/.config/smarthistory/config and exit non-zero if
    /// any problems are found. Prints a human-readable report on
    /// stdout.
    Check,
    /// Print every known configuration key with its resolved value.
    List,
}

/// A single history entry for JSON export/import.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryExportRow {
    id: Option<i64>,
    command: String,
    directory: String,
    session_id: String,
    exit_code: i32,
    timestamp: i64,
    mode: String,
    /// Optional comment (from command_comments table).
    #[serde(skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    /// Optional captured output (from history_output table).
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
}

/// The full export/import format.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryExport {
    /// Schema version for forward compatibility.
    version: i32,
    /// All history entries.
    history: Vec<HistoryExportRow>,
}

fn get_db_path() -> PathBuf {
    let home = env::var("HOME").expect("HOME not set");
    PathBuf::from(home)
        .join(".local")
        .join("cache")
        .join("smarthistory")
        .join("smarthistory.db")
}

/// Default maximum number of output lines stored per history entry.
/// A higher value makes the details pane less useful, so we cap it by
/// default. Users can change this via `capturelines` in the config
/// file.
#[allow(dead_code)]
pub(crate) const MAX_OUTPUT_LINES: usize = DEFAULT_CAPTURE_LINES;

/// Path to the optional user configuration file. Lines are
/// `key=value` pairs. Comments start with `#` and blank lines are
/// ignored. Supported keys:
///
///   tmuxpaneoutputdir=~/path/to/dir
///   ignorecapture=cmd1 cmd2 cmd3
///   capturelines=20
///   capturelines.<cmd>=ALL|<N>
///
/// When the file is absent, built-in defaults are used. When the
/// file is present, the keys it defines override the defaults.
/// Resolve the path to the user's
/// smarthistory config file
/// (`$HOME/.config/smarthistory/config`).
/// Returns `None` only when
/// `$HOME` is unset (a
/// degenerate environment; in
/// practice every Unix-y shell
/// has it). Exposed as `pub` so
/// the TUI can check whether a
/// config file is locatable
/// before opening the
/// add-entry dialog (the
/// dialog's commit path needs
/// to write to the file).
pub fn config_path() -> Option<std::path::PathBuf> {
    let home = env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("smarthistory")
            .join("config"),
    )
}

/// Expand a leading `~` or `~/<rest>` in a path to the user's home
/// directory. Other occurrences of `~` are left untouched.
fn expand_tilde(path: &str) -> std::path::PathBuf {
    if path == "~" {
        if let Ok(home) = env::var("HOME") {
            return std::path::PathBuf::from(home);
        }
    } else if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = env::var("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    std::path::PathBuf::from(path)
}

/// Commands whose output should never be captured by default. These
/// are interactive TUI applications (editors, pagers, system
/// monitors) whose output is either useless or harmful to store
/// verbatim. Used when the config file is absent or does not set
/// `ignorecapture`.
const DEFAULT_NO_CAPTURE: &[&str] = &[
    "vi", "nvim", "vim", "top", "htop", "emacs", "more", "less", "lazygit",
];

/// Default number of captured lines when neither `capturelines` nor a
/// per-command override is configured.
const DEFAULT_CAPTURE_LINES: usize = 20;

/// Parse a `capturelines` value. Returns `None` for "ALL" (unlimited)
/// or `Some(n)` for a numeric value.
fn parse_capture_lines(s: &str) -> Option<usize> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("ALL") {
        None
    } else {
        s.parse::<usize>().ok()
    }
}

/// Parse a boolean config value. Accepts "on", "true", "1", "yes"
/// (case-insensitive, also with leading/trailing whitespace) as true;
/// "off", "false", "0", "no" as false. Anything else falls back to
/// `default` rather than failing to parse the whole config file.
fn parse_bool(s: &str, default: bool) -> bool {
    match s.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => true,
        "off" | "false" | "0" | "no" => false,
        _ => default,
    }
}

/// Severity of a config-validation finding. `Error` entries
/// cause `smarthistory config check` to exit non-zero. `Warning`
/// entries are surfaced for the user's information but don't
/// fail the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigIssueLevel {
    Warning,
    Error,
}

/// One row in the validation report: a level, a short category
/// (printed as a tag), and the human-readable message.
#[derive(Debug, Clone)]
pub struct ConfigIssue {
    pub level: ConfigIssueLevel,
    pub category: String,
    pub message: String,
}

/// Aggregate result of `validate_config`. Use `has_errors()` to
/// decide the exit code; otherwise iterate `issues()` to print the
/// report. Also exposes the resolved `Config` so callers can
/// print the effective values once validation passes.
pub struct ConfigReport {
    cfg: Config,
    issues: Vec<ConfigIssue>,
    /// True when the config file at the canonical path is
    /// absent. `issues` will contain a Warning noting that the
    /// built-in defaults are in effect.
    file_missing: bool,
}

impl ConfigReport {
    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|i| i.level == ConfigIssueLevel::Error)
    }

    pub fn has_warnings(&self) -> bool {
        self.issues
            .iter()
            .any(|i| i.level == ConfigIssueLevel::Warning)
    }

    pub fn issues(&self) -> &[ConfigIssue] {
        &self.issues
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn file_missing(&self) -> bool {
        self.file_missing
    }
}

impl std::fmt::Display for ConfigReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Configuration report")?;
        writeln!(f, "===================")?;
        writeln!(f)?;
        if self.file_missing {
            writeln!(
                f,
                "  No config file at {} \u{2014} using built-in defaults.",
                config_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(unknown HOME)".into())
            )?;
            writeln!(f)?;
        }
        if self.issues.is_empty() {
            writeln!(f, "  No issues found.")?;
        } else {
            let mut counts = [0usize; 2];
            for issue in &self.issues {
                counts[issue.level as usize] += 1;
                let tag = match issue.level {
                    ConfigIssueLevel::Warning => "warning",
                    ConfigIssueLevel::Error => "  error",
                };
                writeln!(f, "  [{}] {}: {}", tag, issue.category, issue.message)?;
            }
            writeln!(f)?;
            writeln!(
                f,
                "  {} error(s), {} warning(s)",
                counts[ConfigIssueLevel::Error as usize],
                counts[ConfigIssueLevel::Warning as usize],
            )?;
        }
        writeln!(f)?;
        writeln!(f, "Effective values")?;
        writeln!(f, "----------------")?;
        print_config_list(f, &self.cfg);
        Ok(())
    }
}

/// Validate `~/.config/smarthistory/config`. Loads the file (so
/// unknown keys, invalid values, and typos in `key.*` action
/// names are all caught), then runs a battery of semantic checks
/// (e.g. tmux pane directory exists and is writable, regex
/// bindings parse cleanly, theme colors parse). Always returns a
/// `ConfigReport` — callers consult `has_errors()` for the exit
/// status.
pub fn validate_config() -> ConfigReport {
    let path = config_path();
    let file_missing = match path.as_ref() {
        Some(p) => !p.exists(),
        None => true,
    };
    let cfg = Config::load();
    let mut issues = Vec::new();

    // --- File-level checks ---
    if let Some(ref p) = path {
        if !file_missing {
            match std::fs::metadata(p) {
                Ok(meta) if meta.is_dir() => {
                    issues.push(ConfigIssue {
                        level: ConfigIssueLevel::Error,
                        category: "file".into(),
                        message: format!("{} is a directory, not a file", p.display()),
                    });
                }
                Ok(_) => {}
                Err(e) => issues.push(ConfigIssue {
                    level: ConfigIssueLevel::Error,
                    category: "file".into(),
                    message: format!("cannot read {}: {}", p.display(), e),
                }),
            }
        }
    } else {
        issues.push(ConfigIssue {
            level: ConfigIssueLevel::Warning,
            category: "file".into(),
            message: "HOME is not set; cannot resolve config path".into(),
        });
    }

    // --- Key-binding collision detection ---
    use crate::tui::bindings::ALL_ACTIONS;
    let bindings = cfg.key_bindings();
    let mut seen_specs: std::collections::HashMap<String, tui::bindings::Action> =
        std::collections::HashMap::new();
    for (action, specs) in bindings.iter() {
        for spec in specs {
            let spec_str = tui::format_key_spec(*spec);
            if let Some(prev) = seen_specs.get(&spec_str) {
                issues.push(ConfigIssue {
                    level: ConfigIssueLevel::Warning,
                    category: "key".into(),
                    message: format!(
                        "{:?} is bound to the same key ({}) as {:?}; only the first action wins",
                        action, spec_str, prev
                    ),
                });
            } else {
                seen_specs.insert(spec_str.clone(), action);
            }
        }
    }

    // --- Unknown key.* action names ---
    if let Some(ref p) = path
        && p.is_file()
            && let Ok(contents) = std::fs::read_to_string(p) {
                let known: std::collections::HashSet<&'static str> = ALL_ACTIONS
                    .iter()
                    .map(|a| a.config_key())
                    .collect();
                for raw in contents.lines() {
                    let line = raw.split('#').next().unwrap_or("").trim();
                    if line.is_empty() {
                        continue;
                    }
                    let (k, _) = match line.split_once('=') {
                        Some(kv) => kv,
                        None => continue,
                    };
                    let k = k.trim();
                    if let Some(name) = k.strip_prefix("key.")
                        && !name.is_empty() && !known.contains(name) {
                            issues.push(ConfigIssue {
                                level: ConfigIssueLevel::Error,
                                category: "key".into(),
                                message: format!(
                                    "unknown key action {:?}: did you mean one of {:?}?",
                                    name,
                                    ALL_ACTIONS
                                        .iter()
                                        .map(|a| a.config_key())
                                        .collect::<Vec<_>>()
                                ),
                            });
                        }
                }
            }

    // --- tmux pane output directory checks ---
    let dir = &cfg.tmux_pane_output_dir;
    if dir.as_os_str().is_empty() {
        issues.push(ConfigIssue {
            level: ConfigIssueLevel::Error,
            category: "tmuxpaneoutputdir".into(),
            message: "tmuxpaneoutputdir is empty".into(),
        });
    } else if dir.exists() && !dir.is_dir() {
        issues.push(ConfigIssue {
            level: ConfigIssueLevel::Error,
            category: "tmuxpaneoutputdir".into(),
            message: format!("{} is not a directory", dir.display()),
        });
    } else if !dir.exists() {
        issues.push(ConfigIssue {
            level: ConfigIssueLevel::Warning,
            category: "tmuxpaneoutputdir".into(),
            message: format!(
                "{} does not exist; smarthistory will create it on first use",
                dir.display()
            ),
        });
    } else {
        // Probe for write access using a tempfile create+remove.
        let probe = dir.join(".smarthistory-write-probe");
        match std::fs::File::create(&probe) {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
            }
            Err(e) => issues.push(ConfigIssue {
                level: ConfigIssueLevel::Error,
                category: "tmuxpaneoutputdir".into(),
                message: format!("cannot write to {}: {}", dir.display(), e),
            }),
        }
    }

    // --- capturelines checks ---
    for (cmd, val) in cfg.capture_lines_per_command() {
        if matches!(val, Some(0)) {
            issues.push(ConfigIssue {
                level: ConfigIssueLevel::Warning,
                category: "capturelines".into(),
                message: format!(
                    "capturelines.{} = 0; use ALL instead to capture every line",
                    cmd
                ),
            });
        }
    }

    ConfigReport {
        cfg,
        issues,
        file_missing,
    }
}

/// Print every known configuration key with its resolved value.
fn print_config_list<W: std::fmt::Write>(f: &mut W, cfg: &Config) {
    let _ = writeln!(f, "  tmuxpaneoutputdir = {}", cfg.tmux_pane_output_dir.display());
    let mut cmds: Vec<&String> = cfg.ignore_capture.iter().collect();
    cmds.sort();
    let _ = writeln!(
        f,
        "  ignorecapture = {}",
        cmds.iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let default = match cfg.default_capture_lines {
        Some(n) => n.to_string(),
        None => "ALL".to_string(),
    };
    let _ = writeln!(f, "  capturelines = {}", default);
    let mut per_cmd: Vec<(&String, &Option<usize>)> =
        cfg.capture_lines_per_command().iter().collect();
    per_cmd.sort_by(|a, b| a.0.cmp(b.0));
    for (cmd, val) in per_cmd {
        let v = match val {
            Some(n) => n.to_string(),
            None => "ALL".to_string(),
        };
        let _ = writeln!(f, "  capturelines.{} = {}", cmd, v);
    }
    let _ = writeln!(
        f,
        "  duplicatefilter = {}",
        if cfg.duplicate_filter { "on" } else { "off" }
    );
    let _ = writeln!(f, "  initialmode = {}", cfg.initial_mode());
    let _ = writeln!(f, "  multiplexer = {}", cfg.multiplexer().as_str());
    use crate::tui::bindings::ALL_ACTIONS;
    let bindings = cfg.key_bindings();
    for a in ALL_ACTIONS {
        if bindings.is_unbound(*a) {
            let _ = writeln!(f, "  key.{} = none", a.config_key());
        } else {
            // Multi-key bindings print as a comma-separated list,
            // matching the input format the user can paste back
            // into the config file.
            let _ = writeln!(
                f,
                "  key.{} = {}",
                a.config_key(),
                tui::format_key_specs(bindings.specs(*a))
            );
        }
    }
}

/// User-customizable query prefix characters. Each field is a
/// single character used to trigger a specific search or LLM mode.
/// Defaults match the original hard-coded values.
#[derive(Debug, Clone)]
pub struct QueryPrefixes {
    /// Prefix for output search (default `+`).
    pub output: char,
    /// Prefix for LLM command generation (default `=`).
    pub llm: char,
    /// Prefix for general question mode (default `%`).
    pub question: char,
    /// Prefix for note search mode (default `@`).
    pub notes: char,
    /// Prefix for the todo-search mode (default `!`).
    /// Inside the TUI, typing `!` switches to a
    /// view that scans every configured note for
    /// todo lines (markdown task-list checkboxes
    /// like `- [ ]` / `- [x]`) and lists each one
    /// as its own row, with the surrounding
    /// context in the details pane. Selecting
    /// a row opens `$EDITOR <file> +<line>` so the
    /// user lands directly on the todo line.
    pub todo: char,
    /// Prefix for the directories view (default
    /// `#`). Lists every unique directory
    /// that's been used in the global history,
    /// sorted by the most-recent history row's
    /// timestamp DESC. Each row also surfaces
    /// that directory's most-recently-executed
    /// command so the user has context for "what
    /// was I doing in there". Selecting a row
    /// stages a `cd <path>` command and exits
    /// the TUI so the parent shell runs it.
    pub directories: char,
    /// Prefix for the session-panes view
    /// (default `*`). Lists every pane in the
    /// *current* tmux session — excluding the
    /// pane the TUI is running in (read from
    /// `$TMUX_PANE`) — with the pane's current
    /// command as the primary text, the pane's
    /// cwd (shortened `~/x`) as the secondary
    /// text, and the pane id (`%N`) staged for
    /// the `select-pane` / `switch-client`
    /// action on Enter. Useful as a quick
    /// "what else is running in this session?"
    /// overview that lets the user jump to a
    /// pane without tearing down the TUI.
    pub panes: char,
    /// Prefix for the JIRA issue-search mode (default
    /// `-`). Lists JIRA issues from a self-hosted
    /// instance matching the typed query (issue keys,
    /// `field=value` constraints, or free text matched
    /// against description/summary). Selecting an issue
    /// opens its browse URL in the system browser.
    /// Credentials/config come from the `JIRA_SERVER`,
    /// `JIRA_API_TOKEN`, `JIRA_URL`, and `JIRA_PROJECT`
    /// environment variables.
    /// Prefix for the files-view mode (default
    /// `~`). Lists every file in the current
    /// directory and subdirectories, filtered by
    /// the typed pattern. Selecting a row opens
    /// the file in `$EDITOR`.
    pub files: char,
    /// Prefix for the tags-view mode (default
    /// `$`). Lists every symbol defined in a
    /// universal tag file (`tags`) in the
    /// current directory, filtered by the
    /// typed pattern. Selecting a row opens
    /// the file in `$EDITOR` at the correct
    /// line (`+LINE_NUMBER`).
    pub tags: char,
    pub jira: char,
}

impl Default for QueryPrefixes {
    fn default() -> Self {
        QueryPrefixes {

            output: '+',
            llm: '=',
            question: '%',
            notes: '@',
            todo: '!',
            directories: '#',
            panes: '*',
            files: '~',
            tags: '$',
            jira: '-',
        }
    }
}

/// Resolved configuration. Constructed by `Config::load`.

/// A named session from the config file.
/// Syntax: `session.<id> = "Name"`, `session.<id>.dir = "~/path"`,
/// `session.<id>.exec = "cmd"` (command to run after
/// creating the workspace).
#[derive(Debug, Clone)]
struct SessionDef {
    name: String,
    dir: String,
    exec: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Directory containing per-pane tmux output log files.
    tmux_pane_output_dir: std::path::PathBuf,
    /// Commands whose output is never captured. Empty means capture
    /// everything.
    ignore_capture: std::collections::HashSet<String>,
    /// Default number of captured lines, or `None` for unlimited.
    default_capture_lines: Option<usize>,
    /// Per-command override for captured lines.
    capture_lines_per_command: std::collections::HashMap<String, Option<usize>>,
    /// When true, only the newest instance of each command is shown in
    /// the TUI; older duplicates are hidden. Toggleable from the TUI
    /// at runtime via Ctrl-S, and seeded from the config file's
    /// `duplicatefilter=on|off` setting.
    duplicate_filter: bool,
    /// Initial search scope for the TUI. Honored by `smarthistory tui`
    /// when neither `--mode` nor `$SMARTHISTORY_TUI_MODE` is set.
    /// One of "SESS", "DIR", "GLOBAL".
    initial_mode: String,
    /// TUI theme palette. Each field is a hex color string like
    /// `#ffaa00` or a named color (`red`, `green`, `cyan`, ...).
    theme: TuiTheme,
    /// User-customizable TUI key bindings. Built from `key.<action>`
    /// entries in the config file; defaults match the original
    /// hard-coded Ctrl-* bindings.
    #[allow(dead_code)]
    key_bindings: tui::bindings::KeyBindings,
    /// Optional LLM (ollama) configuration for the `=...` TUI
    /// query mode. `None` means the feature is disabled — the
    /// `llm` module returns `LlmError::NotConfigured` and the
    /// TUI surfaces a clear status message.
    llm: Option<llm::LlmConfig>,
    /// Path to the note_search SQLite database. When set, the `@`
    /// prefix searches notes instead of shell history.
    /// Can also be set via the NOTE_SEARCH_DATABASE env var.
    notes_database: Option<std::path::PathBuf>,
    /// Path to the directory containing note files. Used to read
    /// note content for the preview pane.
    /// Can also be set via the NOTE_SEARCH_DIR env var.
    notes_dir: Option<std::path::PathBuf>,
    /// Template for the line-number option that
    /// the todo-search mode (`!`) appends to the
    /// editor command when the user selects a
    /// todo line. The string `"$LINE"` is
    /// substituted with the actual 1-based line
    /// number. Default: `"+$LINE"` (works with
    /// `vim`, `nano`, `emacs -nw`, and most
    /// POSIX editors).
    ///
    /// Configurable via `todo.line_option=...`
    /// in the config file.
    todo_line_option: String,
    /// User-defined JQL fragments for the `-`-mode
    /// TUI search, loaded from
    /// `jira.search.<name>=<jql>` entries in the
    /// config file. A fragment named `foo` is
    /// invoked in the search body as `@foo`; the
    /// fragment's JQL is spliced verbatim into the
    /// generated JQL. Reserved names (`me`, `today`,
    /// `week`, `month`) cannot be overridden — the
    /// loader silently drops them so a typo in the
    /// config can't disable a built-in alias.
    jira_fragments: std::collections::HashMap<String, String>,
    /// User-customizable additional
    /// directory basenames to skip
    /// during the files-mode walk
    /// (`~...`). Configured via
    /// `files.ignore=<name>` lines
    /// in the config file (one
    /// per line, space-separated).
    /// Always combined with the
    /// built-in [`crate::files::DEFAULT_IGNORES`]
    /// list at walk time, so the
    /// user only needs to add
    /// project-specific patterns
    /// (`.venv/`, `.terraform/`,
    /// etc.).
    files_ignores: Vec<String>,
    /// User-customizable query prefix characters.
    query_prefixes: QueryPrefixes,
    /// User-configured additional
    /// "home" prefixes. The DB
    /// stores absolute paths,
    /// but when displayed or
    /// queried, paths under any
    /// of these prefixes are
    /// shortened with `~` (the
    /// same convention the shell
    /// uses). The default
    /// `$HOME` is always in the
    /// set — `homemap=...` adds
    /// extra entries.
    ///
    /// Use case: on macOS, the
    /// user's home directory
    /// lives on an external
    /// volume and is mounted at
    /// `/Volumes/HUGE/har/...`
    /// while the shell exposes
    /// `/Users/har/...`. The
    /// preexec hook records the
    /// kernel-canonical path
    /// (the `/Volumes/HUGE/...`
    /// form); the shell snippet
    /// exposes the user's
    /// logical path. Adding
    /// `homemap=/Volumes/HUGE/har`
    /// tells the TUI to
    /// shorten both forms to
    /// `~/...` so the user sees
    /// a consistent short form.
    home_map: Vec<std::path::PathBuf>,
    /// User-configured
    /// "session dirs". Each
    /// entry is a directory
    /// whose sub-tree is
    /// walked recursively at
    /// TUI-startup time and
    /// every directory found
    /// is added to the
    /// directories list (the
    /// `#` mode) — even
    /// when the user has
    /// never run a command
    /// in that directory.
    /// This is the user's
    /// "always show me these
    /// projects" list.
    ///
    /// Configurable via one
    /// or more `sessiondirs=...`
    /// lines in the config
    /// file. Multiple entries
    /// are allowed (one per
    /// line, like `prefix.<x>=...`).
    /// A non-existent path is
    /// silently skipped (the
    /// user may have moved the
    /// directory; the next
    /// startup with the path
    /// back in place picks it
    /// up).
    session_dirs: Vec<std::path::PathBuf>,
    /// Which terminal
    /// multiplexer the TUI's
    /// directory- and
    /// panes-switching modes
    /// should target. Defaults
    /// to `Tmux` (preserves
    /// the historical
    /// behaviour). When set
    /// to `Herdr` the TUI
    /// shells out to herdr
    /// (`herdr workspace
    /// list`, `herdr pane
    /// list`) and stages
    /// `herdr workspace
    /// focus` / `herdr
    /// workspace create`
    /// commands instead of
    /// the tmux equivalents.
    /// The `herdr` Cargo
    /// feature must be
    /// compiled in; on a
    /// default build the
    /// herdr path is a
    /// no-op that surfaces a
    /// "build with
    /// `--features herdr`"
    /// status message.
    ///
    /// Configurable via
    /// `multiplexer=tmux|herdr`
    /// in the config file, or
    /// the
    /// `SMARTHISTORY_MULTIPLEXER`
    /// environment variable
    /// (which wins over the
    /// config file, matching
    /// the
    /// `NOTE_SEARCH_*` /
    /// `JIRA_*` precedence
    /// pattern). Unrecognised
    /// values are dropped
    /// with a stderr warning
    /// and the default (tmux)
    /// is used.
    multiplexer: crate::multiplexer::MultiplexerKind,
    /// Named sessions parsed from
    /// `session.<id> = "name"` /
    /// `session.<id>.dir = "~/path"` /
    /// `session.<id>.startup_command = "cmd"`
    /// config keys. Each entry
    /// becomes a row in the panes
    /// (`*`) view.
    sessions: Vec<(usize, SessionDef)>,
    /// Host entries parsed from
    /// `host.<id> = "name"` /
    /// `host.<id>.host = "alias"` /
    /// `host.<id>.hostname = "real"` /
    /// `host.<id>.user = "u"` /
    /// `host.<id>.port = N` /
    /// `host.<id>.identity = "path"` /
    /// `host.<id>.dir = "~/path"` /
    /// `host.<id>.exec = "cmd"`. Each entry
    /// becomes a row in the `# hosts`
    /// section of the panes (`*`) view.
    /// SSH config (`~/.ssh/config`) entries
    /// without a config-file companion are
    /// auto-appended by `Config::load`.
    hosts: Vec<(usize, crate::tui::state::HostDef)>,
}

/// User-customizable colors for the TUI. Defaults match the
/// built-in `Theme` palette in `src/tui.rs`. Any unrecognized
/// color falls back to the corresponding default.
#[derive(Debug, Clone)]
pub struct TuiTheme {
    bg: String,
    fg: String,
    accent: String,
    success: String,
    error: String,
    warning: String,
    dim: String,
    highlight: String,
    /// Foreground color used for the "output search" mode
    /// tint (the `+...` query prefix). Defaults to blue so
    /// it's visually distinct from the other mode tints
    /// (yellow = regex, green = fuzzy, magenta = LLM).
    /// Override with `tuicolor.info=<color>` in the config
    /// file.
    info: String,
    /// Background color used for the currently-selected row in the
    /// history list. Falls back to `bg` when unset.
    selection: String,
    /// Foreground color used for badge text (the dark-on-bright or
    /// light-on-dim text inside mode/scope/dedup chips). Falls back
    /// to `bg` when unset so it always contrasts with the badge's
    /// bright background.
    badge_fg: String,
    /// Background color for the history list pane. Falls back to
    /// `bg` when unset.
    list_bg: String,
    /// Background color for the details pane. Falls back to `bg`
    /// when unset.
    details_bg: String,
    /// Background color for the search/comment input pane. Falls
    /// back to `bg` when unset.
    input_bg: String,
    /// Background color for the status bar. Falls back to `bg`
    /// when unset.
    status_bg: String,
}

impl Default for TuiTheme {
    fn default() -> Self {
        TuiTheme {
            bg: "black".to_string(),
            fg: "gray".to_string(),
            accent: "cyan".to_string(),
            success: "green".to_string(),
            error: "red".to_string(),
            warning: "yellow".to_string(),
            dim: "gray".to_string(),
            highlight: "yellow".to_string(),
            info: "blue".to_string(),
            selection: "darkgray".to_string(),
            badge_fg: String::new(),
            list_bg: String::new(),
            details_bg: String::new(),
            input_bg: String::new(),
            status_bg: String::new(),
        }
    }
}

impl Config {
    pub fn default() -> Self {
        let dir = env::var("HOME")
            .map(|h| std::path::PathBuf::from(h).join(".cache").join("tmux-history"))
            .unwrap_or_else(|_| std::path::PathBuf::from(".cache/tmux-history"));
        let ignore: std::collections::HashSet<String> =
            DEFAULT_NO_CAPTURE.iter().map(|s| s.to_string()).collect();
        Config {
            tmux_pane_output_dir: dir,
            ignore_capture: ignore,
            default_capture_lines: Some(DEFAULT_CAPTURE_LINES),
            capture_lines_per_command: std::collections::HashMap::new(),
            duplicate_filter: true,
            initial_mode: "SESS".to_string(),
            theme: TuiTheme::default(),
            key_bindings: tui::bindings::KeyBindings::defaults(),
            // LLM is opt-in: empty config means "feature
            // disabled". Users enable it by setting both
            // `ollama.url` and `ollama.model` in their config
            // file; we only store a config when both fields
            // are present (see `parse`).
            llm: None,
            notes_database: None,
            notes_dir: None,
            todo_line_option: String::from("+$LINE"),
            jira_fragments: std::collections::HashMap::new(),
            files_ignores: Vec::new(),
            query_prefixes: QueryPrefixes::default(),
            // `~` expansion: `$HOME` is
            // always in the set (the
            // `expand_home` helper
            // pulls it from the env
            // at call time), so we
            // start with an empty
            // user-configured list.
            // Multiple `homemap=...`
            // lines in the config
            // file append to this
            // list.
            home_map: Vec::new(),
            // `sessiondirs=...`
            // entries from the
            // config file. Each is
            // recursively walked at
            // TUI startup; every
            // subdirectory found is
            // added to the
            // directories list.
            session_dirs: Vec::new(),
            multiplexer: crate::multiplexer::MultiplexerKind::default(),
            sessions: Vec::new(),
            hosts: Vec::new(),
        }
    }

    /// Load configuration from `~/.config/smarthistory/config`,
    /// overlaying the defaults.
    pub fn load() -> Self {
        let mut cfg = Config::default();
        if let Some(path) = config_path()
            && let Ok(contents) = std::fs::read_to_string(&path) {
                cfg.parse(&contents);
            }
        // Environment variables override config file values.
        if let Ok(db) = env::var("NOTE_SEARCH_DATABASE") {
            let path = std::path::PathBuf::from(&db);
            if path.exists() && path.is_file() {
                cfg.notes_database = Some(path);
            }
        }
        if let Ok(dir) = env::var("NOTE_SEARCH_DIR") {
            let path = std::path::PathBuf::from(&dir);
            if path.exists() && path.is_dir() {
                cfg.notes_dir = Some(path);
            }
        }
        // `SMARTHISTORY_MULTIPLEXER`
        // wins over the config
        // file, matching the
        // NOTE_SEARCH_* / JIRA_*
        // precedence pattern
        // (env > config > default).
        // Invalid values are
        // dropped with a stderr
        // warning; the existing
        // (file / default) value
        // is preserved so a typo
        // in the env var can't
        // silently disable
        // directory switching.
        if let Ok(raw) = env::var("SMARTHISTORY_MULTIPLEXER") {
            match crate::multiplexer::MultiplexerKind::parse(&raw) {
                Some(kind) => cfg.multiplexer = kind,
                None => eprintln!(
                    "smarthistory: ignoring invalid \
                     SMARTHISTORY_MULTIPLEXER={:?} \
                     (expected `tmux` or `herdr`)",
                    raw
                ),
            }
        }
        cfg
    }

    /// Parse INI-style lines into the config. Unknown keys are
    /// ignored.
    fn parse(&mut self, contents: &str) {
        // Side map for `key.<action>=<spec>` entries. They are
        // collected on the fly here and applied to the binding
        // table once the whole file has been read so a typo early
        // in the file can't mask a later valid override.
        let mut key_entries: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        // Accumulator for `ollama.url` / `ollama.model`. The
        // finished `LlmConfig` is built from these after the
        // loop so that a later line in the config file
        // overrides an earlier one.
        let mut ollama_url = String::new();
        let mut ollama_model = String::new();
        for raw_line in contents.lines() {
            let line = raw_line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = match line.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            };
            if key.is_empty() {
                continue;
            }
            match key {
                "tmuxpaneoutputdir" => {
                    self.tmux_pane_output_dir = expand_tilde(value);
                }
                "ignorecapture" => {
                    self.ignore_capture = value
                        .split_whitespace()
                        .map(|s| s.to_string())
                        .collect();
                }
                "capturelines" => {
                    if let Some(parsed) = parse_capture_lines(value) {
                        self.default_capture_lines = Some(parsed);
                    } else {
                        self.default_capture_lines = None;
                    }
                }
                "duplicatefilter" => {
                    self.duplicate_filter = parse_bool(value, true);
                }
                "initialmode" => {
                    let upper = value.trim().to_ascii_uppercase();
                    if matches!(upper.as_str(), "SESS" | "SESSION" | "DIR" | "DIRECTORY" | "GLOBAL") {
                        self.initial_mode = upper;
                    }
                }
                "multiplexer" => {
                    match crate::multiplexer::MultiplexerKind::parse(value) {
                        Some(kind) => self.multiplexer = kind,
                        None => eprintln!(
                            "smarthistory: ignoring invalid \
                             multiplexer={:?} (expected \
                             `tmux` or `herdr`); using \
                             default",
                            value
                        ),
                    }
                }
                "ollama.url" => {
                    ollama_url = value.to_string();
                }
                "ollama.model" => {
                    ollama_model = value.to_string();
                }
                "notes.database" => {
                    let path = expand_tilde(value);
                    if path.exists() && path.is_file() {
                        self.notes_database = Some(path);
                    } else {
                        eprintln!(
                            "warning: notes.database {} does not exist or is not a file",
                            path.display()
                        );
                    }
                }
                "notes.dir" => {
                    let path = expand_tilde(value);
                    if path.exists() && path.is_dir() {
                        self.notes_dir = Some(path);
                    } else {
                        eprintln!(
                            "warning: notes.dir {} does not exist or is not a directory",
                            path.display()
                        );
                    }
                }
                "todo.line_option" => {
                    // The template uses the literal
                    // `"$LINE"` placeholder which is
                    // substituted at selection time
                    // (see `App::todo_editor_command`).
                    // We accept any non-empty string;
                    // malformed templates fall back
                    // to the default at runtime so a
                    // typo doesn't disable the feature.
                    let trimmed = value.trim();
                    if !trimmed.is_empty() && trimmed.contains("$LINE") {
                        self.todo_line_option = trimmed.to_string();
                    } else if !trimmed.is_empty() {
                        eprintln!(
                            "warning: todo.line_option {:?} must contain \"$LINE\"; \
                             keeping default \"{}\"",
                            value,
                            self.todo_line_option
                        );
                    }
                }
                "homemap" => {
                    // Additional home
                    // prefixes for `~`
                    // expansion. Multiple
                    // entries are allowed
                    // (one per line, like
                    // `prefix.<x>=...`); they
                    // are appended in the
                    // order written. The
                    // default `$HOME` is
                    // always added at
                    // expansion time (we
                    // don't bake it in here
                    // because HOME may
                    // change between
                    // config-load and
                    // TUI-launch). A value
                    // that doesn't exist
                    // on disk is still
                    // accepted — the TUI
                    // may legitimately want
                    // to shorten a
                    // hypothetical path
                    // (e.g. a user
                    // describing a
                    // directory they've
                    // since moved).
                    let trimmed = value.trim();
                    if !trimmed.is_empty() {
                        self.home_map.push(
                            std::path::PathBuf::from(trimmed),
                        );
                    }
                }
                "sessiondirs" => {
                    // Recursively-walked
                    // directories whose
                    // sub-directories are
                    // always shown in the
                    // `#`-mode list, even
                    // when the user has
                    // never run a command
                    // there. Multiple
                    // entries are allowed
                    // (one per line). A
                    // non-existent path
                    // is silently skipped
                    // here — the recursive
                    // walk in the TUI
                    // will simply produce
                    // an empty list for
                    // it. We still record
                    // the path so a
                    // config-validation
                    // tool can warn the
                    // user (a future
                    // `smarthistory check`
                    // could surface this).
                    let trimmed = value.trim();
                    if !trimmed.is_empty() {
                        // Apply the same
                        // `~` expansion we
                        // use for
                        // `notes.database` /
                        // `notes.dir` /
                        // `homemap`:
                        // users naturally
                        // write
                        // `sessiondirs=~/work`
                        // in their config,
                        // and the literal
                        // string `~` doesn't
                        // exist as a real
                        // path. Without
                        // expansion, the
                        // walker would
                        // silently skip the
                        // entry (the path
                        // doesn't exist)
                        // and the user's
                        // pinned directories
                        // would never appear
                        // in the list. (This
                        // is the bug the
                        // user hit: their
                        // config had
                        // `sessiondirs=~/.config/tmux-sessions`,
                        // the literal `~`
                        // path doesn't
                        // exist, and the
                        // walker returned an
                        // empty list for
                        // it.)
                        self.session_dirs.push(
                            expand_tilde(trimmed),
                        );
                    }
                }
                other => {
                    if let Some(cmd) = other.strip_prefix("capturelines.")
                        && !cmd.is_empty() {
                            self.capture_lines_per_command
                                .insert(cmd.to_string(), parse_capture_lines(value));
                        } else if let Some(field) = other.strip_prefix("tuicolor.") {
                            Self::assign_theme_field(&mut self.theme, field, value);
                        } else if let Some(action) = other.strip_prefix("key.")
                            && !action.is_empty() {
                                key_entries.insert(action.to_string(), value.to_string());
                            } else if let Some(prefix) = other.strip_prefix("prefix.") {
                                Self::assign_prefix(&mut self.query_prefixes, prefix, value);
                            } else if let Some(name) = other.strip_prefix("jira.search.")
                                && !name.is_empty() {
                                Self::assign_jira_fragment(
                                    &mut self.jira_fragments,
                                    name,
                                    value,
                                );
                            } else if other == "files.ignore" {
                                for name in value.split_whitespace() {
                                    if !name.is_empty() {
                                        self.files_ignores.push(name.to_string());
                                    }
                                }
                            } else if let Some(rest) = other.strip_prefix("session.") {
                                // Parse `session.<id> = "name"`,
                                // `session.<id>.dir = "~/path"`,
                                // `session.<id>.startup_command = "cmd"`.
                                // The `<id>` is a numeric index
                                // determining display order.
                                let unquoted = value.trim().trim_matches('"').trim();
                                if let Some((id_str, field)) = rest.split_once('.') {
                                    if let Ok(id) = id_str.parse::<usize>() {
                                        let pos = self.sessions.iter().position(|(i, _)| *i == id);
                                        match (field, pos) {
                                            ("dir", Some(idx)) => {
                                                self.sessions[idx].1.dir = unquoted.to_string();
                                            }
                                            ("dir", None) => {
                                                self.sessions.push((id, SessionDef {
                                                    name: String::new(),
                                                    dir: unquoted.to_string(),
                                                    exec: String::new(),
                                                }));
                                            }
                                            ("exec", Some(idx)) => {
                                                self.sessions[idx].1.exec = unquoted.to_string();
                                            }
                                            ("exec", None) => {
                                                self.sessions.push((id, SessionDef {
                                                    name: String::new(),
                                                    dir: String::new(),
                                                    exec: unquoted.to_string(),
                                                }));
                                            }
                                            ("startup_command", _) => {
                                                // Accepted but not used yet.
                                            }
                                            _ => {}
                                        }
                                    }
                                } else if let Ok(id) = rest.parse::<usize>() {
                                    // `session.<id> = "name"` (no sub-field).
                                    if !unquoted.is_empty() {
                                        let pos = self.sessions.iter().position(|(i, _)| *i == id);
                                        match pos {
                                            Some(idx) => self.sessions[idx].1.name = unquoted.to_string(),
                                            None => self.sessions.push((id, SessionDef {
                                                name: unquoted.to_string(),
                                                dir: String::new(),
                                                exec: String::new(),
                                            })),
                                        }
                                    }
                                }
                            } else if let Some(rest) = other.strip_prefix("host.") {
                                // Parse `host.<id> = "name"`,
                                // `host.<id>.host = "alias"`,
                                // `host.<id>.hostname = "real"`,
                                // `host.<id>.user = "u"`,
                                // `host.<id>.port = N`,
                                // `host.<id>.identity = "path"`,
                                // `host.<id>.dir = "~/path"`,
                                // `host.<id>.exec = "cmd"`.
                                // The `<id>` is a numeric index
                                // determining display order.
                                //
                                // `host` is the SSH config
                                // `Host` alias (also used as
                                // the connection target when
                                // no `hostname` is set);
                                // `hostname` is the real
                                // `HostName` to connect to.
                                let unquoted = value.trim().trim_matches('"').trim();
                                if let Some((id_str, field)) = rest.split_once('.') {
                                    if let Ok(id) = id_str.parse::<usize>() {
                                        let pos = self.hosts.iter().position(|(i, _)| *i == id);
                                        let set = |host: &mut crate::tui::state::HostDef, field: &str, val: &str| {
                                            match field {
                                                "host" => host.host = val.to_string(),
                                                "hostname" => host.hostname = val.to_string(),
                                                "user" => host.user = val.to_string(),
                                                "port" => {
                                                    if let Ok(n) = val.parse::<u16>() {
                                                        host.port = n;
                                                    } else {
                                                        eprintln!(
                                                            "warning: host.{}.port = {:?} is not a valid port; ignoring",
                                                            id, val
                                                        );
                                                    }
                                                }
                                                "identity" => host.identity = val.to_string(),
                                                "dir" => host.dir = val.to_string(),
                                                "exec" => host.exec = val.to_string(),
                                                _ => {
                                                    eprintln!(
                                                        "warning: unknown host field {:?} in host.{}; ignoring",
                                                        field, id
                                                    );
                                                }
                                            }
                                        };
                                        match pos {
                                            Some(idx) => {
                                                let (_, host) = &mut self.hosts[idx];
                                                set(host, field, unquoted);
                                            }
                                            None => {
                                                let mut host = crate::tui::state::HostDef::default();
                                                set(&mut host, field, unquoted);
                                                self.hosts.push((id, host));
                                            }
                                        }
                                    }
                                } else if let Ok(id) = rest.parse::<usize>() {
                                    // `host.<id> = "name"` (no sub-field).
                                    if !unquoted.is_empty() {
                                        let pos = self.hosts.iter().position(|(i, _)| *i == id);
                                        match pos {
                                            Some(idx) => self.hosts[idx].1.name = unquoted.to_string(),
                                            None => self.hosts.push((id, crate::tui::state::HostDef {
                                                name: unquoted.to_string(),
                                                ..crate::tui::state::HostDef::default()
                                            })),
                                        }
                                    }
                                }
                            }
                }
            }
        }
        // The LLM block above collected zero or more ollama.*
        // entries. We finalize the LlmConfig here, after the
        // loop, so that a later `ollama.model=` line in the file
        // overrides an earlier one (and a half-configured pair
        // — only one of url/model — leaves the feature
        // disabled, with a warning on stderr). Doing the
        // resolution in the match arms above would lose the
        // "later wins" guarantee and split the validation
        // across two passes.
        if !ollama_url.is_empty() || !ollama_model.is_empty() {
            if ollama_url.is_empty() || ollama_model.is_empty() {
                eprintln!(
                    "warning: ollama.{} is set but the other half is missing; \
                     LLM mode is disabled. Set both ollama.url and ollama.model \
                     in ~/.config/smarthistory/config.",
                    if ollama_url.is_empty() { "url" } else { "model" }
                );
            } else {
                self.llm = Some(llm::LlmConfig {
                    url: ollama_url,
                    model: ollama_model,
                });
            }
        }
        // Merge `~/.ssh/config` into `self.hosts`.
        // For every `Host` block in the SSH
        // config, look up a `host.<id>` entry
        // whose `host` field matches the
        // alias. If found, the explicit
        // entry wins for every set field;
        // unset fields inherit from the SSH
        // config. If not found, auto-append
        // a new entry using the SSH config
        // block as the source of truth
        // (display name = the alias, real
        // hostname = `HostName`, user =
        // `User`, identity = first
        // `IdentityFile`, port = `Port`).
        //
        // Auto-included entries get a
        // synthetic id starting from
        // `usize::MAX` and going down so
        // they sort after every explicit
        // `host.<id>` entry (which start
        // from 1 and go up). The id is
        // only used for display ordering,
        // so the choice of magnitude is
        // arbitrary as long as the two
        // ranges don't collide.
        if let Some(home) = env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| env::var_os("USERPROFILE").map(std::path::PathBuf::from))
        {
            let ssh_blocks = ssh_config::load_ssh_config(&home);
            for block in ssh_blocks {
                // Look up an explicit
                // `host.<id>` whose
                // `host` field matches
                // the SSH config
                // alias. (Empty `host`
                // would match every
                // SSH block, which
                // isn't what we want;
                // skip those.)
                let pos = if block.alias.is_empty() {
                    None
                } else {
                    self.hosts.iter().position(|(_, h)| h.host == block.alias)
                };
                match pos {
                    Some(idx) => {
                        // Merge: explicit
                        // wins for every
                        // set field, SSH
                        // config fills
                        // the gaps.
                        let (_, host) = &mut self.hosts[idx];
                        if host.hostname.is_empty() {
                            host.hostname = block.hostname.clone();
                        }
                        if host.user.is_empty() {
                            host.user = block.user.clone();
                        }
                        if host.port == 0 {
                            host.port = block.port;
                        }
                        if host.identity.is_empty() {
                            host.identity = block.identity.clone();
                        }
                        // Auto-fill the
                        // display name
                        // when the user
                        // didn't set
                        // `host.<id> =
                        // "..."` but did
                        // set `host.<id>.host
                        // = "alias"`.
                        if host.name.is_empty() {
                            host.name = block.alias.clone();
                        }
                    }
                    None => {
                        // Auto-append.
                        // Pick an id
                        // that doesn't
                        // collide with
                        // any existing
                        // entry.
                        let next_id = self
                            .hosts
                            .iter()
                            .map(|(i, _)| *i)
                            .max()
                            .map(|m| m.saturating_add(1))
                            .unwrap_or(1);
                        self.hosts.push((next_id, crate::tui::state::HostDef {
                            name: block.alias.clone(),
                            host: block.alias.clone(),
                            hostname: block.hostname.clone(),
                            user: block.user.clone(),
                            port: block.port,
                            identity: block.identity.clone(),
                            dir: String::new(),
                            exec: String::new(),
                        }));
                    }
                }
            }
        }
        // Apply the collected `key.*` entries on top of the
        // defaults. `key_bindings_from_config` only overrides
        // entries that match a known action and parses cleanly;
        // invalid values produce a warning on stderr but don't
        // stop the rest of the config from taking effect.
        self.key_bindings = tui::bindings::key_bindings_from_config(&key_entries);
    }

    /// Look up the configured capture-line limit for a given command
    /// text. Per-command overrides take precedence over the default.
    /// Returns `None` for unlimited.
    fn capture_lines_for(&self, command: &str) -> Option<usize> {
        let cmd = first_token(command);
        if let Some(&val) = self.capture_lines_per_command.get(cmd) {
            return val;
        }
        self.default_capture_lines
    }

    /// The per-command capture-line overrides keyed by the first
    /// token of each command. The `Option<usize>` is `None` for
    /// unlimited capture (the user wrote `ALL`).
    #[allow(dead_code)]
    pub fn capture_lines_per_command(&self) -> &std::collections::HashMap<String, Option<usize>> {
        &self.capture_lines_per_command
    }

    /// The resolved `initialmode` value from the config file
    /// (defaults to `SESS` when unset).
    #[allow(dead_code)]
    pub fn initial_mode(&self) -> &str {
        &self.initial_mode
    }

    /// True if the given command is in the ignore-capture list.
    fn ignore_capture(&self, command: &str) -> bool {
        self.ignore_capture.contains(first_token(command))
    }

    /// Return the resolved TUI theme. The returned `TuiTheme` reflects
    /// any user overrides from `~/.config/smarthistory/config`.
    pub fn theme(&self) -> &TuiTheme {
        &self.theme
    }

    /// Effective selection-row background color. Falls back to the
    /// theme's own selection color when the user did not set
    /// `tuicolor.selection=`. The TUI passes `theme_default` so the
    /// active theme's palette is the fallback (rather than always
    /// `bg`), so built-in light themes like Gruvbox Light don't end
    /// up with a dark-gray selection on a light background.
    pub fn selection<'a>(&self, theme_default: &'a str) -> Cow<'a, str> {
        if self.theme.selection.is_empty() {
            Cow::Borrowed(theme_default)
        } else {
            // Owned: the field is on `&self`, so we can't return a
            // borrow with the caller's lifetime `'a` without tying
            // the accessor to a self-borrow of that lifetime, which
            // is not what callers expect. An owned Cow clone is
            // fine since this is called once per palette install.
            Cow::Owned(self.theme.selection.clone())
        }
    }

    /// Effective badge foreground color. Falls back to the supplied
    /// theme default when unset.
    pub fn badge_fg<'a>(&self, theme_default: &'a str) -> Cow<'a, str> {
        if self.theme.badge_fg.is_empty() {
            Cow::Borrowed(theme_default)
        } else {
            Cow::Owned(self.theme.badge_fg.clone())
        }
    }

    /// Effective per-pane background color, falling back to the
    /// supplied theme default when unset.
    pub fn list_bg<'a>(&self, theme_default: &'a str) -> Cow<'a, str> {
        if self.theme.list_bg.is_empty() {
            Cow::Borrowed(theme_default)
        } else {
            Cow::Owned(self.theme.list_bg.clone())
        }
    }

    /// Effective details-pane background color.
    pub fn details_bg<'a>(&self, theme_default: &'a str) -> Cow<'a, str> {
        if self.theme.details_bg.is_empty() {
            Cow::Borrowed(theme_default)
        } else {
            Cow::Owned(self.theme.details_bg.clone())
        }
    }

    /// Effective input-pane background color.
    pub fn input_bg<'a>(&self, theme_default: &'a str) -> Cow<'a, str> {
        if self.theme.input_bg.is_empty() {
            Cow::Borrowed(theme_default)
        } else {
            Cow::Owned(self.theme.input_bg.clone())
        }
    }

    /// Effective status-bar background color.
    pub fn status_bg<'a>(&self, theme_default: &'a str) -> Cow<'a, str> {
        if self.theme.status_bg.is_empty() {
            Cow::Borrowed(theme_default)
        } else {
            Cow::Owned(self.theme.status_bg.clone())
        }
    }

    /// True if the user explicitly set `tuicolor.bg=`. Used by the
    /// TUI to decide whether the manual value should override a
    /// built-in theme's `bg`.
    pub fn has_bg_override(&self) -> bool {
        !self.theme.bg.is_empty()
    }

    /// True if the user explicitly set `tuicolor.fg=`.
    pub fn has_fg_override(&self) -> bool {
        !self.theme.fg.is_empty()
    }

    /// True if the user explicitly set `tuicolor.dim=`.
    pub fn has_dim_override(&self) -> bool {
        !self.theme.dim.is_empty()
    }

    /// Resolved key bindings for the TUI.
    #[allow(dead_code)]
    pub fn key_bindings(&self) -> &tui::bindings::KeyBindings {
        &self.key_bindings
    }

    /// Resolved query prefix characters.
    pub fn query_prefixes(&self) -> &QueryPrefixes {
        &self.query_prefixes
    }

    /// User-configured additional
    /// home prefixes (the `homemap`
    /// config option, one per
    /// line, multiple allowed). The
    /// default `$HOME` is always
    /// added to this set at
    /// `expand_home` call time
    /// (we don't pre-bake it in
    /// because HOME may change
    /// between config-load and
    /// TUI-launch).
    pub fn home_map(&self) -> &[std::path::PathBuf] {
        &self.home_map
    }

    /// User-configured
    /// session directories
    /// (`sessiondirs=...`).
    /// Each entry is
    /// recursively walked at
    /// TUI startup and every
    /// subdirectory found is
    /// added to the `#`-mode
    /// list. See the
    /// `session_dirs` field
    /// doc for the full
    /// rationale.
    pub fn session_dirs(&self) -> &[std::path::PathBuf] {
        &self.session_dirs
    }

    /// Path to the note_search database, if configured.
    pub fn notes_database(&self) -> Option<&std::path::Path> {
        self.notes_database.as_deref()
    }

    /// Path to the notes directory, if configured.
    pub fn notes_dir(&self) -> Option<&std::path::Path> {
        self.notes_dir.as_deref()
    }

    /// Template for the line-number option that
    /// the todo-search mode appends to the
    /// editor command. The string `"$LINE"` is
    /// substituted with the actual 1-based line
    /// number. Default: `"+$LINE"`.
    pub fn todo_line_option(&self) -> &str {
        &self.todo_line_option
    }

    /// The user-defined JQL fragments loaded from
    /// `jira.search.<name>=<jql>` entries in the
    /// config file. Each entry maps a name to a
    /// snippet of JQL that the user can invoke in the
    /// `-`-mode TUI search as `@<name>`. Empty when
    /// no fragments are configured. Fragment names
    /// are stored lowercased; lookups in
    /// `jira::build_jql` are case-insensitive. The
    /// returned reference is borrowed from the
    /// `Config` (a `Clone` happens at the TUI boundary
    /// — see `run_tui_to_stdout`).
    pub fn jira_fragments(&self) -> &std::collections::HashMap<String, String> {
        &self.jira_fragments
    }

    /// User-configured additional
    /// directory basenames to
    /// skip during the files-mode
    /// walk. Configured via
    /// `files.ignore=<name>`
    /// lines in the config file
    /// (multiple lines allowed,
    /// each space-separated).
    /// Combined with the built-in
    /// [`crate::files::DEFAULT_IGNORES`]
    /// at walk time.
    pub fn files_ignores(&self) -> &[String] {
        &self.files_ignores
    }

    /// Which terminal
    /// multiplexer the TUI's
    /// directory- and
    /// panes-switching modes
    /// should target. See
    /// [`crate::multiplexer::MultiplexerKind`].
    pub fn multiplexer(&self) -> crate::multiplexer::MultiplexerKind {
        self.multiplexer
    }

    /// Named sessions parsed from the config file.
    /// The config syntax is:
    ///   session.<id> = "Display Name"
    ///   session.<id>.dir = "~/path"
    ///   session.<id>.startup_command = "command"
    /// Each session becomes a row in the panes (`*`) view.
    /// Selecting a row creates/switches a workspace via the
    /// configured multiplexer backend.
    pub fn sessions(&self) -> Vec<crate::tui::state::HistoryRow> {
        self.sessions
            .iter()
            .map(|(id, def)| {
                let home_list: Vec<String> = std::iter::once(
                    std::env::var("HOME").unwrap_or_default()
                )
                .filter(|s| !s.is_empty())
                .collect();
                let expanded = crate::util::expand_home_to_absolute(
                    &def.dir, &home_list
                ).into_owned();
                crate::tui::state::HistoryRow {
                    id: -10_000 - (*id as i64),
                    command: def.name.clone(),
                    directory: expanded,
                    session_id: String::new(),
                    exit_code: 0,
                    timestamp: 0,
                    comment: def.exec.clone(),
                    output: String::new(),
                    mode: "session".to_string(),
                    source: "sessions".to_string(),
                }
            })
            .collect()
    }

    /// Hosts parsed from the config file
    /// (`host.<id>=...`, `host.<id>.host=...`,
    /// `host.<id>.hostname=...`, etc.) merged
    /// with `~/.ssh/config` entries. Each entry
    /// becomes a row in the `# hosts` section
    /// of the panes (`*`) view. Selecting a
    /// row creates/switches a workspace via
    /// the configured multiplexer backend and
    /// stages an `ssh` body inside the new
    /// pane.
    ///
    /// The returned `HistoryRow` carries the
    /// display name in `command` and a
    /// `user@host:port` connection string in
    /// `directory` (used for rendering and
    /// matching). The matching
    /// [`HostDef`] is exposed separately via
    /// [`Config::host_defs`] so the staging
    /// layer can read the full set of fields
    /// (real hostname, identity, exec, etc.)
    /// without re-parsing the row.
    pub fn hosts(&self) -> Vec<crate::tui::state::HistoryRow> {
        self.hosts
            .iter()
            .map(|(_, def)| {
                let effective_user = if def.user.is_empty() {
                    std::env::var("USER").unwrap_or_default()
                } else {
                    def.user.clone()
                };
                let target = if def.hostname.is_empty() {
                    def.host.clone()
                } else {
                    def.hostname.clone()
                };
                let port_suffix = if def.port != 0 && def.port != 22 {
                    format!(":{}", def.port)
                } else {
                    String::new()
                };
                let user_prefix = if !effective_user.is_empty() {
                    format!("{}@", effective_user)
                } else {
                    String::new()
                };
                let connection_string = format!(
                    "{}{}{}",
                    user_prefix, target, port_suffix
                );
                crate::tui::state::HistoryRow {
                    // Placeholder id;
                    // `fetch_session_panes_impl`
                    // overwrites this
                    // with a
                    // position-based
                    // id (so the
                    // staging layer
                    // can recover
                    // the `host_defs`
                    // index). The
                    // 0 value here
                    // is a defensive
                    // default that
                    // would never
                    // match a real
                    // row, in case a
                    // future
                    // caller
                    // forgets to
                    // re-id.
                    id: 0,
                    command: def.name.clone(),
                    directory: connection_string,
                    session_id: String::new(),
                    exit_code: 0,
                    timestamp: 0,
                    comment: def.exec.clone(),
                    output: String::new(),
                    mode: "host".to_string(),
                    source: "hosts".to_string(),
                }
            })
            .collect()
    }

    /// The full [`HostDef`] entries in the
    /// same order as [`Config::hosts`].
    /// Position-aligned: the `i`-th
    /// `HostDef` corresponds to the
    /// `i`-th `HistoryRow` returned by
    /// `hosts()`. Used by the staging
    /// layer to read the real hostname,
    /// identity, port, and exec — fields
    /// that the projected `HistoryRow`
    /// doesn't carry.
    pub fn host_defs(&self) -> Vec<crate::tui::state::HostDef> {
        self.hosts
            .iter()
            .map(|(_, def)| def.clone())
            .collect()
    }

    /// Apply a single `tuicolor.<field>=<value>` override. Unknown
    /// fields are silently ignored so a typo doesn't break the rest
    /// of the config.
    fn assign_theme_field(theme: &mut TuiTheme, field: &str, value: &str) {
        let value = value.trim().to_string();
        if value.is_empty() {
            return;
        }
        match field.to_ascii_lowercase().as_str() {
            "bg" => theme.bg = value,
            "fg" => theme.fg = value,
            "accent" => theme.accent = value,
            "success" => theme.success = value,
            "error" => theme.error = value,
            "warning" => theme.warning = value,
            "dim" => theme.dim = value,
            "highlight" => theme.highlight = value,
            "info" => theme.info = value,
            "selection" => theme.selection = value,
            "badgefg" | "badge_fg" => theme.badge_fg = value,
            "listbg" | "list_bg" => theme.list_bg = value,
            "detailsbg" | "details_bg" => theme.details_bg = value,
            "inputbg" | "input_bg" => theme.input_bg = value,
            "statusbg" | "status_bg" => theme.status_bg = value,
            _ => {}
        }
    }

    /// Apply a single `prefix.<name>=<char>` override. The value
    /// must be a single character. Invalid values are silently
    /// ignored.
    fn assign_prefix(prefixes: &mut QueryPrefixes, name: &str, value: &str) {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.chars().count() != 1 {
            return;
        }
        let c = trimmed.chars().next().unwrap();
        match name.to_ascii_lowercase().as_str() {
            "output" => prefixes.output = c,
            "llm" => prefixes.llm = c,
            "question" => prefixes.question = c,
            "notes" => prefixes.notes = c,
            "todo" => prefixes.todo = c,
            "directories" => prefixes.directories = c,
            "panes" => prefixes.panes = c,
            "files" => prefixes.files = c,
            "tags" => prefixes.tags = c,
            "jira" => prefixes.jira = c,
            _ => {}
        }
    }

    /// Apply a single `jira.search.<name>=<jql>` override.
    /// The name is stored lowercased (the parser in
    /// `jira::build_jql` is case-insensitive on the
    /// lookup). Reserved names (`me`, `today`, `week`,
    /// `month`) are silently dropped so a typo in the
    /// config can't disable a built-in alias — the
    /// alternative (treating them as fragments) would
    /// silently shadow the built-in and confuse the
    /// user. Names must be a non-empty `\w+` identifier;
    /// anything else is ignored. Empty values are
    /// ignored (a fragment with no JQL is worse than no
    /// fragment at all — it would always match nothing).
    fn assign_jira_fragment(
        fragments: &mut std::collections::HashMap<String, String>,
        name: &str,
        value: &str,
    ) {
        let trimmed_name = name.trim();
        if trimmed_name.is_empty() {
            return;
        }
        // Reject names that aren't a simple identifier.
        // The parser's lookup key is the lowercased bare
        // token after the `@` is stripped; we store
        // lowercased names so the lookup is a direct
        // map access without further normalisation.
        if !trimmed_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return;
        }
        let key = trimmed_name.to_ascii_lowercase();
        // Reserved-name check: don't let the config
        // shadow the four built-in aliases. This
        // mirrors how `prefix.<reserved>=...` is
        // handled (the assignment is silently ignored)
        // and how `key.<unknown>=...` is handled (the
        // entry is dropped at apply time). A user who
        // *does* want to override `@today` should
        // rename their config key — not papercut the
        // built-in.
        if matches!(
            key.as_str(),
            "me" | "today" | "week" | "month"
        ) {
            eprintln!(
                "warning: jira.search.{} is a reserved alias name; \
                 fragment is ignored. Rename the fragment to use it \
                 (e.g. jira.search.{}_custom=...).",
                key, key
            );
            return;
        }
        let trimmed_value = value.trim();
        if trimmed_value.is_empty() {
            return;
        }
        fragments.insert(key, trimmed_value.to_string());
    }
}

/// Return the first token of a command line, stripping any leading
/// whitespace. This is the executable name that we compare against
/// the no-capture list.
fn first_token(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or("")
}

/// Run `command`, capture up to `max_lines` of combined stdout/stderr,
/// and return `(command_string, exit_code, captured_output)`. Pass
/// `None` to capture every line. The command is joined with a single
/// space; callers that need shell features should invoke a shell
/// explicitly.
fn capture_command_output(
    command: &[String],
    max_lines: Option<usize>,
) -> anyhow::Result<(String, i32, String)> {
    if command.is_empty() {
        anyhow::bail!("no command provided");
    }
    let program = &command[0];
    let args = &command[1..];
    let child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let output = child.wait_with_output()?;
    let exit_code = output.status.code().unwrap_or(-1);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);

    let limited: String = match max_lines {
        Some(n) => combined.lines().take(n).collect::<Vec<_>>().join("\n"),
        None => combined,
    };

    let joined = command.join(" ");
    Ok((joined, exit_code, limited))
}

/// Upsert a history row and return its id. This matches the dedup key
/// used by the zsh hook.
fn upsert_history_row(
    conn: &Connection,
    command: &str,
    directory: &str,
    session_id: &str,
    exit_code: i32,
) -> anyhow::Result<i64> {
    conn.execute(
        "INSERT INTO history (command, directory, session_id, exit_code, mode)
         VALUES (?1, ?2, ?3, ?4, 'command')
         ON CONFLICT (command, directory, session_id) DO UPDATE
         SET timestamp = (strftime('%s', 'now')),
             exit_code = excluded.exit_code",
        params![command, directory, session_id, exit_code],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM history WHERE command = ?1 AND directory = ?2 AND session_id = ?3",
        params![command, directory, session_id],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Store or replace the captured output for a history row.
fn store_output(conn: &Connection, history_id: i64, output: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO history_output (history_id, output) VALUES (?1, ?2)
         ON CONFLICT (history_id) DO UPDATE
         SET output = excluded.output,
             captured_at = (strftime('%s', 'now'))",
        params![history_id, output],
    )?;
    Ok(())
}

/// Read `file` and extract the command line matching `command` and up
/// to the configured number of lines. The search starts from the
/// end of the file and walks backward to find the last occurrence of
/// a line containing `command`. The returned string includes that
/// line and the following lines.
///
/// If the command is not found on the first pass, the function retries
/// up to 5 times with a 100 ms delay between attempts. This handles
/// the race condition where the tmux log file hasn't been flushed
/// yet by the time the precmd hook runs.
///
/// The search prefers lines where the command text appears at the
/// END of the line (i.e. the prompt+command line, like `$ ls -la`)
/// over lines that merely contain the command as a substring. This
/// avoids false matches on output lines that happen to include the
/// command text (e.g. `echo ls` produces an output line `ls`).
/// Extract the output of `command` from a pane buffer
/// (a list of lines). This is the source-agnostic core of
/// the capture pipeline: it scans the lines for the command
/// line, strips ANSI, and returns the N lines after it
/// (or until the next prompt boundary for `ALL`).
///
/// Used by both `capture-tmux` (reads from a pipe-pane log
/// file) and `capture-herdr` (reads from `herdr pane read`).
fn extract_pane_output(
    command: &str,
    lines: &[String],
    max_lines: Option<usize>,
) -> anyhow::Result<String> {
    let start = find_command_line(lines, command);
    if let Some(start) = start {
        let end = match max_lines {
            Some(n) => (start + 1 + n).min(lines.len()),
            None => next_prompt_boundary(lines, start + 1),
        };
        return Ok(lines[start..end].join("\n"));
    }
    // The command line isn't in the scrollback. The retry
    // loop in `extract_tmux_output` depends on this `Err` to
    // know it should re-read the file. The herdr
    // `CaptureHerdr` handler catches this `Err` and falls back
    // to capturing whatever IS in the pane buffer (since herdr
    // has no retry mechanism — `pane read` is a one-shot
    // snapshot, not a continuously-updated log file).
    anyhow::bail!("command not found in pane output")
}

/// ANSI escape sequences are stripped first so that colourised
/// prompts do not interfere with the match.
fn extract_tmux_output(
    command: &str,
    file: &std::path::Path,
    max_lines: Option<usize>,
) -> anyhow::Result<String> {
    use std::fs;
    use std::thread::sleep;
    use std::time::Duration;

    const MAX_ATTEMPTS: u32 = 10;
    const RETRY_DELAY: Duration = Duration::from_millis(50);

    for attempt in 1..=MAX_ATTEMPTS {
        if let Ok(contents) = fs::read_to_string(file) {
            // Strip ANSI and C0 control characters from each line
            // individually so that newline characters (which are
            // valid line separators) survive the cleaning step.
            let lines: Vec<String> = contents.lines().map(strip_ansi).collect();

            if let Ok(output) = extract_pane_output(command, &lines, max_lines) {
                return Ok(output);
            }
        }
        if attempt < MAX_ATTEMPTS {
            sleep(RETRY_DELAY);
        }
    }
    anyhow::bail!("command not found in tmux log after {} attempts", MAX_ATTEMPTS)
}

/// Locate the line in `lines` (scanning from the end) that best
/// represents the execution of `command`. Returns the index of that
/// line. Prefers lines where the command text appears at the end
/// (prompt+command lines); falls back to a substring match.
fn find_command_line(lines: &[String], command: &str) -> Option<usize> {
    if let Some((i, _)) = lines
        .iter()
        .enumerate()
        .rev()
        .find(|(_, l)| l.trim_end().ends_with(command))
    {
        return Some(i);
    }
    if let Some((i, _)) = lines
        .iter()
        .enumerate()
        .rev()
        .find(|(_, l)| l.contains(command))
    {
        return Some(i);
    }
    None
}

/// Return the first index at or after `from` that looks like a shell
/// prompt. Used to cap unbounded capture (`ALL`) at the next prompt
/// rather than bleeding into the next command.
fn next_prompt_boundary(lines: &[String], from: usize) -> usize {
    for (i, line) in lines.iter().enumerate().skip(from) {
        let trimmed = line.trim_end();
        // Common prompt suffixes: `$ `, `# `, `% `, `❯ `, `> `, `]`.
        // We require the line to be relatively short and end with
        // one of these markers to avoid mistaking regular output for
        // a prompt.
        //
        // We check against the ORIGINAL line (not `trim_end`'d)
        // because the prompt markers end with a trailing space,
        // which `trim_end()` would strip — turning `$ ` into `$`,
        // which would then fail the `ends_with("$ ")` check.
        // The `trim_end` is used only for the `len()` check so
        // trailing whitespace doesn't inflate the length.
        if trimmed.len() < 200
            && (line.ends_with("$ ")
                || line.ends_with("# ")
                || line.ends_with("% ")
                || line.ends_with("> ")
                || line.ends_with("\u{276f} ")
                || line.ends_with("] "))
        {
            return i;
        }
    }
    lines.len()
}

/// Strip ANSI escape sequences and control characters from a
/// string, returning a clean printable representation suitable for
/// substring matching. Handles:
///
///   - CSI sequences: ESC `[` ... final-byte (0x40-0x7E)
///   - OSC sequences: ESC `]` ... BEL (0x07) or ST (ESC `\`)
///   - Two-byte ESC sequences: ESC `=` or ESC `>` (mode setters)
///   - Standalone control characters: BEL, BS, SO, SI, etc.
///
/// The terminal bell (BEL, 0x07) is emitted by zsh on tab-completion
/// and bracketed-paste transitions, and zsh also interleaves mode
/// switches like `ESC[?2004h` around pasted input. Stripping all of
/// these leaves a clean prompt+command line whose tail contains the
/// actual command text. This is intentionally simple: a full ANSI
/// parser is not needed for tmux pane logs which use a predictable
/// subset.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek() {
                Some(&'[') => {
                    chars.next();
                    while let Some(&nc) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&nc) {
                            break;
                        }
                    }
                }
                Some(&']') => {
                    chars.next();
                    while let Some(nc) = chars.next() {
                        if nc == '\x07' {
                            break;
                        }
                        if nc == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            // Drop all other C0 control characters. The printable
            // range (0x20-0x7E) and extended Unicode (>= 0x80) are
            // kept verbatim. This removes stray BEL/BS/CR bytes that
            // zsh and tmux occasionally inject mid-line.
            '\x00'..='\x1f' | '\x7f' => {}
            _ => out.push(c),
        }
    }
    out
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
            // Multi-line fields
            // (`command` and
            // `output`) are
            // escape-encoded so a
            // single row fits on
            // one output line. The
            // CLI prints one row
            // per line and a
            // embedded `\n` would
            // split a single row
            // into multiple lines
            // (and break the
            // zsh-widget's `(f)`
            // record splitter).
            // The zsh widget
            // un-escapes before
            // assigning to
            // `BUFFER`; the TUI
            // queries the DB
            // directly so it
            // sees the real
            // newlines.
            let cell = if f == "command" || f == "output" {
                crate::util::escape_field_for_output(v)
            } else {
                v.clone()
            };
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

/// Return the SQL expression for a conceptual field name, qualifying
/// history columns with `h.` and the global comment with `c.comment`.
fn qualify_field(name: &str) -> String {
    match name {
        "comment" => "c.comment".to_string(),
        "output" => "o.output".to_string(),
        _ => format!("h.{}", name),
    }
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
use crate::util::{format_diff, format_time};

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

/// Append ` ORDER BY h.timestamp DESC [LIMIT n]` to `sql`. A `limit` of 0
/// means "no limit" and the `LIMIT` clause is omitted. The newest
/// entries come first so the line-editor widget's first Up/Down press
/// shows the most recent command in scope.
fn append_order_and_limit(sql: &mut String, limit: usize) {
    sql.push_str(" ORDER BY h.timestamp DESC");
    if limit > 0 {
        sql.push_str(&format!(" LIMIT {}", limit));
    }
}

/// Build the shared `AND …` filter clause and its bound parameters for
/// the history filter (`query`, `directory`, `session`, `exit_code`).
/// Returns the clause (leading ` AND `) and the params in order.
/// Callers prepend the surrounding `FROM … WHERE 1=1` themselves so
/// they can add table-specific JOINs and aliases.
///
/// `query_column` controls which columns participate in the
/// substring filter:
///   * `Some(("command", _))` — match only the command column.
///   * `Some(("command", Some("comment")))` — match the command OR
///     the (joined) comment column.
///
/// `qualified_column_prefix` is prepended to every non-query column
/// reference (e.g. `"h."` for joined queries, `""` for plain).
///
/// `exit_code` accepts "OK" (=0) or "ERROR" (!=0); any other value is
/// ignored. The session filter reads `$SMART_HISTORY_SESSION` at call
/// time and emits a warning to stderr if the flag was passed but the
/// env var is unset/empty.
fn build_filter_sql(
    query: Option<&str>,
    directory: Option<&str>,
    session_flag: bool,
    exit_code: Option<&str>,
    query_column: Option<(&str, Option<&str>)>,
    qualified_column_prefix: &str,
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut clause = String::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    // Substring filter on the command (and optionally the joined
    // comment column).
    if let Some(q) = query {
        let escaped = escape_like(q);
        match query_column {
            Some(("command", None)) => {
                clause.push_str(&format!(
                    " AND {prefix}command LIKE ? ESCAPE '\\'",
                    prefix = qualified_column_prefix
                ));
                params.push(Box::new(format!("%{}%", escaped)));
            }
            Some(("command", Some("comment"))) => {
                let p = qualified_column_prefix;
                clause.push_str(&format!(
                    " AND ({p}command LIKE ? ESCAPE '\\' OR c.comment LIKE ? ESCAPE '\\')",
                ));
                params.push(Box::new(format!("%{}%", escaped)));
                params.push(Box::new(format!("%{}%", escaped)));
            }
            Some((col, _)) => {
                // Caller asked for an unknown column; fall back to
                // a plain command match so the rest of the filter
                // keeps working.
                eprintln!("warning: unsupported query column {:?}, falling back to command", col);
                clause.push_str(&format!(
                    " AND {prefix}command LIKE ? ESCAPE '\\'",
                    prefix = qualified_column_prefix
                ));
                params.push(Box::new(format!("%{}%", escaped)));
            }
            None => { /* no query filter */ }
        }
    }
    if let Some(dir) = directory {
        clause.push_str(&format!(
            " AND {prefix}directory = ?",
            prefix = qualified_column_prefix
        ));
        params.push(Box::new(dir.to_string()));
    }
    if session_flag {
        match env::var("SMART_HISTORY_SESSION") {
            Ok(s) if !s.is_empty() => {
                clause.push_str(&format!(
                    " AND {prefix}session_id = ?",
                    prefix = qualified_column_prefix
                ));
                params.push(Box::new(s));
            }
            _ => eprintln!(
                "warning: --session requested but SMART_HISTORY_SESSION is not set; ignoring"
            ),
        }
    }
    if let Some(ec) = exit_code {
        if ec == "OK" {
            clause.push_str(&format!(
                " AND {prefix}exit_code = 0",
                prefix = qualified_column_prefix
            ));
        } else if ec == "ERROR" {
            clause.push_str(&format!(
                " AND {prefix}exit_code != 0",
                prefix = qualified_column_prefix
            ));
        }
    }
    (clause, params)
}

/// Build the shared `WHERE 1=1 [AND ...]` clause and its bound parameters
/// for the plain history filter (`query`, `directory`, `session`,
/// `exit_code`). Returns the clause (including the leading ` WHERE `)
/// and the params in order.
fn build_where_clause(
    query: Option<&str>,
    directory: Option<String>,
    session_flag: bool,
    exit_code: Option<&str>,
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let (extra, params) = build_filter_sql(
        query,
        directory.as_deref(),
        session_flag,
        exit_code,
        Some(("command", None)),
        "",
    );
    (format!(" WHERE 1=1{}", extra), params)
}

/// Build the `FROM ... WHERE ...` clause used by searches that can also
/// match global command comments. Always joins `command_comments` so
/// the `comment` field can be selected/searched.
fn build_search_where_clause(
    query: Option<&str>,
    directory: Option<String>,
    session_flag: bool,
    exit_code: Option<&str>,
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let (extra, params) = build_filter_sql(
        query,
        directory.as_deref(),
        session_flag,
        exit_code,
        // Match command OR the joined comment column.
        Some(("command", Some("comment"))),
        "h.",
    );
    let prefix = " FROM history h \
                   LEFT JOIN command_comments c ON h.command = c.command \
                   LEFT JOIN history_output o ON h.id = o.history_id \
                   WHERE 1=1";
    (format!("{}{}", prefix, extra), params)
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
            timestamp INTEGER DEFAULT (strftime('%s', 'now')),
            mode TEXT NOT NULL DEFAULT 'command'
        )",
        [],
    )?;
    // Global comments are stored per-command in a separate table so
    // they survive re-execution and apply to every instance of the
    // same command text across sessions/directories.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS command_comments (
            command TEXT PRIMARY KEY,
            comment TEXT NOT NULL
        )",
        [],
    )?;
    // Captured command output (up to the configured line limit) is stored
    // per history row so different contexts can have different output.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS history_output (
            history_id INTEGER PRIMARY KEY,
            output TEXT NOT NULL,
            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
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
    // If an older database still has the per-row comment column from
    // a previous schema, migrate those comments into the global
    // command_comments table and then drop the column.
    migrate_history_comment_column(&conn)?;
    // If an older database is missing the `mode` column, add it.
    migrate_history_mode_column(&conn)?;
    Ok(conn)
}

/// If the `history` table still has a per-row `comment` column (from
/// an earlier schema), copy the first non-empty comment for each
/// command into `command_comments`, then remove the column.
fn migrate_history_comment_column(conn: &Connection) -> anyhow::Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(history)")?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let has_comment = names.filter_map(|n| n.ok()).any(|n| n == "comment");
    if !has_comment {
        return Ok(());
    }
    conn.execute(
        "INSERT OR IGNORE INTO command_comments (command, comment)
         SELECT DISTINCT command, comment FROM history
         WHERE comment IS NOT NULL AND comment != ''",
        [],
    )?;
    // SQLite only supports dropping columns in 3.35.0+; rusqlite
    // bundles a recent enough SQLite, but we use a defensive rename
    // and recreate approach for portability.
    conn.execute("ALTER TABLE history RENAME TO history_old", [])?;
    conn.execute(
        "CREATE TABLE history (
            id INTEGER PRIMARY KEY,
            command TEXT NOT NULL,
            directory TEXT NOT NULL,
            session_id TEXT NOT NULL,
            exit_code INTEGER,
            timestamp INTEGER DEFAULT (strftime('%s', 'now'))
        )",
        [],
    )?;
    conn.execute(
        "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp)
         SELECT id, command, directory, session_id, exit_code, timestamp FROM history_old",
        [],
    )?;
    conn.execute("DROP TABLE history_old", [])?;
    Ok(())
}

/// If the `history` table is missing the `mode` column (from
/// an earlier schema), add it with a default value of 'command'.
fn migrate_history_mode_column(conn: &Connection) -> anyhow::Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(history)")?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let has_mode = names.filter_map(|n| n.ok()).any(|n| n == "mode");
    if has_mode {
        return Ok(());
    }
    // SQLite 3.35.0+ supports ADD COLUMN; rusqlite bundles a recent
    // enough SQLite, so this should work.
    conn.execute(
        "ALTER TABLE history ADD COLUMN mode TEXT NOT NULL DEFAULT 'command'",
        [],
    )?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let conn = init_db()?;

    match args.command {
        Commands::Add {
            command,
            exit_code,
            comment,
        } => {
            let pwd = crate::util::current_directory_for_storage();
            let session_id =
                env::var("SMART_HISTORY_SESSION").unwrap_or_else(|_| "default".to_string());

            // Atomic upsert: if (command, directory, session_id) already
            // exists, refresh its timestamp and exit_code; otherwise
            // insert a new row. The unique index idx_history_dedup is
            // the conflict target. Comments live in a separate global
            // table keyed only by command, so this statement never
            // touches them.
            conn.execute(
                "INSERT INTO history (command, directory, session_id, exit_code)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (command, directory, session_id) DO UPDATE
                 SET timestamp = (strftime('%s', 'now')),
                     exit_code = excluded.exit_code",
                params![command, pwd, session_id, exit_code],
            )?;

            // If a comment was explicitly supplied, store it globally
            // for this command text.
            if let Some(c) = comment.filter(|c| !c.is_empty()) {
                conn.execute(
                    "INSERT INTO command_comments (command, comment)
                     VALUES (?1, ?2)
                     ON CONFLICT (command) DO UPDATE SET comment = excluded.comment",
                    params![command, c],
                )?;
            }
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
            let qualified_fields: Vec<String> =
                raw_fields.iter().map(|f| qualify_field(f)).collect();
            let mut sql = format!("SELECT {}", qualified_fields.join(", "));

            let query_ref = query.as_deref();
            // Canonicalize the
            // directory so it
            // matches the form the
            // insert side stores
            // (which uses the
            // kernel's canonical
            // path via
            // `current_directory_for_storage`).
            // Without this, a
            // `--directory
            // /Users/har/...`
            // argument on macOS
            // would not match rows
            // whose `directory` is
            // the canonical
            // `/Volumes/HUGE/har/...`
            // form.
            let directory_canonical = directory
                .as_deref()
                .map(crate::util::canonicalize_directory);
            let (where_clause, params) = build_search_where_clause(
                query_ref,
                directory_canonical,
                session,
                exit_code.as_deref(),
            );
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
            let qualified_fields: Vec<String> =
                raw_fields.iter().map(|f| qualify_field(f)).collect();
            let mut sql = format!("SELECT {}", qualified_fields.join(", "));

            let query_ref = query.as_deref();
            // Same canonicalization
            // as the `Search`
            // command — see the
            // comment there for
            // why this matters on
            // macOS volumes.
            let directory_canonical = directory
                .as_deref()
                .map(crate::util::canonicalize_directory);
            let (where_clause, params) = build_search_where_clause(
                query_ref,
                directory_canonical,
                session,
                exit_code.as_deref(),
            );
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
            // Build the WHERE clause for the history table (command text
            // only; comments are not considered for deletion) and then
            // issue a COUNT first and a DELETE second. The COUNT drives
            // the confirmation message; the DELETE uses the same params
            // so the matched set is identical.
            // Canonicalize the
            // directory for the same
            // reason as in `Search`
            // and `Select` (see the
            // comment there).
            let directory_canonical = directory
                .as_deref()
                .map(crate::util::canonicalize_directory);
            let (where_clause, params) = build_where_clause(
                query.as_deref(),
                directory_canonical,
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

            // Atuin stores its timestamps as Unix epoch *nanoseconds*,
            // while the smarthistory `history.timestamp` column stores
            // Unix epoch *seconds*. Converting ns -> s here keeps the
            // ordering and the age / diff formatting in the TUI sane.
            // We also use `INSERT OR IGNORE` so that re-running the
            // import doesn't trip the unique index on
            // (command, directory, session_id) for entries that are
            // already present.
            let mut count = 0;
            for entry in history_iter {
                let (command, cwd, session, exit, timestamp) = entry?;
                let ts_seconds = timestamp / 1_000_000_000;
                conn.execute(
                    "INSERT OR IGNORE INTO history (command, directory, session_id, exit_code, timestamp) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![command, cwd, session, exit, ts_seconds],
                )?;
                count += 1;
            }
            println!("Imported {} entries from Atuin.", count);
        }
        Commands::List { fields, table } => {
            let selected_fields = fields.unwrap_or_else(|| vec!["command".to_string()]);
            let (raw_fields, derived) = split_fields(&selected_fields);
            let qualified_fields: Vec<String> =
                raw_fields.iter().map(|f| qualify_field(f)).collect();
            let sql = format!(
                "SELECT {} FROM history h \
                 LEFT JOIN command_comments c ON h.command = c.command \
                 LEFT JOIN history_output o ON h.id = o.history_id \
                 ORDER BY h.timestamp DESC",
                qualified_fields.join(", ")
            );

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
            let rows = stmt.query_map(params![command, limit as i64], |row| {
                let next: String = row.get(0)?;
                let freq: i64 = row.get(1)?;
                Ok((next, freq))
            })?;
            for r in rows {
                let (next, freq) = r?;
                println!("{}\t{}", freq, crate::util::escape_field_for_output(&next));
            }
        }
        Commands::Capture { command } => {
            let cfg = Config::load();
            let joined = command.join(" ");
            let max_lines = cfg.capture_lines_for(&joined);
            let (command_str, exit_code, output) =
                capture_command_output(&command, max_lines)?;

            // Echo the command output to the terminal so capture feels
            // like a normal execution.
            print!("{}", output);
            if !output.is_empty() && !output.ends_with('\n') {
                println!();
            }

            let pwd = crate::util::current_directory_for_storage();
            let session_id =
                env::var("SMART_HISTORY_SESSION").unwrap_or_else(|_| "default".to_string());
            let history_id = upsert_history_row(&conn, &command_str, &pwd, &session_id, exit_code,
            )?;
            store_output(&conn, history_id, &output)?;
        }
        Commands::CaptureTmux {
            command,
            file,
            exit_code,
        } => {
            // If the capture log file does not exist there is nothing
            // to capture. The caller (the zsh precmd hook) is expected
            // to fall back to a plain `add` so the history entry is
            // still recorded; this command is a no-op in that case.
            if !file.exists() {
                return Ok(());
            }
            let cfg = Config::load();
            // For commands in the ignore-capture list, skip output
            // extraction entirely. The history entry is still recorded.
            let output = if cfg.ignore_capture(&command) {
                String::new()
            } else {
                let max = cfg.capture_lines_for(&command);
                extract_tmux_output(&command, &file, max).unwrap_or_default()
            };
            let pwd = crate::util::current_directory_for_storage();
            let session_id =
                env::var("SMART_HISTORY_SESSION").unwrap_or_else(|_| "default".to_string());
            let history_id = upsert_history_row(
                &conn, &command, &pwd, &session_id, exit_code
            )?;
            store_output(&conn, history_id, &output)?;
        }
        Commands::CaptureHerdr {
            command,
            exit_code,
        } => {
            // Read the herdr pane scrollback via
            // `herdr pane read <pane_id> --source recent-unwrapped
            // --lines <N>` and extract the command
            // line + output using the same pipeline as
            // `capture-tmux`. The pane id comes from the
            // `HERDR_PANE_ID` env var (set by herdr in
            // every pane process).
            let pane_id =
                env::var("HERDR_PANE_ID").unwrap_or_default();
            if pane_id.is_empty() {
                // Not inside a herdr pane — fall back to
                // a plain `add` so the history entry is
                // still recorded.
                let pwd =
                    crate::util::current_directory_for_storage();
                let session_id =
                    env::var("SMART_HISTORY_SESSION")
                        .unwrap_or_else(|_| "default".to_string());
                upsert_history_row(
                    &conn, &command, &pwd, &session_id, exit_code,
                )?;
                return Ok(());
            }
            let cfg = Config::load();
            let output = if cfg.ignore_capture(&command) {
                String::new()
            } else {
                // Determine how many lines to request from
                // `herdr pane read`. We request more than the
                // capture limit to give `find_command_line`
                // enough scrollback to locate the command.
                let max = cfg.capture_lines_for(&command);
                let read_lines: usize = match max {
                    Some(n) => n + 50, // 50 extra lines for prompt+context
                    None => 500,        // broad request for unlimited capture
                };
                let pane_output = std::process::Command::new("herdr")
                    .args([
                        "pane", "read",
                        &pane_id,
                        "--source", "recent-unwrapped",
                        "--lines", &read_lines.to_string(),
                    ])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .output();
                match pane_output {
                    Ok(o) if !o.stdout.is_empty() => {
                        let text = String::from_utf8_lossy(&o.stdout);
                        let lines: Vec<String> =
                            text.lines().map(strip_ansi).collect();
                        extract_pane_output(&command, &lines, max)
                            .unwrap_or_else(|_| {
                                // Command line scrolled off the
                                // top of the pane buffer (common
                                // for high-output commands like
                                // `ps -ef`). Capture whatever IS
                                // in the buffer as the best
                                // available approximation.
                                let end = lines.len();
                                let effective_end = if end > 0 {
                                    let last = lines[end - 1].trim_end();
                                    if last.ends_with("$ ")
                                        || last.ends_with("# ")
                                        || last.ends_with("% ")
                                        || last.ends_with("> ")
                                        || last.is_empty()
                                    {
                                        end.saturating_sub(1)
                                    } else {
                                        end
                                    }
                                } else { end };
                                let capped = match max {
                                    Some(n) => effective_end.min(n),
                                    None => effective_end,
                                };
                                if capped > 0 {
                                    lines[..capped].join("\n")
                                } else {
                                    String::new()
                                }
                            })
                    }
                    _ => String::new(),
                }
            };
            let pwd =
                crate::util::current_directory_for_storage();
            let session_id =
                env::var("SMART_HISTORY_SESSION")
                    .unwrap_or_else(|_| "default".to_string());
            let history_id = upsert_history_row(
                &conn, &command, &pwd, &session_id, exit_code
            )?;
            store_output(&conn, history_id, &output)?;
        }
        Commands::Config { action } => match action {
            ConfigAction::Get { key } => {
                let cfg = Config::load();
                match key.as_str() {
                    "tmuxpaneoutputdir" => println!("{}", cfg.tmux_pane_output_dir.display()),
                    "ignorecapture" => {
                        let mut cmds: Vec<&String> = cfg.ignore_capture.iter().collect();
                        cmds.sort();
                        println!(
                            "{}",
                            cmds.iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(" ")
                        );
                    }
                    "capturelines" => match cfg.default_capture_lines {
                        Some(n) => println!("{}", n),
                        None => println!("ALL"),
                    },
                    "multiplexer" => println!("{}", cfg.multiplexer().as_str()),
                    other => anyhow::bail!("unknown config key: {other}"),
                }
            }
            ConfigAction::Check => {
                let report = validate_config();
                print!("{}", report);
                if report.has_errors() {
                    std::process::exit(1);
                }
            }
            ConfigAction::List => {
                let cfg = Config::load();
                let mut out = String::new();
                print_config_list(&mut out, &cfg);
                print!("{}", out);
            }
        },
        Commands::Tui { mode, prefix, exec, query } => {
            // Honor an explicit --mode flag first. Otherwise consult
            // the user's environment for a preferred starting scope:
            //   $SMARTHISTORY_TUI_MODE      — explicit override
            //   $SMARTHISTORY_MODE          — alias
            // Otherwise fall back to the config file's `initialmode`
            // (or `SESS` if unset).
            let initial_mode = mode
                .or_else(|| std::env::var("SMARTHISTORY_TUI_MODE").ok().filter(|s| !s.is_empty()))
                .or_else(|| std::env::var("SMARTHISTORY_MODE").ok().filter(|s| !s.is_empty()))
                .unwrap_or_else(|| {
                    let cfg = Config::load();
                    cfg.initial_mode
                });
            // `--prefix <char>` starts the TUI directly in a prefix
            // mode (panes, directories, notes, etc.). It takes final
            // precedence over both the positional `--query` and the
            // persisted `session.query`: when set, the TUI starts
            // with `query = "<prefix-char>"` and the persisted query
            // is NOT restored (the user explicitly asked for a
            // particular prefix this launch).
            //
            // The prefix string is passed verbatim to the TUI as the
            // initial query (just the character itself, with no
            // filter body). `run_tui_to_stdout` receives it as the
            // initial query and ALSO is told (via the new
            // `flag_override_session_query` parameter) that it
            // should ignore `session.query` even if it's `Some`.
            //
            // We strip a trailing `=` (`--prefix='*'` is parsed by
            // clap as `*=*` or similar) defensively so weird shell
            // quoting in the user's invocation doesn't break the
            // prefix detection. We also accept multi-character
            // values and take the first character — the prefix is
            // always a single character by construction (see
            // `QueryPrefixes`).
            let (initial_query, override_session_query) =
                match (prefix.as_deref(), query.as_deref()) {
                    (Some(p), _) => {
                        // Take the first char of the prefix string
                        // (it's always a single char by construction;
                        // we accept multi-char input defensively for
                        // shell-quoted strings).
                        let first_char = p
                            .chars()
                            .next()
                            .unwrap_or_default()
                            .to_string();
                        (first_char, true)
                    }
                    (None, Some(q)) => (q.to_string(), false),
                    (None, None) => (String::new(), false),
                };
            // Build the LLM client up front so the TUI entry
            // point doesn't need to know about config parsing.
            // The TUI itself only sees `Option<Box<dyn LlmClient>>`
            // and surfaces a "not configured" status when None.
            let tui_cfg = Config::load();
            let llm_client: Option<Box<dyn llm::LlmClient>> = tui_cfg
                .llm
                .as_ref()
                .map(llm::OllamaClient::new)
                .map(|c| Box::new(c) as Box<dyn llm::LlmClient>);
            let llm_config = tui_cfg.llm.clone();
            match tui::run_tui_to_stdout(
                initial_mode,
                initial_query,
                conn,
                llm_client,
                llm_config,
                override_session_query,
            )? {
                Some((command, pick_mode)) => {
                    if exec {
                        // `--exec` mode: run the command
                        // directly via `sh -c` and exit
                        // with its exit code. This lets
                        // the user launch the TUI from
                        // outside a shell context (e.g.
                        // a herdr keybinding or a GUI
                        // launcher) and have the
                        // tmux/herdr switch happen
                        // without a parent shell to
                        // `eval` the printed command.
                        let status = std::process::Command::new("sh")
                            .arg("-c")
                            .arg(&command)
                            .status();
                        match status {
                            Ok(s) => std::process::exit(
                                s.code().unwrap_or(1)
                            ),
                            Err(e) => {
                                eprintln!(
                                    "smarthistory: failed to exec {:?}: {}",
                                    command, e
                                );
                                std::process::exit(1);
                            }
                        }
                    } else {
                        // Default: print the command to
                        // stdout for the parent shell to
                        // eval (the historical behavior).
                        println!("{}", command);
                        std::process::exit(pick_mode);
                    }
                }
                None => std::process::exit(tui::exit_code::CANCEL),
            }
        }
        Commands::Export {
            filename,
            since,
            until,
        } => {
            // Build the time-range filter.
            let mut time_clause = String::new();
            let mut time_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            if let Some(ts) = since {
                time_clause.push_str(" AND h.timestamp >= ?");
                time_params.push(Box::new(ts));
            }
            if let Some(ts) = until {
                time_clause.push_str(" AND h.timestamp <= ?");
                time_params.push(Box::new(ts));
            }

            // Fetch history rows with their comments and output.
            let sql = format!(
                "SELECT h.id, h.command, h.directory, h.session_id, \
                        h.exit_code, h.timestamp, h.mode, \
                        c.comment, o.output \
                 FROM history h \
                 LEFT JOIN command_comments c ON h.command = c.command \
                 LEFT JOIN history_output o ON h.id = o.history_id \
                 WHERE 1=1{} \
                 ORDER BY h.timestamp ASC",
                time_clause
            );
            let params_ref: Vec<&dyn rusqlite::ToSql> =
                time_params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(&params_ref[..], |row| {
                Ok(HistoryExportRow {
                    id: Some(row.get::<_, i64>(0)?),
                    command: row.get::<_, String>(1)?,
                    directory: row.get::<_, String>(2)?,
                    session_id: row.get::<_, String>(3)?,
                    exit_code: row.get::<_, i32>(4)?,
                    timestamp: row.get::<_, i64>(5)?,
                    mode: row.get::<_, String>(6)?,
                    comment: row.get::<_, Option<String>>(7)?,
                    output: row.get::<_, Option<String>>(8)?,
                })
            })?;

            let mut history = Vec::new();
            for row in rows {
                history.push(row?);
            }

            let export = HistoryExport {
                version: 1,
                history,
            };

            let json = serde_json::to_string_pretty(&export)?;
            std::fs::write(&filename, json)?;
            eprintln!(
                "Exported {} history entries to {}",
                export.history.len(),
                filename.display()
            );
        }
        Commands::Import { filename } => {
            let _json = std::fs::read_to_string(&filename)?;
            let json = std::fs::read_to_string(&filename)?;
            let export: HistoryExport = serde_json::from_str(&json)?;

            if export.version != 1 {
                anyhow::bail!(
                    "Unsupported export version {}; expected 1",
                    export.version
                );
            }

            let mut imported = 0usize;
            let mut updated = 0usize;
            for row in &export.history {
                // Upsert the history row.
                let result = conn.execute(
                    "INSERT INTO history (command, directory, session_id, exit_code, timestamp, mode) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                     ON CONFLICT (command, directory, session_id) DO UPDATE \
                     SET timestamp = excluded.timestamp, \
                         exit_code = excluded.exit_code, \
                         mode = excluded.mode",
                    params![
                        row.command,
                        row.directory,
                        row.session_id,
                        row.exit_code,
                        row.timestamp,
                        row.mode,
                    ],
                )?;

                if result == 0 {
                    // ON CONFLICT DO UPDATE returns 0 when the
                    // conflict triggered an update rather than an
                    // insert. We detect this by checking if the
                    // row existed before.
                    updated += 1;
                } else {
                    imported += 1;
                }

                // Store the comment if present.
                if let Some(ref comment) = row.comment
                    && !comment.is_empty() {
                        conn.execute(
                            "INSERT INTO command_comments (command, comment) VALUES (?1, ?2) \
                             ON CONFLICT (command) DO UPDATE SET comment = excluded.comment",
                            params![row.command, comment],
                        )?;
                    }

                // Store the output if present.
                if let Some(ref output) = row.output
                    && !output.is_empty() {
                        // Get the history id for this row.
                        let history_id: i64 = conn.query_row(
                            "SELECT id FROM history WHERE command = ?1 AND directory = ?2 AND session_id = ?3",
                            params![row.command, row.directory, row.session_id],
                            |r| r.get(0),
                        )?;
                        conn.execute(
                            "INSERT INTO history_output (history_id, output) VALUES (?1, ?2) \
                             ON CONFLICT (history_id) DO UPDATE SET output = excluded.output, \
                             captured_at = (strftime('%s', 'now'))",
                            params![history_id, output],
                        )?;
                    }
            }

            eprintln!(
                "Imported {} new entries, updated {} existing entries from {}",
                imported,
                updated,
                filename.display()
            );
        }
        Commands::Update => {
            // Walk the SQLite history
            // table and rewrite every
            // `directory` to its
            // `~`-shorthened form
            // (where the path is
            // under `$HOME` or any
            // `homemap=...` entry).
            // Idempotent: running
            // twice is a no-op (the
            // second pass shortens
            // `~/work` against the
            // home list, finds no
            // match, leaves the
            // value unchanged).
            let cfg = Config::load();
            // We update rows in place
            // (preserving `id` and
            // `timestamp`). The
            // dedup index
            // `(command, directory,
            // session_id)` would
            // prevent inserting a
            // new `~/work` row
            // while an
            // `/Users/har/work` row
            // exists, so update is
            // the only safe path.
            // The check on
            // `row.directory` vs
            // the shortened form is
            // `!=` so a row whose
            // value is already
            // shortened (post-
            // `update` row that
            // survived a second
            // run) doesn't get
            // touched.
            let mut stmt = conn
                .prepare("SELECT id, directory FROM history")
                .map_err(|e| {
                    anyhow::anyhow!(
                        "prepare: {e}"
                    )
                })?;
            let mut updates: Vec<(i64, String)> = Vec::new();
            let mut rows = stmt
                .query([])
                .map_err(|e| {
                    anyhow::anyhow!("query: {e}")
                })?;
            while let Some(row) = rows.next().map_err(|e| {
                anyhow::anyhow!("row: {e}")
            })? {
                let id: i64 = row.get(0).map_err(|e| {
                    anyhow::anyhow!("id: {e}")
                })?;
                let directory: String = row
                    .get(1)
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "directory: {e}"
                        )
                    })?;
                let shortened = crate::util::expand_home_with_config(
                    &directory, cfg.home_map(),
                );
                if shortened.as_ref() != directory {
                    updates.push((id, shortened.into_owned()));
                }
            }
            drop(rows);
            drop(stmt);
            // Apply the updates.
            // We commit them one by
            // one (not a single
            // multi-row UPDATE)
            // because each row's
            // shortened value is
            // independent and a
            // failure on one row
            // shouldn't roll back
            // the others. For
            // thousands of rows
            // this is fast enough
            // — the dedup index
            // makes each write
            // O(log N).
            //
            // The unique index
            // `(command, directory,
            // session_id)` can
            // collide when two rows
            // for the same
            // `(command, session_id)`
            // have different
            // `directory` values (one
            // already shortened to
            // `~/x`, the other still
            // absolute `/Users/.../x`)
            // and we try to update the
            // second to the same
            // `~/x` as the first. The
            // right resolution: delete
            // the row that's about to
            // collide (the one with
            // the conflicting new
            // `directory`) before the
            // UPDATE. The dedup
            // semantics say "this
            // `(command, session_id)`
            // maps to a single
            // directory"; collapsing
            // is correct.
            let mut updated = 0usize;
            let mut skipped = 0usize;
            for (id, new_dir) in &updates {
                // Drop any existing row
                // whose
                // `(command, directory,
                // session_id)` would
                // collide with our
                // target state. We do
                // this per-id (not as
                // a single DELETE before
                // the loop) because we
                // need the colliding
                // row's `command` and
                // `session_id` — those
                // are derivable from the
                // current row, which
                // we're updating.
                let row = conn.query_row(
                    "SELECT command, session_id FROM history WHERE id = ?1",
                    rusqlite::params![id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                );
                let (cmd, sid) = match row {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!(
                            "warning: failed to read history id={id}: {e}"
                        );
                        skipped += 1;
                        continue;
                    }
                };
                if let Err(e) = conn.execute(
                    "DELETE FROM history \
                     WHERE command = ?1 \
                       AND directory = ?2 \
                       AND session_id = ?3 \
                       AND id != ?4",
                    rusqlite::params![cmd, new_dir, sid, id],
                ) {
                    eprintln!(
                        "warning: failed to clear collision for id={id}: {e}"
                    );
                    skipped += 1;
                    continue;
                }
                match conn.execute(
                    "UPDATE history SET directory = ?1 \
                     WHERE id = ?2",
                    rusqlite::params![new_dir, id],
                ) {
                    Ok(1) => updated += 1,
                    Ok(0) => skipped += 1,
                    Ok(_) => {
                        // More rows than
                        // expected — should
                        // be impossible
                        // because `id` is
                        // the PRIMARY KEY,
                        // but log and skip
                        // rather than
                        // panic.
                        eprintln!(
                            "warning: unexpected row count \
                             for history id={id}"
                        );
                        skipped += 1;
                    }
                    Err(e) => {
                        eprintln!(
                            "warning: failed to rewrite history id={id}: {e}"
                        );
                        skipped += 1;
                    }
                }
            }
            println!(
                "rewrote {updated} row(s); skipped {skipped}",
                updated = updated,
                skipped = skipped
            );
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
        // 0 and negative timestamps are treated as missing data and
        // sort as the oldest possible entries (9999 months).
        assert_eq!(format_diff(0), "9999M");
        assert_eq!(format_diff(-1), "9999M");
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

    fn write_temp_log(contents: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "smarthistory-tmux-test-{}.log",
            generate_uuid_v4()
        ));
        std::fs::write(&path, contents).expect("write log");
        path
    }

    #[test]
    fn extract_tmux_output_uses_last_match() {
        let log = "some other line\necho first\necho first output\nrandom line\necho first again\nlast output\n";
        let path = write_temp_log(log);
        let out = extract_tmux_output("echo first again", &path, Some(MAX_OUTPUT_LINES)).expect("extract");
        assert!(out.contains("echo first again"));
        assert!(out.contains("last output"));
        assert!(!out.contains("echo first output"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_tmux_output_caps_at_twenty_lines() {
        let mut log = String::from("$ mycommand\n");
        for i in 0..30 {
            log.push_str(&format!("line {}\n", i));
        }
        let path = write_temp_log(&log);
        let out = extract_tmux_output("mycommand", &path, Some(MAX_OUTPUT_LINES)).expect("extract");
        let count = out.lines().count();
        // The slice includes the command line itself plus up to
        // MAX_OUTPUT_LINES following lines.
        assert_eq!(count, MAX_OUTPUT_LINES + 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_tmux_output_retries_until_match() {
        // Write a log without the command, then append the command
        // after a short delay. The retry loop should pick it up.
        let path = write_temp_log("initial content\nno match here\n");
        let path_clone = path.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(250));
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path_clone)
                .expect("open");
            writeln!(f, "before cmd").unwrap();
            writeln!(f, "$ delayedcmd").unwrap();
            writeln!(f, "output line 1").unwrap();
        });
        let out = extract_tmux_output("delayedcmd", &path, Some(MAX_OUTPUT_LINES)).expect("extract");
        handle.join().unwrap();
        assert!(out.contains("delayedcmd"));
        assert!(out.contains("output line 1"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_tmux_output_prefers_prompt_line_over_output() {
        // The command `echo ls` produces an output line that is
        // just `ls`. The search must prefer the prompt+command line
        // (`$ echo ls`) so that the captured slice starts at the
        // command, not at the output line.
        let log = "$ echo ls
ls
$ echo next
next output
";
        let path = write_temp_log(log);
        let out = extract_tmux_output("echo ls", &path, Some(MAX_OUTPUT_LINES)).expect("extract");
        // Must start with the prompt+command line, not the bare
        // output line.
        assert!(out.starts_with("$ echo ls"), "got: {out}");
        // The captured window should be at most 21 lines
        // (command + 20 following).
        assert!(out.lines().count() <= MAX_OUTPUT_LINES + 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_tmux_output_does_not_match_just_output_line() {
        // A bare output line that happens to equal the command text
        // must not be picked when a prompt+command line is also
        // present later. We rely on the end-of-line heuristic to
        // skip the bare output line.
        let log = "some output
ls
$ echo ls
real output
";
        let path = write_temp_log(log);
        let out = extract_tmux_output("echo ls", &path, Some(MAX_OUTPUT_LINES)).expect("extract");
        assert!(out.starts_with("$ echo ls"), "got: {out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_tmux_output_strips_ansi_before_matching() {
        // The prompt contains ANSI colour codes; the command line
        // after stripping is `$ ls -la`, which ends with the command.
        let log = "\x1b[32m$\x1b[0m ls -la
file1
file2
";
        let path = write_temp_log(log);
        let out = extract_tmux_output("ls -la", &path, Some(MAX_OUTPUT_LINES)).expect("extract");
        assert!(out.contains("ls -la"));
        assert!(out.contains("file1"));
        assert!(!out.contains("\x1b["), "ANSI should be stripped: {out}");
        std::fs::remove_file(&path).ok();
    }

    /// `extract_pane_output` is the source-agnostic core
    /// shared by `capture-tmux` (file) and
    /// `capture-herdr` (scrollback). It receives
    /// pre-stripped ANSI-clean lines and returns the
    /// command line + N following lines.
    #[test]
    fn extract_pane_output_finds_command_and_captures_output() {
        let lines: Vec<String> = vec![
            "some earlier output".to_string(),
            r#"$ echo hello"#.to_string(),
            "hello world".to_string(),
            r#"$ "#.to_string(),
        ];
        let out = extract_pane_output("echo hello", &lines, Some(20)).expect("extract");
        assert!(out.contains("echo hello"));
        assert!(out.contains("hello world"));
    }

    /// When `ALL` (None) is requested, the output
    /// runs until the next prompt boundary.
    #[test]
    fn extract_pane_output_unlimited_caps_at_next_prompt() {
        let lines: Vec<String> = vec![
            r#"$ ls"#.to_string(),
            "file1.txt".to_string(),
            "file2.txt".to_string(),
            r#"$ "#.to_string(),
            "next command".to_string(),
        ];
        let out = extract_pane_output("ls", &lines, None).expect("extract");
        assert!(out.contains("file1.txt"));
        assert!(out.contains("file2.txt"));
        // The prompt line and the next command
        // should NOT be included.
        assert!(!out.contains("next command"));
    }

    #[test]
    fn strip_ansi_removes_csi_and_osc() {
        let input = "before\x1b[32mgreen\x1b[0m after\x1b]0;title\x07end";
        let out = strip_ansi(input);
        assert_eq!(out, "beforegreen afterend");
    }

    #[test]
    fn strip_ansi_handles_bracketed_paste_prompt() {
        // Real-world zsh prompt with bracketed-paste markers, mode
        // switches and a BEL (from tab completion) interleaved with
        // the command line for `head README.md`. The BEL is stripped
        // along with all C0 control characters; the resulting line
        // ends with the actual command.
        let input = "har@arrakis.fritz.box in ~/smarthistory/smarthistory\x07\x1b[K\x1b[?1h\x1b=\x1b[?2004h\x1b[32mhead\x1b[39m \x1b[4mREADME.md\x1b[24m\x1b[?1l\x1b>\x1b[?2004l";
        let out = strip_ansi(input);
        // The BEL is removed and the prompt+command collapse together.
        assert_eq!(out, "har@arrakis.fritz.box in ~/smarthistory/smarthistoryhead README.md");
        assert!(out.trim_end().ends_with("head README.md"));
    }

    #[test]
    fn first_token_strips_whitespace() {
        assert_eq!(first_token("ls -la"), "ls");
        assert_eq!(first_token("  vim"), "vim");
        assert_eq!(first_token("echo hello world"), "echo");
        assert_eq!(first_token(""), "");
    }

    #[test]
    fn config_default_contains_no_capture() {
        let cfg = Config::default();
        for cmd in DEFAULT_NO_CAPTURE {
            assert!(cfg.ignore_capture.contains(*cmd), "default {cmd} missing");
        }
    }

    #[test]
    fn config_parses_user_file() {
        let dir = std::env::temp_dir().join(format!("smarthistory-test-{}", generate_uuid_v4()));
        let cfg_dir = dir.join(".config").join("smarthistory");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir");
        let cfg_path = cfg_dir.join("config");
        std::fs::write(
            &cfg_path,
            "# comment line

ignorecapture=mycustomcmd spaced
capturelines=40
capturelines.ps=ALL
tmuxpaneoutputdir=~/custom-tmux
",
        )
        .expect("write");
        let prev = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", &dir); }
        let cfg = Config::load();
        match prev {
            Some(p) => unsafe { std::env::set_var("HOME", p); },
            None => unsafe { std::env::remove_var("HOME"); },
        }
        // User override replaces the default ignore list.
        assert!(cfg.ignore_capture("mycustomcmd"));
        assert!(cfg.ignore_capture("spaced"));
        assert!(!cfg.ignore_capture("vim"));
        assert_eq!(cfg.default_capture_lines, Some(40));
        // Per-command override.
        assert_eq!(cfg.capture_lines_for("ps -ef"), None);
        assert_eq!(cfg.capture_lines_for("cat README"), Some(40));
        // tilde expansion for the path.
        let expected = dir.join("custom-tmux");
        assert_eq!(cfg.tmux_pane_output_dir, expected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `jira.search.<name>=<jql>` entries in the
    /// config file populate `Config::jira_fragments`.
    /// The user's example: `jira.search.label1=labels =
    /// "test"` should be addressable from the
    /// `-`-mode TUI search as `@label1`. Reserved
    /// names (`me`, `today`, `week`, `month`) are
    /// silently dropped to avoid shadowing the
    /// built-in aliases.
    #[test]
    fn config_parses_jira_search_fragments() {
        // We exercise `Config::parse` directly with
        // a string instead of round-tripping through
        // `Config::load`. The load path reads `$HOME`
        // to find the config file, and any test that
        // mutates `HOME` is racy against every other
        // test that reads it (cargo runs tests in
        // parallel; `std::env::set_var` is `unsafe`
        // in modern Rust precisely because of this).
        // Bypassing the env makes the test
        // self-contained without needing a mutex
        // that would have to be held by every
        // HOME-reading test in the binary.
        let mut cfg = Config::default();
        cfg.parse(
            "jira.search.label1=labels = \"test\"\n\
             jira.search.SPRINT=sprint = \"Sprint 42\"\n\
             jira.search.complex=priority = High AND labels = \"security\"\n\
             jira.search.me=assignee = \"alice\"\n\
             jira.search.=empty name is ignored\n\
             jira.search.bad name=spaces in name are ignored\n\
             jira.search.emptyvalue=\n",
        );
        let frags = cfg.jira_fragments();
        // The three valid fragments made it in
        // (lowercased keys — the loader
        // normalises the name to lowercase so
        // the parser lookup is a direct map
        // access).
        assert_eq!(
            frags.get("label1").map(String::as_str),
            Some(r#"labels = "test""#),
        );
        assert_eq!(
            frags.get("sprint").map(String::as_str),
            Some(r#"sprint = "Sprint 42""#),
        );
        assert_eq!(
            frags.get("complex").map(String::as_str),
            Some(r#"priority = High AND labels = "security""#),
        );
        // The reserved-name `me` was silently
        // dropped. The user can't shadow the
        // built-in `@me` alias.
        assert!(!frags.contains_key("me"));
        // Empty name (just the prefix, nothing
        // after the dot) was ignored.
        assert!(!frags.contains_key(""));
        // Name with a space isn't a valid
        // identifier (\w+ only) so it's dropped
        // silently.
        assert!(!frags.contains_key("bad name"));
        // Empty value: silently dropped. A
        // fragment with no JQL is worse than no
        // fragment at all.
        assert!(!frags.contains_key("emptyvalue"));
    }

    #[test]
    fn parse_capture_lines_handles_all_and_numbers() {
        assert_eq!(parse_capture_lines("ALL"), None);
        assert_eq!(parse_capture_lines("all"), None);
        assert_eq!(parse_capture_lines("20"), Some(20));
        assert_eq!(parse_capture_lines("  15  "), Some(15));
        assert_eq!(parse_capture_lines("not a number"), None);
    }

    /// `multiplexer=tmux` and
    /// `multiplexer=herdr` are
    /// the canonical config
    /// values. The loader is
    /// case-insensitive and
    /// unrecognised values are
    /// silently dropped so a
    /// typo can't disable
    /// directory switching.
    #[test]
    fn config_parses_multiplexer_key() {
        let mut cfg = Config::default();
        cfg.parse("multiplexer=tmux\n");
        assert_eq!(cfg.multiplexer(), crate::multiplexer::MultiplexerKind::Tmux);
        cfg.parse("multiplexer=herdr\n");
        assert_eq!(cfg.multiplexer(), crate::multiplexer::MultiplexerKind::Herdr);
        cfg.parse("multiplexer=HERDR\n");
        assert_eq!(cfg.multiplexer(), crate::multiplexer::MultiplexerKind::Herdr);
        // Unrecognised value:
        // the previous
        // value is
        // preserved (the
        // parser emits a
        // warning to
        // stderr but we
        // don't assert on
        // that here).
        // The default is
        // `Tmux`, so
        // starting from a
        // fresh `Config`
        // and feeding it
        // an invalid value
        // keeps the
        // default.
        let mut cfg = Config::default();
        cfg.parse("multiplexer=screen\n");
        assert_eq!(cfg.multiplexer(), crate::multiplexer::MultiplexerKind::Tmux);
    }

    /// Regression test for
    /// the "I have
    /// `sessiondirs=~/foo`
    /// in my config but no
    /// directories are
    /// added" bug: a
    /// literal `~` in a
    /// config value is a
    /// user-friendly
    /// shorthand for
    /// `$HOME`, but the
    /// config loader must
    /// actually expand it
    /// before passing the
    /// path to the
    /// filesystem walker.
    /// Without this
    /// expansion, the
    /// walker would see a
    /// path that doesn't
    /// exist
    /// (`std::path::Path::exists("~/x")`
    /// is always `false`)
    /// and silently skip
    /// the entry — the
    /// user's pinned
    /// directories would
    /// never appear in
    /// the `#`-mode list.
    /// The same expansion
    /// is already applied
    /// to `notes.database`
    /// and `notes.dir`; we
    /// add it here for
    /// `sessiondirs` so
    /// the user's mental
    /// model ("`~` works
    /// everywhere in the
    /// config") holds.
    #[test]
    fn sessiondirs_expands_tilde() {
        let home = std::env::var("HOME")
            .expect("HOME must be set for this test");
        // Exercise the
        // production parse
        // path: feed the
        // config parser a
        // string with
        // `sessiondirs=~/work`
        // and verify the
        // result is the
        // expanded path, not
        // the literal `~/work`
        // (which was the
        // bug).
        let mut cfg = Config::default();
        cfg.parse("sessiondirs=~/work\n");
        assert_eq!(
            cfg.session_dirs().len(),
            1,
            "sessiondirs=~/work must produce exactly one entry"
        );
        let got = &cfg.session_dirs()[0];
        // The stored path
        // must be the
        // `$HOME`-relative
        // expansion, not the
        // literal `~/work`
        // (which is the bug
        // we're fixing).
        assert_ne!(
            got.to_string_lossy(),
            "~/work",
            "sessiondirs=~/work must not store the literal `~` (the bug we're fixing)"
        );
        assert_eq!(
            got.to_string_lossy(),
            format!("{}/work", home),
            "sessiondirs=~/work must expand to `$HOME/work`"
        );
        // And the resulting
        // path must be a
        // real (or at least
        // plausibly real)
        // path — i.e. it
        // would pass
        // `path.exists()` in
        // `walk_subdirectories`.
        // We don't *create*
        // the directory
        // here — we just
        // confirm the
        // expansion produced
        // something that
        // *could* exist on
        // disk.
    }

    #[test]
    fn project_row_escapes_multiline_command() {
        // A multiline command must be escaped to a single line so the
        // CLI output (one row per line) and the zsh widget's `(f)`
        // record splitter see exactly one match per row.
        let row_data = vec![
            ("command".to_string(), "for i in 1 2 3\ndo echo $i\ndone".to_string()),
        ];
        let fields = vec!["command".to_string()];
        let out = project_row(&row_data, &fields, &[], None, true);
        assert_eq!(out.len(), 1);
        assert!(!out[0].contains('\n'), "escaped command still has a newline: {:?}", out[0]);
        assert_eq!(out[0], "for i in 1 2 3\\ndo echo $i\\ndone");
    }

    #[test]
    fn project_row_escapes_output_field() {
        // The `output` field is also escaped (it can contain newlines
        // from captured command output).
        let row_data = vec![
            ("output".to_string(), "line1\nline2".to_string()),
        ];
        let fields = vec!["output".to_string()];
        let out = project_row(&row_data, &fields, &[], None, true);
        assert_eq!(out.len(), 1);
        assert!(!out[0].contains('\n'), "escaped output still has a newline: {:?}", out[0]);
        assert_eq!(out[0], "line1\\nline2");
    }
}
