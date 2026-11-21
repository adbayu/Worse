use super::{
    lerp, Arc, Blend, Engine, Frame, Marker, OrbitView, Overlay, Seen, Space, SpaceView, Style,
    ARRIVE, SPIN,
};

impl Engine {
    /// Moves time on by `seconds` and the camera towards where it was asked to go.
    pub fn step(&mut self, seconds: f32) {
        let dt = if seconds.is_finite() {
            seconds.clamp(0.0, 0.1)
        } else {
            0.0
        };
        self.birth = (self.birth + dt / ARRIVE).min(1.0);
        if !self.paused {
            self.time += dt;
            if self.style == Style::Space && self.press.is_none() {
                self.goal.yaw += SPIN * dt * if self.focus.is_some() { 0.45 } else { 1.0 };
            }
        }
        if let (Style::Orbit, Some(cluster)) = (self.style, self.focus) {
            let time = self.time;
            self.goal.target = self.orbit().anchor(cluster, time);
        }
        let ease = |rate: f32| 1.0 - (-rate * dt).exp();
        self.camera.yaw = lerp(self.camera.yaw, self.goal.yaw, ease(9.0));
        self.camera.pitch = lerp(self.camera.pitch, self.goal.pitch, ease(9.0));
        self.camera.zoom *= (self.goal.zoom / self.camera.zoom.max(1e-3)).powf(ease(6.0));
        self.camera.target = self.camera.target.lerp(self.goal.target, ease(6.0));
        let wanted = if self.focus.is_some() { 1.0 } else { 0.0 };
        self.focus_mix = lerp(self.focus_mix, wanted, ease(5.0));
        if (self.focus_mix - wanted).abs() < 0.004 {
            self.focus_mix = wanted;
        }
        self.made = lerp(self.made, self.inset, ease(7.0));
        if (self.made - self.inset).abs() < 0.5 {
            self.made = self.inset;
        }
    }

    /// Whether the picture is still changing by itself: a UI keeps drawing frames while it is.
    #[must_use]
    pub fn busy(&self) -> bool {
        if !self.paused {
            return true;
        }
        let wanted = if self.focus.is_some() { 1.0 } else { 0.0 };
        self.birth < 1.0
            || (self.camera.yaw - self.goal.yaw).abs() > 2e-3
            || (self.camera.pitch - self.goal.pitch).abs() > 2e-3
            || (self.camera.zoom / self.goal.zoom - 1.0).abs() > 2e-3
            || (self.camera.target - self.goal.target).len() > 0.02
            || (self.focus_mix - wanted).abs() > 0.004
            || (self.made - self.inset).abs() > 0.5
    }

    /// Draws a frame into `data`: RGBA with premultiplied alpha, `size()` pixels, rows from the top. The
    /// galaxy is opaque; the solar system is see-through around its bodies.
    pub fn render(&mut self, data: &mut [u8]) {
        let (width, height) = (self.width, self.height);
        if width == 0 || height == 0 || data.len() < width * height * 4 {
            return;
        }
        let graph = Arc::clone(&self.graph);
        match self.style {
            Style::Space => {
                let view = SpaceView {
                    camera: self.camera,
                    time: self.time,
                    birth: self.birth,
                    focus: self.focus,
                    focus_mix: self.focus_mix,
                    hover: self.hover,
                    selected: self.selected,
                    unit: self.unit,
                    slide: self.slide(),
                };
                let mut frame = Frame::new(data, width, height, Blend::Add);
                self.space().draw(&mut frame, &graph, &view);
            }
            Style::Orbit => {
                let view = OrbitView {
                    camera: self.camera,
                    time: self.time,
                    birth: self.birth,
                    focus: self.focus,
                    focus_mix: self.focus_mix,
                    hover: self.hover,
                    selected: self.selected,
                    unit: self.unit,
                    slide: self.slide(),
                    palette: self.palette,
                };
                let mut frame = Frame::new(data, width, height, Blend::Over);
                self.orbit().draw(&mut frame, &graph, &view);
            }
        }
    }

    pub(super) fn marker(&self, seen: &Seen, below: f32, alpha: f32) -> Marker {
        Marker {
            x: seen.x / self.unit,
            y: (seen.y + below) / self.unit,
            alpha: alpha.clamp(0.0, 1.0),
        }
    }

    /// Where the names go over the frame drawn last.
    #[must_use]
    pub fn overlay(&self) -> Overlay {
        let mut overlay = Overlay::default();
        let (width, height) = (self.width as f32, self.height as f32);
        if width < 1.0 || height < 1.0 {
            return overlay;
        }
        let arrived = ((self.birth - 0.35) / 0.65).clamp(0.0, 1.0);
        let point =
            |seen: Option<Seen>, node: usize| seen.map(|seen| (node, self.marker(&seen, 0.0, 1.0)));
        match self.style {
            Style::Space => {
                let Some(space) = &self.space else {
                    return overlay;
                };
                let lens = Space::lens(&self.camera, width, height).slid(self.slide(), 0.0);
                let grow = 0.7 + 0.3 * self.birth;
                let stand = lens.distance();
                let hole = space.hole_seen();
                for cluster in 0..self.graph.clusters.len() {
                    let Some(mut seen) = lens
                        .see(space.anchor(cluster) * grow)
                        .and_then(|seen| space.shown(seen))
                    else {
                        continue;
                    };
                    // A name keeps clear of the hole and its disc.
                    if let Some(hole) = hole {
                        let (dx, dy) = (seen.x - hole.x, seen.y - hole.y);
                        let (away, clear) = (dx.hypot(dy), hole.scale * 5.0);
                        if away < clear {
                            let (dx, dy) = if away < 1.0 {
                                (0.0, -1.0)
                            } else {
                                (dx / away, dy / away)
                            };
                            seen.x = hole.x + dx * clear;
                            seen.y = hole.y + dy * clear;
                        }
                    }
                    // Names on the far side are fainter.
                    let depth = ((seen.depth - stand) / crate::space::GALAXY).clamp(-1.0, 1.0);
                    let alpha = arrived * (1.0 - self.focus_mix) * (0.8 - 0.3 * depth);
                    overlay
                        .labels
                        .push((cluster, self.marker(&seen, 0.0, alpha)));
                }
                // Two names never lie on top of each other: the later one steps down or up.
                for i in 1..overlay.labels.len() {
                    for j in 0..i {
                        let other = overlay.labels[j].1;
                        let label = &mut overlay.labels[i].1;
                        if (label.x - other.x).abs() < 96.0 && (label.y - other.y).abs() < 30.0 {
                            label.y = other.y + if label.y < other.y { -30.0 } else { 30.0 };
                        }
                    }
                }
                overlay.core = space.hole_seen().map(|seen| {
                    self.marker(
                        &seen,
                        seen.scale * 1.95,
                        arrived * (1.0 - 0.7 * self.focus_mix),
                    )
                });
                overlay.hover = self.hover.and_then(|node| point(space.seen(node), node));
                overlay.selected = self.selected.and_then(|node| point(space.seen(node), node));
            }
            Style::Orbit => {
                let Some(orbit) = &self.orbit else {
                    return overlay;
                };
                let lens = orbit
                    .lens(&self.camera, width, height, self.unit)
                    .slid(self.slide(), 0.0);
                let flat = self.camera.pitch.sin().abs();
                let grow = 0.35 + 0.65 * self.birth;
                for cluster in 0..self.graph.clusters.len() {
                    let reach = orbit.system_reach(cluster);
                    if reach <= 0.0 {
                        continue;
                    }
                    let Some(seen) = lens.see(orbit.anchor(cluster, self.time) * grow) else {
                        continue;
                    };
                    let quiet = match self.focus {
                        Some(focus) if focus != cluster => lerp(1.0, 0.35, self.focus_mix),
                        _ => 1.0,
                    };
                    overlay.labels.push((
                        cluster,
                        self.marker(
                            &seen,
                            reach * seen.scale * flat + 5.0 * self.unit,
                            arrived * quiet,
                        ),
                    ));
                }
                overlay.core = orbit
                    .sun_seen()
                    .map(|seen| self.marker(&seen, 15.0 * seen.scale + 6.0 * self.unit, arrived));
                overlay.hover = self.hover.and_then(|node| point(orbit.seen(node), node));
                overlay.selected = self.selected.and_then(|node| point(orbit.seen(node), node));
            }
        }
        overlay
    }
}
