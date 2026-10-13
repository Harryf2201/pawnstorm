//! UCI protocol loop. The search runs on its own thread so "stop" and "isready" stay responsive.

use crate::bench;
use crate::board::*;
use crate::eval::evaluate;
use crate::search::*;
use crate::tt::TT;
use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

pub const NAME: &str = "Pawnstorm";
const SEARCH_STACK: usize = 64 * 1024 * 1024;

struct Engine {
    board: Board,
    hashes: Vec<u64>,
    tt: Arc<TT>,
    stop: Arc<AtomicBool>,
    searcher: Option<Box<Searcher>>,
    /// Extra Lazy-SMP threads (Threads - 1 of them), each with its own histories.
    helpers: Vec<Box<Searcher>>,
    handle: Option<JoinHandle<(Box<Searcher>, Vec<Box<Searcher>>)>>,
    hash_mb: usize,
    overhead_ms: u64,
    /// True from "go ponder" until "ponderhit" (shared with the main searcher).
    ponder: Arc<AtomicBool>,
}

const MAX_THREADS: usize = 64;

fn helper(tt: &Arc<TT>, stop: &Arc<AtomicBool>) -> Box<Searcher> {
    let mut s = Box::new(Searcher::new(tt.clone(), stop.clone()));
    s.print = false;
    s.main_thread = false;
    s
}

impl Engine {
    fn new() -> Engine {
        let tt = Arc::new(TT::new(16));
        let stop = Arc::new(AtomicBool::new(false));
        let searcher = Box::new(Searcher::new(tt.clone(), stop.clone()));
        let board = Board::startpos();
        Engine {
            hashes: vec![board.hash],
            board,
            tt,
            stop,
            searcher: Some(searcher),
            helpers: Vec::new(),
            handle: None,
            hash_mb: 16,
            overhead_ms: 10,
            ponder: Arc::new(AtomicBool::new(false)),
        }
    }

    fn wait(&mut self) {
        if let Some(h) = self.handle.take() {
            let (main, helpers) = h.join().expect("search thread panicked");
            self.searcher = Some(main);
            self.helpers = helpers;
        }
    }

    fn set_hash(&mut self, mb: usize) {
        self.wait();
        self.hash_mb = mb.clamp(1, 65536);
        self.tt = Arc::new(TT::new(self.hash_mb));
        self.searcher = Some(Box::new(Searcher::new(self.tt.clone(), self.stop.clone())));
        let n = self.helpers.len();
        self.helpers = (0..n).map(|_| helper(&self.tt, &self.stop)).collect();
    }

    fn set_threads(&mut self, n: usize) {
        self.wait();
        let n = n.clamp(1, MAX_THREADS);
        self.helpers.truncate(n - 1);
        while self.helpers.len() < n - 1 {
            self.helpers.push(helper(&self.tt, &self.stop));
        }
    }

    fn position(&mut self, tokens: &[&str]) {
        let i;
        let board = match tokens.get(1) {
            Some(&"startpos") => {
                i = 2;
                Board::startpos()
            }
            Some(&"fen") => {
                let end = tokens.iter().position(|&t| t == "moves").unwrap_or(tokens.len());
                let fen = tokens[2..end].join(" ");
                i = end;
                match Board::from_fen(&fen) {
                    Ok(b) => b,
                    Err(e) => {
                        println!("info string invalid fen: {e}");
                        return;
                    }
                }
            }
            _ => return,
        };
        self.board = board;
        self.hashes.clear();
        self.hashes.push(self.board.hash);
        if tokens.get(i) == Some(&"moves") {
            for m in &tokens[i + 1..] {
                match self.board.parse_move(m) {
                    Some(mv) => {
                        self.board.make_move(mv);
                        self.hashes.push(self.board.hash);
                    }
                    None => {
                        println!("info string illegal move: {m}");
                        break;
                    }
                }
            }
        }
    }

    fn go(&mut self, tokens: &[&str]) {
        self.wait();
        let mut limits = Limits::default();
        let num = |i: usize| tokens.get(i + 1).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0).max(0) as u64;
        for (i, &t) in tokens.iter().enumerate() {
            match t {
                "depth" => limits.depth = Some(num(i) as i32),
                "nodes" => limits.nodes = Some(num(i)),
                "movetime" => limits.movetime = Some(num(i)),
                "wtime" => limits.time[0] = Some(num(i)),
                "btime" => limits.time[1] = Some(num(i)),
                "winc" => limits.inc[0] = num(i),
                "binc" => limits.inc[1] = num(i),
                "movestogo" => limits.movestogo = Some(num(i)),
                "infinite" => limits.infinite = true,
                _ => {}
            }
        }
        self.stop.store(false, Ordering::Relaxed);
        self.ponder.store(tokens.contains(&"ponder"), Ordering::Relaxed);
        let mut searcher = self.searcher.take().unwrap();
        searcher.ponder = self.ponder.clone();
        searcher.hashes.clone_from(&self.hashes);
        searcher.print = true;
        searcher.overhead_ms = self.overhead_ms;
        let board = self.board;
        let mut helpers = std::mem::take(&mut self.helpers);
        for h in helpers.iter_mut() {
            h.hashes.clone_from(&self.hashes);
        }
        let stop = self.stop.clone();
        let handle = std::thread::Builder::new()
            .stack_size(SEARCH_STACK)
            .spawn(move || {
                // Helpers search until the main thread is done (or the depth limit); only the
                // main thread watches the clock and the node limit.
                let helper_limits = Limits { depth: limits.depth, ..Limits::default() };
                let running: Vec<JoinHandle<Box<Searcher>>> = helpers
                    .into_iter()
                    .map(|mut h| {
                        let lim = helper_limits.clone();
                        std::thread::Builder::new()
                            .stack_size(SEARCH_STACK)
                            .spawn(move || {
                                h.search(&board, &lim);
                                h
                            })
                            .unwrap()
                    })
                    .collect();
                let (mv, _) = searcher.search(&board, &limits);
                stop.store(true, Ordering::Relaxed);
                if searcher.ponder_move.is_null() {
                    println!("bestmove {mv}");
                } else {
                    println!("bestmove {mv} ponder {}", searcher.ponder_move);
                }
                let helpers = running.into_iter().map(|j| j.join().expect("helper thread panicked")).collect();
                (searcher, helpers)
            })
            .unwrap();
        self.handle = Some(handle);
    }
}

pub fn run() {
    let mut e = Engine::new();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let tokens: Vec<&str> = line.trim_start_matches('\u{feff}').split_whitespace().collect();
        let Some(&cmd) = tokens.first() else { continue };
        match cmd {
            "uci" => {
                println!("id name {NAME} {}", env!("CARGO_PKG_VERSION"));
                println!("id author Harryf2201 & Claude");
                println!("option name Hash type spin default 16 min 1 max 65536");
                println!("option name Threads type spin default 1 min 1 max {MAX_THREADS}");
                println!("option name Move Overhead type spin default 10 min 0 max 5000");
                println!("option name Ponder type check default false");
                println!("option name EvalFile type string default <embedded>");
                println!("option name SyzygyPath type string default <empty>");
                for (name, def, min, max) in crate::params::DEFS {
                    println!("option name {name} type spin default {def} min {min} max {max}");
                }
                println!("uciok");
            }
            "isready" => println!("readyok"),
            "ucinewgame" => {
                e.wait();
                e.tt.clear();
                e.searcher.as_mut().unwrap().clear();
                for h in e.helpers.iter_mut() {
                    h.clear();
                }
            }
            "setoption" => {
                let name_end = tokens.iter().position(|&t| t == "value").unwrap_or(tokens.len());
                let name = tokens.get(2..name_end).map(|s| s.join(" ")).unwrap_or_default().to_lowercase();
                let value = tokens.get(name_end + 1).copied().unwrap_or("");
                match name.as_str() {
                    "hash" => e.set_hash(value.parse().unwrap_or(16)),
                    "threads" => e.set_threads(value.parse().unwrap_or(1)),
                    "move overhead" => e.overhead_ms = value.parse().unwrap_or(10),
                    "evalfile" => {
                        e.wait();
                        let path = tokens.get(name_end + 1..).map(|s| s.join(" ")).unwrap_or_default();
                        if path != "<embedded>" && !path.is_empty() {
                            match crate::nnue::load_file(&path) {
                                Ok(()) => println!("info string loaded network {path}"),
                                Err(err) => println!("info string {err}"),
                            }
                        }
                    }
                    "syzygypath" => {
                        e.wait();
                        let path = tokens.get(name_end + 1..).map(|s| s.join(" ")).unwrap_or_default();
                        let (n, max) = crate::tb::init(&path);
                        println!("info string syzygy: {n} tables, up to {max} pieces");
                    }
                    _ => {
                        e.wait();
                        if let Ok(v) = value.parse::<i32>() {
                            crate::params::set(&name, v);
                        }
                    }
                }
            }
            "position" => {
                e.wait();
                e.position(&tokens);
            }
            "go" => e.go(&tokens),
            // The opponent played the expected move: the ponder search carries on as a normal timed search.
            "ponderhit" => e.ponder.store(false, Ordering::Relaxed),
            "stop" => {
                e.stop.store(true, Ordering::Relaxed);
                e.wait();
            }
            "quit" => {
                e.stop.store(true, Ordering::Relaxed);
                e.wait();
                break;
            }
            "d" => print!("{}", e.board.pretty()),
            "eval" => {
                println!("classical: {} cp (side to move)", evaluate(&e.board));
                if crate::nnue::active() {
                    println!("nnue:      {} cp (side to move)", crate::nnue::evaluate_board(&e.board));
                } else {
                    println!("nnue:      no network loaded");
                }
            }
            "perft" => {
                let depth = tokens.get(1).and_then(|s| s.parse().ok()).unwrap_or(5);
                bench::divide(&e.board, depth);
            }
            "bench" => {
                e.wait();
                let depth = tokens.get(1).and_then(|s| s.parse().ok()).unwrap_or(bench::DEFAULT_DEPTH);
                bench::bench(depth);
            }
            _ => println!("info string unknown command: {cmd}"),
        }
    }
    e.stop.store(true, Ordering::Relaxed);
    e.wait();
}
