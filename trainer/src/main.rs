//! NNUE trainer for Pawnstorm (runs on the GPU via bullet).
//!
//! Architecture: (768 -> HIDDEN)x2 -> BUCKETS with SCReLU, quantised for fast integer inference.
//! With BUCKETS > 1 the output layer has one set of weights per material bucket
//! (bucket = (pieces - 2) / (32 / BUCKETS)), saved transposed: one row of 2*HIDDEN weights per bucket.
//! Usage: trainer <net-id> <data.bin> <output-dir> [superbatches] [wdl] [hidden] [buckets]
//! The engine's src/nnue.rs HIDDEN / OUTPUT_BUCKETS must match the network it loads.

use bullet_lib::{
    game::{
        inputs::{Chess768, ChessBucketsMirrored, get_num_buckets},
        outputs::MaterialCount,
    },
    nn::{
        InitSettings, Shape,
        optimiser::{AdamW, AdamWParams},
    },
    trainer::{
        save::SavedFormat,
        schedule::{TrainingSchedule, TrainingSteps, lr, wdl},
        settings::LocalSettings,
    },
    value::{ValueTrainerBuilder, loader},
};

const SCALE: i32 = 400;
const QA: i16 = 255;
const QB: i16 = 64;

/// King buckets by own-king square (files a-d; e-h are mirrored). Must match src/nnue.rs.
#[rustfmt::skip]
const KING_BUCKETS: [usize; 32] = [
    0, 1, 2, 3,
    4, 4, 5, 5,
    6, 6, 6, 6,
    7, 7, 7, 7,
    8, 8, 8, 8,
    8, 8, 8, 8,
    9, 9, 9, 9,
    9, 9, 9, 9,
];
/// "kb4": a/b and c/d files on the back rank, the 2nd rank, everything further up.
#[rustfmt::skip]
const KING_BUCKETS_4: [usize; 32] = [
    0, 0, 1, 1,
    2, 2, 2, 2,
    3, 3, 3, 3,
    3, 3, 3, 3,
    3, 3, 3, 3,
    3, 3, 3, 3,
    3, 3, 3, 3,
    3, 3, 3, 3,
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let net_id = args.get(1).cloned().unwrap_or_else(|| "net".into());
    let data = args.get(2).cloned().expect("usage: trainer <net-id> <data.bin> <output-dir> [superbatches] [wdl] [hidden] [buckets]");
    let out = args.get(3).cloned().unwrap_or_else(|| "checkpoints".into());
    let superbatches: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(40);
    // "0.4" = constant result weight; "0.2-0.6" = rising linearly from the first to the last superbatch.
    let wdl_arg = args.get(5).cloned().unwrap_or_else(|| "0.4".into());
    let (wdl_start, wdl_end): (f32, f32) = match wdl_arg.split_once('-') {
        Some((a, b)) => (a.parse().expect("wdl start"), b.parse().expect("wdl end")),
        None => {
            let w = wdl_arg.parse().expect("wdl");
            (w, w)
        }
    };
    let hidden: usize = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(512);
    let buckets: usize = args.get(7).and_then(|s| s.parse().ok()).unwrap_or(1);
    // "768" = plain piece-square inputs; "kb10" = 10 mirrored king buckets (see BUCKET_LAYOUT).
    let inputs = args.get(8).cloned().unwrap_or_else(|| "768".into());

    let schedule = TrainingSchedule {
        net_id,
        eval_scale: SCALE as f32,
        steps: TrainingSteps {
            batch_size: 16_384,
            // 6104 * 16384 ≈ 100M positions per superbatch
            batches_per_superbatch: 6104,
            start_superbatch: 1,
            end_superbatch: superbatches,
        },
        wdl_scheduler: wdl::LinearWDL { start: wdl_start, end: wdl_end },
        lr_scheduler: lr::CosineDecayLR { initial_lr: 0.001, final_lr: 0.001 * 0.3f32.powi(5), final_superbatch: superbatches },
        save_rate: 10,
    };
    let settings = LocalSettings { threads: 4, test_set: None, output_directory: &out, batch_queue_size: 64 };
    let data_loader = loader::DirectSequentialDataLoader::new(&[data.as_str()]);
    println!("Architecture           : ({inputs} -> {hidden})x2 -> {buckets}");

    // target = wdl * game_result + (1 - wdl) * sigmoid(score / SCALE)
    if inputs == "ml" || inputs == "ml32" || inputs == "mlkb4" {
        // ml32: 32 neurons in the first hidden layer instead of 16. mlkb4: 4 mirrored king buckets on the inputs,
        // with a shared factoriser (merged into the bucket weights when saving), like kb4.
        let l1n: usize = if inputs == "ml32" { 32 } else { 16 };
        let kb = inputs == "mlkb4";
        let num_kb = if kb { get_num_buckets(&KING_BUCKETS_4) } else { 1 };
        // Multi-layer: (768 -> hidden, clipped ReLU, pairwise products of the two halves) x2 -> 16 -> 32 -> 1,
        // each layer after the first picked by material (8 output buckets). The first layer is quantised
        // to i16 like the single-layer nets; the small layers stay f32 in their native (input-major) layout.
        assert_eq!(buckets, 8, "the multi-layer net is set up with 8 output buckets");
        // Same network either way; only the input features (and, with king buckets, the factoriser) differ.
        macro_rules! ml_trainer {
            ($inputs:expr, $factorised:expr) => {{
                let mut trainer = ValueTrainerBuilder::default()
                    .dual_perspective()
                    .optimiser(AdamW)
                    .inputs($inputs)
                    .output_buckets(MaterialCount::<8>)
                    .save_format(&[
                        SavedFormat::id("l0w")
                            .transform(move |store, weights| {
                                if !$factorised {
                                    return weights;
                                }
                                let factoriser = store.get("l0f").values.f32().repeat(num_kb);
                                weights.into_iter().zip(factoriser).map(|(a, b)| a + b).collect()
                            })
                            .round()
                            .quantise::<i16>(QA),
                        SavedFormat::id("l0b").round().quantise::<i16>(QA),
                        SavedFormat::id("l1w"),
                        SavedFormat::id("l1b"),
                        SavedFormat::id("l2w"),
                        SavedFormat::id("l2b"),
                        SavedFormat::id("l3w"),
                        SavedFormat::id("l3b"),
                    ])
                    .loss_fn(|output, target| output.sigmoid().squared_error(target))
                    .build(|builder, stm_inputs, ntm_inputs, output_buckets| {
                        let mut l0 = builder.new_affine("l0", 768 * num_kb, hidden);
                        if $factorised {
                            let l0f = builder.new_weights("l0f", Shape::new(hidden, 768), InitSettings::Zeroed);
                            l0.weights = l0.weights + l0f.repeat(num_kb);
                        }
                        let l1 = builder.new_affine("l1", hidden, 8 * l1n);
                        let l2 = builder.new_affine("l2", l1n, 8 * 32);
                        let l3 = builder.new_affine("l3", 32, 8);
                        let ft = |input, start, end| l0.slice(start, end).forward(input).crelu();
                        let stm_hidden = ft(stm_inputs, 0, hidden / 2) * ft(stm_inputs, hidden / 2, hidden);
                        let ntm_hidden = ft(ntm_inputs, 0, hidden / 2) * ft(ntm_inputs, hidden / 2, hidden);
                        let hl1 = stm_hidden.concat(ntm_hidden);
                        let hl2 = l1.forward(hl1).select(output_buckets).screlu();
                        let hl3 = l2.forward(hl2).select(output_buckets).screlu();
                        l3.forward(hl3).select(output_buckets)
                    });
                if $factorised {
                    // Merged weights must still fit the quantisation range.
                    let clip = AdamWParams { max_weight: 0.99, min_weight: -0.99, ..Default::default() };
                    trainer.optimiser.set_params_for_weight("l0w", clip);
                    trainer.optimiser.set_params_for_weight("l0f", clip);
                }
                // Reference evaluations for checking the engine's inference: trainer <id> <data> <checkpoint-dir> ... ml eval=<fen file>
                if let Some(fens) = args.get(9).and_then(|a| a.strip_prefix("eval=")) {
                    trainer.load_from_checkpoint(&out);
                    for fen in std::fs::read_to_string(fens).expect("fen file").lines().filter(|l| !l.trim().is_empty()) {
                        println!("{:.2}\t{fen}", trainer.eval(fen.trim()));
                    }
                    return;
                }
                trainer.run(&schedule, &settings, &data_loader);
                return;
            }};
        }
        if kb {
            ml_trainer!(ChessBucketsMirrored::new(KING_BUCKETS_4), true);
        } else {
            ml_trainer!(Chess768, false);
        }
    }
    if inputs == "kb10" || inputs == "kb4" {
        assert_eq!(buckets, 1, "king buckets are only set up with a single output bucket");
        let layout = if inputs == "kb4" { KING_BUCKETS_4 } else { KING_BUCKETS };
        let num_kb = get_num_buckets(&layout);
        // A shared "factoriser" (one 768-input copy added to every bucket) lets what is learned
        // in one king bucket help the others; it is merged into the bucket weights when saving.
        let mut trainer = ValueTrainerBuilder::default()
            .dual_perspective()
            .optimiser(AdamW)
            .inputs(ChessBucketsMirrored::new(layout))
            .save_format(&[
                SavedFormat::id("l0w")
                    .transform(move |store, weights| {
                        let factoriser = store.get("l0f").values.f32().repeat(num_kb);
                        weights.into_iter().zip(factoriser).map(|(a, b)| a + b).collect()
                    })
                    .round()
                    .quantise::<i16>(QA),
                SavedFormat::id("l0b").round().quantise::<i16>(QA),
                SavedFormat::id("l1w").round().quantise::<i16>(QB),
                SavedFormat::id("l1b").round().quantise::<i16>(QA * QB),
            ])
            .loss_fn(|output, target| output.sigmoid().squared_error(target))
            .build(|builder, stm_inputs, ntm_inputs| {
                let l0f = builder.new_weights("l0f", Shape::new(hidden, 768), InitSettings::Zeroed);
                let mut l0 = builder.new_affine("l0", 768 * num_kb, hidden);
                l0.weights = l0.weights + l0f.repeat(num_kb);
                let l1 = builder.new_affine("l1", 2 * hidden, 1);
                let stm_hidden = l0.forward(stm_inputs).screlu();
                let ntm_hidden = l0.forward(ntm_inputs).screlu();
                l1.forward(stm_hidden.concat(ntm_hidden))
            });
        // Merged weights must still fit the quantisation range.
        let clip = AdamWParams { max_weight: 0.99, min_weight: -0.99, ..Default::default() };
        trainer.optimiser.set_params_for_weight("l0w", clip);
        trainer.optimiser.set_params_for_weight("l0f", clip);
        trainer.run(&schedule, &settings, &data_loader);
        return;
    }
    match buckets {
        1 => {
            let mut trainer = ValueTrainerBuilder::default()
                .dual_perspective()
                .optimiser(AdamW)
                .inputs(Chess768)
                .save_format(&[
                    SavedFormat::id("l0w").round().quantise::<i16>(QA),
                    SavedFormat::id("l0b").round().quantise::<i16>(QA),
                    SavedFormat::id("l1w").round().quantise::<i16>(QB),
                    SavedFormat::id("l1b").round().quantise::<i16>(QA * QB),
                ])
                .loss_fn(|output, target| output.sigmoid().squared_error(target))
                .build(|builder, stm_inputs, ntm_inputs| {
                    let l0 = builder.new_affine("l0", 768, hidden);
                    let l1 = builder.new_affine("l1", 2 * hidden, 1);
                    let stm_hidden = l0.forward(stm_inputs).screlu();
                    let ntm_hidden = l0.forward(ntm_inputs).screlu();
                    l1.forward(stm_hidden.concat(ntm_hidden))
                });
            trainer.run(&schedule, &settings, &data_loader);
        }
        8 => {
            let mut trainer = ValueTrainerBuilder::default()
                .dual_perspective()
                .optimiser(AdamW)
                .inputs(Chess768)
                .output_buckets(MaterialCount::<8>)
                .save_format(&[
                    SavedFormat::id("l0w").round().quantise::<i16>(QA),
                    SavedFormat::id("l0b").round().quantise::<i16>(QA),
                    // transposed: [bucket][2 * hidden] for fast CPU inference
                    SavedFormat::id("l1w").round().quantise::<i16>(QB).transpose(),
                    SavedFormat::id("l1b").round().quantise::<i16>(QA * QB),
                ])
                .loss_fn(|output, target| output.sigmoid().squared_error(target))
                .build(|builder, stm_inputs, ntm_inputs, output_buckets| {
                    let l0 = builder.new_affine("l0", 768, hidden);
                    let l1 = builder.new_affine("l1", 2 * hidden, 8);
                    let stm_hidden = l0.forward(stm_inputs).screlu();
                    let ntm_hidden = l0.forward(ntm_inputs).screlu();
                    l1.forward(stm_hidden.concat(ntm_hidden)).select(output_buckets)
                });
            trainer.run(&schedule, &settings, &data_loader);
        }
        _ => panic!("supported output bucket counts: 1 or 8"),
    }
}
