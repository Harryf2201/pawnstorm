//! Syzygy endgame tablebases (UCI option SyzygyPath), probed through shakmaty-syzygy.
//! The search asks for win/draw/loss after captures and pawn moves (where the 50-move counter is
//! zero, which the WDL tables assume); at the root, the DTZ tables pick a move that wins (or holds)
//! within the 50-move rule.

use crate::board::Board;
use crate::types::Move;
use shakmaty::{fen::Fen, CastlingMode, Chess};
use shakmaty_syzygy::{Tablebase, Wdl};
use std::sync::RwLock;

static TB: RwLock<Option<Tablebase<Chess>>> = RwLock::new(None);
static mut MAX_PIECES: u32 = 0;

/// Loads every table in `paths` (separated by ';'); an empty path unloads them. Returns how many
/// tables were found and the largest piece count covered.
pub fn init(paths: &str) -> (usize, u32) {
    let mut tables = Tablebase::new();
    let mut n = 0;
    for p in paths.split(';').map(str::trim).filter(|p| !p.is_empty() && *p != "<empty>") {
        n += tables.add_directory(p).unwrap_or(0);
    }
    let max = if n > 0 { tables.max_pieces() as u32 } else { 0 };
    *TB.write().unwrap() = if n > 0 { Some(tables) } else { None };
    unsafe { MAX_PIECES = max };
    (n, max)
}

/// Largest number of pieces (kings included) the loaded tables cover; 0 when none are loaded.
#[inline(always)]
pub fn max_pieces() -> u32 {
    unsafe { MAX_PIECES }
}

fn to_shakmaty(b: &Board) -> Option<Chess> {
    b.to_fen().parse::<Fen>().ok()?.into_position(CastlingMode::Standard).ok()
}

/// Win (1), draw (0) or loss (-1) for the side to move, for a position just after a capture or
/// pawn move. Wins and losses that the 50-move rule would turn into draws count as draws.
pub fn probe_wdl(b: &Board) -> Option<i32> {
    if b.occ().count_ones() > max_pieces() || b.castling != 0 {
        return None;
    }
    let guard = TB.read().ok()?;
    let tables = guard.as_ref()?;
    let wdl = tables.probe_wdl_after_zeroing(&to_shakmaty(b)?).ok()?;
    Some(match wdl {
        Wdl::Win => 1,
        Wdl::Loss => -1,
        _ => 0,
    })
}

/// At the root: the tablebase's best move (shortest safe path to a win, or the most stubborn
/// defence) and its outcome for the side to move (1, 0, -1).
pub fn root_move(b: &Board) -> Option<(Move, i32)> {
    if b.occ().count_ones() > max_pieces() || b.castling != 0 {
        return None;
    }
    let guard = TB.read().ok()?;
    let tables = guard.as_ref()?;
    let pos = to_shakmaty(b)?;
    let (mv, _) = tables.best_move(&pos).ok()??;
    let uci = mv.to_uci(CastlingMode::Standard).to_string();
    let ours = b.legal_moves().into_iter().find(|m| m.to_uci() == uci)?;
    // The outcome from DTZ's point of view, respecting the 50-move rule from the current counter.
    let wdl = tables.probe_wdl(&pos).ok()?;
    use shakmaty_syzygy::AmbiguousWdl as A;
    let outcome = match wdl {
        A::Win | A::MaybeWin => 1,
        A::Loss | A::MaybeLoss => -1,
        _ => 0,
    };
    Some((ours, outcome))
}
