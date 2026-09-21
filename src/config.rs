//! Resolved runtime configuration (populated from CLI/TOML). Every visual aspect
//! is toggleable/tunable here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Mp4,
    Webm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Draft,
    Balanced,
    High,
}

/// What to do with files/folders deeper than `max_depth`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DepthMode {
    /// Drop anything beyond the depth entirely.
    Omit,
    /// Merge each too-deep subfolder into a single aggregate tile at the cutoff.
    Collapse,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    // ---- input ----
    pub repo: PathBuf,
    pub rev: String,
    pub since: Option<String>,
    pub until: Option<String>,
    pub max_commits: usize,
    pub first_parent: bool,
    pub submodules: bool,
    pub submodule_include: Vec<String>,
    pub submodule_exclude: Vec<String>,

    // ---- files & folders (path globs against the repo-relative path) ----
    pub include: Vec<String>,
    pub exclude: Vec<String>,

    // ---- output / timing ----
    pub out: PathBuf,
    pub codec: Codec,
    pub quality: Quality,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Target video length in seconds (mutually exclusive-ish with spc).
    pub seconds: Option<f64>,
    /// Seconds per commit (overrides `seconds` when set).
    pub seconds_per_commit: Option<f64>,
    /// Minimum/maximum total frames guardrails.
    pub max_frames: u64,
    pub threads: usize,

    // ---- layout ----
    pub max_depth: u32,
    pub depth_mode: DepthMode,
    pub gamma: f32,
    /// Folder balancing 0..0.9: compresses how area is split among sibling
    /// folders so small modules stay visible (0 = strictly proportional).
    pub balance: f32,
    pub size_cap: Option<f32>, // None => auto from final state percentile
    pub min_open_px: f32,
    pub pad: f32,
    pub margin: f32,
    pub saturation: f32,
    pub background: [u8; 3],

    // ---- color groups ----
    /// Folders whose direct subfolders each get their own color.
    pub color_roots: Vec<String>,
    /// Folders that are one color module of their own.
    pub color_modules: Vec<String>,
    /// Auto-descend through a dominant folder chain (e.g. src/app) to find the
    /// level where the tree actually splits, and color by that level.
    pub auto_color: bool,

    // ---- tiles / minimap ----
    pub show_minimap: bool,
    pub minimap_line_gap: f32,
    pub minimap_max_lines: u32,
    pub border: bool,
    pub border_width: f32,

    // ---- labels ----
    pub show_dir_names: bool,
    pub dir_name_max_depth: u32,
    pub show_file_names: bool,
    pub file_name_min_px: f32,
    pub label_size: f32,

    // ---- avatars / beams ----
    pub show_avatars: bool,
    pub show_avatar_names: bool,
    pub avatar_size: u32,
    pub avatar_config: Option<PathBuf>,
    pub avatar_dir: Option<PathBuf>,
    pub gravatar: bool,
    pub gravatar_timeout_ms: u64,
    pub show_beams: bool,
    pub beam_intensity: f32,
    /// Minimum beam visibility in seconds; beams otherwise live exactly as long
    /// as their commit is on screen.
    pub beam_seconds: f32,

    // ---- animation ----
    /// Fraction of a commit's on-screen time spent in the grow/shrink transition.
    pub transition: f32,
    /// How long (in seconds) a commit's highlight/avatar lingers.
    pub highlight_seconds: f32,
    pub avatar_idle_seconds: f32,

    // ---- HUD ----
    pub show_hud: bool,
    pub show_date: bool,
    pub show_author: bool,
    pub show_progress: bool,
    pub show_langs: bool,
    pub title: Option<String>,

    // ---- misc ----
    pub cache: bool,
    /// Start a windowed walk (--since, --max-commits, …) from the repository as
    /// it already was, instead of from an empty map.
    pub baseline: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            repo: PathBuf::from("."),
            rev: "HEAD".into(),
            since: None,
            until: None,
            max_commits: 0,
            first_parent: true,
            submodules: true,
            submodule_include: Vec::new(),
            submodule_exclude: Vec::new(),
            include: Vec::new(),
            exclude: Vec::new(),

            out: PathBuf::from("output/atlas.mp4"),
            codec: Codec::Mp4,
            quality: Quality::Balanced,
            width: 2560,
            height: 1440,
            fps: 30,
            seconds: Some(60.0),
            seconds_per_commit: None,
            max_frames: 0,
            threads: 0,

            max_depth: 0,
            depth_mode: DepthMode::Omit,
            gamma: 0.5,
            balance: 0.25,
            size_cap: None,
            min_open_px: 34.0,
            pad: 3.0,
            margin: 8.0,
            saturation: 0.72,
            background: [12, 13, 16],
            color_roots: Vec::new(),
            color_modules: Vec::new(),
            auto_color: true,

            show_minimap: true,
            minimap_line_gap: 3.0,
            minimap_max_lines: 240,
            border: true,
            border_width: 1.0,

            show_dir_names: true,
            dir_name_max_depth: 0,
            show_file_names: false,
            file_name_min_px: 42.0,
            label_size: 13.0,

            show_avatars: true,
            show_avatar_names: true,
            avatar_size: 72,
            avatar_config: None,
            avatar_dir: None,
            gravatar: false,
            gravatar_timeout_ms: 2500,
            show_beams: true,
            beam_intensity: 1.0,
            beam_seconds: 0.25,

            transition: 0.55,
            highlight_seconds: 1.2,
            avatar_idle_seconds: 5.0,

            show_hud: true,
            show_date: true,
            show_author: true,
            show_progress: true,
            show_langs: true,
            title: None,

            cache: true,
            baseline: true,
        }
    }
}

impl Config {
    /// The git-walk options implied by this config.
    /// Deepest 0-based folder depth that is labeled (`dir_name_max_depth` counts
    /// levels, 0 = all).
    pub fn dir_label_depth(&self) -> u32 {
        match self.dir_name_max_depth {
            0 => u32::MAX,
            n => n - 1,
        }
    }

    /// Minimum height of an open folder for it to get a header label.
    pub fn label_min_px(&self) -> f32 {
        self.label_size + 30.0
    }

    pub fn ingest_options(&self) -> crate::ingest::IngestOptions {
        crate::ingest::IngestOptions {
            first_parent: self.first_parent,
            since: self.since.clone(),
            until: self.until.clone(),
            max_commits: self.max_commits,
            rev: self.rev.clone(),
            submodules: self.submodules,
            submodule_include: self.submodule_include.clone(),
            submodule_exclude: self.submodule_exclude.clone(),
        }
    }

    /// Compute the total number of output frames from the timing options.
    pub fn total_frames(&self, num_commits: usize) -> u64 {
        let n = num_commits.max(1) as f64;
        let frames = if let Some(spc) = self.seconds_per_commit {
            spc * n * self.fps as f64
        } else {
            self.seconds.unwrap_or(60.0) * self.fps as f64
        };
        let mut f = frames.round() as u64;
        if f < 2 {
            f = 2;
        }
        if self.max_frames > 0 && f > self.max_frames {
            f = self.max_frames;
        }
        f
    }
}
