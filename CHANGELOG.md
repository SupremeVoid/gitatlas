# Changelog

All notable changes to gitatlas are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

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
