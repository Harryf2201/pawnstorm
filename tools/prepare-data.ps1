# Snapshots self-play text data (safe while datagen is still writing), converts it to
# bulletformat and shuffles it, ready for the trainer.
# Usage: .\tools\prepare-data.ps1 -Sources gen1-pesto-5k -Name gen1
param(
    [Parameter(Mandatory)][string[]]$Sources,
    [Parameter(Mandatory)][string]$Name,
    [int]$ShuffleMemMb = 8000
)

. "$PSScriptRoot\paths.ps1"
$Root = $LabRoot
$Utils = "$BulletRoot\target\release\bullet-utils.exe"
$out = "$Root\training\data"
New-Item -ItemType Directory -Force $out | Out-Null
$text = "$out\$Name.txt"
$raw = "$out\$Name.raw.bin"
$final = "$out\$Name.bin"

# Concatenate every data file. Local files may still be growing, so their last line is dropped
# in case it is half-written; cloud files (.txt.gz) are complete.
$writer = [IO.StreamWriter]::new($text, $false, [Text.UTF8Encoding]::new($false))
$count = 0
foreach ($src in $Sources) {
    foreach ($f in Get-ChildItem "$Root\datagen\$src" -File | Where-Object { $_.Name -like 'data_*.txt' -or $_.Name -like 'data_*.txt.gz' }) {
        $stream = [IO.FileStream]::new($f.FullName, 'Open', 'Read', 'ReadWrite')
        $gz = $f.Name.EndsWith('.gz')
        if ($gz) { $stream = [IO.Compression.GZipStream]::new($stream, [IO.Compression.CompressionMode]::Decompress) }
        $reader = [IO.StreamReader]::new($stream)
        $prev = $null
        while ($null -ne ($line = $reader.ReadLine())) {
            if ($null -ne $prev) { $writer.WriteLine($prev); $count++ }
            $prev = $line
        }
        if ($gz -and $null -ne $prev) { $writer.WriteLine($prev); $count++ }
        $reader.Close()
    }
}
$writer.Close()
"Snapshot: $count positions"

& $Utils convert --from text --input $text --output $raw
& $Utils shuffle --input $raw --mem-used-mb $ShuffleMemMb --output $final
Remove-Item $text, $raw
"Ready: $final ($([math]::Round((Get-Item $final).Length / 1MB)) MB)"
