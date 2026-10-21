//! Builds the real-opening start file for self-play data generation (gen7 on): one line of UCI moves from the
//! start position per entry, taken from human games only.
//!
//! Reads a Lichess monthly database (PGN, zstd-compressed) from stdin and keeps rated human-vs-human games
//! (no BOT titles) where both players are rated at least --min-elo, at blitz or slower (base time >= 180 s).
//! Each kept game contributes its first L plies, L between --min-ply and --max-ply (chosen from a seeded hash,
//! so the file is reproducible). Lines are deduplicated by the position they reach (pieces, side to move,
//! castling, en passant), and any line that reaches a test-book start position (the end of each --book line)
//! is rejected, so self-play never starts from a test opening. Stops after --lines unique lines.
//! No evaluations or annotations are read: only moves and the eligibility headers.
//!
//! Usage: curl -s <url.pgn.zst> | openings --book 8moves_v3.pgn --out gen7.uci [--lines 100000]
//!        [--min-elo 2200] [--min-ply 8] [--max-ply 16] [--seed 7]

use pawnstorm::board::Board;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};

struct Opts {
    book: String,
    out: String,
    lines: usize,
    min_elo: u32,
    min_ply: usize,
    max_ply: usize,
    seed: u64,
}

/// Position identity without the move counters.
fn key(b: &Board) -> String {
    b.to_fen().split(' ').take(4).collect::<Vec<_>>().join(" ")
}

/// SAN tokens of a PGN movetext: comments, move numbers, annotations and the result removed.
fn san_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut clean = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '{' | '(' => depth += 1,
            '}' | ')' => depth -= 1,
            _ if depth == 0 => clean.push(c),
            _ => {}
        }
    }
    for t in clean.split_whitespace() {
        if ["1-0", "0-1", "1/2-1/2", "*"].contains(&t) {
            continue;
        }
        let t = t.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches('.');
        let t = t.trim_end_matches(|c| c == '!' || c == '?');
        if t.is_empty() || t.starts_with('$') || ["1-0", "0-1", "1/2-1/2", "*"].contains(&t) {
            continue;
        }
        out.push(t.to_string());
    }
    out
}

/// Plays SAN moves from the start; the UCI moves and the boards after each ply, or None on an illegal move.
fn play(sans: &[String], plies: usize) -> Option<(Vec<String>, Vec<Board>)> {
    let mut b = Board::startpos();
    let (mut ucis, mut boards) = (Vec::new(), Vec::new());
    for s in sans.iter().take(plies) {
        let Some(mv) = b.parse_san(s) else {
            if std::env::var("OPENINGS_DEBUG").is_ok() { eprintln!("unparsed SAN {s:?} after {ucis:?}"); }
            return None;
        };
        ucis.push(mv.to_uci());
        b.make_move(mv);
        boards.push(b);
    }
    (ucis.len() == plies).then_some((ucis, boards))
}

/// Games as (headers, movetext) from a PGN stream.
fn games(r: impl BufRead, mut f: impl FnMut(&[(String, String)], &str) -> bool) {
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut moves = String::new();
    let mut flush = |headers: &mut Vec<(String, String)>, moves: &mut String| -> bool {
        let go = if moves.trim().is_empty() { true } else { f(headers, moves) };
        headers.clear();
        moves.clear();
        go
    };
    for line in r.lines() {
        let Ok(line) = line else { break };
        let l = line.trim();
        if l.starts_with('[') {
            if !moves.trim().is_empty() && !flush(&mut headers, &mut moves) {
                return;
            }
            if let Some((k, v)) = l.trim_start_matches('[').trim_end_matches(']').split_once(' ') {
                headers.push((k.to_string(), v.trim_matches('"').to_string()));
            }
        } else if !l.is_empty() {
            moves.push(' ');
            moves.push_str(l);
        }
    }
    flush(&mut headers, &mut moves);
}

/// A zstd stream of several frames, skippable frames included (the Lichess dumps start with one).
struct Frames<R: Read> {
    src: R,
    dec: ruzstd::FrameDecoder,
    active: bool,
}

impl<R: Read> Read for Frames<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use ruzstd::frame::ReadFrameHeaderError::SkipFrame;
        use ruzstd::frame_decoder::FrameDecoderError::ReadFrameHeaderError;
        loop {
            if !self.active {
                match self.dec.reset(&mut self.src) {
                    Ok(()) => self.active = true,
                    Err(ReadFrameHeaderError(SkipFrame { length, .. })) => {
                        std::io::copy(&mut (&mut self.src).take(length as u64), &mut std::io::sink())?;
                        continue;
                    }
                    Err(_) => return Ok(0), // end of the stream (or of what was downloaded)
                }
            }
            while self.dec.can_collect() < buf.len() && !self.dec.is_finished() {
                let need = buf.len() - self.dec.can_collect();
                self.dec
                    .decode_blocks(&mut self.src, ruzstd::BlockDecodingStrategy::UptoBytes(need))
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            }
            let n = self.dec.read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            if self.dec.is_finished() {
                self.active = false;
            }
        }
    }
}

fn hash(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

fn main() {
    pawnstorm::attacks::init();
    let a: Vec<String> = std::env::args().collect();
    let get = |k: &str, d: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned().unwrap_or(d.to_string());
    let o = Opts {
        book: get("--book", ""),
        out: get("--out", "gen7.uci"),
        lines: get("--lines", "100000").parse().unwrap(),
        min_elo: get("--min-elo", "2200").parse().unwrap(),
        min_ply: get("--min-ply", "8").parse().unwrap(),
        max_ply: get("--max-ply", "16").parse().unwrap(),
        seed: get("--seed", "7").parse().unwrap(),
    };

    // The test book's start positions (the end of each of its lines): a line that reaches one is rejected, so
    // self-play never starts from (or passes through) a position our tests start from. (Denying every book
    // position from ply 8 on would remove ~75% of real openings: the book covers the common main lines.)
    // The same positions are written (as FENs) to --deny-out for datagen's --deny, which also rejects games whose
    // random plies after a line reach one. The book is required and must parse completely.
    assert!(!o.book.is_empty(), "--book <test book PGN> is required");
    let mut deny: HashSet<String> = HashSet::new();
    let mut deny_fens = Vec::new();
    let mut book_lines = 0;
    let f = std::fs::File::open(&o.book).expect("book file");
    games(BufReader::new(f), |_, mv| {
        let mut b = Board::startpos();
        for s in san_tokens(mv) {
            let m = b.parse_san(&s).unwrap_or_else(|| panic!("test book line {}: cannot parse {s}", book_lines + 1));
            b.make_move(m);
        }
        book_lines += 1;
        if deny.insert(key(&b)) {
            deny_fens.push(b.to_fen());
        }
        true
    });
    eprintln!("test book: {book_lines} lines, {} distinct start positions denied", deny.len());
    let deny_out = get("--deny-out", "");
    if !deny_out.is_empty() {
        std::fs::write(&deny_out, deny_fens.join("\n") + "\n").expect("deny-out file");
    }
    if o.lines == 0 {
        return;
    }

    let mut out = std::io::BufWriter::new(std::fs::File::create(&o.out).expect("out file"));
    let (mut read, mut eligible, mut kept, mut dup, mut denied) = (0u64, 0u64, 0usize, 0u64, 0u64);
    let mut seen: HashSet<String> = HashSet::new();
    let stdin = std::io::stdin().lock();
    let dec = Frames { src: stdin, dec: ruzstd::FrameDecoder::new(), active: false };
    let r = BufReader::with_capacity(1 << 20, dec.take(u64::MAX));
    games(r, |h, mv| {
        read += 1;
        let hv = |k: &str| h.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str()).unwrap_or("");
        let elo = |k: &str| hv(k).parse::<u32>().unwrap_or(0);
        let base = hv("TimeControl").split('+').next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        let ok = hv("Event").starts_with("Rated")
            && hv("WhiteTitle") != "BOT"
            && hv("BlackTitle") != "BOT"
            && elo("WhiteElo") >= o.min_elo
            && elo("BlackElo") >= o.min_elo
            && base >= 180
            && hv("Termination") == "Normal";
        if ok {
            eligible += 1;
            let plies = o.min_ply + (hash(o.seed ^ eligible) as usize) % (o.max_ply - o.min_ply + 1);
            if let Some((ucis, boards)) = play(&san_tokens(mv), plies) {
                let k = key(boards.last().unwrap());
                if boards.iter().skip(o.min_ply - 1).any(|b| deny.contains(&key(b))) {
                    denied += 1;
                } else if !seen.insert(k) {
                    dup += 1;
                } else {
                    writeln!(out, "{}", ucis.join(" ")).unwrap();
                    kept += 1;
                }
            }
        }
        if read % 200_000 == 0 {
            eprintln!("read {read}, eligible {eligible}, kept {kept}, duplicates {dup}, test-book rejects {denied}");
        }
        kept < o.lines
    });
    out.flush().unwrap();
    eprintln!("done: read {read}, eligible {eligible}, kept {kept}, duplicates {dup}, test-book rejects {denied}");
    if kept < o.lines {
        eprintln!("ERROR: only {kept} of {} lines (the stream ended or broke early)", o.lines);
        std::process::exit(1);
    }
}
