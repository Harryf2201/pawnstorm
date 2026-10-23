# How to run the multi-day data run

This is a data-quality build, not a new engine release. Do not start jobs until the
net/architecture preflight below is satisfied. No jobs were launched by this change.

## Preflight

This `targeted` checkout actually has `HIDDEN=768` and a 768-wide embedded net, despite
the handoff describing w1024. Default builds retain that baseline. Both supplied
`pawnstorm-g567-1024.nnue` (v2.2) and `pawnstorm-g567k-480.nnue` (v2.3) are **1024-wide**;
480 is a training label, not their hidden width. To use either:

```powershell
$env:CARGO_BUILD_JOBS=2
cargo build --release -p pawnstorm --features nnue1024
# Use --eval-file <compatible net> on datagen/rescore, or first install the intended
# compatible net as nets/default.bin and rebuild to embed it.
```

Use a fresh, clearly labelled output directory per recipe, including soft nodes and
net label (e.g. `gen7k-v23-n5000-k4-m32`). The status records soft/hard nodes and all
sampling flags. A restart rejects changed recorded settings; the duplicate table
starts empty on each process restart. Keep a copy/hash of the executable and net
with the batch. Do not change the deployed bot or running jobs for this build.

## Recommended arms

Start conservatively: `--keep-every 4 --max-per-game 32 --dup-cap 0`. Thinning takes
one random phase per game over eligible quiet positions, then reservoir sampling
uniformly caps the retained positions. Neither changes which moves the engine searches.
Default values `1 / 0 / 0` preserve the old generator and its RNG stream.

* GitHub: normal batches at 5,000 soft / 100,000 hard nodes; about **one in five
  sequential batches** at 20,000 soft / 400,000 hard nodes. This is a CPU-time arm
  allocation, not an expected 20% share of saved positions. Pass `-Eval nnue
  -SoftNodes 20000 -HardNodes 400000 -KeepEvery 4 -MaxPerGame 32 -DupCap 0` to
  `tools/cloud-datagen.ps1` for the deep batch. For normal batches use 5000/100000.
  Set `-NnueWidth 1024` only after nets/default.bin is the intended 1024 net on the
  workflow ref. All inputs propagate through chains. Use short finite chains to
  alternate arms; do not enable an endless normal-only chain. Workflow output
  directories include `-n<soft_nodes>`, with limits also in status and launch records.
  Keep artifact names `shard-N` for existing sync compatibility; archive the deep
  batches under a separate source name when assembling the mix. Stop if GitHub warns.
* Oracle ARM: 16 threads, same 5k limits and sampling, a compatible ARM build and
  embedded net. Keep existing yielding during bot games and below-normal priority:
  `nice -n 10 ./pawnstorm datagen 16 1000000000000 gen7-v23-n5000 259200 12345
  --hard-nodes 100000 --keep-every 4 --max-per-game 32 --dup-cap 0` (one shell line).
  Build there with `--features nnue1024` for the supplied nets; ARM parity still needs
  checking on that machine before deployment. Never create paid resources.
* PC targeted gen7k: initially 2 threads (increase only within the existing heat
  budget), 5k limits, 100% targeted starts. Run a detached, hidden PowerShell wrapper
  at BelowNormal priority, using the following command within it:

```powershell
.\target\release\pawnstorm datagen 2 1000000000000 <lab>\datagen\gen7k-v23-n5000 259200 67890 --eval-file <v2.3-net> --fens <king-attack-seeds.fen> --fen-percent 100 --deny openings/testbook-starts.fen --hard-nodes 100000 --keep-every 4 --max-per-game 32 --dup-cap 0
```

`--dup-cap K` optionally limits each exact board hash (including side to move) to K
saves **per thread per process run**: up to threads*K globally. It uses 16 MiB per
thread and never evicts. Hash-bucket collisions conservatively reject extra distinct
positions. The table can saturate on multi-day runs and greatly reduce yield, so keep
it off for the primary run; test it as a separate short diversity arm if desired.
It does not recognize halfmove/fullmove differences as distinct boards. Default-off
uses no table memory. Game caps apply before duplicate rejection, so final game
samples may be smaller than the cap.

## Rescore existing data

```powershell
.\target\release\pawnstorm rescore <input.txt-or-input.txt.gz-or-directory> <rescored.txt> --nodes 20000 --threads 2 --sample 0.2 --limit 1000000 --blend 1 --eval-file <compatible-net>
```

Nodes are a hard search budget. Each FEN has cleared TT, cleared search histories
and no game history; the new score is White-relative. `--blend W` rounds
`W*new+(1-W)*old`; the original result is retained. Check positions and old/new mate
scores are skipped. Directory inputs are sorted `data_*.txt[.gz]` files, nonrecursive.
Sampling is deterministic by input line index; sample and blend default to 1, limit
to unlimited. Malformed complete records fail with a source location; incomplete
last lines are omitted. Gzip uses system `gzip`, or Git for Windows' gzip.

Restart the **identical command** to resume. Keep the `.rescore.manifest` and
`.rescore.checkpoint` sidecars with the output: they record input progress, net hash,
source sizes/timestamps, arguments and committed output length. Any uncheckpointed
output tail is truncated; no previously committed searches are repeated. Inputs must
be frozen snapshots; changed input/recipe is rejected. Do not edit output or reuse it
for another command. No cross-process writer locking: one rescore process per output.

## Build the training mix

Archive normal, deep and targeted data as separate source directories under lab
`datagen`. Example: 75% normal + 20% deep + **5% targeted**:

```powershell
.\tools\prepare-data.ps1 -Sources gen7-v23-n5000,gen7-v23-n20000,gen7k-v23-n5000 -Name datagen2-mix -MaxPositions 800000000 -Quota 0.75,0.20,0.05 -SampleRate 1
```

Quotas are fractions in Sources order and must sum to one; they require MaxPositions.
SampleRate is the maximum retained fraction per source. Insufficient sources underfill
rather than silently borrowing another source's share. Without quotas the capped mix
is proportional to available source positions. Selection is uniform across each
source's complete valid records, including across its files, with no prefix bias.
The native path uses two streaming passes (count, then exact uniform selection),
keeping native throughput and bounded memory; gzip sources are decompressed twice.
The fallback implements the same selection policy and honours SampleRate. A source
snapshot freezes plain file byte lengths; compressed sources must be immutable.

## Local verification

```powershell
cargo test -p pawnstorm -- --test-threads=1
cargo build --release -p pawnstorm
powershell -NoProfile -File tools/tests/test-prepare-data.ps1
powershell -NoProfile -File tools/tests/test-rescore.ps1
```

No Elo claim or version promotion. See EXPERIMENTS.md for measured parity and limitations.
