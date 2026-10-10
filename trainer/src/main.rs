//! NNUE trainer for Pawnstorm (runs on the GPU via bullet).
//!
//! Architecture: (768 -> HIDDEN)x2 -> BUCKETS with SCReLU, quantised for fast integer inference.
//! With BUCKETS > 1 the output layer has one set of weights per material bucket
//! (bucket = (pieces - 2) / (32 / BUCKETS)), saved transposed: one row of 2*HIDDEN weights per bucket.
//! Usage: trainer <net-id> <data.bin> <output-dir> [superbatches] [wdl] [hidden] [buckets]
//! The engine's src/nnue.rs HIDDEN / OUTPUT_BUCKETS must match the network it loads.

use bullet_lib::{
    game::{inputs::Chess768, outputs::MaterialCount},
    nn::optimiser::AdamW,
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let net_id = args.get(1).cloned().unwrap_or_else(|| "net".into());
    let data = args.get(2).cloned().expect("usage: trainer <net-id> <data.bin> <output-dir> [superbatches] [wdl] [hidden] [buckets]");
    let out = args.get(3).cloned().unwrap_or_else(|| "checkpoints".into());
    let superbatches: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(40);
    let wdl_weight: f32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0.4);
    let hidden: usize = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(512);
    let buckets: usize = args.get(7).and_then(|s| s.parse().ok()).unwrap_or(1);

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
        wdl_scheduler: wdl::ConstantWDL { value: wdl_weight },
        lr_scheduler: lr::CosineDecayLR { initial_lr: 0.001, final_lr: 0.001 * 0.3f32.powi(5), final_superbatch: superbatches },
        save_rate: 10,
    };
    let settings = LocalSettings { threads: 4, test_set: None, output_directory: &out, batch_queue_size: 64 };
    let data_loader = loader::DirectSequentialDataLoader::new(&[data.as_str()]);
    println!("Architecture           : (768 -> {hidden})x2 -> {buckets}");

    // target = wdl * game_result + (1 - wdl) * sigmoid(score / SCALE)
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
