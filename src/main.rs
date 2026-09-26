// Several helpers and config/API fields are kept for completeness and future
// options even when not currently referenced.
// A handful of struct fields (e.g. Keyframe.k, FramePlan.frame_index,
// Hud.commit_hash, LaidTile.path_id/child_count) are populated for
// completeness/debuggability but not currently read; keep the compiler quiet
// about them without hiding whole unused functions (those have been removed).
#![allow(dead_code)]
// Rendering/rasterization helpers naturally take many positional coordinates,
// and tiny-skia's Paint/Stroke have no builder, so these lints add noise here.
#![allow(clippy::too_many_arguments)]
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::explicit_counter_loop)]

mod cache;
mod cli;
mod color;
mod config;
mod configfile;
mod driver;
mod easing;
mod encode;
mod fmt;
mod geom;
mod groups;
mod ingest;
mod intern;
mod lang;
mod layout;
mod model;
mod render;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::Instant;

use crate::cli::{Cli, Command, GenConfigArgs, ScanArgs};
use crate::config::Config;
use crate::fmt::{commafy, fmt_date, parse_date_to_epoch};
use crate::geom::Rect;
use crate::ingest::{History, IngestOptions};
use crate::layout::LayoutParams;
use crate::render::avatar::{AvatarOptions, AvatarSet, parse_avatar_config};
use crate::render::frame::RenderCtx;
use crate::render::text::{GlyphCache, load_font};

fn main() {
    if let Err(e) = real_main() {
        eprintln!("\x1b[31merror:\x1b[0m {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    // Bare `gitatlas` shows the help instead of rendering the current directory
    // (`gitatlas .` does that).
    if std::env::args_os().len() <= 1 {
        Cli::command().print_long_help()?;
        return Ok(());
    }
    let cli = Cli::parse();
    match cli.command {
        None => render(cli.render),
        Some(Command::Render(a)) => render(a),
        Some(Command::Info(s)) => info(&s),
        Some(Command::Languages(s)) => languages(&s),
        Some(Command::GenConfig(a)) => gen_config(&a),
        Some(Command::Snapshot(a)) => snapshot(a),
    }
}

/// Load history, using the on-disk cache when enabled.
fn load_history(repo: &std::path::Path, opts: &IngestOptions, use_cache: bool) -> Result<History> {
    let t0 = Instant::now();
    let specs = ingest::discover(repo, opts, false)?;
    if use_cache && let Some(h) = cache::load(&specs, opts) {
        eprintln!(
            "• loaded cached history ({} commits, {:.2}s)",
            h.commits.len(),
            t0.elapsed().as_secs_f64()
        );
        return Ok(h);
    }
    eprintln!("• reading git history from {} ...", repo.display());
    let h = ingest::ingest(&specs, opts)?;
    if h.commits.is_empty() {
        anyhow::bail!("no commits found for the selected range");
    }
    eprintln!(
        "  {} commits, {} unique paths, {} authors  ({:.1}s)",
        h.commits.len(),
        h.paths.len(),
        h.authors.len(),
        t0.elapsed().as_secs_f64()
    );
    if use_cache && let Err(e) = cache::save(&specs, opts, &h) {
        eprintln!("  (cache write skipped: {e})");
    }
    Ok(h)
}

/// Resolve a `RenderArgs` into a final `Config` plus committer label/image maps,
/// merging a `--config` file (if given) with CLI flags taking precedence.
type ResolvedConfig = (Config, HashMap<String, String>, HashMap<String, PathBuf>);
fn resolve_config(args: cli::RenderArgs) -> Result<ResolvedConfig> {
    let config_path = args.config.clone();
    let cli_cfg = args.into_config()?;
    if let Some(cp) = config_path {
        let loaded = configfile::load(&cp)?;
        let merged = configfile::merge_cli_over(&loaded.options, &cli_cfg)?;
        eprintln!(
            "• loaded config {} ({} committer entries)",
            cp.display(),
            loaded.labels.len()
        );
        Ok((merged, loaded.labels, loaded.images))
    } else {
        Ok((cli_cfg, HashMap::new(), HashMap::new()))
    }
}

/// Build the shared render context (fonts, avatars, HUD bar height), layout
/// params, and the atlas frame rectangle.
fn setup_render(
    cfg: &Config,
    history: &History,
    labels: &HashMap<String, String>,
    images_extra: &HashMap<String, PathBuf>,
    size_cap: f32,
    colors: groups::ColorMap,
    dir_balance: rustc_hash::FxHashMap<u64, f32>,
) -> (RenderCtx, LayoutParams, Rect) {
    // Merge avatar image sources: config-file committer images, then --avatar-config.
    let mut images = images_extra.clone();
    if let Some(acfg) = &cfg.avatar_config {
        for (k, v) in parse_avatar_config(acfg) {
            images.insert(k, v);
        }
    }

    let font = load_font();
    let name_chars: BTreeSet<char> = history
        .authors
        .iter()
        .flat_map(|a| a.name.chars())
        .chain(labels.values().flat_map(|l| l.chars()))
        .collect();
    let label_cache = GlyphCache::warm(&font, cfg.label_size, name_chars.iter().copied());
    let hud_cache = GlyphCache::warm(
        &font,
        (cfg.label_size * 1.7).round(),
        name_chars.iter().copied(),
    );

    let avatars = AvatarSet::build(
        history.authors.as_slice(),
        &AvatarOptions {
            size: cfg.avatar_size,
            images: &images,
            labels,
            dir: cfg.avatar_dir.as_deref(),
            gravatar: cfg.gravatar,
            gravatar_timeout_ms: cfg.gravatar_timeout_ms,
        },
    );

    let row1 = 6.0 + hud_cache.px() * 0.95;
    let row2 = row1 + label_cache.line_height + 2.0;
    let hud_h = if cfg.show_hud {
        if cfg.show_langs {
            (row2 + label_cache.line_height + 2.0 + label_cache.px() * 0.4 + 4.0).round()
        } else {
            (row2 + label_cache.px() * 0.4 + 6.0).round()
        }
    } else {
        0.0
    };

    // Precompute the handful of distinct group colors once; folders that span
    // several groups are neutral gray.
    let mut group_colors = rustc_hash::FxHashMap::default();
    for &hue in &colors.path_hue {
        group_colors
            .entry(hue.to_bits())
            .or_insert_with(|| crate::color::GroupColor::from_hue(hue, cfg.saturation));
    }
    group_colors.insert(
        groups::NEUTRAL_HUE.to_bits(),
        crate::color::GroupColor::from_hue(0.0, 0.0),
    );

    let ctx = RenderCtx {
        cfg: cfg.clone(),
        label_cache,
        hud_cache,
        avatars,
        bg: cfg.background,
        hud_h,
        group_colors,
        colors,
    };
    let params = LayoutParams {
        gamma: cfg.gamma,
        balance: cfg.balance,
        dir_balance: std::sync::Arc::new(dir_balance),
        min_weight: 1.0,
        size_cap,
        min_open_px: cfg.min_open_px,
        pad: cfg.pad,
        label_h: if cfg.show_dir_names {
            cfg.label_size + 3.0
        } else {
            0.0
        },
        label_min_px: cfg.label_min_px(),
        label_max_depth: cfg.dir_label_depth(),
    };
    let frame_rect = Rect::new(
        cfg.margin,
        cfg.margin + hud_h,
        cfg.width as f32 - 2.0 * cfg.margin,
        cfg.height as f32 - 2.0 * cfg.margin - hud_h,
    );
    (ctx, params, frame_rect)
}

fn render(args: cli::RenderArgs) -> Result<()> {
    let dry_run = args.dry_run;
    let (cfg, cfg_labels, cfg_images) = resolve_config(args)?;

    if cfg.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cfg.threads)
            .build_global()
            .ok();
    }
    if !dry_run {
        encode::check_ffmpeg(&cfg)?;
    }

    let opts = cfg.ingest_options();
    let mut history = load_history(&cfg.repo, &opts, cfg.cache)?;
    if !cfg.baseline {
        history.baseline.clear();
    }
    filter_history(&mut history, &cfg.include, &cfg.exclude)?;

    // Final-state pass for stats + auto size cap.
    let mut final_state = model::WorldState::new(&history);
    for c in &history.commits {
        final_state.apply(c);
    }
    let size_cap = cfg
        .size_cap
        .unwrap_or_else(|| auto_size_cap(&final_state, &history));

    let total_frames = cfg.total_frames(history.commits.len());
    let fpc = total_frames as f64 / history.commits.len().max(1) as f64;
    eprintln!(
        "• plan: {}x{} @ {}fps, {} frames (~{:.1}s), {:.2} frames/commit [{}]",
        cfg.width,
        cfg.height,
        cfg.fps,
        total_frames,
        total_frames as f64 / cfg.fps as f64,
        fpc,
        if fpc >= 1.0 { "smooth" } else { "fast-forward" },
    );
    eprintln!(
        "  final state: {} files, {} lines, size-cap {:.0}",
        final_state.present_files(),
        final_state.total_lines(),
        size_cap
    );
    if dry_run {
        eprintln!("(dry run — not rendering)");
        return Ok(());
    }

    // Precompute path -> language id once.
    let path_lang = lang::build_path_lang(&history.paths);

    let colors = build_colors(&cfg, &history, &final_state, size_cap);

    eprintln!("• building {} avatars ...", history.authors.len());
    let (ctx, params, frame_rect) = setup_render(
        &cfg,
        &history,
        &cfg_labels,
        &cfg_images,
        size_cap,
        colors,
        dir_balance_map(&cfg, &history)?,
    );

    eprintln!("• rendering → {} ...", cfg.out.display());
    let t1 = Instant::now();
    driver::run(&history, &ctx, &params, frame_rect, &path_lang)?;
    let dt = t1.elapsed();
    eprintln!(
        "✓ done in {:.1}s ({:.1} fps render)  →  {}",
        dt.as_secs_f64(),
        total_frames as f64 / dt.as_secs_f64(),
        cfg.out.display()
    );
    Ok(())
}

fn snapshot(args: cli::SnapshotArgs) -> Result<()> {
    let cli::SnapshotArgs {
        render,
        at,
        commit,
        date,
        no_highlight,
    } = args;

    // Decide the PNG output path.
    let mut out = render.out.clone();
    let is_default = out == *"output/atlas.mp4";
    let is_video = out
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_lowercase().as_str(), "mp4" | "webm" | "mov" | "mkv"))
        .unwrap_or(false);
    if is_default {
        out = PathBuf::from("snapshot.png");
    } else if is_video {
        out.set_extension("png");
    }

    let (mut cfg, cfg_labels, cfg_images) = resolve_config(render)?;
    // A still never shows floating committer avatars or beams.
    cfg.show_avatars = false;
    cfg.show_beams = false;

    if cfg.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cfg.threads)
            .build_global()
            .ok();
    }

    let opts = cfg.ingest_options();
    // A revision or date needs only the repository's state at that commit, not
    // its history: read the tree directly (fast on huge repos). `--commit N`
    // counts through the walked history, and `--empty-start` makes the state
    // depend on the window, so those still walk it.
    let fast = commit.is_none() && cfg.baseline;
    let (mut history, idx, pos, n) = if fast {
        let sha = snapshot_target(&cfg, &at, date.as_deref())?;
        let t0 = Instant::now();
        eprintln!(
            "• reading the repository at {} (no history walk) ...",
            &sha[..sha.len().min(9)]
        );
        let h = ingest::snapshot_at(&cfg.repo, &opts, &sha)?;
        eprintln!(
            "  {} files  ({:.1}s)",
            commafy(h.baseline.len() as u64),
            t0.elapsed().as_secs_f64()
        );
        let (pos, n) = ingest::pool_position(&cfg.repo, &opts, &sha, h.commits[0].time);
        (h, 0, pos, n)
    } else {
        let mut h = load_history(&cfg.repo, &opts, cfg.cache)?;
        if !cfg.baseline {
            h.baseline.clear();
        }
        let idx = resolve_commit_index(&cfg.repo, &h, &at, commit, date.as_deref())?;
        let n = h.commits.len();
        (h, idx, idx, n)
    };
    filter_history(&mut history, &cfg.include, &cfg.exclude)?;
    let target = &history.commits[idx];

    // Build the state at the target commit (the fast path's baseline already
    // is that state; the walked path replays up to and including it).
    let mut state = model::WorldState::new(&history);
    if !fast {
        for c in &history.commits[..=idx] {
            state.apply(c);
        }
    }
    let snap = state.snapshot();
    let size_cap = cfg
        .size_cap
        .unwrap_or_else(|| auto_size_cap(&state, &history));

    // Language split for the HUD.
    let path_lang = lang::build_path_lang(&history.paths);
    let mut lang_lines = vec![0u64; lang::LANGS.len()];
    let mut lang_files = vec![0u32; lang::LANGS.len()];
    for &(pid, sz) in &snap.files {
        let li = path_lang[pid as usize] as usize;
        lang_lines[li] += sz as u64;
        lang_files[li] += 1;
    }
    let langs = lang::top_langs_fl(&lang_lines, &lang_files, 6);

    // Color groups come from the FINAL state, so a still matches the video.
    for c in &history.commits[idx + 1..] {
        state.apply(c);
    }
    let colors = build_colors(&cfg, &history, &state, size_cap);

    eprintln!("• building context ...");
    let (ctx, params, frame_rect) = setup_render(
        &cfg,
        &history,
        &cfg_labels,
        &cfg_images,
        size_cap,
        colors,
        dir_balance_map(&cfg, &history)?,
    );

    // Tree + layout at the target state.
    let collapse = cfg.depth_mode == crate::config::DepthMode::Collapse;
    let tree = model::build_tree(&snap.files, &history, &ctx.colors, cfg.max_depth, collapse);
    let laid = layout::layout(&tree, frame_rect, &params);
    let kf = std::sync::Arc::new(render::Keyframe {
        k: idx,
        tree,
        layout: laid,
        files: snap.files_count,
        lines: snap.lines,
        langs: langs.clone(),
    });

    // Highlight the files this commit changed.
    let mut changed = rustc_hash::FxHashMap::default();
    if !no_highlight {
        for ch in &target.changes {
            let path = &history.paths[ch.path as usize];
            if let Some(key) = driver::resolve_tile(&kf.layout, path) {
                changed.entry(key).or_insert(render::ChangeGlow {
                    intensity: 1.0,
                    kind: ch.kind,
                });
            }
        }
    }

    let author = ctx
        .avatars
        .names
        .get(target.author as usize)
        .cloned()
        .unwrap_or_default();
    let hud = render::Hud {
        date: fmt_date(target.time),
        author,
        subject: target.subject.clone(),
        commit_hash: target.hash.chars().take(9).collect(),
        commit_idx: pos,
        total_commits: n,
        files: snap.files_count,
        lines: snap.lines,
        progress: if n > 1 {
            pos as f32 / (n - 1) as f32
        } else {
            0.0
        },
        langs,
    };
    let plan = render::FramePlan {
        frame_index: idx as u64,
        lo: kf.clone(),
        hi: kf,
        frac: 0.0,
        changed,
        beams: Vec::new(),
        avatars: Vec::new(),
        hud,
    };

    // Render a single frame into a pixmap and save it as PNG (no ffmpeg).
    let mut scratch = render::frame::Scratch::new(cfg.width, cfg.height);
    let mut buf = vec![0u8; (cfg.width * cfg.height * 3) as usize];
    render::frame::render_frame(&ctx, &plan, &mut scratch, &mut buf);
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).ok();
    }
    scratch
        .pm
        .save_png(&out)
        .map_err(|e| anyhow::anyhow!("saving PNG {}: {e}", out.display()))?;

    eprintln!(
        "✓ wrote {}  (commit {}/{}, {}, {})",
        out.display(),
        pos + 1,
        n,
        fmt_date(target.time),
        &target.hash[..target.hash.len().min(9)]
    );
    Ok(())
}

/// Resolve which commit (index into `history.commits`) to snapshot.
/// Commit sha a fast snapshot renders: `--date` = the last commit on/before
/// that day on `--rev`, otherwise the `--at` revision.
fn snapshot_target(cfg: &Config, at: &str, date: Option<&str>) -> Result<String> {
    if let Some(d) = date {
        parse_date_to_epoch(d)?; // same validation / error as the walked path
        let before = format!("--before={d} 23:59:59");
        let mut a = vec!["rev-list", "-1"];
        if cfg.first_parent {
            a.push("--first-parent");
        }
        a.push(&before);
        a.push(&cfg.rev);
        return git_output(&cfg.repo, &a)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("no commit on or before {d}"));
    }
    git_output(
        &cfg.repo,
        &["rev-parse", "--verify", &format!("{at}^{{commit}}")],
    )
    .ok_or_else(|| anyhow::anyhow!("could not resolve revision '{at}'"))
}

fn resolve_commit_index(
    repo: &std::path::Path,
    history: &History,
    at: &str,
    commit: Option<usize>,
    date: Option<&str>,
) -> Result<usize> {
    let n = history.commits.len();
    if n == 0 {
        anyhow::bail!("no commits");
    }
    if let Some(c) = commit {
        return Ok(c.min(n - 1));
    }
    if let Some(d) = date {
        let epoch = parse_date_to_epoch(d)?;
        let mut idx = 0usize;
        let mut found = false;
        for (i, c) in history.commits.iter().enumerate() {
            if c.time <= epoch {
                idx = i;
                found = true;
            }
        }
        if !found {
            idx = 0;
        }
        return Ok(idx);
    }
    // --at revision: resolve via git and locate it in the walked history.
    let sha = git_output(repo, &["rev-parse", at])
        .ok_or_else(|| anyhow::anyhow!("could not resolve revision '{at}'"))?;
    if let Some(i) = history
        .commits
        .iter()
        .position(|c| c.hash == sha || c.hash.starts_with(&sha) || sha.starts_with(&c.hash))
    {
        return Ok(i);
    }
    // Not on the walked (first-parent) line: fall back to its timestamp.
    if let Some(ts) = git_output(repo, &["show", "-s", "--format=%ct", &sha])
        .and_then(|s| s.trim().parse::<i64>().ok())
    {
        let mut idx = n - 1;
        let mut found = false;
        for (i, c) in history.commits.iter().enumerate() {
            if c.time <= ts {
                idx = i;
                found = true;
            }
        }
        if !found {
            idx = 0;
        }
        return Ok(idx);
    }
    anyhow::bail!("could not locate commit '{at}' in the walked history")
}

fn git_output(repo: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn gen_config(args: &GenConfigArgs) -> Result<()> {
    let opts = args.scan.ingest_options();
    let history = load_history(&args.scan.repo, &opts, !args.scan.no_cache)?;
    let text = configfile::generate(&Config::default(), &history);
    std::fs::write(&args.out, &text)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", args.out.display()))?;
    // Report the resolved location, not just the (possibly relative) argument.
    let full = std::path::absolute(&args.out).unwrap_or_else(|_| args.out.clone());
    eprintln!(
        "✓ wrote {} ({} committers)",
        full.display(),
        history.authors.len()
    );
    eprintln!(
        "  Edit labels/images, then: gitatlas --config {} <repo>",
        args.out.display()
    );
    Ok(())
}

fn info(scan: &ScanArgs) -> Result<()> {
    let opts = scan.ingest_options();
    let history = load_history(&scan.repo, &opts, !scan.no_cache)?;
    let mut state = model::WorldState::new(&history);
    for c in &history.commits {
        state.apply(c);
    }
    let first = history.commits.first().map(|c| c.time).unwrap_or(0);
    let last = history.commits.last().map(|c| c.time).unwrap_or(0);
    println!("Repository: {}", scan.repo.display());
    println!("Commits:    {}", history.commits.len());
    println!("Authors:    {}", history.authors.len());
    println!("Unique paths (all-time): {}", history.paths.len());
    println!("Date range: {} → {}", fmt_date(first), fmt_date(last));
    println!(
        "Final tree: {} files, {} lines",
        state.present_files(),
        state.total_lines()
    );
    Ok(())
}

fn languages(scan: &ScanArgs) -> Result<()> {
    let opts = scan.ingest_options();
    let history = load_history(&scan.repo, &opts, !scan.no_cache)?;
    let path_lang = lang::build_path_lang(&history.paths);
    let mut state = model::WorldState::new(&history);
    for c in &history.commits {
        state.apply(c);
    }
    let mut lang_lines = vec![0u64; lang::LANGS.len()];
    let mut lang_files = vec![0u32; lang::LANGS.len()];
    for pid in 0..history.paths.len() as u32 {
        if state.is_present(pid) {
            let s = state.size_of(pid);
            if s > 0 {
                let li = path_lang[pid as usize] as usize;
                lang_lines[li] += s as u64;
                lang_files[li] += 1;
            }
        }
    }
    let total: u64 = lang_lines.iter().sum::<u64>().max(1);
    let top = lang::top_langs_fl(&lang_lines, &lang_files, lang::LANGS.len());
    println!("Language breakdown ({} LoC total):", commafy(total));
    println!(
        "  {:<14} {:>6}  {:>9}  {:>13}",
        "language", "share", "files", "LoC"
    );
    for (lid, lines, files) in top {
        let l = &lang::LANGS[lid as usize];
        println!(
            "  {:<14} {:>5.1}%  {:>9}  {:>13}",
            l.name,
            lines as f64 / total as f64 * 100.0,
            commafy(files as u64),
            commafy(lines)
        );
    }
    Ok(())
}

fn build_globset(pats: &[String]) -> Result<Option<GlobSet>> {
    if pats.is_empty() {
        return Ok(None);
    }
    let mut b = GlobSetBuilder::new();
    for p in pats {
        b.add(build_glob(p)?);
    }
    Ok(Some(b.build()?))
}

/// One path glob. Case-insensitive (so `*.json` also catches `X.JSON`, matching
/// how languages are detected), and tolerant of quotes that a shell passed
/// through literally (cmd.exe does not strip '...').
fn build_glob(p: &str) -> Result<Glob> {
    let t = p.trim();
    let t = t
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| t.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .unwrap_or(t);
    // "src/docs/" means the folder, same as "src/docs" (see `glob_hits`).
    let t = if t.len() > 1 {
        t.trim_end_matches('/')
    } else {
        t
    };
    globset::GlobBuilder::new(t)
        .case_insensitive(true)
        .build()
        .map_err(|e| anyhow::anyhow!("bad glob '{p}': {e}"))
}

/// A path glob selects a file when it matches the file's path or any folder
/// above it, so `src/docs`, `src/docs/` and `src/docs/**` all select everything
/// in that folder — including folders that come from a submodule.
fn glob_hits(is_match: impl Fn(&str) -> bool, path: &str) -> bool {
    if is_match(path) {
        return true;
    }
    let mut dir = path;
    while let Some(i) = dir.rfind('/') {
        dir = &dir[..i];
        if is_match(dir) {
            return true;
        }
    }
    false
}

/// Effective balance of every folder matched by a balance rule (keyed like tree
/// nodes); unmatched folders keep the global balance. Last matching rule wins.
fn dir_balance_map(cfg: &Config, history: &History) -> Result<rustc_hash::FxHashMap<u64, f32>> {
    let mut map = rustc_hash::FxHashMap::default();
    let rules = &cfg.balance_rules;
    if rules.is_empty() {
        return Ok(map);
    }
    let globs: Vec<globset::GlobMatcher> = rules
        .iter()
        .map(|r| build_glob(&r.folder).map(|g| g.compile_matcher()))
        .collect::<Result<_>>()?;
    let base = cfg.balance.clamp(0.0, 0.9);
    let mut seen: rustc_hash::FxHashSet<&str> = Default::default();
    let mut hits = vec![0usize; rules.len()];
    for path in &history.paths {
        let mut dir = path.as_str();
        while let Some(i) = dir.rfind('/') {
            dir = &dir[..i];
            if !seen.insert(dir) {
                break; // this folder and everything above it are done
            }
            if let Some(r) = (0..rules.len()).rev().find(|&r| globs[r].is_match(dir)) {
                hits[r] += 1;
                map.insert(
                    crate::color::hash_str(dir),
                    (base + rules[r].balance).clamp(-1.0, 1.0),
                );
            }
        }
    }
    eprintln!(
        "• balance rules: {} rule(s), {} folder(s) adjusted",
        rules.len(),
        commafy(map.len() as u64)
    );
    for (r, n) in rules.iter().zip(hits) {
        if n == 0 {
            eprintln!(
                "  ! warning: balance rule \"{}\" matches no folder in this repository",
                r.folder
            );
        }
    }
    Ok(map)
}

/// Apply file/folder include/exclude globs, dropping changes for paths that don't
/// pass (exclude wins). Returns the number of change records removed.
fn filter_history(history: &mut History, include: &[String], exclude: &[String]) -> Result<usize> {
    if include.is_empty() && exclude.is_empty() {
        return Ok(0);
    }
    let inc = build_globset(include)?;
    let exc = build_globset(exclude)?;
    let allowed: Vec<bool> = history
        .paths
        .iter()
        .map(|p| {
            if let Some(e) = &exc
                && glob_hits(|s| e.is_match(s), p)
            {
                return false;
            }
            if let Some(i) = &inc
                && !glob_hits(|s| i.is_match(s), p)
            {
                return false;
            }
            true
        })
        .collect();
    history.baseline.retain(|&(path, _)| allowed[path as usize]);
    let mut removed = 0usize;
    for c in &mut history.commits {
        let before = c.changes.len();
        c.changes.retain(|ch| allowed[ch.path as usize]);
        removed += before - c.changes.len();
    }

    // Always say what the filters did: a glob that matches nothing (typo, shell
    // quoting, wrong separator) would otherwise fail silently.
    let kept = allowed.iter().filter(|a| **a).count();
    eprintln!(
        "• path filter: {} include / {} exclude globs → {} of {} paths kept, {} change records dropped",
        include.len(),
        exclude.len(),
        commafy(kept as u64),
        commafy(allowed.len() as u64),
        commafy(removed as u64)
    );
    for (kind, pats) in [("include", include), ("exclude", exclude)] {
        for p in pats {
            let m = build_glob(p)?.compile_matcher();
            if !history
                .paths
                .iter()
                .any(|path| glob_hits(|s| m.is_match(s), path))
            {
                eprintln!("  ! warning: --{kind} \"{p}\" matches no file in this repository");
            }
        }
    }
    Ok(removed)
}

/// Decide which folders get their own color (see `groups.rs`) from the final
/// repository state, and say so when auto detection moved off the top level.
fn build_colors(
    cfg: &Config,
    history: &History,
    final_state: &model::WorldState,
    size_cap: f32,
) -> groups::ColorMap {
    let mut modules = cfg.color_modules.clone();
    if cfg.submodule_colors {
        modules.extend(history.submodules.iter().cloned());
    }
    let colors = groups::ColorMap::build(
        &history.paths,
        &final_state.snapshot().files,
        size_cap,
        &cfg.color_roots,
        &modules,
        cfg.auto_color,
    );
    if !colors.auto_root.is_empty() {
        eprintln!(
            "• colors: most of the repo is under {0}/ — coloring by its subfolders (--no-auto-color to disable)",
            colors.auto_root
        );
    }
    colors
}

/// Robust, stable area cap: ~95th percentile of final-state file sizes.
fn auto_size_cap(state: &model::WorldState, history: &ingest::History) -> f32 {
    let mut sizes: Vec<u32> = Vec::new();
    for pid in 0..history.paths.len() as u32 {
        if state.is_present(pid) {
            let s = state.size_of(pid);
            if s > 0 {
                sizes.push(s as u32);
            }
        }
    }
    if sizes.is_empty() {
        return 2000.0;
    }
    sizes.sort_unstable();
    let idx = ((sizes.len() as f32) * 0.95) as usize;
    (sizes[idx.min(sizes.len() - 1)] as f32).max(200.0)
}
