# Attack seeds

Build with `cargo build --release -p attack-seeds`. Requires `gzip` on PATH for
`.txt.gz` inputs (Git for Windows supplies it).

```
attack-seeds --root <lab/datagen> --count 200000 --deny <testbook-starts.fen> --out <attack-seeds.fen>
```

Streams the two local gen7 directories and recursively reads cloud directories
whose status contains `gen=7`. Filters to fullmoves 12–40, scores within 300 cp,
and either both queens or two non-pawn, non-king pieces per side. Kings on a/b/c or
f/g/h files within two ranks of home count as castled-wing kings. Attack features
are opposite wings; two enemy pawns advanced to their fifth rank on adjacent
files; a sixth-rank pawn wedge; at most one home-rank shield pawn plus an advanced
enemy pawn or nearby heavy piece; or two blocked pawn pairs with an attacking
rook/queen on the same files at least three ranks from home.

Keeps every eighth qualifying record per inferred game region (move-number
reset), excludes the deny book, deduplicates on the first four FEN fields, then
uses a fixed-seed reservoir over the entire source collection. The text format
has no game IDs, so regions are approximate; this is subsampling, not a strict
per-game cap. Hit rates overlap and use material/score/move-eligible positions
as their denominator. Memory is proportional to unique subsampled positions,
not input size. Input files can be live: only complete valid records are useful;
reruns against changing inputs need not produce identical output.

Use starts with:

```
pawnstorm datagen 2 1000000 5000 <out> 30 123 --fens <attack-seeds.fen> --fen-percent 100 --deny <testbook-starts.fen>
```

FEN selection precedes normal opening selection; remaining games use the old
opening mix. Selected FENs receive 0–2 seeded random legal plies. With no FEN
file the old opening path consumes exactly the same random numbers.
