//! Self-play data generation for NNUE training.
//!
//! Many threads play fast games from random openings. Quiet positions are written as
//! `<fen> | <score> | <result>` lines (score in centipawns, both from White's point of view;
//! result 1.0 / 0.5 / 0.0), the text format bullet's tools convert to training data.
//! A status.txt file is refreshed every couple of seconds so the dashboard can show progress.

use crate::board::*;
use crate::search::*;
use crate::tt::TT;
use crate::types::*;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct Config {
    pub threads: usize,
    pub positions: u64,
    pub soft_nodes: u64,
    pub out: PathBuf,
}

#[derive(Default)]
struct Stats {
    positions: AtomicU64,
    games: AtomicU64,
    white_wins: AtomicU64,
    draws: AtomicU64,
    black_wins: AtomicU64,
    plies: AtomicU64,
    nodes: AtomicU64,
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Result of the game from White's point of view, if it is over.
fn game_over(b: &Board, hashes: &[u64]) -> Option<f32> {
    if b.legal_moves().is_empty() {
        return Some(if !b.in_check() {
            0.5
        } else if b.stm == WHITE {
            0.0
        } else {
            1.0
        });
    }
    if b.halfmove >= 100 || b.is_insufficient_material() {
        return Some(0.5);
    }
    let n = hashes.len();
    let window = (b.halfmove as usize + 1).min(n);
    if hashes[n - window..].iter().filter(|&&h| h == b.hash).count() >= 3 {
        return Some(0.5);
    }
    None
}

fn play_game(searcher: &mut Searcher, tt: &TT, rng: &mut Rng, cfg: &Config, stats: &Stats, out: &mut impl Write) -> std::io::Result<()> {
    // Random opening: 8 or 9 random plies so games (and side to move) vary.
    let mut b = Board::startpos();
    let mut hashes = vec![b.hash];
    for _ in 0..8 + rng.below(2) {
        let moves = b.legal_moves();
        if moves.is_empty() {
            return Ok(());
        }
        b.make_move(moves[rng.below(moves.len())]);
        hashes.push(b.hash);
    }
    if game_over(&b, &hashes).is_some() {
        return Ok(());
    }

    tt.clear();
    searcher.clear();
    searcher.print = false;

    // Skip openings that are already lopsided.
    searcher.hashes = hashes.clone();
    let (_, s) = searcher.search(&b, &Limits { depth: Some(10), nodes: Some(200_000), ..Default::default() });
    stats.nodes.fetch_add(searcher.nodes, Relaxed);
    if s.abs() > 1000 {
        return Ok(());
    }

    let limits = Limits { soft_nodes: Some(cfg.soft_nodes), nodes: Some(cfg.soft_nodes * 20), ..Default::default() };
    let mut records: Vec<(String, i32)> = Vec::with_capacity(256);
    let (mut win_run, mut loss_run, mut draw_run) = (0, 0, 0);
    let mut plies = 0;
    let result = loop {
        if let Some(r) = game_over(&b, &hashes) {
            break r;
        }
        if plies >= 400 {
            break 0.5;
        }
        searcher.hashes = hashes.clone();
        let (mv, score) = searcher.search(&b, &limits);
        stats.nodes.fetch_add(searcher.nodes, Relaxed);
        if mv.is_null() {
            break 0.5;
        }
        let white_score = if b.stm == WHITE { score } else { -score };

        // Adjudication: a clearly decided or dead-drawn game doesn't need playing out.
        if white_score >= 2000 { win_run += 1 } else { win_run = 0 }
        if white_score <= -2000 { loss_run += 1 } else { loss_run = 0 }
        if plies >= 80 && white_score.abs() <= 10 { draw_run += 1 } else { draw_run = 0 }
        if win_run >= 4 {
            break 1.0;
        }
        if loss_run >= 4 {
            break 0.0;
        }
        if draw_run >= 10 {
            break 0.5;
        }

        // Keep quiet positions only: the network learns static evaluation, not tactics.
        if !b.in_check() && mv.is_quiet() && score.abs() < MATE_BOUND {
            records.push((b.to_fen(), white_score));
        }
        b.make_move(mv);
        hashes.push(b.hash);
        plies += 1;
    };

    for (fen, score) in &records {
        writeln!(out, "{fen} | {score} | {result:.1}")?;
    }
    out.flush()?;
    stats.positions.fetch_add(records.len() as u64, Relaxed);
    stats.games.fetch_add(1, Relaxed);
    stats.plies.fetch_add(plies, Relaxed);
    match result {
        r if r > 0.75 => stats.white_wins.fetch_add(1, Relaxed),
        r if r < 0.25 => stats.black_wins.fetch_add(1, Relaxed),
        _ => stats.draws.fetch_add(1, Relaxed),
    };
    Ok(())
}

fn write_status(path: &Path, cfg: &Config, stats: &Stats, started: u64, state: &str) {
    let text = format!(
        "state={state}\nstarted={started}\nupdated={}\nthreads={}\ntarget={}\nsoft_nodes={}\npositions={}\ngames={}\nwhite_wins={}\ndraws={}\nblack_wins={}\nplies={}\nnodes={}\n",
        unix_now(),
        cfg.threads,
        cfg.positions,
        cfg.soft_nodes,
        stats.positions.load(Relaxed),
        stats.games.load(Relaxed),
        stats.white_wins.load(Relaxed),
        stats.draws.load(Relaxed),
        stats.black_wins.load(Relaxed),
        stats.plies.load(Relaxed),
        stats.nodes.load(Relaxed),
    );
    // Write then rename so readers never see a half-written file.
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, text).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

pub fn run(cfg: Config) {
    fs::create_dir_all(&cfg.out).expect("cannot create output directory");
    let cfg = Arc::new(cfg);
    let stats = Arc::new(Stats::default());
    let done = Arc::new(AtomicBool::new(false));
    let started = unix_now();
    let status_path = cfg.out.join("status.txt");
    println!(
        "datagen: {} threads, target {} positions, {} soft nodes/move -> {}",
        cfg.threads,
        cfg.positions,
        cfg.soft_nodes,
        cfg.out.display()
    );

    let mut handles = Vec::new();
    for t in 0..cfg.threads {
        let (cfg, stats, done) = (cfg.clone(), stats.clone(), done.clone());
        handles.push(
            std::thread::Builder::new()
                .stack_size(64 * 1024 * 1024)
                .spawn(move || {
                    let tt = Arc::new(TT::new(16));
                    let mut searcher = Box::new(Searcher::new(tt.clone(), Arc::new(AtomicBool::new(false))));
                    let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
                    let mut rng = Rng(seed ^ (t as u64 + 1).wrapping_mul(0x9E3779B97F4A7C15) | 1);
                    let file = fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(cfg.out.join(format!("data_{t:02}.txt")))
                        .expect("cannot open data file");
                    let mut out = BufWriter::with_capacity(1 << 20, file);
                    while !done.load(Relaxed) && stats.positions.load(Relaxed) < cfg.positions {
                        if let Err(e) = play_game(&mut searcher, &tt, &mut rng, &cfg, &stats, &mut out) {
                            eprintln!("thread {t}: write failed: {e}");
                            break;
                        }
                    }
                    let _ = out.flush();
                })
                .unwrap(),
        );
    }

    let t0 = Instant::now();
    while handles.iter().any(|h| !h.is_finished()) {
        std::thread::sleep(std::time::Duration::from_secs(2));
        write_status(&status_path, &cfg, &stats, started, "running");
        let pos = stats.positions.load(Relaxed);
        let secs = t0.elapsed().as_secs_f64().max(1.0);
        print!(
            "\r{pos} / {} positions | {} games | {:.0} pos/s   ",
            cfg.positions,
            stats.games.load(Relaxed),
            pos as f64 / secs
        );
        let _ = std::io::stdout().flush();
    }
    for h in handles {
        let _ = h.join();
    }
    write_status(&status_path, &cfg, &stats, started, "finished");
    println!("\ndone.");
}
