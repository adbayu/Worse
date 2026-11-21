use super::{falloff, lerp, Noise, Rng, TAU};

/// A pattern drawn once: across the disc (first index) by around it (second, wrapping).
pub(super) struct Pattern {
    pub(super) across: usize,
    pub(super) around: usize,
    pub(super) cells: Vec<f32>,
}

impl Pattern {
    /// `across` in `0..=1`; `turn` in turns, any number (it wraps).
    pub(super) fn at(&self, across: f32, turn: f32) -> f32 {
        let x = across * (self.across - 1) as f32;
        let i = (x as usize).min(self.across - 2);
        let tx = x - i as f32;
        let y = (turn - turn.floor()) * self.around as f32;
        let j = (y as usize).min(self.around - 1);
        let ty = y - j as f32;
        let k = if j + 1 == self.around { 0 } else { j + 1 };
        let row = i * self.around;
        let next = row + self.around;
        lerp(
            lerp(self.cells[row + j], self.cells[next + j], tx),
            lerp(self.cells[row + k], self.cells[next + k], tx),
            ty,
        )
    }

    /// The same pattern with half as many steps across the disc: each the mean of two.
    pub(super) fn halved(&self) -> Pattern {
        let across = self.across / 2;
        let mut cells = Vec::with_capacity(across * self.around);
        for i in 0..across {
            let (upper, lower) = (2 * i * self.around, (2 * i + 1) * self.around);
            for j in 0..self.around {
                cells.push(0.5 * (self.cells[upper + j] + self.cells[lower + j]));
            }
        }
        Pattern {
            across,
            around: self.around,
            cells,
        }
    }
}

/// A pattern at every fineness, the finest first: each has half as many steps across the disc as the one
/// before. A frame is drawn from the one whose steps are about a pixel wide, or strands thinner than a
/// pixel would flicker.
pub(super) struct Mips {
    pub(super) levels: Vec<Pattern>,
}

impl Mips {
    pub(super) fn new(finest: Pattern) -> Self {
        let mut levels = vec![finest];
        while levels[levels.len() - 1].across >= 8 {
            let next = levels[levels.len() - 1].halved();
            levels.push(next);
        }
        Self { levels }
    }

    /// Writes the pattern as it is at one moment into `out`, at fineness `level` (0 the finest; in between
    /// two levels it is a mix of them). Each lane across the disc has turned by its own speed: `turned`
    /// gives the turns of a lane, by its place across the disc, for the two copies of the flow, and
    /// `weight` is the share of the first copy.
    pub(super) fn bake(
        &self,
        level: f32,
        turned: impl Fn(f32) -> (f32, f32),
        weight: f32,
        out: &mut Pattern,
    ) {
        let last = self.levels.len() - 1;
        let level = level.clamp(0.0, last as f32);
        let fine = &self.levels[level as usize];
        let coarse = &self.levels[(level as usize + 1).min(last)];
        let mix = level - level.floor();
        let (rows, cols) = (fine.across, fine.around);
        out.across = rows;
        out.around = cols;
        out.cells.clear();
        out.cells.resize(rows * cols, 0.0);
        for i in 0..rows {
            let (first, second) = turned(i as f32 / (rows - 1) as f32);
            let fine_row = &fine.cells[i * cols..(i + 1) * cols];
            let coarse_at = (i / 2).min(coarse.across - 1) * cols;
            let coarse_row = &coarse.cells[coarse_at..coarse_at + cols];
            let into = &mut out.cells[i * cols..(i + 1) * cols];
            for (turn, share) in [(first, weight), (second, 1.0 - weight)] {
                if share <= 0.005 {
                    continue;
                }
                let shift = (turn - turn.floor()) * cols as f32;
                let whole = shift as usize;
                let part = shift - whole as f32;
                for (j, cell) in into.iter_mut().enumerate() {
                    let a = (j + whole) % cols;
                    let b = (a + 1) % cols;
                    let sharp = lerp(fine_row[a], fine_row[b], part);
                    let blurred = lerp(coarse_row[a], coarse_row[b], part);
                    *cell += share * lerp(sharp, blurred, mix);
                }
            }
        }
    }
}

/// How the strands of a pattern are laid.
pub(super) struct Weave {
    pub(super) across: usize,
    pub(super) around: usize,
    pub(super) strands: usize,
    /// Shortest and longest strand, in turns.
    pub(super) long: (f32, f32),
    /// Above 1 crowds the strands towards the hole.
    pub(super) crowd: f32,
    /// One strand in this many is a broad streak, several steps wide, that still shows when the disc is
    /// drawn small (0: none).
    pub(super) broad: usize,
    /// How far a strand strays from its circle, in disc widths, and how many times a turn it wavers.
    pub(super) stray: f32,
    pub(super) waves: (f32, f32),
    /// How far the whole flow is pushed off its circles, in disc widths.
    pub(super) warp: f32,
    /// How unevenly the light is spread in patches (0: evenly).
    pub(super) patchy: f32,
    /// Light everywhere under the strands, so the gaps between them are dim rather than black.
    pub(super) floor: f32,
    pub(super) seed: u64,
}

/// Lays thin strands along the flow, each the trail of a grain of gas: an arc that drifts inwards a little,
/// wavers, and fades at both ends. Where they crowd they merge into a glow; where they are few, each one
/// shows. The result is evened out so that its mean is about one at every distance from the hole.
pub(super) fn weave(plan: &Weave) -> Pattern {
    let (rows, cols) = (plan.across, plan.around);
    let mut rng = Rng::new(plan.seed);
    let noise = Noise::new(64, plan.seed ^ 0x5151);
    let mut cells = vec![0.0_f32; rows * cols];
    for strand in 0..plan.strands {
        let wide = if plan.broad > 0 && strand % plan.broad == 0 {
            rng.range(1.6, 4.2)
        } else {
            0.0
        };
        let across = rng.unit().powf(plan.crowd);
        let long = plan.long.0 + (plan.long.1 - plan.long.0) * rng.unit().powi(2);
        let start = rng.unit();
        // Short strands are faint hairs, long ones carry the light.
        let weight =
            (0.25 + 1.5 * rng.unit().powi(3)) * (0.35 + 0.65 * (long / plan.long.1).sqrt());
        let drift = (rng.unit() - 0.35) * 0.12 * long.sqrt();
        let stray = plan.stray * rng.unit();
        let waves = rng.range(plan.waves.0, plan.waves.1);
        let phase = rng.range(0.0, TAU);
        let steps = ((long * cols as f32) as usize).max(2);
        for k in 0..steps {
            let t = k as f32 / (steps - 1) as f32;
            let turn = start + t * long;
            let push = plan.warp * (noise.at_wrapped(across * 3.0 + 50.0, turn * 9.0, 9) - 0.5);
            let at = across + drift * t + stray * (TAU * waves * turn + phase).sin() + push;
            let row = at * (rows - 1) as f32;
            if row < 0.0 || row >= (rows - 1) as f32 {
                continue;
            }
            let col = ((turn - turn.floor()) * cols as f32) as usize % cols;
            let light = weight * (std::f32::consts::PI * t).sin().max(0.0).sqrt();
            if wide > 0.0 {
                // A streak with a soft edge, a few steps wide.
                let reach = (wide * 1.6) as usize + 1;
                let middle = row.round() as usize;
                for k in middle.saturating_sub(reach)..=(middle + reach).min(rows - 1) {
                    let away = (k as f32 - row) / wide;
                    cells[k * cols + col] += light * 0.75 * falloff(away * away);
                }
                continue;
            }
            let low = row as usize;
            let share = row - low as f32;
            cells[low * cols + col] += light * (1.0 - share);
            cells[(low + 1) * cols + col] += light * share;
        }
    }
    // Even out by distance: the mean of each row and its neighbours becomes one.
    let means: Vec<f32> = (0..rows)
        .map(|row| cells[row * cols..(row + 1) * cols].iter().sum::<f32>() / cols as f32)
        .collect();
    let span = (rows / 24).max(2);
    for row in 0..rows {
        let from = row.saturating_sub(span);
        let to = (row + span + 1).min(rows);
        let mean = means[from..to].iter().sum::<f32>() / (to - from) as f32;
        let scale = 1.0 / mean.max(0.02);
        let across = row as f32 / (rows - 1) as f32;
        for col in 0..cols {
            let turn = col as f32 / cols as f32;
            let patch = noise.at_wrapped(across * 5.0, turn * 8.0, 8) * 0.65
                + noise.at_wrapped(across * 11.0 + 7.0, turn * 20.0, 20) * 0.35;
            let patches = lerp(1.0, 0.1 + 1.8 * patch * patch * 2.0, plan.patchy);
            let cell = &mut cells[row * cols + col];
            *cell = ((plan.floor + (1.0 - plan.floor) * *cell * scale) * patches).min(4.0);
        }
    }
    Pattern {
        across: rows,
        around: cols,
        cells,
    }
}
