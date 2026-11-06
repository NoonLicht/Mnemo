//! Times `walk_directories` alone on one folder: `walkbench <dir>`.
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("usage: walkbench <dir>"));
    let t = Instant::now();
    let entries = mnemo_lib::indexer::walker::walk_directories(&[root], &[], true);
    let secs = t.elapsed().as_secs_f64();
    println!(
        "{} entries in {:.2}s = {:.0}/s",
        entries.len(),
        secs,
        entries.len() as f64 / secs
    );
}
