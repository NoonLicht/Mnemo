//! End-to-end crawl benchmark.
//!
//! Builds a throwaway index in an isolated config directory, runs a fresh crawl
//! and an incremental crawl over the given fixture, and reports wall time,
//! counts, and peak RSS. Linux only (uses `/proc/self/status` for RSS).
//!
//! ```bash
//! cargo run --example crawl_bench --release -- ~/Pictures
//! ```

use mnemo_lib::config::MnemoConfig;
use mnemo_lib::indexer::IndexerManager;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn peak_rss_mb() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .map(|kb| kb / 1024)
}

fn crawl(manager: &IndexerManager) -> (Duration, mnemo_lib::indexer::IndexingStatus) {
    let t = Instant::now();
    manager.start_indexing();
    // Let the crawl thread publish its first status.
    std::thread::sleep(Duration::from_millis(200));
    while manager.get_status().is_indexing {
        std::thread::sleep(Duration::from_millis(100));
    }
    (t.elapsed(), manager.get_status())
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("fixture dir"));
    let config_dir = std::env::temp_dir().join(format!("mnemo-crawl-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&config_dir);
    // SAFETY: single-threaded setup before any config or index code runs.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &config_dir);
    }

    let config = MnemoConfig {
        watched_dirs: vec![root],
        semantic_enabled: true,
        ocr_enabled: true,
        is_setup_complete: true,
        ..MnemoConfig::default()
    };

    let manager = IndexerManager::new(config);

    let (fresh, st) = crawl(&manager);
    println!(
        "fresh: {:.2}s files={} content={} units={} docs={} peak_rss={}",
        fresh.as_secs_f64(),
        st.files_indexed,
        st.content_indexed,
        st.units_indexed,
        manager
            .content_index
            .read()
            .as_ref()
            .map(|i| i.num_docs())
            .unwrap_or(0),
        peak_rss_mb()
            .map(|mb| format!("{mb} MB"))
            .unwrap_or_else(|| "n/a".to_string())
    );

    let (incremental, st) = crawl(&manager);
    println!(
        "incremental: {:.3}s files={} content={} units={} peak_rss={}",
        incremental.as_secs_f64(),
        st.files_indexed,
        st.content_indexed,
        st.units_indexed,
        peak_rss_mb()
            .map(|mb| format!("{mb} MB"))
            .unwrap_or_else(|| "n/a".to_string())
    );

    drop(manager);
    let _ = std::fs::remove_dir_all(&config_dir);
}
