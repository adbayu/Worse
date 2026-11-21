use super::{
    lerp, smoothstep, Frame, Noise, Rng, Seen, Vec3, BEND, CLEARING, CLOUD_STEP, DISC_LEAN,
    FLATTEN, GALAXY, WHITE,
};

/// `count` places spread evenly through the galaxy: each is the best of a few tries, the one furthest from
/// all the places before it.
pub(super) fn scatter(count: usize, rng: &mut Rng) -> Vec<Vec3> {
    let mut places: Vec<Vec3> = Vec::with_capacity(count);
    for _ in 0..count {
        let mut best = (Vec3::ZERO, -1.0_f32);
        for _ in 0..10 {
            // Between even over the picture and even through the ball: a little denser towards the middle.
            let mut place = rng.direction() * (GALAXY * rng.unit().powf(1.0 / 2.4));
            place.y *= FLATTEN;
            if place.len() < CLEARING {
                place = place.unit() * (CLEARING + rng.unit());
            }
            let nearest = places
                .iter()
                .map(|other| {
                    let d = *other - place;
                    d.dot(d)
                })
                .fold(f32::MAX, f32::min);
            if nearest > best.1 {
                best = (place, nearest);
            }
        }
        places.push(best.0);
    }
    places
}

/// Two directions across `axis`, at right angles to each other: with the axis they make a right-handed set.
pub(super) fn plane(axis: Vec3) -> (Vec3, Vec3) {
    let helper = if axis.y.abs() < 0.9 {
        Vec3::UP
    } else {
        Vec3::new(1.0, 0.0, 0.0)
    };
    let e1 = axis.cross(helper).unit();
    (e1, axis.cross(e1))
}

pub(super) fn disc_axis() -> Vec3 {
    Vec3::new(DISC_LEAN.sin(), DISC_LEAN.cos(), 0.0)
}

/// Where something behind the hole is seen: its light is bent around the hole, so it appears pushed away
/// from the hole's middle, onto a ring when it stands right behind. `None` when it is hidden by the shadow.
pub(super) fn bent(seen: Seen, hole: Option<&Seen>) -> Option<Seen> {
    let Some(hole) = hole else {
        return Some(seen);
    };
    let behind = seen.depth - hole.depth;
    if behind <= 0.0 {
        return Some(seen);
    }
    let (dx, dy) = (seen.x - hole.x, seen.y - hole.y);
    let away = dx.hypot(dy) / hole.scale;
    if away > 12.0 {
        return Some(seen);
    }
    let lifted = 0.5 * (away + (away * away + 4.0 * BEND * behind).sqrt());
    if lifted < 1.0 {
        return None;
    }
    let stretch = lifted / away.max(1e-3);
    Some(Seen {
        x: hole.x + dx * stretch,
        y: hole.y + dy * stretch,
        ..seen
    })
}

/// Four long rays and four short ones between them.
pub(super) fn sparkle(frame: &mut Frame, x: f32, y: f32, reach: f32, gain: f32, width: f32) {
    if gain <= 0.01 || reach < 1.5 {
        return;
    }
    for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        frame.line(
            x,
            y,
            x + dx * reach,
            y + dy * reach,
            width,
            WHITE,
            gain,
            0.0,
        );
    }
    let short = reach * 0.33;
    for (dx, dy) in [(1.0, 1.0), (-1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)] {
        frame.line(
            x,
            y,
            x + dx * short,
            y + dy * short,
            width,
            WHITE,
            gain * 0.6,
            0.0,
        );
    }
}

/// Faint grey smoke on black, in blotches, darker towards the corners; RGBA, opaque.
pub(super) fn clouds(noise: &Noise, width: usize, height: usize, unit: f32) -> Vec<u8> {
    let columns = width / CLOUD_STEP + 2;
    let rows = height / CLOUD_STEP + 2;
    let mut coarse = Vec::with_capacity(columns * rows);
    for row in 0..rows {
        for column in 0..columns {
            // In logical pixels, so the smoke looks the same on any screen.
            let x = (column * CLOUD_STEP) as f32 / unit;
            let y = (row * CLOUD_STEP) as f32 / unit;
            let puffs = noise.fbm(x / 120.0, y / 120.0, 4);
            let patches = noise.fbm(x / 520.0 + 40.5, y / 520.0 + 9.5, 2);
            let edge_x = (x * unit / width.max(1) as f32 - 0.5) * 2.0;
            let edge_y = (y * unit / height.max(1) as f32 - 0.5) * 2.0;
            let corner = 1.0 - 0.3 * smoothstep(0.8, 1.5, edge_x.hypot(edge_y));
            coarse.push(
                smoothstep(0.3, 0.74, puffs) * smoothstep(0.36, 0.62, patches) * corner * 34.0,
            );
        }
    }
    let mut out = vec![0u8; width * height * 4];
    let step = CLOUD_STEP as f32;
    for y in 0..height {
        let fy = y as f32 / step;
        let row = fy as usize;
        let ty = fy - row as f32;
        for x in 0..width {
            let fx = x as f32 / step;
            let column = fx as usize;
            let tx = fx - column as f32;
            let i = row * columns + column;
            let grey = lerp(
                lerp(coarse[i], coarse[i + 1], tx),
                lerp(coarse[i + columns], coarse[i + columns + 1], tx),
                ty,
            );
            let o = (y * width + x) * 4;
            let level = (grey + 0.5) as u8;
            out[o] = level;
            out[o + 1] = level;
            out[o + 2] = level;
            out[o + 3] = 255;
        }
    }
    out
}
