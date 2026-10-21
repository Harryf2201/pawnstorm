# Hardware watchdog: polls temperatures, power, RAM and disk every few seconds and pauses
# (suspends) heavy workloads if anything gets unsafe, resuming once things are back to normal.
#   warn     -> pause data generation (the biggest CPU load)
#   critical -> pause everything: data generation, GPU training, tournaments
# State is written to testing\health\ for the dashboard. Control it by writing "auto", "pause"
# or "resume" to testing\health\control.txt (the dashboard buttons do this).
# CPU sensors come from LibreHardwareMonitor's local web server (http://localhost:8085).

. "$PSScriptRoot\paths.ps1"
$Root = "$LabRoot\health"
New-Item -ItemType Directory -Force $Root | Out-Null
$StatusFile = "$Root\status.txt"
$EventsFile = "$Root\events.log"
$HistoryFile = "$Root\history.csv"
$SuspendedFile = "$Root\suspended.txt"
$ControlFile = "$Root\control.txt"
$Interval = 5

# name = (warn, critical, resume); "low" checks trigger when the value is BELOW the limits.
$Limits = @{
    cpu_temp     = @{ warn = 92; crit = 98; ok = 85 }
    gpu_temp     = @{ warn = 83; crit = 88; ok = 75 }
    gpu_mem_temp = @{ warn = 95; crit = 100; ok = 85 }
    ssd_temp     = @{ warn = 70; crit = 78; ok = 60 }
    # RAM: pausing processes does not free their memory, so only a true emergency (0.5 GB) pauses everything.
    ram_free_gb  = @{ warn = 3; crit = 0.5; ok = 4; low = $true }
    disk_free_gb = @{ warn = 25; crit = 10; ok = 30; low = $true }
}
$Labels = @{ cpu_temp = 'CPU temperature'; gpu_temp = 'GPU temperature'; gpu_mem_temp = 'GPU memory temperature'; ssd_temp = 'SSD temperature'; ram_free_gb = 'free RAM'; disk_free_gb = 'free disk space' }
# (The degree sign is built in code: Windows PowerShell reads this BOM-less file as ANSI.)
$deg = "$([char]0xB0)C"
$Units = @{ cpu_temp = $deg; gpu_temp = $deg; gpu_mem_temp = $deg; ssd_temp = $deg; ram_free_gb = ' GB'; disk_free_gb = ' GB' }

Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Proc {
    [DllImport("ntdll.dll")] public static extern int NtSuspendProcess(IntPtr h);
    [DllImport("ntdll.dll")] public static extern int NtResumeProcess(IntPtr h);
    [DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint flags);
    [StructLayout(LayoutKind.Sequential)]
    public struct PowerThrottling { public uint Version; public uint ControlMask; public uint StateMask; }
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool SetProcessInformation(IntPtr h, int infoClass, ref PowerThrottling info, int size);
    // Opt a process out of Windows 11 power throttling ("efficiency mode" / EcoQoS), which can
    // move background work onto slow cores when the user is away.
    public static bool NoThrottle(IntPtr h) {
        var s = new PowerThrottling { Version = 1, ControlMask = 0x1 | 0x4, StateMask = 0 };
        return SetProcessInformation(h, 4, ref s, Marshal.SizeOf(typeof(PowerThrottling)));
    }
}
'@

# Keep the PC from idle-sleeping while the lab runs (the display may still turn off).
# ES_CONTINUOUS | ES_SYSTEM_REQUIRED; released automatically when this script stops.
[void][Proc]::SetThreadExecutionState(0x80000001)
$Unthrottled = [Collections.Generic.HashSet[int]]::new()

function Log($msg) {
    $line = "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') $msg"
    Add-Content -Path $EventsFile -Value $line -Encoding utf8
}

function Notify($title, $text) {
    try {
        Add-Type -AssemblyName System.Windows.Forms
        $n = New-Object System.Windows.Forms.NotifyIcon
        $n.Icon = [System.Drawing.SystemIcons]::Warning
        $n.Visible = $true
        $n.ShowBalloonTip(10000, $title, $text, 'Warning')
        Start-Sleep 1
        $n.Dispose()
    } catch {}
}

function Num($s) {
    if ($null -eq $s) { return $null }
    $m = [regex]::Match([string]$s, '-?[0-9]+([.,][0-9]+)?')
    if (-not $m.Success) { return $null }
    [double]::Parse($m.Value.Replace(',', '.'), [Globalization.CultureInfo]::InvariantCulture)
}

$script:Fans = @()
$SeenSpinning = [Collections.Generic.HashSet[string]]::new()
$fanStopTicks = 0

function Read-Sensors {
    $script:Fans = @()
    $r = [ordered]@{}
    $ids = @{}
    try {
        $j = Invoke-RestMethod http://localhost:8085/data.json -TimeoutSec 3
        $stack = [Collections.Stack]::new(); $stack.Push($j)
        while ($stack.Count) {
            $n = $stack.Pop()
            if ($n.SensorId) { $ids[$n.SensorId] = $n.Value }
            if ($n.Type -eq 'Fan' -and $n.SensorId) { $script:Fans += @{ id = $n.SensorId; name = $n.Text; rpm = (Num $n.Value) } }
            foreach ($c in $n.Children) { $stack.Push($c) }
        }
    } catch {}
    $r.cpu_temp = Num $ids['/intelcpu/0/temperature/16']
    $r.cpu_power = Num $ids['/intelcpu/0/power/0']
    $r.cpu_load = Num $ids['/intelcpu/0/load/0']
    $r.gpu_mem_temp = Num $ids['/gpu-nvidia/0/temperature/3']
    $r.ssd_temp = Num $ids['/nvme/0/temperature/0']
    # GPU straight from the driver (works even if LibreHardwareMonitor is closed).
    try {
        $g = (nvidia-smi --query-gpu=temperature.gpu,power.draw,utilization.gpu,memory.used --format=csv,noheader,nounits) -split ','
        $r.gpu_temp = Num $g[0]; $r.gpu_power = Num $g[1]; $r.gpu_load = Num $g[2]; $r.gpu_mem_used_gb = [math]::Round((Num $g[3]) / 1024, 1)
    } catch { $r.gpu_temp = Num $ids['/gpu-nvidia/0/temperature/0'] }
    $os = Get-CimInstance Win32_OperatingSystem
    $r.ram_free_gb = [math]::Round($os.FreePhysicalMemory / 1MB, 1)
    $r.ram_total_gb = [math]::Round($os.TotalVisibleMemorySize / 1MB, 1)
    $r.disk_free_gb = [math]::Round((Get-PSDrive C).Free / 1GB, 1)
    $r
}

# Workload groups. Tournaments include every engine process fastchess started.
function Get-Group($group) {
    switch ($group) {
        'datagen' { @(Get-Process pawnstorm-datagen* -ErrorAction SilentlyContinue) }
        'training' { @(Get-Process trainer -ErrorAction SilentlyContinue) }
        'tournaments' {
            $fc = @(Get-Process fastchess -ErrorAction SilentlyContinue)
            if (-not $fc) { return @() }
            $all = Get-CimInstance Win32_Process
            $ids = [Collections.Generic.HashSet[int]]::new()
            foreach ($p in $fc) { [void]$ids.Add($p.Id) }
            do {
                $added = $false
                foreach ($p in $all) {
                    if ($ids.Contains([int]$p.ParentProcessId) -and $ids.Add([int]$p.ProcessId)) { $added = $true }
                }
            } while ($added)
            @($ids | ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue })
        }
    }
}

$Suspended = @{}   # pid -> group

function Save-Suspended { [IO.File]::WriteAllLines($SuspendedFile, [string[]]($Suspended.Keys | ForEach-Object { "$_" })) }

function Suspend-Group($group) {
    $n = 0
    foreach ($p in Get-Group $group) {
        if ($Suspended.ContainsKey($p.Id)) { continue }
        try { [void][Proc]::NtSuspendProcess($p.Handle); $Suspended[$p.Id] = $group; $n++ } catch {}
    }
    if ($n) { Save-Suspended }
    $n
}

function Resume-All {
    foreach ($id in @($Suspended.Keys)) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p) { try { [void][Proc]::NtResumeProcess($p.Handle) } catch {} }
        $Suspended.Remove($id)
    }
    Save-Suspended
}

# Recover from a previous watchdog that died while processes were suspended.
if (Test-Path $SuspendedFile) {
    foreach ($line in Get-Content $SuspendedFile) {
        $p = Get-Process -Id ([int]$line) -ErrorAction SilentlyContinue
        if ($p) { try { [void][Proc]::NtResumeProcess($p.Handle) } catch {} }
    }
    Remove-Item $SuspendedFile
}
if (-not (Test-Path $ControlFile)) { Set-Content $ControlFile 'auto' }

Log 'watchdog started'
$level = 'ok'           # current enforced level: ok | warn | critical | manual
$warnTicks = 0; $critTicks = 0; $okTicks = 0; $missingTicks = 0
$pauseCount = 0
$lastPause = Get-Date
$reason = ''

while ($true) {
    [void][Proc]::SetThreadExecutionState(0x80000001)
    foreach ($g in 'datagen', 'training', 'tournaments') {
        foreach ($p in Get-Group $g) {
            if ($Unthrottled.Contains($p.Id)) { continue }
            try { if ([Proc]::NoThrottle($p.Handle)) { [void]$Unthrottled.Add($p.Id) } } catch {}
        }
    }
    $s = Read-Sensors
    $control = (Get-Content $ControlFile -ErrorAction SilentlyContinue | Select-Object -First 1)
    $control = if ($control) { $control.Trim().ToLower() } else { 'auto' }

    # Evaluate every limit.
    $worst = 'ok'; $why = @(); $allOk = $true
    foreach ($k in $Limits.Keys) {
        $v = $s[$k]; $L = $Limits[$k]
        if ($null -eq $v) { continue }
        $low = [bool]$L.low
        $isCrit = if ($low) { $v -le $L.crit } else { $v -ge $L.crit }
        $isWarn = if ($low) { $v -le $L.warn } else { $v -ge $L.warn }
        $isOk = if ($low) { $v -ge $L.ok } else { $v -le $L.ok }
        if (-not $isOk) { $allOk = $false }
        if ($isCrit) { $worst = 'critical'; $why += "$($Labels[$k]) $v$($Units[$k])" }
        elseif ($isWarn) { if ($worst -ne 'critical') { $worst = 'warn' }; $why += "$($Labels[$k]) $v$($Units[$k])" }
    }
    # Fan failure: a fan we've seen spinning that now reads ~0 RPM. Headers that never spin are
    # ignored (unconnected), and the GPU fan only counts when the GPU is hot (it stops when idle).
    $stopped = @()
    foreach ($f in $script:Fans) {
        if ($null -eq $f.rpm) { continue }
        if ($f.rpm -gt 300) { [void]$SeenSpinning.Add($f.id) }
        $isGpu = $f.id -like '*gpu*'
        if ($SeenSpinning.Contains($f.id) -and $f.rpm -lt 100 -and (-not $isGpu -or $s.gpu_temp -ge 70)) { $stopped += $f.name }
    }
    $fanStopTicks = if ($stopped) { $fanStopTicks + 1 } else { 0 }
    if ($fanStopTicks -ge 3) {
        $worst = 'critical'; $allOk = $false
        $why += "fan stopped: $($stopped -join ', ')"
    } elseif ($stopped) { $allOk = $false }

    # No CPU temperature at all is treated as a warning: we can't see what's happening.
    if ($null -eq $s.cpu_temp) { $missingTicks++ } else { $missingTicks = 0 }
    if ($missingTicks -ge 12) {
        if ($worst -eq 'ok') { $worst = 'warn' }
        $why += 'CPU temperature unavailable (is LibreHardwareMonitor running?)'
        $allOk = $false
    }

    $critTicks = if ($worst -eq 'critical') { $critTicks + 1 } else { 0 }
    $warnTicks = if ($worst -ne 'ok') { $warnTicks + 1 } else { 0 }
    $okTicks = if ($allOk) { $okTicks + 1 } else { 0 }

    if ($control -eq 'pause') {
        if ($level -ne 'manual') { Log 'manual pause requested'; $level = 'manual'; $reason = 'paused by you' }
        foreach ($g in 'datagen', 'training', 'tournaments') { [void](Suspend-Group $g) }
    } elseif ($control -eq 'resume') {
        Log 'manual resume requested'
        Resume-All; $level = 'ok'; $reason = ''; $okTicks = 0
        Set-Content $ControlFile 'auto'
    } else {
        if ($level -eq 'manual') { Resume-All; $level = 'ok'; $reason = '' }
        # Escalate: critical needs 2 readings (10s), warn needs 6 (30s), so brief spikes are ignored.
        if ($critTicks -ge 2 -and $level -ne 'critical') {
            $level = 'critical'; $reason = $why -join ', '; $pauseCount++; $lastPause = Get-Date
            $n = 0; foreach ($g in 'datagen', 'training', 'tournaments') { $n += Suspend-Group $g }
            Log "CRITICAL: paused everything ($n processes) - $reason"
            Notify 'Pawnstorm: everything paused' "Critical: $reason"
        } elseif ($warnTicks -ge 6 -and $level -eq 'ok') {
            $level = 'warn'; $reason = $why -join ', '; $pauseCount++; $lastPause = Get-Date
            $n = Suspend-Group 'datagen'
            Log "WARNING: paused data generation ($n processes) - $reason"
            Notify 'Pawnstorm: data generation paused' $reason
        }
        # Keep newly started processes paused too while a pause is in force.
        if ($level -eq 'critical') { foreach ($g in 'datagen', 'training', 'tournaments') { [void](Suspend-Group $g) } }
        elseif ($level -eq 'warn') { [void](Suspend-Group 'datagen') }
        # Two quiet hours forget earlier pauses, so one hot spell doesn't make every later cool-down 20 minutes.
        if ($level -eq 'ok' -and $pauseCount -gt 0 -and ((Get-Date) - $lastPause).TotalHours -ge 2) { $pauseCount = 0 }
        # Resume after a cool-down that grows if pauses keep recurring (60s, 120s, 240s, ... max 20 min).
        $needed = [math]::Min(240, 12 * [math]::Pow(2, [math]::Max(0, $pauseCount - 1)))
        if ($level -in 'warn', 'critical' -and $okTicks -ge $needed) {
            Resume-All
            Log "resumed: all readings back to normal for $([math]::Round($needed * $Interval / 60, 1)) min"
            $level = 'ok'; $reason = ''
        }
    }

    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $groups = ($Suspended.Values | Sort-Object -Unique) -join ','
    $lines = @("updated=$now", "level=$level", "control=$control", "reason=$reason", "warn_now=$($why -join ', ')", "paused_groups=$groups", "paused_processes=$($Suspended.Count)", "pause_count=$pauseCount")
    foreach ($k in $s.Keys) { $lines += "$k=$($s[$k])" }
    # Spinning fans as "name:rpm" pairs (headers that have never spun are left out).
    $lines += "fans=$(($script:Fans | Where-Object { $SeenSpinning.Contains($_.id) } | ForEach-Object { "$($_.name):$([int]$_.rpm)" }) -join '|')"
    foreach ($k in $Limits.Keys) { $lines += "limit_${k}=$($Limits[$k].warn),$($Limits[$k].crit)" }
    [IO.File]::WriteAllLines("$StatusFile.tmp", [string[]]$lines)
    Move-Item -Force "$StatusFile.tmp" $StatusFile

    Add-Content -Path $HistoryFile -Value "$now,$($s.cpu_temp),$($s.gpu_temp),$($s.cpu_power),$($s.gpu_power),$($s.ssd_temp),$($s.ram_free_gb)"
    if ((Get-Item $HistoryFile).Length -gt 2MB) {
        $keep = Get-Content $HistoryFile | Select-Object -Last 5000
        [IO.File]::WriteAllLines($HistoryFile, [string[]]$keep)
    }
    # Engines and tournaments always run below normal priority, so the PC stays usable (videos,
    # games) even with every thread busy; they just get less CPU while something else needs it.
    Get-Process fastchess, pawnstorm*, stockfish* -ErrorAction SilentlyContinue | Where-Object { $_.PriorityClass -eq 'Normal' } |
        ForEach-Object { try { $_.PriorityClass = 'BelowNormal' } catch {} }
    Start-Sleep $Interval
}
