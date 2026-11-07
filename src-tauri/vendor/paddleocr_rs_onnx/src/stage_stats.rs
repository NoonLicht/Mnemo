//! Mnemo patch: cumulative per-stage wall time, for finding where OCR time goes.
//! Costs two atomic adds per stage; read with [`take`].
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub const DET_PRE: usize = 0;
pub const DET_RUN: usize = 1;
pub const DET_POST: usize = 2;
pub const REC_PRE: usize = 3;
pub const REC_RUN: usize = 4;
pub const REC_DEC: usize = 5;
const NAMES: [&str; 6] = [
    "det preprocess",
    "det inference",
    "det postprocess",
    "rec crop+normalise",
    "rec inference",
    "rec decode",
];
static NS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

pub fn add(stage: usize, started: Instant) {
    NS[stage].fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
}

/// Returns and clears the totals as `(name, seconds)`.
pub fn take() -> Vec<(&'static str, f64)> {
    NAMES
        .iter()
        .enumerate()
        .map(|(i, n)| (*n, NS[i].swap(0, Ordering::Relaxed) as f64 / 1e9))
        .collect()
}
