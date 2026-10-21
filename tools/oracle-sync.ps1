# Collects self-play data from the always-on Oracle Cloud server (see tools\engines.local.ps1 for its
# address). The server writes finished 1-hour segments to ~/outbox/seg-<time>/ (gzipped data plus
# status.txt); this copies each one into testing\datagen\<Name>\, then deletes it on the server.
# testing\datagen\<Name>\status.txt totals every collected segment for the dashboard and training.
# Usage: .\tools\oracle-sync.ps1            (loop every 10 minutes; run hidden in the background)
#        .\tools\oracle-sync.ps1 -Once
# -Name: the data folder; it changes with each data generation (oracle-gen2 = v0.8's network, oracle-gen3 = v1.3's, oracle-gen4 = v1.7m's, oracle-gen5 = v1.11's, oracle-gen6 = v1.14's, oracle-gen7 = 2.0's).
# -From / -Before (segment time stamps, yyyyMMdd-HHmmss UTC): only collect segments from that time on / before it, so a
# generation switch on the server never mixes data (segments carry no generation label).
param([switch]$Once, [int]$IntervalSeconds = 600, [string]$Name = 'oracle-gen7', [string]$From = '', [string]$Before = '')

. "$PSScriptRoot\paths.ps1"
if (-not $OracleHost) { throw 'Set $OracleHost and $OracleKey in tools\engines.local.ps1' }
$dir = "$LabRoot\datagen\$Name"
$events = "$LabRoot\datagen\oracle-events.log"
New-Item -ItemType Directory -Force $dir | Out-Null
# Keepalives make a dead connection fail after ~1 minute instead of hanging the sync forever.
$ssh = @('-i', $OracleKey, '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=20', '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=4')
function Log($m) { Add-Content $events "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') $m" -Encoding utf8 }

# Runs ssh/scp with stdin closed and a hard time limit. (Started from a hidden window, Windows'
# ssh could otherwise wait on its stdin after the remote command had finished, and the
# keepalives kept that connection open: the sync hung for hours.)
# Returns @{ Ok = exited with 0 in time; Out = stdout lines }.
function Invoke-Remote([string]$exe, [string[]]$arguments, [int]$TimeoutSeconds = 120) {
    $psi = [Diagnostics.ProcessStartInfo]::new("$env:WINDIR\System32\OpenSSH\$exe.exe")
    $psi.Arguments = ($arguments | ForEach-Object { if ($_ -match '[\s"]') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ } }) -join ' '
    $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
    $p = [Diagnostics.Process]::Start($psi)
    $p.StandardInput.Close()
    $out = $p.StandardOutput.ReadToEndAsync()
    if (-not $p.WaitForExit($TimeoutSeconds * 1000)) {
        try { $p.Kill() } catch {}
        Log "$exe timed out after ${TimeoutSeconds}s: $($arguments[-1])"
        return @{ Ok = $false; Out = @() }
    }
    return @{ Ok = ($p.ExitCode -eq 0); Out = @($out.Result -split "`r?`n" | Where-Object { $_ }) }
}
function Remote([string]$cmd) { (Invoke-Remote 'ssh' ($ssh + $OracleHost + $cmd)).Out }

function Sync-Once {
    $segs = @(Remote 'ls -1 ~/outbox 2>/dev/null' | Where-Object { $_ -match '^seg-\d{8}-\d{6}$' -and (-not $From -or $_.Substring(4) -ge $From) -and (-not $Before -or $_.Substring(4) -lt $Before) })
    foreach ($s in $segs) {
        $tmp = "$dir\_dl_$s"
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
        $copied = Invoke-Remote 'scp' ($ssh + '-q', '-r', "${OracleHost}:outbox/$s", $tmp) 900
        if (-not $copied.Ok -or -not (Test-Path "$tmp\status.txt")) { Log "copy of $s failed"; continue }
        $tag = $s -replace '^seg-', ''
        foreach ($f in Get-ChildItem "$tmp\data_*.txt.gz") { Move-Item -Force $f.FullName "$dir\data_$($tag)_$($f.Name.Substring(5))" }
        Move-Item -Force "$tmp\status.txt" "$dir\seg_$tag.status"
        Remove-Item -Recurse -Force $tmp
        $null = Remote "rm -rf ~/outbox/$s"
        Log "collected $s"
    }
    # The segment still being generated, for a live count.
    $live = @(Remote 'cat ~/work/seg-*/status.txt 2>/dev/null')
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
