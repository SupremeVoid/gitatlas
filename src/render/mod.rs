//! Rendering: shared plan types plus the per-frame rasterizer.

pub mod avatar;
pub mod frame;
pub mod text;

use std::sync::Arc;

use crate::ingest::ChangeKind;
use crate::layout::Layout;
use crate::model::Tree;

/// A fully-built repository state at commit position `k`: the folder tree and its
/// treemap layout. Shared (Arc) across the frames of a window.
pub struct Keyframe {
    pub k: usize,
    pub tree: Tree,
    pub layout: Layout,
    pub files: usize,
    pub lines: u64,
    /// Top languages at this state: (lang id into `lang::LANGS`, lines, files).
    pub langs: Vec<(u16, u64, u32)>,
}

/// A change highlight applied to a drawn tile this frame.
#[derive(Clone, Copy)]
pub struct ChangeGlow {
    pub intensity: f32,
    pub kind: ChangeKind,
}

#[derive(Clone, Copy)]
pub struct Beam {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub hue: f32,
    pub intensity: f32,
}

#[derive(Clone, Copy)]
pub struct AvatarDraw {
    pub author: u32,
    pub x: f32,
    pub y: f32,
    pub alpha: f32,
}

#[derive(Clone, Default)]
pub struct Hud {
    pub date: String,
    pub author: String,
    pub subject: String,
    pub commit_hash: String,
    pub commit_idx: usize,
    pub total_commits: usize,
    pub files: usize,
    pub lines: u64,
    pub progress: f32,
    /// Top languages: (lang id, lines, files).
    pub langs: Vec<(u16, u64, u32)>,
}

/// Everything needed to render one frame, self-contained (Arc keyframes) so it
/// can be rendered on any worker thread.
pub struct FramePlan {
    pub frame_index: u64,
    pub lo: Arc<Keyframe>,
    pub hi: Arc<Keyframe>,
    /// Geometric transition fraction in [0,1] between lo and hi (already eased).
    pub frac: f32,
    pub changed: rustc_hash::FxHashMap<u64, ChangeGlow>,
    pub beams: Vec<Beam>,
    pub avatars: Vec<AvatarDraw>,
    pub hud: Hud,
}
