# Starts a game between the Lichess bot and the Lichess AI (Stockfish, level 1-8), then opens it in the browser.
# The bot (on the Oracle server) plays it like any other game. Runs ~/challenge-ai.sh on the server, which reads the
# bot's token from its own config there (the token never leaves the server).
# Usage: .\tools\lichess-ai.ps1 [-Level 8] [-Minutes 5] [-Increment 3] [-Color random|white|black]
# A desktop shortcut "Bot vs Lichess AI" runs it with the defaults.
param([ValidateRange(1, 8)][int]$Level = 8, [int]$Minutes = 5, [int]$Increment = 3, [ValidateSet('random', 'white', 'black')][string]$Color = 'random')
. "$PSScriptRoot\paths.ps1"
$out = & ssh -i $OracleKey -o BatchMode=yes -o ConnectTimeout=20 $OracleHost "~/challenge-ai.sh $Level $Minutes $Increment $Color" 2>&1
$out
$url = ([regex]::Match(($out -join ' '), 'https://lichess\.org/\w+')).Value
if ($url) { Start-Process $url }
if ($Host.Name -eq 'ConsoleHost' -and [Environment]::UserInteractive) { Start-Sleep 4 }
