# Worktree mode (`;`)

| Default prefix | `;`                      |
| -------------- | ------------------------ |
| Configurable   | `prefix.worktree=<char>` |

Worktree mode lists every `git worktree` checkout for the repo containing the
current directory (`git worktree list --porcelain`), filtered by the typed
query. Selecting a row creates a new tmux session / herdr workspace rooted
there — the exact same staging `#` (Directories) and `~` (Zoxide) modes use
for an unmarked row, including the `T`-marked "jump to an already-active pane
there instead" behavior.

Requires the current directory to be inside a git repository (`git
rev-parse --show-toplevel`) and the `git` binary on `$PATH`. Outside a repo,
the mode is empty rather than an error — same "degrade to no rows" policy
`~` (Zoxide) uses for a missing `zoxide` binary.

This first phase is read-only: list + select to jump into a worktree.
Creating and disposing worktrees from inside the TUI are planned as later
additions.

## What it does

- `;` (empty) — every worktree for the current repo, in `git worktree
  list`'s own order (the main worktree first).
- `;login` — every worktree whose branch name or path contains `login`
  (substring AND across whitespace-separated tokens, same contract as every
  other mode).
- The first text column is the branch name (`(detached)` for a detached-HEAD
  worktree, `(bare)` for a bare repository); the second is the shortened
  path (`~/work/foo`).

## Selecting a row

- `Enter` on an unmarked row creates a new tmux session / herdr workspace
  rooted at the worktree's directory and switches to it.
- `Enter` on a `T`-marked row (a tmux / herdr pane is already active there)
  focuses that existing pane instead of creating a duplicate session.
- Both paths are handled by the same staging function `#` Directories mode
  uses (`App::stage_directory_selection`) — a worktree row is tagged
  `mode == "directory"` internally, so the rest of the TUI treats it
  identically to a Directories-mode row.

## Relationship to Directories and Zoxide mode

All three modes list directories and stage the same session-creation on
selection — the difference is the **data source**:

|                | Directories (`#`)                        | Zoxide (`~`)                          | Worktree (`;`)                             |
| -------------- | ----------------------------------------- | -------------------------------------- | -------------------------------------------- |
| Source         | smarthistory's `history` table + config   | zoxide's own database                  | `git worktree list` for the current repo     |
| Ranking        | most-recent history activity              | zoxide's frecency score                | `git worktree list`'s own order              |
| Requires       | nothing extra                             | the `zoxide` binary on `$PATH`         | being inside a git repo, `git` on `$PATH`    |

## Health check

`smarthistory check --prefix ';'` (or `smarthistory check` with no filter,
which checks every mode) verifies the current directory is inside a git
repository and reports how many worktrees `git worktree list` returns.

## Cross-references

- [Directories mode — the history-derived directory view; shares the exact same row-selection staging](directories.md)
- [Zoxide mode — the zoxide-derived directory view; shares the exact same row-selection staging](zoxide.md)
- [README — multiplexer integration](../../README.md#multiplexer-integration-tmux--herdr)
