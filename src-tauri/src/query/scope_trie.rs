use crate::indexer::walker::FileEntry;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Default)]
struct TrieNode {
    children: HashMap<char, TrieNode>,
    dir_indices: Vec<usize>,
}

type ScopeCandidateCache = Mutex<HashMap<(usize, u64), Arc<Vec<usize>>>>;

pub struct DirectoryTrie {
    root: TrieNode,
    // Cached mapping from parent directory index to children indices
    children_map: HashMap<usize, Vec<usize>>,
    // Cache for scope descendant candidate lists: (dir_idx, generation) -> Arc<Vec<usize>>
    scope_cache: ScopeCandidateCache,
    generation: u64,
}

#[derive(Debug, Clone)]
pub struct ScopeResolution {
    pub dir_idx: usize,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub enum TrieMatchResult {
    Single(ScopeResolution),
    Ambiguous(Vec<ScopeResolution>),
    None,
}

impl DirectoryTrie {
    pub fn build(
        entries: &[FileEntry],
        path_index: &HashMap<PathBuf, usize>,
        generation: u64,
    ) -> Self {
        let mut root = TrieNode::default();
        let mut children_map: HashMap<usize, Vec<usize>> = HashMap::new();

        // 1. Build children map for DFS
        for (idx, entry) in entries.iter().enumerate() {
            if let Some(parent_path) = entry.path.parent()
                && let Some(&parent_idx) = path_index.get(parent_path)
            {
                children_map.entry(parent_idx).or_default().push(idx);
            }
        }

        // 2. Insert all directory names into trie (folded to lowercase)
        for (idx, entry) in entries.iter().enumerate() {
            if entry.is_dir {
                let name_folded = entry.name.to_lowercase();
                let mut current = &mut root;
                for ch in name_folded.chars() {
                    current = current.children.entry(ch).or_default();
                }
                current.dir_indices.push(idx);
            }
        }

        Self {
            root,
            children_map,
            scope_cache: Mutex::new(HashMap::new()),
            generation,
        }
    }

    /// Resolve a directory-looking token or `in:` target: exact > prefix > fuzzy.
    pub fn resolve(
        &self,
        target: &str,
        entries: &[FileEntry],
        path_index: &HashMap<PathBuf, usize>,
    ) -> TrieMatchResult {
        let trimmed = target.trim();
        if trimmed.is_empty() {
            return TrieMatchResult::None;
        }

        // 1. If target looks like a path (starts with ~, ., / or has path separator)
        let expanded_path = if trimmed.starts_with('~') {
            if let Some(home) = dirs::home_dir() {
                let rest = trimmed
                    .trim_start_matches('~')
                    .trim_start_matches(['/', '\\']);
                if rest.is_empty() {
                    home
                } else {
                    home.join(rest)
                }
            } else {
                PathBuf::from(trimmed)
            }
        } else {
            PathBuf::from(trimmed)
        };

        if let Some(&idx) = path_index.get(&expanded_path)
            && entries.get(idx).map(|e| e.is_dir).unwrap_or(false)
        {
            return TrieMatchResult::Single(ScopeResolution {
                dir_idx: idx,
                path: entries[idx].path.clone(),
            });
        }

        // Try matching path suffix / relative path
        let target_norm = trimmed.replace('\\', "/").to_lowercase();
        let target_clean = target_norm.trim_end_matches('/');

        let mut path_matches = Vec::new();
        for (idx, entry) in entries.iter().enumerate() {
            if entry.is_dir {
                let entry_path_str = entry
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_lowercase();
                if entry_path_str.ends_with(target_clean) {
                    path_matches.push(ScopeResolution {
                        dir_idx: idx,
                        path: entry.path.clone(),
                    });
                }
            }
        }

        if path_matches.len() == 1 {
            return TrieMatchResult::Single(path_matches.remove(0));
        } else if path_matches.len() > 1 {
            return TrieMatchResult::Ambiguous(path_matches);
        }

        // 2. Trie match on directory name: exact > prefix > fuzzy
        let name_folded = target_clean.split('/').next_back().unwrap_or(target_clean);

        // 2a. Exact match
        if let Some(indices) = self.find_exact(name_folded) {
            if indices.len() == 1 {
                let idx = indices[0];
                return TrieMatchResult::Single(ScopeResolution {
                    dir_idx: idx,
                    path: entries[idx].path.clone(),
                });
            } else if indices.len() > 1 {
                let candidates = indices
                    .into_iter()
                    .map(|idx| ScopeResolution {
                        dir_idx: idx,
                        path: entries[idx].path.clone(),
                    })
                    .collect();
                return TrieMatchResult::Ambiguous(candidates);
            }
        }

        // 2b. Prefix match
        let prefix_indices = self.find_prefix(name_folded);
        if prefix_indices.len() == 1 {
            let idx = prefix_indices[0];
            return TrieMatchResult::Single(ScopeResolution {
                dir_idx: idx,
                path: entries[idx].path.clone(),
            });
        } else if prefix_indices.len() > 1 {
            let candidates = prefix_indices
                .into_iter()
                .map(|idx| ScopeResolution {
                    dir_idx: idx,
                    path: entries[idx].path.clone(),
                })
                .collect();
            return TrieMatchResult::Ambiguous(candidates);
        }

        // 2c. Subsequence / fuzzy match on directory names
        let mut fuzzy_matches = Vec::new();
        for (idx, entry) in entries.iter().enumerate() {
            if entry.is_dir {
                let entry_name_folded = entry.name.to_lowercase();
                if entry_name_folded.contains(name_folded) {
                    fuzzy_matches.push(ScopeResolution {
                        dir_idx: idx,
                        path: entry.path.clone(),
                    });
                }
            }
        }

        if fuzzy_matches.len() == 1 {
            TrieMatchResult::Single(fuzzy_matches.remove(0))
        } else if fuzzy_matches.len() > 1 {
            TrieMatchResult::Ambiguous(fuzzy_matches)
        } else {
            TrieMatchResult::None
        }
    }

    fn find_exact(&self, name_folded: &str) -> Option<Vec<usize>> {
        let mut current = &self.root;
        for ch in name_folded.chars() {
            current = current.children.get(&ch)?;
        }
        if current.dir_indices.is_empty() {
            None
        } else {
            Some(current.dir_indices.clone())
        }
    }

    fn find_prefix(&self, prefix_folded: &str) -> Vec<usize> {
        let mut current = &self.root;
        for ch in prefix_folded.chars() {
            match current.children.get(&ch) {
                Some(next) => current = next,
                None => return Vec::new(),
            }
        }
        let mut results = Vec::new();
        self.collect_all(current, &mut results);
        results
    }

    fn collect_all(&self, node: &TrieNode, out: &mut Vec<usize>) {
        out.extend_from_slice(&node.dir_indices);
        for child in node.children.values() {
            self.collect_all(child, out);
        }
    }

    /// Retrieve all descendant entry indices using DFS with generational cache.
    pub fn get_scope_candidates(&self, dir_idx: usize, current_gen: u64) -> Arc<Vec<usize>> {
        let key = (dir_idx, current_gen);
        {
            let cache = self.scope_cache.lock();
            if let Some(cached) = cache.get(&key) {
                return Arc::clone(cached);
            }
        }

        // Compute DFS traversal over child lists: O(subtree)
        let mut candidates = Vec::new();
        candidates.push(dir_idx);
        self.dfs_collect(dir_idx, &mut candidates);

        let arc = Arc::new(candidates);
        let mut cache = self.scope_cache.lock();
        // Evict stale generations if generation changed
        if self.generation != current_gen {
            cache.retain(|&(_, cached_generation), _| cached_generation == current_gen);
        }
        cache.insert(key, Arc::clone(&arc));
        arc
    }

    fn dfs_collect(&self, dir_idx: usize, out: &mut Vec<usize>) {
        if let Some(children) = self.children_map.get(&dir_idx) {
            for &child_idx in children {
                out.push(child_idx);
                self.dfs_collect(child_idx, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::walker::FileEntry;

    /// entries: absolute-ish dirs/files. Returns (entries, path_index).
    fn fixture(entries: &[(&str, bool)]) -> (Vec<FileEntry>, HashMap<PathBuf, usize>) {
        let mut list = Vec::new();
        let mut index = HashMap::new();
        for (i, (path, is_dir)) in entries.iter().enumerate() {
            let path = PathBuf::from(path);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            list.push(FileEntry {
                id: i as u64,
                path: path.clone(),
                name,
                extension: None,
                size: 0,
                modified: 0,
                is_dir: *is_dir,
                dev: 0,
                ino: 0,
                char_bloom: 0,
                content_pending: false,
            });
            index.insert(path, i);
        }
        (list, index)
    }

    fn sample() -> (Vec<FileEntry>, HashMap<PathBuf, usize>) {
        fixture(&[
            ("/home/user", true),
            ("/home/user/projects", true),
            ("/home/user/projects/mnemo", true),
            ("/home/user/projects/other", true),
            ("/home/user/projects/mnemo/src", true),
            ("/home/user/projects/mnemo/src/main.rs", false),
            ("/home/user/notes", true),
        ])
    }

    fn trie(entries: &[FileEntry], index: &HashMap<PathBuf, usize>) -> DirectoryTrie {
        DirectoryTrie::build(entries, index, 1)
    }

    fn dir_of(result: &TrieMatchResult) -> Option<usize> {
        match result {
            TrieMatchResult::Single(r) => Some(r.dir_idx),
            _ => None,
        }
    }

    #[test]
    fn resolve_exact_directory_name() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        assert_eq!(dir_of(&t.resolve("mnemo", &entries, &index)), Some(2));
        assert_eq!(dir_of(&t.resolve("notes", &entries, &index)), Some(6));
    }

    #[test]
    fn resolve_is_case_insensitive() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        // Trie lookup folds to lowercase via build().
        assert_eq!(dir_of(&t.resolve("MNEmo", &entries, &index)), Some(2));
    }

    #[test]
    fn resolve_ambiguous_exact_name_lists_all_candidates() {
        let (entries, index) = sample();
        let everything = fixture(&[
            ("/home/user", true),
            ("/home/user/src", true),
            ("/opt/src", true),
        ]);
        let sub_trie = DirectoryTrie::build(&everything.0, &everything.1, 1);
        match sub_trie.resolve("src", &everything.0, &everything.1) {
            TrieMatchResult::Ambiguous(c) => {
                assert_eq!(c.len(), 2);
                let dir_idxs: Vec<usize> = c.iter().map(|r| r.dir_idx).collect();
                assert!(dir_idxs.contains(&1) && dir_idxs.contains(&2));
            }
            other => panic!("expected ambiguous, got {other:?}"),
        }
        let _ = (entries, index);
    }

    #[test]
    fn resolve_unique_prefix_matches_single_directory() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        // "mne" prefix-matches only "mnemo".
        assert_eq!(dir_of(&t.resolve("mne", &entries, &index)), Some(2));
    }

    #[test]
    fn resolve_ambiguous_prefix_lists_candidates() {
        let (entries, index) = fixture(&[
            ("/w/proj-light", true),
            ("/w/proj-heavy", true),
            ("/w/baseline", true),
        ]);
        let t = DirectoryTrie::build(&entries, &index, 1);
        // "proj" prefix-matches two differently-suffixed dirs.
        match t.resolve("proj", &entries, &index) {
            TrieMatchResult::Ambiguous(c) => {
                assert_eq!(c.len(), 2);
                let dir_idxs: Vec<usize> = c.iter().map(|r| r.dir_idx).collect();
                assert!(dir_idxs.contains(&0) && dir_idxs.contains(&1));
            }
            other => panic!("expected ambiguous prefix, got {other:?}"),
        }
        // A prefix that narrows to one dir resolves fully again.
        assert_eq!(dir_of(&t.resolve("proj-light", &entries, &index)), Some(0));
    }

    #[test]
    fn resolve_prefix_beats_fuzzy_tier() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        // "ot" exactly prefixes "other" and no other dir name, even though
        // fuzzy "contains" would also accept nothing else. Single result.
        assert_eq!(dir_of(&t.resolve("ot", &entries, &index)), Some(3));
        // "ther" matches nothing exactly or by prefix; the fuzzy tier still
        // finds "other" via substring containment.
        assert_eq!(dir_of(&t.resolve("ther", &entries, &index)), Some(3));
    }

    #[test]
    fn resolve_unknown_name_is_none() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        assert!(matches!(
            t.resolve("zzzznothing", &entries, &index),
            TrieMatchResult::None
        ));
        assert!(matches!(
            t.resolve("", &entries, &index),
            TrieMatchResult::None
        ));
        assert!(matches!(
            t.resolve("   ", &entries, &index),
            TrieMatchResult::None
        ));
    }

    #[test]
    fn resolve_full_path_direct_hit() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        assert_eq!(
            dir_of(&t.resolve("/home/user/projects/mnemo", &entries, &index)),
            Some(2)
        );
    }

    #[test]
    fn resolve_path_suffix_match() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        // A relative path that ends a real directory path, and is not a
        // registered path index entry itself.
        assert_eq!(
            dir_of(&t.resolve("projects/mnemo", &entries, &index)),
            Some(2)
        );
    }

    #[test]
    fn resolve_file_path_never_becomes_a_dir_scope() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        // The file is registered in path_index but is not a dir; the suffix
        // branch and the name trie also only ever contain dirs. A file path
        // must resolve to None, never pretend to be a directory scope.
        assert_eq!(
            dir_of(&t.resolve("/home/user/projects/mnemo/src/main.rs", &entries, &index)),
            None
        );
    }

    #[test]
    fn resolve_trailing_slash_is_normalized() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        assert_eq!(
            dir_of(&t.resolve("projects/mnemo/", &entries, &index)),
            Some(2)
        );
    }

    #[test]
    fn resolve_tilde_expands_to_home_and_matches() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        // Only meaningful if the fixture home matches the real home dir;
        // build refs the real $HOME through dirs::home_dir.
        if dirs::home_dir()
            .map(|h| h.ends_with("user"))
            .unwrap_or(false)
        {
            assert_eq!(
                dir_of(&t.resolve("~/projects/mnemo", &entries, &index)),
                Some(2)
            );
        }
    }

    #[test]
    fn scope_candidates_include_the_dir_and_all_descendants() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        let candidates = t.get_scope_candidates(1, 1);
        assert!(candidates.contains(&1), "the dir itself is searchable");
        assert!(
            candidates.contains(&2) && candidates.contains(&4) && candidates.contains(&5),
            "descendants at every depth: {candidates:?}"
        );
        assert!(!candidates.contains(&6), "unrelated subtree excluded");
    }

    #[test]
    fn scope_candidates_do_not_leak_upwards_to_siblings() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        let candidates = t.get_scope_candidates(2, 1);
        assert!(candidates.contains(&2) && candidates.contains(&4));
        assert!(
            !candidates.contains(&3),
            "sibling dir excluded: {candidates:?}"
        );
    }

    #[test]
    fn scope_candidates_for_a_leaf_is_just_itself() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        let candidates = t.get_scope_candidates(5, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0], 5);
    }

    #[test]
    fn scope_candidates_cache_returns_same_list_per_generation() {
        let (entries, index) = sample();
        let t = trie(&entries, &index);
        let first = t.get_scope_candidates(1, 1);
        let second = t.get_scope_candidates(1, 1);
        assert!(Arc::ptr_eq(&first, &second), "same generation hits cache");
    }

    #[test]
    fn scope_candidates_recomputes_after_generation_bump() {
        let (entries, index) = sample();
        let mut t = trie(&entries, &index);
        let stale = t.get_scope_candidates(1, 1);
        // Build replaces the whole structure on a real refresh; here the
        // generation differs from the cache key, so a new gen re-computes
        // and evicts the stale entries.
        t.generation = 2;
        let fresh = t.get_scope_candidates(1, 2);
        assert!(
            !Arc::ptr_eq(&stale, &fresh),
            "new generation must re-compute"
        );
        assert_eq!(*stale, *fresh, "same data either way");
    }
}
