//! Lockless transposition table (safe to share between search threads).
//! Each entry is two atomics: `key ^ data` and `data`, so a torn write is detected on probe.

use crate::types::Move;
use crate::params::{p, P};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering::Relaxed};

pub const BOUND_NONE: u8 = 0;
pub const BOUND_UPPER: u8 = 1;
pub const BOUND_LOWER: u8 = 2;
pub const BOUND_EXACT: u8 = 3;

#[derive(Clone, Copy)]
pub struct TTData {
    pub mv: Move,
    pub score: i32,
    pub eval: i32,
    pub depth: i32,
    pub bound: u8,
    /// The position was on a principal variation when stored (or earlier).
    pub pv: bool,
}

#[derive(Default)]
struct Entry {
    key: AtomicU64,
    data: AtomicU64,
}

pub struct TT {
    entries: Vec<Entry>,
    age: AtomicU8,
}

// data layout: mv 0-15 | score 16-31 | eval 32-47 | depth 48-55 | bound 56-57 | age 58-62 | pv 63
fn pack(mv: Move, score: i32, eval: i32, depth: i32, bound: u8, age: u8, pv: bool) -> u64 {
    mv.0 as u64
        | ((score as i16 as u16 as u64) << 16)
        | ((eval as i16 as u16 as u64) << 32)
        | ((depth.clamp(0, 255) as u64) << 48)
        | ((bound as u64) << 56)
        | (((age & 31) as u64) << 58)
        | ((pv as u64) << 63)
}

impl TT {
    pub fn new(mb: usize) -> TT {
        let count = (mb.max(1) * 1024 * 1024 / std::mem::size_of::<Entry>()).max(1);
        let mut entries = Vec::with_capacity(count);
        entries.resize_with(count, Entry::default);
        TT { entries, age: AtomicU8::new(0) }
    }

    #[inline(always)]
    fn index(&self, key: u64) -> usize {
        ((key as u128 * self.entries.len() as u128) >> 64) as usize
    }

    pub fn clear(&self) {
        for e in &self.entries {
            e.key.store(0, Relaxed);
            e.data.store(0, Relaxed);
        }
        self.age.store(0, Relaxed);
    }

    pub fn new_search(&self) {
        self.age.store(self.age.load(Relaxed).wrapping_add(1) & 31, Relaxed);
    }

    pub fn prefetch(&self, key: u64) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            use core::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
            _mm_prefetch(&self.entries[self.index(key)] as *const Entry as *const i8, _MM_HINT_T0);
        }
    }

    pub fn probe(&self, key: u64) -> Option<TTData> {
        let e = &self.entries[self.index(key)];
        let data = e.data.load(Relaxed);
        if e.key.load(Relaxed) ^ data != key {
            return None;
        }
        let bound = ((data >> 56) & 3) as u8;
        if bound == BOUND_NONE {
            return None;
        }
        Some(TTData {
            mv: Move(data as u16),
            score: (data >> 16) as u16 as i16 as i32,
            eval: (data >> 32) as u16 as i16 as i32,
            depth: ((data >> 48) & 0xFF) as i32,
            bound,
            pv: data >> 63 != 0,
        })
    }

    pub fn store(&self, key: u64, mut mv: Move, score: i32, eval: i32, depth: i32, bound: u8, pv: bool) {
        let e = &self.entries[self.index(key)];
        let old_data = e.data.load(Relaxed);
        let same = e.key.load(Relaxed) ^ old_data == key;
        let age = self.age.load(Relaxed);
        if same && mv.is_null() {
            mv = Move(old_data as u16);
        }
        let old_depth = ((old_data >> 48) & 0xFF) as i32;
        let old_age = ((old_data >> 58) & 31) as u8;
        // Sprint 7 (SprTtAge > 0): a different position only replaces an empty slot, or a deep entry once it is
        // shallow enough relative to its age.
        let keep_other = p(P::SprTtAge) > 0
            && !same
            && (old_data >> 56) & 3 != 0
            && depth + 4 + p(P::SprTtAge) * ((age as i32 - old_age as i32).rem_euclid(32)) <= old_depth;
        if !keep_other && (!same || bound == BOUND_EXACT || depth + 4 > old_depth || old_age != age) {
            let data = pack(mv, score, eval, depth, bound, age, pv);
            e.data.store(data, Relaxed);
            e.key.store(key ^ data, Relaxed);
        }
    }

    /// Permill of sampled entries written during the current search.
    pub fn hashfull(&self) -> usize {
        let age = self.age.load(Relaxed);
        self.entries
            .iter()
            .take(1000)
            .filter(|e| {
                let d = e.data.load(Relaxed);
                (d >> 56) & 3 != 0 && ((d >> 58) & 31) as u8 == age
            })
            .count()
    }
}
