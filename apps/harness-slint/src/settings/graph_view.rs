//! The picture of the project graph on Settings › Memory › Graph, and over the whole window when asked.
//!
//! `harness-graph-view` draws it, pixel by pixel, on a thread of its own: the window stays quick while a
//! frame is worked out. The page sends that thread what happens (the pointer, the wheel, the frame clock,
//! a new size) and gets finished frames back, each with the places where names go. One frame is on its way
//! at a time: the next is drawn when the window has shown the last, so a slow computer shows fewer frames
//! instead of falling behind. Nothing is drawn while the picture is off screen: the frame clock belongs to
//! the canvas and stops with it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use harness_graph_view::{Engine, Graph, Overlay, Palette, Style};
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use super::graph::grouped;
use crate::ui::{AppWindow, GraphHit, GraphLabel, GraphUi, Theme};

mod present;
mod worker;

use present::{hits, palette, present, summary};
use worker::work;

/// Nodes listed at most: search results, and a chosen node's neighbours.
const LIST: usize = 60;
const FRAME_BUDGET: Duration = Duration::from_millis(16);

enum Cmd {
    Graph(Option<Arc<Graph>>),
    Style(Style),
    Palette(Palette),
    /// The canvas in device pixels, and how many of those make a logical pixel.
    Size(u32, u32, f32),
    /// The frame clock.
    Tick,
    /// The window has shown the last frame.
    Shown,
    /// 0 moved, 1 pressed, 2 dragged, 3 released, 4 left; in logical pixels.
    Pointer(i32, f32, f32),
    Zoom(f32),
    Reset,
    /// Hold still (paused, window in the background, reduced motion); with `calm`, also skip the arrival.
    Rest(bool, bool),
    Focus(Option<usize>),
    /// Choose a node from a list.
    Pick(Option<usize>),
    /// Room a panel takes on the right, in logical pixels.
    Inset(f32),
}

/// A finished frame and what goes over it.
struct Shot {
    buffer: SharedPixelBuffer<Rgba8Pixel>,
    overlay: Overlay,
    busy: bool,
    focus: Option<usize>,
    fps: f32,
}

struct View {
    commands: Sender<Cmd>,
    graph: Option<Arc<Graph>>,
    labels: Rc<VecModel<GraphLabel>>,
    /// What the detail panel and the tooltip show now, to fill them only when it changes.
    selected: Option<usize>,
    hover: Option<usize>,
    prefs: PathBuf,
    workspace: PathBuf,
}

thread_local! {
    static VIEW: RefCell<Option<View>> = const { RefCell::new(None) };
}

fn send(cmd: Cmd) {
    VIEW.with(|cell| {
        if let Some(view) = cell.borrow().as_ref() {
            let _ = view.commands.send(cmd);
        }
    });
}

/// The style and whether it is paused, kept between runs.
fn load_prefs(path: &Path) -> (Style, bool) {
    let prefs: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let style = prefs
        .get("style")
        .and_then(serde_json::Value::as_str)
        .map_or(Style::Space, Style::named);
    let paused = prefs
        .get("paused")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    (style, paused)
}

fn save_prefs(ui: &GraphUi<'_>) {
    let Some(path) = VIEW.with(|cell| cell.borrow().as_ref().map(|view| view.prefs.clone())) else {
        return;
    };
    let prefs = serde_json::json!({
        "style": ui.get_view_style().as_str(),
        "paused": ui.get_view_paused(),
    });
    let _ = std::thread::Builder::new()
        .name("harness-graph-prefs".into())
        .spawn(move || {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, prefs.to_string());
        });
}

pub fn install(window: &AppWindow, home: &Path, workspace: &Path) {
    let (commands, inbox) = mpsc::channel();
    let prefs = home.join("memory").join("graph-view.json");
    let (style, paused) = load_prefs(&prefs);
    let ui = window.global::<GraphUi>();
    ui.set_view_style(style.name().into());
    ui.set_view_paused(paused);
    ui.set_view_title(
        workspace
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into(),
    );
    let labels = Rc::new(VecModel::<GraphLabel>::default());
    ui.set_view_labels(ModelRc::from(labels.clone()));
    let _ = commands.send(Cmd::Style(style));
    let _ = commands.send(Cmd::Palette(palette(window)));
    VIEW.with(|cell| {
        *cell.borrow_mut() = Some(View {
            commands,
            graph: None,
            labels,
            selected: None,
            hover: None,
            prefs,
            workspace: workspace.to_path_buf(),
        });
    });
    let weak = window.as_weak();
    let _ = std::thread::Builder::new()
        .name("harness-graph-view".into())
        .spawn(move || work(&inbox, &weak));

    let weak = window.as_weak();
    ui.on_view_resized(move |width, height| {
        let Some(window) = weak.upgrade() else { return };
        let scale = window.window().scale_factor();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let size = |logical: f32| (logical * scale).round().max(0.0) as u32;
        send(Cmd::Palette(palette(&window)));
        send(Cmd::Size(size(width), size(height), scale));
    });
    let weak = window.as_weak();
    ui.on_view_themed(move || {
        if let Some(window) = weak.upgrade() {
            send(Cmd::Palette(palette(&window)));
        }
    });
    ui.on_view_pointer(|kind, x, y| send(Cmd::Pointer(kind, x, y)));
    ui.on_view_zoom(|factor| send(Cmd::Zoom(factor)));
    ui.on_view_tick(|| send(Cmd::Tick));
    ui.on_view_reset(|| send(Cmd::Reset));
    ui.on_view_rest(|resting, calm| send(Cmd::Rest(resting, calm)));
    ui.on_view_focus_cluster(|cluster| send(Cmd::Focus(usize::try_from(cluster).ok())));
    ui.on_view_pick(|node| send(Cmd::Pick(usize::try_from(node).ok())));
    ui.on_view_inset(|width| send(Cmd::Inset(width)));

    let weak = window.as_weak();
    ui.on_view_set_style(move |name| {
        let Some(window) = weak.upgrade() else { return };
        let ui = window.global::<GraphUi>();
        let style = Style::named(name.as_str());
        ui.set_view_style(style.name().into());
        save_prefs(&ui);
        send(Cmd::Palette(palette(&window)));
        send(Cmd::Style(style));
    });
    let weak = window.as_weak();
    ui.on_view_set_paused(move |paused| {
        let Some(window) = weak.upgrade() else { return };
        let ui = window.global::<GraphUi>();
        ui.set_view_paused(paused);
        save_prefs(&ui);
    });
    let weak = window.as_weak();
    ui.on_view_search(move |query| {
        let Some(window) = weak.upgrade() else { return };
        VIEW.with(|cell| {
            if let Some(graph) = cell.borrow().as_ref().and_then(|view| view.graph.clone()) {
                let found = graph.search(query.as_str(), LIST);
                window
                    .global::<GraphUi>()
                    .set_list_hits(hits(&graph, &found));
            }
        });
    });
    ui.on_view_open_source(|| {
        let path = VIEW.with(|cell| {
            let cell = cell.borrow();
            let view = cell.as_ref()?;
            let node = view.graph.as_ref()?.nodes.get(view.selected?)?;
            (!node.source.is_empty()).then(|| view.workspace.join(&node.source))
        });
        if let Some(path) = path {
            super::graph::open_path(&path);
        }
    });
}

pub fn set_workspace(window: &AppWindow, workspace: &Path) {
    window.global::<GraphUi>().set_view_title(
        workspace
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into(),
    );
    VIEW.with(|cell| {
        if let Some(view) = cell.borrow_mut().as_mut() {
            view.workspace = workspace.to_path_buf();
        }
    });
    show(window, None);
}

/// Shows `graph` (or clears the picture); on the UI thread.
pub fn show(window: &AppWindow, graph: Option<Arc<Graph>>) {
    let ui = window.global::<GraphUi>();
    VIEW.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(view) = cell.as_mut() else { return };
        let rows: Vec<GraphLabel> = graph.as_ref().map_or_else(Vec::new, |graph| {
            graph
                .clusters
                .iter()
                .enumerate()
                .map(|(i, cluster)| GraphLabel {
                    cluster: i32::try_from(i).unwrap_or(-1),
                    x: 0.0,
                    y: 0.0,
                    alpha: 0.0,
                    title: cluster.name.as_str().into(),
                    note: format!("{} nodes", grouped(cluster.size)).into(),
                    tone: cluster.tone,
                })
                .collect()
        });
        view.labels.set_vec(rows);
        view.selected = None;
        view.hover = None;
        ui.set_view_note(graph.as_deref().map(summary).unwrap_or_default().into());
        ui.set_view_selected(-1);
        ui.set_tip_shown(false);
        ui.set_view_focus(-1);
        ui.set_list_query("".into());
        if let Some(graph) = &graph {
            ui.set_list_hits(hits(graph, &graph.search("", LIST)));
        } else {
            ui.set_list_hits(ModelRc::default());
            ui.set_view_ready(false);
        }
        view.graph.clone_from(&graph);
        let _ = view.commands.send(Cmd::Graph(graph));
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::present::hit;
    use super::*;

    #[test]
    fn a_graph_is_summed_up_and_its_nodes_listed() {
        let graph = Graph::parse(
            r#"{"nodes":[{"id":"a","label":"a.rs","source_file":"src/a.rs","community":0},{"id":"b","community":0}],"links":[{"source":"a","target":"b"}]}"#,
        )
        .expect("graph");
        assert_eq!(summary(&graph), "2 nodes · 1 links");
        let row = hit(&graph, 0);
        assert_eq!((row.index, row.tone), (0, 0));
        assert_eq!(row.note.as_str(), "1 link · src/a.rs");
        assert_eq!(hit(&graph, 99).index, 99);
        assert_eq!(hit(&graph, 99).title.as_str(), "");
    }

    #[test]
    fn preferences_default_to_the_galaxy_and_survive_a_bad_file() {
        let dir = std::env::temp_dir().join(format!("harness-graph-view-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("graph-view.json");
        assert_eq!(load_prefs(&path), (Style::Space, false));
        std::fs::write(&path, "not json").expect("write");
        assert_eq!(load_prefs(&path), (Style::Space, false));
        std::fs::write(&path, r#"{"style":"orbit","paused":true}"#).expect("write");
        assert_eq!(load_prefs(&path), (Style::Orbit, true));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
