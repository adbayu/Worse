use super::{
    clamp01, falloff, fast_atan2, lerp, smoothstep, Shade, Vec3, BAND, BEND_REACH, BEND_STEPS,
    DISC_OUT, REACH, TAU,
};

/// Everything one frame's shading needs, shared by the threads that do it.
pub(super) struct Pass<'a> {
    pub(super) shade: Shade<'a>,
    /// The hole's middle and the shadow's radius, in pixels of the hole's own picture.
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) scale: f32,
    /// Length of the disc's axis as it lies on the picture.
    pub(super) flat: f32,
    /// Strength of the bright bar of an edge-on disc, along the line where the disc cuts the picture.
    pub(super) bar: f32,
    /// 0 seen edge-on, 1 seen from straight above.
    pub(super) lean: f32,
    /// On which side of the picture the near half of the disc lies, along the disc's axis.
    pub(super) near_side: f32,
    /// Turns the dashes of the thin rings.
    pub(super) turn: f32,
    pub(super) width: usize,
}

impl Pass<'_> {
    /// Shades whole rows of the hole's picture, starting at row `first`.
    #[allow(clippy::too_many_lines)]
    pub(super) fn rows(&self, light: &mut [f32], keep: &mut [f32], first: usize) {
        let normal = self.shade.normal;
        let (scale, flat) = (self.scale, self.flat);
        let (axis_x, axis_y) = (normal.x / flat, normal.y / flat);
        let gas = (DISC_OUT + 0.35) * (DISC_OUT + 0.35);
        let far = REACH * REACH;
        let bend = &self.shade.gas.bend;
        let rows = light
            .chunks_exact_mut(self.width * 3)
            .zip(keep.chunks_exact_mut(self.width));
        for (line, (light, keep)) in rows.enumerate() {
            let v = (self.y - ((first + line) as f32 + 0.5)) / scale;
            for px in 0..self.width {
                let u = (px as f32 + 0.5 - self.x) / scale;
                let near2 = u * u + v * v;
                if near2 >= far {
                    continue;
                }
                // Height over the disc's plane at the point nearest the hole.
                let lift = normal.x * u + normal.y * v;
                let across = lift / flat;
                let along = (normal.y * u - normal.x * v) / flat;
                let mut flare = 0.0;
                if self.bar > 0.004 && across.abs() < 0.5 && along.abs() < 5.5 {
                    // An edge-on disc is thinner than a pixel: its light is put back as a bar, brighter on
                    // the side where the gas comes towards the eye.
                    let side = 1.0 - 0.45 * (along / (0.7 + along.abs()));
                    flare = self.bar
                        * (3.2
                            * falloff((across / 0.06).powi(2))
                            * falloff(((along + 0.7) / 0.8).powi(2))
                            + 1.0
                                * falloff((across / 0.035).powi(2))
                                * falloff((along / 3.0).powi(2)))
                        * side;
                }
                let out = &mut light[px * 3..px * 3 + 3];
                if near2 >= gas {
                    out[0] = flare;
                    out[1] = flare * 0.86;
                    out[2] = flare * 0.66;
                    continue;
                }
                let near = near2.sqrt();

                // In front of the hole the ray is still straight.
                let mut front = ([0.0_f32; 3], 0.0_f32);
                let before = -lift / normal.z;
                if before < 0.0 {
                    let radius = (near2 + before * before).sqrt();
                    if radius <= DISC_OUT {
                        front = self.shade.emit(Vec3::new(u, v, before), radius, 0.0);
                    }
                }
                // Behind it the ray has been bent towards the hole. Rays that pass inside the shadow never
                // come out, and what the near half of the disc covers need not be worked out.
                let mut back = ([0.0_f32; 3], 0.0_f32);
                if near > 0.98 && front.1 < 0.97 {
                    let at = near * (BEND_STEPS as f32 / BEND_REACH);
                    let i = (at as usize).min(BEND_STEPS - 1);
                    let t = at - i as f32;
                    let (sin, cos) = (
                        lerp(bend[i].0, bend[i + 1].0, t),
                        lerp(bend[i].1, bend[i + 1].1, t),
                    );
                    let slope = normal.z * cos - sin * lift / near;
                    if slope.abs() > 1e-5 {
                        let after = -lift / slope;
                        if after > 0.0 {
                            let pull = after * sin / near;
                            let point = Vec3::new(u - pull * u, v - pull * v, after * cos);
                            let radius = point.len();
                            if radius <= DISC_OUT {
                                // Where the bend has carried the ray far, the disc is squeezed into rings.
                                // At the seam with the near half (`pull` zero) it is the same pattern as
                                // there.
                                back = self.shade.emit(point, radius, smoothstep(0.05, 0.5, pull));
                            }
                        }
                    }
                }

                let shadow = clamp01((1.0 - near) * scale + 0.5);
                let open = 1.0 - shadow;
                // Thin rings of light that went all the way around: one on the rim of the shadow, a dotted
                // one inside it, and a wider one that slides towards the near side as the disc tips up.
                let thin = 0.012 + 0.55 / scale;
                let slide = 0.72 * self.lean * self.near_side;
                let (du, dv) = (u - axis_x * slide, v - axis_y * slide);
                let wide = (du * du + dv * dv).sqrt();
                let rim = (near - 1.01).abs() < thin * 3.0;
                let inner = (near - 0.85).abs() < thin * 3.0;
                let outer = (wide - 1.27).abs() < thin * 3.0;
                let place = (near - 1.0) / BAND;
                let hugs = (0.0..1.0).contains(&place);
                let angle = if rim || inner || outer || hugs {
                    fast_atan2(v, u) / TAU
                } else {
                    0.0
                };
                let dashes = |count: f32, turn: f32| -> f32 {
                    smoothstep(-0.2, 0.35, (TAU * (angle * count + turn)).sin())
                };
                let mut ring = 0.0;
                if rim {
                    ring += 0.75
                        * falloff(((near - 1.01) / thin).powi(2))
                        * (0.75 + 0.25 * dashes(31.0, self.turn));
                }
                if inner {
                    ring += 0.2
                        * falloff(((near - 0.85) / thin).powi(2))
                        * dashes(17.0, -self.turn * 1.4);
                }
                if outer {
                    ring += 0.26
                        * falloff(((wide - 1.27) / thin).powi(2))
                        * (0.6 + 0.4 * dashes(23.0, self.turn * 0.7));
                }
                // The band that hugs the shadow: light from all of the disc that went around the hole,
                // squeezed into thin rings, brighter where the gas comes towards the eye.
                let mut band = 0.0;
                if hugs {
                    let lines = self.shade.band.at(place, angle);
                    // About the power 0.8, without the cost of a power.
                    let fall = (1.0 - place).sqrt()
                        * (1.0 - place).sqrt().sqrt()
                        * smoothstep(0.0, 0.05, place);
                    band = 1.75 * fall * lines * (1.0 - 0.45 * along / near);
                }

                let through = 1.0 - front.1;
                let hugging = through * open * (1.0 - back.1);
                // The shadow is not black to an eye dazzled by the gas around it: a warm haze lies over it,
                // thicker towards the bright side.
                let veil = through * shadow * (0.13 - 0.05 * along);
                out[0] =
                    front.0[0] + through * back.0[0] * open + hugging * band + ring + flare + veil;
                out[1] = front.0[1]
                    + through * back.0[1] * open
                    + hugging * band * 0.92
                    + ring * 0.93
                    + flare * 0.86
                    + veil * 0.8;
                out[2] = front.0[2]
                    + through * back.0[2] * open
                    + hugging * band * 0.78
                    + ring * 0.82
                    + flare * 0.66
                    + veil * 0.58;
                keep[px] = through * (1.0 - back.1 * open) * open;
            }
        }
    }
}

/// A picture `by` times smaller, each pixel the mean of what it covers (red, green, blue per pixel).
pub(super) fn shrink(
    src: &[f32],
    width: usize,
    height: usize,
    by: usize,
) -> (Vec<f32>, usize, usize) {
    let (w, h) = (width.div_ceil(by), height.div_ceil(by));
    let mut out = vec![0.0_f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0_f32; 3];
            let mut count = 0.0;
            for sy in y * by..((y + 1) * by).min(height) {
                let row = sy * width;
                for sx in x * by..((x + 1) * by).min(width) {
                    let i = (row + sx) * 3;
                    sum[0] += src[i];
                    sum[1] += src[i + 1];
                    sum[2] += src[i + 2];
                    count += 1.0;
                }
            }
            let o = (y * w + x) * 3;
            out[o] = sum[0] / count;
            out[o + 1] = sum[1] / count;
            out[o + 2] = sum[2] / count;
        }
    }
    (out, w, h)
}

/// A soft blur, two pixels wide, along the rows and then down the columns.
pub(super) fn blur(data: &mut [f32], width: usize, height: usize) {
    const TAPS: [f32; 5] = [0.0625, 0.25, 0.375, 0.25, 0.0625];
    let mut line = vec![0.0_f32; width.max(height) * 3];
    for y in 0..height {
        let row = y * width * 3;
        line[..width * 3].copy_from_slice(&data[row..row + width * 3]);
        for x in 0..width {
            for c in 0..3 {
                let mut sum = 0.0;
                for (k, tap) in TAPS.iter().enumerate() {
                    let sx = (x + k).saturating_sub(2).min(width - 1);
                    sum += tap * line[sx * 3 + c];
                }
                data[row + x * 3 + c] = sum;
            }
        }
    }
    for x in 0..width {
        for y in 0..height {
            for c in 0..3 {
                line[y * 3 + c] = data[(y * width + x) * 3 + c];
            }
        }
        for y in 0..height {
            for c in 0..3 {
                let mut sum = 0.0;
                for (k, tap) in TAPS.iter().enumerate() {
                    let sy = (y + k).saturating_sub(2).min(height - 1);
                    sum += tap * line[sy * 3 + c];
                }
                data[(y * width + x) * 3 + c] = sum;
            }
        }
    }
}

/// Adds a picture half this one's size, stretched smoothly over it, `weight` times.
#[allow(clippy::too_many_arguments)]
pub(super) fn grow_add(
    into: &mut [f32],
    width: usize,
    height: usize,
    small: &[f32],
    small_w: usize,
    small_h: usize,
    weight: f32,
) {
    for y in 0..height {
        let sy = ((y as f32 + 0.5) * 0.5 - 0.5).clamp(0.0, (small_h - 1) as f32);
        let y0 = sy as usize;
        let y1 = (y0 + 1).min(small_h - 1);
        let ty = sy - y0 as f32;
        for x in 0..width {
            let sx = ((x as f32 + 0.5) * 0.5 - 0.5).clamp(0.0, (small_w - 1) as f32);
            let x0 = sx as usize;
            let x1 = (x0 + 1).min(small_w - 1);
            let tx = sx - x0 as f32;
            for c in 0..3 {
                let a = lerp(
                    small[(y0 * small_w + x0) * 3 + c],
                    small[(y0 * small_w + x1) * 3 + c],
                    tx,
                );
                let b = lerp(
                    small[(y1 * small_w + x0) * 3 + c],
                    small[(y1 * small_w + x1) * 3 + c],
                    tx,
                );
                into[(y * width + x) * 3 + c] += weight * lerp(a, b, ty);
            }
        }
    }
}
