# Configuration reference

Every value `smarthistory` reads at startup, with the config-file key, default, semantics, and a worked example. The canonical source is [`src/main.rs::Config`](../src/main.rs) (the `Config` struct, `Config::default`, and `Config::parse`); this file mirrors that source so the docs and code stay close enough to spot drift.

**Where the config lives**

| Location | What |
| --- | --- |
| `~/.config/smarthistory/config` | The user config file (INI-style `key=value` lines; `#` starts a comment; `~` is expanded in path values). Read once at startup by `Config::load`. Missing file → built-in defaults. |
| `~/.local/cache/smarthistory/` | Runtime cache: `query_history.json` (per-mode recall), `widget-debug.log` (TUI debug trace), `last_session.json` (the most recent session, for `smarthistory tui` resume). Not hand-edited. |
| `~/.cache/tmux-history/` | Per-pane tmux output logs (set via `tmuxpaneoutputdir`). |
| `~/.config/smarthistory/themes/` | Optional user theme directory (TOML files matching the built-in theme shape). |

**Loading order** (later wins):

1. Built-in defaults (`Config::default`).
2. `~/.config/smarthistory/config` — each `key=value` line is parsed in order; later values override earlier ones.
3. Environment variables (noted per-item below; they always win over the config file when set).

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
  - [`session.<id>`](#sessionid)
  - [`host.<id>`](#hostid)
- [Modes](#modes)
  - [Notes (`@` mode)](#notes--mode)
  - [Todo (`!` mode)](#todo--mode)
  - [Tags (`$` mode)](#tags--mode)
  - [Files (`~` mode)](#files--mode)
  - [JIRA (`-` mode)](#jira--mode)
  - [LLM (`=` mode)](#llm--mode)
- [Environment variables](#environment-variables)
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

The built-in theme is set via the TUI's theme picker (`F2` / `ThemePicker` by default). The selected theme is persisted to the session file (`~/.local/cache/smarthistory/last_session.json`) and reapplied on next TUI launch. There is no config-file key to set the theme — the picker is the canonical path (so the searchable picker, the live preview, and the session-file write all stay in sync). 73 themes ship in `src/tui/theme/curated/` (15 upstream from `ratatui_themes` + 58 curated).

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
| `prefix.question` | general question | `%` | one-off LLM chat |
| `prefix.notes` | note search | `@` | searches the notes database |
| `prefix.todo` | todo search | `!` | markdown task-list scanner |
| `prefix.directories` | directories | `#` | unique dirs from history |
| `prefix.panes` | session panes | `*` | tmux / herdr panes |
| `prefix.files` | files | `~` | file browser |
| `prefix.tags` | tags | `$` | ctags symbol search |
| `prefix.ag` | ag content search | `,` | silver-searcher |
| `prefix.codegraph` | codegraph | `&` | FTS5 symbol search |
| `prefix.jira` | JIRA | `-` | JIRA issue search |

```ini
# Move JIRA off `-` (a frequently mistyped key) to backtick:
prefix.jira=`

# Move the rarely-used question mode to a less crowded slot:
prefix.question=?
```

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

Directories whose sub-tree is recursively walked at TUI startup; every directory found is added to the `#`-mode list, even when the user has never run a command there. The user's "always show me these projects" list. Multiple entries are allowed (one per line, like `prefix.<x>=`). A non-existent path is silently skipped (the user may have moved the directory; the next startup with the path back picks it up). The `~` is expanded at config-load time so `sessiondirs=~/.config/tmux-sessions` resolves to your real home, not the literal `~` directory.

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

### `session.<id>`

| | |
| --- | --- |
| **Type** | sub-keyed group (`<id>` is a non-negative integer) |
| **Default** | — |
| **Tilde expansion** | Yes (on `dir`) |
| **Env override** | — |

A named session row in the `*` (panes) view. The `<id>` is the display order (1-based, ascending); a missing `<id>` keeps the entry sorted after the existing max. Sub-keys:

| Sub-key | Required? | Meaning |
| --- | --- | --- |
| `session.<id>` | yes | The display name (used in the picker / status bar) |
| `session.<id>.dir` | no | The directory the session starts in (after `cd`) |
| `session.<id>.exec` | no | The command to run after creating the workspace (e.g. `nvim`, `claude`) |
| `session.<id>.startup_command` | accepted, not yet used | Reserved for future use |

```ini
session.1="monorepo"
session.1.dir=~/work/monorepo
session.1.exec=claude

session.2="notes"
session.2.dir=~/Documents/notes
```

### `host.<id>`

| | |
| --- | --- |
| **Type** | sub-keyed group (`<id>` is a non-negative integer) |
| **Default** | — (auto-appended from `~/.ssh/config`) |
| **Tilde expansion** | Yes (on `dir`, `identity`) |
| **Env override** | — |

An SSH host row in the `# hosts` block of the `*` (panes) view. The `<id>` is the display order; missing `<id>` sorts after the existing max. Hosts in `~/.ssh/config` are auto-appended (one per `Host` block) when the config file is loaded, so users only need explicit `host.<id>` entries for the fields `~/.ssh/config` doesn't already cover, or to override what the SSH config says.

| Sub-key | Meaning |
| --- | --- |
| `host.<id>` | Display name (used in the picker) |
| `host.<id>.host` | The SSH `Host` alias (the connection target) |
| `host.<id>.hostname` | The real `HostName` to connect to (falls back to `host` if unset) |
| `host.<id>.user` | The SSH `User` |
| `host.<id>.port` | The SSH `Port` (positive integer; invalid values are dropped with a warning) |
| `host.<id>.identity` | The `IdentityFile` (path with `~` expanded) |
| `host.<id>.dir` | The directory the session starts in on the remote (after `ssh -t host 'cd … && $SHELL'`) |
| `host.<id>.exec` | The command to run after `cd` (e.g. `tmux new-session -A -s main`) |

```ini
host.1="prod-db"
host.1.host=db1
host.1.hostname=db1.internal.example.com
host.1.user=ops
host.1.port=2222
host.1.identity=~/.ssh/id_ed25519_prod
host.1.dir=/srv/observability
host.1.exec=tmux new-session -A -s observability
```

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

### Files (`~` mode)

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

Per-extension shell command for the `~` (files) mode's `SmartOpen` dive (`Ctrl-]` by default). The selected file's absolute path is appended to the command (with POSIX single-quote escaping so spaces and shell metacharacters can't break the staged command), and the TUI exits so the parent shell runs it. The lookup is **case-insensitive**: `smart-open.MD=leaf` and `smart-open.md=leaf` are the same entry.

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

The `=` (LLM command generation) and `%` (general question) modes require a running ollama instance. The configuration is **config-file only** (no env vars) and the feature is opt-in: if either `ollama.url` or `ollama.model` is missing, the LLM mode is disabled with a stderr warning.

| Key | Meaning |
| --- | --- |
| `ollama.url` | The ollama API base URL (e.g. `http://localhost:11434`). |
| `ollama.model` | The model name (e.g. `qwen2.5-coder:7b`, `llama3.1`). Must be one ollama has pulled (`ollama list` to see what's available). |

```ini
ollama.url=http://localhost:11434
ollama.model=qwen2.5-coder:7b
```

Full reference: **[docs/modes/llm.md](modes/llm.md)**.

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
| `prefix.question` | char | `%` | [Query prefixes](#query-prefixes) |
| `prefix.notes` | char | `@` | [Query prefixes](#query-prefixes) |
| `prefix.todo` | char | `!` | [Query prefixes](#query-prefixes) |
| `prefix.directories` | char | `#` | [Query prefixes](#query-prefixes) |
| `prefix.panes` | char | `*` | [Query prefixes](#query-prefixes) |
| `prefix.files` | char | `~` | [Query prefixes](#query-prefixes) |
| `prefix.tags` | char | `$` | [Query prefixes](#query-prefixes) |
| `prefix.ag` | char | `,` | [Query prefixes](#query-prefixes) |
| `prefix.codegraph` | char | `&` | [Query prefixes](#query-prefixes) |
| `prefix.jira` | char | `-` | [Query prefixes](#query-prefixes) |
| `multiplexer` | `tmux` \| `herdr` | `tmux` | [Multiplexer integration](#multiplexer-integration) |
| `sessiondirs` | path list | — | [Multiplexer integration](#multiplexer-integration) |
| `homemap` | path prefix list | — | [Multiplexer integration](#multiplexer-integration) |
| `session.<id>` | string | — | [Multiplexer integration](#multiplexer-integration) |
| `session.<id>.dir` | path | — | [Multiplexer integration](#multiplexer-integration) |
| `session.<id>.exec` | string | — | [Multiplexer integration](#multiplexer-integration) |
| `session.<id>.startup_command` | string | (reserved) | [Multiplexer integration](#multiplexer-integration) |
| `host.<id>` | string | — (auto from `~/.ssh/config`) | [Multiplexer integration](#multiplexer-integration) |
| `host.<id>.host` | string | — | [Multiplexer integration](#multiplexer-integration) |
| `host.<id>.hostname` | string | — | [Multiplexer integration](#multiplexer-integration) |
| `host.<id>.user` | string | — | [Multiplexer integration](#multiplexer-integration) |
| `host.<id>.port` | int | — | [Multiplexer integration](#multiplexer-integration) |
| `host.<id>.identity` | path | — | [Multiplexer integration](#multiplexer-integration) |
| `host.<id>.dir` | path | — | [Multiplexer integration](#multiplexer-integration) |
| `host.<id>.exec` | string | — | [Multiplexer integration](#multiplexer-integration) |
| `notes.database` | path | — (feature disabled) | [Notes (`@` mode)](#notes--mode) |
| `notes.dir` | path | — (feature disabled) | [Notes (`@` mode)](#notes--mode) |
| `todo.line_option` | template | `+$LINE` | [Todo (`!` mode)](#todo--mode) |
| `files.ignore` | list | — (uses built-in) | [Files (`~` mode)](#files--mode) |
| `smart-open.<ext>` | command | — (falls through to `Run`) | [Files (`~` mode)](#files--mode) |
| `smart-open.default` | command | — | [Files (`~` mode)](#files--mode) |
| `jira.search.<name>` | JQL | — | [JIRA (`-` mode)](#jira--mode) |
| `ollama.url` | URL | — (LLM disabled) | [LLM (`=` mode)](#llm--mode) |
| `ollama.model` | model name | — (LLM disabled) | [LLM (`=` mode)](#llm--mode) |

---

**See also**:

- **[docs/actions.md](actions.md)** — every key binding action (48 actions, with default keys and detailed descriptions).
- **[docs/modes/](modes/README.md)** — per-mode reference (every prefix mode's behavior, example queries, special tokens).
- **[docs/multiplexer.md](multiplexer.md)** — tmux / herdr backend setup, environment variable precedence, troubleshooting.
- **[README.md](../README.md)** — the high-level overview; this file is the long-form config reference.
- **[TECHNICAL.md](../TECHNICAL.md)** — implementation-level reference for the data model and code structure.
