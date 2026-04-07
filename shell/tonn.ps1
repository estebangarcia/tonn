# Tonn shell integration for PowerShell
# Source this in your $PROFILE: . /path/to/tonn.ps1

# Only activate inside Tonn
if (-not $env:TONN) { return }

$Global:__TonnLastHistoryId = -1
$Global:__TonnExecuting = $false

function Global:__Tonn-Osc133 {
    param([string]$Code)
    [Console]::Write("`e]133;$Code`a")
}

function Global:__Tonn-Osc1337 {
    param([string]$Payload)
    [Console]::Write("`e]1337;Tonn=$Payload`a")
}

function Global:__Tonn-Get-LastExitCode {
    if ($? -eq $True) { return 0 }
    $LastHistoryEntry = $(Get-History -Count 1)
    if ($Error.Count -gt 0 -and $Error[0].InvocationInfo.HistoryId -eq $LastHistoryEntry.Id) {
        return -1
    }
    return $LastExitCode
}

# Override prompt function (precmd equivalent)
function prompt {
    $gle = $(__Tonn-Get-LastExitCode)
    $LastHistoryEntry = $(Get-History -Count 1)

    # Report command finished (if a command was executed)
    if ($Global:__TonnLastHistoryId -ne -1) {
        if ($LastHistoryEntry.Id -eq $Global:__TonnLastHistoryId) {
            __Tonn-Osc133 "D"
        } else {
            __Tonn-Osc133 "D;$gle"
        }
    }

    $loc = $executionContext.SessionState.Path.CurrentLocation

    # Report CWD
    __Tonn-Osc1337 "cwd;$loc"

    # Report git state
    if (Test-Path .git -ErrorAction SilentlyContinue) {
        $branch = git symbolic-ref --short HEAD 2>$null
        if (-not $branch) { $branch = git rev-parse --short HEAD 2>$null }
        $statusCount = (git status --porcelain 2>$null | Measure-Object).Count
        __Tonn-Osc1337 "git;$branch;$statusCount changed"
    }

    # Mark prompt start
    __Tonn-Osc133 "A"

    # Actual prompt text
    $out = "PS $loc> "

    # Mark command input start
    __Tonn-Osc133 "B"

    $Global:__TonnLastHistoryId = $LastHistoryEntry.Id
    return $out
}

# Preexec via PSReadLine (if available)
if (Get-Module -Name PSReadLine -ErrorAction SilentlyContinue) {
    $Global:__TonnOriginalReadLine = $Function:PSConsoleHostReadLine

    function Global:PSConsoleHostReadLine {
        $command = [Microsoft.PowerShell.PSConsoleReadLine]::ReadLine(
            $Host.Runspace, $ExecutionContext, $null)

        if ($command) {
            $Global:__TonnExecuting = $true
            __Tonn-Osc133 "C"
        }

        return $command
    }
}
