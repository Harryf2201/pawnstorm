use pawnstorm::{attacks, bench, datagen, nnue, uci};

fn main() {
    attacks::init();
    nnue::init();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("bench") => {
            let depth = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(bench::DEFAULT_DEPTH);
            bench::bench(depth);
        }
        Some("perft") => {
            if !bench::perft_suite() {
                std::process::exit(1);
            }
        }
        // datagen <threads> <positions> <soft-nodes> <output-dir> [max-seconds] [seed]
        //         [--openings <file> [--opening-percent 50] --deny <test-book start FENs>]
        Some("datagen") => {
            // Positional arguments end at the first --flag.
            let npos = args.iter().position(|a| a.starts_with("--")).unwrap_or(args.len());
            let pos = |i: usize| if i < npos { args.get(i) } else { None };
            let arg = |i: usize, d: u64| pos(i).and_then(|s| s.parse().ok()).unwrap_or(d);
            let opt = |i: usize| pos(i).and_then(|s| s.parse().ok()).filter(|&v: &u64| v > 0);
            let flag = |k: &str| {
                args.iter().position(|a| a == k).map(|i| args.get(i + 1).filter(|v| !v.starts_with("--")).unwrap_or_else(|| panic!("{k} needs a value")).clone())
            };
            let openings = flag("--openings").map(|p| datagen::load_openings(&p)).unwrap_or_default();
            let opening_percent = flag("--opening-percent").map(|s| s.parse::<usize>().ok().filter(|&p| p <= 100).expect("--opening-percent must be 0-100")).unwrap_or(50);
            let deny = flag("--deny").map(|p| datagen::load_deny(&p)).unwrap_or_default();
            assert!(openings.is_empty() || !deny.is_empty(), "--openings needs --deny (the test book's start positions)");
            datagen::run(datagen::Config {
                threads: arg(2, 8) as usize,
                positions: arg(3, 100_000_000),
                soft_nodes: arg(4, 5000),
                out: pos(5).map(Into::into).unwrap_or_else(|| "datagen".into()),
                max_seconds: opt(6),
                seed: opt(7),
                openings,
                opening_percent,
                deny,
            });
        }
        _ => uci::run(),
    }
}
