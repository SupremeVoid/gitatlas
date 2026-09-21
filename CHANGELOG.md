# Changelog

All notable changes to gitatlas are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## 1.2.0

- **Folder balancing (`--balance`, default 0.25).** The split of area among
  sibling folders is compressed (`size^(1-balance)`), so a dominant module no
  longer squeezes the rest of a monorepo into specks — without excluding
  anything. It uses true sizes at each level (no compounding with depth) and
  treats a folder's loose files as one sibling. `--balance 0` restores strictly
  proportional areas.
- Default frame rate is now **30 fps** (was 60); pass `--fps 60` for the old look.
- **Windowed renders start from the existing repository.** With `--since`,
  `--max-commits` or a rev range, the map now opens with everything that already
  existed before the first commit (read from that commit's parent tree) instead
  of only growing the files the window happens to touch. File/LoC totals, the
  language split, auto size cap and color groups are correct from frame one.
  `--empty-start` (config: `baseline = false`) restores the old behavior. The
  history cache format changed, so the first run re-reads git once.
- **Path filters can no longer fail silently.** `--include` / `--exclude` always
  report how many paths were kept, and warn about any glob that matches nothing.
  Globs are now case-insensitive (`*.json` also drops `X.JSON`, like language
  detection), and literal quotes passed through by the shell (cmd.exe `'…'`) are
  stripped.
- Config files reject unknown keys, so a misspelled or misplaced option (e.g.
  `exclude` written below a `[[committer]]` block) is an error instead of being
  ignored.

## 1.1.0

- **Color groups.** Repos that keep nearly everything under one chain (e.g.
  `src/app/`) no longer render in a single color: the dominant chain is detected
  from the final state and its subfolders become the color groups, with the chain
  folders drawn neutral. New `--color-root`, `--color-module`, `--no-auto-color`
  (config: `color_roots`, `color_modules`, `auto_color`). Files sitting directly in
  the repo root now share one color instead of one hue per file. A declared
  module's hue is guaranteed to differ from the group around it by at least 60°.
- Each author draws at most one beam per tile, so many changed files under one
  collapsed folder no longer stack into a white bar.
- **Folder labels.** Every open folder that reserves a header strip now shows its
  name — previously folders deeper than `dir_name_max_depth` kept an empty header.
  `--dir-name-max-depth N` now means "label the top N levels" (default 0 = all);
  unlabeled levels reserve no header space.
- **Fewer blank tiles.** Collapsed folders now get minimap lines (from their
  aggregated line count), tiles down to ~5 px get a line or two, and a short file
  fills at least ~60% of its tile instead of a few lines atop an empty box.
- `gen-config` prints the full path of the file it wrote.
- **Beams** now live exactly as long as their commit is on screen instead of
  lingering with the avatar; `--beam-seconds` (default 0.25) is the minimum.

## 1.0.0

First stable release.

### Rendering
- Evolving **treemap "atlas" video** of a git repository, encoded with the system
  `ffmpeg` (MP4/H.264 or WebM/VP9).
- Stable, order-preserving squarified layout with recursive containment and
  level-of-detail folder collapse; per-root-folder border colors.
- File tiles filled with size-proportional "minimap" lines; smooth grow/shrink/
  fade transitions between commit states.
- One avatar per active committer with a unique color, flowing name, wandering
  motion and idle fade-out; additive glow beams to changed files. Avatar images
  from a config file, an image directory, gravatar (opt-in), or 14 built-in icons.
- Top meta bar (date, author, subject, commit counter, file/LoC totals) plus a
  live **language split** with colored swatches.

### CLI
- Subcommands: `render` (default), `info`, `languages`, `gen-config`, `snapshot`
  (still PNG of one commit, no ffmpeg).
- Reusable, fully-commented TOML config (`gen-config` / `--config`) with per-
  committer label/image overrides.
- Path filters (`--include` / `--exclude` globs), folder-depth control
  (`--max-depth`, `--depth-mode omit|collapse`), submodule support and filters,
  and a large set of layout/label/HUD/avatar/timing options.

### Engineering
- Pure-Rust (no C toolchain): history read via the `git` CLI, parsed once and
  cached on disk; multi-threaded CPU rendering piped to ffmpeg.
- Unit tests, and CI (fmt, clippy, tests, build, smoke) + a tag-triggered Linux
  release workflow.
