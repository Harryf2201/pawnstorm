//! NNUE evaluation, multi-layer (trainer -Inputs ml): (768 -> HIDDEN, clipped ReLU, pairwise products of the
//! two halves) x2 -> L1 -> L2 -> 1, every layer after the first chosen by material (OUTPUT_BUCKETS).
//! The first layer is i16 (incrementally updated accumulators); the small layers run in f32.
//!
//! Each side has an accumulator (the first layer's output). Moves only change a few input
//! features, so the search updates accumulators incrementally instead of recomputing them.
//! The default network is embedded from nets/default.bin; an empty file means "no network",
//! in which case the engine falls back to the hand-crafted evaluation.

use crate::board::Board;
use crate::types::*;

/// Must match the trainer's hidden size and bucket count (trainer args 6 and 7).
pub const HIDDEN: usize = 768;
pub const OUTPUT_BUCKETS: usize = 8;
const L1: usize = 16;
const L2: usize = 32;
const SCALE: f32 = 400.0;
const QA: i32 = 255;

/// King input buckets (bullet's ChessBucketsMirrored, trainer -Inputs mlkb4): each side's inputs use a different set of
/// first-layer weights depending on where its own king stands (from its own point of view), and the board is
/// mirrored left-right when that king is on the e-h files. Must match the trainer's KING_BUCKETS_4.
#[rustfmt::skip]
const BUCKET_LAYOUT: [usize; 32] = [
    0, 0, 1, 1,
    2, 2, 2, 2,
    3, 3, 3, 3,
    3, 3, 3, 3,
    3, 3, 3, 3,
    3, 3, 3, 3,
    3, 3, 3, 3,
    3, 3, 3, 3,
];
pub const INPUT_BUCKETS: usize = 4;

/// (bucket, file flip) for a king on `ksq`, given in its own side's orientation.
#[inline(always)]
fn king_bucket(ksq: usize) -> (usize, usize) {
    let file = ksq % 8;
    let col = if file > 3 { 7 - file } else { file };
    (BUCKET_LAYOUT[(ksq / 8) * 4 + col], if file > 3 { 7 } else { 0 })
}

/// The bucket and flip used for `persp`'s inputs in position `b`.
#[inline(always)]
fn persp_bucket(b: &Board, persp: usize) -> (usize, usize) {
    let ksq = (b.pieces[KING] & b.colors[persp]).trailing_zeros() as usize;
    king_bucket(if persp == WHITE { ksq } else { ksq ^ 56 })
}

/// Size of bullet's quantised output (it may be padded to a multiple of 64).
pub const NET_BYTES: usize = (768 * INPUT_BUCKETS * HIDDEN + HIDDEN) * 2
    + (HIDDEN * OUTPUT_BUCKETS * L1 + OUTPUT_BUCKETS * L1 + L1 * OUTPUT_BUCKETS * L2 + OUTPUT_BUCKETS * L2 + L2 * OUTPUT_BUCKETS + OUTPUT_BUCKETS) * 4;

#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct Accumulator {
    pub vals: [i16; HIDDEN],
}

#[repr(C)]
pub struct Network {
    feature_weights: [Accumulator; 768 * INPUT_BUCKETS],
    feature_bias: Accumulator,
    // bullet's native (input-major) layout: for each input, the outputs of every bucket in a row.
    l1w: [[[f32; L1]; OUTPUT_BUCKETS]; HIDDEN],
    l1b: [[f32; L1]; OUTPUT_BUCKETS],
    l2w: [[[f32; L2]; OUTPUT_BUCKETS]; L1],
    l2b: [[f32; L2]; OUTPUT_BUCKETS],
    l3w: [[f32; OUTPUT_BUCKETS]; L2],
    l3b: [f32; OUTPUT_BUCKETS],
}

static DEFAULT_NET: &[u8] = include_bytes!("../nets/default.bin");
static mut NET: Option<&'static Network> = None;
/// Bumped whenever a network is loaded, so accumulator caches know to start over.
static NET_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Layer 1 quantised to i8 for speed (built from the f32 weights when a network loads), one row of HIDDEN
/// weights per (bucket, output). `mul` turns the integer dot product back into the float pre-activation.
struct L1Quant {
    w: Vec<i8>,
    /// Per (bucket, output) row.
    mul: Vec<f32>,
}
static mut L1Q: Option<&'static L1Quant> = None;

fn set_net(net: &'static Network) {
    let mut w = vec![0i8; OUTPUT_BUCKETS * L1 * HIDDEN];
    let mut mul = vec![0f32; OUTPUT_BUCKETS * L1];
    for b in 0..OUTPUT_BUCKETS {
        for o in 0..L1 {
            // Each row gets its own scale, so small rows keep their precision.
            let max = (0..HIDDEN).fold(0f32, |m, i| m.max(net.l1w[i][b][o].abs())).max(1e-6);
            let scale = 127.0 / max;
            for i in 0..HIDDEN {
                w[(b * L1 + o) * HIDDEN + i] = (net.l1w[i][b][o] * scale).round() as i8;
            }
            // Inputs are round(a * b / 512) with a, b in 0..=255, i.e. x * 65025 / 512.
            mul[b * L1 + o] = 512.0 / (65025.0 * scale);
        }
    }
    unsafe {
        L1Q = Some(Box::leak(Box::new(L1Quant { w, mul })));
        NET = Some(net);
    }
    NET_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn from_bytes(bytes: &[u8]) -> Result<&'static Network, String> {
    if bytes.len() < NET_BYTES {
        return Err(format!("network file is {} bytes, expected at least {NET_BYTES}", bytes.len()));
    }
    unsafe {
        let layout = std::alloc::Layout::new::<Network>();
        let ptr = std::alloc::alloc_zeroed(layout);
        if ptr.is_null() {
            return Err("out of memory".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, NET_BYTES.min(layout.size()));
        Ok(&*(ptr as *const Network))
    }
}

/// Loads the embedded network, if one was built in.
pub fn init() {
    if let Ok(net) = from_bytes(DEFAULT_NET) {
        set_net(net);
    }
}

/// Loads a network from disk (UCI option EvalFile). Not safe to call during a search.
pub fn load_file(path: &str) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let net = from_bytes(&bytes)?;
    set_net(net);
    Ok(())
}

#[inline(always)]
fn net() -> &'static Network {
    unsafe { NET.unwrap_unchecked() }
}

#[inline(always)]
pub fn active() -> bool {
    unsafe { NET.is_some() }
}

/// Input feature for piece `pc` (color * 6 + type) on `sq`, seen from `persp`'s side, whose king
/// bucket and file flip are `kb`.
#[inline(always)]
fn feature(persp: usize, kb: (usize, usize), pc: usize, sq: usize) -> usize {
    let (c, pt) = (pc / 6, pc % 6);
    let f = if persp == WHITE {
        if c == WHITE { 64 * pt + sq } else { 384 + 64 * pt + sq }
    } else if c == BLACK {
        64 * pt + (sq ^ 56)
    } else {
        384 + 64 * pt + (sq ^ 56)
    };
    768 * kb.0 + (f ^ kb.1)
}

/// Accumulators for both perspectives, indexed by color.
pub type AccPair = [Accumulator; 2];

pub fn refresh(b: &Board, acc: &mut AccPair) {
    let net = net();
    for (persp, a) in acc.iter_mut().enumerate() {
        let kb = persp_bucket(b, persp);
        *a = net.feature_bias;
        for sq in 0..64 {
            let pc = b.mailbox[sq];
            if pc != NO_PIECE {
                let w = &net.feature_weights[feature(persp, kb, pc as usize, sq)].vals;
                for (v, d) in a.vals.iter_mut().zip(w) {
                    *v += *d;
                }
            }
        }
    }
}

#[derive(Clone)]
struct CacheEntry {
    acc: Accumulator,
    /// The pieces (color * 6 + type) the cached accumulator was built for.
    bbs: [u64; 12],
}

/// Accumulator cache ("Finny table"), one entry per perspective, king bucket and mirror side.
/// When a king move changes its bucket, the new accumulator is made from the cached one for that
/// bucket by applying only the pieces that differ, instead of summing every piece from scratch.
pub struct AccCache {
    gen: u32,
    e: Vec<CacheEntry>,
}

impl AccCache {
    pub fn new() -> AccCache {
        AccCache { gen: u32::MAX, e: vec![CacheEntry { acc: Accumulator { vals: [0; HIDDEN] }, bbs: [0; 12] }; 2 * INPUT_BUCKETS * 2] }
    }

    /// Makes `persp`'s accumulator for position `b` into `out`, via the cache.
    fn refresh_one(&mut self, b: &Board, persp: usize, out: &mut Accumulator) {
        let net = net();
        let gen = NET_GEN.load(std::sync::atomic::Ordering::Relaxed);
        if gen != self.gen {
            for e in self.e.iter_mut() {
                e.acc = net.feature_bias;
                e.bbs = [0; 12];
            }
            self.gen = gen;
        }
        let kb = persp_bucket(b, persp);
        let e = &mut self.e[(persp * INPUT_BUCKETS + kb.0) * 2 + (kb.1 != 0) as usize];
        for pc in 0..12 {
            let cur = b.pieces[pc % 6] & b.colors[pc / 6];
            let (mut add, mut sub) = (cur & !e.bbs[pc], e.bbs[pc] & !cur);
            while add != 0 {
                let w = &net.feature_weights[feature(persp, kb, pc, add.trailing_zeros() as usize)].vals;
                for (v, d) in e.acc.vals.iter_mut().zip(w) {
                    *v += *d;
                }
                add &= add - 1;
            }
            while sub != 0 {
                let w = &net.feature_weights[feature(persp, kb, pc, sub.trailing_zeros() as usize)].vals;
                for (v, d) in e.acc.vals.iter_mut().zip(w) {
                    *v -= *d;
                }
                sub &= sub - 1;
            }
            e.bbs[pc] = cur;
        }
        *out = e.acc;
    }
}

impl Default for AccCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Computes the child's accumulators from the parent's for `mv` played in position `b`
/// (`after` is the position after the move). A king move into another bucket, or across the
/// d/e-file mirror line, changes every input of that side, so its accumulator is rebuilt (from `cache`).
pub fn update(b: &Board, after: &Board, mv: Move, parent: &AccPair, child: &mut AccPair, cache: &mut AccCache) {
    let (from, to) = (mv.from(), mv.to());
    let us = b.stm;
    let moved = b.mailbox[from] as usize;
    let placed = if mv.is_promo() { us * 6 + mv.promo_piece() } else { moved };

    let mut adds = [(0usize, 0usize); 2];
    let mut subs = [(0usize, 0usize); 2];
    let (mut na, mut ns) = (0, 0);
    adds[na] = (placed, to);
    na += 1;
    subs[ns] = (moved, from);
    ns += 1;
    if mv.is_capture() {
        let cap_sq = if mv.is_ep() { to ^ 8 } else { to };
        subs[ns] = (b.mailbox[cap_sq] as usize, cap_sq);
        ns += 1;
    } else if mv.is_castle() {
        let rook = us * 6 + ROOK;
        let (rf, rt) = if mv.flag() == Move::KING_CASTLE { (to + 1, to - 1) } else { (to - 2, to + 1) };
        adds[na] = (rook, rt);
        na += 1;
        subs[ns] = (rook, rf);
        ns += 1;
    }

    let net = net();
    for persp in 0..2 {
        let kb = persp_bucket(b, persp);
        if persp == us && moved % 6 == KING && persp_bucket(after, persp) != kb {
            cache.refresh_one(after, persp, &mut child[persp]);
            continue;
        }
        let add: [&[i16; HIDDEN]; 2] = [
            &net.feature_weights[feature(persp, kb, adds[0].0, adds[0].1)].vals,
            &net.feature_weights[feature(persp, kb, adds[1].0, adds[1].1)].vals,
        ];
        let sub: [&[i16; HIDDEN]; 2] = [
            &net.feature_weights[feature(persp, kb, subs[0].0, subs[0].1)].vals,
            &net.feature_weights[feature(persp, kb, subs[1].0, subs[1].1)].vals,
        ];
        let src = &parent[persp].vals;
        let dst = &mut child[persp].vals;
        // Fused copy + update, specialised so each case is one vectorised pass.
        match (na, ns) {
            (1, 1) => {
                for i in 0..HIDDEN {
                    dst[i] = src[i] + add[0][i] - sub[0][i];
                }
            }
            (1, 2) => {
                for i in 0..HIDDEN {
                    dst[i] = src[i] + add[0][i] - sub[0][i] - sub[1][i];
                }
            }
            _ => {
                for i in 0..HIDDEN {
                    dst[i] = src[i] + add[0][i] + add[1][i] - sub[0][i] - sub[1][i];
                }
            }
        }
    }
}

/// One side's layer-1 inputs: round(clip(a) * clip(b) / 512) for the two halves of its accumulator.
#[cfg(target_feature = "avx2")]
#[inline(always)]
fn pairwise_inputs(v: &[i16; HIDDEN], out: &mut [u8]) {
    use std::arch::x86_64::*;
    const HALF: usize = HIDDEN / 2;
    unsafe {
        let zero = _mm256_setzero_si256();
        let qa = _mm256_set1_epi16(QA as i16);
        let clip = |p: *const i16| _mm256_min_epi16(_mm256_max_epi16(_mm256_load_si256(p as *const __m256i), zero), qa);
        let mut i = 0;
        while i < HALF {
            // mulhrs((a << 6), b) = (a * b * 64 + 2^14) >> 15 = round(a * b / 512)
            let r0 = _mm256_mulhrs_epi16(_mm256_slli_epi16::<6>(clip(v.as_ptr().add(i))), clip(v.as_ptr().add(i + HALF)));
            let r1 = _mm256_mulhrs_epi16(_mm256_slli_epi16::<6>(clip(v.as_ptr().add(i + 16))), clip(v.as_ptr().add(i + HALF + 16)));
            // packus interleaves the 128-bit lanes; the permute puts them back in order.
            let packed = _mm256_permute4x64_epi64::<0b11_01_10_00>(_mm256_packus_epi16(r0, r1));
            _mm256_storeu_si256(out.as_mut_ptr().add(i) as *mut __m256i, packed);
            i += 32;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn pairwise_inputs(v: &[i16; HIDDEN], out: &mut [u8]) {
    use std::arch::aarch64::*;
    const HALF: usize = HIDDEN / 2;
    unsafe {
        let zero = vdupq_n_s16(0);
        let qa = vdupq_n_s16(QA as i16);
        let clip = |p: *const i16| vminq_s16(vmaxq_s16(vld1q_s16(p), zero), qa);
        let mut i = 0;
        while i < HALF {
            // vqrdmulh(a << 6, b) = (a * b * 64 * 2 + 2^15) >> 16 = round(a * b / 512), as mulhrs on x86.
            let r = vqrdmulhq_s16(vshlq_n_s16::<6>(clip(v.as_ptr().add(i))), clip(v.as_ptr().add(i + HALF)));
            vst1_u8(out.as_mut_ptr().add(i), vqmovun_s16(r));
            i += 8;
        }
    }
}

#[cfg(not(any(target_feature = "avx2", target_arch = "aarch64")))]
#[inline(always)]
fn pairwise_inputs(v: &[i16; HIDDEN], out: &mut [u8]) {
    const HALF: usize = HIDDEN / 2;
    for i in 0..HALF {
        let a = v[i].clamp(0, QA as i16) as u32;
        let b = v[i + HALF].clamp(0, QA as i16) as u32;
        out[i] = ((a * b + 256) >> 9) as u8;
    }
}

/// Dot product of 0..=127 inputs with i8 weights (HIDDEN long).
#[cfg(target_feature = "avx2")]
#[inline(always)]
fn dot_u8_i8(x: &[u8; HIDDEN], w: &[i8]) -> i32 {
    use std::arch::x86_64::*;
    unsafe {
        let ones = _mm256_set1_epi16(1);
        let mut sum = _mm256_setzero_si256();
        let mut i = 0;
        while i < HIDDEN {
            let xv = _mm256_load_si256(x.as_ptr().add(i) as *const __m256i);
            let wv = _mm256_loadu_si256(w.as_ptr().add(i) as *const __m256i);
            // u8 * i8 pairs summed into i16 (at most 2 * 127 * 127, no saturation), then into i32.
            sum = _mm256_add_epi32(sum, _mm256_madd_epi16(_mm256_maddubs_epi16(xv, wv), ones));
            i += 32;
        }
        let s = _mm_add_epi32(_mm256_castsi256_si128(sum), _mm256_extracti128_si256::<1>(sum));
        let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b01_00_11_10>(s));
        let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b10_11_00_01>(s));
        _mm_cvtsi128_si32(s)
    }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn dot_u8_i8(x: &[u8; HIDDEN], w: &[i8]) -> i32 {
    use std::arch::aarch64::*;
    // The inputs are 0..=127, so they are also valid i8. Four independent sums so the multiply-accumulates
    // don't wait on each other.
    unsafe {
        let mut s = [vdupq_n_s32(0); 4];
        let mut i = 0;
        while i < HIDDEN {
            for (k, sk) in s.iter_mut().enumerate() {
                let xv = vreinterpretq_s8_u8(vld1q_u8(x.as_ptr().add(i + 16 * k)));
                let wv = vld1q_s8(w.as_ptr().add(i + 16 * k));
                // sdot: four i8 products summed into each i32 lane (ARMv8.2 dot product); otherwise widening
                // multiplies pairwise-added into i32.
                #[cfg(target_feature = "dotprod")]
                {
                    *sk = vdotq_s32(*sk, xv, wv);
                }
                #[cfg(not(target_feature = "dotprod"))]
                {
                    *sk = vpadalq_s16(*sk, vmull_s8(vget_low_s8(xv), vget_low_s8(wv)));
                    *sk = vpadalq_s16(*sk, vmull_high_s8(xv, wv));
                }
            }
            i += 64;
        }
        vaddvq_s32(vaddq_s32(vaddq_s32(s[0], s[1]), vaddq_s32(s[2], s[3])))
    }
}

#[cfg(not(any(target_feature = "avx2", target_arch = "aarch64")))]
#[inline(always)]
fn dot_u8_i8(x: &[u8; HIDDEN], w: &[i8]) -> i32 {
    x.iter().zip(w).map(|(&a, &b)| a as i32 * b as i32).sum()
}

/// Evaluation in centipawns from the side to move's point of view.
/// `pieces` = number of pieces on the board (both colors, kings included), which picks the bucket.
pub fn evaluate(acc: &AccPair, stm: usize, pieces: u32) -> i32 {
    let net = net();
    // Same rule as bullet's MaterialCount<N>: (pieces - 2) / ceil(32 / N).
    let bucket = ((pieces.saturating_sub(2)) as usize / 32usize.div_ceil(OUTPUT_BUCKETS)).min(OUTPUT_BUCKETS - 1);
    const HALF: usize = HIDDEN / 2;
    // Layer 1 in 8-bit integers: each side's clipped first-layer outputs, the first half times the second
    // half (side to move first), scaled to 0..=127, dotted with the i8 weights of this bucket's 16 outputs.
    #[repr(align(32))]
    struct Inputs([u8; HIDDEN]);
    let mut x = Inputs([0; HIDDEN]);
    for (side, base) in [(stm, 0), (stm ^ 1, HALF)] {
        pairwise_inputs(&acc[side].vals, &mut x.0[base..base + HALF]);
    }
    let q = unsafe { L1Q.unwrap_unchecked() };
    let mut h1 = net.l1b[bucket];
    for (o, h) in h1.iter_mut().enumerate() {
        let row = &q.w[(bucket * L1 + o) * HIDDEN..(bucket * L1 + o + 1) * HIDDEN];
        *h += dot_u8_i8(&x.0, row) as f32 * q.mul[bucket * L1 + o];
    }
    let mut h2 = net.l2b[bucket];
    for (j, &h) in h1.iter().enumerate() {
        let x = h.clamp(0.0, 1.0);
        let x = x * x;
        let w = &net.l2w[j][bucket];
        for o in 0..L2 {
            h2[o] += w[o] * x;
        }
    }
    let mut out = net.l3b[bucket];
    for (k, &h) in h2.iter().enumerate() {
        let x = h.clamp(0.0, 1.0);
        out += net.l3w[k][bucket] * x * x;
    }
    (out * SCALE) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attacks;

    /// Incremental updates must exactly match a from-scratch refresh after every kind of move.
    #[test]
    fn incremental_matches_refresh() {
        attacks::init();
        let mut seed = 0x1234_5678_9abc_def1u64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let bytes: Vec<u8> = (0..NET_BYTES / 2).flat_map(|_| ((rand() % 101) as i16 - 50).to_le_bytes()).collect();
        set_net(from_bytes(&bytes).unwrap());

        let fens = [
            crate::board::START_FEN,
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
            "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
        ];
        let mut checked = 0;
        let mut cache = AccCache::new();
        for fen in fens {
            for _ in 0..40 {
                let mut b = Board::from_fen(fen).unwrap();
                let mut acc = [Accumulator { vals: [0; HIDDEN] }; 2];
                refresh(&b, &mut acc);
                for _ in 0..120 {
                    let moves = b.legal_moves();
                    if moves.is_empty() {
                        break;
                    }
                    let mv = moves[(rand() % moves.len() as u64) as usize];
                    let mut next = [Accumulator { vals: [0; HIDDEN] }; 2];
                    let mut after = b;
                    after.make_move(mv);
                    update(&b, &after, mv, &acc, &mut next, &mut cache);
                    b = after;
                    let mut fresh = [Accumulator { vals: [0; HIDDEN] }; 2];
                    refresh(&b, &mut fresh);
                    for p in 0..2 {
                        assert_eq!(next[p].vals, fresh[p].vals, "mismatch after {mv} in {}", b.to_fen());
                    }
                    acc = next;
                    checked += 1;
                }
            }
        }
        assert!(checked > 10_000);
    }
}

/// Full evaluation from scratch (for the "eval" command and tests).
pub fn evaluate_board(b: &Board) -> i32 {
    let mut acc = [Accumulator { vals: [0; HIDDEN] }; 2];
    refresh(b, &mut acc);
    evaluate(&acc, b.stm, b.occ().count_ones())
}
