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

#[derive(Clone, Copy)]
pub struct LayoutParams {
    /// Gamma compression exponent for the area metric (0.5 ≈ sqrt).
    pub gamma: f32,
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
    let mut out = Layout {
        tiles: Vec::with_capacity(tree.nodes.len()),
        index: FxHashMap::with_capacity_and_hasher(tree.nodes.len() * 2, Default::default()),
        frame,
    };
    // Lay the root's children directly into the frame (root itself isn't drawn).
    lay_children(tree, 0, frame, &weights, p, &mut out);
    out
}

fn lay_children(
    tree: &Tree,
    parent: u32,
    rect: Rect,
    weights: &[f32],
    p: &LayoutParams,
    out: &mut Layout,
) {
    let children = &tree.nodes[parent as usize].children;
    if children.is_empty() || rect.w <= 0.5 || rect.h <= 0.5 {
        return;
    }
    let child_weights: Vec<f32> = children.iter().map(|&c| weights[c as usize]).collect();
    let rects = squarified_ordered(rect, &child_weights);

    for (&cidx, crect) in children.iter().zip(rects.iter()) {
        lay_node(tree, cidx, *crect, weights, p, out);
    }
}

fn lay_node(
    tree: &Tree,
    idx: u32,
    rect: Rect,
    weights: &[f32],
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
        lay_children(tree, idx, inner, weights, p, out);
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
}
