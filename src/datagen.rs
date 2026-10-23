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
    pub hard_nodes: u64,
    pub keep_every: usize,
    pub max_per_game: usize,
    pub dup_cap: u32,
    pub out: PathBuf,
    /// Stop (cleanly, finishing each thread's current game) after this many seconds.
    pub max_seconds: Option<u64>,
    /// Base random seed; each thread mixes in its index. Defaults to the clock.
    pub seed: Option<u64>,
    /// Real opening lines (UCI moves from the start position, one line per entry; human games, never the
    /// test book): `opening_percent`% of games start from a random one plus 0-4 random plies, the rest
    /// from 8-9 random plies as before. Empty = random openings only.
    pub openings: Vec<Vec<Move>>,
    pub opening_percent: usize,
    /// Targeted FEN starts, selected before the normal opening path.
    pub fens: Vec<Board>,
    pub fen_percent: usize,
    /// Hashes of the test book's start positions: a game whose opening reaches one is skipped.
    pub deny: std::collections::HashSet<u64>,
}

/// Reads a file of FENs (one per line) into position hashes.
pub fn load_deny(path: &str) -> std::collections::HashSet<u64> {
    let text =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read deny file {path}: {e}"));
    let set: std::collections::HashSet<u64> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            Board::from_fen(l)
                .unwrap_or_else(|e| panic!("{path}: bad FEN {l}: {e}"))
                .hash
        })
        .collect();
    assert!(!set.is_empty(), "deny file {path} is empty");
    set
}

/// Reads targeted starting positions (one FEN per line, '#' comments allowed).
pub fn load_fens(path: &str) -> Vec<Board> {
    let text =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read FEN file {path}: {e}"));
    let boards: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| Board::from_fen(l).unwrap_or_else(|e| panic!("{path}: bad FEN {l}: {e}")))
        .collect();
    assert!(!boards.is_empty(), "FEN file {path} is empty");
    boards
}

/// Reads an opening file (one line of UCI moves per entry, '#' comments allowed); every line must be legal.
pub fn load_openings(path: &str) -> Vec<Vec<Move>> {
    let text =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read opening file {path}: {e}"));
    let mut lines = Vec::new();
    for (n, l) in text.lines().enumerate() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let mut b = Board::startpos();
        let mut moves = Vec::new();
        for u in l.split_whitespace() {
            let m = b
                .parse_move(u)
                .unwrap_or_else(|| panic!("{path}:{}: illegal move {u}", n + 1));
            b.make_move(m);
            moves.push(m);
        }
        lines.push(moves);
    }
    assert!(!lines.is_empty(), "opening file {path} has no lines");
    lines
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
    if hashes[n - window..]
        .iter()
        .filter(|&&h| h == b.hash)
        .count()
        >= 3
    {
        return Some(0.5);
    }
    None
}

fn play_game(
    searcher: &mut Searcher,
    tt: &TT,
    rng: &mut Rng,
    cfg: &Config,
    stats: &Stats,
    out: &mut impl Write,
    duplicates: &mut DuplicateCap,
) -> std::io::Result<()> {
    // Opening: a real opening line plus 0-4 random plies (opening_percent% of games), else 8 or 9 random
    // plies, so games (and side to move) vary.
    let mut b = Board::startpos();
    let mut hashes = vec![b.hash];
    let random_plies = if !cfg.fens.is_empty() && rng.below(100) < cfg.fen_percent {
        b = cfg.fens[rng.below(cfg.fens.len())];
        hashes = vec![b.hash];
        rng.below(3)
    } else if !cfg.openings.is_empty() && rng.below(100) < cfg.opening_percent {
        for &m in &cfg.openings[rng.below(cfg.openings.len())] {
            b.make_move(m);
            hashes.push(b.hash);
        }
        rng.below(5)
    } else {
        8 + rng.below(2)
    };
    for _ in 0..random_plies {
        let moves = b.legal_moves();
        if moves.is_empty() {
            return Ok(());
        }
        b.make_move(moves[rng.below(moves.len())]);
        hashes.push(b.hash);
    }
    if hashes.iter().any(|h| cfg.deny.contains(h)) {
        return Ok(()); // reached a test-book start position
    }
    if game_over(&b, &hashes).is_some() {
        return Ok(());
    }

    tt.clear();
    searcher.clear();
    searcher.print = false;

    // Skip openings that are already lopsided.
    searcher.hashes = hashes.clone();
    let (_, s) = searcher.search(
        &b,
        &Limits {
            depth: Some(10),
            nodes: Some(200_000),
            ..Default::default()
        },
    );
    stats.nodes.fetch_add(searcher.nodes, Relaxed);
    if s.abs() > 1000 {
        return Ok(());
    }

    let limits = Limits {
        soft_nodes: Some(cfg.soft_nodes),
        nodes: Some(cfg.hard_nodes),
        ..Default::default()
    };
    let mut records: Vec<(String, i32, u64)> = Vec::with_capacity(256);
    let mut thinning = Thinning::new(cfg.keep_every, rng);
    let mut eligible = 0;
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
        if white_score >= 2000 {
            win_run += 1
        } else {
            win_run = 0
        }
        if white_score <= -2000 {
            loss_run += 1
        } else {
            loss_run = 0
        }
        if plies >= 80 && white_score.abs() <= 10 {
            draw_run += 1
        } else {
            draw_run = 0
        }
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
            if thinning.keep() {
                eligible += 1;
                reservoir(
                    &mut records,
                    (b.to_fen(), white_score, b.hash),
                    eligible,
                    cfg.max_per_game,
                    rng,
                );
            }
        }
        b.make_move(mv);
        hashes.push(b.hash);
        plies += 1;
    };

    let mut saved = 0;
    for (fen, score, hash) in &records {
        if !duplicates.allow(*hash) {
            continue;
        }
        saved += 1;
        writeln!(out, "{fen} | {score} | {result:.1}")?;
    }
    out.flush()?;
    stats.positions.fetch_add(saved, Relaxed);
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
    let text = format!(
        "{text}hard_nodes={}\nkeep_every={}\nmax_per_game={}\ndup_cap={}\n",
        cfg.hard_nodes, cfg.keep_every, cfg.max_per_game, cfg.dup_cap
    );
    // Write then rename so readers never see a half-written file.
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, text).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// Resuming into an existing folder: keep the running totals from its status.txt and cut each data
/// file back to its last complete line (the process may have been killed mid-write).
fn resume(out: &Path, stats: &Stats) -> Option<u64> {
    for e in fs::read_dir(out).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !(name.starts_with("data_") && name.ends_with(".txt")) {
            continue;
        }
        if let Ok(bytes) = fs::read(e.path()) {
            let keep = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
            if keep < bytes.len() {
                if let Ok(f) = fs::OpenOptions::new().write(true).open(e.path()) {
                    let _ = f.set_len(keep as u64);
                }
            }
        }
    }
    let text = fs::read_to_string(out.join("status.txt")).ok()?;
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    stats.positions.store(get("positions")?, Relaxed);
    stats.games.store(get("games").unwrap_or(0), Relaxed);
    stats
        .white_wins
        .store(get("white_wins").unwrap_or(0), Relaxed);
    stats.draws.store(get("draws").unwrap_or(0), Relaxed);
    stats
        .black_wins
        .store(get("black_wins").unwrap_or(0), Relaxed);
    stats.plies.store(get("plies").unwrap_or(0), Relaxed);
    stats.nodes.store(get("nodes").unwrap_or(0), Relaxed);
    get("started")
}

pub fn run(cfg: Config) {
    assert!(cfg.threads > 0 && cfg.threads <= 64, "threads must be 1-64");
    assert!(
        cfg.soft_nodes > 0 && cfg.hard_nodes > 0 && cfg.keep_every > 0,
        "node limits and keep-every must be positive"
    );
    if let Ok(status) = fs::read_to_string(cfg.out.join("status.txt")) {
        for (key, value) in [
            ("soft_nodes", cfg.soft_nodes),
            ("hard_nodes", cfg.hard_nodes),
            ("keep_every", cfg.keep_every as u64),
            ("max_per_game", cfg.max_per_game as u64),
            ("dup_cap", cfg.dup_cap as u64),
        ] {
            if let Some(old) = status
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{key}=")))
            {
                assert_eq!(
                    old.parse::<u64>().unwrap(),
                    value,
                    "{key} changed; use a new output directory"
                );
            }
        }
    }
    fs::create_dir_all(&cfg.out).expect("cannot create output directory");
    let cfg = Arc::new(cfg);
    let stats = Arc::new(Stats::default());
    let done = Arc::new(AtomicBool::new(false));
    let started = match resume(&cfg.out, &stats) {
        Some(t) => {
            println!(
                "resuming: {} positions already in {}",
                stats.positions.load(Relaxed),
                cfg.out.display()
            );
            t
        }
        None => unix_now(),
    };
    let status_path = cfg.out.join("status.txt");
    println!(
        "datagen: {} threads, target {} positions, {} soft nodes/move -> {}",
        cfg.threads,
        cfg.positions,
        cfg.soft_nodes,
        cfg.out.display()
    );
    if !cfg.openings.is_empty() {
        println!(
            "openings: {} real lines, used for {}% of games",
            cfg.openings.len(),
            cfg.opening_percent
        );
    }
    if !cfg.fens.is_empty() {
        println!(
            "FEN starts: {} positions, used for {}% of games",
            cfg.fens.len(),
            cfg.fen_percent
        );
    }

    let mut handles = Vec::new();
    for t in 0..cfg.threads {
        let (cfg, stats, done) = (cfg.clone(), stats.clone(), done.clone());
        handles.push(
            std::thread::Builder::new()
                .stack_size(64 * 1024 * 1024)
                .spawn(move || {
                    let tt = Arc::new(TT::new(16));
                    let mut searcher =
                        Box::new(Searcher::new(tt.clone(), Arc::new(AtomicBool::new(false))));
                    let seed = cfg.seed.unwrap_or_else(|| {
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_nanos() as u64
                    });
                    let mut rng = Rng(seed ^ (t as u64 + 1).wrapping_mul(0x9E3779B97F4A7C15) | 1);
                    let file = fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(cfg.out.join(format!("data_{t:02}.txt")))
                        .expect("cannot open data file");
                    let mut out = BufWriter::with_capacity(1 << 20, file);
                    let mut duplicates = DuplicateCap::new(cfg.dup_cap);
                    while !done.load(Relaxed) && stats.positions.load(Relaxed) < cfg.positions {
                        if let Err(e) = play_game(
                            &mut searcher,
                            &tt,
                            &mut rng,
                            &cfg,
                            &stats,
                            &mut out,
                            &mut duplicates,
                        ) {
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
        if cfg.max_seconds.is_some_and(|s| t0.elapsed().as_secs() >= s) {
            done.store(true, Relaxed);
        }
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

/// One random phase per game; the default consumes no RNG, preserving existing games.
struct Thinning {
    every: usize,
    phase: usize,
}
impl Thinning {
    fn new(every: usize, rng: &mut Rng) -> Self {
        Self {
            every,
            phase: if every == 1 { 0 } else { rng.below(every) },
        }
    }
    fn keep(&mut self) -> bool {
        let keep = self.phase == 0;
        self.phase = (self.phase + 1) % self.every;
        keep
    }
}
fn reservoir<T>(items: &mut Vec<T>, value: T, seen: usize, cap: usize, rng: &mut Rng) {
    if cap == 0 || items.len() < cap {
        items.push(value);
    } else {
        let i = rng.below(seen);
        if i < cap {
            items[i] = value;
        }
    }
}
/// Conservative bounded counting table: collisions can reject distinct positions, never
/// allow an exact hash more than K times. 16 MiB/thread when enabled; no eviction.
struct DuplicateCap {
    cap: u32,
    counts: Vec<u32>,
}
impl DuplicateCap {
    fn new(cap: u32) -> Self {
        Self {
            cap,
            counts: if cap == 0 {
                Vec::new()
            } else {
                vec![0; 1 << 22]
            },
        }
    }
    fn allow(&mut self, hash: u64) -> bool {
        if self.cap == 0 {
            return true;
        }
        let i = (hash ^ (hash >> 32)) as usize & (self.counts.len() - 1);
        if self.counts[i] >= self.cap {
            false
        } else {
            self.counts[i] += 1;
            true
        }
    }
}
#[cfg(test)]
mod quality_tests {
    use super::*;
    #[test]
    fn thinning_fraction_and_offsets() {
        let mut rng = Rng(123);
        let mut offsets = std::collections::HashSet::new();
        for _ in 0..100 {
            let mut t = Thinning::new(7, &mut rng);
            offsets.insert(t.phase);
            assert_eq!((0..700).filter(|_| t.keep()).count(), 100);
        }
        assert_eq!(offsets.len(), 7);
        let before = rng.0;
        let mut t = Thinning::new(1, &mut rng);
        assert!(t.keep());
        assert_eq!(rng.0, before);
    }
    #[test]
    fn uniform_game_cap() {
        let mut rng = Rng(456);
        let mut counts = [0; 100];
        for _ in 0..2000 {
            let mut v = Vec::new();
            for i in 0..100 {
                reservoir(&mut v, i, i + 1, 5, &mut rng);
            }
            assert_eq!(v.len(), 5);
            for i in v {
                counts[i] += 1;
            }
        }
        assert!(counts.iter().all(|&n| (60..145).contains(&n)), "{counts:?}");
    }
    #[test]
    fn duplicate_bound() {
        let mut d = DuplicateCap::new(2);
        assert!(d.allow(0));
        assert!(d.allow(0));
        assert!(!d.allow(0));
        assert!(d.allow(1));
        assert_eq!(d.counts.len(), 1 << 22);
        let mut off = DuplicateCap::new(0);
        for _ in 0..10 {
            assert!(off.allow(9));
        }
        assert!(off.counts.is_empty());
    }
}
