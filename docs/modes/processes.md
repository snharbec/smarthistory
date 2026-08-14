# Processes mode (`%`)

| Default prefix | `%`                       |
| -------------- | ------------------------- |
| Configurable   | `prefix.processes=<char>` |

Processes mode lists every running OS process on the machine — macOS and Linux,
via the [`sysinfo`](https://docs.rs/sysinfo) crate — filtered by the typed
query. Every process is shown regardless of who owns it; selecting a row opens a
confirmation dialog to send it a signal.

Nothing to configure — processes mode works out of the box, with no external
binary or service dependency.

## What it does

- `%` (empty) — every running process on the machine.
- `%nginx` — every process whose name/cmdline, working directory, or executable
  path contains `nginx` (substring AND across whitespace-separated tokens, same
  contract as every other mode).
- The primary text is the process's command line (falling back to its bare name
  when the full cmdline can't be read); the details pane shows its working
  directory (`Dir`) and executable path (`Rem`).
- The docked preview pane (and the `Ctrl-O` full overlay) shows the process's
  full environment (`NAME=value`, one per line, sorted), loaded lazily the first
  time a row is selected.

## Selecting a row

`Enter` on a process row does **not** stage or run its command line — it opens a
confirmation dialog:

> Send SIGTERM to pid 1234 (nginx: worker process)?

- `y` / `Y` sends the currently-selected signal and reports success or failure
  in the status line.
- `n` / `N` or the configured Cancel key dismisses the dialog without sending
  anything.
- `Tab` / `Shift-Tab` cycle the signal to send: **SIGTERM** (default) → SIGKILL
  → SIGHUP → SIGINT → back to SIGTERM. The dialog's message updates live as you
  cycle.
- `Ctrl-C` aborts the whole TUI, same as every other confirmation overlay.

Sending a signal to a process you don't own fails with a permission-denied
message in the status line rather than crashing — the same way `kill(1)` itself
would fail.

## Cross-platform notes

`sysinfo` abstracts macOS vs. Linux for process listing,
`cwd()`/`exe()`/`environ()` reads, and signal-sending — there's no
platform-specific code path to be aware of. The one place platform matters is
**permission behavior**, not API shape: reading another (non-owned, non-child)
process's environment can fail on both platforms — macOS's hardened runtime /
SIP restrictions, or Linux's uid/`CAP_SYS_PTRACE` checks on
`/proc/<pid>/environ`. When that happens, the preview shows a placeholder
(`(permission denied — cannot read environment for pid N)`) instead of erroring
out.

## Health check

`smarthistory check --prefix '%'` (or `smarthistory check` with no filter, which
checks every mode) refreshes the full process list and reports how many
processes are visible.

## Cross-references

- [Panes mode — the multiplexer pane view; also not command-history-backed, also not dedup-eligible](panes.md)
- [Directories mode — the history-derived directory view](directories.md)
