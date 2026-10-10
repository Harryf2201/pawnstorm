# Builds an engine binary from any git ref (branch, tag or commit) without touching the working copy.
# Usage: .\tools\build-engine.ps1 -Ref exp/lmr-tweak -Name dev-lmr [-Net testing\engines\pawnstorm-v0.8-220m.nnue]
# Output: testing\engines\<Name>.exe (plus its bench node count, the usual fingerprint of search behaviour).
# Use the same -Net for both sides of an A/B test so only the code differs.
param(
    [Parameter(Mandatory)][string]$Ref,
    [Parameter(Mandatory)][string]$Name,
    [string]$Net = ''
)
# Native tools write progress to stderr; with 'Stop', PowerShell 5 would treat that as a failure.
$ErrorActionPreference = 'Continue'
. "$PSScriptRoot\paths.ps1"

$build = "$LabRoot\build"
$tree = "$build\tree-$Name"
New-Item -ItemType Directory -Force $build | Out-Null
Push-Location $RepoRoot
try {
    if (Test-Path $tree) { git worktree remove --force $tree 2>$null; Remove-Item -Recurse -Force $tree -ErrorAction SilentlyContinue }
    git worktree prune
    git worktree add --detach $tree $Ref 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "unknown git ref: $Ref" }
    $commit = (git -C $tree rev-parse --short HEAD)
    if ($Net) { Copy-Item (Resolve-Path $Net) "$tree\nets\default.bin" -Force }
    # One shared target directory keeps rebuilds fast across experiments.
    $env:CARGO_TARGET_DIR = "$build\target"
    Push-Location $tree
    cargo build --release -p pawnstorm 2>&1 | Select-String -Pattern '^(error|warning: unused)' -Context 0, 6
    Pop-Location
    if (-not (Test-Path "$build\target\release\pawnstorm.exe")) { throw 'build failed' }
    Copy-Item "$build\target\release\pawnstorm.exe" "$LabRoot\engines\$Name.exe" -Force
    $bench = (& "$LabRoot\engines\$Name.exe" bench 10 | Select-Object -Last 1)
    "built $Name from $Ref ($commit)$(if ($Net) { " with net $(Split-Path $Net -Leaf)" }): bench $bench"
} finally {
    git worktree remove --force $tree 2>$null
    Pop-Location
}
