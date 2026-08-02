# shellcheck shell=bash
# Smart History Bash Init
#
# A deliberately smaller subset of what `smarthistory init zsh`
# (src/init.zsh) provides: the history-capture pipeline plus a
# handful of line-editor widgets. What's NOT here, and why: the live
# dropdown completion box (candidates, syntax highlighting, shadow
# text, Tab-expand-to-common-prefix) is built entirely on zsh's
# `POSTDISPLAY` (live-redrawing content below the cursor) and
# `region_highlight` (per-character coloring) — GNU Readline (bash's
# line editor) has no equivalent of either. The only ways to
# approximate it are hand-rolled raw ANSI cursor manipulation (fragile
# across terminal resize, multi-line prompts, tmux) or depending on
# `ble.sh` (a whole separate alternative bash line editor) — neither
# is worth it here.
#
# The interactive widgets below all need bash >= 4.0: they read/write
# `READLINE_LINE`/`READLINE_POINT`, special variables `bind -x`
# callbacks use to see/edit the current input line (bash's closest
# equivalent of zsh's `BUFFER`/`CURSOR`) — added in bash 4.0 (2009).
# macOS ships bash 3.2.57 by default (Apple's last GPLv2 release;
# they never updated past it) and does NOT have them — on 3.2 this
# script still installs the history-capture pipeline (preexec/precmd
# below need nothing newer than ancient `trap ... DEBUG` support) but
# skips widget registration entirely, so Ctrl-R/Up/Down/etc. keep
# their normal Readline behavior. Install a newer bash (`brew install
# bash` on macOS) to get the widgets too.

export SMART_HISTORY_SESSION="{session_id}"

# ---- preexec/precmd equivalents ----
#
# Bash has no native preexec/precmd hooks. The standard technique
# (popularized by the `bash-preexec` project, which most bash
# frameworks build on rather than reinventing) is a `DEBUG` trap plus
# `PROMPT_COMMAND`. Two things make this safe rather than a source of
# duplicate/spurious captures:
#
# 1. By default (without `set -o functrace`, which we never set),
#    bash's DEBUG trap does NOT propagate into shell functions — so
#    routing PROMPT_COMMAND through a function (`_smarthistory_precmd_wrapper`
#    below), rather than inline commands, means the trap never re-fires
#    for the precmd body's own internal commands.
# 2. The DEBUG trap fires once per SIMPLE command, not once per typed
#    LINE — `echo a; echo b` fires it twice. zsh's preexec fires once
#    per line. Rather than trying to reconstruct the typed line from
#    `BASH_COMMAND` (which only ever holds one simple command at a
#    time), the trap is used purely as a "something ran" signal; the
#    actual command TEXT is read back from bash's own history at
#    precmd time (`history 1`) and deduped against the last-seen
#    entry — the same technique bash-preexec itself uses.
_smarthistory_cmd_ran=0
_smarthistory_debug_trap() {
    _smarthistory_cmd_ran=1
}
trap '_smarthistory_debug_trap' DEBUG

_smarthistory_last_history_line=""
_smarthistory_last_cmd=""

# Record a finished command: mirrors `_smarthistory_precmd` in
# src/init.zsh (same capture-tmux/capture-herdr/add branches, same
# space-prefix privacy convention), minus the dropdown-specific
# state resets (`_smarthistory_dropdown_clear` etc. — dropdown isn't
# part of this port).
_smarthistory_precmd() {
    local cmd="$1" exit_code="$2"
    [ -n "$cmd" ] || return 0
    # Space-prefixed commands are sensitive (a credential, a
    # destructive op, a private URL) and must not be recorded — same
    # convention `_smarthistory_precmd` documents in src/init.zsh,
    # re-implemented explicitly here since bash's own HISTCONTROL
    # settings (which decide what even reaches `history 1`) aren't a
    # reliable substitute: a user without HISTCONTROL=ignorespace set
    # would otherwise still have it recorded.
    case "$cmd" in
        [[:space:]]*)
            return 0
            ;;
    esac
    if [ -n "$HERDR_PANE_ID" ]; then
        smarthistory capture-herdr "$cmd" --exit-code "$exit_code" 2>/dev/null
    elif [ -n "$TMUX" ] && [ -n "$TMUX_PANE" ]; then
        local tmux_dir
        tmux_dir=$(smarthistory config get tmuxpaneoutputdir 2>/dev/null)
        if [ -z "$tmux_dir" ]; then
            tmux_dir="$HOME/.cache/tmux-history"
        fi
        local tmux_log="$tmux_dir/output-${TMUX_PANE}.log"
        if [ -f "$tmux_log" ]; then
            smarthistory capture-tmux "$cmd" "$tmux_log" --exit-code "$exit_code" 2>/dev/null
        else
            smarthistory add "$cmd" --exit-code "$exit_code"
        fi
    else
        smarthistory add "$cmd" --exit-code "$exit_code"
    fi
    _smarthistory_last_cmd="$cmd"
    _smarthistory_next_index=0
}

_smarthistory_precmd_wrapper() {
    local exit_code=$?
    if [ "$_smarthistory_cmd_ran" = "1" ]; then
        _smarthistory_cmd_ran=0
        local hist_line
        hist_line=$(HISTTIMEFORMAT= builtin history 1)
        # Strip the leading "  123  " index bash's `history` prints —
        # same stripping expression `bash-preexec` uses: up to the
        # first digit followed by two spaces.
        hist_line="${hist_line#*[[:digit:]]  }"
        if [ -n "$hist_line" ] && [ "$hist_line" != "$_smarthistory_last_history_line" ]; then
            _smarthistory_last_history_line="$hist_line"
            _smarthistory_precmd "$hist_line" "$exit_code"
        fi
    fi
}
PROMPT_COMMAND="_smarthistory_precmd_wrapper${PROMPT_COMMAND:+
$PROMPT_COMMAND}"

# ---- Interactive widgets (bash >= 4.0 only) ----
#
# `READLINE_LINE`/`READLINE_POINT` (the special variables `bind -x`
# callbacks use) were added in bash 4.0. `${BASH_VERSINFO[0]}` itself
# is safe to read on any bash back to 2.0, so this check works even
# on the ancient bash the rest of this file already tolerates.
if [ -n "$BASH_VERSINFO" ] && [ "${BASH_VERSINFO[0]}" -ge 4 ] 2>/dev/null; then

    # Same escape `smarthistory`'s CLI uses for multi-line commands in
    # its one-row-per-line output (`\n`/`\r` as literal two-char
    # escapes) — mirrors `_smarthistory_unescape` in src/init.zsh.
    _smarthistory_unescape() {
        local out=$1
        out=${out//\\n/$'\n'}
        out=${out//\\r/$'\r'}
        printf %s "$out"
    }

    # Ctrl-R: history picker via the smarthistory TUI. Unlike zsh's
    # `_smarthistory_select`, there's no way to programmatically
    # invoke "accept-line" from inside a `bind -x` callback in bash —
    # so unlike zsh (which can auto-submit on some exit codes), this
    # always just fills the line and leaves it for the user to press
    # Enter (the same behavior fzf's own bash Ctrl-R integration has).
    _smarthistory_select() {
        local selected
        selected=$(smarthistory tui 2>/dev/tty)
        if [ -n "$selected" ]; then
            READLINE_LINE="$selected"
            READLINE_POINT=${#READLINE_LINE}
        fi
    }
    bind -x '"\C-r": _smarthistory_select'

    # ---- Up/Down history-walk ----
    _smarthistory_matches=""
    _smarthistory_index=0
    _smarthistory_query_key=""
    _smarthistory_last_match=""
    _smarthistory_mode=$(smarthistory config get zsh.mode 2>/dev/null)
    case "$_smarthistory_mode" in
        sess | dir | global) ;;
        *) _smarthistory_mode="sess" ;;
    esac

    _smarthistory_reset_state() {
        _smarthistory_matches=""
        _smarthistory_index=0
        _smarthistory_query_key=""
        _smarthistory_last_match=""
    }

    _smarthistory_prime_cache() {
        if [ -n "$_smarthistory_last_match" ] && [ "$READLINE_LINE" = "$_smarthistory_last_match" ]; then
            return
        fi
        local lbuffer="${READLINE_LINE:0:READLINE_POINT}"
        local query_key="$_smarthistory_mode|$PWD|$lbuffer"
        if [ "$query_key" = "$_smarthistory_query_key" ]; then
            return
        fi
        local args=("$lbuffer" --limit 0 --no-highlight)
        case "$_smarthistory_mode" in
            sess) args+=(--session) ;;
            dir) args+=(--directory "$PWD") ;;
            global) ;;
        esac
        _smarthistory_matches=$(smarthistory search "${args[@]}" 2>/dev/null)
        _smarthistory_index=0
        _smarthistory_query_key="$query_key"
        _smarthistory_last_match=""
    }

    # Array-access note: `_smarthistory_index`'s increment/decrement
    # and boundary checks are byte-for-byte the same counter zsh's
    # `_smarthistory_up_history`/`_smarthistory_down_history` use
    # (starts 0, walked the same way) — only the final array-access
    # expression differs, `[index - 1]` here vs zsh's 1-based
    # `[index]`, since bash arrays are 0-based.
    _smarthistory_up_history() {
        _smarthistory_prime_cache
        local -a lines
        mapfile -t lines <<<"$_smarthistory_matches"
        local n=${#lines[@]}
        if [ "$n" -eq 0 ] || { [ "$n" -eq 1 ] && [ -z "${lines[0]}" ]; }; then
            return
        fi
        if [ "$_smarthistory_index" -ge "$n" ]; then
            return
        fi
        _smarthistory_index=$((_smarthistory_index + 1))
        local raw_match=${lines[$((_smarthistory_index - 1))]}
        local match
        match=$(_smarthistory_unescape "$raw_match")
        READLINE_LINE="$match"
        READLINE_POINT=${#READLINE_LINE}
        _smarthistory_last_match="$match"
    }

    _smarthistory_down_history() {
        _smarthistory_prime_cache
        local -a lines
        mapfile -t lines <<<"$_smarthistory_matches"
        local n=${#lines[@]}
        if [ "$n" -eq 0 ] || { [ "$n" -eq 1 ] && [ -z "${lines[0]}" ]; }; then
            return
        fi
        if [ "$_smarthistory_index" -le 0 ]; then
            READLINE_LINE=""
            READLINE_POINT=0
            _smarthistory_last_match=""
            return
        fi
        _smarthistory_index=$((_smarthistory_index - 1))
        # `index` can now be 0, which would make the access below
        # `lines[-1]` — bash's negative-index syntax for "last
        # element", NOT "out of range" the way zsh's 1-based `lines[0]`
        # is. zsh's boundary at this exact point relies on THAT
        # out-of-range access silently yielding an empty string
        # (reaching the explicit `index <= 0` clear above only takes
        # one MORE press) — replicate that empty-string result
        # explicitly rather than let bash's wraparound return the
        # wrong (last) element.
        local raw_match=""
        if [ "$_smarthistory_index" -gt 0 ]; then
            raw_match=${lines[$((_smarthistory_index - 1))]}
        fi
        local match
        match=$(_smarthistory_unescape "$raw_match")
        READLINE_LINE="$match"
        READLINE_POINT=${#READLINE_LINE}
        _smarthistory_last_match="$match"
    }
    bind -x '"\e[A": _smarthistory_up_history'
    bind -x '"\e[B": _smarthistory_down_history'

    # ---- Ctrl-G: cycle search scope (sess -> dir -> global -> sess) ----
    # No visual indicator (bash has no right-prompt concept the way
    # zsh's RPROMPT is) — the scope just cycles silently.
    _smarthistory_cycle_mode() {
        case "$_smarthistory_mode" in
            sess) _smarthistory_mode="dir" ;;
            dir) _smarthistory_mode="global" ;;
            global) _smarthistory_mode="sess" ;;
        esac
        _smarthistory_reset_state
    }
    bind -x '"\C-g": _smarthistory_cycle_mode'

    # ---- Ctrl-S: insert the most probable next command ----
    # `^S` is normally the terminal's XOFF flow-control character;
    # `stty -ixon` frees it for readline, same fix zsh's version needs.
    stty -ixon 2>/dev/null
    _smarthistory_next_index=0
    _smarthistory_next_history() {
        if [ -z "$_smarthistory_last_cmd" ]; then
            return
        fi
        local -a candidates
        mapfile -t candidates < <(smarthistory next "$_smarthistory_last_cmd" --limit 10 2>/dev/null | cut -f2)
        local n=${#candidates[@]}
        if [ "$n" -eq 0 ]; then
            return
        fi
        if [ "$_smarthistory_next_index" -ge "$n" ]; then
            _smarthistory_next_index=0
        fi
        local raw_match=${candidates[$_smarthistory_next_index]}
        local match
        match=$(_smarthistory_unescape "$raw_match")
        READLINE_LINE="$match"
        READLINE_POINT=${#READLINE_LINE}
        _smarthistory_next_index=$((_smarthistory_next_index + 1))
    }
    bind -x '"\C-s": _smarthistory_next_history'

    # ---- Comment-expansion (space-triggered) ----
    # Type a comment's text at the start of the line, then a space,
    # and it expands to the most recently used command carrying that
    # exact comment (`smarthistory add ... --comment ...`). Off by
    # default; enable with `commentexpand.enabled=on` in
    # ~/.config/smarthistory/config.
    #
    # Structural difference from the zsh version: zsh wraps
    # `self-insert` so the REAL space gets inserted first, then the
    # post-hook fires and can find it already in `LBUFFER`. Binding
    # the literal space key in bash via `bind -x` REPLACES self-insert
    # for that key entirely — the real space is never auto-inserted —
    # so when no expansion applies, this function must manually splice
    # it into `READLINE_LINE` itself, or the keystroke is silently
    # swallowed.
    _smarthistory_commentexpand_enabled="0"
    if [ "$(smarthistory config get commentexpand.enabled 2>/dev/null)" = "on" ]; then
        _smarthistory_commentexpand_enabled="1"
    fi
    _smarthistory_insert_space() {
        READLINE_LINE="${READLINE_LINE:0:READLINE_POINT} ${READLINE_LINE:READLINE_POINT}"
        READLINE_POINT=$((READLINE_POINT + 1))
    }
    if [ "$_smarthistory_commentexpand_enabled" = "1" ]; then
        _smarthistory_commentexpand_check() {
            # Only the first word on the line (nothing typed before
            # it) is eligible — same "written to the begin of the
            # command line" rule the zsh version enforces.
            if [ "$READLINE_POINT" -ne "${#READLINE_LINE}" ] || [ -z "$READLINE_LINE" ]; then
                _smarthistory_insert_space
                return
            fi
            case "$READLINE_LINE" in
                *[[:space:]]*)
                    _smarthistory_insert_space
                    return
                    ;;
            esac
            local resolved
            resolved=$(smarthistory expand "$READLINE_LINE" 2>/dev/null)
            if [ -n "$resolved" ]; then
                READLINE_LINE="$resolved "
                READLINE_POINT=${#READLINE_LINE}
            else
                _smarthistory_insert_space
            fi
        }
        bind -x '" ": _smarthistory_commentexpand_check'
    fi

fi

# This init script's OWN setup commands above (the `smarthistory
# config get ...`/`stty`/`bind -x` calls, even `eval` itself for the
# line that sourced this whole script) fire the DEBUG trap too, since
# it was installed partway through this same top-level script and
# DEBUG traps aren't scoped to "only user-typed commands" — they fire
# for anything at the top execution level. Without priming the
# baseline here, the very first `PROMPT_COMMAND` cycle after this
# script finishes would see `_smarthistory_cmd_ran=1` (residue from
# OUR OWN setup) and `_smarthistory_last_history_line` still empty,
# and wrongly record the `eval "$(smarthistory init bash)"` line
# itself as if the user had just run it. Snapshotting the current
# `history 1` as the baseline now — plus resetting the flag — means
# the first real PROMPT_COMMAND cycle correctly sees "nothing new"
# until the user actually types and runs something.
_smarthistory_last_history_line=$(HISTTIMEFORMAT= builtin history 1 2>/dev/null)
_smarthistory_last_history_line="${_smarthistory_last_history_line#*[[:digit:]]  }"
_smarthistory_cmd_ran=0
