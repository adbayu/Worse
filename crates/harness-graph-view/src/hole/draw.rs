use super::{
    blur, clamp01, falloff, grow_add, lerp, shrink, smoothstep, Frame, Hole, HoleView, Pass, Shade,
    Vec3, BAND, BLOOM, DISC_IN, DISC_OUT, FLOW_PERIOD, LANE_STEPS, REACH, SHOULDER,
};

impl Hole {
    /// Draws the hole over what is in the frame: the shadow and the gas hide what lies behind them.
    #[allow(clippy::too_many_lines)]
    pub fn draw(&mut self, frame: &mut Frame, view: &HoleView) {
        let scale = view.radius;
        let gain = clamp01(view.gain);
        if scale < 0.5 || gain <= 0.0 || !view.x.is_finite() || !view.y.is_finite() {
            return;
        }
        let mut normal = view.normal.unit();
        if normal == Vec3::ZERO {
            return;
        }
        // Exactly edge-on the flat disc would vanish; keep a sliver of tilt.
        if normal.z.abs() < 0.02 {
            normal.z = if normal.z < 0.0 { -0.02 } else { 0.02 };
            normal = normal.unit();
        }
        let width = frame.width();
        let height = frame.height();
        let reach = REACH * scale;
        if width == 0
            || height == 0
            || view.x + reach < 0.0
            || view.y + reach < 0.0
            || view.x - reach >= width as f32
            || view.y - reach >= height as f32
        {
            return;
        }
        let x0 = (view.x - reach).floor().max(0.0) as usize;
        let y0 = (view.y - reach).floor().max(0.0) as usize;
        let x1 = ((view.x + reach).ceil().max(0.0) as usize).min(width - 1);
        let y1 = ((view.y + reach).ceil().max(0.0) as usize).min(height - 1);
        let (box_w, box_h) = (x1 + 1 - x0, y1 + 1 - y0);
        let same = self.layer_view.is_some_and(|(x, y, radius, w, h)| {
            (x - view.x).abs() < 0.25
                && (y - view.y).abs() < 0.25
                && (radius / scale - 1.0).abs() < 0.003
                && (w, h) == (width, height)
        });
        // A large picture costs more to shade: it is kept a frame longer.
        let rests = if box_w * box_h > 220_000 { 2 } else { 1 };
        if same && gain >= 1.0 && self.rested < rests {
            self.rested += 1;
            self.lay(frame);
            return;
        }
        self.rested = 0;
        self.layer_view = (gain >= 1.0).then_some((view.x, view.y, scale, width, height));
        self.layer_box = (x0, y0, box_w, box_h);

        let helper = if normal.z.abs() < 0.9 {
            Vec3::new(0.0, 0.0, 1.0)
        } else {
            Vec3::new(1.0, 0.0, 0.0)
        };
        let e1 = normal.cross(helper).unit();
        let phase = (view.time / FLOW_PERIOD).rem_euclid(1.0);
        let flat = normal.x.hypot(normal.y).max(1e-4);
        let edge_on = (1.0 - normal.z.abs()).powi(2);
        // The patterns as they are now, at the fineness this frame can show: a step across the disc about a
        // pixel wide. Seen at a low angle the disc is squeezed, so its steps must be wider.
        let (time1, time2) = (
            phase * FLOW_PERIOD,
            (phase + 0.5).rem_euclid(1.0) * FLOW_PERIOD,
        );
        let weight = 1.0 - (2.0 * phase - 1.0).abs();
        let lanes = &self.gas.lanes;
        let turned = |across: f32| -> (f32, f32) {
            let at = across * (LANE_STEPS - 1) as f32;
            let i = (at as usize).min(LANE_STEPS - 2);
            let speed = lerp(lanes[i].1, lanes[i + 1].1, at - i as f32);
            (-speed * time1, -speed * time2 + 0.37)
        };
        let wide = (DISC_OUT - DISC_IN) * scale;
        let squeezed = normal.z.abs().max(0.02).sqrt();
        let steps = self.gas.strands.levels[0].across as f32;
        self.gas.strands.bake(
            (steps / (wide * squeezed)).max(1.0).log2() - 0.9,
            turned,
            weight,
            &mut self.plain,
        );
        let steps = self.gas.rings.levels[0].across as f32;
        self.gas.rings.bake(
            (steps / (BAND * scale)).max(1.0).log2() - 0.4,
            turned,
            weight,
            &mut self.band,
        );
        let light = &mut self.light;
        let keep = &mut self.keep;
        light.clear();
        light.resize(box_w * box_h * 3, 0.0);
        keep.clear();
        keep.resize(box_w * box_h, 1.0);
        let pass = Pass {
            shade: Shade {
                gas: &self.gas,
                plain: &self.plain,
                band: &self.band,
                normal,
                e1,
                e2: normal.cross(e1),
                depth: 1.0 + 0.3 * (1.0 - smoothstep(0.06, 0.36, normal.z.abs())),
                lifted: 0.12 + 0.88 * smoothstep(0.14, 0.4, normal.z.abs()),
            },
            x: view.x - x0 as f32,
            y: view.y - y0 as f32,
            scale,
            flat,
            bar: edge_on * smoothstep(0.34, 0.1, normal.z.abs()),
            lean: normal.z.abs(),
            near_side: if normal.z < 0.0 { -1.0 } else { 1.0 },
            turn: view.time * 0.05,
            width: box_w,
        };
        // The shading is the heavy part of a frame and every row stands alone: share the rows between a few
        // threads when there are enough of them to be worth it.
        let helpers = std::thread::available_parallelism()
            .map_or(1, |n| n.get().saturating_sub(1).clamp(1, 4));
        if helpers < 2 || box_w * box_h < 30_000 {
            pass.rows(light, keep, 0);
        } else {
            let band = box_h.div_ceil(helpers);
            std::thread::scope(|scope| {
                let lights = light.chunks_mut(band * box_w * 3);
                let keeps = keep.chunks_mut(band * box_w);
                for (i, (light, keep)) in lights.zip(keeps).enumerate() {
                    let pass = &pass;
                    scope.spawn(move || pass.rows(light, keep, i * band));
                }
            });
        }

        // The eye's bloom: the light spread at four widths, the widest about a shadow's radius.
        let step = ((scale * 0.085).round() as usize).clamp(1, 16);
        let (mut glow, glow_w, glow_h) = shrink(light, box_w, box_h, step);
        blur(&mut glow, glow_w, glow_h);
        let mut levels = vec![(glow, glow_w, glow_h)];
        while levels.len() < BLOOM.len() {
            let (last, w, h) = &levels[levels.len() - 1];
            if *w < 6 || *h < 6 {
                break;
            }
            let (mut next, next_w, next_h) = shrink(last, *w, *h, 2);
            blur(&mut next, next_w, next_h);
            levels.push((next, next_w, next_h));
        }
        while levels.len() > 1 {
            let Some((wide, wide_w, wide_h)) = levels.pop() else {
                break;
            };
            let weight = BLOOM[levels.len()] / BLOOM[levels.len() - 1];
            let last = levels.len() - 1;
            let (into, w, h) = &mut levels[last];
            grow_add(into, *w, *h, &wide, wide_w, wide_h, weight);
        }
        let (glow, glow_w, glow_h) = (&levels[0].0, levels[0].1, levels[0].2);

        // The picture for the screen: the light and its bloom, burnt towards white where it is too strong.
        let layer = &mut self.layer;
        layer.clear();
        layer.resize(box_w * box_h * 4, 0);
        let light = &self.light;
        let keep = &self.keep;
        let to_small = 1.0 / step as f32;
        let finish = |rows: &mut [u8], first: usize| {
            for (line, row) in rows.chunks_exact_mut(box_w * 4).enumerate() {
                let by = first + line;
                let gy = ((by as f32 + 0.5) * to_small - 0.5).clamp(0.0, (glow_h - 1) as f32);
                let gy0 = gy as usize;
                let gy1 = (gy0 + 1).min(glow_h - 1);
                let ty = gy - gy0 as f32;
                for bx in 0..box_w {
                    let gx = ((bx as f32 + 0.5) * to_small - 0.5).clamp(0.0, (glow_w - 1) as f32);
                    let gx0 = gx as usize;
                    let gx1 = (gx0 + 1).min(glow_w - 1);
                    let tx = gx - gx0 as f32;
                    let i = (by * box_w + bx) * 3;
                    let mut rgb = [0.0_f32; 3];
                    for (c, value) in rgb.iter_mut().enumerate() {
                        let a = lerp(
                            glow[(gy0 * glow_w + gx0) * 3 + c],
                            glow[(gy0 * glow_w + gx1) * 3 + c],
                            tx,
                        );
                        let b = lerp(
                            glow[(gy1 * glow_w + gx0) * 3 + c],
                            glow[(gy1 * glow_w + gx1) * 3 + c],
                            tx,
                        );
                        *value = light[i + c] + BLOOM[0] * lerp(a, b, ty);
                    }
                    // Light too strong for the screen: bright parts are eased towards full strength, as film
                    // does, so they keep their grain, and only the very hottest burn out to white.
                    let over = (rgb[0].max(rgb[1]).max(rgb[2]) - 1.5).max(0.0) * 0.5;
                    let px = &mut row[bx * 4..bx * 4 + 4];
                    for c in 0..3 {
                        let light = rgb[c] + over;
                        let shown = if light > SHOULDER {
                            SHOULDER + (1.0 - SHOULDER) * (1.0 - falloff((light - SHOULDER) / 0.6))
                        } else {
                            light
                        };
                        px[c] = (shown * gain * 255.0 + 0.5).min(255.0) as u8;
                    }
                    px[3] = ((1.0 - (1.0 - keep[by * box_w + bx]) * gain) * 255.0 + 0.5) as u8;
                }
            }
        };
        if helpers < 2 || box_w * box_h < 30_000 {
            finish(layer, 0);
        } else {
            let band = box_h.div_ceil(helpers);
            std::thread::scope(|scope| {
                for (i, rows) in layer.chunks_mut(band * box_w * 4).enumerate() {
                    let finish = &finish;
                    scope.spawn(move || finish(rows, i * band));
                }
            });
        }
        self.lay(frame);
    }

    /// Lays the hole's picture over the frame.
    pub(super) fn lay(&self, frame: &mut Frame) {
        let (x0, y0, box_w, box_h) = self.layer_box;
        let width = frame.width();
        if x0 + box_w > width || y0 + box_h > frame.height() || self.layer.len() < box_w * box_h * 4
        {
            return;
        }
        let data = frame.bytes();
        for (by, row) in self.layer.chunks_exact(box_w * 4).enumerate().take(box_h) {
            let start = ((y0 + by) * width + x0) * 4;
            let into = &mut data[start..start + box_w * 4];
            for (px, layer) in into.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                if layer[3] == 255 && layer[0] == 0 {
                    continue;
                }
                let stay = u32::from(layer[3]);
                for c in 0..3 {
                    px[c] = ((u32::from(px[c]) * stay + 127) / 255 + u32::from(layer[c])).min(255)
                        as u8;
                }
            }
        }
    }
}
