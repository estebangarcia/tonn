#!/usr/bin/env bash
# Nexterm shell integration for Bash
# Source this in your .bashrc: source /path/to/nexterm.bash

# Only activate inside Nexterm
[[ -z "$NEXTERM" ]] && return

_nexterm_osc133() {
    printf "\033]133;%s\007" "$1"
}

_nexterm_osc1337() {
    printf "\033]1337;Nexterm=%s\007" "$1"
}

_nexterm_prompt_command() {
    local exit_code=$?

    # Report command finished
    if [[ -n "$_nexterm_executing" ]]; then
        _nexterm_osc133 "D;$exit_code"
        unset _nexterm_executing
    fi

    # Report CWD
    _nexterm_osc1337 "cwd;$PWD"

    # Report git state
    if git rev-parse --git-dir &>/dev/null 2>&1; then
        local branch=$(git symbolic-ref --short HEAD 2>/dev/null || git rev-parse --short HEAD 2>/dev/null)
        local status=$(git status --porcelain 2>/dev/null | wc -l | tr -d ' ')
        _nexterm_osc1337 "git;$branch;$status changed"
    fi

    # Mark prompt start
    _nexterm_osc133 "A"
}

# Pre-exec via DEBUG trap (avoids subshell so _nexterm_executing persists)
_nexterm_preexec() {
    if [[ -n "$COMP_LINE" ]]; then
        return
    fi
    # Avoid firing during prompt rendering itself
    if [[ "$BASH_COMMAND" == "_nexterm_prompt_command" ]]; then
        return
    fi
    _nexterm_executing=1
    _nexterm_osc133 "C"
}
trap '_nexterm_preexec' DEBUG

# Install
PROMPT_COMMAND="_nexterm_prompt_command${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
# Append command input marker after PS1
PS1="$PS1\[$(_nexterm_osc133 B)\]"
