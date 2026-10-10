//! Attack tables. Knights, kings and pawns are computed at compile time; sliding pieces use
//! PEXT-indexed lookup tables built once at startup by `init()`.

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

#[inline(always)]
fn pext(x: u64, mask: u64) -> u64 {
    #[cfg(target_feature = "bmi2")]
    unsafe {
        core::arch::x86_64::_pext_u64(x, mask)
    }
    #[cfg(not(target_feature = "bmi2"))]
    {
        let (mut res, mut bb, mut m) = (0u64, 1u64, mask);
        while m != 0 {
            if x & m & m.wrapping_neg() != 0 {
                res |= bb;
            }
            bb <<= 1;
            m &= m - 1;
        }
        res
    }
}

pub fn init() {
    unsafe {
        let mut off = 0;
        for sq in 0..64 {
            let mask = relevant_mask(sq, &ROOK_DIRS);
            ROOK_MASK[sq] = mask;
            ROOK_OFF[sq] = off;
            let mut sub = 0u64;
            loop {
                ROOK_TABLE[off + pext(sub, mask) as usize] = slide(sq, sub, &ROOK_DIRS);
                sub = sub.wrapping_sub(mask) & mask;
                if sub == 0 {
                    break;
                }
            }
            off += 1 << mask.count_ones();
        }
        let mut off = 0;
        for sq in 0..64 {
            let mask = relevant_mask(sq, &BISHOP_DIRS);
            BISHOP_MASK[sq] = mask;
            BISHOP_OFF[sq] = off;
            let mut sub = 0u64;
            loop {
                BISHOP_TABLE[off + pext(sub, mask) as usize] = slide(sq, sub, &BISHOP_DIRS);
                sub = sub.wrapping_sub(mask) & mask;
                if sub == 0 {
                    break;
                }
            }
            off += 1 << mask.count_ones();
        }
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
    unsafe { ROOK_TABLE[ROOK_OFF[sq] + pext(occ, ROOK_MASK[sq]) as usize] }
}

#[inline(always)]
pub fn bishop_attacks(sq: usize, occ: u64) -> u64 {
    unsafe { BISHOP_TABLE[BISHOP_OFF[sq] + pext(occ, BISHOP_MASK[sq]) as usize] }
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
