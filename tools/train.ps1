# Starts a GPU training run in the background, logging where the dashboard can see it.
# Usage: .\tools\train.ps1 -Name net1 -Data gen1 [-Superbatches 40] [-Wdl 0.4] [-Inputs 768|kb10|kb4]
#   -Inputs kb10 = 10 king buckets, mirrored (needs an engine built from the kb branch).
param(
    [Parameter(Mandatory)][string]$Name,
    [Parameter(Mandatory)][string]$Data,
    [int]$Superbatches = 40,
    [string]$Wdl = '0.4',   # constant, or 'start-end' to rise linearly over training
    [int]$Hidden = 512,
    [int]$Buckets = 1,
    [ValidateSet('768', 'kb10', 'kb4', 'ml', 'ml32', 'mlkb4')][string]$Inputs = '768',
    [switch]$Build
)

# The driver supports CUDA 13.3, so pin the toolkit (compile-time) and its DLLs (run-time) to it.
$cuda = 'C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.3'
$env:CUDA_PATH = $cuda
$env:Path = "$cuda\bin\x64;$cuda\bin;" + [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User') + ";$env:USERPROFILE\.cargo\bin"

$trainer = "$PSScriptRoot\..\trainer"
if ($Build -or -not (Test-Path "$trainer\target\release\trainer.exe")) {
    Push-Location $trainer
    cargo build --release
    Pop-Location
}

. "$PSScriptRoot\paths.ps1"
$Root = "$LabRoot\training"
$dir = "$Root\$Name"
if (Test-Path "$dir\log.txt") { throw "Training run '$Name' already exists" }
New-Item -ItemType Directory -Force $dir | Out-Null
$dataPath = "$Root\data\$Data.bin"
if (-not (Test-Path $dataPath)) { throw "No prepared data at $dataPath (run prepare-data.ps1)" }

$p = Start-Process -FilePath "$trainer\target\release\trainer.exe" `
    -ArgumentList $Name, $dataPath, "$dir\checkpoints", $Superbatches, $Wdl, $Hidden, $Buckets, $Inputs `
    -RedirectStandardOutput "$dir\log.txt" -RedirectStandardError "$dir\err.txt" -WindowStyle Hidden -PassThru
"Training '$Name' started (pid $($p.Id)) -> $dir"
