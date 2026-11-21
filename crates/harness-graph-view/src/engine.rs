//! One picture of a graph that can be looked around in: it owns the camera, moves it smoothly towards where
//! it was asked to go, answers the pointer and draws frames in either style.
//!
//! Positions that come in from the pointer and go out for labels are in logical pixels (the unit a UI lays
//! out in); the frame itself may have more pixels than that, or fewer when it would be too many to draw.

use std::sync::Arc;

use crate::camera::{Camera, Seen};
use crate::math::{lerp, Vec3, TAU};
use crate::model::Graph;
use crate::orbit::{Orbit, OrbitView, Palette, SLANT};
use crate::raster::{Blend, Frame};
use crate::space::{Space, SpaceView};

mod frame;
mod input;

/// A frame is never larger than this many pixels; a bigger canvas is drawn smaller and stretched.
pub const MAX_PIXELS: usize = 1_500_000;
/// Seconds the picture takes to arrive.
const ARRIVE: f32 = 1.3;
/// The galaxy turns this fast on its own, in radians a second.
const SPIN: f32 = 0.055;
/// How far the pointer may move between press and release and still count as a click.
const CLICK_SLOP: f32 = 4.0;
/// How near a star or a body the pointer must be to pick it.
const PICK_REACH: f32 = 11.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    /// The flat solar system.
    #[default]
    Orbit,
    /// The galaxy around a black hole, in 3D.
    Space,
}

impl Style {
    /// "space" is the galaxy; anything else is the solar system.
    #[must_use]
    pub fn named(name: &str) -> Self {
        if name == "space" {
            Self::Space
        } else {
            Self::Orbit
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Orbit => "orbit",
            Self::Space => "space",
        }
    }
}

/// A place on the canvas, in logical pixels, and how strongly to show what stands there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Marker {
    pub x: f32,
    pub y: f32,
    pub alpha: f32,
}

/// What a UI lays over a frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Overlay {
    /// Where each community's name goes: (index into the graph's clusters, place).
    pub labels: Vec<(usize, Marker)>,
    /// Where the name of the middle (the hole, the sun) goes.
    pub core: Option<Marker>,
    pub hover: Option<(usize, Marker)>,
    pub selected: Option<(usize, Marker)>,
}

struct Press {
    from: (f32, f32),
    last: (f32, f32),
    moved: bool,
}

pub struct Engine {
    graph: Arc<Graph>,
    style: Style,
    space: Option<Space>,
    orbit: Option<Orbit>,
    palette: Palette,
    width: usize,
    height: usize,
    unit: f32,
    time: f32,
    birth: f32,
    camera: Camera,
    goal: Camera,
    focus: Option<usize>,
    focus_mix: f32,
    hover: Option<usize>,
    selected: Option<usize>,
    paused: bool,
    press: Option<Press>,
    /// Room kept free on the right for a panel, in logical pixels, and how much of it the picture has
    /// made so far.
    inset: f32,
    made: f32,
}

fn home(style: Style) -> Camera {
    match style {
        Style::Space => Camera {
            yaw: 0.6,
            pitch: 0.11,
            zoom: 1.0,
            target: Vec3::ZERO,
        },
        Style::Orbit => Camera {
            yaw: 0.0,
            pitch: SLANT,
            zoom: 1.0,
            target: Vec3::ZERO,
        },
    }
}

fn zoom_range(style: Style) -> (f32, f32) {
    match style {
        Style::Space => (0.55, 3.0),
        Style::Orbit => (0.6, 8.0),
    }
}

impl Engine {
    #[must_use]
    pub fn new(graph: Arc<Graph>, style: Style) -> Self {
        Self {
            graph,
            style,
            space: None,
            orbit: None,
            palette: Palette::default(),
            width: 0,
            height: 0,
            unit: 1.0,
            time: 0.0,
            birth: 0.0,
            camera: home(style),
            goal: home(style),
            focus: None,
            focus_mix: 0.0,
            hover: None,
            selected: None,
            paused: false,
            press: None,
            inset: 0.0,
            made: 0.0,
        }
    }

    #[must_use]
    pub fn graph(&self) -> &Arc<Graph> {
        &self.graph
    }

    #[must_use]
    pub fn style(&self) -> Style {
        self.style
    }

    /// Switches the picture; it arrives anew, seen from its home view.
    pub fn set_style(&mut self, style: Style) {
        if style == self.style {
            return;
        }
        self.style = style;
        self.camera = home(style);
        self.goal = home(style);
        self.birth = 0.0;
        self.focus = None;
        self.focus_mix = 0.0;
        self.hover = None;
        self.selected = None;
        self.press = None;
    }

    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }

    /// The canvas: its size in device pixels and how many of those make a logical pixel.
    pub fn resize(&mut self, width: usize, height: usize, scale: f32) {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let pixels = width * height;
        let shrink = if pixels > MAX_PIXELS {
            (MAX_PIXELS as f32 / pixels as f32).sqrt()
        } else {
            1.0
        };
        self.width = (width as f32 * shrink).round() as usize;
        self.height = (height as f32 * shrink).round() as usize;
        self.unit = scale * shrink;
    }

    /// The size of a frame in pixels: [`Engine::render`] wants four bytes for each.
    #[must_use]
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    #[must_use]
    pub fn focus(&self) -> Option<usize> {
        self.focus
    }

    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    #[must_use]
    pub fn hover(&self) -> Option<usize> {
        self.hover
    }

    #[must_use]
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Stops the clock: nothing turns or flies until it is started again. Looking around still works.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    fn space(&mut self) -> &mut Space {
        let graph = &self.graph;
        self.space.get_or_insert_with(|| Space::new(graph))
    }

    fn orbit(&mut self) -> &mut Orbit {
        let graph = &self.graph;
        self.orbit.get_or_insert_with(|| Orbit::new(graph))
    }

    /// Keeps `width` logical pixels on the right free (a panel lies there): the picture moves over so its
    /// middle is the middle of what is left.
    pub fn set_inset(&mut self, width: f32) {
        self.inset = if width.is_finite() {
            width.max(0.0)
        } else {
            0.0
        };
    }

    fn slide(&self) -> f32 {
        -self.made * self.unit * 0.5
    }

    /// Skips the arrival: the picture is all there at once, for those who asked for less motion.
    pub fn arrive(&mut self) {
        self.birth = 1.0;
        self.camera = self.goal;
        self.made = self.inset;
    }

    /// Looks at one community (the others dim), or at everything again.
    pub fn set_focus(&mut self, cluster: Option<usize>) {
        let cluster = cluster.filter(|&c| c < self.graph.clusters.len());
        self.focus = cluster;
        match (self.style, cluster) {
            (Style::Space, Some(c)) => {
                // Bring it to the near side, a little to one side of the hole, and step closer.
                let anchor = self.space().anchor(c);
                let facing = anchor.x.atan2(anchor.z) - 0.5;
                let turns = ((self.goal.yaw - facing) / TAU).round();
                self.goal.yaw = facing + turns * TAU;
                self.goal.target = anchor * 0.3;
                self.goal.zoom = 1.25;
            }
            (Style::Orbit, Some(c)) => {
                self.goal.zoom = self.orbit().close_zoom(c);
            }
            (_, None) => {
                self.goal.target = Vec3::ZERO;
                self.goal.zoom = 1.0;
            }
        }
    }

    /// Chooses a node (its links light up), or none. With `show`, the view also turns to it: for a node
    /// picked from a list rather than on the picture.
    pub fn select(&mut self, node: Option<usize>, show: bool) {
        let node = node.filter(|&n| n < self.graph.nodes.len());
        self.selected = node;
        let Some(node) = node.filter(|_| show) else {
            return;
        };
        let cluster = self.graph.nodes[node].cluster;
        if self.focus.is_some_and(|c| c != cluster) {
            self.set_focus(Some(cluster));
        }
        if self.style == Style::Space {
            if let Some(at) = self.space().place(node) {
                // Bring the star to the near side, a little off the hole.
                let facing = at.x.atan2(at.z) + 0.35;
                let turns = ((self.goal.yaw - facing) / TAU).round();
                self.goal.yaw = facing + turns * TAU;
            }
        }
    }

    /// Back to the home view, nothing chosen.
    pub fn reset(&mut self) {
        let yaw = self.goal.yaw;
        self.goal = home(self.style);
        if self.style == Style::Space {
            // Keep turning from where it is instead of spinning all the way back.
            self.goal.yaw = yaw;
        }
        self.focus = None;
        self.selected = None;
    }
}

#[cfg(test)]
mod tests;
