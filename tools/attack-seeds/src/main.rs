//! Stream gen7 self-play into a reproducible reservoir of diverse attack starts.
use pawnstorm::{attacks, board::Board, types::*};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

fn key(fen: &str) -> String {
    fen.split_whitespace().take(4).collect::<Vec<_>>().join(" ")
}

// Overlapping categories: opposite wings, storm, wedge, exposed shield, blocked attack.
fn criteria(b: &Board) -> [bool; 5] {
    let kings = [
        lsb(b.pieces[KING] & b.colors[0]),
        lsb(b.pieces[KING] & b.colors[1]),
    ];
    let wing = |sq: usize| {
        if sq % 8 <= 2 {
            -1
        } else if sq % 8 >= 5 {
            1
        } else {
            0
        }
    };
    let home = |c: usize, sq: usize| if c == WHITE { sq / 8 <= 1 } else { sq / 8 >= 6 };
    let mut hits = [
        wing(kings[0]) * wing(kings[1]) == -1 && home(0, kings[0]) && home(1, kings[1]),
        false,
        false,
        false,
        false,
    ];
    for defender in 0..2 {
        let k = kings[defender];
        if wing(k) == 0 || !home(defender, k) {
            continue;
        }
        let attacker = defender ^ 1;
        let mut advanced = 0;
        let mut shield = 0;
        let mut blocked = 0;
        let mut heavy = 0;
        for sq in 0..64 {
            if (sq as i32 % 8 - k as i32 % 8).abs() > 1 {
                continue;
            }
            let progress = if attacker == WHITE {
                sq / 8
            } else {
                7 - sq / 8
            };
            if b.pieces[PAWN] & b.colors[attacker] & bit(sq) != 0 {
                if progress >= 4 {
                    advanced += 1;
                }
                if progress == 5 {
                    hits[2] = true;
                }
                let next = if attacker == WHITE {
                    sq as i32 + 8
                } else {
                    sq as i32 - 8
                };
                if (0..64).contains(&next)
                    && b.pieces[PAWN] & b.colors[defender] & bit(next as usize) != 0
                {
                    blocked += 1;
                }
            }
            let rank = if defender == WHITE {
                sq / 8
            } else {
                7 - sq / 8
            };
            if b.pieces[PAWN] & b.colors[defender] & bit(sq) != 0 && rank == 1 {
                shield += 1;
            }
            if (b.pieces[ROOK] | b.pieces[QUEEN]) & b.colors[attacker] & bit(sq) != 0
                && progress >= 2
            {
                heavy += 1;
            }
        }
        hits[1] |= advanced >= 2;
        // Require nearby attacking force as well as a damaged shield.
        hits[3] |= shield <= 1 && (advanced >= 1 || heavy >= 1);
        hits[4] |= blocked >= 2 && heavy >= 1;
    }
    hits
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            files(&p, out)?;
        } else {
            let n = p.file_name().unwrap().to_string_lossy();
            if n.starts_with("data_") && (n.ends_with(".txt") || n.ends_with(".txt.gz")) {
                out.push(p);
            }
        }
    }
    Ok(())
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    let root = Path::new(arg("--root"));
    let count: usize = arg("--count").parse().expect("bad count");
    assert!(count > 0);
    // Fail before a potentially long scan if the output is not writable.
    let output = File::create(arg("--out"))?;
    let deny: HashSet<_> = fs::read_to_string(arg("--deny"))?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(key)
        .collect();
    attacks::init();
    let mut sources = Vec::new();
    for entry in fs::read_dir(root)? {
        let p = entry?.path();
        let n = p.file_name().unwrap().to_string_lossy();
        if p.is_dir()
            && (n == "gen7o-v20-5k"
                || n == "gen7-v20-5k"
                || (n.starts_with("cloud-")
                    && fs::read_to_string(p.join("status.txt"))
                        .unwrap_or_default()
                        .lines()
                        .any(|l| l.trim() == "gen=7")))
        {
            files(&p, &mut sources)?;
        }
    }
    sources.sort();
    let mut seen = HashSet::new();
    let mut reservoir = Vec::new();
    let (mut lines, mut eligible, mut qualifying, mut unique, mut denied, mut duplicates) =
        (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut hits = [0u64; 5];
    let mut rng = 0x5eedcafe12345678u64;
    for (index, path) in sources.iter().enumerate() {
        // Use gzip's streaming decoder (also handles concatenated gzip members).
        let mut decoder = None;
        let reader: Box<dyn std::io::Read> = if path.extension().is_some_and(|e| e == "gz") {
            let mut child = std::process::Command::new("gzip")
                .arg("-dc")
                .arg(path)
                .stdout(std::process::Stdio::piped())
                .spawn()?;
            let stdout = child.stdout.take().unwrap();
            decoder = Some(child);
            Box::new(stdout)
        } else {
            Box::new(File::open(path)?)
        };
        let mut reader = BufReader::with_capacity(1 << 20, reader);
        let mut line = String::new();
        let (mut region, mut previous_ply) = (0u64, None);
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            if !line.ends_with('\n') {
                break; // A live writer may not have finished its final record.
            }
            lines += 1;
            let mut fields = line.split('|');
            let fen = fields.next().unwrap().trim();
            let Some(score) = fields.next().and_then(|s| s.trim().parse::<i32>().ok()) else {
                continue;
            };
            if !fields
                .next()
                .is_some_and(|s| matches!(s.trim(), "0.0" | "0.5" | "1.0"))
            {
                continue;
            }
            let mut parts = fen.split_whitespace();
            let _placement = parts.next();
            let side = parts.next();
            let Some(fullmove) = parts.nth(3).and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            if parts.next().is_some() {
                continue;
            }
            let ply = fullmove * 2 + u32::from(side == Some("b"));
            // Saved quiet records omit plies; resets/result changes cannot fully identify games.
            if previous_ply.is_some_and(|p| ply <= p) {
                region = 0;
            }
            previous_ply = Some(ply);
            if score.unsigned_abs() > 300 || !(12..=40).contains(&fullmove) {
                continue;
            }
            let Ok(b) = Board::from_fen(fen) else {
                continue;
            };
            let pieces = b.pieces[KNIGHT] | b.pieces[BISHOP] | b.pieces[ROOK] | b.pieces[QUEEN];
            if !((b.pieces[QUEEN] & b.colors[0] != 0 && b.pieces[QUEEN] & b.colors[1] != 0)
                || (0..2).all(|c| (pieces & b.colors[c]).count_ones() >= 2))
            {
                continue;
            }
            eligible += 1;
            let flags = criteria(&b);
            if !flags.iter().any(|&h| h) {
                continue;
            }
            qualifying += 1;
            for i in 0..5 {
                hits[i] += u64::from(flags[i]);
            }
            region += 1;
            if (region - 1) % 8 != 0 {
                continue;
            }
            let k = key(fen);
            if deny.contains(&k) {
                denied += 1;
                continue;
            }
            if !seen.insert(k) {
                duplicates += 1;
                continue;
            }
            unique += 1;
            if reservoir.len() < count {
                reservoir.push(fen.to_owned());
            } else {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let slot = (rng % unique) as usize;
                if slot < count {
                    reservoir[slot] = fen.to_owned();
                }
            }
        }
        if let Some(mut child) = decoder {
            assert!(child.wait()?.success(), "gzip failed: {}", path.display());
        }
        eprintln!(
            "file {}/{}: lines={lines} qualifying={qualifying} unique={unique}",
            index + 1,
            sources.len()
        );
    }
    let mut out = BufWriter::new(output);
    for fen in &reservoir {
        writeln!(out, "{fen}")?;
    }
    out.flush()?;
    println!("files={} lines={lines} eligible={eligible} qualifying={qualifying} unique_sampled={unique} denied={denied} duplicates={duplicates} output={}", sources.len(), reservoir.len());
    for (name, n) in ["opposite_wings", "storm", "wedge", "shield", "blocked"]
        .iter()
        .zip(hits)
    {
        println!(
            "{name}={n} ({:.2}% of eligible)",
            100.0 * n as f64 / eligible.max(1) as f64
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_ignores_clocks_but_preserves_position_state() {
        let a = "8/8/8/8/8/8/8/8 w KQ e3 0 12";
        assert_eq!(key(a), key("8/8/8/8/8/8/8/8 w KQ e3 90 40"));
        assert_ne!(key(a), key("8/8/8/8/8/8/8/8 b KQ e3 0 12"));
        assert_ne!(key(a), key("8/8/8/8/8/8/8/8 w K e3 0 12"));
        assert_ne!(key(a), key("8/8/8/8/8/8/8/8 w KQ - 0 12"));
    }

    #[test]
    fn attack_features_are_color_symmetric() {
        attacks::init();
        let white = Board::from_fen("6k1/5ppp/5PPP/8/8/8/PPP5/2K5 w - - 0 20").unwrap();
        let black = Board::from_fen("2k5/ppp5/8/8/8/5ppp/5PPP/6K1 b - - 0 20").unwrap();
        assert_eq!(criteria(&white), criteria(&black));
        assert!(criteria(&white)[0]);
        assert!(criteria(&white)[1]);
        assert!(criteria(&white)[2]);
        assert!(!criteria(&Board::startpos()).iter().any(|&v| v));
    }
}
