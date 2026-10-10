# Keeps the dashboard in sync with cloud data generation on GitHub Actions:
#  - writes testing\datagen\cloud-<run id>\status.txt (jobs running/done, positions so far)
#  - downloads each finished job's data (gzipped text, same format as the local generator)
#  - if GitHub reports throttling, usage limits or account warnings, it stops syncing,
#    records the problem for the dashboard and shows a Windows notification.
# Usage: .\tools\cloud-sync.ps1            (loop every 5 minutes; run hidden in the background)
#        .\tools\cloud-sync.ps1 -Once      (single pass)
param([switch]$Once, [int]$IntervalSeconds = 300)

. "$PSScriptRoot\paths.ps1"
$DataRoot = "$LabRoot\datagen"
$EventsFile = "$DataRoot\cloud-events.log"
New-Item -ItemType Directory -Force $DataRoot | Out-Null
Set-Location $RepoRoot

function Log($msg) { Add-Content -Path $EventsFile -Value "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') $msg" -Encoding utf8 }

function Notify($title, $text) {
    try {
        Add-Type -AssemblyName System.Windows.Forms
        $n = New-Object System.Windows.Forms.NotifyIcon
        $n.Icon = [System.Drawing.SystemIcons]::Warning; $n.Visible = $true
        $n.ShowBalloonTip(15000, $title, $text, 'Warning'); Start-Sleep 1; $n.Dispose()
    } catch {}
}

function Unix($iso) { if ($iso) { [DateTimeOffset]::Parse($iso).ToUnixTimeSeconds() } else { 0 } }

# Phrases GitHub uses when it limits or flags Actions usage.
$WarningPatterns = 'rate limit|usage limit|spending limit|exceeded|abuse|suspended|terms of service|billing|quota|throttl|has been disabled|account is locked'

# Expected positions from one job of a batch, for live estimates before any job has finished:
# the average job of earlier batches with the same eval (scaled to the job length). With no
# finished NNUE batch yet, NNUE is assumed to give half the classical rate (it searches about
# half as many nodes per second; positions per node are the same).
function Estimate-PerJob($eval, $minutes) {
    $per = @{}
    foreach ($d in Get-ChildItem $DataRoot -Directory -Filter 'cloud-*') {
        $shards = @(Get-ChildItem "$($d.FullName)\shard_*.status" -ErrorAction SilentlyContinue)
        if (-not $shards.Count) { continue }
        $ev = 'classical'; $m = 330
        if (Test-Path "$($d.FullName)\launch.txt") {
            $l = Get-Content "$($d.FullName)\launch.txt"
            $e = $l | Select-String '^eval='; if ($e) { $ev = $e.ToString().Split('=')[1] }
            $mm = $l | Select-String '^minutes='; if ($mm) { $m = [int]$mm.ToString().Split('=')[1] }
        }
        foreach ($sh in $shards) {
            $p = (Get-Content $sh | Select-String '^positions=').ToString().Split('=')[1]
            if (-not $per[$ev]) { $per[$ev] = @() }
            $per[$ev] += [double]$p / $m
        }
    }
    $rate = if ($per[$eval]) { ($per[$eval] | Measure-Object -Average).Average }
            elseif ($per['classical']) { 0.5 * ($per['classical'] | Measure-Object -Average).Average } else { 0 }
    [int64]($rate * $minutes)
}

function Sync-Run($run) {
    $id = $run.databaseId
    $dir = "$DataRoot\cloud-$id"
    New-Item -ItemType Directory -Force $dir | Out-Null
    # A chained follow-up batch (started by GitHub, not cloud-datagen.ps1) inherits its parent's settings.
    if (-not (Test-Path "$dir\launch.txt")) {
        $parent = Get-ChildItem $DataRoot -Directory -Filter 'cloud-*' |
            Where-Object { [int64]($_.Name -replace '\D', '') -lt $id -and (Test-Path "$($_.FullName)\launch.txt") } |
            Sort-Object { [int64]($_.Name -replace '\D', '') } | Select-Object -Last 1
        $pl = if ($parent) { Get-Content "$($parent.FullName)\launch.txt" } else { @() }
        $pc = $pl | Select-String '^chain=(\d+)'
        if ($pc -and [int]$pc.Matches[0].Groups[1].Value -gt 0) {
            $child = $pl | ForEach-Object { if ($_ -match '^chain=(\d+)') { "chain=$([int]$Matches[1] - 1)" } else { $_ } }
            [IO.File]::WriteAllLines("$dir\launch.txt", [string[]]$child)
        }
    }
    $done = @{}
    if (Test-Path "$dir\downloaded.txt") { Get-Content "$dir\downloaded.txt" | ForEach-Object { $done[$_] = $true } }

    $view = gh run view $id --json jobs,status,conclusion,createdAt,updatedAt,url | ConvertFrom-Json
    $gen = @($view.jobs | Where-Object { $_.name -like 'generate*' })
    $warning = ''

    # Look for GitHub-side problems in failed or cancelled jobs.
    foreach ($j in $gen | Where-Object { $_.conclusion -in 'failure', 'cancelled', 'startup_failure' }) {
        $log = (gh run view $id --job $j.databaseId --log-failed 2>&1 | Out-String)
        $m = [regex]::Match($log, ".{0,80}($WarningPatterns).{0,120}", 'IgnoreCase')
        if ($m.Success) { $warning = "GitHub: $($m.Value.Trim())" }
    }
    $annotations = gh api "repos/{owner}/{repo}/check-runs/$($gen[0].databaseId)/annotations" 2>$null | ConvertFrom-Json -ErrorAction SilentlyContinue
    foreach ($a in @($annotations)) {
        if ($a.message -match $WarningPatterns) { $warning = "GitHub: $($a.message)" }
    }

    # Download artifacts from jobs that have finished (available even while the run continues).
    $arts = (gh api "repos/{owner}/{repo}/actions/runs/$id/artifacts?per_page=100" | ConvertFrom-Json).artifacts
    foreach ($a in $arts) {
        if ($done[$a.name] -or $a.expired) { continue }
        $tmp = "$dir\_dl_$($a.name)"
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
        gh run download $id -n $a.name -D $tmp 2>&1 | Out-Null
        if (-not (Test-Path "$tmp\status.txt")) { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue; continue }
        $shard = ($a.name -replace '\D', '').PadLeft(2, '0')
        foreach ($f in Get-ChildItem "$tmp\data_*.txt.gz") { Move-Item -Force $f.FullName "$dir\data_s${shard}_$($f.Name.Substring(5))" }
        Move-Item -Force "$tmp\status.txt" "$dir\shard_$shard.status"
        Remove-Item -Recurse -Force $tmp
        Add-Content "$dir\downloaded.txt" $a.name
        $done[$a.name] = $true
        Log "run $id`: downloaded $($a.name)"
    }

    # Totals from the downloaded shards.
    $sum = @{ positions = 0; games = 0; white_wins = 0; draws = 0; black_wins = 0; plies = 0; nodes = 0; threads = 0 }
    $cpus = @{}
    foreach ($s in Get-ChildItem "$dir\shard_*.status" -ErrorAction SilentlyContinue) {
        foreach ($line in Get-Content $s) {
            $k, $v = $line -split '=', 2
            if ($sum.ContainsKey($k)) { $sum[$k] += [int64]$v }
            if ($k -eq 'cpu') { $cpus[$v] = 1 }
        }
    }
    $running = @($gen | Where-Object { $_.status -eq 'in_progress' }).Count
    $queued = @($gen | Where-Object { $_.status -in 'queued', 'waiting', 'pending' }).Count
    $finished = @($gen | Where-Object { $_.status -eq 'completed' }).Count
    $failed = @($gen | Where-Object { $_.conclusion -in 'failure', 'cancelled', 'timed_out' }).Count
    $state = if ($view.status -eq 'completed') { 'finished' } elseif ($view.status -in 'queued', 'pending', 'waiting', 'requested') { 'queued' } else { 'running' }
    # Count time from when the first job actually started (a batch can sit queued for hours).
    $jobStarts = @($gen | Where-Object { $_.startedAt -and $_.startedAt -notlike '0001*' } | ForEach-Object { Unix $_.startedAt })
    $started = if ($jobStarts.Count) { ($jobStarts | Measure-Object -Minimum).Minimum } else { Unix $view.createdAt }
    $updated = if ($state -eq 'running') { [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() } else { Unix $view.updatedAt }
    # cloud-datagen.ps1 records the requested job length; GitHub's run API doesn't expose inputs.
    $minutes = 330
    if (Test-Path "$dir\launch.txt") { $minutes = [int]((Get-Content "$dir\launch.txt" | Select-String '^minutes=').ToString().Split('=')[1]) }
    $eval = 'classical'
    if (Test-Path "$dir\launch.txt") { $e = Get-Content "$dir\launch.txt" | Select-String '^eval='; if ($e) { $eval = $e.ToString().Split('=')[1] } }
    $lines = @(
        "source=cloud", "state=$state", "run_id=$id", "url=$($view.url)", "started=$started", "updated=$updated",
        "jobs_total=$($gen.Count)", "jobs_running=$running", "jobs_queued=$queued", "jobs_done=$finished", "jobs_failed=$failed",
        "jobs_downloaded=$($done.Count)", "minutes=$minutes", "soft_nodes=5000", "target=0",
        "cpu=$($cpus.Keys -join ' / ')", "warning=$warning", "eval=$eval", "est_per_job=$(Estimate-PerJob $eval $minutes)"
    )
    foreach ($k in $sum.Keys) { $lines += "$k=$($sum[$k])" }
    [IO.File]::WriteAllLines("$dir\status.tmp", [string[]]$lines)
    Move-Item -Force "$dir\status.tmp" "$dir\status.txt"
    return $warning
}

Log 'cloud sync started'
while ($true) {
    Set-Content "$DataRoot\cloud-sync.heartbeat" (Get-Date -Format o)
    $stop = $false
    try {
        $runs = gh run list --workflow datagen.yml --limit 10 --json databaseId,status,conclusion,createdAt | ConvertFrom-Json
        foreach ($run in $runs) {
            $dir = "$DataRoot\cloud-$($run.databaseId)"
            # Skip runs that are finished and fully downloaded.
            if ($run.status -eq 'completed' -and (Test-Path "$dir\status.txt") -and
                (Select-String -Path "$dir\status.txt" -Pattern '^state=finished' -Quiet) -and
                -not (Get-ChildItem $dir -Filter '_dl_*' -ErrorAction SilentlyContinue)) {
                $s = Get-Content "$dir\status.txt"
                $total = ($s | Select-String '^jobs_total=').ToString().Split('=')[1]
                $got = ($s | Select-String '^jobs_downloaded=').ToString().Split('=')[1]
                if ($got -ge $total) { continue }
            }
            $w = Sync-Run $run
            if ($w) {
                Log "WARNING from GitHub on run $($run.databaseId): $w - cloud sync stopped, no further batches."
                Notify 'Pawnstorm: GitHub warning' "$w`nCloud sync stopped. Ask Claude to look at it."
                Set-Content "$DataRoot\cloud-STOPPED.txt" $w
                $stop = $true
            }
        }
    } catch {
        Log "sync error: $($_.Exception.Message)"
    }
    if ($Once -or $stop) { break }
    Start-Sleep $IntervalSeconds
}
