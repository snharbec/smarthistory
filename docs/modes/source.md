# Source mode (`|`)

| Default prefix | `\|`                   |
| -------------- | ---------------------- |
| Configurable   | `prefix.source=<char>` |

Source mode searches the project checkouts you have visited and opens one in its
own tmux session or herdr workspace.

## What counts as a source

A source is a directory containing `.git` or a `build.xml` file. The zsh and
bash hooks check each directory you change into and record it, along with
whether it is a git source. If both markers exist, it is a git source.

- A linked git worktree is recorded as its main repository, so each repository
  appears once.
- A submodule is recorded as its own source.
- Only the directory holding the marker counts. A subdirectory of a repository
  is not recorded.
- Directories on network mounts are recorded too. The check runs detached from
  the prompt, so an unreachable mount cannot delay your shell.

Sources are included in `smarthistory export` and `import`.

## Selecting a source

Type `|` and part of the path. Rows show a `git` or `build` marker, and typing
`git` or `build` filters on it. Press Enter on a row:

- A non-git source opens directly.
- A git source with one checkout opens directly.
- A git source with several worktrees shows a picker. Type to filter by branch
  or path, then press Enter.

The new session or workspace is named after the selected directory's basename.
`.` and `:` become `-`, because tmux treats them as target separators.

If a session with that name is already rooted in the selected directory, it is
reused. If the name belongs to another directory, a new session is created as
`name-2`, `name-3`, and so on. herdr workspaces have no root directory of their
own, so a herdr workspace counts as rooted there when any of its panes is
currently in that directory.

## Removing a source

The `DeleteSelected` action removes the highlighted row after a confirmation.
The directory and its command history are not changed. A removed directory is
recorded again the next time you change into it.
