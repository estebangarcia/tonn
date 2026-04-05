#!/usr/bin/env zsh
# Nexterm shell integration for Zsh
# Auto-injected by Nexterm — do not source manually.

# Only activate inside Nexterm
[[ -z "$NEXTERM" ]] && return

_nexterm_osc133() {
    printf "\033]133;%s\007" "$1"
}

_nexterm_osc1337() {
    printf "\033]1337;Nexterm=%s\007" "$1"
}

# precmd: runs before each prompt
_nexterm_precmd() {
    local exit_code=$?

    # Report command finished (if a command was executed)
    if [[ -n "$_nexterm_executing" ]]; then
        _nexterm_osc133 "D;$exit_code"
        unset _nexterm_executing
    fi

    # Report CWD
    _nexterm_osc1337 "cwd;$PWD"

    # Report git state if in a repo
    if git rev-parse --git-dir &>/dev/null 2>&1; then
        local branch=$(git symbolic-ref --short HEAD 2>/dev/null || git rev-parse --short HEAD 2>/dev/null)
        local changed_count=$(git status --porcelain 2>/dev/null | wc -l | tr -d ' ')
        _nexterm_osc1337 "git;$branch;$changed_count changed"
    fi

    # Mark prompt start + end (A then B back-to-back).
    # We don't modify PS1 — that breaks prompt managers like Powerlevel10k.
    # The block model mainly needs C and D for command output boundaries.
    _nexterm_osc133 "A"
    _nexterm_osc133 "B"
}

# preexec: runs before each command execution
_nexterm_preexec() {
    _nexterm_executing=1
    _nexterm_osc133 "C"
}

# Install hooks
autoload -Uz add-zsh-hook
add-zsh-hook precmd _nexterm_precmd
add-zsh-hook preexec _nexterm_preexec
