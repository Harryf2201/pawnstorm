# Starts one batch of cloud data generation on GitHub Actions (see .github/workflows/datagen.yml).
# Refuses to start if a batch is already running or queued, so only one batch is ever in flight.
# Usage: .\tools\cloud-datagen.ps1 [-Jobs 20] [-Minutes 330]
param(
    [ValidateRange(1, 20)][int]$Jobs = 20,
    [ValidateRange(10, 340)][int]$Minutes = 330,
    [int]$SoftNodes = 5000
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\paths.ps1"
if (Test-Path "$LabRoot\datagen\cloud-STOPPED.txt") {
    "GitHub reported a problem earlier, so cloud batches are on hold:"
    Get-Content "$LabRoot\datagen\cloud-STOPPED.txt"
    "Delete $LabRoot\datagen\cloud-STOPPED.txt once it's resolved."
    return
}
Push-Location $RepoRoot
try {
    $active = gh run list --workflow datagen.yml --limit 20 --json databaseId,status | ConvertFrom-Json |
        Where-Object { $_.status -in 'queued', 'in_progress', 'waiting', 'pending', 'requested' }
    if ($active) {
        "A cloud batch is already in progress (run $($active[0].databaseId)). Not starting another."
        return
    }
    gh workflow run datagen.yml -f jobs=$Jobs -f minutes=$Minutes -f soft_nodes=$SoftNodes
    Start-Sleep 5
    $run = gh run list --workflow datagen.yml --limit 1 --json databaseId,url | ConvertFrom-Json
    $dir = "$LabRoot\datagen\cloud-$($run.databaseId)"
    New-Item -ItemType Directory -Force $dir | Out-Null
    [IO.File]::WriteAllLines("$dir\launch.txt", [string[]]@("jobs=$Jobs", "minutes=$Minutes", "soft_nodes=$SoftNodes"))
    "Started cloud batch: $Jobs jobs x $Minutes min -> $($run.url)"
    "The dashboard shows it once tools\cloud-sync.ps1 is running (it downloads data as jobs finish)."
} finally {
    Pop-Location
}
