//! Principal variation search with the standard modern pruning/reduction toolkit.

use crate::board::*;
use crate::eval::evaluate;
use crate::nnue::{self, AccPair, Accumulator};
use crate::tt::*;
use crate::types::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub const MAX_PLY: usize = 128;
pub const INF: i32 = 32000;
pub const MATE: i32 = 31000;
pub const MATE_BOUND: i32 = MATE - MAX_PLY as i32;
const NO_EVAL: i32 = -INF - 1;
const NO_CONT: usize = usize::MAX;
const HIST_MAX: i32 = 16384;

#[derive(Clone, Default)]
pub struct Limits {
    pub depth: Option<i32>,
    pub nodes: Option<u64>,
    /// Stop starting new iterations after this many nodes (used by datagen).
    pub soft_nodes: Option<u64>,
    pub movetime: Option<u64>,
    pub time: [Option<u64>; 2],
    pub inc: [u64; 2],
    pub movestogo: Option<u64>,
    pub infinite: bool,
}

#[derive(Clone, Copy)]
struct PlyInfo {
    static_eval: i32,
    /// piece * 64 + to of the move played at this ply (for continuation history), or NO_CONT.
    cont: usize,
    excluded: Move,
}

impl Default for PlyInfo {
    fn default() -> Self {
        PlyInfo { static_eval: NO_EVAL, cont: NO_CONT, excluded: Move::NULL }
    }
}

pub struct Searcher {
    tt: Arc<TT>,
    stop: Arc<AtomicBool>,
    pub nodes: u64,
    seldepth: usize,
    /// Position hashes of the game so far plus the current search path (last = current position).
    pub hashes: Vec<u64>,
    butterfly: Box<[[[i32; 64]; 64]; 2]>,
    capt_hist: Box<[[[i32; 6]; 64]; 12]>,
    cont1: Vec<i32>,
    cont2: Vec<i32>,
    killers: [[Move; 2]; MAX_PLY + 2],
    stack: [PlyInfo; MAX_PLY + 4],
    pv: Box<[[Move; MAX_PLY + 1]; MAX_PLY + 1]>,
    pv_len: [usize; MAX_PLY + 1],
    lmr: [[i32; 64]; 64],
    /// Correction history: per side to move and pawn structure, how far the static eval has
    /// tended to be from the search result (in 1/CORR_GRAIN pawns-of-centipawns).
    corr: Box<[[i32; CORR_SIZE]; 2]>,
    /// NNUE accumulators per ply.
    acc: Vec<AccPair>,
    /// Nodes spent below each root move (from, to) in this search, for time management.
    root_nodes: Box<[[u64; 64]; 64]>,
    /// Whether the soft limit comes from a game clock (and may be stretched or shortened).
    clock_tm: bool,
    start: Instant,
    soft_ms: u64,
    hard_ms: u64,
    node_limit: u64,
    stopped: bool,
    pub print: bool,
    pub overhead_ms: u64,
}

const CORR_SIZE: usize = 16384;
const CORR_GRAIN: i32 = 256;
const CORR_WEIGHT_SCALE: i32 = 256;
const CORR_MAX: i32 = CORR_GRAIN * 32;

/// Hash of the pawn structure (both colors), for the correction history.
#[inline(always)]
fn pawn_key(b: &Board) -> usize {
    let mix = |mut x: u64| {
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51afd7ed558ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
        x ^ (x >> 33)
    };
    let p = b.pieces[PAWN];
    (mix(p & b.colors[WHITE]) ^ mix((p & b.colors[BLACK]).rotate_left(17) ^ 0x9E3779B97F4A7C15)) as usize % CORR_SIZE
}

#[inline(always)]
fn gravity(v: &mut i32, bonus: i32) {
    *v += bonus - *v * bonus.abs() / HIST_MAX;
}

fn score_to_tt(s: i32, ply: usize) -> i32 {
    if s >= MATE_BOUND {
        s + ply as i32
    } else if s <= -MATE_BOUND {
        s - ply as i32
    } else {
        s
    }
}

fn score_from_tt(s: i32, ply: usize) -> i32 {
    if s >= MATE_BOUND {
        s - ply as i32
    } else if s <= -MATE_BOUND {
        s + ply as i32
    } else {
        s
    }
}

pub fn format_score(s: i32) -> String {
    if s >= MATE_BOUND {
        format!("mate {}", (MATE - s + 1) / 2)
    } else if s <= -MATE_BOUND {
        format!("mate {}", -(MATE + s) / 2)
    } else {
        format!("cp {s}")
    }
}

impl Searcher {
    pub fn new(tt: Arc<TT>, stop: Arc<AtomicBool>) -> Searcher {
        let mut lmr = [[0; 64]; 64];
        for (d, row) in lmr.iter_mut().enumerate().skip(1) {
            for (m, r) in row.iter_mut().enumerate().skip(1) {
                *r = (0.77 + (d as f64).ln() * (m as f64).ln() / 2.36) as i32;
            }
        }
        Searcher {
            tt,
            stop,
            nodes: 0,
            seldepth: 0,
            hashes: Vec::with_capacity(1024),
            butterfly: Box::new([[[0; 64]; 64]; 2]),
            capt_hist: Box::new([[[0; 6]; 64]; 12]),
            cont1: vec![0; 768 * 768],
            cont2: vec![0; 768 * 768],
            killers: [[Move::NULL; 2]; MAX_PLY + 2],
            stack: [PlyInfo::default(); MAX_PLY + 4],
            pv: Box::new([[Move::NULL; MAX_PLY + 1]; MAX_PLY + 1]),
            pv_len: [0; MAX_PLY + 1],
            lmr,
            corr: vec![[0i32; CORR_SIZE]; 2].into_boxed_slice().try_into().unwrap(),
            acc: vec![[Accumulator { vals: [0; nnue::HIDDEN] }; 2]; MAX_PLY + 8],
            root_nodes: Box::new([[0; 64]; 64]),
            clock_tm: false,
            start: Instant::now(),
            soft_ms: u64::MAX,
            hard_ms: u64::MAX,
            node_limit: u64::MAX,
            stopped: false,
            print: true,
            overhead_ms: 10,
        }
    }

    pub fn clear(&mut self) {
        *self.butterfly = [[[0; 64]; 64]; 2];
        *self.capt_hist = [[[0; 6]; 64]; 12];
        self.cont1.fill(0);
        self.cont2.fill(0);
        self.killers = [[Move::NULL; 2]; MAX_PLY + 2];
        for side in self.corr.iter_mut() {
            side.fill(0);
        }
    }

    /// The static eval adjusted by the correction history.
    #[inline(always)]
    fn corrected(&self, b: &Board, raw: i32) -> i32 {
        (raw + self.corr[b.stm][pawn_key(b)] / CORR_GRAIN).clamp(-MATE_BOUND + 1, MATE_BOUND - 1)
    }

    /// Moves the correction for this pawn structure towards (search score - raw static eval).
    fn update_corr(&mut self, b: &Board, raw: i32, score: i32, depth: i32) {
        let e = &mut self.corr[b.stm][pawn_key(b)];
        let diff = (score - raw) * CORR_GRAIN;
        let w = (depth + 1).min(16);
        *e = ((*e * (CORR_WEIGHT_SCALE - w) + diff * w) / CORR_WEIGHT_SCALE).clamp(-CORR_MAX, CORR_MAX);
    }

    fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn setup_time(&mut self, limits: &Limits, stm: usize) {
        self.soft_ms = u64::MAX;
        self.hard_ms = u64::MAX;
        self.node_limit = limits.nodes.unwrap_or(u64::MAX);
        self.clock_tm = false;
        if let Some(mt) = limits.movetime {
            let t = mt.saturating_sub(self.overhead_ms).max(1);
            self.soft_ms = t;
            self.hard_ms = t;
        } else if let Some(time) = limits.time[stm] {
            let time = time.saturating_sub(self.overhead_ms).max(1);
            let inc = limits.inc[stm];
            let mtg = limits.movestogo.unwrap_or(25).clamp(1, 25);
            let base = (time / mtg + inc * 3 / 4).min(time);
            self.soft_ms = (base * 6 / 10).max(1);
            self.hard_ms = (base * 3).min(time * 6 / 10).max(1);
            self.clock_tm = true;
        }
    }

    fn check_time(&mut self) {
        if self.stop.load(Ordering::Relaxed)
            || self.nodes >= self.node_limit
            || (self.hard_ms != u64::MAX && self.elapsed_ms() >= self.hard_ms)
        {
            self.stopped = true;
        }
    }

    fn is_repetition(&self, b: &Board) -> bool {
        let n = self.hashes.len();
        let limit = (b.halfmove as usize).min(n - 1);
        let mut i = 2;
        while i <= limit {
            if self.hashes[n - 1 - i] == b.hash {
                return true;
            }
            i += 2;
        }
        false
    }

    fn eval(&self, b: &Board, ply: usize) -> i32 {
        let e = if nnue::active() { nnue::evaluate(&self.acc[ply], b.stm, b.occ().count_ones()) } else { evaluate(b) };
        // Fade the eval towards a draw as the 50-move rule approaches.
        let e = e * (200 - b.halfmove.min(100) as i32) / 200;
        e.clamp(-MATE_BOUND + 1, MATE_BOUND - 1)
    }

    /// Fills the accumulators for ply + 1 after `mv` is played from `b` at `ply`.
    #[inline(always)]
    fn nnue_update(&mut self, b: &Board, mv: Move, ply: usize) {
        if nnue::active() {
            let (parent, child) = self.acc.split_at_mut(ply + 1);
            nnue::update(b, mv, &parent[ply], &mut child[0]);
        }
    }

    #[inline(always)]
    fn prev_cont(&self, ply: usize, back: usize) -> usize {
        if ply >= back {
            self.stack[ply - back].cont
        } else {
            NO_CONT
        }
    }

    fn quiet_history(&self, b: &Board, mv: Move, ply: usize) -> i32 {
        let idx = b.mailbox[mv.from()] as usize * 64 + mv.to();
        let mut s = self.butterfly[b.stm][mv.from()][mv.to()];
        let c1 = self.prev_cont(ply, 1);
        if c1 != NO_CONT {
            s += self.cont1[c1 * 768 + idx];
        }
        let c2 = self.prev_cont(ply, 2);
        if c2 != NO_CONT {
            s += self.cont2[c2 * 768 + idx];
        }
        s
    }

    fn update_quiet(&mut self, b: &Board, mv: Move, ply: usize, bonus: i32) {
        let idx = b.mailbox[mv.from()] as usize * 64 + mv.to();
        gravity(&mut self.butterfly[b.stm][mv.from()][mv.to()], bonus);
        let c1 = self.prev_cont(ply, 1);
        if c1 != NO_CONT {
            gravity(&mut self.cont1[c1 * 768 + idx], bonus);
        }
        let c2 = self.prev_cont(ply, 2);
        if c2 != NO_CONT {
            gravity(&mut self.cont2[c2 * 768 + idx], bonus);
        }
    }

    fn captured_type(b: &Board, mv: Move) -> usize {
        if mv.is_ep() || !mv.is_capture() {
            PAWN
        } else {
            b.mailbox[mv.to()] as usize % 6
        }
    }

    fn capt_entry(&mut self, b: &Board, mv: Move) -> &mut i32 {
        let victim = Self::captured_type(b, mv);
        &mut self.capt_hist[b.mailbox[mv.from()] as usize][mv.to()][victim]
    }

    fn score_moves(&self, b: &Board, list: &MoveList, scores: &mut [i32; 256], tt_move: Move, ply: usize) {
        for i in 0..list.len {
            let mv = list.moves[i];
            scores[i] = if mv == tt_move {
                10_000_000
            } else if mv.is_capture() || mv.is_promo() {
                if mv.is_promo() && mv.promo_piece() != QUEEN {
                    -5_000_000
                } else {
                    let mut value = if mv.is_capture() { SEE_VAL[Self::captured_type(b, mv)] } else { 0 };
                    if mv.is_promo() {
                        value += 800;
                    }
                    let hist = if mv.is_capture() {
                        self.capt_hist[b.mailbox[mv.from()] as usize][mv.to()][Self::captured_type(b, mv)]
                    } else {
                        0
                    };
                    let base = if b.see(mv, 0) { 4_000_000 } else { -4_000_000 };
                    base + value * 16 + hist
                }
            } else if mv == self.killers[ply][0] {
                2_000_001
            } else if mv == self.killers[ply][1] {
                2_000_000
            } else {
                self.quiet_history(b, mv, ply)
            };
        }
    }

    /// Selection sort step: brings the best remaining move to index i.
    #[inline(always)]
    fn pick(list: &mut MoveList, scores: &mut [i32; 256], i: usize) -> Move {
        let mut best = i;
        for j in i + 1..list.len {
            if scores[j] > scores[best] {
                best = j;
            }
        }
        list.moves.swap(i, best);
        scores.swap(i, best);
        list.moves[i]
    }

    pub fn search(&mut self, board: &Board, limits: &Limits) -> (Move, i32) {
        self.start = Instant::now();
        self.nodes = 0;
        self.stopped = false;
        self.seldepth = 0;
        self.setup_time(limits, board.stm);
        self.tt.new_search();
        self.killers = [[Move::NULL; 2]; MAX_PLY + 2];
        self.stack = [PlyInfo::default(); MAX_PLY + 4];
        *self.root_nodes = [[0; 64]; 64];
        if self.hashes.last() != Some(&board.hash) {
            self.hashes.push(board.hash);
        }
        if nnue::active() {
            nnue::refresh(board, &mut self.acc[0]);
        }

        let legal = board.legal_moves();
        let mut best_move = match legal.first() {
            Some(&m) => m,
            None => return (Move::NULL, if board.in_check() { -MATE } else { 0 }),
        };
        let mut best_score = 0;
        let max_depth = limits.depth.unwrap_or(MAX_PLY as i32 - 4).clamp(1, MAX_PLY as i32 - 4);
        let mut score = 0;
        // Iterations in a row the best move has stayed the same.
        let mut stable = 0usize;
        let mut prev_best = Move::NULL;

        for depth in 1..=max_depth {
            let mut delta = 20;
            let (mut alpha, mut beta) = if depth >= 4 { (score - delta, score + delta) } else { (-INF, INF) };
            loop {
                let s = self.negamax(board, depth, alpha, beta, 0, false);
                if self.stopped {
                    break;
                }
                if s <= alpha {
                    beta = (alpha + beta) / 2;
                    alpha = (s - delta).max(-INF);
                } else if s >= beta {
                    beta = (s + delta).min(INF);
                } else {
                    score = s;
                    break;
                }
                delta += delta / 2;
            }
            if self.pv_len[0] > 0 && !self.pv[0][0].is_null() {
                best_move = self.pv[0][0];
            }
            if self.stopped {
                break;
            }
            best_score = score;
            if self.print {
                let ms = self.elapsed_ms();
                let pv: Vec<String> = self.pv[0][..self.pv_len[0]].iter().map(|m| m.to_uci()).collect();
                println!(
                    "info depth {} seldepth {} score {} nodes {} nps {} hashfull {} time {} pv {}",
                    depth,
                    self.seldepth,
                    format_score(score),
                    self.nodes,
                    self.nodes * 1000 / ms.max(1),
                    self.tt.hashfull(),
                    ms,
                    pv.join(" ")
                );
            }
            // On a clock, spend less time when the best move is stable and soaks up most of the
            // nodes, more when the search keeps changing its mind.
            stable = if best_move == prev_best { (stable + 1).min(6) } else { 0 };
            prev_best = best_move;
            let mut soft = self.soft_ms;
            if self.clock_tm && depth >= 6 && self.nodes > 0 {
                let frac = self.root_nodes[best_move.from()][best_move.to()] as f64 / self.nodes as f64;
                let stab = [1.5, 1.25, 1.1, 1.0, 0.95, 0.9, 0.85][stable];
                soft = ((soft as f64 * (1.5 - frac) * 1.35 * stab) as u64).min(self.hard_ms);
            }
            if self.elapsed_ms() >= soft || limits.soft_nodes.is_some_and(|n| self.nodes >= n) {
                break;
            }
        }

        if limits.infinite {
            while !self.stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        (best_move, best_score)
    }

    fn negamax(&mut self, b: &Board, mut depth: i32, mut alpha: i32, mut beta: i32, ply: usize, cut_node: bool) -> i32 {
        let pv_node = beta - alpha > 1;
        let root = ply == 0;
        self.pv_len[ply] = ply;

        if depth <= 0 {
            return self.qsearch(b, alpha, beta, ply);
        }
        if self.nodes & 2047 == 0 {
            self.check_time();
        }
        if self.stopped {
            return 0;
        }
        let in_check = b.in_check();
        if !root {
            if b.halfmove >= 100 || b.is_insufficient_material() || self.is_repetition(b) {
                return 0;
            }
            if ply >= MAX_PLY - 1 {
                return if in_check { 0 } else { self.eval(b, ply) };
            }
            alpha = alpha.max(-MATE + ply as i32);
            beta = beta.min(MATE - ply as i32 - 1);
            if alpha >= beta {
                return alpha;
            }
        }
        self.nodes += 1;
        self.seldepth = self.seldepth.max(ply);

        let excluded = self.stack[ply].excluded;
        let tt_entry = if excluded.is_null() { self.tt.probe(b.hash) } else { None };
        let tt_move = tt_entry.map_or(Move::NULL, |e| e.mv);
        if let Some(e) = tt_entry {
            let s = score_from_tt(e.score, ply);
            if !pv_node
                && e.depth >= depth
                && (e.bound == BOUND_EXACT
                    || (e.bound == BOUND_LOWER && s >= beta)
                    || (e.bound == BOUND_UPPER && s <= alpha))
            {
                return s;
            }
        }

        // raw_eval: the network's evaluation (what the TT stores); static_eval: raw_eval adjusted
        // by the correction history (what pruning decisions use).
        let raw_eval;
        let static_eval;
        let mut eval;
        if in_check {
            raw_eval = NO_EVAL;
            static_eval = NO_EVAL;
            eval = NO_EVAL;
        } else if !excluded.is_null() {
            static_eval = self.stack[ply].static_eval;
            raw_eval = static_eval;
            eval = static_eval;
        } else {
            raw_eval = match tt_entry {
                Some(e) if e.eval > -MATE_BOUND => e.eval,
                _ => self.eval(b, ply),
            };
            static_eval = self.corrected(b, raw_eval);
            eval = static_eval;
            // A TT score is a better estimate than the static eval when its bound allows it.
            if let Some(e) = tt_entry {
                let s = score_from_tt(e.score, ply);
                if s.abs() < MATE_BOUND
                    && (e.bound == BOUND_EXACT
                        || (e.bound == BOUND_LOWER && s > eval)
                        || (e.bound == BOUND_UPPER && s < eval))
                {
                    eval = s;
                }
            }
        }
        self.stack[ply].static_eval = static_eval;
        let improving = !in_check
            && ply >= 2
            && self.stack[ply - 2].static_eval != NO_EVAL
            && static_eval > self.stack[ply - 2].static_eval;
        self.killers[ply + 1] = [Move::NULL; 2];

        if !pv_node && !in_check && excluded.is_null() {
            // Reverse futility pruning: we're so far above beta that a shallow search won't change it.
            if depth <= 8 && eval - 75 * (depth - improving as i32) >= beta && eval < MATE_BOUND {
                return eval;
            }
            // Razoring: hopelessly below alpha at low depth, just check captures.
            if depth <= 3 && eval + 250 * depth < alpha {
                let s = self.qsearch(b, alpha, beta, ply);
                if s < beta {
                    return s;
                }
            }
            // Null move pruning: give the opponent a free move; if we still beat beta, cut.
            if depth >= 3
                && eval >= beta
                && static_eval >= beta
                && ply >= 1
                && self.stack[ply - 1].cont != NO_CONT
                && b.has_non_pawn_material(b.stm)
            {
                let r = 3 + depth / 3 + ((eval - beta) / 200).min(3);
                let mut nb = *b;
                nb.make_null();
                self.acc[ply + 1] = self.acc[ply];
                self.stack[ply].cont = NO_CONT;
                self.hashes.push(nb.hash);
                let s = -self.negamax(&nb, depth - r, -beta, -beta + 1, ply + 1, !cut_node);
                self.hashes.pop();
                if self.stopped {
                    return 0;
                }
                if s >= beta {
                    return if s >= MATE_BOUND { beta } else { s };
                }
            }
            // ProbCut: a good capture that beats beta by a margin in a reduced search almost
            // certainly beats beta in a full one.
            let pc_beta = beta + 200;
            if depth >= 5
                && beta.abs() < MATE_BOUND
                && !tt_entry.is_some_and(|e| e.depth >= depth - 3 && score_from_tt(e.score, ply) < pc_beta)
            {
                let mut list = MoveList::new();
                b.gen_moves(&mut list, true);
                let mut scores = [0i32; 256];
                self.score_moves(b, &list, &mut scores, tt_move, ply);
                for i in 0..list.len {
                    let mv = Self::pick(&mut list, &mut scores, i);
                    if !b.is_legal(mv) || !b.see(mv, pc_beta - static_eval) {
                        continue;
                    }
                    let mut nb = *b;
                    nb.make_move(mv);
                    self.tt.prefetch(nb.hash);
                    self.nnue_update(b, mv, ply);
                    self.stack[ply].cont = b.mailbox[mv.from()] as usize * 64 + mv.to();
                    self.hashes.push(nb.hash);
                    let mut s = -self.qsearch(&nb, -pc_beta, -pc_beta + 1, ply + 1);
                    if s >= pc_beta {
                        s = -self.negamax(&nb, depth - 4, -pc_beta, -pc_beta + 1, ply + 1, !cut_node);
                    }
                    self.hashes.pop();
                    if self.stopped {
                        return 0;
                    }
                    if s >= pc_beta && s.abs() < MATE_BOUND {
                        self.tt.store(b.hash, mv, score_to_tt(s, ply), raw_eval, depth - 3, BOUND_LOWER);
                        return s;
                    }
                }
            }
        }

        // Internal iterative reduction: no TT move means this node was never searched well.
        if depth >= 4 && tt_move.is_null() && (pv_node || cut_node) && excluded.is_null() {
            depth -= 1;
        }

        let mut list = MoveList::new();
        b.gen_moves(&mut list, false);
        let mut scores = [0i32; 256];
        self.score_moves(b, &list, &mut scores, tt_move, ply);

        let mut best = -INF;
        let mut best_move = Move::NULL;
        let mut legal = 0;
        let mut quiets = [Move::NULL; 64];
        let mut n_quiets = 0;
        let mut captures = [Move::NULL; 32];
        let mut n_captures = 0;
        let mut skip_quiets = false;

        for i in 0..list.len {
            let mv = Self::pick(&mut list, &mut scores, i);
            if mv == excluded || !b.is_legal(mv) {
                continue;
            }
            let is_quiet = mv.is_quiet();
            if is_quiet && skip_quiets {
                continue;
            }

            if !root && best > -MATE_BOUND {
                if is_quiet {
                    let lmp_limit = (4 + depth * depth) / (2 - improving as i32);
                    if depth <= 8 && legal >= lmp_limit {
                        skip_quiets = true;
                        continue;
                    }
                    if !in_check && depth <= 8 && eval + 120 + 80 * depth <= alpha {
                        skip_quiets = true;
                        continue;
                    }
                    // History pruning: quiet moves that have kept failing elsewhere.
                    if !in_check && depth <= 4 && scores[i] < -2048 * depth {
                        continue;
                    }
                    if depth <= 8 && !b.see(mv, -50 * depth) {
                        continue;
                    }
                } else if depth <= 6 && !b.see(mv, -30 * depth * depth) {
                    continue;
                }
            }

            // Singular extension: if the TT move is much better than every alternative, extend it.
            let mut ext = 0;
            if !root && depth >= 8 && mv == tt_move && excluded.is_null() {
                let e = tt_entry.unwrap();
                let tt_score = score_from_tt(e.score, ply);
                if e.depth >= depth - 3 && e.bound != BOUND_UPPER && tt_score.abs() < MATE_BOUND {
                    let s_beta = tt_score - depth * 2;
                    self.stack[ply].excluded = mv;
                    let s = self.negamax(b, (depth - 1) / 2, s_beta - 1, s_beta, ply, cut_node);
                    self.stack[ply].excluded = Move::NULL;
                    if self.stopped {
                        return 0;
                    }
                    if s < s_beta {
                        ext = if !pv_node && s < s_beta - 20 { 2 } else { 1 };
                    } else if s_beta >= beta {
                        return s_beta; // multi-cut
                    } else if tt_score >= beta {
                        ext = -1;
                    }
                }
            }

            let mut nb = *b;
            nb.make_move(mv);
            self.tt.prefetch(nb.hash);
            self.nnue_update(b, mv, ply);
            if ext == 0 && nb.in_check() {
                ext = 1;
            }
            self.stack[ply].cont = b.mailbox[mv.from()] as usize * 64 + mv.to();
            self.hashes.push(nb.hash);
            legal += 1;
            let new_depth = depth - 1 + ext;
            let nodes_before = self.nodes;

            let score = if legal == 1 {
                -self.negamax(&nb, new_depth, -beta, -alpha, ply + 1, false)
            } else {
                let mut r = 0;
                if depth >= 3 && legal > 1 + root as i32 + pv_node as i32 && (is_quiet || scores[i] < 0) {
                    r = self.lmr[depth.min(63) as usize][legal.min(63) as usize];
                    r += !pv_node as i32;
                    r += cut_node as i32;
                    r += !improving as i32;
                    r -= nb.in_check() as i32;
                    if is_quiet {
                        if scores[i] >= 2_000_000 {
                            r -= 1; // killer
                        } else {
                            r -= (scores[i] / 8192).clamp(-2, 2);
                        }
                    }
                    r = r.clamp(0, (new_depth - 1).max(0));
                }
                let mut s = -self.negamax(&nb, new_depth - r, -alpha - 1, -alpha, ply + 1, true);
                if s > alpha && r > 0 {
                    s = -self.negamax(&nb, new_depth, -alpha - 1, -alpha, ply + 1, !cut_node);
                }
                if s > alpha && s < beta {
                    s = -self.negamax(&nb, new_depth, -beta, -alpha, ply + 1, false);
                }
                s
            };
            self.hashes.pop();
            if self.stopped {
                return 0;
            }
            if root {
                self.root_nodes[mv.from()][mv.to()] += self.nodes - nodes_before;
            }

            if score > best {
                best = score;
                if score > alpha {
                    best_move = mv;
                    alpha = score;
                    self.pv[ply][ply] = mv;
                    let child_len = self.pv_len[ply + 1].max(ply + 1);
                    for j in ply + 1..child_len {
                        self.pv[ply][j] = self.pv[ply + 1][j];
                    }
                    self.pv_len[ply] = child_len;
                    if score >= beta {
                        let bonus = (150 * depth - 50).min(1500);
                        if is_quiet {
                            if self.killers[ply][0] != mv {
                                self.killers[ply][1] = self.killers[ply][0];
                                self.killers[ply][0] = mv;
                            }
                            self.update_quiet(b, mv, ply, bonus);
                            for &q in &quiets[..n_quiets] {
                                self.update_quiet(b, q, ply, -bonus);
                            }
                        } else if mv.is_capture() {
                            gravity(self.capt_entry(b, mv), bonus);
                        }
                        for &c in &captures[..n_captures] {
                            gravity(self.capt_entry(b, c), -bonus);
                        }
                        break;
                    }
                }
            }
            if is_quiet {
                if n_quiets < 64 {
                    quiets[n_quiets] = mv;
                    n_quiets += 1;
                }
            } else if mv.is_capture() && n_captures < 32 {
                captures[n_captures] = mv;
                n_captures += 1;
            }
        }

        if legal == 0 {
            return if !excluded.is_null() {
                alpha
            } else if in_check {
                -MATE + ply as i32
            } else {
                0
            };
        }

        if excluded.is_null() {
            let bound = if best >= beta {
                BOUND_LOWER
            } else if !best_move.is_null() {
                BOUND_EXACT
            } else {
                BOUND_UPPER
            };
            // Learn how far off the static eval was, from quiet positions where the result is
            // trustworthy in that direction (a fail-high only says "at least", a fail-low "at most").
            if !in_check
                && (best_move.is_null() || !best_move.is_capture())
                && best.abs() < MATE_BOUND
                && !(bound == BOUND_LOWER && best <= static_eval)
                && !(bound == BOUND_UPPER && best >= static_eval)
            {
                self.update_corr(b, raw_eval, best, depth);
            }
            self.tt.store(b.hash, best_move, score_to_tt(best, ply), raw_eval, depth, bound);
        }
        best
    }

    fn qsearch(&mut self, b: &Board, mut alpha: i32, beta: i32, ply: usize) -> i32 {
        self.pv_len[ply] = ply;
        if self.nodes & 2047 == 0 {
            self.check_time();
        }
        if self.stopped {
            return 0;
        }
        self.nodes += 1;
        self.seldepth = self.seldepth.max(ply);
        let in_check = b.in_check();
        if ply >= MAX_PLY - 1 {
            return if in_check { 0 } else { self.eval(b, ply) };
        }
        let pv_node = beta - alpha > 1;

        let tt_entry = self.tt.probe(b.hash);
        if let Some(e) = tt_entry {
            let s = score_from_tt(e.score, ply);
            if !pv_node
                && (e.bound == BOUND_EXACT
                    || (e.bound == BOUND_LOWER && s >= beta)
                    || (e.bound == BOUND_UPPER && s <= alpha))
            {
                return s;
            }
        }
        let tt_move = tt_entry.map_or(Move::NULL, |e| e.mv);

        let mut raw_eval = NO_EVAL;
        let mut best;
        if in_check {
            best = -INF;
        } else {
            raw_eval = match tt_entry {
                Some(e) if e.eval > -MATE_BOUND => e.eval,
                _ => self.eval(b, ply),
            };
            best = self.corrected(b, raw_eval);
            if best >= beta {
                return best;
            }
            alpha = alpha.max(best);
        }

        let mut list = MoveList::new();
        b.gen_moves(&mut list, !in_check);
        let mut scores = [0i32; 256];
        self.score_moves(b, &list, &mut scores, tt_move, ply);

        let mut best_move = Move::NULL;
        let mut legal = 0;
        let futility = if in_check { NO_EVAL } else { best + 150 };
        for i in 0..list.len {
            let mv = Self::pick(&mut list, &mut scores, i);
            if !b.is_legal(mv) {
                continue;
            }
            if !in_check {
                if !b.see(mv, 0) {
                    continue;
                }
                if futility <= alpha && !b.see(mv, 1) {
                    best = best.max(futility);
                    continue;
                }
            }
            let mut nb = *b;
            nb.make_move(mv);
            self.tt.prefetch(nb.hash);
            self.nnue_update(b, mv, ply);
            legal += 1;
            self.hashes.push(nb.hash);
            let score = -self.qsearch(&nb, -beta, -alpha, ply + 1);
            self.hashes.pop();
            if self.stopped {
                return 0;
            }
            if score > best {
                best = score;
                if score > alpha {
                    alpha = score;
                    best_move = mv;
                    if score >= beta {
                        break;
                    }
                }
            }
        }
        if in_check && legal == 0 {
            return -MATE + ply as i32;
        }
        let bound = if best >= beta { BOUND_LOWER } else { BOUND_UPPER };
        self.tt.store(b.hash, best_move, score_to_tt(best, ply), raw_eval, 0, bound);
        best
    }
}
