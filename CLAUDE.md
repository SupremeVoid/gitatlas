# CLAUDE.md — gitatlas architecture & working notes

`gitatlas` renders a git repository's evolution as an animated treemap "atlas"
video. Pure Rust, single binary; shells out to
`git` (read history) and `ffmpeg` (encode). See `README.md` for user docs.

## Build / test

```bash
cargo build --release                 # primary; LTO + panic=abort + strip
cargo build --profile bench-dev       # fast-compile, optimized runtime (iteration)
./target/release/gitatlas <repo> -o out.mp4 [opts]
./target/release/gitatlas <repo> --dry-run     # print plan, no render
```

Test repos live in `repos/` (gitignored): `repos/synthetic` (tiny, fast smoke
test), `repos/gitea` (~20k commits), `repos/angular` (~37k commits). All render
output goes to `output/` (gitignored). Inspect frames with
`ffmpeg -i out.mp4 -vf "select='eq(n\,N)'" -vframes 1 frame.png`.

Parsed history is cached at `<repo>/.git/gitatlas-history.bin` (keyed by HEAD +
ingest options), so only the first run per repo pays the `git log` cost (~23s for
gitea, ~38s for angular). Re-renders with different visual options are instant to
start.

## Pipeline / module map

```
ingest.rs   git log --raw --numstat (one stream)  → History{paths, authors, commits}
            - line-count deltas (metric for minimap); binary files ('-') dropped
            - --raw gives A/M/D status + file mode; --numstat gives counts+binary
            - submodules: gitlinks (mode 160000) included by default as nominal
              tiles (SUBMODULE_NOMINAL lines); IngestOptions has submodules +
              include/exclude filters
lang.rs     extension -> language id + color (LANGS table); build_path_lang once,
            per-keyframe line totals feed the HUD language split
cache.rs    binary (de)serialize History, keyed by HEAD+opts
model.rs    WorldState: path_id -> line count, advanced per commit (apply())
            snapshot() → StateSnapshot; build_tree(files,…,collapse) → Tree (arena).
            max_depth cutoff: omit (drop deeper files) or collapse (deeper
            subfolders become childless aggregate Dir nodes; layout renders any
            childless dir as a collapsed tile).
layout.rs   ordered "squarified strip" treemap (STABLE: name-sorted siblings,
            recursive containment, LOD folder collapse). layout(tree,rect,params)→Layout
driver.rs   the orchestrator. Chunked pipeline:
              1. sequentially advance state + snapshot the states a chunk needs
              2. PARALLEL build keyframes (tree+layout) from snapshots
              3. PARALLEL build FramePlan + rasterize each frame
              4. write frames to ffmpeg in order
            maps frames↔commit positions; "smooth" (≥1 frame/commit) vs
            "fast-forward"; per-frame active-commit → avatars/beams/highlights.
render/
  text.rs   fontdue glyph coverage cache (pre-warmed), alpha-blit onto pixmap
  avatar.rs avatar-config file / image dir / gravatar / 14 built-in pixel icons
            → premult circular sprites; unique per-author hue (golden-angle by
            index) shared by avatar tint + beams; wander + 5s idle fade; names
  frame.rs  RenderCtx + Scratch; render_frame(): interpolate tiles (lo→hi),
            fills/minimap-lines/borders/labels (direct pixel writes, no AA on hot
            paths), change-glow, beams (tiny-skia BlendMode::Plus), avatars, HUD;
            repack premult-RGBA → rgb24
encode.rs   spawn system ffmpeg once, feed rgb24 over stdin (in order),
            drain stderr on a thread, friendly errors. mp4(x264)/webm(vp9).
groups.rs   ColorMap (built once from the FINAL state): path id -> group hue.
            Group = direct subfolder of the deepest "color root" above a file;
            roots = "" + --color-root (+ancestors) + the auto-detected dominant
            chain (child >= 70% of parent's capped lines); --color-module folders
            are their own group. Root dirs get NEUTRAL_HUE (gray). Lives in
            RenderCtx.colors; build_tree takes it.
color.rs    stable hue from folder-name hash → per-root border/fill/minimap colors
config.rs   resolved Config (all options).
configfile.rs  gen-config writes a fully-commented TOML (all options at default +
            a [[committer]] block per author with label/image); load() parses it
            (serde, comments ignored) → base Config + label/image maps;
            merge_cli_over() lets CLI flags override the file via a toml-Value diff.
            (Config + Codec/Quality are serde-derived; #[serde(default)].)
cli.rs      clap: `render` (default) + `info` + `languages` + `gen-config` +
            `snapshot` subcommands; grouped help via per-arg help_heading; --config.
            Path filters: filter_history() (globset) drops changes for paths not
            matching --include / --exclude (exclude wins), applied post-cache in
            render()/snapshot() so changing filters needs no re-ingest.
main.rs     dispatch subcommands; resolve_config() merges --config; setup_render()
            builds ctx/params/frame_rect (shared by render + snapshot). snapshot()
            renders one commit's FramePlan (no avatars/beams) to PNG via
            Pixmap::save_png — no ffmpeg; commit picked by --at/--commit/--date.
fmt.rs      shared number/date helpers (commafy, fmt_compact, fmt_date,
            civil<->days, parse_date_to_epoch) used by HUD + CLI + snapshot.
geom.rs easing.rs intern.rs   small helpers
```

## Perf notes

- GroupColor is precomputed once per distinct root hue (RenderCtx.group_colors)
  instead of ~4 HSL conversions per tile per frame; hot u64-keyed maps
  (layout.index, plan.changed, model dir_index) use FxHashMap since the keys are
  already well-mixed `hash_str` values. Together ~38% faster render (draft).
- A render/encode overlap via a dedicated writer thread was tried and REVERTED:
  libx264 is itself multi-threaded, so overlapping it with the 16-thread render
  oversubscribes cores and was ~10% slower at balanced. Balanced/high are
  ffmpeg-encode-bound; render is CPU-bound.

## Tests / CI

- In-module `#[cfg(test)]` tests (bin-only crate, no lib target): fmt date/number
  round-trips, cli parse_hex_color/parse_resolution (incl. panic-guard cases),
  WorldState::apply invariants, build_tree depth-mode, layout determinism.
- `.github/workflows/ci.yml` (fmt/clippy -D warnings/test/build/smoke on PRs) and
  `release.yml` (tag-triggered Linux `.tar.gz` GitHub release).

## Key design decisions (and why)

- **git CLI, not libgit2.** The MSVC host had no cmake/C toolchain; shelling to
  `git log --numstat` keeps the crate 100% pure-Rust (clean Windows build, trivial
  `cargo install` on Linux), streams history in one pass, gives free binary
  detection, and yields **line counts** — the natural metric for "minimap lines".
- **Layout stability = pure function of the tree.** Siblings are sorted by NAME
  (never size), packed order-preserving, and each folder is packed inside its own
  rect. So a change in one folder can't move tiles in another. Stable global size
  cap (95th pct of final state) keeps weights frame-to-frame stable.
- **Parallelism.** Keyframe build and frame raster are pure functions → run on all
  cores; only the cheap state-snapshot pass is sequential. rgb24 (not rgba) to
  ffmpeg saves 25% pipe bandwidth; opaque frames so premult==straight.
- **Fast-forward optimization.** When <1 frame/commit, one commit barely changes a
  huge treemap, so the second (hi) keyframe + interpolation are skipped, and only
  the most-recent ~600 active commits drive highlights/avatars.

## Benchmarks (this machine, 16 cores)

- angular (37k commits, 10.4k files) 1080p/30fps/1200f draft: ~14s render (85 fps),
  keyframes 1.4s.
- angular 1440p balanced: ~31 fps (ffmpeg-encode-bound at balanced).
- gitea (20k commits) 1440p/60fps/1800f balanced: ~53s (34 fps).

## Known future work

- Optional git rename detection (`-M`) so refactors animate as moves, not
  delete+add (currently `--no-renames` for simplicity/stability).
- Historically-accurate per-commit `.gitignore` (currently tracked files only,
  which are by definition not ignored).
- A few write-only struct fields remain under the crate `#![allow(dead_code)]`
  (Keyframe.k, FramePlan.frame_index, Hud.commit_hash, LaidTile.path_id/child_count);
  remove them and drop the blanket allow to keep the compiler honest.
- Musl static release build (would need to feature-gate/replace the ureq+TLS
  stack for gravatar, which pulls C/asm crypto).
```
