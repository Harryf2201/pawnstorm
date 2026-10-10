use std::fmt;

pub const WHITE: usize = 0;
pub const BLACK: usize = 1;

pub const PAWN: usize = 0;
pub const KNIGHT: usize = 1;
pub const BISHOP: usize = 2;
pub const ROOK: usize = 3;
pub const QUEEN: usize = 4;
pub const KING: usize = 5;

/// Pieces in the mailbox are encoded as color * 6 + type; 12 means empty.
pub const NO_PIECE: u8 = 12;
pub const NO_SQ: u8 = 64;

pub const PIECE_CHARS: &[u8; 12] = b"PNBRQKpnbrqk";

#[inline(always)]
pub fn bit(sq: usize) -> u64 {
    1u64 << sq
}

#[inline(always)]
pub fn lsb(b: u64) -> usize {
    b.trailing_zeros() as usize
}

#[inline(always)]
pub fn pop_lsb(b: &mut u64) -> usize {
    let sq = b.trailing_zeros() as usize;
    *b &= b.wrapping_sub(1);
    sq
}

pub fn sq_name(sq: usize) -> String {
    format!("{}{}", (b'a' + (sq & 7) as u8) as char, (b'1' + (sq >> 3) as u8) as char)
}

pub fn parse_sq(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 2 {
        return None;
    }
    let f = b[0].wrapping_sub(b'a');
    let r = b[1].wrapping_sub(b'1');
    if f < 8 && r < 8 {
        Some((r * 8 + f) as usize)
    } else {
        None
    }
}

/// A move packed into 16 bits: from (6) | to (6) | flags (4). Squares: a1 = 0 ... h8 = 63.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Move(pub u16);

impl Move {
    pub const NULL: Move = Move(0);

    pub const QUIET: u16 = 0;
    pub const DOUBLE_PUSH: u16 = 1;
    pub const KING_CASTLE: u16 = 2;
    pub const QUEEN_CASTLE: u16 = 3;
    pub const CAPTURE: u16 = 4;
    pub const EP: u16 = 5;
    /// Promotion flags: PROMO + (piece - 1), i.e. 8 = knight ... 11 = queen.
    pub const PROMO: u16 = 8;
    pub const PROMO_CAPTURE: u16 = 12;

    #[inline(always)]
    pub const fn new(from: usize, to: usize, flag: u16) -> Move {
        Move(from as u16 | (to as u16) << 6 | flag << 12)
    }
    #[inline(always)]
    pub fn from(self) -> usize {
        (self.0 & 63) as usize
    }
    #[inline(always)]
    pub fn to(self) -> usize {
        ((self.0 >> 6) & 63) as usize
    }
    #[inline(always)]
    pub fn flag(self) -> u16 {
        self.0 >> 12
    }
    #[inline(always)]
    pub fn is_null(self) -> bool {
        self.0 == 0
    }
    #[inline(always)]
    pub fn is_capture(self) -> bool {
        self.0 & (4 << 12) != 0
    }
    #[inline(always)]
    pub fn is_promo(self) -> bool {
        self.0 & (8 << 12) != 0
    }
    #[inline(always)]
    pub fn is_ep(self) -> bool {
        self.flag() == Move::EP
    }
    #[inline(always)]
    pub fn is_castle(self) -> bool {
        let f = self.flag();
        f == Move::KING_CASTLE || f == Move::QUEEN_CASTLE
    }
    /// Neither a capture nor a promotion (castling counts as quiet).
    #[inline(always)]
    pub fn is_quiet(self) -> bool {
        self.0 & (12 << 12) == 0
    }
    #[inline(always)]
    pub fn promo_piece(self) -> usize {
        (((self.0 >> 12) & 3) + 1) as usize
    }

    pub fn to_uci(self) -> String {
        if self.is_null() {
            return "0000".to_string();
        }
        let mut s = sq_name(self.from()) + &sq_name(self.to());
        if self.is_promo() {
            s.push(b"nbrq"[self.promo_piece() - 1] as char);
        }
        s
    }
}

impl fmt::Display for Move {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_uci())
    }
}

pub struct MoveList {
    pub moves: [Move; 256],
    pub len: usize,
}

impl MoveList {
    #[inline(always)]
    pub fn new() -> MoveList {
        MoveList { moves: [Move::NULL; 256], len: 0 }
    }
    #[inline(always)]
    pub fn push(&mut self, mv: Move) {
        self.moves[self.len] = mv;
        self.len += 1;
    }
    pub fn iter(&self) -> impl Iterator<Item = Move> + '_ {
        self.moves[..self.len].iter().copied()
    }
}
