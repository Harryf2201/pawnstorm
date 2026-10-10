# Snapshots self-play text data (safe while datagen is still writing), converts it to
# bulletformat and shuffles it, ready for the trainer.
# Usage: .\tools\prepare-data.ps1 -Sources gen1-pesto-5k -Name gen1
param(
    [Parameter(Mandatory)][string[]]$Sources,
    [Parameter(Mandatory)][string]$Name,
    [int]$ShuffleMemMb = 8000,
    # Stop after this many positions (0 = use everything).
    [int64]$MaxPositions = 0
)

. "$PSScriptRoot\paths.ps1"
$Root = $LabRoot
$Utils = "$BulletRoot\target\release\bullet-utils.exe"
$out = "$Root\training\data"
New-Item -ItemType Directory -Force $out | Out-Null
$text = "$out\$Name.txt"
$raw = "$out\$Name.raw.bin"
$final = "$out\$Name.bin"

$files = @(foreach ($src in $Sources) {
    Get-ChildItem "$Root\datagen\$src" -File | Where-Object { $_.Name -like 'data_*.txt' -or $_.Name -like 'data_*.txt.gz' }
})
$bash = "$env:ProgramFiles\Git\bin\bash.exe"
$count = 0
if (Test-Path $bash) {
    # Fast path (GB/s): Git's cat/zcat/grep/head. Only complete, well-formed lines are kept, so a
    # half-written last line in a file that is still growing can never get in.
    $q = { param($p) "'" + ($p -replace '\\', '/') + "'" }
    $plain = ($files | Where-Object { $_.Extension -eq '.txt' } | ForEach-Object { & $q $_.FullName }) -join ' '
    $gzips = ($files | Where-Object { $_.Extension -eq '.gz' } | ForEach-Object { & $q $_.FullName }) -join ' '
    $limit = if ($MaxPositions -gt 0) { "head -n $MaxPositions" } else { 'cat' }
    $script = "$out\prep-$Name.sh"
    $lines = @(
        "{ $(if ($plain) { "cat $plain;" }) $(if ($gzips) { "zcat $gzips;" }) } 2>/dev/null |",
        "  grep -aE '^[^|]+ [wb] [^|]+ \| -?[0-9]+ \| (1\.0|0\.5|0\.0)$' | $limit > $(& $q $text)",
        "wc -l < $(& $q $text)"
    )
    [IO.File]::WriteAllText($script, ($lines -join "`n") + "`n")
    $count = [int64](& $bash $script | Select-Object -Last 1)
    Remove-Item $script
    "Snapshot: $count positions"
    & $Utils convert --from text --input $text --output $raw
    & $Utils shuffle --input $raw --mem-used-mb $ShuffleMemMb --output $final
    Remove-Item $text, $raw
    "Ready: $final ($([math]::Round((Get-Item $final).Length / 1MB)) MB)"
    return
}

# Fallback without Git Bash: slower line-by-line copy. Local files may still be growing, so
# their last line is dropped in case it is half-written; cloud files (.txt.gz) are complete.
$writer = [IO.StreamWriter]::new($text, $false, [Text.UTF8Encoding]::new($false))
foreach ($src in $Sources) {
    foreach ($f in Get-ChildItem "$Root\datagen\$src" -File | Where-Object { $_.Name -like 'data_*.txt' -or $_.Name -like 'data_*.txt.gz' }) {
        $stream = [IO.FileStream]::new($f.FullName, 'Open', 'Read', 'ReadWrite')
        $gz = $f.Name.EndsWith('.gz')
        if ($gz) { $stream = [IO.Compression.GZipStream]::new($stream, [IO.Compression.CompressionMode]::Decompress) }
        $reader = [IO.StreamReader]::new($stream)
        $prev = $null
        while ($null -ne ($line = $reader.ReadLine())) {
            if ($MaxPositions -gt 0 -and $count -ge $MaxPositions) { break }
            if ($null -ne $prev) { $writer.WriteLine($prev); $count++ }
            $prev = $line
        }
        if ($gz -and $null -ne $prev -and ($MaxPositions -le 0 -or $count -lt $MaxPositions)) { $writer.WriteLine($prev); $count++ }
        $reader.Close()
        if ($MaxPositions -gt 0 -and $count -ge $MaxPositions) { break }
    }
    if ($MaxPositions -gt 0 -and $count -ge $MaxPositions) { break }
}
$writer.Close()
"Snapshot: $count positions"

& $Utils convert --from text --input $text --output $raw
& $Utils shuffle --input $raw --mem-used-mb $ShuffleMemMb --output $final
Remove-Item $text, $raw
"Ready: $final ($([math]::Round((Get-Item $final).Length / 1MB)) MB)"
