//! Replays games from a PGN with a full-strength engine and reports where the loser went wrong.
//! Used to sanity-check surprising results (e.g. wins against Elo-limited Stockfish).
//!
//! Usage: verify-games <games.pgn> <analysis-engine.exe> <winner-substring> <loser-substring> [max-games] [nodes]

use pawnstorm::attacks;
use pawnstorm::board::Board;
use pawnstorm::types::*;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Game {
    tags: HashMap<String, String>,
    sans: Vec<String>,
}

fn parse_pgn(text: &str) -> Vec<Game> {
    let mut games = Vec::new();
    let mut tags = HashMap::new();
    let mut moves = String::new();
    let flush = |tags: &mut HashMap<String, String>, moves: &mut String, games: &mut Vec<Game>| {
        if tags.is_empty() {
            return;
        }
        // Strip {comments}, then move numbers and the result token.
        let mut clean = String::new();
        let mut depth = 0;
        for c in moves.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ if depth == 0 => clean.push(c),
                _ => {}
            }
        }
        let sans = clean
            .split_whitespace()
            .filter(|t| !matches!(*t, "1-0" | "0-1" | "1/2-1/2" | "*"))
            .map(|t| t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.'))
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        games.push(Game { tags: std::mem::take(tags), sans });
        moves.clear();
    };
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            if !moves.trim().is_empty() {
                flush(&mut tags, &mut moves, &mut games);
            }
            let inner = l.trim_start_matches('[').trim_end_matches(']');
            if let Some((k, v)) = inner.split_once(' ') {
                tags.insert(k.to_string(), v.trim().trim_matches('"').to_string());
            }
        } else {
            moves.push(' ');
            moves.push_str(l);
        }
    }
    flush(&mut tags, &mut moves, &mut games);
    games
}

struct Engine {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Engine {
    fn start(path: &str) -> Engine {
        let mut child = Command::new(path).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().expect("cannot start analysis engine");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut e = Engine { _child: child, stdin, stdout };
        e.send("uci");
        e.wait_for("uciok");
        e.send("setoption name Threads value 4");
        e.send("setoption name Hash value 256");
        e.send("isready");
        e.wait_for("readyok");
        e
    }
    fn send(&mut self, s: &str) {
        writeln!(self.stdin, "{s}").unwrap();
        self.stdin.flush().unwrap();
    }
    fn wait_for(&mut self, token: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            if self.stdout.read_line(&mut line).unwrap_or(0) == 0 {
                panic!("analysis engine exited");
            }
            let l = line.trim().to_string();
            let done = l.starts_with(token);
            lines.push(l);
            if done {
                return lines;
            }
        }
    }
    /// (score in centipawns for the side to move, best move in UCI form)
    fn analyse(&mut self, position: &str, nodes: u64) -> (i32, String) {
        self.send(position);
        self.send(&format!("go nodes {nodes}"));
        let lines = self.wait_for("bestmove");
        let mut score = 0;
        for l in &lines {
            let tk: Vec<&str> = l.split_whitespace().collect();
            if let Some(i) = tk.iter().position(|&t| t == "score") {
                let v: i32 = tk.get(i + 2).and_then(|s| s.parse().ok()).unwrap_or(0);
                score = if tk.get(i + 1) == Some(&"mate") { if v > 0 { 10000 - v } else { -10000 - v } } else { v };
            }
        }
        let best = lines.last().unwrap().split_whitespace().nth(1).unwrap_or("").to_string();
        (score, best)
    }
}

fn pawns(cp: i32) -> String {
    if cp.abs() >= 9000 {
        format!("{}M", if cp > 0 { "+" } else { "-" })
    } else {
        format!("{:+.2}", cp as f64 / 100.0)
    }
}

fn main() {
    attacks::init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: verify-games <games.pgn> <engine.exe> <winner> <loser> [max-games] [nodes]");
        std::process::exit(2);
    }
    let text = std::fs::read_to_string(&args[1]).expect("cannot read PGN");
    let (winner, loser) = (&args[3], &args[4]);
    let max_games: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(3);
    let nodes: u64 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    let mut engine = Engine::start(&args[2]);

    let games = parse_pgn(&text);
    let picked: Vec<&Game> = games
        .iter()
        .filter(|g| {
            let (w, b, r) = (&g.tags["White"], &g.tags["Black"], g.tags.get("Result").map(String::as_str).unwrap_or(""));
            (w.contains(winner.as_str()) && b.contains(loser.as_str()) && r == "1-0")
                || (b.contains(winner.as_str()) && w.contains(loser.as_str()) && r == "0-1")
        })
        .take(max_games)
        .collect();
    let wins = games
        .iter()
        .filter(|g| {
            let (w, b, r) = (&g.tags["White"], &g.tags["Black"], g.tags.get("Result").map(String::as_str).unwrap_or(""));
            (w.contains(winner.as_str()) && b.contains(loser.as_str()) && r == "1-0")
                || (b.contains(winner.as_str()) && w.contains(loser.as_str()) && r == "0-1")
        })
        .count();
    println!("{wins} of {} games are wins for {winner} over {loser}; analysing {} at {nodes} nodes per position\n", games.len(), picked.len());

    for (n, g) in picked.iter().enumerate() {
        let loser_is_white = g.tags["White"].contains(loser.as_str());
        let start = g.tags.get("FEN").cloned();
        let mut b = match &start {
            Some(f) => Board::from_fen(f).unwrap(),
            None => Board::startpos(),
        };
        let base = match &start {
            Some(f) => format!("position fen {f} moves"),
            None => "position startpos moves".to_string(),
        };
        let mut played: Vec<String> = Vec::new();
        let mut mistakes: Vec<(i32, String)> = Vec::new();
        // The biggest error made while the game was still roughly level (eval >= -1.5 before the move).
        let mut turning: Option<(i32, String)> = None;
        let mut bad = 0;
        let mut opening_moves = 0;
        for (ply, san) in g.sans.iter().enumerate() {
            let Some(mv) = b.parse_san(san) else {
                println!("  !! could not parse move {san} at ply {ply}");
                break;
            };
            let to_move_is_loser = (b.stm == WHITE) == loser_is_white;
            // Skip the book opening (the first 8 moves come from the opening file, not the engines).
            let in_book = ply < 16;
            if in_book {
                opening_moves += 1;
            }
            if to_move_is_loser && !in_book {
                let pos = format!("{base} {}", played.join(" "));
                let (best_score, best) = engine.analyse(pos.trim(), nodes);
                let uci = mv.to_uci();
                if uci != best {
                    let after = format!("{base} {} {uci}", played.join(" "));
                    let (s, _) = engine.analyse(after.trim(), nodes);
                    let after_score = -s;
                    let loss = best_score - after_score;
                    if loss >= 100 {
                        bad += 1;
                    }
                    let best_san = b.parse_uci_san(&best);
                    let label = format!(
                        "move {:>3}{} {:<8} best {:<8} eval {} -> {} (lost {:.1} pawns)",
                        b.fullmove,
                        if b.stm == WHITE { "." } else { "..." },
                        san,
                        best_san,
                        pawns(best_score),
                        pawns(after_score),
                        loss as f64 / 100.0
                    );
                    if best_score >= -150 && turning.as_ref().is_none_or(|t| loss > t.0) {
                        turning = Some((loss, label.clone()));
                    }
                    mistakes.push((loss, label));
                }
            }
            played.push(mv.to_uci());
            b.make_move(mv);
        }
        mistakes.sort_by(|a, b| b.0.cmp(&a.0));
        println!(
            "Game {} - {} vs {} {} ({} moves, termination: {}, {} book moves skipped)",
            n + 1,
            g.tags["White"],
            g.tags["Black"],
            g.tags.get("Result").cloned().unwrap_or_default(),
            g.sans.len().div_ceil(2),
            g.tags.get("Termination").cloned().unwrap_or_default(),
            opening_moves
        );
        println!("  {loser} made {bad} mistakes costing 1+ pawn according to full-strength analysis.");
        match &turning {
            Some((_, m)) => println!("  Turning point (worst error while the game was still level):\n    {m}"),
            None => println!("  No real error while the game was level: {winner} outplayed it gradually."),
        }
        println!("  Largest errors overall:");
        for (_, m) in mistakes.iter().take(2) {
            println!("    {m}");
        }
        println!();
    }
    engine.send("quit");
}

trait SanFromUci {
    fn parse_uci_san(&self, uci: &str) -> String;
}

impl SanFromUci for Board {
    fn parse_uci_san(&self, uci: &str) -> String {
        self.parse_move(uci).map(|m| self.to_san(m)).unwrap_or_else(|| uci.to_string())
    }
}
