//! The "modern" visual style (the default; `--style classic` keeps the original
//! flat wireframe look in `frame.rs`).
//!
//! Design language:
//! - **Depth.** A gradient backdrop with a vignette; folder fills lighten as they
//!   nest (elevation) and folder edges fade with depth, so top-level modules
//!   read as crisp regions and deep folders recede.
//! - **Cards, not wireframes.** Files are filled cards separated by a 1px gutter
//!   with a lit top edge — no strokes. Their minimap reads like an editor
//!   minimap: indented, tokenized lines with keyword accents and comments.
//! - **Light.** Changes bloom (additive halo) in mint / amber / red; beams are
//!   glowing arcs with a gradient tail, an impact glow and a travelling pulse;
//!   avatars sit in a coloured ring over a soft shadow and an aura.
//! - **Quiet typography.** Labels without boxes (text shadow instead), clean
//!   ellipsis truncation, stubs too short to read are dropped.

use std::borrow::Cow;

use tiny_skia::{
    BlendMode, Color, GradientStop, LineCap, LinearGradient, Paint, Path, PathBuilder, Pixmap,
    Point, SpreadMode, Stroke, Transform,
};

use super::avatar::{blit_glow, blit_sprite};
use super::frame::{RTile, RenderCtx, border_rect, fill_rect, frac01, hline, tile_name};
use super::text::GlyphCache;
use super::{AvatarDraw, Beam, FramePlan};
use crate::color::{GroupColor, hsl_to_rgb, mix};
use crate::fmt::{commafy, fmt_compact};
use crate::geom::Rect;
use crate::ingest::ChangeKind;

pub const ADDED: [u8; 3] = [86, 232, 162];
pub const MODIFIED: [u8; 3] = [255, 196, 92];
pub const DELETED: [u8; 3] = [255, 92, 112];

#[inline]
fn glow_color(kind: ChangeKind) -> [u8; 3] {
    match kind {
        ChangeKind::Added => ADDED,
        ChangeKind::Modified => MODIFIED,
        ChangeKind::Deleted => DELETED,
    }
}

/// 4x4 Bayer matrix, for dithering the backdrop gradient (dark gradients band
/// badly once encoded).
const BAYER: [[f32; 4]; 4] = [
    [0.0, 8.0, 2.0, 10.0],
    [12.0, 4.0, 14.0, 6.0],
    [3.0, 11.0, 1.0, 9.0],
    [15.0, 7.0, 13.0, 5.0],
];

/// Backdrop: `bg` lifted and cooled at the top, deepened at the bottom, with a
/// faint pool of light at the top centre. Prebuilt once (premultiplied RGBA,
/// opaque) and copied into every frame.
pub fn build_backdrop(w: u32, h: u32, bg: [u8; 3]) -> Vec<u8> {
    let top = mix(bg, [30, 36, 58], 0.55);
    let bottom = mix(bg, [0, 0, 0], 0.4);
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        let t = (y as f32 / h.max(1) as f32).powf(0.8);
        let row = [0, 1, 2].map(|k| top[k] as f32 + (bottom[k] as f32 - top[k] as f32) * t);
        for x in 0..w {
            let dx = (x as f32 / w as f32 - 0.5) * 1.5;
            let dy = y as f32 / h as f32 * 2.0;
            let light = (1.0 - (dx * dx + dy * dy)).max(0.0).powi(2) * 12.0;
            let dither = BAYER[(y & 3) as usize][(x & 3) as usize] / 16.0 - 0.47;
            let i = ((y * w + x) * 4) as usize;
            let tint = [0.8, 0.9, 1.25];
            for k in 0..3 {
                out[i + k] = (row[k] + light * tint[k] + dither).clamp(0.0, 255.0) as u8;
            }
            out[i + 3] = 255;
        }
    }
    out
}

/// Per-pixel brightness factor (0..=256) darkening the atlas toward its edges;
/// applied while repacking to rgb24, so it costs nothing extra. The HUD band
/// (`top` px) is left untouched.
pub fn build_vignette(w: u32, h: u32, top: f32) -> Vec<u16> {
    let top = top.max(0.0) as u32;
    let ah = (h.saturating_sub(top)).max(1) as f32;
    let mut out = vec![256u16; (w * h) as usize];
    for y in top..h {
        let ny = ((y - top) as f32 / ah - 0.5) * 2.0;
        for x in 0..w {
            let nx = (x as f32 / w as f32 - 0.5) * 2.0;
            let d = (nx * nx * 0.6 + ny * ny * 0.75).sqrt();
            let f = 1.0 - 0.32 * smoothstep(0.5, 1.3, d);
            out[(y * w + x) as usize] = (f * 256.0) as u16;
        }
    }
    out
}

/// Premultiplied RGBA -> rgb24 with the vignette applied.
pub fn repack_vignette(rgba: &[u8], vig: &[u16], out: &mut [u8]) {
    let (px, _) = out.as_chunks_mut::<3>();
    for (p, (&f, o)) in vig.iter().zip(px).enumerate() {
        let si = p * 4;
        let f = f as u32;
        o[0] = ((rgba[si] as u32 * f) >> 8) as u8;
        o[1] = ((rgba[si + 1] as u32 * f) >> 8) as u8;
        o[2] = ((rgba[si + 2] as u32 * f) >> 8) as u8;
    }
}

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ------------------------------------------------------------------ tiles ---

pub(super) fn draw_tile(
    ctx: &RenderCtx,
    plan: &FramePlan,
    t: &RTile,
    pm: &mut Pixmap,
    pw: i32,
    ph: i32,
) {
    let cfg = &ctx.cfg;
    let r = t.rect;
    if r.w < 1.0 || r.h < 1.0 || t.alpha <= 0.01 || !r.intersects_viewport(pw as f32, ph as f32) {
        return;
    }
    let gc = ctx.group_color(t.group_hue);
    let glow = plan.changed.get(&t.key);
    let gi = glow.map(|g| g.intensity).unwrap_or(0.0);
    let gcol = glow.map(|g| glow_color(g.kind)).unwrap_or(MODIFIED);
    let alpha = t.alpha.min(1.0);
    let data = pm.data_mut();

    if t.is_dir && !t.collapsed {
        // Open folder: an elevated panel whose edge fades with depth.
        let tier = (t.depth as usize).min(3);
        let et = (t.depth as usize).min(2);
        let mut fill = gc.m_dir[tier];
        if gi > 0.0 {
            fill = mix(fill, gcol, gi * 0.18);
        }
        fill_rect(data, pw, ph, &r, fill, alpha);
        // Header tab, where the layout reserved room for the name.
        if cfg.show_dir_names && t.depth <= cfg.dir_label_depth() && r.h >= cfg.label_min_px() {
            let band = Rect::new(r.x, r.y, r.w, cfg.label_size + 3.0 + cfg.pad);
            fill_rect(data, pw, ph, &band, mix(fill, gc.m_edge[et], 0.12), alpha);
        }
        if cfg.border && r.w > 3.0 && r.h > 3.0 {
            let edge = if gi > 0.0 {
                mix(gc.m_edge[et], gcol, gi)
            } else {
                gc.m_edge[et]
            };
            let ea = [0.95, 0.6, 0.38][et];
            border_rect(data, pw, ph, &r, edge, alpha * ea, cfg.border_width);
            // A faint lit top edge on modules, like light catching a panel.
            if t.depth == 0 && r.h > 8.0 {
                let y = r.y.round() as i32 + cfg.border_width.max(1.0).round() as i32;
                let (x0, x1) = ((r.x + 2.0) as i32, (r.x + r.w - 2.0) as i32);
                hline(
                    data,
                    pw,
                    ph,
                    y,
                    x0,
                    x1,
                    mix(edge, [255, 255, 255], 0.35),
                    alpha * 0.28,
                );
            }
        }
    } else {
        // File (or folder too small to open): a card with a 1px gutter.
        let card = if r.w > 4.0 && r.h > 4.0 {
            Rect::new(r.x, r.y, r.w - 1.0, r.h - 1.0)
        } else {
            r
        };
        let mut fill = if t.collapsed {
            gc.m_collapsed
        } else {
            gc.m_file
        };
        let mut top = gc.m_file_top;
        if gi > 0.0 {
            fill = mix(fill, gcol, (gi * 0.42).min(0.55));
            top = mix(top, gcol, gi);
        }
        fill_rect(data, pw, ph, &card, fill, alpha);
        if card.w >= 4.0 && card.h >= 4.0 {
            let y = card.y.round() as i32;
            hline(
                data,
                pw,
                ph,
                y,
                card.x as i32,
                (card.x + card.w) as i32,
                top,
                alpha * 0.9,
            );
        }
        if cfg.show_minimap && card.h >= 5.0 && card.w >= 6.0 {
            draw_code(
                data,
                pw,
                ph,
                &card,
                t,
                &gc,
                cfg.minimap_line_gap,
                cfg.minimap_max_lines,
                alpha,
            );
        }
        if gi > 0.0 && card.w > 3.0 && card.h > 3.0 {
            border_rect(data, pw, ph, &card, gcol, alpha * gi, 1.0);
        } else if t.collapsed && cfg.border && card.w > 3.0 && card.h > 3.0 {
            border_rect(data, pw, ph, &card, gc.m_edge[2], alpha * 0.45, 1.0);
        }
    }

    // Bloom around a changed tile.
    if gi > 0.02 {
        const RINGS: [f32; 5] = [0.34, 0.22, 0.13, 0.07, 0.035];
        for (k, a) in RINGS.iter().enumerate() {
            let e = (k + 1) as f32;
            let rr = Rect::new(r.x - e, r.y - e, r.w + 2.0 * e, r.h + 2.0 * e);
            add_outline(data, pw, ph, &rr, gcol, a * gi * alpha);
        }
    }

    draw_label(ctx, plan, t, pm);
}

/// Editor-style minimap: indented lines of 1-4 "tokens", some starting with a
/// keyword accent, some whole-line comments. Stable per file (seeded by key).
#[allow(clippy::too_many_arguments)]
fn draw_code(
    data: &mut [u8],
    pw: i32,
    ph: i32,
    r: &Rect,
    t: &RTile,
    gc: &GroupColor,
    line_gap: f32,
    max_lines: u32,
    alpha: f32,
) {
    let gap = line_gap.max(1.5);
    let inset = if r.h < 12.0 || r.w < 12.0 { 2.0 } else { 3.0 };
    if t.raw_size == 0 || r.h < inset * 2.0 + 1.0 {
        return;
    }
    let top = r.y + inset + 1.0; // clear of the lit top edge
    let rows = (((r.h - inset * 2.0 - 1.0) / gap).floor() as u32 + 1).min(max_lines);
    let n = t.raw_size.max((rows as f32 * 0.6).ceil() as u32).min(rows);
    let usable = r.w - inset * 2.0;
    if usable < 2.0 {
        return;
    }
    let left = r.x + inset;
    let right = r.x + r.w - inset;
    let token_gap = if usable > 40.0 { 3.0 } else { 2.0 };
    for i in 0..n {
        let y = (top + i as f32 * gap) as i32;
        if y < 0 || y >= ph {
            continue;
        }
        let seed = t.key ^ (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let level = [0.0, 1.0, 1.0, 2.0, 2.0, 3.0][(frac01(seed >> 17) * 6.0) as usize % 6];
        let x0 = left + (usable * 0.07 * level).min(usable * 0.3);
        let end = (x0 + usable * (0.22 + 0.66 * frac01(seed))).min(right);
        if frac01(seed >> 11) < 0.1 {
            hline(data, pw, ph, y, x0 as i32, end as i32, gc.m_code[2], alpha);
            continue;
        }
        // Narrow cards: one plain line (short tokens would read as dot noise).
        if usable < 48.0 {
            let col = if frac01(seed >> 29) < 0.3 {
                gc.m_code[1]
            } else {
                gc.m_code[0]
            };
            hline(data, pw, ph, y, x0 as i32, end as i32, col, alpha);
            continue;
        }
        let mut x = x0;
        for k in 0..4u32 {
            if x >= end {
                break;
            }
            let tl = (usable * (0.06 + 0.22 * frac01(seed.rotate_left(k * 13 + 5)))).max(5.0);
            let x1 = (x + tl).min(end);
            if x1 - x < 3.0 {
                break;
            }
            let col = if k == 0 && frac01(seed >> 29) < 0.4 {
                gc.m_code[1]
            } else {
                gc.m_code[0]
            };
            hline(data, pw, ph, y, x as i32, x1 as i32, col, alpha);
            x = x1 + token_gap;
        }
    }
}

fn draw_label(ctx: &RenderCtx, plan: &FramePlan, t: &RTile, pm: &mut Pixmap) {
    let cfg = &ctx.cfg;
    let r = t.rect;
    let name = tile_name(plan, t);
    if name.is_empty() {
        return;
    }
    let cache = &ctx.label_cache;
    let px = cache.px();
    let alpha = t.alpha.min(1.0);
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);

    if t.is_dir {
        if !cfg.show_dir_names || t.depth > cfg.dir_label_depth() {
            return;
        }
        if !t.collapsed && r.h < cfg.label_min_px() {
            return;
        }
        let maxw = r.w - cfg.pad * 2.0 - 2.0;
        if maxw < 12.0 || r.h < px + 4.0 {
            return;
        }
        let Some(text) = fit(cache, name, maxw) else {
            return;
        };
        let gc = ctx.group_color(t.group_hue);
        let tier = if t.collapsed {
            2
        } else {
            (t.depth as usize).min(2)
        };
        let x = r.x + cfg.pad + 1.0;
        let baseline = r.y + cfg.pad + px * 0.85;
        if t.collapsed {
            // Sits on code lines: give it a soft backing.
            let tw = cache.measure(&text);
            let data = pm.data_mut();
            let back = Rect::new(x - 2.0, baseline - px * 0.9, tw + 4.0, px + 4.0);
            fill_rect(data, pw, ph, &back, [5, 6, 11], 0.55 * alpha);
        }
        cache.draw(
            pm,
            &text,
            x + 1.0,
            baseline + 1.0,
            [0, 0, 0],
            0.75 * alpha,
            maxw + 2.0,
        );
        cache.draw(pm, &text, x, baseline, gc.m_label[tier], alpha, maxw + 2.0);
    } else {
        if !cfg.show_file_names || r.w < cfg.file_name_min_px || r.h < px + 2.0 {
            return;
        }
        let maxw = r.w - 8.0;
        let Some(text) = fit(cache, name, maxw) else {
            return;
        };
        let tw = cache.measure(&text);
        let baseline = r.y + r.h * 0.5 + px * 0.35;
        {
            let data = pm.data_mut();
            let back = Rect::new(r.x + 2.0, baseline - px * 0.88, tw + 6.0, px + 4.0);
            fill_rect(data, pw, ph, &back, [5, 6, 11], 0.62 * alpha);
        }
        cache.draw(
            pm,
            &text,
            r.x + 5.0,
            baseline,
            [230, 234, 242],
            alpha,
            maxw + 2.0,
        );
    }
}

/// The name, or its longest prefix (at least 3 chars) plus an ellipsis, that
/// fits `maxw`; `None` when not even that fits — a 1-2 letter stub reads as noise.
fn fit<'a>(cache: &GlyphCache, name: &'a str, maxw: f32) -> Option<Cow<'a, str>> {
    if cache.measure(name) <= maxw {
        return Some(Cow::Borrowed(name));
    }
    let ell = cache.measure("…");
    let mut best = None;
    let mut w = 0.0;
    for (n, (i, c)) in name.char_indices().enumerate() {
        w += cache.measure(c.encode_utf8(&mut [0u8; 4]));
        if w + ell > maxw {
            break;
        }
        if n + 1 >= 3 {
            best = Some(i + c.len_utf8());
        }
    }
    best.map(|end| Cow::Owned(format!("{}…", &name[..end])))
}

/// Additive 1px rectangle outline (for blooms).
fn add_outline(data: &mut [u8], pw: i32, ph: i32, r: &Rect, c: [u8; 3], a: f32) {
    if a <= 0.004 {
        return;
    }
    let (x0, y0) = (r.x.round() as i32, r.y.round() as i32);
    let (x1, y1) = ((r.x + r.w).round() as i32, (r.y + r.h).round() as i32);
    let add = [0, 1, 2].map(|k| (c[k] as f32 * a) as u32);
    let mut px = |x: i32, y: i32| {
        if x >= 0 && y >= 0 && x < pw && y < ph {
            let i = ((y * pw + x) * 4) as usize;
            for k in 0..3 {
                data[i + k] = (data[i + k] as u32 + add[k]).min(255) as u8;
            }
        }
    };
    for x in x0..x1 {
        px(x, y0);
        px(x, y1 - 1);
    }
    for y in (y0 + 1)..(y1 - 1) {
        px(x0, y);
        px(x1 - 1, y);
    }
}

// ------------------------------------------------------------------ beams ---

/// A glowing arc from the avatar to a changed file: faint at the avatar,
/// brightening toward the file, with an impact glow and a travelling pulse.
pub fn draw_beam(pm: &mut Pixmap, b: &Beam, global: f32, time: f32) {
    let intensity = (b.intensity * global).clamp(0.0, 1.5);
    if intensity <= 0.01 {
        return;
    }
    let (dx, dy) = (b.x1 - b.x0, b.y1 - b.y0);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 2.0 {
        return;
    }
    let seed = ((b.x1.to_bits() as u64) << 32) | b.y1.to_bits() as u64;
    let side = if frac01(seed) < 0.5 { -1.0 } else { 1.0 };
    let bend = len * 0.16 * side;
    let (cx, cy) = (
        (b.x0 + b.x1) * 0.5 - dy / len * bend,
        (b.y0 + b.y1) * 0.5 + dx / len * bend,
    );
    let col = hsl_to_rgb(b.hue, 0.9, 0.62);
    let core = mix(col, [255, 255, 255], 0.55);

    let mut pb = PathBuilder::new();
    pb.move_to(b.x0, b.y0);
    pb.quad_to(cx, cy, b.x1, b.y1);
    let Some(path) = pb.finish() else {
        return;
    };
    // (width, alpha, colour, anti-alias): the wide faint glow skips AA (its
    // edges are invisible anyway) — AA on wide strokes is the costly part.
    let passes = [
        (7.0, 0.1, col, false),
        (2.4, 0.42, col, true),
        (1.1, 0.7, core, true),
    ];
    for (width, a, c, aa) in passes {
        let a1 = (a * intensity).min(1.0);
        let shader = LinearGradient::new(
            Point::from_xy(b.x0, b.y0),
            Point::from_xy(b.x1, b.y1),
            vec![
                GradientStop::new(0.0, rgba(c, a1 * 0.12)),
                GradientStop::new(1.0, rgba(c, a1)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        );
        let Some(shader) = shader else {
            continue;
        };
        let paint = Paint {
            shader,
            blend_mode: BlendMode::Plus,
            anti_alias: aa,
            ..Paint::default()
        };
        let stroke = Stroke {
            width,
            line_cap: LineCap::Round,
            ..Stroke::default()
        };
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    // Impact glow at the file.
    for (r, a, c) in [(11.0, 0.1, col), (5.5, 0.24, col), (2.4, 0.8, core)] {
        glow_dot(pm, b.x1, b.y1, r, c, a * intensity);
    }
    // A pulse travelling along the arc toward the file.
    let t = (time * 0.8 + frac01(seed >> 20)).fract();
    let u = 1.0 - t;
    let (px, py) = (
        u * u * b.x0 + 2.0 * u * t * cx + t * t * b.x1,
        u * u * b.y0 + 2.0 * u * t * cy + t * t * b.y1,
    );
    glow_dot(pm, px, py, 6.0, col, 0.14 * intensity);
    glow_dot(pm, px, py, 2.4, core, 0.75 * intensity);
}

fn glow_dot(pm: &mut Pixmap, x: f32, y: f32, r: f32, c: [u8; 3], a: f32) {
    let Some(path) = PathBuilder::from_circle(x, y, r) else {
        return;
    };
    let mut paint = Paint {
        blend_mode: BlendMode::Plus,
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(rgba(c, a.min(1.0)));
    pm.fill_path(
        &path,
        &paint,
        tiny_skia::FillRule::Winding,
        Transform::identity(),
        None,
    );
}

#[inline]
fn rgba(c: [u8; 3], a: f32) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], (a.clamp(0.0, 1.0) * 255.0) as u8)
}

// ---------------------------------------------------------------- avatars ---

pub fn draw_avatar(ctx: &RenderCtx, pm: &mut Pixmap, a: &AvatarDraw) {
    let i = a.author as usize;
    if let Some((size, mask)) = &ctx.avatars.halo {
        let hue = ctx.avatars.hues.get(i).copied().unwrap_or(0.0);
        let col = hsl_to_rgb(hue, 0.9, 0.58);
        blit_glow(pm, (*size, mask), a.x, a.y, col, a.alpha * 0.9);
    }
    if let Some(sprite) = ctx.avatars.sprites.get(i) {
        blit_sprite(pm, sprite, a.x, a.y, a.alpha);
    }
    if ctx.cfg.show_avatar_names
        && let Some(name) = ctx.avatars.names.get(i)
    {
        let hue = ctx.avatars.hues.get(i).copied().unwrap_or(0.0);
        name_pill(ctx, pm, name, a.x, a.y, hue, a.alpha);
    }
}

/// The committer's name on a translucent pill below the avatar, with a dot in
/// their colour.
fn name_pill(ctx: &RenderCtx, pm: &mut Pixmap, name: &str, cx: f32, cy: f32, hue: f32, alpha: f32) {
    let cache = &ctx.label_cache;
    let px = cache.px();
    let alpha = alpha.clamp(0.0, 1.0);
    let tw = cache.measure(name).min(360.0);
    if tw < 4.0 || alpha <= 0.01 {
        return;
    }
    let h = px + 9.0;
    let dot_w = 12.0;
    let w = tw + 18.0 + dot_w;
    let top = cy + ctx.cfg.avatar_size as f32 * 0.5 + 10.0;
    let x = cx - w * 0.5;
    let accent = hsl_to_rgb(hue, 0.85, 0.63);
    if let Some(path) = pill(x, top, w, h) {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(rgba([9, 11, 19], 0.78 * alpha));
        pm.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
        paint.set_color(rgba(accent, 0.55 * alpha));
        let stroke = Stroke {
            width: 1.0,
            ..Stroke::default()
        };
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
    if let Some(dot) = PathBuilder::from_circle(x + 11.0, top + h * 0.5, 3.0) {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(rgba(accent, alpha));
        pm.fill_path(
            &dot,
            &paint,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    let text = hsl_to_rgb(hue, 0.45, 0.88);
    let baseline = top + h * 0.5 + px * 0.36;
    cache.draw(pm, name, x + 9.0 + dot_w, baseline, text, alpha, tw + 2.0);
}

/// A fully rounded rectangle (capsule).
fn pill(x: f32, y: f32, w: f32, h: f32) -> Option<Path> {
    let r = (h * 0.5).min(w * 0.5);
    let k = 0.552_284_8 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

// -------------------------------------------------------------------- HUD ---

/// Glassy top bar: gradient band, gradient progress line with a glow, author
/// highlighted, language split with colour dots.
pub fn draw_hud(ctx: &RenderCtx, plan: &FramePlan, pm: &mut Pixmap) {
    let cfg = &ctx.cfg;
    let hud = &plan.hud;
    let w = pm.width() as f32;
    let hb = ctx.hud_h;
    if hb <= 1.0 {
        return;
    }
    let (iw, ih) = (pm.width() as i32, pm.height() as i32);
    let big = &ctx.hud_cache;
    let small = &ctx.label_cache;
    let fg = [236, 239, 246];
    let dim = [150, 158, 176];
    let accent = [140, 190, 255];
    let pad = 20.0;

    {
        let data = pm.data_mut();
        let top = mix(ctx.bg, [40, 46, 70], 0.62);
        let bottom = mix(ctx.bg, [22, 26, 40], 0.55);
        for y in 0..(hb as i32) {
            let c = mix(top, bottom, y as f32 / hb);
            hline(data, iw, ih, y, 0, iw, c, 1.0);
        }
        hline(data, iw, ih, hb as i32 - 1, 0, iw, [48, 56, 78], 1.0);
    }

    let y1 = 6.0 + big.px() * 0.95;
    let y2 = y1 + small.line_height + 2.0;

    let title = cfg.title.clone().unwrap_or_else(|| "gitatlas".to_string());
    let mut x = big.draw(pm, &title, pad, y1, fg, 1.0, w * 0.55);
    if cfg.show_date && !hud.date.is_empty() {
        x += 18.0;
        big.draw(pm, &hud.date, x, y1, accent, 1.0, 240.0);
    }

    let num = commafy((hud.commit_idx + 1) as u64).to_string();
    let of = format!(" / {}", commafy(hud.total_commits as u64));
    let (nw, ow) = (big.measure(&num), small.measure(&of));
    let cx = w - pad - nw - ow;
    let lbl = "commit ";
    big.draw(pm, &num, cx, y1, fg, 1.0, nw + 4.0);
    small.draw(pm, &of, cx + nw, y1, dim, 1.0, ow + 4.0);
    small.draw(pm, lbl, cx - small.measure(lbl), y1, dim, 1.0, 80.0);

    if cfg.show_author {
        let x = small.draw(pm, &hud.author, pad, y2, [214, 220, 234], 1.0, w * 0.3);
        if !hud.subject.is_empty() {
            let rest = format!("  {}", hud.subject);
            small.draw(pm, &rest, x, y2, dim, 1.0, w * 0.64 - (x - pad));
        }
    }

    let stats = format!(
        "{} files · {} LoC",
        commafy(hud.files as u64),
        commafy(hud.lines)
    );
    let sw = small.measure(&stats);
    small.draw(pm, &stats, w - sw - pad, y2, dim, 1.0, sw + 4.0);

    if cfg.show_langs && !hud.langs.is_empty() {
        let total = hud.lines.max(1);
        let y3 = y2 + small.line_height + 2.0;
        let dot = small.px() * 0.36;
        let gap = 18.0;
        let mut toks: Vec<([u8; 3], String, u64)> = hud
            .langs
            .iter()
            .take(6)
            .map(|(lid, lines, files)| {
                let l = &crate::lang::LANGS[*lid as usize];
                let pct = *lines as f64 / total as f64 * 100.0;
                let text = format!(
                    "{} {:.1}% {}f·{}",
                    l.name,
                    pct,
                    commafy(*files as u64),
                    fmt_compact(*lines)
                );
                (l.color, text, *lines)
            })
            .collect();
        let tok_w = |t: &str| dot * 2.0 + 6.0 + small.measure(t);
        let total_w = |ts: &[([u8; 3], String, u64)]| -> f32 {
            ts.iter().map(|(_, t, _)| tok_w(t) + gap).sum::<f32>() - gap
        };
        while toks.len() > 1 && total_w(&toks) > w - 2.0 * pad {
            toks.pop();
        }
        let span = total_w(&toks).max(0.0);
        let x0 = (w - pad - span).max(pad);
        let mut x = x0;
        for (color, text, _) in &toks {
            if let Some(p) = PathBuilder::from_circle(x + dot, y3 - small.px() * 0.33, dot) {
                let mut paint = Paint {
                    anti_alias: true,
                    ..Paint::default()
                };
                paint.set_color(rgba(*color, 1.0));
                pm.fill_path(
                    &p,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
            x += dot * 2.0 + 6.0;
            x = small.draw(
                pm,
                text,
                x,
                y3,
                [206, 212, 226],
                1.0,
                small.measure(text) + 4.0,
            );
            x += gap;
        }
    }

    if cfg.show_progress {
        let by = hb - 3.0;
        let filled = (w * hud.progress.clamp(0.0, 1.0)).max(0.0);
        if filled >= 1.0 {
            let shader = LinearGradient::new(
                Point::from_xy(0.0, 0.0),
                Point::from_xy(w, 0.0),
                vec![
                    GradientStop::new(0.0, rgba([96, 170, 255], 1.0)),
                    GradientStop::new(1.0, rgba([196, 120, 255], 1.0)),
                ],
                SpreadMode::Pad,
                Transform::identity(),
            );
            if let Some(shader) = shader {
                let mut paint = Paint {
                    shader,
                    anti_alias: false,
                    ..Paint::default()
                };
                if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, by, filled, 2.0) {
                    pm.fill_rect(rect, &paint, Transform::identity(), None);
                }
                // Glow above the line (the atlas below stays clean).
                paint.blend_mode = BlendMode::Plus;
                paint.shader = LinearGradient::new(
                    Point::from_xy(0.0, 0.0),
                    Point::from_xy(w, 0.0),
                    vec![
                        GradientStop::new(0.0, rgba([96, 170, 255], 0.18)),
                        GradientStop::new(1.0, rgba([196, 120, 255], 0.18)),
                    ],
                    SpreadMode::Pad,
                    Transform::identity(),
                )
                .unwrap_or(paint.shader);
                if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, by - 4.0, filled, 4.0) {
                    pm.fill_rect(rect, &paint, Transform::identity(), None);
                }
                // Bright head at the current position.
                glow_dot(pm, filled, by + 1.0, 5.0, [196, 150, 255], 0.35);
            }
        }
    }
}
