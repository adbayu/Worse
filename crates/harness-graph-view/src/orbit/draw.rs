use super::{lerp, smoothstep, Blend, Frame, Graph, Orbit, OrbitView, Vec3, SUN};

impl Orbit {
    /// Draws one frame on a cleared, see-through frame.
    #[allow(clippy::too_many_lines)]
    pub fn draw(&mut self, frame: &mut Frame, graph: &Graph, view: &OrbitView) {
        if frame.width() == 0 || frame.height() == 0 {
            return;
        }
        frame.set_blend(Blend::Over);
        frame.clear([0.0; 3], 0.0);
        let unit = view.unit.max(0.25);
        let lens = self
            .lens(
                &view.camera,
                frame.width() as f32,
                frame.height() as f32,
                unit,
            )
            .slid(view.slide, 0.0);
        let palette = &view.palette;
        let birth = smoothstep(0.0, 1.0, view.birth);
        // The orbits open out from the sun as the picture arrives.
        let grow = 0.35 + 0.65 * birth;
        let flat = view.camera.pitch.sin().abs();
        let line = unit.max(1.0);
        self.seen.fill(None);
        let sun = lens.see(Vec3::ZERO);
        self.sun_seen = sun;
        let Some(sun) = sun else { return };
        let scale = sun.scale;
        // Bodies grow more slowly than the distances between them: zoomed in, a system opens up instead of
        // turning into a blot, and zoomed out its moons stay visible.
        let body = unit * (scale / unit).powf(0.55) * 1.05;

        // With a node chosen (or under the pointer), it and what it is linked to stay bright.
        let lit = view.selected.or(view.hover);
        let related = |node: usize| -> bool {
            lit.map_or(true, |chosen| {
                chosen == node || graph.neighbours(chosen).contains(&node)
            })
        };
        let quiet = lerp(1.0, 0.3, view.focus_mix);
        let mute = |cluster: usize| -> f32 {
            match view.focus {
                Some(focus) if focus != cluster => quiet,
                _ => 1.0,
            }
        };

        let mut drawn = 0.0;
        for system in &self.systems {
            // Systems that share an orbit share its line.
            if (system.orbit - drawn).abs() < 0.01 {
                continue;
            }
            drawn = system.orbit;
            let radius = system.orbit * grow * scale;
            frame.ellipse(
                sun.x,
                sun.y,
                radius,
                radius * flat,
                line,
                palette.line,
                0.42 * birth,
            );
        }
        // The sun: a soft light and a solid middle.
        let sun_size = SUN * body;
        frame.disc(sun.x, sun.y, sun_size * 2.3, palette.sun, 0.07 * birth);
        frame.disc(sun.x, sun.y, sun_size * 1.55, palette.sun, 0.13 * birth);
        frame.disc(sun.x, sun.y, sun_size, palette.sun, birth);

        // Systems further from the eye first, so nearer ones lie over them.
        let mut order: Vec<(f32, usize)> = self
            .systems
            .iter()
            .enumerate()
            .map(|(k, system)| (lens.turn(Self::planet_at(system, view.time)).z, k))
            .collect();
        order.sort_by(|a, b| b.0.total_cmp(&a.0));
        for &(_, k) in &order {
            let system = &self.systems[k];
            let tone = palette.tone(system.tone);
            let dim = mute(system.cluster) * birth;
            let centre = Self::planet_at(system, view.time) * grow;
            let Some(planet) = lens.see(centre) else {
                continue;
            };
            for ring in &system.rings {
                let radius = ring * scale;
                frame.ellipse(
                    planet.x,
                    planet.y,
                    radius,
                    radius * flat,
                    line,
                    tone,
                    0.2 * dim,
                );
            }
            for moon in &system.moons {
                let (sin, cos) = (moon.start + moon.pace * view.time).sin_cos();
                let at = centre + Vec3::new(moon.ring * cos, 0.0, moon.ring * sin);
                let Some(seen) = lens.see(at) else { continue };
                let size = (moon.size * body).max(1.2 * unit);
                let shown = if related(moon.node) { 1.0 } else { 0.28 };
                frame.disc(seen.x, seen.y, size, tone, 0.92 * dim * shown);
                self.seen[moon.node] = Some((seen, size));
            }
            let size = (system.planet_size * body).max(2.0 * unit);
            let shown = if related(system.planet) { 1.0 } else { 0.4 };
            frame.disc(planet.x, planet.y, size * 1.7, tone, 0.16 * dim * shown);
            frame.disc(planet.x, planet.y, size, tone, dim * shown);
            self.seen[system.planet] = Some((planet, size));
        }

        // The chosen node: lines to everything it is linked to, and a ring around it and each of them.
        if let Some((chosen, (from, size))) =
            lit.and_then(|i| Some((i, self.seen.get(i).copied().flatten()?)))
        {
            let strong = if view.selected == Some(chosen) {
                0.85
            } else {
                0.55
            };
            for &other in graph.neighbours(chosen) {
                if let Some((to, other_size)) = self.seen.get(other).copied().flatten() {
                    frame.line(
                        from.x,
                        from.y,
                        to.x,
                        to.y,
                        line * 1.15,
                        palette.ink,
                        strong * birth,
                        strong * 0.7 * birth,
                    );
                    frame.ring(
                        to.x,
                        to.y,
                        other_size + 2.0 * unit,
                        line,
                        palette.ink,
                        0.8 * birth,
                    );
                }
            }
            frame.ring(
                from.x,
                from.y,
                size + 3.0 * unit,
                line * 1.5,
                palette.ink,
                birth,
            );
        }
    }
}
