//! The black hole in the middle of the galaxy picture.
//!
//! It is shaded pixel by pixel, in units of the shadow's radius, into a small picture of its own that holds
//! more light than a screen can show, and that picture is then given a camera's bloom: its light spreads,
//! veils the shadow in a warm haze and burns out to white where the gas is hottest.
//!
//! A flat disc of gas turns around the hole. It is drawn from a pattern of thousands of thin strands laid
//! along the flow, the trails of grains of gas, which is what makes it look like matter rather than a
//! gradient. The eye's rays are bent on their way past the hole: each one runs straight to the point where it
//! comes nearest and leaves that point turned towards the hole by `BEND / distance` radians. That one bend
//! is a rough stand-in for the real curved path, and it is enough to lift the far half of the disc into
//! view over the hole, whichever side the camera looks from. Light that went all the way around the hole
//! is not traced: it is drawn as what it adds up to, a band of thin rings that hugs the shadow.

use crate::math::{clamp01, falloff, fast_atan2, lerp, smoothstep, Noise, Rng, Vec3, TAU};
use crate::raster::{Frame, Rgb};

mod draw;
mod pass;
mod shade;
mod weave;

use pass::{blur, grow_add, shrink, Pass};
use shade::{heat, Shade};
use weave::{weave, Mips, Pattern, Weave};

/// Where the gas begins and ends, in shadow radii.
const DISC_IN: f32 = 1.08;
const DISC_OUT: f32 = 4.15;
/// How far from the middle the hole's own picture reaches: the gas and the room its light spreads into.
const REACH: f32 = 6.2;
/// The bend of a ray that passes at distance `b` is `BEND / b`.
pub const BEND: f32 = 0.35;
/// Rays that pass further out than this are not bent.
const BEND_REACH: f32 = 6.0;
const BEND_STEPS: usize = 512;
const LANE_STEPS: usize = 160;
/// The gas flows faster near the hole, which would wind the strands ever tighter; two copies of the pattern
/// start over in turn, half a period apart, and are cross-faded so no restart shows.
const FLOW_PERIOD: f32 = 50.0;
/// How bright the gas is by distance from the hole (before the eye's bloom), measured off the look aimed for.
const HEAT: [(f32, f32); 10] = [
    (1.08, 0.74),
    (1.3, 0.68),
    (1.6, 0.54),
    (1.9, 0.44),
    (2.5, 0.3),
    (3.0, 0.18),
    (3.4, 0.115),
    (3.7, 0.07),
    (3.95, 0.035),
    (4.15, 0.0),
];
/// Light up to this strength is shown as it is; above it, it is eased towards the screen's full strength.
const SHOULDER: f32 = 0.6;
/// How wide the band of rings around the shadow is.
const BAND: f32 = 0.44;
/// How much of the light each width of the bloom adds, narrowest first.
const BLOOM: [f32; 5] = [0.16, 0.2, 0.22, 0.24, 0.24];

/// One frame's view of the hole.
#[derive(Debug, Clone, Copy)]
pub struct HoleView {
    /// The middle, in pixels.
    pub x: f32,
    pub y: f32,
    /// The shadow's radius, in pixels.
    pub radius: f32,
    /// The disc's axis as the camera sees it: x to the right, y up, z away from the eye.
    pub normal: Vec3,
    pub time: f32,
    /// 0 to 1: fades the whole thing in.
    pub gain: f32,
}

/// What the shading reads; the pictures it writes are kept apart so several threads can share this.
struct Gas {
    /// Thin strands of gas, for the disc seen plainly.
    strands: Mips,
    /// A few long thin rings: for the band around the shadow and the far side seen around the hole, where
    /// the whole disc is squeezed into a narrow strip.
    rings: Mips,
    /// Sine and cosine of the bend, by how near the ray passes (`0..BEND_REACH`).
    bend: Vec<(f32, f32)>,
    /// By place across the disc (`0..1`): the twist of the spiral there, how fast the gas turns, its heat.
    lanes: Vec<(f32, f32, f32)>,
}

pub struct Hole {
    gas: Gas,
    /// The two patterns as they are in the frame being drawn.
    plain: Pattern,
    band: Pattern,
    /// The hole's own picture: light (red, green, blue) and how much of what is behind shows through.
    light: Vec<f32>,
    keep: Vec<f32>,
    /// That picture ready for the screen (red, green, blue, and how much of the frame stays, a byte each),
    /// where it lies on the frame, and the view it was made for. The hole changes slowly: while it stays
    /// where it is, every other frame lays the last picture down again instead of shading a new one.
    layer: Vec<u8>,
    layer_box: (usize, usize, usize, usize),
    layer_view: Option<(f32, f32, f32, usize, usize)>,
    rested: u32,
}

impl Default for Hole {
    fn default() -> Self {
        Self::new()
    }
}

impl Hole {
    #[must_use]
    pub fn new() -> Self {
        let fine = weave(&Weave {
            across: 256,
            around: 1024,
            strands: 26000,
            long: (0.008, 0.1),
            crowd: 1.25,
            broad: 9,
            stray: 0.02,
            waves: (12.0, 44.0),
            warp: 0.07,
            patchy: 0.9,
            floor: 0.15,
            seed: 0x00B1_ACC0,
        });
        let rings = weave(&Weave {
            across: 96,
            around: 1024,
            strands: 90,
            long: (0.2, 0.95),
            crowd: 1.0,
            broad: 0,
            stray: 0.004,
            waves: (3.0, 9.0),
            warp: 0.0,
            patchy: 0.3,
            floor: 0.4,
            seed: 0x0000_0D15,
        });
        let bend = (0..=BEND_STEPS)
            .map(|i| {
                let near = (i as f32 * BEND_REACH / BEND_STEPS as f32).max(0.5);
                // It fades to nothing at BEND_REACH, so the picture has no seam there.
                (BEND / near * (1.0 - smoothstep(4.4, BEND_REACH, near))).sin_cos()
            })
            .collect();
        let lanes = (0..LANE_STEPS)
            .map(|i| {
                let radius = DISC_IN + (DISC_OUT - DISC_IN) * i as f32 / (LANE_STEPS - 1) as f32;
                // Turns a second: Kepler's law, inner lanes faster.
                (
                    2.2 * radius.ln(),
                    -0.022 * (DISC_IN / radius).powf(1.5),
                    heat(radius),
                )
            })
            .collect();
        let empty = || Pattern {
            across: 2,
            around: 2,
            cells: vec![1.0; 4],
        };
        Self {
            gas: Gas {
                strands: Mips::new(fine),
                rings: Mips::new(rings),
                bend,
                lanes,
            },
            plain: empty(),
            band: empty(),
            light: Vec::new(),
            keep: Vec::new(),
            layer: Vec::new(),
            layer_box: (0, 0, 0, 0),
            layer_view: None,
            rested: 0,
        }
    }

    /// How far from the middle the hole's light reaches, in shadow radii.
    #[must_use]
    pub fn reach() -> f32 {
        REACH
    }
}

#[cfg(test)]
mod tests;
