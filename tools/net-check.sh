#!/bin/bash
# Network check on held-out positions (testing/datagen/holdout-gen6, never used for training):
#   1. validation loss of the trainer's float network (same target as training: wdl * result + (1 - wdl) * sigmoid(score / 400))
#   2. trainer-vs-engine agreement: how far the engine's quantised inference is from the float network.
# Usage: tools/net-check.sh <training-run> <checkpoint> <inputs: ml|ml32|mlkb4|mlthr> <hidden> <engine-exe-name> [positions=4000] [wdl=0.4]
#   e.g. tools/net-check.sh net-gen5x net-gen5x-240 ml 768 pawnstorm-w768-build
set -e
run=$1; ckpt=$2; inputs=$3; hidden=$4; exe=$5; n=${6:-4000}; wdl=${7:-0.4}
here=$(cd "$(dirname "$0")/.." && pwd); lab=$(cd "$here/../testing" && pwd)
work=$lab/analysis/net-check/$ckpt; mkdir -p "$work"
# A fixed, spread-out sample of the hold-out (same positions every time).
if [ ! -s "$lab/analysis/net-check/sample-$n.txt" ]; then
    zcat "$lab"/datagen/holdout-gen6/*.gz | awk -v n="$n" 'NR % 1500 == 0 && c < n { print; c++ }' > "$lab/analysis/net-check/sample-$n.txt"
fi
sample=$lab/analysis/net-check/sample-$n.txt
cut -d'|' -f1 "$sample" | sed 's/ *$//' > "$work/fens.txt"
# Trainer (float) evaluation, side to move, in units of 400 cp.
"$here/trainer/target/release/trainer.exe" "$run" "$(cygpath -w "$lab/training/data/val-gen6.bin")" \
    "$(cygpath -w "$lab/training/$run/checkpoints/$ckpt")" 1 "$wdl" "$hidden" 8 "$inputs" "eval=$(cygpath -w "$work/fens.txt")" 2>/dev/null \
    | grep -E '^-?[0-9]' | cut -f1 > "$work/trainer.txt"
# Engine (quantised) evaluation of the same network, side to move, centipawns.
net=$(cygpath -m "$lab/training/$run/checkpoints/$ckpt/quantised.bin")
{ printf 'uci\nsetoption name EvalFile value %s\nisready\n' "$net"; while read -r f; do printf 'position fen %s\neval\n' "$f"; done < "$work/fens.txt"; echo quit; } \
    | "$lab/engines/$exe.exe" | grep -oE '^nnue: +-?[0-9]+' | grep -oE -- '-?[0-9]+$' > "$work/engine.txt"
paste "$sample" "$work/trainer.txt" "$work/engine.txt" | awk -F'\t' -v wdl="$wdl" '
function sig(x) { return 1 / (1 + exp(-x)) }
{
    split($1, p, "|"); fen = p[1]; score = p[2] + 0; result = p[3] + 0
    split(fen, f, " "); stm = f[2]
    if (stm == "b") { score = -score; result = 1 - result }
    t = $2 + 0; e = $3 + 0
    target = wdl * result + (1 - wdl) * sig(score / 400)
    loss += (sig(t) - target) ^ 2
    d = e - t * 400; if (d < 0) d = -d
    diffs[NR] = d; sum += d; if (d > mx) mx = d
    if ((t > 0.05 && e < -20) || (t < -0.05 && e > 20)) flips++
    n++
}
END {
    asort(diffs)
    printf "positions          %d\n", n
    printf "validation loss    %.6f\n", loss / n
    printf "engine vs trainer  mean %.1f cp, median %.1f, 95th %.1f, 99th %.1f, max %.1f cp\n", sum / n, diffs[int(n * 0.5)], diffs[int(n * 0.95)], diffs[int(n * 0.99)], mx
    printf "sign flips         %d (trainer and engine disagree on who is better by >20 cp)\n", flips + 0
}' | tee "$work/report.txt"
