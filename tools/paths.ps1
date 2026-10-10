# Shared locations for the tools scripts (dot-source this file).
# Layout: <parent>\pawnstorm (this repo), <parent>\testing (games, data, nets), <parent>\bullet (trainer library).
$RepoRoot = (Resolve-Path "$PSScriptRoot\..").Path
$Parent = Split-Path $RepoRoot -Parent
$LabRoot = Join-Path $Parent 'testing'
$BulletRoot = Join-Path $Parent 'bullet'

# Opponent engines used by match.ps1. Override any of these in tools\engines.local.ps1 (not committed).
$Downloads = Join-Path $HOME 'Downloads'
$Known = @{
    stockfish   = "$Downloads\stockfish-windows-x86-64-avx2\stockfish\stockfish-windows-x86-64-avx2.exe"
    dragon      = "$Downloads\dragon\dragon_05e2a7\Windows\dragon-64bit-avx2.exe"
    rubichess   = "$Downloads\RubiChess-20240817\RubiChess-20240817\windows\RubiChess-20240817_x86-64-avx2.exe"
    plentychess = "$Downloads\PlentyChess-7.0.37-windows-avx2.exe"
}
if (Test-Path "$PSScriptRoot\engines.local.ps1") { . "$PSScriptRoot\engines.local.ps1" }
