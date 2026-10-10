//! Pawnstorm Lab: a live dashboard for fastchess tournaments.
//!
//! Each run lives in `<root>/runs/<id>/` and contains:
//!   meta.txt    key=value lines written by tools/match.ps1
//!   games.pgn   finished games (fastchess -pgnout)
//!   engine.log  engine traffic (fastchess -log engine=true), used for live boards
//!   output.txt  fastchess stdout (SPRT/LLR lines, "Finished match")
//!
//! Files are tailed incrementally, so even huge logs are only read once.
//! Usage: dashboard <testing-root> [port]

use pawnstorm::attacks;
use pawnstorm::board::Board;
use pawnstorm::types::*;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const INDEX: &str = include_str!("index.html");
const STALE_SECS: u64 = 30;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn js(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => {}
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn num(x: f64) -> String {
    if x.is_finite() {
        format!("{:.2}", x)
    } else {
        "null".into()
    }
}

/// Reads only the bytes appended to a file since the last call, returning complete lines.
#[derive(Default)]
struct Tail {
    offset: u64,
    partial: String,
}

impl Tail {
    fn read_new(&mut self, path: &Path) -> Vec<String> {
        let Ok(mut f) = File::open(path) else { return vec![] };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return vec![];
        }
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return vec![];
        }
        let mut buf = Vec::new();
        let _ = (&mut f).take(len - self.offset).read_to_end(&mut buf);
        self.offset += buf.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&buf));
        match self.partial.rfind('\n') {
            Some(pos) => {
                let rest = self.partial.split_off(pos + 1);
                let lines = self.partial.lines().map(|l| l.trim_end_matches('\r').to_string()).collect();
                self.partial = rest;
                lines
            }
            None => vec![],
        }
    }
}

#[derive(Default, Clone)]
struct Meta {
    name: String,
    kind: String,
    tc: String,
    total: u64,
    main: String,
    anchors: Vec<(String, f64)>,
    created: u64,
    concurrency: u64,
    notes: String,
}

fn read_meta(dir: &Path) -> Option<Meta> {
    let text = std::fs::read_to_string(dir.join("meta.txt")).ok()?;
    let mut m = Meta::default();
    for line in text.lines() {
        let Some((k, v)) = line.trim_start_matches('\u{feff}').split_once('=') else { continue };
        let v = v.trim().to_string();
        match k.trim() {
            "name" => m.name = v,
            "type" => m.kind = v,
            "tc" => m.tc = v,
            "total" => m.total = v.parse().unwrap_or(0),
            "main" => m.main = v,
            "created" => m.created = v.parse().unwrap_or(0),
            "concurrency" => m.concurrency = v.parse().unwrap_or(0),
            "notes" => m.notes = v,
            "anchor" => {
                if let Some((n, e)) = v.rsplit_once(':') {
                    if let Ok(e) = e.parse() {
                        m.anchors.push((n.to_string(), e));
                    }
                }
            }
            _ => {}
        }
    }
    Some(m)
}

#[derive(Default, Clone)]
struct Game {
    white: String,
    black: String,
    result: String,
    termination: String,
    reason: String,
    plies: u32,
    duration: String,
}

#[derive(Default)]
struct PgnParser {
    cur: Game,
    last_comment: String,
}

fn looks_like_eval(c: &str) -> bool {
    let c = c.trim();
    c.starts_with('+') || c.starts_with('-') || c.starts_with(|ch: char| ch.is_ascii_digit()) || c.starts_with("book")
}

impl PgnParser {
    fn feed(&mut self, line: &str, out: &mut Vec<Game>) {
        let l = line.trim();
        if l.is_empty() {
            return;
        }
        if l.starts_with('[') {
            let inner = l.trim_start_matches('[').trim_end_matches(']');
            if let Some((k, v)) = inner.split_once(' ') {
                let v = v.trim().trim_matches('"').to_string();
                match k {
                    "White" => self.cur.white = v,
                    "Black" => self.cur.black = v,
                    "Result" => self.cur.result = v,
                    "Termination" => self.cur.termination = v,
                    "PlyCount" => self.cur.plies = v.parse().unwrap_or(0),
                    "GameDuration" => self.cur.duration = v,
                    _ => {}
                }
            }
            return;
        }
        let mut s = l;
        while let Some(a) = s.find('{') {
            match s[a..].find('}') {
                Some(b) => {
                    let c = &s[a + 1..a + b];
                    if !looks_like_eval(c) {
                        self.last_comment = c.trim().to_string();
                    } else {
                        // fastchess appends the game-ending reason to the last move's comment:
                        // {+M1/124 0.001s, Black mates}
                        for part in c.split(',').skip(1) {
                            let t = part.trim();
                            if !t.is_empty() && !t.contains('=') {
                                self.last_comment = t.to_string();
                            }
                        }
                    }
                    s = &s[a + b + 1..];
                }
                None => break,
            }
        }
        if matches!(l.split_whitespace().last(), Some("1-0" | "0-1" | "1/2-1/2" | "*")) {
            let mut g = std::mem::take(&mut self.cur);
            g.reason = std::mem::take(&mut self.last_comment);
            out.push(g);
        }
    }
}

#[derive(Default, Clone)]
struct Slot {
    names: Vec<String>,
    reset_pending: bool,
    pos_cmd: String,
    to_move: String,
    wtime: u64,
    btime: u64,
    mate: bool,
    score: Option<i32>,
    depth: u32,
    nps: u64,
    updated: u64,
}

#[derive(Default)]
struct RunState {
    meta: Meta,
    pgn_tail: Tail,
    pgn: PgnParser,
    games: Vec<Game>,
    log_tail: Tail,
    slots: BTreeMap<u32, Slot>,
    out_tail: Tail,
    llr: Option<Vec<f64>>,
    verdict: String,
    finished: bool,
    last_activity: u64,
    /// (rating, ±95% error, derived from an earlier run's rating rather than a fixed anchor)
    rating: Option<(f64, f64, bool)>,
}

fn after<'a>(tokens: &[&'a str], key: &str) -> Option<&'a str> {
    tokens.iter().position(|&t| t == key).and_then(|i| tokens.get(i + 1)).copied()
}

impl RunState {
    fn refresh(&mut self, dir: &Path) {
        if let Some(m) = read_meta(dir) {
            self.meta = m;
        }
        let t = now();
        for line in self.pgn_tail.read_new(&dir.join("games.pgn")) {
            self.pgn.feed(&line, &mut self.games);
        }
        for line in self.log_tail.read_new(&dir.join("engine.log")) {
            self.feed_log(&line, t);
        }
        for line in self.out_tail.read_new(&dir.join("output.txt")) {
            let l = line.trim();
            if l.starts_with("LLR:") {
                let nums: Vec<f64> = l
                    .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if nums.len() >= 3 {
                    self.llr = Some(nums);
                }
            }
            if l.contains("was accepted") {
                self.verdict = l.to_string();
            }
            if l.starts_with("Finished match") {
                self.finished = true;
            }
        }
        self.last_activity = ["games.pgn", "engine.log", "output.txt"].iter().map(|f| mtime(&dir.join(f))).max().unwrap_or(0);
    }

    fn feed_log(&mut self, line: &str, t: u64) {
        if !line.starts_with("[Engine]") {
            return;
        }
        let Some(a) = line.find('<') else { return };
        let Some(b) = line[a..].find('>') else { return };
        let Ok(slot_id) = line[a + 1..a + b].trim().parse::<u32>() else { return };
        let rest = &line[a + b + 1..];
        let (name, incoming, msg) = if let Some(i) = rest.find(" <--- ") {
            (rest[..i].trim(), true, &rest[i + 6..])
        } else if let Some(i) = rest.find(" ---> ") {
            (rest[..i].trim(), false, &rest[i + 6..])
        } else {
            return;
        };
        let slot = self.slots.entry(slot_id).or_default();
        slot.updated = t;
        if incoming {
            if msg == "ucinewgame" {
                if !slot.reset_pending {
                    *slot = Slot { reset_pending: true, updated: t, ..Default::default() };
                }
                if !slot.names.iter().any(|n| n == name) {
                    slot.names.push(name.to_string());
                }
            } else if msg.starts_with("position") {
                slot.reset_pending = false;
                slot.pos_cmd = msg.to_string();
                slot.to_move = name.to_string();
                slot.score = None;
                slot.depth = 0;
                if !slot.names.iter().any(|n| n == name) {
                    slot.names.push(name.to_string());
                }
            } else if msg.starts_with("go") {
                let tk: Vec<&str> = msg.split_whitespace().collect();
                slot.wtime = after(&tk, "wtime").and_then(|s| s.parse().ok()).unwrap_or(slot.wtime);
                slot.btime = after(&tk, "btime").and_then(|s| s.parse().ok()).unwrap_or(slot.btime);
            }
        } else if msg.starts_with("info") && name == slot.to_move && !msg.starts_with("info string") {
            let tk: Vec<&str> = msg.split_whitespace().collect();
            if let Some(d) = after(&tk, "depth").and_then(|s| s.parse().ok()) {
                slot.depth = d;
            }
            if let Some(n) = after(&tk, "nps").and_then(|s| s.parse().ok()) {
                slot.nps = n;
            }
            if let Some(i) = tk.iter().position(|&x| x == "score") {
                if let (Some(&kind), Some(v)) = (tk.get(i + 1), tk.get(i + 2).and_then(|s| s.parse().ok())) {
                    slot.mate = kind == "mate";
                    slot.score = Some(v);
                }
            }
        }
    }
}

fn erf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.3275911 * x.abs());
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592)
            * t
            * (-x * x).exp();
    if x >= 0.0 {
        y
    } else {
        -y
    }
}

fn elo_of(p: f64) -> f64 {
    let p = p.clamp(0.001, 0.999);
    -400.0 * (1.0 / p - 1.0).log10()
}

/// (elo, ±95% error, lower, upper, LOS, score fraction)
fn elo_stats(w: f64, d: f64, l: f64) -> (f64, f64, f64, f64, f64, f64) {
    let n = w + d + l;
    if n == 0.0 {
        return (0.0, 0.0, 0.0, 0.0, 0.5, 0.5);
    }
    let p = (w + d / 2.0) / n;
    let var = (w * (1.0 - p).powi(2) + d * (0.5 - p).powi(2) + l * p.powi(2)) / n;
    let se = (var / n).sqrt();
    let (lo, hi) = (elo_of(p - 1.96 * se), elo_of(p + 1.96 * se));
    let los = if w + l > 0.0 { 0.5 * (1.0 + erf((w - l) / (2.0 * (w + l)).sqrt())) } else { 0.5 };
    (elo_of(p), (hi - lo) / 2.0, lo, hi, los, p)
}

fn result_for(g: &Game, name: &str) -> Option<f64> {
    let white = match g.result.as_str() {
        "1-0" => 1.0,
        "0-1" => 0.0,
        "1/2-1/2" => 0.5,
        _ => return None,
    };
    if g.white == name {
        Some(white)
    } else if g.black == name {
        Some(1.0 - white)
    } else {
        None
    }
}

fn status(r: &RunState) -> &'static str {
    if r.finished {
        "finished"
    } else if now().saturating_sub(r.last_activity) < 90 {
        "running"
    } else {
        "stopped"
    }
}

struct Summary {
    w: f64,
    d: f64,
    l: f64,
}

/// The main engine's overall W/D/L plus W/D/L against each opponent.
fn summarize(r: &RunState) -> (Summary, BTreeMap<String, [f64; 3]>) {
    let main = &r.meta.main;
    let mut per: BTreeMap<String, [f64; 3]> = BTreeMap::new();
    for g in &r.games {
        let Some(s) = result_for(g, main) else { continue };
        let opp = if &g.white == main { g.black.clone() } else { g.white.clone() };
        let e = per.entry(opp).or_insert([0.0; 3]);
        e[if s == 1.0 { 0 } else if s == 0.5 { 1 } else { 2 }] += 1.0;
    }
    let (mut w, mut d, mut l) = (0.0, 0.0, 0.0);
    for c in per.values() {
        w += c[0];
        d += c[1];
        l += c[2];
    }
    (Summary { w, d, l }, per)
}

/// Performance rating with a 95% error margin: the rating R at which the expected score
/// against all rated opponents equals the actual score. Entries are (opponent rating, [w, d, l]).
fn perf_rating(opps: &[(f64, [f64; 3])]) -> Option<(f64, f64)> {
    let (w, d, l) = opps.iter().fold((0.0, 0.0, 0.0), |a, o| (a.0 + o.1[0], a.1 + o.1[1], a.2 + o.1[2]));
    let games = w + d + l;
    if games == 0.0 {
        return None;
    }
    let points = w + d / 2.0;
    let p = points / games;
    let sd = ((w * (1.0 - p).powi(2) + d * (0.5 - p).powi(2) + l * p.powi(2)) / games * games).sqrt().max(0.5);
    let hi_a = opps.iter().map(|o| o.0).fold(f64::MIN, f64::max);
    let lo_a = opps.iter().map(|o| o.0).fold(f64::MAX, f64::min);
    let solve = |pts: f64| {
        // Keep perfect scores half a point short so the estimate stays finite.
        let pts = pts.clamp(0.5, (games - 0.5).max(0.5));
        let expected = |r: f64| opps.iter().map(|o| (o.1[0] + o.1[1] + o.1[2]) / (1.0 + 10f64.powf((o.0 - r) / 400.0))).sum::<f64>();
        let (mut lo, mut hi) = (lo_a - 1500.0, hi_a + 1500.0);
        for _ in 0..60 {
            let mid = (lo + hi) / 2.0;
            if expected(mid) < pts {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) / 2.0
    };
    let r = solve(points);
    let err = (solve(points + 1.96 * sd) - solve(points - 1.96 * sd)) / 2.0;
    Some((r, err))
}

/// Rates every run in creation order. Opponents are rated either by a fixed anchor from meta.txt
/// (e.g. Elo-limited Stockfish) or by a rating an earlier run established for that engine, so
/// version-vs-version tests also move the rating.
fn compute_ratings(runs: &mut HashMap<String, RunState>, ids_by_age: &[String]) {
    let mut known: HashMap<String, (f64, f64)> = HashMap::new();
    for id in ids_by_age {
        let r = runs.get_mut(id).unwrap();
        let (_, per) = summarize(r);
        let mut rated = Vec::new();
        let mut anchor_err2 = 0.0;
        let mut games = 0.0;
        let mut derived = false;
        for (opp, c) in &per {
            let n = c[0] + c[1] + c[2];
            if let Some((_, a)) = r.meta.anchors.iter().find(|(name, _)| name == opp) {
                rated.push((*a, *c));
            } else if let Some(&(a, e)) = known.get(opp) {
                rated.push((a, *c));
                anchor_err2 += e * e * n;
                derived = true;
            } else {
                continue;
            }
            games += n;
        }
        r.rating = perf_rating(&rated).map(|(v, e)| {
            // Fold in the uncertainty of opponents whose ratings were themselves estimated.
            let e = if games > 0.0 { (e * e + anchor_err2 / games).sqrt() } else { e };
            (v, e, derived)
        });
        if let Some((v, e, _)) = r.rating {
            if !r.meta.main.is_empty() && r.finished {
                known.insert(r.meta.main.clone(), (v, e));
            }
        }
    }
}

fn run_brief(id: &str, r: &RunState) -> String {
    let (s, _) = summarize(r);
    let (elo, err, ..) = elo_stats(s.w, s.d, s.l);
    let llr = r.llr.as_ref().map_or("null".into(), |v| num(v[0]));
    let (perf, perf_err, derived) = match r.rating {
        Some((v, e, d)) => (num(v), num(e), d),
        None => ("null".into(), "null".into(), false),
    };
    format!(
        "{{\"id\":{},\"name\":{},\"type\":{},\"tc\":{},\"status\":{},\"games\":{},\"total\":{},\"created\":{},\"ended\":{},\"main\":{},\"elo\":{},\"err\":{},\"perf\":{},\"perf_err\":{},\"derived\":{},\"llr\":{},\"verdict\":{}}}",
        js(id),
        js(&r.meta.name),
        js(&r.meta.kind),
        js(&r.meta.tc),
        js(status(r)),
        r.games.len(),
        r.meta.total,
        r.meta.created,
        r.last_activity,
        js(&r.meta.main),
        num(elo),
        num(err),
        perf,
        perf_err,
        derived,
        llr,
        js(&r.verdict)
    )
}

fn replay(cmd: &str) -> Option<(Board, Option<Move>, usize)> {
    let tk: Vec<&str> = cmd.split_whitespace().collect();
    let moves_at = tk.iter().position(|&t| t == "moves");
    let mut b = match tk.get(1) {
        Some(&"startpos") => Board::startpos(),
        Some(&"fen") => Board::from_fen(&tk[2..moves_at.unwrap_or(tk.len())].join(" ")).ok()?,
        _ => return None,
    };
    let mut last = None;
    let mut n = 0;
    if let Some(i) = moves_at {
        for m in &tk[i + 1..] {
            let mv = b.parse_move(m)?;
            b.make_move(mv);
            last = Some(mv);
            n += 1;
        }
    }
    Some((b, last, n))
}

fn run_detail(id: &str, r: &RunState) -> String {
    let (s, per) = summarize(r);
    let main = &r.meta.main;
    let (elo, err, _, _, los, score) = elo_stats(s.w, s.d, s.l);
    let mut o = String::new();
    let _ = write!(o, "{{\"brief\":{},", run_brief(id, r));
    let _ = write!(
        o,
        "\"summary\":{{\"w\":{},\"d\":{},\"l\":{},\"elo\":{},\"err\":{},\"los\":{},\"score\":{}}},",
        s.w,
        s.d,
        s.l,
        num(elo),
        num(err),
        num(los),
        num(score)
    );
    let _ = write!(o, "\"concurrency\":{},\"notes\":{},", r.meta.concurrency, js(&r.meta.notes));
    let elapsed = if status(r) == "running" { now() } else { r.last_activity }.saturating_sub(r.meta.created);
    let _ = write!(o, "\"elapsed\":{},", elapsed);
    match &r.llr {
        Some(v) => {
            let _ = write!(
                o,
                "\"llr\":{{\"v\":{},\"lo\":{},\"hi\":{},\"elo0\":{},\"elo1\":{}}},",
                num(v[0]),
                num(v[1]),
                num(v[2]),
                v.get(3).map_or("null".into(), |x| num(*x)),
                v.get(4).map_or("null".into(), |x| num(*x))
            );
        }
        None => o.push_str("\"llr\":null,"),
    }

    o.push_str("\"opponents\":[");
    for (i, (opp, c)) in per.iter().enumerate() {
        let (e, er, _, _, _, sc) = elo_stats(c[0], c[1], c[2]);
        let anchor = r.meta.anchors.iter().find(|(n, _)| n == opp).map_or("null".into(), |(_, a)| num(*a));        let _ = write!(
            o,
            "{}{{\"name\":{},\"anchor\":{},\"w\":{},\"d\":{},\"l\":{},\"elo\":{},\"err\":{},\"score\":{}}}",
            if i > 0 { "," } else { "" },
            js(opp),
            anchor,
            c[0],
            c[1],
            c[2],
            num(e),
            num(er),
            num(sc)
        );
    }
    o.push_str("],");

    // Overall standings (useful for round robins).
    let mut table: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for g in &r.games {
        for name in [&g.white, &g.black] {
            if let Some(sc) = result_for(g, name) {
                let e = table.entry(name.clone()).or_insert((0.0, 0.0));
                e.0 += sc;
                e.1 += 1.0;
            }
        }
    }
    let mut standings: Vec<_> = table.into_iter().collect();
    standings.sort_by(|a, b| (b.1 .0 / b.1 .1.max(1.0)).total_cmp(&(a.1 .0 / a.1 .1.max(1.0))));
    o.push_str("\"standings\":[");
    for (i, (n, (pts, g))) in standings.iter().enumerate() {
        let _ = write!(o, "{}{{\"name\":{},\"points\":{},\"games\":{}}}", if i > 0 { "," } else { "" }, js(n), pts, g);
    }
    o.push_str("],");

    // Running Elo estimate for the main engine.
    let main_games: Vec<f64> = r.games.iter().filter_map(|g| result_for(g, main)).collect();
    let step = (main_games.len() / 150).max(1);
    let (mut w, mut d, mut l) = (0.0, 0.0, 0.0);
    o.push_str("\"series\":[");
    let mut first = true;
    for (i, &sc) in main_games.iter().enumerate() {
        if sc == 1.0 {
            w += 1.0
        } else if sc == 0.5 {
            d += 1.0
        } else {
            l += 1.0
        }
        if (i + 1) % step == 0 || i + 1 == main_games.len() {
            let (e, _, lo, hi, ..) = elo_stats(w, d, l);
            let _ = write!(o, "{}[{},{},{},{}]", if first { "" } else { "," }, i + 1, num(e), num(lo), num(hi));
            first = false;
        }
    }
    o.push_str("],");

    // Abnormal game endings.
    let mut problems: BTreeMap<(String, String), u32> = BTreeMap::new();
    for g in &r.games {
        let t = g.termination.as_str();
        if !t.is_empty() && t != "normal" && t != "adjudication" {
            let loser = match g.result.as_str() {
                "1-0" => g.black.clone(),
                "0-1" => g.white.clone(),
                _ => "-".into(),
            };
            *problems.entry((t.to_string(), loser)).or_default() += 1;
        }
    }
    o.push_str("\"problems\":[");
    for (i, ((t, loser), n)) in problems.iter().enumerate() {
        let _ = write!(o, "{}{{\"kind\":{},\"engine\":{},\"count\":{}}}", if i > 0 { "," } else { "" }, js(t), js(loser), n);
    }
    o.push_str("],");

    o.push_str("\"live\":[");
    let mut first = true;
    if !r.finished {
        let t = now();
        for (id, slot) in &r.slots {
            if t.saturating_sub(slot.updated) > STALE_SECS || slot.pos_cmd.is_empty() {
                continue;
            }
            let Some((b, last, plies)) = replay(&slot.pos_cmd) else { continue };
            let other = slot.names.iter().find(|n| **n != slot.to_move).cloned().unwrap_or_else(|| "?".into());
            let (white, black) =
                if b.stm == WHITE { (slot.to_move.clone(), other) } else { (other, slot.to_move.clone()) };
            let sign = if b.stm == WHITE { 1 } else { -1 };
            let (eval_text, eval_cp) = match slot.score {
                Some(v) if slot.mate => {
                    let v = v * sign;
                    (format!("{}M{}", if v > 0 { "+" } else { "-" }, v.abs()), if v > 0 { 3000 } else { -3000 })
                }
                Some(v) => {
                    let v = v * sign;
                    (format!("{:+.2}", v as f64 / 100.0), v)
                }
                None => ("…".into(), 0),
            };
            let last_json = last.map_or("null".into(), |m| format!("[{},{}]", js(&sq_name(m.from())), js(&sq_name(m.to()))));
            let _ = write!(
                o,
                "{}{{\"slot\":{},\"white\":{},\"black\":{},\"fen\":{},\"last\":{},\"thinking\":{},\"eval\":{},\"cp\":{},\"depth\":{},\"nps\":{},\"wtime\":{},\"btime\":{},\"plies\":{},\"move\":{}}}",
                if first { "" } else { "," },
                id,
                js(&white),
                js(&black),
                js(&b.to_fen()),
                last_json,
                js(&slot.to_move),
                js(&eval_text),
                eval_cp,
                slot.depth,
                slot.nps,
                slot.wtime,
                slot.btime,
                plies,
                b.fullmove
            );
            first = false;
        }
    }
    o.push_str("],");

    o.push_str("\"recent\":[");
    let n = r.games.len();
    for (i, g) in r.games.iter().enumerate().rev().take(25) {
        let _ = write!(
            o,
            "{}{{\"n\":{},\"white\":{},\"black\":{},\"result\":{},\"termination\":{},\"reason\":{},\"plies\":{},\"duration\":{}}}",
            if i + 1 == n { "" } else { "," },
            i + 1,
            js(&g.white),
            js(&g.black),
            js(&g.result),
            js(&g.termination),
            js(&g.reason),
            g.plies,
            js(&g.duration)
        );
    }
    o.push_str("]}");
    o
}

struct App {
    root: PathBuf,
    runs: Mutex<HashMap<String, RunState>>,
}

impl App {
    fn refresh_all(&self) -> Vec<String> {
        let dir = self.root.join("runs");
        let mut ids: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().join("meta.txt").exists())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        let mut runs = self.runs.lock().unwrap();
        runs.retain(|k, _| ids.contains(k));
        for id in &ids {
            runs.entry(id.clone()).or_default().refresh(&dir.join(id));
        }
        ids.sort_by_key(|id| runs[id].meta.created);
        compute_ratings(&mut runs, &ids);
        ids.reverse();
        ids
    }

    /// Milestones reached so far plus locked goals still ahead, as JSON.
    fn achievements(&self) -> String {
        let mut ids = self.refresh_all();
        ids.reverse(); // oldest first
        let runs = self.runs.lock().unwrap();
        struct Ach {
            icon: &'static str,
            title: String,
            desc: String,
            date: u64,
            run: String,
            tier: &'static str,
        }
        let mut got: Vec<Ach> = Vec::new();
        let mut have = std::collections::HashSet::new();
        let mut unlock = |key: String, a: Ach, got: &mut Vec<Ach>| {
            if have.insert(key) {
                got.push(a);
            }
        };

        let mut total_games = 0usize;
        let mut sprt_passes = 0;
        const RATING_STEPS: [f64; 9] = [2000.0, 2200.0, 2400.0, 2600.0, 2800.0, 3000.0, 3200.0, 3400.0, 3600.0];
        const GAME_STEPS: [usize; 4] = [100, 1000, 10_000, 100_000];
        for id in &ids {
            let r = &runs[id];
            let main = &r.meta.main;
            let date = if r.last_activity > 0 { r.last_activity } else { r.meta.created };
            let finished = r.finished || status(r) == "stopped";

            // First game won against each opponent (counts as soon as it happens, even mid-tournament).
            for g in &r.games {
                if result_for(g, main) == Some(1.0) {
                    let opp = if &g.white == main { &g.black } else { &g.white };
                    let strong = ["stockfish", "dragon", "plentychess", "rubichess", "lc0"].contains(&opp.to_lowercase().as_str());
                    unlock(
                        format!("win:{opp}"),
                        Ach {
                            icon: if strong { "🏆" } else { "⚔️" },
                            title: format!("First win vs {opp}"),
                            desc: format!("{main} won a game against {opp}{}", g.reason.is_empty().then(String::new).unwrap_or_else(|| format!(" ({})", g.reason))),
                            date,
                            run: r.meta.name.clone(),
                            tier: if strong { "gold" } else { "bronze" },
                        },
                        &mut got,
                    );
                }
            }

            // Outscored an opponent over a whole finished match.
            if finished {
                let (_, per) = summarize(r);
                for (opp, c) in &per {
                    let n = c[0] + c[1] + c[2];
                    let score = (c[0] + c[1] / 2.0) / n.max(1.0);
                    if n >= 10.0 && score > 0.5 {
                        unlock(
                            format!("match:{opp}"),
                            Ach {
                                icon: "🥇",
                                title: format!("Beat {opp} in a match"),
                                desc: format!("Scored {:.0}% over {} games (+{} ={} −{})", score * 100.0, n, c[0], c[1], c[2]),
                                date,
                                run: r.meta.name.clone(),
                                tier: "silver",
                            },
                            &mut got,
                        );
                    }
                }
                if let Some((v, e, _)) = r.rating {
                    for step in RATING_STEPS {
                        if v >= step {
                            unlock(
                                format!("rating:{step}"),
                                Ach {
                                    icon: "📈",
                                    title: format!("Reached {step:.0} rating"),
                                    desc: format!("{main} measured at {v:.0} ±{e:.0}"),
                                    date,
                                    run: r.meta.name.clone(),
                                    tier: if step >= 3000.0 { "gold" } else { "silver" },
                                },
                                &mut got,
                            );
                        }
                    }
                }
            }

            if r.verdict.contains("H1") {
                sprt_passes += 1;
                if sprt_passes == 1 || sprt_passes == 10 || sprt_passes == 50 {
                    unlock(
                        format!("sprt:{sprt_passes}"),
                        Ach {
                            icon: "✅",
                            title: if sprt_passes == 1 { "First improvement proven".into() } else { format!("{sprt_passes} improvements proven") },
                            desc: format!("Passed SPRT: {}", r.meta.name),
                            date,
                            run: r.meta.name.clone(),
                            tier: "silver",
                        },
                        &mut got,
                    );
                }
            }

            let before = total_games;
            total_games += r.games.iter().filter(|g| result_for(g, main).is_some()).count();
            for step in GAME_STEPS {
                if before < step && total_games >= step {
                    unlock(
                        format!("games:{step}"),
                        Ach {
                            icon: "♟️",
                            title: format!("{step} games played"),
                            desc: "Across all tests".into(),
                            date,
                            run: r.meta.name.clone(),
                            tier: "bronze",
                        },
                        &mut got,
                    );
                }
            }
        }

        // Goals still ahead.
        let mut locked: Vec<(String, String, String)> = Vec::new();
        for step in RATING_STEPS {
            if !got.iter().any(|a| a.title == format!("Reached {step:.0} rating")) {
                locked.push(("📈".into(), format!("Reach {step:.0} rating"), "Confirmed by a finished rating test".into()));
            }
        }
        for opp in ["stockfish", "dragon", "plentychess", "rubichess"] {
            if !got.iter().any(|a| a.title == format!("First win vs {opp}")) {
                locked.push(("🏆".into(), format!("Win a game vs {opp}"), "Full-strength engine".into()));
            }
        }
        for step in GAME_STEPS {
            if total_games < step {
                locked.push(("♟️".into(), format!("{step} games played"), format!("{total_games} so far")));
            }
        }

        got.sort_by_key(|a| std::cmp::Reverse(a.date));
        let mut o = String::from("{\"unlocked\":[");
        for (i, a) in got.iter().enumerate() {
            let _ = write!(
                o,
                "{}{{\"icon\":{},\"title\":{},\"desc\":{},\"date\":{},\"run\":{},\"tier\":{}}}",
                if i > 0 { "," } else { "" },
                js(a.icon),
                js(&a.title),
                js(&a.desc),
                a.date,
                js(&a.run),
                js(a.tier)
            );
        }
        o.push_str("],\"locked\":[");
        for (i, (icon, t, d)) in locked.iter().enumerate() {
            let _ = write!(o, "{}{{\"icon\":{},\"title\":{},\"desc\":{}}}", if i > 0 { "," } else { "" }, js(icon), js(t), js(d));
        }
        o.push_str("]}");
        o
    }

    fn handle(&self, path: &str) -> (&'static str, String, &'static str) {
        let path = path.split('?').next().unwrap_or("/");
        if path == "/" || path == "/index.html" {
            return ("200 OK", INDEX.to_string(), "text/html; charset=utf-8");
        }
        if path == "/api/runs" {
            let ids = self.refresh_all();
            let runs = self.runs.lock().unwrap();
            let items: Vec<String> = ids.iter().map(|id| run_brief(id, &runs[id])).collect();
            return ("200 OK", format!("[{}]", items.join(",")), "application/json");
        }
        if path == "/api/achievements" {
            return ("200 OK", self.achievements(), "application/json");
        }
        if let Some(id) = path.strip_prefix("/api/run/") {
            let id = id.replace("%20", " ");
            self.refresh_all();
            let runs = self.runs.lock().unwrap();
            if let Some(r) = runs.get(&id) {
                return ("200 OK", run_detail(&id, r), "application/json");
            }
        }
        ("404 Not Found", "not found".into(), "text/plain")
    }
}

fn serve(app: &App, mut stream: TcpStream) {
    let mut buf = vec![0u8; 8192];
    let mut len = 0;
    loop {
        match stream.read(&mut buf[len..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                len += n;
                if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") || len == buf.len() {
                    break;
                }
            }
        }
    }
    let req = String::from_utf8_lossy(&buf[..len]);
    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
    let (status, body, ctype) = app.handle(&path);
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn main() {
    attacks::init();
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(args.get(1).cloned().unwrap_or_else(|| "C:\\Users\\you\\Claude\\testing".into()));
    let port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8000);
    let _ = std::fs::create_dir_all(root.join("runs"));
    let app = Arc::new(App { root, runs: Mutex::new(HashMap::new()) });
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("cannot bind port");
    println!("Pawnstorm Lab running at http://localhost:{port}");
    for stream in listener.incoming().flatten() {
        let app = app.clone();
        std::thread::spawn(move || serve(&app, stream));
    }
}
