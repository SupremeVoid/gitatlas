//! Small geometry helpers shared across layout and rendering.

/// An axis-aligned rectangle in floating-point pixel space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    #[inline]
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }

    #[inline]
    pub fn shorter(&self) -> f32 {
        self.w.min(self.h)
    }

    #[inline]
    pub fn cx(&self) -> f32 {
        self.x + self.w * 0.5
    }

    #[inline]
    pub fn cy(&self) -> f32 {
        self.y + self.h * 0.5
    }

    /// Shrink the rectangle inward by `pad` on every side, clamped to non-negative size.
    #[inline]
    pub fn inset(&self, pad: f32) -> Rect {
        let w = (self.w - 2.0 * pad).max(0.0);
        let h = (self.h - 2.0 * pad).max(0.0);
        Rect::new(self.x + pad, self.y + pad, w, h)
    }

    /// Linearly interpolate two rectangles by center + size (stable morphing).
    #[inline]
    pub fn lerp(&self, other: &Rect, t: f32) -> Rect {
        let cx = lerp(self.cx(), other.cx(), t);
        let cy = lerp(self.cy(), other.cy(), t);
        let w = lerp(self.w, other.w, t);
        let h = lerp(self.h, other.h, t);
        Rect::new(cx - w * 0.5, cy - h * 0.5, w, h)
    }

    /// Uniformly scale about the rectangle center by factor `s`.
    #[inline]
    pub fn scaled_about_center(&self, s: f32) -> Rect {
        let w = self.w * s;
        let h = self.h * s;
        Rect::new(self.cx() - w * 0.5, self.cy() - h * 0.5, w, h)
    }

    #[inline]
    pub fn intersects_viewport(&self, vw: f32, vh: f32) -> bool {
        self.x < vw && self.y < vh && self.x + self.w > 0.0 && self.y + self.h > 0.0
    }
}

#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
pub fn clamp01(t: f32) -> f32 {
    t.clamp(0.0, 1.0)
}
