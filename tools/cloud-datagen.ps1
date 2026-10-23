# Starts one batch of cloud data generation on GitHub Actions (see .github/workflows/datagen.yml).
# Refuses to start if a batch is already running or queued, so only one batch is ever in flight.
# Usage: .\tools\cloud-datagen.ps1 [-Jobs 0] [-Minutes 330]
# -Jobs 0 = auto: the workflow picks 20 when no test match is running or queued, else 14 (6 runners stay free for a test).
param(
    [ValidateRange(0, 20)][int]$Jobs = 0,
    [ValidateRange(10, 340)][int]$Minutes = 330,
    [ValidateRange(1, 1000000000)][int]$SoftNodes = 5000,
    [ValidateSet(768,1024)][int]$NnueWidth = 768,
    [ValidateRange(0, 2000000000)][int]$HardNodes = 0,
    [ValidateRange(1, 1000000)][int]$KeepEvery = 1,
    [ValidateRange(0, 400)][int]$MaxPerGame = 0,
    [ValidateRange(0, 1000000)][int]$DupCap = 0,
    # Follow-up batches GitHub starts by itself after this one (e.g. -Chain 1 = two batches back to back).
    [ValidateRange(0, 3)][int]$Chain = 0,
    # classical = gen1 data (hand-crafted eval); nnue = gen2 data from the network in nets\default.bin
    [ValidateSet('classical', 'nnue')][string]$Eval = 'classical'
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
    # Parentheses make PowerShell 5 enumerate the JSON array instead of passing it as one object.
    $active = (gh run list --workflow datagen.yml --limit 20 --json databaseId,status | ConvertFrom-Json) |
        Where-Object { $_.status -in 'queued', 'in_progress', 'waiting', 'pending', 'requested' }
    if ($active) {
        "A cloud batch is already in progress (run $($active[0].databaseId)). Not starting another."
        return
    }
    $before = @(gh run list --workflow datagen.yml --limit 1 --json databaseId | ConvertFrom-Json).databaseId
    gh workflow run datagen.yml -f jobs=$(if ($Jobs) { $Jobs } else { 'auto' }) -f minutes=$Minutes -f soft_nodes=$SoftNodes -f chain=$Chain -f eval=$Eval -f nnue_width=$NnueWidth -f hard_nodes=$HardNodes -f keep_every=$KeepEvery -f max_per_game=$MaxPerGame -f dup_cap=$DupCap
    if ($LASTEXITCODE -ne 0) { throw "GitHub refused to start the workflow (see the message above)." }
    # Wait for the new run to appear.
    $run = $null
    for ($i = 0; $i -lt 20 -and -not $run; $i++) {
        Start-Sleep 3
        $latest = gh run list --workflow datagen.yml --limit 1 --json databaseId,url | ConvertFrom-Json
        if ($latest -and $latest.databaseId -ne $before) { $run = $latest }
    }
    if (-not $run) { throw "Started the workflow but couldn't find its run yet; check the Actions tab." }
    $dir = "$LabRoot\datagen\cloud-$($run.databaseId)"
    New-Item -ItemType Directory -Force $dir | Out-Null
    [IO.File]::WriteAllLines("$dir\launch.txt", [string[]]@("jobs=$Jobs", "minutes=$Minutes", "soft_nodes=$SoftNodes", "nnue_width=$NnueWidth", "hard_nodes=$HardNodes", "keep_every=$KeepEvery", "max_per_game=$MaxPerGame", "dup_cap=$DupCap", "eval=$Eval", "chain=$Chain"))
    "Started cloud batch: $Jobs jobs x $Minutes min$(if ($Chain) { ", then $Chain more batch(es) back to back" }) -> $($run.url)"
    "The dashboard shows it once tools\cloud-sync.ps1 is running (it downloads data as jobs finish)."
} finally {
    Pop-Location
}
