#!/usr/bin/env fish
# Nexterm shell integration for Fish
# Source this in config.fish: source /path/to/nexterm.fish

# Only activate inside Nexterm
if not set -q NEXTERM
    return
end

function _nexterm_osc133
    printf "\033]133;%s\007" $argv[1]
end

function _nexterm_osc1337
    printf "\033]1337;Nexterm=%s\007" $argv[1]
end

function _nexterm_prompt --on-event fish_prompt
    set -l exit_code $status

    # Report command finished
    if set -q _nexterm_executing
        _nexterm_osc133 "D;$exit_code"
        set -e _nexterm_executing
    end

    # Report CWD
    _nexterm_osc1337 "cwd;$PWD"

    # Report git state
    if git rev-parse --git-dir &>/dev/null
        set -l branch (git symbolic-ref --short HEAD 2>/dev/null; or git rev-parse --short HEAD 2>/dev/null)
        set -l status_count (git status --porcelain 2>/dev/null | wc -l | string trim)
        _nexterm_osc1337 "git;$branch;$status_count changed"
    end

    # Mark prompt start
    _nexterm_osc133 "A"
end

function _nexterm_preexec --on-event fish_preexec
    set -g _nexterm_executing 1
    _nexterm_osc133 "C"
end

function _nexterm_postprompt --on-event fish_postprompt
    _nexterm_osc133 "B"
end
