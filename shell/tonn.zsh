#!/usr/bin/env zsh
# Tonn shell integration for Zsh
# Auto-injected by Tonn — do not source manually.

# Only activate inside Tonn
[[ -z "$TONN" ]] && return

_tonn_osc133() {
    printf "\033]133;%s\007" "$1"
}

_tonn_osc1337() {
    printf "\033]1337;Tonn=%s\007" "$1"
}

# precmd: runs before each prompt
_tonn_precmd() {
    local exit_code=$?

    # Report command finished (if a command was executed)
    if [[ -n "$_tonn_executing" ]]; then
        _tonn_osc133 "D;$exit_code"
        unset _tonn_executing
    fi

    # Report CWD
    _tonn_osc1337 "cwd;$PWD"

    # Report git state if in a repo
    if git rev-parse --git-dir &>/dev/null 2>&1; then
        local branch=$(git symbolic-ref --short HEAD 2>/dev/null || git rev-parse --short HEAD 2>/dev/null)
        local changed_count=$(git status --porcelain 2>/dev/null | wc -l | tr -d ' ')
        _tonn_osc1337 "git;$branch;$changed_count changed"
    fi

    # Mark prompt start + end (A then B back-to-back).
    # We don't modify PS1 — that breaks prompt managers like Powerlevel10k.
    # The block model mainly needs C and D for command output boundaries.
    _tonn_osc133 "A"
    _tonn_osc133 "B"
}

# preexec: runs before each command execution
_tonn_preexec() {
    _tonn_executing=1
    _tonn_osc133 "C"
}

# Install hooks
autoload -Uz add-zsh-hook
add-zsh-hook precmd _tonn_precmd
add-zsh-hook preexec _tonn_preexec
