//! Board representation (bitboards + mailbox), copy-make move execution, move generation,
//! legality testing and static exchange evaluation.

use crate::attacks::*;
use crate::types::*;

pub const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

pub struct Zobrist {
    pub pieces: [[u64; 64]; 12],
    pub castling: [u64; 16],
    pub ep: [u64; 8],
    pub side: u64,
}

const fn splitmix(s: &mut u64) -> u64 {
    *s = s.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

const fn gen_zobrist() -> Zobrist {
    let mut s = 0x5EED_CAFE_F00D_u64;
    let mut z = Zobrist { pieces: [[0; 64]; 12], castling: [0; 16], ep: [0; 8], side: 0 };
    let mut p = 0;
    while p < 12 {
        let mut sq = 0;
        while sq < 64 {
            z.pieces[p][sq] = splitmix(&mut s);
            sq += 1;
        }
        p += 1;
    }
    let mut i = 0;
    while i < 16 {
        z.castling[i] = splitmix(&mut s);
        i += 1;
    }
    i = 0;
    while i < 8 {
        z.ep[i] = splitmix(&mut s);
        i += 1;
    }
    z.side = splitmix(&mut s);
    z
}

pub static Z: Zobrist = gen_zobrist();

// Castling rights bits: 1 = white O-O, 2 = white O-O-O, 4 = black O-O, 8 = black O-O-O.
const CASTLE_MASK: [u8; 64] = {
    let mut m = [15u8; 64];
    m[0] = 15 & !2;
    m[7] = 15 & !1;
    m[4] = 15 & !3;
    m[56] = 15 & !8;
    m[63] = 15 & !4;
    m[60] = 15 & !12;
    m
};

const FILE_A: u64 = 0x0101010101010101;
const FILE_H: u64 = FILE_A << 7;
const RANK_1: u64 = 0xFF;
const RANK_3: u64 = RANK_1 << 16;
const RANK_6: u64 = RANK_1 << 40;
const RANK_8: u64 = RANK_1 << 56;

pub const SEE_VAL: [i32; 6] = [100, 300, 300, 500, 900, 0];

#[derive(Clone, Copy)]
pub struct Board {
    pub pieces: [u64; 6],
    pub colors: [u64; 2],
    pub mailbox: [u8; 64],
    pub stm: usize,
    pub castling: u8,
    pub ep: u8,
    pub halfmove: u16,
    pub fullmove: u16,
    pub hash: u64,
    /// Enemy pieces giving check to the side to move.
    pub checkers: u64,
    /// Pieces of the side to move that are pinned to their king.
    pub pinned: u64,
}

impl Board {
    pub fn startpos() -> Board {
        Board::from_fen(START_FEN).unwrap()
    }

    fn empty() -> Board {
        Board {
            pieces: [0; 6],
            colors: [0; 2],
            mailbox: [NO_PIECE; 64],
            stm: WHITE,
            castling: 0,
            ep: NO_SQ,
            halfmove: 0,
            fullmove: 1,
            hash: 0,
            checkers: 0,
            pinned: 0,
        }
    }

    #[inline(always)]
    pub fn occ(&self) -> u64 {
        self.colors[0] | self.colors[1]
    }

    #[inline(always)]
    pub fn bb(&self, color: usize, pt: usize) -> u64 {
        self.pieces[pt] & self.colors[color]
    }

    #[inline(always)]
    pub fn king_sq(&self, color: usize) -> usize {
        lsb(self.bb(color, KING))
    }

    #[inline(always)]
    fn add_piece(&mut self, sq: usize, pc: u8) {
        let b = bit(sq);
        self.pieces[pc as usize % 6] |= b;
        self.colors[pc as usize / 6] |= b;
        self.mailbox[sq] = pc;
        self.hash ^= Z.pieces[pc as usize][sq];
    }

    #[inline(always)]
    fn remove_piece(&mut self, sq: usize) {
        let pc = self.mailbox[sq];
        let b = bit(sq);
        self.pieces[pc as usize % 6] ^= b;
        self.colors[pc as usize / 6] ^= b;
        self.mailbox[sq] = NO_PIECE;
        self.hash ^= Z.pieces[pc as usize][sq];
    }

    #[inline(always)]
    fn move_piece(&mut self, from: usize, to: usize) {
        let pc = self.mailbox[from];
        let b = bit(from) | bit(to);
        self.pieces[pc as usize % 6] ^= b;
        self.colors[pc as usize / 6] ^= b;
        self.mailbox[from] = NO_PIECE;
        self.mailbox[to] = pc;
        self.hash ^= Z.pieces[pc as usize][from] ^ Z.pieces[pc as usize][to];
    }

    pub fn from_fen(fen: &str) -> Result<Board, String> {
        let parts: Vec<&str> = fen.split_whitespace().collect();
        if parts.is_empty() {
            return Err("empty FEN".into());
        }
        let mut b = Board::empty();
        let (mut rank, mut file) = (7i32, 0i32);
        for c in parts[0].chars() {
            match c {
                '/' => {
                    rank -= 1;
                    file = 0;
                }
                '1'..='8' => file += c as i32 - '0' as i32,
                _ => {
                    let pc = PIECE_CHARS.iter().position(|&x| x as char == c).ok_or(format!("bad piece '{c}'"))?;
                    if !(0..8).contains(&rank) || !(0..8).contains(&file) {
                        return Err("bad FEN board".into());
                    }
                    b.add_piece((rank * 8 + file) as usize, pc as u8);
                    file += 1;
                }
            }
        }
        if b.bb(WHITE, KING).count_ones() != 1 || b.bb(BLACK, KING).count_ones() != 1 {
            return Err("each side needs exactly one king".into());
        }
        b.stm = if parts.get(1) == Some(&"b") { BLACK } else { WHITE };
        if let Some(c) = parts.get(2) {
            for ch in c.chars() {
                b.castling |= match ch {
                    'K' => 1,
                    'Q' => 2,
                    'k' => 4,
                    'q' => 8,
                    _ => 0,
                };
            }
        }
        if let Some(ep) = parts.get(3).and_then(|s| parse_sq(s)) {
            // Only keep the en-passant square if a capture is actually possible.
            if PAWN_ATTACKS[b.stm ^ 1][ep] & b.bb(b.stm, PAWN) != 0 {
                b.ep = ep as u8;
            }
        }
        b.halfmove = parts.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
        b.fullmove = parts.get(5).and_then(|s| s.parse().ok()).unwrap_or(1);
        if b.stm == BLACK {
            b.hash ^= Z.side;
        }
        b.hash ^= Z.castling[b.castling as usize];
        if b.ep != NO_SQ {
            b.hash ^= Z.ep[b.ep as usize & 7];
        }
        b.update_checks_pins();
        Ok(b)
    }

    pub fn to_fen(&self) -> String {
        let mut s = String::new();
        for r in (0..8).rev() {
            let mut empty = 0;
            for f in 0..8 {
                let pc = self.mailbox[r * 8 + f];
                if pc == NO_PIECE {
                    empty += 1;
                    continue;
                }
                if empty > 0 {
                    s += &empty.to_string();
                    empty = 0;
                }
                s.push(PIECE_CHARS[pc as usize] as char);
            }
            if empty > 0 {
                s += &empty.to_string();
            }
            if r > 0 {
                s.push('/');
            }
        }
        s += if self.stm == WHITE { " w " } else { " b " };
        let mut c = String::new();
        for (bitv, ch) in [(1, 'K'), (2, 'Q'), (4, 'k'), (8, 'q')] {
            if self.castling & bitv != 0 {
                c.push(ch);
            }
        }
        s += if c.is_empty() { "-" } else { &c };
        s += " ";
        s += &if self.ep == NO_SQ { "-".to_string() } else { sq_name(self.ep as usize) };
        s += &format!(" {} {}", self.halfmove, self.fullmove);
        s
    }

    /// All pieces (both colors) attacking `sq` given occupancy `occ`.
    #[inline]
    pub fn attackers_to(&self, sq: usize, occ: u64) -> u64 {
        (PAWN_ATTACKS[WHITE][sq] & self.bb(BLACK, PAWN))
            | (PAWN_ATTACKS[BLACK][sq] & self.bb(WHITE, PAWN))
            | (KNIGHT_ATTACKS[sq] & self.pieces[KNIGHT])
            | (KING_ATTACKS[sq] & self.pieces[KING])
            | (bishop_attacks(sq, occ) & (self.pieces[BISHOP] | self.pieces[QUEEN]))
            | (rook_attacks(sq, occ) & (self.pieces[ROOK] | self.pieces[QUEEN]))
    }

    /// Every square attacked by `by` (for threat-aware move ordering).
    pub fn threats(&self, by: usize) -> u64 {
        let occ = self.occ();
        let c = self.colors[by];
        let mut t = 0;
        let mut x = self.pieces[PAWN] & c;
        while x != 0 {
            t |= PAWN_ATTACKS[by][x.trailing_zeros() as usize];
            x &= x - 1;
        }
        let mut x = self.pieces[KNIGHT] & c;
        while x != 0 {
            t |= KNIGHT_ATTACKS[x.trailing_zeros() as usize];
            x &= x - 1;
        }
        let mut x = (self.pieces[BISHOP] | self.pieces[QUEEN]) & c;
        while x != 0 {
            t |= bishop_attacks(x.trailing_zeros() as usize, occ);
            x &= x - 1;
        }
        let mut x = (self.pieces[ROOK] | self.pieces[QUEEN]) & c;
        while x != 0 {
            t |= rook_attacks(x.trailing_zeros() as usize, occ);
            x &= x - 1;
        }
        t | KING_ATTACKS[(self.pieces[KING] & c).trailing_zeros() as usize]
    }

    #[inline]
    pub fn is_attacked(&self, sq: usize, by: usize, occ: u64) -> bool {
        let c = self.colors[by];
        (PAWN_ATTACKS[by ^ 1][sq] & self.pieces[PAWN] & c) != 0
            || (KNIGHT_ATTACKS[sq] & self.pieces[KNIGHT] & c) != 0
            || (KING_ATTACKS[sq] & self.pieces[KING] & c) != 0
            || (bishop_attacks(sq, occ) & (self.pieces[BISHOP] | self.pieces[QUEEN]) & c) != 0
            || (rook_attacks(sq, occ) & (self.pieces[ROOK] | self.pieces[QUEEN]) & c) != 0
    }

    #[inline(always)]
    pub fn in_check(&self) -> bool {
        self.checkers != 0
    }

    fn update_checks_pins(&mut self) {
        let us = self.stm;
        let them = us ^ 1;
        let k = self.king_sq(us);
        let occ = self.occ();
        self.checkers = self.attackers_to(k, occ) & self.colors[them];
        self.pinned = 0;
        let mut snipers = ((rook_attacks(k, 0) & (self.pieces[ROOK] | self.pieces[QUEEN]))
            | (bishop_attacks(k, 0) & (self.pieces[BISHOP] | self.pieces[QUEEN])))
            & self.colors[them];
        while snipers != 0 {
            let s = pop_lsb(&mut snipers);
            let b = between(k, s) & occ;
            if b.count_ones() == 1 {
                self.pinned |= b & self.colors[us];
            }
        }
    }

    pub fn has_non_pawn_material(&self, color: usize) -> bool {
        self.colors[color] & !(self.pieces[PAWN] | self.pieces[KING]) != 0
    }

    pub fn is_insufficient_material(&self) -> bool {
        if self.pieces[PAWN] | self.pieces[ROOK] | self.pieces[QUEEN] != 0 {
            return false;
        }
        (self.pieces[KNIGHT] | self.pieces[BISHOP]).count_ones() <= 1
    }

    /// Plays a legal move (check with `is_legal` first).
    pub fn make_move(&mut self, mv: Move) {
        let (from, to, flag) = (mv.from(), mv.to(), mv.flag());
        let us = self.stm;
        let them = us ^ 1;
        let pt = self.mailbox[from] as usize % 6;

        if self.ep != NO_SQ {
            self.hash ^= Z.ep[self.ep as usize & 7];
            self.ep = NO_SQ;
        }
        self.halfmove += 1;

        if flag == Move::EP {
            self.remove_piece(to ^ 8);
            self.halfmove = 0;
        } else if mv.is_capture() {
            self.remove_piece(to);
            self.halfmove = 0;
        }

        self.move_piece(from, to);

        if pt == PAWN {
            self.halfmove = 0;
            if mv.is_promo() {
                self.remove_piece(to);
                self.add_piece(to, (us * 6 + mv.promo_piece()) as u8);
            } else if flag == Move::DOUBLE_PUSH {
                let ep = (from + to) / 2;
                if PAWN_ATTACKS[us][ep] & self.bb(them, PAWN) != 0 {
                    self.ep = ep as u8;
                    self.hash ^= Z.ep[ep & 7];
                }
            }
        } else if flag == Move::KING_CASTLE {
            self.move_piece(to + 1, to - 1);
        } else if flag == Move::QUEEN_CASTLE {
            self.move_piece(to - 2, to + 1);
        }

        self.hash ^= Z.castling[self.castling as usize];
        self.castling &= CASTLE_MASK[from] & CASTLE_MASK[to];
        self.hash ^= Z.castling[self.castling as usize];

        self.stm = them;
        self.hash ^= Z.side;
        if us == BLACK {
            self.fullmove += 1;
        }
        self.update_checks_pins();
    }

    pub fn make_null(&mut self) {
        if self.ep != NO_SQ {
            self.hash ^= Z.ep[self.ep as usize & 7];
            self.ep = NO_SQ;
        }
        self.stm ^= 1;
        self.hash ^= Z.side;
        self.halfmove = 0;
        self.update_checks_pins();
    }

    #[inline(always)]
    fn add_promos(list: &mut MoveList, from: usize, to: usize, capture: bool, queen_only: bool) {
        let base = if capture { Move::PROMO_CAPTURE } else { Move::PROMO };
        list.push(Move::new(from, to, base + 3));
        if !queen_only {
            list.push(Move::new(from, to, base));
            list.push(Move::new(from, to, base + 1));
            list.push(Move::new(from, to, base + 2));
        }
    }

    /// Generates pseudo-legal moves. `noisy_only` = captures and queen promotions (for quiescence).
    pub fn gen_moves(&self, list: &mut MoveList, noisy_only: bool) {
        let us = self.stm;
        let them = us ^ 1;
        let ours = self.colors[us];
        let theirs = self.colors[them];
        let occ = ours | theirs;
        let empty = !occ;
        let targets = if noisy_only { theirs } else { !ours };

        // Pawns
        let pawns = self.bb(us, PAWN);
        let (push1, push2, cap_w, cap_e, up, d_w, d_e, promo_rank);
        if us == WHITE {
            push1 = (pawns << 8) & empty;
            push2 = ((push1 & RANK_3) << 8) & empty;
            cap_w = ((pawns & !FILE_A) << 7) & theirs;
            cap_e = ((pawns & !FILE_H) << 9) & theirs;
            up = 8i32;
            d_w = 7i32;
            d_e = 9i32;
            promo_rank = RANK_8;
        } else {
            push1 = (pawns >> 8) & empty;
            push2 = ((push1 & RANK_6) >> 8) & empty;
            cap_w = ((pawns & !FILE_A) >> 9) & theirs;
            cap_e = ((pawns & !FILE_H) >> 7) & theirs;
            up = -8;
            d_w = -9;
            d_e = -7;
            promo_rank = RANK_1;
        }
        let back = |to: usize, d: i32| (to as i32 - d) as usize;

        if !noisy_only {
            let mut b = push1 & !promo_rank;
            while b != 0 {
                let to = pop_lsb(&mut b);
                list.push(Move::new(back(to, up), to, Move::QUIET));
            }
            let mut b = push2;
            while b != 0 {
                let to = pop_lsb(&mut b);
                list.push(Move::new(back(to, 2 * up), to, Move::DOUBLE_PUSH));
            }
        }
        let mut b = push1 & promo_rank;
        while b != 0 {
            let to = pop_lsb(&mut b);
            Self::add_promos(list, back(to, up), to, false, noisy_only);
        }
        for (caps, d) in [(cap_w, d_w), (cap_e, d_e)] {
            let mut b = caps & !promo_rank;
            while b != 0 {
                let to = pop_lsb(&mut b);
                list.push(Move::new(back(to, d), to, Move::CAPTURE));
            }
            let mut b = caps & promo_rank;
            while b != 0 {
                let to = pop_lsb(&mut b);
                Self::add_promos(list, back(to, d), to, true, noisy_only);
            }
        }
        if self.ep != NO_SQ {
            let ep = self.ep as usize;
            let mut b = PAWN_ATTACKS[them][ep] & pawns;
            while b != 0 {
                list.push(Move::new(pop_lsb(&mut b), ep, Move::EP));
            }
        }

        // Pieces
        for pt in KNIGHT..=KING {
            let mut b = self.bb(us, pt);
            while b != 0 {
                let from = pop_lsb(&mut b);
                let mut att = match pt {
                    KNIGHT => KNIGHT_ATTACKS[from],
                    BISHOP => bishop_attacks(from, occ),
                    ROOK => rook_attacks(from, occ),
                    QUEEN => queen_attacks(from, occ),
                    _ => KING_ATTACKS[from],
                } & targets;
                while att != 0 {
                    let to = pop_lsb(&mut att);
                    let flag = if theirs & bit(to) != 0 { Move::CAPTURE } else { Move::QUIET };
                    list.push(Move::new(from, to, flag));
                }
            }
        }

        // Castling. Destination safety is checked in is_legal.
        if !noisy_only && self.checkers == 0 {
            let (k_right, q_right, k) = if us == WHITE { (1, 2, 4) } else { (4, 8, 60) };
            if self.castling & k_right != 0
                && occ & (0x60u64 << (k - 4)) == 0
                && !self.is_attacked(k + 1, them, occ)
            {
                list.push(Move::new(k, k + 2, Move::KING_CASTLE));
            }
            if self.castling & q_right != 0
                && occ & (0x0Eu64 << (k - 4)) == 0
                && !self.is_attacked(k - 1, them, occ)
            {
                list.push(Move::new(k, k - 2, Move::QUEEN_CASTLE));
            }
        }
    }

    /// Full legality test for a pseudo-legal move.
    pub fn is_legal(&self, mv: Move) -> bool {
        let us = self.stm;
        let them = us ^ 1;
        let (from, to) = (mv.from(), mv.to());
        let k = self.king_sq(us);
        let occ = self.occ();

        if from == k {
            if mv.is_castle() {
                return !self.is_attacked(to, them, occ);
            }
            return !self.is_attacked(to, them, occ ^ bit(from));
        }
        if mv.is_ep() {
            let cap = to ^ 8;
            let new_occ = (occ ^ bit(from) ^ bit(cap)) | bit(to);
            return self.attackers_to(k, new_occ) & self.colors[them] & !bit(cap) == 0;
        }
        if self.checkers != 0 {
            if self.checkers.count_ones() > 1 {
                return false;
            }
            let c = lsb(self.checkers);
            if (between(k, c) | bit(c)) & bit(to) == 0 {
                return false;
            }
        }
        if self.pinned & bit(from) != 0 {
            return line(k, from) & bit(to) != 0;
        }
        true
    }

    /// Static exchange evaluation: does `mv` win at least `threshold` material
    /// once all captures on the target square are played out?
    pub fn see(&self, mv: Move, threshold: i32) -> bool {
        if mv.is_castle() || mv.is_ep() || mv.is_promo() {
            return threshold <= 0;
        }
        let (from, to) = (mv.from(), mv.to());
        let victim = self.mailbox[to];
        let mut swap = if victim == NO_PIECE { 0 } else { SEE_VAL[victim as usize % 6] } - threshold;
        if swap < 0 {
            return false;
        }
        swap = SEE_VAL[self.mailbox[from] as usize % 6] - swap;
        if swap <= 0 {
            return true;
        }
        let mut occ = self.occ() ^ bit(from) ^ bit(to);
        let mut stm = self.stm;
        let mut attackers = self.attackers_to(to, occ);
        let bishops = self.pieces[BISHOP] | self.pieces[QUEEN];
        let rooks = self.pieces[ROOK] | self.pieces[QUEEN];
        let mut res = 1;
        loop {
            stm ^= 1;
            attackers &= occ;
            let my = attackers & self.colors[stm];
            if my == 0 {
                break;
            }
            res ^= 1;
            let mut pt = PAWN;
            while pt < KING && my & self.pieces[pt] == 0 {
                pt += 1;
            }
            if pt == KING {
                return if attackers & self.colors[stm ^ 1] != 0 { res ^ 1 == 1 } else { res == 1 };
            }
            swap = SEE_VAL[pt] - swap;
            if swap < res {
                break;
            }
            occ ^= bit(lsb(my & self.pieces[pt]));
            if pt == PAWN || pt == BISHOP || pt == QUEEN {
                attackers |= bishop_attacks(to, occ) & bishops;
            }
            if pt == ROOK || pt == QUEEN {
                attackers |= rook_attacks(to, occ) & rooks;
            }
        }
        res == 1
    }

    /// Finds the legal move matching a UCI string like "e2e4" or "e7e8q".
    pub fn parse_move(&self, uci: &str) -> Option<Move> {
        let mut list = MoveList::new();
        self.gen_moves(&mut list, false);
        let uci = uci.trim().to_ascii_lowercase();
        let found = list.iter().find(|&m| m.to_uci() == uci && self.is_legal(m));
        found
    }

    /// Standard algebraic notation for a legal move, without the check/mate suffix (e.g. "Nbd7", "exd5", "e8=Q", "O-O").
    pub fn to_san(&self, mv: Move) -> String {
        if mv.is_castle() {
            return if mv.flag() == Move::KING_CASTLE { "O-O".into() } else { "O-O-O".into() };
        }
        let (from, to) = (mv.from(), mv.to());
        let pt = self.mailbox[from] as usize % 6;
        let dest = sq_name(to);
        if pt == PAWN {
            let mut s = if mv.is_capture() { format!("{}x{dest}", (b'a' + (from & 7) as u8) as char) } else { dest };
            if mv.is_promo() {
                s.push('=');
                s.push(b"NBRQ"[mv.promo_piece() - 1] as char);
            }
            return s;
        }
        let mut s = String::from(b"PNBRQK"[pt] as char);
        // Disambiguate between identical pieces that can also reach the destination.
        let rivals: Vec<usize> = self
            .legal_moves()
            .into_iter()
            .filter(|m| m.to() == to && m.from() != from && self.mailbox[m.from()] == self.mailbox[from])
            .map(|m| m.from())
            .collect();
        if !rivals.is_empty() {
            let file = sq_name(from).chars().next().unwrap();
            let rank = sq_name(from).chars().nth(1).unwrap();
            if rivals.iter().all(|&r| r & 7 != from & 7) {
                s.push(file);
            } else if rivals.iter().all(|&r| r >> 3 != from >> 3) {
                s.push(rank);
            } else {
                s.push(file);
                s.push(rank);
            }
        }
        if mv.is_capture() {
            s.push('x');
        }
        s + &dest
    }

    /// Finds the legal move for a SAN string such as "Nf3", "exd5", "O-O+", "e8=Q#".
    pub fn parse_san(&self, san: &str) -> Option<Move> {
        let clean: String = san.trim().trim_end_matches(['+', '#', '!', '?']).replace('0', "O");
        self.legal_moves().into_iter().find(|&m| self.to_san(m) == clean)
    }

    pub fn legal_moves(&self) -> Vec<Move> {
        let mut list = MoveList::new();
        self.gen_moves(&mut list, false);
        list.iter().filter(|&m| self.is_legal(m)).collect()
    }

    pub fn pretty(&self) -> String {
        let mut s = String::new();
        for r in (0..8).rev() {
            s += &format!(" {}  ", r + 1);
            for f in 0..8 {
                let pc = self.mailbox[r * 8 + f];
                s.push(if pc == NO_PIECE { '.' } else { PIECE_CHARS[pc as usize] as char });
                s.push(' ');
            }
            s.push('\n');
        }
        s += "    a b c d e f g h\n\n";
        s += &format!("FEN: {}\nKey: {:016x}\n", self.to_fen(), self.hash);
        s
    }
}

pub fn perft(b: &Board, depth: u32) -> u64 {
    if depth == 0 {
        return 1;
    }
    let mut list = MoveList::new();
    b.gen_moves(&mut list, false);
    let mut n = 0;
    for mv in list.iter() {
        if !b.is_legal(mv) {
            continue;
        }
        if depth == 1 {
            n += 1;
        } else {
            let mut c = *b;
            c.make_move(mv);
            n += perft(&c, depth - 1);
        }
    }
    n
}
