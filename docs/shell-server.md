# Shell server

`smarthistory shell-server` is a small long-running process that answers the
zsh integration's non-interactive calls without exec'ing the `smarthistory`
binary each time.

## Why

With the [live dropdown](configuration.md#dropdownenabled) on, zsh runs
`smarthistory search` on **every keystroke** and redraws the line only after
it returns; `add`/`capture-*` run on every prompt, and the init snippet calls
`config get` about fifteen times at shell startup. From a local disk an exec
costs a few milliseconds. When the binary lives on a network share
(SMB/NFS), every exec revalidates the file and pages it in over the network,
which shows up as typed characters echoing late.

The server is started once and stays resident, so none of those calls touch
the binary file again.

## Lifecycle

- `eval "$(smarthistory init zsh)"` checks for a running server and starts one
  (detached, in its own session) when there is none. If the running server
  comes from a different binary (version, mtime, or size differ — i.e. you
  upgraded), it is stopped and replaced.
- Every shell then talks to that one server through the `zsh/net/socket`
  builtin (`_smarthistory_run` in the snippet).
- If the socket can't be reached (server stopped, `zsh/net/socket` missing),
  the snippet transparently execs the binary instead, exactly as before.
- The server exits on `smarthistory shell-server --stop`, or by itself within
  a minute of its socket file being removed or replaced.

```bash
smarthistory shell-server --status   # "running at <socket> (<identity>)", exit 1 if not
smarthistory shell-server --stop
smarthistory shell-server            # run in the foreground (normally not needed)
```

Disable it with `shellserver.enabled=off` in the config; new shells then exec
the binary for every call. bash has no Unix-socket builtin and always execs.

## What it runs

Only the commands the zsh hooks need: `search`, `next`, `expand`, `add`,
`capture-tmux`, `capture-herdr`, `config`, `project`. Anything interactive
(`tui`, `ask`, `create-note`, …) is refused by the server and always run by
exec'ing the binary, and your own `smarthistory …` invocations at the prompt
are never routed through it.

Each request runs in a fresh process forked from the server, with the calling
shell's working directory and exported environment, so it behaves exactly
like an exec: the config and database are read anew on every call, and the
output, error messages, and exit status are identical.

## Socket

`<temp dir>/smarthistory-<uid>/shell.sock` — `$TMPDIR` on macOS, `/tmp` on
Linux (falling back to `/tmp` if `$TMPDIR` is too long for a socket path).
The temp dir is local disk even when your home directory is on a share; Unix
sockets don't work on SMB mounts. The directory is created with mode `0700`
and the server refuses to use one owned by another user or accessible by
others; the socket itself is `0600`. Override the location with
`SMARTHISTORY_SERVER_SOCKET` (must be set the same for `init` and the server).

## Troubleshooting

- **Still slow?** `smarthistory shell-server --status` — if it says "not
  running", check that `zmodload zsh/net/socket` works in your zsh and that the
  socket directory is private (see above).
- **Changed behavior after an upgrade?** Open a new shell (or run
  `smarthistory shell-server --stop`); the next `init zsh` starts the new
  binary's server.
