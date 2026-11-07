//! Local speed report: how long each stage of a scan and of a search took,
//! per drive, with the drive type (NVMe SSD, SATA SSD, HDD).
//!
//! Everything stays on this machine. Records are kept in memory and appended
//! to JSON-lines files under `<config>/perf/`, so a report can be produced
//! after a restart and pasted into an issue. Search records never contain the
//! query text, only its length.

use crate::config::MnemoConfig;
use crate::util::drive::{self, DriveInfo};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_SEARCHES: usize = 500;
const MAX_SCANS: usize = 10;
/// A perf file past this size is restarted rather than grown without bound.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StageMs {
    pub name: String,
    pub ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchPerf {
    pub at_unix: u64,
    /// Length of the query in characters. The text itself is never recorded.
    pub query_chars: usize,
    pub mode: String,
    pub results: usize,
    pub indexed_entries: usize,
    pub total_ms: f64,
    pub stages: Vec<StageMs>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RootPerf {
    pub path: String,
    pub drive: DriveInfoRec,
    pub walk_ms: f64,
    pub walk_entries: u64,
    pub extract_files: u64,
    /// Time spent reading and extracting, summed over all worker threads.
    pub extract_read_ms: f64,
    pub extract_bytes: u64,
    pub image_files: u64,
    pub image_ms: f64,
}

/// Serializable copy of [`DriveInfo`] (the util type is serialize-only).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DriveInfoRec {
    pub root: String,
    pub bus: String,
    pub media: String,
    pub model: String,
}

impl From<DriveInfo> for DriveInfoRec {
    fn from(d: DriveInfo) -> Self {
        Self {
            root: d.root,
            bus: d.bus,
            media: d.media,
            model: d.model,
        }
    }
}

impl DriveInfoRec {
    pub fn label(&self) -> String {
        DriveInfo {
            root: self.root.clone(),
            bus: self.bus.clone(),
            media: self.media.clone(),
            model: self.model.clone(),
        }
        .label()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScanPerf {
    pub at_unix: u64,
    pub total_ms: f64,
    pub walk_ms: f64,
    pub metadata_ms: f64,
    pub extract_ms: f64,
    pub finalize_ms: f64,
    pub entries: u64,
    pub extract_jobs: u64,
    pub roots: Vec<RootPerf>,
}

#[derive(Default)]
struct State {
    loaded: bool,
    searches: VecDeque<SearchPerf>,
    scans: VecDeque<ScanPerf>,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> std::sync::MutexGuard<'static, State> {
    let m = STATE.get_or_init(|| Mutex::new(State::default()));
    let mut guard = m.lock().unwrap_or_else(|e| e.into_inner());
    if !guard.loaded {
        guard.loaded = true;
        guard.searches = load_tail(&file_path("searches.jsonl"), MAX_SEARCHES);
        guard.scans = load_tail(&file_path("scans.jsonl"), MAX_SCANS);
    }
    guard
}

fn perf_dir() -> PathBuf {
    MnemoConfig::config_dir().join("perf")
}

fn file_path(name: &str) -> PathBuf {
    perf_dir().join(name)
}

fn load_tail<T: for<'de> Deserialize<'de>>(path: &Path, keep: usize) -> VecDeque<T> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return VecDeque::new();
    };
    let mut out: VecDeque<T> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    while out.len() > keep {
        out.pop_front();
    }
    out
}

fn append_line<T: Serialize>(path: &Path, record: &T) {
    // Unit tests run searches against throwaway indexes; their timings do not
    // belong in the user's real history.
    if cfg!(test) {
        return;
    }
    let Ok(line) = serde_json::to_string(record) else {
        return;
    };
    let _ = std::fs::create_dir_all(perf_dir());
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
        let _ = std::fs::remove_file(path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Times the stages of one search. `mark` closes the stage that just ended.
pub struct StageClock {
    started: Instant,
    last: Instant,
    stages: Vec<StageMs>,
}

impl StageClock {
    pub fn start() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last: now,
            stages: Vec::with_capacity(10),
        }
    }

    pub fn mark(&mut self, name: &str) {
        let now = Instant::now();
        self.stages.push(StageMs {
            name: name.to_string(),
            ms: ms(now - self.last),
        });
        self.last = now;
    }

    /// Records the search and returns its total time in milliseconds.
    pub fn finish(
        mut self,
        mode: &str,
        query_chars: usize,
        results: usize,
        indexed_entries: usize,
    ) -> f64 {
        self.mark("tail");
        let total_ms = ms(self.started.elapsed());
        let record = SearchPerf {
            at_unix: now_unix(),
            query_chars,
            mode: mode.to_string(),
            results,
            indexed_entries,
            total_ms,
            stages: self.stages,
        };
        append_line(&file_path("searches.jsonl"), &record);
        let mut st = state();
        st.searches.push_back(record);
        while st.searches.len() > MAX_SEARCHES {
            st.searches.pop_front();
        }
        total_ms
    }
}

#[derive(Default)]
struct RootAcc {
    files: AtomicU64,
    bytes: AtomicU64,
    read_ns: AtomicU64,
    image_files: AtomicU64,
    image_ns: AtomicU64,
}

/// Collects per-drive extraction cost from the worker threads of one scan.
pub struct ScanAccumulator {
    roots: Vec<PathBuf>,
    per: Vec<RootAcc>,
}

impl ScanAccumulator {
    pub fn new(roots: &[PathBuf]) -> Self {
        Self {
            roots: roots.to_vec(),
            per: roots.iter().map(|_| RootAcc::default()).collect(),
        }
    }

    /// Index of the most specific watched root containing `path`.
    fn root_of(&self, path: &Path) -> Option<usize> {
        self.roots
            .iter()
            .enumerate()
            .filter(|(_, r)| path.starts_with(r))
            .max_by_key(|(_, r)| r.components().count())
            .map(|(i, _)| i)
    }

    pub fn record(&self, path: &Path, is_image: bool, bytes: u64, took: Duration) {
        let Some(i) = self.root_of(path) else {
            return;
        };
        let acc = &self.per[i];
        let ns = took.as_nanos().min(u64::MAX as u128) as u64;
        acc.files.fetch_add(1, Ordering::Relaxed);
        acc.bytes.fetch_add(bytes, Ordering::Relaxed);
        acc.read_ns.fetch_add(ns, Ordering::Relaxed);
        if is_image {
            acc.image_files.fetch_add(1, Ordering::Relaxed);
            acc.image_ns.fetch_add(ns, Ordering::Relaxed);
        }
    }

    /// Per-root rows, with the walk timings supplied by the caller.
    pub fn into_roots(&self, walks: &[(PathBuf, Duration, u64)]) -> Vec<RootPerf> {
        self.roots
            .iter()
            .zip(self.per.iter())
            .map(|(root, acc)| {
                let (walk_ms, walk_entries) = walks
                    .iter()
                    .find(|(p, _, _)| p == root)
                    .map(|(_, d, n)| (ms(*d), *n))
                    .unwrap_or((0.0, 0));
                RootPerf {
                    path: root.to_string_lossy().into_owned(),
                    drive: drive::describe(root).into(),
                    walk_ms,
                    walk_entries,
                    extract_files: acc.files.load(Ordering::Relaxed),
                    extract_read_ms: acc.read_ns.load(Ordering::Relaxed) as f64 / 1e6,
                    extract_bytes: acc.bytes.load(Ordering::Relaxed),
                    image_files: acc.image_files.load(Ordering::Relaxed),
                    image_ms: acc.image_ns.load(Ordering::Relaxed) as f64 / 1e6,
                }
            })
            .collect()
    }
}

pub fn record_scan(scan: ScanPerf) {
    append_line(&file_path("scans.jsonl"), &scan);
    let mut st = state();
    st.scans.push_back(scan);
    while st.scans.len() > MAX_SCANS {
        st.scans.pop_front();
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn fmt_ms(v: f64) -> String {
    if v >= 10_000.0 {
        format!("{:.1} s", v / 1000.0)
    } else if v >= 100.0 {
        format!("{v:.0} ms")
    } else {
        format!("{v:.1} ms")
    }
}

fn fmt_mb(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_048_576.0)
}

fn rate(count: u64, total_ms: f64) -> String {
    if total_ms <= 0.0 {
        "n/a".to_string()
    } else {
        format!("{:.0}/s", count as f64 / (total_ms / 1000.0))
    }
}

/// Markdown report ready to paste into a GitHub issue or README.
pub fn report_markdown() -> String {
    let (searches, scans) = {
        let st = state();
        (
            st.searches.iter().cloned().collect::<Vec<_>>(),
            st.scans.iter().cloned().collect::<Vec<_>>(),
        )
    };
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let index_drive = drive::describe(&MnemoConfig::config_dir());
    let mut out = String::new();
    out.push_str("## Mnemo speed report\n\n");
    out.push_str(&format!(
        "- Mnemo {}, {} {}, {} logical CPUs\n- Index stored on: {} ({})\n\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        threads,
        index_drive.label(),
        if index_drive.model.is_empty() {
            "model unknown"
        } else {
            index_drive.model.as_str()
        }
    ));

    match scans.last() {
        None => out.push_str("### Last scan\n\nNo scan recorded yet. Run an index first.\n\n"),
        Some(scan) => {
            out.push_str("### Last scan\n\n");
            out.push_str("| Stage | Time |\n|---|---|\n");
            for (name, v) in [
                ("Walk all folders (incl. ignore-file rules)", scan.walk_ms),
                ("Metadata and change detection", scan.metadata_ms),
                ("Extract text / OCR and index", scan.extract_ms),
                ("Commit and clean up", scan.finalize_ms),
                ("**Total**", scan.total_ms),
            ] {
                out.push_str(&format!("| {name} | {} |\n", fmt_ms(v)));
            }
            out.push_str(&format!(
                "\n{} entries found, {} files sent to extraction.\n\n",
                scan.entries, scan.extract_jobs
            ));
            out.push_str("#### Per drive\n\n");
            out.push_str(
                "| Folder | Drive type | Model | Entries | Walk | Walk speed | Files read | Read time\u{b9} | Data | Images | Image time\u{b9} |\n|---|---|---|---|---|---|---|---|---|---|---|\n",
            );
            for r in &scan.roots {
                out.push_str(&format!(
                    "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                    r.path,
                    r.drive.label(),
                    if r.drive.model.is_empty() {
                        "-"
                    } else {
                        r.drive.model.as_str()
                    },
                    r.walk_entries,
                    fmt_ms(r.walk_ms),
                    rate(r.walk_entries, r.walk_ms),
                    r.extract_files,
                    fmt_ms(r.extract_read_ms),
                    fmt_mb(r.extract_bytes),
                    r.image_files,
                    fmt_ms(r.image_ms),
                ));
            }
            out.push_str(
                "\n\u{b9} Summed over all worker threads, so it can exceed the wall-clock time of the stage.\n\n",
            );
        }
    }

    out.push_str("### Search latency\n\n");
    if searches.is_empty() {
        out.push_str("No searches recorded yet.\n");
        return out;
    }
    let mut totals: Vec<f64> = searches.iter().map(|s| s.total_ms).collect();
    totals.sort_by(|a, b| a.total_cmp(b));
    out.push_str(&format!(
        "{} searches over {} indexed entries. Total: p50 {}, p95 {}, max {}.\n\n",
        searches.len(),
        searches.last().map(|s| s.indexed_entries).unwrap_or(0),
        fmt_ms(percentile(&totals, 50.0)),
        fmt_ms(percentile(&totals, 95.0)),
        fmt_ms(totals.last().copied().unwrap_or(0.0)),
    ));
    out.push_str("| Stage | p50 | p95 | max |\n|---|---|---|---|\n");
    let mut names: Vec<String> = Vec::new();
    for s in &searches {
        for st in &s.stages {
            if !names.contains(&st.name) {
                names.push(st.name.clone());
            }
        }
    }
    for name in &names {
        let mut v: Vec<f64> = searches
            .iter()
            .filter_map(|s| s.stages.iter().find(|st| &st.name == name).map(|st| st.ms))
            .collect();
        v.sort_by(|a, b| a.total_cmp(b));
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            name,
            fmt_ms(percentile(&v, 50.0)),
            fmt_ms(percentile(&v, 95.0)),
            fmt_ms(v.last().copied().unwrap_or(0.0)),
        ));
    }
    out.push_str("\nQuery text is never recorded, only its length.\n");
    out
}

/// Deletes the recorded history (memory and files).
pub fn clear() {
    let mut st = state();
    st.searches.clear();
    st.scans.clear();
    let _ = std::fs::remove_file(file_path("searches.jsonl"));
    let _ = std::fs::remove_file(file_path("scans.jsonl"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_handles_edges() {
        assert_eq!(percentile(&[], 50.0), 0.0);
        assert_eq!(percentile(&[5.0], 95.0), 5.0);
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), 51.0);
        assert_eq!(percentile(&v, 100.0), 100.0);
    }

    #[test]
    fn accumulator_attributes_files_to_the_most_specific_root() {
        let (outer, inner, file) = if cfg!(windows) {
            (r"C:\data", r"C:\data\deep", r"C:\data\deep\a.txt")
        } else {
            ("/data", "/data/deep", "/data/deep/a.txt")
        };
        let acc = ScanAccumulator::new(&[PathBuf::from(outer), PathBuf::from(inner)]);
        acc.record(Path::new(file), false, 100, Duration::from_millis(5));
        acc.record(Path::new(file), true, 50, Duration::from_millis(20));
        let rows = acc.into_roots(&[]);
        assert_eq!(rows[0].extract_files, 0);
        assert_eq!(rows[1].extract_files, 2);
        assert_eq!(rows[1].extract_bytes, 150);
        assert_eq!(rows[1].image_files, 1);
        assert!(rows[1].image_ms >= 20.0);
    }

    #[test]
    fn stage_clock_records_each_mark_in_order() {
        let mut clock = StageClock::start();
        clock.mark("a");
        clock.mark("b");
        assert_eq!(clock.stages.len(), 2);
        assert_eq!(clock.stages[0].name, "a");
        assert_eq!(clock.stages[1].name, "b");
    }

    #[test]
    fn formatting_is_stable() {
        assert_eq!(fmt_ms(0.5), "0.5 ms");
        assert_eq!(fmt_ms(250.0), "250 ms");
        assert_eq!(fmt_ms(12_000.0), "12.0 s");
        assert_eq!(rate(1000, 0.0), "n/a");
        assert_eq!(rate(1000, 2000.0), "500/s");
    }
}
