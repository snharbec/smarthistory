# Panes mode (`*`)

| Default prefix | `*` |
| --- | --- |
| Configurable | `prefix.panes=<char>` |

Panes mode lists every pane across every tmux session and every herdr workspace, organised as a tree: each session / workspace gets a header row (`# workspace-label`), and its child panes appear indented underneath (`· [workspace-label] command  cwd`). Selecting a row stages the command to focus that pane / workspace.

## What it does

- `*` (empty) — every pane across every multiplexer session / workspace.
- `*nvim` — every pane whose `command` (or `cwd`) contains `nvim`, plus the parent workspace header (group-aware filter).
- The first text column is the agent / command; the second is the cwd; the third is the timestamp.
- Each pane row carries a `[workspace-label]` chip in the info color, e.g. `[smarthistory]` or `[dir: Downloads]`. The chip is the primary signal when the workspace header is hidden by a filter.

## Workspace headers

A workspace header row is rendered for every tmux session / herdr workspace that owns at least one pane. The header's command column shows the workspace label (e.g. `smarthistory`, `dir: Downloads`); selecting it stages the focus command for the whole workspace.

## Selecting a row

- `Enter` on a **pane** row stages `tmux select-pane -t <pane-id>` / `tmux switch-client -t <pane-id>` (tmux) or `herdr workspace focus <ws> && herdr tab focus <tab-id>` (herdr). The TUI exits and the parent shell runs the command — your terminal flips to the target pane.
- `Enter` on a **workspace** header row stages the workspace-focus command (no specific pane). Useful when the workspace is in another window / tab and you just want to land in it.

## Group-aware filter

The filter is **group-aware**: typing a token that matches a workspace label keeps the whole workspace (header + every child pane); typing a token that matches a pane's command or cwd keeps that pane and its parent workspace header.

The intuition: a pane that *transiently* runs `nvim` shouldn't orphan its workspace from the list when the user types `nvim` to find it. The workspace header is always kept as the group anchor.

## Sources

The panes view is built from two queries run once at TUI start:

1. `tmux list-windows -a` for tmux (parsed to extract `pane_id`, `pane_current_path`, `window_id`, `session_name`).
2. `herdr workspace list` + `herdr pane list` for herdr.

The multiplexer is selected via `multiplexer=tmux|herdr` in the config (default `tmux`). The snapshot is cached for the session and not refreshed on navigation.

## `--panes-filter` initial filter

`F7` / `F8` / `F9` cycle the panes filter between `all`, `windows` (live multiplexer panes only), `hosts` (the `# hosts` block only), `sessions` (the `# sessions` block only). The active filter is shown as a chip in the mode strip.

## Cross-references

- [Directories mode — the per-directory view; `#` shows the *unique* directories the shell has been in, `*` shows every pane currently running in them](directories.md)
- [TECHNICAL — multiplexer backend details](../../TECHNICAL.md#multiplexer-integration)
- [README — multiplexer integration](../../README.md#multiplexer-integration-tmux--herdr)
- **[Multiplexer backend reference](../../docs/multiplexer.md)** — backend selection, building with the `herdr` feature, setup guides for both backends, troubleshooting.
