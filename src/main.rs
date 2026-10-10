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
        // datagen <threads> <positions> <soft-nodes> <output-dir>
        Some("datagen") => {
            let arg = |i: usize, d: u64| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
            datagen::run(datagen::Config {
                threads: arg(2, 8) as usize,
                positions: arg(3, 100_000_000),
                soft_nodes: arg(4, 5000),
                out: args.get(5).map(Into::into).unwrap_or_else(|| "datagen".into()),
            });
        }
        _ => uci::run(),
    }
}
