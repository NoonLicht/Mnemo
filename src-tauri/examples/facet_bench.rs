//! Facet-count latency harness.
//!
//! Usage: `cargo run --release --example facet_bench -- <results> [runs]`
//!
//! Builds a synthetic matched set with shared parent directories and reports
//! p50/p95 for [`mnemo_lib::search::compute_facets`]. The K12 decision memo
//! fixed the content tier; this covers the post-ranking facet cost at 100k and
//! 1M results.

use mnemo_lib::search::{MatchType, SearchResult, compute_facets};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn corpus(size: usize) -> Vec<SearchResult> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    (0..size)
        .map(|index| {
            let extension = match index % 6 {
                0 => "pdf",
                1 => "png",
                2 => "rs",
                3 => "md",
                4 => "mp4",
                _ => "zip",
            };
            SearchResult {
                path: format!(
                    "/corpus/d{:04}/file{:07}.{}",
                    index % 1000,
                    index,
                    extension
                ),
                filename: format!("file{:07}.{}", index, extension),
                snippet: None,
                score: 1.0,
                file_size: (index as u64 % 200) * 1_048_576,
                modified: now.saturating_sub((index as u64 % 400) * 86_400),
                is_dir: false,
                match_type: MatchType::FileName,
                best_range: None,
                group_count: 1,
                group_paths: Vec::new(),
            }
        })
        .collect()
}

fn percentile(sorted: &[u128], percentile: f64) -> u128 {
    let index = ((sorted.len() as f64 - 1.0) * percentile).round() as usize;
    sorted[index]
}

fn main() {
    let size = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100_000);
    let runs = std::env::args()
        .nth(2)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(20)
        .max(1);

    let started = Instant::now();
    let results = corpus(size);
    println!("built {size} synthetic results in {:?}", started.elapsed());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    // Warm caches and the allocator.
    let facets = compute_facets(&results, now);
    assert_eq!(facets.all, size);

    let mut timings: Vec<u128> = (0..runs)
        .map(|_| {
            let started = Instant::now();
            let facets = compute_facets(&results, now);
            std::hint::black_box(facets);
            started.elapsed().as_micros()
        })
        .collect();
    timings.sort_unstable();
    println!(
        "compute_facets: p50 {} µs · p95 {} µs ({} runs)",
        percentile(&timings, 0.50),
        percentile(&timings, 0.95),
        timings.len()
    );
    println!(
        "buckets: types={} languages={} places={} time={} size={}",
        facets.types.len(),
        facets.languages.len(),
        facets.places.len(),
        facets.time.len(),
        facets.size.len()
    );
}
