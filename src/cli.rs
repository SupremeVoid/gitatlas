//! Command-line interface: a `render` command (also the default) plus `info`
//! and `languages` inspection commands, with grouped, concise help.

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::config::{Codec, Config, DepthMode, Quality};
use crate::ingest::IngestOptions;

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum CodecArg {
    Mp4,
    Webm,
}
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum QualityArg {
    Draft,
    Balanced,
    High,
}
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum DepthModeArg {
    Omit,
    Collapse,
}

/// Render the evolution of a git repository as an animated treemap "atlas" video.
///
/// gitatlas replays a repository commit-by-commit as a dense, tiled treemap:
/// folders are bordered regions (one color per top-level folder), files are tiles
/// filled with "minimap" lines, and committer avatars hover over their changes
/// with glowing beams. The output is a video encoded with the system ffmpeg.
///
/// With no subcommand, gitatlas renders (so `gitatlas .` just works); with no
/// arguments at all it prints this help. Use the `info` and `languages`
/// subcommands to inspect a repo without rendering.
///
/// EXAMPLES:
///   gitatlas . -o atlas.mp4
///   gitatlas /path/to/repo --resolution 1920x1080 --seconds 30 --quality draft
///   gitatlas /repo --seconds-per-commit 0.4 --max-commits 300   # slow, smooth
///   gitatlas info /repo
///   gitatlas languages /repo
#[derive(Parser)]
#[command(
    name = "gitatlas",
    version,
    about,
    long_about = None,
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub render: RenderArgs,
}

#[derive(Subcommand)]
pub enum Command {
    /// Render the atlas video (the default action).
    Render(RenderArgs),
    /// Print repository statistics (commits, authors, files, date range) and exit.
    Info(ScanArgs),
    /// Print the language breakdown of the final tree and exit.
    #[command(alias = "langs")]
    Languages(ScanArgs),
    /// Write a reusable, fully-commented config file listing every committer.
    #[command(alias = "genconfig", alias = "init-config")]
    GenConfig(GenConfigArgs),
    /// Render a single commit's state as a still PNG image (no video, no avatars).
    #[command(alias = "still")]
    Snapshot(SnapshotArgs),
}

/// Arguments for `snapshot` — a still PNG of one commit. Reuses the render
/// options (timing/codec options are ignored); picks the commit via one of
/// --at / --commit / --date.
#[derive(Args)]
pub struct SnapshotArgs {
    #[command(flatten)]
    pub render: RenderArgs,
    /// Commit to snapshot: a git revision (sha, tag, HEAD, HEAD~20). [default: HEAD]
    #[arg(long, default_value = "HEAD", help_heading = "Snapshot")]
    pub at: String,
    /// Snapshot the Nth commit (0-based) in the walked history (overrides --at).
    #[arg(long, help_heading = "Snapshot")]
    pub commit: Option<usize>,
    /// Snapshot the state as of this date YYYY-MM-DD (last commit on/before it).
    #[arg(long, help_heading = "Snapshot")]
    pub date: Option<String>,
    /// Do not highlight the files changed by the snapshotted commit.
    #[arg(long, help_heading = "Snapshot")]
    pub no_highlight: bool,
}

/// Arguments for `gen-config`.
#[derive(Args)]
pub struct GenConfigArgs {
    #[command(flatten)]
    pub scan: ScanArgs,
    /// Where to write the config file.
    #[arg(short, long, default_value = "gitatlas.toml")]
    pub out: PathBuf,
}

/// Arguments shared by the inspection subcommands.
#[derive(Args)]
pub struct ScanArgs {
    /// Path to the git repository.
    #[arg(default_value = ".")]
    pub repo: PathBuf,
    /// Revision / branch to walk.
    #[arg(long, default_value = "HEAD")]
    pub rev: String,
    /// Only include commits after this date (git --since).
    #[arg(long)]
    pub since: Option<String>,
    /// Only include commits before this date (git --until).
    #[arg(long)]
    pub until: Option<String>,
    /// Limit to the most recent N commits (0 = all).
    #[arg(long, default_value_t = 0)]
    pub max_commits: usize,
    /// Walk all commits, not just the first-parent mainline.
    #[arg(long)]
    pub full_history: bool,
    /// Levels of git submodules whose history is merged in (0 = none, 1 = direct
    /// submodules, 2 = also theirs, ...).
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub submodule_depth: u32,
    /// Shorthand for --submodule-depth 0.
    #[arg(long)]
    pub no_submodules: bool,
    /// Only include submodules matching NAME (repeatable).
    #[arg(long = "include-submodule", value_name = "NAME")]
    pub include_submodule: Vec<String>,
    /// Exclude submodules matching NAME (repeatable).
    #[arg(long = "exclude-submodule", value_name = "NAME")]
    pub exclude_submodule: Vec<String>,
    /// Ignore the on-disk history cache.
    #[arg(long)]
    pub no_cache: bool,
}

impl ScanArgs {
    pub fn ingest_options(&self) -> IngestOptions {
        IngestOptions {
            first_parent: !self.full_history,
            since: self.since.clone(),
            until: self.until.clone(),
            max_commits: self.max_commits,
            rev: self.rev.clone(),
            submodule_depth: if self.no_submodules {
                0
            } else {
                self.submodule_depth
            },
            submodule_include: self.include_submodule.clone(),
            submodule_exclude: self.exclude_submodule.clone(),
        }
    }
}

/// All render options, grouped into sections in `--help`.
#[derive(Args)]
pub struct RenderArgs {
    // ---------------- Input ----------------
    /// Path to the git repository.
    #[arg(default_value = ".", help_heading = "Git & input")]
    pub repo: PathBuf,
    /// Revision / branch to walk.
    #[arg(long, default_value = "HEAD", help_heading = "Git & input")]
    pub rev: String,
    /// Only include commits after this date (git --since, e.g. 2020-01-01).
    #[arg(long, help_heading = "Git & input")]
    pub since: Option<String>,
    /// Only include commits before this date (git --until).
    #[arg(long, help_heading = "Git & input")]
    pub until: Option<String>,
    /// Limit to the most recent N commits (0 = all).
    #[arg(long, default_value_t = 0, help_heading = "Git & input")]
    pub max_commits: usize,
    /// Walk all commits, not just the first-parent mainline.
    #[arg(long, help_heading = "Git & input")]
    pub full_history: bool,
    /// Levels of git submodules whose full history is merged into the timeline
    /// and drawn as ordinary folders: 0 = none, 1 = the repo's own submodules,
    /// 2 = also their submodules, ... Submodules must be checked out.
    #[arg(
        long,
        value_name = "N",
        default_value_t = 1,
        help_heading = "Git & input"
    )]
    pub submodule_depth: u32,
    /// Shorthand for --submodule-depth 0.
    #[arg(long, help_heading = "Git & input")]
    pub no_submodules: bool,
    /// Only include submodules matching NAME (repeatable; name, path, or substring).
    #[arg(
        long = "include-submodule",
        value_name = "NAME",
        help_heading = "Git & input"
    )]
    pub include_submodule: Vec<String>,
    /// Exclude submodules matching NAME (repeatable; name, path, or substring).
    #[arg(
        long = "exclude-submodule",
        value_name = "NAME",
        help_heading = "Git & input"
    )]
    pub exclude_submodule: Vec<String>,
    /// With --since / --max-commits / a rev range, start from an empty map
    /// instead of the repository as it already was before the first commit.
    #[arg(long, help_heading = "Git & input")]
    pub empty_start: bool,

    // ---------------- Files & folders ----------------
    /// Only visualize files whose path matches GLOB (repeatable). Globs are
    /// matched against the repo-relative path, e.g. "src/**", "**/*.rs".
    #[arg(long, value_name = "GLOB", help_heading = "Files & folders")]
    pub include: Vec<String>,
    /// Exclude files whose path matches GLOB (repeatable), e.g. "**/tests/**",
    /// "vendor/**". Exclude wins over include.
    #[arg(long, value_name = "GLOB", help_heading = "Files & folders")]
    pub exclude: Vec<String>,

    // ---------------- Output & timing ----------------
    /// Output video file. Extension implies the codec unless --codec is given.
    #[arg(
        short,
        long,
        default_value = "output/atlas.mp4",
        help_heading = "Output & timing"
    )]
    pub out: PathBuf,
    /// Container/codec: mp4 (H.264) or webm (VP9).
    #[arg(long, value_enum, help_heading = "Output & timing")]
    pub codec: Option<CodecArg>,
    /// Encode speed/quality tradeoff.
    #[arg(
        long,
        value_enum,
        default_value = "balanced",
        help_heading = "Output & timing"
    )]
    pub quality: QualityArg,
    /// Resolution "WxH" (e.g. 2560x1440). Overrides --width/--height.
    #[arg(long, help_heading = "Output & timing")]
    pub resolution: Option<String>,
    /// Frame width in pixels (even).
    #[arg(long, default_value_t = 2560, help_heading = "Output & timing")]
    pub width: u32,
    /// Frame height in pixels (even).
    #[arg(long, default_value_t = 1440, help_heading = "Output & timing")]
    pub height: u32,
    /// Frames per second.
    #[arg(long, default_value_t = 30, help_heading = "Output & timing")]
    pub fps: u32,
    /// Target video length in seconds.
    #[arg(long, default_value_t = 60.0, help_heading = "Output & timing")]
    pub seconds: f64,
    /// Seconds of screen time per commit (overrides --seconds for smooth playback).
    #[arg(long, help_heading = "Output & timing")]
    pub seconds_per_commit: Option<f64>,
    /// Hard cap on total frames (0 = none).
    #[arg(long, default_value_t = 0, help_heading = "Output & timing")]
    pub max_frames: u64,
    /// Worker threads (0 = auto / all cores).
    #[arg(long, default_value_t = 0, help_heading = "Output & timing")]
    pub threads: usize,

    // ---------------- Layout ----------------
    /// Maximum folder depth to render (0 = unlimited).
    #[arg(long, default_value_t = 0, help_heading = "Layout")]
    pub max_depth: u32,
    /// What to do with items deeper than --max-depth: omit (drop) or collapse
    /// (merge each too-deep subfolder into one tile).
    #[arg(long, value_enum, default_value = "omit", help_heading = "Layout")]
    pub depth_mode: DepthModeArg,
    /// Area-metric gamma compression exponent (0.3..1.0; lower = flatter sizes).
    #[arg(long, default_value_t = 0.5, help_heading = "Layout")]
    pub gamma: f32,
    /// Balance sibling folders (0..0.9). 0 = area strictly proportional to size;
    /// higher gives dominant folders less and small ones more, so no module ends
    /// up a speck (0.25: a folder 100x its neighbour gets ~32x the area).
    #[arg(long, default_value_t = 0.25, help_heading = "Layout")]
    pub balance: f32,
    /// Adjust --balance for folders matching GLOB by DELTA (repeatable; last
    /// match wins). Higher pulls the folder toward a typical sibling's size (a
    /// giant shrinks, a speck grows), lower toward its true proportion.
    /// E.g. "src/docs=+0.4", "vendor/**=-0.25". Effective range -1..1.
    #[arg(
        long = "balance-rule",
        value_name = "GLOB=DELTA",
        help_heading = "Layout"
    )]
    pub balance_rule: Vec<String>,
    /// Fixed area-metric cap in lines (default: auto = 95th percentile).
    #[arg(long, help_heading = "Layout")]
    pub size_cap: Option<f32>,
    /// Folders whose shorter side is below this many px are drawn collapsed.
    #[arg(long, default_value_t = 34.0, help_heading = "Layout")]
    pub min_open_px: f32,
    /// Padding inside each folder (px).
    #[arg(long, default_value_t = 3.0, help_heading = "Layout")]
    pub pad: f32,
    /// Outer margin around the whole atlas (px).
    #[arg(long, default_value_t = 8.0, help_heading = "Layout")]
    pub margin: f32,
    /// Color saturation for group borders (0..1).
    #[arg(long, default_value_t = 0.72, help_heading = "Layout")]
    pub saturation: f32,
    /// Background color as #RRGGBB.
    #[arg(long, default_value = "#0c0d10", help_heading = "Layout")]
    pub background: String,

    // ---------------- Color groups ----------------
    /// Color by the subfolders of FOLDER instead of by top-level folder
    /// (repeatable), e.g. "src/app" gives every src/app/* its own color.
    #[arg(
        long = "color-root",
        value_name = "FOLDER",
        help_heading = "Color groups"
    )]
    pub color_root: Vec<String>,
    /// Declare FOLDER as one color module of its own (repeatable), e.g.
    /// "src/app/shared/ui". Wins over --color-root and auto detection.
    #[arg(
        long = "color-module",
        value_name = "FOLDER",
        help_heading = "Color groups"
    )]
    pub color_module: Vec<String>,
    /// Don't auto-descend through a dominant folder chain (like src/app) to find
    /// the level where the tree splits; always color by top-level folder.
    #[arg(long, help_heading = "Color groups")]
    pub no_auto_color: bool,
    /// Don't give merged submodules a color of their own; they are colored like
    /// any other folder instead.
    #[arg(long, help_heading = "Color groups")]
    pub no_submodule_colors: bool,

    // ---------------- Tiles & minimap ----------------
    /// Hide the minimap lines inside file tiles.
    #[arg(long, help_heading = "Tiles & minimap")]
    pub no_minimap: bool,
    /// Vertical gap between minimap lines (px).
    #[arg(long, default_value_t = 3.0, help_heading = "Tiles & minimap")]
    pub minimap_line_gap: f32,
    /// Maximum minimap lines drawn per file tile.
    #[arg(long, default_value_t = 240, help_heading = "Tiles & minimap")]
    pub minimap_max_lines: u32,
    /// Hide tile borders.
    #[arg(long, help_heading = "Tiles & minimap")]
    pub no_border: bool,
    /// Tile border width (px).
    #[arg(long, default_value_t = 1.0, help_heading = "Tiles & minimap")]
    pub border_width: f32,

    // ---------------- Labels ----------------
    /// Hide folder names.
    #[arg(long, help_heading = "Labels")]
    pub no_dir_names: bool,
    /// Only label the top N folder levels (0 = every folder big enough). Folders
    /// below that reserve no header space.
    #[arg(long, default_value_t = 0, help_heading = "Labels")]
    pub dir_name_max_depth: u32,
    /// Show file names on sufficiently large tiles.
    #[arg(long, help_heading = "Labels")]
    pub file_names: bool,
    /// Minimum tile width (px) needed to draw a file name.
    #[arg(long, default_value_t = 42.0, help_heading = "Labels")]
    pub file_name_min_px: f32,
    /// Label font size (px).
    #[arg(long, default_value_t = 13.0, help_heading = "Labels")]
    pub label_size: f32,

    // ---------------- Avatars & beams ----------------
    /// Hide committer avatars.
    #[arg(long, help_heading = "Avatars & beams")]
    pub no_avatars: bool,
    /// Hide the committer name flowing beneath each avatar.
    #[arg(long, help_heading = "Avatars & beams")]
    pub no_avatar_names: bool,
    /// Avatar diameter (px).
    #[arg(long, default_value_t = 72, help_heading = "Avatars & beams")]
    pub avatar_size: u32,
    /// Avatar config file mapping "name or email = /path/to/image" per line.
    #[arg(long, value_name = "FILE", help_heading = "Avatars & beams")]
    pub avatar_config: Option<PathBuf>,
    /// Directory of avatar images named by committer email or name.
    #[arg(long, value_name = "DIR", help_heading = "Avatars & beams")]
    pub avatar_dir: Option<PathBuf>,
    /// Download gravatars (opt-in; needs network).
    #[arg(long, help_heading = "Avatars & beams")]
    pub gravatar: bool,
    /// Gravatar request timeout (ms).
    #[arg(long, default_value_t = 2500, help_heading = "Avatars & beams")]
    pub gravatar_timeout_ms: u64,
    /// Hide the beams from avatars to changed files.
    #[arg(long, help_heading = "Avatars & beams")]
    pub no_beams: bool,
    /// Beam glow intensity multiplier.
    #[arg(long, default_value_t = 1.0, help_heading = "Avatars & beams")]
    pub beam_intensity: f32,
    /// Minimum seconds a beam stays visible. Beams otherwise live exactly as
    /// long as their commit is on screen.
    #[arg(long, default_value_t = 0.25, help_heading = "Avatars & beams")]
    pub beam_seconds: f32,

    // ---------------- Animation ----------------
    /// Fraction of each commit's interval spent morphing geometry (smooth mode).
    #[arg(long, default_value_t = 0.55, help_heading = "Animation")]
    pub transition: f32,
    /// Seconds a changed tile stays highlighted.
    #[arg(long, default_value_t = 1.2, help_heading = "Animation")]
    pub highlight_seconds: f32,
    /// Seconds of video time an avatar lingers after its author's last commit.
    #[arg(long, default_value_t = 5.0, help_heading = "Animation")]
    pub avatar_idle_seconds: f32,

    // ---------------- HUD ----------------
    /// Hide the entire top meta bar.
    #[arg(long, help_heading = "HUD")]
    pub no_hud: bool,
    /// Hide the date.
    #[arg(long, help_heading = "HUD")]
    pub no_date: bool,
    /// Hide the author + subject line.
    #[arg(long, help_heading = "HUD")]
    pub no_author: bool,
    /// Hide the progress bar.
    #[arg(long, help_heading = "HUD")]
    pub no_progress: bool,
    /// Hide the language split.
    #[arg(long, help_heading = "HUD")]
    pub no_langs: bool,
    /// Title shown at the top-left of the meta bar.
    #[arg(long, help_heading = "HUD")]
    pub title: Option<String>,

    // ---------------- Misc ----------------
    /// Load options + committer labels/images from a config file (see gen-config).
    #[arg(long, value_name = "FILE", help_heading = "Misc")]
    pub config: Option<PathBuf>,
    /// Ignore the on-disk history cache.
    #[arg(long, help_heading = "Misc")]
    pub no_cache: bool,
    /// Print the resolved plan and exit without rendering.
    #[arg(long, help_heading = "Misc")]
    pub dry_run: bool,
}

fn parse_hex_color(s: &str) -> Result<[u8; 3]> {
    let s = s.trim_start_matches('#');
    // Guard byte-boundary slicing: reject anything that isn't 6 ASCII hex digits
    // (a 6-byte multibyte value would otherwise panic on the &s[0..2] slice).
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("color must be #RRGGBB (6 hex digits), got '{s}'");
    }
    Ok([
        u8::from_str_radix(&s[0..2], 16)?,
        u8::from_str_radix(&s[2..4], 16)?,
        u8::from_str_radix(&s[4..6], 16)?,
    ])
}

fn parse_resolution(s: &str) -> Result<(u32, u32)> {
    let parts: Vec<&str> = s.split(['x', 'X', '*']).collect();
    if parts.len() != 2 {
        bail!("resolution must be WxH, got '{s}'");
    }
    Ok((parts[0].trim().parse()?, parts[1].trim().parse()?))
}

impl RenderArgs {
    pub fn into_config(self) -> Result<Config> {
        let (width, height) = if let Some(r) = &self.resolution {
            parse_resolution(r)?
        } else {
            (self.width, self.height)
        };
        if width == 0 || height == 0 {
            bail!("width and height must be non-zero");
        }
        if width > 16384 || height > 16384 {
            bail!("width and height must be <= 16384");
        }
        if width % 2 != 0 || height % 2 != 0 {
            bail!("width and height must be even (yuv420p requirement)");
        }
        if self.fps == 0 {
            bail!("fps must be >= 1");
        }

        let codec = match self.codec {
            Some(CodecArg::Mp4) => Codec::Mp4,
            Some(CodecArg::Webm) => Codec::Webm,
            None => match self
                .out
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .as_deref()
            {
                Some("webm") => Codec::Webm,
                _ => Codec::Mp4,
            },
        };
        let quality = match self.quality {
            QualityArg::Draft => Quality::Draft,
            QualityArg::Balanced => Quality::Balanced,
            QualityArg::High => Quality::High,
        };

        Ok(Config {
            repo: self.repo,
            rev: self.rev,
            since: self.since,
            until: self.until,
            max_commits: self.max_commits,
            first_parent: !self.full_history,
            submodule_depth: if self.no_submodules {
                0
            } else {
                self.submodule_depth
            },
            submodule_include: self.include_submodule,
            submodule_exclude: self.exclude_submodule,
            include: self.include,
            exclude: self.exclude,

            out: self.out,
            codec,
            quality,
            width,
            height,
            fps: self.fps,
            seconds: Some(self.seconds),
            seconds_per_commit: self.seconds_per_commit,
            max_frames: self.max_frames,
            threads: self.threads,

            max_depth: self.max_depth,
            depth_mode: match self.depth_mode {
                DepthModeArg::Omit => DepthMode::Omit,
                DepthModeArg::Collapse => DepthMode::Collapse,
            },
            gamma: self.gamma,
            balance: self.balance,
            balance_rules: self
                .balance_rule
                .iter()
                .map(|r| parse_balance_rule(r))
                .collect::<Result<_>>()?,
            size_cap: self.size_cap,
            min_open_px: self.min_open_px,
            pad: self.pad,
            margin: self.margin,
            saturation: self.saturation,
            background: parse_hex_color(&self.background)?,
            color_roots: self.color_root,
            color_modules: self.color_module,
            auto_color: !self.no_auto_color,
            submodule_colors: !self.no_submodule_colors,

            show_minimap: !self.no_minimap,
            minimap_line_gap: self.minimap_line_gap,
            minimap_max_lines: self.minimap_max_lines,
            border: !self.no_border,
            border_width: self.border_width,

            show_dir_names: !self.no_dir_names,
            dir_name_max_depth: self.dir_name_max_depth,
            show_file_names: self.file_names,
            file_name_min_px: self.file_name_min_px,
            label_size: self.label_size,

            show_avatars: !self.no_avatars,
            show_avatar_names: !self.no_avatar_names,
            avatar_size: self.avatar_size,
            avatar_config: self.avatar_config,
            avatar_dir: self.avatar_dir,
            gravatar: self.gravatar,
            gravatar_timeout_ms: self.gravatar_timeout_ms,
            show_beams: !self.no_beams,
            beam_intensity: self.beam_intensity,
            beam_seconds: self.beam_seconds,

            transition: self.transition,
            highlight_seconds: self.highlight_seconds,
            avatar_idle_seconds: self.avatar_idle_seconds,

            show_hud: !self.no_hud,
            show_date: !self.no_date,
            show_author: !self.no_author,
            show_progress: !self.no_progress,
            show_langs: !self.no_langs,
            title: self.title,

            cache: !self.no_cache,
            baseline: !self.empty_start,
        })
    }
}

/// "GLOB=DELTA" (DELTA may carry a sign, e.g. "src/docs=+0.4").
fn parse_balance_rule(s: &str) -> Result<crate::config::BalanceRule> {
    let (glob, delta) = s
        .rsplit_once('=')
        .ok_or_else(|| anyhow::anyhow!("--balance-rule '{s}': expected GLOB=DELTA"))?;
    let balance: f32 = delta
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("--balance-rule '{s}': '{delta}' is not a number"))?;
    if glob.trim().is_empty() || !balance.is_finite() {
        bail!("--balance-rule '{s}': expected GLOB=DELTA");
    }
    Ok(crate::config::BalanceRule {
        folder: glob.trim().to_string(),
        balance,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn balance_rule_parses() {
        let r = super::parse_balance_rule("src/docs=+0.4").unwrap();
        assert_eq!((r.folder.as_str(), r.balance), ("src/docs", 0.4));
        let r = super::parse_balance_rule("a=b/**=-0.25").unwrap();
        assert_eq!((r.folder.as_str(), r.balance), ("a=b/**", -0.25));
        assert!(super::parse_balance_rule("src/docs").is_err());
        assert!(super::parse_balance_rule("src/docs=big").is_err());
    }

    use super::*;

    #[test]
    fn hex_color_ok() {
        assert_eq!(parse_hex_color("#0c0d10").unwrap(), [12, 13, 16]);
        assert_eq!(parse_hex_color("ffffff").unwrap(), [255, 255, 255]);
    }

    #[test]
    fn hex_color_rejects_bad_input() {
        assert!(parse_hex_color("#12345").is_err()); // too short
        assert!(parse_hex_color("#gggggg").is_err()); // non-hex
        assert!(parse_hex_color("#a\u{20ac}bc").is_err()); // 6 bytes, multibyte (must not panic)
    }

    #[test]
    fn resolution_cases() {
        assert_eq!(parse_resolution("1920x1080").unwrap(), (1920, 1080));
        assert_eq!(parse_resolution("640X480").unwrap(), (640, 480));
        assert!(parse_resolution("1920").is_err());
        assert!(parse_resolution("axb").is_err());
    }
}
