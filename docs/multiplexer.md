# Multiplexer support (tmux + herdr)

The TUI's directory-switching (`#` mode), pane-switching (`*` mode), and host-bootstrapping flows all need to talk to a terminal multiplexer: to discover which directories are currently active, to focus an existing pane or workspace, and to create a new one rooted at a given directory. The integration is abstracted behind a `MultiplexerBackend` trait with **two implementations** — [tmux](https://github.com/tmux/tmux) (default, always available) and [herdr](https://herdr.dev) (opt-in via the `herdr` Cargo feature, enabled by default).

This page documents:

- [Supported backends](#supported-backends)
- [Picking a backend](#picking-a-backend)
- [Building with herdr support](#building-with-herdr-support)
- [How the integration works](#how-the-integration-works)
- [What flows use the backend](#what-flows-use-the-backend)
- [Output capture](#output-capture)
- [Setup guides](#setup-guides)
- [Troubleshooting](#troubleshooting)

## Supported backends

| Backend | Default | Pane id shape | Workspace id | Binary required |
| --- | --- | --- | --- | --- |
| [tmux](https://github.com/tmux/tmux) | yes | `%N` (globally unique on the local server) | session name | `tmux` on `PATH` |
| [herdr](https://herdr.dev) | opt-in (default feature) | `w1:p1` (workspace-scoped) | positional integer (`1`, `2`, …) | `herdr` on `PATH` |

Both backends produce the same data model (an "active context" = a pane with a `cwd`); the differences are in id conventions and in which CLI commands do the focus / create / send-text work.

## Picking a backend

The active backend is selected via a single config key:

```ini
# ~/.config/smarthistory/config
multiplexer=herdr   # default: tmux
```

The same key is also accepted from the `SMARTHISTORY_MULTIPLEXER` environment variable, which wins over the config file (matching the `NOTE_SEARCH_*` / `JIRA_*` precedence pattern). Unrecognised values are dropped with a stderr warning; the default (`tmux`) is preserved so a typo can't silently disable directory switching:

```
$ SMARTHISTORY_MULTIPLEXER=foo smarthistory tui
smarthistory: ignoring invalid SMARTHISTORY_MULTIPLEXER="foo" (expected `tmux` or `herdr`)
```

A successful `smarthistory config check` prints the active backend in the *Effective values* section:

```
  multiplexer = tmux
```

## Building with herdr support

The `herdr` Cargo feature is **enabled by default** so a fresh `cargo build --release` (or `cargo install`) produces a binary that can target either backend without extra flags. The feature exists for users who want a smaller tmux-only binary:

```bash
# Default build (herdr + tmux, both backends)
cargo build --release

# Smaller tmux-only build
cargo build --release --no-default-features
```

If a user has `multiplexer=herdr` in their config but the binary was built with `--no-default-features`, the parser accepts the value (so the config doesn't silently fail to load) but every directory / pane action that needs the backend surfaces a clear "build with default features" status message instead of staging a broken command. The `multiplexer=herdr` config key is accepted in both builds (so a hand-edited config doesn't suddenly break a no-herdr binary); the runtime just degrades to a status message instead of a tmux-style command.

## How the integration works

The TUI's `MultiplexerBackend` trait (in [`src/multiplexer.rs`](../src/multiplexer.rs)) has five methods, and both backends implement them the same way:

| Method | tmux backend | herdr backend |
| --- | --- | --- |
| `snapshot()` | `tmux list-windows -a -F` (active panes only) | `herdr workspace list` |
| `snapshot_current_panes()` | `tmux list-panes -s -F` (current session, current pane excluded) | `herdr pane list` (all panes; current-pane exclusion is best-effort) |
| `focus_command(id)` | `tmux select-pane -t <id> && tmux switch-client -t <id>` | `herdr workspace focus <id>` |
| `create_command(dir, label)` | `tmux new-session -d -s <label> -c <dir>; tmux switch-client -t <label>` | `herdr workspace create --cwd <dir> --label <label> --focus` |
| `send_in_pane_command(id, body)` | `tmux send-keys -t <id> <body> Enter` | `herdr pane send-text <id> <body>` |

Both backends are silent on failure: `tmux` / `herdr` missing from `PATH`, no server running, or a subprocess hanging past `TMUX_PANE_PROBE_TIMEOUT_MS` (default 1000 ms) all leave the snapshot empty and the marker invisible. The TUI never blocks on the multiplexer for more than that.

## What flows use the backend

### `#` mode (directories) — see [`docs/modes/directories.md`](modes/directories.md)

The `#` view lists every directory the shell has ever been in, merged from three sources (history `cwd`s, `sessiondirs=...` config entries, and active multiplexer panes). The `T` marker on a row means at least one multiplexer pane is currently rooted there. When the user picks a row:

- **`T`-marked** → the staged command focuses the existing pane (or its workspace, on herdr): `tmux select-pane -t %N && tmux switch-client -t %N` / `herdr workspace focus <ws>`.
- **unmarked** → the staged command creates a new session / workspace rooted there: `tmux new-session -d -s <label> -c <dir>` / `herdr workspace create --cwd <dir> --label <label> --focus`.

### `*` mode (panes) — see [`docs/modes/panes.md`](modes/panes.md)

The `*` view lists every pane across every session / workspace as a tree (workspace header + child panes indented). Selecting:

- a **pane** row stages the per-pane focus command: `tmux select-pane -t <id> && tmux switch-client -t <id>` / `herdr pane zoom <id> && herdr pane zoom <id> --off` (the second un-zooms while keeping focus on the right pane, so the user lands without a zoomed view).
- a **workspace** header row stages the workspace focus command: `tmux switch-client -t <session>` / `herdr workspace focus <ws>`.

### `# hosts` block (inside `*` mode)

A `# hosts` block at the bottom of the panes view. Each `host.<id>` from the config file (merged with `~/.ssh/config`) becomes a row; selecting a row either focuses an existing workspace already running that host's `ssh` connection, or creates a new workspace and bootstraps the `ssh` body into its first pane.

- **tmux**: matches a pane whose `#{pane_current_command}` starts with `ssh` and contains the host's `user@host`. If found, focuses that pane. Otherwise stages `tmux new-session -d -s <display-name>; tmux switch-client -t <display-name>; tmux send-keys <ssh-argv> Enter`.
- **herdr**: matches a workspace whose `label` equals the host's display name. If found, focuses that workspace. Otherwise stages `herdr workspace create --label <display-name>` and `herdr pane send-text` to send the `ssh` body into the new workspace's first pane.

### `.command` file bootstrap

The `session.<id>.exec` config key (or a per-host equivalent) is the command to bootstrap a project after focus / create. The TUI runs it via `send_in_pane_command`:

- **tmux**: `tmux send-keys -t <id> <body> Enter` — types the body into the existing pane of the new session.
- **herdr**: `herdr pane send-text <id> <body>` — same effect via herdr's pane API.

tmux runs the script *inside* the new session's first command position, so the user lands in a fully-set-up project. herdr's `workspace create` doesn't accept a startup command yet, so the bootstrap is best-effort: the bare create command is staged and the user can re-select the row to retry the bootstrap once the workspace is up.

## Output capture

Output capture is the second half of the "multiplexer support" story. The `+` (output) mode and the `Ctrl-O` (Show output) overlay both read from the `history_output` SQLite table; the precmd hook in `init.zsh` is what populates it. The capture pipeline works for both multiplexers:

- **tmux**: reads the continuous `pipe-pane` log file (`~/.cache/tmux-history/output-<pane>.log`). The hooks in `~/.tmux.conf` write one log per pane (see [Setup guides](#setup-guides) below); the precmd hook reads the most recent N lines on every command exit.
- **herdr**: reads the pane's scrollback buffer via `herdr pane read <pane_id> --source recent-unwrapped`. If the command line has scrolled off the top (common for high-output commands like `ps -ef`), the entire remaining buffer is captured as the best available approximation.

The `capturelines` config key controls the default number of lines captured (default 20). Per-command overrides: `capturelines.ps=ALL` captures every line of `ps` output; `capturelines.less=none` disables capture for `less` (which would otherwise be enormous).

The two are independent: a user who picks `multiplexer=herdr` keeps tmux (or just nothing) for per-pane output capture. The `+` mode keeps working because it reads from the SQLite table populated by whichever precmd hook ran, not from the live multiplexer.

## Setup guides

### tmux hooks (for output capture)

The output-capture feature inside tmux requires hooks in your `~/.tmux.conf` so each pane's output is tee'd to a per-pane log file. The recommended setup (see the tmux section of [`README.md`](../README.md) and the auto-generated `~/.local/bin/log_tmux_pane.sh`):

```sh
# ~/.tmux.conf
set-hook -g after-split-window  'run-shell "~/.local/bin/log_tmux_pane.sh #{pane_id}"'
set-hook -g after-new-window    'run-shell "~/.local/bin/log_tmux_pane.sh #{pane_id}"'
set-hook -g session-created     'run-shell "~/.local/bin/log_tmux_pane.sh #{pane_id}"'
set-hook -g after-kill-pane     'run-shell "~/.local/bin/stop_tmux_pane.sh #{pane_id}"'
```

The `log_tmux_pane.sh` script:

```sh
#!/usr/bin/env bash
PANE=$1
LOG=~/.cache/tmux-history/output-%$PANE.log
touch "$LOG"
tmux pipe-pane -t $PANE "cat >> $LOG"
```

The `stop_tmux_pane.sh` script removes the log file when the pane is killed (so the directory doesn't grow forever).

The log directory is configurable via `tmuxpaneoutputdir=~/path` (default `~/.cache/tmux-history`).

### herdr

[herdr](https://herdr.dev) is a workspace manager for AI-agent / polyglot workflows. Install the CLI:

```sh
# macOS
brew install herdr/tap/herdr

# Linux / via cargo
cargo install herdr-cli
```

Start a server (the `herdr` daemon) and you're ready to go — no hook configuration needed. The `herdr workspace list`, `herdr pane list`, `herdr workspace focus`, `herdr pane send-text`, and `herdr pane read` commands used by the TUI are all stable CLI surface.

## Troubleshooting

### The `T` marker is missing in `#` mode

The marker means "at least one multiplexer pane is currently rooted here". If it's missing everywhere:

- **`tmux`** — check `echo $TMUX` in the shell you launched the TUI from. If empty, you launched outside tmux. The TUI can still *read* existing tmux sessions (`tmux list-windows -a` works across sessions), but it only discovers panes the local tmux server is aware of.
- **`herdr`** — check `herdr workspace list` from the shell. If empty, the daemon isn't running (`herdr daemon start` or however the install starts it).
- **The snapshot is fetched once at TUI start** — it doesn't refresh while you navigate. Restart the TUI to pick up new panes.
- **No rows are `T`-marked at all** — the multiplexer probe timed out (default 1000 ms). Increase `TMUX_PANE_PROBE_TIMEOUT_MS` env var (tmux only).

### `multiplexer=herdr` is ignored

- Run `smarthistory config check` — it prints the resolved `multiplexer` value. If it's still `tmux`, the env var or config-file value is being shadowed.
- The `SMARTHISTORY_MULTIPLEXER` env var wins over the config file. Unset it to use the file value: `unset SMARTHISTORY_MULTIPLEXER`.
- The config-file value is read from the `multiplexer=...` line. A typo (e.g. `multiplexor=herdr`) is silently dropped.
- The binary may have been built with `--no-default-features`. Re-run `cargo build --release` without the flag, or check `smarthistory --version` output for the build features.

### `Enter` on a `#` row stages nothing

- If the row is `T`-marked, the staged command is the focus command for the pane's workspace. Check that `multiplexer=` is set correctly (`smarthistory config check`).
- If the row is unmarked, the staged command is `cd <abs-path>` (plus an optional `tmux new-session` / `herdr workspace create`). If nothing appears, the precmd hook probably never captured any history for that path. Try running a command there first to seed the `history` table.
- The `T`-marked branch on herdr does `herdr workspace focus <id>` rather than per-pane focus — the `pane_id`'s `:pN` suffix is stripped because `workspace focus` accepts a workspace id, not a pane id. So a `T`-marked row in herdr jumps to the workspace the directory lives in, not the specific pane. This is deliberate: herdr's "pane focus" (`pane zoom`) is a zoom toggle, not a permanent focus; the workspace-level focus is what users typically want.

### `+` (output) mode returns nothing

- **tmux**: the `pipe-pane` hooks must be installed (see [Setup guides](#setup-guides)) and the log directory must exist. `ls ~/.cache/tmux-history/output-*.log` — if it's empty, the hooks aren't running.
- **herdr**: the precmd hook calls `herdr pane read` for every command exit. If herdr isn't on `PATH` (the precmd hook falls through silently), `+` mode returns nothing for herdr-sourced commands.
- The output search matches `LIKE` on the captured `output` column. A command with no captured output (e.g. a `[ -n "$TMUX" ]` test) has no `history_output` row.

### herdr binary missing from PATH

The TUI surfaces a `command not found` (or similar) in the status bar. Install herdr (see [Setup guides](#setup-guides)) and restart the TUI.

### I want both multiplexers active

The TUI supports only one active backend at a time (the `multiplexer=` config key). Both tmux and herdr can run on the same machine simultaneously — the backend the TUI *uses* for the `#` / `*` / `# hosts` flows is the one in the config. Output capture (via the precmd hook) is independent and can use either: the precmd hook detects the multiplexer (herdr-first, then tmux) and writes to the same `history_output` table regardless of which one produced the capture.

## Cross-references

- [`docs/modes/directories.md`](modes/directories.md) — the `#` mode that uses the backend for the `T` marker and focus / create staging.
- [`docs/modes/panes.md`](modes/panes.md) — the `*` mode that lists every pane across every workspace.
- [`docs/modes/output.md`](modes/output.md) — the `+` mode that searches the captured-output table.
- [`TECHNICAL.md`](../TECHNICAL.md#multiplexer-backend) — the `MultiplexerBackend` trait / two-backends design notes.
- [`README.md`](../README.md#multiplexer-integration-tmux--herdr) — the high-level overview; this file is the long reference.
- [`src/multiplexer.rs`](../src/multiplexer.rs) — the trait and both backend implementations.
- [`src/init.zsh`](../src/init.zsh) — the precmd hook that captures output for both multiplexers.
