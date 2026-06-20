mod tui;
mod util;

use clap::Parser;
use rusqlite::{params, Connection};
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
fn config_path() -> Option<std::path::PathBuf> {
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
    for (action, spec) in bindings.iter() {
        let spec_str = tui::format_key_spec(spec);
        if let Some(prev) = seen_specs.get(&spec_str) {
            issues.push(ConfigIssue {
                level: ConfigIssueLevel::Warning,
                category: "key".into(),
                message: format!(
                    "{:?} is bound to the same key ({}) as {:?}; only the first action wins",
                    action,
                    spec_str,
                    prev
                ),
            });
        } else {
            seen_specs.insert(spec_str.clone(), action);
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
    use crate::tui::bindings::ALL_ACTIONS;
    let bindings = cfg.key_bindings();
    for a in ALL_ACTIONS {
        if bindings.is_unbound(*a) {
            let _ = writeln!(f, "  key.{} = none", a.config_key());
        } else if let Some(spec) = bindings.get(*a) {
            let _ = writeln!(
                f,
                "  key.{} = {}",
                a.config_key(),
                tui::format_key_spec(spec)
            );
        }
    }
}

/// Resolved configuration. Constructed by `Config::load`.
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
            "selection" => theme.selection = value,
            "badgefg" | "badge_fg" => theme.badge_fg = value,
            "listbg" | "list_bg" => theme.list_bg = value,
            "detailsbg" | "details_bg" => theme.details_bg = value,
            "inputbg" | "input_bg" => theme.input_bg = value,
            "statusbg" | "status_bg" => theme.status_bg = value,
            _ => {}
        }
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
        "INSERT INTO history (command, directory, session_id, exit_code)
         VALUES (?1, ?2, ?3, ?4)
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

            let start = find_command_line(&lines, command);

            if let Some(start) = start {
                let end = match max_lines {
                    Some(n) => (start + 1 + n).min(lines.len()),
                    // Unlimited: capture until the next prompt-like
                    // line, or end of file.
                    None => next_prompt_boundary(&lines, start + 1),
                };
                return Ok(lines[start..end].join("\n"));
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
        if trimmed.len() < 200
            && (trimmed.ends_with("$ ")
                || trimmed.ends_with("# ")
                || trimmed.ends_with("% ")
                || trimmed.ends_with("> ")
                || trimmed.ends_with("\u{276f} ")
                || trimmed.ends_with("] "))
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
            timestamp INTEGER DEFAULT (strftime('%s', 'now'))
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

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let conn = init_db()?;

    match args.command {
        Commands::Add {
            command,
            exit_code,
            comment,
        } => {
            let pwd = env::current_dir()?.to_string_lossy().into_owned();
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
            let (where_clause, params) =
                build_search_where_clause(query_ref, directory, session, exit_code.as_deref());
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
            let (where_clause, params) =
                build_search_where_clause(query_ref, directory, session, exit_code.as_deref());
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

            let pwd = env::current_dir()?.to_string_lossy().into_owned();
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
            let pwd = env::current_dir()?.to_string_lossy().into_owned();
            let session_id =
                env::var("SMART_HISTORY_SESSION").unwrap_or_else(|_| "default".to_string());
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
        Commands::Tui { mode, query } => {
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

    #[test]
    fn parse_capture_lines_handles_all_and_numbers() {
        assert_eq!(parse_capture_lines("ALL"), None);
        assert_eq!(parse_capture_lines("all"), None);
        assert_eq!(parse_capture_lines("20"), Some(20));
        assert_eq!(parse_capture_lines("  15  "), Some(15));
        assert_eq!(parse_capture_lines("not a number"), None);
    }
}
