# Collects self-play data from the always-on Oracle Cloud server (see tools\engines.local.ps1 for its
# address). The server writes finished 1-hour segments to ~/outbox/seg-<time>/ (gzipped data plus
# status.txt); this copies each one into testing\datagen\oracle-gen2\, then deletes it on the server.
# testing\datagen\oracle-gen2\status.txt totals every collected segment for the dashboard and training.
# Usage: .\tools\oracle-sync.ps1            (loop every 10 minutes; run hidden in the background)
#        .\tools\oracle-sync.ps1 -Once
param([switch]$Once, [int]$IntervalSeconds = 600)

. "$PSScriptRoot\paths.ps1"
if (-not $OracleHost) { throw 'Set $OracleHost and $OracleKey in tools\engines.local.ps1' }
$dir = "$LabRoot\datagen\oracle-gen2"
$events = "$LabRoot\datagen\oracle-events.log"
New-Item -ItemType Directory -Force $dir | Out-Null
$ssh = @('-i', $OracleKey, '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=20')
function Log($m) { Add-Content $events "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') $m" -Encoding utf8 }

function Sync-Once {
    $segs = @(ssh @ssh $OracleHost 'ls -1 ~/outbox 2>/dev/null' | Where-Object { $_ -match '^seg-\d{8}-\d{6}$' })
    foreach ($s in $segs) {
        $tmp = "$dir\_dl_$s"
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
        scp @ssh -q -r "${OracleHost}:outbox/$s" $tmp
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path "$tmp\status.txt")) { Log "copy of $s failed"; continue }
        $tag = $s -replace '^seg-', ''
        foreach ($f in Get-ChildItem "$tmp\data_*.txt.gz") { Move-Item -Force $f.FullName "$dir\data_$($tag)_$($f.Name.Substring(5))" }
        Move-Item -Force "$tmp\status.txt" "$dir\seg_$tag.status"
        Remove-Item -Recurse -Force $tmp
        ssh @ssh $OracleHost "rm -rf ~/outbox/$s" | Out-Null
        Log "collected $s"
    }
    # The segment still being generated, for a live count.
    $live = @(ssh @ssh $OracleHost 'cat ~/work/seg-*/status.txt 2>/dev/null')
    $sum = @{ positions = 0; games = 0; white_wins = 0; draws = 0; black_wins = 0; plies = 0; nodes = 0 }
    $first = 0
    foreach ($st in Get-ChildItem "$dir\seg_*.status") {
        foreach ($line in Get-Content $st) {
            $k, $v = $line -split '=', 2
            if ($sum.ContainsKey($k)) { $sum[$k] += [int64]$v }
            if ($k -eq 'started' -and ($first -eq 0 -or [int64]$v -lt $first)) { $first = [int64]$v }
        }
    }
    $pending = 0; $threads = 0; $segStart = 0; $segUpdated = 0
    foreach ($line in $live) {
        if ($line -match '^positions=(\d+)') { $pending = [int64]$Matches[1] }
        if ($line -match '^threads=(\d+)') { $threads = [int]$Matches[1] }
        if ($line -match '^started=(\d+)') { $segStart = [int64]$Matches[1] }
        if ($line -match '^updated=(\d+)') { $segUpdated = [int64]$Matches[1] }
    }
    # Positions per hour, from the segment being generated now.
    $rate = if ($segUpdated -gt $segStart + 60) { [int64]($pending * 3600 / ($segUpdated - $segStart)) } else { 0 }
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    # positions = data already on this PC (what training uses); pending = in the server's current segment.
    $lines = @('source=oracle', "state=$(if ($live.Count) { 'running' } else { 'stopped' })", "started=$first", "updated=$now",
        "threads=$threads", 'target=1000000000', 'soft_nodes=5000', "pending=$pending", "rate_h=$rate", "sync_every=$IntervalSeconds", "host=Oracle Cloud ARM ($threads cores)")
    foreach ($k in $sum.Keys) { $lines += "$k=$($sum[$k])" }
    [IO.File]::WriteAllLines("$dir\status.tmp", [string[]]$lines)
    Move-Item -Force "$dir\status.tmp" "$dir\status.txt"
}

Log 'oracle sync started'
while ($true) {
    Set-Content "$LabRoot\datagen\oracle-sync.heartbeat" (Get-Date -Format o)
    try { Sync-Once } catch { Log "sync error: $($_.Exception.Message)" }
    if ($Once) { break }
    Start-Sleep $IntervalSeconds
}
