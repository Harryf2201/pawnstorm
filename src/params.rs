//! Search parameters that can be tuned (SPSA, tools\spsa.ps1). Each is a UCI spin option with the
//! same name; the defaults are what the engine plays with.

macro_rules! params {
    ($($id:ident = $def:expr, $min:expr, $max:expr;)*) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[derive(Clone, Copy)]
        #[repr(usize)]
        pub enum P { $($id),* }
        /// (name, default, min, max)
        pub const DEFS: &[(&str, i32, i32, i32)] = &[$((stringify!($id), $def, $min, $max)),*];
        static mut VALUES: [i32; DEFS.len()] = [$($def),*];
    };
}

params! {
    AspDelta = 20, 8, 50;
    RfpMargin = 78, 40, 150;
    RfpDepth = 8, 4, 12;
    RazorMargin = 246, 100, 500;
    NmpBase = 3, 2, 5;
    NmpDepthDiv = 3, 2, 6;
    NmpEvalDiv = 206, 100, 400;
    ProbcutMargin = 191, 100, 350;
    LmpBase = 4, 2, 8;
    FutBase = 127, 50, 250;
    FutMul = 82, 40, 160;
    HistPrune = 1791, 512, 8192;
    SeeQuiet = 51, 20, 100;
    SeeNoisy = 30, 10, 60;
    SeMul16 = 32, 8, 64;
    SeDouble = 20, 5, 50;
    LmrBase100 = 77, 30, 130;
    LmrDiv100 = 233, 150, 350;
    LmrHistDiv = 7722, 2048, 16384;
    HistBonusMul = 155, 50, 300;
    HistBonusMax = 1572, 500, 3000;
    QsFutility = 149, 50, 300;
    TmSoftPct = 60, 30, 90;
    CorrPawnW = 252, 64, 512;
    CorrNonPawnW = 98, 0, 384;
    CorrContW = 92, 0, 384;
    TtPvLmr = 1, 0, 2;
    DeeperMargin = 41, 10, 100;
    TmForcedDepth = 4, 0, 8;
    // Evaluation experiments (Oct 24): identity defaults.
    SprEvalScale = 100, 50, 200;
    SprMatScale = 0, 0, 1024;
    SprOptimism = 0, 0, 200;
    // Search sprint 1 (Oct 19): each 0 = off.
    SprFhDepth = 0, 0, 1;
    SprCaptSeeDiv = 0, 0, 256;
    SprCutNoTt = 0, 0, 2;
    SprSeCutNeg = 2, 0, 2;
    SprLmrRes = 64, 0, 128;
    SprTtCaptLmr = 0, 0, 2;
    // Search sprint 2: each 0 = off.
    SprTtHist = 0, 0, 1;
    SprQsEvas = 0, 0, 1;
    SprAspFh = 1, 0, 1;
    CorrMinorW = 0, 0, 384;
    CorrMajorW = 0, 0, 384;
    // Search sprint 3 (ideas from Codex's review): each 0 = off.
    SprNmpVerify = 12, 0, 20;
    SprCorrLmr = 64, 0, 512;
    SprLazyScore = 0, 0, 1;
    // Search sprint 4: each 0 = off.
    SprHindsight = 1, 0, 1;
    SprCaptFut = 0, 0, 400;
}

static mut GENERATION: u32 = 0;

/// The current value of a parameter.
#[inline(always)]
pub fn p(x: P) -> i32 {
    unsafe { VALUES[x as usize] }
}

/// Changes a parameter by (case-insensitive) name; false if there is no such parameter.
pub fn set(name: &str, value: i32) -> bool {
    match DEFS.iter().position(|d| d.0.eq_ignore_ascii_case(name)) {
        Some(i) => {
            unsafe {
                VALUES[i] = value.clamp(DEFS[i].2, DEFS[i].3);
                GENERATION += 1;
            }
            true
        }
        None => false,
    }
}

/// Bumped on every change, so tables derived from the parameters know to rebuild.
pub fn generation() -> u32 {
    unsafe { GENERATION }
}
