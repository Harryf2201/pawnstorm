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
| SPSA run 3 (v1.13/v1.14, 1715 rounds) | settings test running | | testing\spsa |
| Search sprint 1 (cloud, 3600 g each): LMR re-search history update | +6.0 +/- 5.4 | Candidate | branch sprint1 SprLmrRes |
| Negative singular extension (-2) at cut-nodes | +5.2 +/- 5.2 | Candidate | SprSeCutNeg |
| Cut-node reduction without TT move / capture-TT-move quiet reduction / capture history in SEE / fail-high depth bonus | -1.3 / -0.7 / -1.8 / -4.3 | Dropped | sprint1 |
| Sprint 2: aspiration fail-high depth reduction | +4.8 +/- 5.5 | Candidate | branch sprint2 SprAspFh |
| TT-cutoff quiet history / quiescence quiet-evasion pruning | -0.7 / +0.2 | Dropped | sprint2 |

Tooling added: held-out validation (testing\datagen\holdout-gen6, trainer -Val, tools/net-check.sh: validation loss matched every
SPRT direction except where speed differed); cloud test matches on GitHub runners (tools/cloud-test.ps1, test-match workflow);
match.ps1 -Nodes; prepare-data -SampleRate. Fixed: engines leaked a network copy per EvalFile resend (OOM in long matches);
watchdog RAM rule could freeze the lab (pausing does not free memory).
