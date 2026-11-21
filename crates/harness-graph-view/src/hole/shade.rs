use super::{
    clamp01, fast_atan2, lerp, smoothstep, Gas, Pattern, Rgb, Vec3, DISC_IN, DISC_OUT, HEAT,
    LANE_STEPS, TAU,
};

pub(super) fn heat(radius: f32) -> f32 {
    for pair in HEAT.windows(2) {
        let ((from, a), (to, b)) = (pair[0], pair[1]);
        if radius <= to {
            return lerp(a, b, clamp01((radius - from) / (to - from)));
        }
    }
    0.0
}

pub(super) fn ramp(across: f32) -> Rgb {
    const STOPS: [(f32, Rgb); 4] = [
        (0.0, [1.0, 0.94, 0.83]),
        (0.3, [1.0, 0.83, 0.64]),
        (0.62, [1.0, 0.74, 0.53]),
        (1.0, [1.0, 0.7, 0.5]),
    ];
    for pair in STOPS.windows(2) {
        let (from, a) = pair[0];
        let (to, b) = pair[1];
        if across <= to {
            let t = clamp01((across - from) / (to - from));
            return [
                lerp(a[0], b[0], t),
                lerp(a[1], b[1], t),
                lerp(a[2], b[2], t),
            ];
        }
    }
    STOPS[3].1
}

pub(super) struct Shade<'a> {
    pub(super) gas: &'a Gas,
    pub(super) plain: &'a Pattern,
    pub(super) band: &'a Pattern,
    pub(super) normal: Vec3,
    pub(super) e1: Vec3,
    pub(super) e2: Vec3,
    /// Seen edge-on the eye looks through more of the gas: it is brighter.
    pub(super) depth: f32,
    /// Seen edge-on, the far side lifted over the hole gives way to the band of rings around the shadow.
    pub(super) lifted: f32,
}

impl Shade<'_> {
    /// The light of the disc at `point` (`radius` from the hole): its colour times its strength, and how much
    /// it hides what is behind it. `squeeze` is 0 for the disc seen plainly and 1 for the far side seen
    /// around the hole.
    pub(super) fn emit(&self, point: Vec3, radius: f32, squeeze: f32) -> (Rgb, f32) {
        let across = (radius - DISC_IN) / (DISC_OUT - DISC_IN);
        if !(0.0..=1.0).contains(&across) {
            return ([0.0; 3], 0.0);
        }
        let at = across * (LANE_STEPS - 1) as f32;
        let i = (at as usize).min(LANE_STEPS - 2);
        let twist = lerp(self.gas.lanes[i].0, self.gas.lanes[i + 1].0, at - i as f32);
        let angle = fast_atan2(point.dot(self.e2), point.dot(self.e1));
        let turn = (angle + twist) / TAU;
        // The rim is ragged: how far out the gas reaches wavers around the disc, and turns with it.
        let reach = if across > 0.4 {
            let rag = self.band.at(0.3, turn) - 0.9;
            (across + 0.11 * rag * smoothstep(0.4, 1.0, across)).clamp(0.0, 1.0)
        } else {
            across
        };
        let at = reach * (LANE_STEPS - 1) as f32;
        let i = (at as usize).min(LANE_STEPS - 2);
        let body = lerp(self.gas.lanes[i].2, self.gas.lanes[i + 1].2, at - i as f32);
        let strands = if squeeze <= 0.01 {
            self.plain.at(across, turn)
        } else if squeeze >= 0.99 {
            self.band.at(across, turn)
        } else {
            lerp(
                self.plain.at(across, turn),
                self.band.at(across, turn),
                squeeze,
            )
        };
        // Gas coming towards the eye looks brighter than gas going away.
        let swing = self.normal.cross(point).z / radius;
        let beam = (1.0 + 0.2 * swing).powi(3);
        let strength = body * strands * beam * lerp(self.depth, self.lifted, squeeze);
        let colour = ramp(across);
        (
            [
                colour[0] * strength,
                colour[1] * strength,
                colour[2] * strength,
            ],
            clamp01(body * 2.2) * smoothstep(0.0, 0.02, across),
        )
    }
}
