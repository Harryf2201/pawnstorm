//! Attack tables. Knights, kings and pawns are computed at compile time; sliding pieces use
//! lookup tables built once at startup by `init()`, indexed by PEXT where the CPU has it (BMI2)
//! and by magic multiplication elsewhere.

use crate::types::*;

const fn gen_leaper(deltas: &[(i32, i32)]) -> [u64; 64] {
    let mut t = [0u64; 64];
    let mut sq = 0;
    while sq < 64 {
        let f = (sq % 8) as i32;
        let r = (sq / 8) as i32;
        let mut i = 0;
        while i < deltas.len() {
            let nf = f + deltas[i].0;
            let nr = r + deltas[i].1;
            if nf >= 0 && nf < 8 && nr >= 0 && nr < 8 {
                t[sq] |= 1u64 << (nr * 8 + nf);
            }
            i += 1;
        }
        sq += 1;
    }
    t
}

pub static KNIGHT_ATTACKS: [u64; 64] =
    gen_leaper(&[(1, 2), (2, 1), (2, -1), (1, -2), (-1, -2), (-2, -1), (-2, 1), (-1, 2)]);
pub static KING_ATTACKS: [u64; 64] =
    gen_leaper(&[(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)]);
/// PAWN_ATTACKS[color][sq] = squares attacked by a pawn of `color` standing on `sq`.
pub static PAWN_ATTACKS: [[u64; 64]; 2] = [gen_leaper(&[(-1, 1), (1, 1)]), gen_leaper(&[(-1, -1), (1, -1)])];

const ROOK_DIRS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const BISHOP_DIRS: [(i32, i32); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];

static mut ROOK_MASK: [u64; 64] = [0; 64];
static mut ROOK_OFF: [usize; 64] = [0; 64];
static mut ROOK_TABLE: [u64; 102400] = [0; 102400];
static mut BISHOP_MASK: [u64; 64] = [0; 64];
static mut BISHOP_OFF: [usize; 64] = [0; 64];
static mut BISHOP_TABLE: [u64; 5248] = [0; 5248];
static mut BETWEEN: [[u64; 64]; 64] = [[0; 64]; 64];
static mut LINE: [[u64; 64]; 64] = [[0; 64]; 64];

fn on_board(f: i32, r: i32) -> bool {
    (0..8).contains(&f) && (0..8).contains(&r)
}

fn slide(sq: usize, occ: u64, dirs: &[(i32, i32); 4]) -> u64 {
    let mut att = 0;
    for &(df, dr) in dirs {
        let (mut f, mut r) = ((sq % 8) as i32 + df, (sq / 8) as i32 + dr);
        while on_board(f, r) {
            let s = (r * 8 + f) as usize;
            att |= bit(s);
            if occ & bit(s) != 0 {
                break;
            }
            f += df;
            r += dr;
        }
    }
    att
}

/// Squares whose occupancy matters for a slider on `sq` (the ray minus its final edge square).
fn relevant_mask(sq: usize, dirs: &[(i32, i32); 4]) -> u64 {
    let mut mask = 0;
    for &(df, dr) in dirs {
        let (mut f, mut r) = ((sq % 8) as i32 + df, (sq / 8) as i32 + dr);
        while on_board(f + df, r + dr) {
            mask |= bit((r * 8 + f) as usize);
            f += df;
            r += dr;
        }
    }
    mask
}

// Index into a slider's table. With BMI2 (x86 since Haswell/Zen 3) this is PEXT; elsewhere (e.g. ARM)
// it is a "fancy magic" multiply-and-shift, with magics found by `find_magic` at startup.
static mut ROOK_MAGIC: [u64; 64] = [0; 64];
static mut BISHOP_MAGIC: [u64; 64] = [0; 64];

#[cfg(target_feature = "bmi2")]
#[inline(always)]
fn index(occ: u64, mask: u64, _magic: u64) -> usize {
    unsafe { core::arch::x86_64::_pext_u64(occ, mask) as usize }
}

#[cfg(not(target_feature = "bmi2"))]
#[inline(always)]
fn index(occ: u64, mask: u64, magic: u64) -> usize {
    ((occ & mask).wrapping_mul(magic) >> (64 - mask.count_ones())) as usize
}

/// A multiplier that maps every occupancy subset of `mask` to a table slot without two subsets with
/// different attacks colliding. Deterministic (fixed seed), so every machine builds the same tables.
#[cfg(not(target_feature = "bmi2"))]
fn find_magic(subsets: &[u64], attacks: &[u64], mask: u64, seed: &mut u64) -> u64 {
    let bits = mask.count_ones();
    let mut used = vec![0u64; 1 << bits];
    let mut stamp = vec![0u32; 1 << bits];
    let mut rand = || {
        // xorshift64*
        *seed ^= *seed >> 12;
        *seed ^= *seed << 25;
        *seed ^= *seed >> 27;
        seed.wrapping_mul(0x2545F4914F6CDD1D)
    };
    for attempt in 1u32.. {
        let magic = rand() & rand() & rand();
        if (mask.wrapping_mul(magic) >> 56).count_ones() < 6 {
            continue;
        }
        let ok = subsets.iter().zip(attacks).all(|(&occ, &att)| {
            let i = index(occ, mask, magic);
            if stamp[i] != attempt {
                stamp[i] = attempt;
                used[i] = att;
                true
            } else {
                used[i] == att
            }
        });
        if ok {
            return magic;
        }
    }
    unreachable!()
}

/// Fills one slider's mask/offset/magic/table arrays for all 64 squares.
unsafe fn init_slider(dirs: &[(i32, i32); 4], masks: *mut [u64; 64], offs: *mut [usize; 64], magics: *mut [u64; 64], table: *mut u64) {
    let mut off = 0;
    #[cfg(not(target_feature = "bmi2"))]
    let mut seed = 0x9E3779B97F4A7C15u64;
    for sq in 0..64 {
        let mask = relevant_mask(sq, dirs);
        let (mut subsets, mut attacks) = (Vec::new(), Vec::new());
        let mut sub = 0u64;
        loop {
            subsets.push(sub);
            attacks.push(slide(sq, sub, dirs));
            sub = sub.wrapping_sub(mask) & mask;
            if sub == 0 {
                break;
            }
        }
        #[cfg(not(target_feature = "bmi2"))]
        let magic = find_magic(&subsets, &attacks, mask, &mut seed);
        #[cfg(target_feature = "bmi2")]
        let magic = 0;
        (*masks)[sq] = mask;
        (*offs)[sq] = off;
        (*magics)[sq] = magic;
        for (&occ, &att) in subsets.iter().zip(&attacks) {
            *table.add(off + index(occ, mask, magic)) = att;
        }
        off += 1 << mask.count_ones();
    }
}

pub fn init() {
    unsafe {
        init_slider(&ROOK_DIRS, &raw mut ROOK_MASK, &raw mut ROOK_OFF, &raw mut ROOK_MAGIC, (&raw mut ROOK_TABLE).cast());
        init_slider(&BISHOP_DIRS, &raw mut BISHOP_MASK, &raw mut BISHOP_OFF, &raw mut BISHOP_MAGIC, (&raw mut BISHOP_TABLE).cast());
        for a in 0..64 {
            for b in 0..64 {
                if a == b {
                    continue;
                }
                if rook_attacks(a, 0) & bit(b) != 0 {
                    LINE[a][b] = (rook_attacks(a, 0) & rook_attacks(b, 0)) | bit(a) | bit(b);
                    BETWEEN[a][b] = rook_attacks(a, bit(b)) & rook_attacks(b, bit(a));
                } else if bishop_attacks(a, 0) & bit(b) != 0 {
                    LINE[a][b] = (bishop_attacks(a, 0) & bishop_attacks(b, 0)) | bit(a) | bit(b);
                    BETWEEN[a][b] = bishop_attacks(a, bit(b)) & bishop_attacks(b, bit(a));
                }
            }
        }
    }
}

#[inline(always)]
pub fn rook_attacks(sq: usize, occ: u64) -> u64 {
    unsafe { ROOK_TABLE[ROOK_OFF[sq] + index(occ, ROOK_MASK[sq], ROOK_MAGIC[sq])] }
}

#[inline(always)]
pub fn bishop_attacks(sq: usize, occ: u64) -> u64 {
    unsafe { BISHOP_TABLE[BISHOP_OFF[sq] + index(occ, BISHOP_MASK[sq], BISHOP_MAGIC[sq])] }
}

#[inline(always)]
pub fn queen_attacks(sq: usize, occ: u64) -> u64 {
    rook_attacks(sq, occ) | bishop_attacks(sq, occ)
}

/// Squares strictly between a and b if they share a line, else 0.
#[inline(always)]
pub fn between(a: usize, b: usize) -> u64 {
    unsafe { BETWEEN[a][b] }
}

/// The full line through a and b (edge to edge) if they share one, else 0.
#[inline(always)]
pub fn line(a: usize, b: usize) -> u64 {
    unsafe { LINE[a][b] }
}
