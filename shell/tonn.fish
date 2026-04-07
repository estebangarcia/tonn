#!/usr/bin/env fish
# Tonn shell integration for Fish
# Source this in config.fish: source /path/to/tonn.fish

# Only activate inside Tonn
if not set -q TONN
    return
end

function _tonn_osc133
    printf "\033]133;%s\007" $argv[1]
end

function _tonn_osc1337
    printf "\033]1337;Tonn=%s\007" $argv[1]
end

function _tonn_prompt --on-event fish_prompt
    set -l exit_code $status

    # Report command finished
    if set -q _tonn_executing
        _tonn_osc133 "D;$exit_code"
        set -e _tonn_executing
    end

    # Report CWD
    _tonn_osc1337 "cwd;$PWD"

    # Report git state
    if git rev-parse --git-dir &>/dev/null
        set -l branch (git symbolic-ref --short HEAD 2>/dev/null; or git rev-parse --short HEAD 2>/dev/null)
        set -l status_count (git status --porcelain 2>/dev/null | wc -l | string trim)
        _tonn_osc1337 "git;$branch;$status_count changed"
    end

    # Mark prompt start
    _tonn_osc133 "A"
end

function _tonn_preexec --on-event fish_preexec
    set -g _tonn_executing 1
    _tonn_osc133 "C"
end

function _tonn_postprompt --on-event fish_postprompt
    _tonn_osc133 "B"
end
