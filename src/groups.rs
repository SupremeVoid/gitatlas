//! Color groups: which folder decides a tile's border color.
//!
//! By default every top-level folder is one group. That falls apart for repos
//! where (almost) everything lives under one chain like `src/app/…` — the whole
//! atlas ends up a single color. So groups hang off a set of *color roots*: a
//! file's group is the direct subfolder of the deepest color root above it. The
//! repo root `""` is always a color root; more come from `--color-root`, and from
//! auto detection, which follows the dominant folder chain of the final state
//! down to the level where the tree actually splits. `--color-module` folders
//! are a group of their own, whatever is above them.
//!
//! Computed once per run (never per frame), so colors are stable over a video.

use rustc_hash::FxHashSet;

use crate::color::{hash_str, hue_from_hash};

/// Sentinel hue for folders that *contain* several color groups (the color
/// roots themselves, e.g. `src` and `src/app`): drawn in neutral gray.
pub const NEUTRAL_HUE: f32 = -1.0;

/// A child must hold at least this share of its parent's (capped) lines for
/// auto detection to descend into it.
const DOMINANT_SHARE: f64 = 0.7;
const MAX_AUTO_DEPTH: usize = 8;
const MODULE_MIN_HUE_DIST: f32 = 60.0;

/// Distance between two hues on the color wheel, in degrees (0..=180).
fn hue_dist(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

pub struct ColorMap {
    /// Group hue per path id.
    pub path_hue: Vec<f32>,
    /// `hash_str(full path)` of every non-empty color root.
    neutral_dirs: FxHashSet<u64>,
    roots: FxHashSet<String>,
    modules: FxHashSet<String>,
    /// The chain auto detection settled on ("" when it stayed at the top).
    pub auto_root: String,
}

impl ColorMap {
    /// `final_files` is the present-file list `(path id, lines)` of the final
    /// state; `size_cap` keeps one huge generated file from looking dominant.
    pub fn build(
        paths: &[String],
        final_files: &[(u32, u32)],
        size_cap: f32,
        color_roots: &[String],
        color_modules: &[String],
        auto: bool,
    ) -> Self {
        let auto_root = if auto {
            dominant_chain(paths, final_files, size_cap)
        } else {
            String::new()
        };

        // Every ancestor of a color root is a color root too, so `src/assets`
        // still gets its own color when `src/app` is the declared root.
        let mut roots: FxHashSet<String> = FxHashSet::default();
        roots.insert(String::new());
        for r in color_roots
            .iter()
            .map(|r| normalize(r))
            .chain([auto_root.clone()])
        {
            let mut full = String::new();
            for seg in r.split('/').filter(|s| !s.is_empty()) {
                if !full.is_empty() {
                    full.push('/');
                }
                full.push_str(seg);
                roots.insert(full.clone());
            }
        }
        let modules: FxHashSet<String> = color_modules
            .iter()
            .map(|m| normalize(m))
            .filter(|m| !m.is_empty())
            .collect();

        let mut cm = ColorMap {
            path_hue: Vec::new(),
            neutral_dirs: FxHashSet::default(),
            roots,
            modules,
            auto_root,
        };
        cm.path_hue = paths
            .iter()
            .map(|p| cm.group_hue(group_of(p, &cm.roots, &cm.modules)))
            .collect();
        let (roots, modules) = (&cm.roots, &cm.modules);
        let neutral_dirs = roots
            .iter()
            .filter(|r| !r.is_empty() && !modules.contains(*r))
            .map(|r| hash_str(r))
            .collect();
        cm.neutral_dirs = neutral_dirs;
        cm
    }

    /// Hue of a group folder. Plain groups hash their path. A declared module
    /// must stand out from the group around it, so its hash is re-salted until
    /// the hue is at least `MODULE_MIN_HUE_DIST` degrees away from that group's.
    fn group_hue(&self, group: &str) -> f32 {
        let plain = hue_from_hash(hash_str(group));
        if !self.modules.contains(group) {
            return plain;
        }
        // Treating the module path as a file yields the group enclosing it.
        let around = self.group_hue(group_of(group, &self.roots, &self.modules));
        let mut hue = plain;
        let mut salt = 0u32;
        while hue_dist(hue, around) < MODULE_MIN_HUE_DIST && salt < 16 {
            salt += 1;
            hue = hue_from_hash(hash_str(&format!("{group}#{salt}")));
        }
        hue
    }

    /// Top-level-folder coloring only (tests, and the pre-1.1 behavior).
    pub fn top_level(paths: &[String]) -> Self {
        Self::build(paths, &[], 0.0, &[], &[], false)
    }

    /// Hue for the directory `full` (full-path hash `key`), given the hue of the
    /// file that created it.
    #[inline]
    pub fn dir_hue(&self, full: &str, key: u64, file_hue: f32) -> f32 {
        if !self.neutral_dirs.is_empty() && self.neutral_dirs.contains(&key) {
            NEUTRAL_HUE
        } else if self.modules.is_empty() {
            file_hue
        } else {
            // A folder above a module (e.g. `shared` over `shared/ui`) must not
            // inherit the module's hue from whichever file created it first.
            let probe = format!("{full}/");
            self.group_hue(group_of(&probe, &self.roots, &self.modules))
        }
    }
}

fn normalize(p: &str) -> String {
    p.replace('\\', "/")
        .trim_start_matches("./")
        .trim_matches('/')
        .to_string()
}

/// The group folder of `path` as a prefix slice of it ("" for files sitting
/// directly in the repo root).
fn group_of<'a>(path: &'a str, roots: &FxHashSet<String>, modules: &FxHashSet<String>) -> &'a str {
    let mut group_end = 0usize; // path[..group_end] is the group
    let mut cur_end = 0usize; // path[..cur_end] is the folder walked so far
    let mut rest = path;
    while let Some(i) = rest.find('/') {
        let next_end = if cur_end == 0 { i } else { cur_end + 1 + i };
        let parent_is_root = roots.contains(&path[..cur_end]);
        let next = &path[..next_end];
        if parent_is_root || modules.contains(next) {
            group_end = next_end;
        }
        cur_end = next_end;
        rest = &path[next_end + 1..];
    }
    // A file directly inside a color root belongs to that root, not to a
    // one-file group of its own.
    &path[..group_end]
}

/// Follow the chain of folders that each hold most of their parent's lines.
fn dominant_chain(paths: &[String], final_files: &[(u32, u32)], size_cap: f32) -> String {
    let cap = if size_cap >= 1.0 {
        size_cap as u64
    } else {
        u64::MAX
    };
    let mut prefix = String::new(); // "" or "a/b/" (with trailing slash)
    for _ in 0..MAX_AUTO_DEPTH {
        let mut total = 0u64;
        let mut children: rustc_hash::FxHashMap<&str, u64> = Default::default();
        for &(pid, lines) in final_files {
            let Some(rest) = paths[pid as usize].strip_prefix(prefix.as_str()) else {
                continue;
            };
            let w = (lines as u64).clamp(1, cap);
            total += w;
            if let Some(i) = rest.find('/') {
                *children.entry(&rest[..i]).or_default() += w;
            }
        }
        // Name as tiebreaker keeps this deterministic.
        let Some((name, w)) = children.into_iter().max_by_key(|&(n, w)| (w, n)) else {
            break;
        };
        if (w as f64) < total as f64 * DOMINANT_SHARE {
            break;
        }
        prefix.push_str(name);
        prefix.push('/');
    }
    prefix.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[&str]) -> FxHashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn groups_follow_roots_and_modules() {
        let roots = set(&["", "src", "src/app"]);
        let modules = set(&["src/app/shared/ui"]);
        let g = |p| group_of(p, &roots, &modules);
        assert_eq!(g("README.md"), "");
        assert_eq!(g("docs/a/b.md"), "docs");
        assert_eq!(g("src/main.ts"), "src");
        assert_eq!(g("src/assets/x.svg"), "src/assets");
        assert_eq!(g("src/app/app.ts"), "src/app");
        assert_eq!(g("src/app/orders/list/list.ts"), "src/app/orders");
        assert_eq!(g("src/app/shared/pipes/p.ts"), "src/app/shared");
        assert_eq!(g("src/app/shared/ui/btn/btn.ts"), "src/app/shared/ui");
    }

    #[test]
    fn auto_descends_dominant_chain_only() {
        let paths: Vec<String> = [
            "README.md",
            "src/main.ts",
            "src/app/a/x.ts",
            "src/app/b/y.ts",
            "src/app/c/z.ts",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let files = [(0, 10), (1, 10), (2, 100), (3, 100), (4, 100)];
        assert_eq!(dominant_chain(&paths, &files, 0.0), "src/app");

        let cm = ColorMap::build(&paths, &files, 0.0, &[], &[], true);
        assert_ne!(cm.path_hue[2], cm.path_hue[3]);
        assert_eq!(cm.dir_hue("src/app", hash_str("src/app"), 5.0), NEUTRAL_HUE);
        assert_eq!(cm.dir_hue("src/app/a", hash_str("src/app/a"), 5.0), 5.0);

        // A module never blends into the group around it.
        for name in ["src/app/a", "src/app/b", "src/app/c"] {
            let cm = ColorMap::build(&paths, &files, 0.0, &[], &[format!("{name}/deep")], true);
            let around = hue_from_hash(hash_str(name));
            assert!(hue_dist(cm.group_hue(&format!("{name}/deep")), around) >= MODULE_MIN_HUE_DIST);
        }

        // Balanced top level: stay put, identical to top-level coloring.
        let flat = [(0, 100), (1, 100), (2, 100)];
        assert_eq!(dominant_chain(&paths, &flat, 0.0), "");
        let top = ColorMap::top_level(&paths);
        assert_eq!(top.path_hue[2], top.path_hue[3]);
        assert_eq!(top.path_hue[2], hue_from_hash(hash_str("src")));
    }
}
