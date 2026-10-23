# Lightweight isolated regression test; no lab files or bullet-utils needed.
$ErrorActionPreference='Stop'
$root = Join-Path (Resolve-Path "$PSScriptRoot\..\..") '.scratchpad\datagen2\prepare-test'
New-Item -ItemType Directory -Force "$root\a", "$root\b", "$root\out" | Out-Null
$fen = 'rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1'
$lines = @(for ($i=0; $i -lt 1000; $i++) { "$fen | $i | 1.0" })
[IO.File]::WriteAllText("$root\a\data_00.txt", ($lines -join "`r`n")+"`r`npartial")
$stream=[IO.File]::Create("$root\b\data_00.txt.gz")
$gzip=[IO.Compression.GZipStream]::new($stream,[IO.Compression.CompressionMode]::Compress)
$bytes=[Text.Encoding]::UTF8.GetBytes((($lines | ForEach-Object { $_.Replace('1.0','0.0') }) -join "`n")+"`n")
$gzip.Write($bytes,0,$bytes.Length); $gzip.Dispose(); $stream.Dispose()
foreach ($fallback in @($true,$false)) {
    & "$PSScriptRoot\..\prepare-data.ps1" -Sources a,b -Name test -DataRoot $root -OutputDirectory "$root\out" -SnapshotOnly -ForceFallback:$fallback -SampleRate 0.5 -MaxPositions 200
    $data=@(Get-Content "$root\out\test.txt")
    if ($data.Count -ne 200 -or @($data | Where-Object { $_.EndsWith('1.0') }).Count -ne 100) { throw 'source-order bias or sample/cap failure' }
    # Sampling must reach the end of each source, rather than select its first N records.
    if (@($data | Where-Object { [int]($_.Split('|')[1]) -gt 800 }).Count -lt 10) { throw 'prefix-biased sample' }
    & "$PSScriptRoot\..\prepare-data.ps1" -Sources a,b -Name quota -DataRoot $root -OutputDirectory "$root\out" -SnapshotOnly -ForceFallback:$fallback -MaxPositions 200 -Quota 0.95,0.05
    $data=@(Get-Content "$root\out\quota.txt")
    if ($data.Count -ne 200 -or @($data | Where-Object { $_.EndsWith('0.0') }).Count -ne 10) { throw 'quota failure' }
    & "$PSScriptRoot\..\prepare-data.ps1" -Sources a,b -Name rate -DataRoot $root -OutputDirectory "$root\out" -SnapshotOnly -ForceFallback:$fallback -SampleRate 0.1
    if (@(Get-Content "$root\out\rate.txt").Count -ne 200) { throw 'SampleRate ignored' }
}
'prepare-data tests passed (fallback and native paths)'
