//! Text rasterization: a pre-warmed, immutable glyph coverage cache (fontdue)
//! blitted directly onto the premultiplied RGBA pixmap. One cache per pixel size.

use std::collections::HashMap;
use std::sync::Arc;

use fontdue::{Font, FontSettings};
use tiny_skia::Pixmap;

pub struct Glyph {
    pub cov: Vec<u8>,
    pub w: usize,
    pub h: usize,
    pub xmin: i32,
    pub ymin: i32,
    pub adv: f32,
}

pub struct GlyphCache {
    px: f32,
    glyphs: HashMap<char, Arc<Glyph>>,
    pub line_height: f32,
    pub space_adv: f32,
}

impl GlyphCache {
    /// Pre-warm the cache for printable ASCII plus any `extra` chars (author names).
    pub fn warm(font: &Font, px: f32, extra: impl Iterator<Item = char>) -> Self {
        let mut glyphs = HashMap::new();
        let chars = (0x20u8..=0x7e).map(|b| b as char).chain(extra);
        let mut space_adv = px * 0.5;
        for c in chars {
            if glyphs.contains_key(&c) {
                continue;
            }
            let (m, cov) = font.rasterize(c, px);
            if c == ' ' {
                space_adv = m.advance_width;
            }
            glyphs.insert(
                c,
                Arc::new(Glyph {
                    cov,
                    w: m.width,
                    h: m.height,
                    xmin: m.xmin,
                    ymin: m.ymin,
                    adv: m.advance_width,
                }),
            );
        }
        // line metrics from font
        let lm = font.horizontal_line_metrics(px).map(|l| l.new_line_size);
        GlyphCache {
            px,
            glyphs,
            line_height: lm.unwrap_or(px * 1.3),
            space_adv,
        }
    }

    #[inline]
    pub fn px(&self) -> f32 {
        self.px
    }

    pub fn glyph(&self, c: char) -> Option<&Arc<Glyph>> {
        self.glyphs.get(&c)
    }

    /// Total advance width of `text` (for centering / fit checks).
    pub fn measure(&self, text: &str) -> f32 {
        let mut w = 0.0;
        for c in text.chars() {
            w += self.glyphs.get(&c).map(|g| g.adv).unwrap_or(self.space_adv);
        }
        w
    }

    /// Draw `text` with its baseline at `baseline_y`, pen starting at `pen_x`,
    /// clipped so it does not exceed `max_w` pixels. Alpha-over onto premult RGBA.
    /// Returns the advanced pen x.
    pub fn draw(
        &self,
        pm: &mut Pixmap,
        text: &str,
        mut pen_x: f32,
        baseline_y: f32,
        color: [u8; 3],
        alpha: f32,
        max_w: f32,
    ) -> f32 {
        let pw = pm.width() as i32;
        let ph = pm.height() as i32;
        let data = pm.data_mut();
        let ca = (alpha.clamp(0.0, 1.0) * 255.0) as u32;
        let (cr, cg, cb) = (color[0] as u32, color[1] as u32, color[2] as u32);
        let start = pen_x;
        for ch in text.chars() {
            let g = match self.glyphs.get(&ch) {
                Some(g) => g,
                None => {
                    pen_x += self.space_adv;
                    continue;
                }
            };
            if pen_x - start + g.adv > max_w {
                break;
            }
            if g.w == 0 || g.h == 0 {
                pen_x += g.adv;
                continue;
            }
            let gx0 = (pen_x + g.xmin as f32).round() as i32;
            let gy0 = (baseline_y - g.ymin as f32 - g.h as f32).round() as i32;
            let x0 = gx0.max(0);
            let y0 = gy0.max(0);
            let x1 = (gx0 + g.w as i32).min(pw);
            let y1 = (gy0 + g.h as i32).min(ph);
            for y in y0..y1 {
                let grow = (y - gy0) as usize * g.w;
                let mut di = ((y * pw + x0) * 4) as usize;
                let mut sx = (x0 - gx0) as usize;
                for _ in x0..x1 {
                    let cov = g.cov[grow + sx] as u32;
                    if cov != 0 {
                        let a = cov * ca / 255;
                        if a != 0 {
                            let inv = 255 - a;
                            data[di] = ((cr * a + data[di] as u32 * inv + 127) / 255) as u8;
                            data[di + 1] = ((cg * a + data[di + 1] as u32 * inv + 127) / 255) as u8;
                            data[di + 2] = ((cb * a + data[di + 2] as u32 * inv + 127) / 255) as u8;
                            data[di + 3] = (a + data[di + 3] as u32 * inv / 255) as u8;
                        }
                    }
                    di += 4;
                    sx += 1;
                }
            }
            pen_x += g.adv;
        }
        pen_x
    }
}

/// Load the embedded font.
pub fn load_font() -> Font {
    let bytes = include_bytes!("../../assets/RobotoMono.ttf");
    Font::from_bytes(bytes as &[u8], FontSettings::default()).expect("embedded font is valid")
}
