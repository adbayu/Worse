use super::{
    bent, clouds, disc_axis, lerp, smoothstep, sparkle, Blend, Frame, Graph, HoleView, Seen, Space,
    SpaceView, Vec3, COOL, TAU, WHITE,
};

impl Space {
    pub(super) fn paint_backdrop(&mut self, frame: &mut Frame, unit: f32) {
        let (width, height) = (frame.width(), frame.height());
        if self.backdrop_size != (width, height) {
            self.backdrop = clouds(&self.clouds, width, height, unit);
            self.backdrop_size = (width, height);
        }
        let bytes = frame.bytes();
        let len = bytes.len().min(self.backdrop.len());
        bytes[..len].copy_from_slice(&self.backdrop[..len]);
    }

    /// Draws one frame; the frame is opaque when it is done.
    #[allow(clippy::too_many_lines)]
    pub fn draw(&mut self, frame: &mut Frame, graph: &Graph, view: &SpaceView) {
        let (width, height) = (frame.width(), frame.height());
        if width == 0 || height == 0 {
            return;
        }
        frame.set_blend(Blend::Add);
        let unit = view.unit.max(0.25);
        self.paint_backdrop(frame, unit);
        let lens = Self::lens(&view.camera, width as f32, height as f32).slid(view.slide, 0.0);
        let birth = smoothstep(0.0, 1.0, view.birth);
        // The stars fly out to their places as the picture arrives.
        let grow = 0.7 + 0.3 * birth;
        let stand = lens.distance();
        let hole_seen = lens.see(Vec3::ZERO);
        self.hole_seen = hole_seen;
        let hole = hole_seen.as_ref();
        let hole_depth = hole_seen.map_or(f32::MAX, |seen| seen.depth);
        self.seen.clear();
        self.seen.extend(
            self.stars
                .iter()
                .map(|star| lens.see(star.at * grow).and_then(|seen| bent(seen, hole))),
        );
        // With a community in focus the others all but go out, and it shines a little more.
        let quiet = lerp(1.0, 0.07, view.focus_mix);
        let lit = lerp(1.0, 1.3, view.focus_mix);
        let mute = |cluster: usize| -> f32 {
            match view.focus {
                Some(focus) if focus != cluster => quiet,
                Some(_) => lit,
                None => 1.0,
            }
        };
        let scenery = birth * lerp(1.0, 0.5, view.focus_mix);
        // Nearer the eye: brighter and a little larger.
        let nearness = |seen: &Seen| (stand / seen.depth).clamp(0.4, 3.0);
        let line = unit.max(1.0);

        for far in [true, false] {
            for mote in &self.motes {
                let Some(seen) = lens.see(mote.at * grow).and_then(|seen| bent(seen, hole)) else {
                    continue;
                };
                if (seen.depth > hole_depth) == far {
                    frame.dot(
                        seen.x,
                        seen.y,
                        WHITE,
                        mote.light * scenery * nearness(&seen).powf(1.4),
                    );
                }
            }
            for grain in &self.grains {
                let angle = grain.start + grain.pace * view.time;
                let at = |angle: f32| -> Option<Seen> {
                    let (sin, cos) = angle.sin_cos();
                    lens.see((grain.e1 * cos + grain.e2 * sin) * (grain.radius * grow))
                        .and_then(|seen| bent(seen, hole))
                };
                let Some(head) = at(angle) else { continue };
                if (head.depth > hole_depth) != far {
                    continue;
                }
                let gain = grain.light * scenery * nearness(&head);
                match at(angle + grain.trail) {
                    Some(tail) => frame.line(
                        head.x,
                        head.y,
                        tail.x,
                        tail.y,
                        line * 1.2,
                        WHITE,
                        gain,
                        gain * 0.3,
                    ),
                    None => frame.dot(head.x, head.y, WHITE, gain),
                }
            }
            for shell in &self.shells {
                let turn = shell.pace * view.time;
                for i in 0..shell.count {
                    let (sin, cos) = (TAU * i as f32 / shell.count as f32 + turn).sin_cos();
                    let point = (shell.e1 * cos + shell.e2 * sin) * (shell.radius * grow);
                    let Some(seen) = lens.see(point).and_then(|seen| bent(seen, hole)) else {
                        continue;
                    };
                    if (seen.depth > hole_depth) == far {
                        frame.dot(
                            seen.x,
                            seen.y,
                            WHITE,
                            shell.light * scenery * nearness(&seen),
                        );
                    }
                }
            }
            for link in &self.links {
                let (Some(a), Some(b)) = (self.seen[link.a], self.seen[link.b]) else {
                    continue;
                };
                let depth = (a.depth + b.depth) * 0.5;
                if (depth > hole_depth) != far {
                    continue;
                }
                let shown = mute(self.stars[link.a].cluster).min(mute(self.stars[link.b].cluster));
                let gain = birth * shown * shown * (stand / depth).clamp(0.5, 2.0);
                frame.line(
                    a.x,
                    a.y,
                    b.x,
                    b.y,
                    line,
                    WHITE,
                    link.light.0 * gain,
                    link.light.1 * gain,
                );
            }
            for (star, seen) in self.stars.iter().zip(&self.seen) {
                let Some(seen) = seen else { continue };
                if (seen.depth > hole_depth) != far {
                    continue;
                }
                let near = nearness(seen);
                let twinkle = 1.0 + 0.2 * (view.time * star.pace + star.beat).sin();
                let gain = star.light * twinkle * birth * mute(star.cluster) * near.powf(1.2);
                let size = star.size * unit * near.powf(0.75) * 0.6;
                // A hot middle, a soft light around it, and a wide faint haze.
                frame.glow(seen.x, seen.y, size, WHITE, gain * 1.3);
                frame.glow(seen.x, seen.y, size * 2.2, star.tint, gain * 0.36);
                frame.glow(seen.x, seen.y, size * 5.0, star.tint, gain * 0.06);
                if star.flare > 0.0 {
                    sparkle(
                        frame,
                        seen.x,
                        seen.y,
                        unit * near * (1.5 + 4.5 * star.flare),
                        gain * 0.2,
                        line,
                    );
                }
            }
            if far {
                if let Some(seen) = hole_seen {
                    self.hole.draw(
                        frame,
                        &HoleView {
                            x: seen.x,
                            y: seen.y,
                            radius: seen.scale * grow,
                            normal: lens.turn(disc_axis()),
                            time: view.time,
                            gain: birth,
                        },
                    );
                }
            }
        }

        let roaming = birth * (1.0 - view.focus_mix);
        if roaming > 0.01 {
            const PIECES: usize = 28;
            let (right, up) = lens.axes();
            for comet in &self.comets {
                let head = comet.start + comet.pace * view.time;
                let back = -comet.pace.signum() * comet.tail / PIECES as f32;
                let (lean_sin, lean_cos) = comet.lean.sin_cos();
                let at = |piece: usize| -> Option<Seen> {
                    let (sin, cos) = (head + back * piece as f32).sin_cos();
                    let (x, y) = (cos, sin * comet.squash);
                    let across =
                        right * (x * lean_cos - y * lean_sin) + up * (x * lean_sin + y * lean_cos);
                    lens.see(view.camera.target + across * comet.radius)
                };
                let strength = |piece: usize| -> f32 {
                    let along = 1.0 - piece as f32 / PIECES as f32;
                    0.6 * roaming * along * along
                };
                let mut from = at(0);
                if let Some(seen) = from {
                    let near = nearness(&seen);
                    frame.glow(seen.x, seen.y, 1.0 * unit * near, WHITE, roaming * 1.3);
                    frame.glow(seen.x, seen.y, 3.0 * unit * near, COOL, roaming * 0.22);
                }
                for piece in 1..=PIECES {
                    let to = at(piece);
                    if let (Some(a), Some(b)) = (from, to) {
                        frame.line(
                            a.x,
                            a.y,
                            b.x,
                            b.y,
                            line,
                            WHITE,
                            strength(piece - 1),
                            strength(piece),
                        );
                    }
                    from = to;
                }
            }
        }

        // The chosen star: lines to everything it is linked to, over the rest of the picture.
        if let Some((selected, seen)) = view.selected.and_then(|i| Some((i, self.seen(i)?))) {
            for &other in graph.neighbours(selected) {
                if let Some(to) = self.seen(other) {
                    frame.line(
                        seen.x,
                        seen.y,
                        to.x,
                        to.y,
                        line * 1.2,
                        WHITE,
                        0.95 * birth,
                        0.6 * birth,
                    );
                    frame.glow(to.x, to.y, 1.5 * unit, WHITE, birth);
                }
            }
            sparkle(frame, seen.x, seen.y, unit * 15.0, birth, line);
            frame.glow(seen.x, seen.y, 2.0 * unit, WHITE, 1.4 * birth);
        }
        if let Some(seen) = view
            .hover
            .filter(|i| view.selected != Some(*i))
            .and_then(|i| self.seen(i))
        {
            sparkle(frame, seen.x, seen.y, unit * 11.0, 0.9 * birth, line);
            frame.glow(seen.x, seen.y, 1.7 * unit, WHITE, birth);
        }
    }
}
