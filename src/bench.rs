//! Correctness (perft) and speed (bench) tests.

use crate::board::*;
use crate::search::*;
use crate::tt::TT;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

pub const DEFAULT_DEPTH: i32 = 10;

const BENCH_FENS: &[&str] = &[
    START_FEN,
    "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
    "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
    "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
    "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
    "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
    "r1bqkbnr/pppp1ppp/2n5/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R w KQkq - 2 3",
    "r1b2rk1/2q1b1pp/p2ppn2/1p6/3QP3/1BN1B3/PPP3PP/R4RK1 w - - 0 1",
    "2r3k1/pp3ppp/4p3/3pP3/1P1n4/P2B4/5PPP/2R3K1 b - - 0 1",
    "6k1/5ppp/8/8/8/8/5PPP/3R2K1 w - - 0 1",
    "8/8/1p6/p1p5/P1P2k2/1P6/5K2/8 w - - 0 1",
    "4k3/8/8/8/8/8/4P3/4K3 w - - 0 1",
];

/// Known perft results used to verify move generation.
const PERFT_SUITE: &[(&str, u32, u64)] = &[
    (START_FEN, 6, 119_060_324),
    ("r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1", 5, 193_690_690),
    ("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1", 7, 178_633_661),
    ("r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1", 5, 15_833_292),
    ("rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8", 5, 89_941_194),
    ("r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10", 5, 164_075_551),
];

pub fn divide(b: &Board, depth: u32) {
    let start = Instant::now();
    let mut total = 0;
    for mv in b.legal_moves() {
        let mut c = *b;
        c.make_move(mv);
        let n = if depth <= 1 { 1 } else { perft(&c, depth - 1) };
        println!("{mv}: {n}");
        total += n;
    }
    let ms = start.elapsed().as_millis().max(1) as u64;
    println!("\nNodes: {total}  Time: {ms} ms  NPS: {}", total * 1000 / ms);
}

pub fn perft_suite() -> bool {
    let mut ok = true;
    let start = Instant::now();
    for &(fen, depth, expected) in PERFT_SUITE {
        let b = Board::from_fen(fen).unwrap();
        let t = Instant::now();
        let n = perft(&b, depth);
        let status = if n == expected { "ok" } else { "FAIL" };
        ok &= n == expected;
        println!("{status:4} depth {depth} nodes {n:>11} expected {expected:>11} ({} ms)  {fen}", t.elapsed().as_millis());
    }
    println!("{} in {:.1}s", if ok { "ALL PASSED" } else { "FAILURES" }, start.elapsed().as_secs_f64());
    ok
}

pub fn bench(depth: i32) {
    let tt = Arc::new(TT::new(16));
    let stop = Arc::new(AtomicBool::new(false));
    let mut s = Box::new(Searcher::new(tt.clone(), stop));
    s.print = false;
    let mut nodes = 0u64;
    let start = Instant::now();
    for fen in BENCH_FENS {
        let b = Board::from_fen(fen).unwrap();
        tt.clear();
        s.clear();
        s.hashes.clear();
        s.hashes.push(b.hash);
        let limits = Limits { depth: Some(depth), ..Default::default() };
        s.search(&b, &limits);
        nodes += s.nodes;
    }
    let ms = start.elapsed().as_millis().max(1) as u64;
    println!("{nodes} nodes {} nps", nodes * 1000 / ms);
}
