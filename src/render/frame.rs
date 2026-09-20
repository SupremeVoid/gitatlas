//! The per-frame rasterizer. Renders one `FramePlan` into a reusable pixmap and
//! repacks it to an rgb24 buffer for ffmpeg.

use tiny_skia::{BlendMode, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::color::{GroupColor, mix};
use crate::config::Config;
use crate::easing;
use crate::fmt::{commafy, fmt_compact};
use crate::geom::Rect;
use crate::ingest::ChangeKind;
use crate::render::avatar::{AvatarSet, blit_sprite};
use crate::render::text::GlyphCache;
use crate::render::{FramePlan, Keyframe};

pub struct RenderCtx {
    pub cfg: Config,
    pub label_cache: GlyphCache,
    pub hud_cache: GlyphCache,
    pub avatars: AvatarSet,
    pub bg: [u8; 3],
    /// Height of the reserved top meta bar (0 when the HUD is disabled).
    pub hud_h: f32,
    /// Precomputed group colors keyed by hue bits (there are only a handful of
    /// distinct root-folder hues, so this avoids ~4 HSL conversions per tile per
    /// frame).
    pub group_colors: rustc_hash::FxHashMap<u32, GroupColor>,
    /// Which folder colors each path (see `groups.rs`).
    pub colors: crate::groups::ColorMap,
}

impl RenderCtx {
    #[inline]
    pub fn group_color(&self, hue: f32) -> GroupColor {
        self.group_colors
            .get(&hue.to_bits())
            .copied()
            .unwrap_or_else(|| GroupColor::from_hue(hue, self.cfg.saturation))
    }
}

pub struct Scratch {
    pub pm: Pixmap,
    tiles: Vec<RTile>,
}

impl Scratch {
    pub fn new(w: u32, h: u32) -> Self {
        Scratch {
            pm: Pixmap::new(w, h).expect("pixmap alloc"),
            tiles: Vec::with_capacity(8192),
        }
    }
}

#[derive(Clone, Copy)]
struct RTile {
    key: u64,
    rect: Rect,
    depth: u32,
    is_dir: bool,
    collapsed: bool,
    is_file: bool,
    group_hue: f32,
    raw_size: u32,
    alpha: f32,
    src_hi: bool,
    node: u32,
    child_count: u32,
}

const ADDED_GLOW: [u8; 3] = [120, 235, 150];
const MOD_GLOW: [u8; 3] = [245, 220, 120];
const DEL_GLOW: [u8; 3] = [240, 110, 90];

#[inline]
fn glow_color(kind: ChangeKind) -> [u8; 3] {
    match kind {
        ChangeKind::Added => ADDED_GLOW,
        ChangeKind::Modified => MOD_GLOW,
        ChangeKind::Deleted => DEL_GLOW,
    }
}

/// Render one frame, writing rgb24 into `out` (len = w*h*3).
pub fn render_frame(ctx: &RenderCtx, plan: &FramePlan, s: &mut Scratch, out: &mut [u8]) {
    let cfg = &ctx.cfg;
    let (w, h) = (s.pm.width(), s.pm.height());
    // Clear to opaque background.
    s.pm.fill(tiny_skia::Color::from_rgba8(
        ctx.bg[0], ctx.bg[1], ctx.bg[2], 255,
    ));

    build_tiles(plan, &mut s.tiles);

    // Draw tiles (already in depth/pre-order for the hi set; deleted appended).
    let pw = w as i32;
    let ph = h as i32;
    for t in &s.tiles {
        draw_tile(ctx, plan, t, &mut s.pm, pw, ph);
    }

    // Beams (additive) over the tiles.
    if cfg.show_beams {
        for b in &plan.beams {
            draw_beam(&mut s.pm, b, cfg.beam_intensity);
        }
    }

    // Avatars (+ optional flowing name) on top.
    if cfg.show_avatars {
        for a in &plan.avatars {
            if a.alpha <= 0.01 {
                continue;
            }
            if let Some(sprite) = ctx.avatars.sprites.get(a.author as usize) {
                blit_sprite(&mut s.pm, sprite, a.x, a.y, a.alpha);
            }
            if cfg.show_avatar_names
                && let Some(name) = ctx.avatars.names.get(a.author as usize)
            {
                let hue = ctx
                    .avatars
                    .hues
                    .get(a.author as usize)
                    .copied()
                    .unwrap_or(0.0);
                draw_avatar_name(ctx, &mut s.pm, name, a.x, a.y, hue, a.alpha);
            }
        }
    }

    // HUD overlay.
    if cfg.show_hud {
        draw_hud(ctx, plan, &mut s.pm);
    }

    // Repack premultiplied RGBA -> rgb24 (frames are opaque so premult == straight).
    repack_rgb24(s.pm.data(), out);
}

fn build_tiles(plan: &FramePlan, tiles: &mut Vec<RTile>) {
    tiles.clear();
    let frac = plan.frac;
    let tfrac = easing::ease_in_out_cubic(frac);
    let grow = easing::ease_out_cubic(frac);
    let pop = easing::ease_out_back(frac); // overshoot for a lively grow-in

    // Present (in hi): morph from lo if present, else grow-in.
    for ht in &plan.hi.layout.tiles {
        let (rect, alpha) = match plan.lo.layout.get(ht.key) {
            Some(lt) => (lt.rect.lerp(&ht.rect, tfrac), 1.0),
            None => {
                // Newly added: grow from center (with a slight overshoot) + fade in.
                let s = (0.35 + 0.65 * pop).clamp(0.0, 1.12);
                (ht.rect.scaled_about_center(s), grow)
            }
        };
        tiles.push(RTile {
            key: ht.key,
            rect,
            depth: ht.depth,
            is_dir: ht.is_dir,
            collapsed: ht.collapsed,
            is_file: !ht.is_dir,
            group_hue: ht.group_hue,
            raw_size: ht.raw_size,
            alpha,
            src_hi: true,
            node: ht.node,
            child_count: ht.child_count,
        });
    }

    // Deleted (in lo, not hi): shrink + fade out.
    if frac < 0.999 {
        for lt in &plan.lo.layout.tiles {
            if plan.hi.layout.index.contains_key(&lt.key) {
                continue;
            }
            let sfac = 1.0 - grow;
            if sfac <= 0.02 {
                continue;
            }
            tiles.push(RTile {
                key: lt.key,
                rect: lt.rect.scaled_about_center(0.35 + 0.65 * sfac),
                depth: lt.depth,
                is_dir: lt.is_dir,
                collapsed: lt.collapsed,
                is_file: !lt.is_dir,
                group_hue: lt.group_hue,
                raw_size: lt.raw_size,
                alpha: sfac,
                src_hi: false,
                node: lt.node,
                child_count: lt.child_count,
            });
        }
    }
}

fn draw_tile(ctx: &RenderCtx, plan: &FramePlan, t: &RTile, pm: &mut Pixmap, pw: i32, ph: i32) {
    let cfg = &ctx.cfg;
    let r = t.rect;
    if r.w < 1.0 || r.h < 1.0 || t.alpha <= 0.01 {
        return;
    }
    if !r.intersects_viewport(pw as f32, ph as f32) {
        return;
    }

    let gc = ctx.group_color(t.group_hue);
    let glow = plan.changed.get(&t.key);
    let gi = glow.map(|g| g.intensity).unwrap_or(0.0);

    // Base fill.
    let mut fillc = if t.collapsed { gc.fill_lit } else { gc.fill };
    if gi > 0.0 {
        let gcolor = glow.map(|g| glow_color(g.kind)).unwrap_or(MOD_GLOW);
        fillc = mix(fillc, gcolor, (gi * 0.38).min(0.6));
    }

    let data = pm.data_mut();

    // Tiny tiles: opaque fill only (LOD).
    if r.w < 3.0 || r.h < 3.0 {
        fill_rect(data, pw, ph, &r, fillc, t.alpha);
        return;
    }

    fill_rect(data, pw, ph, &r, fillc, t.alpha);

    // Minimap lines for files — and for collapsed folders, whose aggregated
    // line count stands in for the content that is too small to open.
    if cfg.show_minimap && (t.is_file || t.collapsed) && r.h >= 5.0 && r.w >= 6.0 {
        draw_minimap(data, pw, ph, &r, t, gc.minimap, cfg, t.alpha);
    }

    // Border.
    if cfg.border && r.w > 3.0 && r.h > 3.0 {
        let bcolor = if gi > 0.0 {
            mix(
                gc.border,
                glow.map(|g| glow_color(g.kind)).unwrap_or(MOD_GLOW),
                gi,
            )
        } else {
            gc.border
        };
        let balpha = (t.alpha * if gi > 0.0 { 1.0 } else { 0.9 }).min(1.0);
        border_rect(data, pw, ph, &r, bcolor, balpha, cfg.border_width);
    }

    // Labels.
    draw_tile_label(ctx, plan, t, pm);
}

fn draw_tile_label(ctx: &RenderCtx, plan: &FramePlan, t: &RTile, pm: &mut Pixmap) {
    let cfg = &ctx.cfg;
    let r = t.rect;
    let name = tile_name(plan, t);
    if name.is_empty() {
        return;
    }
    let cache = &ctx.label_cache;
    let px = cache.px();
    let pw = pm.width() as i32;
    let ph = pm.height() as i32;

    if t.is_dir {
        if !cfg.show_dir_names || t.depth > cfg.dir_label_depth() {
            return;
        }
        // An open folder is labeled exactly when layout reserved its header
        // strip; otherwise the name would be drawn over its children.
        if !t.collapsed && r.h < cfg.label_min_px() {
            return;
        }
        let maxw = r.w - cfg.pad * 2.0 - 2.0;
        if maxw < 12.0 || r.h < px + 4.0 {
            return;
        }
        let baseline = r.y + cfg.pad + px * 0.85;
        let tw = cache.measure(name).min(maxw);
        // Dark backing strip for contrast against busy tile fills.
        {
            let data = pm.data_mut();
            let by = baseline - px * 0.92;
            let bh = px + 4.0;
            let bw = (tw + cfg.pad).min(r.w - 2.0);
            fill_rect(
                data,
                pw,
                ph,
                &Rect::new(r.x + cfg.pad * 0.5, by, bw, bh),
                [6, 7, 10],
                0.52 * t.alpha.min(1.0),
            );
        }
        let color = mix(ctx.group_color(t.group_hue).border, [242, 245, 250], 0.62);
        cache.draw(
            pm,
            name,
            r.x + cfg.pad + 1.0,
            baseline,
            color,
            t.alpha.min(1.0),
            maxw,
        );
    } else {
        if !cfg.show_file_names || r.w < cfg.file_name_min_px || r.h < px + 2.0 {
            return;
        }
        let maxw = r.w - 6.0;
        let baseline = r.y + r.h * 0.5 + px * 0.35;
        let tw = cache.measure(name).min(maxw);
        {
            let data = pm.data_mut();
            fill_rect(
                data,
                pw,
                ph,
                &Rect::new(
                    r.x + 2.0,
                    baseline - px * 0.85,
                    (tw + 4.0).min(maxw),
                    px + 3.0,
                ),
                [6, 7, 10],
                0.5 * t.alpha.min(1.0),
            );
        }
        cache.draw(
            pm,
            name,
            r.x + 3.0,
            baseline,
            [226, 230, 238],
            t.alpha.min(1.0),
            maxw,
        );
    }
}

fn tile_name<'a>(plan: &'a FramePlan, t: &RTile) -> &'a str {
    let kf: &Keyframe = if t.src_hi { &plan.hi } else { &plan.lo };
    kf.tree
        .nodes
        .get(t.node as usize)
        .map(|n| n.name.as_str())
        .unwrap_or("")
}

fn draw_minimap(
    data: &mut [u8],
    pw: i32,
    ph: i32,
    r: &Rect,
    t: &RTile,
    color: [u8; 3],
    cfg: &Config,
    alpha: f32,
) {
    let gap = cfg.minimap_line_gap.max(1.5);
    // Small tiles get a tighter inset so they still fit a line or two.
    let inset = if r.h < 12.0 || r.w < 12.0 { 2.0 } else { 3.0 };
    let top = r.y + inset;
    let rows = (((r.h - inset * 2.0) / gap).floor() as u32 + 1).min(cfg.minimap_max_lines);
    if t.raw_size == 0 || r.h < inset * 2.0 + 1.0 {
        return;
    }
    // Line count scales with size (lines of code), but never leaves more than
    // ~40% of the tile blank: tile area is gamma-compressed, so a short file
    // would otherwise sit as a few lines atop an empty box.
    let floor = (rows as f32 * 0.6).ceil() as u32;
    let n = t.raw_size.max(floor).min(rows);
    let usable_w = r.w - inset * 2.0;
    if usable_w < 2.0 {
        return;
    }
    for i in 0..n {
        let y = (top + i as f32 * gap) as i32;
        if y < 0 || y >= ph {
            continue;
        }
        // Pseudo-random-but-stable length + indent per line.
        let seed = t.key ^ (i as u64).wrapping_mul(0x9e3779b97f4a7c15);
        let lf = 0.25 + 0.7 * frac01(seed);
        let indent = 0.06 * frac01(seed >> 17);
        let x0 = (r.x + inset + usable_w * indent) as i32;
        let x1 = (r.x + inset + usable_w * (indent + lf).min(1.0)) as i32;
        hline(data, pw, ph, y, x0, x1, color, alpha);
    }
}

#[inline]
fn frac01(seed: u64) -> f32 {
    // xorshift-ish scramble to [0,1)
    let mut x = seed.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

// ---- direct pixel helpers (premultiplied RGBA, opaque background) ----

#[inline]
fn blend_px(data: &mut [u8], di: usize, c: [u8; 3], a: u32) {
    // a in 0..=255, dst opaque -> straight over.
    let inv = 255 - a;
    data[di] = ((c[0] as u32 * a + data[di] as u32 * inv + 127) / 255) as u8;
    data[di + 1] = ((c[1] as u32 * a + data[di + 1] as u32 * inv + 127) / 255) as u8;
    data[di + 2] = ((c[2] as u32 * a + data[di + 2] as u32 * inv + 127) / 255) as u8;
    // alpha stays (opaque background keeps 255).
}

fn fill_rect(data: &mut [u8], pw: i32, ph: i32, r: &Rect, c: [u8; 3], alpha: f32) {
    let x0 = (r.x.floor() as i32).max(0);
    let y0 = (r.y.floor() as i32).max(0);
    let x1 = ((r.x + r.w).ceil() as i32).min(pw);
    let y1 = ((r.y + r.h).ceil() as i32).min(ph);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    if alpha >= 0.999 {
        let px = [c[0], c[1], c[2], 255];
        for y in y0..y1 {
            let mut di = ((y * pw + x0) * 4) as usize;
            for _ in x0..x1 {
                data[di..di + 4].copy_from_slice(&px);
                di += 4;
            }
        }
    } else {
        let a = (alpha * 255.0) as u32;
        for y in y0..y1 {
            let mut di = ((y * pw + x0) * 4) as usize;
            for _ in x0..x1 {
                blend_px(data, di, c, a);
                di += 4;
            }
        }
    }
}

fn hline(data: &mut [u8], pw: i32, ph: i32, y: i32, x0: i32, x1: i32, c: [u8; 3], alpha: f32) {
    if y < 0 || y >= ph {
        return;
    }
    let x0 = x0.max(0);
    let x1 = x1.min(pw);
    if x0 >= x1 {
        return;
    }
    if alpha >= 0.999 {
        let px = [c[0], c[1], c[2], 255];
        let mut di = ((y * pw + x0) * 4) as usize;
        for _ in x0..x1 {
            data[di..di + 4].copy_from_slice(&px);
            di += 4;
        }
    } else {
        let a = (alpha * 255.0) as u32;
        let mut di = ((y * pw + x0) * 4) as usize;
        for _ in x0..x1 {
            blend_px(data, di, c, a);
            di += 4;
        }
    }
}

fn vline(data: &mut [u8], pw: i32, ph: i32, x: i32, y0: i32, y1: i32, c: [u8; 3], alpha: f32) {
    if x < 0 || x >= pw {
        return;
    }
    let y0 = y0.max(0);
    let y1 = y1.min(ph);
    if y0 >= y1 {
        return;
    }
    let a = (alpha.clamp(0.0, 1.0) * 255.0) as u32;
    for y in y0..y1 {
        let di = ((y * pw + x) * 4) as usize;
        if a >= 255 {
            data[di] = c[0];
            data[di + 1] = c[1];
            data[di + 2] = c[2];
        } else {
            blend_px(data, di, c, a);
        }
    }
}

fn border_rect(data: &mut [u8], pw: i32, ph: i32, r: &Rect, c: [u8; 3], alpha: f32, width: f32) {
    let bw = width.max(1.0).round() as i32;
    let x0 = r.x.round() as i32;
    let y0 = r.y.round() as i32;
    let x1 = (r.x + r.w).round() as i32;
    let y1 = (r.y + r.h).round() as i32;
    for k in 0..bw {
        hline(data, pw, ph, y0 + k, x0, x1, c, alpha);
        hline(data, pw, ph, y1 - 1 - k, x0, x1, c, alpha);
        vline(data, pw, ph, x0 + k, y0, y1, c, alpha);
        vline(data, pw, ph, x1 - 1 - k, y0, y1, c, alpha);
    }
}

fn draw_beam(pm: &mut Pixmap, b: &crate::render::Beam, global: f32) {
    let intensity = (b.intensity * global).clamp(0.0, 1.5);
    if intensity <= 0.01 {
        return;
    }
    let rgb = crate::color::hsl_to_rgb(b.hue, 0.72, 0.62);
    // 3 additive passes: wide+faint -> thin+bright.
    let passes = [(6.5f32, 0.045f32), (3.2, 0.09), (1.5, 0.34)];
    for (width, a) in passes {
        let mut pb = PathBuilder::new();
        pb.move_to(b.x0, b.y0);
        pb.line_to(b.x1, b.y1);
        let path = match pb.finish() {
            Some(p) => p,
            None => continue,
        };
        let mut paint = Paint::default();
        paint.anti_alias = true;
        paint.blend_mode = BlendMode::Plus;
        let av = (a * intensity * 255.0).min(255.0) as u8;
        paint.set_color_rgba8(rgb[0], rgb[1], rgb[2], av);
        let mut stroke = Stroke::default();
        stroke.width = width;
        stroke.line_cap = LineCap::Round;
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

/// Draw a committer name centered beneath their avatar, tinted with the avatar's
/// unique color and backed by a dark strip for readability.
fn draw_avatar_name(
    ctx: &RenderCtx,
    pm: &mut Pixmap,
    name: &str,
    cx: f32,
    cy: f32,
    hue: f32,
    alpha: f32,
) {
    let cache = &ctx.label_cache;
    let px = cache.px();
    let asize = ctx.cfg.avatar_size as f32;
    let tw = cache.measure(name).min(360.0);
    if tw < 4.0 {
        return;
    }
    let baseline = cy + asize * 0.5 + px + 2.0;
    let x0 = cx - tw * 0.5;
    let pw = pm.width() as i32;
    let ph = pm.height() as i32;
    {
        let data = pm.data_mut();
        fill_rect(
            data,
            pw,
            ph,
            &Rect::new(x0 - 3.0, baseline - px * 0.9, tw + 6.0, px + 4.0),
            [6, 7, 10],
            0.5 * alpha.clamp(0.0, 1.0),
        );
    }
    let color = crate::color::hsl_to_rgb(hue, 0.55, 0.78);
    cache.draw(
        pm,
        name,
        x0,
        baseline,
        color,
        alpha.clamp(0.0, 1.0),
        tw + 2.0,
    );
}

/// Draw the meta information in a reserved top bar, so it never overlaps (and
/// obscures) the atlas. The atlas is laid out below this band; when the HUD is
/// disabled the band has zero height and this is skipped.
fn draw_hud(ctx: &RenderCtx, plan: &FramePlan, pm: &mut Pixmap) {
    let cfg = &ctx.cfg;
    let hud = &plan.hud;
    let w = pm.width() as f32;
    let hb = ctx.hud_h;
    if hb <= 1.0 {
        return;
    }
    let iw = pm.width() as i32;
    let ih = pm.height() as i32;
    let big = &ctx.hud_cache;
    let small = &ctx.label_cache;
    let fg = [232, 235, 242];
    let dim = [156, 162, 174];
    let accent = [120, 170, 250];
    let pad = 18.0;

    // Band background + a divider line separating it from the atlas.
    let band_bg = mix(ctx.bg, [255, 255, 255], 0.07);
    {
        let data = pm.data_mut();
        fill_rect(data, iw, ih, &Rect::new(0.0, 0.0, w, hb), band_bg, 1.0);
        hline(data, iw, ih, (hb as i32) - 2, 0, iw, [64, 70, 82], 1.0);
    }

    let y1 = 6.0 + big.px() * 0.95; // row 1 baseline
    let y2 = y1 + small.line_height + 2.0; // row 2 baseline

    // Row 1 left: title, then date in an accent color.
    let title = cfg.title.clone().unwrap_or_else(|| "gitatlas".to_string());
    let mut x = big.draw(pm, &title, pad, y1, fg, 1.0, w * 0.55);
    if cfg.show_date && !hud.date.is_empty() {
        x += 18.0;
        big.draw(pm, &hud.date, x, y1, accent, 1.0, 240.0);
    }

    // Row 1 right: commit counter.
    let counter = format!("commit {}/{}", hud.commit_idx + 1, hud.total_commits);
    let cw = big.measure(&counter);
    big.draw(pm, &counter, w - cw - pad, y1, fg, 1.0, cw + 4.0);

    // Row 2 left: author — subject.
    if cfg.show_author {
        let line = if hud.subject.is_empty() {
            hud.author.clone()
        } else {
            format!("{}  —  {}", hud.author, hud.subject)
        };
        small.draw(pm, &line, pad, y2, dim, 1.0, w * 0.66);
    }

    // Row 2 right: total file / LoC stats (with thousands separators).
    let stats = format!(
        "{} files · {} LoC",
        commafy(hud.files as u64),
        commafy(hud.lines)
    );
    let sw = small.measure(&stats);
    small.draw(pm, &stats, w - sw - pad, y2, dim, 1.0, sw + 4.0);

    // Row 3 right (beneath the total): per-language split — percent, files, LoC.
    if cfg.show_langs && !hud.langs.is_empty() {
        let total_loc = hud.lines.max(1);
        let y3 = y2 + small.line_height + 2.0;
        let sw_box = small.px() * 0.72;
        let gap = 16.0;

        let mut toks: Vec<([u8; 3], String)> = hud
            .langs
            .iter()
            .take(6)
            .map(|(lid, lines, files)| {
                let l = &crate::lang::LANGS[*lid as usize];
                let pct = *lines as f64 / total_loc as f64 * 100.0;
                (
                    l.color,
                    format!(
                        "{} {:.1}% {}f·{}",
                        l.name,
                        pct,
                        commafy(*files as u64),
                        fmt_compact(*lines)
                    ),
                )
            })
            .collect();

        let tok_w = |t: &str| sw_box + 5.0 + small.measure(t);
        let total_w = |ts: &[([u8; 3], String)]| -> f32 {
            if ts.is_empty() {
                0.0
            } else {
                ts.iter().map(|(_, t)| tok_w(t) + gap).sum::<f32>() - gap
            }
        };
        let avail = w - 2.0 * pad;
        while total_w(&toks) > avail && toks.len() > 1 {
            toks.pop();
        }

        let mut x = (w - pad - total_w(&toks)).max(pad);
        for (color, text) in &toks {
            {
                let data = pm.data_mut();
                fill_rect(
                    data,
                    iw,
                    ih,
                    &Rect::new(x, y3 - sw_box, sw_box, sw_box),
                    *color,
                    1.0,
                );
            }
            x += sw_box + 5.0;
            x = small.draw(
                pm,
                text,
                x,
                y3,
                [206, 210, 220],
                1.0,
                small.measure(text) + 4.0,
            );
            x += gap;
        }
    }

    // Progress bar along the divider.
    if cfg.show_progress {
        let data = pm.data_mut();
        let by = (hb as i32) - 2;
        let filled = (w * hud.progress) as i32;
        hline(data, iw, ih, by, 0, filled, accent, 1.0);
        hline(data, iw, ih, by + 1, 0, filled, accent, 1.0);
    }
}

fn repack_rgb24(rgba: &[u8], out: &mut [u8]) {
    // out len = (rgba.len()/4)*3
    let px = rgba.len() / 4;
    let mut si = 0;
    let mut di = 0;
    for _ in 0..px {
        out[di] = rgba[si];
        out[di + 1] = rgba[si + 1];
        out[di + 2] = rgba[si + 2];
        si += 4;
        di += 3;
    }
}
