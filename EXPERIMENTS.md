# Pawnstorm experiments log

Everything tried so far, what happened, and where the details live. Newest work is at the bottom of
each section.

## Where things are recorded

| What | Where |
|---|---|
| Every engine version (47 so far, incl. failed ones): title, status, network, data, summary | `versions.ini` (also the dashboard's Versions tab) |
| Every test/tournament: settings, notes, games, results | `testing\runs\<date>_<name>\` (`meta.txt`, `output.txt`, `games.pgn`); dashboard Tournaments tab |
| Tests discarded as invalid (wrong setup) | `testing\runs-invalid\` |
| Search tuning (SPSA): current values and every iteration | `testing\spsa\state.json`, `history.csv`; run 1 in `testing\spsa-run1\` |
| Network training logs and checkpoints | `testing\training\<net>\` |
| Automation scripts behind each experiment (with a header comment saying what they did) | `testing\training\*.ps1` + `.log` |
| Code for each idea | git branches below (all pushed to origin) |

Elo below is from SPRT/self-play unless it says "vs Stockfish". ± is the 95% margin.

## Training data

| Experiment | Result | Verdict | Where |
|---|---|---|---|
| More data, gen1 (hand-eval self-play): 2.4M → 10M → 20M → 40M → 80M → 160M → 219M → 316M | Each doubling helped a lot (~+100 at the start, shrinking later) | Kept: v0.2–v0.8.1 | versions.ini |
| Gen2 (self-play by the v0.8 network): 50M → 100M → 260M → 330M → all | Better than the same amount of gen1 | Kept: v1.0–v1.5n | versions.ini |
| Mixing old gen1 data into gen2 | Worse than gen2 alone | Dropped (v1.2b) | versions.ini |
| Gen3 only (484M) vs gen2+gen3 mix (984M) | Mix ≥ gen3 alone; both ~+15 vs v1.6 | Mix kept (v1.7m → v1.7) | runs 2026-10-12_19*, 20* |
| Gen4 (by v1.7) mixed with gen2+gen3 | Level with v1.7 head to head, slightly better vs Stockfish | Kept in v1.9 | v1.9w, v1.9g |
| gen3+gen4 only (no gen2) | No clear gain | Shelved (v1.9g) | versions.ini |
| Fresh mix with all gen4 so far (1.3B, newest first) | Training now (Oct 14 evening) | — | `testing\training\v20-chain.ps1` |

## Network architecture and training recipe

| Experiment | Result | Verdict | Where |
|---|---|---|---|
| 512 hidden (first nets) | Baseline | — | master |
| Wide (1024) at 219M / longer training | Too little data for the size at the time | Failed (v0.9a, v0.9c) | branch v0.9a |
| Output buckets (8 by material) | −9 ±15 vs v0.8 | Failed (v0.9b); retried later as v1.7b/v1.7c, failed again | v0.9b, wide8, ob8 |
| King input buckets | No gain at 512 / with wide net | Failed/shelved (v1.2k, v1.5k, v1.7k, v1.8k) | kb, kb4, widekb |
| Wide (1024) with enough data (984M) | +36 ±26 vs v1.6 | **Kept: v1.7** | wide |
| Longer training (100 rounds vs 40) | −17 ±24 | Failed (v1.5l) | versions.ini |
| WDL weight 0.6 / 0.2 instead of 0.4 | Not finished | Left as testing (v1.5r, v1.5e) | versions.ini |
| Multi-layer, 1024 wide (pairwise → 16 → 32 → 1, 8 buckets), 60 and 120 rounds | −11 ±18 (ml1), −17 ±21 (ml2) vs v1.9's net: ~36% slower to evaluate | Dropped | branch ml; runs 2026-10-13_2330*, 2026-10-14_0012* |
| Sparse first-hidden-layer evaluation for the multi-layer net | Slower than dense | Reverted | branch ml history |
| Multi-layer, **512 wide**, 120 rounds | **+32 ±22 vs v1.9's net**; 3225 ±28 vs Stockfish ladder | **Kept: v1.11** | branch ml512; run 2026-10-14_010146 |
| Same shape on new data, 120 and 240 rounds | Testing now | — | v20-chain |

## Search

| Experiment | Result | Verdict | Where |
|---|---|---|---|
| Pawn correction history | +33 ±31 | Kept (v1.2c → v1.4) | corrhist |
| Search upgrade 1 (AVX2 speed-up and pruning) | +18 ±11 | Kept (v1.3s → v1.4) | next, simd |
| Search upgrade 2: node/stability time management, ProbCut, history pruning, 50-move eval fade | +13 ±28 quick check, confirmed in Championship 2 | Kept (v1.5) | search2 |
| Syzygy tablebases | Not measurable at test time controls | Shelved for testing (v1.7t); on in the bot | wide |
| Lazy SMP threads | 4 threads clearly stronger than 1 | Kept (bot uses many threads) | run 2026-10-12_235033 |
| SPSA run 1 (23 search settings, ~1400 iterations) | v1.7s alone failed, but combined with gen4 net (v1.9ws) it won Championship 5 | Kept in v1.9 | `testing\spsa-run1` |
| Non-pawn + continuation correction histories alone (corr2) | +2.4 ±6.9 (2000 games) | Kept as part of stack5 | dev |
| stack5: corr2 + TT-PV + threat-aware history + pawn history + deeper/shallower re-searches | +12.7 ±8.4 (1476 games); tied with v1.9 vs Stockfish | Kept (v1.10 → v1.11) | dev |
| Pondering (thinks on the opponent's time) | Works on Lichess; not measured in tests | Kept (bot) | dev |
| Finny accumulator cache for king buckets | +10–17% speed, identical search | Only useful with king buckets (not in use) | widekb |
| SPSA run 2 (28 settings incl. the 5 new ones) on v1.11 | +2 ±7 self-play; 54.7% vs 51.8% for v1.11 against Stockfish | Kept (v1.11s → v1.12) | `testing\spsa` |
| ARM (NEON) kernels for the multi-layer network: sdot dot product with 4 independent sums | +22% speed on Oracle (595k → 726k nps), identical search; plain NEON widening multiplies gave only +1% | Kept (master) | branch neon |

## Lichess bot

| Change | Result | Where |
|---|---|---|
| PC → Oracle server hosting, failover timer, datagen pauses during games | Plays 24/7 | `tools\oracle\`, README |
| Re-challenges at other time controls, rapid fallback | More games found | lichess-bot config |
| v1.9 → v1.10 search + pondering → v1.11 | Ratings rising (rapid ~2897) | Oracle `~/pawnstorm-v111` |

## Oct 16-19: toward 2.0

Versions: v1.12 (240-round gen4 net + SPSA run 2), v1.13 (gen5 net + forced-move clock + TT eval fix), v1.14 (768-wide gen5 net,
3218 +/- 9), v1.15 (768 on gen5 only; level with v1.14 head to head, better against limited Stockfish). See versions.ini.

| Experiment | Result | Verdict | Where |
|---|---|---|---|
| Forced-move clock saving (ChatGPT) | +5.0 +/- 4.7 (4000 g) | Kept (v1.13) | codex/forced-clock-20261016 |
| TT raw static eval fix (ChatGPT) | +7.4 +/- 8.3 (1460 g) | Kept (v1.13) | codex/tt-static-eval-20261016 |
| gen5 network (512) | +18.3 +/- 7.9 | Kept (v1.13) | net-gen5a |
| 768-wide gen5 network | +15.5 +/- 7.8 | Kept (v1.14) | branch w768, net-gen5w |
| 400 training rounds (vs 240) | -0.5 | Dropped | net-gen5b |
| 4-ply continuation history | -0.2 | Dropped | branch cont4 |
| AVX2 4-sum dot (x86) | identical, no speed gain | Dropped | dot-lab |
| Rising result weight 0.2 -> 0.6 | +0.4 | Dropped | net-r1 |
| 32-neuron first hidden layer | -25.8 (15% slower); +5.5 +/- 12 at fixed nodes; best validation loss | Dropped (not worth the speed, even for long games) | branch ml32, net-l32 |
| 4 king input buckets (multi-layer) | -11.5 | Shelved until much more data | branch mlkb4, net-kb |
| Threat inputs (attacked-piece features) | +10.9 +/- 14.7 at fixed nodes, but 25-35% slower (-30 +/- 27 at 8+0.08); fused and incremental versions exact but not faster | Shelved until more data | branch threat, net-thr |
| Quantisation cost (layer 1 in f32 vs 8-bit, fixed nodes) | -1.6 +/- 11.7 | No cost; quantisation-aware training not needed | branch f32l1 |
| Lazy SMP thread voting (4 threads) | -1.9 +/- 8.9 | Dropped | branch vote |
| SPSA run 3 (v1.13/v1.14, 1715 rounds) | -4.5 +/- 6.7 (2000 g) | Dropped | testingspsa-run3 | testing\spsa |
| Search sprint 1 (cloud, 3600 g each): LMR re-search history update | +6.0 +/- 5.4 | Candidate | branch sprint1 SprLmrRes |
| Negative singular extension (-2) at cut-nodes | +5.2 +/- 5.2 | Candidate | SprSeCutNeg |
| Cut-node reduction without TT move / capture-TT-move quiet reduction / capture history in SEE / fail-high depth bonus | -1.3 / -0.7 / -1.8 / -4.3 | Dropped | sprint1 |
| Sprint 2: aspiration fail-high depth reduction | +4.8 +/- 5.5 | Candidate | branch sprint2 SprAspFh |
| TT-cutoff quiet history / quiescence quiet-evasion pruning | -0.7 / +0.2 | Dropped | sprint2 |
| Sprint 2: minor / major piece correction histories | -0.6 / -2.9 | Dropped | sprint2 |
| Sprint 1-2 winners combined (combo1) | +7.3 +/- 3.8 (7200 g) | Kept | sprint3 |
| Sprint 3: null-move verification at depth >= 12 (Codex idea) | +4.1 +/- 5.1 | Candidate | branch sprint3 SprNmpVerify |
| Less LMR for quiets when the correction history moved the eval a lot (K=64) | +6.4 +/- 5.2 | Candidate | SprCorrLmr |
| Search the TT move before scoring the rest | +1.7 | Dropped | SprLazyScore |
| Sprint 4: hindsight depth (parent reduction + eval change; fixed after Codex review) | +10.6 +/- 5.3 | Candidate | branch sprint4 SprHindsight |
| Capture futility pruning | +0.2 | Dropped | SprCaptFut |
| Result weight 0.25 / 0.55 / 0.7 (v1.15 recipe, hold-out loss vs 0.011055 at 0.4) | 0.011122 / 0.010951 / 0.011287 | 0.55 to SPRT | net-w25, net-w55, net-w70 |
| Result weight 0.5 / 0.6 (hold-out) | 0.010989 / 0.011134 | 0.55 stays best | net-w50, net-w60 |
| v1.16 (six search winners + wdl 0.55 net) vs v1.14 | +22.7 +/- 4.4 (7200 g, 8+0.08); +20.7 +/- 6.1 (2400 g, 40+0.4) | Released as v1.16; 2.0 gate failed narrowly (3222 +/- 28) | branch sprint4 |
| Sprint 5 (Codex ideas): eval feedback into history / TT ProbCut / child cutoff-count LMR | -14.4 / -5.4 / -9.4 | Dropped | branch sprint5 |
| Sprint 6: score-drop time extension / symmetric eval-history feedback (vs v1.16) | +0.9 / -6.8 | Dropped | branch sprint6 |
| Sprint 7 (Codex code review, vs v1.16): qsearch draw checks / qsearch promo+ep futility / killer history LMR / age-aware TT / no ext for losing checks / IIR at all-nodes | -1.6 / -0.5 / -8.5 / -7.0 / -4.3 / -22.5 | Dropped | branch sprint7 |
| gen5/gen6 75/25 network (800M, wdl 0.55, v1.16 search) vs v1.16 | -0.7 +/- 4.1 (7200 g); hold-out 0.011087 vs 0.010951 | Level, not used | net-g56 |
| Championship 7 (13 engines, 10+0.1, clean games only) | v1.16 3227, v1.15 3226, v1.14 3209 (+/-38); SF full 3589, PlentyChess 3552, RubiChess 3447 | Ended early after a lab pause; 2736 later games excluded | runs/2026-10-19_151514 |

Tooling added: held-out validation (testing\datagen\holdout-gen6, trainer -Val, tools/net-check.sh: validation loss matched every
SPRT direction except where speed differed); cloud test matches on GitHub runners (tools/cloud-test.ps1, test-match workflow);
match.ps1 -Nodes; prepare-data -SampleRate. Fixed: engines leaked a network copy per EvalFile resend (OOM in long matches);
watchdog RAM rule could freeze the lab (pausing does not free memory).

## Oct 20-21: 2.0, time management, Codex search ideas, CCRL Blitz-scale estimate

| Experiment | Result | Verdict | Where |
|---|---|---|---|
| All gen5 + gen6 network (1.24B, wdl 0.55, v1.16 search) vs v1.16 | +10.8 (7200 g) | Kept: 2.0's network | net-g56all |
| SPSA run 4 settings (v1.16 search, 1715 rounds x 7 pairs) vs v1.16 | -0.9 +/- 3.8 (7200 g) | Dropped; big retunes stopped (only small targeted ones from now on) | testing\spsa |
| Sprint 8 (owner's ideas, vs 2.0): +50% / +25% soft time after an unexpected reply; +20% in the middlegame (material phase 8-20) | -2.3 / -6.2 / -4.6 (+/-3.5, 7200 g each) | Dropped (a score-drop-gated version vs stronger engines is a possible later idea) | branch sprint8 |
| Sprint 9 (Codex's ranked ideas, vs 2.0): quiet checks exempt from LMP/futility / SEE ignores pinned recapturers / qsearch continuation context | +3.2 / -4.7 / -9.9 (+/-3.8, 7200 g each) | Quiet checks: confirmation run; others dropped | branch sprint9 |
| Local CCRL Blitz-scale benchmark: 2.0 vs 12 CCRL Blitz-rated engines (3292-3747), 2'+1", 1 thread, P-cores only, identical openings per opponent, Ordo with fixed anchors | 3557 +/- 58 (104 g); 3561 +/- 54 on the first 69 g (gauntlet with uneven openings); 3559 +/- 39 combined | Estimate on the CCRL Blitz scale (local 2+1, not an official rating) | testing\ccrl-bench, runs/2026-10-20_232145 |

Tooling added: testing\ccrl-bench (reference-engine manifest with CCRL ratings and hashes, bench.ps1 per-pairing scheduler
with per-core pinning and exact resume, Ordo 1.2.6 with fixed and loose anchors). Found: fastchess -use-affinity did not pin
engine processes on Windows (process affinity on fastchess is inherited instead); in multi-engine gauntlets fastchess assigns
openings per pairing, so pairings get different, repeating openings; fastchess -config resume only works for two-engine matches.
| Sprint 10 (Codex ideas 4-5, vs 2.0): time from the latest iteration's node share / wider RFP margin when the correction is large | -17.3 / -7.4 (7200 g each) | Dropped | branch sprint10 |
| Quiet checks exempt from LMP/futility, confirmation and long TC (vs 2.0) | +2.1 +/- 3.8 (7200 g, 8+0.08); -1.3 +/- 5.3 (2400 g, 40+0.4) | Not kept (too small); candidate for a later combined test | sprint9 SprQuietChk |
| 1024-wide multi-layer network on all gen5+gen6 (2.0 recipe), vs 2.0 | hold-out 0.010718 vs 0.010896; +2.5 +/- 4.2 (7200 g, 8+0.08); +4.3 +/- 5.9 and +6.4 +/- 5.7 (2x2400 g, 40+0.4); 23% slower on the PC, 16% on ARM; ARM node-identical | Better at longer time controls: candidate for the bot (v2.1) and the size for gen7 nets | net-g56all-1024, branch w1024 |
| Gen7 real-opening starts: 100k human 2200+ Lichess lines (Sept 2026), 50% of games + 0-4 random plies, test-book start positions denied | Running on GitHub (next batch), Oracle (from 2026-10-21 07:39 UTC, oracle-gen7o), PC (gen7o-v20-5k) | Gen7 data from here on | openings/, tools/openings |
| Sparse first hidden layer for the 1024 net (AVX2/NEON, transposed weights) | Bit-identical nodes but 8-10% slower; skipping L1 entirely did not change nps (so L1 is not the 1024 bottleneck; accumulator updates/memory are) | Dropped | (branch deleted) |
| 1024 net training length (same data/recipe as v2.1, 240 sb) | Hold-out 160 sb 0.010806, 320 sb 0.010589, 480 sb 0.010555 vs 0.010718; 320 vs v2.1: -3.3 +/- 5.7 (3600 g, 8+0.08), +2.6 +/- 8.2 (1200 g, 40+0.4); 480 at 40+0.4 running | Lower hold-out did not show up in games; 240 stays the standard | net-g56all-1024-sb320/480 |
| 4 mirrored king input buckets at 1024 (mlkb4, v2.1 recipe) | Hold-out 0.011345 vs 0.010718 | Dropped (second failure for king buckets) | net-g56all-1024-mlkb4 |
| Sprint 11 (from the Lichess loss review, vs v2.1): losing captures that give check at the first qsearch ply (SprQsChkSac=1) / less LMR for quiet moves hitting the enemy king ring (SprKingAtk=1) / checks + captures next to the king (SprQsChkSac=2) | -3.7 +/- 5.5 / +1.7 +/- 5.4 (3600 g each, 8+0.08) / running | QsChkSac=1 dropped; KingAtk too small so far | branch sprint11 |

Lichess loss review (Oct 21): the 15 losses since 2.0 were re-analysed with 2.1 at 1.5M nodes per position
(testing\lichess\review: analyse.py, swings.py). About 7 of the 13 real losses were king attacks: sacrifices on g6/g7 and
h-pawn storms that even the deeper search missed one move earlier. 2 losses were Lichess phantom time forfeits.
Opening report over all 597 bot games (openings.py): as Black, 1.e4 e6 3.Nc3 Nf6 4.e5 scored 29% (50 games), 1...c5 60%,
2.Nf3 e6 3.d4 37%, 2...d6 54%. This led to a small bot-only polyglot book (1...c5, 2...d6, a weighted mix of first moves
as White, 7.Bg5 in the QGD exchange line).
| Exact i8 feature-weight table + per-row i16 correction lists (7,149 exceptions, 818 KB vs 1.5 MB; Codex) | Bit-exact (635k differential updates, identical nodes) on AVX2 and NEON; bench: PC 4.2% slower, ARM about 1% slower | Dropped: the cache saving does not pay for the sign-extension and fixups | branch i8w |
| First gen7 network (v2.1 recipe on g56all + 236M gen7 = 1.48B, 16% gen7) vs v2.1 | Hold-out 0.010568 vs 0.010718; +9.5 +/- 6.0 (3600 g, 8+0.08), +9.6 +/- 8.0 (1200 g, 40+0.4) | Kept: v2.2, on the Lichess bot from Oct 22 03:21 | net-g567-1024 |
| 480 vs 240 superbatches (1024, g56all) at 8+0.08 | +6.4 +/- 5.9 (3600 g); at 40+0.4 +8.7 and -1.2 (2 x 1200 g) | Longer training probably helps a little: use it on the gen7 nets | net-g56all-1024-sb480 |
| Exact speed-ups (Codex): aarch64 TT prefetch, null-child prefetch, node-local history keys, SEE reuse | Node-identical on PC and ARM; ARM +6-7% nps, PC +0.7% | Kept: merged into w1024, on the bot from Oct 22 00:29 | branch speed1 |
| Second gen7 net (25% gen7, 240 sb) vs v2.2 | Hold-out 0.010516; +0.9 +/- 5.6 (3600 g, 8+0.08), -8.4 +/- 8.2 (1200 g, 40+0.4) | Dropped: more gen7 alone didn't help yet | net-g567b-1024 |
| Same gen7 mix at 480 sb vs v2.2 | Hold-out 0.010540; +3.5 +/- 6.0 (3600 g, 8+0.08), +1.5 +/- 9.0 (1200 g, 40+0.4) | Not clearly better: v2.2 stays on the bot | net-g567b-480 |
| Gen7 + ~4% targeted king-attack data (gen7k), 480 sb vs v2.2 | Hold-out 0.010657; +6.8 +/- 5.1 (3962 g, 8+0.08, PC, LLR 1.94 when cut off), +3.8 +/- 8.0 (1200 g, 40+0.4) | Kept: v2.3, on the bot from Oct 23 13:10 UTC (owner's call) | net-g567k-1024-sb480 |
| Repetition: game-history positions need a threefold (search-path repeats still draw) vs v2.2 | -2.9 +/- 5.1 (3600 g, 8+0.08) | Not deployed: no gain in self-play (history repeats are rare there); revisit with v2.3 or at longer TC | branch rep3 |
