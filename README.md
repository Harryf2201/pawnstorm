# Pyther

A UCI chess engine written in Rust (formerly called Pawnstorm; the crate, binaries and lab files still use that name).

- Bitboard move generation with PEXT slider attacks (perft-verified)
- Principal variation search with transposition table, null-move pruning, late move reductions,
  singular extensions, history/continuation-history move ordering and more
- NNUE evaluation `(768 -> 1024)x2 pairwise -> 16 -> 32 -> 1` with 8 output buckets, trained with [bullet](https://github.com/jw1912/bullet)
  on self-play data, with a hand-crafted fallback evaluation

See [EXPERIMENTS.md](EXPERIMENTS.md) for everything tried so far and where each result lives.

## Building

```
cargo build --release
./target/release/pawnstorm          # UCI mode, for use in any chess GUI
./target/release/pawnstorm bench    # speed test
./target/release/pawnstorm perft    # move generation correctness test
```

## Training data

`pawnstorm datagen <threads> <positions> <soft-nodes> <out-dir> [seconds] [seed]` plays fast
self-play games and writes `<fen> | <score> | <result>` lines. The `datagen` GitHub Actions
workflow runs the same generator on hosted runners.

Targeted self-play starts: `--fens <one-FEN-per-line file> --fen-percent 100
--deny <testbook-starts.fen>` selects a random seed plus 0–2 random legal plies.
FEN starts take precedence over the existing opening mix; the percentage defaults
to 50. See [tools/attack-seeds](tools/attack-seeds/README.md) for the streaming gen7
king-attack seed extractor.

Datagen quality controls: `--keep-every N` (default 1), `--max-per-game M`
(default unlimited), `--dup-cap K` (default off), `--hard-nodes N` (default
20 x soft nodes), and `--eval-file <net>`. Status records the recipe. `rescore
<input.txt[.gz]|dir> <out.txt> --nodes N --threads T` relabels frozen text data
while retaining game results; sampling, blending and checkpointed resume are supported.
`tools/prepare-data.ps1 -Quota 0.75,0.20,0.05` assigns source shares with unbiased
sampling. See [DATAGEN2.md](DATAGEN2.md) for the multi-day recipe and the important
768/1024 build distinction in this checkout.

## Layout

- `src/` engine
- `trainer/` NNUE trainer (needs CUDA)
- `tools/` local tooling: tournament launcher, dashboard, data preparation, hardware watchdog
