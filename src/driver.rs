//! The rendering driver.
//!
//! Frames are processed in chunks. For each chunk we (1) advance the evolving
//! model sequentially and snapshot the handful of commit states the chunk needs
//! (cheap), then (2) build those keyframes (tree + treemap layout), (3) assemble
//! each frame's self-contained plan (interpolation + change highlights + avatars
//! + beams + HUD) and (4) rasterize it — steps 2–4 all run in parallel across
//! every core. Finished frames are written to ffmpeg in order.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;

use rustc_hash::FxHashMap;

use crate::color::hash_str;
use crate::config::Config;
use crate::encode::Encoder;
use crate::fmt::fmt_date;
use crate::geom::Rect;
use crate::ingest::History;
use crate::layout::{Layout, LayoutParams, layout};
use crate::model::{WorldState, build_tree};
use crate::render::frame::{RenderCtx, Scratch, render_frame};
use crate::render::{AvatarDraw, Beam, ChangeGlow, FramePlan, Hud, Keyframe};

const MAX_AVATARS: usize = 16;
const MAX_BEAMS: usize = 120;
const BEAMS_PER_AUTHOR: usize = 14;
const ACTIVE_COMMIT_CAP: isize = 600;

/// Immutable per-run timing parameters shared with the (parallel) plan builder.
#[derive(Clone, Copy)]
struct TimeMap {
    n: usize,
    denom: f64,
    n_f: f64,
    max_hold: f64,
    hold_frames: f64,
    avatar_life: f64,
    beam_life: f64,
    fps: f64,
    frame_x: f32,
    frame_y: f32,
    frame_w: f32,
    frame_h: f32,
    avatar_size: f32,
    show_beams: bool,
}

impl TimeMap {
    #[inline]
    fn commit_frame(&self, c: usize) -> f64 {
        c as f64 * self.denom / self.n_f.max(1.0)
    }
}

pub fn run(
    history: &History,
    ctx: &RenderCtx,
    params: &LayoutParams,
    frame_rect: Rect,
    path_lang: &[u16],
) -> Result<()> {
    let cfg = &ctx.cfg;
    let n = history.commits.len();
    let total_frames = cfg.total_frames(n);
    let denom = (total_frames.max(2) - 1) as f64;
    let n_f = n as f64;

    let frames_per_commit = denom / n_f.max(1.0);
    let smooth = frames_per_commit >= 1.0;
    // In smooth mode the geometric morph completes within the first `transition`
    // fraction of each commit's on-screen interval, then holds.
    let transition = cfg.transition.clamp(0.05, 1.0) as f64;
    let hold_frames = (cfg.highlight_seconds * cfg.fps as f32) as f64;
    let avatar_life = frames_per_commit.max((cfg.avatar_idle_seconds * cfg.fps as f32) as f64);
    // A beam lives exactly as long as its commit is on screen (with a small
    // floor so it still registers when fast-forwarding), unlike the avatar,
    // which lingers.
    let beam_life = frames_per_commit.max((cfg.beam_seconds.max(0.0) * cfg.fps as f32) as f64);
    let max_hold = hold_frames.max(avatar_life);

    let tm = TimeMap {
        n,
        denom,
        n_f,
        max_hold,
        hold_frames,
        avatar_life,
        beam_life,
        fps: cfg.fps as f64,
        frame_x: frame_rect.x,
        frame_y: frame_rect.y,
        frame_w: frame_rect.w,
        frame_h: frame_rect.h,
        avatar_size: cfg.avatar_size as f32,
        show_beams: cfg.show_beams,
    };

    let mut enc = Encoder::new(cfg)?;
    let frame_bytes = enc.frame_bytes;
    let (w, h) = (cfg.width, cfg.height);

    let pb = ProgressBar::new(total_frames);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner} {bar:40.cyan/blue} {pos}/{len} frames  {percent}%  {per_sec}  eta {eta}",
        )
        .unwrap(),
    );

    let mut state = WorldState::new(history);
    let mut applied = 0usize;
    let chunk = (num_threads(cfg) * 2).max(16) as u64;

    let mut t_snap = Duration::ZERO;
    let mut t_kf = Duration::ZERO;
    let mut t_render = Duration::ZERO;
    let mut kf_built = 0usize;

    let mut f0 = 0u64;
    while f0 < total_frames {
        let f1 = (f0 + chunk).min(total_frames);

        // Frame specs + the set of commit states this chunk needs.
        let mut needed: BTreeSet<usize> = BTreeSet::new();
        let mut specs: Vec<(u64, usize, usize, f32)> = Vec::with_capacity((f1 - f0) as usize);
        for f in f0..f1 {
            let p = f as f64 * n_f / denom;
            let lo = (p.floor() as usize).min(n);
            let hi = if smooth { (lo + 1).min(n) } else { lo };
            let frac = if hi > lo {
                ((p - lo as f64) / transition).min(1.0) as f32
            } else {
                0.0
            };
            needed.insert(lo);
            needed.insert(hi);
            specs.push((f, lo, hi, frac));
        }

        // (1) sequential snapshot of each needed state.
        let ts = Instant::now();
        let mut snaps: Vec<(usize, crate::model::StateSnapshot)> = Vec::with_capacity(needed.len());
        for &k in &needed {
            while applied < k {
                state.apply(&history.commits[applied]);
                applied += 1;
            }
            snaps.push((k, state.snapshot()));
        }
        t_snap += ts.elapsed();

        // (2) parallel keyframe (tree + layout) build.
        let tk = Instant::now();
        let kfs: HashMap<usize, Arc<Keyframe>> = snaps
            .par_iter()
            .map(|(k, snap)| {
                let collapse = cfg.depth_mode == crate::config::DepthMode::Collapse;
                let tree = build_tree(&snap.files, history, &ctx.colors, cfg.max_depth, collapse);
                let lay: Layout = layout(&tree, frame_rect, params);
                // Language split for the HUD (cheap array sum over present files).
                let mut lang_lines = vec![0u64; crate::lang::LANGS.len()];
                let mut lang_files = vec![0u32; crate::lang::LANGS.len()];
                for &(pid, sz) in &snap.files {
                    let li = path_lang[pid as usize] as usize;
                    lang_lines[li] += sz as u64;
                    lang_files[li] += 1;
                }
                let langs = crate::lang::top_langs_fl(&lang_lines, &lang_files, 6);
                (
                    *k,
                    Arc::new(Keyframe {
                        k: *k,
                        tree,
                        layout: lay,
                        files: snap.files_count,
                        lines: snap.lines,
                        langs,
                    }),
                )
            })
            .collect();
        kf_built += kfs.len();
        t_kf += tk.elapsed();

        // (3+4) parallel plan build + rasterize; rayon preserves input order.
        let tr = Instant::now();
        let frames: Vec<Vec<u8>> = specs
            .par_iter()
            .map_init(
                || Scratch::new(w, h),
                |scratch, &(f, lo, hi, frac)| {
                    let plan = build_plan(history, ctx, &tm, &kfs, f, lo, hi, frac);
                    let mut buf = vec![0u8; frame_bytes];
                    render_frame(ctx, &plan, scratch, &mut buf);
                    buf
                },
            )
            .collect();
        t_render += tr.elapsed();

        for buf in &frames {
            enc.write_frame(buf)?;
            pb.inc(1);
        }
        f0 = f1;
    }

    pb.finish_and_clear();
    enc.finish()?;
    eprintln!(
        "  [timing] snapshot {:.2}s, keyframes {:.2}s ({} built), plan+render+encode {:.2}s",
        t_snap.as_secs_f64(),
        t_kf.as_secs_f64(),
        kf_built,
        t_render.as_secs_f64()
    );
    Ok(())
}

#[derive(Default)]
struct AuthorAgg {
    sx: f32,
    sy: f32,
    sw: f32,
    alpha: f32,
    hue: f32,
    targets: Vec<(f32, f32, f32)>,
}

#[allow(clippy::too_many_arguments)]
fn build_plan(
    history: &History,
    ctx: &RenderCtx,
    tm: &TimeMap,
    kfs: &HashMap<usize, Arc<Keyframe>>,
    f: u64,
    lo: usize,
    hi: usize,
    frac: f32,
) -> FramePlan {
    let n = tm.n;
    let kf_lo = kfs[&lo].clone();
    let kf_hi = kfs[&hi].clone();

    let p = f as f64 * tm.n_f / tm.denom;
    let c_hi = (p.floor() as isize).min(n as isize - 1);
    let mut c_lo = (((f as f64 - tm.max_hold) * tm.n_f / tm.denom)
        .ceil()
        .max(0.0) as isize)
        .min(n as isize);
    if c_hi - c_lo > ACTIVE_COMMIT_CAP {
        c_lo = c_hi - ACTIVE_COMMIT_CAP;
    }

    let mut changed: FxHashMap<u64, ChangeGlow> = FxHashMap::default();
    let mut avatars: Vec<AvatarDraw> = Vec::new();
    let mut beams: Vec<Beam> = Vec::new();
    let mut agg: HashMap<u32, AuthorAgg> = HashMap::new();

    if c_hi >= 0 {
        for c in (c_lo.max(0)..=c_hi).rev() {
            let c = c as usize;
            let age = f as f64 - tm.commit_frame(c);
            if age < 0.0 {
                continue;
            }
            // Fade in quickly (~0.35s), hold, fade out over the idle tail (~1.6s).
            let alpha = life_alpha(age, tm.avatar_life, tm.fps * 0.35, tm.fps * 1.6) as f32;
            let hl = life_alpha(age, tm.hold_frames, tm.fps * 0.08, tm.hold_frames * 0.55) as f32;
            let beam = life_alpha(age, tm.beam_life, tm.fps * 0.06, tm.fps * 0.2) as f32;
            let recency = 1.0 - (age / tm.max_hold).clamp(0.0, 1.0) as f32;
            let commit = &history.commits[c];
            let hue = ctx
                .avatars
                .hues
                .get(commit.author as usize)
                .copied()
                .unwrap_or(0.0);

            let a = agg.entry(commit.author).or_insert_with(|| AuthorAgg {
                hue,
                ..Default::default()
            });
            if alpha > a.alpha {
                a.alpha = alpha;
            }
            for ch in &commit.changes {
                let path = &history.paths[ch.path as usize];
                if let Some(key) = resolve_tile(&kf_hi.layout, path)
                    && let Some(tile) = kf_hi.layout.get(key)
                {
                    let cx = tile.rect.cx();
                    let cy = tile.rect.cy();
                    let wgt = (tile.raw_size.max(1) as f32).sqrt() * (0.15 + recency);
                    a.sx += cx * wgt;
                    a.sy += cy * wgt;
                    a.sw += wgt;
                    // One beam per drawn tile: files under the same collapsed
                    // folder would otherwise stack additively into a white bar.
                    if beam > 0.02
                        && a.targets.len() < BEAMS_PER_AUTHOR
                        && !a.targets.iter().any(|t| t.0 == cx && t.1 == cy)
                    {
                        a.targets.push((cx, cy, beam));
                    }
                    let e = changed.entry(key).or_insert(ChangeGlow {
                        intensity: 0.0,
                        kind: ch.kind,
                    });
                    if hl > e.intensity {
                        e.intensity = hl;
                        e.kind = ch.kind;
                    }
                }
            }
        }
    }

    let mut authors_sorted: Vec<(u32, AuthorAgg)> = agg.into_iter().collect();
    // Sort by alpha desc, with author id as a deterministic tiebreaker so the
    // selected/drawn avatars are reproducible across runs (HashMap iteration
    // order is otherwise randomized).
    authors_sorted.sort_by(|x, y| {
        y.1.alpha
            .partial_cmp(&x.1.alpha)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| x.0.cmp(&y.0))
    });
    for (author, a) in authors_sorted.into_iter().take(MAX_AVATARS) {
        if a.sw <= 0.0 || a.alpha <= 0.02 {
            continue;
        }
        let anchor_x = a.sx / a.sw;
        let anchor_y = a.sy / a.sw;
        // Gentle 2D wander so avatars drift through the map instead of sitting still.
        let secs = f as f32 / tm.fps as f32;
        let (p0, p1, p2, p3) = author_phases(author);
        let amp = (tm.frame_w.min(tm.frame_h)) * 0.022;
        let wx = ((secs * 0.5 + p0).sin() + 0.6 * (secs * 0.23 + p1).sin()) * amp;
        let wy = ((secs * 0.42 + p2).sin() + 0.6 * (secs * 0.19 + p3).sin()) * amp;
        let float = tm.avatar_size * 0.7;
        let ax = (anchor_x + wx).clamp(
            tm.frame_x + tm.avatar_size * 0.5,
            tm.frame_x + tm.frame_w - tm.avatar_size * 0.5,
        );
        let ay = (anchor_y - float + wy).clamp(
            tm.frame_y + tm.avatar_size * 0.5,
            tm.frame_y + tm.frame_h - tm.avatar_size * 0.5,
        );
        avatars.push(AvatarDraw {
            author,
            x: ax,
            y: ay,
            alpha: a.alpha,
        });
        if tm.show_beams && beams.len() < MAX_BEAMS {
            for (tx, ty, beam) in a.targets.iter() {
                if beams.len() >= MAX_BEAMS {
                    break;
                }
                beams.push(Beam {
                    x0: ax,
                    y0: ay,
                    x1: *tx,
                    y1: *ty,
                    hue: a.hue,
                    intensity: beam * (0.4 + 0.6 * a.alpha),
                });
            }
        }
    }

    let cur = lo.min(n.saturating_sub(1));
    let commit = &history.commits[cur];
    // Use the avatar set's display name (which honors config-file label overrides).
    let author = ctx
        .avatars
        .names
        .get(commit.author as usize)
        .cloned()
        .unwrap_or_default();
    let hud = Hud {
        date: fmt_date(commit.time),
        author,
        subject: truncate(&commit.subject, 90),
        commit_hash: commit.hash.chars().take(9).collect(),
        commit_idx: cur,
        total_commits: n,
        files: kf_hi.files,
        lines: kf_hi.lines,
        progress: (f as f32 / tm.denom as f32).clamp(0.0, 1.0),
        langs: kf_hi.langs.clone(),
    };

    FramePlan {
        frame_index: f,
        lo: kf_lo,
        hi: kf_hi,
        frac,
        changed,
        beams,
        avatars,
        hud,
    }
}

fn num_threads(cfg: &Config) -> usize {
    if cfg.threads > 0 {
        cfg.threads
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
    }
}

/// Envelope over a life of `life` frames: ramp up over `fade_in` frames, hold at
/// 1, ramp down over the last `fade_out` frames. Returns 0 outside [0, life].
fn life_alpha(age: f64, life: f64, fade_in: f64, fade_out: f64) -> f64 {
    if life <= 0.0 {
        return if age <= 0.0 { 1.0 } else { 0.0 };
    }
    if age < 0.0 || age > life {
        return 0.0;
    }
    let fade_in = fade_in.max(1.0).min(life * 0.5);
    let fade_out = fade_out.max(1.0).min(life * 0.9);
    let a_in = (age / fade_in).min(1.0);
    let a_out = ((life - age) / fade_out).min(1.0);
    a_in.min(a_out).clamp(0.0, 1.0)
}

/// Four stable wander phases in [0, 2π) derived from an author id.
fn author_phases(author: u32) -> (f32, f32, f32, f32) {
    let h = (author as u64).wrapping_mul(0x9e3779b97f4a7c15) ^ 0xdead_beef_cafe_f00d;
    let frac =
        |shift: u32| -> f32 { (((h >> shift) & 0xffff) as f32 / 65535.0) * std::f32::consts::TAU };
    (frac(3), frac(19), frac(33), frac(47))
}

/// Find the drawn tile for a path, walking up to the deepest drawn ancestor.
pub fn resolve_tile(layout: &Layout, path: &str) -> Option<u64> {
    let mut p = path;
    loop {
        let key = hash_str(p);
        if layout.index.contains_key(&key) {
            return Some(key);
        }
        {
            let i = p.rfind('/')?;
            p = &p[..i]
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}
