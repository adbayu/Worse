use super::{
    disc_axis, plane, scatter, Comet, Grain, Graph, Hole, Link, Mote, Noise, Rng, Shell, Space,
    Star, Vec3, VecDeque, CLEARING, COOL, FLATTEN, GALAXY, GOLDEN, TAU, WARM, WHITE,
};

impl Space {
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn new(graph: &Graph) -> Self {
        let mut rng = Rng::new(0x5EED_CAFE);
        let count = graph.nodes.len();
        let groups = graph.clusters.len().max(1);
        // One place for every node, spread evenly through the galaxy.
        let places = scatter(count, &mut rng);
        // A patch of sky for each community: the largest near the galaxy's equator, where there is most
        // room, the smaller ones above and below, each a golden angle further round. Every place goes to
        // the community whose direction it is nearest to, as long as that community still needs places.
        let directions: Vec<Vec3> = (0..groups)
            .map(|c| {
                let side = if c % 2 == 0 { 1.0 } else { -1.0 };
                let height = if groups == 1 {
                    0.0
                } else {
                    side * ((c / 2) as f32 + 0.5) / groups.div_ceil(2) as f32 * 0.72
                };
                let around = c as f32 * GOLDEN + 0.6;
                let level = (1.0 - height * height).sqrt();
                Vec3::new(level * around.cos(), height, level * around.sin())
            })
            .collect();
        let mut choices: Vec<(f32, usize, usize)> = Vec::with_capacity(count * groups);
        for (p, place) in places.iter().enumerate() {
            let toward = place.unit();
            for (c, direction) in directions.iter().enumerate() {
                choices.push((1.0 - toward.dot(*direction), p, c));
            }
        }
        choices.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let mut room: Vec<usize> = graph.clusters.iter().map(|c| c.members.len()).collect();
        let mut owned: Vec<Vec<usize>> = vec![Vec::new(); groups];
        let mut taken = vec![false; count];
        for &(_, p, c) in &choices {
            if !taken[p] && room.get(c).copied().unwrap_or(0) > 0 {
                taken[p] = true;
                room[c] -= 1;
                owned[c].push(p);
            }
        }
        // Inside its patch a community grows from its hub: each node takes the free place nearest to the
        // node it was reached from, so linked stars stand close together.
        let mut at = vec![Vec3::ZERO; count];
        let mut placed = vec![false; count];
        let mut anchors = Vec::with_capacity(groups);
        for (c, cluster) in graph.clusters.iter().enumerate() {
            let mut free: Vec<Vec3> = owned[c].iter().map(|&p| places[p]).collect();
            let middle =
                free.iter().fold(Vec3::ZERO, |sum, &p| sum + p) * (1.0 / free.len().max(1) as f32);
            anchors.push(middle);
            let mut take = |near: Vec3| -> Vec3 {
                let mut best = 0;
                let mut best_away = f32::MAX;
                for (i, place) in free.iter().enumerate() {
                    let away = (*place - near).len();
                    if away < best_away {
                        best = i;
                        best_away = away;
                    }
                }
                if free.is_empty() {
                    near
                } else {
                    free.swap_remove(best)
                }
            };
            for &root in &cluster.members {
                if placed[root] {
                    continue;
                }
                at[root] = take(middle);
                placed[root] = true;
                let mut queue = VecDeque::from([root]);
                while let Some(node) = queue.pop_front() {
                    for &next in graph.neighbours(node) {
                        if placed[next] || graph.nodes[next].cluster != c {
                            continue;
                        }
                        at[next] = take(at[node]);
                        placed[next] = true;
                        queue.push_back(next);
                    }
                }
            }
        }
        let top = graph
            .nodes
            .iter()
            .map(|n| n.degree)
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let rank = |node: usize| (graph.nodes[node].degree as f32 / top).sqrt();
        let stars: Vec<Star> = graph
            .nodes
            .iter()
            .enumerate()
            .zip(&at)
            .map(|((i, node), &at)| {
                let rank = rank(i);
                let warmth = rng.unit();
                Star {
                    at,
                    // A few large soft lights among many small ones.
                    size: 0.42
                        + 0.9 * rank * rank
                        + if node.hub { 0.35 } else { 0.0 }
                        + 0.25 * rng.unit(),
                    light: 0.3 + 0.7 * rank.sqrt(),
                    flare: if node.hub || rank > 0.6 {
                        0.35 + 0.65 * rank
                    } else {
                        0.0
                    },
                    tint: if warmth < 0.3 {
                        WARM
                    } else if warmth > 0.85 {
                        COOL
                    } else {
                        WHITE
                    },
                    cluster: node.cluster,
                    beat: rng.range(0.0, TAU),
                    pace: rng.range(0.7, 2.1),
                }
            })
            .collect();
        // The constellation lines: links between stars that stand near each other. A line is brightest at
        // its better connected end and fades towards the other, so a hub reads as a burst of rays. Long
        // links are left out, they would only cross the picture; choosing a star shows all of its links.
        let volume = 4.19 * (GALAXY.powi(3) - CLEARING.powi(3)) * FLATTEN;
        let apart = (volume / count.max(1) as f32).cbrt();
        let longest = (apart * 1.5).max(1.5);
        let links = graph
            .links
            .iter()
            .filter_map(|&(a, b)| {
                let long = (at[a] - at[b]).len() / longest;
                (long < 1.0).then(|| {
                    let base = 0.035 + 0.1 * (1.0 - long);
                    Link {
                        a,
                        b,
                        light: (base * (0.3 + 1.5 * rank(a)), base * (0.3 + 1.5 * rank(b))),
                    }
                })
            })
            .collect();

        let motes = (0..1700)
            .map(|_| {
                let mut at = rng.direction() * (GALAXY * (0.3 + 0.95 * rng.unit().powf(0.6)));
                at.y *= 0.8;
                let dim = rng.unit();
                Mote {
                    at,
                    light: 0.06 + 0.4 * dim * dim * dim,
                }
            })
            .collect();
        // Grains around the hole: most of them in the plane of the gas disc, out beyond its rim, where the
        // disc thins into single grains; the rest a loose ball around the hole. They turn the way the gas
        // does, the inner ones faster.
        let axis = disc_axis();
        let grains = (0..1500)
            .map(|i| {
                let flat = i % 5 != 0;
                let (normal, radius) = if flat {
                    (
                        (axis + rng.direction() * 0.06).unit(),
                        3.3 + 4.0 * rng.unit().powf(1.7),
                    )
                } else {
                    (rng.direction(), 1.8 + 4.4 * rng.unit().powf(1.3))
                };
                let (e1, e2) = plane(normal);
                Grain {
                    e1,
                    e2,
                    radius,
                    start: rng.range(0.0, TAU),
                    pace: -0.36 * radius.powf(-1.5),
                    trail: rng.range(0.006, 0.03),
                    light: 0.14 + 0.5 * rng.unit().powi(2),
                }
            })
            .collect();
        let shells = (0..9)
            .map(|i| {
                let flat = i < 6;
                let (normal, radius) = if flat {
                    ((axis + rng.direction() * 0.03).unit(), 4.4 + 0.5 * i as f32)
                } else {
                    (rng.direction(), 2.0 + 0.6 * (i - 6) as f32)
                };
                let (e1, e2) = plane(normal);
                Shell {
                    e1,
                    e2,
                    radius,
                    count: (TAU * radius / 0.24) as usize,
                    pace: -0.36 * radius.powf(-1.5),
                    light: 0.2 - 0.012 * i as f32,
                }
            })
            .collect();
        let comets = (0..11)
            .map(|i| Comet {
                squash: rng.range(0.82, 1.0),
                lean: rng.range(0.0, TAU),
                radius: GALAXY * (1.18 + 0.6 * rng.unit()),
                pace: rng.range(0.07, 0.14) * if i % 3 == 0 { -1.0 } else { 1.0 },
                start: rng.range(0.0, TAU),
                tail: rng.range(0.5, 1.2),
            })
            .collect();
        Self {
            stars,
            links,
            anchors,
            motes,
            grains,
            shells,
            comets,
            hole: Hole::new(),
            clouds: Noise::new(64, 0x00C1_00D5),
            backdrop: Vec::new(),
            backdrop_size: (0, 0),
            seen: Vec::new(),
            hole_seen: None,
        }
    }
}
