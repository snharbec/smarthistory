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

## Configured groups: Directories and Hosts

Below the live multiplexer workspaces, `*` mode appends two more groups built from your config, each with its own `# `-prefixed header row and indented children, same tree shape as a live workspace:

- **Directories** — the `session.N` quick-launch entries from `~/.config/smarthistory/config` (e.g. `session.1 = "⛩️ Home"`, `session.1.dir = "~/"`). Displayed as `# Directories` — despite the `session.N` config key name, these are directory shortcuts, not multiplexer sessions, so the group is labeled for what it actually is rather than the config syntax that defines it.
- **Hosts** — the `host.N` entries (SSH connections). Displayed as `# hosts`.

These groups use the *same* group-aware filter as live workspaces: the header stays visible (with every sibling entry) as long as any row in the group matches, not just the one(s) that do. Searching `*Home` shows the `Directories` header and every configured directory entry, with `Home` pre-selected — not just the `Home` row with its header stripped away.

**Group-name scoping**: a query token that's a case-insensitive substring of a group's displayed header label narrows the list to just that group, even though the token doesn't literally appear in any child row's own text — `dir` for `Directories`, `host`/`hosts` for `Hosts`, or **any live workspace's own name**, e.g. `note` for a workspace named `NoteSearch`. This works uniformly across all three kinds of group because they're all just `mode == "workspace"` header rows with a label — nothing about `Directories`/`Hosts` is special-cased. Combine a scope token with a content token to pick a specific entry within that group: `*note claude` scopes to the `NoteSearch` workspace via `note` (not "claude" — nothing about the workspace itself need mention "note", it's the workspace's own name that matches) and selects its `claude` pane specifically — the whole `NoteSearch` workspace stays shown, not narrowed to just that one pane, and a `claude` pane in a *different*, unscoped workspace is excluded entirely rather than accidentally selected. Useful whenever a name could otherwise be ambiguous between a directory shortcut, a host, and a live workspace of the same or a similar name. A scope token alone (`*dir`, `*note`) shows the whole group with its header selected; there's no single row to prefer without an accompanying content token. A scope token must be at least 3 characters — shorter tokens are treated as ordinary content (matching almost every group's label as a substring otherwise makes 1–2 character searches unusably eager). This scoping only applies in Substring match mode (`Ctrl-F` default) — Fuzzy/Regex mode matches the whole query as one pattern and doesn't split it into scope vs. content tokens.

`F7` / `F8` / `F9` (below) do the same group restriction, just via a fixed keybinding instead of typing a token — the two mechanisms are complementary, not exclusive: `F9` toggles the `Directories`-only view, and typing `dir` as a token does the same thing without touching the filter keys.

## Sources

The panes view is built from two queries run once at TUI start:

1. `tmux list-windows -a` for tmux (parsed to extract `pane_id`, `pane_current_path`, `window_id`, `session_name`).
2. `herdr workspace list` + `herdr pane list` for herdr.

The multiplexer is selected via `multiplexer=tmux|herdr` in the config (default `tmux`). The snapshot is cached for the session and not refreshed on navigation.

## `--panes-filter` initial filter

`F7` / `F8` / `F9` cycle the panes filter between `all`, `windows` (live multiplexer panes only), `hosts` (the `# hosts` block only), `sessions` (the `# Directories` block only — the filter's internal name/config value stays `sessions` for backward compatibility, but also accepts `directories` / `directory` / `dir` / `dirs` as aliases). The active filter is shown as a chip in the mode strip, labeled `DIRECTORIES` for the sessions/directories filter.

## Cross-references

- [Directories mode — the per-directory view; `#` shows the *unique* directories the shell has been in, `*` shows every pane currently running in them](directories.md)
- [TECHNICAL — multiplexer backend details](../../TECHNICAL.md#multiplexer-integration)
- [README — multiplexer integration](../../README.md#multiplexer-integration-tmux--herdr)
- **[Multiplexer backend reference](../../docs/multiplexer.md)** — backend selection, building with the `herdr` feature, setup guides for both backends, troubleshooting.
