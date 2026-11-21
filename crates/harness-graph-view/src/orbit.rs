//! The simple picture: the graph as a small solar system.
//!
//! The project is the sun. Every community is a planet (its best connected node) with the community's other
//! nodes as moons around it, on rings. Small communities circle close to the sun, large ones further out;
//! when there are many, two or three of about the same size share an orbit, spread evenly around it. It is
//! flat and seen at a slant, so the orbits are ellipses, and it is drawn in the theme's colours on a
//! see-through frame. No two systems can touch: orbits are spaced by the sizes of what travels on them, and
//! systems on one orbit keep their distance because they travel at the same pace.

use crate::camera::{Camera, Lens, Seen};
use crate::math::{lerp, smoothstep, Vec3, TAU};
use crate::model::Graph;
use crate::raster::{Blend, Frame, Rgb};

mod draw;

/// Nodes drawn at most; a readable orrery has fewer bodies than a sky has stars.
pub const MAX_BODIES: usize = 260;
/// The slant the system is seen at: the sine of it is how flat the orbits look.
pub const SLANT: f32 = std::f32::consts::FRAC_PI_6;
const SUN: f32 = 15.0;
/// Room between the sun and the first system, and between one system and the next.
const FIRST_GAP: f32 = 46.0;
const GAP: f32 = 15.0;
/// Room between a planet and its first ring of moons, and from ring to ring.
const RING_GAP: f32 = 8.5;
/// Room a moon needs along its ring.
const MOON_ROOM: f32 = 8.2;

/// The theme's colours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// Lines to a chosen node's neighbours, and the ring around it.
    pub ink: Rgb,
    /// Orbits.
    pub line: Rgb,
    /// The sun.
    pub sun: Rgb,
    /// One per community, largest first; a community past the end, and "other", get `quiet`.
    pub tones: [Rgb; 7],
    pub quiet: Rgb,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            ink: [0.93, 0.93, 0.94],
            line: [0.5, 0.52, 0.56],
            sun: [0.96, 0.78, 0.36],
            tones: [
                [0.55, 0.62, 0.98],
                [0.38, 0.78, 0.9],
                [0.72, 0.56, 0.96],
                [0.92, 0.6, 0.42],
                [0.95, 0.78, 0.4],
                [0.42, 0.82, 0.6],
                [0.94, 0.52, 0.66],
            ],
            quiet: [0.6, 0.62, 0.66],
        }
    }
}

impl Palette {
    #[must_use]
    pub fn tone(&self, tone: i32) -> Rgb {
        usize::try_from(tone)
            .ok()
            .and_then(|i| self.tones.get(i))
            .copied()
            .unwrap_or(self.quiet)
    }
}

struct Moon {
    node: usize,
    ring: f32,
    start: f32,
    pace: f32,
    size: f32,
}

struct System {
    cluster: usize,
    tone: i32,
    planet: usize,
    planet_size: f32,
    orbit: f32,
    start: f32,
    pace: f32,
    reach: f32,
    rings: Vec<f32>,
    moons: Vec<Moon>,
}

/// What one frame shows.
#[derive(Debug, Clone, Copy)]
pub struct OrbitView {
    pub camera: Camera,
    pub time: f32,
    pub birth: f32,
    pub focus: Option<usize>,
    pub focus_mix: f32,
    pub hover: Option<usize>,
    pub selected: Option<usize>,
    pub unit: f32,
    /// How far the middle of the picture is moved to the right, in frame pixels.
    pub slide: f32,
    pub palette: Palette,
}

pub struct Orbit {
    systems: Vec<System>,
    /// The radius of the whole system, the outermost moons included.
    reach: f32,
    /// By node: where it was in the last frame drawn, and its radius there.
    seen: Vec<Option<(Seen, f32)>>,
    /// By community: the system's index.
    of_cluster: Vec<Option<usize>>,
    sun_seen: Option<Seen>,
}

impl Orbit {
    #[must_use]
    pub fn new(graph: &Graph) -> Self {
        let bodies = graph.nodes.len().min(MAX_BODIES);
        let top = graph.nodes.first().map_or(1, |n| n.degree).max(1) as f32;
        let mut systems: Vec<System> = Vec::new();
        // Smallest community first: it gets the innermost orbit.
        let mut order: Vec<usize> = (0..graph.clusters.len()).collect();
        order.sort_by_key(|&c| {
            let drawn = graph.clusters[c]
                .members
                .iter()
                .filter(|&&i| i < bodies)
                .count();
            (drawn, std::cmp::Reverse(c))
        });
        for cluster in order {
            let members: Vec<usize> = graph.clusters[cluster]
                .members
                .iter()
                .copied()
                .filter(|&i| i < bodies)
                .collect();
            let Some((&planet, moons)) = members.split_first() else {
                continue;
            };
            let planet_size = 6.0 + 4.0 * ((members.len() as f32) / bodies.max(1) as f32).sqrt();
            let mut rings = Vec::new();
            let mut placed = Vec::with_capacity(moons.len());
            let mut left = moons;
            while !left.is_empty() {
                let ring = planet_size + RING_GAP * (rings.len() + 1) as f32 + 1.0;
                let room = ((TAU * ring / MOON_ROOM) as usize).max(3);
                let (here, rest) = left.split_at(left.len().min(room));
                let turn = rings.len() as f32 * 0.7;
                // Kepler again: the inner rings turn faster.
                let pace = 0.3 * ((planet_size + RING_GAP) / ring).powf(1.5);
                for (k, &node) in here.iter().enumerate() {
                    placed.push(Moon {
                        node,
                        ring,
                        start: turn + TAU * k as f32 / here.len() as f32,
                        pace,
                        size: 2.3 + 1.5 * (graph.nodes[node].degree as f32 / top).sqrt(),
                    });
                }
                rings.push(ring);
                left = rest;
            }
            let reach = rings.last().map_or(planet_size + 3.0, |ring| ring + 5.0);
            systems.push(System {
                cluster,
                tone: graph.clusters[cluster].tone,
                planet,
                planet_size,
                orbit: 0.0,
                start: 0.0,
                pace: 0.0,
                reach,
                rings,
                moons: placed,
            });
        }
        let sharing = match systems.len() {
            0..=4 => 1,
            5..=8 => 2,
            _ => 3,
        };
        let mut edge = SUN + FIRST_GAP - GAP;
        let mut first = 0.0;
        for (lane, group) in systems.chunks_mut(sharing).enumerate() {
            let widest = group.iter().map(|s| s.reach).fold(0.0, f32::max);
            // Wide enough to clear the orbit inside it, and long enough for its systems side by side.
            let along: f32 = group.iter().map(|s| 2.0 * s.reach + GAP).sum();
            let orbit = (edge + GAP + widest).max(along / (TAU * 0.8));
            if lane == 0 {
                first = orbit;
            }
            let count = group.len() as f32;
            for (k, system) in group.iter_mut().enumerate() {
                system.orbit = orbit;
                // Each orbit starts a golden angle further round, so the planets begin well spread.
                system.start = 0.4 + lane as f32 * crate::math::GOLDEN + TAU * k as f32 / count;
                system.pace = 0.05 * (first / orbit).powf(1.5);
            }
            edge = orbit + widest;
        }
        let mut of_cluster = vec![None; graph.clusters.len()];
        for (k, system) in systems.iter().enumerate() {
            of_cluster[system.cluster] = Some(k);
        }
        Self {
            reach: edge.max(SUN + FIRST_GAP),
            systems,
            seen: vec![None; graph.nodes.len()],
            of_cluster,
            sun_seen: None,
        }
    }

    /// The camera for a frame of this size: at zoom 1 the whole system fits, with room for the names.
    #[must_use]
    pub fn lens(&self, camera: &Camera, width: f32, height: f32, unit: f32) -> Lens {
        let wide = (width - 70.0 * unit) / (2.0 * self.reach);
        let tall = (height - 96.0 * unit) / (2.0 * self.reach * SLANT.sin());
        Lens::level(camera, width, height, wide.min(tall).max(0.05))
    }

    fn planet_at(system: &System, time: f32) -> Vec3 {
        let (sin, cos) = (system.start + system.pace * time).sin_cos();
        Vec3::new(system.orbit * cos, 0.0, system.orbit * sin)
    }

    /// Where community `cluster`'s planet is at `time`; the sun's place when it has none.
    #[must_use]
    pub fn anchor(&self, cluster: usize, time: f32) -> Vec3 {
        self.system(cluster)
            .map_or(Vec3::ZERO, |system| Self::planet_at(system, time))
    }

    fn system(&self, cluster: usize) -> Option<&System> {
        self.of_cluster
            .get(cluster)
            .copied()
            .flatten()
            .and_then(|k| self.systems.get(k))
    }

    /// The radius of community `cluster`'s system, in world units; 0 when it is not drawn.
    #[must_use]
    pub fn system_reach(&self, cluster: usize) -> f32 {
        self.system(cluster).map_or(0.0, |system| system.reach)
    }

    /// The zoom at which community `cluster`'s system fills a good part of the frame.
    #[must_use]
    pub fn close_zoom(&self, cluster: usize) -> f32 {
        let reach = self.system_reach(cluster).max(SUN);
        (self.reach / (reach * 3.2)).clamp(1.3, 5.0)
    }

    /// Where node `node` was in the last frame drawn.
    #[must_use]
    pub fn seen(&self, node: usize) -> Option<Seen> {
        self.seen.get(node).copied().flatten().map(|(seen, _)| seen)
    }

    #[must_use]
    pub fn sun_seen(&self) -> Option<Seen> {
        self.sun_seen
    }

    /// The body under a pointer at `(x, y)`: the nearest within `reach` pixels of its edge.
    #[must_use]
    pub fn pick(&self, x: f32, y: f32, reach: f32) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (i, seen) in self.seen.iter().enumerate() {
            let Some((seen, radius)) = seen else { continue };
            let away = ((seen.x - x).hypot(seen.y - y) - radius).max(0.0);
            if away <= reach && best.map_or(true, |(_, least)| away < least) {
                best = Some((i, away));
            }
        }
        best.map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests;
