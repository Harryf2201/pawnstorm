# Starts a fastchess tournament in the background, laid out for the Pawnstorm Lab dashboard.
#
# Engine specs:
#   sf:2400            Stockfish limited to UCI_Elo 2400 (used as a rating anchor)
#   stockfish, dragon, rubichess, plentychess   full-strength engines in Downloads
#   <name> with <name>.nnue + <name>.runner   that network on the build named in .runner
#   (any of ours may also have <name>.options: extra UCI options, one Name=Value per line)
#   anything else      testing\engines\<name>.exe (e.g. pawnstorm-v0.1)
#
# Examples:
#   .\tools\match.ps1 -Name "v0.2 rating" -Type gauntlet -Engines pawnstorm-v0.2,sf:2000,sf:2400,sf:2800 -Rounds 50 -Live
#   .\tools\match.ps1 -Name "lmr tweak" -Type sprt -Engines pawnstorm-dev,pawnstorm-base -TC 8+0.08
param(
    [Parameter(Mandatory)][string]$Name,
    [ValidateSet('gauntlet', 'sprt', 'roundrobin')][string]$Type = 'gauntlet',
    [Parameter(Mandatory)][string[]]$Engines,
    [string]$TC = '10+0.1',
    [int]$Rounds = 0,
    [int]$Concurrency = 8,
    [string]$Book = '8moves_v3.pgn',
    [string]$Sprt = '0,3',
    [int]$Hash = 16,
    [switch]$Live,
    # Gauntlet only: the first N engines each play every other engine, but not each other.
    [int]$Seeds = 1,
    [string]$Notes = ''
)

. "$PSScriptRoot\paths.ps1"
$Root = $LabRoot
$Fastchess = "$Root\fastchess\fastchess-windows-x86-64\fastchess.exe"

$fcArgs = @()
$anchors = @()
$names = @()
foreach ($spec in $Engines) {
    if ($spec -match '^sf:(\d+)$') {
        $elo = $Matches[1]; $n = "SF-$elo"
        $fcArgs += '-engine', "cmd=$($Known.stockfish)", "name=$n", 'option.UCI_LimitStrength=true', "option.UCI_Elo=$elo"
        $anchors += "anchor=${n}:$elo"
    } elseif ($Known.ContainsKey($spec)) {
        $n = $spec
        $fcArgs += '-engine', "cmd=$($Known[$spec])", "name=$n"
    } elseif ((Test-Path "$Root\engines\$spec.nnue") -and (Test-Path "$Root\engines\$spec.runner")) {
        # A network tested on a particular search: <spec>.runner names the build (e.g. pawnstorm-v1.5).
        $n = $spec
        $runner = (Get-Content "$Root\engines\$spec.runner" -TotalCount 1).Trim()
        $fcArgs += '-engine', "cmd=$Root\engines\$runner.exe", "name=$n", "option.EvalFile=$Root\engines\$spec.nnue"
    } elseif ((Test-Path "$Root\engines\$spec.nnue") -and -not (Test-Path "$Root\engines\$spec.exe")) {
        # (A version with its own build, e.g. a different network architecture, always runs its .exe:
        # the shared build can only read standard networks.)
        # A network-only version: the shared NNUE build loads this version's network file.
        $n = $spec
        $fcArgs += '-engine', "cmd=$Root\engines\pawnstorm-nnue.exe", "name=$n", "option.EvalFile=$Root\engines\$spec.nnue"
    } else {
        $path = "$Root\engines\$spec.exe"
        if (-not (Test-Path $path)) { throw "Engine not found: $path" }
        $n = $spec
        $fcArgs += '-engine', "cmd=$path", "name=$n"
    }
    # Extra UCI options for one of our engines: <spec>.options, one "Name=Value" per line
    # (e.g. SyzygyPath=C:\...\syzygy).
    if (Test-Path "$Root\engines\$spec.options") {
        foreach ($o in Get-Content "$Root\engines\$spec.options" | Where-Object { $_ -match '=' }) { $fcArgs += "option.$($o.Trim())" }
    }
    $names += $n
}

$n = $names.Count
if ($Rounds -eq 0) { $Rounds = if ($Type -eq 'sprt') { 20000 } else { 50 } }
$total = switch ($Type) {
    'gauntlet' { $Rounds * 2 * $Seeds * ($n - $Seeds) }
    'roundrobin' { $Rounds * 2 * $n * ($n - 1) / 2 }
    'sprt' { $Rounds * 2 }
}

$stamp = Get-Date -Format 'yyyy-MM-dd_HHmmss'
$slug = ($Name -replace '[^A-Za-z0-9.-]+', '-').Trim('-')
$dir = "$Root\runs\${stamp}_$slug"
New-Item -ItemType Directory -Force $dir | Out-Null

$bookPath = "$Root\books\$Book"
$format = if ($Book -like '*.epd') { 'epd' } else { 'pgn' }
$fcArgs += '-each', "tc=$TC", "option.Hash=$Hash"
$fcArgs += '-openings', "file=$bookPath", "format=$format", 'order=random'
$fcArgs += '-rounds', $Rounds, '-games', 2, '-repeat', '-concurrency', $Concurrency, '-recover'
# Print results (incl. SPRT LLR) every 10 games so the dashboard can follow along.
$fcArgs += '-ratinginterval', 10
$fcArgs += '-pgnout', "file=$dir\games.pgn"
if ($Type -eq 'gauntlet') { $fcArgs += '-tournament', 'gauntlet'; if ($Seeds -gt 1) { $fcArgs += '-seeds', $Seeds } }
if ($Type -eq 'sprt') {
    $e0, $e1 = $Sprt.Split(',')
    $fcArgs += '-sprt', "elo0=$e0", "elo1=$e1", 'alpha=0.05', 'beta=0.05', 'model=normalized'
}
if ($Live) { $fcArgs += '-log', "file=$dir\engine.log", 'level=info', 'engine=true' }

$created = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
$meta = @("name=$Name", "type=$Type", "tc=$TC", "total=$total", "main=$($names[0])", "created=$created", "concurrency=$Concurrency", "notes=$Notes") + $anchors
[IO.File]::WriteAllLines("$dir\meta.txt", $meta)
[IO.File]::WriteAllText("$dir\command.txt", "$Fastchess $($fcArgs -join ' ')")

# Run inside the run folder: fastchess writes a config.json into its working directory.
$p = Start-Process -FilePath $Fastchess -ArgumentList $fcArgs -WorkingDirectory $dir -RedirectStandardOutput "$dir\output.txt" -RedirectStandardError "$dir\stderr.txt" -WindowStyle Hidden -PassThru
# Below normal priority (the engines it starts inherit it), so the PC stays usable while it plays.
try { $p.PriorityClass = 'BelowNormal' } catch {}
Add-Content "$dir\meta.txt" "pid=$($p.Id)"
"Started '$Name' ($total games max) -> $dir  [pid $($p.Id)]"
