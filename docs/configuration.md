# Configuration reference

Every value `smarthistory` reads at startup, with the config-file key, default, semantics, and a worked example. The canonical source is [`src/main.rs::Config`](../src/main.rs) (the `Config` struct, `Config::default`, and `Config::parse`); this file mirrors that source so the docs and code stay close enough to spot drift.

**Where the config lives**

| Location | What |
| --- | --- |
| `~/.config/smarthistory/config` | The user config file (INI-style `key=value` lines; `#` starts a comment; `~` is expanded in path values). Read at startup by `Config::load`. Missing file → built-in defaults. |
| `~/.config/smarthistory/hosts` | Optional: `host.<key>.*` entries, same syntax as in the main config file. Only read by the TUI (`Config::load_tui`) — see [`host.<key>`](#hostkey). |
| `~/.config/smarthistory/sessions` | Optional: `session.<key>.*` entries, same syntax as in the main config file. Only read by the TUI (`Config::load_tui`) — see [`session.<key>`](#sessionkey). |
| `~/.local/cache/smarthistory/` | Runtime cache: `query_history.json` (per-mode recall), `global_query_history.json` (cross-mode recall, `Ctrl+Shift+P`/`Ctrl+Shift+N`), `widget-debug.log` (TUI debug trace), `last_session.json` (the most recent session, for `smarthistory tui` resume). Not hand-edited. |
| `~/.cache/tmux-history/` | Per-pane tmux output logs (set via `tmuxpaneoutputdir`). |
| `~/.config/smarthistory/themes/` | Optional user theme directory (TOML files matching the built-in theme shape). |

**Loading order** (later wins):

1. Built-in defaults (`Config::default`).
2. `~/.config/smarthistory/config` — each `key=value` line is parsed in order; later values override earlier ones.
3. **TUI only**: `~/.config/smarthistory/hosts` and `~/.config/smarthistory/sessions`, folded in as if they were appended to the main config file (`Config::load_tui`; see [`session.<key>`](#sessionkey) / [`host.<key>`](#hostkey)). Every other CLI subcommand uses the plain `Config::load` and never reads these two files.
4. Environment variables (noted per-item below; they always win over the config file when set).

**Validation**

```sh
smarthistory config check     # exits non-zero on errors, prints warnings
```

`config check` catches unknown keys, invalid values (e.g. a non-numeric port, a `capturelines` that isn't a number or `ALL`), theme colors that don't parse, and conflicting `key.<action>=` bindings. The output also lists every "effective value" so you can confirm what the TUI actually sees.

---

## Table of contents

- [Capture & output](#capture--output)
  - [`tmuxpaneoutputdir`](#tmuxpaneoutputdir)
  - [`ignorecapture`](#ignorecapture)
  - [`capturelines`](#capturelines)
  - [`capturelines.<cmd>`](#capturelinescmd)
- [History list & filtering](#history-list--filtering)
  - [`duplicatefilter`](#duplicatefilter)
  - [`initialmode`](#initialmode)
  - [`zsh.mode`](#zshmode)
  - [`segments.minwords`](#segmentsminwords)
- [Live dropdown completion](#live-dropdown-completion)
  - [`dropdown.enabled`](#dropdownenabled)
  - [`dropdown.limit`](#dropdownlimit)
  - [`dropdown.minchars`](#dropdownminchars)
  - [`dropdown.highlight`](#dropdownhighlight)
  - [`dropdown.matchmode`](#dropdownmatchmode)
- [Comment expansion](#comment-expansion)
  - [`commentexpand.enabled`](#commentexpandenabled)
- [Glob-triggered Tab file completion](#glob-triggered-tab-file-completion)
  - [`globcomplete.enabled`](#globcompleteenabled)
- [Theme](#theme)
  - [`tuicolor.*`](#tuicolor)
- [Key bindings](#key-bindings)
  - [`key.<action>`](#keyaction)
  - [Built-in theme selection](#built-in-theme)
  - [User theme directory](#user-theme-directory)
- [Query prefixes](#query-prefixes)
  - [`prefix.<name>`](#prefixname)
- [Multiplexer integration](#multiplexer-integration)
  - [`multiplexer`](#multiplexer)
  - [`sessiondirs`](#sessiondirs)
  - [`homemap`](#homemap)
  - [`session.<key>`](#sessionkey)
  - [`host.<key>`](#hostkey)
- [Modes](#modes)
  - [Notes (`@` mode)](#notes--mode)
  - [Todo (`!` mode)](#todo--mode)
  - [Tags (`$` mode)](#tags--mode)
  - [Files (`/` mode)](#files--mode)
  - [JIRA (`-` mode)](#jira--mode)
  - [LLM (`=` mode)](#llm--mode)
  - [Paperless (`<` mode)](#paperless--mode)
  - [Browser (`^` mode)](#browser--mode)
- [Environment variables](#environment-variables)
  - [Published environment variables](#published-environment-variables)
- [All keys at a glance](#all-keys-at-a-glance)

---

## Capture & output

These control how much of each command's output is recorded, where those log files live, and which commands are excluded from capture entirely.

### `tmuxpaneoutputdir`

| | |
| --- | --- |
| **Type** | Path |
| **Default** | `~/.cache/tmux-history` |
| **Tilde expansion** | Yes |
| **Env override** | — |

The directory containing per-pane tmux output log files. The preexec hook (`_smarthistory_precmd` in `init.zsh`) writes to `output-${TMUX_PANE}.log` inside this directory; the TUI reads them for the `+` (output search) mode and the `*` (panes) view. Path is created on first write by the shell hook.

```ini
tmuxpaneoutputdir=~/.cache/tmux-history
# Point to a fast SSD if you have one — log writes happen on every prompt:
tmuxpaneoutputdir=/Volumes/Fast/tmux-history
```

### `ignorecapture`

| | |
| --- | --- |
| **Type** | Space-separated list of command names (first token) |
| **Default** | `cd ls pwd exit clear history fc jobs bg fg wait disown suspend` |
| **Env override** | — |

Commands whose output is never captured. The list is matched against the **first token** of each command (the executable name), so `git` covers every `git` invocation but `cargo build` only matches `cargo`. The TUI still records the command text in the history list; only the captured output is skipped. Empty value (`ignorecapture=`) means capture everything.

```ini
# Add a few chatty commands to the default skip list:
ignorecapture=cd ls pwd exit clear history fc jobs bg fg wait disown suspend neofetch fastfetch
```

### `capturelines`

| | |
| --- | --- |
| **Type** | `ALL` \| positive integer |
| **Default** | `20` (built-in `DEFAULT_CAPTURE_LINES`) |
| **Env override** | — |

The default number of lines captured per command. The cap is applied **after** deduplication, so a command that ran 5 times with 4 lines of output each gives 4 lines, not 20. `ALL` (case-insensitive) means unlimited capture; invalid values fall through to the default with a stderr warning.

```ini
capturelines=20
capturelines=100
capturelines=ALL    # no cap (use for log-mining sessions)
```

### `capturelines.<cmd>`

| | |
| --- | --- |
| **Type** | `ALL` \| positive integer |
| **Default** | — (no per-command override) |
| **Env override** | — |

Per-command override for `capturelines`. The `<cmd>` is the first token of the command; the override takes precedence over the global `capturelines`. Useful for keeping verbose tools (`cargo build`, `npm install`) under tight caps while leaving room for log-shaped commands.

```ini
capturelines.cargo=4            # just the summary
capturelines.git=10
capturelines.kubectl=20
capturelines.kubectl_logs=ALL   # one-off: capture all of `kubectl logs`
```

Multiple overrides stack; the matching is by **first token**, not substring. `capturelines.kubectl=10` matches `kubectl get pods`, `kubectl apply -f …`, and any other command that starts with `kubectl`.

---

## History list & filtering

### `duplicatefilter`

| | |
| --- | --- |
| **Type** | `on` \| `off` (anything else → default) |
| **Default** | `on` |
| **Env override** | — |

When `on`, the TUI shows only the newest instance of each identical command — older duplicates are hidden. Toggleable at runtime via the dedup chip in the TUI's header (the `ToggleDuplicateFilter` action). The setting on disk is the initial value at TUI startup; the TUI's runtime toggle is not persisted across launches.

```ini
duplicatefilter=on    # see the breadth of recent work
duplicatefilter=off   # see every invocation in order
```

### `initialmode`

| | |
| --- | --- |
| **Type** | `SESS` \| `DIR` \| `GLOBAL` |
| **Default** | `SESS` |
| **Env override** | `SMARTHISTORY_TUI_MODE` (the CLI flag `--mode` also wins) |

The initial search scope for `smarthistory tui`. Precedence (highest first): `--mode` CLI flag → `SMARTHISTORY_TUI_MODE` env var → `initialmode` config value → built-in default. Accepted (case-insensitive) values:

- `SESS` / `SESSION` — show only the current tmux session's history.
- `DIR` / `DIRECTORY` — show only the current working directory's history.
- `GLOBAL` — show all commands across every session and directory.

The `SESS` and `DIR` scopes require a running tmux session with `TMUX_PANE` set; without tmux, the TUI falls back to `GLOBAL` with a status-bar message.

```ini
initialmode=SESS
```

### `zsh.mode`

| | |
| --- | --- |
| **Type** | `sess` \| `dir` \| `global` |
| **Default** | `sess` |
| **Env override** | — |

The Up/Down history-walk widget's search scope (`_smarthistory_mode` in `init.zsh`/`init.bash` — despite the `zsh.`-prefixed key name, this is shared by the bash port's own Up/Down and Ctrl-g widgets too, not just zsh's) a brand-new shell starts on: `sess` (current `$SMART_HISTORY_SESSION` only), `dir` (current working directory only), or `global` (no scope filter). This is unrelated to [`initialmode`](#initialmode) above, which controls `smarthistory tui`'s own starting scope — `zsh.mode` controls the readline-level widgets instead. `Ctrl-g` (`_smarthistory_cycle_mode`) still cycles `sess` → `dir` → `global` → `sess` at runtime regardless of this setting; it only picks the starting point for a new shell, not the cycle order. Each press confirms the new scope with a transient `zle -M` status message ("smarthistory mode set to DIR") — shown until the next keystroke, not a permanent prompt fixture.

```ini
zsh.mode=global
```

### `segments.minwords`

| | |
| --- | --- |
| **Type** | non-negative integer |
| **Default** | `5` |
| **Env override** | — |

Minimum word count a segment's **body** — its text minus its own header line — must have for [`:` (segment search)](modes/segments.md) or [`"` (similar)](modes/similar.md) mode to keep it. A segment at or under this threshold (a heading with little or nothing under it) is dropped as noise. The header line itself never counts toward the total, however long it is — only what's actually written below it does. `0` disables the filter entirely, keeping every segment regardless of length.

```ini
segments.minwords=5
segments.minwords=0    # keep every segment, however short
```

---

## Live dropdown completion

### `dropdown.enabled`

| | |
| --- | --- |
| **Type** | `on` \| `off` |
| **Default** | `off` |
| **Env override** | — |

Whether the live dropdown is active. Defaults off — unlike most opt-in features in this app, enabling it means every keystroke at the shell prompt triggers a `smarthistory search` call and a render, so it's a bigger behavior change than e.g. `ollama.*` or `paperless.*`, which only activate on an explicit prefix character.

```ini
dropdown.enabled=on
```

**Keys**: `Up`/`Down` (or `Ctrl-N`/`Ctrl-P`) navigate (highlight) a candidate — this is the only way to select one. Once a candidate is highlighted, `Enter` commits it into the command line and runs it immediately (one key), and `Tab` commits it into the command line WITHOUT running it, if you want to review or edit before pressing `Enter` separately. With nothing highlighted yet, `Tab` falls straight through to zsh's normal completion (exactly as if the dropdown weren't showing) and `Enter` just runs whatever you've typed — so a fresh dropdown (including the single-candidate case) never gets substituted in by an unmodified `Tab` or `Enter` press; only an explicit `Up`/`Down` selection unlocks that. `Ctrl-A`/`Ctrl-E`/`Right`/`Left` also commit the highlighted candidate (cursor at start/end/unmoved respectively), and `Esc` dismisses the menu for the rest of the line. This keeps history completion from ever rewriting the buffer to something you didn't deliberately select — typing a new argument that happens to be a prefix of exactly one old history entry (e.g. `less /tmp/test2` with `less /tmp/test1` in history) doesn't risk the whole line jumping to the old entry unless you actually navigate to it first.

**Exit status**: each row also shows a `✓`/`✗` marker (green/red) for the
candidate's last exit code, so you can spot a previously-failed command
without opening the TUI or re-running it.

### `dropdown.limit`

| | |
| --- | --- |
| **Type** | positive integer |
| **Default** | `6` |
| **Env override** | — |

Maximum number of candidates shown in the dropdown. Passed as `--limit` to the same `smarthistory search` call the `Up`/`Down` history-walk widget already uses.

```ini
dropdown.limit=10
```

### `dropdown.minchars`

| | |
| --- | --- |
| **Type** | non-negative integer |
| **Default** | `1` |
| **Env override** | — |

Minimum number of typed characters (with the cursor at end-of-buffer) before the dropdown appears. Keeps an empty or near-empty prompt from showing a large, low-signal candidate list.

```ini
dropdown.minchars=2
```

### `dropdown.highlight`

| | |
| --- | --- |
| **Type** | `on` \| `off` |
| **Default** | `off` |
| **Env override** | — |

Syntax-highlight each dropdown candidate instead of plain text: lexical token coloring (strings, flags, operators, …) via [`bat`](https://github.com/sharkdp/bat) — the same `bat --plain --color=always --theme <light|dark>` invocation this app's `highlight_with_bat` already uses for the `$` tags-mode preview and the `smart-open.default` fallback — plus a self-checked green/red for the first word, since `bat`'s highlighting is purely lexical and can't tell a valid command from a typo. The check mirrors what a real shell highlighter looks at: aliases, functions, builtins, `$PATH` commands, and common reserved words (`if`, `for`, `sudo`, …) are green; anything else is red.

The `--theme` choice matches the resolved `tuicolor.bg`'s perceived brightness (the same ITU-R BT.601 formula `highlight_with_bat`'s Rust-side theme detection uses), read once at shell-init time from `smarthistory config get palette` — so dropdown colors read correctly against the same light/dark background the rest of the app already assumes, not `bat`'s own default theme. The palette itself is resolved using whichever scheme (light/dark) your last TUI session actually had active (`Action::ToggleColorScheme`, persisted in the session file) — with `theme.dark`/`theme.light` both configured, toggling in the TUI and opening a new shell changes the dropdown's colors too.

Off by default: it adds one `bat` subprocess call per dropdown render (all candidates batched into a single call, not one per row), on top of the `smarthistory search` call every keystroke already makes. Silently stays off — no error, no startup warning — when `bat` isn't on `$PATH`, even if this is `on`.

```ini
dropdown.enabled=on
dropdown.highlight=on
```

### `dropdown.matchmode`

| | |
| --- | --- |
| **Type** | `prefix` \| `substring` |
| **Default** | `prefix` |
| **Env override** | — |

The dropdown's match mode a brand-new shell starts on: `prefix` matches only commands that START WITH what's typed (the historical, hardcoded behavior — a plain substring match made typing `ls` match `open "http://.../details"`, since that URL contains "ls"); `substring` matches anywhere in the command, the same broader match the `Up`/`Down` history-walk widget and the TUI's own search already use. `Ctrl-t` (`_smarthistory_cycle_matchmode`) toggles between the two at runtime regardless of this setting — it only picks the starting point for a new shell, same relationship [`zsh.mode`](#zshmode)/`Ctrl-g` has. Each press confirms the new mode with a transient `zle -M` status message ("smarthistory match set to substring"), regardless of whether the dropdown is currently on.

```ini
dropdown.enabled=on
dropdown.matchmode=substring
```

---

## Comment expansion

### `commentexpand.enabled`

| | |
| --- | --- |
| **Type** | `on` \| `off` |
| **Default** | `off` |
| **Env override** | — |

Whether the comment-expansion widget is active — supported in both `smarthistory init zsh` and `smarthistory init bash` (the bash port needs bash >= 4.0; see the README's Installation section). When on, typing a comment's text (as set via `smarthistory add ... --comment "..."`) at the very start of the command line, then a space, replaces it with the most recently used command carrying that exact comment — the same UX as zsh-abbr/fish abbreviations, sourced from smarthistory's own comment data. Matching is exact and case-insensitive against `command_comments.comment` (not a substring match, and not scoped to the command text), so a comment shared by multiple commands always resolves to whichever was run most recently. Off by default, same opt-in reasoning as `dropdown.enabled` above: it hooks keystroke widgets, a bigger behavior change than the prefix-triggered modes.

```ini
commentexpand.enabled=on
```

```sh
smarthistory add "docker compose up -d" --exit-code 0 --comment deploy
# then in a fresh shell, typing "deploy" + space expands to:
# docker compose up -d
```

**Which widget the space bar actually triggers.** Plain zsh binds the space key to `self-insert`, but many setups (including stock oh-my-zsh, via `lib/key-bindings.zsh`) rebind it to `magic-space` instead (zsh's built-in history-bang expansion, e.g. `!!` + space). `init.zsh` hooks both `self-insert`/`self-insert-unmeta` and `magic-space`, so the feature works either way — no setup needed beyond `commentexpand.enabled=on`.

**Re-sourcing `init.zsh` is safe.** Every keystroke widget this feature (and `dropdown.enabled`) touches goes through one dispatcher per widget, backed by a growable, dedup'd hook list — re-running `eval "$(smarthistory init zsh)"` in an already-initialized shell (e.g. after editing the config) only appends to that list, it never re-wraps a widget. A brand-new shell is still the simplest way to pick up config or binary changes.

---

## Glob-triggered Tab file completion

### `globcomplete.enabled`

| | |
| --- | --- |
| **Type** | `on` \| `off` |
| **Default** | `off` |
| **Env override** | — |

Replaces fzf-tab-style file completion (phase 1 of a planned fzf replacement — process and directory completion are not implemented yet). **zsh only** (unlike `commentexpand.enabled`, there's no bash port of this feature). When on, pressing `Tab` on a word containing shell-glob syntax (`*`, `?`, or `[`) launches the TUI locked into a file-completion picker instead of running normal zsh completion:

```ini
globcomplete.enabled=on
```

```sh
$ vi a*<TAB>
# launches the picker, pre-filtered to "a", searching the current directory
$ vi foo/a*<TAB>
# scopes the walk to the foo/ directory (still recursive underneath it —
# this is a fuzzy-find replacement for fzf, not literal single-level
# shell-glob expansion)
```

Selecting a row (`Enter`) splices its path — relative to the shell's cwd, the way a real shell glob expansion reads — back into the command line in place of the typed glob word — `vi a*` + selecting `apple.txt` becomes `vi apple.txt` (a nested match keeps its intermediate directories, e.g. `sub/apple.txt`, not just the bare filename). Any word NOT containing `* ? [` falls through to normal completion untouched, and the trigger only fires when the cursor is at the end of the line (a glob word earlier in the buffer is not detected).

**Inside the picker**, mode-switching is locked — the query can never leave files mode, and `F1`/`Ctrl-]` are disabled. Two dedicated keys:

- `Ctrl-A` marks every visible row.
- `Enter` returns every marked row's path (space-joined, individually shell-quoted), or just the highlighted row if nothing is marked. It never runs the line — accepting a completion behaves like normal Tab-completion, not like the main history picker's Enter.

`Esc`/`Ctrl-C` cancels without touching the command line at all.

**Root scoping.** The word is split on its last `/`: everything before it becomes the walk root (`foo/bar/a*` scopes to `foo/bar/`, filtering by `a*`), everything after is matched against each file's basename — recursively, at any depth under that root. A leading segment that's itself glob-like (`**/*.rs`, `src/*/test.rs`) can't be used to scope the root, so the walk falls back to the base directory (still fully recursive — nothing is missed, just less pruned).

**Narrowing further inside the picker.** Once the picker is open, typing a space then more text adds a plain substring filter on top of the glob — the FIRST word is always the glob (established when you pressed Tab), every word after it narrows the results further by substring against each file's path. `*.md jira` matches every markdown file whose path contains "jira", not just files literally named `jira*.md`.

**`cd` opens a directory picker instead.** When the command being completed is `cd` (the first word of the line — compound commands like `ls && cd proj*` aren't detected, only a simple leading `cd`), pressing Tab on a glob word opens the SAME picker but showing only directories, never files — `cd proj*<TAB>` finds every real subdirectory matching `proj*` on disk, recursively, the same glob/root-scoping/narrowing rules as the file picker. The one difference: there's no multi-select — `Ctrl-A` is a no-op (cd-ing into more than one directory at once doesn't mean anything), and `Enter` always returns just the single highlighted directory. Selecting a directory row shows its immediate contents in the output preview pane (directories first, suffixed with `/`, then files, alphabetical within each group, hidden entries excluded) — a quick look inside before committing to `cd` there.

**`kill` opens the process picker instead.** When the command being completed is `kill` (again, a simple leading `kill`, not after `;`/`&&`/`||`), pressing Tab — with or without anything typed after it, and with or without glob syntax, since PIDs have no glob concept — opens the processes (`%`) mode picker instead of the file picker, pre-filtered by whatever's typed (matched against name/cmdline/cwd/exe, same as typing it into `%` mode directly). `kill <TAB>` shows every process; `kill firefox<TAB>` narrows to processes matching "firefox". Multi-select IS available here, same as the file picker — `Ctrl-A` marks every visible row, and `Enter` returns every marked (or just the selected) process's PID, space-joined, instead of opening `%` mode's normal signal-confirmation dialog (sending the signal is still the shell's `kill` command's job here, not smarthistory's).

---

## Theme

Colors are configured in two layers: the built-in theme picker (F2 by default) gives a complete palette in one go, and the `tuicolor.*` keys are the surgical override for users who want a built-in theme with a tweaked accent or selection color. The two layers compose: a built-in theme sets the full palette, and any `tuicolor.*` key with a non-empty value replaces the corresponding slot.

### `tuicolor.*`

Every field accepts a hex color (`#rrggbb`) or a named color (`black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `gray`, `grey`, `darkgray`, `darkgrey`, `lightred`, `lightgreen`, `lightyellow`, `lightblue`, `lightmagenta`, `lightcyan`, `white`). Empty values are silently dropped (so `tuicolor.accent=` is a no-op, not an error).

When a built-in theme is active, each `tuicolor.*` field defaults to **empty** (meaning "use the theme's own value"). When no theme is selected (`SelectedTheme::None` / the manual palette), empty fields fall back to the hardcoded `Palette::builtin()` defaults (black bg, gray fg, cyan accent, darkgray selection, etc.).

| Key | Slot | Notes |
| --- | --- | --- |
| `tuicolor.bg` | `bg` | Main app background. |
| `tuicolor.fg` | `fg` | Primary text. |
| `tuicolor.accent` | `accent` | Borders, focused input, mode tint. |
| `tuicolor.success` | `success` | Success / exit-0 indicators. |
| `tuicolor.error` | `error` | Error / exit-nonzero indicators. |
| `tuicolor.warning` | `warning` | Warning indicators. |
| `tuicolor.dim` | `dim` | Secondary text (timestamps, secondary metadata). |
| `tuicolor.highlight` | `highlight` | Selected row's left-edge bar (the `▌` glyph) and the `highlight` slot for picked cells. Falls back to `accent` when unset. |
| `tuicolor.info` | `info` | Foreground tint for the `+` (output search) mode badge. |
| `tuicolor.selection` | `selection` | Background of the currently-selected row in the history list. |
| `tuicolor.badge_fg` | `badge_fg` | Foreground for badge text (the `SESS`, `DEDUP`, `+` chips in the header). Falls back to `bg` so the chip text contrasts with the bright badge background. |
| `tuicolor.list_bg` | `list_bg` | Background of the history list pane. |
| `tuicolor.details_bg` | `details_bg` | Background of the details pane. |
| `tuicolor.input_bg` | `input_bg` | Background of the search / comment input. |
| `tuicolor.status_bg` | `status_bg` | Background of the status bar. |

```ini
# Tweak Leuven (a light theme) so the selected row is less aggressive:
tuicolor.selection=#d6c7a1   # slightly darker tan than the theme default

# Force a particular accent across all themes:
tuicolor.accent=#ffb86c      # a warm orange that works on both light and dark backgrounds
```

### Built-in theme

The built-in theme is set via the TUI's theme picker (`F2` / `ThemePicker` by default). The selected theme is persisted to the session file (`~/.local/cache/smarthistory/last_session.json`) and reapplied on next TUI launch. There is no config-file key to set the theme — the picker is the canonical path (so the searchable picker, the live preview, and the session-file write all stay in sync). 74 themes ship in `src/tui/theme/curated/` (15 upstream from `ratatui_themes` + 59 curated).

### User theme directory

Drop a TOML file in `~/.config/smarthistory/themes/` matching the built-in theme shape (`bg`, `fg`, `accent`, `success`, `error`, `warning`, `muted`, `selection`, `info` — all hex strings or named colors). The theme appears in the picker alongside the built-ins. See [`src/tui/theme/curated.rs`](../src/tui/theme/curated.rs) for the full schema.

---

## Key bindings

### `key.<action>`

| | |
| --- | --- |
| **Type** | `KeySpec` (one or more, comma-separated) |
| **Default** | action's `default_key()` from `src/tui/bindings.rs` |
| **Env override** | — |

Every TUI action ships with a default key; this key sets a new binding (or `none` to unbind entirely). The action name is the kebab-case `config_key()` from [`src/tui/bindings.rs`](../src/tui/bindings.rs) — see [docs/actions.md](actions.md) for the full list (48 actions, with default keys, categories, and detailed descriptions).

Key spec grammar:

| Spec | Example | Meaning |
| --- | --- | --- |
| `C-<key>` | `C-c` | Ctrl + key |
| `M-<key>` | `M-h` | Alt + key |
| `S-<key>` | `S-Return` | Shift + key (use `BackTab` for Shift-Tab) |
| `C-M-<key>` | `C-M-s` | Ctrl + Alt + key (modifiers in any order) |
| named | `Up`, `PageDown`, `F1`, `Insert`, `Backspace`, `Esc`, `Enter`, `Tab`, `Home`, `End` | Special key |
| char | `T` | A single character |
| `none` | `none` | Unbind (the action ships unbound; rebinding is the user's choice) |

Multiple specs for one action are comma-separated: `key.cancel=C-c,Esc`. The same key can't map to two actions — the first match in `ALL_ACTIONS` order wins; `smarthistory config check` warns about conflicts. Unknown key specs are dropped with a stderr warning.

```ini
# Use vim-style navigation:
key.up=k
key.down=j
key.page-up=C-b
key.page-down=C-f

# Bind SmartOpen to a comfortable key:
key.smart-open=C-]

# Unbind an action you don't use:
key.mark-todo-done=none

# Two keys for the same action:
key.cancel=C-c,Esc
```

The full action reference (every action, default key, category, and detailed description) is in **[docs/actions.md](actions.md)**.

---

## Query prefixes

### `prefix.<name>`

| | |
| --- | --- |
| **Type** | single character |
| **Default** | see table below |
| **Env override** | — |

The first character the user types to enter a mode. The default keymap covers every printable ASCII character; the config lets you remap any of them to a free key on your keyboard. Values must be a single character; multi-character values are silently ignored.

| Key | Slot | Default | Mode |
| --- | --- | --- | --- |
| `prefix.output` | output search | `+` | searches captured output |
| `prefix.llm` | LLM command generation | `=` | ask ollama to draft a command |
| `prefix.question` | general question | `?` | one-off LLM chat |
| `prefix.notes` | note search | `@` | searches the notes database |
| `prefix.todo` | todo search | `!` | markdown task-list scanner |
| `prefix.directories` | directories | `#` | unique dirs from history |
| `prefix.panes` | session panes | `*` | tmux / herdr panes |
| `prefix.files` | files | `/` | file browser |
| `prefix.tags` | tags | `$` | ctags symbol search |
| `prefix.ag` | ag content search | `,` | silver-searcher |
| `prefix.codegraph` | codegraph | `&` | FTS5 symbol search |
| `prefix.jira` | JIRA | `-` | JIRA issue search |
| `prefix.segments` | segment search | `:` | note_search `segments` table (header-anchored sections); `prefix.elements=` still works as a back-compat alias |
| `prefix.similar` | similar/phrase search | `"` | same `segments` table, ranked by embedding similarity to the typed phrase (requires a reachable Ollama instance) |
| `prefix.paperless` | paperless document search | `<` | search a Paperless-ngx backend by title / tag / correspondent |
| `prefix.browser` | browser bookmarks + history | `^` | merged Chrome / Firefox / Safari bookmarks + history, tagged `bookmark` / `history` |
| `prefix.zoxide` | zoxide directories | `~` | directories from the local `zoxide` database, highest frecency score first; selecting one creates a new tmux session / herdr workspace there (requires the `zoxide` binary on `$PATH`) |
| `prefix.processes` | running processes | `%` | list every OS process (macOS + Linux, all users); selecting one opens a confirm dialog to send it a signal (SIGTERM by default, Tab/Shift-Tab cycles SIGKILL/SIGHUP/SIGINT) |
| `prefix.meta` | meta-prefix (mode picker) | `'` | not a search mode itself — type a partial mode name then Tab to expand/activate; see below |

```ini
# Move JIRA off `-` (a frequently mistyped key) to backtick:
prefix.jira=`

# Move the rarely-used question mode to a less crowded slot:
prefix.question=?
```

**Meta-prefix mode (`'`)**: type `'` then a partial mode name (e.g. `'jir`)
and press `Tab`. A unique name match activates that mode immediately — the
query becomes just the target mode's real prefix character (e.g. `-`),
discarding the typed `'jir` text entirely. An ambiguous match (e.g. `'s`,
matching both `segments` and `similar`) opens the same picker overlay as
`PickPrefix` (`F1`), pre-filtered to just the matching names; the bare `'` +
`Tab` (nothing typed yet) opens that same picker showing every mode. Mode
names match `smarthistory tui`'s `--prefix`/config naming (`jira`, `notes`,
`paperless`, `browser`, …), not the picker's display labels.

See **[docs/modes/](modes/README.md)** for a full per-mode reference.

---

## Multiplexer integration

The TUI's directory- and panes-switching modes can target either **tmux** (the historical default) or **herdr** (a Cargo workspace multiplexer, behind a feature flag). Most users only need [`multiplexer`](#multiplexer); the rest of this section is for users who want to pre-seed the `#` mode with project directories, give the shell a hint about how to shorten paths, or define named sessions and SSH hosts for the `*` mode.

The full reference (per-backend setup, environment variable precedence, troubleshooting) is in **[docs/multiplexer.md](multiplexer.md)**.

### `multiplexer`

| | |
| --- | --- |
| **Type** | `tmux` \| `herdr` |
| **Default** | `tmux` |
| **Env override** | `SMARTHISTORY_MULTIPLEXER` (wins over config) |

Which terminal multiplexer the TUI's directory- and panes-switching modes should target. Unrecognised values are dropped with a stderr warning; the existing value (file / default) is preserved so a typo in the env var can't silently disable directory switching.

The `herdr` Cargo feature must be compiled in for the herdr path to be active; on a default build `herdr` is a no-op that surfaces a "build with `--features herdr`" status message.

```ini
multiplexer=tmux
multiplexer=herdr
```

```sh
# Or via the environment (wins over the config file):
export SMARTHISTORY_MULTIPLEXER=herdr
```

### `sessiondirs`

| | |
| --- | --- |
| **Type** | path (one per line) |
| **Default** | — |
| **Tilde expansion** | Yes |
| **Env override** | — |

Directories whose sub-tree is recursively walked; every directory found is added to the `#`-mode list, even when the user has never run a command there. The user's "always show me these projects" list. Multiple entries are allowed (one per line, like `prefix.<x>=`). A non-existent path is silently skipped (the user may have moved the directory; the next startup with the path back picks it up). The `~` is expanded at config-load time so `sessiondirs=~/.config/tmux-sessions` resolves to your real home, not the literal `~` directory.

The walk itself is **lazy**, not run at startup: it fires once, the first time you enter `#` (Directories) mode, and is cached for the rest of that TUI session. A launch that never visits `#` mode never pays for it at all — useful if a `sessiondirs=` entry points at a large tree.

```ini
sessiondirs=~/work/monorepo
sessiondirs=~/work/oss/smarthistory
sessiondirs=~/Documents/notes
```

### `homemap`

| | |
| --- | --- |
| **Type** | path prefix (one per line) |
| **Default** | — (only `$HOME` is shortened) |
| **Tilde expansion** | No (the value is itself a path prefix) |
| **Env override** | — |

Additional path prefixes shortened to `~/...` in the TUI display. The history DB stores absolute paths, but on display the TUI rewrites the matched prefix as `~`. `$HOME` is always in the set; `homemap` adds extras.

Use case: on macOS the user's home directory may live on an external volume and be mounted at `/Volumes/HUGE/har/...` while the shell exposes `/Users/har/...`. The preexec hook records the kernel-canonical path (the `/Volumes/HUGE/...` form); the shell snippet exposes the user's logical path. Adding `homemap=/Volumes/HUGE/har` tells the TUI to shorten both forms to `~/...` so the user sees one consistent short form.

```ini
homemap=/Volumes/HUGE/har
homemap=/Volumes/Backup
```

### `session.<key>`

| | |
| --- | --- |
| **Type** | sub-keyed group (`<key>` is an arbitrary identifier — see below) |
| **Default** | — |
| **File** | `~/.config/smarthistory/sessions` (or the main config file — see below) |
| **Tilde expansion** | Yes (on `dir`) |
| **Env override** | — |

A named session row in the `*` (panes) view. `<key>` is just a join key tying an entry's lines together — it never has to be typed elsewhere and isn't shown anywhere itself (the display name is the separate `session.<key>` bare value, below). Display order is file declaration order (first-seen order wins); there's no separate numbering to maintain. `smarthistory` picks `<key>` automatically when it writes a new entry (F5, or the `~` Zoxide save prompt): it slugifies the display name you typed — lowercased, spaces/punctuation collapsed to `-` — and appends `-2`, `-3`, … only if that slug is already taken, so `session.monorepo`, not `session.3`. You're free to hand-edit `<key>` to anything you like (or add entries by hand with whatever key you want) — it just has to be unique among `session.*` entries and contain no `.` (dots split it from the sub-key that follows).

**Older configs**: entries written before this scheme (numeric `session.1`, `session.2`, …) keep working exactly as before — the numeric id is just an opaque key to the parser, same as a slug. Nothing needs migrating; new entries just get better keys going forward.

Sub-keys:

| Sub-key | Required? | Meaning |
| --- | --- | --- |
| `session.<key>` | yes | The display name (used in the picker / status bar) |
| `session.<key>.dir` | no | The directory the session starts in (after `cd`) |
| `session.<key>.exec` | no | The command to run after creating the workspace (e.g. `nvim`, `claude`) |
| `session.<key>.startup_command` | accepted, not yet used | Reserved for future use |

```ini
# ~/.config/smarthistory/sessions
session.monorepo="monorepo"
session.monorepo.dir=~/work/monorepo
session.monorepo.exec=claude

session.notes="notes"
session.notes.dir=~/Documents/notes
```

**File location**: `session.<key>` entries can live in their own dedicated `~/.config/smarthistory/sessions` file, in the main `~/.config/smarthistory/config` file, or split across both — they're folded together as if it were one file (later-defined `<key>` sub-keys for the same entry win, same as within a single file). This file (like `hosts`, below) is read **only by the TUI** (`smarthistory tui` / `smarthistory check`), not by the plain CLI subcommands (`search`, `add`, `capture-*`, …), since session/host data is exclusively a `*`-mode (panes) concern and those commands run on every shell prompt — keeping them off that hot path avoids two needless file reads per command. The in-TUI "add session" dialog (`F5` by default) always writes new entries to `~/.config/smarthistory/sessions`, creating the file (and the `~/.config/smarthistory/` directory) if it doesn't exist yet. `~` (Zoxide) mode's "save this directory?" prompt (see [`zoxide.md`](modes/zoxide.md#selecting-a-row)) writes here too — a plain name + `.dir` entry, no `.exec`. `smarthistory prune-directories [-f]` is the cleanup side of that: it checks every `session.<key>.dir` (in both `sessions` and the main `config` file) against the filesystem and removes the whole entry for any that no longer exist, after listing them and asking for confirmation (`-f`/`--force` skips the prompt). Entries with no `.dir` set are left alone.

### `host.<key>`

| | |
| --- | --- |
| **Type** | sub-keyed group (`<key>` is an arbitrary identifier — see [`session.<key>`](#sessionkey) above) |
| **Default** | — (auto-appended from `~/.ssh/config`) |
| **File** | `~/.config/smarthistory/hosts` (or the main config file — see below) |
| **Tilde expansion** | Yes (on `dir`, `identity`) |
| **Env override** | — |

An SSH host row in the `# hosts` block of the `*` (panes) view. Same key scheme as `session.<key>` above — display order is file declaration order, and `<key>` is just a join key (auto-picked as a slug of the display name when written by the TUI, hand-editable to anything unique otherwise). Hosts in `~/.ssh/config` are auto-appended (one per `Host` block, keyed off the SSH alias itself) when the config is loaded, so users only need explicit `host.<key>` entries for the fields `~/.ssh/config` doesn't already cover, or to override what the SSH config says.

| Sub-key | Meaning |
| --- | --- |
| `host.<key>` | Display name (used in the picker) |
| `host.<key>.host` | The SSH `Host` alias (the connection target) |
| `host.<key>.hostname` | The real `HostName` to connect to (falls back to `host` if unset) |
| `host.<key>.user` | The SSH `User` |
| `host.<key>.port` | The SSH `Port` (positive integer; invalid values are dropped with a warning) |
| `host.<key>.identity` | The `IdentityFile` (path with `~` expanded) |
| `host.<key>.dir` | The directory the session starts in on the remote (after `ssh -t host 'cd … && $SHELL'`) |
| `host.<key>.exec` | The command to run after `cd` (e.g. `tmux new-session -A -s main`) |

```ini
# ~/.config/smarthistory/hosts
host.prod-db="prod-db"
host.prod-db.host=db1
host.prod-db.hostname=db1.internal.example.com
host.prod-db.user=ops
host.prod-db.port=2222
host.prod-db.identity=~/.ssh/id_ed25519_prod
host.prod-db.dir=/srv/observability
host.prod-db.exec=tmux new-session -A -s observability
```

**File location**: same split-or-combined rule as `session.<key>` above — `host.<key>` entries can live in `~/.config/smarthistory/hosts`, in the main config file, or both. The in-TUI "add host" dialog (`F6` by default) always writes new entries to `~/.config/smarthistory/hosts`.

**Reconnecting inside a new pane**: `smarthistory pane-exec` is a manual command for a fresh pane/window opened directly in tmux or herdr (e.g. `Ctrl-b c`, not through smarthistory's own `*` panes picker). tmux sessions and herdr workspaces are already named after the `session.<key>`/`host.<key>` entry's DISPLAY NAME (not `<key>` itself) that created them, so no separate registration step is needed — `pane-exec` just reads the current session name (or workspace label) and looks it up against every entry's display name. A `session.<key>` match re-runs its `.exec`; a `host.<key>` match re-runs its `ssh` connection only — the host's `.exec` is deliberately not replayed, since it's meant to be typed into the remote shell after connecting (via the multiplexer backend's pane-injection API), not run as a local follow-up command.

---

## Modes

Each mode has its own config keys (paths, file formats, per-mode behavior) plus a long-form doc page under **[docs/modes/](modes/README.md)**. This section only catalogs the config keys; click through for usage examples and per-mode detail.

### Notes (`@` mode)

| | |
| --- | --- |
| **Type** | path |
| **Default** | — (feature disabled) |
| **Tilde expansion** | Yes |
| **Env override** | `NOTE_SEARCH_DATABASE` / `NOTE_SEARCH_DIR` (win over config) |

Two paths the TUI needs to enable the `@` mode:

| Key | Path to | What |
| --- | --- | --- |
| `notes.database` | The `note_search` SQLite database | The FTS-indexed search index |
| `notes.dir` | The notes directory | The directory of note files (used to read content for the preview pane) |

Both paths are validated at config-load time: the database must exist as a file, the directory must exist as a directory. Missing paths produce a stderr warning and the `@` mode is disabled for that session. The environment variables `NOTE_SEARCH_DATABASE` and `NOTE_SEARCH_DIR` win over the config file (matching the `JIRA_*` / `SMARTHISTORY_MULTIPLEXER` precedence pattern), and they're also validated — a non-existent path silently leaves the config value in place rather than erroring.

```ini
notes.database=~/Documents/notes/.search.sqlite
notes.dir=~/Documents/notes
```

Both keys are also required by the TUI's `create-note` dialog (`Action::CreateNote`), including when reached via `smarthistory create-note [--title <T>] [--content <C>]` — a shortcut that opens the same interactive dialog directly (equivalent to `smarthistory tui --create-note`), with `--title`/`--content` pre-filling its fields — see [README — Quick-create from notes/todo mode](../README.md#quick-create-from-notestodo-mode).

Full reference: **[docs/modes/notes.md](modes/notes.md)**.

### Todo (`!` mode)

#### `todo.line_option`

| | |
| --- | --- |
| **Type** | template string containing the literal `$LINE` |
| **Default** | `+$LINE` |
| **Env override** | — |

Template for the line-number option that the todo-search mode appends to the editor command when the user selects a todo line. The string `"$LINE"` is substituted with the actual 1-based line number. The default `+$LINE` works with `vim`, `nano`, `emacs -nw`, and most POSIX editors.

A non-empty value that doesn't contain `$LINE` is rejected with a stderr warning (the default is preserved); an empty value is silently dropped.

```ini
todo.line_option=+$LINE        # vim / nano / emacs -nw
todo.line_option=--line $LINE  # micro
todo.line_option=+N$LINE       # unusual editors that want a literal 'N' before the number
```

Full reference: **[docs/modes/todo.md](modes/todo.md)**.

### Tags (`$` mode)

The `$` (tags) mode reads `./tags` in the current directory; there is no config key for the file path — it's the convention used by every ctags-compatible tool. When `./tags` is missing, the `$` mode falls back to the local `.codegraph/codegraph.db` index (FTS5), so a repo without a TAGS file still has symbol navigation as long as CodeGraph has indexed it.

The source-context preview (the 50-line window around a selected symbol) is loaded lazily on selection; this keeps the initial TAGS load fast even on multi-megabyte tag files. The preview is rendered through `bat` with the matching `--theme=light` / `--theme=dark` flag derived from the active theme's `bg` brightness.

Full reference: **[docs/modes/tags.md](modes/tags.md)**.

### Files (`/` mode)

#### `files.ignore`

| | |
| --- | --- |
| **Type** | space-separated list of directory basenames |
| **Default** | — (uses the built-in list) |
| **Env override** | — |

Additional directory basenames to skip during the files-mode walk. The list is **combined** with the built-in [`DEFAULT_IGNORES`](../src/files.rs) (`target`, `node_modules`, `.git`, `.codegraph`, `.github`, `.vscode`, `.idea`, `build`, `dist`, `_build`, `bazel-out`, `bazel-testlogs`, `bazel-bin`, `__pycache__`, `.next`, `.cache`, `.sass-cache`, `coverage`, `.nyc_output`) — so the user only needs to add project-specific patterns.

```ini
files.ignore=.venv .terraform .direnv .pytest_cache .mypy_cache
```

#### `smart-open.<ext>`

| | |
| --- | --- |
| **Type** | shell command (one per extension) |
| **Default** | — (falls through to the default `Run` action, which opens in `$EDITOR`) |
| **Env override** | — |

Per-extension shell command for the `/` (files) mode's `SmartOpen` dive (`Ctrl-]` by default). The selected file's absolute path is appended to the command (with POSIX single-quote escaping so spaces and shell metacharacters can't break the staged command), and the TUI exits so the parent shell runs it. The lookup is **case-insensitive**: `smart-open.MD=leaf` and `smart-open.md=leaf` are the same entry.

The reserved key `smart-open.default` is the fallback for any extension without an explicit mapping. Empty `<cmd>` values (e.g. `smart-open.rs=`) are silently dropped so a typo doesn't bind to an empty command.

```ini
smart-open.md=leaf            # markdown files → `leaf README.md`
smart-open.rs=bat             # rust code → `bat src/main.rs`
smart-open.py=bat             # python code → `bat script.py`
smart-open.default=bat        # any other text file → `bat <path>`
smart-open.png=xdg-open       # images → `xdg-open photo.png`
smart-open.pdf=zathura        # PDFs → `zathura file.pdf`
```

Full reference: **[docs/modes/files.md](modes/files.md)**.

### JIRA (`-` mode)

The `-` mode is configured entirely by environment variables — there are no `jira.*` config-file keys. Every variable is read at every search (not cached), so changes take effect on the next query.

| Variable | Required? | Default | Meaning |
| --- | --- | --- | --- |
| `JIRA_SERVER` | yes | — | The JIRA base URL (e.g. `https://jira.example.com`). Trailing slashes are stripped. |
| `JIRA_API_TOKEN` | yes | — | The API token (used as a Bearer token on the `/rest/api/3/search` endpoint). |
| `JIRA_URL` | no | same as `JIRA_SERVER` | The browse URL base (the `browse` link). Defaults to `JIRA_SERVER` when unset, so the API and browse URLs always share a host. |
| `JIRA_PROJECT` | no | — | A project key to scope the search (e.g. `ENG`). When unset, the empty-body query degrades to a server-wide `ORDER BY updated DESC`. |
| `JIRA_MAX_RESULTS` | no | `5` | The number of results to fetch per search (non-negative integer; invalid values fall back to `5`). |
| `JIRA_HOST_CERTIFICATE` | no | — | Path to a client certificate (PEM) for mTLS to the JIRA host. |
| `JIRA_HOST_CERTIFICATE_PASSWORD` | no | — | Password for the client certificate (if encrypted). |
| `JIRA_CA_CERTIFICATE` | no | — | Path to a CA bundle for verifying the JIRA server's TLS certificate (useful for self-signed or corporate CA setups). |

```sh
export JIRA_SERVER=https://jira.example.com
export JIRA_API_TOKEN=ATATTxxxxxxxxxxxx
export JIRA_PROJECT=ENG
export JIRA_MAX_RESULTS=20
```

#### `jira.search.<name>`

| | |
| --- | --- |
| **Type** | JQL fragment |
| **Default** | — |
| **Env override** | — |

User-defined JQL fragments. A fragment named `foo` is invoked in the `-`-mode TUI search as `@foo`; the fragment's JQL is spliced verbatim into the generated JQL. Names must be a non-empty `\w+` identifier; anything else is silently ignored. Empty JQL values are dropped. Reserved names (`me`, `today`, `week`, `month`) cannot be overridden — the loader silently drops them so a typo in the config can't disable a built-in alias.

User-defined fragments require the `@` prefix; the built-in aliases (`me`, `today`, `week`, `month`) remain permissive (work with or without `@`).

```ini
# Short aliases for the queries you run most often:
jira.search.mine=assignee = currentUser() AND status != Done
jira.search.review=assignee = currentUser() AND status = "In Review"
jira.search.recent=project = ENG AND updated >= -7d ORDER BY updated DESC
jira.search.kramfors=project = ENG AND text ~ "kramfors"
```

Full reference: **[docs/modes/jira.md](modes/jira.md)**.

### LLM (`=` mode)

The `=` (LLM command generation) and `?` (general question) modes require a running ollama instance. The configuration is **config-file only** (no env vars) and the feature is opt-in: if either `ollama.url` or `ollama.model` is missing, the LLM mode is disabled with a stderr warning.

| Key | Meaning |
| --- | --- |
| `ollama.url` | The ollama API base URL (e.g. `http://localhost:11434`). |
| `ollama.model` | The model name (e.g. `qwen2.5-coder:7b`, `llama3.1`). Must be one ollama has pulled (`ollama list` to see what's available). |

```ini
ollama.url=http://localhost:11434
ollama.model=qwen2.5-coder:7b
```

Full reference: **[docs/modes/llm.md](modes/llm.md)**.

### Paperless (`<` mode)

The `<` mode requires a self-hosted Paperless-ngx v3 instance. The configuration is **config-file only** (no env vars) and the feature is opt-in: if either `paperless.url` or `paperless.token` is missing, the mode is disabled with a stderr warning.

| Key | Meaning |
| --- | --- |
| `paperless.url` | The Paperless-ngx base URL (e.g. `https://paperless.example.com`). Used both as the REST API base and the web-UI base for the details-page URL opened on `Enter`. |
| `paperless.token` | The API token, sent as `Authorization: Token <token>`. Create one in Paperless-ngx under Settings → API Tokens. |

```ini
paperless.url=https://paperless.example.com
paperless.token=abcdef0123456789abcdef0123456789abcdef01
```

Full reference: **[docs/modes/paperless.md](modes/paperless.md)**.

### Browser (`^` mode)

The `^` mode reads bookmarks + history directly from locally-installed browsers' own profile files — no config is required. Zero or more `browser.<id>.*` entries let you override which browsers / profiles are read; when none are set, Chrome, Firefox, and Safari are all auto-detected at their platform-default profile locations (only a browser that's actually installed is included).

| Key | Meaning |
| --- | --- |
| `browser.<id>.type` | `chrome`, `firefox`, or `safari`. `<id>` is any numeric index — order doesn't matter. |
| `browser.<id>.profile` | Optional path override: for Chrome, the directory directly containing `Bookmarks` / `History`; for Firefox, the directory directly containing `places.sqlite`; for Safari, the directory directly containing `Bookmarks.plist` / `History.db` (normally `~/Library/Safari` — Safari has no separate profile concept, so this is rarely worth overriding). Omit to use that browser's platform-default profile location. |

```ini
# Read a non-default Chrome profile ("Profile 2") instead of "Default":
browser.1.type=chrome
browser.1.profile=~/Library/Application Support/Google/Chrome/Profile 2

# Add Firefox and Safari too (both using their auto-detected default profile):
browser.2.type=firefox
browser.3.type=safari
```

Full reference: **[docs/modes/browser.md](modes/browser.md)**.

---

## Environment variables

Most config has a config-file equivalent; the env-var form is for users who want to keep secrets out of a dotfile repo or override per-invocation.

| Variable | Overrides | Purpose |
| --- | --- | --- |
| `HOME` | — | Used to locate `~/.config/smarthistory/config`, `~/.local/cache/smarthistory/`, and `~/.cache/tmux-history/` (and `~/.ssh/config` for host auto-append). On Windows, `USERPROFILE` is also consulted. |
| `TMUX` | — | Set by tmux when running inside a session. Without it, the `SESS` and `DIR` scopes fall back to `GLOBAL` and the `*` mode shows nothing. |
| `TMUX_PANE` | — | The TUI's own pane id (set by tmux). Used to filter "self" out of the `*`-mode list, and as the suffix of the per-pane log file (`output-${TMUX_PANE}.log`). |
| `SMARTHISTORY_TUI_MODE` | `initialmode` | Initial TUI scope: `SESS` / `DIR` / `GLOBAL` (case-insensitive). |
| `SMARTHISTORY_MULTIPLEXER` | `multiplexer` | `tmux` or `herdr` (case-insensitive). Invalid values are dropped with a warning. |
| `NOTE_SEARCH_DATABASE` | `notes.database` | Path to the note_search SQLite database. Validated at startup; non-existent paths are dropped with a warning. |
| `NOTE_SEARCH_DIR` | `notes.dir` | Path to the notes directory. Validated at startup. |
| `JIRA_SERVER` | — | JIRA base URL (required for `-` mode). |
| `JIRA_API_TOKEN` | — | JIRA API token (required for `-` mode). |
| `JIRA_URL` | — | Browse URL base (defaults to `JIRA_SERVER`). |
| `JIRA_PROJECT` | — | Default project key. |
| `JIRA_MAX_RESULTS` | — | Results per search (default `5`). |
| `JIRA_HOST_CERTIFICATE` | — | Client certificate path (mTLS). |
| `JIRA_HOST_CERTIFICATE_PASSWORD` | — | Client certificate password. |
| `JIRA_CA_CERTIFICATE` | — | CA bundle for server-cert verification. |

### Published environment variables

The table above is env vars you set to configure smarthistory; these two go the other way — `init.zsh` publishes them so a *separate* prompt system (oh-my-posh, starship, a custom `precmd`, …) can show the current widget state without needing to know anything about `init.zsh`'s internal shell variables. A prompt system like oh-my-posh runs as its own subprocess on every prompt render, so it can only see real exported env vars — not zsh-internal state like `$_smarthistory_mode`. `init.zsh`'s own [`zsh.mode`](#zshmode)/`Ctrl-g` and [`dropdown.matchmode`](#dropdownmatchmode)/`Ctrl-t` widgets confirm each toggle with a transient `zle -M` status message ("smarthistory mode set to DIR") driven by this same underlying state, kept in sync by `_smarthistory_sync_prompt_env` — these two env vars are that state, exported for anyone who'd rather render it as a persistent prompt segment instead of a one-off message.

| Variable | Values | Updated by |
| --- | --- | --- |
| `SMARTHISTORY_MODE` | `sess` \| `dir` \| `global` | `Ctrl-g` (`_smarthistory_cycle_mode`) |
| `SMARTHISTORY_MATCHMODE` | `prefix` \| `substring` | `Ctrl-t` (`_smarthistory_cycle_matchmode`) |

**oh-my-posh** example — a `text` segment reading both via Go templates (add to your theme's `blocks[].segments`):

```json
{
  "type": "text",
  "style": "plain",
  "template": "[smarthistory: {{ .Env.SMARTHISTORY_MODE | upper }}{{ if eq .Env.SMARTHISTORY_MATCHMODE \"substring\" }}~{{ end }}]"
}
```

**starship** example (`~/.config/starship.toml`) — a `custom` command segment:

```toml
[custom.smarthistory]
command = "printf '[smarthistory: %s%s]' \"$(echo $SMARTHISTORY_MODE | tr a-z A-Z)\" \"$([ \"$SMARTHISTORY_MATCHMODE\" = substring ] && echo '~')\""
when = true
```

Both examples re-run their command/template on every prompt draw, so the indicator stays current as you toggle `Ctrl-g`/`Ctrl-t` — no shell restart needed.

---

## All keys at a glance

A flat index of every config-file key. Use this as a quick "does this key exist?" reference; the sections above are the long-form per-key docs.

| Key | Type | Default | Section |
| --- | --- | --- | --- |
| `tmuxpaneoutputdir` | path | `~/.cache/tmux-history` | [Capture & output](#capture--output) |
| `ignorecapture` | list | `cd ls pwd exit clear history fc jobs bg fg wait disown suspend` | [Capture & output](#capture--output) |
| `capturelines` | `ALL` \| int | `20` | [Capture & output](#capture--output) |
| `capturelines.<cmd>` | `ALL` \| int | — | [Capture & output](#capture--output) |
| `duplicatefilter` | `on` \| `off` | `on` | [History list & filtering](#history-list--filtering) |
| `initialmode` | enum | `SESS` | [History list & filtering](#history-list--filtering) |
| `zsh.mode` | `sess` \| `dir` \| `global` | `sess` | [History list & filtering](#history-list--filtering) |
| `segments.minwords` | non-negative int | `5` | [History list & filtering](#history-list--filtering) |
| `dropdown.enabled` | `on` \| `off` | `off` | [Live dropdown completion](#live-dropdown-completion) |
| `dropdown.limit` | positive int | `6` | [Live dropdown completion](#live-dropdown-completion) |
| `dropdown.minchars` | non-negative int | `1` | [Live dropdown completion](#live-dropdown-completion) |
| `dropdown.highlight` | `on` \| `off` | `off` | [Live dropdown completion](#live-dropdown-completion) |
| `dropdown.matchmode` | `prefix` \| `substring` | `prefix` | [Live dropdown completion](#live-dropdown-completion) |
| `commentexpand.enabled` | `on` \| `off` | `off` | [Comment expansion](#comment-expansion) |
| `globcomplete.enabled` | `on` \| `off` | `off` | [Glob-triggered Tab file completion](#glob-triggered-tab-file-completion) |
| `tuicolor.bg` | color | theme's `bg` | [Theme](#theme) |
| `tuicolor.fg` | color | theme's `fg` | [Theme](#theme) |
| `tuicolor.accent` | color | theme's `accent` | [Theme](#theme) |
| `tuicolor.success` | color | theme's `success` | [Theme](#theme) |
| `tuicolor.error` | color | theme's `error` | [Theme](#theme) |
| `tuicolor.warning` | color | theme's `warning` | [Theme](#theme) |
| `tuicolor.dim` | color | theme's `muted` | [Theme](#theme) |
| `tuicolor.highlight` | color | theme's `accent` | [Theme](#theme) |
| `tuicolor.info` | color | theme's `info` | [Theme](#theme) |
| `tuicolor.selection` | color | theme's `selection` | [Theme](#theme) |
| `tuicolor.badge_fg` | color | theme's `bg` | [Theme](#theme) |
| `tuicolor.list_bg` | color | theme's `bg` | [Theme](#theme) |
| `tuicolor.details_bg` | color | theme's `bg` | [Theme](#theme) |
| `tuicolor.input_bg` | color | theme's `bg` | [Theme](#theme) |
| `tuicolor.status_bg` | color | theme's `bg` | [Theme](#theme) |
| `key.<action>` | `KeySpec` | action's default key | [Key bindings](#key-bindings) |
| `prefix.output` | char | `+` | [Query prefixes](#query-prefixes) |
| `prefix.llm` | char | `=` | [Query prefixes](#query-prefixes) |
| `prefix.question` | char | `?` | [Query prefixes](#query-prefixes) |
| `prefix.notes` | char | `@` | [Query prefixes](#query-prefixes) |
| `prefix.todo` | char | `!` | [Query prefixes](#query-prefixes) |
| `prefix.directories` | char | `#` | [Query prefixes](#query-prefixes) |
| `prefix.panes` | char | `*` | [Query prefixes](#query-prefixes) |
| `prefix.files` | char | `/` | [Query prefixes](#query-prefixes) |
| `prefix.tags` | char | `$` | [Query prefixes](#query-prefixes) |
| `prefix.ag` | char | `,` | [Query prefixes](#query-prefixes) |
| `prefix.codegraph` | char | `&` | [Query prefixes](#query-prefixes) |
| `prefix.jira` | char | `-` | [Query prefixes](#query-prefixes) |
| `prefix.segments` | char | `:` | [Query prefixes](#query-prefixes) |
| `prefix.similar` | char | `"` | [Query prefixes](#query-prefixes) |
| `prefix.paperless` | char | `<` | [Query prefixes](#query-prefixes) |
| `prefix.browser` | char | `^` | [Query prefixes](#query-prefixes) |
| `prefix.zoxide` | char | `~` | [Query prefixes](#query-prefixes) |
| `prefix.processes` | char | `%` | [Query prefixes](#query-prefixes) |
| `prefix.meta` | char | `'` | [Query prefixes](#query-prefixes) |
| `multiplexer` | `tmux` \| `herdr` | `tmux` | [Multiplexer integration](#multiplexer-integration) |
| `sessiondirs` | path list | — | [Multiplexer integration](#multiplexer-integration) |
| `homemap` | path prefix list | — | [Multiplexer integration](#multiplexer-integration) |
| `session.<key>` | string | — | [Multiplexer integration](#multiplexer-integration) |
| `session.<key>.dir` | path | — | [Multiplexer integration](#multiplexer-integration) |
| `session.<key>.exec` | string | — | [Multiplexer integration](#multiplexer-integration) |
| `session.<key>.startup_command` | string | (reserved) | [Multiplexer integration](#multiplexer-integration) |
| `host.<key>` | string | — (auto from `~/.ssh/config`) | [Multiplexer integration](#multiplexer-integration) |
| `host.<key>.host` | string | — | [Multiplexer integration](#multiplexer-integration) |
| `host.<key>.hostname` | string | — | [Multiplexer integration](#multiplexer-integration) |
| `host.<key>.user` | string | — | [Multiplexer integration](#multiplexer-integration) |
| `host.<key>.port` | int | — | [Multiplexer integration](#multiplexer-integration) |
| `host.<key>.identity` | path | — | [Multiplexer integration](#multiplexer-integration) |
| `host.<key>.dir` | path | — | [Multiplexer integration](#multiplexer-integration) |
| `host.<key>.exec` | string | — | [Multiplexer integration](#multiplexer-integration) |
| `notes.database` | path | — (feature disabled) | [Notes (`@` mode)](#notes--mode) |
| `notes.dir` | path | — (feature disabled) | [Notes (`@` mode)](#notes--mode) |
| `todo.line_option` | template | `+$LINE` | [Todo (`!` mode)](#todo--mode) |
| `files.ignore` | list | — (uses built-in) | [Files (`/` mode)](#files--mode) |
| `smart-open.<ext>` | command | — (falls through to `Run`) | [Files (`/` mode)](#files--mode) |
| `smart-open.default` | command | — | [Files (`/` mode)](#files--mode) |
| `jira.search.<name>` | JQL | — | [JIRA (`-` mode)](#jira--mode) |
| `ollama.url` | URL | — (LLM disabled) | [LLM (`=` mode)](#llm--mode) |
| `ollama.model` | model name | — (LLM disabled) | [LLM (`=` mode)](#llm--mode) |
| `paperless.url` | URL | — (paperless disabled) | [Paperless (`<` mode)](#paperless--mode) |
| `paperless.token` | API token | — (paperless disabled) | [Paperless (`<` mode)](#paperless--mode) |
| `browser.<id>.type` | `chrome` \| `firefox` \| `safari` | — (auto-detected) | [Browser (`^` mode)](#browser--mode) |
| `browser.<id>.profile` | path | — (platform default) | [Browser (`^` mode)](#browser--mode) |

---

**See also**:

- **[docs/actions.md](actions.md)** — every key binding action (48 actions, with default keys and detailed descriptions).
- **[docs/modes/](modes/README.md)** — per-mode reference (every prefix mode's behavior, example queries, special tokens).
- **[docs/multiplexer.md](multiplexer.md)** — tmux / herdr backend setup, environment variable precedence, troubleshooting.
- **[README.md](../README.md)** — the high-level overview; this file is the long-form config reference.
- **[TECHNICAL.md](../TECHNICAL.md)** — implementation-level reference for the data model and code structure.

---

## Troubleshooting with `smarthistory check`

```sh
smarthistory check              # health-check every prefix mode
smarthistory check --prefix @   # check only the notes mode
smarthistory check --prefix '&' # check only the codegraph mode
```

The `check` command builds the same `App` as the TUI startup (so it reads the same config file, opens the same DB, resolves the same multiplexer backend) and then runs a **progressive** per-mode health check. Each mode digs down as far as it can before reporting:

- **Notes (`@`)** / **Todos (`!`)**: `notes.database` is configured → the file exists → opens as sqlite → has the required tables (`markdown_data`, `todo_entries`) → a sample `search_notes` / `search_todos` round-trip succeeds → row count.
- **Tags (`$`)**: a `tags`/`TAGS` file is discoverable (walk upward from cwd) → readable → parses (has `\x0c`-separated sections) → if no tags file, checks the CodeGraph fallback.
- **CodeGraph (`&`)**: `.codegraph/codegraph.db` is discoverable → opens as sqlite → has `nodes` + `edges` tables + `nodes_fts` FTS5 virtual table → a trivial FTS5 search succeeds → row/edge counts.
- **Files (`/`)**: cwd exists → `walk_dir` returns at least one entry (or the dir is genuinely empty → `Warning`).
- **Ag (`,`)**: `ag` binary is on `$PATH` → `ag --version` succeeds.
- **LLM (`=`)**: `ollama.url` + `ollama.model` are configured → ollama server is reachable (`GET /api/tags`) → the configured model is in the loaded-models list.
- **JIRA (`-`)**: `JIRA_SERVER` + `JIRA_API_TOKEN` env vars are set → the server is reachable (`GET /rest/api/3/myself` with Bearer auth) → the `JIRA_PROJECT` (if set) exists.
- **Paperless (`<`)**: `paperless.url` + `paperless.token` are configured → the server is reachable and the token is accepted (`GET /api/documents/?page_size=1` with `Authorization: Token ...`).
- **Browser (`^`)**: at least one `browser.<id>.*` entry (or an auto-detected Chrome/Firefox/Safari install) resolves → each source's primary file (`Bookmarks` / `places.sqlite` / `Bookmarks.plist`) actually opens. A missing profile is a per-source `Warning` (the other configured sources still work); a permission error (the common case: Safari on macOS without Full Disk Access — see `docs/modes/browser.md`) is a per-source `Error` with the fix spelled out.
- **Directories (`#`)**: SQL history DB is open (with a `COUNT(DISTINCT directory)` round-trip) → multiplexer backend is configured → each `sessiondirs` entry exists on disk.
- **Panes (`*`)**: the user is inside a multiplexer session (`$TMUX` or `$HERDR_PANE_ID` is set) → the backend's `snapshot_current_panes` returns at least one pane.

**Exit code**: 0 = all checks pass, 1 = one or more warnings, 2 = one or more errors.
