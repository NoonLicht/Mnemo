//! Owner-run speed probe. Crawls the folders in `MNEMO_PROBE_ROOTS` (separated
//! by `;`) into a throwaway index, runs a few searches and prints the same
//! Markdown report the app shows in Settings. The real index is never touched.
//!
//! ```text
//! set MNEMO_PROBE_ROOTS=C:\Projects;D:\;F:\
//! cargo test --release --test perf_probe -- --ignored --nocapture
//! ```

use mnemo_lib::config::MnemoConfig;
use mnemo_lib::indexer::IndexerManager;
use mnemo_lib::search::SearchDispatcher;
use std::path::PathBuf;
use std::time::Duration;

#[test]
#[ignore = "owner-run: needs MNEMO_PROBE_ROOTS and reads the listed folders"]
fn probe_speed_report() {
    let roots: Vec<PathBuf> = std::env::var("MNEMO_PROBE_ROOTS")
        .expect("MNEMO_PROBE_ROOTS=folder;folder")
        .split(';')
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .collect();
    let data_dir = std::env::temp_dir().join(format!("mnemo-perf-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::fs::create_dir_all(&data_dir).unwrap();
    // SAFETY: this is the only test in the binary and sets the variable before
    // any config or index code reads it.
    unsafe { std::env::set_var("MNEMO_DATA_DIR", &data_dir) };

    let config = MnemoConfig {
        watched_dirs: roots,
        semantic_enabled: false,
        ocr_enabled: false,
        is_setup_complete: true,
        ..MnemoConfig::default()
    };
    let manager = IndexerManager::new(config);
    manager.start_indexing();
    std::thread::sleep(Duration::from_millis(300));
    while manager.get_status().is_indexing {
        std::thread::sleep(Duration::from_millis(200));
    }
    let status = manager.get_status();
    eprintln!(
        "crawl finished: {} entries, {} with full text",
        status.files_indexed, status.content_indexed
    );

    let queries = [
        "readme",
        "config json",
        "invoice",
        "screenshot png",
        "def main",
        "ext:py import",
        "report",
    ];
    for round in 0..3 {
        for (i, q) in queries.iter().enumerate() {
            let metadata = manager.metadata.read();
            let content = manager.content_index.read();
            let _ = SearchDispatcher::search_response(
                q,
                &metadata,
                &content,
                None,
                false,
                50,
                (round * queries.len() + i) as u64,
            );
        }
    }
    println!("{}", mnemo_lib::perf::report_markdown());
    drop(manager);
    let _ = std::fs::remove_dir_all(&data_dir);
}
