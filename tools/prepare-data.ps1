# Snapshots self-play text data (safe while datagen is still writing), converts it to
# bulletformat and shuffles it, ready for the trainer.
# Usage: .\tools\prepare-data.ps1 -Sources gen1-pesto-5k -Name gen1
param(
    [Parameter(Mandatory)][string[]]$Sources,
    [Parameter(Mandatory)][string]$Name,
    [int]$ShuffleMemMb = 8000
)

$Root = 'C:\Users\you\Claude\testing'
$Utils = 'C:\Users\you\Claude\bullet\target\release\bullet-utils.exe'
$out = "$Root\training\data"
New-Item -ItemType Directory -Force $out | Out-Null
$text = "$out\$Name.txt"
$raw = "$out\$Name.raw.bin"
$final = "$out\$Name.bin"

# Concatenate every data file, dropping each file's last line in case it is half-written.
$writer = [IO.StreamWriter]::new($text, $false, [Text.UTF8Encoding]::new($false))
$count = 0
foreach ($src in $Sources) {
    foreach ($f in Get-ChildItem "$Root\datagen\$src\data_*.txt") {
        $stream = [IO.FileStream]::new($f.FullName, 'Open', 'Read', 'ReadWrite')
        $reader = [IO.StreamReader]::new($stream)
        $prev = $null
        while ($null -ne ($line = $reader.ReadLine())) {
            if ($null -ne $prev) { $writer.WriteLine($prev); $count++ }
            $prev = $line
        }
        $reader.Close()
    }
}
$writer.Close()
"Snapshot: $count positions"

& $Utils convert --from text --input $text --output $raw
& $Utils shuffle --input $raw --mem-used-mb $ShuffleMemMb --output $final
Remove-Item $text, $raw
"Ready: $final ($([math]::Round((Get-Item $final).Length / 1MB)) MB)"
