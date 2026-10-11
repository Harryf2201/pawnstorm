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
use std::process::Stdio;
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

/// "LLR: -0.00 (-0.1%) (-2.94, 2.94) [0.00, 5.00]" -> [llr, lower bound, upper bound, elo0, elo1].
/// The "(-0.1%)" progress figure is skipped, so it can't be mistaken for the lower bound.
fn parse_llr(line: &str) -> Option<Vec<f64>> {
    let mut rest = line.trim().strip_prefix("LLR:")?.to_string();
    // Remove every "( ... % )" group.
    let mut from = 0;
    while let Some(a) = rest[from..].find('(').map(|i| i + from) {
        let Some(b) = rest[a..].find(')') else { break };
        if rest[a..a + b].contains('%') {
            rest.replace_range(a..a + b + 1, " ");
            from = a;
        } else {
            from = a + b + 1;
        }
    }
    let nums: Vec<f64> = rest
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .filter_map(|s| s.parse().ok())
        .collect();
    (nums.len() >= 3 && nums[1] < nums[2]).then_some(nums)
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
            if let Some(nums) = parse_llr(l) {
                self.llr = Some(nums);
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

/// "Pawnstorm-0.1" and "pawnstorm-v0.1" are the same engine; "pawnstorm-v0.3-10m" stays distinct.
fn engine_key(name: &str) -> String {
    let s: String = name.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.').collect();
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in b.iter().enumerate() {
        // drop a "v" sitting between a letter and a digit ("pawnstormv0.1" -> "pawnstorm0.1")
        if c == b'v' && i > 0 && !b[i - 1].is_ascii_digit() && b.get(i + 1).is_some_and(|n| n.is_ascii_digit()) {
            continue;
        }
        out.push(c as char);
    }
    out
}

/// "net-v0.8.1-316m" -> 316_000_000 (the position count tools/train-version.ps1 puts in the name).
fn positions_from_name(name: &str) -> u64 {
    name.split('-')
        .rev()
        .find_map(|seg| {
            let (n, mult) = match (seg.strip_suffix('m'), seg.strip_suffix('b')) {
                (Some(n), _) => (n, 1_000_000),
                (_, Some(n)) => (n, 1_000_000_000),
                _ => return None,
            };
            n.parse::<u64>().ok().map(|n| n * mult)
        })
        .unwrap_or(0)
}

/// One section of versions.ini: a named engine version and every engine name it has used.
#[derive(Default, Clone)]
struct Version {
    name: String,
    engines: Vec<String>,
    status: String,
    date: String,
    parent: String,
    branch: String,
    network: String,
    data: String,
    summary: String,
}

/// `[section]` + `key = value` lines; `;` and `#` start comments. Sections keep file order.
fn parse_versions(text: &str) -> Vec<Version> {
    let mut out: Vec<Version> = Vec::new();
    for line in text.lines() {
        let l = line.trim_start_matches('\u{feff}').trim();
        if l.is_empty() || l.starts_with(';') || l.starts_with('#') {
            continue;
        }
        if let Some(name) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            out.push(Version { name: name.trim().to_string(), ..Default::default() });
            continue;
        }
        let (Some(v), Some((k, val))) = (out.last_mut(), l.split_once('=')) else { continue };
        let val = val.trim().to_string();
        match k.trim().to_lowercase().as_str() {
            "engines" => v.engines = val.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            "status" => v.status = val,
            "date" => v.date = val,
            "parent" => v.parent = val,
            "branch" => v.branch = val,
            "network" => v.network = val,
            "data" => v.data = val,
            "summary" => v.summary = val,
            _ => {}
        }
    }
    out
}

/// engine_key(any engine name of a version, or the version's own name) -> version name.
fn alias_map(versions: &[Version]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for v in versions {
        m.insert(engine_key(&v.name), v.name.clone());
        for e in &v.engines {
            m.insert(engine_key(e), v.name.clone());
        }
    }
    m
}

/// Identity of an engine for rating purposes: all names of one catalogued version are one player.
fn canon_key(alias: &HashMap<String, String>, n: &str) -> String {
    let k = engine_key(n);
    alias.get(&k).map_or(k, |v| format!("@{v}"))
}

struct Rated {
    name: String,
    rating: f64,
    err: f64,
    games: f64,
    first_seen: u64,
    live: bool,
    /// Record against every opponent over all runs: (name, wins, draws, losses, anchor rating).
    vs: Vec<(String, f64, f64, f64, Option<f64>)>,
}

/// One joint maximum-likelihood fit over every game in every run. Anchor engines (Elo-limited
/// Stockfish) keep their fixed ratings; everyone else is solved together, so new games against a
/// version refine its rating retroactively and version-vs-version games count for both sides.
fn global_ratings(runs: &HashMap<String, RunState>, alias: &HashMap<String, String>) -> Vec<Rated> {
    let mut idx: HashMap<String, usize> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    let mut anchor: Vec<Option<f64>> = Vec::new();
    let mut first_seen: Vec<u64> = Vec::new();
    let mut live: Vec<bool> = Vec::new();
    let id_of = |n: &str, t: u64, running: bool, idx: &mut HashMap<String, usize>, names: &mut Vec<String>, anchor: &mut Vec<Option<f64>>, first_seen: &mut Vec<u64>, live: &mut Vec<bool>| {
        let i = *idx.entry(canon_key(alias, n)).or_insert_with(|| {
            // Catalogued engines are shown by their version name.
            names.push(alias.get(&engine_key(n)).cloned().unwrap_or_else(|| n.to_string()));
            anchor.push(None);
            first_seen.push(t);
            live.push(false);
            names.len() - 1
        });
        first_seen[i] = first_seen[i].min(t);
        live[i] |= running;
        i
    };
    // pair (i, j) with i < j -> [wins for i, draws, wins for j]
    let mut pairs: HashMap<(usize, usize), [f64; 3]> = HashMap::new();
    for r in runs.values() {
        let running = status(r) == "running";
        // Only the engine under test counts as live, not the opponents it is playing.
        let main_key = canon_key(alias, &r.meta.main);
        let t = r.meta.created;
        for (n, a) in &r.meta.anchors {
            let i = id_of(n, t, false, &mut idx, &mut names, &mut anchor, &mut first_seen, &mut live);
            anchor[i] = Some(*a);
        }
        for g in &r.games {
            let s = match g.result.as_str() {
                "1-0" => 1.0,
                "0-1" => 0.0,
                "1/2-1/2" => 0.5,
                _ => continue,
            };
            let w = id_of(&g.white, t, running && canon_key(alias, &g.white) == main_key, &mut idx, &mut names, &mut anchor, &mut first_seen, &mut live);
            let b = id_of(&g.black, t, running && canon_key(alias, &g.black) == main_key, &mut idx, &mut names, &mut anchor, &mut first_seen, &mut live);
            if w == b {
                continue;
            }
            let (i, j, si) = if w < b { (w, b, s) } else { (b, w, 1.0 - s) };
            let e = pairs.entry((i, j)).or_insert([0.0; 3]);
            e[if si == 1.0 { 0 } else if si == 0.5 { 1 } else { 2 }] += 1.0;
        }
    }
    let n = names.len();
    let mut rating: Vec<f64> = anchor.iter().map(|a| a.unwrap_or(2500.0)).collect();
    let k = 400.0 / std::f64::consts::LN_10;
    let expected = |a: f64, b: f64| 1.0 / (1.0 + 10f64.powf((b - a) / 400.0));
    let mut info = vec![0.0; n];
    let mut games = vec![0.0; n];
    for _ in 0..2000 {
        let mut grad = vec![0.0; n];
        info.iter_mut().for_each(|x| *x = 0.0);
        for (&(i, j), c) in &pairs {
            // One virtual draw per pairing keeps 100% scores finite.
            let games_ij = c[0] + c[1] + c[2] + 1.0;
            let si = c[0] + c[1] / 2.0 + 0.5;
            let e = expected(rating[i], rating[j]);
            grad[i] += si - games_ij * e;
            grad[j] -= si - games_ij * e;
            info[i] += games_ij * e * (1.0 - e);
            info[j] += games_ij * e * (1.0 - e);
        }
        let mut moved = 0.0f64;
        for i in 0..n {
            if anchor[i].is_none() && info[i] > 0.0 {
                let step = (k * grad[i] / info[i]).clamp(-100.0, 100.0) * 0.5;
                rating[i] += step;
                moved = moved.max(step.abs());
            }
        }
        if moved < 0.01 {
            break;
        }
    }
    let mut vs: Vec<Vec<(String, f64, f64, f64, Option<f64>)>> = vec![Vec::new(); n];
    for (&(i, j), c) in &pairs {
        games[i] += c[0] + c[1] + c[2];
        games[j] += c[0] + c[1] + c[2];
        vs[i].push((names[j].clone(), c[0], c[1], c[2], anchor[j]));
        vs[j].push((names[i].clone(), c[2], c[1], c[0], anchor[i]));
    }
    (0..n)
        .filter(|&i| anchor[i].is_none() && games[i] > 0.0)
        .map(|i| Rated {
            name: names[i].clone(),
            rating: rating[i],
            err: if info[i] > 0.0 { 1.96 * k / info[i].sqrt() } else { 999.0 },
            games: games[i],
            first_seen: first_seen[i],
            live: live[i],
            vs: std::mem::take(&mut vs[i]),
        })
        .collect()
}

/// Rates every run in creation order. Opponents are rated either by a fixed anchor from meta.txt
/// (e.g. Elo-limited Stockfish) or by a rating an earlier run established for that engine, so
/// version-vs-version tests also move the rating.
fn compute_ratings(runs: &mut HashMap<String, RunState>, ids_by_age: &[String], alias: &HashMap<String, String>) {
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
            } else if let Some(&(a, e)) = known.get(&canon_key(alias, opp)) {
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
                known.insert(canon_key(alias, &r.meta.main), (v, e));
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

/// For round robins: every pairing's W/D/L ("cross") and ratings fitted from this run's games alone,
/// with its Stockfish anchors fixed ("fit"). Same maximum-likelihood fit as the overall ratings,
/// including one virtual draw per pairing so 100% scores stay finite.
fn cross_and_fit(r: &RunState) -> String {
    let mut names: Vec<String> = Vec::new();
    let mut pairs: BTreeMap<(usize, usize), [f64; 3]> = BTreeMap::new();
    let idx = |n: &str, names: &mut Vec<String>| match names.iter().position(|x| x == n) {
        Some(i) => i,
        None => {
            names.push(n.to_string());
            names.len() - 1
        }
    };
    for g in &r.games {
        let Some(s) = result_for(g, &g.white) else { continue };
        let (w, b) = (idx(&g.white, &mut names), idx(&g.black, &mut names));
        if w == b {
            continue;
        }
        let (i, j, si) = if w < b { (w, b, s) } else { (b, w, 1.0 - s) };
        let e = pairs.entry((i, j)).or_insert([0.0; 3]);
        e[if si == 1.0 { 0 } else if si == 0.5 { 1 } else { 2 }] += 1.0;
    }
    let n = names.len();
    let anchor: Vec<Option<f64>> = names.iter().map(|x| r.meta.anchors.iter().find(|(a, _)| a == x).map(|(_, v)| *v)).collect();
    let mut rating: Vec<f64> = anchor.iter().map(|a| a.unwrap_or(3000.0)).collect();
    let k = 400.0 / std::f64::consts::LN_10;
    let expected = |a: f64, b: f64| 1.0 / (1.0 + 10f64.powf((b - a) / 400.0));
    let mut info = vec![0.0; n];
    for _ in 0..3000 {
        let mut grad = vec![0.0; n];
        info.iter_mut().for_each(|x| *x = 0.0);
        for (&(i, j), c) in &pairs {
            let games = c[0] + c[1] + c[2] + 1.0;
            let si = c[0] + c[1] / 2.0 + 0.5;
            let e = expected(rating[i], rating[j]);
            grad[i] += si - games * e;
            grad[j] -= si - games * e;
            info[i] += games * e * (1.0 - e);
            info[j] += games * e * (1.0 - e);
        }
        let mut moved = 0.0f64;
        for i in 0..n {
            if anchor[i].is_none() && info[i] > 0.0 {
                let step = (k * grad[i] / info[i]).clamp(-100.0, 100.0) * 0.5;
                rating[i] += step;
                moved = moved.max(step.abs());
            }
        }
        if moved < 0.01 {
            break;
        }
    }
    let mut o = String::from("\"cross\":[");
    for (c, (&(i, j), v)) in pairs.iter().enumerate() {
        let _ = write!(o, "{}[{},{},{},{},{}]", if c > 0 { "," } else { "" }, js(&names[i]), js(&names[j]), v[0], v[1], v[2]);
    }
    o.push_str("],\"fit\":[");
    for i in 0..n {
        let err = if anchor[i].is_some() { 0.0 } else if info[i] > 0.0 { 1.96 * k / info[i].sqrt() } else { 999.0 };
        let _ = write!(
            o,
            "{}{{\"name\":{},\"rating\":{},\"err\":{},\"anchor\":{}}}",
            if i > 0 { "," } else { "" },
            js(&names[i]),
            num(rating[i]),
            num(err),
            anchor[i].is_some()
        );
    }
    o.push_str("],");
    o
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
    o.push_str(&cross_and_fit(r));

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
    /// lab-start / lab-stop script launched from the dashboard, while it runs.
    lab_child: Mutex<Option<(String, std::process::Child)>>,
    /// Where versions.ini may live (repo root next to the exe, or the working directory); first existing wins.
    version_files: Vec<PathBuf>,
}

impl App {
    /// The version catalog, re-read on every call so edits show up immediately.
    fn versions(&self) -> Vec<Version> {
        self.version_files
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
            .map(|t| parse_versions(&t))
            .unwrap_or_default()
    }

    fn versions_json(&self) -> String {
        let items: Vec<String> = self
            .versions()
            .iter()
            .map(|v| {
                format!(
                    "{{\"name\":{},\"engines\":[{}],\"status\":{},\"date\":{},\"parent\":{},\"branch\":{},\"network\":{},\"data\":{},\"summary\":{}}}",
                    js(&v.name),
                    v.engines.iter().map(|e| js(e)).collect::<Vec<_>>().join(","),
                    js(&v.status),
                    js(&v.date),
                    js(&v.parent),
                    js(&v.branch),
                    js(&v.network),
                    js(&v.data),
                    js(&v.summary)
                )
            })
            .collect();
        format!("[{}]", items.join(","))
    }

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
        compute_ratings(&mut runs, &ids, &alias_map(&self.versions()));
        ids.reverse();
        ids
    }

    /// Self-play data generation runs: `<root>/datagen/<name>/status.txt` plus the size of the data written.
    fn datagen(&self) -> String {
        let dir = self.root.join("datagen");
        let mut items = Vec::new();
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            let Ok(text) = std::fs::read_to_string(p.join("status.txt")) else { continue };
            let mut fields = Vec::new();
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    let v = v.trim();
                    let val = if v.parse::<f64>().is_ok() { v.to_string() } else { js(v) };
                    fields.push(format!("{}:{}", js(k.trim()), val));
                }
            }
            let bytes: u64 = std::fs::read_dir(&p)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|f| f.file_name().to_string_lossy().starts_with("data_"))
                // Ask the file itself: directory listings cache sizes of files still being written.
                .filter_map(|f| std::fs::metadata(f.path()).ok())
                .map(|m| m.len())
                .sum();
            fields.push(format!("\"bytes\":{bytes}"));
            if let Ok(stop) = std::fs::read_to_string(dir.join("cloud-STOPPED.txt")) {
                fields.push(format!("\"cloud_stopped\":{}", js(stop.trim())));
            }
            fields.push(format!("\"name\":{}", js(&e.file_name().to_string_lossy())));
            fields.push(format!("\"now\":{}", now()));
            items.push(format!("{{{}}}", fields.join(",")));
        }
        format!("[{}]", items.join(","))
    }

    /// Watchdog state: latest readings, the last hour of temperatures, and recent events.
    fn health(&self) -> String {
        let dir = self.root.join("health");
        let mut fields = Vec::new();
        for line in std::fs::read_to_string(dir.join("status.txt")).unwrap_or_default().lines() {
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim();
                let val = if !v.is_empty() && v.parse::<f64>().is_ok() { v.to_string() } else { js(v) };
                fields.push(format!("{}:{}", js(k.trim()), val));
            }
        }
        fields.push(format!("\"now\":{}", now()));
        let history = std::fs::read_to_string(dir.join("history.csv")).unwrap_or_default();
        let rows: Vec<String> = history
            .lines()
            .rev()
            .take(720)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|l| format!("[{}]", l.split(',').map(|x| if x.trim().parse::<f64>().is_ok() { x.trim() } else { "null" }).collect::<Vec<_>>().join(",")))
            .collect();
        fields.push(format!("\"history\":[{}]", rows.join(",")));
        let events = std::fs::read_to_string(dir.join("events.log")).unwrap_or_default();
        let ev: Vec<String> = events.lines().rev().take(12).map(js).collect();
        fields.push(format!("\"events\":[{}]", ev.join(",")));
        format!("{{{}}}", fields.join(","))
    }

    /// Network training runs: `<root>/training/<name>/log.txt` is the trainer's stdout.
    fn training(&self) -> String {
        let dir = self.root.join("training");
        let mut items = Vec::new();
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            let log_path = p.join("log.txt");
            let Ok(raw) = std::fs::read_to_string(&log_path) else { continue };
            // Strip ANSI colour/cursor codes; progress updates are separated by cursor-up codes.
            let mut text = String::with_capacity(raw.len());
            let mut chars = raw.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\x1b' {
                    if chars.peek() == Some(&'[') {
                        chars.next();
                        while let Some(&n) = chars.peek() {
                            chars.next();
                            if n.is_ascii_alphabetic() {
                                // Colour codes (…m) vanish; cursor moves separate progress updates.
                                if n != 'm' {
                                    text.push('\n');
                                }
                                break;
                            }
                        }
                    }
                } else {
                    text.push(c);
                }
            }
            let field = |key: &str| {
                text.lines().find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().trim_start_matches(':').trim().to_string()))
            };
            let end_sb: u64 = field("End Superbatch").and_then(|v| v.parse().ok()).unwrap_or(0);
            let device = text.lines().find_map(|l| l.trim().strip_prefix("Training on ").map(str::to_string)).unwrap_or_default();
            let mut losses = Vec::new();
            let (mut cur_sb, mut pct, mut pos_s, mut total_time) = (0u64, 0.0f64, 0.0f64, 0.0f64);
            let nums = |s: &str| -> Vec<f64> {
                s.split(|c: char| !(c.is_ascii_digit() || c == '.'))
                    .filter_map(|x| x.parse().ok())
                    .collect()
            };
            for l in text.lines().map(str::trim) {
                if !l.starts_with("superbatch ") {
                    continue;
                }
                let n = nums(l);
                if l.contains("running loss") && n.len() >= 5 {
                    // superbatch N | time T s | running loss L | P pos/sec | total time T s
                    losses.push(format!("[{},{}]", n[0], n[2]));
                    cur_sb = n[0] as u64;
                    pct = 100.0;
                    pos_s = n[3];
                    total_time = n[4];
                } else if l.contains('[') && n.len() >= 5 {
                    // superbatch N [pct% (b/B batches, P pos/sec)]
                    cur_sb = n[0] as u64;
                    pct = n[1];
                    pos_s = n[4];
                }
            }
            let finished = text.contains("Total Training Time");
            let errors = std::fs::read_to_string(p.join("err.txt")).unwrap_or_default();
            let failed = errors.contains("panicked");
            let updated = mtime(&log_path).max(mtime(&p.join("err.txt")));
            let state = if failed {
                "failed"
            } else if finished {
                "finished"
            } else if now().saturating_sub(updated) < 60 {
                "running"
            } else {
                "stopped"
            };
            let started = std::fs::metadata(&log_path)
                .and_then(|m| m.created())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let err_line = errors.lines().find(|l| l.contains("Err") || l.contains("error")).unwrap_or("").to_string();
            items.push(format!(
                "{{\"name\":{},\"net\":{},\"device\":{},\"state\":{},\"end_sb\":{},\"sb\":{},\"pct\":{},\"pos_s\":{},\"total_time\":{},\"started\":{},\"updated\":{},\"now\":{},\"losses\":[{}],\"error\":{}}}",
                js(&e.file_name().to_string_lossy()),
                js(&field("Net Name").unwrap_or_default()),
                js(&device),
                js(state),
                end_sb,
                cur_sb,
                num(pct),
                num(pos_s),
                num(total_time),
                started,
                updated,
                now(),
                losses.join(","),
                js(&err_line)
            ));
        }
        format!("[{}]", items.join(","))
    }

    /// Training data being prepared by tools/prepare-data.ps1 before the trainer starts:
    /// `<name>.txt` (snapshot) -> `<name>.raw.bin` (convert) -> `<name>.bin` (shuffle). The text and raw
    /// files only exist while this runs, so their presence means "still preparing".
    fn prepare(&self) -> String {
        let dir = self.root.join("training").join("data");
        let mut names: Vec<String> = Vec::new();
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let f = e.file_name().to_string_lossy().to_string();
            if let Some(b) = f.strip_suffix(".raw.bin").or_else(|| f.strip_suffix(".txt")) {
                if !names.iter().any(|n| n == b) {
                    names.push(b.to_string());
                }
            }
        }
        names.sort();
        let items: Vec<String> = names
            .iter()
            .map(|name| {
                let path = |ext: &str| dir.join(format!("{name}{ext}"));
                let size = |ext: &str| std::fs::metadata(path(ext)).map(|m| m.len()).unwrap_or(0);
                let (txt, raw, bin) = (size(".txt"), size(".raw.bin"), size(".bin"));
                // The shuffle first writes shuffled parts into `<name>.bin.tmp/`, then merges them into `<name>.bin`.
                let tmp_dir = path(".bin.tmp");
                let tmp_files: Vec<std::fs::Metadata> =
                    std::fs::read_dir(&tmp_dir).into_iter().flatten().flatten().filter_map(|f| f.metadata().ok()).collect();
                let tmp_bytes: u64 = tmp_files.iter().map(|m| m.len()).sum();
                let (step, stage, bytes) = if path(".bin").exists() || tmp_dir.exists() {
                    (3, "shuffle", tmp_bytes + bin)
                } else if raw > 0 {
                    (2, "convert", raw)
                } else {
                    (1, "snapshot", txt)
                };
                let positions = positions_from_name(name);
                // Converted files take 32 bytes per position (the shuffle writes everything twice); text lines
                // are measured from the start of the file.
                let total = if positions == 0 {
                    0
                } else if step == 3 {
                    positions * 64
                } else if step == 2 {
                    positions * 32
                } else {
                    let mut buf = vec![0u8; 1 << 20];
                    let n = File::open(path(".txt")).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
                    let lines = buf[..n].iter().filter(|&&c| c == b'\n').count() as u64;
                    if lines > 0 { positions * (n as u64 / lines) } else { 0 }
                };
                let tmp_mtime = std::fs::read_dir(&tmp_dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|f| mtime(&f.path()))
                    .max()
                    .unwrap_or(0);
                let updated = [".txt", ".raw.bin", ".bin"].iter().map(|e| mtime(&path(e))).max().unwrap_or(0).max(tmp_mtime);
                format!(
                    "{{\"name\":{},\"stage\":{},\"step\":{},\"bytes\":{},\"total\":{},\"positions\":{},\"updated\":{},\"now\":{}}}",
                    js(name),
                    js(stage),
                    step,
                    bytes,
                    total,
                    positions,
                    updated,
                    now()
                )
            })
            .collect();
        format!("[{}]", items.join(","))
    }

    /// Every achievement, unlocked or not, with a category and a difficulty key so the page
    /// can show each category as a ladder from easiest to hardest.
    fn achievements(&self) -> String {
        let mut ids = self.refresh_all();
        ids.reverse(); // oldest first
        let runs = self.runs.lock().unwrap();

        struct Ach {
            cat: &'static str,
            key: f64,
            icon: &'static str,
            title: String,
            desc: String,
            date: Option<u64>,
            run: String,
            /// The engine version that earned it (empty while locked).
            main: String,
            tier: &'static str,
        }
        // Keyed by an id so the first (earliest) unlock wins.
        let mut got: BTreeMap<String, Ach> = BTreeMap::new();

        const RATING_STEPS: [f64; 9] = [2000.0, 2200.0, 2400.0, 2600.0, 2800.0, 3000.0, 3200.0, 3400.0, 3600.0];
        const GAME_STEPS: [usize; 4] = [100, 1000, 10_000, 100_000];
        const SPRT_STEPS: [usize; 3] = [1, 10, 50];
        // Opponents worth showing as goals even before we've played them, weakest first.
        const LADDER: [&str; 8] = ["SF-2000", "SF-2400", "SF-2800", "SF-3000", "rubichess", "dragon", "plentychess", "stockfish"];
        let full_strength = |name: &str| match name.to_lowercase().as_str() {
            "rubichess" => Some(3400.0),
            "dragon" => Some(3550.0),
            "plentychess" => Some(3600.0),
            "lc0" => Some(3600.0),
            "stockfish" => Some(3650.0),
            _ => None,
        };
        let strength = |name: &str| {
            full_strength(name).or_else(|| name.strip_prefix("SF-").and_then(|e| e.parse().ok())).unwrap_or(0.0)
        };
        let is_ours = |name: &str| name.to_lowercase().starts_with("pawnstorm");

        let mut total_games = 0usize;
        let mut sprt_passes = 0usize;
        for id in &ids {
            let r = &runs[id];
            let main = &r.meta.main;
            let date = Some(if r.last_activity > 0 { r.last_activity } else { r.meta.created });
            let finished = r.finished || status(r) == "stopped";

            // First game won against each opponent (counts the moment it happens).
            for g in &r.games {
                if result_for(g, main) != Some(1.0) {
                    continue;
                }
                let opp = if &g.white == main { &g.black } else { &g.white };
                if is_ours(opp) {
                    continue;
                }
                let strong = full_strength(opp).is_some();
                got.entry(format!("win:{opp}")).or_insert(Ach {
                    cat: "wins",
                    key: strength(opp),
                    icon: if strong { "🏆" } else { "⚔️" },
                    title: format!("First win vs {opp}"),
                    desc: if g.reason.is_empty() { format!("{main} won a game") } else { format!("{main} won a game ({})", g.reason) },
                    date,
                    run: r.meta.name.clone(),
                    main: main.clone(),
                    tier: if strong { "gold" } else { "bronze" },
                });
            }

            if finished {
                // Outscored an opponent over a whole match.
                let (_, per) = summarize(r);
                for (opp, c) in &per {
                    let n = c[0] + c[1] + c[2];
                    let score = (c[0] + c[1] / 2.0) / n.max(1.0);
                    if n >= 10.0 && score > 0.5 && !is_ours(opp) {
                        got.entry(format!("match:{opp}")).or_insert(Ach {
                            cat: "matches",
                            key: strength(opp),
                            icon: if full_strength(opp).is_some() { "👑" } else { "🥇" },
                            title: format!("Beat {opp} in a match"),
                            desc: format!("Scored {:.0}% over {} games (+{} ={} −{})", score * 100.0, n, c[0], c[1], c[2]),
                            date,
                            run: r.meta.name.clone(),
                            main: main.clone(),
                            tier: if full_strength(opp).is_some() { "gold" } else { "silver" },
                        });
                    }
                }
                if let Some((v, e, _)) = r.rating {
                    for step in RATING_STEPS.iter().filter(|&&s| v >= s) {
                        got.entry(format!("rating:{step}")).or_insert(Ach {
                            cat: "rating",
                            key: *step,
                            icon: "📈",
                            title: format!("Reached {step:.0} rating"),
                            desc: format!("{main} measured at {v:.0} ±{e:.0}"),
                            date,
                            run: r.meta.name.clone(),
                            main: main.clone(),
                            tier: if *step >= 3000.0 { "gold" } else { "silver" },
                        });
                    }
                }
            }

            if r.verdict.contains("H1") {
                sprt_passes += 1;
                if SPRT_STEPS.contains(&sprt_passes) {
                    got.entry(format!("sprt:{sprt_passes}")).or_insert(Ach {
                        cat: "sprt",
                        key: sprt_passes as f64,
                        icon: "✅",
                        title: if sprt_passes == 1 { "First improvement proven".into() } else { format!("{sprt_passes} improvements proven") },
                        desc: format!("Passed SPRT: {}", r.meta.name),
                        date,
                        run: r.meta.name.clone(),
                        main: main.clone(),
                        tier: "silver",
                    });
                }
            }

            let before = total_games;
            total_games += r.games.iter().filter(|g| result_for(g, main).is_some()).count();
            for step in GAME_STEPS.iter().filter(|&&s| before < s && total_games >= s) {
                got.entry(format!("games:{step}")).or_insert(Ach {
                    cat: "games",
                    key: *step as f64,
                    icon: "♟️",
                    title: format!("{step} games played"),
                    desc: "Across all tests".into(),
                    date,
                    run: r.meta.name.clone(),
                    main: main.clone(),
                    tier: "bronze",
                });
            }
        }

        // Goals still ahead.
        let locked = |cat, key, icon, title: String, desc: String| Ach {
            cat,
            key,
            icon,
            title,
            desc,
            date: None,
            run: String::new(),
            main: String::new(),
            tier: "",
        };
        for step in RATING_STEPS {
            got.entry(format!("rating:{step}"))
                .or_insert_with(|| locked("rating", step, "📈", format!("Reach {step:.0} rating"), "Confirmed by a finished rating test".into()));
        }
        for opp in LADDER {
            let s = strength(opp);
            let what = if full_strength(opp).is_some() { "Full-strength engine" } else { "Stockfish limited by UCI_Elo" };
            got.entry(format!("win:{opp}"))
                .or_insert_with(|| locked("wins", s, "⚔️", format!("Win a game vs {opp}"), what.into()));
            got.entry(format!("match:{opp}")).or_insert_with(|| {
                locked("matches", s, "🥇", format!("Beat {opp} in a match"), format!("{what}; score over 50% in 10+ games"))
            });
        }
        for step in GAME_STEPS {
            got.entry(format!("games:{step}"))
                .or_insert_with(|| locked("games", step as f64, "♟️", format!("{step} games played"), format!("{total_games} so far")));
        }
        for step in SPRT_STEPS {
            let title = if step == 1 { "First improvement proven".to_string() } else { format!("{step} improvements proven") };
            got.entry(format!("sprt:{step}"))
                .or_insert_with(|| locked("sprt", step as f64, "✅", title, format!("Changes that passed an SPRT test ({sprt_passes} so far)")));
        }

        let mut o = String::from("{\"items\":[");
        for (i, a) in got.values().enumerate() {
            let _ = write!(
                o,
                "{}{{\"cat\":{},\"key\":{},\"icon\":{},\"title\":{},\"desc\":{},\"date\":{},\"run\":{},\"main\":{},\"tier\":{},\"unlocked\":{}}}",
                if i > 0 { "," } else { "" },
                js(a.cat),
                num(a.key),
                js(a.icon),
                js(&a.title),
                js(&a.desc),
                a.date.map_or("null".into(), |d| d.to_string()),
                js(&a.run),
                js(&a.main),
                js(a.tier),
                a.date.is_some()
            );
        }
        o.push_str("]}");
        o
    }

    /// Automatic jobs (scripts that train and rate versions on a schedule), from
    /// training/jobs/<id>.txt written by tools/job-status.ps1. A job whose file has gone stale
    /// without finishing is reported as "stopped" (its script died, e.g. the PC restarted).
    fn jobs(&self) -> String {
        let mut out = Vec::new();
        for e in std::fs::read_dir(self.root.join("training").join("jobs")).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("txt") {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let mut f: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
            let id = p.file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string();
            let updated: u64 = f.get("updated").and_then(|v| v.parse().ok()).unwrap_or(0);
            let state = f.get("state").copied().unwrap_or("waiting");
            let finished = state == "done" || state == "failed";
            // Finished jobs stay listed for a day; stalled ones are flagged.
            if finished && now().saturating_sub(updated) > 86400 {
                continue;
            }
            let stalled = !finished && now().saturating_sub(updated) > 300;
            f.insert("state", if stalled { "stopped" } else { state });
            out.push(format!(
                "{{\"id\":{},\"title\":{},\"state\":{},\"detail\":{},\"start\":{},\"updated\":{}}}",
                js(&id),
                js(f.get("title").copied().unwrap_or(&id)),
                js(f["state"]),
                js(f.get("detail").copied().unwrap_or("")),
                f.get("start").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0),
                updated
            ));
        }
        format!("[{}]", out.join(","))
    }

    /// Which parts of the lab are running, judged from the heartbeat files each part keeps fresh.
    fn lab_state(&self) -> String {
        let age = |p: PathBuf| now().saturating_sub(mtime(&p));
        let watchdog = age(self.root.join("health").join("status.txt")) < 30;
        let sync = age(self.root.join("datagen").join("cloud-sync.heartbeat")) < 300;
        // oracle-sync.ps1 checks the Oracle server every 10 minutes.
        let oracle = age(self.root.join("datagen").join("oracle-sync.heartbeat")) < 1300;
        let mut datagen = false;
        for e in std::fs::read_dir(self.root.join("datagen")).into_iter().flatten().flatten() {
            let st = e.path().join("status.txt");
            let text = std::fs::read_to_string(&st).unwrap_or_default();
            // Only generators on this PC (cloud and Oracle status files carry a source= line).
            if !text.contains("source=") && text.contains("state=running") && age(st) < 15 {
                datagen = true;
            }
        }
        let log = std::fs::read_to_string(self.root.join("lab-control.log")).unwrap_or_default();
        let tail: Vec<String> = log.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().map(js).collect();
        let busy = self.lab_busy();
        format!(
            "{{\"watchdog\":{watchdog},\"cloud_sync\":{sync},\"oracle_sync\":{oracle},\"datagen\":{datagen},\"busy\":{},\"log\":[{}]}}",
            js(&busy),
            tail.join(",")
        )
    }

    /// "start"/"stop" while a lab-start/lab-stop script launched from here is still running.
    fn lab_busy(&self) -> String {
        let mut child = self.lab_child.lock().unwrap();
        if let Some((what, c)) = child.as_mut() {
            if matches!(c.try_wait(), Ok(None)) {
                return what.clone();
            }
        }
        *child = None;
        String::new()
    }

    /// Runs tools/lab-start.ps1 or tools/lab-stop.ps1 hidden, appending its output to lab-control.log.
    fn lab_action(&self, what: &str, start_cloud: bool) -> String {
        if !self.lab_busy().is_empty() {
            return "{\"ok\":false,\"error\":\"already busy\"}".into();
        }
        let exe = std::env::current_exe().unwrap_or_default();
        let Some(repo) = exe.ancestors().nth(3) else { return "{\"ok\":false}".into() };
        let script = repo.join("tools").join(if what == "start" { "lab-start.ps1" } else { "lab-stop.ps1" });
        let log_path = self.root.join("lab-control.log");
        let mut log = match std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
            Ok(f) => f,
            Err(_) => return "{\"ok\":false}".into(),
        };
        let stamp = std::process::Command::new("powershell").args(["-NoProfile", "-Command", "Get-Date -Format 'yyyy-MM-dd HH:mm:ss'"]).output();
        let stamp = stamp.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        let _ = writeln!(log, "--- {stamp} {what} requested from the dashboard{}", if start_cloud { " (with overnight cloud batch)" } else { "" });
        let mut args = vec!["-NoProfile".to_string(), "-ExecutionPolicy".into(), "Bypass".into(), "-File".into(), script.display().to_string()];
        if what == "start" {
            args.push("-NoBrowser".into());
        } else {
            args.push("-KeepDashboard".into());
            if start_cloud {
                args.push("-StartCloud".into());
            }
        }
        let mut cmd = std::process::Command::new("powershell");
        cmd.args(&args).stdout(log.try_clone().map(Stdio::from).unwrap_or(Stdio::null())).stderr(log.try_clone().map(Stdio::from).unwrap_or(Stdio::null()));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        match cmd.spawn() {
            Ok(c) => {
                *self.lab_child.lock().unwrap() = Some((what.to_string(), c));
                "{\"ok\":true}".into()
            }
            Err(e) => format!("{{\"ok\":false,\"error\":{}}}", js(&e.to_string())),
        }
    }

    /// `trusted` = POST with the X-Lab header. Browsers won't send a custom header cross-site
    /// without a CORS preflight (which this server never approves), so other websites can't
    /// trigger these actions.
    fn handle(&self, method: &str, full_path: &str, trusted: bool) -> (&'static str, String, &'static str) {
        let path = full_path.split('?').next().unwrap_or("/");
        let query = full_path.split_once('?').map(|q| q.1).unwrap_or("");
        let action = method == "POST" && trusted;
        if path == "/api/lab" {
            return ("200 OK", self.lab_state(), "application/json");
        }
        if let Some(what) = path.strip_prefix("/api/lab/") {
            if !action {
                return ("403 Forbidden", "{\"ok\":false}".into(), "application/json");
            }
            if matches!(what, "start" | "stop") {
                return ("200 OK", self.lab_action(what, query.contains("cloud=1")), "application/json");
            }
        }
        if path == "/" || path == "/index.html" {
            return ("200 OK", INDEX.to_string(), "text/html; charset=utf-8");
        }
        if path == "/api/runs" {
            let ids = self.refresh_all();
            let runs = self.runs.lock().unwrap();
            let items: Vec<String> = ids.iter().map(|id| run_brief(id, &runs[id])).collect();
            return ("200 OK", format!("[{}]", items.join(",")), "application/json");
        }
        if path == "/api/ratings" {
            self.refresh_all();
            let runs = self.runs.lock().unwrap();
            let mut list = global_ratings(&runs, &alias_map(&self.versions()));
            list.sort_by_key(|r| r.first_seen);
            let items: Vec<String> = list
                .iter()
                .map(|r| {
                    let mut vs = r.vs.clone();
                    vs.sort_by(|a, b| a.0.cmp(&b.0));
                    let vs: Vec<String> = vs
                        .iter()
                        .map(|(n, w, d, l, a)| {
                            format!(
                                "{{\"name\":{},\"w\":{},\"d\":{},\"l\":{},\"anchor\":{}}}",
                                js(n),
                                w,
                                d,
                                l,
                                a.map_or("null".into(), num)
                            )
                        })
                        .collect();
                    format!(
                        "{{\"name\":{},\"rating\":{},\"err\":{},\"games\":{},\"first_seen\":{},\"live\":{},\"vs\":[{}]}}",
                        js(&r.name),
                        num(r.rating),
                        num(r.err),
                        r.games,
                        r.first_seen,
                        r.live,
                        vs.join(",")
                    )
                })
                .collect();
            return ("200 OK", format!("[{}]", items.join(",")), "application/json");
        }
        if path == "/api/prepare" {
            return ("200 OK", self.prepare(), "application/json");
        }
        if path == "/api/versions" {
            return ("200 OK", self.versions_json(), "application/json");
        }
        if path == "/api/health" {
            return ("200 OK", self.health(), "application/json");
        }
        if let Some(mode) = path.strip_prefix("/api/control/") {
            if !action {
                return ("403 Forbidden", "{\"ok\":false}".into(), "application/json");
            }
            if matches!(mode, "pause" | "resume" | "auto") {
                let ok = std::fs::write(self.root.join("health").join("control.txt"), mode).is_ok();
                return ("200 OK", format!("{{\"ok\":{ok}}}"), "application/json");
            }
        }
        if path == "/api/training" {
            return ("200 OK", self.training(), "application/json");
        }
        if path == "/api/schedule" {
            return ("200 OK", self.jobs(), "application/json");
        }
        if path == "/api/datagen" {
            return ("200 OK", self.datagen(), "application/json");
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
    let mut first = req.split_whitespace();
    let method = first.next().unwrap_or("GET").to_string();
    let path = first.next().unwrap_or("/").to_string();
    let trusted = req.lines().any(|l| l.to_ascii_lowercase().starts_with("x-lab:"));
    let (status, body, ctype) = app.handle(&method, &path, trusted);
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
    // Default: the "testing" folder next to the repo (exe lives in <repo>/target/release/).
    let root = args.get(1).map(PathBuf::from).unwrap_or_else(|| {
        let exe = std::env::current_exe().unwrap_or_default();
        exe.ancestors().nth(4).map(|p| p.join("testing")).unwrap_or_else(|| PathBuf::from("testing"))
    });
    let port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8000);
    let _ = std::fs::create_dir_all(root.join("runs"));
    // The catalog sits at the repo root; the exe lives in <repo>/target/release/.
    let mut version_files: Vec<PathBuf> = std::env::current_exe()
        .ok()
        .and_then(|e| e.ancestors().nth(3).map(|p| p.join("versions.ini")))
        .into_iter()
        .collect();
    version_files.push(PathBuf::from("versions.ini"));
    let app = Arc::new(App { root, runs: Mutex::new(HashMap::new()), lab_child: Mutex::new(None), version_files });
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("cannot bind port");
    println!("Pawnstorm Lab running at http://localhost:{port}");
    for stream in listener.incoming().flatten() {
        let app = app.clone();
        std::thread::spawn(move || serve(&app, stream));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llr_line_from_fastchess() {
        let v = parse_llr("LLR: -0.00 (-0.1%) (-2.94, 2.94) [0.00, 5.00]").unwrap();
        assert_eq!(v, vec![-0.0, -2.94, 2.94, 0.0, 5.0]);
        let v = parse_llr("LLR: 1.20 (40.8%) (-2.94, 2.94) [0.00, 5.00]").unwrap();
        assert_eq!((v[0], v[1], v[2]), (1.2, -2.94, 2.94));
        // Older format without the percentage.
        let v = parse_llr("LLR: 0.50 (-2.94, 2.94) [0.00, 5.00]").unwrap();
        assert_eq!((v[0], v[1], v[2]), (0.5, -2.94, 2.94));
        assert!(parse_llr("Finished match").is_none());
    }

    #[test]
    fn positions_in_net_names() {
        assert_eq!(positions_from_name("net-v0.8.1-316m"), 316_000_000);
        assert_eq!(positions_from_name("net-v1.0-1.5b"), 0); // not an integer: unknown
        assert_eq!(positions_from_name("net-v2.0-2b"), 2_000_000_000);
        assert_eq!(positions_from_name("net-1024x8-219m-long"), 219_000_000);
        assert_eq!(positions_from_name("gen1"), 0);
    }

    #[test]
    fn catalog_merges_engine_names() {
        let vs = parse_versions("; c\n\u{feff}[v0.1]\nengines = Pawnstorm-0.1, pawnstorm-v0.1\nstatus = main\n\n[v0.9b]\nengines = pawnstorm-v0.9b-512x8\nsummary = x = y\n");
        assert_eq!(vs.len(), 2);
        assert_eq!(vs[0].engines.len(), 2);
        assert_eq!(vs[1].summary, "x = y");
        let a = alias_map(&vs);
        assert_eq!(canon_key(&a, "Pawnstorm-0.1"), canon_key(&a, "pawnstorm-v0.1"));
        assert_eq!(canon_key(&a, "pawnstorm-v0.9b-512x8"), "@v0.9b");
        assert_eq!(canon_key(&a, "SF-2900"), "sf2900");
    }
}
