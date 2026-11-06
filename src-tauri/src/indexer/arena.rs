use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub type NodeId = u32;

#[derive(Debug, Clone, Copy)]
pub struct Node {
    pub parent: NodeId,
    pub name_off: u32,
    pub name_len: u32,
    pub ext_id: u16,
    pub flags: u8,
    pub size: u64,
    pub mtime: u64,
    pub dev: u64,
    pub ino: u64,
    pub char_bloom: u64,
    pub node_id: u64,
}

impl Node {
    pub const FLAG_DIR: u8 = 1;
    pub const FLAG_CONTENT_PENDING: u8 = 2;
    pub fn is_dir(&self) -> bool {
        self.flags & Self::FLAG_DIR != 0
    }
    pub fn content_pending(&self) -> bool {
        self.flags & Self::FLAG_CONTENT_PENDING != 0
    }
    pub fn set_dir(&mut self, v: bool) {
        if v {
            self.flags |= Self::FLAG_DIR;
        } else {
            self.flags &= !Self::FLAG_DIR;
        }
    }
    pub fn set_content_pending(&mut self, v: bool) {
        if v {
            self.flags |= Self::FLAG_CONTENT_PENDING;
        } else {
            self.flags &= !Self::FLAG_CONTENT_PENDING;
        }
    }
}

pub const NO_PARENT: NodeId = u32::MAX;

#[derive(Debug, Default)]
pub struct NameArena {
    buf: Vec<u8>,
}

impl NameArena {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn push(&mut self, name: &str) -> (u32, u32) {
        let off = self.buf.len() as u32;
        self.buf.extend_from_slice(name.as_bytes());
        (off, name.len() as u32)
    }
    pub fn get(&self, off: u32, len: u32) -> &str {
        let start = off as usize;
        let end = start + len as usize;
        if end > self.buf.len() {
            return "";
        }
        std::str::from_utf8(&self.buf[start..end]).unwrap_or("")
    }
}

#[derive(Debug)]
pub struct NodeArena {
    pub nodes: Vec<Node>,
    pub names: NameArena,
    exts: Vec<Box<str>>,
    ext_index: HashMap<Box<str>, u16>,
}

impl Default for NodeArena {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeArena {
    pub fn new() -> Self {
        // Reserve ext id 0 for "no extension".
        let exts = vec![Box::from("")];
        Self {
            nodes: Vec::new(),
            names: NameArena::new(),
            exts,
            ext_index: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, idx: u32) -> &Node {
        &self.nodes[idx as usize]
    }

    pub fn node_mut(&mut self, idx: u32) -> &mut Node {
        &mut self.nodes[idx as usize]
    }

    pub fn name(&self, idx: u32) -> &str {
        let n = &self.nodes[idx as usize];
        self.names.get(n.name_off, n.name_len)
    }

    pub fn extension(&self, idx: u32) -> Option<String> {
        let n = &self.nodes[idx as usize];
        if n.ext_id == 0 {
            None
        } else {
            Some(self.exts[n.ext_id as usize].to_string())
        }
    }

    pub fn ext_count(&self) -> usize {
        self.exts.len()
    }

    pub fn ext_str(&self, ext_id: u16) -> &str {
        if ext_id == 0 {
            ""
        } else {
            &self.exts[ext_id as usize]
        }
    }

    pub fn intern_ext(&mut self, ext: Option<&str>) -> u16 {
        match ext {
            None => 0,
            Some("") => 0,
            Some(e) => {
                let key: Box<str> = Box::from(e);
                if let Some(&id) = self.ext_index.get(&key) {
                    return id;
                }
                let id = self.exts.len() as u16;
                self.exts.push(key.clone());
                self.ext_index.insert(key, id);
                id
            }
        }
    }

    pub fn push(&mut self, node: Node) -> u32 {
        let idx = self.nodes.len() as u32;
        self.nodes.push(node);
        idx
    }

    pub fn path(&self, idx: u32) -> PathBuf {
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = Some(idx);
        let mut guard = 0;
        while let Some(i) = cur {
            if guard > self.nodes.len() + 1 {
                break;
            }
            guard += 1;
            let node = &self.nodes[i as usize];
            parts.push(self.names.get(node.name_off, node.name_len));
            if node.parent == NO_PARENT {
                break;
            } else {
                cur = Some(node.parent);
            }
        }
        parts.reverse();
        let mut p = PathBuf::new();
        for part in parts {
            if !part.is_empty() {
                p = p.join(part);
            }
        }
        p
    }

    /// Compares an arena node's reconstructed path with `path` without
    /// allocating a `PathBuf`.
    pub fn path_eq(&self, idx: u32, path: &Path) -> bool {
        let components: Vec<&std::ffi::OsStr> = path
            .components()
            .map(|component| component.as_os_str())
            .collect();
        let mut current = Some(idx);
        let mut position = components.len();
        let mut guard = 0usize;
        while let Some(node_idx) = current {
            if guard > self.nodes.len() + 1 || node_idx as usize >= self.nodes.len() {
                return false;
            }
            guard += 1;
            let node = &self.nodes[node_idx as usize];
            let name = self.names.get(node.name_off, node.name_len);
            if node.parent == NO_PARENT {
                if position == 0 {
                    return name.is_empty();
                }
                let remaining: PathBuf = components[..position].iter().collect();
                return name == remaining.to_string_lossy();
            }
            if position == 0 {
                return false;
            }
            position -= 1;
            if components[position] != name {
                return false;
            }
            current = Some(node.parent);
        }
        position == 0
    }

    /// Reconstruct parent index if needed (already stored).
    pub fn parent(&self, idx: u32) -> NodeId {
        self.nodes[idx as usize].parent
    }
}

pub fn path_hash(path: &Path) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in path.to_string_lossy().as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

const PATH_INDEX_EMPTY: u32 = u32::MAX;
const PATH_INDEX_TOMBSTONE: u32 = u32::MAX - 1;

#[derive(Default)]
pub struct PathIndex {
    fingerprints: Vec<u32>,
    indices: Vec<u32>,
    mask: usize,
    len: usize,
    tombstones: usize,
}

impl PathIndex {
    pub fn new() -> Self {
        Self::with_capacity(16)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.next_power_of_two().max(16);
        Self {
            fingerprints: vec![0; capacity],
            indices: vec![PATH_INDEX_EMPTY; capacity],
            mask: capacity - 1,
            len: 0,
            tombstones: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn capacity(&self) -> usize {
        self.indices.len()
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    fn fingerprint(hash: u64) -> u32 {
        (hash as u32) ^ ((hash >> 32) as u32)
    }

    pub fn find(&self, arena: &NodeArena, path: &Path) -> Option<u32> {
        if self.indices.is_empty() {
            return None;
        }
        let hash = path_hash(path);
        let fingerprint = Self::fingerprint(hash);
        let mut slot = (hash as usize) & self.mask;
        loop {
            let idx = self.indices[slot];
            if idx == PATH_INDEX_EMPTY {
                return None;
            }
            if idx != PATH_INDEX_TOMBSTONE
                && self.fingerprints[slot] == fingerprint
                && arena.path_eq(idx, path)
            {
                return Some(idx);
            }
            slot = (slot + 1) & self.mask;
        }
    }

    pub fn insert(&mut self, arena: &NodeArena, path: &Path, idx: u32) {
        if self.len + self.tombstones + 1 > self.indices.len() * 7 / 10 {
            self.grow(arena);
        }
        let hash = path_hash(path);
        let fingerprint = Self::fingerprint(hash);
        let mut first_tombstone = None;
        let mut slot = (hash as usize) & self.mask;
        loop {
            let current = self.indices[slot];
            if current == PATH_INDEX_EMPTY {
                let target = first_tombstone.unwrap_or(slot);
                self.fingerprints[target] = fingerprint;
                self.indices[target] = idx;
                self.len += 1;
                if first_tombstone.is_some() {
                    self.tombstones = self.tombstones.saturating_sub(1);
                }
                return;
            }
            if current == PATH_INDEX_TOMBSTONE {
                if first_tombstone.is_none() {
                    first_tombstone = Some(slot);
                }
            } else if self.fingerprints[slot] == fingerprint && arena.path_eq(current, path) {
                self.indices[slot] = idx;
                return;
            }
            slot = (slot + 1) & self.mask;
        }
    }

    pub fn remove(&mut self, arena: &NodeArena, path: &Path) -> Option<u32> {
        let hash = path_hash(path);
        let fingerprint = Self::fingerprint(hash);
        let mut slot = (hash as usize) & self.mask;
        loop {
            let idx = self.indices[slot];
            if idx == PATH_INDEX_EMPTY {
                return None;
            }
            if idx != PATH_INDEX_TOMBSTONE
                && self.fingerprints[slot] == fingerprint
                && arena.path_eq(idx, path)
            {
                self.indices[slot] = PATH_INDEX_TOMBSTONE;
                self.tombstones += 1;
                self.len = self.len.saturating_sub(1);
                if self.tombstones > self.len / 4 + 1 && self.indices.len() > 16 {
                    self.rehash(arena);
                }
                return Some(idx);
            }
            slot = (slot + 1) & self.mask;
        }
    }

    fn grow(&mut self, arena: &NodeArena) {
        let new_capacity = (self.indices.len() * 2).max(16);
        let old_fingerprints = std::mem::take(&mut self.fingerprints);
        let old_indices = std::mem::take(&mut self.indices);
        self.fingerprints = vec![0; new_capacity];
        self.indices = vec![PATH_INDEX_EMPTY; new_capacity];
        self.mask = new_capacity - 1;
        self.len = 0;
        self.tombstones = 0;
        for &idx in &old_indices {
            if idx != PATH_INDEX_EMPTY && idx != PATH_INDEX_TOMBSTONE {
                let path = arena.path(idx);
                self.insert(arena, &path, idx);
            }
        }
        drop(old_fingerprints);
        drop(old_indices);
    }

    fn rehash(&mut self, arena: &NodeArena) {
        let capacity = self.indices.len();
        let old_fingerprints = std::mem::take(&mut self.fingerprints);
        let old_indices = std::mem::take(&mut self.indices);
        self.fingerprints = vec![0; capacity];
        self.indices = vec![PATH_INDEX_EMPTY; capacity];
        self.mask = capacity - 1;
        self.len = 0;
        self.tombstones = 0;
        for &idx in &old_indices {
            if idx != PATH_INDEX_EMPTY && idx != PATH_INDEX_TOMBSTONE {
                let path = arena.path(idx);
                self.insert(arena, &path, idx);
            }
        }
        drop(old_fingerprints);
        drop(old_indices);
    }
}

#[derive(Default)]
pub struct IdIndex {
    ids: Vec<u64>,
    idxs: Vec<u32>,
}

impl IdIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn get(&self, id: u64) -> Option<u32> {
        let pos = self.ids.binary_search(&id).ok()?;
        Some(self.idxs[pos])
    }

    pub fn insert(&mut self, id: u64, idx: u32) {
        match self.ids.binary_search(&id) {
            Ok(pos) => self.idxs[pos] = idx,
            Err(pos) => {
                self.ids.insert(pos, id);
                self.idxs.insert(pos, idx);
            }
        }
    }

    pub fn remove(&mut self, id: u64) {
        if let Ok(pos) = self.ids.binary_search(&id) {
            self.ids.remove(pos);
            self.idxs.remove(pos);
        }
    }

    pub fn clear(&mut self) {
        self.ids.clear();
        self.idxs.clear();
    }

    pub fn from_pairs(mut pairs: Vec<(u64, u32)>) -> Self {
        pairs.sort_unstable_by_key(|pair| pair.0);
        Self {
            ids: pairs.iter().map(|pair| pair.0).collect(),
            idxs: pairs.iter().map(|pair| pair.1).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ── NameArena ─────────────────────────────────────────────────────────────

    #[test]
    fn name_arena_push_and_get_round_trips() {
        let mut a = NameArena::new();
        let (off, len) = a.push("hello");
        assert_eq!(a.get(off, len), "hello");
        let (off2, len2) = a.push("world");
        assert_eq!(a.get(off2, len2), "world");
        // First entry must still be intact.
        assert_eq!(a.get(off, len), "hello");
    }

    #[test]
    fn name_arena_empty_push() {
        let mut a = NameArena::new();
        let (off, len) = a.push("");
        assert_eq!(a.get(off, len), "");
        assert_eq!(a.len(), 0);
    }

    #[test]
    fn name_arena_oob_get_returns_empty() {
        let a = NameArena::new();
        // offset + len beyond buffer → empty string.
        assert_eq!(a.get(0, 10), "");
    }

    #[test]
    fn name_arena_unicode_round_trips() {
        let mut a = NameArena::new();
        let s = "日本語ファイル.txt";
        let (off, len) = a.push(s);
        assert_eq!(a.get(off, len), s);
    }

    // ── NodeArena ─────────────────────────────────────────────────────────────

    fn node(parent: NodeId, name: &str, is_dir: bool, arena: &mut NodeArena) -> u32 {
        let (name_off, name_len) = arena.names.push(name);
        let n = Node {
            parent,
            name_off,
            name_len,
            ext_id: 0,
            flags: if is_dir { Node::FLAG_DIR } else { 0 },
            size: 0,
            mtime: 0,
            dev: 0,
            ino: 0,
            char_bloom: 0,
            node_id: 0,
        };
        arena.push(n)
    }

    #[test]
    fn node_flags_dir_and_content_pending() {
        let mut n = Node {
            parent: NO_PARENT,
            name_off: 0,
            name_len: 0,
            ext_id: 0,
            flags: 0,
            size: 0,
            mtime: 0,
            dev: 0,
            ino: 0,
            char_bloom: 0,
            node_id: 0,
        };
        assert!(!n.is_dir());
        assert!(!n.content_pending());

        n.set_dir(true);
        assert!(n.is_dir());
        assert!(!n.content_pending());

        n.set_content_pending(true);
        assert!(n.content_pending());

        n.set_dir(false);
        assert!(!n.is_dir());
        assert!(n.content_pending()); // content flag survives clearing dir flag

        n.set_content_pending(false);
        assert!(!n.content_pending());
    }

    #[test]
    fn node_arena_path_reconstruction() {
        let mut arena = NodeArena::new();
        let root = node(NO_PARENT, "home", true, &mut arena);
        let user = node(root, "alice", true, &mut arena);
        let file = node(user, "notes.txt", false, &mut arena);

        let path = arena.path(file);
        assert_eq!(path, Path::new("home/alice/notes.txt"));
    }

    #[test]
    fn node_arena_path_eq_matches_correctly() {
        let mut arena = NodeArena::new();
        let root = node(NO_PARENT, "home", true, &mut arena);
        let user = node(root, "alice", true, &mut arena);
        let file = node(user, "doc.pdf", false, &mut arena);

        assert!(arena.path_eq(file, Path::new("home/alice/doc.pdf")));
        assert!(!arena.path_eq(file, Path::new("home/alice/other.pdf")));
        assert!(!arena.path_eq(file, Path::new("home/doc.pdf")));
    }

    #[test]
    fn node_arena_intern_ext_deduplicates() {
        let mut arena = NodeArena::new();
        let id1 = arena.intern_ext(Some("rs"));
        let id2 = arena.intern_ext(Some("rs"));
        let id3 = arena.intern_ext(Some("toml"));
        assert_eq!(id1, id2, "same extension must get same id");
        assert_ne!(id1, id3);
        // No extension → id 0.
        assert_eq!(arena.intern_ext(None), 0);
        assert_eq!(arena.intern_ext(Some("")), 0);
    }

    #[test]
    fn node_arena_extension_lookup() {
        let mut arena = NodeArena::new();
        let rs_id = arena.intern_ext(Some("rs"));
        let (name_off, name_len) = arena.names.push("main.rs");
        let idx = arena.push(Node {
            parent: NO_PARENT,
            name_off,
            name_len,
            ext_id: rs_id,
            flags: 0,
            size: 0,
            mtime: 0,
            dev: 0,
            ino: 0,
            char_bloom: 0,
            node_id: 0,
        });
        assert_eq!(arena.extension(idx), Some("rs".to_string()));
        assert_eq!(arena.ext_str(rs_id), "rs");
        assert_eq!(arena.ext_str(0), "");
    }

    // ── IdIndex ───────────────────────────────────────────────────────────────

    #[test]
    fn id_index_insert_get_remove() {
        let mut idx = IdIndex::new();
        assert!(idx.is_empty());

        idx.insert(10, 1);
        idx.insert(20, 2);
        idx.insert(30, 3);
        assert_eq!(idx.len(), 3);
        assert_eq!(idx.get(10), Some(1));
        assert_eq!(idx.get(20), Some(2));
        assert_eq!(idx.get(99), None);

        idx.remove(20);
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.get(20), None);
        assert_eq!(idx.get(10), Some(1));
    }

    #[test]
    fn id_index_update_existing_id() {
        let mut idx = IdIndex::new();
        idx.insert(5, 100);
        idx.insert(5, 200); // update
        assert_eq!(idx.get(5), Some(200));
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn id_index_from_pairs_is_sorted() {
        let pairs = vec![(30u64, 3u32), (10, 1), (20, 2)];
        let idx = IdIndex::from_pairs(pairs);
        assert_eq!(idx.get(10), Some(1));
        assert_eq!(idx.get(20), Some(2));
        assert_eq!(idx.get(30), Some(3));
    }

    // ── PathIndex ─────────────────────────────────────────────────────────────

    #[test]
    fn path_index_insert_find_remove() {
        let mut arena = NodeArena::new();
        let root = node(NO_PARENT, "home", true, &mut arena);
        let file = node(root, "a.txt", false, &mut arena);
        let path = arena.path(file);

        let mut pidx = PathIndex::new();
        assert!(pidx.find(&arena, &path).is_none());

        pidx.insert(&arena, &path, file);
        assert_eq!(pidx.find(&arena, &path), Some(file));
        assert_eq!(pidx.len(), 1);

        let removed = pidx.remove(&arena, &path);
        assert_eq!(removed, Some(file));
        assert_eq!(pidx.len(), 0);
        assert!(pidx.find(&arena, &path).is_none());
    }

    #[test]
    fn path_index_update_existing_path() {
        let mut arena = NodeArena::new();
        let root = node(NO_PARENT, "home", true, &mut arena);
        let old = node(root, "x.txt", false, &mut arena);
        let new = node(root, "x.txt", false, &mut arena);
        let path = arena.path(old);

        let mut pidx = PathIndex::new();
        pidx.insert(&arena, &path, old);
        pidx.insert(&arena, &path, new); // overwrite
        assert_eq!(pidx.find(&arena, &path), Some(new));
        assert_eq!(pidx.len(), 1);
    }

    #[test]
    fn path_hash_is_deterministic() {
        let p = Path::new("home/alice/notes.txt");
        assert_eq!(path_hash(p), path_hash(p));
        assert_ne!(path_hash(p), path_hash(Path::new("home/bob/notes.txt")));
    }
}
