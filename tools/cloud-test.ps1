# Runs a fixed-length test match on GitHub-hosted runners (.github/workflows/test-match.yml) and stores the
# combined result like a local run (testing\runs\<date>_<name>: meta.txt, output.txt, games.pgn).
# Specs are the usual testing\engines\<spec>.nnue + .options; -DevRef / -BaseRef are the git refs to build.
#   .\tools\cloud-test.ps1 -Name 'Sprint 1: lmrres (cloud)' -Dev pawnstorm-s1-lmrres -DevRef sprint1 -Base pawnstorm-v1.14 -BaseRef w768
param(
    [Parameter(Mandatory)][string]$Name,
    [Parameter(Mandatory)][string]$Dev,
    [Parameter(Mandatory)][string]$DevRef,
    [Parameter(Mandatory)][string]$Base,
    [Parameter(Mandatory)][string]$BaseRef,
    [string]$TC = '8+0.08',
    [int]$Rounds = 300,
    [int]$Jobs = 6,
    [string]$Notes = ''
)
. "$PSScriptRoot\paths.ps1"
$ErrorActionPreference = 'Stop'
Set-Location $RepoRoot
$tmp = Join-Path $LabRoot "cloud-tests\$([DateTimeOffset]::Now.ToUnixTimeSeconds())"
New-Item -ItemType Directory -Force $tmp | Out-Null

# 1. Upload both specs (network, options, commit to build) to the testkit release.
foreach ($s in @(@($Dev, $DevRef), @($Base, $BaseRef))) {
    $spec, $ref = $s
    $sha = (git rev-parse "$ref^{commit}").Trim()
    if (-not (git ls-remote origin | Select-String $sha -Quiet)) { git push -q origin "$ref" 2>$null }
    Set-Content "$tmp\$spec.ref" $sha -NoNewline -Encoding ascii
    Copy-Item "$LabRoot\engines\$spec.nnue", "$LabRoot\engines\$spec.options" $tmp
    gh release upload testkit "$tmp\$spec.nnue" "$tmp\$spec.options" "$tmp\$spec.ref" --clobber | Out-Null
}

# 2. Start the run and find its id.
$before = (Get-Date).ToUniversalTime()
gh workflow run test-match.yml -f dev=$Dev -f base=$Base -f tc=$TC -f rounds=$Rounds -f jobs=$Jobs -f label=$Name | Out-Null
$id = $null
for ($i = 0; $i -lt 30 -and -not $id; $i++) {
    Start-Sleep 10
    $runs = gh run list --workflow test-match.yml -L 5 --json databaseId,createdAt,displayTitle | ConvertFrom-Json
    $id = ($runs | Where-Object { [datetime]$_.createdAt -ge $before.AddSeconds(-30) -and $_.displayTitle -like "*$Dev vs $Base*" } | Select-Object -First 1).databaseId
}
if (-not $id) { throw 'could not find the started run' }
"cloud run $id started"

# 3. Wait for it, then combine the shards.
do { Start-Sleep 120; $st = (gh run view $id --json status | ConvertFrom-Json).status } while ($st -ne 'completed')
gh run download $id -D "$tmp\shards" | Out-Null
$runDir = Join-Path $LabRoot "runs\$(Get-Date -Format 'yyyy-MM-dd_HHmmss')_$(($Name -replace '[^A-Za-z0-9.]+', '-').Trim('-'))"
New-Item -ItemType Directory -Force $runDir | Out-Null
$p = @(0, 0, 0, 0, 0); $w = 0; $d = 0; $l = 0
$pgn = New-Object System.Text.StringBuilder
foreach ($sh in Get-ChildItem "$tmp\shards" -Directory) {
    $out = Get-Content "$($sh.FullName)\out.txt" -Raw -ErrorAction SilentlyContinue
    if (-not $out) { continue }
    $pm = [regex]::Matches($out, 'Ptnml\(0-2\): \[(\d+), (\d+), (\d+), (\d+), (\d+)\]')
    $gm = [regex]::Matches($out, 'Games: \d+, Wins: (\d+), Losses: (\d+), Draws: (\d+)')
    if ($pm.Count -gt 0 -and $gm.Count -gt 0) {
        $last = $pm[$pm.Count - 1]; for ($k = 0; $k -lt 5; $k++) { $p[$k] += [int]$last.Groups[$k + 1].Value }
        $g = $gm[$gm.Count - 1]; $w += [int]$g.Groups[1].Value; $l += [int]$g.Groups[2].Value; $d += [int]$g.Groups[3].Value
    }
    if (Test-Path "$($sh.FullName)\games.pgn") { [void]$pgn.Append([IO.File]::ReadAllText("$($sh.FullName)\games.pgn")) }
}
[IO.File]::WriteAllText("$runDir\games.pgn", $pgn.ToString())
# Elo from the pentanomial (game pairs scored 0, 0.25, 0.5, 0.75, 1).
$pairs = ($p | Measure-Object -Sum).Sum
$elo = 0; $err = 0
if ($pairs -gt 0) {
    $mean = (0 * $p[0] + 0.25 * $p[1] + 0.5 * $p[2] + 0.75 * $p[3] + 1 * $p[4]) / $pairs
    $var = ((0 - $mean) * (0 - $mean) * $p[0] + (0.25 - $mean) * (0.25 - $mean) * $p[1] + (0.5 - $mean) * (0.5 - $mean) * $p[2] + (0.75 - $mean) * (0.75 - $mean) * $p[3] + (1 - $mean) * (1 - $mean) * $p[4]) / $pairs
    $m = [math]::Min(0.999, [math]::Max(0.001, $mean))
    $elo = -400 * [math]::Log10(1 / $m - 1)
    $err = 1.96 * [math]::Sqrt($var / $pairs) * 400 / ([math]::Log(10) * $m * (1 - $m))
}
$games = $w + $d + $l
@(
    "Results of $Dev vs $Base ($TC, cloud run $id, $Jobs jobs x $Rounds pairs):",
    ('Elo: {0:N2} +/- {1:N2}' -f $elo, $err),
    "Games: $games, Wins: $w, Losses: $l, Draws: $d, Points: $($w + $d / 2) ($([math]::Round(100 * ($w + $d / 2) / [math]::Max(1, $games), 2)) %)",
    "Ptnml(0-2): [$($p -join ', ')]",
    'Finished match'
) | Set-Content "$runDir\output.txt" -Encoding ascii
@("name=$Name", 'type=gauntlet', "tc=$TC", "total=$games", "main=$Dev", "created=$([DateTimeOffset]::Now.ToUnixTimeSeconds())", "concurrency=cloud", "notes=$Notes (GitHub Actions run $id)") |
    Set-Content "$runDir\meta.txt" -Encoding ascii
Get-Content "$runDir\output.txt"
