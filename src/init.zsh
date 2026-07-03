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
    # Skip empty command lines (e.g. bare Enter presses).
    [ -n "$_smarthistory_cmd" ] || return 0
    # When running inside a tmux session, the full pane is mirrored to
    # ~/.cache/tmux-history/output-${TMUX_PANE}.log. If that file
    # exists, use `smarthistory capture-tmux` to grab the command line
    # and the following output (up to 20 lines) automatically. This
    # avoids an explicit `smarthistory capture <cmd>` call.
    if [ -n "$TMUX" ] && [ -n "$TMUX_PANE" ]; then
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
    # next press starts with the most probable candidate.
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

_smarthistory_reset_state() {
    _smarthistory_matches=""
    _smarthistory_index=0
    _smarthistory_query_key=""
    _smarthistory_last_match=""
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
        RPROMPT="$label $_smarthistory_rprompt_save"
    else
        RPROMPT="$label"
    fi
    # Force a redraw only if ZLE is active (i.e. we're in a widget).
    # During the first precmd (before ZLE is fully initialized),
    # zle reset-prompt would error; the next prompt will pick up
    # the new RPROMPT automatically.
    zle reset-prompt 2>/dev/null
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
