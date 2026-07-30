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
# Cycled with Ctrl-g.
_smarthistory_mode="sess"

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
# design rationale (why POSTDISPLAY, why no color in v1, why no
# debounce).
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
# Whether the menu is currently drawn, the 0-based highlighted row,
# and the raw (still `\n`/`\r`-escaped, per the CLI's one-line-per-row
# convention) candidate commands for the current render.
typeset -g _smarthistory_dropdown_visible=0
typeset -g _smarthistory_dropdown_selected=0
typeset -ga _smarthistory_dropdown_candidates

# Clear the menu (if any) and mark it not visible. Safe to call
# unconditionally (e.g. from precmd) even when nothing is showing.
_smarthistory_dropdown_clear() {
    [[ -n "$POSTDISPLAY" ]] && POSTDISPLAY=""
    _smarthistory_dropdown_visible=0
}

# Redraw POSTDISPLAY from the current `_smarthistory_dropdown_candidates`
# / `_smarthistory_dropdown_selected` state, WITHOUT re-querying the DB
# (used by Up/Down to move the selection, and after any state change
# that doesn't change the candidate set).
_smarthistory_dropdown_paint() {
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
    for raw in "${_smarthistory_dropdown_candidates[@]}"; do
        (( i >= max_rows )) && break
        c=$(_smarthistory_unescape "$raw")
        # A multiline command would otherwise break the one-row-per-
        # candidate layout; show the visible-newline marker instead,
        # same convention the Rust TUI list uses for the same reason.
        c=${c//$'\n'/↵}
        c=${c//$'\r'/}
        if (( i == _smarthistory_dropdown_selected )); then
            marker="❯ "
        else
            marker="  "
        fi
        row="${marker}${c}"
        if (( ${#row} > interior_max )); then
            row="${row[1,$((interior_max-1))]}…"
        fi
        rows+=("$row")
        i=$((i+1))
    done
    if (( ${#rows} == 0 )); then
        POSTDISPLAY=""
        return
    fi
    # Box width = the widest row actually produced this render (not
    # the full interior_max budget) — every row pads to this exact
    # width so the right border lines up, and the box visibly shrinks
    # when the candidate set does, same as the un-boxed layout did.
    # `─`/`│`/`╭╮╰╯` are plain printable Unicode characters, not ANSI
    # escape codes — POSTDISPLAY's width math (and zle's own
    # redraw-diffing) handles them like any other text, unlike color
    # SGR codes, which is why this part carries no open safety
    # question the way color does.
    local width=0
    for row in "${rows[@]}"; do
        (( ${#row} > width )) && width=${#row}
    done
    # `${(l:width::─:)}` pads an empty string to `width` columns using
    # `─` as the fill character — i.e. `width` dashes, built by zsh's
    # own padding expansion rather than a manual loop.
    local hr="${(l:width::─:)}"
    local out=$'\n'"╭─${hr}─╮"
    # No color is available (POSTDISPLAY doesn't interpret ANSI SGR
    # codes — see the module doc comment), so the selected row is set
    # apart with a thicker `┃` side border instead of the plain `│`
    # every other row gets — still just plain printable Unicode
    # characters, same safety as the box itself.
    local side
    for (( i = 0; i < ${#rows}; i++ )); do
        if (( i == _smarthistory_dropdown_selected )); then
            side="┃"
        else
            side="│"
        fi
        # `${(r:width:: :)row}` left-justifies `row` and pads it with
        # spaces on the right to exactly `width` columns.
        out+=$'\n'"${side} ${(r:width:: :)rows[$((i+1))]} ${side}"
    done
    out+=$'\n'"╰─${hr}─╯"
    POSTDISPLAY="$out"
}

# Re-query smarthistory for the current LBUFFER and redraw. Called
# after every keystroke (via the wrapped self-insert/delete/paste
# widgets below) when the dropdown is enabled.
_smarthistory_dropdown_render() {
    [[ "$_smarthistory_dropdown_enabled" = "1" ]] || return
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
    args=("$LBUFFER" --limit "$_smarthistory_dropdown_limit" --no-highlight --prefix)
    case "$_smarthistory_mode" in
        sess)   args+=(--session) ;;
        dir)    args+=(--directory "$PWD") ;;
        global) ;;
    esac
    local raw
    raw=$(smarthistory search "${args[@]}" 2>/dev/null)
    _smarthistory_dropdown_candidates=("${(f)raw}")
    # `${(f)raw}` on an empty string yields one empty element, not
    # zero — drop it so "no matches" is correctly detected below.
    if (( ${#_smarthistory_dropdown_candidates} == 1 )) && [[ -z "${_smarthistory_dropdown_candidates[1]}" ]]; then
        _smarthistory_dropdown_candidates=()
    fi
    if (( ${#_smarthistory_dropdown_candidates} == 0 )); then
        _smarthistory_dropdown_clear
        return
    fi
    _smarthistory_dropdown_visible=1
    if (( _smarthistory_dropdown_selected >= ${#_smarthistory_dropdown_candidates} )); then
        _smarthistory_dropdown_selected=0
    fi
    _smarthistory_dropdown_paint
}

# Wrap (not replace) a keystroke-handling widget so whatever was
# bound before still runs, then re-render the dropdown after it.
# `.widget` is only valid as an argument TO `zle` (to call a builtin
# bypassing overrides), not as a `zle -N` target directly — a plain
# `zle -N $orig .$widget` fails with "No such shell function". The
# fix (the exact pattern zsh-autosuggestions uses in src/bind.zsh):
# define a tiny wrapper function whose body does the dot-call, then
# register THAT function as the widget.
_smarthistory_dropdown_bind_widget() {
    local widget=$1
    local orig="_smarthistory_dropdown_orig_${widget}"
    case ${widgets[$widget]:-} in
        user:_smarthistory_dropdown_wrap_*) return ;;  # already wrapped (re-sourced init.zsh)
        builtin)
            eval "${orig}() { zle .${widget} }"
            zle -N $orig $orig
            ;;
        user:*)
            zle -N $orig ${widgets[$widget]#user:}
            ;;
        *) return ;;
    esac
    eval "_smarthistory_dropdown_wrap_${widget}() { zle $orig -- \"\$@\"; _smarthistory_dropdown_render; }"
    zle -N $widget _smarthistory_dropdown_wrap_${widget}
}
if [[ "$_smarthistory_dropdown_enabled" = "1" ]]; then
    for _smarthistory_dropdown_w in self-insert self-insert-unmeta \
        backward-delete-char delete-char backward-kill-word \
        kill-whole-line bracketed-paste; do
        _smarthistory_dropdown_bind_widget $_smarthistory_dropdown_w
    done
    unset _smarthistory_dropdown_w
    # Tab cycles the highlighted candidate forward (same wraparound
    # math as Down) WITHOUT touching BUFFER or closing the menu —
    # only Enter (see `_smarthistory_reset_and_accept` below) commits
    # the highlighted candidate. Falls through to the normal
    # completion widget when no menu is showing (zsh's documented
    # default Tab binding in emacs mode, preserved explicitly since
    # we're taking over `^I`).
    _smarthistory_dropdown_accept() {
        if [[ $_smarthistory_dropdown_visible -eq 1 ]]; then
            _smarthistory_dropdown_selected=$(( (_smarthistory_dropdown_selected + 1) % ${#_smarthistory_dropdown_candidates} ))
            _smarthistory_dropdown_paint
            return
        fi
        zle expand-or-complete
    }
    zle -N _smarthistory_dropdown_accept
    bindkey '^I' _smarthistory_dropdown_accept
    # Esc dismisses the menu without touching BUFFER. Nothing was
    # bound to bare Esc before this feature existed, so the "menu
    # not visible" branch is a genuine no-op (preserves prior
    # behavior exactly).
    _smarthistory_dropdown_dismiss() {
        if [[ $_smarthistory_dropdown_visible -eq 1 ]]; then
            _smarthistory_dropdown_clear
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
            _smarthistory_dropdown_selected=$(( (_smarthistory_dropdown_selected - 1 + ${#_smarthistory_dropdown_candidates}) % ${#_smarthistory_dropdown_candidates} ))
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
    # When the live dropdown is showing, Up/Down move the highlighted
    # row instead of walking the (separate, keypress-only) Up/Down
    # history cache below — pure index arithmetic against the
    # already-fetched candidate array, no subprocess call. Everything
    # below this branch is completely unchanged from before the
    # dropdown feature existed.
    if [[ "$_smarthistory_dropdown_enabled" = "1" && $_smarthistory_dropdown_visible -eq 1 ]]; then
        _smarthistory_dropdown_selected=$(( (_smarthistory_dropdown_selected - 1 + ${#_smarthistory_dropdown_candidates}) % ${#_smarthistory_dropdown_candidates} ))
        _smarthistory_dropdown_paint
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
    # Same dropdown-navigation branch as `_smarthistory_up_history`
    # above — see its comment for the rationale.
    if [[ "$_smarthistory_dropdown_enabled" = "1" && $_smarthistory_dropdown_visible -eq 1 ]]; then
        _smarthistory_dropdown_selected=$(( (_smarthistory_dropdown_selected + 1) % ${#_smarthistory_dropdown_candidates} ))
        _smarthistory_dropdown_paint
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
    # If the live dropdown is showing, Enter commits whatever's
    # currently highlighted (Tab only cycles the selection — see
    # `_smarthistory_dropdown_accept` — so this is the one place the
    # highlighted candidate actually lands in BUFFER). Must read the
    # candidate BEFORE `_smarthistory_reset_state` clears the
    # dropdown state below.
    if [[ "$_smarthistory_dropdown_enabled" = "1" && $_smarthistory_dropdown_visible -eq 1 ]]; then
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
