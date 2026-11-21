use super::*;
use crate::model::tests::sample;

fn view() -> OrbitView {
    OrbitView {
        camera: Camera {
            pitch: SLANT,
            ..Camera::default()
        },
        time: 0.0,
        birth: 1.0,
        focus: None,
        focus_mix: 0.0,
        hover: None,
        selected: None,
        unit: 1.0,
        slide: 0.0,
        palette: Palette::default(),
    }
}

fn render(orbit: &mut Orbit, graph: &Graph, view: &OrbitView) -> Vec<u8> {
    let mut data = vec![0u8; 1000 * 532 * 4];
    let mut frame = Frame::new(&mut data, 1000, 532, Blend::Over);
    orbit.draw(&mut frame, graph, view);
    data
}

#[test]
fn systems_never_touch_and_small_ones_are_innermost() {
    let graph = Graph::parse(&sample(5, 30)).expect("graph");
    let orbit = Orbit::new(&graph);
    assert_eq!(orbit.systems.len(), 5);
    for system in &orbit.systems {
        assert!(
            system.orbit - system.reach >= SUN + FIRST_GAP - 0.01,
            "a system reaches into the sun"
        );
        assert!(system.orbit + system.reach <= orbit.reach + 0.01);
        assert_eq!(system.moons.len(), 29);
        // Moons of one ring have room: no two closer than a moon is wide.
        for ring in &system.rings {
            let on_ring = system
                .moons
                .iter()
                .filter(|m| (m.ring - ring).abs() < 0.01)
                .count();
            assert!(TAU * ring / on_ring as f32 >= MOON_ROOM - 0.01);
        }
    }
    // Whatever the number of communities, no two systems touch, now or later.
    for groups in 1..=10 {
        let graph = Graph::parse(&sample(groups, 9 + groups * 3)).expect("graph");
        let orbit = Orbit::new(&graph);
        assert_eq!(orbit.systems.len(), groups);
        for time in [0.0, 37.0, 900.0] {
            for (i, a) in orbit.systems.iter().enumerate() {
                for b in &orbit.systems[i + 1..] {
                    let apart = (Orbit::planet_at(a, time) - Orbit::planet_at(b, time)).len();
                    assert!(
                        apart >= a.reach + b.reach,
                        "{groups} communities at {time}: {apart}"
                    );
                }
            }
        }
    }
    let uneven = Graph::parse(
        r#"{"nodes":[{"id":"a","community":0},{"id":"b","community":0},{"id":"c","community":0},{"id":"d","community":1}],"links":[]}"#,
    )
    .expect("graph");
    let orbit = Orbit::new(&uneven);
    assert_eq!(
        orbit.systems[0].cluster, 1,
        "the one-node community circles closest to the sun"
    );
}

#[test]
fn the_whole_system_fits_the_frame_and_every_body_can_be_picked() {
    let graph = Graph::parse(&sample(6, 40)).expect("graph");
    let mut orbit = Orbit::new(&graph);
    let data = render(&mut orbit, &graph, &view());
    // See-through outside the picture, painted inside.
    assert_eq!(&data[..4], &[0, 0, 0, 0]);
    assert!(data.chunks_exact(4).filter(|px| px[3] > 200).count() > 2000);
    for node in 0..graph.nodes.len() {
        let seen = orbit.seen(node).expect("every node is drawn");
        assert!(
            (0.0..1000.0).contains(&seen.x) && (0.0..532.0).contains(&seen.y),
            "{seen:?}"
        );
        assert_eq!(orbit.pick(seen.x, seen.y, 3.0), Some(node));
    }
    assert!(orbit.pick(3.0, 3.0, 3.0).is_none());
    let sun = orbit.sun_seen().expect("sun");
    assert!((sun.x - 500.0).abs() < 0.5 && (sun.y - 266.0).abs() < 0.5);
}

#[test]
fn planets_move_and_a_big_graph_is_cut_short() {
    let graph = Graph::parse(&sample(4, 150)).expect("graph");
    let mut orbit = Orbit::new(&graph);
    let bodies: usize = orbit.systems.iter().map(|s| 1 + s.moons.len()).sum();
    assert_eq!(bodies, MAX_BODIES);
    let before = orbit.anchor(0, 0.0);
    let after = orbit.anchor(0, 20.0);
    assert!((before - after).len() > 1.0);
    assert!(
        (before.len() - after.len()).abs() < 1e-3,
        "it stays on its orbit"
    );
    render(&mut orbit, &graph, &view());
    assert!(orbit.seen(MAX_BODIES).is_none());
    assert!(orbit.close_zoom(0) >= 1.3);
}

#[test]
fn choosing_a_node_draws_its_links_and_dims_the_rest() {
    let graph = Graph::parse(&sample(3, 20)).expect("graph");
    let mut orbit = Orbit::new(&graph);
    let hub = graph.clusters[0].members[0];
    let plain = render(&mut orbit, &graph, &view());
    let chosen = render(
        &mut orbit,
        &graph,
        &OrbitView {
            selected: Some(hub),
            ..view()
        },
    );
    assert_ne!(plain, chosen);
    // A moon of another community is not linked to the hub: it is paler than before.
    let other = graph.clusters[1].members[3];
    let seen = orbit.seen(other).expect("seen");
    let alpha = |data: &[u8]| data[((seen.y as usize) * 1000 + seen.x as usize) * 4 + 3];
    assert!(alpha(&chosen) < alpha(&plain));
}
