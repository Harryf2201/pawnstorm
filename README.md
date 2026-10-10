# Pawnstorm

A UCI chess engine written in Rust.

- Bitboard move generation with PEXT slider attacks (perft-verified)
- Principal variation search with transposition table, null-move pruning, late move reductions,
  singular extensions, history/continuation-history move ordering and more
- NNUE evaluation `(768 -> 512)x2 -> 1` trained with [bullet](https://github.com/jw1912/bullet)
  on self-play data, with a hand-crafted fallback evaluation

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

## Layout

- `src/` engine
- `trainer/` NNUE trainer (needs CUDA)
- `tools/` local tooling: tournament launcher, dashboard, data preparation, hardware watchdog
