//! Deterministic color derivation: stable hues from folder names, HSL->RGB, and
//! the tile/border/text color scheme.

/// Deterministic FNV-1a 64-bit hash of a string (stable across runs & platforms).
#[inline]
pub fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Map a hash to a hue in [0,360), avoiding a fully uniform look by using the
/// golden-ratio conjugate so nearby hashes land far apart on the wheel.
#[inline]
pub fn hue_from_hash(h: u64) -> f32 {
    // Use the high bits scaled by the golden ratio conjugate for good spread.
    let frac = (h >> 11) as f64 * (1.0 / (1u64 << 53) as f64);
    let golden = 0.618_033_988_749_895_f64;
    (((frac + golden) % 1.0) * 360.0) as f32
}

/// HSL (h in degrees, s/l in [0,1]) to 8-bit sRGB.
pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [
        (((r1 + m) * 255.0).round()).clamp(0.0, 255.0) as u8,
        (((g1 + m) * 255.0).round()).clamp(0.0, 255.0) as u8,
        (((b1 + m) * 255.0).round()).clamp(0.0, 255.0) as u8,
    ]
}

/// The set of colors derived for one "root group" (top-level folder), matching
/// the reference atlas look: a saturated border and a dark, slightly-tinted fill.
#[derive(Clone, Copy, Debug)]
pub struct GroupColor {
    pub hue: f32,
    pub border: [u8; 3],
    pub fill: [u8; 3],
    pub fill_lit: [u8; 3],
    pub minimap: [u8; 3],
    /// Modern style: folder fill per depth (0..=3+), lighter as they nest.
    pub m_dir: [[u8; 3]; 4],
    /// Modern style: folder edge per depth tier (module / sub-module / deep).
    pub m_edge: [[u8; 3]; 3],
    /// Modern style: file card fill, its top highlight, collapsed-folder fill.
    pub m_file: [u8; 3],
    pub m_file_top: [u8; 3],
    pub m_collapsed: [u8; 3],
    /// Modern style: minimap "code" tones — plain, keyword accent, comment.
    pub m_code: [[u8; 3]; 3],
    /// Modern style: folder label colour per depth tier.
    pub m_label: [[u8; 3]; 3],
}

impl GroupColor {
    pub fn from_hue(hue: f32, saturation: f32) -> Self {
        let border = hsl_to_rgb(hue, saturation, 0.55);
        // Dark tinted fill so borders read as the grouping cue, like the atlas image.
        let fill = hsl_to_rgb(hue, saturation * 0.55, 0.09);
        let fill_lit = hsl_to_rgb(hue, saturation * 0.6, 0.16);
        let minimap = hsl_to_rgb(hue, saturation * 0.35, 0.42);
        let s = saturation;
        GroupColor {
            hue,
            border,
            fill,
            fill_lit,
            minimap,
            m_dir: [
                hsl_to_rgb(hue, s * 0.34, 0.05),
                hsl_to_rgb(hue, s * 0.32, 0.062),
                hsl_to_rgb(hue, s * 0.30, 0.074),
                hsl_to_rgb(hue, s * 0.28, 0.084),
            ],
            m_edge: [
                hsl_to_rgb(hue, s * 1.05, 0.62),
                hsl_to_rgb(hue, s * 0.85, 0.46),
                hsl_to_rgb(hue, s * 0.6, 0.30),
            ],
            m_file: hsl_to_rgb(hue, s * 0.42, 0.15),
            m_file_top: hsl_to_rgb(hue, s * 0.6, 0.30),
            m_collapsed: hsl_to_rgb(hue, s * 0.45, 0.175),
            m_code: [
                hsl_to_rgb(hue, s * 0.40, 0.47),
                hsl_to_rgb(hue + 28.0, s * 0.95, 0.66),
                hsl_to_rgb(hue, s * 0.18, 0.33),
            ],
            m_label: [
                mix(hsl_to_rgb(hue, s, 0.72), [255, 255, 255], 0.55),
                mix(hsl_to_rgb(hue, s, 0.68), [255, 255, 255], 0.35),
                hsl_to_rgb(hue, s * 0.6, 0.62),
            ],
        }
    }
}

#[inline]
pub fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t) as u8,
    ]
}
