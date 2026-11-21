//! The galaxy picture: every node a star, every community a constellation, all of it around a black hole
//! and seen through a camera that can circle it.
//!
//! Nothing here is simulated. Each community gets a patch of sky and grows from its best connected node
//! outwards, link by link, so linked stars stand close and the lines between them stay short. Dust, the
//! grains that circle the hole and the comets beyond the stars are scenery, drawn from one fixed seed: the
//! same graph is the same sky every time.

use std::collections::VecDeque;

use crate::camera::{Camera, Lens, Seen};
use crate::hole::{Hole, HoleView, BEND};
use crate::math::{lerp, smoothstep, Noise, Rng, Vec3, GOLDEN, TAU};
use crate::model::Graph;
use crate::raster::{Blend, Frame, Rgb};

mod build;
mod render;
mod shapes;

use shapes::{bent, clouds, disc_axis, plane, scatter, sparkle};

/// How far the stars reach, in radii of the hole's shadow (the unit of this world).
pub const GALAXY: f32 = 9.5;
/// At zoom 1 the galaxy's radius is this share of the frame's shorter side.
const SPREAD: f32 = 0.43;
/// At zoom 1 the eye stands this many galaxy radii away.
const STAND: f32 = 3.4;
/// No star nearer the hole than this.
const CLEARING: f32 = 4.7;
/// The galaxy is a ball pressed a little flat.
const FLATTEN: f32 = 0.86;
/// The gas disc leans a little, so its bar tips one way and then the other as the camera goes round.
const DISC_LEAN: f32 = 0.09;
const WHITE: Rgb = [1.0, 1.0, 1.0];
const COOL: Rgb = [0.8, 0.87, 1.0];
const WARM: Rgb = [1.0, 0.93, 0.8];
/// The cloud layer is worked out at a quarter of the frame's size and stretched.
const CLOUD_STEP: usize = 4;

struct Star {
    at: Vec3,
    size: f32,
    light: f32,
    /// Length of the four rays of a bright star; 0 for none.
    flare: f32,
    tint: Rgb,
    cluster: usize,
    beat: f32,
    pace: f32,
}

struct Link {
    a: usize,
    b: usize,
    /// How bright the line is at each of its ends: brightest at the better connected star.
    light: (f32, f32),
}

struct Mote {
    at: Vec3,
    light: f32,
}

/// A grain of dust circling the hole, seen as a short dash along its path.
struct Grain {
    e1: Vec3,
    e2: Vec3,
    radius: f32,
    start: f32,
    pace: f32,
    trail: f32,
    light: f32,
}

/// A ring of evenly spaced dots turning around the hole: an orbit crowded with grains.
struct Shell {
    e1: Vec3,
    e2: Vec3,
    radius: f32,
    count: usize,
    pace: f32,
    light: f32,
}

/// A bright head on a wide circle around the whole galaxy, with a tail along its path. The circle always
/// faces the eye, a little squashed and turned, so the comets stay at the rim of the picture whichever way
/// the camera looks.
struct Comet {
    squash: f32,
    lean: f32,
    radius: f32,
    pace: f32,
    start: f32,
    tail: f32,
}

/// What one frame shows.
#[derive(Debug, Clone, Copy)]
pub struct SpaceView {
    pub camera: Camera,
    /// Seconds; drives the gas, the grains, the comets and the twinkle.
    pub time: f32,
    /// 0 to 1: the picture arriving.
    pub birth: f32,
    /// The community being looked at, and how far the others have dimmed (0 to 1).
    pub focus: Option<usize>,
    pub focus_mix: f32,
    pub hover: Option<usize>,
    pub selected: Option<usize>,
    /// Frame pixels per logical pixel: sizes are given in logical pixels.
    pub unit: f32,
    /// How far the middle of the picture is moved to the right, in frame pixels.
    pub slide: f32,
}

pub struct Space {
    stars: Vec<Star>,
    links: Vec<Link>,
    anchors: Vec<Vec3>,
    motes: Vec<Mote>,
    grains: Vec<Grain>,
    shells: Vec<Shell>,
    comets: Vec<Comet>,
    hole: Hole,
    clouds: Noise,
    backdrop: Vec<u8>,
    backdrop_size: (usize, usize),
    seen: Vec<Option<Seen>>,
    hole_seen: Option<Seen>,
}

impl Space {
    /// The camera for a frame of this size.
    #[must_use]
    pub fn lens(camera: &Camera, width: f32, height: f32) -> Lens {
        let fit = height.min(width * 0.62);
        Lens::deep(camera, width, height, GALAXY, SPREAD * fit, STAND)
    }

    /// The middle of community `cluster`, where its name goes.
    #[must_use]
    pub fn anchor(&self, cluster: usize) -> Vec3 {
        self.anchors.get(cluster).copied().unwrap_or(Vec3::ZERO)
    }

    /// Where node `node` stands in the galaxy.
    #[must_use]
    pub fn place(&self, node: usize) -> Option<Vec3> {
        self.stars.get(node).map(|star| star.at)
    }

    /// Where node `node` was in the last frame drawn; `None` also while the hole hides it.
    #[must_use]
    pub fn seen(&self, node: usize) -> Option<Seen> {
        self.seen.get(node).copied().flatten()
    }

    /// Where the hole was in the last frame drawn; its `scale` is the shadow's radius in pixels.
    #[must_use]
    pub fn hole_seen(&self) -> Option<Seen> {
        self.hole_seen
    }

    /// Where a point of the galaxy shows on the frame drawn last, its light bent around the hole like the
    /// stars'; `None` when the hole hides it.
    #[must_use]
    pub fn shown(&self, seen: Seen) -> Option<Seen> {
        bent(seen, self.hole_seen.as_ref())
    }

    /// The star under a pointer at `(x, y)`, within `reach` pixels: the nearest on the frame, and of two
    /// about as near, the one nearer the eye. With a community in focus only its stars count.
    #[must_use]
    pub fn pick(&self, x: f32, y: f32, reach: f32, focus: Option<usize>) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (i, seen) in self.seen.iter().enumerate() {
            let Some(seen) = seen else { continue };
            if focus.is_some_and(|c| self.stars[i].cluster != c) {
                continue;
            }
            let away = (seen.x - x).hypot(seen.y - y);
            if away > reach {
                continue;
            }
            // A star nearer the eye wins over one a couple of pixels closer to the pointer.
            let score = away + seen.depth * 0.08;
            if best.map_or(true, |(_, least)| score < least) {
                best = Some((i, score));
            }
        }
        best.map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests;
