//! The evolving repository model: a flat `path_id -> current line count` state
//! that we advance commit-by-commit, plus construction of the folder tree that
//! the treemap layout consumes.

use rustc_hash::FxHashMap;

use crate::color::{hash_str, hue_from_hash};
use crate::ingest::{ChangeKind, Commit, History};

/// Current size (line count) of every path; -1 means "not present".
pub struct WorldState {
    size: Vec<i32>,
    present_files: usize,
    total_lines: u64,
}

impl WorldState {
    pub fn new(history: &History) -> Self {
        WorldState {
            size: vec![-1; history.paths.len()],
            present_files: 0,
            total_lines: 0,
        }
    }

    #[inline]
    pub fn present_files(&self) -> usize {
        self.present_files
    }

    #[inline]
    pub fn total_lines(&self) -> u64 {
        self.total_lines
    }

    /// Apply one commit's file changes to the state.
    pub fn apply(&mut self, commit: &Commit) {
        for ch in &commit.changes {
            let idx = ch.path as usize;
            let prev = self.size[idx];
            match ch.kind {
                ChangeKind::Added => {
                    if prev < 0 {
                        self.present_files += 1;
                    } else {
                        self.total_lines -= prev as u64;
                    }
                    let s = ch.added as i32;
                    self.size[idx] = s;
                    self.total_lines += s as u64;
                }
                ChangeKind::Modified => {
                    if prev < 0 {
                        // First time we see this path (e.g. earlier state excluded);
                        // treat as an addition.
                        self.present_files += 1;
                        let s = ch.added as i32;
                        self.size[idx] = s;
                        self.total_lines += s as u64;
                    } else {
                        self.total_lines -= prev as u64;
                        let mut s = prev + ch.added as i32 - ch.deleted as i32;
                        if s < 0 {
                            s = 0;
                        }
                        self.size[idx] = s;
                        self.total_lines += s as u64;
                    }
                }
                ChangeKind::Deleted => {
                    if prev >= 0 {
                        self.present_files -= 1;
                        self.total_lines -= prev as u64;
                        self.size[idx] = -1;
                    }
                }
            }
        }
    }

    #[inline]
    pub fn is_present(&self, path_id: u32) -> bool {
        self.size[path_id as usize] >= 0
    }

    #[inline]
    pub fn size_of(&self, path_id: u32) -> i32 {
        self.size[path_id as usize]
    }

    /// Capture the current present-file set as a compact, cloneable snapshot so
    /// keyframes can be built in parallel off the sequential state advance.
    pub fn snapshot(&self) -> StateSnapshot {
        let mut files = Vec::with_capacity(self.present_files);
        for (pid, &sz) in self.size.iter().enumerate() {
            if sz >= 0 {
                files.push((pid as u32, sz as u32));
            }
        }
        StateSnapshot {
            files,
            files_count: self.present_files,
            lines: self.total_lines,
        }
    }
}

/// A compact, cloneable capture of the present files at one commit state.
#[derive(Clone)]
pub struct StateSnapshot {
    pub files: Vec<(u32, u32)>,
    pub files_count: usize,
    pub lines: u64,
}

/// Build the folder tree from a present-file list. Pure function of its inputs,
/// so it can be run on any worker thread.
///
/// `max_depth` (0 = unlimited) caps folder nesting. Beyond it, `collapse=false`
/// (omit) drops the file entirely, while `collapse=true` merges each too-deep
/// subfolder into a single aggregate tile at the cutoff.
pub fn build_tree(files: &[(u32, u32)], history: &History, max_depth: u32, collapse: bool) -> Tree {
    {
        let mut tree = Tree::new();
        // dir full-path hash -> node index
        let mut dir_index: FxHashMap<u64, u32> = FxHashMap::default();
        dir_index.insert(hash_str(""), 0);

        for &(pid, sz) in files.iter() {
            let pid = pid as usize;
            let sz = sz as i32;
            let path = &history.paths[pid];
            let root_seg = path.split('/').next().unwrap_or("");
            let group_hue = hue_from_hash(hash_str(root_seg));

            let mut parent: u32 = 0;
            let mut full = String::new();
            let mut segs = path.split('/').peekable();
            let mut depth: u32 = 0;
            while let Some(seg) = segs.next() {
                if seg.is_empty() {
                    continue;
                }
                let is_last = segs.peek().is_none();
                if !full.is_empty() {
                    full.push('/');
                }
                full.push_str(seg);

                if is_last {
                    // File leaf.
                    let key = hash_str(&full);
                    let idx = tree.nodes.len() as u32;
                    tree.nodes.push(Node {
                        name: seg.to_string(),
                        is_dir: false,
                        raw_size: sz as u32,
                        weight: 0.0,
                        children: Vec::new(),
                        path_id: pid as i32,
                        key,
                        group_hue,
                        depth,
                    });
                    tree.nodes[parent as usize].children.push(idx);
                } else {
                    // We are about to create the (max_depth+1)-th directory, i.e.
                    // this file is deeper than the cap.
                    if max_depth > 0 && depth >= max_depth {
                        if collapse {
                            // Merge into a single aggregate tile for this too-deep
                            // subfolder (named after the first over-cap segment),
                            // accumulating the sizes of everything below it.
                            let key = hash_str(&full);
                            let idx = if let Some(&i) = dir_index.get(&key) {
                                i
                            } else {
                                let idx = tree.nodes.len() as u32;
                                tree.nodes.push(Node {
                                    name: seg.to_string(),
                                    is_dir: true,
                                    raw_size: 0,
                                    weight: 0.0,
                                    children: Vec::new(),
                                    path_id: -1,
                                    key,
                                    group_hue,
                                    depth,
                                });
                                tree.nodes[parent as usize].children.push(idx);
                                dir_index.insert(key, idx);
                                idx
                            };
                            let n = &mut tree.nodes[idx as usize];
                            n.raw_size = n.raw_size.saturating_add(sz as u32);
                        }
                        // omit: fall through and drop this file (add nothing).
                        break;
                    }
                    let key = hash_str(&full);
                    let node_idx = if let Some(&i) = dir_index.get(&key) {
                        i
                    } else {
                        let idx = tree.nodes.len() as u32;
                        tree.nodes.push(Node {
                            name: seg.to_string(),
                            is_dir: true,
                            raw_size: 0,
                            weight: 0.0,
                            children: Vec::new(),
                            path_id: -1,
                            key,
                            group_hue,
                            depth,
                        });
                        tree.nodes[parent as usize].children.push(idx);
                        dir_index.insert(key, idx);
                        idx
                    };
                    parent = node_idx;
                    depth += 1;
                }
            }
        }

        tree.finalize();
        tree
    }
}

/// A node in the folder tree. Node 0 is the synthetic root.
pub struct Node {
    pub name: String,
    pub is_dir: bool,
    /// Line count for files; aggregated sum for directories (for labels/minimap).
    pub raw_size: u32,
    /// Area weight used by the treemap (gamma-compressed); filled by layout.
    pub weight: f32,
    pub children: Vec<u32>,
    /// Interned path id for files, -1 for directories.
    pub path_id: i32,
    /// Stable hash of the full path (cross-frame matching key).
    pub key: u64,
    /// Hue derived from the root folder name (shared border color per root group).
    pub group_hue: f32,
    pub depth: u32,
}

pub struct Tree {
    pub nodes: Vec<Node>,
}

impl Tree {
    fn new() -> Self {
        let root = Node {
            name: String::new(),
            is_dir: true,
            raw_size: 0,
            weight: 0.0,
            children: Vec::new(),
            path_id: -1,
            key: hash_str(""),
            group_hue: 0.0,
            depth: 0,
        };
        Tree { nodes: vec![root] }
    }

    /// Sort children (dirs first, then by name) and aggregate raw sizes bottom-up.
    fn finalize(&mut self) {
        // Sort each node's children for a stable, size-independent order.
        let n = self.nodes.len();
        for i in 0..n {
            let mut kids = std::mem::take(&mut self.nodes[i].children);
            kids.sort_by(|&a, &b| {
                let na = &self.nodes[a as usize];
                let nb = &self.nodes[b as usize];
                nb.is_dir
                    .cmp(&na.is_dir) // dirs (true) first
                    .then_with(|| na.name.cmp(&nb.name))
            });
            self.nodes[i].children = kids;
        }
        // Aggregate raw_size bottom-up. Children indices are always greater than
        // their parent (arena built top-down), so one reverse pass computes the
        // post-order sums with no recursion or allocation. Childless dirs keep
        // their own raw_size (this is how collapse-mode aggregate tiles carry a
        // size).
        for i in (0..self.nodes.len()).rev() {
            if !self.nodes[i].is_dir || self.nodes[i].children.is_empty() {
                continue;
            }
            let kids = std::mem::take(&mut self.nodes[i].children);
            let mut sum: u32 = 0;
            for &c in &kids {
                sum = sum.saturating_add(self.nodes[c as usize].raw_size);
            }
            self.nodes[i].children = kids;
            self.nodes[i].raw_size = sum;
        }
    }

    #[inline]
    pub fn root(&self) -> &Node {
        &self.nodes[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{Author, ChangeKind, Commit, FileDelta};

    fn hist(paths: &[&str]) -> History {
        History {
            paths: paths.iter().map(|s| s.to_string()).collect(),
            authors: vec![Author {
                name: "a".into(),
                email: "a@x".into(),
            }],
            commits: vec![],
        }
    }
    fn commit(changes: Vec<FileDelta>) -> Commit {
        Commit {
            hash: String::new(),
            author: 0,
            time: 0,
            subject: String::new(),
            changes,
        }
    }
    fn d(path: u32, added: u32, deleted: u32, kind: ChangeKind) -> FileDelta {
        FileDelta {
            path,
            added,
            deleted,
            kind,
        }
    }

    #[test]
    fn world_state_apply_invariants() {
        let h = hist(&["a.rs", "b.rs"]);
        let mut s = WorldState::new(&h);
        s.apply(&commit(vec![d(0, 10, 0, ChangeKind::Added)]));
        assert_eq!(s.present_files(), 1);
        assert_eq!(s.total_lines(), 10);
        s.apply(&commit(vec![
            d(0, 5, 3, ChangeKind::Modified),
            d(1, 4, 0, ChangeKind::Added),
        ]));
        assert_eq!(s.present_files(), 2);
        assert_eq!(s.total_lines(), 16); // a: 12, b: 4
        s.apply(&commit(vec![d(0, 0, 0, ChangeKind::Deleted)]));
        assert_eq!(s.present_files(), 1);
        assert_eq!(s.total_lines(), 4);
        // deleting an already-absent file is a no-op
        s.apply(&commit(vec![d(0, 0, 0, ChangeKind::Deleted)]));
        assert_eq!(s.present_files(), 1);
        assert_eq!(s.total_lines(), 4);
    }

    #[test]
    fn build_tree_depth_modes() {
        // path 0 has 1 parent folder, path 1 has 3.
        let h = hist(&["a/x.rs", "a/b/c/d.rs"]);
        let files = vec![(0u32, 5u32), (1u32, 10u32)];

        let full = build_tree(&files, &h, 0, false);
        assert_eq!(full.nodes[0].raw_size, 15);
        assert_eq!(full.nodes.iter().filter(|n| !n.is_dir).count(), 2);

        // omit at depth 1 drops the too-deep file.
        let omit = build_tree(&files, &h, 1, false);
        assert_eq!(omit.nodes[0].raw_size, 5);

        // collapse at depth 1 keeps the size via an aggregate tile.
        let coll = build_tree(&files, &h, 1, true);
        assert_eq!(coll.nodes[0].raw_size, 15);
    }
}
