# Starts everything for a session: sensors, dashboard, watchdog, cloud sync, auto-training and
# local data generation (resuming where it left off). Anything already running is left alone.
# Usage: .\tools\lab-start.ps1            (or the "Pawnstorm Lab - Start" desktop shortcut)
# Local data generation: gen3 = self-play with the v1.3 network (gen2-v08-5k used v0.8's, gen1-pesto-5k the hand-crafted eval).
param([int]$DatagenThreads = 18, [int64]$DatagenTarget = 1000000000, [string]$DatagenName = 'gen3-v13-5k', [string]$DatagenExe = 'pawnstorm-datagen-gen3', [switch]$NoBrowser)

. "$PSScriptRoot\paths.ps1"
$ps = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-WindowStyle', 'Hidden', '-File')

function Running-Script($name) {
    @(Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -like "*$name*" }).Count -gt 0
}
function Start-Hidden($script, [string[]]$extra = @()) {
    if (Running-Script $script) { "  $script already running"; return }
    Start-Process powershell -WindowStyle Hidden -ArgumentList ($ps + "$PSScriptRoot\$script" + $extra) | Out-Null
    "  started $script"
}

"Starting Pawnstorm Lab..."

# 1. CPU temperature sensors (needs admin: Windows will ask once).
if (-not (Get-Process LibreHardwareMonitor -ErrorAction SilentlyContinue)) {
    $lhm = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Recurse -Filter LibreHardwareMonitor.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($lhm) {
        "  starting LibreHardwareMonitor (approve the admin prompt)"
        try { Start-Process $lhm.FullName -Verb RunAs } catch { "  !! LibreHardwareMonitor not started: $($_.Exception.Message)" }
        Start-Sleep 8
    } else { "  !! LibreHardwareMonitor not found; CPU temperatures won't be monitored" }
} else { "  LibreHardwareMonitor already running" }

# 2. Safety watchdog before any heavy work.
Start-Hidden 'watchdog.ps1'

# 3. Dashboard.
if (-not (Get-Process dashboard -ErrorAction SilentlyContinue)) {
    Start-Process "$RepoRoot\target\release\dashboard.exe" -ArgumentList $LabRoot -WindowStyle Hidden
    "  started dashboard"
} else { "  dashboard already running" }

# 4. Cloud sync: downloads any cloud batches that finished while the PC was off.
Start-Hidden 'cloud-sync.ps1' @('-IntervalSeconds', '120')
# Data from the always-on Oracle Cloud server (configured in tools\engines.local.ps1, if any).
if ($OracleHost) { Start-Hidden 'oracle-sync.ps1' }

# 5. Local self-play data generation (resumes counts and files).
if (-not (Get-Process pawnstorm-datagen* -ErrorAction SilentlyContinue)) {
    $dir = "$LabRoot\datagen\$DatagenName"
    $have = 0
    if (Test-Path "$dir\status.txt") { $have = [int64](((Get-Content "$dir\status.txt" | Select-String '^positions=') -replace 'positions=', '')) }
    if ($have -lt $DatagenTarget) {
        $p = Start-Process "$LabRoot\engines\$DatagenExe.exe" -ArgumentList 'datagen', $DatagenThreads, $DatagenTarget, 5000, $dir `
            -RedirectStandardOutput "$dir\stdout.txt" -RedirectStandardError "$dir\stderr.txt" -WindowStyle Hidden -PassThru
        Start-Sleep 1
        $p.PriorityClass = 'BelowNormal'
        "  resumed local data generation at $([math]::Round($have / 1e6, 1))M positions"
    } else { "  local data generation already reached its target" }
} else { "  local data generation already running" }

# 6. Keep datagen sized to the load (few threads during tournaments, many when idle).
#    (auto-train.ps1, the old milestone scheduler, is retired: versions are now trained deliberately
#    with train-version.ps1.)
Start-Hidden 'datagen-adapt.ps1'

Start-Sleep 2
if (-not $NoBrowser) { Start-Process 'http://localhost:8000' }
"All started. Dashboard: http://localhost:8000"
