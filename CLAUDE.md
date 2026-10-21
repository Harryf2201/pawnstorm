# Notes for AI agents working on Pawnstorm

Pawnstorm is a Rust NNUE chess engine, plus the lab around it: self-play data generation, network
training, automated testing against Stockfish, a dashboard, and a Lichess bot. Read this first, then
[README.md](README.md), [EXPERIMENTS.md](EXPERIMENTS.md) (everything tried and the results) and
[versions.ini](versions.ini) (every engine version with notes).

## Where things are

The repo holds code and write-ups only. Everything else lives in the **lab folder**, `testing\`, next to
the repo (its path is `$LabRoot` in `tools\paths.ps1`). The lab folder is never committed.

| What | Where |
|---|---|
| Engine | `src\` (search.rs, nnue.rs, params.rs = SPSA-tunable settings exposed as UCI options) |
| Network trainer (uses the bullet library, cloned next to the repo) | `trainer\`, `tools\train.ps1`, `tools\prepare-data.ps1` |
| Dashboard (Rust server + one HTML page, http://localhost:8000) | `tools\dashboard\` |
| Lab scripts (start/stop, tests, tuning, syncs, watchdog) | `tools\*.ps1` |
| Cloud data generation (GitHub Actions) | `.github\workflows\datagen.yml`; the network it uses is `nets\default.bin`, labelled by `nets\default.gen` |
| Test/tournament results | `testing\runs\<date>_<name>\` (meta.txt, output.txt, games.pgn) |
| Engine specs used by tests | `testing\engines\<name>.nnue / .options / .runner` (runner = which build exe runs it) |
| Training runs and the scripts that chained experiments | `testing\training\` |
| SPSA tuning state | `testing\spsa\state.json`, `history.csv` |
| Training data by generation | `testing\datagen\` (gen1 hand-eval, gen2 v0.8, gen3 v1.3, gen4 v1.7m, gen5 v1.11) |
| Hardware health | `testing\health\status.txt`, `events.log` |

Branches: `master` is the current engine (v1.11's code + NEON speed-ups); older experiments keep their
own branches (listed in EXPERIMENTS.md and versions.ini).

## Running things

- `tools\lab-start.ps1 -NoBrowser` starts the sensors, watchdog, dashboard and cloud/Oracle syncs
  (skips anything already running). `-LocalDatagen` also runs self-play here. `tools\lab-stop.ps1` stops.
- Tests: `tools\match.ps1 -Name ... -Type sprt|gauntlet|roundrobin -Engines a,b[,sf:3100,stockfish] -TC 8+0.08 -Concurrency N`.
  **It returns as soon as the match starts**: a script that chains steps must wait until fastchess.exe has gone.
- Standard checks: SPRT `[0,5]` at 8+0.08 for improvements; the "Stockfish ladder" (gauntlet vs SF
  2600/3100/3150/3190/full at 10+0.1, `-Seeds` = number of our engines) for ratings.
- SPSA: `tools\spsa.ps1 -Engine <build> -Net <net> -Iterations N -PairsPerIteration P -Concurrency C`
  (resumes from state.json; the schedule depends on Iterations x Pairs, so keep that product fixed when resuming).
- Launch long jobs detached (`Start-Process powershell -WindowStyle Hidden ...`) so they survive the session.

## Machines

- **This PC:** tests, GPU training, some data generation. Heat is the limit: about 14–18 games at a time.
  The watchdog pauses work at 92°C and everything at 98°C, and resumes by itself.
- **Oracle Cloud server (ARM, 16 cores):** runs the Lichess bot and gen5 self-play. Data generation pauses
  automatically during bot games (`datagen-yield` service). Connection details are in the untracked
  `tools\engines.local.ps1`. It is a free trial: never create or resize paid resources.
- **GitHub Actions:** one data-generation batch at a time, chained forever (`DATAGEN_FOREVER=1`). If GitHub
  warns, throttles or fails jobs, stop and tell the owner.

## Rules from the owner

- **Never commit or print secrets or personal details:** the Lichess token (in the lichess-bot config), the
  Oracle IP/key, the owner's email or name. The repo is public; check new files before pushing.
- **Nothing that costs money.** No admin (UAC) prompts unless the owner is at the PC.
- Keep engines below normal priority so the PC stays usable.
- Deleting Pawnstorm files (old data mixes, scratch builds) is fine without asking.
- **Version names:** plain numbers (v1.12, v1.13, …); add every version to versions.ini. **v2.0** (owner's
  decision, Oct 19): v1.16 plus whatever passes in the current round of experiments and SPSA run 4; gen7 data is then
  generated with 2.0. (Earlier rule: the first version clearly above 3200 on every measure; v1.16 missed that gate narrowly.)
- Stop clearly losing tests early; only run long tests when results are close or promising. Verify
  surprising results before believing them.
- The Lichess bot is a shared project (owner, Oct 21): changes expected to raise its Elo (tested engine/net upgrades,
  draw/time/matchmaking settings) need no prior OK. Apply them between games, keep a config backup and tell the owner
  afterwards. Cosmetic or social changes, and anything that risks the account, still need a word first.
- Record every experiment in EXPERIMENTS.md and versions.ini.

## Gotchas

- Windows PowerShell 5.1: no `&&`; files written by Git Bash tools may be CRLF/LF mixed (convert before regex edits).
- `tools\match.ps1` returning early (above) is the most common chaining bug.
- Engine/data hot paths have separate x86 (AVX2) and ARM (NEON) code in `src\nnue.rs`: change both and
  check that search output (nodes at a fixed depth) is identical on both machines.
