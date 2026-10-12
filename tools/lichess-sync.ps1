# Keeps testing\lichess\status.json up to date for the dashboard's Lichess panel:
# the bot account's ratings, record and last games, read from the Lichess API every 2 minutes.
# Reads the token from <parent>\lichess-bot\config.yml (never committed, never written to status.json).
. "$PSScriptRoot\paths.ps1"
$cfg = Join-Path $Parent 'lichess-bot\config.yml'
$outDir = Join-Path $LabRoot 'lichess'
New-Item -ItemType Directory -Force $outDir | Out-Null
$statusFile = Join-Path $outDir 'status.json'

function Get-Token {
    if (-not (Test-Path $cfg)) { return $null }
    $m = Select-String -Path $cfg -Pattern '^token:\s*"([^"]+)"' | Select-Object -First 1
    if ($m) { return $m.Matches[0].Groups[1].Value }
    return $null
}

function Invoke-Lichess([string]$path, [hashtable]$headers) {
    $h = @{ Authorization = "Bearer $script:token" }
    if ($headers) { $headers.GetEnumerator() | ForEach-Object { $h[$_.Key] = $_.Value } }
    Invoke-RestMethod -Uri "https://lichess.org$path" -Headers $h -TimeoutSec 30
}

while ($true) {
    try {
        $script:token = Get-Token
        if ($script:token) {
            $acc = Invoke-Lichess '/api/account'
            $name = $acc.username
            $perfs = @()
            foreach ($p in 'bullet', 'blitz', 'rapid', 'classical') {
                $x = $acc.perfs.$p
                if ($x -and $x.games -gt 0) {
                    $perfs += [ordered]@{ perf = $p; rating = $x.rating; rd = $x.rd; games = $x.games; prov = [bool]$x.prov }
                }
            }
            $recent = @()
            $raw = Invoke-Lichess "/api/games/user/$($name)?max=15&moves=false&opening=false" @{ Accept = 'application/x-ndjson' }
            $lines = if ($raw -is [string]) { $raw -split "`n" } else { @($raw) }
            foreach ($line in $lines) {
                $g = if ($line -is [string]) { if ($line.Trim()) { $line | ConvertFrom-Json } else { $null } } else { $line }
                if (-not $g -or -not $g.players) { continue }
                $white = $g.players.white.user.id -eq $acc.id
                $me = if ($white) { $g.players.white } else { $g.players.black }
                $opp = if ($white) { $g.players.black } else { $g.players.white }
                $oppName = if ($opp.user) { $opp.user.name } elseif ($opp.aiLevel) { "Stockfish level $($opp.aiLevel)" } else { '?' }
                $result = if ($g.status -in 'started', 'created') { 'playing' }
                          elseif (-not $g.winner) { 'draw' }
                          elseif (($g.winner -eq 'white') -eq $white) { 'win' } else { 'loss' }
                $recent += [ordered]@{
                    id = $g.id; opponent = $oppName; opp_rating = $opp.rating; color = $(if ($white) { 'white' } else { 'black' })
                    result = $result; speed = $g.speed; rated = [bool]$g.rated; clock = $(if ($g.clock) { "$($g.clock.initial / 60)+$($g.clock.increment)" } else { '' })
                    status = $g.status; ended = $g.lastMoveAt; bot = [bool]($opp.user.title -eq 'BOT'); my_rating = $me.rating; diff = $me.ratingDiff
                }
            }
            $playing = @(Invoke-Lichess '/api/account/playing').nowPlaying
            $count = $acc.count
            $obj = [ordered]@{
                updated = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds(); username = $name
                title = $acc.title; perfs = $perfs; playing = $(if ($playing) { @($playing).Count } else { 0 })
                count = [ordered]@{ all = $count.all; rated = $count.rated; win = $count.win; loss = $count.loss; draw = $count.draw }
                recent = $recent
            }
            $json = $obj | ConvertTo-Json -Depth 6
            [IO.File]::WriteAllText("$statusFile.tmp", $json, (New-Object Text.UTF8Encoding($false)))
            Move-Item -Force "$statusFile.tmp" $statusFile
        }
    } catch {
        Add-Content (Join-Path $outDir 'sync.log') ("{0} {1}" -f (Get-Date -Format s), $_.Exception.Message)
    }
    Start-Sleep -Seconds 120
}
