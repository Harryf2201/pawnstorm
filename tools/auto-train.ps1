# Trains and rates a new engine version each time the training data reaches a milestone
# (5M, 10M, 20M, 40M, ... positions, local + cloud). Each version gets a rating gauntlet, so the
# dashboard's rating graph shows how strength grows with data.
# Runs in the background: .\tools\auto-train.ps1   (state in testing\training\schedule.txt)
param([int]$IntervalSeconds = 120, [int64]$FirstMilestone = 5000000)

. "$PSScriptRoot\paths.ps1"
$TrainRoot = "$LabRoot\training"
$StateFile = "$TrainRoot\schedule.txt"
$LogFile = "$TrainRoot\schedule.log"
New-Item -ItemType Directory -Force $TrainRoot, "$LabRoot\engines" | Out-Null

function Log($m) { Add-Content $LogFile "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') $m" -Encoding utf8 }

function Read-State {
    $s = @{ next_version = 3; done = '' }
    if (Test-Path $StateFile) { foreach ($l in Get-Content $StateFile) { $k, $v = $l -split '=', 2; if ($k) { $s[$k] = $v } } }
    $s
}
function Write-State($s) { [IO.File]::WriteAllLines($StateFile, [string[]]($s.Keys | Sort-Object | ForEach-Object { "$_=$($s[$_])" })) }

# Total generated positions and the data folders they live in (local generators + downloaded cloud data).
function Get-Data {
    $total = 0; $sources = @()
    foreach ($d in Get-ChildItem "$LabRoot\datagen" -Directory) {
        $st = "$($d.FullName)\status.txt"
        if (-not (Test-Path $st)) { continue }
        $p = (Get-Content $st | Select-String '^positions=') -replace 'positions=', ''
        if ($p -and [int64]$p -gt 0) { $total += [int64]$p; $sources += $d.Name }
    }
    @{ total = $total; sources = $sources }
}

function Milestones { $m = $FirstMilestone; while ($m -le 100e9) { $m; $m *= 2 } }
function Label($n) { if ($n -ge 1e9) { '{0:0.#}b' -f ($n / 1e9) } else { '{0:0}m' -f ($n / 1e6) } }

function Busy {
    # Don't start while a training run or another of our gauntlets is still going.
    (Get-Process trainer -ErrorAction SilentlyContinue) -or
    (Get-Process fastchess -ErrorAction SilentlyContinue)
}

function Current-Rating {
    try {
        $runs = Invoke-RestMethod http://localhost:8000/api/runs -TimeoutSec 5
        $r = $runs | Where-Object { $_.status -eq 'finished' -and $_.perf } | Sort-Object perf -Descending | Select-Object -First 1
        if ($r) { return [double]$r.perf }
    } catch {}
    2700
}

function Train-Version($milestone, $data, $state) {
    $ver = "pawnstorm-v0.$($state.next_version)-$(Label $milestone)"
    $name = "net-v0.$($state.next_version)-$(Label $milestone)"
    Log "milestone $(Label $milestone): building $ver (data available: $($data.total) positions in $($data.sources -join ', '))"
    $state.status = "preparing data for $ver"; Write-State $state

    # Exactly the milestone's worth of data, so each version measures "strength at N positions".
    & "$PSScriptRoot\prepare-data.ps1" -Sources $data.sources -Name $name -MaxPositions $milestone | ForEach-Object { Log "  $_" }
    # About 25 passes over the data, between 2 and 40 rounds of 100M positions.
    $sb = [int][math]::Min(40, [math]::Max(2, [math]::Round($milestone * 25 / 100e6)))
    $state.status = "training $ver on the GPU ($sb rounds)"; Write-State $state
    $dir = "$TrainRoot\$name"
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue   # leftovers from a failed attempt
    & "$PSScriptRoot\train.ps1" -Name $name -Data $name -Superbatches $sb | ForEach-Object { Log "  $_" }
    while (-not (Select-String -Path "$dir\log.txt" -Pattern 'Total Training Time' -Quiet -ErrorAction SilentlyContinue)) {
        if (Select-String -Path "$dir\err.txt" -Pattern 'panicked' -Quiet -ErrorAction SilentlyContinue) { throw "training $name failed" }
        Start-Sleep 15
    }
    $net = Get-ChildItem -Recurse "$dir\checkpoints" -Filter quantised.bin | Sort-Object LastWriteTime | Select-Object -Last 1
    Copy-Item $net.FullName "$LabRoot\engines\$ver.nnue" -Force
    # The prepared training file is no longer needed (the text data stays in testing\datagen).
    Remove-Item "$TrainRoot\data\$name.bin" -ErrorAction SilentlyContinue

    # Make sure the engine really loads this network before rating it.
    $check = & "$PSScriptRoot\uci-test.ps1" -Engine "$LabRoot\engines\pawnstorm-nnue.exe" -Commands "setoption name EvalFile value $LabRoot\engines\$ver.nnue", 'eval' -WaitMs 1500
    if ($check -notmatch 'loaded network') { throw "engine could not load $ver.nnue" }

    $r = Current-Rating
    $lo = [int][math]::Max(1400, [math]::Min(3000, [math]::Round(($r - 200) / 100) * 100))
    $hi = [int][math]::Max(1600, [math]::Min(3190, [math]::Round(($r + 200) / 100) * 100))
    # Play every earlier version too: the dashboard rates all versions jointly from every game,
    # so these games sharpen the ratings of the old versions as well as the new one.
    $previous = @('pawnstorm-v0.1', 'pawnstorm-v0.2') + @(Get-ChildItem "$LabRoot\engines\*.nnue" | Sort-Object CreationTime | ForEach-Object BaseName) |
        Where-Object { $_ -ne $ver }
    $state.status = "rating $ver against Stockfish $lo/$hi and $($previous.Count) earlier versions"; Write-State $state
    & "$PSScriptRoot\match.ps1" -Name "$ver rating ($(Label $milestone) positions)" -Type gauntlet `
        -Engines (@($ver, "sf:$lo", "sf:$hi") + $previous) -Rounds 15 -Concurrency 6 -Live `
        -Notes "network trained on $(Label $milestone) positions, $sb rounds" | ForEach-Object { Log "  $_" }
    $state.next_version = [int]$state.next_version + 1
}

Log 'auto-train started'
while ($true) {
    Set-Content "$TrainRoot\auto-train.heartbeat" (Get-Date -Format o)
    $state = Read-State
    try {
        $data = Get-Data
        $done = @($state.done -split ',' | Where-Object { $_ })
        $reached = @(Milestones | Where-Object { $_ -le $data.total -and "$_" -notin $done })
        $next = Milestones | Where-Object { "$_" -notin $done -and $_ -gt $data.total } | Select-Object -First 1
        $state.positions = $data.total
        $state.next_milestone = $next
        if ($reached -and -not (Busy)) {
            # One milestone at a time, smallest first: a cloud batch that jumps past several
            # milestones produces a version for each of them (one after another), so the graph
            # keeps a point at every step.
            $target = ($reached | Measure-Object -Minimum).Minimum
            Train-Version $target $data $state
            $state.done = (@($done) + "$target") -join ','
            $next = Milestones | Where-Object { "$_" -notin ($state.done -split ',') } | Select-Object -First 1
            $state.status = "waiting for $(Label $next) positions"
        } elseif ($reached) {
            $state.status = "next: $(Label (($reached | Measure-Object -Minimum).Minimum)) version (data is ready), waiting for the current tournament/training to finish"
        } else {
            $state.status = "waiting for $(Label $next) positions"
        }
    } catch {
        Log "ERROR: $($_.Exception.Message)"
        $state.status = "error: $($_.Exception.Message)"
    }
    Write-State $state
    Start-Sleep $IntervalSeconds
}
