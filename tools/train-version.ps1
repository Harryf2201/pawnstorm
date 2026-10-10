# Trains a network on the GPU from self-play data and rates the resulting engine version.
# Usage: .\tools\train-version.ps1 -Version v0.8 [-MaxPositions 0] [-Sources a,b] [-Anchors 2900,3100] [-Concurrency 8]
# Produces testing\engines\pawnstorm-<Version>-<size>.nnue and starts a gauntlet against the Stockfish
# anchors plus every earlier version (the dashboard rates everything jointly).
param(
    [Parameter(Mandatory)][string]$Version,
    [int64]$MaxPositions = 0,
    [string[]]$Sources = @(),
    [int[]]$Anchors = @(2900, 3100),
    [int]$Concurrency = 8,
    [int]$Rounds = 15,
    [double]$Wdl = 0.4,
    [switch]$NoGauntlet
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\paths.ps1"

# Data: every generator folder that has positions, unless given explicitly.
$total = 0
$found = @()
foreach ($d in Get-ChildItem "$LabRoot\datagen" -Directory) {
    $st = "$($d.FullName)\status.txt"
    if (-not (Test-Path $st)) { continue }
    $p = (Get-Content $st | Select-String '^positions=') -replace 'positions=', ''
    if ($p -and [int64]$p -gt 0 -and (-not $Sources -or $d.Name -in $Sources)) { $total += [int64]$p; $found += $d.Name }
}
$use = if ($MaxPositions -gt 0) { [math]::Min($MaxPositions, $total) } else { $total }
$size = if ($use -ge 1e9) { '{0:0.#}b' -f ($use / 1e9) } else { '{0:0}m' -f ($use / 1e6) }
$ver = "pawnstorm-$Version-$size"
$name = "net-$Version-$size"
"[$(Get-Date -Format HH:mm)] $ver from $use positions ($($found -join ', '))"

& "$PSScriptRoot\prepare-data.ps1" -Sources $found -Name $name -MaxPositions $use
$sb = [int][math]::Min(40, [math]::Max(2, [math]::Round($use * 25 / 100e6)))
$dir = "$LabRoot\training\$name"
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
"[$(Get-Date -Format HH:mm)] training $sb rounds"
& "$PSScriptRoot\train.ps1" -Name $name -Data $name -Superbatches $sb -Wdl $Wdl
while (-not (Select-String -Path "$dir\log.txt" -Pattern 'Total Training Time' -Quiet -ErrorAction SilentlyContinue)) {
    if (Select-String -Path "$dir\err.txt" -Pattern 'panicked' -Quiet -ErrorAction SilentlyContinue) { throw "training failed: see $dir\err.txt" }
    Start-Sleep 10
}
$net = Get-ChildItem -Recurse "$dir\checkpoints" -Filter quantised.bin | Sort-Object LastWriteTime | Select-Object -Last 1
Copy-Item $net.FullName "$LabRoot\engines\$ver.nnue" -Force
Remove-Item "$LabRoot\training\data\$name.bin" -ErrorAction SilentlyContinue
$check = & "$PSScriptRoot\uci-test.ps1" -Engine "$LabRoot\engines\pawnstorm-nnue.exe" -Commands "setoption name EvalFile value $LabRoot\engines\$ver.nnue", 'eval' -WaitMs 1500
if ($check -notmatch 'loaded network') { throw "engine could not load $ver.nnue" }
"[$(Get-Date -Format HH:mm)] network ready: $LabRoot\engines\$ver.nnue"

if (-not $NoGauntlet) {
    $previous = @('pawnstorm-v0.1', 'pawnstorm-v0.2') + @(Get-ChildItem "$LabRoot\engines\*.nnue" | Sort-Object CreationTime | ForEach-Object BaseName) |
        Where-Object { $_ -ne $ver }
    & "$PSScriptRoot\match.ps1" -Name "$ver rating ($size positions)" -Type gauntlet `
        -Engines (@($ver) + ($Anchors | ForEach-Object { "sf:$_" }) + $previous) -Rounds $Rounds -Concurrency $Concurrency -Live `
        -Notes "network trained on $size positions, $sb rounds"
}
