# Snapshot complete training records, select an unbiased mix, convert and shuffle.
param(
    [Parameter(Mandatory)][string[]]$Sources,
    [Parameter(Mandatory)][string]$Name,
    [int]$ShuffleMemMb = 8000,
    [ValidateRange(0, [long]::MaxValue)][int64]$MaxPositions = 0,
    [ValidateRange(0.0, 1.0)][double]$SampleRate = 1.0,
    # Shares in Sources order, summing to 1. Requires MaxPositions; shortages underfill.
    [double[]]$Quota,
    # Also useful for isolated verification without touching the lab.
    [string]$DataRoot,
    [string]$OutputDirectory,
    [switch]$SnapshotOnly,
    [switch]$ForceFallback
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\paths.ps1"
if (-not $DataRoot) { $DataRoot = "$LabRoot\datagen" }
if (-not $OutputDirectory) { $OutputDirectory = "$LabRoot\training\data" }
if ($Quota) {
    if ($Quota.Count -ne $Sources.Count -or $MaxPositions -le 0 -or @($Quota | Where-Object { $_ -lt 0 -or [double]::IsNaN($_) }).Count -gt 0 -or [math]::Abs(($Quota | Measure-Object -Sum).Sum - 1) -gt 0.000001) {
        throw 'Quota must contain one nonnegative share per source, sum to 1, and requires MaxPositions.'
    }
}
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$text = "$OutputDirectory\$Name.txt"
$raw = "$OutputDirectory\$Name.raw.bin"
$final = "$OutputDirectory\$Name.bin"
$Utils = "$BulletRoot\target\release\bullet-utils.exe"
$bash = "$env:ProgramFiles\Git\bin\bash.exe"
$fast = (Test-Path $bash) -and -not $ForceFallback
$pattern = '^[^|]+ [wb] [^|]+ \| -?[0-9]+ \| (1\.0|0\.5|0\.0)$'
# Freeze file lengths: both passes see the same snapshot, even while datagen appends.
$groups = @(foreach ($src in $Sources) {
    $files = @(Get-ChildItem (Join-Path $DataRoot $src) -File | Where-Object { $_.Name -like 'data_*.txt' -or $_.Name -like 'data_*.txt.gz' } | Sort-Object Name | ForEach-Object {
        [pscustomobject]@{ FullName=$_.FullName; Length=$_.Length; Gzip=$_.Name.EndsWith('.gz') }
    })
    [pscustomobject]@{ Source=$src; Files=$files; Count=[long]0; Take=[long]0 }
})
# Shell quote paths (including apostrophes); all numeric script values come from validated types.
$q = { param($p) "'" + ($p.Replace('\','/').Replace("'", "'\''")) + "'" }
if (-not ('PawnstormSnapshot' -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.IO;
using System.IO.Compression;
using System.Collections.Generic;
using System.Text;
public static class PawnstormSnapshot {
    public static IEnumerable<string> Lines(string path, long length, bool gzip) {
        using (var file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite))
        using (Stream stream = gzip ? (Stream)new GZipStream(file, CompressionMode.Decompress) : file) {
            byte[] buffer = new byte[65536];
            var line = new StringBuilder();
            while (gzip || length > 0) {
                int count = stream.Read(buffer, 0, gzip ? buffer.Length : (int)Math.Min(length, buffer.Length));
                if (count == 0) break;
                length -= count;
                for (int i=0; i<count; i++) {
                    if (buffer[i] == 10) { yield return line.ToString().TrimEnd('\r'); line.Clear(); }
                    else line.Append((char)buffer[i]);
                }
            }
        }
    }
}
"@
}
function Invoke-Lines($group, $action) {
    foreach ($f in $group.Files) {
        foreach ($line in [PawnstormSnapshot]::Lines($f.FullName, $f.Length, $f.Gzip)) {
            if ($line -cmatch $pattern) { & $action $line }
        }
    }
}
$script = "$OutputDirectory\prep-$Name.sh"
function Source-Pipe($group) {
    $cmds = @(foreach ($f in $group.Files) {
        if ($f.Gzip) { "gzip -dc -- $(& $q $f.FullName) | perl -ne 'print if /\n$/';" }
        else { 'head -c ' + $f.Length + ' -- ' + (& $q $f.FullName) + ' | perl -ne ''print if /\n$/'';' }
    })
    '{ :; ' + ($cmds -join ' ') + " } | tr -d '\r' | awk '/$pattern/'"
}
try {
    foreach ($g in $groups) {
        if ($fast) {
            [IO.File]::WriteAllText($script, "set -eo pipefail`n$(Source-Pipe $g) | wc -l`n")
            $g.Count = [long](& $bash $script | Select-Object -Last 1)
            if ($LASTEXITCODE -ne 0) { throw "Snapshot count failed: $($g.Source)" }
        } else {
            $counter = @{ Value=[long]0 }
            Invoke-Lines $g { param($line) $counter.Value++ }
            $g.Count = $counter.Value
        }
    }
    $total = [long](($groups | Measure-Object Count -Sum).Sum)
    $budget = [long][math]::Floor($total * $SampleRate)
    if ($MaxPositions -gt 0) { $budget = [math]::Min($budget, $MaxPositions) }
    # Largest-remainder allocation avoids source-order bias from the global head limit.
    $allocated = [long]0
    $fractions = @()
    for ($i=0; $i -lt $groups.Count; $i++) {
        $g=$groups[$i]
        $share = if ($Quota) { $Quota[$i] } elseif ($total -gt 0) { $g.Count / [double]$total } else { 0 }
        $target = $budget * $share
        $g.Take = [math]::Min([long][math]::Floor($g.Count * $SampleRate), [long][math]::Floor($target))
        $allocated += $g.Take
        $fractions += [pscustomobject]@{ Index=$i; Fraction=$target-[math]::Floor($target) }
    }
    foreach ($f in ($fractions | Sort-Object Fraction -Descending)) {
        if ($allocated -ge $budget) { break }
        $g=$groups[$f.Index]
        if ($f.Fraction -gt 0 -and $g.Take -lt [math]::Floor($g.Count*$SampleRate)) { $g.Take++; $allocated++ }
    }
    [IO.File]::WriteAllText($text, '')
    $writer = if (-not $fast) { [IO.StreamWriter]::new($text, $false, [Text.UTF8Encoding]::new($false)) }
    try {
        for ($i=0; $i -lt $groups.Count; $i++) {
            $g=$groups[$i]
            "Source $($g.Source): $($g.Take) / $($g.Count) positions"
            if ($g.Take -eq 0) { continue }
            if ($fast) {
                $awk = 'awk -v n=' + $g.Take + ' -v left=' + $g.Count + ' ''BEGIN { srand(' + (20261023+$i) + ') } { if (rand()*left < n) { print; n-- } left-- } END { if(n != 0) exit 2 }'''
                [IO.File]::WriteAllText($script, "set -eo pipefail`n$(Source-Pipe $g) | $awk >> $(& $q $text)`n")
                & $bash $script
                if ($LASTEXITCODE -ne 0) { throw 'Snapshot selection failed' }
            } else {
                $rng = [Random]::new(20261023+$i)
                $state = @{ Left=$g.Count; Need=$g.Take }
                Invoke-Lines $g { param($line)
                    if ($rng.NextDouble()*$state.Left -lt $state.Need) { $writer.WriteLine($line); $state.Need-- }
                    $state.Left--
                }
                if ($state.Need -ne 0) { throw 'Source changed between passes' }
            }
        }
    } finally { if ($writer) { $writer.Dispose() } }
} finally { if (Test-Path $script) { Remove-Item -LiteralPath $script } }
"Snapshot: $allocated positions"
if ($SnapshotOnly) { return }
& $Utils convert --from text --input $text --output $raw
if ($LASTEXITCODE -ne 0) { throw 'bullet conversion failed' }
Remove-Item -LiteralPath $text
& $Utils shuffle --input $raw --mem-used-mb $ShuffleMemMb --output $final
if ($LASTEXITCODE -ne 0) { throw 'bullet shuffle failed' }
Remove-Item -LiteralPath $raw
"Ready: $final ($([math]::Round((Get-Item $final).Length / 1MB)) MB)"
