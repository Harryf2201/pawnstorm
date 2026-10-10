use pawnstorm::{attacks, bench, uci};

fn main() {
    attacks::init();
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
        _ => uci::run(),
    }
}
