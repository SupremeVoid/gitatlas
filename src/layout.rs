//! Treemap layout: a stable, order-preserving "ordered squarified" subdivision.
//!
//! Stability (the #1 requirement for a watchable evolving video) comes from:
//!   1. size-INDEPENDENT sibling order (the tree pre-sorts children by name),
//!   2. an order-preserving strip/squarified pack (we never reorder by size),
//!   3. recursive containment (each folder is packed inside its own rect, so a
//!      change inside one folder cannot move tiles in another).
//! Level-of-detail collapses folders too small to resolve into a single tile.

use rustc_hash::FxHashMap;

use crate::geom::Rect;
use crate::model::Tree;

#[derive(Clone)]
pub struct LayoutParams {
    /// Gamma compression exponent for the area metric (0.5 ≈ sqrt).
    pub gamma: f32,
    /// Folder balancing 0..0.9 (see `balanced_shares`); 0 disables it.
    pub balance: f32,
    /// Area multiplier (1 + rule delta) for folders matched by a balance rule,
    /// keyed like tree nodes (`hash_str(full path)`); see `rule_areas`.
    pub dir_scale: std::sync::Arc<FxHashMap<u64, f32>>,
    /// Upper limit for a folder a rule enlarges, as a share of the whole atlas.
    pub max_share: f32,
    /// Minimum weight floor (in "lines") so tiny files stay visible.
    pub min_weight: f32,
    /// Cap on the area metric (stable, computed once globally) so one huge file
    /// cannot dominate the canvas.
    pub size_cap: f32,
    /// A folder whose shorter side is below this many pixels is drawn collapsed.
    pub min_open_px: f32,
    /// Padding inside each folder before laying out its children.
    pub pad: f32,
    /// Height reserved at the top of a folder for its label (0 to disable).
    pub label_h: f32,
    /// Only reserve label space if the folder is at least this tall.
    pub label_min_px: f32,
    /// Deepest folder depth (0-based) that gets a label; deeper folders reserve
    /// no label strip, so they never show an empty header.
    pub label_max_depth: u32,
}

impl Default for LayoutParams {
    fn default() -> Self {
        LayoutParams {
            gamma: 0.5,
            balance: 0.0,
            dir_scale: Default::default(),
            max_share: 0.85,
            min_weight: 1.0,
            size_cap: 4000.0,
            min_open_px: 34.0,
            pad: 3.0,
            label_h: 14.0,
            label_min_px: 46.0,
            label_max_depth: u32::MAX,
        }
    }
}

#[derive(Clone, Copy)]
pub struct LaidTile {
    pub key: u64,
    pub rect: Rect,
    pub depth: u32,
    pub is_dir: bool,
    pub collapsed: bool,
    pub group_hue: f32,
    pub raw_size: u32,
    pub path_id: i32,
    pub node: u32,
    pub child_count: u32,
}

pub struct Layout {
    pub tiles: Vec<LaidTile>,
    pub index: FxHashMap<u64, u32>,
    pub frame: Rect,
}

impl Layout {
    #[inline]
    pub fn get(&self, key: u64) -> Option<&LaidTile> {
        self.index.get(&key).map(|&i| &self.tiles[i as usize])
    }
}

#[inline]
fn weight_metric(raw: u32, p: &LayoutParams) -> f32 {
    let s = (raw as f32).clamp(p.min_weight, p.size_cap);
    s.powf(p.gamma)
}

/// Compute a stable area weight for every node (post-order).
fn compute_weights(tree: &Tree, p: &LayoutParams) -> Vec<f32> {
    let n = tree.nodes.len();
    let mut w = vec![0.0f32; n];
    // Iterative post-order to avoid recursion depth issues on deep trees.
    // Since children indices are always greater than parents here (arena built
    // top-down), a simple reverse pass works: process high indices first so a
    // parent sees its children's weights.
    for i in (0..n).rev() {
        let node = &tree.nodes[i];
        if node.is_dir {
            let mut sum = 0.0;
            for &c in &node.children {
                sum += w[c as usize];
            }
            // A directory with no laid children still gets a floor.
            w[i] = if sum > 0.0 {
                sum
            } else {
                weight_metric(node.raw_size.max(1), p)
            };
        } else {
            w[i] = weight_metric(node.raw_size, p);
        }
    }
    w
}

pub fn layout(tree: &Tree, frame: Rect, p: &LayoutParams) -> Layout {
    let weights = compute_weights(tree, p);
    let areas = if p.dir_scale.is_empty() {
        None
    } else {
        rule_areas(tree, &weights, p)
    };
    let mut out = Layout {
        tiles: Vec::with_capacity(tree.nodes.len()),
        index: FxHashMap::with_capacity_and_hasher(tree.nodes.len() * 2, Default::default()),
        frame,
    };
    // Lay the root's children directly into the frame (root itself isn't drawn).
    lay_children(tree, 0, frame, &weights, areas.as_deref(), p, &mut out);
    out
}

/// Sibling weights with folder balancing applied.
///
/// `weights` holds true (proportional) subtree sums. With `balance` b > 0 each
/// sibling folder's share becomes `sum^(1-b)`: a module 100x the size of its
/// neighbour gets ~32x the area at b = 0.25 instead of 100x, so big modules stop
/// crowding out small ones. Only the split among siblings is compressed — a
/// parent still uses its true sum one level up, so the effect does not compound
/// with depth. A folder's loose files count as ONE sibling (their combined sum,
/// split proportionally), otherwise hundreds of small files would each be
/// boosted and swamp the folders. Pure function of the tree, so still stable.
fn balanced_shares(tree: &Tree, children: &[u32], weights: &[f32], balance: f32) -> Vec<f32> {
    let b = balance.clamp(0.0, 0.9);
    if b <= 0.0 {
        return children.iter().map(|&c| weights[c as usize]).collect();
    }
    let e = 1.0 - b;
    let files_sum: f32 = children
        .iter()
        .filter(|&&c| !tree.nodes[c as usize].is_dir)
        .map(|&c| weights[c as usize])
        .sum();
    let file_scale = if files_sum > 0.0 {
        files_sum.powf(e) / files_sum
    } else {
        0.0
    };
    children
        .iter()
        .map(|&c| {
            let w = weights[c as usize];
            if tree.nodes[c as usize].is_dir {
                w.powf(e)
            } else {
                w * file_scale
            }
        })
        .collect()
}

/// Page areas with balance rules applied, for every node; `None` when no rule
/// matches a folder of this tree (then the plain balanced layout is used).
///
/// A rule scales a folder's area on the whole page relative to what the global
/// balance gives it: multiplier 2 (`+1`) doubles it, 0.5 (`-0.5`) halves it.
/// Its parent folders grow (or shrink) by the same absolute amount, and the rest
/// of the page makes room proportionally. A folder a rule enlarges is capped at
/// `max_share` of the page.
fn rule_areas(tree: &Tree, weights: &[f32], p: &LayoutParams) -> Option<Vec<f64>> {
    let nodes = &tree.nodes;
    let n = nodes.len();
    let mut mult = vec![1.0f64; n];
    let mut ruled = Vec::new();
    for (i, node) in nodes.iter().enumerate().skip(1) {
        if node.is_dir
            && let Some(&m) = p.dir_scale.get(&node.key)
        {
            mult[i] = m.max(0.01) as f64;
            ruled.push(i);
        }
    }
    if ruled.is_empty() {
        return None;
    }

    // Page fraction of every node under the global balance alone (top-down;
    // a child always comes after its parent in the arena).
    let mut base = vec![0.0f64; n];
    base[0] = 1.0;
    for i in 0..n {
        let kids = &nodes[i].children;
        if kids.is_empty() {
            continue;
        }
        let shares = balanced_shares(tree, kids, weights, p.balance);
        let sum: f64 = shares.iter().map(|&s| s as f64).sum();
        if sum > 0.0 {
            for (&c, &s) in kids.iter().zip(&shares) {
                base[c as usize] = base[i] * s as f64 / sum;
            }
        }
    }

    // Each ruled folder should end up with exactly `base * multiplier` of the
    // page (at most `max_share` when enlarged); everything else shares the
    // rest. Solve by fixed-point iteration on an effective multiplier: sum the
    // tree bottom-up (a folder = its children, times its multiplier, so parents
    // grow along), compare each ruled folder's share to its target, correct.
    let cap = (p.max_share as f64).clamp(0.05, 1.0);
    let mut target: Vec<f64> = ruled.iter().map(|&i| base[i] * mult[i]).collect();

    // Several enlarged folders together may still want more than the cap. Then
    // they balance each other: every enlargement (target - base) is scaled by
    // one common factor so the outermost enlarged folders (a ruled folder
    // inside another counts once, via its ancestor) fit within `max_share` —
    // each keeps its growth relative to the others and never drops below its
    // balanced size.
    let mut parent = vec![usize::MAX; n];
    for (i, node) in nodes.iter().enumerate() {
        for &c in &node.children {
            parent[c as usize] = i;
        }
    }
    let is_ruled: Vec<bool> = {
        let mut v = vec![false; n];
        ruled.iter().for_each(|&i| v[i] = true);
        v
    };
    let outermost = |i: usize| {
        let mut a = parent[i];
        while a != usize::MAX {
            if is_ruled[a] && mult[a] > 1.0 {
                return false;
            }
            a = parent[a];
        }
        true
    };
    let (mut grow, mut kept) = (0.0f64, 0.0f64);
    for (k, &i) in ruled.iter().enumerate() {
        if mult[i] > 1.0 && outermost(i) {
            grow += target[k] - base[i];
            kept += base[i];
        }
    }
    if grow > 0.0 && kept + grow > cap {
        let f = ((cap - kept) / grow).clamp(0.0, 1.0);
        for (k, &i) in ruled.iter().enumerate() {
            // Only the outermost ones compete for page room; a ruled folder
            // inside an enlarged one grows at its siblings' expense instead.
            if mult[i] > 1.0 && outermost(i) {
                target[k] = base[i] + (target[k] - base[i]) * f;
            }
        }
    }
    // Final guard for folders nested in another enlarged folder.
    for (k, &i) in ruled.iter().enumerate() {
        if mult[i] > 1.0 {
            target[k] = target[k].min(cap.max(base[i]));
        }
    }
    let mut area = vec![0.0f64; n];
    for _ in 0..48 {
        for i in (0..n).rev() {
            let kids = &nodes[i].children;
            let a = if kids.is_empty() {
                base[i]
            } else {
                kids.iter().map(|&c| area[c as usize]).sum()
            };
            area[i] = a * mult[i];
        }
        let total = area[0];
        let mut settled = true;
        for (&i, &t) in ruled.iter().zip(&target) {
            let share = area[i] / total;
            if share > 0.0 && (share / t - 1.0).abs() > 1e-4 {
                settled = false;
                // Moving this folder from share s to t (others fixed) scales it
                // by t(1-s) / (s(1-t)); exact for one rule, converges for more.
                let f = (t * (1.0 - share)) / (share * (1.0 - t).max(1e-9));
                mult[i] = (mult[i] * f).clamp(1e-4, 1e7);
            }
        }
        if settled {
            break;
        }
    }
    Some(area)
}

fn lay_children(
    tree: &Tree,
    parent: u32,
    rect: Rect,
    weights: &[f32],
    areas: Option<&[f64]>,
    p: &LayoutParams,
    out: &mut Layout,
) {
    let children = &tree.nodes[parent as usize].children;
    if children.is_empty() || rect.w <= 0.5 || rect.h <= 0.5 {
        return;
    }
    let child_weights = match areas {
        // Rules: page areas are already balanced and scaled.
        Some(a) => children.iter().map(|&c| a[c as usize] as f32).collect(),
        None => balanced_shares(tree, children, weights, p.balance),
    };
    let rects = squarified_ordered(rect, &child_weights);

    for (&cidx, crect) in children.iter().zip(rects.iter()) {
        lay_node(tree, cidx, *crect, weights, areas, p, out);
    }
}

fn lay_node(
    tree: &Tree,
    idx: u32,
    rect: Rect,
    weights: &[f32],
    areas: Option<&[f64]>,
    p: &LayoutParams,
    out: &mut Layout,
) {
    let node = &tree.nodes[idx as usize];
    let tile_i = out.tiles.len() as u32;
    let mut tile = LaidTile {
        key: node.key,
        rect,
        depth: node.depth,
        is_dir: node.is_dir,
        collapsed: false,
        group_hue: node.group_hue,
        raw_size: node.raw_size,
        path_id: node.path_id,
        node: idx,
        child_count: node.children.len() as u32,
    };

    if !node.is_dir {
        out.index.insert(node.key, tile_i);
        out.tiles.push(tile);
        return;
    }

    // Directory: draw as one collapsed tile when too small to open (LOD) or when
    // it is an aggregate leaf (a collapsed too-deep subfolder with no children).
    if rect.shorter() < p.min_open_px || node.children.is_empty() {
        tile.collapsed = true;
        out.index.insert(node.key, tile_i);
        out.tiles.push(tile);
        return;
    }

    out.index.insert(node.key, tile_i);
    out.tiles.push(tile);

    // Reserve padding and optional label space, then recurse.
    let mut inner = rect.inset(p.pad);
    if p.label_h > 0.0 && rect.h >= p.label_min_px && node.depth <= p.label_max_depth {
        inner.y += p.label_h;
        inner.h = (inner.h - p.label_h).max(0.0);
    }
    if inner.w > 0.5 && inner.h > 0.5 {
        lay_children(tree, idx, inner, weights, areas, p, out);
    }
}

/// Ordered squarified layout: pack `weights` (already in stable order) into
/// `rect`, grouping consecutive items into strips laid along the shorter side to
/// keep aspect ratios near square, WITHOUT reordering by size.
fn squarified_ordered(rect: Rect, weights: &[f32]) -> Vec<Rect> {
    let n = weights.len();
    let mut out = vec![Rect::new(rect.x, rect.y, 0.0, 0.0); n];
    let total: f32 = weights.iter().copied().filter(|w| *w > 0.0).sum();
    if total <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
        return out;
    }

    let mut area = rect;
    let mut remaining_total = total;
    let mut i = 0usize;

    while i < n {
        // Strip spans the shorter side of the current area.
        let horizontal = area.w <= area.h; // horizontal band spans full width
        let span = if horizontal { area.w } else { area.h };
        if span <= 0.0 {
            break;
        }
        let area_size = area.w * area.h;

        // Greedily grow the strip while it improves the worst aspect ratio.
        let mut j = i;
        let mut strip_weight = 0.0f32;
        let mut best_worst = f32::INFINITY;
        while j < n {
            let w = weights[j];
            let new_weight = strip_weight + w;
            if new_weight <= 0.0 {
                j += 1;
                continue;
            }
            let strip_area = new_weight / remaining_total * area_size;
            let thickness = strip_area / span;
            let worst = worst_ratio(&weights[i..=j], span, thickness);
            if worst <= best_worst {
                best_worst = worst;
                strip_weight = new_weight;
                j += 1;
            } else {
                break;
            }
        }
        if j == i {
            // Degenerate (all-zero weights ahead); place one and move on.
            j = i + 1;
            strip_weight = weights[i].max(f32::EPSILON);
        }

        // Finalize the strip covering items i..j.
        let strip_area = strip_weight / remaining_total * area_size;
        let thickness = (strip_area / span).min(if horizontal { area.h } else { area.w });

        if horizontal {
            let mut x = area.x;
            for k in i..j {
                let frac = if strip_weight > 0.0 {
                    weights[k] / strip_weight
                } else {
                    1.0 / (j - i) as f32
                };
                let w = span * frac;
                out[k] = Rect::new(x, area.y, w, thickness);
                x += w;
            }
            area.y += thickness;
            area.h -= thickness;
        } else {
            let mut y = area.y;
            for k in i..j {
                let frac = if strip_weight > 0.0 {
                    weights[k] / strip_weight
                } else {
                    1.0 / (j - i) as f32
                };
                let h = span * frac;
                out[k] = Rect::new(area.x, y, thickness, h);
                y += h;
            }
            area.x += thickness;
            area.w -= thickness;
        }

        remaining_total -= strip_weight;
        i = j;
        if remaining_total <= 0.0 {
            break;
        }
    }

    out
}

/// Worst (max) aspect ratio among items placed along `span` with the given
/// strip `thickness`. Lower is better (1.0 == square).
#[inline]
fn worst_ratio(weights: &[f32], span: f32, thickness: f32) -> f32 {
    let sum: f32 = weights.iter().copied().sum();
    if sum <= 0.0 || span <= 0.0 || thickness <= 0.0 {
        return f32::INFINITY;
    }
    let mut worst = 1.0f32;
    for &w in weights {
        let len = w / sum * span;
        if len <= 0.0 {
            continue;
        }
        let ratio = (len / thickness).max(thickness / len);
        if ratio > worst {
            worst = ratio;
        }
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;
    use crate::ingest::History;
    use crate::model::build_tree;

    fn hist(paths: &[&str]) -> History {
        History {
            paths: paths.iter().map(|s| s.to_string()).collect(),
            authors: vec![],
            commits: vec![],
            baseline: vec![],
            submodules: vec![],
        }
    }

    #[test]
    fn layout_is_deterministic() {
        let h = hist(&["src/a.rs", "src/b.rs", "tests/c.rs"]);
        let files = vec![(0u32, 10u32), (1u32, 20u32), (2u32, 5u32)];
        let tree = build_tree(
            &files,
            &h,
            &crate::groups::ColorMap::top_level(&h.paths),
            0,
            false,
        );
        let p = LayoutParams::default();
        let frame = Rect::new(0.0, 0.0, 800.0, 600.0);
        let l1 = layout(&tree, frame, &p);
        let l2 = layout(&tree, frame, &p);
        assert!(!l1.tiles.is_empty());
        assert_eq!(l1.tiles.len(), l2.tiles.len());
        for (a, b) in l1.tiles.iter().zip(&l2.tiles) {
            assert_eq!(a.key, b.key);
            assert_eq!(a.rect, b.rect);
        }
    }

    #[test]
    fn balance_shrinks_dominant_folder_but_keeps_order() {
        // `big` holds 100x the lines of `small`, plus many loose root files.
        let mut paths = vec!["big/a.rs".to_string(), "small/b.rs".to_string()];
        let mut files = vec![(0u32, 100_000u32), (1u32, 1_000u32)];
        for i in 0..50u32 {
            paths.push(format!("f{i}.txt"));
            files.push((i + 2, 10));
        }
        let h = History {
            paths,
            authors: vec![],
            commits: vec![],
            baseline: vec![],
            submodules: vec![],
        };
        let tree = build_tree(
            &files,
            &h,
            &crate::groups::ColorMap::top_level(&h.paths),
            0,
            false,
        );
        let frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let area = |balance: f32, name: &str| {
            let p = LayoutParams {
                balance,
                size_cap: 1e9,
                gamma: 1.0,
                ..LayoutParams::default()
            };
            let l = layout(&tree, frame, &p);
            let t = l.get(crate::color::hash_str(name)).expect("tile");
            t.rect.w * t.rect.h
        };
        let (big0, small0) = (area(0.0, "big"), area(0.0, "small"));
        let (big1, small1) = (area(0.5, "big"), area(0.5, "small"));
        assert!((big0 / small0 - 100.0).abs() < 5.0, "proportional when off");
        assert!(big1 < big0 && small1 > small0, "balance evens siblings out");
        assert!(big1 > small1, "the bigger folder stays bigger");
        // Loose files act as one sibling: 50 tiny files must not swamp `small`.
        let loose: f32 = (0..50).map(|i| area(0.5, &format!("f{i}.txt"))).sum();
        assert!(loose < small1);
    }

    #[test]
    fn balance_rule_scales_page_area_relative_to_balanced_size() {
        let h = History {
            paths: vec![
                "big/a.rs".into(),
                "s1/b.rs".into(),
                "s2/c.rs".into(),
                "big/sub/d.rs".into(),
            ],
            authors: vec![],
            commits: vec![],
            baseline: vec![],
            submodules: vec![],
        };
        let files = [(0u32, 100_000u32), (1, 100), (2, 100), (3, 300)];
        let tree = build_tree(
            &files,
            &h,
            &crate::groups::ColorMap::top_level(&h.paths),
            0,
            false,
        );
        let frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let page = frame.w * frame.h;
        let key = |n: &str| crate::color::hash_str(n);
        let area = |rules: &[(&str, f32)], name: &str| {
            let p = LayoutParams {
                balance: 0.25,
                size_cap: 1e9,
                gamma: 1.0,
                pad: 0.0,
                label_h: 0.0,
                min_open_px: 0.0,
                dir_scale: std::sync::Arc::new(
                    rules.iter().map(|&(n, d)| (key(n), 1.0 + d)).collect(),
                ),
                ..LayoutParams::default()
            };
            let l = layout(&tree, frame, &p);
            let t = l.get(key(name)).expect("tile");
            t.rect.w * t.rect.h
        };
        let near = |a: f32, b: f32| (a / b - 1.0).abs() < 0.03;
        let s1 = area(&[], "s1");
        // +1 doubles a folder's share of the page; -0.5 halves it.
        assert!(near(area(&[("s1", 1.0)], "s1"), 2.0 * s1));
        assert!(near(area(&[("s1", -0.5)], "s1"), 0.5 * s1));
        // Nested folders grow in absolute terms too (their parent makes room).
        let sub = area(&[], "big/sub");
        assert!(near(area(&[("big/sub", 1.0)], "big/sub"), 2.0 * sub));
        // A rule matching no folder of this tree changes nothing.
        assert_eq!(area(&[("nope", 5.0)], "s1"), s1);
        // Absurd rules are capped at max_share (0.85) of the page.
        assert!(near(area(&[("s1", 1000.0)], "s1"), 0.85 * page));
        // Several enlarged folders over the limit share it, keeping their growth
        // relative to each other: equal rules on equal folders -> equal halves.
        let both = [("s1", 1000.0), ("s2", 1000.0)];
        let (a1, a2) = (area(&both, "s1"), area(&both, "s2"));
        assert!(near(a1, a2));
        assert!(near(a1 + a2, 0.85 * page));
        // Unequal rules: growth stays proportional (s1 wants 3x the growth of s2).
        let uneq = [("s1", 300.0), ("s2", 100.0)];
        let (g1, g2) = (area(&uneq, "s1") - s1, area(&uneq, "s2") - s1);
        assert!((g1 / g2 - 3.0).abs() < 0.1, "growth ratio {}", g1 / g2);
        assert!(
            near(area(&uneq, "s1") + area(&uneq, "s2"), 0.85 * page),
            "uneq sum {}",
            (area(&uneq, "s1") + area(&uneq, "s2")) / page
        );
        // `big` alone already exceeds the cap: it keeps its balanced size (never
        // shrunk), so the cap leaves no room for `s1` to grow — but a folder
        // nested in `big` still grows, taking space from its siblings in `big`.
        let nested = [("big", 5.0), ("big/sub", 1.0), ("s1", 5.0)];
        assert!(near(area(&nested, "big"), area(&[], "big")));
        assert!(near(area(&nested, "s1"), s1));
        assert!(near(area(&nested, "big/sub"), 2.0 * sub));
    }
}
