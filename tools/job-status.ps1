# Lets an automatic job (a script that trains and tests versions on a schedule) tell the dashboard
# what it is doing: testing\training\jobs\<id>.txt, shown under "Automatic jobs" in the Lab tab.
# Usage (dot-source, then call as often as you like; the dashboard treats a job as alive while
# the file is fresher than 5 minutes, or for good once its state is "done"):
#   . "$PSScriptRoot\job-status.ps1"
#   Set-JobStatus -Id v1.3 -Title 'Train and rate v1.3' -State waiting -Detail '...' -Start (Get-Date '14:30')
function Set-JobStatus {
    param(
        [Parameter(Mandatory)][string]$Id,
        [Parameter(Mandatory)][string]$Title,
        [ValidateSet('waiting', 'preparing', 'training', 'testing', 'done', 'failed')][string]$State = 'waiting',
        [string]$Detail = '',
        [Nullable[datetime]]$Start = $null
    )
    $root = Join-Path (Split-Path $PSScriptRoot -Parent | Split-Path -Parent) 'testing\training\jobs'
    New-Item -ItemType Directory -Force $root | Out-Null
    $unix = { param($d) [DateTimeOffset]::new($d).ToUnixTimeSeconds() }
    $lines = @("title=$Title", "state=$State", "detail=$($Detail -replace '[\r\n]+', ' ')",
        "start=$(if ($Start) { & $unix $Start } else { 0 })", "updated=$(& $unix (Get-Date))")
    [IO.File]::WriteAllLines("$root\$Id.tmp", [string[]]$lines)
    Move-Item -Force "$root\$Id.tmp" "$root\$Id.txt"
}
