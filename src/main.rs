#![allow(clippy::should_implement_trait)]
#![allow(clippy::empty_line_after_doc_comments)]
mod ag;
mod browser;
mod codegraph;
mod debounce;
mod files;
mod highlight;
mod jira;
mod llm;
mod multiplexer;
mod paperless;
mod ssh_config;
mod tui;
mod util;

use clap::{Parser, ValueEnum};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Controls how `smarthistory search` / `select` decorate the
/// matched substring on the `command` field with ANSI / bracket
/// markers. The default (`bold`) preserves the historical behavior;
/// `full` is the new opt-in that wraps the *rest* of the line in
/// dim, leaving only the matched prefix at full brightness — the
/// styling the line-editor dropdown widget needs to render an
/// at-a-glance history list. `off` is the no-decoration mode the
/// dropdown uses when the user has explicitly disabled styling
/// (it still inserts the literal command, with no markers, into
/// the zsh buffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub(crate) enum AnsiMode {
    /// No markers at all, even when stdout is a TTY. Used
    /// internally by the zsh dropdown widget when the user
    /// has explicitly opted out of styling, so a chosen
    /// command is inserted verbatim.
    Off,
    /// Wrap the matched prefix in `\x1b[1m...\x1b[0m` on a TTY
    /// (the historical default) and `[...]` on a pipe so
    /// downstream consumers (grep, awk, …) still see the
    /// match without ANSI noise.
    #[default]
    Bold,
    /// Like `bold`, but on a TTY also dim the *rest* of the
    /// command so only the matched substring stands out. The
    /// full emitted form is
    /// `\x1b[2m<prefix-before>\x1b[0m\x1b[1m<match>\x1b[0m\x1b[2m<suffix>\x1b[0m`,
    /// with a single reset at end of line so a downstream
    /// consumer (e.g. a less-able pager) doesn't bleed dim
    /// state into the next prompt. On a non-TTY the
    /// `bold`-style `[<match>]` is used instead, for the
    /// same pipe-safety reason.
    Full,
}

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
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15],
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
    /// Record a completed command in the history database.
    ///
    /// Called by the zsh precmd hook after every command; not
    /// normally invoked by hand.
    Add {
        /// The command text to record.
        command: String,
        /// The command's exit status.
        #[arg(short, long)]
        exit_code: i32,
        /// Optional comment attached to the history entry. Searchable
        /// from the TUI and CLI.
        #[arg(long)]
        comment: Option<String>,
    },
    /// Search history and print matching rows.
    Search {
        /// Substring/pattern to match against the command text.
        #[arg(index = 1)]
        query: Option<String>,
        /// Restrict results to this directory.
        #[arg(short, long)]
        directory: Option<String>,
        /// When set, restrict results to the current $SMART_HISTORY_SESSION.
        #[arg(short, long)]
        session: bool,
        /// Restrict results by exit status: `OK` or `ERROR`.
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
        /// Match only commands that START WITH `query` (a plain
        /// prefix match), instead of the default substring match
        /// anywhere in the command. Also drops the comment-substring
        /// side of the default OR entirely (see
        /// `build_filter_sql`'s doc comment) rather than prefix-
        /// matching it, since a comment matching mid-word is exactly
        /// the kind of unrelated hit this flag exists to rule out.
        /// Used by the live dropdown-completion widget in
        /// `init.zsh`, where a substring match on the whole command
        /// produces surprising results (e.g. typing "ls" matching
        /// `open "http://.../details"` because it contains "ls"
        /// somewhere inside the URL).
        #[arg(long)]
        prefix: bool,
        /// How to decorate the matched substring in the `command`
        /// field. Default `bold` (preserves historical behavior);
        /// `full` additionally dims the *rest* of the line so the
        /// match stands out — the styling the zsh dropdown widget
        /// reads. `off` is equivalent to `--no-highlight` and is
        /// kept as a separate flag so callers can opt into a
        /// three-way choice (off / bold / full) without two
        /// overlapping booleans. When both `--no-highlight` and
        /// `--ansi=off` are given, the explicit `--ansi` value
        /// wins; `--no-highlight` maps to `AnsiMode::Off`
        /// internally.
        #[arg(long, value_enum, default_value_t = AnsiMode::Bold)]
        ansi: AnsiMode,
    },
    /// Resolve a comment to its most recently used command.
    ///
    /// Used by the zsh comment-expansion widget: `text` is matched
    /// exactly (case-insensitively) against `command_comments.comment`;
    /// if multiple commands share that exact comment, the one most
    /// recently run wins. Prints the bare command with no formatting,
    /// or nothing if there's no match.
    Expand {
        /// The comment text to resolve.
        text: String,
    },
    /// Ask the LLM a question directly from the shell prompt — no
    /// TUI. Called by the `accept-line` widget in `init.zsh` when
    /// the typed line starts with the configured question prefix
    /// (`?` by default); not normally invoked by hand.
    ///
    /// Prints a colorized answer to stderr (so it's visible on the
    /// real terminal without being captured), tagged as coming
    /// from the LLM. If the answer suggests one or more shell
    /// commands, prompts the user to pick one (also on stderr,
    /// reading the choice from stdin) and prints ONLY the chosen
    /// command to stdout, so the zsh wrapper can stage it into the
    /// next prompt for review — it is never run automatically.
    Ask {
        /// The question text (everything after the `?` prefix).
        question: String,
    },
    /// Search history and print matching rows, like `search` but
    /// with a 1000-row default limit.
    ///
    /// Exists primarily as an integration hook for external
    /// pickers (e.g. `fzf`) rather than everyday interactive use.
    Select {
        /// Substring/pattern to match against the command text.
        #[arg(index = 1)]
        query: Option<String>,
        /// Restrict results to this directory.
        #[arg(short, long)]
        directory: Option<String>,
        /// When set, restrict results to the current $SMART_HISTORY_SESSION.
        #[arg(short, long)]
        session: bool,
        /// Restrict results by exit status: `OK` or `ERROR`.
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
        /// How to decorate the matched substring in the `command`
        /// field. See `search --ansi` for the three values.
        #[arg(long, value_enum, default_value_t = AnsiMode::Bold)]
        ansi: AnsiMode,
    },
    /// Launch the full-screen TUI picker.
    Tui {
        /// Starting scope: SESS, DIR, or GLOBAL.
        #[arg(short, long)]
        mode: Option<String>,
        /// Start the TUI directly in a specific prefix mode
        /// (e.g. `--prefix '*'` for panes, `--prefix '#'`
        /// for directories, `--prefix '@'` for notes,
        /// `--prefix '!'` for todos, `--prefix '-'` for
        /// JIRA, `--prefix '/'` for files, `--prefix '='`
        /// for LLM command generation, `--prefix '?'`
        /// for the question mode, `--prefix '+'`
        /// for output search, `--prefix '<'` for
        /// paperless document search). The prefix character is
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
        /// Launch the TUI locked into the file-completion picker,
        /// pre-filtered to `PATTERN` — a raw word straight from the
        /// shell buffer (e.g. `foo/a*`, `**/*.rs`), as produced by
        /// the new `globcomplete.enabled` zsh Tab widget. `PATTERN`
        /// is parsed into a walk root + basename glob by
        /// `crate::files::split_glob_root` (re-parsed on every
        /// keystroke as the user refines the filter, not just once
        /// at startup). Implies the files (`/`) prefix and locks
        /// mode-switching for the whole session: the query's
        /// leading prefix character can never change, `F1`
        /// (PickPrefix) and `Ctrl-]` (SmartOpen) are disabled, and
        /// Enter/Left/Right all return the marked (or selected)
        /// file path(s) — space-joined, shell-quoted — instead of
        /// staging an `$EDITOR` command. `Ctrl-A` marks every
        /// visible row. Mutually exclusive with `--prefix` (this
        /// flag already implies the files prefix).
        #[arg(long, value_name = "PATTERN", conflicts_with_all = ["prefix", "glob_complete_dir", "pid_complete"])]
        glob_complete: Option<String>,
        /// Same as `--glob-complete`, but locked into a DIRECTORY
        /// picker instead of a file picker — every behavior is
        /// identical (root-scoping, extra-word substring narrowing,
        /// mode-switching locked) except: only directory entries are
        /// shown (`walk_dir` already tags every matched entry `mode
        /// == "file"` or `mode == "directory"`; this just keeps the
        /// other kind), `Ctrl-A` is a no-op (cd-ing into more than
        /// one directory doesn't mean anything), and Enter always
        /// returns just the single selected directory, ignoring
        /// marks even if somehow set. Produced by the zsh widget
        /// when the command being completed is `cd` (see
        /// `_smarthistory_globcomplete_is_cd` in `init.zsh`).
        /// Mutually exclusive with `--prefix` and `--glob-complete`.
        #[arg(long, value_name = "PATTERN", conflicts_with_all = ["prefix", "pid_complete"])]
        glob_complete_dir: Option<String>,
        /// Launch the TUI locked into the process-completion picker
        /// (the processes `%` prefix, pre-filtered to `PATTERN` —
        /// matched against name/cmdline/cwd/exe exactly like typing
        /// it into `%` mode interactively; no glob syntax involved,
        /// `PATTERN` is free text). Multi-select IS available here
        /// (unlike `--glob-complete-dir`) — `Ctrl-A` marks every
        /// visible row, and Enter returns every marked (or just the
        /// selected) process's PID, space-joined, instead of opening
        /// the normal `%` mode signal-confirmation dialog. Produced
        /// by the zsh widget when the command being completed is
        /// `kill` (see `_smarthistory_globcomplete_is_kill` in
        /// `init.zsh`) — no glob-syntax trigger required, unlike
        /// `--glob-complete[-dir]`, since PIDs have no glob concept.
        /// Mutually exclusive with `--prefix`.
        #[arg(long, value_name = "PATTERN", conflicts_with = "prefix")]
        pid_complete: Option<String>,
        /// Override the base directory used to resolve relative
        /// walk roots — both the ordinary `/` mode's cwd-rooted
        /// walk and `--glob-complete`/`--glob-complete-dir`'s
        /// root-scoping. Defaults to the process's actual current
        /// directory. Unused by `--pid-complete` (the process picker
        /// has no filesystem walk to root).
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
        /// Execute the selected command directly (via `sh -c`)
        /// instead of printing it to stdout for the parent
        /// shell to eval. Use this when launching the TUI
        /// from outside a shell context (e.g. a herdr
        /// keybinding, a GUI launcher, or a systemd
        /// service) where there's no parent shell to
        /// `eval` the printed command. Implied automatically
        /// by `--create-note` — see its help text.
        #[arg(long)]
        exec: bool,
        /// Which detail pane layout to use on startup.
        /// Values: `both` (default, Details + Output Preview
        /// side-by-side), `details` (only the Details pane),
        /// `output` (only the Output Preview pane).
        /// The persisted session value takes precedence if
        /// the user has changed the layout interactively in
        /// a previous session; this flag overrides when the
        /// user explicitly passes it on the CLI.
        #[arg(long, value_name = "LAYOUT")]
        pane: Option<String>,
        /// Initial filter for the panes (`*`) prefix mode.
        /// Values: `all` (default), `windows` (show only
        /// live multiplexer panes), `hosts` (show only the
        /// hosts block), `sessions` (show only the sessions
        /// block). Only has an effect when the initial
        /// query is the `*` prefix (or when `--prefix '*'`
        /// is used).
        #[arg(long, value_name = "FILTER")]
        panes_filter: Option<String>,
        /// Height of the Details + Output Preview rows, in
        /// terminal lines (e.g. `--pane-height 14`). The
        /// historical default is 8 lines; `F11` / `Shift-F11`
        /// grow/shrink it by one line at a time in the TUI.
        /// This flag sets the starting height for this launch
        /// only (the value is NOT persisted to the session
        /// file, so a one-off `smarthistory tui --pane-height
        /// 20` from a herdr keybinding doesn't change the
        /// user's default pane height).
        #[arg(long, value_name = "HEIGHT")]
        pane_height: Option<String>,
        /// Starting query text (overridden by `--prefix`).
        #[arg(index = 1)]
        query: Option<String>,
        /// Open the two-field `create-note` dialog (Title + Content
        /// with inline completion for `@`-prefixed note links and
        /// `#`-prefixed tags) on startup. The dialog is the same one
        /// the `Action::CreateNote` key binding opens from inside
        /// the TUI; the flag is just a way to launch the TUI
        /// pre-configured for note creation (e.g. from a herdr
        /// keybinding or a shell alias).
        ///
        /// On `Ctrl-S` the TUI stages the same
        /// `note_search create-note ...` command line that the
        /// interactive path stages. `--create-note` implies `--exec`
        /// (runs the staged command itself via `sh -c`) so a bare
        /// `smarthistory tui --create-note` — typed directly, from a
        /// herdr keybinding, or a shell alias — actually creates the
        /// note. Without this default a caller would have to know to
        /// wrap the invocation in `eval "$(...)"`, since otherwise
        /// the staged command is only printed to stdout, never run.
        #[arg(long)]
        create_note: bool,
    },
    /// One-time import of history from an existing atuin database.
    ImportAtuin,
    /// Print every history entry (no filter).
    List {
        /// Comma-separated list of columns to return. Available: command,
        /// directory, session_id, exit_code, timestamp, id, comment, output,
        /// time, diff, base. May also be passed multiple times: -f command
        /// -f directory.
        #[arg(short, long, value_delimiter = ',')]
        fields: Option<Vec<String>>,
        /// Print as an aligned table instead of one row per line.
        #[arg(short, long)]
        table: bool,
    },
    /// Print the shell init snippet to `eval` from `.zshrc`/`.bashrc`.
    ///
    /// Sets up the preexec/precmd hooks and a set of line-editor
    /// widgets. `shell` must be `zsh` or `bash`. The `bash` snippet
    /// is a smaller subset — no live dropdown box (no Readline
    /// equivalent of zsh's POSTDISPLAY/region_highlight exists) —
    /// and needs bash >= 4.0 for the widgets specifically (the
    /// history-capture pipeline alone still works on older bash,
    /// e.g. macOS's stock 3.2.57).
    Init {
        /// `zsh` or `bash`.
        shell: String,
    },
    /// Read or validate the resolved configuration.
    ///
    /// Used by the zsh precmd hook to discover the tmux pane output
    /// directory. See the `Commands:` list below for the `get` /
    /// `check` / `list` sub-commands.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Time tracking: report on project sessions, or (in later
    /// phases) select the active project explicitly.
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Time tracking: record a file view/edit/creation event, called
    /// from an editor hook (a Vim autocmd, an LSP client, …). See the
    /// `viewed` / `modified` / `created` sub-commands.
    File {
        #[command(subcommand)]
        action: FileAction,
    },
    /// Delete entries matching the given filter.
    ///
    /// With no filter, deletes every entry in the database. Prompts
    /// for confirmation unless `--force` is passed.
    Clean {
        /// Substring/pattern to match against the command text.
        #[arg(index = 1)]
        query: Option<String>,
        /// Restrict deletion to this directory.
        #[arg(short, long)]
        directory: Option<String>,
        /// When set, restrict deletion to the current $SMART_HISTORY_SESSION.
        #[arg(short, long)]
        session: bool,
        /// Restrict deletion by exit status: `OK` or `ERROR`.
        #[arg(long)]
        exit_code: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(short, long)]
        force: bool,
    },
    /// Delete every history entry older than N days.
    ///
    /// Unlike `clean` (which filters by command/directory/exit-code),
    /// `prune` is a time-based bulk delete: every row whose
    /// `timestamp` is older than `now - N days` is removed, along
    /// with its `history_output` row and its `command_comments`
    /// entry (if no other history row shares the same command text
    /// after the prune).
    ///
    /// Example: `smarthistory prune 30` deletes everything older
    /// than 30 days.
    Prune {
        /// Number of days. Entries older than this are removed.
        /// Must be >= 0. A value of 0 removes everything (same as
        /// `clean --force`).
        #[arg(index = 1)]
        days: u32,
        /// Skip the confirmation prompt.
        #[arg(short, long)]
        force: bool,
    },
    /// Remove `session.<id>` entries (the panes-mode "Directories
    /// list") whose `.dir` no longer exists on disk.
    ///
    /// Reads every `session.<id>` entry from both
    /// `~/.config/smarthistory/config` and
    /// `~/.config/smarthistory/sessions`, checks whether its `.dir`
    /// still exists, and removes the whole entry (name, `.dir`,
    /// `.exec`, `.startup_command` lines) from whichever file(s) it
    /// lives in when it doesn't. Entries with no `.dir` set are left
    /// alone — there's nothing to check for them. `host.<id>`
    /// entries are untouched (a host is a remote target, not a local
    /// directory).
    PruneDirectories {
        /// Skip the confirmation prompt.
        #[arg(short, long)]
        force: bool,
    },
    /// Check the health of every TUI prefix mode's dependencies.
    ///
    /// Verifies each mode's (notes, todos, tags, codegraph, files,
    /// ag, LLM, JIRA, directories, panes) external dependencies are
    /// configured and reachable. When `--prefix` is given only that
    /// mode is checked.
    ///
    /// Exit code: 0 = all ok, 1 = warnings, 2 = errors.
    Check {
        /// Only check the mode with this prefix character
        /// (e.g. `--prefix @` for notes, `--prefix &` for
        /// codegraph). When omitted, every prefix mode is
        /// checked.
        #[arg(long, value_name = "PREFIX")]
        prefix: Option<String>,
    },
    /// Suggest the most probable next command after the given one.
    ///
    /// Candidates are drawn from the global history and ordered by
    /// frequency (then lexicographically for ties). Used by the
    /// Ctrl-S line-editor widget to suggest likely next steps.
    Next {
        /// The command whose successors to look up.
        command: String,
        /// Maximum number of candidates to return. Default 5.
        #[arg(short, long)]
        limit: Option<usize>,
    },
    /// Re-run the connection command for the current tmux session or
    /// herdr workspace, if it was created from a configured
    /// `session.<id>`/`host.<id>` entry.
    ///
    /// For a fresh pane/window opened directly in tmux or herdr
    /// (e.g. `Ctrl-b c`, not through smarthistory's own `*` panes
    /// picker) — the session/workspace itself already carries the
    /// connection, but a newly-created pane inside it starts a plain
    /// local shell with no automatic reconnect. Run this inside that
    /// new pane to reconnect: it reads the current tmux session name
    /// (or herdr workspace label) and looks it up against the same
    /// `session.<id>`/`host.<id>` config that would have created it
    /// in the first place — no separate registration step needed,
    /// since the session/workspace is already named after the
    /// config entry that created it.
    ///
    /// A `host.<id>` match re-runs just the `ssh` connection — its
    /// optional `.exec` (meant to be typed into the remote shell
    /// after connecting, not run locally) isn't replayed here, since
    /// that injection only works through the multiplexer backend's
    /// own pane-focused API, not a plain foreground child process. A
    /// `session.<id>` match re-runs its `.exec` directly (a normal
    /// local command, no such mismatch).
    PaneExec,
    /// Open the interactive `create-note` dialog directly, without
    /// having to launch the TUI and press the `Action::CreateNote`
    /// keybinding first.
    ///
    /// Shorthand for `smarthistory tui --create-note`, plus
    /// `--title`/`--content` to pre-fill the dialog's fields (in
    /// place of the interactive path's usual "prefill from the
    /// currently selected row", since there's no TUI selection yet
    /// at this point). The dialog itself — completion, `Ctrl-S` to
    /// save, `Ctrl-O` to save and open in `$EDITOR` — is exactly the
    /// one `Action::CreateNote` opens from inside the TUI; this is
    /// only a different way to reach it.
    CreateNote {
        /// Pre-fill the dialog's Title field.
        #[arg(long)]
        title: Option<String>,
        /// Pre-fill the dialog's Content field.
        #[arg(long)]
        content: Option<String>,
    },
    /// Run a command and capture its output alongside the history entry.
    ///
    /// Captures up to 20 lines of combined stdout/stderr and stores
    /// them in the database.
    Capture {
        /// The command to run (pass remaining args verbatim).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Capture a command's output from a tmux pane log file.
    ///
    /// Extracts the command line and the following output (up to 20
    /// lines) and stores it in the database. Called automatically by
    /// the zsh precmd hook when running inside tmux; not normally
    /// invoked by hand.
    CaptureTmux {
        /// The command that was executed (as recorded by zsh preexec).
        command: String,
        /// Path to the tmux pane log file.
        file: PathBuf,
        /// The command's exit status.
        #[arg(short, long)]
        exit_code: i32,
    },
    /// Capture a command's output from a herdr pane's scrollback.
    ///
    /// Reads via `herdr pane read`, extracts the command line and
    /// the following output, and stores it in the database. Called
    /// automatically by the zsh precmd hook when running inside a
    /// herdr workspace pane; not normally invoked by hand.
    CaptureHerdr {
        /// The command that was executed (as recorded by zsh preexec).
        command: String,
        /// The command's exit status.
        #[arg(short, long)]
        exit_code: i32,
    },
    /// Export all history data to a JSON file.
    ///
    /// The file contains every history entry, command comment, and
    /// captured output, so a complete round-trip import is possible.
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
    /// Import history data from a JSON file created by `export`.
    ///
    /// Existing entries with the same (command, directory,
    /// session_id) are updated; new entries are inserted.
    Import {
        /// Path to the input JSON file.
        filename: PathBuf,
    },
    /// One-time migration: shorten every stored directory to `~` form.
    ///
    /// Rewrites every `directory` value in the database to its
    /// `~`-shortened form (where the directory is under `$HOME` or
    /// any `homemap=...` entry in the config file).
    ///
    /// `smarthistory add` (the preexec hook entry point) always
    /// records the kernel-canonical absolute path. For the
    /// directories view and the staged `tmux new-session` command,
    /// the user wants the short `~` form. `smarthistory update`
    /// updates existing rows in place (preserving `id`/`timestamp`);
    /// running it twice is a no-op. New rows added after the
    /// migration are stored `~`-shortened from the start (see
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
    ///
    /// Used by the zsh precmd hook to discover the tmux pane output
    /// directory.
    Get {
        /// One of: `tmuxpaneoutputdir`, `ignorecapture`, `capturelines`,
        /// `palette`.
        key: String,
    },
    /// Validate the config file, printing a human-readable report.
    ///
    /// Checks `~/.config/smarthistory/config` and exits non-zero if
    /// any problems are found.
    Check,
    /// Print every known configuration key with its resolved value.
    List,
}

/// Sub-commands of `smarthistory project`. `Report` rolls up a
/// day's tracked time per project (directories, commands, notes —
/// websites are added in a later phase). `Select` (added in a
/// later phase) sets the explicit "current project" fallback.
#[derive(clap::Subcommand, Debug)]
enum ProjectAction {
    /// Print a per-project time-tracking report for a single day.
    ///
    /// Joins `project_sessions` against `history` (directories,
    /// commands and their derived durations) and, when
    /// `notes.database` is configured, notes created during a
    /// tracked window. Commands run with no project active at all
    /// are grouped under "untracked".
    Report {
        /// Day to report on: `YYYY-MM-DD`, `today`, or `yesterday`.
        /// Defaults to `today`.
        #[arg(long)]
        day: Option<String>,
        /// Restrict the report to a single project slug. Omit to
        /// report every project active that day, plus "untracked".
        #[arg(long)]
        project: Option<String>,
        /// Only list commands whose derived active duration is at
        /// least this many seconds. Does not affect the per-project
        /// total or the directories breakdown. Defaults to 0 (list
        /// everything).
        #[arg(long)]
        min_duration: Option<i64>,
    },
    /// Set the explicit "current project" fallback and switch the
    /// active `project_sessions` row to it immediately.
    ///
    /// Staged by the `.` prefix-mode picker as the shell command run
    /// after a Project note is selected; not normally invoked by
    /// hand. Unlike a directory-detected switch (`end_reason =
    /// "directory_change"`), an explicit switch closes any open
    /// session with `end_reason = "switch"` — though when the
    /// newly-resolved slug is the same as the one already open,
    /// `switch_project` treats it as "reaffirming the current
    /// project" and stays a no-op regardless of the forced reason.
    Select {
        /// The project's slug (`crate::util::slugify` of its note's
        /// filename stem — the same identity `project.<slug>.dir`
        /// and the report's `--project` filter use).
        slug: String,
    },
    /// Print the project the current directory resolves to, using
    /// the exact same priority `smarthistory add` uses: an in-repo
    /// `.smarthistory-project` marker file, then the longest
    /// `project.<slug>.dir` prefix match, then the last explicit
    /// `smarthistory project select` (i.e. `.`-mode) choice, falling
    /// back to "no project" when none apply. Prints just the slug on
    /// stdout (no trailing decoration) so it's easy to embed in a
    /// shell prompt or script; prints nothing (exit code 1) when
    /// unresolved.
    Current,
    /// Toggle project tracking off/on: a manual "stop attributing
    /// time to any project" switch, e.g. for a lunch break or a
    /// meeting where you don't want the current directory's project
    /// to keep accruing time. First call pauses — closes the open
    /// `project_sessions` row (if any, `end_reason = "paused"`) and
    /// remembers whatever project was active, so directory/marker
    /// resolution is fully suppressed (not just the session that
    /// happened to be open) until you resume, even if you `cd`
    /// around a directory-bound project's tree in the meantime.
    /// Second call resumes — restores that exact remembered project
    /// (`end_reason = "switch"` on the reopened session), not
    /// whatever the current directory would resolve to on its own.
    /// Prints which state it switched to.
    Pause,
    /// Print the files viewed, modified, and created since the
    /// currently-open project session started — a quick "what have I
    /// touched right now" view, unlike `report` (which is a whole
    /// calendar day, across every project). Reads the open
    /// `project_sessions` row directly (`end_ts IS NULL`) rather than
    /// re-resolving the project from the current directory, so it
    /// reflects the session `smarthistory add`/`file` actually has
    /// open right now, not what the cwd would resolve to if a
    /// directory change hasn't triggered a switch yet. Prints "no
    /// active project session" (exit code 1) when nothing is open —
    /// including while paused, since pausing closes the open
    /// session.
    Files,
}

/// Sub-commands of `smarthistory file`. Each records one row in
/// `file_events` for the given path, attributed to a project using
/// the exact same resolution `smarthistory add` uses for directories
/// (`resolve_current_project`) — but resolved from the FILE's own
/// directory, not the caller's cwd, since an editor hook usually
/// runs with the editor process's cwd, not necessarily the file's
/// directory (a background LSP server, a globally-running editor
/// instance with files open from several projects, etc.).
#[derive(clap::Subcommand, Debug)]
enum FileAction {
    /// Record that `path` was viewed (opened, read) in an editor.
    Viewed {
        /// Path to the file. Relative paths are resolved against the
        /// current working directory and canonicalized; a path that
        /// no longer exists on disk is stored as given (best effort
        /// — a file viewed and then deleted before the hook fires is
        /// an edge case, not a reason to drop the event).
        path: String,
    },
    /// Record that `path` was modified (saved with changes) in an
    /// editor.
    Modified {
        /// See `Viewed::path`.
        path: String,
    },
    /// Record that `path` was newly created in an editor.
    Created {
        /// See `Viewed::path`.
        path: String,
    },
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

/// Resolve the path to the optional `~/.config/smarthistory/hosts`
/// file — `host.<id>.*` entries can live here instead of (or in
/// addition to) the main config file. Only read by
/// [`Config::load_tui`], not the plain [`Config::load`] every CLI
/// subcommand uses: host/session data is exclusively a TUI (`*`
/// panes mode) concern, so keeping it out of `load()` avoids two
/// extra file-existence checks on the hot path the shell hook fires
/// on every prompt (`smarthistory add` / `search` / `capture-*`).
/// Returns `None` only when `$HOME` is unset, same as
/// [`config_path`].
pub fn hosts_path() -> Option<std::path::PathBuf> {
    let home = env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("smarthistory")
            .join("hosts"),
    )
}

/// Resolve the path to the optional `~/.config/smarthistory/sessions`
/// file — `session.<id>.*` entries can live here instead of (or in
/// addition to) the main config file. Same "TUI-only, not the CLI
/// hot path" rationale as [`hosts_path`].
pub fn sessions_path() -> Option<std::path::PathBuf> {
    let home = env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("smarthistory")
            .join("sessions"),
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
        && let Ok(home) = env::var("HOME")
    {
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

/// Default `fileviewcommands` set — commands whose first non-flag
/// argument is recorded as a `viewed` file event when the config
/// file is absent or does not set `fileviewcommands`.
const DEFAULT_FILE_VIEW_COMMANDS: &[&str] = &["less", "more", "bat", "tail", "head"];

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
        && let Ok(contents) = std::fs::read_to_string(p)
    {
        let known: std::collections::HashSet<&'static str> =
            ALL_ACTIONS.iter().map(|a| a.config_key()).collect();
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
                && !name.is_empty()
                && !known.contains(name)
            {
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

    // --- Unknown prefix.* mode names ---
    if let Some(ref p) = path
        && p.is_file()
        && let Ok(contents) = std::fs::read_to_string(p)
    {
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
            if let Some(name) = k.strip_prefix("prefix.")
                && !name.is_empty()
                && !Config::KNOWN_PREFIX_NAMES.contains(&name.to_ascii_lowercase().as_str())
            {
                let hint = match name.to_ascii_lowercase().as_str() {
                    "fuzzy" | "regex" => "fuzzy/regex aren't separate prefix modes — they're \
                         match algorithms, toggled with Ctrl-F for whatever mode is active"
                        .to_string(),
                    _ => format!("did you mean one of {:?}?", Config::KNOWN_PREFIX_NAMES),
                };
                issues.push(ConfigIssue {
                    level: ConfigIssueLevel::Error,
                    category: "prefix".into(),
                    message: format!("unknown prefix mode {:?}: {}", name, hint),
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

    // --- project.idlethreshold validation ---
    // Bad input here is already caught (and defaulted) at parse
    // time with an `eprintln!` warning — see `parse_multi`'s
    // `project.` branch — but `Config::load()` gives no way to
    // distinguish "the file set a bad value, silently kept the old
    // one" from "the file never mentioned this key at all". Re-scan
    // the raw file text (same idiom the `key.*`/`prefix.*` checks
    // above use) so `config check` can surface it as a real error
    // rather than staying silent.
    if let Some(ref p) = path
        && p.is_file()
        && let Ok(contents) = std::fs::read_to_string(p)
    {
        for raw in contents.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            if k.trim() != "project.idlethreshold" {
                continue;
            }
            let v = v.trim().trim_matches('"').trim();
            match v.parse::<i64>() {
                Ok(n) if n > 0 => {}
                _ => issues.push(ConfigIssue {
                    level: ConfigIssueLevel::Error,
                    category: "project".into(),
                    message: format!(
                        "project.idlethreshold = {:?} is not a positive integer; the previous value was kept at runtime, but this should be fixed",
                        v
                    ),
                }),
            }
        }
    }

    // --- project.<slug>.dir / jiralabel.<slug>.match / weburl.<slug>.match
    //     vs. `type: project` notes cross-check ---
    // Both directions are legitimate on their own (a directory- or
    // label-only project with no note yet; a note tracked purely by
    // explicit `.`-mode selection with no directory/label binding) —
    // see the time-tracking plan's design notes — so both surface as
    // warnings, not errors. Skipped entirely when `notes.database`
    // isn't configured (nothing to cross-check against).
    {
        let mut config_slugs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (slug, _) in &cfg.projects {
            config_slugs.insert(slug.clone());
        }
        for (slug, _) in &cfg.jira_labels {
            config_slugs.insert(slug.clone());
        }
        for (slug, _) in &cfg.web_urls {
            config_slugs.insert(slug.clone());
        }
        if let Some(db_path) = cfg.notes_database() {
            let service = note_search::database_service::DatabaseService::new(
                &db_path.display().to_string(),
            );
            let criteria = note_search::SearchCriteria {
                list_only: true,
                query_expr: Some(note_search::QueryExpr::Attribute {
                    key: "type".to_string(),
                    value: Some("project".to_string()),
                }),
                ..Default::default()
            };
            if let Ok(notes) = service.search_notes(&criteria) {
                let note_slugs: std::collections::BTreeSet<String> = notes
                    .iter()
                    .filter_map(|n| {
                        std::path::Path::new(&n.filename)
                            .file_stem()
                            .and_then(|s| s.to_str())
                    })
                    .map(|stem| crate::util::slugify(stem, "project"))
                    .collect();
                for slug in &config_slugs {
                    if !note_slugs.contains(slug) {
                        issues.push(ConfigIssue {
                            level: ConfigIssueLevel::Warning,
                            category: "project".into(),
                            message: format!(
                                "project.{slug}.dir / jiralabel.{slug}.match / weburl.{slug}.match is configured but no `type: project` note has a matching slug (fine for directory/label/URL-only tracking; check for a typo otherwise)"
                            ),
                        });
                    }
                }
                for slug in &note_slugs {
                    if !config_slugs.contains(slug) {
                        issues.push(ConfigIssue {
                            level: ConfigIssueLevel::Warning,
                            category: "project".into(),
                            message: format!(
                                "`type: project` note slug {slug:?} has no project.{slug}.dir / jiralabel.{slug}.match / weburl.{slug}.match entry (fine for explicit-selection-only tracking; check for a typo otherwise)"
                            ),
                        });
                    }
                }
            }
        }
    }

    // --- jiralabel.* configured without JIRA credentials ---
    if !cfg.jira_labels.is_empty() && crate::jira::JiraConfig::from_env().is_none() {
        issues.push(ConfigIssue {
            level: ConfigIssueLevel::Warning,
            category: "jiralabel".into(),
            message: "jiralabel.<slug>.match is configured but JIRA_SERVER/JIRA_API_TOKEN are not set; the JIRA-label resolution tier will be skipped for every report".into(),
        });
    }

    ConfigReport {
        cfg,
        issues,
        file_missing,
    }
}

/// Print every known configuration key with its resolved value.
fn print_config_list<W: std::fmt::Write>(f: &mut W, cfg: &Config) {
    let _ = writeln!(
        f,
        "  tmuxpaneoutputdir = {}",
        cfg.tmux_pane_output_dir.display()
    );
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
    let mut view_cmds: Vec<&String> = cfg.file_view_commands.iter().collect();
    view_cmds.sort();
    let _ = writeln!(
        f,
        "  fileviewcommands = {}",
        view_cmds
            .iter()
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
    let _ = writeln!(
        f,
        "  dropdown.enabled = {}",
        if cfg.dropdown_enabled { "on" } else { "off" }
    );
    let _ = writeln!(f, "  dropdown.limit = {}", cfg.dropdown_limit);
    let _ = writeln!(f, "  dropdown.minchars = {}", cfg.dropdown_min_chars);
    let _ = writeln!(f, "  segments.minwords = {}", cfg.segments_min_words);
    let _ = writeln!(
        f,
        "  dropdown.highlight = {}",
        if cfg.dropdown_highlight { "on" } else { "off" }
    );
    let _ = writeln!(f, "  dropdown.matchmode = {}", cfg.dropdown_matchmode);
    let _ = writeln!(
        f,
        "  commentexpand.enabled = {}",
        if cfg.commentexpand_enabled { "on" } else { "off" }
    );
    let _ = writeln!(
        f,
        "  globcomplete.enabled = {}",
        if cfg.globcomplete_enabled { "on" } else { "off" }
    );
    let _ = writeln!(f, "  zsh.mode = {}", cfg.zsh_default_mode);
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
    /// Prefix for general question mode (default `?`).
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
    /// `/`). Lists every file in the current
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
    /// Prefix for the ag content-search mode
    /// (default `,`). Searches the current
    /// directory tree using `ag` (The Silver
    /// Searcher). Tokens containing `*` are
    /// treated as file-pattern globs (`-G`)
    /// and restrict which files are searched.
    /// Selecting a row opens the file in
    /// `$EDITOR` at the matching line.
    pub ag: char,
    /// Prefix for the CodeGraph symbol-search
    /// mode (default `&`). Searches the local
    /// `.codegraph/codegraph.db` index by
    /// symbol name (FTS5) and lists matching
    /// functions/methods/classes. The selected
    /// row's details pane shows the source
    /// context plus the symbol's callers and
    /// callees (edges with `kind='calls'`).
    /// Selecting a row opens the file in
    /// `$EDITOR` at `start_line`. When no
    /// `.codegraph/` index exists the `$`
    /// (tags) mode falls back to this index,
    /// so a repo without a `TAGS` file still
    /// has symbol navigation as long as
    /// CodeGraph has indexed it.
    pub codegraph: char,
    pub jira: char,
    /// Prefix for the segment-search mode (default `:`).
    /// Searches `note_search`'s `segments` table — a segment is
    /// one markdown header (level 1-4) plus everything below it
    /// up to the next level-<=4 header — finer-grained than
    /// `notes` (`@`), which searches whole files. Selecting a row
    /// opens the file in `$EDITOR` at the segment's start line,
    /// same as `tags` / `codegraph`.
    ///
    /// Was called `elements` (`note_search`'s prior "element
    /// search" feature) before upstream's segment redesign;
    /// `prefix.elements=` in an existing config file is still
    /// honored as an alias for `prefix.segments=` — see
    /// `assign_prefix`.
    pub segments: char,
    /// Prefix for the similar/phrase-search mode (default `"`).
    /// Same `segments` table and Tab-completion namespace as
    /// `segments` (`:`), but the whole typed body is one literal
    /// phrase — embedded via `note_search::embeddings::embed_text`
    /// (a local Ollama call) and ranked against every segment's
    /// stored embedding by cosine similarity, rather than parsed
    /// as a query DSL. Requires a `note_search` build with segment-
    /// embeddings support and a reachable local Ollama instance.
    pub similar: char,
    /// Prefix for the paperless-ngx document-search mode
    /// (default `<`). Searches a configured Paperless-ngx v3
    /// backend by title (bare words), tag (`#TAG`), or
    /// correspondent/author (`@AUTHOR`). Requires
    /// `paperless.url` and `paperless.token` in the config
    /// file.
    pub paperless: char,
    /// Prefix for the browser bookmarks + history mode
    /// (default `^`). Reads bookmarks and visited-URL history
    /// directly from locally-installed browsers' profile files
    /// (Chrome, Firefox, Safari) and merges them into one list,
    /// tagged `bookmark` / `history` so the user can narrow with
    /// those words. Selecting a row opens the URL in the system
    /// browser. Configured via zero or more `browser.<id>.type`
    /// / `browser.<id>.profile` pairs in the config file;
    /// auto-detects Chrome / Firefox / Safari at their platform-
    /// default locations when none are configured.
    pub browser: char,
    /// Prefix for the zoxide directory-jump mode (default `~`).
    /// Lists every directory in the local `zoxide` database (`zoxide
    /// query -l`, highest frecency score first), filtered by the
    /// typed body the same way every other mode filters. Selecting a
    /// row creates a new tmux session / herdr workspace rooted
    /// there — the exact same staging as the `#` Directories mode's
    /// "unmarked row" path (`App::stage_directory_selection`),
    /// including the `T`-marked "jump to an already-active pane
    /// there instead" behavior. Requires the `zoxide` binary on
    /// `$PATH`; a directory whose entry no longer exists on disk is
    /// skipped.
    pub zoxide: char,
    /// Prefix for the processes mode (default `%`). Lists every
    /// running OS process (macOS + Linux, via the `sysinfo` crate),
    /// filtered by the typed body against the process name/cmdline,
    /// cwd, and executable path. Selecting a row opens a
    /// confirmation dialog to send it a signal — defaults to
    /// SIGTERM, Tab/Shift-Tab cycle to SIGKILL/SIGHUP/SIGINT before
    /// confirming with `y`. Shows processes regardless of owner;
    /// signaling one the user doesn't own fails with a status-line
    /// message rather than crashing.
    pub processes: char,
    /// Prefix for the pass password-manager mode (default `)`).
    /// Lists every entry in `$PASSWORD_STORE_DIR` (defaults to
    /// `~/.password-store`). Selecting a row stages
    /// `pass show --clip <entry>` as the shell command, which copies
    /// the first line of the entry (the password) to the clipboard
    /// via `pass`'s built-in clipboard support.
    pub pass: char,
    /// Prefix for the project picker mode (default `.`). Lists
    /// `type: project` frontmatter notes from `notes.database`.
    /// Selecting a row stages `smarthistory project select <slug>`
    /// as the shell command, setting the explicit "current project"
    /// fallback used by time tracking's directory-based resolution
    /// (see `resolve_current_project`).
    pub project_pick: char,
    /// Prefix for the meta-prefix mode (default `'`). Not a search
    /// mode itself — typing `'` then a partial mode name (e.g.
    /// `'jir`) and pressing Tab expands to that mode's real prefix
    /// character, discarding the typed `'<name>` text entirely. A
    /// unique name match activates immediately; an ambiguous match
    /// (including the bare `'` + Tab case) opens the same picker
    /// overlay as `PickPrefix` (F1), pre-filtered to the matching
    /// names. See `App::meta_tab_complete_at_cursor`.
    pub meta: char,
}

impl Default for QueryPrefixes {
    fn default() -> Self {
        QueryPrefixes {
            output: '+',
            llm: '=',
            question: '?',
            notes: '@',
            todo: '!',
            directories: '#',
            panes: '*',
            files: '/',
            tags: '$',
            ag: ',',
            codegraph: '&',
            jira: '-',
            segments: ':',
            similar: '"',
            paperless: '<',
            browser: '^',
            zoxide: '~',
            processes: '%',
            pass: ')',
            project_pick: '.',
            meta: '\'',
        }
    }
}

impl QueryPrefixes {
    /// Every prefix character currently assigned, in no particular
    /// order. Single source of truth for "is this character one of
    /// the known prefixes" — three call sites in `src/tui.rs`
    /// (`query_mode_char`'s dispatch, `apply_prefix`'s
    /// strip-old-prefix check, and `open_prefix_picker`'s row
    /// preselection) used to each maintain their own hand-written
    /// list of fields, and drifted out of sync with each other
    /// (missing `paperless` in two of the three) as fields were
    /// added over time. Add a new prefix field here too when one is
    /// added to the struct.
    pub(crate) fn all_chars(&self) -> [char; 21] {
        [
            self.output,
            self.llm,
            self.question,
            self.notes,
            self.todo,
            self.directories,
            self.panes,
            self.files,
            self.tags,
            self.ag,
            self.codegraph,
            self.jira,
            self.segments,
            self.similar,
            self.paperless,
            self.browser,
            self.zoxide,
            self.processes,
            self.pass,
            self.project_pick,
            self.meta,
        ]
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

/// A directory-to-project binding from the config file.
/// Syntax: `project.<slug>.dir = "~/path"`. `<slug>` is the same
/// identifier used to derive the binding's owning note (see
/// `crate::util::slugify`, same convention `session.<key>`/
/// `host.<key>` use) — there is no separate display-name field here,
/// unlike `SessionDef`/`HostDef`, since a project's name lives on its
/// `type: project` note, not in this config entry. `dir` is
/// tilde-expanded and matched by longest-prefix against the current
/// working directory to resolve "which project is this shell in"
/// (`resolve_project_dir`).
#[derive(Debug, Clone)]
struct ProjectDef {
    dir: String,
}

/// Display-only clustering for the `project report` website section:
/// `weburlgroup.<name>.match = "<substring>"` / `weburlgroup.<name>.label
/// = "<label>"`. `<name>` is an arbitrary opaque key (like
/// `session.<key>`/`host.<key>`, order doesn't matter here); `match`
/// is a plain substring tested against a visited URL's host+path, and
/// `label` is what the report prints instead of the individual URLs
/// when at least one matches. Independent of project *assignment*
/// (`weburl.<slug>.match` / `jiralabel.<slug>.match`) — a URL's
/// project and its display cluster are separate concerns, and a
/// visit can belong to a project while also being clustered under an
/// unrelated group label (e.g. every JIRA/docs URL bucketed together
/// regardless of which project the ticket belongs to).
#[derive(Debug, Clone, Default)]
struct WebUrlGroupDef {
    match_pattern: String,
    label: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Directory containing per-pane tmux output log files.
    tmux_pane_output_dir: std::path::PathBuf,
    /// Commands whose output is never captured. Empty means capture
    /// everything.
    ignore_capture: std::collections::HashSet<String>,
    /// Commands whose first non-flag argument is recorded as a
    /// `viewed` file event (`file_events`, same table
    /// `smarthistory file viewed` writes to) — a `less`/`cat`-style
    /// pager or file dump is itself evidence the file was viewed,
    /// with no editor hook required. Matched against the command's
    /// literal first whitespace token (same convention
    /// `ignore_capture` uses — no `$PATH` resolution or basename
    /// stripping, so `/usr/bin/less` won't match a bare `less`
    /// entry). See `first_non_flag_argument` for how the "file"
    /// argument itself is picked out from the rest of the command.
    file_view_commands: std::collections::HashSet<String>,
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
    /// The built-in theme to use when the terminal is
    /// detected as LIGHT (i.e. light background). Set via
    /// `theme.light=<slug>` in the config file; the slug
    /// is the same identifier the theme picker shows
    /// (kebab-case, e.g. `gruvbox-light`, `catppuccin-latte`,
    /// `leuven`). The user's choice is independent of
    /// `theme.dark` — you can set one without the other
    /// and the unset slot falls back to the OTHER slot
    /// at runtime, so a single `theme.light=gruvbox-light`
    /// line is enough to opt into a light theme on
    /// light terminals while keeping the existing dark
    /// theme for dark terminals. The theme picker
    /// writes to the active scheme's slot on commit, so
    /// choosing a new theme in a light terminal
    /// automatically updates `theme.light` (the
    /// `theme.dark` value stays untouched).
    theme_light: Option<String>,
    /// The built-in theme to use when the terminal is
    /// detected as DARK. Same semantics as
    /// `theme_light`. Set via `theme.dark=<slug>`.
    theme_dark: Option<String>,
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
    /// Optional Paperless-ngx configuration for the `<...` TUI
    /// query mode. `None` means the feature is disabled — the
    /// `paperless` module returns `PaperlessError::NotConfigured`
    /// and the TUI surfaces a clear status message. Set via
    /// `paperless.url` + `paperless.token` in the config file
    /// (both required; a half-configured pair disables the
    /// feature with a stderr warning, same policy as `ollama.*`).
    paperless: Option<paperless::PaperlessConfig>,
    /// Whether the live-as-you-type zsh dropdown suggestion menu
    /// (`init.zsh`'s self-insert-triggered `POSTDISPLAY` overlay)
    /// is enabled. Default `false` — a bigger behavior change than
    /// existing opt-in features (it hooks every keystroke), so it
    /// defaults off. Set via `dropdown.enabled=on|off`.
    dropdown_enabled: bool,
    /// Max number of candidates the dropdown shows. Read by
    /// `init.zsh` via `smarthistory config get dropdown.limit` at
    /// shell-init time and passed as `--limit` to the same
    /// `smarthistory search` call Up/Down already uses. Set via
    /// `dropdown.limit=<N>`.
    dropdown_limit: usize,
    /// Minimum number of typed characters (after the cursor is at
    /// end-of-buffer) before the dropdown appears. Avoids showing
    /// a huge, low-signal candidate list on an empty or 1-char
    /// buffer. Set via `dropdown.minchars=<N>`.
    dropdown_min_chars: usize,
    /// Minimum word count a segment's body (its text minus its own
    /// header line) must have for `:` (segment search) to keep it —
    /// segments at or under this are dropped as noise (a heading
    /// with little or nothing under it). `0` disables the filter.
    /// Set via `segments.minwords=<N>`.
    segments_min_words: usize,
    /// Whether the dropdown widget syntax-highlights each candidate
    /// (via `bat`, plus a self-checked green/red for the first
    /// word's alias/function/builtin/command validity) instead of
    /// plain text. Default `false`: it adds a `bat` subprocess call
    /// per dropdown render, on top of the `smarthistory search` call
    /// every keystroke already does. Silently stays off if `bat`
    /// isn't on `$PATH`, even when this is `true`. Set via
    /// `dropdown.highlight=on|off`.
    dropdown_highlight: bool,
    /// The dropdown widget's match mode at shell-init time — one of
    /// `prefix` (only commands STARTING WITH what's typed; the
    /// historical, hardcoded behavior — see `--prefix` on
    /// `Commands::Search` for why: a plain substring match made "ls"
    /// match `open "http://.../details"` because it contains "ls"
    /// inside the URL) or `substring` (matches anywhere in the
    /// command, same as the Up/Down history-walk widget and the TUI's
    /// own search). Read by `init.zsh` via `smarthistory config get
    /// dropdown.matchmode` and assigned to `_smarthistory_matchmode`,
    /// the same variable `Ctrl-t` (`_smarthistory_cycle_matchmode`)
    /// toggles at runtime — this only changes what a brand-new shell
    /// starts on. Default `prefix`, matching the historical hardcoded
    /// behavior. Set via `dropdown.matchmode=prefix|substring`.
    dropdown_matchmode: String,
    /// Whether the space-triggered comment-expansion zsh widget
    /// (typing a comment's text at the start of the line, then a
    /// space, expands it to the most recently used command carrying
    /// that comment) is enabled. Default `false`, opt-in like
    /// `dropdown.enabled` above. Set via
    /// `commentexpand.enabled=on|off`.
    commentexpand_enabled: bool,
    /// Whether the glob-triggered Tab file-completion widget
    /// (`init.zsh`'s `_smarthistory_globcomplete_accept`) is
    /// enabled. Default `false`, same opt-in reasoning as
    /// `dropdown.enabled`/`commentexpand.enabled` above — it hooks
    /// Tab. When on, pressing Tab on a word containing shell-glob
    /// syntax (`* ? [ ]`) launches `smarthistory tui
    /// --glob-complete <word>` (a locked file-completion picker)
    /// instead of running normal zsh completion; anything else
    /// still falls through unchanged. Set via
    /// `globcomplete.enabled=on|off`.
    globcomplete_enabled: bool,
    /// The zsh widgets' search scope at shell-init time — one of
    /// `sess` (current `$SMART_HISTORY_SESSION` only), `dir`
    /// (current working directory only), or `global` (no scope
    /// filter). Read by `init.zsh` via `smarthistory config get
    /// zsh.mode` and assigned to `_smarthistory_mode`, the same
    /// variable `Ctrl-g` (`_smarthistory_cycle_mode`) cycles
    /// through at runtime — this only changes what a brand-new
    /// shell starts on, not the cycle order. Default `sess`,
    /// matching the historical hardcoded starting value. Set via
    /// `zsh.mode=sess|dir|global`.
    zsh_default_mode: String,
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
    /// Per-extension shell commands invoked by the
    /// [`Action::SmartOpen`] "dive" key (`Ctrl-]` by
    /// default) when the active mode is `/` (files) and
    /// the selected row is a regular file. Configured
    /// via `smart-open.<ext>=<cmd>` lines in the
    /// config file, where `<ext>` is the file
    /// extension **without** the leading `.` (e.g.
    /// `md`, `rs`, `py`), and `<cmd>` is the shell
    /// command to run. The selected file's absolute
    /// path is appended to `<cmd>` (with POSIX
    /// single-quote escaping so paths with spaces or
    /// shell metacharacters can't break the staged
    /// command) and the TUI exits so the parent shell
    /// runs it. The optional key
    /// `smart-open.default=<cmd>` is the fallback
    /// for any extension without an explicit mapping;
    /// absent both, [`Action::SmartOpen`] falls
    /// through to the default `Run` action (open in
    /// `$EDITOR`) so the file-type config is purely
    /// additive.
    ///
    /// Examples:
    /// ```ini
    /// smart-open.md=leaf          # markdown files → `leaf README.md`
    /// smart-open.rs=bat           # rust code → `bat src/main.rs`
    /// smart-open.default=bat      # any other text → `bat README`
    /// smart-open.png=xdg-open     # images → `xdg-open photo.png`
    /// ```
    ///
    /// The key prefix is `smart-open.` (NOT `key.`) so
    /// the existing `key.smart-open=<spec>` binding
    /// parser doesn't try to interpret `<ext>=<cmd>`
    /// as a `parse_key_spec_opt` value. Reserved
    /// `<ext>` names: the single key `default` is
    /// special (the fallback); any other key is taken
    /// verbatim as the file extension to match (case-
    /// insensitive at lookup time). Empty `<cmd>`
    /// values are silently dropped so a typo like
    /// `smart-open.rs=` doesn't bind to an empty
    /// command.
    smart_open_file_commands: std::collections::HashMap<String, String>,
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
    /// `session.<key> = "name"` /
    /// `session.<key>.dir = "~/path"` /
    /// `session.<key>.startup_command = "cmd"`
    /// config keys — `<key>` is an opaque
    /// join key (a legacy numeric index or a
    /// name-derived slug, see `unique_slug`;
    /// the parser doesn't care which). Each
    /// entry becomes a row in the panes (`*`)
    /// view, in file declaration order.
    sessions: Vec<(String, SessionDef)>,
    /// Directory-to-project bindings parsed from `project.<slug>.dir
    /// = "~/path"` config keys — see `ProjectDef`. `<slug>` matches
    /// the slug of a `type: project` note; entries are matched by
    /// longest-`dir`-prefix against the shell's cwd (`resolve_project_dir`)
    /// to resolve which project a command belongs to.
    projects: Vec<(String, ProjectDef)>,
    /// How long (seconds) a project session stays open with no
    /// commands before it's considered idle and closed. Set via
    /// `project.idlethreshold=<seconds>` (a top-level scalar, NOT a
    /// `project.<slug>.*` sub-key — `parse_multi`'s `project.`
    /// branch special-cases this one key before falling through to
    /// slug parsing). Default 1800 (30 minutes).
    project_idle_threshold_secs: i64,
    /// JIRA-label-to-project bindings parsed from `jiralabel.<slug>.match
    /// = "<label>"` config keys — the first (highest-priority) tier
    /// of time tracking's website-project resolution priority: a
    /// JIRA ticket carrying this label is attributed to `<slug>`
    /// regardless of which directory or explicit selection was active
    /// at visit time. `<slug>` matches the same `type: project` note
    /// slug `project.<slug>.dir` uses. One label per project slug —
    /// unlike `project.<slug>.dir`'s longest-prefix match, label
    /// matching is exact-string, so only the last `jiralabel.<slug>.match`
    /// for a given slug wins if the config sets it twice.
    jira_labels: Vec<(String, String)>,
    /// Sparse URL overrides parsed from `weburl.<slug>.match =
    /// "<substring>"` — the second tier of website-project
    /// resolution, for domains that are structurally single-project
    /// (e.g. a project's own dedicated docs site). `match` is a
    /// plain substring tested against a visited URL's host+path (no
    /// regex dependency in this codebase for config-driven matching
    /// — see `resolve_project_by_weburl`). Lower priority than
    /// `jiralabel.<slug>.match` (a labeled JIRA ticket wins even on
    /// a domain that also has a `weburl` override), higher priority
    /// than the time-based fallback.
    web_urls: Vec<(String, String)>,
    /// Display-only URL clustering groups parsed from
    /// `weburlgroup.<name>.match` / `weburlgroup.<name>.label` — see
    /// `WebUrlGroupDef`'s doc comment. Independent of `web_urls`
    /// (assignment vs. clustering are separate concerns).
    web_url_groups: Vec<(String, WebUrlGroupDef)>,
    /// Host entries parsed from
    /// `host.<key> = "name"` /
    /// `host.<key>.host = "alias"` /
    /// `host.<key>.hostname = "real"` /
    /// `host.<key>.user = "u"` /
    /// `host.<key>.port = N` /
    /// `host.<key>.identity = "path"` /
    /// `host.<key>.dir = "~/path"` /
    /// `host.<key>.exec = "cmd"`. Each entry
    /// becomes a row in the `# hosts`
    /// section of the panes (`*`) view.
    /// SSH config (`~/.ssh/config`) entries
    /// without a config-file companion are
    /// auto-appended by `Config::load`.
    hosts: Vec<(String, crate::tui::state::HostDef)>,
    /// Browser sources for the `^`-prefix mode, parsed from
    /// `browser.<id>.type = "chrome"|"firefox"` /
    /// `browser.<id>.profile = "~/path"` config keys. `profile`
    /// is optional per entry — when unset, [`crate::browser::
    /// BrowserSource`] resolution falls back to that browser's
    /// platform-default profile location. When this list is
    /// empty (no `browser.*` keys at all), `browser::
    /// resolve_configured` auto-detects installed browsers
    /// instead — see that function's doc comment.
    browsers: Vec<(usize, BrowserSourceRaw)>,
}

/// One `browser.<id>.*` config entry, before resolving the
/// optional `profile` override into a `crate::browser::
/// BrowserSource` (which requires a concrete path — see
/// `Config::browser_sources`).
#[derive(Debug, Clone, Default)]
struct BrowserSourceRaw {
    kind: Option<crate::browser::BrowserKind>,
    profile: Option<String>,
}

/// User-customizable colors for the TUI. Defaults match the
/// built-in `Theme` palette in `src/tui.rs`. Any unrecognized
/// color falls back to the corresponding default.
#[derive(Debug, Clone)]
#[derive(Default)]
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


impl Config {
    pub fn default() -> Self {
        let dir = env::var("HOME")
            .map(|h| {
                std::path::PathBuf::from(h)
                    .join(".cache")
                    .join("tmux-history")
            })
            .unwrap_or_else(|_| std::path::PathBuf::from(".cache/tmux-history"));
        let ignore: std::collections::HashSet<String> =
            DEFAULT_NO_CAPTURE.iter().map(|s| s.to_string()).collect();
        let file_view_commands: std::collections::HashSet<String> = DEFAULT_FILE_VIEW_COMMANDS
            .iter()
            .map(|s| s.to_string())
            .collect();
        Config {
            tmux_pane_output_dir: dir,
            ignore_capture: ignore,
            file_view_commands,
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
            // Paperless is opt-in, same pairing policy as `llm`
            // above: empty config means "feature disabled".
            paperless: None,
            dropdown_enabled: false,
            dropdown_limit: 6,
            dropdown_min_chars: 1,
            segments_min_words: 5,
            dropdown_highlight: false,
            dropdown_matchmode: "prefix".to_string(),
            commentexpand_enabled: false,
            globcomplete_enabled: false,
            zsh_default_mode: "sess".to_string(),
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
            projects: Vec::new(),
            project_idle_threshold_secs: 1800,
            jira_labels: Vec::new(),
            web_urls: Vec::new(),
            web_url_groups: Vec::new(),
            hosts: Vec::new(),
            browsers: Vec::new(),
            // Empty by default — populated from
            // `smart-open.<ext>=<cmd>` lines in the
            // config file. See the field doc for the
            // matching / fallback semantics.
            smart_open_file_commands: std::collections::HashMap::new(),
            // No theme is selected by default — the
            // TUI falls back to its built-in default
            // (`SelectedTheme::None` plus the
            // manual `tuicolor.*` palette). Users
            // opt in by setting `theme.light=...`
            // and/or `theme.dark=...`. The two are
            // independent: setting only one applies
            // the same theme in BOTH light and dark
            // terminals (the unset slot falls back
            // to the set one at runtime). Setting
            // both lets the user pick a separate
            // light / dark theme.
            theme_light: None,
            theme_dark: None,
        }
    }

    /// Load configuration from `~/.config/smarthistory/config`,
    /// overlaying the defaults.
    pub fn load() -> Self {
        let mut cfg = Config::default();
        if let Some(path) = config_path()
            && let Ok(contents) = std::fs::read_to_string(&path)
        {
            cfg.parse(&contents);
        }
        cfg.apply_env_overrides();
        cfg
    }

    /// Like [`Config::load`], but additionally folds
    /// `~/.config/smarthistory/hosts` and `~/.config/smarthistory/
    /// sessions` in as if their content were appended to the main
    /// config file — see [`Config::parse_multi`] for why this has
    /// to be one combined parse rather than three separate calls to
    /// `parse()`. Either file may be absent (a missing file
    /// contributes an empty string, same as the main config file
    /// being absent).
    ///
    /// Only the TUI startup path (`run_tui_to_stdout` /
    /// `run_tui_check`) calls this — every other CLI subcommand
    /// uses the plain `load()`, since `session.<id>` / `host.<id>`
    /// data is exclusively a `*`-mode (panes) concern and those
    /// commands have no reason to pay for two extra file reads on
    /// every shell prompt.
    pub fn load_tui() -> Self {
        let mut cfg = Config::default();
        let main_contents = config_path()
            .and_then(|p| std::fs::read_to_string(&p).ok())
            .unwrap_or_default();
        let hosts_contents = hosts_path()
            .and_then(|p| std::fs::read_to_string(&p).ok())
            .unwrap_or_default();
        let sessions_contents = sessions_path()
            .and_then(|p| std::fs::read_to_string(&p).ok())
            .unwrap_or_default();
        cfg.parse_multi(&[&main_contents, &hosts_contents, &sessions_contents]);
        cfg.apply_env_overrides();
        cfg
    }

    /// Environment-variable overrides shared by [`Config::load`]
    /// and [`Config::load_tui`]. Env vars always win over the
    /// config file when set (same precedence as `NOTE_SEARCH_*` /
    /// `JIRA_*` elsewhere in this app).
    fn apply_env_overrides(&mut self) {
        if let Ok(db) = env::var("NOTE_SEARCH_DATABASE") {
            let path = std::path::PathBuf::from(&db);
            if path.exists() && path.is_file() {
                self.notes_database = Some(path);
            }
        }
        if let Ok(dir) = env::var("NOTE_SEARCH_DIR") {
            let path = std::path::PathBuf::from(&dir);
            if path.exists() && path.is_dir() {
                self.notes_dir = Some(path);
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
                Some(kind) => self.multiplexer = kind,
                None => eprintln!(
                    "smarthistory: ignoring invalid \
                     SMARTHISTORY_MULTIPLEXER={:?} \
                     (expected `tmux` or `herdr`)",
                    raw
                ),
            }
        }
    }

    /// Parse INI-style lines into the config. Unknown keys are
    /// ignored. Thin wrapper around [`Config::parse_multi`] for the
    /// common single-source case (the main config file, and every
    /// existing test).
    fn parse(&mut self, contents: &str) {
        self.parse_multi(&[contents]);
    }

    /// Parse INI-style lines from multiple sources into the config,
    /// as if they were concatenated into one file — later sources
    /// win for any key set more than once, matching the "later
    /// line wins" rule that already applies within a single file.
    /// Finalization (the `ollama.*` / `paperless.*` pairing
    /// validation, the `~/.ssh/config` merge into `self.hosts`, and
    /// applying the collected `key.*` entries) runs exactly ONCE,
    /// after every source has been read — not per-source. This
    /// matters concretely for the SSH-config merge: it both fills
    /// in gaps on existing `host.<id>` entries AND auto-appends a
    /// synthetic entry for any SSH `Host` block with no matching
    /// explicit entry. Running it after only a partial host list
    /// (e.g. before the `hosts` file has contributed its entries)
    /// would auto-append a synthetic entry for an alias the `hosts`
    /// file was ABOUT to define explicitly, producing a duplicate
    /// row in the `*`-mode panes view.
    ///
    /// Used by [`Config::load_tui`] to fold `~/.config/smarthistory/
    /// hosts` and `~/.config/smarthistory/sessions` into the main
    /// config file's content as if they were one file.
    fn parse_multi(&mut self, contents_list: &[&str]) {
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
        // Accumulator for `paperless.url` / `paperless.token`,
        // same "finalize after the loop" rationale as ollama_*
        // above.
        let mut paperless_url = String::new();
        let mut paperless_token = String::new();
        for contents in contents_list {
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
                    self.ignore_capture = value.split_whitespace().map(|s| s.to_string()).collect();
                }
                "fileviewcommands" => {
                    self.file_view_commands =
                        value.split_whitespace().map(|s| s.to_string()).collect();
                }
                "capturelines" => {
                    if let Some(parsed) = parse_capture_lines(value) {
                        self.default_capture_lines = Some(parsed);
                    } else {
                        self.default_capture_lines = None;
                    }
                }
                "duplicatefilter" => {
                    self.duplicate_filter = crate::util::parse_bool(value, true);
                }
                "dropdown.enabled" => {
                    self.dropdown_enabled = crate::util::parse_bool(value, false);
                }
                "dropdown.limit" => match value.trim().parse::<usize>() {
                    Ok(n) if n > 0 => self.dropdown_limit = n,
                    _ => eprintln!(
                        "warning: dropdown.limit={:?} is not a positive integer; keeping the previous value",
                        value
                    ),
                },
                "zsh.mode" => match value.trim() {
                    "sess" | "dir" | "global" => self.zsh_default_mode = value.trim().to_string(),
                    _ => eprintln!(
                        "warning: zsh.mode={:?} is not one of sess/dir/global; keeping the previous value",
                        value
                    ),
                },
                "dropdown.minchars" => match value.trim().parse::<usize>() {
                    Ok(n) => self.dropdown_min_chars = n,
                    _ => eprintln!(
                        "warning: dropdown.minchars={:?} is not a non-negative integer; keeping the previous value",
                        value
                    ),
                },
                "segments.minwords" => match value.trim().parse::<usize>() {
                    Ok(n) => self.segments_min_words = n,
                    _ => eprintln!(
                        "warning: segments.minwords={:?} is not a non-negative integer; keeping the previous value",
                        value
                    ),
                },
                "commentexpand.enabled" => {
                    self.commentexpand_enabled = crate::util::parse_bool(value, false);
                }
                "globcomplete.enabled" => {
                    self.globcomplete_enabled = crate::util::parse_bool(value, false);
                }
                "dropdown.highlight" => {
                    self.dropdown_highlight = crate::util::parse_bool(value, false);
                }
                "dropdown.matchmode" => match value.trim() {
                    "prefix" | "substring" => self.dropdown_matchmode = value.trim().to_string(),
                    _ => eprintln!(
                        "warning: dropdown.matchmode={:?} is not one of prefix/substring; keeping the previous value",
                        value
                    ),
                },
                "initialmode" => {
                    let upper = value.trim().to_ascii_uppercase();
                    if matches!(
                        upper.as_str(),
                        "SESS" | "SESSION" | "DIR" | "DIRECTORY" | "GLOBAL"
                    ) {
                        self.initial_mode = upper;
                    }
                }
                "multiplexer" => match crate::multiplexer::MultiplexerKind::parse(value) {
                    Some(kind) => self.multiplexer = kind,
                    None => eprintln!(
                        "smarthistory: ignoring invalid \
                             multiplexer={:?} (expected \
                             `tmux` or `herdr`); using \
                             default",
                        value
                    ),
                },
                "ollama.url" => {
                    ollama_url = value.to_string();
                }
                "ollama.model" => {
                    ollama_model = value.to_string();
                }
                "paperless.url" => {
                    paperless_url = value.trim_end_matches('/').to_string();
                }
                "paperless.token" => {
                    paperless_token = value.to_string();
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
                            value, self.todo_line_option
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
                        self.home_map.push(std::path::PathBuf::from(trimmed));
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
                        self.session_dirs.push(expand_tilde(trimmed));
                    }
                }
                other => {
                    if let Some(cmd) = other.strip_prefix("capturelines.")
                        && !cmd.is_empty()
                    {
                        self.capture_lines_per_command
                            .insert(cmd.to_string(), parse_capture_lines(value));
                    } else if let Some(field) = other.strip_prefix("tuicolor.") {
                        Self::assign_theme_field(&mut self.theme, field, value);
                    } else if other == "theme-light" {
                        // Alias for the dashed form
                        // `theme-light=...` (some
                        // config-file writers prefer
                        // dashes over dots in keys).
                        // The canonical form is
                        // `theme.light=` (see the
                        // else-if branch below).
                        if !value.is_empty() {
                            self.theme_light = Some(value.to_string());
                        }
                    } else if other == "theme-dark" {
                        // Alias for the dashed form
                        // `theme-dark=...`. The canonical
                        // form is `theme.dark=` (see the
                        // else-if branch below).
                        if !value.is_empty() {
                            self.theme_dark = Some(value.to_string());
                        }
                    } else if let Some(scheme) = other.strip_prefix("theme.") {
                        // `theme.light=<slug>` /
                        // `theme.dark=<slug>` — the
                        // per-scheme theme selection.
                        // An empty value clears the
                        // slot (the user can wipe
                        // one scheme without
                        // touching the other).
                        let value = value.trim();
                        match scheme.to_ascii_lowercase().as_str() {
                            "light" => {
                                self.theme_light = if value.is_empty() {
                                    None
                                } else {
                                    Some(value.to_string())
                                };
                            }
                            "dark" => {
                                self.theme_dark = if value.is_empty() {
                                    None
                                } else {
                                    Some(value.to_string())
                                };
                            }
                            _ => {
                                // Unknown scheme name —
                                // silently ignore so a
                                // typo doesn't break the
                                // rest of the config
                                // (matching the
                                // `tuicolor.<unknown>`
                                // policy).
                            }
                        }
                    } else if let Some(action) = other.strip_prefix("key.")
                        && !action.is_empty()
                    {
                        key_entries.insert(action.to_string(), value.to_string());
                    } else if let Some(prefix) = other.strip_prefix("prefix.") {
                        Self::assign_prefix(&mut self.query_prefixes, prefix, value);
                    } else if let Some(name) = other.strip_prefix("jira.search.")
                        && !name.is_empty()
                    {
                        Self::assign_jira_fragment(&mut self.jira_fragments, name, value);
                    } else if let Some(ext) = other.strip_prefix("smart-open.") {
                        // Per-extension shell command for the
                        // `/` (files) mode's SmartOpen
                        // dispatch. The key is
                        // `smart-open.<ext>=<cmd>` (NOT
                        // `key.<action>=<spec>`) so the
                        // action-binding parser doesn't
                        // try to parse `<ext>=<cmd>` as a
                        // `parse_key_spec_opt` value. The
                        // special name `default` is the
                        // fallback for any extension
                        // without an explicit mapping.
                        //
                        // Empty / whitespace-only values
                        // are silently dropped (a typo
                        // like `smart-open.rs=` shouldn't
                        // bind to an empty command). The
                        // key after `smart-open.` is taken
                        // verbatim as the extension to
                        // match; lookup at the call site is
                        // case-insensitive, so
                        // `smart-open.MD=leaf` and
                        // `smart-open.md=leaf` are the
                        // same entry. Empty extensions
                        // (e.g. `smart-open.=bat`) are
                        // also dropped so a bare `=`
                        // separator can't smuggle a
                        // fallthrough.
                        let cmd = value.trim();
                        let key = ext.trim();
                        if !key.is_empty() && !cmd.is_empty() {
                            self.smart_open_file_commands
                                .insert(key.to_string(), cmd.to_string());
                        }
                    } else if other == "files.ignore" {
                        for name in value.split_whitespace() {
                            if !name.is_empty() {
                                self.files_ignores.push(name.to_string());
                            }
                        }
                    } else if let Some(rest) = other.strip_prefix("project.") {
                        // `project.idlethreshold = <seconds>` is a
                        // top-level scalar (no `<slug>.` join key),
                        // so it's special-cased before falling
                        // through to the `project.<slug>.dir` slug
                        // parsing below — same idea as `dropdown.*`
                        // being distinct from `session.<key>.*`
                        // despite both starting with letters that
                        // could otherwise be confused for a slug.
                        let unquoted = value.trim().trim_matches('"').trim();
                        if rest == "idlethreshold" {
                            match unquoted.parse::<i64>() {
                                Ok(n) if n > 0 => self.project_idle_threshold_secs = n,
                                _ => eprintln!(
                                    "warning: project.idlethreshold={:?} is not a positive integer; keeping the previous value",
                                    value
                                ),
                            }
                        } else if let Some((slug, field)) = rest.split_once('.') {
                            // `project.<slug>.dir = "~/path"` — the
                            // only recognized sub-field today (no
                            // display-name field: a project's name
                            // lives on its `type: project` note, not
                            // in this config entry — see `ProjectDef`'s
                            // doc comment).
                            let pos = self.projects.iter().position(|(k, _)| k == slug);
                            match (field, pos) {
                                ("dir", Some(idx)) => {
                                    self.projects[idx].1.dir = unquoted.to_string();
                                }
                                ("dir", None) => {
                                    self.projects.push((
                                        slug.to_string(),
                                        ProjectDef { dir: unquoted.to_string() },
                                    ));
                                }
                                _ => {
                                    eprintln!(
                                        "warning: unknown project field {:?} in project.{}; ignoring",
                                        field, slug
                                    );
                                }
                            }
                        } else {
                            // `project.<slug> = "..."` (no
                            // sub-field) has no defined meaning —
                            // unlike `session.<key>`/`host.<key>`,
                            // there's no display-name field to set
                            // here, so this is almost certainly a
                            // typo for `project.<slug>.dir`.
                            eprintln!(
                                "warning: `project.{}` has no meaning on its own; did you mean `project.{}.dir`?",
                                rest, rest
                            );
                        }
                    } else if let Some(rest) = other.strip_prefix("jiralabel.") {
                        // `jiralabel.<slug>.match = "<label>"` — the
                        // JIRA-label tier of website-project
                        // resolution. Same single-field find-or-insert
                        // shape as `project.<slug>.dir` above; the
                        // only recognized sub-field is `match`.
                        let unquoted = value.trim().trim_matches('"').trim();
                        if let Some((slug, field)) = rest.split_once('.') {
                            let pos = self.jira_labels.iter().position(|(k, _)| k == slug);
                            match (field, pos) {
                                ("match", Some(idx)) => {
                                    self.jira_labels[idx].1 = unquoted.to_string();
                                }
                                ("match", None) => {
                                    self.jira_labels.push((slug.to_string(), unquoted.to_string()));
                                }
                                _ => {
                                    eprintln!(
                                        "warning: unknown jiralabel field {:?} in jiralabel.{}; ignoring",
                                        field, slug
                                    );
                                }
                            }
                        } else {
                            eprintln!(
                                "warning: `jiralabel.{}` has no meaning on its own; did you mean `jiralabel.{}.match`?",
                                rest, rest
                            );
                        }
                    } else if let Some(rest) = other.strip_prefix("weburlgroup.") {
                        // `weburlgroup.<name>.match = "<substring>"` /
                        // `weburlgroup.<name>.label = "<label>"` —
                        // display-only clustering, checked before
                        // `weburl.` below even though the two prefixes
                        // can't actually collide (`weburlgroup.` and
                        // `weburl.` diverge at the 7th character), for
                        // readability: the more-specific prefix reads
                        // first. Same two-field find-or-insert shape
                        // as `host.<key>.*`.
                        let unquoted = value.trim().trim_matches('"').trim();
                        if let Some((name, field)) = rest.split_once('.') {
                            let pos = self.web_url_groups.iter().position(|(k, _)| k == name);
                            let set = |group: &mut WebUrlGroupDef, field: &str, val: &str| match field
                            {
                                "match" => group.match_pattern = val.to_string(),
                                "label" => group.label = val.to_string(),
                                _ => {
                                    eprintln!(
                                        "warning: unknown weburlgroup field {:?} in weburlgroup.{}; ignoring",
                                        field, name
                                    );
                                }
                            };
                            match pos {
                                Some(idx) => set(&mut self.web_url_groups[idx].1, field, unquoted),
                                None => {
                                    let mut group = WebUrlGroupDef::default();
                                    set(&mut group, field, unquoted);
                                    self.web_url_groups.push((name.to_string(), group));
                                }
                            }
                        } else {
                            eprintln!(
                                "warning: `weburlgroup.{}` has no meaning on its own; did you mean `weburlgroup.{}.match` or `.label`?",
                                rest, rest
                            );
                        }
                    } else if let Some(rest) = other.strip_prefix("weburl.") {
                        // `weburl.<slug>.match = "<substring>"` — the
                        // sparse-URL-override tier of website-project
                        // resolution. Same single-field find-or-insert
                        // shape as `jiralabel.<slug>.match` above.
                        let unquoted = value.trim().trim_matches('"').trim();
                        if let Some((slug, field)) = rest.split_once('.') {
                            let pos = self.web_urls.iter().position(|(k, _)| k == slug);
                            match (field, pos) {
                                ("match", Some(idx)) => {
                                    self.web_urls[idx].1 = unquoted.to_string();
                                }
                                ("match", None) => {
                                    self.web_urls.push((slug.to_string(), unquoted.to_string()));
                                }
                                _ => {
                                    eprintln!(
                                        "warning: unknown weburl field {:?} in weburl.{}; ignoring",
                                        field, slug
                                    );
                                }
                            }
                        } else {
                            eprintln!(
                                "warning: `weburl.{}` has no meaning on its own; did you mean `weburl.{}.match`?",
                                rest, rest
                            );
                        }
                    } else if let Some(rest) = other.strip_prefix("session.") {
                        // Parse `session.<key> = "name"`,
                        // `session.<key>.dir = "~/path"`,
                        // `session.<key>.startup_command = "cmd"`.
                        // `<key>` determines display order (first
                        // declaration wins) and doubles as the
                        // cross-line join key for an entry's fields.
                        // Historically a numeric index (`session.1`);
                        // new entries are written with a name-derived
                        // slug instead (`session.monorepo`, see
                        // `unique_slug`) — both are just opaque
                        // strings to this parser, so old numeric-keyed
                        // entries keep working unmodified forever.
                        let unquoted = value.trim().trim_matches('"').trim();
                        if let Some((key, field)) = rest.split_once('.') {
                            let pos = self.sessions.iter().position(|(k, _)| k == key);
                            match (field, pos) {
                                ("dir", Some(idx)) => {
                                    self.sessions[idx].1.dir = unquoted.to_string();
                                }
                                ("dir", None) => {
                                    self.sessions.push((
                                        key.to_string(),
                                        SessionDef {
                                            name: String::new(),
                                            dir: unquoted.to_string(),
                                            exec: String::new(),
                                        },
                                    ));
                                }
                                ("exec", Some(idx)) => {
                                    self.sessions[idx].1.exec = unquoted.to_string();
                                }
                                ("exec", None) => {
                                    self.sessions.push((
                                        key.to_string(),
                                        SessionDef {
                                            name: String::new(),
                                            dir: String::new(),
                                            exec: unquoted.to_string(),
                                        },
                                    ));
                                }
                                ("startup_command", _) => {
                                    // Accepted but not used yet.
                                }
                                _ => {}
                            }
                        } else {
                            // `session.<key> = "name"` (no sub-field).
                            let key = rest;
                            if !unquoted.is_empty() {
                                let pos = self.sessions.iter().position(|(k, _)| k == key);
                                match pos {
                                    Some(idx) => self.sessions[idx].1.name = unquoted.to_string(),
                                    None => self.sessions.push((
                                        key.to_string(),
                                        SessionDef {
                                            name: unquoted.to_string(),
                                            dir: String::new(),
                                            exec: String::new(),
                                        },
                                    )),
                                }
                            }
                        }
                    } else if let Some(rest) = other.strip_prefix("host.") {
                        // Parse `host.<key> = "name"`,
                        // `host.<key>.host = "alias"`,
                        // `host.<key>.hostname = "real"`,
                        // `host.<key>.user = "u"`,
                        // `host.<key>.port = N`,
                        // `host.<key>.identity = "path"`,
                        // `host.<key>.dir = "~/path"`,
                        // `host.<key>.exec = "cmd"`. `<key>` is an
                        // opaque join key (numeric or slug — see the
                        // `session.` branch above for why both work).
                        //
                        // `host` is the SSH config
                        // `Host` alias (also used as
                        // the connection target when
                        // no `hostname` is set);
                        // `hostname` is the real
                        // `HostName` to connect to.
                        let unquoted = value.trim().trim_matches('"').trim();
                        if let Some((key, field)) = rest.split_once('.') {
                            let pos = self.hosts.iter().position(|(k, _)| k == key);
                            let set = |host: &mut crate::tui::state::HostDef,
                                       field: &str,
                                       val: &str| {
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
                                                key, val
                                            );
                                        }
                                    }
                                    "identity" => host.identity = val.to_string(),
                                    "dir" => host.dir = val.to_string(),
                                    "exec" => host.exec = val.to_string(),
                                    _ => {
                                        eprintln!(
                                            "warning: unknown host field {:?} in host.{}; ignoring",
                                            field, key
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
                                    self.hosts.push((key.to_string(), host));
                                }
                            }
                        } else {
                            // `host.<key> = "name"` (no sub-field).
                            let key = rest;
                            if !unquoted.is_empty() {
                                let pos = self.hosts.iter().position(|(k, _)| k == key);
                                match pos {
                                    Some(idx) => self.hosts[idx].1.name = unquoted.to_string(),
                                    None => self.hosts.push((
                                        key.to_string(),
                                        crate::tui::state::HostDef {
                                            name: unquoted.to_string(),
                                            ..crate::tui::state::HostDef::default()
                                        },
                                    )),
                                }
                            }
                        }
                    } else if let Some(rest) = other.strip_prefix("browser.") {
                        // Parse `browser.<id>.type = "chrome"|"firefox"`
                        // and `browser.<id>.profile = "~/path"`. The
                        // `<id>` is a numeric index; order doesn't
                        // matter for this mode (unlike `host.<id>`,
                        // which determines display order), but the
                        // same `Vec<(usize, _)>` shape is reused for
                        // consistency with `sessions` / `hosts`.
                        let unquoted = value.trim().trim_matches('"').trim();
                        if let Some((id_str, field)) = rest.split_once('.')
                            && let Ok(id) = id_str.parse::<usize>()
                        {
                            let pos = self.browsers.iter().position(|(i, _)| *i == id);
                            let set = |b: &mut BrowserSourceRaw, field: &str, val: &str| match field
                            {
                                "type" => {
                                    match crate::browser::BrowserKind::parse(val) {
                                        Some(k) => b.kind = Some(k),
                                        None => eprintln!(
                                            "warning: browser.{}.type = {:?} is not a supported browser (chrome, firefox); ignoring",
                                            id, val
                                        ),
                                    }
                                }
                                "profile" => b.profile = Some(val.to_string()),
                                _ => {
                                    eprintln!(
                                        "warning: unknown browser field {:?} in browser.{}; ignoring",
                                        field, id
                                    );
                                }
                            };
                            match pos {
                                Some(idx) => {
                                    let (_, b) = &mut self.browsers[idx];
                                    set(b, field, unquoted);
                                }
                                None => {
                                    let mut b = BrowserSourceRaw::default();
                                    set(&mut b, field, unquoted);
                                    self.browsers.push((id, b));
                                }
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
                    if ollama_url.is_empty() {
                        "url"
                    } else {
                        "model"
                    }
                );
            } else {
                self.llm = Some(llm::LlmConfig {
                    url: ollama_url,
                    model: ollama_model,
                });
            }
        }
        // Same pairing validation for `paperless.url` /
        // `paperless.token`.
        if !paperless_url.is_empty() || !paperless_token.is_empty() {
            if paperless_url.is_empty() || paperless_token.is_empty() {
                eprintln!(
                    "warning: paperless.{} is set but the other half is missing; \
                     paperless mode is disabled. Set both paperless.url and \
                     paperless.token in ~/.config/smarthistory/config.",
                    if paperless_url.is_empty() {
                        "url"
                    } else {
                        "token"
                    }
                );
            } else {
                self.paperless = Some(paperless::PaperlessConfig {
                    url: paperless_url,
                    token: paperless_token,
                });
            }
        }
        // Merge `~/.ssh/config` into `self.hosts`.
        // For every `Host` block in the SSH
        // config, look up a `host.<key>` entry
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
        // Auto-appended entries key off the
        // SSH alias itself (slugified —
        // aliases are almost always already
        // key-safe), disambiguated against
        // every existing key via
        // `unique_slug` so an alias that
        // collides with an explicit
        // `host.<key>` entry gets `-2`
        // appended rather than silently
        // merging into it.
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
                        // Auto-append, keyed off the SSH alias
                        // (slugified, disambiguated against every
                        // existing key).
                        let key = crate::util::unique_slug(
                            self.hosts.iter().map(|(k, _)| k.as_str()),
                            &block.alias,
                            "host",
                        );
                        self.hosts.push((
                            key,
                            crate::tui::state::HostDef {
                                name: block.alias.clone(),
                                host: block.alias.clone(),
                                hostname: block.hostname.clone(),
                                user: block.user.clone(),
                                port: block.port,
                                identity: block.identity.clone(),
                                dir: String::new(),
                                exec: String::new(),
                            },
                        ));
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

    /// True when `command`'s program name is in `fileviewcommands`
    /// (default `less`/`more`/`bat`/`tail`/`head`) — i.e. this
    /// command's first non-flag argument should be recorded as a
    /// `viewed` file event.
    fn is_file_view_command(&self, command: &str) -> bool {
        self.file_view_commands.contains(first_token(command))
    }

    /// Return the resolved TUI theme. The returned `TuiTheme` reflects
    /// any user overrides from `~/.config/smarthistory/config`.
    pub fn theme(&self) -> &TuiTheme {
        &self.theme
    }

    /// The user-configured theme for the LIGHT scheme
    /// (`theme.light=<slug>` in the config file). `None`
    /// when the user hasn't set one — the TUI loader
    /// then falls back to the active scheme's
    /// `theme.dark=` value, then to the built-in
    /// default, then to `SelectedTheme::None` (the
    /// manual `tuicolor.*` palette). The active scheme
    /// defaults to `Dark` and is persisted in the
    /// session file's `colorscheme=` line once the user
    /// toggles it (`Action::ToggleColorScheme`); the TUI
    /// shows the active scheme in the status bar so the
    /// user always knows which slot is in effect.
    pub fn theme_light(&self) -> Option<&str> {
        self.theme_light.as_deref()
    }

    /// The user-configured theme for the DARK scheme
    /// (`theme.dark=<slug>` in the config file). `None`
    /// when the user hasn't set one. Same fallback
    /// chain as `theme_light()`.
    pub fn theme_dark(&self) -> Option<&str> {
        self.theme_dark.as_deref()
    }

    /// The theme to actually use for a given scheme.
    /// Returns the user-configured `theme.<scheme>=`
    /// value when set, otherwise falls back to the
    /// OTHER scheme (so the user only has to set one
    /// and the same theme applies in both light and
    /// dark terminals), otherwise `None` (the TUI
    /// falls back to its built-in default).
    pub fn theme_for(&self, scheme: crate::tui::theme::ColorScheme) -> Option<&str> {
        match scheme {
            crate::tui::theme::ColorScheme::Light => self
                .theme_light
                .as_deref()
                .or(self.theme_dark.as_deref()),
            crate::tui::theme::ColorScheme::Dark => self
                .theme_dark
                .as_deref()
                .or(self.theme_light.as_deref()),
        }
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

    /// Resolved palette as a flat `key=value` block, ready to
    /// feed to the line-editor dropdown widget (and to any
    /// other consumer that wants "what the TUI is *actually*
    /// rendering right now", as opposed to the user's raw
    /// config-file input).
    ///
    /// The resolution is the same one `tui::theme::install_palette`
    /// uses at TUI startup:
    ///   1. If the user set `tuicolor.<field>=`, that value wins.
    ///   2. Otherwise, look up `<field>` in the active built-in
    ///      theme's `ratatui_themes::ThemePalette` (the theme
    ///      chosen by `theme.<scheme>=<slug>` for `scheme`).
    ///   3. Otherwise, fall back to the built-in default.
    ///
    /// The `scheme` argument selects which config-slot is
    /// consulted: `theme.dark=<slug>` is honored when `scheme`
    /// is `Dark`; `theme.light=<slug>` for `Light`. The two
    /// slots fall back to each other (a user who only set
    /// `theme.dark=dracula` gets dracula on both schemes),
    /// matching `theme_for()`.
    ///
    /// Each entry is `(field_name, css_value)` — the value is
    /// a CSS color name (`red`, `cyan`, …), a 16-color name
    /// (`lightblue`, …), or a `#rrggbb` hex string. The format
    /// mirrors what the user would write in the config file
    /// under `tuicolor.<field>=…`, so the widget can re-use
    /// the same `resolve_color`-style parser as the CLI side.
    /// See `docs/dropdown-completion.md` for the contract.
    pub fn resolved_palette(
        &self,
        scheme: crate::tui::theme::ColorScheme,
    ) -> Vec<(&'static str, String)> {
        use crate::tui::theme::SelectedTheme;
        let theme = match self.theme_for(scheme) {
            Some(slug) => SelectedTheme::from_slug(slug),
            None => SelectedTheme::None,
        };
        // Built-in palette for the active theme, used as the
        // fallback for every field the user didn't explicitly
        // override. `None` when no theme is selected — the
        // built-in defaults kick in via the manual-only path.
        let builtin = match theme {
            SelectedTheme::Builtin(b) => Some(b.palette()),
            SelectedTheme::None => None,
        };
        let mut out: Vec<(&'static str, String)> = Vec::with_capacity(14);
        let cfg_theme = &self.theme;
        // Helper: emit `<key>=<value>` with the user's
        // `tuicolor.<key>=` value when set, else the built-in
        // theme's CSS conversion, else the default CSS name.
        let mut push = |key: &'static str,
                        user: &str,
                        fallback_builtin: Option<ratatui::style::Color>,
                        default: &'static str| {
            let value = if !user.is_empty() {
                user.to_string()
            } else if let Some(c) = fallback_builtin {
                crate::tui::theme::color_to_css(c)
            } else {
                default.to_string()
            };
            out.push((key, value));
        };
        // Mirrors `install_palette`'s field-by-field resolution.
        // The `muted` slot on `ThemePalette` maps to `tuicolor.dim`,
        // and the `accent` slot maps to `tuicolor.highlight` (the
        // TUI uses the theme's accent as the highlight fallback
        // when `tuicolor.highlight=` isn't set — see
        // `install_palette`).
        push("bg", &cfg_theme.bg, builtin.map(|p| p.bg), "black");
        push("fg", &cfg_theme.fg, builtin.map(|p| p.fg), "white");
        push(
            "accent",
            &cfg_theme.accent,
            builtin.map(|p| p.accent),
            "cyan",
        );
        push(
            "success",
            &cfg_theme.success,
            builtin.map(|p| p.success),
            "green",
        );
        push(
            "error",
            &cfg_theme.error,
            builtin.map(|p| p.error),
            "red",
        );
        push(
            "warning",
            &cfg_theme.warning,
            builtin.map(|p| p.warning),
            "yellow",
        );
        push(
            "dim",
            &cfg_theme.dim,
            builtin.map(|p| p.muted),
            "gray",
        );
        push(
            "highlight",
            &cfg_theme.highlight,
            builtin.map(|p| p.accent),
            "cyan",
        );
        push(
            "info",
            &cfg_theme.info,
            builtin.map(|p| p.info),
            "blue",
        );
        push(
            "selection",
            &cfg_theme.selection,
            builtin.map(|p| p.selection),
            "blue",
        );
        push(
            "badgefg",
            &cfg_theme.badge_fg,
            builtin.map(|p| p.bg),
            "black",
        );
        push(
            "listbg",
            &cfg_theme.list_bg,
            builtin.map(|p| p.bg),
            "black",
        );
        push(
            "detailsbg",
            &cfg_theme.details_bg,
            builtin.map(|p| p.bg),
            "black",
        );
        push(
            "inputbg",
            &cfg_theme.input_bg,
            builtin.map(|p| p.bg),
            "black",
        );
        push(
            "statusbg",
            &cfg_theme.status_bg,
            builtin.map(|p| p.bg),
            "black",
        );
        out
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

    /// Resolved LLM (ollama) configuration, if any. When
    /// `None`, the `=` and `?` TUI modes are disabled.
    pub fn llm(&self) -> Option<&llm::LlmConfig> {
        self.llm.as_ref()
    }

    /// Resolved Paperless-ngx configuration, if any. When
    /// `None`, the `<` TUI mode is disabled.
    pub fn paperless(&self) -> Option<&paperless::PaperlessConfig> {
        self.paperless.as_ref()
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

    /// Minimum word count a `:` (segment search) result's body
    /// must have to be kept — see `segments.minwords` / the
    /// `segments_min_words` field doc comment. Default `5`.
    pub fn segments_min_words(&self) -> usize {
        self.segments_min_words
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

    /// Per-extension shell commands invoked by
    /// [`Action::SmartOpen`] in `/` (files) mode.
    /// Configured via `smart-open.<ext>=<cmd>`
    /// lines in the config file (see the
    /// [`Config`] doc for the matching / fallback
    /// semantics). The returned map is a
    /// case-preserved snapshot — lookup at the
    /// call site is case-insensitive.
    pub fn smart_open_file_commands(
        &self,
    ) -> &std::collections::HashMap<String, String> {
        &self.smart_open_file_commands
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
            .enumerate()
            .map(|(idx, (_, def))| {
                let home_list: Vec<String> =
                    std::iter::once(std::env::var("HOME").unwrap_or_default())
                        .filter(|s| !s.is_empty())
                        .collect();
                let expanded =
                    crate::util::expand_home_to_absolute(&def.dir, &home_list).into_owned();
                crate::tui::state::HistoryRow {
                    id: -10_000 - (idx as i64),
                    command: def.name.clone(),
                    directory: expanded,
                    session_id: String::new(),
                    exit_code: 0,
                    timestamp: 0,
                    comment: def.exec.clone(),
                    output: String::new(),
                    mode: "session".to_string(),
                    source: "sessions".to_string(),
                    ..Default::default()
                }
            })
            .collect()
    }

    /// Session entries that have a `.dir` set, with the directory
    /// expanded to an absolute path — used by the `prune-directories`
    /// CLI subcommand to find entries whose directory no longer
    /// exists. Entries with no `.dir` are omitted (nothing to check).
    /// Same expansion `sessions()` uses, so a directory this reports
    /// as existing/missing matches what the picker itself would show.
    pub fn session_directories(&self) -> Vec<(String, String, String)> {
        self.sessions
            .iter()
            .filter(|(_, def)| !def.dir.is_empty())
            .map(|(key, def)| {
                let home_list: Vec<String> =
                    std::iter::once(std::env::var("HOME").unwrap_or_default())
                        .filter(|s| !s.is_empty())
                        .collect();
                let expanded =
                    crate::util::expand_home_to_absolute(&def.dir, &home_list).into_owned();
                (key.clone(), def.name.clone(), expanded)
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
                let connection_string = format!("{}{}{}", user_prefix, target, port_suffix);
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
                    ..Default::default()
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
        self.hosts.iter().map(|(_, def)| def.clone()).collect()
    }

    /// Resolve the configured `browser.<id>.*` entries into
    /// `crate::browser::BrowserSource`s. An entry without a
    /// `type` is dropped (there's nothing to resolve); an entry
    /// without a `profile` falls back to that browser kind's
    /// platform-default profile location the same way an entry
    /// wouldn't exist at all — see
    /// `crate::browser::BrowserSource::autodetect`'s per-kind
    /// default-path helpers. Returns an empty `Vec` when no
    /// `browser.*` keys are set at all, which is the signal
    /// `crate::browser::resolve_configured` uses to fall back to
    /// full auto-detection instead of "configured, but empty".
    pub fn browser_sources(&self) -> Vec<crate::browser::BrowserSource> {
        self.browsers
            .iter()
            .filter_map(|(_, raw)| {
                let kind = raw.kind?;
                let profile = match raw.profile.as_deref() {
                    Some(p) => std::path::PathBuf::from(crate::util::expand_home(p).into_owned()),
                    None => crate::browser::default_profile_for(kind)?,
                };
                Some(crate::browser::BrowserSource { kind, profile })
            })
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
    /// Canonical set of recognized `prefix.<name>` config keys,
    /// including the `elements` back-compat alias for `segments`.
    /// Used by `validate_config` to flag a typo'd or made-up
    /// `prefix.<name>=` line (e.g. `prefix.fuzzy=`, `prefix.regex=`
    /// — there is no such mode; fuzzy/regex are match *algorithms*,
    /// toggled by `Ctrl-F` for whatever mode is active, not
    /// separate prefix-triggered modes) the same way the
    /// `key.<action>` check already flags an unknown action name.
    /// Kept in sync with `assign_prefix`'s match arms below by
    /// hand — same tradeoff `ALL_ACTIONS` avoids for `key.*`, but
    /// prefix names change about as rarely as actions do, and a
    /// shared table would mean `assign_prefix` looping over string
    /// comparisons instead of a plain `match`.
    const KNOWN_PREFIX_NAMES: &[&str] = &[
        "output", "llm", "question", "notes", "todo", "directories", "panes", "files", "tags",
        "ag", "codegraph", "jira", "segments", "elements", "similar", "paperless", "browser",
        "processes", "pass", "project", "meta",
    ];

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
            "ag" => prefixes.ag = c,
            "codegraph" => prefixes.codegraph = c,
            "jira" => prefixes.jira = c,
            "segments" => prefixes.segments = c,
            // Back-compat: `elements` was this mode's name before
            // note_search's segment redesign — an existing config
            // file's `prefix.elements=` still applies rather than
            // silently going unrecognized.
            "elements" => prefixes.segments = c,
            "similar" => prefixes.similar = c,
            "paperless" => prefixes.paperless = c,
            "browser" => prefixes.browser = c,
            "zoxide" => prefixes.zoxide = c,
            "processes" => prefixes.processes = c,
            "pass" => prefixes.pass = c,
            "project" => prefixes.project_pick = c,
            "meta" => prefixes.meta = c,
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
        if matches!(key.as_str(), "me" | "today" | "week" | "month") {
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

/// The first non-flag (not `-`-prefixed) argument of a command line
/// — used to pick the "file" out of a `fileviewcommands` invocation
/// like `tail -f app.log` or `less -N config.yaml`. Skips every
/// leading `-...` token, not just one; doesn't understand flags that
/// take a separate value (`head -n 20 file.csv` picks `20`, not
/// `file.csv`) — a known, accepted limitation, not a bug to fix
/// here. Returns `None` when the command has no arguments at all, or
/// only flag arguments.
fn first_non_flag_argument(command: &str) -> Option<&str> {
    let mut tokens = command.split_whitespace();
    tokens.next()?; // the command name itself, not an argument
    tokens.find(|tok| !tok.starts_with('-'))
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
    anyhow::bail!(
        "command not found in tmux log after {} attempts",
        MAX_ATTEMPTS
    )
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

/// True if stderr is connected to a terminal. `smarthistory ask`
/// writes its human-readable answer to stderr (stdout is reserved
/// for the chosen command, captured by the zsh wrapper's `$()`), so
/// it gates its own ANSI coloring on stderr's TTY-ness rather than
/// stdout's.
fn stderr_is_tty() -> bool {
    std::io::stderr().is_terminal()
}

/// The transient "the LLM is working" line `smarthistory ask` prints
/// to stderr right before the blocking HTTP call — otherwise the
/// console just sits there for the 1-5s a local model typically
/// takes, indistinguishable from a hang. Colorized dim (matching the
/// `\x1b[2m` convention `highlight_full` already uses elsewhere in
/// this file) when `color` is true; plain text otherwise.
fn format_thinking_message(color: bool) -> String {
    if color {
        "\x1b[2mThinking…\x1b[0m".to_string()
    } else {
        "Thinking…".to_string()
    }
}

/// Build the colorized `LLM Answer` header + answer block, and the
/// numbered suggestion lines, for `smarthistory ask`. The header
/// sits on its own line, with the prose answer starting on the line
/// after it — distinguishes "here's the LLM's answer" from the
/// preceding `Thinking…` line and any `Choose [1-N]` prompt that
/// follows. Colorized only when `color` is true (the caller gates
/// this on `stderr_is_tty()` — plain ANSI on a non-terminal stderr
/// would just be noise for whatever's consuming it).
/// `suggestion_lines[i]` is `suggestions[i]` prefixed with its
/// 1-based pick-list index, matching what the `Choose [1-N]` prompt
/// expects the user to type.
fn format_ask_output(prose: &str, suggestions: &[String], color: bool) -> (String, Vec<String>) {
    let (header_open, header_close, num_open, num_close) = if color {
        ("\x1b[1;35m", "\x1b[0m", "\x1b[1;36m", "\x1b[0m")
    } else {
        ("", "", "", "")
    };
    let answer_block = format!("{header_open}LLM Answer{header_close}\n{prose}");
    let suggestion_lines = suggestions
        .iter()
        .enumerate()
        .map(|(i, cmd)| format!("{num_open}{}){num_close} {cmd}", i + 1))
        .collect();
    (answer_block, suggestion_lines)
}

/// Build the open/close markers for the matched prefix in
/// `AnsiMode::Bold` (the historical default) and `AnsiMode::Off`.
/// `AnsiMode::Full` is handled by the dedicated `highlight_full`
/// path below because it needs to wrap the *whole* cell, not just
/// the matched prefix, so the simple `open…close` shape doesn't
/// fit — hence the `("", "")` return for `Full` on a TTY (a
/// signal to the caller to take the `Full` branch).
fn decoration(ansi: AnsiMode) -> (&'static str, &'static str) {
    match (ansi, stdout_is_tty()) {
        (AnsiMode::Off, _) => ("", ""),
        (AnsiMode::Bold, true) => ("\x1b[1m", "\x1b[0m"), // bold on, reset
        (AnsiMode::Bold, false) => ("[", "]"),            // pipe-safe markers
        (AnsiMode::Full, false) => ("[", "]"),             // same as Bold on a pipe
        (AnsiMode::Full, true) => ("", ""),                // see highlight_full
    }
}

/// `Full` mode wraps the entire cell in dim SGR, with the matched
/// prefix upgraded to bold. Returns
/// `<dim-open><prefix-before><reset><bold-open><match><reset><dim-open><suffix>`
/// where each SGR pair uses the standard escape codes. The trailing
/// reset is omitted because the caller (`project_row`) emits a
/// trailing reset anyway, and double-resetting is harmless. `needle`
/// is the user-typed query; empty needle means no match, so the
/// whole cell is plain (same as the `Bold` path with empty needle).
fn highlight_full(cell: &str, needle: &str) -> String {
    const DIM_OPEN: &str = "\x1b[2m";
    const DIM_CLOSE: &str = "\x1b[0m";
    const BOLD_OPEN: &str = "\x1b[1m";
    const BOLD_CLOSE: &str = "\x1b[0m";

    if needle.is_empty() {
        return cell.to_string();
    }
    let mut out = String::with_capacity(cell.len() + DIM_OPEN.len() * 2 + BOLD_OPEN.len() * 2);
    out.push_str(DIM_OPEN);
    let mut rest = cell;
    while let Some(pos) = rest.find(needle) {
        out.push_str(&rest[..pos]);
        out.push_str(DIM_CLOSE);
        out.push_str(BOLD_OPEN);
        out.push_str(needle);
        out.push_str(BOLD_CLOSE);
        out.push_str(DIM_OPEN);
        rest = &rest[pos + needle.len()..];
    }
    out.push_str(rest);
    out
}

fn project_row(
    row_data: &[(String, String)],
    fields: &[String],
    derived: &[String],
    query: Option<&str>,
    ansi: AnsiMode,
) -> Vec<String> {
    let (open, close) = decoration(ansi);
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
                match (ansi, stdout_is_tty()) {
                    (AnsiMode::Off, _) => out.push(cell),
                    (AnsiMode::Full, true) => {
                        // Dim the whole cell, bold the matched prefix.
                        // `query` is `None` for a scope-only search
                        // (no user-typed filter); in that case there's
                        // nothing to highlight, so emit the plain cell.
                        if let Some(q) = query {
                            out.push(highlight_full(&cell, q));
                        } else {
                            out.push(cell);
                        }
                    }
                    (AnsiMode::Full, false) | (AnsiMode::Bold, _) => {
                        // Bold mode on a TTY/pipe, or Full on a pipe
                        // (falls back to the bold-wrapping marker
                        // pair for downstream pipe-safety — the zsh
                        // widget always invokes the CLI from a pipe,
                        // so it never sees Full's dim+bold split
                        // either way).
                        if !open.is_empty() {
                            if let Some(q) = query {
                                out.push(highlight(&cell, q, open, close));
                            } else {
                                out.push(cell);
                            }
                        } else {
                            out.push(cell);
                        }
                    }
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
/// The `history` table's real columns, plus the two joined
/// pseudo-columns (`comment` from `command_comments`, `output` from
/// `history_output`). This is the complete allowlist of names
/// `qualify_field` will accept — anything else is user-supplied
/// `--fields` input that must not be spliced into the SQL text
/// unchecked (a bare `format!("h.{}", name)` on an unvalidated name
/// is a SQL injection primitive: a value containing a `--` comment
/// marker can terminate the SELECT list and append arbitrary SQL).
const KNOWN_RAW_FIELDS: &[&str] = &[
    "id",
    "command",
    "directory",
    "session_id",
    "exit_code",
    "timestamp",
    "mode",
    "comment",
    "output",
];

/// Return the SQL expression for a conceptual field name, qualifying
/// history columns with `h.` and the global comment with `c.comment`.
/// Unknown field names fall back to `command`, matching the existing
/// "unsupported query column" fallback used elsewhere in this file.
fn qualify_field(name: &str) -> String {
    if !KNOWN_RAW_FIELDS.contains(&name) {
        eprintln!("warning: unsupported field {:?}, falling back to command", name);
        return "h.command".to_string();
    }
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

/// Remove every `session.<key>` line — the name line (`session.<key> =
/// ...`) and every sub-field (`session.<key>.dir = ...`,
/// `.exec`, `.startup_command`, ...) — for each key in `keys`, from
/// `path`. Used by `smarthistory prune-directories` to delete stale
/// entries from whichever of `~/.config/smarthistory/config` /
/// `~/.config/smarthistory/sessions` they live in.
///
/// A missing file is not an error (a from-scratch install, or an
/// entry that only lives in the other file) — returns `Ok(0)`. If no
/// line in the file matches any key, the file is left untouched
/// entirely (no needless rewrite). Otherwise the file is rewritten
/// atomically (temp file + rename), same pattern as
/// `App::write_new_entry_to_config`.
///
/// Matching is a literal prefix check (`session.<key> ` / `session.<key>.`
/// / `session.<key>=`) against each line's trimmed start — the exact
/// three forms `write_new_entry_to_config` ever emits — so `session.foo`
/// never matches a `session.foobar` line.
fn remove_session_lines(
    path: &std::path::Path,
    keys: &std::collections::HashSet<String>,
) -> std::io::Result<usize> {
    let contents = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut removed = 0usize;
    let kept: Vec<&str> = contents
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            let is_stale_session_line = keys.iter().any(|key| {
                trimmed.starts_with(&format!("session.{} ", key))
                    || trimmed.starts_with(&format!("session.{}.", key))
                    || trimmed.starts_with(&format!("session.{}=", key))
            });
            if is_stale_session_line {
                removed += 1;
            }
            !is_stale_session_line
        })
        .collect();
    if removed == 0 {
        return Ok(0);
    }
    let mut new_contents = kept.join("\n");
    if contents.ends_with('\n') {
        new_contents.push('\n');
    }
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, new_contents.as_bytes())?;
    std::fs::rename(&tmp_path, path)?;
    Ok(removed)
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
    prefix_only: bool,
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut clause = String::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    // Substring (or, with `prefix_only`, prefix-only) filter on the
    // command (and optionally the joined comment column).
    if let Some(q) = query {
        let escaped = escape_like(q);
        // `prefix_only` drops the comment side of the OR entirely
        // (not just switches it to a prefix match): a comment
        // matching the typed text as a substring is exactly the
        // kind of unrelated hit `prefix_only` exists to rule out
        // (e.g. typing "ls" matching a comment that happens to
        // contain "ls" mid-word), so it's simplest and most
        // correct to only ever prefix-match the command itself.
        let pattern = if prefix_only {
            format!("{}%", escaped)
        } else {
            format!("%{}%", escaped)
        };
        match query_column {
            Some(("command", None)) => {
                clause.push_str(&format!(
                    " AND {prefix}command LIKE ? ESCAPE '\\'",
                    prefix = qualified_column_prefix
                ));
                params.push(Box::new(pattern));
            }
            Some(("command", Some("comment"))) if prefix_only => {
                clause.push_str(&format!(
                    " AND {prefix}command LIKE ? ESCAPE '\\'",
                    prefix = qualified_column_prefix
                ));
                params.push(Box::new(pattern));
            }
            Some(("command", Some("comment"))) => {
                let p = qualified_column_prefix;
                clause.push_str(&format!(
                    " AND ({p}command LIKE ? ESCAPE '\\' OR c.comment LIKE ? ESCAPE '\\')",
                ));
                params.push(Box::new(pattern.clone()));
                params.push(Box::new(pattern));
            }
            Some((col, _)) => {
                // Caller asked for an unknown column; fall back to
                // a plain command match so the rest of the filter
                // keeps working.
                eprintln!(
                    "warning: unsupported query column {:?}, falling back to command",
                    col
                );
                clause.push_str(&format!(
                    " AND {prefix}command LIKE ? ESCAPE '\\'",
                    prefix = qualified_column_prefix
                ));
                params.push(Box::new(pattern));
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
        false,
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
    prefix_only: bool,
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let (extra, params) = build_filter_sql(
        query,
        directory.as_deref(),
        session_flag,
        exit_code,
        // Match command OR the joined comment column (unless
        // `prefix_only`, which drops the comment side — see
        // `build_filter_sql`'s doc comment on that branch).
        Some(("command", Some("comment"))),
        "h.",
        prefix_only,
    );
    let prefix = " FROM history h \
                   LEFT JOIN command_comments c ON h.command = c.command \
                   LEFT JOIN history_output o ON h.id = o.history_id \
                   WHERE 1=1";
    (format!("{}{}", prefix, extra), params)
}

/// Resolve a comment to the most recently used command that carries
/// it, for the zsh comment-expansion widget (`smarthistory expand`).
/// Matches `command_comments.comment` exactly (case-insensitively,
/// matching the case-insensitivity `LIKE` already gives substring
/// search elsewhere in this file) rather than as a substring — unlike
/// `build_search_where_clause`, which is deliberately substring-based
/// and matches command-or-comment together, this needs an unambiguous
/// single answer for a specific typed word.
fn resolve_comment(conn: &Connection, text: &str) -> anyhow::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT h.command FROM history h \
         JOIN command_comments c ON h.command = c.command \
         WHERE c.comment = ?1 COLLATE NOCASE \
         ORDER BY h.timestamp DESC LIMIT 1",
        params![text],
        |row| row.get(0),
    )
    .optional()
    .map_err(anyhow::Error::from)
}

/// What `smarthistory pane-exec` should do for a given current
/// session/workspace name, resolved against the configured
/// `session.<id>`/`host.<id>` entries.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PaneExecTarget {
    /// Run this shell command (a `session.<id>.exec`, or a
    /// `host.<id>`'s `ssh` connection).
    Run(String),
    /// A `session.<id>` entry matched, but it has no `.exec` set —
    /// nothing to run, not an error.
    NoExecConfigured,
    /// No `session.<id>`/`host.<id>` entry has this display name.
    NotFound,
}

/// Pure matching logic behind `Commands::PaneExec`, split out from
/// the process-spawning/exit-code handling around it so it can be
/// unit-tested directly (calling the real command handler would
/// `std::process::exit` the test process).
///
/// `session.<id>` entries are checked first: an exact match on
/// display name, `.exec` (if set) run as-is — a normal local
/// command, no ambiguity. `host.<id>` entries are checked next: an
/// exact match on display name OR on `host:<name>` (the label herdr
/// may show if the user renamed the workspace — see
/// `stage_pane_selection`'s own matcher, which accepts the same two
/// forms). A host match's `.exec` is deliberately NOT included in
/// the returned command — see `Commands::PaneExec`'s doc comment for
/// why (it's meant to be typed into the remote shell after
/// connecting, not run as a local follow-up command).
fn resolve_pane_exec(cfg: &Config, current_name: &str) -> PaneExecTarget {
    if let Some(row) = cfg.sessions().into_iter().find(|r| r.command == current_name) {
        return if row.comment.is_empty() {
            PaneExecTarget::NoExecConfigured
        } else {
            PaneExecTarget::Run(row.comment)
        };
    }
    let host_rows = cfg.hosts();
    let host_defs = cfg.host_defs();
    let host_match = host_rows.iter().position(|r| {
        r.command == current_name || format!("host:{}", r.command) == current_name
    });
    if let Some(pos) = host_match
        && let Some(host_def) = host_defs.get(pos)
    {
        return PaneExecTarget::Run(host_def.ssh_command());
    }
    PaneExecTarget::NotFound
}

/// Upsert every row of an imported `HistoryExport` into `history`
/// (plus its `command_comments` / `history_output` side tables),
/// returning `(imported, updated)` counts. Extracted from
/// `Commands::Import` so the insert-vs-update distinction is
/// independently testable.
fn import_history_rows(
    conn: &Connection,
    rows: &[HistoryExportRow],
) -> anyhow::Result<(usize, usize)> {
    let mut imported = 0usize;
    let mut updated = 0usize;
    for row in rows {
        // `INSERT ... ON CONFLICT DO UPDATE` reports a changed-row
        // count of 1 for BOTH a fresh insert and a
        // conflict-triggered update, so the count itself can't
        // distinguish the two cases. Check for the row's existence
        // up front instead.
        use rusqlite::OptionalExtension;
        let existed = conn
            .query_row(
                "SELECT 1 FROM history WHERE command = ?1 AND directory = ?2 AND session_id = ?3",
                params![row.command, row.directory, row.session_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();

        // Upsert the history row.
        conn.execute(
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

        if existed {
            updated += 1;
        } else {
            imported += 1;
        }

        // Store the comment if present.
        if let Some(ref comment) = row.comment
            && !comment.is_empty()
        {
            conn.execute(
                "INSERT INTO command_comments (command, comment) VALUES (?1, ?2) \
                     ON CONFLICT (command) DO UPDATE SET comment = excluded.comment",
                params![row.command, comment],
            )?;
        }

        // Store the output if present.
        if let Some(ref output) = row.output
            && !output.is_empty()
        {
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
    Ok((imported, updated))
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
    // Time-tracking: each row is a span of time attributed to one
    // project. `end_ts IS NULL` marks the currently-open session —
    // enforced as an application-level invariant (at most one open
    // row at a time), not a schema constraint, since SQLite has no
    // clean way to express "at most one NULL" directly. See
    // `switch_project` for the only code path that opens/closes rows.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS project_sessions (
            id INTEGER PRIMARY KEY,
            project_slug TEXT NOT NULL,
            start_ts INTEGER NOT NULL,
            end_ts INTEGER,
            end_reason TEXT
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_project_sessions_slug
         ON project_sessions (project_slug)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_project_sessions_range
         ON project_sessions (start_ts, end_ts)",
        [],
    )?;
    // The last EXPLICITLY selected project (via `smarthistory project
    // select`, see the `.` picker) — the fallback used when neither
    // an in-repo marker file nor a `project.<slug>.dir` entry
    // resolves the current directory. A singleton row: the CHECK
    // constraint plus `INSERT ... ON CONFLICT(id) DO UPDATE` (see
    // `switch_project`) keeps exactly one row, ever.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS project_current (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            project_slug TEXT NOT NULL,
            set_ts INTEGER NOT NULL
        )",
        [],
    )?;
    // `smarthistory project pause`'s toggle state — a singleton row
    // (same `CHECK (id = 1)` invariant as `project_current`) present
    // only while tracking is paused. `paused_slug` snapshots whatever
    // project was active at the moment of pausing (NULL when nothing
    // was), so `pause` called again restores exactly that project
    // rather than re-resolving from the directory the user happens
    // to be in when they resume.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS project_pause (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            paused_slug TEXT,
            paused_at INTEGER NOT NULL
        )",
        [],
    )?;
    // File-tracking events (`smarthistory file viewed/modified/created`).
    // `project_slug` is resolved once, at record time, exactly like
    // `history.directory` bakes in its project attribution only
    // indirectly (via the timestamp-range join `project_sessions`
    // does) — here it's stored directly on the row since a file
    // event has no natural session-membership window of its own to
    // join against; it's a point-in-time editor event, not a shell
    // command bounded by "next command in this session".
    conn.execute(
        "CREATE TABLE IF NOT EXISTS file_events (
            id INTEGER PRIMARY KEY,
            path TEXT NOT NULL,
            event_kind TEXT NOT NULL CHECK (event_kind IN ('viewed', 'modified', 'created')),
            project_slug TEXT,
            timestamp INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_file_events_project_ts ON file_events (project_slug, timestamp)",
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

/// Longest-`dir`-prefix match: which configured project's directory
/// binding is `pwd` nested under (or equal to)? The most specific
/// (longest) matching `project.<slug>.dir` wins over a broader
/// ancestor's binding, mirroring how nested directory bindings are
/// expected to behave (e.g. a sub-project inside a monorepo with its
/// own binding). Same tilde-expansion helper `Config::sessions()`/
/// `session_directories()` already use.
fn resolve_project_dir(cfg: &Config, pwd: &str) -> Option<String> {
    let home_list: Vec<String> = std::iter::once(std::env::var("HOME").unwrap_or_default())
        .filter(|s| !s.is_empty())
        .collect();
    cfg.projects
        .iter()
        .filter_map(|(slug, def)| {
            let expanded = crate::util::expand_home_to_absolute(&def.dir, &home_list);
            let expanded = expanded.trim_end_matches('/');
            if pwd == expanded || pwd.starts_with(&format!("{expanded}/")) {
                Some((slug.clone(), expanded.len()))
            } else {
                None
            }
        })
        .max_by_key(|(_, len)| *len)
        .map(|(slug, _)| slug)
}

/// Resolve a project slug from a JIRA issue's labels — the first
/// (highest-priority) tier of time tracking's website-project
/// resolution (see `jiralabel.<slug>.match` on `Config`). Exact
/// string match, case-sensitive (JIRA labels are themselves
/// case-sensitive and typically lowercase-with-hyphens by
/// convention, so this matches how the user would configure
/// `jiralabel.<slug>.match` against them). Returns the first
/// configured slug whose label appears in `labels`; when more than
/// one configured label matches (a ticket carrying two labels that
/// each map to a different project), the earliest-declared
/// `jiralabel.<slug>.match` entry wins — same "first in file order"
/// tie-break `session.<key>`/`host.<key>` use elsewhere.
fn resolve_project_by_label(cfg: &Config, labels: &[String]) -> Option<String> {
    cfg.jira_labels
        .iter()
        .find(|(_, label)| labels.iter().any(|l| l == label))
        .map(|(slug, _)| slug.clone())
}

/// Strip a URL down to `host+path` (no scheme, no query string, no
/// fragment) for `weburl`/`weburlgroup` substring matching. Plain
/// string slicing rather than a URL-parsing crate — this codebase
/// has no such dependency and the config's `match` values are
/// themselves plain substrings, so a parser would be more precision
/// than the matching semantics need. Not meant to be a fully correct
/// URL parser (e.g. userinfo `user:pass@host` isn't stripped) — just
/// good enough that a query-string cache-buster or `#anchor` doesn't
/// cause an otherwise-matching pattern to miss.
fn url_host_and_path(url: &str) -> &str {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let end = without_scheme.find(['?', '#']).unwrap_or(without_scheme.len());
    &without_scheme[..end]
}

/// Extract just the host from a URL (strip scheme, userinfo, port,
/// path/query/fragment) — the report's default website-clustering
/// key when no `weburlgroup.<name>.match` override applies, so every
/// `github.com` page (say) groups under one `github.com` cluster
/// without needing per-domain config. A leading `www.` is stripped
/// so `www.github.com` and `github.com` cluster together — the two
/// are the same site to a human reading the report, even though
/// they're technically different hostnames.
fn url_host(url: &str) -> &str {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let without_userinfo = without_scheme
        .split_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(without_scheme);
    let end = without_userinfo
        .find(['/', '?', '#'])
        .unwrap_or(without_userinfo.len());
    let host_and_port = &without_userinfo[..end];
    let host = host_and_port.split_once(':').map(|(h, _)| h).unwrap_or(host_and_port);
    host.strip_prefix("www.").unwrap_or(host)
}

/// Pull the URL out of a staged `open "<url>"` / `xdg-open "<url>"`
/// shell command — the exact, literal double-quoted format
/// `stage_jira_selection` (`src/tui/actions.rs`) always uses, so a
/// simple "text between the first pair of double quotes" scan is
/// reliable here (unlike `stage_browser_selection`, which
/// POSIX-shell-quotes with single quotes — but browser-mode visits
/// are read directly from `BrowserEntry.url`, never scanned back out
/// of `history.command`, so that format never needs parsing here).
fn extract_quoted_url(command: &str) -> Option<&str> {
    let start = command.find('"')? + 1;
    let end = command[start..].find('"')? + start;
    Some(&command[start..end])
}

/// Resolve a project slug from a sparse URL override
/// (`weburl.<slug>.match`) — the second tier of time tracking's
/// website-project resolution, for domains that are structurally
/// single-project. Plain substring match against `url`'s host+path.
/// First configured match wins (same file-order tie-break as
/// `resolve_project_by_label`).
fn resolve_project_by_weburl(cfg: &Config, url: &str) -> Option<String> {
    let haystack = url_host_and_path(url);
    cfg.web_urls
        .iter()
        .find(|(_, pattern)| !pattern.is_empty() && haystack.contains(pattern.as_str()))
        .map(|(slug, _)| slug.clone())
}

/// Resolve the display-only cluster label for a URL
/// (`weburlgroup.<name>.match`/`.label`) — independent of project
/// *assignment*, see `WebUrlGroupDef`'s doc comment. First
/// configured match wins.
fn cluster_label_for_url(cfg: &Config, url: &str) -> Option<String> {
    let haystack = url_host_and_path(url);
    cfg.web_url_groups
        .iter()
        .find(|(_, def)| !def.match_pattern.is_empty() && haystack.contains(def.match_pattern.as_str()))
        .map(|(_, def)| def.label.clone())
}

/// The full 3-tier website-project resolution priority: (1) a JIRA
/// label match (via `extract_issue_key` + `labels_for_issue` +
/// `resolve_project_by_label` — skipped entirely when `jira_client`
/// is `None`, i.e. JIRA isn't configured), (2) a sparse `weburl`
/// override, (3) the time-based fallback — whichever `project_sessions`
/// interval (if any) was open at `timestamp`. `text` is either a
/// visited URL (browser mode) or a staged shell command that embeds
/// one (`open "https://.../browse/KEY"` — see `extract_issue_key`'s
/// doc comment on why REST-mode JIRA visits aren't a separate data
/// source); both are valid input to every tier since each tier's
/// match is itself just a substring/regex scan, indifferent to
/// whether it's scanning a bare URL or a whole command line.
fn resolve_project_for_website_visit(
    cfg: &Config,
    jira_client: Option<&dyn crate::jira::JiraClient>,
    label_cache: &mut std::collections::HashMap<String, Vec<String>>,
    text: &str,
    timestamp: i64,
    sessions: &[ProjectSessionInterval],
) -> Option<String> {
    if let Some(client) = jira_client
        && let Some(key) = crate::jira::extract_issue_key(text)
    {
        let labels = crate::jira::labels_for_issue(client, key, label_cache);
        if let Some(slug) = resolve_project_by_label(cfg, &labels) {
            return Some(slug);
        }
    }
    if let Some(slug) = resolve_project_by_weburl(cfg, text) {
        return Some(slug);
    }
    sessions
        .iter()
        .find(|s| timestamp >= s.start_ts && (s.still_open || timestamp < s.effective_end))
        .map(|s| s.slug.clone())
}

/// Walk upward from `pwd` looking for an in-repo marker file
/// (`.smarthistory-project`) whose first non-blank line names the
/// project slug directly — lets a portable/shared checkout pin its
/// project without every user needing a matching `project.<slug>.dir`
/// entry in their own config (whose absolute paths differ per
/// machine). Bounded at `$HOME` (or `MAX_LEVELS`, whichever comes
/// first) since this runs on every `smarthistory add` call — an
/// unbounded walk to `/` would make every shell prompt pay for a
/// stat-storm on a deeply nested or oddly-rooted cwd. A marker file
/// that exists but has no non-blank line stops the walk (the marker
/// establishes this directory as the project root, even if
/// malformed) rather than deferring to a grandparent's marker.
fn find_project_marker(pwd: &std::path::Path) -> Option<String> {
    const MARKER_NAME: &str = ".smarthistory-project";
    const MAX_LEVELS: usize = 25;
    let home = std::env::var("HOME").ok();
    let mut dir = pwd;
    for _ in 0..MAX_LEVELS {
        let marker = dir.join(MARKER_NAME);
        if let Ok(contents) = std::fs::read_to_string(&marker) {
            return contents
                .lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string());
        }
        if home.as_deref() == Some(&dir.to_string_lossy()) {
            break;
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }
    None
}

/// True while `smarthistory project pause` has tracking paused (a
/// `project_pause` row exists). Checked first, and unconditionally,
/// by `resolve_current_project` — a pause must stick even if the
/// user `cd`s into a directory-bound project's tree while paused, not
/// just suppress whatever was resolved at the moment of pausing.
fn is_project_tracking_paused(conn: &Connection) -> anyhow::Result<bool> {
    use rusqlite::OptionalExtension;
    let row: Option<i64> = conn
        .query_row("SELECT id FROM project_pause WHERE id = 1", [], |row| row.get(0))
        .optional()?;
    Ok(row.is_some())
}

/// Resolve which project (if any) owns the current directory/context,
/// in priority order: is tracking paused (`smarthistory project
/// pause`) — always `None` if so, regardless of the rest; in-repo
/// marker file; longest `project.<slug>.dir` prefix match; the last
/// explicitly-selected project (`project_current`, set by
/// `smarthistory project select` — see the `.` picker); then `None`
/// (untracked).
fn resolve_current_project(conn: &Connection, cfg: &Config, pwd: &str) -> anyhow::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    if is_project_tracking_paused(conn)? {
        return Ok(None);
    }
    if let Some(slug) = find_project_marker(std::path::Path::new(pwd)) {
        return Ok(Some(slug));
    }
    if let Some(slug) = resolve_project_dir(cfg, pwd) {
        return Ok(Some(slug));
    }
    let current: Option<String> = conn
        .query_row("SELECT project_slug FROM project_current WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(current)
}

/// Close the currently-open `project_sessions` row (if any) and open
/// a new one for `resolved_slug`, implementing the unified
/// directory/idle/explicit-switch lifecycle — deliberately ONE
/// algorithm rather than separate code paths for each trigger:
///
/// - No open row, `resolved_slug` is `Some`: open a new row.
/// - Open row's project differs from `resolved_slug` (including
///   `resolved_slug = None`, i.e. moved to an unresolvable
///   directory): close immediately, `end_ts = now`. `forced_reason`
///   (used by `smarthistory project select`, an explicit switch)
///   overrides the default `"directory_change"` end_reason — leaving
///   a mismatched session open until the idle timeout would
///   misattribute every command run in the new context during that
///   window, so this case never waits.
/// - Open row's project matches `resolved_slug`: close only if the
///   gap since the last command observed in this session exceeds
///   `idle_threshold_secs`. `end_ts` backdates to
///   `last_activity + idle_threshold_secs` (when activity actually
///   stopped being observed), not `now` (when this function happened
///   to run) — same "duration reflects real activity, not wall-clock
///   luck" principle the per-command duration query uses.
///
/// "Last activity" is derived from `history.timestamp` (not tracked
/// as a separate mutable column, to avoid an extra write on every
/// `Add`), bounded below by the session's own `start_ts` so a
/// freshly-opened session with no commands yet still idles out
/// correctly relative to when it opened, not some earlier command.
fn switch_project(
    conn: &Connection,
    resolved_slug: Option<&str>,
    now: i64,
    idle_threshold_secs: i64,
    forced_reason: Option<&str>,
) -> anyhow::Result<()> {
    use rusqlite::OptionalExtension;
    let open: Option<(i64, String, i64)> = conn
        .query_row(
            "SELECT id, project_slug, start_ts FROM project_sessions WHERE end_ts IS NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    let mut still_open = open.is_some();
    if let Some((id, slug, start_ts)) = open {
        if resolved_slug != Some(slug.as_str()) {
            let reason = forced_reason.unwrap_or("directory_change");
            conn.execute(
                "UPDATE project_sessions SET end_ts = ?1, end_reason = ?2 WHERE id = ?3",
                params![now, reason, id],
            )?;
            still_open = false;
        } else {
            let last_activity: Option<i64> = conn.query_row(
                "SELECT MAX(timestamp) FROM history WHERE timestamp >= ?1",
                params![start_ts],
                |row| row.get(0),
            )?;
            let last_activity = last_activity.unwrap_or(start_ts);
            if now - last_activity > idle_threshold_secs {
                conn.execute(
                    "UPDATE project_sessions SET end_ts = ?1, end_reason = 'idle' WHERE id = ?2",
                    params![last_activity + idle_threshold_secs, id],
                )?;
                still_open = false;
            }
        }
    }

    if !still_open
        && let Some(slug) = resolved_slug
    {
        conn.execute(
            "INSERT INTO project_sessions (project_slug, start_ts, end_ts, end_reason)
             VALUES (?1, ?2, NULL, NULL)",
            params![slug, now],
        )?;
    }
    Ok(())
}

/// Resolve a `smarthistory project report --day` value (`YYYY-MM-DD`,
/// `today`, `yesterday`, or omitted) to the `[start, end)` Unix
/// timestamp range covering that local calendar day, plus the
/// resolved `NaiveDate` (used only for the report header).
fn parse_project_report_day(
    day: &Option<String>,
) -> anyhow::Result<(i64, i64, chrono::NaiveDate)> {
    use chrono::{Duration, Local, NaiveDate, TimeZone};
    let today = Local::now().date_naive();
    let date = match day.as_deref() {
        None | Some("today") => today,
        Some("yesterday") => today - Duration::days(1),
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| {
            anyhow::anyhow!(
                "invalid --day value {:?}; expected YYYY-MM-DD, \"today\", or \"yesterday\"",
                s
            )
        })?,
    };
    let start_naive = date.and_hms_opt(0, 0, 0).unwrap();
    let end_naive = (date + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap();
    // `.single()` can fail across a DST transition; fall back to
    // treating the naive wall-clock time as UTC rather than erroring
    // out of the report for what is, at worst, an hour of skew on
    // two days a year.
    let start_ts = Local
        .from_local_datetime(&start_naive)
        .single()
        .map(|dt| dt.timestamp())
        .unwrap_or_else(|| start_naive.and_utc().timestamp());
    let end_ts = Local
        .from_local_datetime(&end_naive)
        .single()
        .map(|dt| dt.timestamp())
        .unwrap_or_else(|| end_naive.and_utc().timestamp());
    Ok((start_ts, end_ts, date))
}

/// A `project_sessions` row overlapping the report's day range, with
/// its still-open end clamped to `now` so a caller that just wants
/// *a* concrete upper bound (e.g. for display) never needs to
/// special-case `NULL`. Interval-membership checks (notes/website
/// resolution) must still branch on `still_open` rather than
/// comparing straight against `effective_end`: a still-open
/// session's clamp is only an artifact of "we needed some number",
/// not a real boundary — a timestamp landing in the same
/// wall-clock second the report runs would otherwise be excluded by
/// a strict `<` against its own clamped-to-now value.
struct ProjectSessionInterval {
    slug: String,
    start_ts: i64,
    effective_end: i64,
    still_open: bool,
}

/// Every `project_sessions` row whose interval overlaps
/// `[range_start, range_end)`, ordered by `start_ts`. A session can
/// extend past the report's day boundary on either end — callers
/// that need to clip to the day range do so themselves; this just
/// answers "which sessions were open at some point during the day".
fn project_sessions_in_range(
    conn: &Connection,
    range_start: i64,
    range_end: i64,
    now: i64,
) -> anyhow::Result<Vec<ProjectSessionInterval>> {
    let mut stmt = conn.prepare(
        "SELECT project_slug, start_ts, end_ts FROM project_sessions
         WHERE start_ts < ?1 AND (end_ts IS NULL OR end_ts > ?2)
         ORDER BY start_ts ASC",
    )?;
    let rows = stmt.query_map(params![range_end, range_start], |row| {
        let slug: String = row.get(0)?;
        let start_ts: i64 = row.get(1)?;
        let end_ts: Option<i64> = row.get(2)?;
        Ok((slug, start_ts, end_ts))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (slug, start_ts, end_ts) = r?;
        let still_open = end_ts.is_none();
        let effective_end = end_ts.unwrap_or(now).min(now);
        out.push(ProjectSessionInterval {
            slug,
            start_ts,
            effective_end,
            still_open,
        });
    }
    Ok(out)
}

/// One `history` row within the report's day range, joined against
/// whichever `project_sessions` interval (if any) was open at its
/// timestamp, with its active duration already derived and capped —
/// see [`switch_project`]'s module-level doc for why the cap exists.
struct ReportCommandRow {
    command: String,
    directory: String,
    project_slug: Option<String>,
    timestamp: i64,
    active_secs: i64,
}

/// Fetch every `history` command row timestamped within
/// `[range_start, range_end)`, each paired with its resolved project
/// (via the same `project_sessions` timestamp-range join
/// [`switch_project`] writes) and its derived active duration:
/// `min(next_command_in_session_ts - ts, idle_threshold)`, falling
/// back to the session's own end (or "now") for a session's last
/// command. Partitioned by `session_id` so a gap in one pane's
/// activity never inflates a command's duration in another —
/// deliberately different from [`Commands::Next`]'s "next command"
/// predictor query, which isn't session-scoped.
fn report_command_rows(
    conn: &Connection,
    range_start: i64,
    range_end: i64,
    idle_threshold_secs: i64,
) -> anyhow::Result<Vec<ReportCommandRow>> {
    let sql = "
        WITH pairs AS (
            SELECT h.command, h.directory, h.timestamp,
                   LEAD(h.timestamp) OVER (
                       PARTITION BY h.session_id ORDER BY h.timestamp ASC, h.id ASC
                   ) AS next_ts,
                   ps.project_slug, ps.end_ts AS session_end
            FROM history h
            LEFT JOIN project_sessions ps
              ON h.timestamp >= ps.start_ts
             AND (ps.end_ts IS NULL OR h.timestamp < ps.end_ts)
            WHERE h.mode = 'command'
        )
        SELECT command, directory, project_slug, timestamp,
               MIN(COALESCE(next_ts, session_end, strftime('%s', 'now')) - timestamp, ?1) AS active_secs
        FROM pairs
        WHERE timestamp >= ?2 AND timestamp < ?3
        ORDER BY timestamp ASC
    ";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(
        params![idle_threshold_secs, range_start, range_end],
        |row| {
            Ok(ReportCommandRow {
                command: row.get(0)?,
                directory: row.get(1)?,
                project_slug: row.get(2)?,
                timestamp: row.get(3)?,
                active_secs: row.get(4)?,
            })
        },
    )?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Fetch every `file_events` row timestamped within `[range_start,
/// range_end)` and group it by project (`None` = an event whose
/// directory resolved to no project at record time — see
/// `Commands::File`'s handler) and event kind, deduplicating by path
/// with an occurrence count. Pulled out of `ProjectAction::Report`'s
/// handler as its own function, the same stdout-free-testable-helper
/// split `report_command_rows`/`group_command_rows` already use.
fn report_file_events(
    conn: &Connection,
    range_start: i64,
    range_end: i64,
) -> anyhow::Result<std::collections::BTreeMap<Option<String>, FileEventGroups>> {
    let mut by_slug: std::collections::BTreeMap<Option<String>, FileEventGroups> =
        std::collections::BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT path, event_kind, project_slug FROM file_events \
         WHERE timestamp >= ?1 AND timestamp < ?2",
    )?;
    let rows = stmt.query_map(params![range_start, range_end], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for r in rows {
        let (path, kind, slug) = r?;
        let groups = by_slug.entry(slug).or_default();
        let bucket = match kind.as_str() {
            "viewed" => &mut groups.viewed,
            "modified" => &mut groups.modified,
            "created" => &mut groups.created,
            _ => continue,
        };
        *bucket.entry(path).or_insert(0) += 1;
    }
    Ok(by_slug)
}

/// Format a duration in seconds as `HhMMm` / `Mm` / `Ss`, matching
/// the compactness of this project's other plain-text CLI output
/// (no table/color crate dependency — see `Commands::Export`'s and
/// `Commands::List`'s handlers for the established style).
fn format_duration_secs(secs: i64) -> String {
    let secs = secs.max(0);
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Print one project's (or "untracked"'s) section of `project
/// report`: a heading, total active time, a directories breakdown,
/// and a commands table filtered to `>= min_duration` seconds. The
/// total and directories breakdown intentionally use every row
/// (`min_duration` only trims the commands table itself — see
/// `ProjectAction::Report`'s `min_duration` doc comment).
/// One collapsed line in the report's Commands table: every row
/// sharing the same `(command, directory)` pair, folded into a
/// single entry with a total duration and an occurrence count.
/// `timestamp` is only meaningful (and only printed) when `count ==
/// 1` — see `print_project_report_section`.
struct CommandGroup<'a> {
    command: &'a str,
    directory: &'a str,
    timestamp: i64,
    total_secs: i64,
    count: usize,
}

/// Collapse repeated `(command, directory)` pairs into one
/// [`CommandGroup`] each, in first-appearance order, summing
/// `active_secs` and counting occurrences. `history`'s dedup upsert
/// (`idx_history_dedup`, keyed on `(command, directory, session_id)`)
/// already collapses repeats *within one shell session*, so a
/// `count > 1` here specifically means the same command ran in the
/// same directory across two or more *different* sessions (panes) —
/// without this, a command run across ten panes over the day would
/// print as ten near-identical lines, reading as noise (or looking
/// like a duplicate-row bug) rather than "this happened a lot".
fn group_command_rows<'a>(rows: &[&'a ReportCommandRow]) -> Vec<CommandGroup<'a>> {
    let mut groups: Vec<CommandGroup> = Vec::new();
    for r in rows {
        match groups
            .iter_mut()
            .find(|g| g.command == r.command && g.directory == r.directory)
        {
            Some(g) => {
                g.total_secs += r.active_secs;
                g.count += 1;
            }
            None => groups.push(CommandGroup {
                command: &r.command,
                directory: &r.directory,
                timestamp: r.timestamp,
                total_secs: r.active_secs,
                count: 1,
            }),
        }
    }
    groups
}

fn print_project_report_section(slug: &str, rows: &[&ReportCommandRow], min_duration: i64) {
    let total: i64 = rows.iter().map(|r| r.active_secs).sum();
    println!("\n## {slug}");
    println!("Total active time: {}", format_duration_secs(total));

    let mut by_dir: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
    for r in rows {
        *by_dir.entry(r.directory.as_str()).or_insert(0) += r.active_secs;
    }
    println!("\n### Directories");
    if by_dir.is_empty() {
        println!("(none)");
    } else {
        let mut dirs: Vec<(&str, i64)> = by_dir.into_iter().collect();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.1));
        for (dir, secs) in dirs {
            println!(
                "- {} ({})",
                crate::util::expand_home(dir),
                format_duration_secs(secs)
            );
        }
    }

    println!("\n### Commands");
    let filtered: Vec<&ReportCommandRow> = rows
        .iter()
        .filter(|r| r.active_secs >= min_duration)
        .copied()
        .collect();
    if filtered.is_empty() {
        println!("(none)");
    } else {
        println!("| Time | Duration | Directory | Command |");
        println!("| --- | --- | --- | --- |");
        // A single-occurrence command keeps its actual timestamp in
        // the Time column; a repeated one (same command, same
        // directory, run across two or more shell sessions during
        // the day — see `group_command_rows`) shows a "Nx" count
        // there instead, since there's no single timestamp left to
        // show once the rows are folded together.
        for g in &group_command_rows(&filtered) {
            let time_cell = if g.count > 1 {
                format!("{}x", g.count)
            } else {
                chrono::DateTime::from_timestamp(g.timestamp, 0)
                    .map(|dt| {
                        dt.with_timezone(&chrono::Local)
                            .format("%H:%M:%S")
                            .to_string()
                    })
                    .unwrap_or_else(|| g.timestamp.to_string())
            };
            println!(
                "| {} | {} | {} | {} |",
                time_cell,
                format_duration_secs(g.total_secs),
                escape_md_table_cell(&crate::util::expand_home(g.directory)),
                escape_md_table_cell(g.command)
            );
        }
    }
}

/// Escape a value for embedding in a Markdown table cell: `|` would
/// otherwise be parsed as a column separator, and an embedded
/// newline would break the row onto its own (unterminated) table
/// line — a real risk here since both `directory` and `command` are
/// arbitrary shell text, not report-controlled strings.
fn escape_md_table_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// One project's (or "untracked"'s) file-tracking events for the
/// report's day range, already deduplicated by path with an
/// occurrence count (`path -> count`) per event kind — see the
/// `files_by_slug` construction in `ProjectAction::Report`'s handler.
#[derive(Default)]
struct FileEventGroups {
    viewed: std::collections::BTreeMap<String, usize>,
    modified: std::collections::BTreeMap<String, usize>,
    created: std::collections::BTreeMap<String, usize>,
}

/// Print one `### Files <label>` list: each deduplicated path, with
/// an `(Nx)` occurrence count when the file was viewed/modified/
/// created more than once in the day (same convention the Commands
/// table's `Nx` counter uses) — a single occurrence prints bare, no
/// `(1x)` noise. Paths are `$HOME`-shortened to `~` like every other
/// path in this report.
fn print_file_events_section(label: &str, paths: &std::collections::BTreeMap<String, usize>) {
    println!("\n### Files {label}");
    if paths.is_empty() {
        println!("(none)");
        return;
    }
    for (path, count) in paths {
        if *count > 1 {
            println!("- {} ({}x)", crate::util::expand_home(path), count);
        } else {
            println!("- {}", crate::util::expand_home(path));
        }
    }
}

/// One website visit, already resolved to a display cluster
/// (`weburlgroup.<name>.match`/`.label`, or the visit's host when no
/// override applies — see `print_website_section`'s call site).
struct WebsiteLink {
    cluster: String,
    title: String,
    url: String,
}

/// The basename of a note's filename, stripped of its extension —
/// `note_search`'s `[[link]]` wiki-link target (the same identity
/// `stage_project_selection`'s slug derivation, and every other
/// `[[...]]` reference in this codebase, uses; notes are always
/// referenced by basename, not the frontmatter title — see
/// `notes::fetch`'s doc comment). `filename` here is already a bare
/// basename with extension (e.g. `Standup.md`), never a path with
/// directory components — `NoteResult::filename`'s own contract —
/// so this only ever strips the extension. Falls back to the input
/// verbatim on the (unreachable in practice) case where
/// `file_stem()` finds nothing to strip.
fn note_basename(filename: &str) -> &str {
    std::path::Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename)
}

/// Escape the handful of Markdown link syntax characters that could
/// otherwise break `[title](url)`: `]` would close the link text
/// early, and `(`/`)` inside a bare (parenthesized) link destination
/// would prematurely end the URL. Titles/URLs here are page titles
/// and real URLs, not report-controlled strings, so this is a real
/// risk, not just defensive.
fn escape_md_link_text(s: &str) -> String {
    s.replace('[', "(").replace(']', ")")
}

/// Group website links by cluster and deduplicate by URL within each
/// group (the same page visited more than once in the day collapses
/// to one entry, keeping the first title seen for it). Clusters and
/// the URLs within them come out in alphabetical order — a stable
/// order not dependent on visit timestamps, since websites aren't
/// given a time column in this report (unlike commands). Pulled out
/// of `print_website_section` as a pure function so the grouping/
/// dedup logic is unit-testable without capturing stdout.
fn group_website_links(items: &[WebsiteLink]) -> Vec<(&str, Vec<(&str, &str)>)> {
    let mut by_cluster: std::collections::BTreeMap<&str, std::collections::BTreeMap<&str, &str>> =
        std::collections::BTreeMap::new();
    for link in items {
        by_cluster
            .entry(link.cluster.as_str())
            .or_default()
            .entry(link.url.as_str())
            .or_insert(link.title.as_str());
    }
    by_cluster
        .into_iter()
        .map(|(cluster, links)| (cluster, links.into_iter().collect()))
        .collect()
}

/// Print one project's (or "untracked"'s) `### Websites` list,
/// grouped by cluster (a `weburlgroup.<name>.match` label, or —
/// falling back automatically — the visit's own host, so every
/// `github.com` page lands under one `github.com` group without
/// needing per-domain config) via `group_website_links`. Each URL
/// renders as a Markdown link, `[title](url)`.
fn print_website_section(items: &[WebsiteLink]) {
    for (cluster, links) in group_website_links(items) {
        println!("- **{}**", escape_md_link_text(cluster));
        for (url, title) in links {
            println!("  - [{}]({})", escape_md_link_text(title), url);
        }
    }
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
    // Recreate the dedup index immediately — before dropping
    // `history_old` below — so there's no window where the new
    // `history` table lacks `idx_history_dedup`. `Commands::Add`'s
    // `INSERT ... ON CONFLICT (command, directory, session_id)`
    // upsert requires this index to exist; without it, the very
    // first history write in the same process right after an
    // upgrade (typically the zsh precmd hook on the next prompt)
    // fails with "ON CONFLICT clause does not match any…constraint"
    // and that entry is lost. (`init_db`'s own `CREATE UNIQUE INDEX
    // IF NOT EXISTS` call runs *before* this migration and so
    // doesn't cover the index this migration just dropped by
    // recreating the table.)
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_history_dedup
         ON history (command, directory, session_id)",
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

/// Shared body for `Commands::Tui` and `Commands::CreateNote` — the
/// latter is exactly the former with `create_note` forced on,
/// `--exec` on by default (same rationale `--create-note` already
/// has, so a bare `smarthistory create-note` doesn't need `eval
/// "$(...)"` to actually run the staged save command once the user
/// submits the dialog), no positional query/prefix/mode, and an
/// optional pre-filled title/content pair for the dialog in place of
/// the interactive path's usual "prefill from the selected row"
/// (there's no TUI selection yet at this point).
///
/// Always exits the process (never returns `Ok(())` in practice) —
/// kept as `-> anyhow::Result<()>` so the one early-return validation
/// error (`--pane-height`) can use `?` at the call site like every
/// other fallible command.
/// The CLI flags that launch a locked completion picker
/// (`--glob-complete[-dir]`/`--pid-complete`) plus the
/// `--root` they scope against. Bundled into one struct so
/// `run_tui_command`'s signature doesn't carry four
/// separate same-typed positional parameters that a future
/// edit could silently transpose — see `CliOverrides` for
/// the same rationale applied to the persistence-override
/// flags.
struct CompletionPickerArgs {
    glob_complete: Option<String>,
    glob_complete_dir: Option<String>,
    pid_complete: Option<String>,
    root: Option<PathBuf>,
}

#[allow(clippy::too_many_arguments)]
fn run_tui_command(
    conn: Connection,
    mode: Option<String>,
    prefix: Option<String>,
    picker: CompletionPickerArgs,
    exec: bool,
    query: Option<String>,
    pane: Option<String>,
    panes_filter: Option<String>,
    pane_height: Option<String>,
    create_note: bool,
    create_note_prefill: Option<(String, String)>,
) -> anyhow::Result<()> {
    let CompletionPickerArgs {
        glob_complete,
        glob_complete_dir,
        pid_complete,
        root,
    } = picker;
    // `--create-note` defaults to `--exec`: without it, a
    // bare `smarthistory tui --create-note` just prints the
    // staged `note_search create-note ...` command and exits
    // — nothing runs it unless the caller wraps the
    // invocation in `eval "$(...)"`. That's an easy trap for
    // a flag meant to be launched standalone (a herdr
    // keybinding, a shell alias) rather than always through
    // a shell wrapper, so `--create-note` runs the staged
    // command itself via `sh -c`, same as passing `--exec`
    // explicitly. `--exec` can still be passed on its own
    // with any other flag; this only widens its default.
    let exec = exec || create_note;
    // Honor an explicit --mode flag first. Otherwise consult
    // the user's environment for a preferred starting scope:
    //   $SMARTHISTORY_TUI_MODE      — explicit override
    //   $SMARTHISTORY_MODE          — alias
    // Otherwise fall back to the config file's `initialmode`
    // (or `SESS` if unset).
    //
    // Track which source actually supplied the scope so
    // we can mark the corresponding `CliOverrides.mode`
    // field. The semantic is: `--mode` and the env vars
    // are treated as "one-off, don't persist" overrides
    // (a herdr keybinding setting `--mode GLOBAL` for a
    // single invocation shouldn't change the user's next
    // plain `smarthistory tui` launch). The config-file
    // `initialmode=` setting is treated as a persistent
    // preference (the user wrote it in their config; they
    // expect it to apply every launch and they expect the
    // session file to keep their currently-active scope in
    // sync).
    let cli_mode_override = mode.is_some()
        || std::env::var("SMARTHISTORY_TUI_MODE")
            .ok()
            .filter(|s| !s.is_empty())
            .is_some()
        || std::env::var("SMARTHISTORY_MODE")
            .ok()
            .filter(|s| !s.is_empty())
            .is_some();
    let initial_mode = mode
        .or_else(|| {
            std::env::var("SMARTHISTORY_TUI_MODE")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::env::var("SMARTHISTORY_MODE")
                .ok()
                .filter(|s| !s.is_empty())
        })
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
    //
    // `cli_query_override` is the trigger for
    // `CliOverrides.query` — BOTH `--prefix` and a
    // positional `--query` (or `$SMARTHISTORY_TUI_QUERY`)
    // are treated as one-off CLI overrides that should
    // not leak into the next launch.
    let env_query = std::env::var("SMARTHISTORY_TUI_QUERY")
        .ok()
        .filter(|s| !s.is_empty());
    // `--glob-complete` and `--glob-complete-dir` are mutually
    // exclusive (enforced by clap) and share everything except which
    // row kind (`FilePickerKind`) the resulting picker keeps.
    // Resolved together here so the rest of this function only has
    // to branch on ONE `Option`, not two.
    let glob_complete_effective: Option<(&str, tui::FilePickerKind)> = glob_complete
        .as_deref()
        .map(|p| (p, tui::FilePickerKind::Files))
        .or_else(|| glob_complete_dir.as_deref().map(|p| (p, tui::FilePickerKind::Directories)));
    let cli_query_override = glob_complete_effective.is_some()
        || pid_complete.is_some()
        || prefix.is_some()
        || query.is_some()
        || env_query.is_some();
    // Loaded early (moved ahead of its original position, just
    // below) so `--glob-complete`/`--glob-complete-dir`/
    // `--pid-complete` can read the configured prefix characters
    // before the `initial_query` match runs.
    let tui_cfg = Config::load();
    let (initial_query, override_session_query) =
        match (
            glob_complete_effective,
            pid_complete.as_deref(),
            prefix.as_deref(),
            query.as_deref(),
            env_query.as_deref(),
        ) {
            (Some((pattern, _kind)), _, _, _, _) => {
                // `--glob-complete[-dir] <PATTERN>` implies the
                // files prefix REGARDLESS of kind — a directory
                // picker still drives the exact same underlying `/`
                // files-mode walk/fetch pipeline, just filtered down
                // to directory entries (see `FilePickerKind`). Same
                // "one-off, starts in a specific mode, don't
                // persist" treatment `--prefix` gets, just pre-filled
                // with the raw glob word instead of a bare prefix
                // char. `clap`'s `conflicts_with` already rules out
                // `prefix`/`pid_complete` being set here.
                //
                // Trailing space: lands the cursor ready for an
                // immediate extra narrowing word (`*.md jira`, see
                // `FilesFilter::Glob`'s `extra_tokens`) without the
                // user having to press space first. Harmless for the
                // filter itself — `FilesState::current_pattern`
                // trims the body before tokenizing either way.
                let files_prefix = tui_cfg.query_prefixes().files;
                (format!("{}{} ", files_prefix, pattern), true)
            }
            (None, Some(pattern), _, _, _) => {
                // `--pid-complete <PATTERN>` implies the processes
                // prefix — same shape as `--glob-complete`, just a
                // different target mode and no glob translation
                // (the pattern is passed through verbatim; `%` mode
                // does its own free-text substring matching).
                // Trailing space for the same "ready to keep typing"
                // reason as the glob-complete branch above.
                let processes_prefix = tui_cfg.query_prefixes().processes;
                (format!("{}{} ", processes_prefix, pattern), true)
            }
            (None, None, Some(p), _, _) => {
                // Take the first char of the prefix string
                // (it's always a single char by construction;
                // we accept multi-char input defensively for
                // shell-quoted strings).
                let first_char = p.chars().next().unwrap_or_default().to_string();
                (first_char, true)
            }
            (None, None, None, Some(q), _) => (q.to_string(), false),
            (None, None, None, None, Some(q)) => (q.to_string(), false),
            (None, None, None, None, None) => (String::new(), false),
        };
    // Build the LLM client up front so the TUI entry
    // point doesn't need to know about config parsing.
    // The TUI itself only sees `Option<Box<dyn LlmClient>>`
    // and surfaces a "not configured" status when None.
    let llm_client: Option<Box<dyn llm::LlmClient>> = tui_cfg
        .llm
        .as_ref()
        .map(llm::OllamaClient::new)
        .map(|c| Box::new(c) as Box<dyn llm::LlmClient>);
    let llm_config = tui_cfg.llm.clone();
    // Bundle the four CLI override flags into a
    // single struct. The TUI uses this to decide
    // which session fields to NOT persist on
    // exit, so a one-off CLI invocation (e.g.
    // `smarthistory tui --prefix '*'` from a
    // herdr keybinding) doesn't leak the
    // resulting state into the next plain
    // launch. See `CliOverrides` for the
    // per-field rationale.
    //
    // Validate `--pane-height` up front so an
    // invalid value produces a clear error
    // message instead of being silently
    // dropped. We don't apply the value to
    // `app.pane_height` here — the TUI does
    // that via `run_tui_to_stdout` — but a
    // typo like `--pane-height fourteen`
    // would be caught by the TUI's own
    // `PaneHeight::parse` call. Pre-validate
    // here so a typo surfaces a clean error
    // and the TUI never starts.
    if let Some(ref h) = pane_height
        && crate::tui::state::PaneHeight::parse(h).is_none()
    {
        return Err(anyhow::anyhow!(
            "invalid --pane-height {:?}; expected a non-negative integer number of lines",
            h
        ));
    }
    let cli_overrides = tui::CliOverrides {
        mode: cli_mode_override,
        query: cli_query_override,
        pane_visibility: pane.is_some(),
        // `panes_filter` isn't currently
        // persisted in the session file
        // (the field is reset to its default
        // on every launch), so this flag is
        // informational. It's tracked for
        // symmetry with the other four and
        // so the documentation accurately
        // lists all five CLI flags as
        // "one-off".
        panes_filter: panes_filter.is_some(),
        // `--pane-height <HEIGHT>` is a
        // one-off override: applied for this
        // launch but not persisted. See
        // `CliOverrides::pane_height` for
        // the rationale and the corresponding
        // `paneheight=` save-site gate.
        pane_height: pane_height.is_some(),
    };
    match tui::run_tui_to_stdout(
        initial_mode,
        initial_query,
        conn,
        llm_client,
        llm_config,
        override_session_query,
        pane.as_deref(),
        panes_filter.as_deref(),
        pane_height.as_deref(),
        cli_overrides,
        create_note,
        create_note_prefill,
        glob_complete_effective.map(|(_, kind)| kind),
        pid_complete.is_some(),
        root,
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
                    Ok(s) => std::process::exit(s.code().unwrap_or(1)),
                    Err(e) => {
                        eprintln!("smarthistory: failed to exec {:?}: {}", command, e);
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

            // Time tracking: resolve which project (if any) this
            // command belongs to and open/close `project_sessions`
            // rows accordingly, BEFORE recording the command itself
            // — a directory change or idle gap must close the prior
            // session using ITS last real activity, not a timestamp
            // that already reflects the command about to be
            // inserted. `Config::load()` here is the same cheap
            // single-file read/parse every other hot-path CLI
            // command already pays (e.g. the dropdown widget's
            // `smarthistory search` call on every keystroke).
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let project_cfg = Config::load();
            let resolved_project = resolve_current_project(&conn, &project_cfg, &pwd)?;
            switch_project(
                &conn,
                resolved_project.as_deref(),
                now,
                project_cfg.project_idle_threshold_secs,
                None,
            )?;

            // File tracking: a `less`/`bat`/`tail`/`head`-style
            // command (`fileviewcommands`, configurable) is itself
            // evidence its file argument was viewed — record it the
            // same as an explicit `smarthistory file viewed <path>`
            // call, no editor hook required. Resolved relative to
            // `pwd` (the argument is usually a relative path) and
            // attributed by the FILE's own directory, same as the
            // `file viewed` subcommand — not necessarily `pwd`
            // itself, though in practice they're almost always the
            // same directory.
            if project_cfg.is_file_view_command(&command)
                && let Some(arg) = first_non_flag_argument(&command)
            {
                let candidate = if std::path::Path::new(arg).is_absolute() {
                    arg.to_string()
                } else {
                    format!("{pwd}/{arg}")
                };
                let canonical = crate::util::canonicalize_directory(&candidate);
                let dir = std::path::Path::new(&canonical)
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let file_project = resolve_current_project(&conn, &project_cfg, &dir)?;
                conn.execute(
                    "INSERT INTO file_events (path, event_kind, project_slug, timestamp) VALUES (?1, 'viewed', ?2, ?3)",
                    params![canonical, file_project, now],
                )?;
            }

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
        Commands::Expand { text } => {
            if let Some(command) = resolve_comment(&conn, &text)? {
                println!("{}", command);
            }
        }
        Commands::Ask { question } => {
            use llm::LlmClient;
            use std::io::Write;

            let question = question.trim();
            if question.is_empty() {
                eprintln!("Ask: provide a question after the question prefix");
                std::process::exit(1);
            }
            let cfg = Config::load();
            let Some(llm_cfg) = cfg.llm() else {
                eprintln!("{}", llm::LlmError::NotConfigured);
                std::process::exit(1);
            };
            let session_id =
                env::var("SMART_HISTORY_SESSION").unwrap_or_else(|_| "default".to_string());
            let context = llm::last_command_context(&conn, &session_id);
            let prompt = llm::build_question_console_prompt(question, context.as_ref());

            let color = stderr_is_tty();
            eprintln!("{}", format_thinking_message(color));

            let client = llm::OllamaClient::new(llm_cfg);
            let raw = match client.prompt(&prompt) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            };
            let (prose, suggestions) = llm::split_question_answer(&raw);
            let (answer_block, suggestion_lines) = format_ask_output(&prose, &suggestions, color);
            eprintln!("{}", answer_block);

            let mut chosen: Option<&str> = None;
            if !suggestions.is_empty() {
                for line in &suggestion_lines {
                    eprintln!("{}", line);
                }
                eprint!("Choose [1-{}], Enter to skip: ", suggestions.len());
                std::io::stderr().flush().ok();
                let mut input = String::new();
                if std::io::stdin().read_line(&mut input).is_ok()
                    && let Ok(n) = input.trim().parse::<usize>()
                    && n >= 1
                    && n <= suggestions.len()
                {
                    chosen = Some(&suggestions[n - 1]);
                }
            }
            if let Some(cmd) = chosen {
                println!("{}", cmd);
            }

            // Persist the same way the TUI's `App::stage_question`
            // does, so `?`-mode history search and
            // `project report`'s question sections behave
            // identically regardless of whether the question was
            // asked from the console or the TUI.
            let directory =
                crate::util::canonicalize_directory(&env::var("PWD").unwrap_or_default());
            let query_command = format!("{}{}", cfg.query_prefixes().question, question);
            let insert_result: anyhow::Result<i64> = (|| {
                conn.execute(
                    "INSERT INTO history (command, directory, session_id, exit_code, mode) \
                     VALUES (?1, ?2, ?3, -1, 'question') \
                     ON CONFLICT (command, directory, session_id) DO UPDATE \
                     SET timestamp = (strftime('%s', 'now')), mode = 'question'",
                    params![&query_command, &directory, &session_id],
                )?;
                let id: i64 = conn.query_row(
                    "SELECT id FROM history WHERE command = ?1 AND directory = ?2 AND session_id = ?3",
                    params![&query_command, &directory, &session_id],
                    |row| row.get(0),
                )?;
                Ok(id)
            })();
            if let Ok(history_id) = insert_result {
                conn.execute(
                    "INSERT INTO history_output (history_id, output) VALUES (?1, ?2) \
                     ON CONFLICT (history_id) DO UPDATE SET output = excluded.output, captured_at = (strftime('%s', 'now'))",
                    params![history_id, &raw.trim()],
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
            prefix,
            ansi,
        } => {
            // `--no-highlight` is the legacy way to disable styling
            // entirely; treat it as `AnsiMode::Off` so the two flags
            // compose cleanly. When both `--no-highlight` and
            // `--ansi=off` are given, the explicit `--ansi` value
            // wins (clap parses them independently and we just
            // forward `ansi` here, so this is the natural fallthrough).
            let ansi = if no_highlight { AnsiMode::Off } else { ansi };
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
                prefix,
            );
            sql.push_str(&where_clause);

            append_order_and_limit(&mut sql, limit.unwrap_or(100));

            let raw_names: Vec<String> = raw_fields.clone();
            let mut stmt = conn.prepare(&sql)?;
            let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

            let rows = stmt.query_map(
                &params_ref[..],
                move |row| -> Result<Vec<(String, String)>, rusqlite::Error> {
                    let row_data: Vec<(String, String)> = raw_names
                        .iter()
                        .enumerate()
                        .map(|(i, name)| (name.clone(), cell_to_string(row, i)))
                        .collect();
                    Ok(row_data)
                },
            )?;

            let mut out_rows: Vec<Vec<String>> = Vec::new();
            for row in rows {
                let raw_row = row?;
                out_rows.push(project_row(
                    &raw_row,
                    &selected_fields,
                    &derived,
                    query_ref,
                    ansi,
                ));
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
            ansi,
        } => {
            // Same `--no-highlight` / `--ansi` composition as the
            // `Search` arm above.
            let ansi = if no_highlight { AnsiMode::Off } else { ansi };
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
                false,
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
                out_rows.push(project_row(
                    &raw_row,
                    &selected_fields,
                    &derived,
                    query_ref,
                    ansi,
                ));
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
            let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

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

            // Delete captured output first (explicit cascade — SQLite
            // doesn't enforce `ON DELETE CASCADE` unless `PRAGMA
            // foreign_keys = ON` is issued, which this connection
            // never does). Without this, a row deleted here can free
            // up its `id` for reuse (the `history.id` column has no
            // `AUTOINCREMENT`), and the orphaned `history_output` row
            // at that same id would resurface as the captured output
            // of a later, unrelated command — most concerning for the
            // exact "scrub a command containing a secret" workflow
            // this subcommand exists for.
            let output_delete_sql = format!(
                "DELETE FROM history_output WHERE history_id IN (SELECT id FROM history{})",
                where_clause
            );
            conn.execute(&output_delete_sql, &params_ref[..])?;

            let delete_sql = format!("DELETE FROM history{}", where_clause);
            let deleted = conn.execute(&delete_sql, &params_ref[..])?;
            println!(
                "Deleted {} entr{}.",
                deleted,
                if deleted == 1 { "y" } else { "ies" }
            );
        }
        Commands::Init { shell } => {
            let snippet = match shell.as_str() {
                "zsh" => include_str!("init.zsh"),
                "bash" => include_str!("init.bash"),
                _ => anyhow::bail!(
                    "unsupported shell: {}. Supported: 'zsh', 'bash'.",
                    shell
                ),
            };
            let session_id = generate_uuid_v4();
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
            let rows = stmt.query_map(
                [],
                move |row| -> Result<Vec<(String, String)>, rusqlite::Error> {
                    let row_data: Vec<(String, String)> = raw_names
                        .iter()
                        .enumerate()
                        .map(|(i, name)| (name.clone(), cell_to_string(row, i)))
                        .collect();
                    Ok(row_data)
                },
            )?;

            let mut out_rows: Vec<Vec<String>> = Vec::new();
            for row in rows {
                let raw_row = row?;
                out_rows.push(project_row(
                    &raw_row,
                    &selected_fields,
                    &derived,
                    None,
                    AnsiMode::Bold,
                ));
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
        Commands::PaneExec => {
            // Herdr first, then tmux — same precedence
            // `_smarthistory_precmd` uses for capture.
            let current_name: Option<String> = {
                #[cfg(feature = "herdr")]
                {
                    crate::multiplexer::herdr_current_workspace_label()
                }
                #[cfg(not(feature = "herdr"))]
                {
                    None
                }
            }
            .or_else(|| {
                if env::var("TMUX").is_err() {
                    return None;
                }
                std::process::Command::new("tmux")
                    .args(["display-message", "-p", "#S"])
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
            })
            .filter(|s| !s.is_empty());

            let Some(current_name) = current_name else {
                eprintln!("smarthistory: not inside a tmux session or herdr workspace");
                std::process::exit(1);
            };

            let cfg = Config::load_tui();
            match resolve_pane_exec(&cfg, &current_name) {
                PaneExecTarget::Run(cmd) => {
                    let status = std::process::Command::new("sh").arg("-c").arg(&cmd).status();
                    match status {
                        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
                        Err(e) => {
                            eprintln!("smarthistory: failed to run {:?}: {}", cmd, e);
                            std::process::exit(1);
                        }
                    }
                }
                PaneExecTarget::NoExecConfigured => {
                    println!(
                        "smarthistory: session {:?} has no configured exec command",
                        current_name
                    );
                }
                PaneExecTarget::NotFound => {
                    eprintln!(
                        "smarthistory: no session/host config entry named {:?}; nothing to run",
                        current_name
                    );
                    std::process::exit(1);
                }
            }
        }
        Commands::CreateNote { title, content } => {
            run_tui_command(
                conn,
                None,
                None,
                CompletionPickerArgs {
                    glob_complete: None,
                    glob_complete_dir: None,
                    pid_complete: None,
                    root: None,
                },
                true,
                None,
                None,
                None,
                None,
                true,
                Some((title.unwrap_or_default(), content.unwrap_or_default())),
            )?;
        }
        Commands::Capture { command } => {
            let cfg = Config::load();
            let joined = command.join(" ");
            let max_lines = cfg.capture_lines_for(&joined);
            let (command_str, exit_code, output) = capture_command_output(&command, max_lines)?;

            // Echo the command output to the terminal so capture feels
            // like a normal execution.
            print!("{}", output);
            if !output.is_empty() && !output.ends_with('\n') {
                println!();
            }

            let pwd = crate::util::current_directory_for_storage();
            let session_id =
                env::var("SMART_HISTORY_SESSION").unwrap_or_else(|_| "default".to_string());
            let history_id = upsert_history_row(&conn, &command_str, &pwd, &session_id, exit_code)?;
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
            let history_id = upsert_history_row(&conn, &command, &pwd, &session_id, exit_code)?;
            store_output(&conn, history_id, &output)?;
        }
        Commands::CaptureHerdr { command, exit_code } => {
            // Read the herdr pane scrollback via
            // `herdr pane read <pane_id> --source recent-unwrapped
            // --lines <N>` and extract the command
            // line + output using the same pipeline as
            // `capture-tmux`. The pane id comes from the
            // `HERDR_PANE_ID` env var (set by herdr in
            // every pane process).
            let pane_id = env::var("HERDR_PANE_ID").unwrap_or_default();
            if pane_id.is_empty() {
                // Not inside a herdr pane — fall back to
                // a plain `add` so the history entry is
                // still recorded.
                let pwd = crate::util::current_directory_for_storage();
                let session_id =
                    env::var("SMART_HISTORY_SESSION").unwrap_or_else(|_| "default".to_string());
                upsert_history_row(&conn, &command, &pwd, &session_id, exit_code)?;
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
                    None => 500,       // broad request for unlimited capture
                };
                let pane_output = std::process::Command::new("herdr")
                    .args([
                        "pane",
                        "read",
                        &pane_id,
                        "--ansi",
                        "--source",
                        "recent-unwrapped",
                        "--lines",
                        &read_lines.to_string(),
                    ])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .output();
                match pane_output {
                    Ok(o) if !o.stdout.is_empty() => {
                        let text = String::from_utf8_lossy(&o.stdout);
                        let lines: Vec<String> = text.lines().map(strip_ansi).collect();
                        extract_pane_output(&command, &lines, max).unwrap_or_else(|_| {
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
                            } else {
                                end
                            };
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
            let pwd = crate::util::current_directory_for_storage();
            let session_id =
                env::var("SMART_HISTORY_SESSION").unwrap_or_else(|_| "default".to_string());
            let history_id = upsert_history_row(&conn, &command, &pwd, &session_id, exit_code)?;
            store_output(&conn, history_id, &output)?;
        }
        Commands::Project { action } => match action {
            ProjectAction::Report {
                day,
                project,
                min_duration,
            } => {
                let cfg = Config::load();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let (range_start, range_end, date) = parse_project_report_day(&day)?;
                let min_duration = min_duration.unwrap_or(0);

                let sessions =
                    project_sessions_in_range(&conn, range_start, range_end, now)?;
                let commands = report_command_rows(
                    &conn,
                    range_start,
                    range_end,
                    cfg.project_idle_threshold_secs,
                )?;

                // Notes created during a tracked window, bucketed by
                // whichever project's interval contains their
                // `created` timestamp. Notes outside every interval
                // (or with no `created` timestamp) aren't shown —
                // "untracked" is a bucket for command time, not for
                // notes with no project to attribute them to.
                let mut notes_by_slug: std::collections::BTreeMap<String, Vec<String>> =
                    std::collections::BTreeMap::new();
                if let Some(db_path) = cfg.notes_database() {
                    let service = note_search::database_service::DatabaseService::new(
                        &db_path.display().to_string(),
                    );
                    let criteria = note_search::SearchCriteria {
                        list_only: true,
                        ..Default::default()
                    };
                    match service.search_notes(&criteria) {
                        Ok(notes) => {
                            for note in notes {
                                let Some(created) = note.created else {
                                    continue;
                                };
                                if created < range_start || created >= range_end {
                                    continue;
                                }
                                let hit = sessions.iter().find(|s| {
                                    created >= s.start_ts
                                        && (s.still_open || created < s.effective_end)
                                });
                                if let Some(session) = hit {
                                    notes_by_slug
                                        .entry(session.slug.clone())
                                        .or_default()
                                        .push(note.filename);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("warning: notes lookup failed, skipping notes section: {e}");
                        }
                    }
                } else {
                    eprintln!(
                        "warning: notes.database is not configured; skipping notes section"
                    );
                }

                // Website visits: browser bookmarks/history in range,
                // plus `-` mode's JIRA REST visits (which land in
                // `history.command` as `open "<browse_url>"`, not a
                // separate table — see `resolve_project_for_website_visit`'s
                // doc comment). Each visit is resolved through the
                // full 3-tier priority and, independently, clustered
                // for display via `weburlgroup`. The JIRA client is
                // built once (`None` when JIRA isn't configured, in
                // which case tier 1 is simply skipped for every
                // visit) and the label cache is shared across every
                // visit so a ticket referenced by multiple visits
                // costs one REST round-trip, not one per visit.
                let jira_client: Option<Box<dyn crate::jira::JiraClient>> =
                    crate::jira::JiraConfig::from_env()
                        .map(|c| Box::new(crate::jira::RestJiraClient::new(c)) as Box<_>);
                let mut label_cache: std::collections::HashMap<String, Vec<String>> =
                    std::collections::HashMap::new();

                // `resolve_text` is what tier 1/2 resolution scans
                // (a raw command still needs its embedded issue key
                // / URL substring findable — see
                // `resolve_project_for_website_visit`'s doc comment);
                // `url`/`title` are the clean values used for
                // display, clustering, and dedup, which a raw
                // `open "<url>"` command string is not.
                struct WebsiteVisit {
                    resolve_text: String,
                    url: String,
                    title: String,
                    timestamp: i64,
                }
                let mut visits: Vec<WebsiteVisit> = Vec::new();
                let browser_sources = crate::browser::resolve_configured();
                for entry in crate::browser::read_all_entries(&browser_sources) {
                    if entry.timestamp < range_start || entry.timestamp >= range_end {
                        continue;
                    }
                    let title = if entry.title.is_empty() {
                        entry.url.clone()
                    } else {
                        entry.title.clone()
                    };
                    visits.push(WebsiteVisit {
                        resolve_text: entry.url.clone(),
                        url: entry.url,
                        title,
                        timestamp: entry.timestamp,
                    });
                }
                for c in &commands {
                    if let Some(key) = crate::jira::extract_issue_key(&c.command)
                        && let Some(url) = extract_quoted_url(&c.command)
                    {
                        visits.push(WebsiteVisit {
                            resolve_text: c.command.clone(),
                            url: url.to_string(),
                            title: key.to_string(),
                            timestamp: c.timestamp,
                        });
                    }
                }

                let mut websites_by_slug: std::collections::BTreeMap<Option<String>, Vec<WebsiteLink>> =
                    std::collections::BTreeMap::new();
                for visit in &visits {
                    let slug = resolve_project_for_website_visit(
                        &cfg,
                        jira_client.as_deref(),
                        &mut label_cache,
                        &visit.resolve_text,
                        visit.timestamp,
                        &sessions,
                    );
                    // Auto-cluster by host (`github.com`, stripped of
                    // a leading `www.`) when no `weburlgroup.<name>.match`
                    // override applies — every visit ends up in some
                    // cluster, not just the ones an admin thought to
                    // configure ahead of time.
                    let cluster = cluster_label_for_url(&cfg, &visit.url)
                        .unwrap_or_else(|| url_host(&visit.url).to_string());
                    websites_by_slug.entry(slug).or_default().push(WebsiteLink {
                        cluster,
                        title: visit.title.clone(),
                        url: visit.url.clone(),
                    });
                }

                let files_by_slug = report_file_events(&conn, range_start, range_end)?;

                // Slugs to report on, in a stable order: explicit
                // `--project` narrows to just that one; otherwise
                // every slug seen in either the session intervals or
                // the command rows, plus a trailing "untracked"
                // bucket for command rows with no resolved project.
                let mut slugs: Vec<String> = Vec::new();
                if let Some(p) = project.as_ref() {
                    slugs.push(p.clone());
                } else {
                    let mut seen = std::collections::BTreeSet::new();
                    for s in &sessions {
                        seen.insert(s.slug.clone());
                    }
                    for c in &commands {
                        if let Some(slug) = &c.project_slug {
                            seen.insert(slug.clone());
                        }
                    }
                    for slug in websites_by_slug.keys().flatten() {
                        seen.insert(slug.clone());
                    }
                    for slug in files_by_slug.keys().flatten() {
                        seen.insert(slug.clone());
                    }
                    slugs.extend(seen);
                }

                println!("# Project Report — {}", date.format("%Y-%m-%d"));

                // One (slug, rows) pair per section — `None` is the
                // trailing "untracked" bucket. Built once, up front,
                // so the summary table below and each section's own
                // total (`print_project_report_section` re-sums the
                // same `rows` slice) can never drift apart.
                let mut sections: Vec<(Option<String>, Vec<&ReportCommandRow>)> = slugs
                    .iter()
                    .map(|slug| {
                        let rows: Vec<&ReportCommandRow> = commands
                            .iter()
                            .filter(|c| c.project_slug.as_deref() == Some(slug.as_str()))
                            .collect();
                        (Some(slug.clone()), rows)
                    })
                    .collect();
                if project.is_none() {
                    let untracked: Vec<&ReportCommandRow> = commands
                        .iter()
                        .filter(|c| c.project_slug.is_none())
                        .collect();
                    let untracked_websites = websites_by_slug.get(&None);
                    let untracked_files = files_by_slug.get(&None).is_some_and(|g| {
                        !g.viewed.is_empty() || !g.modified.is_empty() || !g.created.is_empty()
                    });
                    if !untracked.is_empty()
                        || untracked_websites.is_some_and(|v| !v.is_empty())
                        || untracked_files
                    {
                        sections.push((None, untracked));
                    }
                }

                println!("\n## Summary");
                println!("| Project | Active Time |");
                println!("| --- | --- |");
                for (slug, rows) in &sections {
                    let label = slug.as_deref().unwrap_or("untracked");
                    let total: i64 = rows.iter().map(|r| r.active_secs).sum();
                    println!(
                        "| {} | {} |",
                        escape_md_table_cell(label),
                        format_duration_secs(total)
                    );
                }

                for (slug, rows) in &sections {
                    let label = slug.as_deref().unwrap_or("untracked");
                    print_project_report_section(label, rows, min_duration);
                    if let Some(notes) = slug.as_ref().and_then(|s| notes_by_slug.get(s)) {
                        println!("\n### Notes created");
                        for n in notes {
                            println!("- [[{}]]", note_basename(n));
                        }
                    }
                    let empty_file_groups = FileEventGroups::default();
                    let file_groups = files_by_slug.get(slug).unwrap_or(&empty_file_groups);
                    print_file_events_section("viewed", &file_groups.viewed);
                    print_file_events_section("modified", &file_groups.modified);
                    print_file_events_section("created", &file_groups.created);
                    println!("\n### Websites");
                    match websites_by_slug.get(slug) {
                        Some(items) if !items.is_empty() => print_website_section(items),
                        _ => println!("(none)"),
                    }
                    println!();
                }
            }
            ProjectAction::Select { slug } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                conn.execute(
                    "INSERT INTO project_current (id, project_slug, set_ts) VALUES (1, ?1, ?2)
                     ON CONFLICT (id) DO UPDATE SET project_slug = excluded.project_slug, set_ts = excluded.set_ts",
                    params![slug, now],
                )?;
                let cfg = Config::load();
                switch_project(
                    &conn,
                    Some(&slug),
                    now,
                    cfg.project_idle_threshold_secs,
                    Some("switch"),
                )?;
                eprintln!("smarthistory: current project set to {slug:?}");
            }
            ProjectAction::Current => {
                let cfg = Config::load();
                let pwd = crate::util::current_directory_for_storage();
                match resolve_current_project(&conn, &cfg, &pwd)? {
                    Some(slug) => println!("{slug}"),
                    None => {
                        eprintln!("smarthistory: no active project");
                        std::process::exit(1);
                    }
                }
            }
            ProjectAction::Pause => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                if is_project_tracking_paused(&conn)? {
                    // Resume: restore the snapshot taken when we
                    // paused, then discard it — `switch_project` is
                    // called with `forced_reason = Some("switch")`
                    // even when `paused_slug` is `None`, so a still-
                    // open session from some OTHER project (opened
                    // by a plain `smarthistory add` racing with this
                    // resume) still gets closed correctly rather
                    // than silently left open past the resume point.
                    let paused_slug: Option<String> = conn.query_row(
                        "SELECT paused_slug FROM project_pause WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )?;
                    conn.execute("DELETE FROM project_pause WHERE id = 1", [])?;
                    let cfg = Config::load();
                    switch_project(
                        &conn,
                        paused_slug.as_deref(),
                        now,
                        cfg.project_idle_threshold_secs,
                        Some("switch"),
                    )?;
                    match paused_slug {
                        Some(slug) => eprintln!("smarthistory: project tracking resumed: {slug:?}"),
                        None => eprintln!("smarthistory: project tracking resumed (no project was active)"),
                    }
                } else {
                    // Pause: snapshot whatever project is active
                    // right now (same resolution `smarthistory add`
                    // uses), then close it — `switch_project(...,
                    // None, ...)` closes the open session without
                    // opening a replacement.
                    let cfg = Config::load();
                    let pwd = crate::util::current_directory_for_storage();
                    let active = resolve_current_project(&conn, &cfg, &pwd)?;
                    switch_project(
                        &conn,
                        None,
                        now,
                        cfg.project_idle_threshold_secs,
                        Some("paused"),
                    )?;
                    conn.execute(
                        "INSERT INTO project_pause (id, paused_slug, paused_at) VALUES (1, ?1, ?2)",
                        params![active, now],
                    )?;
                    match &active {
                        Some(slug) => eprintln!("smarthistory: project tracking paused (was: {slug:?})"),
                        None => eprintln!("smarthistory: project tracking paused (no project was active)"),
                    }
                }
            }
            ProjectAction::Files => {
                use rusqlite::OptionalExtension;
                let open: Option<(String, i64)> = conn
                    .query_row(
                        "SELECT project_slug, start_ts FROM project_sessions WHERE end_ts IS NULL",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let Some((slug, start_ts)) = open else {
                    eprintln!("smarthistory: no active project session");
                    std::process::exit(1);
                };
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                // `report_file_events`'s range is a half-open
                // `[start, end)` — `now + 1` so an event recorded in
                // this same second is still included.
                let by_slug = report_file_events(&conn, start_ts, now + 1)?;
                let empty = FileEventGroups::default();
                let groups = by_slug.get(&Some(slug.clone())).unwrap_or(&empty);
                let started = chrono::DateTime::from_timestamp(start_ts, 0)
                    .map(|dt| {
                        dt.with_timezone(&chrono::Local)
                            .format("%H:%M:%S")
                            .to_string()
                    })
                    .unwrap_or_else(|| start_ts.to_string());
                println!("# {slug} — session started {started}");
                print_file_events_section("viewed", &groups.viewed);
                print_file_events_section("modified", &groups.modified);
                print_file_events_section("created", &groups.created);
            }
        },
        Commands::File { action } => {
            let (kind, path) = match &action {
                FileAction::Viewed { path } => ("viewed", path),
                FileAction::Modified { path } => ("modified", path),
                FileAction::Created { path } => ("created", path),
            };
            // Reused despite the name — `canonicalize_directory` is
            // just `std::fs::canonicalize` with a same-string
            // fallback on failure, which works identically for a
            // file path as for a directory.
            let canonical = crate::util::canonicalize_directory(path);
            let dir = std::path::Path::new(&canonical)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let cfg = Config::load();
            let project_slug = resolve_current_project(&conn, &cfg, &dir)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            conn.execute(
                "INSERT INTO file_events (path, event_kind, project_slug, timestamp) VALUES (?1, ?2, ?3, ?4)",
                params![canonical, kind, project_slug, now],
            )?;
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
                    "dropdown.enabled" => {
                        println!("{}", if cfg.dropdown_enabled { "on" } else { "off" })
                    }
                    "dropdown.limit" => println!("{}", cfg.dropdown_limit),
                    "dropdown.minchars" => println!("{}", cfg.dropdown_min_chars),
                    "segments.minwords" => println!("{}", cfg.segments_min_words),
                    "dropdown.highlight" => {
                        println!("{}", if cfg.dropdown_highlight { "on" } else { "off" })
                    }
                    "dropdown.matchmode" => println!("{}", cfg.dropdown_matchmode),
                    "commentexpand.enabled" => {
                        println!("{}", if cfg.commentexpand_enabled { "on" } else { "off" })
                    }
                    "globcomplete.enabled" => {
                        println!("{}", if cfg.globcomplete_enabled { "on" } else { "off" })
                    }
                    "zsh.mode" => println!("{}", cfg.zsh_default_mode),
                    // The resolved `?` question-mode prefix
                    // character. Cached once at shell-init time so
                    // the `accept-line` widget can recognize a
                    // `?question<Enter>` line without a
                    // `smarthistory config get` round-trip on every
                    // keypress.
                    "prefix.question" => println!("{}", cfg.query_prefixes().question),
                    // Resolved palette as a flat `key=value` block,
                    // one entry per `tuicolor.<field>` slot. The
                    // widget reads this once at init time and
                    // converts each value to an ANSI SGR sequence
                    // (CSS name / 16-color name / `#rrggbb` hex).
                    // Same shape as the `tuicolor.<field>=…` lines
                    // the user writes in the config file, so the
                    // widget's parser doesn't need a second
                    // format. `scheme` is whatever the user's last
                    // TUI session actually had active
                    // (`TuiSession::persisted_scheme`, the
                    // `colorscheme=` line in the session file,
                    // toggled via `Action::ToggleColorScheme`),
                    // falling back to `ColorScheme::default()`
                    // (`Dark`) when there's no session file yet or
                    // it has no scheme recorded.
                    "palette" => {
                        let scheme = crate::tui::TuiSession::persisted_scheme()
                            .unwrap_or_default();
                        for (key, value) in cfg.resolved_palette(scheme) {
                            println!("{key}={value}");
                        }
                    }
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
        Commands::Tui {
            mode,
            prefix,
            glob_complete,
            glob_complete_dir,
            pid_complete,
            root,
            exec,
            query,
            pane,
            panes_filter,
            pane_height,
            create_note,
        } => {
            run_tui_command(
                conn,
                mode,
                prefix,
                CompletionPickerArgs {
                    glob_complete,
                    glob_complete_dir,
                    pid_complete,
                    root,
                },
                exec,
                query,
                pane,
                panes_filter,
                pane_height,
                create_note,
                None,
            )?;
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
            let json = std::fs::read_to_string(&filename)?;
            let export: HistoryExport = serde_json::from_str(&json)?;

            if export.version != 1 {
                anyhow::bail!("Unsupported export version {}; expected 1", export.version);
            }

            let (imported, updated) = import_history_rows(&conn, &export.history)?;

            eprintln!(
                "Imported {} new entries, updated {} existing entries from {}",
                imported,
                updated,
                filename.display()
            );
        }
        Commands::Prune { days, force } => {
            // Compute the cutoff timestamp: now - days*86400.
            // Uses the same epoch-seconds convention as the
            // `timestamp` column in the history table.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let cutoff = now - (days as i64) * 86_400;

            // Count the rows that will be deleted (for the
            // confirmation message).
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM history WHERE timestamp < ?1",
                params![cutoff],
                |row| row.get::<_, i64>(0),
            )?;

            if n == 0 {
                println!(
                    "No entries older than {} day{}; nothing to prune.",
                    days,
                    if days == 1 { "" } else { "s" }
                );
                return Ok(());
            }

            if !force
                && !confirm(&format!(
                    "Prune {} entr{} older than {} day{}? [y/N] ",
                    n,
                    if n == 1 { "y" } else { "ies" },
                    days,
                    if days == 1 { "" } else { "s" }
                ))
            {
                println!("Aborted.");
                return Ok(());
            }

            // Delete captured output first (explicit cascade —
            // SQLite doesn't enforce FK constraints by default).
            let output_deleted = conn.execute(
                "DELETE FROM history_output WHERE history_id IN \
                 (SELECT id FROM history WHERE timestamp < ?1)",
                params![cutoff],
            )?;
            // Delete the history rows.
            let deleted =
                conn.execute("DELETE FROM history WHERE timestamp < ?1", params![cutoff])?;
            // Delete orphaned comments (command_comments entries
            // whose command text no longer appears in any history
            // row after the prune).
            let comments_deleted = conn.execute(
                "DELETE FROM command_comments WHERE command NOT IN \
                 (SELECT command FROM history)",
                [],
            )?;
            println!(
                "Pruned {} entr{} older than {} day{} ({} output row{}, {} orphaned comment{}).",
                deleted,
                if deleted == 1 { "y" } else { "ies" },
                days,
                if days == 1 { "" } else { "s" },
                output_deleted,
                if output_deleted == 1 { "" } else { "s" },
                comments_deleted,
                if comments_deleted == 1 { "" } else { "s" },
            );
        }
        Commands::PruneDirectories { force } => {
            let cfg = Config::load_tui();
            let stale: Vec<(String, String, String)> = cfg
                .session_directories()
                .into_iter()
                .filter(|(_, _, dir)| !std::path::Path::new(dir).is_dir())
                .collect();

            if stale.is_empty() {
                println!("No stale directory entries found.");
                return Ok(());
            }

            println!(
                "The following session director{} no longer exist{}:",
                if stale.len() == 1 { "y" } else { "ies" },
                if stale.len() == 1 { "s" } else { "" }
            );
            for (_, name, dir) in &stale {
                println!("  {} ({})", name, dir);
            }

            if !force
                && !confirm(&format!(
                    "Remove {} entr{}? [y/N] ",
                    stale.len(),
                    if stale.len() == 1 { "y" } else { "ies" },
                ))
            {
                println!("Aborted.");
                return Ok(());
            }

            let keys: std::collections::HashSet<String> =
                stale.iter().map(|(key, _, _)| key.clone()).collect();
            let mut removed = 0usize;
            for path in [sessions_path(), config_path()].into_iter().flatten() {
                removed += remove_session_lines(&path, &keys)?;
            }
            println!(
                "Removed {} stale directory entr{} from configuration ({} line{}).",
                stale.len(),
                if stale.len() == 1 { "y" } else { "ies" },
                removed,
                if removed == 1 { "" } else { "s" },
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
                .map_err(|e| anyhow::anyhow!("prepare: {e}"))?;
            let mut updates: Vec<(i64, String)> = Vec::new();
            let mut rows = stmt.query([]).map_err(|e| anyhow::anyhow!("query: {e}"))?;
            while let Some(row) = rows.next().map_err(|e| anyhow::anyhow!("row: {e}"))? {
                let id: i64 = row.get(0).map_err(|e| anyhow::anyhow!("id: {e}"))?;
                let directory: String =
                    row.get(1).map_err(|e| anyhow::anyhow!("directory: {e}"))?;
                let shortened = crate::util::expand_home_with_config(&directory, cfg.home_map());
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
                        eprintln!("warning: failed to read history id={id}: {e}");
                        skipped += 1;
                        continue;
                    }
                };
                // Cascade the captured output of the row(s) we're
                // about to collide-delete (SQLite doesn't enforce
                // `ON DELETE CASCADE` here — see the matching
                // comment in `Commands::Clean`), otherwise an
                // orphaned `history_output` row can resurface under
                // a later, unrelated command that reuses the freed
                // `id`.
                if let Err(e) = conn.execute(
                    "DELETE FROM history_output WHERE history_id IN \
                     (SELECT id FROM history \
                      WHERE command = ?1 \
                        AND directory = ?2 \
                        AND session_id = ?3 \
                        AND id != ?4)",
                    rusqlite::params![cmd, new_dir, sid, id],
                ) {
                    eprintln!("warning: failed to clear collision output for id={id}: {e}");
                    skipped += 1;
                    continue;
                }
                if let Err(e) = conn.execute(
                    "DELETE FROM history \
                     WHERE command = ?1 \
                       AND directory = ?2 \
                       AND session_id = ?3 \
                       AND id != ?4",
                    rusqlite::params![cmd, new_dir, sid, id],
                ) {
                    eprintln!("warning: failed to clear collision for id={id}: {e}");
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
                        eprintln!("warning: failed to rewrite history id={id}: {e}");
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
        Commands::Check { prefix } => {
            tui::run_tui_check(prefix, false)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
