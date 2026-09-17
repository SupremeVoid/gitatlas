//! Committer avatars.
//!
//! Resolution order per committer: (1) an explicit avatar-config file mapping a
//! name or email to an image, (2) an avatar directory (files named by name/email),
//! (3) gravatar (opt-in), (4) one of a set of built-in "standard" pixel icons,
//! chosen deterministically per committer and tinted with that committer's own
//! unique color. Every committer also gets a stable, well-spread unique hue that
//! their avatar and beams share.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use tiny_skia::Pixmap;

use crate::color::{hash_str, hsl_to_rgb};
use crate::ingest::Author;

pub struct Sprite {
    pub size: u32,
    /// Premultiplied RGBA8, `size*size*4` bytes.
    pub rgba: Vec<u8>,
}

pub struct AvatarSet {
    pub sprites: Vec<Sprite>,
    /// Unique-ish hue per author (shared by avatar tint and its beams).
    pub hues: Vec<f32>,
    /// Display name per author (for the flowing name label).
    pub names: Vec<String>,
}

pub struct AvatarOptions<'a> {
    pub size: u32,
    /// key (lowercased email or name) -> image path (from config file / --avatar-config).
    pub images: &'a HashMap<String, PathBuf>,
    /// key (lowercased email or name) -> display label override.
    pub labels: &'a HashMap<String, String>,
    pub dir: Option<&'a Path>,
    pub gravatar: bool,
    pub gravatar_timeout_ms: u64,
}

/// Built-in "standard" icons (>=10), 7x7 pixel patterns. Each is tinted with the
/// committer's unique color, so the same shape reads differently per person.
const ICONS: &[[&str; 7]] = &[
    // invader
    [
        "#.....#", "..#.#..", ".#####.", "##.#.##", "#######", "#.#.#.#", ".#...#.",
    ],
    // robot
    [
        ".#####.", "#.#.#.#", "#######", "#.###.#", "#######", ".#...#.", ".#...#.",
    ],
    // cat
    [
        "#.....#", "##...##", "#######", "#.#.#.#", "#######", ".#####.", "..#.#..",
    ],
    // ghost
    [
        ".#####.", "#######", "#.#.#.#", "#######", "#######", "#.#.#.#", "#.#.#.#",
    ],
    // smiley
    [
        ".#####.", "#######", "#.#.#.#", "#######", "#.###.#", "##...##", ".#####.",
    ],
    // heart
    [
        ".#.#.#.", "#######", "#######", "#######", ".#####.", "..###..", "...#...",
    ],
    // star
    [
        "...#...", "...#...", "#######", ".#####.", "..###..", ".##.##.", "##...##",
    ],
    // diamond
    [
        "...#...", "..###..", ".#####.", "#######", ".#####.", "..###..", "...#...",
    ],
    // skull
    [
        ".#####.", "#######", "#.#.#.#", "#######", ".#.#.#.", ".#####.", ".#.#.#.",
    ],
    // flower
    [
        "..#.#..", ".#####.", "##.#.##", "#######", "##.#.##", ".#####.", "..#.#..",
    ],
    // hexagon
    [
        "..###..", ".#####.", "#######", "#######", "#######", ".#####.", "..###..",
    ],
    // bolt
    [
        "...##..", "..##...", ".####..", "..####.", "...##..", "..##...", ".##....",
    ],
    // crab
    [
        "#.....#", "#.###.#", "#######", ".#####.", "#######", "#.#.#.#", "#.....#",
    ],
    // owl
    [
        "#.#.#.#", "#######", "#.#.#.#", "#######", ".#####.", "..###..", ".#...#.",
    ],
];

impl AvatarSet {
    pub fn build(authors: &[Author], opts: &AvatarOptions) -> Self {
        let size = opts.size.max(8);
        let dir_index = opts.dir.map(build_dir_index).unwrap_or_default();
        let mut gravatar_cache: HashMap<String, Option<Vec<u8>>> = HashMap::new();

        let mut sprites = Vec::with_capacity(authors.len());
        let mut hues = Vec::with_capacity(authors.len());
        let mut names = Vec::with_capacity(authors.len());

        for (i, a) in authors.iter().enumerate() {
            let email = a.email.trim().to_lowercase();
            let name_key = a.name.trim().to_lowercase();
            // Unique, well-spread hue via the golden angle over the author index.
            let hue = (i as f32 * 137.507_76).rem_euclid(360.0);
            let default_name = if a.name.trim().is_empty() {
                a.email.clone()
            } else {
                a.name.clone()
            };
            // Display label: config override (by email then name), else the name.
            let display = opts
                .labels
                .get(&email)
                .or_else(|| opts.labels.get(&name_key))
                .cloned()
                .unwrap_or(default_name);

            // 1) explicit image map, then 2) directory, matched by email then name.
            let mut sprite = opts
                .images
                .get(&email)
                .or_else(|| opts.images.get(&name_key))
                .or_else(|| dir_index.get(&email))
                .or_else(|| dir_index.get(&name_key))
                .and_then(|p| load_image_sprite(p, size));

            // 3) gravatar (opt-in, cached).
            if sprite.is_none() && opts.gravatar && !email.is_empty() {
                let bytes = gravatar_cache
                    .entry(email.clone())
                    .or_insert_with(|| fetch_gravatar(&email, size, opts.gravatar_timeout_ms));
                if let Some(b) = bytes {
                    sprite = decode_sprite(b, size);
                }
            }

            // 4) standard icon tinted with the unique color.
            let sprite = sprite.unwrap_or_else(|| {
                let seed = hash_str(if email.is_empty() { &name_key } else { &email });
                let icon = &ICONS[(seed % ICONS.len() as u64) as usize];
                icon_sprite(icon, size, hue)
            });

            sprites.push(sprite);
            hues.push(hue);
            names.push(display);
        }

        AvatarSet {
            sprites,
            hues,
            names,
        }
    }
}

/// Render a 7x7 icon pattern into a circular, premultiplied sprite tinted by hue.
fn icon_sprite(icon: &[&str; 7], size: u32, hue: f32) -> Sprite {
    let fg = hsl_to_rgb(hue, 0.75, 0.64);
    let bg = hsl_to_rgb(hue, 0.5, 0.17);
    let s = size as f32;
    let pad = s * 0.13;
    let cell = (s - 2.0 * pad) / 7.0;
    let center = (s - 1.0) / 2.0;
    let radius = s / 2.0;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let fx = x as f32;
            let fy = y as f32;
            let d = ((fx - center).powi(2) + (fy - center).powi(2)).sqrt();
            let mask = ((radius - d) / 1.5).clamp(0.0, 1.0);
            if mask <= 0.0 {
                continue;
            }
            let gx = ((fx - pad) / cell).floor();
            let gy = ((fy - pad) / cell).floor();
            let on = (0.0..7.0).contains(&gx)
                && (0.0..7.0).contains(&gy)
                && icon[gy as usize].as_bytes()[gx as usize] == b'#';
            let col = if on { fg } else { bg };
            let a = mask;
            let idx = ((y * size + x) * 4) as usize;
            rgba[idx] = (col[0] as f32 * a) as u8;
            rgba[idx + 1] = (col[1] as f32 * a) as u8;
            rgba[idx + 2] = (col[2] as f32 * a) as u8;
            rgba[idx + 3] = (a * 255.0) as u8;
        }
    }
    Sprite { size, rgba }
}

pub fn parse_avatar_config(path: &Path) -> HashMap<String, PathBuf> {
    let mut idx = HashMap::new();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return idx,
    };
    let base = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // key = value  (key may contain spaces; split on the first '=').
        let (key, val) = match line.split_once('=') {
            Some((k, v)) => (k, v),
            None => match line.split_once('\t') {
                Some((k, v)) => (k, v),
                None => continue,
            },
        };
        let key = key.trim().trim_matches('"').to_lowercase();
        let val = val.trim().trim_matches('"');
        if key.is_empty() || val.is_empty() {
            continue;
        }
        let mut p = PathBuf::from(val);
        if p.is_relative() {
            p = base.join(&p);
        }
        idx.insert(key, p);
    }
    idx
}

fn build_dir_index(dir: &Path) -> HashMap<String, PathBuf> {
    let mut idx = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            let ext_ok = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| matches!(e.to_lowercase().as_str(), "png" | "jpg" | "jpeg"))
                .unwrap_or(false);
            if !ext_ok {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                idx.insert(stem.trim().to_lowercase(), path.clone());
            }
        }
    }
    idx
}

fn load_image_sprite(path: &Path, size: u32) -> Option<Sprite> {
    let bytes = std::fs::read(path).ok()?;
    decode_sprite(&bytes, size)
}

fn decode_sprite(bytes: &[u8], size: u32) -> Option<Sprite> {
    let img = image::load_from_memory(bytes).ok()?;
    let scaled = img
        .resize_to_fill(size, size, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let center = (size as f32 - 1.0) / 2.0;
    let radius = size as f32 / 2.0;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - center).powi(2) + (y as f32 - center).powi(2)).sqrt();
            let mask = ((radius - d) / 1.5).clamp(0.0, 1.0);
            let p = scaled.get_pixel(x, y).0;
            let a = mask * (p[3] as f32 / 255.0);
            let i = ((y * size + x) * 4) as usize;
            rgba[i] = (p[0] as f32 * a) as u8;
            rgba[i + 1] = (p[1] as f32 * a) as u8;
            rgba[i + 2] = (p[2] as f32 * a) as u8;
            rgba[i + 3] = (a * 255.0) as u8;
        }
    }
    Some(Sprite { size, rgba })
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn fetch_gravatar(email: &str, size: u32, timeout_ms: u64) -> Option<Vec<u8>> {
    // Gravatar keys on the md5 of the trimmed, lowercased email.
    let hash = hex(&Md5::digest(email.trim().to_ascii_lowercase().as_bytes()));
    let url = format!("https://www.gravatar.com/avatar/{hash}?s={size}&d=404");
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(timeout_ms)))
        .build()
        .new_agent();
    let mut resp = agent.get(&url).call().ok()?;
    if resp.status() != 200 {
        return None;
    }
    resp.body_mut().read_to_vec().ok()
}

/// Alpha-over a premultiplied sprite centered at (cx, cy) with a global alpha.
pub fn blit_sprite(pm: &mut Pixmap, sprite: &Sprite, cx: f32, cy: f32, alpha: f32) {
    let ga = alpha.clamp(0.0, 1.0);
    if ga <= 0.0 {
        return;
    }
    let pw = pm.width() as i32;
    let ph = pm.height() as i32;
    let s = sprite.size as i32;
    let x0 = (cx - sprite.size as f32 / 2.0).round() as i32;
    let y0 = (cy - sprite.size as f32 / 2.0).round() as i32;
    let data = pm.data_mut();
    let gai = (ga * 255.0) as u32;
    for sy in 0..s {
        let dy = y0 + sy;
        if dy < 0 || dy >= ph {
            continue;
        }
        for sx in 0..s {
            let dx = x0 + sx;
            if dx < 0 || dx >= pw {
                continue;
            }
            let si = ((sy * s + sx) * 4) as usize;
            let sa = sprite.rgba[si + 3] as u32;
            if sa == 0 {
                continue;
            }
            let sr = sprite.rgba[si] as u32 * gai / 255;
            let sg = sprite.rgba[si + 1] as u32 * gai / 255;
            let sb = sprite.rgba[si + 2] as u32 * gai / 255;
            let sa = sa * gai / 255;
            let inv = 255 - sa;
            let di = ((dy * pw + dx) * 4) as usize;
            data[di] = (sr + data[di] as u32 * inv / 255) as u8;
            data[di + 1] = (sg + data[di + 1] as u32 * inv / 255) as u8;
            data[di + 2] = (sb + data[di + 2] as u32 * inv / 255) as u8;
            data[di + 3] = (sa + data[di + 3] as u32 * inv / 255) as u8;
        }
    }
}
