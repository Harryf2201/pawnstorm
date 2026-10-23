//! Streaming, bounded-memory rescoring. Ordered chunks make checkpoints independent of scheduling.
use crate::{
    board::Board,
    search::{Limits, Searcher, MATE_BOUND},
    tt::TT,
    types::WHITE,
};
use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, BufWriter, Seek, Write},
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
};

fn parse(line: &str) -> Result<(Board, i32, &str), String> {
    let parts: Vec<_> = line.split('|').map(str::trim).collect();
    if parts.len() != 3 || !["1.0", "0.5", "0.0"].contains(&parts[2]) {
        return Err("expected FEN | integer score | result 1.0/0.5/0.0".into());
    }
    Ok((
        Board::from_fen(parts[0])?,
        parts[1].parse().map_err(|_| "invalid score")?,
        parts[2],
    ))
}
fn format_line(fen: &str, old: i32, new: i32, result: &str, blend: f64) -> String {
    format!(
        "{fen} | {} | {result}\n",
        (blend * new as f64 + (1.0 - blend) * old as f64).round() as i32
    )
}
fn selected(index: u64, sample: f64) -> bool {
    let mut h = index.wrapping_add(0x9e3779b97f4a7c15);
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94d049bb133111eb);
    ((h ^ (h >> 31)) >> 11) as f64 / ((1u64 << 53) as f64) < sample
}
fn reader(path: &Path) -> io::Result<(Box<dyn BufRead>, Option<std::process::Child>)> {
    if path.extension().is_some_and(|s| s == "gz") {
        let gzip = if cfg!(windows) {
            PathBuf::from(
                std::env::var("ProgramFiles").unwrap_or_else(|_| "C:/Program Files".into()),
            )
            .join("Git/usr/bin/gzip.exe")
        } else {
            "gzip".into()
        };
        let mut child = std::process::Command::new(gzip)
            .arg("-dc")
            .arg(path)
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        Ok((
            Box::new(BufReader::new(child.stdout.take().unwrap())),
            Some(child),
        ))
    } else {
        Ok((Box::new(BufReader::new(fs::File::open(path)?)), None))
    }
}
pub fn cli(args: &[String]) {
    if let Err(e) = run(args) {
        eprintln!("rescore: {e}");
        std::process::exit(1);
    }
}
fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        return Err("usage: rescore <input.txt[.gz]|dir> <out.txt> --nodes N --threads T [--sample P] [--limit L] [--blend W] [--eval-file net]".into());
    }
    use std::hash::{Hash, Hasher};
    let mut net_bytes = include_bytes!("../nets/default.bin").to_vec();
    let mut nodes = None;
    let mut threads = 1usize;
    let mut sample = 1.0;
    let mut limit = u64::MAX;
    let mut blend = 1.0;
    let mut i = 2;
    while i < args.len() {
        let v = args.get(i + 1).ok_or("flag needs a value")?;
        match args[i].as_str() {
            "--nodes" => nodes = Some(v.parse::<u64>()?),
            "--threads" => threads = v.parse()?,
            "--sample" => sample = v.parse()?,
            "--limit" => limit = v.parse()?,
            "--blend" => blend = v.parse()?,
            "--eval-file" => {
                crate::nnue::load_file(v)?;
                net_bytes = fs::read(v)?;
            }
            _ => return Err(format!("unknown flag {}", args[i]).into()),
        }
        i += 2;
    }
    if !crate::nnue::active() {
        return Err("rescore requires a compatible network (--eval-file)".into());
    }
    let nodes = nodes.filter(|&n| n > 0).ok_or("--nodes must be positive")?;
    if threads == 0
        || threads > 64
        || !(0.0..=1.0).contains(&sample)
        || !(0.0..=1.0).contains(&blend)
    {
        return Err("invalid threads/sample/blend".into());
    }
    let input = Path::new(&args[0]);
    let output = Path::new(&args[1]);
    let mut files: Vec<PathBuf> = if input.is_dir() {
        fs::read_dir(input)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && (p.extension().is_some_and(|s| s == "txt" || s == "gz"))
                    && p.file_name()
                        .is_some_and(|s| s.to_string_lossy().starts_with("data_"))
            })
            .collect()
    } else {
        vec![input.into()]
    };
    files.sort();
    if files.is_empty() {
        return Err("no input data files".into());
    }
    let output_abs = if output.exists() { fs::canonicalize(output)? } else { std::path::absolute(output)? };
    if files
        .iter()
        .any(|p| fs::canonicalize(p).ok().as_ref() == Some(&output_abs))
    {
        return Err("output must not be an input".into());
    }
    // Include source sizes and arguments: changing the recipe or snapshot requires a new output.
    let mut net_hash = std::collections::hash_map::DefaultHasher::new();
    net_bytes.hash(&mut net_hash);
    let manifest = format!(
        "{:?}\n{:?}\n{:?}",
        args,
        files
            .iter()
            .map(|p| (
                p,
                fs::metadata(p).map(|m| (m.len(), m.modified().ok())).ok()
            ))
            .collect::<Vec<_>>(),
        net_hash.finish()
    );
    let meta = output.with_extension("rescore.manifest");
    let checkpoint = output.with_extension("rescore.checkpoint");
    if meta.exists() {
        if fs::read_to_string(&meta)? != manifest {
            return Err("input/recipe changed; use a new output".into());
        }
    } else {
        if output.exists() && fs::metadata(output)?.len() > 0 {
            return Err("existing output has no checkpoint; use a new output".into());
        }
        fs::write(&meta, manifest)?;
    }
    let (mut scanned, mut written, bytes) = if checkpoint.exists() {
        let c = fs::read_to_string(&checkpoint)?;
        let n: Vec<u64> = c
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()?;
        if n.len() != 3 {
            return Err("invalid checkpoint".into());
        }
        (n[0], n[1], n[2])
    } else {
        (0, 0, 0)
    };
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(output)?;
    if file.metadata()?.len() < bytes {
        return Err("output shorter than checkpoint".into());
    }
    file.set_len(bytes)?;
    let mut writer = BufWriter::new(file);
    writer.seek(std::io::SeekFrom::End(0))?;
    let resume = scanned;
    let mut index = 0u64;
    let mut workers = Vec::new();
    for _ in 0..threads {
        let tt = Arc::new(TT::new(16));
        let mut s = Box::new(Searcher::new(tt.clone(), Arc::new(AtomicBool::new(false))));
        s.print = false;
        workers.push((tt, s));
    }
    let start = std::time::Instant::now();
    'files: for path in files {
        let (mut reader, mut child) = reader(&path)?;
        loop {
            if written >= limit {
                if let Some(c) = child.as_mut() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                break 'files;
            }
            let mut chunk = Vec::new();
            let mut line = String::new();
            while chunk.len() < threads * 8 {
                line.clear();
                if reader.read_line(&mut line)? == 0 {
                    break;
                }
                // Ignore a live file's unfinished final record.
                if !line.ends_with('\n') {
                    break;
                }
                index += 1;
                if index <= resume {
                    continue;
                }
                scanned = index;
                if !selected(index, sample) {
                    continue;
                }
                let (b, old, result) =
                    parse(line.trim()).map_err(|e| format!("{}:{index}: {e}", path.display()))?;
                if b.in_check() || old.unsigned_abs() >= MATE_BOUND as u32 {
                    continue;
                }
                chunk.push((b, old, result.to_owned()));
            }
            let results: Vec<Vec<Option<String>>> = std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for ((tt, s), batch) in workers.iter_mut().zip(chunk.chunks(8)) {
                    handles.push(
                        std::thread::Builder::new()
                            .stack_size(64 * 1024 * 1024)
                            .spawn_scoped(scope, move || {
                                batch
                                    .iter()
                                    .map(|(b, old, result)| {
                                        tt.clear();
                                        s.clear();
                                        s.hashes = vec![b.hash];
                                        let (_, score) = s.search(
                                            b,
                                            &Limits {
                                                nodes: Some(nodes),
                                                ..Default::default()
                                            },
                                        );
                                        if score.abs() >= MATE_BOUND {
                                            None
                                        } else {
                                            Some(format_line(
                                                &b.to_fen(),
                                                *old,
                                                if b.stm == WHITE { score } else { -score },
                                                result,
                                                blend,
                                            ))
                                        }
                                    })
                                    .collect()
                            })
                            .expect("cannot spawn rescore worker"),
                    );
                }
                handles
                    .into_iter()
                    .map(|h| h.join().expect("rescore worker failed"))
                    .collect()
            });
            for line in results.into_iter().flatten().flatten() {
                if written < limit {
                    writer.write_all(line.as_bytes())?;
                    written += 1;
                }
            }
            writer.flush()?;
            writer.get_ref().sync_data()?;
            let bytes = writer.stream_position()?;
            let tmp = checkpoint.with_extension("checkpoint.tmp");
            fs::write(&tmp, format!("{scanned} {written} {bytes}\n"))?;
            fs::rename(tmp, &checkpoint)?;
            eprintln!(
                "rescore: {scanned} input lines, {written} written, {:.1}s",
                start.elapsed().as_secs_f64()
            );
            if chunk.is_empty() {
                break;
            }
        }
        if let Some(mut c) = child {
            if !c.wait()?.success() {
                return Err("gzip decompression failed".into());
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn line_result_and_blend() {
        let fen = Board::startpos().to_fen();
        for result in ["1.0", "0.5", "0.0"] {
            let l = format_line(&fen, -100, 200, result, 0.5);
            let (_, s, r) = parse(l.trim()).unwrap();
            assert_eq!(s, 50);
            assert_eq!(r, result);
        }
        assert!(parse("bad | 1 | 0.7").is_err());
    }
    #[test]
    fn deterministic_sampling() {
        assert!(
            (1..10000)
                .filter(|&i| selected(i, 0.1))
                .count()
                .abs_diff(1000)
                < 100
        );
    }
}
