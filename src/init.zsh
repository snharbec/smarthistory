# shellcheck shell=zsh
# shellcheck disable=SC2296,SC2153,SC2086,SC2034
# Smart History ZSH Init
# Generate a unique session ID 
# The UUID is produced by the smarthistory binary itself (no uuidgen,
# no /dev/urandom, no OS RNG), so it works in any minimal environment.
export SMART_HISTORY_SESSION="{session_id}"

# Debug logging. Set SMARTHISTORY_DEBUG=1 in the environment to enable
# the line-editor widget to log its decisions to
# ~/.local/cache/smarthistory/widget-debug.log. Useful when the Up/Down
# widget shows unexpected matches (e.g. commands from another terminal).
#   export SMARTHISTORY_DEBUG=1
#   tail -f ~/.local/cache/smarthistory/widget-debug.log

# Capture the about-to-run command line in preexec (before execution, when
# $? still reflects the previous command, so we must NOT read it here).
_smarthistory_preexec() {
    _smarthistory_cmd="$1"
}
# Capture $? in precmd (after the command has finished, before the next
# prompt) and record both the command and its real exit code.
_smarthistory_precmd() {
    local exit_code=$?
    # Defensive: make sure no stale dropdown POSTDISPLAY can possibly
    # bleed into the next prompt (belt-and-suspenders on top of the
    # accept-line/send-break clears below; BUFFER is fresh here anyway,
    # but this costs nothing and closes the last edge case).
    _smarthistory_dropdown_clear
    # A new prompt means a new command line — any Esc/Ctrl-K
    # suppression from the line that just ended (however it ended:
    # accepted, Ctrl-C aborted, or a bare empty Enter) must not carry
    # over. See `_smarthistory_dropdown_suppressed`'s declaration.
    _smarthistory_dropdown_suppressed=0
    # Skip empty command lines (e.g. bare Enter presses).
    [ -n "$_smarthistory_cmd" ] || return 0
    # Skip space-prefixed command lines. Zsh's
    # HIST_NO_STORE (default-on) treats any command
    # whose first character is whitespace as "do not
    # save to the shell history" — the canonical
    # convention for "this command shouldn't be
    # persisted". We honour the same convention for
    # the smarthistory database: a space-prefixed
    # command is sensitive (a credential, a
    # destructive op, a private URL) and must not be
    # recorded. The TUI also prepends a space to every
    # staged selection (see `prefix_selection_with_space`
    # in src/tui.rs) so this single guard handles
    # both paths: user-typed space-prefixed commands
    # AND TUI-staged selections stay out of the DB.
    #
    # The pattern `[[:space:]]*` matches any leading
    # whitespace character (space, tab, NBSP, etc.).
    # zsh's `[[ str == pattern ]]` supports POSIX
    # character classes directly. We also clear the
    # captured command and reset the Ctrl-S cycle
    # index, but deliberately do NOT update
    # `_smarthistory_last_cmd` — the Ctrl-S "next
    # probable command" widget should not suggest a
    # command the user explicitly asked not to
    # record.
    if [[ "$_smarthistory_cmd" == [[:space:]]* ]]; then
        _smarthistory_next_index=0
        _smarthistory_cmd=""
        return 0
    fi
    # When running inside a herdr workspace pane, read the
    # pane scrollback via `herdr pane read` and extract
    # the command line + output automatically.
    if [ -n "$HERDR_PANE_ID" ]; then
        smarthistory capture-herdr "$_smarthistory_cmd" --exit-code $exit_code 2>/dev/null
    # When running inside a tmux session, the full pane is mirrored to
    # ~/.cache/tmux-history/output-${TMUX_PANE}.log. If that file
    # exists, use `smarthistory capture-tmux` to grab the command line
    # and the following output (up to 20 lines) automatically. This
    # avoids an explicit `smarthistory capture <cmd>` call.
    elif [ -n "$TMUX" ] && [ -n "$TMUX_PANE" ]; then
        # Discover the configured tmux pane output directory. Falls
        # back to the default location if the binary is unavailable
        # or returns nothing.
        local tmux_dir
        tmux_dir=$(smarthistory config get tmuxpaneoutputdir 2>/dev/null)
        if [ -z "$tmux_dir" ]; then
            tmux_dir="$HOME/.cache/tmux-history"
        fi
        local tmux_log="$tmux_dir/output-${TMUX_PANE}.log"
        if [ -f "$tmux_log" ]; then
            smarthistory capture-tmux "$_smarthistory_cmd" "$tmux_log" --exit-code $exit_code 2>/dev/null
        else
            smarthistory add "$_smarthistory_cmd" --exit-code $exit_code
        fi
    else
        smarthistory add "$_smarthistory_cmd" --exit-code $exit_code
    fi
    # Remember the most recently executed command for the Ctrl-S
    # "next probable command" widget. Reset the cycle index so the
    # next press starts with the most probable candidate. The
    # space-prefixed case (above) early-returns before this point,
    # so `_smarthistory_last_cmd` is only updated for commands
    # the user wants recorded — Ctrl-S will not suggest a
    # sensitive command.
    _smarthistory_last_cmd="$_smarthistory_cmd"
    _smarthistory_next_index=0
    _smarthistory_cmd=""
}
# Cycle index for the Ctrl-S widget (which next-candidate to pick).
# Reset to 0 after each executed command.
_smarthistory_next_index=0
autoload -Uz add-zsh-hook
add-zsh-hook preexec _smarthistory_preexec
add-zsh-hook precmd _smarthistory_precmd

# History selection using the smarthistory TUI (Ctrl+R).
# The TUI draws to stderr (so the user sees the picker) and prints the
# chosen command to stdout (so $() captures it cleanly).
# Exit codes:
#   0 -> Enter:        prefill BUFFER and submit the line
#   2 -> Right:        prefill BUFFER, cursor at end, do NOT submit
#   3 -> Left:         prefill BUFFER, cursor at start, do NOT submit
#   1 -> Esc/Ctrl+C:   cancel, leave BUFFER untouched
_smarthistory_select() {
    local selected rc
    selected=$(smarthistory tui)
    rc=$?
    if [ -n "$selected" ]; then
        BUFFER="$selected"
        case $rc in
            0)  zle accept-line ;;
            2)  CURSOR=${#BUFFER} ;;
            3)  CURSOR=0 ;;
            *)  CURSOR=${#BUFFER} ;;  # unknown code: default to end
        esac
    fi
}
zle -N _smarthistory_select
bindkey '^R' _smarthistory_select

# Up-arrow: when the user has typed something, replace the current line
# with the next match from the smarthistory DB. Each press moves back
# through the result set. When the line is empty, fall through to
# zsh's native history walk so empty Up/Down still does what the user
# expects.
#
# State is cached in two module-level variables so subsequent Up presses
# can walk the result set without re-querying the DB:
#   _smarthistory_matches : newline-separated list of all matches
#   _smarthistory_index   : 0-based position of the currently shown match
# Both are reset whenever LBUFFER changes (see the zle-line-precmd hook).
_smarthistory_matches=""
_smarthistory_index=0
# Cache key for the last search: "mode|pwd|prefix". Used to detect when
# the user changes directory, switches scope (Ctrl-g), or types a new
# prefix, so the match list gets re-queried in those cases.
_smarthistory_query_key=""
# The most recent match we set BUFFER to. We compare the current
# BUFFER against this on each Up/Down to distinguish "user pressed
# Up again" (BUFFER == _smarthistory_last_match) from "user typed
# something new" (anything else).
_smarthistory_last_match=""
# Search scope: "sess" = current $SMART_HISTORY_SESSION only,
# "dir" = current working directory only, "global" = no scope filter.
# Cycled with Ctrl-g. The starting value for a brand-new shell is
# configurable via `zsh.mode=sess|dir|global` in
# ~/.config/smarthistory/config (defaults to "sess", the historical
# hardcoded value); this only picks what a new shell starts on —
# Ctrl-g still cycles sess -> dir -> global -> sess regardless.
_smarthistory_mode=$(smarthistory config get zsh.mode 2>/dev/null)
case "$_smarthistory_mode" in
    sess|dir|global) ;;
    *) _smarthistory_mode="sess" ;;
esac

# Save the user's original RPROMPT (if any) at init time so we can
# append our mode indicator without clobbering their customization.
typeset -g _smarthistory_rprompt_save="$RPROMPT"

# ---- Live dropdown completion (opt-in, config file only) ----
#
# Shows a live, multi-candidate suggestion menu below the cursor as
# the user types — unlike every other widget in this file, which
# fires on an explicit keypress (Ctrl+R, Up/Down, Ctrl+S). Off by
# default: this hooks every keystroke, a bigger behavior change than
# the prefix-triggered features. Enable with `dropdown.enabled=on`
# in ~/.config/smarthistory/config (requires a new shell — config is
# read once here at init time, same as every other cached value in
# this file, e.g. `_smarthistory_rprompt_save` above).
#
# Rendering uses zsh's `POSTDISPLAY` parameter (the same mechanism
# zsh-autosuggestions uses for ghost text) rather than hand-rolled
# ANSI cursor math — `POSTDISPLAY` isn't limited to one line, and
# zle's own redisplay engine handles redraw-diffing and off-screen
# scroll-safety for it. See docs/dropdown-completion.md for the full
# design rationale (why POSTDISPLAY, why the strip-ANSI width math,
# why no debounce).
typeset -g _smarthistory_dropdown_enabled="0"
typeset -g _smarthistory_dropdown_limit=6
typeset -g _smarthistory_dropdown_minchars=1
if [[ "$(smarthistory config get dropdown.enabled 2>/dev/null)" == "on" ]]; then
    _smarthistory_dropdown_enabled="1"
    _smarthistory_dropdown_limit_raw=$(smarthistory config get dropdown.limit 2>/dev/null)
    [[ "$_smarthistory_dropdown_limit_raw" == <-> ]] && _smarthistory_dropdown_limit=$_smarthistory_dropdown_limit_raw
    _smarthistory_dropdown_minchars_raw=$(smarthistory config get dropdown.minchars 2>/dev/null)
    [[ "$_smarthistory_dropdown_minchars_raw" == <-> ]] && _smarthistory_dropdown_minchars=$_smarthistory_dropdown_minchars_raw
    unset _smarthistory_dropdown_limit_raw _smarthistory_dropdown_minchars_raw
fi
# Optional per-candidate syntax highlighting inside the dropdown box
# (`dropdown.highlight=on`): lexical token coloring via `bat`, plus a
# self-checked green/red for the first word's alias/function/
# builtin/command validity (`bat` alone can't tell a valid command
# from a typo — see `_smarthistory_command_validity_hlspec` below).
# Silently stays off if `bat` isn't on `$PATH`, even when the config
# says on — same soft-dependency convention as the rest of this app
# (surfaced via `smarthistory check`, not a startup warning).
typeset -g _smarthistory_dropdown_highlight_enabled="0"
if [[ "$_smarthistory_dropdown_enabled" = "1" \
    && "$(smarthistory config get dropdown.highlight 2>/dev/null)" == "on" \
    && -n "$(command -v bat 2>/dev/null)" ]]; then
    _smarthistory_dropdown_highlight_enabled="1"
fi
# Comment-expansion: type a comment's text at the start of the
# command line, then a space, and it expands to the most recently
# used command carrying that exact comment (`smarthistory add ...
# --comment ...`) — the same UX as zsh-abbr/fish abbreviations. Off
# by default like `dropdown.enabled` above. Enable with
# `commentexpand.enabled=on` in ~/.config/smarthistory/config.
typeset -g _smarthistory_commentexpand_enabled="0"
if [[ "$(smarthistory config get commentexpand.enabled 2>/dev/null)" == "on" ]]; then
    _smarthistory_commentexpand_enabled="1"
fi
# (Palette init runs further down, after
# `_smarthistory_color_to_hlspec` is defined — see the comment block
# marked "Resolved TUI palette" near `_smarthistory_strip_ansi`.)
# Whether the menu is currently drawn, the 0-based highlighted row,
# and the raw (still `\n`/`\r`-escaped, per the CLI's one-line-per-row
# convention) candidate commands for the current render.
typeset -g _smarthistory_dropdown_visible=0
typeset -g _smarthistory_dropdown_selected=0
# Set to 1 by an explicit dismissal (Esc, Ctrl-K) so the menu stays
# closed for the rest of this command line, even though the wrapped
# editing widgets keep firing `_smarthistory_dropdown_render` on every
# keystroke. Cleared back to 0 in `_smarthistory_precmd` — i.e. the
# suppression lasts exactly "until a new command line", matching every
# exit path (Enter, Ctrl-C abort, empty Enter) uniformly, since precmd
# runs before the next prompt regardless of how the previous line ended.
typeset -g _smarthistory_dropdown_suppressed=0
# Whether the user has actually navigated the menu (Tab / Shift-Tab)
# since it was last (re)painted with a new candidate set. Enter only
# commits `_smarthistory_dropdown_selected` when this is 1 — with no
# explicit choice, `_smarthistory_dropdown_selected` sits at its
# default 0 but nothing is highlighted and Enter just runs the typed
# buffer. See `_smarthistory_reset_and_accept`.
typeset -g _smarthistory_dropdown_chosen=0
typeset -ga _smarthistory_dropdown_candidates
# `_smarthistory_dropdown_meta[i]` holds the trimmed `diff` (age,
# e.g. "5m", "2h", "3d") string for `_smarthistory_dropdown_candidates[i]`
# — the two arrays are built in lockstep by `_smarthistory_dropdown_render`
# and are always the same length. Only `_smarthistory_dropdown_paint`
# ever reads this array (to draw the right-aligned "last called"
# column). The commit/accept widgets (`_smarthistory_dropdown_commit`,
# `_smarthistory_reset_and_accept`, `_smarthistory_dropdown_run_first`)
# must keep reading ONLY `_smarthistory_dropdown_candidates` — that
# array holds bare command text and nothing else, so it can be
# unescaped straight into BUFFER without stripping anything back out.
typeset -ga _smarthistory_dropdown_meta
# `_smarthistory_dropdown_hl_spans[i]` holds the optional
# `dropdown.highlight` syntax-color spans for
# `_smarthistory_dropdown_candidates[i]`, as newline-joined
# `"start end fg=#rrggbb"` entries (see `_smarthistory_ansi_to_spans`)
# — empty string when highlighting is off or this row's `bat` output
# didn't round-trip to the exact candidate text. Built in lockstep
# with the two arrays above by `_smarthistory_dropdown_render`, read
# only by `_smarthistory_dropdown_paint`.
typeset -ga _smarthistory_dropdown_hl_spans

# Clear the menu (if any) and mark it not visible. Safe to call
# unconditionally (e.g. from precmd) even when nothing is showing.
_smarthistory_dropdown_clear() {
    [[ -n "$POSTDISPLAY" ]] && POSTDISPLAY=""
    # Also drop any `region_highlight` entries the box paint left
    # behind for the POSTDISPLAY region (offsets >= $#BUFFER).
    # Entries below that offset belong to $BUFFER itself (e.g. a
    # syntax-highlighting plugin's own region_highlight use) and
    # must be left untouched — see `_smarthistory_dropdown_hl_prune`.
    _smarthistory_dropdown_hl_prune
    _smarthistory_dropdown_visible=0
    _smarthistory_dropdown_chosen=0
}

# Remove every `region_highlight` entry whose start offset falls
# in the POSTDISPLAY region (>= $#BUFFER) — i.e. every entry the
# box paint itself added — while leaving any entry that starts
# inside $BUFFER untouched. `region_highlight` is a shared zle
# array (zsh-syntax-highlighting and similar plugins also write to
# it for the command-buffer text), so this widget must never blank
# the whole array — only ever prune its own tail.
_smarthistory_dropdown_hl_prune() {
    (( ${#region_highlight} == 0 )) && return
    local base=$#BUFFER
    # `_entry` (the loop variable) and `_parts` are declared once,
    # before the loop, and only ever plain-assigned inside it — a
    # bare `local` re-declaration inside a loop body makes zsh
    # print the existing binding (`varname=value`) to stdout on
    # every iteration, which would leak into the user's terminal.
    local -a _kept _parts
    local _entry
    for _entry in "${region_highlight[@]}"; do
        _parts=(${=_entry})
        (( _parts[1] < base )) && _kept+=("$_entry")
    done
    region_highlight=("${_kept[@]}")
}

# Convert a CSS color name, 16-color terminal name, or `#rrggbb` /
# `0xrrggbb` hex string into a zle `region_highlight` foreground
# spec (`fg=<index>` or `fg=#rrggbb`). Used by the palette-init
# block above to turn `smarthistory config get palette`'s
# `key=value` lines into specs the box-drawing paint appends to
# `region_highlight`.
#
# IMPORTANT: this deliberately does NOT return raw ANSI SGR bytes
# (`\x1b[36m` etc). `POSTDISPLAY` is plain buffer text as far as
# zle's redisplay engine is concerned — it does not interpret
# embedded escape sequences, it shows the literal ESC byte as
# `^[` (its usual "unprintable control character" rendering).
# The only way to color text in `BUFFER`/`POSTDISPLAY` is zle's
# own `region_highlight` array, whose offsets are measured across
# `$BUFFER` followed directly by `$POSTDISPLAY` (see zshzle(1)).
# That's why the box paint below builds `region_highlight` entries
# instead of splicing SGR codes into the string.
#
# Recognized inputs (case-insensitive):
#   * The 8 standard ANSI names: black / red / green / yellow /
#     blue / magenta / cyan / white
#   * The 8 bright variants: gray (alias for grey), darkgray,
#     lightred, lightgreen, lightyellow, lightblue, lightmagenta,
#     lightcyan
#   * `#rrggbb` and `0xrrggbb` hex (truecolor, `fg=#rrggbb`)
#   * `reset` → empty (no highlight spec needed; region_highlight
#     entries are scoped by start/end, not by an explicit reset)
# Anything else (a typo, an unknown CSS name, a malformed hex)
# returns the empty string so the caller falls through to its
# default — a fail-soft policy that matches the rest of init.zsh
# (a bad palette must not break the dropdown).
#
# Truecolor output is `fg=#rrggbb`, zle's truecolor extension to
# the `region_highlight` / `zle_highlight` spec syntax (zsh
# 5.8+), supported by every modern terminal (kitty, alacritty,
# wezterm, iTerm2, gnome-terminal, …). 256-color fallback is
# intentionally not emitted — a terminal that can read `#rrggbb`
# from the user via the config file can also read it back as
# truecolor.
_smarthistory_color_to_hlspec() {
    local raw=${1:l}  # lowercase, mirrors the Rust resolve_color behavior
    # Strip leading whitespace.
    raw=${raw## }
    raw=${raw%% }
    case "$raw" in
        # Standard ANSI foreground indices (0-7), matching SGR 30-37.
        black)   print -n -- "fg=0" ;;
        red)     print -n -- "fg=1" ;;
        green)   print -n -- "fg=2" ;;
        yellow)  print -n -- "fg=3" ;;
        blue)    print -n -- "fg=4" ;;
        magenta) print -n -- "fg=5" ;;
        cyan)    print -n -- "fg=6" ;;
        white)   print -n -- "fg=7" ;;
        # Bright variants (indices 8-15, matching SGR 90-97;
        # `gray` is the alias for the default "no bold" gray,
        # `darkgray` is the explicit version — same index, kept
        # as separate names so a user reading the source can map
        # `tuicolor.dim=gray` to the palette index without
        # surprise).
        gray|darkgray|darkgrey) print -n -- "fg=8" ;;
        lightred)     print -n -- "fg=9" ;;
        lightgreen)   print -n -- "fg=10" ;;
        lightyellow)  print -n -- "fg=11" ;;
        lightblue)    print -n -- "fg=12" ;;
        lightmagenta) print -n -- "fg=13" ;;
        lightcyan)    print -n -- "fg=14" ;;
        # `reset` needs no spec of its own under region_highlight
        # (kept here for symmetry with the Rust `color_to_css`
        # round-trip — not currently emitted by the widget).
        reset) print -n -- "" ;;
        # Truecolor hex: `#rrggbb` and `0xrrggbb`. The two forms
        # differ by 2 chars (`#` vs `0x`), so a fixed offset is
        # cleaner than re-globbing.
        '#'*|0x*)
            local hex
            if [[ "$raw" == '#'* ]]; then
                hex=${raw[2,-1]}
            else
                hex=${raw[3,-1]}
            fi
            # 6-char hex exactly — anything else is malformed
            # and we fall through to the empty string.
            if [[ "$hex" == [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f] ]]; then
                print -n -- "fg=#${hex}"
            else
                # Malformed hex — fail soft.
                print -n -- ""
            fi
            ;;
        # Unknown name — fail soft.
        *) print -n -- "" ;;
    esac
}

# Strip ANSI CSI / SGR escape sequences from a string and return the
# visible-character length (i.e. the number of columns the string
# actually occupies on screen). Used by the width math in
# `_smarthistory_dropdown_paint` so embedded SGR codes (once the CLI
# starts emitting them — see the `search --ansi=full` flag) don't
# desync the box's `─` border, the row right-pad, and the
# `interior_max` truncation. The implementation recognises the common
# SGR form (`ESC [ … m`) and skips the parameter + final byte; other
# CSI families (cursor movement, erase-in-line, …) are ignored too
# because we only ever embed SGR codes — but the parser is forgiving
# in case the CLI ever emits a stray `\x1b[K` or similar.
#
# Uses zsh's `EXTENDED_GLOB` (enabled per-call via `setopt
# LOCAL_OPTIONS`) because the bare-glob form of `${var//PAT/REPL}`
# treats `[0-9;?]` as a glob character class, then chokes on the
# unquoted `?` (or — worse — silently matches too much) once you
# add a `*` or `#` after it. Under EXTENDED_GLOB the `#` is the
# "zero-or-more of preceding" quantifier (the analog of regex
# `*`), so `[0-9;?]#[a-zA-Z]` means "one char from {digit, ;, ?},
# repeated zero or more times, then one alpha" — which is exactly
# what an SGR parameter string looks like (`1m`, `38;5;208m`, `0m`).
# `LOCAL_OPTIONS` confines the option to the function so a parent
# shell that has `NO_EXTENDED_GLOB` set isn't affected.
#
# The visible length is returned via the `REPLY` parameter (the
# standard zsh convention for "this function's output is in
# `REPLY`") rather than printed to stdout. The earlier
# implementation used `print -n -- $#stripped` which works fine in
# `local x=$(…)` contexts but is fragile: a single missed capture
# (a stray context, an interactive session with the function called
# outside a command substitution, a debug hook that intercepts
# stdout, etc.) leaks the digit string into the terminal as
# visible noise. The widget was suffering from exactly that bug —
# the user typed `g` and saw `row_visible=5` and `row='  [g]'`
# printed as text alongside the box. Using `REPLY` makes the
# function a pure parameter-mutator, so it can't possibly leak.
_smarthistory_strip_ansi() {
    setopt LOCAL_OPTIONS EXTENDED_GLOB
    local stripped=${1//$'\x1b'\[[0-9;?]#[a-zA-Z]/}
    REPLY=$#stripped
}

# Parse one `bat --color=always`-highlighted line into plain text plus
# `region_highlight`-shaped color spans, for the optional
# `dropdown.highlight` feature. `bat` wraps each token as
# `ESC[38;2;R;G;Bm<text>ESC[0m` (24-bit truecolor foreground, no
# nesting observed in practice) — this walks the string, opening a
# span on a `38;2;R;G;B` code and closing it on the next escape of any
# kind (a `0` reset in the common case; any other SGR code the user's
# `bat` theme might emit, e.g. bold, closes it too rather than being
# tracked — a deliberate v1 simplification: worst case a token loses
# its color, nothing corrupts).
#
# `$1` is the ANSI-colored line. `$2` is the NAME of an array
# variable in the caller's scope to fill with `"start end fg=#rrggbb"`
# spans (0-based character offsets into the PLAIN text, matching
# `region_highlight`'s own convention and `_smarthistory_dropdown_paint`'s
# `_hl` array) — same indirect-array-by-name convention
# `_smarthistory_register_hook` uses elsewhere in this file. The
# plain (ANSI-stripped) text is returned via `REPLY`, standard
# convention for this file — and must come out byte-identical to what
# `_smarthistory_strip_ansi` would produce from the same input, since
# that plain text is what actually gets drawn.
_smarthistory_ansi_to_spans() {
    local remaining=$1
    local out_array_name=$2
    local out="" span_color="" code pre rest
    local -i span_open=0 span_start=0
    local -a spans=() rgb
    while [[ -n "$remaining" ]]; do
        if [[ "$remaining" == *$'\x1b'* ]]; then
            # Everything up to (not including) the next ESC byte is
            # plain text — the same anchored-glob-strip style
            # `_smarthistory_strip_ansi` above already uses for this
            # exact family of pattern, rather than `=~`/backreference
            # matching (POSIX ERE's handling of a backslash before a
            # metacharacter like `[` is unspecified — that's what
            # broke an earlier regex-based version of this function).
            pre="${remaining%%$'\x1b'*}"
            out+="$pre"
            rest="${remaining[$#pre+1,-1]}"  # from the ESC byte onward
            if [[ "$rest" == $'\x1b'\[*m* ]]; then
                rest="${rest#$'\x1b'\[}"
                code="${rest%%m*}"
                remaining="${rest#${code}m}"
                if [[ "$code" == 38\;2\;[0-9]*\;[0-9]*\;[0-9]* ]]; then
                    rgb=(${(s:;:)code})
                    if (( span_open )); then
                        (( $#out > span_start )) && spans+=("$span_start $#out $span_color")
                    fi
                    span_color=$(printf 'fg=#%02x%02x%02x' "$rgb[3]" "$rgb[4]" "$rgb[5]")
                    span_start=$#out
                    span_open=1
                else
                    if (( span_open )); then
                        (( $#out > span_start )) && spans+=("$span_start $#out $span_color")
                        span_open=0
                    fi
                fi
            else
                # A lone ESC not followed by a well-formed `...m`
                # SGR sequence — shouldn't happen with `bat`'s
                # output, but treat the byte as plain text rather
                # than looping forever on it.
                out+=$'\x1b'
                remaining="${rest[2,-1]}"
            fi
        else
            out+="$remaining"
            remaining=""
        fi
    done
    if (( span_open )); then
        (( $#out > span_start )) && spans+=("$span_start $#out $span_color")
    fi
    REPLY="$out"
    eval "${out_array_name}=(\"\${spans[@]}\")"
}

# Resolve a candidate's first word to `_smarthistory_dropdown_hl_success`
# (an alias, function, builtin, a command on `$PATH`, or a common zsh
# reserved word — `if`/`for`/`while`/`case`/`{`/`sudo`/… — that a
# lexical-only highlighter like `bat` has no way to validate) or
# `_smarthistory_dropdown_hl_error` (nothing resolves — most likely a
# typo). Same lookups `zsh-patina`'s own (private,
# `_zsh_patina_resolve_callable`) validity check uses, plus the
# reserved-word allowance it doesn't bother with. Result via `REPLY`.
_smarthistory_command_validity_hlspec() {
    local word=$1
    case "$word" in
        if|then|else|elif|fi|for|while|until|do|done|case|esac|select|\
        function|time|coproc|repeat|'{'|'}'|'('|')'|sudo|env|command|exec|\
        builtin|noglob|nocorrect)
            REPLY=$_smarthistory_dropdown_hl_success
            return
            ;;
    esac
    if (( ${+aliases[(e)$word]} )) || (( ${+galiases[(e)$word]} )) \
        || (( ${+functions[(e)$word]} )) || (( ${+builtins[(e)$word]} )) \
        || (( ${+commands[(e)$word]} )) || whence -p -- "$word" >/dev/null 2>&1; then
        REPLY=$_smarthistory_dropdown_hl_success
    else
        REPLY=$_smarthistory_dropdown_hl_error
    fi
}

# ---- Resolved TUI palette (one-shot at init) ----
#
# Read the same `tuicolor.*` block the TUI uses to draw its chrome
# — with the active theme's overrides applied — so the dropdown
# box borders and selection gutter match the user's actual TUI
# theme (e.g. a `theme.dark=gruvbox` user sees orange borders,
# not the hardcoded defaults). One `smarthistory config get
# palette` call per shell startup; the user can `exec zsh` (or
# restart the terminal) to pick up theme changes from the TUI
# after a `Ctrl-N` theme cycle, same as every other cached
# setting in this file. Per-keystroke palette re-reads would add
# a subprocess to every typed character when the dropdown is
# visible — not worth it.
#
# The values land in two `region_highlight` spec strings:
#   _smarthistory_dropdown_hl_accent   — box corners, top/bottom border, unselected-row gutter
#   _smarthistory_dropdown_hl_select   — selected-row gutter
# (no trailing "reset" string is needed — region_highlight entries
# are scoped by explicit start/end offsets, so there's nothing to
# bleed into the next region the way a raw SGR code can.)
# Defaults (accent=cyan, selection=blue) match the TUI's
# built-in palette, so a first-run install (no config file) gets
# a colored dropdown out of the box. Any unparseable value from
# the CLI is silently replaced with the default — same
# fail-soft policy the rest of this file uses.
#
# This block runs at init.zsh *source* time (after the
# dropdown-enabled gating block above and after both helpers,
# `_smarthistory_color_to_hlspec` and `_smarthistory_strip_ansi`,
# are defined). When the dropdown is disabled, none of this
# matters — the specs are set but never consulted by any paint
# call (the dropdown widget is registered in the `if` block
# above).
typeset -g _smarthistory_dropdown_hl_accent="fg=6"  # cyan fallback
typeset -g _smarthistory_dropdown_hl_select="fg=4"  # blue fallback
# Used by the optional `dropdown.highlight` syntax-highlighting
# feature (see `_smarthistory_command_validity_hlspec` below) to mark
# a candidate's first word as a resolvable command (green) or not
# (red) — same green/red convention as `tuicolor.success`/
# `tuicolor.error` elsewhere in the app.
typeset -g _smarthistory_dropdown_hl_success="fg=2"  # green fallback
typeset -g _smarthistory_dropdown_hl_error="fg=1"    # red fallback
# `bat --theme` argument for the `dropdown.highlight` feature —
# "dark" or "light", matching `bg`'s perceived brightness the exact
# way `highlight_with_bat`'s `bat_theme_arg()` does (Rust side, used
# for the `$`/`&`/`,` preview panes and `smart-open.default=bat`), so
# the dropdown's syntax colors read correctly against the same
# background the rest of the app already assumes. Defaults to "dark"
# — same "matches the TUI's default active scheme" convention as
# `smarthistory config get palette`'s own scheme default.
typeset -g _smarthistory_dropdown_bat_theme="dark"

# Detect whether the terminal advertises color support at all.
# `region_highlight` entries are converted to real terminal escape
# sequences by zle's own redisplay engine (not by us), so this is
# no longer a defense against raw ESC bytes leaking out as literal
# text — that failure mode is exactly what moving to
# `region_highlight` (see `_smarthistory_color_to_hlspec` above)
# fixes. This check just avoids asking zle to color a genuinely
# colorless terminal, so the widget falls back to plain Unicode
# box characters there.
#
# Detection is best-effort: we check the standard curses/terminfo
# `colors` capability (a small number = no color) AND look for
# the `RGB` terminfo capability (which 24-bit-color terminals
# advertise). The `COLORTERM=truecolor` env var is a strong
# positive signal too. Any one of these being true is enough to
# trust that the terminal can render color; otherwise we disable
# coloring entirely.
#
# `tput` is a loadable zsh module via `zmodload zsh/terminfo`,
# but the binary isn't guaranteed to be on $PATH inside a
# non-interactive `smarthistory init zsh` source — so the
# checks are wrapped in `(( $+commands[tput] ))` guards and
# fall through to the conservative answer (no color) when the
# binary is missing.
typeset -g _smarthistory_dropdown_color_ok=0
if [[ -n "$COLORTERM" && "$COLORTERM" == *"color"* ]]; then
    _smarthistory_dropdown_color_ok=1
elif (( $+commands[tput] )); then
    # `tput colors` returns the number of colors the terminal
    # advertises. Anything >= 8 means basic ANSI color works.
    # For 24-bit color we additionally check `tput RGB`, which
    # returns the RGB strings if supported.
    local _tput_colors
    _tput_colors=$(tput colors 2>/dev/null)
    if [[ -n "$_tput_colors" && "$_tput_colors" -ge 8 ]]; then
        # `tput RGB` returns a non-empty string iff the
        # terminal supports 24-bit color via the standard
        # SGR form. False negatives are possible on
        # terminals that support truecolor but don't expose
        # it through terminfo; the COLORTERM check above
        # catches those.
        if tput RGB 2>/dev/null | grep -q .; then
            _smarthistory_dropdown_color_ok=1
        else
            # 8/16-color terminal without explicit truecolor
            # support. Still safe to ask zle for basic ANSI
            # colors — accept it.
            _smarthistory_dropdown_color_ok=1
        fi
    fi
fi
# If color isn't OK, blank out the highlight specs so the paint
# function appends no `region_highlight` entries at all (the plain
# Unicode box chars are still drawn, just without color).
if (( _smarthistory_dropdown_color_ok == 0 )); then
    _smarthistory_dropdown_hl_accent=""
    _smarthistory_dropdown_hl_select=""
    _smarthistory_dropdown_hl_success=""
    _smarthistory_dropdown_hl_error=""
fi
if [[ "$_smarthistory_dropdown_enabled" = "1" ]]; then
    # Disable `xtrace` for the palette-init block too — see
    # `_smarthistory_dropdown_paint` for the rationale. The
    # `smarthistory config get palette` subprocess call, the
    # `_smarthistory_dropdown_palette_raw` capture, and the
    # loop body below would otherwise dump a wall of trace
    # noise into the user's terminal at shell startup. Same
    # save/restore dance as the paint function — restoring on
    # the way out so a user with `set -x` enabled in their
    # .zshrc keeps it enabled after init.
    local _sm_dropdown_xtrace_was=$options[xtrace]
    setopt NO_XTRACE
    _smarthistory_dropdown_palette_raw=$(smarthistory config get palette 2>/dev/null)
    # The CLI emits one `key=value` line per `tuicolor.*` slot. A
    # failure (binary missing, corrupt config, permission denied)
    # leaves `_palette_raw` empty; we then skip the loop and the
    # defaults above stand. No `setopt ERR_EXIT` here — a bad
    # palette read must not break the rest of init.zsh.
    if [[ -n "$_smarthistory_dropdown_palette_raw" ]]; then
        local _smarthistory_dropdown_palette_key _smarthistory_dropdown_palette_value
        while IFS='=' read -r _smarthistory_dropdown_palette_key _smarthistory_dropdown_palette_value; do
            # Strip stray whitespace from each side; the CLI
            # doesn't emit spaces, but a hand-edited session file
            # or a future formatting change could.
            _smarthistory_dropdown_palette_key=${_smarthistory_dropdown_palette_key// /}
            _smarthistory_dropdown_palette_value=${_smarthistory_dropdown_palette_value## }
            _smarthistory_dropdown_palette_value=${_smarthistory_dropdown_palette_value%% }
            case "$_smarthistory_dropdown_palette_key" in
                accent)
                    local _hlspec=$(_smarthistory_color_to_hlspec "$_smarthistory_dropdown_palette_value")
                    [[ -n "$_hlspec" ]] && _smarthistory_dropdown_hl_accent=$_hlspec
                    unset _hlspec
                    ;;
                selection)
                    local _hlspec=$(_smarthistory_color_to_hlspec "$_smarthistory_dropdown_palette_value")
                    [[ -n "$_hlspec" ]] && _smarthistory_dropdown_hl_select=$_hlspec
                    unset _hlspec
                    ;;
                success)
                    local _hlspec=$(_smarthistory_color_to_hlspec "$_smarthistory_dropdown_palette_value")
                    [[ -n "$_hlspec" ]] && _smarthistory_dropdown_hl_success=$_hlspec
                    unset _hlspec
                    ;;
                error)
                    local _hlspec=$(_smarthistory_color_to_hlspec "$_smarthistory_dropdown_palette_value")
                    [[ -n "$_hlspec" ]] && _smarthistory_dropdown_hl_error=$_hlspec
                    unset _hlspec
                    ;;
                bg)
                    # `resolved_palette` emits `bg` as `#rrggbb` when
                    # a built-in theme is active, but falls back to
                    # the hardcoded `Palette::builtin()` default
                    # (currently the named color `black`) when no
                    # theme is selected — handle both forms, same as
                    # `_smarthistory_color_to_hlspec` does for the
                    # other palette slots. Named form is checked
                    # against the exact same "light" name list
                    # `is_color_light` uses on the Rust side (White,
                    # the six Light* colors, and Gray); hex form uses
                    # the same ITU-R BT.601 perceived-brightness
                    # formula, light when > 127.
                    case "${_smarthistory_dropdown_palette_value:l}" in
                        '#'[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F])
                            local _sm_bg_hex=${_smarthistory_dropdown_palette_value#'#'}
                            local -i _sm_bg_r=16#${_sm_bg_hex[1,2]}
                            local -i _sm_bg_g=16#${_sm_bg_hex[3,4]}
                            local -i _sm_bg_b=16#${_sm_bg_hex[5,6]}
                            local -i _sm_bg_brightness=$(( (_sm_bg_r * 299 + _sm_bg_g * 587 + _sm_bg_b * 114) / 1000 ))
                            (( _sm_bg_brightness > 127 )) && _smarthistory_dropdown_bat_theme="light" || _smarthistory_dropdown_bat_theme="dark"
                            unset _sm_bg_hex _sm_bg_r _sm_bg_g _sm_bg_b _sm_bg_brightness
                            ;;
                        white|lightred|lightgreen|lightyellow|lightblue|lightmagenta|lightcyan|gray|grey)
                            _smarthistory_dropdown_bat_theme="light"
                            ;;
                        *)
                            _smarthistory_dropdown_bat_theme="dark"
                            ;;
                    esac
                    ;;
                # All other slots (fg, warning, …) are currently
                # unused by the dropdown widget. Reading them anyway
                # keeps the call site identical to the TUI's palette
                # resolution; a future widget addition just needs a
                # new case arm.
                *) ;;
            esac
        done <<< "$_smarthistory_dropdown_palette_raw"
        unset _smarthistory_dropdown_palette_key _smarthistory_dropdown_palette_value
    fi
    unset _smarthistory_dropdown_palette_raw
    [[ "$_sm_dropdown_xtrace_was" == "on" ]] && setopt XTRACE
    unset _sm_dropdown_xtrace_was
fi

# Redraw POSTDISPLAY from the current `_smarthistory_dropdown_candidates`
# / `_smarthistory_dropdown_selected` state, WITHOUT re-querying the DB
# (used by Up/Down to move the selection, and after any state change
# that doesn't change the candidate set).
_smarthistory_dropdown_paint() {
    # Disable `xtrace` for the duration of the paint call. The
    # widget's per-row `local row_visible=$(…)` and similar
    # assignments would otherwise be traced as
    # `+func:line> local row_visible=N` (and the
    # `local -a _hl _parts` style too) every time the user typed — a wall of debug
    # noise that's especially bad inside a live zle widget
    # where the trace bytes overwrite POSTDISPLAY and break
    # the box layout entirely. The user almost certainly has
    # `set -x` enabled in their interactive session for
    # unrelated debugging; the widget has to coexist with
    # that. We unconditionally turn the option off and
    # restore the previous value on return — `LOCAL_OPTIONS`
    # alone wouldn't help here because that option only
    # scopes *changes* made inside the function, not the
    # *inherited* xtrace state.
    local _sm_dropdown_xtrace_was=$options[xtrace]
    setopt NO_XTRACE
    local -a rows
    local raw c marker row i=0
    # Leave margin for the box border (`│ ` / ` │` on each side, 4
    # columns) plus a little breathing room, so a candidate can't
    # wrap onto a second physical row (which would desync "one
    # candidate = one POSTDISPLAY row").
    local interior_max=$(( COLUMNS > 12 ? COLUMNS - 8 : 4 ))
    # Clamp to available terminal rows so a long candidate list can't
    # push content off the bottom of the screen. The extra -2 (on top
    # of the existing headroom) accounts for the box's own top/bottom
    # border rows.
    local max_rows=$(( LINES > 8 ? LINES - 6 : 2 ))
    # Reserve room for the right-aligned "last called" age column
    # (`_smarthistory_dropdown_meta`, e.g. "5m"/"2h"/"3d") BEFORE
    # truncating any command text — the age must never be pushed off
    # the box, the command truncates first. Computed here, ahead of
    # the candidate loop below, because `age_width` doesn't depend on
    # (possibly truncated) command text at all, so it can shrink the
    # command truncation budget (`interior_cmd_max`) for the very
    # same pass that builds `rows[]`. `age_width` stays 0 — and
    # `interior_cmd_max` collapses to plain `interior_max`, i.e.
    # today's exact behavior, no age column drawn at all — whenever
    # every candidate's age came back empty (the render function's
    # defensive fallback for a line it couldn't parse).
    local age_width=0 age_visible age_val
    local -i _sm_age_i=0
    for age_val in "${_smarthistory_dropdown_meta[@]}"; do
        (( _sm_age_i >= max_rows )) && break
        _smarthistory_strip_ansi "$age_val"
        age_visible=$REPLY
        (( age_visible > age_width )) && age_width=$age_visible
        _sm_age_i=$((_sm_age_i+1))
    done
    local -r age_gap=2
    local interior_cmd_max=$interior_max
    if (( age_width > 0 )); then
        interior_cmd_max=$(( interior_max - age_gap - age_width ))
        (( interior_cmd_max < 4 )) && interior_cmd_max=4
    fi
    for raw in "${_smarthistory_dropdown_candidates[@]}"; do
        (( i >= max_rows )) && break
        c=$(_smarthistory_unescape "$raw")
        # A multiline command would otherwise break the one-row-per-
        # candidate layout; show the visible-newline marker instead,
        # same convention the Rust TUI list uses for the same reason.
        c=${c//$'\n'/↵}
        c=${c//$'\r'/}
        if (( _smarthistory_dropdown_chosen == 1 && i == _smarthistory_dropdown_selected )); then
            marker="❯ "
        else
            marker="  "
        fi
        row="${marker}${c}"
        # `smarthistory search` is called with `--ansi=off` (see the
        # args comment in `_smarthistory_dropdown_render`), so `row`
        # never actually carries embedded SGR codes today — the
        # strip is kept anyway as a defensive no-op so the width
        # math can't silently desync the box border if that ever
        # changes.
        _smarthistory_strip_ansi "$row"
        local row_visible=$REPLY
        if (( row_visible > interior_cmd_max )); then
            row="${row[1,$((interior_cmd_max-1))]}…"
        fi
        rows+=("$row")
        i=$((i+1))
    done
    if (( ${#rows} == 0 )); then
        POSTDISPLAY=""
        _smarthistory_dropdown_hl_prune
        return
    fi
    # Box width = the widest row actually produced this render (not
    # the full interior_max budget) — every row pads to this exact
    # width so the right border lines up, and the box visibly shrinks
    # when the candidate set does, same as the un-boxed layout did.
    #
    # Width is measured on the SGR-stripped row (a defensive no-op
    # today — `--ansi=off` means `row` never actually carries escape
    # codes, see the comment on the per-row `_smarthistory_strip_ansi`
    # call above) so the `─` border can't be desynced if that ever
    # changes.
    # `width` is computed as the maximum visible row width; the
    # display loop below reads it once per row. We deliberately
    # re-use the existing `row_visible` parameter (declared in
    # the per-row loop above) instead of re-declaring it via
    # `local width=0 row_visible` — re-declaring an existing
    # local parameter in zsh emits a `varname=oldvalue` line
    # to stdout as a side effect of the declaration itself,
    # which leaks `row_visible=N` debug text into the user's
    # terminal on every keystroke. Same reason we don't use a
    # separate `local` for the second-loop variable below.
    local cmd_width=0
    for row in "${rows[@]}"; do
        _smarthistory_strip_ansi "$row"
        row_visible=$REPLY
        (( row_visible > cmd_width )) && cmd_width=$row_visible
    done
    # Total interior width = the command column plus (when there IS
    # an age column to draw) a fixed gap and the age column itself —
    # everything below this point (`hr`, the borders, the per-row
    # pad) reads `width` exactly like before the age column existed.
    local width=$(( age_width > 0 ? cmd_width + age_gap + age_width : cmd_width ))
    # `${(l:width::─:)}` pads an empty string to `width` columns using
    # `─` as the fill character — i.e. `width` dashes, built by zsh's
    # own padding expansion rather than a manual loop.
    local hr="${(l:width::─:)}"
    # Build `out` as PLAIN TEXT — no embedded escape codes. POSTDISPLAY
    # is inert buffer text as far as zle's redisplay engine is
    # concerned; it does not interpret escape sequences spliced into
    # it, it shows the literal ESC byte as `^[` the same way it would
    # show any other unprintable control character typed at the
    # prompt. Splicing `\x1b[38;2;…m` into POSTDISPLAY (the previous
    # implementation) is exactly why colors showed up as literal
    # `^[[38;2;…m` text instead of an actual accent color.
    #
    # Coloring is applied afterwards via zle's `region_highlight`
    # array, whose offsets are measured across `$BUFFER` followed
    # directly by `$POSTDISPLAY` (zshzle(1)). `_hl` collects one
    # "start end spec" triple per colored span with offsets relative
    # to the start of `out` — they get shifted by `$#BUFFER` in the
    # splice step below, once `out` is finished and about to become
    # POSTDISPLAY.
    #
    # Layout (spec is the empty string, and so no `_hl` entry is
    # recorded, when the terminal color-capability check failed —
    # see `_smarthistory_dropdown_color_ok` above):
    #   top border         → `_smarthistory_dropdown_hl_accent`
    #   bottom border      → `_smarthistory_dropdown_hl_accent`
    #   unselected gutter   → `_smarthistory_dropdown_hl_accent`
    #   selected gutter     → `_smarthistory_dropdown_hl_select`
    #   right-side border   → always `_smarthistory_dropdown_hl_accent`,
    #                          regardless of row selection — only the
    #                          left gutter's glyph (`┃` vs `│`) and
    #                          color signal the selected row.
    #   selected row's text → `bold` (unconditional — bold isn't a
    #                          color, so it isn't gated by
    #                          `_smarthistory_dropdown_color_ok`),
    #                          covering the age column too when one
    #                          is drawn.
    #   age column (unselected rows) → `_smarthistory_dropdown_hl_accent`
    #                          — same spec as the borders, so no new
    #                          palette variable or config key for it.
    #
    # All the loop-driver / scratch locals below (`_hl_start`,
    # `_entry`, `_parts`, `gutter_spec`, `gutter_text`) are declared
    # once, here, before any loop, and only ever plain-assigned
    # inside one — a bare `local` re-declaration inside a loop body
    # makes zsh print the existing binding (`varname=value`) to
    # stdout on every iteration, which would leak into the user's
    # terminal (same fix already applied to `row_visible` / `row` /
    # `side` above).
    local -a _hl _parts _sm_row_hl_entries
    local _sm_word _sm_hl_entry _sm_hl_parts hl_base
    local _hl_start _entry gutter_spec gutter_text
    local row_start bold_start bold_end row_end
    local age_text age_field age_start is_selected
    # `--prefix` search (see the args comment above) guarantees every
    # candidate's command text starts with the exact bytes of
    # `$LBUFFER` — that's the span this widget bolds to recreate the
    # "matched prefix" emphasis `--ansi=full` used to (unreliably)
    # provide. `marker_len` is the fixed width of the `"❯ "` /
    # `"  "` marker prepended to every row in the candidate-building
    # loop above — the bold span starts right after it.
    local matchlen=$#LBUFFER
    local -r marker_len=2
    local out=$'\n'
    if [[ -n "$_smarthistory_dropdown_hl_accent" ]]; then
        _hl_start=$#out
        out+="╭─${hr}─╮"
        _hl+=("$_hl_start $#out $_smarthistory_dropdown_hl_accent")
    else
        out+="╭─${hr}─╮"
    fi
    # The per-row pad must be computed against the *visible* width,
    # not the byte length. `row` doesn't carry embedded SGR codes
    # today (`--ansi=off`), but the pad is still built from
    # `row_visible` rather than `${#row}` as a defensive measure —
    # see the per-row `_smarthistory_strip_ansi` comment above. Build
    # the pad by hand: `width - row_visible` spaces appended to the
    # raw row (the matched-prefix / selected-row emphasis is applied
    # afterwards via `region_highlight`, not embedded in `row`
    # itself, so it can never desync this math).
    local side pad
    for (( i = 0; i < ${#rows}; i++ )); do
        (( is_selected = _smarthistory_dropdown_chosen == 1 && i == _smarthistory_dropdown_selected ))
        if (( is_selected )); then
            side="┃"
            gutter_text="┃ "
            gutter_spec=$_smarthistory_dropdown_hl_select
        else
            side="│"
            gutter_text="│ "
            gutter_spec=$_smarthistory_dropdown_hl_accent
        fi
        row=${rows[$((i+1))]}
        _smarthistory_strip_ansi "$row"
        row_visible=$REPLY
        # Precompute this row's `dropdown.highlight` token-color spans
        # (if any). Guard: `row` minus its marker must equal the RAW
        # candidate text the spans were computed against, byte for
        # byte — a single check that covers both ways the drawn text
        # can diverge from that raw text: `_smarthistory_unescape`
        # (multi-line commands) and truncation (the "…" ellipsis).
        # Either one invalidates the spans' offsets, so highlighting
        # is skipped for this row rather than risk drawing colors
        # against text they don't describe.
        _sm_row_hl_entries=()
        if [[ "$_smarthistory_dropdown_highlight_enabled" = "1" \
            && "${row[$((marker_len+1)),-1]}" == "${_smarthistory_dropdown_candidates[$((i+1))]}" ]]; then
            [[ -n "${_smarthistory_dropdown_hl_spans[$((i+1))]:-}" ]] \
                && _sm_row_hl_entries=("${(f)_smarthistory_dropdown_hl_spans[$((i+1))]}")
            # First-word command-validity span, appended LAST so it
            # takes priority over `bat`'s purely lexical color for
            # that same range (region_highlight applies same-attribute
            # overlaps in array order — later wins).
            _sm_word=${_smarthistory_dropdown_candidates[$((i+1))]%%[[:space:]]*}
            if [[ -n "$_sm_word" ]]; then
                _smarthistory_command_validity_hlspec "$_sm_word"
                [[ -n "$REPLY" ]] && _sm_row_hl_entries+=("0 $#_sm_word $REPLY")
            fi
        fi
        # Right-justify this row's age into a fixed `age_width`
        # column (e.g. "5m" and "10m" both occupy the same 3
        # columns) — empty when no age column is being drawn at all
        # this render (`age_width == 0`).
        age_text=${_smarthistory_dropdown_meta[$((i+1))]}
        if (( age_width > 0 )); then
            age_field="${(l:age_width:: :)age_text}"
        else
            age_field=""
        fi
        # Pad fills up to `cmd_width` ONLY (so every row's command
        # column left-aligns to the same width) — NOT the overall
        # `width`, which also includes the age gap/column. The fixed
        # `age_gap` separator is emitted unconditionally right before
        # the age field below, regardless of how much (if any) pad
        # this particular row needed; folding the gap into `pad`
        # (an earlier version of this code did) meant the gap
        # silently vanished for the widest row (whichever row's
        # `pad` computed to 0), leaving its age column one gap short
        # of every other row's.
        pad=$(( cmd_width - row_visible ))
        # `(l:0:: :)` is the empty-pad guard — `${(l:-1:: :)}` is a
        # zsh error. Clamp to 0 so the negative case (the row was
        # already truncated to `interior_cmd_max - 1` cols) is harmless.
        (( pad < 0 )) && pad=0
        out+=$'\n'
        if [[ -n "$gutter_spec" ]]; then
            _hl_start=$#out
            out+="$gutter_text"
            _hl+=("$_hl_start $#out $gutter_spec")
        else
            out+="$gutter_text"
        fi
        if (( is_selected )); then
            # Bold span opens here; closes AFTER the age column below
            # so the whole selected row — command AND age — reads as
            # highlighted, not just the command part.
            _hl_start=$#out
            row_start=$#out
            out+="$row"
        else
            row_start=$#out
            out+="$row"
            if (( matchlen > 0 )); then
                row_end=$#out
                bold_start=$(( row_start + marker_len ))
                bold_end=$(( bold_start + matchlen ))
                (( bold_end > row_end )) && bold_end=$row_end
                (( bold_start < bold_end )) && _hl+=("$bold_start $bold_end bold")
            fi
        fi
        # Splice in this row's `dropdown.highlight` spans (token
        # colors + the first-word validity span, see above) — offset
        # past the marker, same for selected and unselected rows.
        # Independent `region_highlight` attribute from the `bold`
        # spans above (color vs. weight), so they combine rather than
        # conflict, on both the matched-prefix and the selected row.
        if (( ${#_sm_row_hl_entries} > 0 )); then
            hl_base=$(( row_start + marker_len ))
            for _sm_hl_entry in "${_sm_row_hl_entries[@]}"; do
                _sm_hl_parts=(${=_sm_hl_entry})
                _hl+=("$(( hl_base + _sm_hl_parts[1] )) $(( hl_base + _sm_hl_parts[2] )) ${_sm_hl_parts[3]}")
            done
        fi
        (( pad > 0 )) && out+="${(l:pad:: :)}"
        if (( age_width > 0 )); then
            # The `age_gap`-wide separator is fixed and unconditional
            # (unlike `pad` above, which can be 0) — it's what
            # actually reserves the gap `width` accounts for.
            out+="${(l:age_gap:: :)}"
            if (( is_selected )); then
                out+="$age_field"
            else
                age_start=$#out
                out+="$age_field"
                [[ -n "$_smarthistory_dropdown_hl_accent" ]] && _hl+=("$age_start $#out $_smarthistory_dropdown_hl_accent")
            fi
        fi
        # Close the selected-row bold span (opened above, right
        # before the command text) now that the age column — if any
        # — has been appended too. A no-op duplicate-avoidance: this
        # is the ONLY bold span selected rows get, unlike unselected
        # rows' separate matched-prefix span above.
        (( is_selected )) && _hl+=("$_hl_start $#out bold")
        # The right border is `" ${side}"` (space THEN the bar) to
        # mirror the left gutter's `"${gutter_text}"` (bar THEN
        # space) — both sides need exactly one padding column between
        # the border and the content for the box to be exactly
        # `width + 4` wide (2 border chars + 1 padding column each
        # side), matching the top/bottom border's `╭─<hr>─╮` math.
        # The previous `"${side} "` (bar THEN space, with nothing
        # padding the left side of the bar) put the padding column
        # *outside* the box instead of inside it, making every
        # content row exactly one column narrower than the border —
        # invisible on a live terminal (trailing spaces still occupy
        # a cell) but visible as soon as trailing whitespace is
        # trimmed (e.g. copy-pasting the box out of the terminal).
        #
        # The right-side border always carries the accent spec
        # (never the selection spec) — matches the pre-region_highlight
        # behavior, where only the left gutter differed by selection.
        if [[ -n "$_smarthistory_dropdown_hl_accent" ]]; then
            _hl_start=$#out
            out+=" ${side}"
            _hl+=("$_hl_start $#out $_smarthistory_dropdown_hl_accent")
        else
            out+=" ${side}"
        fi
    done
    out+=$'\n'
    if [[ -n "$_smarthistory_dropdown_hl_accent" ]]; then
        _hl_start=$#out
        out+="╰─${hr}─╯"
        _hl+=("$_hl_start $#out $_smarthistory_dropdown_hl_accent")
    else
        out+="╰─${hr}─╯"
    fi
    # Splice the collected spans into `region_highlight`, shifted by
    # `$#BUFFER` (POSTDISPLAY starts right after BUFFER — see
    # zshzle(1) on region_highlight offsets). Prune this widget's
    # own previous entries first so repainting doesn't accumulate
    # stale spans; entries belonging to $BUFFER itself (e.g. a
    # syntax-highlighting plugin's own region_highlight use) are
    # never touched — see `_smarthistory_dropdown_hl_prune`.
    _smarthistory_dropdown_hl_prune
    if (( ${#_hl} > 0 )); then
        local base=$#BUFFER
        for _entry in "${_hl[@]}"; do
            _parts=(${=_entry})
            region_highlight+=("$((base + _parts[1])) $((base + _parts[2])) ${_parts[3]}")
        done
    fi
    POSTDISPLAY="$out"
    # Restore the xtrace state the user's shell had when we
    # entered the function — see the comment block at the
    # top of the function for why we turn it off in the first
    # place. Without the restore, the widget would silently
    # *disable* xtrace for the rest of the user's session
    # after the first paint, which is a worse failure mode
    # than the original leak.
    [[ "$_sm_dropdown_xtrace_was" == "on" ]] && setopt XTRACE
    unset _sm_dropdown_xtrace_was
}

# Re-query smarthistory for the current LBUFFER and redraw. Called
# after every keystroke (via the wrapped self-insert/delete/paste
# widgets below) when the dropdown is enabled.
_smarthistory_dropdown_render() {
    # See `_smarthistory_dropdown_paint` for why we disable
    # `xtrace` here too — the render function calls paint, and
    # the assignments in this function (the `args` array, the
    # `raw` capture, the `_smarthistory_dropdown_candidates`
    # assignment) would otherwise be traced as debug noise on
    # every keystroke when the user has `set -x` enabled.
    # Same save/restore dance as the paint function.
    local _sm_dropdown_xtrace_was=$options[xtrace]
    setopt NO_XTRACE
    [[ "$_smarthistory_dropdown_enabled" = "1" ]] || return
    # Suppressed by an explicit Esc/Ctrl-K dismissal earlier in this
    # same command line — stay closed until `_smarthistory_precmd`
    # resets the flag for the next prompt. See
    # `_smarthistory_dropdown_suppressed`'s declaration above.
    [[ $_smarthistory_dropdown_suppressed -eq 1 ]] && return
    # POSTDISPLAY always renders after the buffer's current end, so
    # the menu only makes sense with the cursor there — same
    # constraint zsh-autosuggestions' ghost text has, for the same
    # reason.
    if [[ $CURSOR -ne $#BUFFER ]]; then
        _smarthistory_dropdown_clear
        return
    fi
    # Privacy convention: never suggest for a space-prefixed line
    # (see the precmd hook's HIST_NO_STORE handling above).
    if [[ "$LBUFFER" == [[:space:]]* ]]; then
        _smarthistory_dropdown_clear
        return
    fi
    if (( $#LBUFFER < _smarthistory_dropdown_minchars )); then
        _smarthistory_dropdown_clear
        return
    fi
    local -a args
    # `--prefix`: match commands that START WITH what's typed, not a
    # substring anywhere in the command — a plain substring match
    # made "ls" match `open "http://.../details"` (contains "ls"
    # inside the URL), which is surprising for a live as-you-type
    # completion (unlike Up/Down's keypress-triggered walk, which
    # keeps the broader substring match).
    #
    # `--ansi=off`: the widget always pipes the search call into a
    # `$()`-style capture, which is never a TTY — so `--ansi=full`
    # (and `--ansi=bold`) never actually produce styled SGR output
    # here, they fall back to bracket-wrapped text (`[ls]`) for
    # pipe-safety. Those literal `[`/`]` characters aren't part of
    # the history at all; they were the CLI's non-TTY highlight
    # fallback leaking into what the user sees. `--ansi=off` gives
    # plain, unmodified command text instead. The matched-prefix
    # emphasis `--ansi=full` was trying to provide over a real TTY
    # is recreated properly in `_smarthistory_dropdown_paint` via a
    # `region_highlight` `bold` span over the first `$#LBUFFER`
    # characters of each row — the same mechanism already used to
    # color the box chrome and bold the selected row.
    #
    # `--fields diff,command`: `diff` (the "last called" age column,
    # e.g. "5m"/"2h"/"3d") drawn by `_smarthistory_dropdown_paint`.
    # `diff` MUST come first: the CLI joins fields with exactly two
    # spaces and left-pads `diff` for column alignment, but never
    # inserts padding *between* cells — so the first run of 2+
    # spaces in a line is unambiguously the diff/command boundary,
    # even in the rare case the command itself contains a run of 2+
    # spaces later in the line (we only ever split on the FIRST
    # such run).
    args=("$LBUFFER" --limit "$_smarthistory_dropdown_limit" --fields diff,command --ansi=off --prefix)
    case "$_smarthistory_mode" in
        sess)   args+=(--session) ;;
        dir)    args+=(--directory "$PWD") ;;
        global) ;;
    esac
    local raw
    raw=$(smarthistory search "${args[@]}" 2>/dev/null)
    local -a _sm_dropdown_lines
    _sm_dropdown_lines=("${(f)raw}")
    # `${(f)raw}` on an empty string yields one empty element, not
    # zero — drop it so "no matches" is correctly detected below.
    if (( ${#_sm_dropdown_lines} == 1 )) && [[ -z "${_sm_dropdown_lines[1]}" ]]; then
        _sm_dropdown_lines=()
    fi
    # Split each "<diff>  <command>" line and dedup by COMMAND ONLY,
    # keeping the first (newest, since `search` returns newest-first)
    # occurrence — the same command run N times has N different ages,
    # so deduping on the whole line (the old `${(u)...}` one-liner)
    # would stop deduping once the age column was added, reintroducing
    # the duplicate-rows bug this widget already fixed once. The kept
    # occurrence's age is exactly the correct "last called" value for
    # that command. `_smarthistory_dropdown_candidates` ends up with
    # its original contract unchanged (bare command text only), so no
    # other function (commit/accept widgets, etc.) needs to change.
    _smarthistory_dropdown_candidates=()
    _smarthistory_dropdown_meta=()
    local -A _sm_dropdown_seen
    local _sm_dropdown_line _sm_dropdown_diff _sm_dropdown_cmd
    for _sm_dropdown_line in "${_sm_dropdown_lines[@]}"; do
        if [[ "$_sm_dropdown_line" =~ '^[[:space:]]*([^[:space:]]*)[[:space:]]{2,}(.*)$' ]]; then
            _sm_dropdown_diff=$match[1]
            _sm_dropdown_cmd=$match[2]
        else
            # Defensive fallback (e.g. a row with no diff/space run) —
            # treat the whole line as the command with no age, same
            # as this widget's behavior before the age column existed.
            _sm_dropdown_diff=""
            _sm_dropdown_cmd=$_sm_dropdown_line
        fi
        (( ${+_sm_dropdown_seen[$_sm_dropdown_cmd]} )) && continue
        _sm_dropdown_seen[$_sm_dropdown_cmd]=1
        _smarthistory_dropdown_candidates+=("$_sm_dropdown_cmd")
        _smarthistory_dropdown_meta+=("$_sm_dropdown_diff")
    done
    if (( ${#_smarthistory_dropdown_candidates} == 0 )); then
        _smarthistory_dropdown_clear
        return
    fi
    # Optional per-candidate syntax highlighting (`dropdown.highlight=on`):
    # ALL candidates go through a single `bat` call (one line per
    # candidate on stdin) rather than one call per row — keeps this
    # to one extra subprocess per keystroke, matching the cost of the
    # `smarthistory search` call above, instead of scaling with
    # `dropdown.limit`.
    _smarthistory_dropdown_hl_spans=()
    if [[ "$_smarthistory_dropdown_highlight_enabled" = "1" ]]; then
        local _sm_hl_raw _sm_hl_line _sm_hl_i
        local -a _sm_hl_lines _sm_hl_row_spans
        _sm_hl_raw=$(printf '%s\n' "${_smarthistory_dropdown_candidates[@]}" \
            | bat --language=bash --color=always --plain --theme "$_smarthistory_dropdown_bat_theme" \
                  --paging=never --tabs=0 2>/dev/null)
        _sm_hl_lines=("${(f)_sm_hl_raw}")
        for (( _sm_hl_i = 1; _sm_hl_i <= ${#_smarthistory_dropdown_candidates}; _sm_hl_i++ )); do
            _sm_hl_line=${_sm_hl_lines[$_sm_hl_i]:-}
            _sm_hl_row_spans=()
            if [[ -n "$_sm_hl_line" ]]; then
                _smarthistory_ansi_to_spans "$_sm_hl_line" _sm_hl_row_spans
                # `bat` must not have altered the text (reflowed,
                # trimmed, ...) — if it doesn't match byte-for-byte,
                # skip highlighting this row rather than risk
                # splicing spans against text they don't describe.
                if [[ "$REPLY" != "${_smarthistory_dropdown_candidates[$_sm_hl_i]}" ]]; then
                    _sm_hl_row_spans=()
                fi
            fi
            _smarthistory_dropdown_hl_spans+=("${(F)_sm_hl_row_spans}")
        done
        unset _sm_hl_raw _sm_hl_line _sm_hl_i _sm_hl_lines _sm_hl_row_spans
    fi
    _smarthistory_dropdown_visible=1
    # A fresh candidate set from a new keystroke always starts
    # unchosen — the user must re-navigate (Tab / Shift-Tab) to pick a
    # row again, even if they had one highlighted before this render.
    _smarthistory_dropdown_chosen=0
    if (( _smarthistory_dropdown_selected >= ${#_smarthistory_dropdown_candidates} )); then
        _smarthistory_dropdown_selected=0
    fi
    _smarthistory_dropdown_paint
    [[ "$_sm_dropdown_xtrace_was" == "on" ]] && setopt XTRACE
    unset _sm_dropdown_xtrace_was
}

# Register `hookfn` to run after `widget`'s real handler, without
# ever nesting a second smarthistory-owned wrapper function around an
# existing one. `.widget` is only valid as an argument TO `zle` (to
# call a builtin bypassing overrides), not as a `zle -N` target
# directly — a plain `zle -N $orig .$widget` fails with "No such
# shell function". The fix (the same dot-call trick
# zsh-autosuggestions uses in src/bind.zsh): define a tiny dispatcher
# function whose body does the dot-call, then register THAT function
# as the widget — but unlike a naive per-feature wrap-the-current-
# widget approach, there is only ever ONE dispatcher per widget, fed
# by a growable list of hook function names, so re-sourcing this
# file (or having multiple smarthistory features hook the same
# widget, e.g. both `dropdown` and `commentexpand` on `self-insert`)
# never creates a second layer.
#
# This matters because zsh resolves a `zle`-invoked function by NAME
# at call time, not by a snapshotted reference. An earlier version of
# this file wrapped each feature's own wrapper around whatever was
# currently bound, using a fixed per-feature function name. That's
# safe for a single feature re-sourced alone, but with two features
# each wrapping the OTHER's wrapper, a second `eval "$(smarthistory
# init zsh)"` in the same shell would redefine both wrapper functions
# in place while each one's body still referenced the other's name —
# producing a two-function call cycle and a hard "maximum nested
# function level reached" crash on the very next keystroke. A single
# dispatcher that only ever wraps the pristine original widget once,
# plus an idempotent (dedup by name) hook list appended to on every
# source, has no such cycle to form.
_smarthistory_register_hook() {
    local widget=$1 hookfn=$2
    local safe=${widget//-/_}
    local dispatch="_smarthistory_dispatch_${safe}"
    local orig="_smarthistory_orig_${safe}"
    local hooks_name="_smarthistory_hooks_${safe}"
    typeset -g $hooks_name
    if [[ "${widgets[$widget]:-}" != "user:${dispatch}" ]]; then
        case ${widgets[$widget]:-} in
            builtin)
                eval "${orig}() { zle .${widget} }"
                zle -N $orig $orig
                ;;
            user:*)
                zle -N $orig ${widgets[$widget]#user:}
                ;;
            *) return ;;
        esac
        eval "${dispatch}() {
            zle ${orig} -- \"\$@\"
            local _sm_hook
            for _sm_hook in \${(s: :)${hooks_name}}; do
                \$_sm_hook
            done
        }"
        zle -N $widget $dispatch
    fi
    local current="${(P)hooks_name}"
    [[ " $current " == *" $hookfn "* ]] || eval "${hooks_name}=\"\${${hooks_name}} ${hookfn}\""
}
if [[ "$_smarthistory_dropdown_enabled" = "1" ]]; then
    for _smarthistory_dropdown_w in self-insert self-insert-unmeta \
        backward-delete-char delete-char backward-kill-word \
        kill-whole-line bracketed-paste; do
        _smarthistory_register_hook $_smarthistory_dropdown_w _smarthistory_dropdown_render
    done
    unset _smarthistory_dropdown_w
    # Extend `LBUFFER` to the longest prefix common to every current
    # `_smarthistory_dropdown_candidates` entry — readline's classic
    # "expand to longest unambiguous completion". Pairwise-reduces a
    # running prefix string against each candidate in turn; the
    # candidate lists here are tiny (`dropdown.limit`, default 6) so
    # there's no need for anything cleverer. Returns success (and
    # updates `BUFFER`/`CURSOR`) only when that prefix is strictly
    # longer than the current buffer — i.e. there's actually more
    # unambiguous text to add. With exactly one candidate this always
    # succeeds and expands to the whole thing (the `--prefix` search
    # that built the candidate list already guarantees `LBUFFER` is a
    # prefix of it). Failure means every candidate already diverges
    # at the very next character — nothing to expand.
    _smarthistory_dropdown_expand_common_prefix() {
        local lcp=${_smarthistory_dropdown_candidates[1]}
        local cand
        for cand in "${_smarthistory_dropdown_candidates[@]:1}"; do
            while [[ -n "$lcp" && "$cand" != "$lcp"* ]]; do
                lcp=${lcp[1,-2]}
            done
            [[ -z "$lcp" ]] && break
        done
        (( $#lcp > $#LBUFFER )) || return 1
        BUFFER=$lcp
        CURSOR=$#BUFFER
        return 0
    }
    # Tab cycles the highlighted candidate forward (same wraparound
    # math as Down) WITHOUT touching BUFFER or closing the menu —
    # only Enter (see `_smarthistory_reset_and_accept` below) commits
    # the highlighted candidate. Falls through to the normal
    # completion widget when no menu is showing (zsh's documented
    # default Tab binding in emacs mode, preserved explicitly since
    # we're taking over `^I`).
    #
    # On a fresh (not-yet-cycled) candidate set, the FIRST Tab press
    # instead tries `_smarthistory_dropdown_expand_common_prefix`
    # first — if it can extend the buffer, re-render against that
    # longer `LBUFFER` (the same `smarthistory search` round-trip
    # every keystroke already does, naturally narrowing/refreshing
    # the candidate list) and jump straight to "chosen" so the VERY
    # NEXT Tab cycles instead of trying to expand again, which would
    # be a no-op once `LBUFFER` already sits at the common prefix.
    # When there's nothing to expand, falls through to the unchanged
    # "highlight row 0" behavior below. `Down` (`_smarthistory_down_history`)
    # calls this same widget when the dropdown is open, so it picks
    # up the same expand-then-cycle behavior — consistent with that
    # widget's existing "Down works exactly like Tab" design, not a
    # new inconsistency.
    _smarthistory_dropdown_accept() {
        if [[ $_smarthistory_dropdown_visible -eq 1 ]]; then
            if (( _smarthistory_dropdown_chosen == 0 )) \
                && _smarthistory_dropdown_expand_common_prefix; then
                _smarthistory_dropdown_render
                if [[ $_smarthistory_dropdown_visible -eq 1 ]]; then
                    _smarthistory_dropdown_chosen=1
                    _smarthistory_dropdown_paint
                fi
                return
            fi
            # Tab always advances the selection, even from the
            # unchosen default (index 0) — so a lone-candidate menu
            # still lets Tab "choose" it in place rather than needing
            # a wraparound press.
            if (( _smarthistory_dropdown_chosen == 1 )); then
                _smarthistory_dropdown_selected=$(( (_smarthistory_dropdown_selected + 1) % ${#_smarthistory_dropdown_candidates} ))
            fi
            _smarthistory_dropdown_chosen=1
            _smarthistory_dropdown_paint
            return
        fi
        zle expand-or-complete
    }
    zle -N _smarthistory_dropdown_accept
    bindkey '^I' _smarthistory_dropdown_accept
    # Esc dismisses the menu without touching BUFFER, and keeps it
    # closed for the rest of this command line (see
    # `_smarthistory_dropdown_suppressed`). Nothing was bound to bare
    # Esc before this feature existed, so the "menu not visible"
    # branch is a genuine no-op (preserves prior behavior exactly) —
    # in particular it must NOT set the suppression flag, or pressing
    # Esc before the menu has even appeared would silently disable it
    # for the whole line.
    _smarthistory_dropdown_dismiss() {
        if [[ $_smarthistory_dropdown_visible -eq 1 ]]; then
            _smarthistory_dropdown_clear
            _smarthistory_dropdown_suppressed=1
        fi
    }
    zle -N _smarthistory_dropdown_dismiss
    bindkey '^[' _smarthistory_dropdown_dismiss
    # Shift-Tab cycles backward — the mirror of Tab above. Nothing
    # was bound to it before this feature (the terminal sends
    # `\e[Z`); falls through to `reverse-menu-complete`, the natural
    # backward-cycle analog of Tab's `expand-or-complete` fallback,
    # when no menu is showing.
    _smarthistory_dropdown_accept_prev() {
        if [[ $_smarthistory_dropdown_visible -eq 1 ]]; then
            # Same "first press just highlights index 0" rule as Tab
            # above — see `_smarthistory_dropdown_accept`.
            if (( _smarthistory_dropdown_chosen == 1 )); then
                _smarthistory_dropdown_selected=$(( (_smarthistory_dropdown_selected - 1 + ${#_smarthistory_dropdown_candidates}) % ${#_smarthistory_dropdown_candidates} ))
            fi
            _smarthistory_dropdown_chosen=1
            _smarthistory_dropdown_paint
            return
        fi
        zle reverse-menu-complete
    }
    zle -N _smarthistory_dropdown_accept_prev
    bindkey '^[[Z' _smarthistory_dropdown_accept_prev
    # Commit the highlighted candidate into BUFFER and close the
    # menu. `$1` decides where CURSOR ends up: "start" -> 0, "end" ->
    # end of the new BUFFER, anything else (including no argument) ->
    # leave CURSOR at whatever value it already had (used by the
    # Right/Left arrow widgets below, which commit without repositioning
    # the cursor at all). Shared by Ctrl-A/Ctrl-E/Right/Left. (Enter
    # has its own copy of this logic in `_smarthistory_reset_and_accept`,
    # outside this block, since it must run before
    # `_smarthistory_reset_state` at a different call site.)
    _smarthistory_dropdown_commit() {
        local raw=${_smarthistory_dropdown_candidates[$((_smarthistory_dropdown_selected+1))]}
        BUFFER=$(_smarthistory_unescape "$raw")
        case "$1" in
            start) CURSOR=0 ;;
            end)   CURSOR=${#BUFFER} ;;
            *)     ;;  # leave CURSOR untouched
        esac
        _smarthistory_dropdown_clear
        _smarthistory_dropdown_selected=0
    }
    # ^ `_smarthistory_dropdown_clear` already resets `_chosen`; the
    # explicit `_selected=0` above just picks a sane default row for
    # whenever the menu reopens.
    # Ctrl-E: select the highlighted candidate, cursor at the end.
    # Falls through to zsh's default `end-of-line` when no menu is
    # showing (we're taking over `^E`, so preserve its prior meaning
    # explicitly).
    _smarthistory_dropdown_select_end() {
        if [[ $_smarthistory_dropdown_visible -eq 1 ]]; then
            _smarthistory_dropdown_commit end
            return
        fi
        zle end-of-line
    }
    zle -N _smarthistory_dropdown_select_end
    bindkey '^E' _smarthistory_dropdown_select_end
    # Ctrl-A: select the highlighted candidate, cursor at the start.
    # Falls through to `beginning-of-line` otherwise, same reasoning.
    _smarthistory_dropdown_select_start() {
        if [[ $_smarthistory_dropdown_visible -eq 1 ]]; then
            _smarthistory_dropdown_commit start
            return
        fi
        zle beginning-of-line
    }
    zle -N _smarthistory_dropdown_select_start
    bindkey '^A' _smarthistory_dropdown_select_start
    # Ctrl-K: standard kill-line, plus close the dropdown menu first if
    # it's open — and keep it closed for the rest of this command line
    # (see `_smarthistory_dropdown_suppressed`). Nothing was bound to
    # `^K` before this feature (zsh's emacs-mode default is
    # `kill-line`; this dropdown code never touched it), so the
    # fallthrough is `zle kill-line` unconditionally — same "call the
    # currently-registered widget, not a hardcoded builtin" convention
    # the other fallbacks use (see Ctrl-A/Ctrl-E/arrows above). Closing
    # the menu here means every other dropdown-aware binding reverts
    # to its plain zsh meaning on the very next keystroke, since they
    # all branch on `_smarthistory_dropdown_visible`.
    _smarthistory_dropdown_kill_line() {
        if [[ $_smarthistory_dropdown_visible -eq 1 ]]; then
            _smarthistory_dropdown_clear
            _smarthistory_dropdown_suppressed=1
        fi
        zle kill-line
    }
    zle -N _smarthistory_dropdown_kill_line
    bindkey '^K' _smarthistory_dropdown_kill_line
    # Right arrow: select the highlighted candidate WITHOUT moving
    # the cursor — unlike Ctrl-E, CURSOR is left exactly where it was
    # (typically mid-word, right after whatever prefix the user had
    # typed), not jumped to the start or end of the (now longer)
    # committed text. Falls through to normal cursor-right
    # (`forward-char`) when no menu is showing, so ordinary editing
    # is completely unaffected. The actual key bindings (including
    # terminfo/symbolic fallbacks, for terminals that emit a
    # different escape sequence) are registered further down, next
    # to the Up/Down bindings — see the comment there.
    _smarthistory_dropdown_select_end_arrow() {
        if [[ $_smarthistory_dropdown_visible -eq 1 ]]; then
            _smarthistory_dropdown_commit
            return
        fi
        zle forward-char
    }
    zle -N _smarthistory_dropdown_select_end_arrow
    # Left arrow: same commit-without-moving-cursor behavior as Right
    # above (both do exactly the same thing to CURSOR — "leave it" —
    # they only differ in their non-visible-menu fallback). Falls
    # through to normal cursor-left (`backward-char`) otherwise.
    _smarthistory_dropdown_select_start_arrow() {
        if [[ $_smarthistory_dropdown_visible -eq 1 ]]; then
            _smarthistory_dropdown_commit
            return
        fi
        zle backward-char
    }
    zle -N _smarthistory_dropdown_select_start_arrow
    # Ctrl-Space: run the FIRST candidate (index 0) immediately,
    # regardless of whatever row Tab/Down navigation may have
    # highlighted — a "just run the top match" shortcut that skips
    # navigating there first. Mirrors `_smarthistory_reset_and_accept`
    # (Enter, defined further down): commit the candidate into
    # BUFFER, reset the cached dropdown/history state, then run it
    # via `zle .accept-line`. Most terminals send NUL (`^@`) for
    # Ctrl-Space, which is unbound in vanilla zsh, so the no-menu
    # case is a genuine no-op — there's no prior behavior to
    # preserve.
    _smarthistory_dropdown_run_first() {
        if [[ $_smarthistory_dropdown_visible -eq 1 && ${#_smarthistory_dropdown_candidates} -gt 0 ]]; then
            local raw=${_smarthistory_dropdown_candidates[1]}
            BUFFER=$(_smarthistory_unescape "$raw")
            CURSOR=${#BUFFER}
            _smarthistory_reset_state
            zle .accept-line
        fi
    }
    zle -N _smarthistory_dropdown_run_first
    bindkey '^@' _smarthistory_dropdown_run_first
fi
# Post-hook for the comment-expansion widget below: runs after the
# real self-insert, so `LBUFFER` already includes the just-typed
# character. Only fires on a literal space keystroke, with the
# cursor at the end of the buffer, and only when everything typed so
# far on the line (i.e. `word`, the buffer with the trailing space
# stripped) contains no other whitespace — that's what enforces
# "written to the *begin* of the command line": if an earlier space
# had already been typed, `word` would contain it too, and the guard
# below would fail.
_smarthistory_commentexpand_check() {
    [[ "$_smarthistory_commentexpand_enabled" = "1" ]] || return
    [[ $CURSOR -eq $#BUFFER ]] || return
    # Checking `LBUFFER` for a trailing space (rather than requiring
    # `$KEYS` to be exactly one space) is what makes this robust to
    # typeahead batching: when a slow keystroke handler (e.g. the
    # dropdown feature's synchronous `smarthistory search` call on
    # every character) can't keep up with fast typing, the terminal
    # may deliver several buffered characters to a single
    # `self-insert` invocation, so `$KEYS` won't be a lone space even
    # though the buffer now genuinely ends in one.
    [[ "$LBUFFER" == *' ' ]] || return
    local word=${LBUFFER%' '}
    [[ -n "$word" && "$word" != *[[:space:]]* ]] || return
    local resolved
    resolved=$(smarthistory expand "$word" 2>/dev/null)
    [[ -n "$resolved" ]] || return
    BUFFER="${resolved} "
    CURSOR=$#BUFFER
    # Clear any stale dropdown suggestion the buffer replacement left
    # behind — a no-op when the dropdown feature isn't enabled.
    (( ${+functions[_smarthistory_dropdown_clear]} )) && _smarthistory_dropdown_clear
}
if [[ "$_smarthistory_commentexpand_enabled" = "1" ]]; then
    # `magic-space` (not `self-insert`) is what a plain space
    # keypress actually invokes when it's bound the way oh-my-zsh's
    # `lib/key-bindings.zsh` (and plenty of plain .zshrc setups) bind
    # it — `bindkey ' ' magic-space` is the well-known zsh default for
    # history-bang expansion (e.g. `!!ls` + space). A hook only on
    # `self-insert`/`self-insert-unmeta` would then never fire for the
    # one keystroke this feature cares about, so `magic-space` is
    # wrapped here too — harmless to wrap unconditionally even when
    # space is left on plain `self-insert`, since only one of the two
    # widgets is ever actually bound to the space key at a time.
    for _smarthistory_ce_w in self-insert self-insert-unmeta magic-space; do
        _smarthistory_register_hook $_smarthistory_ce_w _smarthistory_commentexpand_check
    done
    unset _smarthistory_ce_w
fi

_smarthistory_reset_state() {
    _smarthistory_matches=""
    _smarthistory_index=0
    _smarthistory_query_key=""
    _smarthistory_last_match=""
    _smarthistory_dropdown_clear
    _smarthistory_dropdown_selected=0
    _smarthistory_debug_log "reset_state: cleared all caches"
}

_smarthistory_update_rprompt() {
    case "$_smarthistory_mode" in
        sess)   label="[smarthistory: SESS]" ;;
        dir)    label="[smarthistory: DIR]" ;;
        global) label="[smarthistory: GLOBAL]" ;;
        *)      label="[smarthistory: ?]" ;;
    esac
    if [ -n "$_smarthistory_rprompt_save" ]; then
        MYPROMPT="$label $_smarthistory_rprompt_save"
    else
        MYPROMPT="$label"
    fi
    echo $MYPROMPT
}

_smarthistory_cycle_mode() {
    local old_mode="$_smarthistory_mode"
    case "$_smarthistory_mode" in
        sess)   _smarthistory_mode="dir" ;;
        dir)    _smarthistory_mode="global" ;;
        global) _smarthistory_mode="sess" ;;
    esac
    _smarthistory_debug_log "cycle_mode: $old_mode -> $_smarthistory_mode"
    # Invalidate the match cache; the next Up/Down will re-query under
    # the new scope.
    _smarthistory_reset_state
    _smarthistory_update_rprompt
    # `_smarthistory_reset_state` just cleared the dropdown (if any
    # was showing); re-render immediately under the new scope rather
    # than leaving the buffer bare until the next keystroke. Ctrl-G
    # doesn't fire self-insert, so nothing else would trigger this.
    if [[ "$_smarthistory_dropdown_enabled" = "1" ]]; then
        _smarthistory_dropdown_render
    fi
}

# Populate the match cache for the current (mode, pwd, prefix) triple.
# Sets _smarthistory_matches and resets _smarthistory_index to 0.
# Called by both Up and Down whenever the cache is stale.
#
# The cache is keyed on the *original* prefix (the LBUFFER at the
# time the user typed before pressing Up), not the current LBUFFER.
# After the first Up, BUFFER contains a full match (e.g. "test-thing-1"),
# not the original prefix ("test"). Re-priming on that would search
# for the full string and return only itself, making Up a no-op.
# To detect "user pressed Up again" vs "user typed new text", we
# compare BUFFER to the most recent match we set; if they match,
# the user just pressed Up/Down and we keep walking.
# Debug logging. Set SMARTHISTORY_DEBUG=1 in the environment to
# enable. The log file is created on first use and appended to.
# Use a small `tail -f ~/.local/cache/smarthistory/widget-debug.log`
# from another terminal to watch what the widget is doing.
_smarthistory_debug_log() {
    [ "$SMARTHISTORY_DEBUG" = "1" ] || return 0
    local msg="$1"
    local logfile="$HOME/.local/cache/smarthistory/widget-debug.log"
    # Best-effort: don't fail the widget if the log can't be written.
    {
        print -r -- "$(date '+%H:%M:%S') $msg" >> "$logfile" 2>/dev/null
    } || true
}

_smarthistory_prime_cache() {
    # Two checks decide whether to re-query:
    #
    # 1. Did the user just press Up/Down (no new typing)? If BUFFER
    #    still equals the last match we showed, the user pressed
    #    Up/Down again. The cached results are still valid; we just
    #    need to advance the index. (Without this check, the
    #    second press would re-query with the previous match as
    #    the new prefix, returning only that one row and effectively
    #    making Up a no-op.)
    #
    # 2. Has the (mode, pwd, prefix) triple changed since the last
    #    query? If the user `cd`'d, switched scope (Ctrl+G), or
    #    typed a new prefix, the cached results may be stale.
    #
    # The first check fires for the common "press Up again" case.
    # The second check fires when state has actually changed.
    if [ -n "$_smarthistory_last_match" ] && [ "$BUFFER" = "$_smarthistory_last_match" ]; then
        _smarthistory_debug_log "prime_cache: BUFFER == last_match, advancing without re-query"
        return
    fi
    local query_key="$_smarthistory_mode|$PWD|$LBUFFER"
    _smarthistory_debug_log "prime_cache: BUFFER=[$BUFFER] LBUFFER=[$LBUFFER] PWD=[$PWD] mode=[$_smarthistory_mode] query_key=[$query_key] cached=[$_smarthistory_query_key]"
    if [ "$query_key" = "$_smarthistory_query_key" ]; then
        _smarthistory_debug_log "prime_cache: cache HIT, skipping re-query"
        return
    fi
    # Re-query with the current LBUFFER (which is the user's typed
    # prefix, since neither check fired).
    local args=("$LBUFFER" --limit 0 --no-highlight)
    case "$_smarthistory_mode" in
        sess)   args+=(--session) ;;
        dir)    args+=(--directory "$PWD") ;;
        global) ;;
    esac
    _smarthistory_debug_log "prime_cache: cache MISS, running: smarthistory search ${args[*]}"
    _smarthistory_matches=$(smarthistory search "${args[@]}" 2>/dev/null)
    _smarthistory_index=0
    _smarthistory_query_key="$query_key"
    _smarthistory_last_match=""
    # Count how many matches we got (one match per non-empty line).
    local match_count=0
    local line
    for line in ${(f)_smarthistory_matches}; do
        [ -n "$line" ] && match_count=$((match_count + 1))
    done
    _smarthistory_debug_log "prime_cache: got $match_count match(es) (LBUFFER=[$LBUFFER], PWD=[$PWD])"
}

_smarthistory_unescape() {
    # The CLI escapes newlines in
    # multiline commands as the
    # two-character sequence `\n`
    # so a single row fits on one
    # line of CLI output and the
    # `(f)` record splitter sees
    # exactly one match per row.
    # Here we convert the escape
    # back to a real newline so the
    # zsh line editor renders the
    # command as the user originally
    # typed it: with multiple
    # physical lines.
    #
    # Zsh's `${var//pattern/repl}`
    # expansion treats the
    # backslashes in `\\n` as
    # literal two-character
    # patterns, and the `$'\n'`
    # replacement is an ANSI-C
    # quoted string that yields a
    # real newline.
    local out=$1
    out=${out//\\n/$'\n'}
    out=${out//\\r/$'\r'}
    printf %s "$out"
}


_smarthistory_up_history() {
    # When the live dropdown is showing, Up works exactly like
    # Shift-Tab (`_smarthistory_dropdown_accept_prev`, bound to
    # `^[[Z` above) instead of walking the (separate, keypress-only)
    # Up/Down history cache below — calling the same function
    # guarantees identical behavior, including the "first press just
    # highlights the current row without moving" rule
    # (`_smarthistory_dropdown_chosen` gating) that a hand-rolled
    # index decrement here previously got wrong: it always moved the
    # index AND never set `_smarthistory_dropdown_chosen=1`, so the
    # very first Up/Down press silently skipped a row and painted
    # with no visible highlight at all (the paint function only
    # marks a row when `chosen == 1`). Everything below this branch
    # is completely unchanged from before the dropdown feature
    # existed.
    if [[ "$_smarthistory_dropdown_enabled" = "1" && $_smarthistory_dropdown_visible -eq 1 ]]; then
        _smarthistory_dropdown_accept_prev
        return
    fi
    # Always use smarthistory, even with an empty LBUFFER (an empty
    # query means "give me the oldest command in the current scope").
    _smarthistory_prime_cache
    # Split the newline-joined match list into a real array. Using
    # `local -a` + assignment is the only reliable way to get the
    # correct element count in zsh.
    local -a _smarthistory_lines
    _smarthistory_lines=("${(f)_smarthistory_matches}")
    local n=${#_smarthistory_lines}
    if [ $n -eq 0 ]; then
        zle -M "no history matches"
        return
    fi
    if [ $_smarthistory_index -ge $n ]; then
        # Already at the newest entry; stay put.
        zle -M "no more history"
        _smarthistory_debug_log "up: at end of list (index=$_smarthistory_index/$n), no-op"
        return
    fi
    _smarthistory_index=$((_smarthistory_index + 1))
    # The CLI escapes newlines in
    # multiline commands; un-escape
    # so the line editor renders
    # the command across multiple
    # physical lines (as the user
    # originally typed it).
    local raw_match=${_smarthistory_lines[$_smarthistory_index]}
    local match
    match=$(_smarthistory_unescape "$raw_match")
    BUFFER="$match"
    CURSOR=${#BUFFER}
    # Store the un-escaped version
    # so the next Up/Down cycle
    # detection (`BUFFER ==
    # last_match`) compares apples
    # to apples — both contain real
    # newlines, not `\n` escapes.
    _smarthistory_last_match="$match"
    _smarthistory_debug_log "up: index=$_smarthistory_index/$n BUFFER=[$match]"
}
_smarthistory_down_history() {
    # Down works exactly like Tab (`_smarthistory_dropdown_accept`,
    # bound to `^I` above) when the dropdown is showing — see
    # `_smarthistory_up_history` above for why this calls the same
    # function rather than re-deriving the index arithmetic.
    if [[ "$_smarthistory_dropdown_enabled" = "1" && $_smarthistory_dropdown_visible -eq 1 ]]; then
        _smarthistory_dropdown_accept
        return
    fi
    # Down walks the match list in the *opposite* direction of Up
    # (Up advances through the array from oldest to newest, Down
    # walks back from newest to oldest). At the very start of the
    # list (index 0 in zsh's 1-based array), there's nothing older
    # to show, so Down clears the line buffer.
    _smarthistory_prime_cache
    local -a _smarthistory_lines
    _smarthistory_lines=("${(f)_smarthistory_matches}")
    local n=${#_smarthistory_lines}
    if [ $n -eq 0 ]; then
        zle -M "no history matches"
        return
    fi
    if [ $_smarthistory_index -le 0 ]; then
        # At the start of the list (oldest entry, or fresh prompt).
        # Clear the buffer to signal "nothing older than this."
        BUFFER=""
        CURSOR=0
        _smarthistory_last_match=""
        zle -M "no older history (line cleared)"
        _smarthistory_debug_log "down: at start of list, cleared BUFFER"
        return
    fi
    _smarthistory_index=$((_smarthistory_index - 1))
    local raw_match=${_smarthistory_lines[$_smarthistory_index]}
    local match
    match=$(_smarthistory_unescape "$raw_match")
    BUFFER="$match"
    CURSOR=${#BUFFER}
    _smarthistory_last_match="$match"
    _smarthistory_debug_log "down: index=$_smarthistory_index/$n BUFFER=[$match]"
}
# Reset bindings for accept-line and send-break are defined further
# down (next to the keybindings).
zle -N _smarthistory_up_history
zle -N _smarthistory_down_history
zle -N _smarthistory_cycle_mode
# Ctrl-S: insert the most probable next command that follows the
# last executed command in the global history. Each subsequent
# press cycles through the next candidates in order of decreasing
# probability. The cycle resets to the top candidate when a new
# command is actually executed (handled in the precmd hook).
_smarthistory_next_history() {
    if [ -z "$_smarthistory_last_cmd" ]; then
        zle -M "no previous command yet"
        return
    fi
    # Fetch the candidate list (freq<TAB>command, one per line,
    # sorted by descending frequency). We fetch on every press so
    # that newly-added commands are visible immediately. The awk
    # script extracts just the command field, one per line.
    local -a _smarthistory_candidates
    _smarthistory_candidates=("${(f)$(smarthistory next "$_smarthistory_last_cmd" --limit 10 2>/dev/null | cut -f2)}")
    local n=${#_smarthistory_candidates}
    if [ $n -eq 0 ]; then
        zle -M "no suggestions after '$_smarthistory_last_cmd'"
        return
    fi
    # Cycle through candidates. Reset on each new command (precmd).
    if [ $_smarthistory_next_index -ge $n ]; then
        _smarthistory_next_index=0
    fi
    local raw_match=${_smarthistory_candidates[$((_smarthistory_next_index + 1))]}
    local match
    match=$(_smarthistory_unescape "$raw_match")
    BUFFER="$match"
    CURSOR=${#BUFFER}
    _smarthistory_next_index=$((_smarthistory_next_index + 1))
    _smarthistory_debug_log "next_history: after=[$_smarthistory_last_cmd] picked=[$match] index=$_smarthistory_next_index/$n"
}
zle -N _smarthistory_next_history
# Use terminfo for robust Up/Down key bindings across terminals.
zmodload zsh/terminfo
bind_key_universal() {
    local key_name=$1
    local widget_name=$2
    if [[ -n "${terminfo[$key_name]}" ]]; then
        bindkey "${terminfo[$key_name]}" "$widget_name"
    fi
}
bind_key_universal kcuu1 _smarthistory_up_history
bind_key_universal kcud1 _smarthistory_down_history
# Fallback/alternative bindings
bindkey '<Up>' _smarthistory_up_history
bindkey '<Down>' _smarthistory_down_history
bindkey '^[[A' _smarthistory_up_history
bindkey '^[[B' _smarthistory_down_history
# Same terminfo + symbolic + raw redundancy for the dropdown's
# Right/Left widgets (registered earlier, in the dropdown-enabled
# block above, via plain `zle -N` — only the actual key bindings live
# here). A single raw `bindkey '^[[C'` isn't enough: depending on
# whether the terminal is in "normal" vs "application" cursor-key
# mode (DECCKM — e.g. left toggled-on by a prior curses/ratatui
# program, like smarthistory's own TUI, not fully resetting it),
# Right/Left can send `^[[C`/`^[[D` OR `^[OC`/`^[OD` instead —
# exactly the ambiguity this same 4-fold pattern already exists to
# paper over for Up/Down.
if [[ "$_smarthistory_dropdown_enabled" = "1" ]]; then
    bind_key_universal kcuf1 _smarthistory_dropdown_select_end_arrow
    bind_key_universal kcub1 _smarthistory_dropdown_select_start_arrow
    bindkey '<Right>' _smarthistory_dropdown_select_end_arrow
    bindkey '<Left>' _smarthistory_dropdown_select_start_arrow
    bindkey '^[[C' _smarthistory_dropdown_select_end_arrow
    bindkey '^[[D' _smarthistory_dropdown_select_start_arrow
    bindkey '^[OC' _smarthistory_dropdown_select_end_arrow
    bindkey '^[OD' _smarthistory_dropdown_select_start_arrow
fi
# Ctrl-g: cycle the search scope (SESS -> DIR -> GLOBAL -> SESS) and
# show the current scope in the RPROMPT.
bindkey '^G' _smarthistory_cycle_mode
# Ctrl-S: insert the most probable next command (see the
# _smarthistory_next_history widget above). On most terminals
# Ctrl-S is the XOFF flow-control character; `stty -ixon` makes
# it available to zle.
stty -ixon 2>/dev/null
bindkey '^S' _smarthistory_next_history
# Reset the cached state whenever the line is accepted (Enter, Ctrl+J)
# or abandoned (Ctrl+C). Without this, the next Up press inherits
# _smarthistory_index from the previous walk and lands on an
# unexpected match.
_smarthistory_reset_and_accept() {
    # If the live dropdown is showing AND the user has actually
    # navigated to a row (Tab / Shift-Tab — see
    # `_smarthistory_dropdown_accept`), Enter commits that row instead
    # of running the typed buffer. With no explicit choice (the
    # default state right after the menu opens), `_dropdown_chosen` is
    # 0 and Enter falls through to `zle .accept-line` on whatever was
    # typed, ignoring the menu entirely. Must read the candidate
    # BEFORE `_smarthistory_reset_state` clears the dropdown state
    # below.
    if [[ "$_smarthistory_dropdown_enabled" = "1" && $_smarthistory_dropdown_visible -eq 1 && $_smarthistory_dropdown_chosen -eq 1 ]]; then
        local raw=${_smarthistory_dropdown_candidates[$((_smarthistory_dropdown_selected+1))]}
        BUFFER=$(_smarthistory_unescape "$raw")
        CURSOR=${#BUFFER}
    fi
    _smarthistory_debug_log "accept-line: resetting state, BUFFER=[$BUFFER]"
    _smarthistory_reset_state
    zle .accept-line
}
_smarthistory_reset_and_break() {
    _smarthistory_debug_log "send-break: resetting state"
    _smarthistory_reset_state
    zle .send-break
}
zle -N accept-line _smarthistory_reset_and_accept
zle -N send-break _smarthistory_reset_and_break
# Ctrl-C is undefined-key in vanilla zsh, so wire it to a widget
# that resets state and aborts the current line. This makes Ctrl-C
# behave like Ctrl-G plus a buffer-cancel.
_smarthistory_reset_and_abort_line() {
    _smarthistory_debug_log "ctrl-c: resetting state, BUFFER=[$BUFFER]"
    _smarthistory_reset_state
    zle .kill-whole-line
    zle .send-break
}
zle -N _smarthistory_reset_and_abort_line
bindkey '^C' _smarthistory_reset_and_abort_line
# Initialize the RPROMPT the first time the prompt is shown. We can't
# call the update function inline at init time because ZLE is not yet
# active (zle reset-prompt would error).
_smarthistory_init_rprompt() {
    _smarthistory_update_rprompt
    add-zsh-hook -d precmd _smarthistory_init_rprompt
}
add-zsh-hook precmd _smarthistory_init_rprompt
