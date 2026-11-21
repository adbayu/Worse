use super::{zoom_range, Engine, Press, Style, CLICK_SLOP, PICK_REACH};

impl Engine {
    /// Zooms by a factor (above 1 comes closer).
    pub fn zoom_by(&mut self, factor: f32) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let (low, high) = zoom_range(self.style);
        self.goal.zoom = (self.goal.zoom * factor).clamp(low, high);
    }

    pub fn pointer_down(&mut self, x: f32, y: f32) {
        self.press = Some(Press {
            from: (x, y),
            last: (x, y),
            moved: false,
        });
    }

    /// The pointer moved with the button held: the galaxy turns, the solar system slides.
    pub fn pointer_drag(&mut self, x: f32, y: f32) {
        let Some(press) = &mut self.press else {
            return;
        };
        let (dx, dy) = (x - press.last.0, y - press.last.1);
        press.last = (x, y);
        if !press.moved && (x - press.from.0).hypot(y - press.from.1) < CLICK_SLOP {
            return;
        }
        press.moved = true;
        match self.style {
            Style::Space => {
                self.goal.yaw -= dx * 0.008;
                self.goal.pitch = (self.goal.pitch + dy * 0.008).clamp(-1.4, 1.4);
                // Under the hand the view follows at once.
                self.camera.yaw = self.goal.yaw;
                self.camera.pitch = self.goal.pitch;
            }
            Style::Orbit => {
                let (width, height, unit) = (self.width as f32, self.height as f32, self.unit);
                let camera = self.camera;
                let lens = self.orbit().lens(&camera, width, height, unit);
                let Some(scale) = lens.see(camera.target).map(|seen| seen.scale / unit) else {
                    return;
                };
                let (right, up) = lens.axes();
                let slide = (right * -dx + up * dy) * (1.0 / scale.max(1e-3));
                // Sliding by hand lets go of a planet that was being followed.
                self.focus = None;
                self.goal.target = self.goal.target + slide;
                self.camera.target = self.camera.target + slide;
            }
        }
    }

    /// The button came up. A press that did not move is a click: on a node it chooses it (again: lets go of
    /// it), elsewhere it lets go of the chosen one. Returns whether it was a click.
    pub fn pointer_up(&mut self, x: f32, y: f32) -> bool {
        let Some(press) = self.press.take() else {
            return false;
        };
        if press.moved {
            return false;
        }
        let hit = self.pick(x, y);
        self.selected = match hit {
            Some(node) if self.selected == Some(node) => None,
            other => other,
        };
        true
    }

    /// The pointer moved with no button held; returns whether what it points at changed.
    pub fn pointer_move(&mut self, x: f32, y: f32) -> bool {
        let hit = self.pick(x, y);
        let changed = hit != self.hover;
        self.hover = hit;
        changed
    }

    pub fn pointer_leave(&mut self) -> bool {
        let changed = self.hover.is_some();
        self.hover = None;
        changed
    }

    pub(super) fn pick(&self, x: f32, y: f32) -> Option<usize> {
        let (x, y, reach) = (x * self.unit, y * self.unit, PICK_REACH * self.unit);
        match self.style {
            Style::Space => {
                self.space
                    .as_ref()?
                    .pick(x, y, reach, self.focus.filter(|_| self.focus_mix > 0.5))
            }
            Style::Orbit => self.orbit.as_ref()?.pick(x, y, reach * 0.6),
        }
    }
}
