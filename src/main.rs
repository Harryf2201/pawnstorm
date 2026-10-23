use pawnstorm::{attacks, bench, datagen, nnue, uci};

fn main() {
    attacks::init();
    nnue::init();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("bench") => {
            let depth = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(bench::DEFAULT_DEPTH);
            bench::bench(depth);
        }
        Some("perft") => {
            if !bench::perft_suite() {
                std::process::exit(1);
            }
        }
        // datagen <threads> <positions> <soft-nodes> <output-dir> [max-seconds] [seed]
        //         [--openings <file> [--opening-percent 50] --deny <test-book start FENs>]
        //         [--fens <file> [--fen-percent 50] --deny <test-book start FENs>]
        Some("datagen") => {
            // Positional arguments end at the first --flag.
            let npos = args
                .iter()
                .position(|a| a.starts_with("--"))
                .unwrap_or(args.len());
            let allowed = ["--hard-nodes", "--keep-every", "--max-per-game", "--dup-cap", "--eval-file", "--openings", "--opening-percent", "--fens", "--fen-percent", "--deny"];
            for pair in args[npos..].chunks(2) {
                assert!(pair.len() == 2 && allowed.contains(&pair[0].as_str()), "unknown datagen flag or missing value: {}", pair[0]);
            }
            let pos = |i: usize| if i < npos { args.get(i) } else { None };
            let arg = |i: usize, d: u64| pos(i).and_then(|s| s.parse().ok()).unwrap_or(d);
            let opt = |i: usize| pos(i).and_then(|s| s.parse().ok()).filter(|&v: &u64| v > 0);
            let flag = |k: &str| {
                args.iter().position(|a| a == k).map(|i| {
                    args.get(i + 1)
                        .filter(|v| !v.starts_with("--"))
                        .unwrap_or_else(|| panic!("{k} needs a value"))
                        .clone()
                })
            };
            if let Some(path) = flag("--eval-file") {
                nnue::load_file(&path).unwrap_or_else(|e| panic!("info string {e}"));
            }
            assert!(
                nnue::embedded_empty() || nnue::active(),
                "embedded network incompatible: use matching architecture/net or --eval-file"
            );
            let openings = flag("--openings")
                .map(|p| datagen::load_openings(&p))
                .unwrap_or_default();
            let opening_percent = flag("--opening-percent")
                .map(|s| {
                    s.parse::<usize>()
                        .ok()
                        .filter(|&p| p <= 100)
                        .expect("--opening-percent must be 0-100")
                })
                .unwrap_or(50);
            let deny = flag("--deny")
                .map(|p| datagen::load_deny(&p))
                .unwrap_or_default();
            let fens = flag("--fens")
                .map(|p| datagen::load_fens(&p))
                .unwrap_or_default();
            let fen_percent = flag("--fen-percent")
                .map(|s| {
                    s.parse::<usize>()
                        .ok()
                        .filter(|&p| p <= 100)
                        .expect("--fen-percent must be 0-100")
                })
                .unwrap_or(50);
            assert!(
                (openings.is_empty() && fens.is_empty()) || !deny.is_empty(),
                "--openings/--fens needs --deny (the test book's start positions)"
            );
            datagen::run(datagen::Config {
                threads: arg(2, 8) as usize,
                positions: arg(3, 100_000_000),
                soft_nodes: arg(4, 5000),
                hard_nodes: flag("--hard-nodes")
                    .map(|v| v.parse().expect("invalid hard nodes"))
                    .unwrap_or(arg(4, 5000).saturating_mul(20)),
                keep_every: flag("--keep-every")
                    .map(|v| {
                        v.parse::<usize>()
                            .ok()
                            .filter(|&n| n > 0)
                            .expect("keep-every must be positive")
                    })
                    .unwrap_or(1),
                max_per_game: flag("--max-per-game")
                    .map(|v| v.parse().expect("invalid max-per-game"))
                    .unwrap_or(0),
                dup_cap: flag("--dup-cap")
                    .map(|v| v.parse().expect("invalid dup-cap"))
                    .unwrap_or(0),
                out: pos(5).map(Into::into).unwrap_or_else(|| "datagen".into()),
                max_seconds: opt(6),
                seed: opt(7),
                openings,
                opening_percent,
                fens,
                fen_percent,
                deny,
            });
        }
        Some("rescore") => pawnstorm::rescore::cli(&args[2..]),
        _ => uci::run(),
    }
}
