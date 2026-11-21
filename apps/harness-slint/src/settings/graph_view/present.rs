use super::{
    grouped, AppWindow, Cmd, ComponentHandle, Graph, GraphHit, GraphUi, Image, Model, ModelRc,
    Palette, Shot, Theme, VecModel, LIST, VIEW,
};

fn rgb(color: slint::Color) -> [f32; 3] {
    [
        f32::from(color.red()) / 255.0,
        f32::from(color.green()) / 255.0,
        f32::from(color.blue()) / 255.0,
    ]
}

/// The theme's colours for the solar system.
pub(super) fn palette(window: &AppWindow) -> Palette {
    let theme = window.global::<Theme>();
    Palette {
        ink: rgb(theme.get_foreground()),
        line: rgb(theme.get_foreground_subtlest()),
        sun: if theme.get_dark() {
            [0.97, 0.78, 0.36]
        } else {
            [0.88, 0.6, 0.12]
        },
        tones: [
            rgb(theme.get_accent_fg()),
            rgb(theme.get_info()),
            rgb(theme.get_agent_agy()),
            rgb(theme.get_agent_claude()),
            rgb(theme.get_warning()),
            rgb(theme.get_success()),
            if theme.get_dark() {
                [0.93, 0.5, 0.66]
            } else {
                [0.78, 0.25, 0.45]
            },
        ],
        quiet: rgb(theme.get_foreground_subtlest()),
    }
}

pub(super) fn hit(graph: &Graph, node: usize) -> GraphHit {
    let tone = graph
        .nodes
        .get(node)
        .and_then(|n| graph.clusters.get(n.cluster))
        .map_or(-1, |cluster| cluster.tone);
    GraphHit {
        index: i32::try_from(node).unwrap_or(-1),
        title: graph
            .nodes
            .get(node)
            .map(|n| n.label.as_str())
            .unwrap_or_default()
            .into(),
        note: graph.note(node).into(),
        tone,
    }
}

pub(super) fn hits(graph: &Graph, nodes: &[usize]) -> ModelRc<GraphHit> {
    let rows: Vec<GraphHit> = nodes.iter().take(LIST).map(|&i| hit(graph, i)).collect();
    ModelRc::new(VecModel::from(rows))
}

/// "300 of 4,210 nodes · 520 links".
pub(super) fn summary(graph: &Graph) -> String {
    let nodes = if graph.total_nodes > graph.nodes.len() {
        format!(
            "{} of {} nodes",
            grouped(graph.nodes.len()),
            grouped(graph.total_nodes)
        )
    } else {
        format!("{} nodes", grouped(graph.total_nodes))
    };
    format!("{nodes} · {} links", grouped(graph.total_links))
}

/// Puts a finished frame on the page; on the UI thread.
pub(super) fn present(window: &AppWindow, shot: Shot) {
    let ui = window.global::<GraphUi>();
    VIEW.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(view) = cell.as_mut() else { return };
        let Some(graph) = view.graph.clone() else {
            return;
        };
        ui.set_view_frame(Image::from_rgba8_premultiplied(shot.buffer));
        ui.set_view_ready(true);
        ui.set_view_busy(shot.busy);
        ui.set_view_fps(if shot.busy && shot.fps > 1.0 {
            format!("{:.0} fps", shot.fps).into()
        } else {
            "".into()
        });
        for row in 0..view.labels.row_count() {
            let Some(mut label) = view.labels.row_data(row) else {
                continue;
            };
            match shot
                .overlay
                .labels
                .iter()
                .find(|(cluster, _)| *cluster == row)
            {
                Some((_, marker)) => {
                    label.x = marker.x;
                    label.y = marker.y;
                    label.alpha = marker.alpha;
                }
                None => label.alpha = 0.0,
            }
            view.labels.set_row_data(row, label);
        }
        if let Some(core) = shot.overlay.core {
            ui.set_core_x(core.x);
            ui.set_core_y(core.y);
            ui.set_core_alpha(core.alpha);
        } else {
            ui.set_core_alpha(0.0);
        }
        ui.set_view_focus(shot.focus.and_then(|c| i32::try_from(c).ok()).unwrap_or(-1));
        ui.set_view_focus_name(
            shot.focus
                .and_then(|c| graph.clusters.get(c))
                .map(|cluster| cluster.name.as_str())
                .unwrap_or_default()
                .into(),
        );
        let hover = shot.overlay.hover.map(|(node, _)| node);
        if let Some((node, marker)) = shot.overlay.hover {
            ui.set_tip_x(marker.x);
            ui.set_tip_y(marker.y);
            if view.hover != hover {
                let row = hit(&graph, node);
                ui.set_tip_title(row.title);
                ui.set_tip_note(row.note);
            }
        }
        ui.set_tip_shown(hover.is_some());
        view.hover = hover;
        let selected = shot.overlay.selected.map(|(node, _)| node);
        if let Some((node, marker)) = shot.overlay.selected {
            ui.set_sel_x(marker.x);
            ui.set_sel_y(marker.y);
            if view.selected != selected {
                let row = hit(&graph, node);
                ui.set_sel_title(row.title);
                ui.set_sel_note(row.note);
                let chosen = graph.nodes.get(node);
                ui.set_sel_group(
                    chosen
                        .and_then(|n| graph.clusters.get(n.cluster))
                        .map(|cluster| cluster.name.as_str())
                        .unwrap_or_default()
                        .into(),
                );
                ui.set_sel_has_source(chosen.is_some_and(|n| !n.source.is_empty()));
                ui.set_sel_links(hits(&graph, graph.neighbours(node)));
            }
        }
        ui.set_view_selected(selected.and_then(|n| i32::try_from(n).ok()).unwrap_or(-1));
        view.selected = selected;
        let _ = view.commands.send(Cmd::Shown);
    });
}
