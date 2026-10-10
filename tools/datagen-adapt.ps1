# Keeps local self-play data generation sized to what else is running: few threads while
# tournaments play (they need the CPU and the heat budget), many when the CPU is free.
# Restarts are cheap: datagen resumes its counts and files exactly where it stopped.
# The watchdog still pauses everything if temperatures get too high.
# Usage (hidden, background): .\tools\datagen-adapt.ps1 [-Busy 6] [-Idle 16]
param(
    [int]$Busy = 6,
    [int]$Idle = 16,
    [string]$Name = 'gen2-v08-5k',
    [string]$Exe = 'pawnstorm-datagen-gen2',
    [int64]$Target = 1000000000
)
. "$PSScriptRoot\paths.ps1"
$dir = "$LabRoot\datagen\$Name"
$log = "$LabRoot\datagen\adapt.log"
function Log($m) { Add-Content $log "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') $m" -Encoding utf8 }

function Start-Datagen($threads) {
    Get-Process pawnstorm-datagen* -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep 2
    $p = Start-Process "$LabRoot\engines\$Exe.exe" -ArgumentList 'datagen', $threads, $Target, 5000, $dir `
        -RedirectStandardOutput "$dir\stdout.txt" -RedirectStandardError "$dir\stderr.txt" -WindowStyle Hidden -PassThru
    Start-Sleep 1
    $p.PriorityClass = 'BelowNormal'
    Log "datagen restarted with $threads threads"
}

Log 'datagen-adapt started'
$idleSince = $null
while ($true) {
    Set-Content "$LabRoot\datagen\adapt.heartbeat" (Get-Date -Format o)
    # (PowerShell names are case-insensitive, so this must not be called $busy: it would overwrite $Busy.)
    $tournament = [bool](Get-Process fastchess -ErrorAction SilentlyContinue)
    $current = 0
    if (Test-Path "$dir\status.txt") { $current = [int](((Get-Content "$dir\status.txt" | Select-String '^threads=') -replace 'threads=', '')) }
    $running = [bool](Get-Process pawnstorm-datagen* -ErrorAction SilentlyContinue)
    if ($tournament) {
        $idleSince = $null
        if (-not $running -or $current -ne $Busy) { Start-Datagen $Busy }
    } else {
        # Wait 2 minutes of no tournaments before scaling up (gauntlets often start back to back).
        if (-not $idleSince) { $idleSince = Get-Date }
        if (((Get-Date) - $idleSince).TotalSeconds -ge 120 -and (-not $running -or $current -ne $Idle)) { Start-Datagen $Idle }
    }
    Start-Sleep 30
}
