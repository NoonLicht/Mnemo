//! Per-watched-folder statistics, computed from the in-memory name index.
//!
//! Settings → Folders shows how many files each watched root actually has in
//! the index, so a root that was never walked (disk asleep, unplugged, access
//! denied) is visible at a glance instead of being hidden in a global total.
//!
//! Roots can nest (`C:\` and `C:\Users` both watched). Counts are inclusive:
//! a file under both is counted for both, so each row answers "how much of
//! this folder is indexed".

use crate::indexer::arena::NO_PARENT;
use crate::indexer::metadata::MetadataStore;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// At most this many roots are tracked (one bit each in a `u64` mask).
const MAX_ROOTS: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RootStat {
    pub path: String,
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    /// Files whose text/OCR extraction is still queued.
    pub pending: u64,
    /// False when the root path does not exist right now (unplugged disk).
    pub available: bool,
}

/// Normalises a path string for comparison: separators unified, trailing
/// separator removed, and case folded on Windows (NTFS is case-insensitive).
fn norm(path: &str) -> String {
    let unified = if cfg!(windows) {
        path.replace('/', "\\")
    } else {
        path.to_string()
    };
    let trimmed = unified.trim_end_matches(['\\', '/']);
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_string()
    }
}

/// Last path component of a root, normalised. Drive roots (`C:\`) have none,
/// so the whole normalised string stands in for it.
fn tail_key(root_norm: &str) -> String {
    root_norm
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(root_norm)
        .to_string()
}

/// Counts indexed entries per watched root. Cost is one pass over the arena
/// plus one path build per directory whose name could be a root.
pub fn compute(metadata: &MetadataStore, roots: &[PathBuf]) -> Vec<RootStat> {
    let roots = &roots[..roots.len().min(MAX_ROOTS)];
    let keys: Vec<String> = roots.iter().map(|r| norm(&r.to_string_lossy())).collect();
    let tails: HashSet<String> = keys.iter().map(|k| tail_key(k)).collect();

    let n = metadata.len();
    let mut masks = vec![0u64; n];
    let mut done = vec![false; n];
    let mut stack: Vec<u32> = Vec::new();

    for start in 0..n {
        if done[start] {
            continue;
        }
        stack.clear();
        let mut cur = start as u32;
        let mut base = 0u64;
        loop {
            if done[cur as usize] {
                base = masks[cur as usize];
                break;
            }
            stack.push(cur);
            let parent = metadata.node(cur).parent;
            if parent == NO_PARENT || parent as usize >= n {
                break;
            }
            cur = parent;
        }
        for &idx in stack.iter().rev() {
            let node = metadata.node(idx);
            let mut mask = base;
            if node.is_dir() {
                let name_hit = node.parent == NO_PARENT || {
                    let name = metadata.name(idx);
                    let folded = if cfg!(windows) {
                        name.to_lowercase()
                    } else {
                        name.to_string()
                    };
                    tails.contains(norm(&folded).as_str()) || tails.contains(folded.as_str())
                };
                if name_hit {
                    let full = norm(&metadata.path(idx).to_string_lossy());
                    for (bit, key) in keys.iter().enumerate() {
                        if &full == key {
                            mask |= 1u64 << bit;
                        }
                    }
                }
            }
            masks[idx as usize] = mask;
            done[idx as usize] = true;
            base = mask;
        }
    }

    let mut stats: Vec<RootStat> = roots
        .iter()
        .map(|r| RootStat {
            path: r.to_string_lossy().into_owned(),
            available: Path::new(r).exists(),
            ..RootStat::default()
        })
        .collect();

    for (i, &mask) in masks.iter().enumerate() {
        if mask == 0 {
            continue;
        }
        let node = metadata.node(i as u32);
        let mut bits = mask;
        while bits != 0 {
            let bit = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let stat = &mut stats[bit];
            if node.is_dir() {
                stat.dirs += 1;
            } else {
                stat.files += 1;
                stat.bytes += node.size;
                if node.content_pending() {
                    stat.pending += 1;
                }
            }
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::walker::FileEntry;

    fn entry(path: &str, is_dir: bool, size: u64) -> FileEntry {
        let p = PathBuf::from(path);
        FileEntry {
            id: 0,
            name: p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            extension: None,
            path: p,
            size,
            modified: 1,
            is_dir,
            dev: 0,
            ino: 0,
            char_bloom: 0,
            content_pending: false,
        }
    }

    #[test]
    fn counts_files_per_root_and_nested_roots_are_inclusive() {
        let mut store = MetadataStore::without_db();
        let (a, b) = if cfg!(windows) {
            ("C:\\data\\a", "C:\\data\\b")
        } else {
            ("/data/a", "/data/b")
        };
        let sep = std::path::MAIN_SEPARATOR;
        // The walker emits every directory, parents first.
        store.add(entry(
            if cfg!(windows) { r"C:\data" } else { "/data" },
            true,
            0,
        ));
        for (dir, count) in [(a, 3u64), (b, 2u64)] {
            store.add(entry(dir, true, 0));
            for i in 0..count {
                store.add(entry(&format!("{dir}{sep}f{i}.txt"), false, 10));
            }
        }
        let roots = vec![
            PathBuf::from(a),
            PathBuf::from(b),
            PathBuf::from(if cfg!(windows) { "C:\\data" } else { "/data" }),
        ];
        let stats = compute(&store, &roots);
        assert_eq!(stats[0].files, 3);
        assert_eq!(stats[0].bytes, 30);
        assert_eq!(stats[1].files, 2);
        // The parent root contains both children.
        assert_eq!(stats[2].files, 5);
    }

    #[test]
    fn unwatched_files_are_not_counted_and_missing_root_is_flagged() {
        let mut store = MetadataStore::without_db();
        let (watched, other) = if cfg!(windows) {
            ("C:\\watched", "C:\\other")
        } else {
            ("/watched", "/other")
        };
        let sep = std::path::MAIN_SEPARATOR;
        store.add(entry(watched, true, 0));
        store.add(entry(&format!("{watched}{sep}x.txt"), false, 5));
        store.add(entry(other, true, 0));
        store.add(entry(&format!("{other}{sep}y.txt"), false, 7));
        let stats = compute(&store, &[PathBuf::from(watched)]);
        assert_eq!(stats[0].files, 1);
        assert_eq!(stats[0].bytes, 5);
        assert!(!stats[0].available || Path::new(watched).exists());
    }

    #[test]
    fn large_index_is_counted_quickly() {
        let mut store = MetadataStore::without_db();
        let sep = std::path::MAIN_SEPARATOR;
        let base = if cfg!(windows) { "C:\\big" } else { "/big" };
        store.add(entry(base, true, 0));
        for d in 0..2000 {
            let dir = format!("{base}{sep}d{d}");
            store.add(entry(&dir, true, 0));
            for f in 0..100 {
                store.add(entry(&format!("{dir}{sep}f{f}.txt"), false, 1));
            }
        }
        let started = std::time::Instant::now();
        let stats = compute(&store, &[PathBuf::from(base)]);
        let elapsed = started.elapsed();
        assert_eq!(stats[0].files, 200_000);
        assert!(elapsed.as_secs() < 5, "took {elapsed:?}");
        eprintln!("root stats for 200k files / 2k dirs: {elapsed:?}");
    }
}
