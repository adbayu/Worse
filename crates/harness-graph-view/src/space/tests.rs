use super::*;
use crate::model::tests::sample;

fn view() -> SpaceView {
    SpaceView {
        camera: Camera {
            yaw: 0.5,
            pitch: 0.11,
            ..Camera::default()
        },
        time: 2.0,
        birth: 1.0,
        focus: None,
        focus_mix: 0.0,
        hover: None,
        selected: None,
        unit: 1.0,
        slide: 0.0,
    }
}

fn render(space: &mut Space, graph: &Graph, view: &SpaceView) -> Vec<u8> {
    let mut data = vec![0u8; 640 * 360 * 4];
    let mut frame = Frame::new(&mut data, 640, 360, Blend::Add);
    space.draw(&mut frame, graph, view);
    data
}

#[test]
fn every_node_is_a_star_inside_the_galaxy_and_clear_of_the_hole() {
    let graph = Graph::parse(&sample(5, 60)).expect("graph");
    let space = Space::new(&graph);
    assert_eq!(space.stars.len(), graph.nodes.len());
    for star in &space.stars {
        let far = star.at.len();
        assert!((CLEARING - 0.01..=GALAXY + 0.01).contains(&far), "{far}");
    }
    assert!(!space.links.is_empty());
    // Communities stand apart: their middles are not all in one place.
    let apart = (space.anchor(0) - space.anchor(1)).len();
    assert!(apart > GALAXY * 0.3, "{apart}");
    // Linked stars stand closer together than stars picked at random.
    let mean = |pairs: &mut dyn Iterator<Item = (usize, usize)>| -> f32 {
        let (mut sum, mut n) = (0.0, 0);
        for (a, b) in pairs {
            sum += (space.stars[a].at - space.stars[b].at).len();
            n += 1;
        }
        sum / n.max(1) as f32
    };
    let linked = mean(&mut graph.links.iter().copied());
    let any = mean(&mut (0..graph.nodes.len() - 7).map(|i| (i, i + 7)));
    assert!(linked < any * 0.8, "{linked} against {any}");
}

#[test]
fn the_same_graph_is_the_same_sky() {
    let graph = Graph::parse(&sample(4, 40)).expect("graph");
    let (mut a, mut b) = (Space::new(&graph), Space::new(&graph));
    assert_eq!(
        render(&mut a, &graph, &view()),
        render(&mut b, &graph, &view())
    );
}

#[test]
fn a_frame_is_opaque_shows_stars_and_can_be_picked_from() {
    let graph = Graph::parse(&sample(4, 40)).expect("graph");
    let mut space = Space::new(&graph);
    let data = render(&mut space, &graph, &view());
    assert!(data.chunks_exact(4).all(|px| px[3] == 255));
    assert!(data.chunks_exact(4).filter(|px| px[0] > 200).count() > 200);
    let shown = (0..graph.nodes.len())
        .find(|&i| space.seen(i).is_some())
        .expect("a visible star");
    let seen = space.seen(shown).expect("seen");
    let picked = space.pick(seen.x + 1.0, seen.y, 8.0, None).expect("picked");
    let at = space.seen(picked).expect("seen");
    assert!((at.x - seen.x).hypot(at.y - seen.y) < 8.0);
    assert!(space.pick(-500.0, -500.0, 8.0, None).is_none());
    // In focus, only that community's stars answer.
    let other = (graph.nodes[shown].cluster + 1) % graph.clusters.len();
    assert!(space
        .pick(seen.x, seen.y, 2.0, Some(other))
        .map_or(true, |i| graph.nodes[i].cluster == other));
}

#[test]
fn light_from_behind_the_hole_is_bent_around_it() {
    let hole = Seen {
        x: 100.0,
        y: 100.0,
        depth: 30.0,
        scale: 20.0,
    };
    let at = |x: f32, y: f32, depth: f32| Seen {
        x,
        y,
        depth,
        scale: 20.0,
    };
    // In front of the hole nothing changes.
    assert_eq!(
        bent(at(110.0, 100.0, 25.0), Some(&hole)),
        Some(at(110.0, 100.0, 25.0))
    );
    // Right behind it: hidden by the shadow when close, pushed out onto a ring when further back.
    assert_eq!(bent(at(104.0, 100.0, 30.5), Some(&hole)), None);
    let ring = bent(at(100.5, 100.0, 36.0), Some(&hole)).expect("seen");
    let away = (ring.x - 100.0) / 20.0;
    assert!((away - (BEND * 6.0).sqrt()).abs() < 0.05, "{away}");
    // Far to the side it hardly moves, and never towards the hole.
    let side = bent(at(260.0, 100.0, 36.0), Some(&hole)).expect("seen");
    assert!(side.x >= 260.0 && side.x < 266.0, "{side:?}");
    assert_eq!(
        bent(at(110.0, 100.0, 60.0), None),
        Some(at(110.0, 100.0, 60.0))
    );
}

#[test]
fn focus_dims_the_other_communities_and_selection_draws_lines() {
    let graph = Graph::parse(&sample(4, 40)).expect("graph");
    let mut space = Space::new(&graph);
    let light = |data: &[u8]| -> u64 { data.chunks_exact(4).map(|px| u64::from(px[1])).sum() };
    let plain = light(&render(&mut space, &graph, &view()));
    let focused = light(&render(
        &mut space,
        &graph,
        &SpaceView {
            focus: Some(0),
            focus_mix: 1.0,
            ..view()
        },
    ));
    assert!(focused < plain, "{focused} < {plain}");
    let hub = graph.clusters[0].members[0];
    let chosen = light(&render(
        &mut space,
        &graph,
        &SpaceView {
            selected: Some(hub),
            ..view()
        },
    ));
    assert!(chosen > plain, "{chosen} > {plain}");
}

#[test]
fn nothing_shows_before_the_picture_arrives() {
    let graph = Graph::parse(&sample(3, 30)).expect("graph");
    let mut space = Space::new(&graph);
    let data = render(
        &mut space,
        &graph,
        &SpaceView {
            birth: 0.0,
            ..view()
        },
    );
    assert!(data.chunks_exact(4).all(|px| px[0] < 60));
}
