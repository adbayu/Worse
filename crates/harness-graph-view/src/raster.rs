//! The pixels: a frame, and the few shapes the pictures are made of.
//!
//! A frame is RGBA with premultiplied alpha, four bytes a pixel, rows from the top. Positions are in pixels
//! with fractions: `(0.5, 0.5)` is the middle of the first pixel. Every shape is anti-aliased by coverage, so
//! something that moves a fraction of a pixel a frame glides instead of hopping.

use crate::math::falloff;

/// How a shape meets what is already in the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blend {
    /// Light on a dark, opaque frame: colours add up and clip at white.
    Add,
    /// Paint over whatever is there, see-through where the shape is; the frame may be transparent.
    Over,
}

/// A colour, each part in `0.0..=1.0`.
pub type Rgb = [f32; 3];

pub struct Frame<'a> {
    width: usize,
    height: usize,
    data: &'a mut [u8],
    blend: Blend,
}

fn add(to: u8, plus: f32) -> u8 {
    (u32::from(to) + (plus + 0.5) as u32).min(255) as u8
}

impl<'a> Frame<'a> {
    /// `data` holds `width * height * 4` bytes; a shorter slice draws nothing (the frame gets no height).
    pub fn new(data: &'a mut [u8], width: usize, height: usize, blend: Blend) -> Self {
        let height = if width == 0 {
            0
        } else {
            height.min(data.len() / (width * 4))
        };
        Self {
            width,
            height,
            data,
            blend,
        }
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    pub fn set_blend(&mut self, blend: Blend) {
        self.blend = blend;
    }

    /// The bytes, for a pass that shades pixel by pixel.
    pub fn bytes(&mut self) -> &mut [u8] {
        self.data
    }

    /// Fills the frame with one colour (premultiplied by `alpha`).
    pub fn clear(&mut self, rgb: Rgb, alpha: f32) {
        let px = [
            (rgb[0] * alpha * 255.0 + 0.5) as u8,
            (rgb[1] * alpha * 255.0 + 0.5) as u8,
            (rgb[2] * alpha * 255.0 + 0.5) as u8,
            (alpha * 255.0 + 0.5) as u8,
        ];
        for chunk in self.data.chunks_exact_mut(4) {
            chunk.copy_from_slice(&px);
        }
    }

    fn put(&mut self, x: usize, y: usize, rgb: Rgb, amount: f32) {
        if amount <= 0.003 {
            return;
        }
        let i = (y * self.width + x) * 4;
        let px = &mut self.data[i..i + 4];
        match self.blend {
            Blend::Add => {
                let k = amount * 255.0;
                px[0] = add(px[0], rgb[0] * k);
                px[1] = add(px[1], rgb[1] * k);
                px[2] = add(px[2], rgb[2] * k);
            }
            Blend::Over => {
                let a = amount.min(1.0);
                let keep = 1.0 - a;
                let k = a * 255.0;
                px[0] = (rgb[0] * k + f32::from(px[0]) * keep + 0.5) as u8;
                px[1] = (rgb[1] * k + f32::from(px[1]) * keep + 0.5) as u8;
                px[2] = (rgb[2] * k + f32::from(px[2]) * keep + 0.5) as u8;
                px[3] = (k + f32::from(px[3]) * keep + 0.5) as u8;
            }
        }
    }

    /// The pixel rows and columns a box touches, or `None` when it misses the frame.
    fn span(
        &self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    ) -> Option<(usize, usize, usize, usize)> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        if !(left.is_finite() && top.is_finite() && right.is_finite() && bottom.is_finite()) {
            return None;
        }
        if right < 0.0 || bottom < 0.0 || left >= self.width as f32 || top >= self.height as f32 {
            return None;
        }
        let x0 = left.floor().max(0.0) as usize;
        let y0 = top.floor().max(0.0) as usize;
        let x1 = (right.floor().max(0.0) as usize).min(self.width - 1);
        let y1 = (bottom.floor().max(0.0) as usize).min(self.height - 1);
        Some((x0, y0, x1, y1))
    }

    /// A soft round light: `gain` at the middle, fading as a bell curve `sigma` pixels wide.
    pub fn glow(&mut self, x: f32, y: f32, sigma: f32, rgb: Rgb, gain: f32) {
        if gain <= 0.003 {
            return;
        }
        let sigma = sigma.max(0.45);
        let reach = sigma * 3.2 + 0.5;
        let Some((x0, y0, x1, y1)) = self.span(x - reach, y - reach, x + reach, y + reach) else {
            return;
        };
        let inv = 0.5 / (sigma * sigma);
        for py in y0..=y1 {
            let dy = py as f32 + 0.5 - y;
            for px in x0..=x1 {
                let dx = px as f32 + 0.5 - x;
                self.put(px, py, rgb, gain * falloff((dx * dx + dy * dy) * inv));
            }
        }
    }

    /// A point one pixel wide, shared between the four pixels it lies on.
    pub fn dot(&mut self, x: f32, y: f32, rgb: Rgb, gain: f32) {
        let fx = (x - 0.5).floor();
        let fy = (y - 0.5).floor();
        let tx = x - 0.5 - fx;
        let ty = y - 0.5 - fy;
        for (ox, oy, share) in [
            (0.0, 0.0, (1.0 - tx) * (1.0 - ty)),
            (1.0, 0.0, tx * (1.0 - ty)),
            (0.0, 1.0, (1.0 - tx) * ty),
            (1.0, 1.0, tx * ty),
        ] {
            let px = fx + ox;
            let py = fy + oy;
            if px >= 0.0 && py >= 0.0 && (px as usize) < self.width && (py as usize) < self.height {
                self.put(px as usize, py as usize, rgb, gain * share);
            }
        }
    }

    /// A straight line `width` pixels thick whose strength runs from `gain0` at the start to `gain1` at the end.
    #[allow(clippy::too_many_arguments)]
    pub fn line(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        width: f32,
        rgb: Rgb,
        gain0: f32,
        gain1: f32,
    ) {
        if gain0 <= 0.003 && gain1 <= 0.003 {
            return;
        }
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = dx.hypot(dy);
        if !len.is_finite() || len < 1e-3 {
            return;
        }
        let half = width.max(0.2) * 0.5;
        if self
            .span(
                x0.min(x1) - half - 1.0,
                y0.min(y1) - half - 1.0,
                x0.max(x1) + half + 1.0,
                y0.max(y1) + half + 1.0,
            )
            .is_none()
        {
            return;
        }
        // Walk the longer axis (`u`) and fill the few pixels across (`v`) at each step.
        let steep = dy.abs() > dx.abs();
        let (mut ua, mut va, mut ga, mut ub, mut vb, mut gb) = if steep {
            (y0, x0, gain0, y1, x1, gain1)
        } else {
            (x0, y0, gain0, x1, y1, gain1)
        };
        if ua > ub {
            std::mem::swap(&mut ua, &mut ub);
            std::mem::swap(&mut va, &mut vb);
            std::mem::swap(&mut ga, &mut gb);
        }
        let (u_limit, v_limit) = if steep {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };
        let run = ub - ua;
        let slope = (vb - va) / run;
        // Across the line a pixel is `cos` times nearer than it is along `v`.
        let cos = run / len;
        let reach = (half + 0.5) / cos;
        let first = ua.floor().max(0.0) as usize;
        let last = (ub.floor().max(0.0) as usize).min(u_limit - 1);
        if ub < 0.0 || first > last {
            return;
        }
        for pu in first..=last {
            let low = (pu as f32).max(ua);
            let high = (pu as f32 + 1.0).min(ub);
            let end = (high - low).clamp(0.0, 1.0);
            if end <= 0.0 {
                continue;
            }
            let uc = (low + high) * 0.5;
            let t = (uc - ua) / run;
            let vc = va + (uc - ua) * slope;
            let gain = (ga + (gb - ga) * t) * end;
            let v_first = (vc - reach).floor();
            let v_last = (vc + reach).floor();
            if v_last < 0.0 || v_first >= v_limit as f32 {
                continue;
            }
            let v_first = v_first.max(0.0) as usize;
            let v_last = (v_last as usize).min(v_limit - 1);
            for pv in v_first..=v_last {
                let away = (pv as f32 + 0.5 - vc).abs() * cos;
                let cover = (half + 0.5 - away).clamp(0.0, 1.0);
                if cover > 0.0 {
                    if steep {
                        self.put(pv, pu, rgb, gain * cover);
                    } else {
                        self.put(pu, pv, rgb, gain * cover);
                    }
                }
            }
        }
    }

    /// A filled circle.
    pub fn disc(&mut self, x: f32, y: f32, radius: f32, rgb: Rgb, amount: f32) {
        let reach = radius + 1.0;
        let Some((x0, y0, x1, y1)) = self.span(x - reach, y - reach, x + reach, y + reach) else {
            return;
        };
        for py in y0..=y1 {
            let dy = py as f32 + 0.5 - y;
            for px in x0..=x1 {
                let dx = px as f32 + 0.5 - x;
                let cover = (radius + 0.5 - dx.hypot(dy)).clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.put(px, py, rgb, amount * cover);
                }
            }
        }
    }

    /// The outline of a circle, `width` pixels thick.
    pub fn ring(&mut self, x: f32, y: f32, radius: f32, width: f32, rgb: Rgb, amount: f32) {
        let half = width * 0.5;
        let reach = radius + half + 1.0;
        let Some((x0, y0, x1, y1)) = self.span(x - reach, y - reach, x + reach, y + reach) else {
            return;
        };
        for py in y0..=y1 {
            let dy = py as f32 + 0.5 - y;
            for px in x0..=x1 {
                let dx = px as f32 + 0.5 - x;
                let cover = (half + 0.5 - (dx.hypot(dy) - radius).abs()).clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.put(px, py, rgb, amount * cover);
                }
            }
        }
    }

    /// The outline of an ellipse with its axes along the frame's, drawn as short straight pieces.
    #[allow(clippy::too_many_arguments)]
    pub fn ellipse(&mut self, x: f32, y: f32, rx: f32, ry: f32, width: f32, rgb: Rgb, amount: f32) {
        let reach = width + 1.0;
        if self
            .span(
                x - rx - reach,
                y - ry - reach,
                x + rx + reach,
                y + ry + reach,
            )
            .is_none()
        {
            return;
        }
        let pieces = ((rx.max(ry) * 0.9) as usize).clamp(24, 360);
        let step = crate::math::TAU / pieces as f32;
        let mut from = (x + rx, y);
        for i in 1..=pieces {
            let (sin, cos) = (step * i as f32).sin_cos();
            let to = (x + rx * cos, y + ry * sin);
            self.line(from.0, from.1, to.0, to.1, width, rgb, amount, amount);
            from = to;
        }
    }
}

#[cfg(test)]
mod tests;
