param([string]$Engine = "$PSScriptRoot\..\..\target\release\pawnstorm.exe")
$ErrorActionPreference='Stop'
[Diagnostics.Process]::GetCurrentProcess().PriorityClass='BelowNormal'
$root=Join-Path (Resolve-Path "$PSScriptRoot\..\..") '.scratchpad\datagen2\rescore-test'
New-Item -ItemType Directory -Force $root | Out-Null
$inputFile="$root\input.txt"
$outputFile="$root\output.txt"
# Remove only this test's prior artifacts, allowing the test to be rerun.
foreach ($name in @('output.txt','output.rescore.manifest','output.rescore.checkpoint')) {
    $path=Join-Path $root $name
    if (Test-Path $path) { Remove-Item -LiteralPath $path }
}
$fen='rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1'
$check='4k3/8/8/8/8/8/4r3/4K3 w - - 0 1'
$lines=@("$fen | 12 | 1.0", "$fen | -14 | 0.0", "$fen | 20 | 0.5", "$fen | 32000 | 1.0", "$check | 2 | 0.0")
$lines += @('7k/5Q2/6K1/8/8/8/8/8 w - - 0 1 | 0 | 0.5')
$lines += @(for($i=0; $i -lt 20; $i++) { "$fen | $i | 0.5" })
[IO.File]::WriteAllText($inputFile, ($lines -join "`n")+"`n")
& $Engine rescore $inputFile $outputFile --nodes 100 --threads 2 --blend 0.5
if ($LASTEXITCODE -ne 0) { throw 'rescore failed' }
$data=@(Get-Content $outputFile)
if ($data.Count -ne 23) { throw 'check/mate filtering failed' }
for ($i=0; $i -lt 3; $i++) { if ($data[$i].Split('|')[2] -ne $lines[$i].Split('|')[2]) { throw 'result changed' } }
$before=[IO.File]::ReadAllText($outputFile)
# Model a crash after output flush but before checkpoint publication.
[IO.File]::AppendAllText($outputFile, 'torn tail')
& $Engine rescore $inputFile $outputFile --nodes 100 --threads 2 --blend 0.5
if ($LASTEXITCODE -ne 0 -or [IO.File]::ReadAllText($outputFile) -ne $before) { throw 'resume failed' }
# Simulate a previously committed prefix and a crash during later work.
$firstBytes=[Text.Encoding]::UTF8.GetByteCount($data[0]+"`n")
[IO.File]::WriteAllText("$root\output.rescore.checkpoint", "1 1 $firstBytes`n")
& $Engine rescore $inputFile $outputFile --nodes 100 --threads 2 --blend 0.5
if ($LASTEXITCODE -ne 0 -or [IO.File]::ReadAllText($outputFile) -ne $before) { throw 'prefix resume differs' }
# Gzip input uses the same text parser and preserves results.
$stream=[IO.File]::Create("$root\input.txt.gz")
$gzip=[IO.Compression.GZipStream]::new($stream,[IO.Compression.CompressionMode]::Compress)
$bytes=[IO.File]::ReadAllBytes($inputFile); $gzip.Write($bytes,0,$bytes.Length); $gzip.Dispose(); $stream.Dispose()
foreach ($name in @('gzip.txt','gzip.rescore.manifest','gzip.rescore.checkpoint')) {
    $path=Join-Path $root $name
    if (Test-Path $path) { Remove-Item -LiteralPath $path }
}
& $Engine rescore "$root\input.txt.gz" "$root\gzip.txt" --nodes 100 --threads 2 --blend 0.5
if ($LASTEXITCODE -ne 0 -or [IO.File]::ReadAllText("$root\gzip.txt") -ne $before) { throw 'gzip differs' }
'rescore tests passed (format, results, filtering, gzip, crash recovery)'
