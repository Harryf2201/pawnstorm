# Stops everything cleanly so the PC can be shut down. Nothing is lost: data generation resumes
# where it stopped, finished games are kept, and any cloud batch keeps running on GitHub.
# Usage: .\tools\lab-stop.ps1               stop now (a tournament in progress is cut short)
#        .\tools\lab-stop.ps1 -StartCloud   also start a cloud batch first, to run while the PC is off
#        .\tools\lab-stop.ps1 -WaitForTournaments   let running tournaments finish first
#        -KeepDashboard   leave the dashboard running (used by the dashboard's own Stop button)
param([switch]$StartCloud, [switch]$WaitForTournaments, [switch]$KeepDashboard)

. "$PSScriptRoot\paths.ps1"

function Stop-Script($name) {
    Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -like "*$name*" } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue; "  stopped $name" }
}

# Kill a process and everything it started (fastchess -> engine processes).
function Stop-Tree($procId) {
    Get-CimInstance Win32_Process -Filter "ParentProcessId=$procId" | ForEach-Object { Stop-Tree $_.ProcessId }
    Stop-Process -Id $procId -Force -ErrorAction SilentlyContinue
}

"Stopping Pawnstorm Lab..."

if ($StartCloud) {
    "  starting a cloud batch to run while the PC is off:"
    & "$PSScriptRoot\cloud-datagen.ps1" | ForEach-Object { "    $_" }
}

# 1. No new training or tournaments from here on.
Stop-Script 'auto-train.ps1'
Stop-Script 'datagen-adapt.ps1'

# 2. Tournaments.
$fc = @(Get-Process fastchess -ErrorAction SilentlyContinue)
if ($fc -and $WaitForTournaments) {
    "  waiting for $($fc.Count) tournament(s) to finish..."
    while (Get-Process fastchess -ErrorAction SilentlyContinue) { Start-Sleep 10 }
} elseif ($fc) {
    foreach ($p in $fc) { Stop-Tree $p.Id }
    "  stopped $($fc.Count) tournament(s); finished games are kept and still count towards ratings"
}

# 3. GPU training takes a few minutes at most: let it finish so the network isn't wasted.
if (Get-Process trainer -ErrorAction SilentlyContinue) {
    "  waiting for GPU training to finish (usually under 10 minutes)..."
    $t0 = Get-Date
    while ((Get-Process trainer -ErrorAction SilentlyContinue) -and ((Get-Date) - $t0).TotalMinutes -lt 15) { Start-Sleep 10 }
    Get-Process trainer -ErrorAction SilentlyContinue | Stop-Process -Force
}

# 4. Local data generation (resumes from the same point next time).
$dg = @(Get-Process pawnstorm-datagen* -ErrorAction SilentlyContinue)
if ($dg) {
    $dg | Stop-Process -Force
    foreach ($d in Get-ChildItem "$LabRoot\datagen" -Directory) {
        $st = "$($d.FullName)\status.txt"
        if ((Test-Path $st) -and (Select-String -Path $st -Pattern '^state=running' -Quiet) -and -not (Select-String -Path $st -Pattern '^source=cloud' -Quiet)) {
            (Get-Content $st) -replace '^state=running', 'state=stopped' | Set-Content $st
        }
    }
    "  stopped local data generation (it will resume where it left off)"
}

# 5. Background helpers and the dashboard.
Stop-Script 'cloud-sync.ps1'
Stop-Script 'oracle-sync.ps1'   # the Oracle server keeps generating; its data waits there for the next sync
Stop-Script 'lichess-sync.ps1'
Stop-Script 'watchdog.ps1'
# Clear the heartbeat files at once so the dashboard shows the lab as stopped immediately.
Remove-Item "$LabRoot\health\status.txt", "$LabRoot\datagen\cloud-sync.heartbeat", "$LabRoot\datagen\oracle-sync.heartbeat", "$LabRoot\training\auto-train.heartbeat" -ErrorAction SilentlyContinue
if (-not $KeepDashboard) { Get-Process dashboard -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue }

$cloud = $null
try {
    Push-Location $RepoRoot
    $cloud = (gh run list --workflow datagen.yml --limit 5 --json status,databaseId | ConvertFrom-Json) | Where-Object { $_.status -ne 'completed' } | Select-Object -First 1
    Pop-Location
} catch {}
""
if ($cloud) { "A cloud batch (run $($cloud.databaseId)) is still running on GitHub; its data downloads automatically next time you start the lab." }
"All stopped. It's safe to shut down. Start again with the 'Pawnstorm Lab - Start' shortcut."
