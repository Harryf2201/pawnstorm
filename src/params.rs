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
    RfpMargin = 75, 40, 150;
    RfpDepth = 8, 4, 12;
    RazorMargin = 250, 100, 500;
    NmpBase = 3, 2, 5;
    NmpDepthDiv = 3, 2, 6;
    NmpEvalDiv = 200, 100, 400;
    ProbcutMargin = 200, 100, 350;
    LmpBase = 4, 2, 8;
    FutBase = 120, 50, 250;
    FutMul = 80, 40, 160;
    HistPrune = 2048, 512, 8192;
    SeeQuiet = 50, 20, 100;
    SeeNoisy = 30, 10, 60;
    SeMul16 = 32, 8, 64;
    SeDouble = 20, 5, 50;
    LmrBase100 = 77, 30, 130;
    LmrDiv100 = 236, 150, 350;
    LmrHistDiv = 8192, 2048, 16384;
    HistBonusMul = 150, 50, 300;
    HistBonusMax = 1500, 500, 3000;
    QsFutility = 150, 50, 300;
    TmSoftPct = 60, 30, 90;
    CorrPawnW = 256, 64, 512;
    CorrNonPawnW = 96, 0, 384;
    CorrContW = 96, 0, 384;
    TtPvLmr = 1, 0, 2;
    DeeperMargin = 40, 10, 100;
    TmForcedDepth = 4, 0, 8;
    // Search sprint 1 (Oct 19): each 0 = off.
    SprFhDepth = 0, 0, 1;
    SprCaptSeeDiv = 0, 0, 256;
    SprCutNoTt = 0, 0, 2;
    SprSeCutNeg = 0, 0, 2;
    SprLmrRes = 0, 0, 128;
    SprTtCaptLmr = 0, 0, 2;
    // Search sprint 2: each 0 = off.
    SprTtHist = 0, 0, 1;
    SprQsEvas = 0, 0, 1;
    SprAspFh = 0, 0, 1;
    CorrMinorW = 0, 0, 384;
    CorrMajorW = 0, 0, 384;
    // Search sprint 3 (ideas from Codex's review): each 0 = off.
    SprNmpVerify = 0, 0, 20;
    SprCorrLmr = 0, 0, 512;
    SprLazyScore = 0, 0, 1;
    // Search sprint 4: each 0 = off.
    SprHindsight = 0, 0, 1;
    SprCaptFut = 0, 0, 400;
    // Search sprint 5: each 0 = off.
    SprEvalHist = 0, 0, 20;
    SprTtProbcut = 0, 0, 800;
    SprCutCnt = 0, 0, 2;
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
