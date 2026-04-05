#!/usr/bin/env zsh
# Nexterm shell integration for Zsh
# Source this in your .zshrc: source /path/to/nexterm.zsh

# Only activate inside Nexterm
[[ -z "$NEXTERM" ]] && return

# OSC 133 sequences for shell integration (semantic prompts)
_nexterm_osc133() {
    printf "\033]133;%s\007" "$1"
}

# Custom Nexterm OSC 1337 extensions
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
        local status=$(git status --porcelain 2>/dev/null | wc -l | tr -d ' ')
        _nexterm_osc1337 "git;$branch;$status changed"
    fi

    # Mark prompt start
    _nexterm_osc133 "A"
}

# preexec: runs before each command execution
_nexterm_preexec() {
    _nexterm_executing=1
    # Mark execution start
    _nexterm_osc133 "C"
}

# Wrap PS1 to add command input marker after prompt
_nexterm_set_prompt() {
    PS1="%{$(_nexterm_osc133 B)%}$PS1"
}

# Install hooks
autoload -Uz add-zsh-hook
add-zsh-hook precmd _nexterm_precmd
add-zsh-hook preexec _nexterm_preexec
add-zsh-hook precmd _nexterm_set_prompt
