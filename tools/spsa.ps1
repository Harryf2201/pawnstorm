# SPSA tuning of the search parameters (src/params.rs, exposed as UCI spin options).
# Each iteration plays a handful of game pairs between two copies of the engine whose parameters
# are nudged in opposite random directions, then moves every parameter towards the side that
# scored better (OpenBench's SPSA formulas). State is saved after every iteration, so it can be
# stopped and resumed at any time.
#   .\tools\spsa.ps1                    start or resume (testing\spsa\state.json)
#   create testing\spsa\STOP            to stop after the current iteration
# Plays 15 games at a time when no other tournament is running, 3 when one is (tests come first;
# one thread stays free for the Lichess bot).
param(
    [string]$Engine = 'pawnstorm-spsa-build',
    [string]$Net = 'pawnstorm-v1.7m.nnue',
    [string]$TC = '6+0.06',
    [int]$PairsPerIteration = 8,
    [int]$Concurrency = 15,
    [int]$Iterations = 2000,
    [double]$REnd = 0.002,
    # Options file (name=value lines): starting values for a new run; its untuned options go to both engines unchanged.
    [string]$Fixed = '',
    # Regex of option names not to tune (they keep their -Fixed value, or the engine default).
    [string]$Exclude = '',
    # Narrower tuning ranges, e.g. 'SprCorrLmr=32:192,SprNmpVerify=8:20' (keeps a tuned value off the engine's 0 = off).
    [string]$Ranges = ''
)
. "$PSScriptRoot\paths.ps1"
. "$PSScriptRoot\job-status.ps1"
# Below normal priority (fastchess and the engines inherit it), so the PC stays usable.
try { (Get-Process -Id $PID).PriorityClass = 'BelowNormal' } catch {}
$dir = "$LabRoot\spsa"
New-Item -ItemType Directory -Force $dir | Out-Null
$exe = "$LabRoot\engines\$Engine.exe"
$fastchess = "$LabRoot\fastchess\fastchess-windows-x86-64\fastchess.exe"
$job = @{ Id = 'spsa'; Title = 'Search tuning (SPSA)' }

# Parameters and their ranges, straight from the engine.
$params = @("uci`nquit" | & $exe | Where-Object { $_ -match '^option name (\S+) type spin default (-?\d+) min (-?\d+) max (-?\d+)' } | ForEach-Object {
        [pscustomobject]@{ Name = $Matches[1]; Default = [int]$Matches[2]; Min = [int]$Matches[3]; Max = [int]$Matches[4] }
    } | Where-Object { $_.Name -notin 'Hash', 'Threads', 'Move' })
if ($Exclude) { $params = @($params | Where-Object { $_.Name -notmatch $Exclude }) }
foreach ($r in ($Ranges -split ',' | Where-Object { $_ })) {
    $n, $lo, $hi = $r -split '[=:]'
    $params | Where-Object { $_.Name -eq $n } | ForEach-Object { $_.Min = [int]$lo; $_.Max = [int]$hi }
}
$fixedVals = [ordered]@{}
if ($Fixed) { Get-Content $Fixed | Where-Object { $_ -match '^(\w+)=(.+)$' -and $Matches[1] -ne 'EvalFile' } | ForEach-Object { $fixedVals[$Matches[1]] = $Matches[2] } }
$fixedOpts = @($fixedVals.Keys | Where-Object { $_ -notin $params.Name } | ForEach-Object { "option.$_=$($fixedVals[$_])" })
# For the dashboard: each parameter's default and range.
$params | ConvertTo-Json | Set-Content "$dir\params.json" -Encoding ascii
$stateFile = "$dir\state.json"
if (Test-Path $stateFile) {
    $state = Get-Content $stateFile -Raw | ConvertFrom-Json
} else {
    $theta = [ordered]@{}
    foreach ($p in $params) { $theta[$p.Name] = if ($fixedVals.Contains($p.Name)) { [double]$fixedVals[$p.Name] } else { [double]$p.Default } }
    $state = [pscustomobject]@{ k = 0; games = 0; started = [int][double]::Parse((Get-Date -UFormat %s)); theta = [pscustomobject]$theta }
}
$history = "$dir\history.csv"
if (-not (Test-Path $history)) { Set-Content $history ('iteration,games,wins,losses,draws,' + ($params.Name -join ',')) -Encoding ascii }

# OpenBench-style schedule over N game pairs.
$N = $Iterations * $PairsPerIteration
$alpha = 0.602; $gamma = 0.101; $A = 0.1 * $N
$rnd = [Random]::new()
while ($state.k -lt $N -and -not (Test-Path "$dir\STOP")) {
    $k = $state.k
    $deltas = @{}; $optsA = @(); $optsB = @(); $ck = @{}
    foreach ($p in $params) {
        $cEnd = [math]::Max(0.5, ($p.Max - $p.Min) / 20)
        $c = $cEnd * [math]::Pow($N, $gamma)
        $ck[$p.Name] = $c / [math]::Pow($k + 1, $gamma)
        $deltas[$p.Name] = if ($rnd.Next(2) -eq 0) { -1 } else { 1 }
        $t = $state.theta.($p.Name)
        $a = [math]::Round([math]::Min($p.Max, [math]::Max($p.Min, $t + $ck[$p.Name] * $deltas[$p.Name])))
        $b = [math]::Round([math]::Min($p.Max, [math]::Max($p.Min, $t - $ck[$p.Name] * $deltas[$p.Name])))
        $optsA += "option.$($p.Name)=$a"; $optsB += "option.$($p.Name)=$b"
    }
    # Tests and tournaments come first: only 4 games at a time while one is running.
    $others = @(Get-CimInstance Win32_Process -Filter "Name='fastchess.exe'" | Where-Object { $_.CommandLine -notmatch 'name=spsa-a' })
    # (One thread always stays free for the Lichess bot, which plays in real time.)
    $conc = if ($others.Count) { 3 } else { $Concurrency }
    $out = & $fastchess -engine "cmd=$exe" name=spsa-a @optsA -engine "cmd=$exe" name=spsa-b @optsB `
        -each "tc=$TC" option.Hash=16 @fixedOpts "option.EvalFile=$LabRoot\engines\$Net" `
        -openings "file=$LabRoot\books\8moves_v3.pgn" format=pgn order=random -srand ($state.started + $k) `
        -rounds $PairsPerIteration -games 2 -repeat -concurrency $conc 2>&1
    $res = $out | Select-String '^Games: (\d+), Wins: (\d+), Losses: (\d+), Draws: (\d+)' | Select-Object -Last 1
    if (-not $res) { Set-JobStatus @job -State failed -Detail 'fastchess gave no result (see testing\spsa\last-output.txt).'; $out | Set-Content "$dir\last-output.txt"; exit 1 }
    $g = $res.Matches[0].Groups
    $w = [int]$g[2].Value; $l = [int]$g[3].Value; $d = [int]$g[4].Value
    # An incomplete batch (an engine crashed or timed out) must not advance the schedule.
    if ($w + $l + $d -lt 2 * $PairsPerIteration) { $out | Set-Content "$dir\last-output.txt"; continue }
    foreach ($p in $params) {
        $cEnd = [math]::Max(0.5, ($p.Max - $p.Min) / 20)
        $aEnd = $REnd * $cEnd * $cEnd
        $aBig = $aEnd * [math]::Pow($A + $N, $alpha)
        $ak = $aBig / [math]::Pow($A + $k + 1, $alpha)
        $t = $state.theta.($p.Name) + $ak * ($w - $l) * $deltas[$p.Name] / $ck[$p.Name]
        $state.theta.($p.Name) = [math]::Min($p.Max, [math]::Max($p.Min, $t))
    }
    $state.k += $PairsPerIteration
    $state.games += $w + $l + $d
    $state | Add-Member -NotePropertyName planned -NotePropertyValue $Iterations -Force
    $state | ConvertTo-Json -Depth 4 | Set-Content $stateFile -Encoding ascii
    Add-Content $history ("$($state.k / $PairsPerIteration),$($state.games),$w,$l,$d," + (($params | ForEach-Object { [math]::Round($state.theta.($_.Name), 2) }) -join ',')) -Encoding ascii
    # The parameters that have moved furthest from their defaults, relative to their range.
    $moved = $params | Sort-Object { - [math]::Abs($state.theta.($_.Name) - $_.Default) / ($_.Max - $_.Min) } | Select-Object -First 4 |
        ForEach-Object { "$($_.Name) $($_.Default) -> $([math]::Round($state.theta.($_.Name), 1))" }
    Set-JobStatus @job -State training -Detail ("Iteration $($state.k / $PairsPerIteration) of $Iterations, $($state.games) games ($conc at a time). Biggest changes: " + ($moved -join ', '))
}
Set-JobStatus @job -State done -Detail ("Stopped after $($state.games) games. Tuned values are in testing\spsa\state.json.")
