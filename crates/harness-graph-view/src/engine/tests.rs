use super::*;
use crate::model::tests::sample;

fn engine(style: Style) -> Engine {
    let graph = Arc::new(Graph::parse(&sample(4, 30)).expect("graph"));
    let mut engine = Engine::new(graph, style);
    engine.resize(800, 440, 1.0);
    engine
}

fn frame(engine: &mut Engine) -> Vec<u8> {
    let (width, height) = engine.size();
    let mut data = vec![0u8; width * height * 4];
    engine.render(&mut data);
    data
}

fn settle(engine: &mut Engine) {
    for _ in 0..120 {
        engine.step(0.05);
    }
}

#[test]
fn both_styles_draw_and_name_every_community() {
    for style in [Style::Orbit, Style::Space] {
        let mut engine = engine(style);
        settle(&mut engine);
        let data = frame(&mut engine);
        assert!(data.iter().any(|b| *b > 100), "{style:?}");
        let overlay = engine.overlay();
        assert!(overlay.labels.len() >= 3, "{style:?}");
        assert!(
            overlay.labels.iter().all(|(_, m)| m.alpha > 0.1),
            "{style:?}"
        );
        let core = overlay.core.expect("core");
        assert!(
            (core.x - 400.0).abs() < 2.0 && core.y > 220.0,
            "{style:?} {core:?}"
        );
    }
    assert_eq!(Style::named("space"), Style::Space);
    assert_eq!(Style::named("anything"), Style::Orbit);
    assert_eq!(Style::named(Style::Orbit.name()), Style::Orbit);
}

#[test]
fn a_click_chooses_a_node_and_a_second_click_lets_go() {
    for style in [Style::Orbit, Style::Space] {
        let mut engine = engine(style);
        engine.set_paused(true);
        settle(&mut engine);
        frame(&mut engine);
        // Find a node on the picture by asking where the hovered one is.
        let mut found = None;
        'search: for y in (20..420).step_by(6) {
            for x in (20..780).step_by(6) {
                if engine.pointer_move(x as f32, y as f32) {
                    if let Some(node) = engine.hover() {
                        found = Some((node, x as f32, y as f32));
                        break 'search;
                    }
                }
            }
        }
        let (node, x, y) = found.expect("a node under the pointer somewhere");
        engine.pointer_down(x, y);
        assert!(engine.pointer_up(x, y));
        assert_eq!(engine.selected(), Some(node), "{style:?}");
        frame(&mut engine);
        let at = engine.overlay().selected.expect("marker").1;
        assert!((at.x - x).hypot(at.y - y) < 20.0, "{style:?}");
        engine.pointer_down(x, y);
        engine.pointer_up(x, y);
        assert_eq!(engine.selected(), None, "{style:?}");
    }
}

#[test]
fn dragging_turns_the_galaxy_and_is_not_a_click() {
    let mut engine = engine(Style::Space);
    settle(&mut engine);
    let before = engine.camera;
    engine.pointer_down(300.0, 200.0);
    engine.pointer_drag(360.0, 240.0);
    assert!(!engine.pointer_up(360.0, 240.0));
    assert!(engine.camera.yaw < before.yaw && engine.camera.pitch > before.pitch);
    engine.pointer_down(300.0, 200.0);
    engine.pointer_drag(300.0, 5000.0);
    engine.pointer_up(300.0, 5000.0);
    assert!(engine.camera.pitch <= 1.4);
}

#[test]
fn dragging_slides_the_solar_system_with_the_hand() {
    let mut engine = engine(Style::Orbit);
    engine.set_paused(true);
    settle(&mut engine);
    frame(&mut engine);
    let before = engine.overlay().core.expect("core");
    engine.pointer_down(300.0, 200.0);
    engine.pointer_drag(350.0, 230.0);
    engine.pointer_up(350.0, 230.0);
    frame(&mut engine);
    let after = engine.overlay().core.expect("core");
    assert!(
        (after.x - before.x - 50.0).abs() < 1.5,
        "{before:?} {after:?}"
    );
    assert!(
        (after.y - before.y - 30.0).abs() < 1.5,
        "{before:?} {after:?}"
    );
}

#[test]
fn it_rests_when_paused_and_settled_and_moves_again_when_asked() {
    let mut engine = engine(Style::Space);
    assert!(engine.busy());
    engine.set_paused(true);
    assert!(engine.busy(), "still arriving");
    settle(&mut engine);
    assert!(!engine.busy());
    let still = frame(&mut engine);
    engine.step(0.05);
    assert_eq!(
        still,
        frame(&mut engine),
        "a paused picture does not change"
    );
    engine.zoom_by(1.5);
    assert!(engine.busy());
    engine.set_focus(Some(1));
    settle(&mut engine);
    assert!(!engine.busy());
    assert_eq!(engine.focus(), Some(1));
    assert!(
        engine.overlay().labels.iter().all(|(_, m)| m.alpha < 0.01),
        "names step back in focus"
    );
    engine.reset();
    assert_eq!((engine.focus(), engine.selected()), (None, None));
    engine.zoom_by(1000.0);
    settle(&mut engine);
    assert!(engine.camera.zoom <= 3.0 + 1e-3);
}

#[test]
fn the_solar_system_follows_a_planet_in_focus() {
    let mut engine = engine(Style::Orbit);
    engine.set_focus(Some(0));
    settle(&mut engine);
    frame(&mut engine);
    let label = engine
        .overlay()
        .labels
        .into_iter()
        .find(|(cluster, _)| *cluster == 0)
        .expect("label")
        .1;
    assert!(
        (label.x - 400.0).abs() < 30.0,
        "the planet is kept in the middle: {label:?}"
    );
    assert!(engine.camera.zoom > 1.2);
}

#[test]
fn a_panel_moves_the_middle_of_the_picture_over() {
    for style in [Style::Orbit, Style::Space] {
        let mut engine = engine(style);
        engine.set_paused(true);
        settle(&mut engine);
        frame(&mut engine);
        let before = engine.overlay().core.expect("core");
        engine.set_inset(300.0);
        assert!(engine.busy());
        settle(&mut engine);
        assert!(!engine.busy());
        frame(&mut engine);
        let after = engine.overlay().core.expect("core");
        assert!(
            (before.x - after.x - 150.0).abs() < 1.0,
            "{style:?} {before:?} {after:?}"
        );
    }
}

#[test]
fn a_huge_canvas_is_drawn_smaller() {
    let mut engine = engine(Style::Space);
    engine.resize(3840, 2160, 2.0);
    let (width, height) = engine.size();
    assert!(width * height <= MAX_PIXELS + 4000, "{width}x{height}");
    settle(&mut engine);
    frame(&mut engine);
    let core = engine.overlay().core.expect("core");
    assert!(
        (core.x - 960.0).abs() < 2.0,
        "places stay in logical pixels: {core:?}"
    );
    engine.resize(0, 0, 1.0);
    frame(&mut engine);
    assert_eq!(engine.overlay(), Overlay::default());
}

#[test]
fn choosing_from_a_list_turns_the_galaxy_to_the_node() {
    let mut engine = engine(Style::Space);
    engine.set_paused(true);
    settle(&mut engine);
    let node = 17;
    engine.select(Some(node), true);
    settle(&mut engine);
    frame(&mut engine);
    let lens = Space::lens(&engine.camera, 800.0, 440.0);
    let place = engine.space().place(node).expect("place");
    let seen = lens.see(place).expect("seen");
    assert!(
        seen.depth < lens.distance(),
        "the chosen star is on the near side"
    );
    engine.select(Some(9999), true);
    assert_eq!(engine.selected(), None);
}
