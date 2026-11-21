//! Settings › Memory › Graph: the project's Graphify knowledge graph (`harness_agent_hub::graphify`).
//! Finds the tool, builds or refreshes the graph with `graphify update <project>` (code only: no model, no
//! API key), opens the graph and its report, and holds the switch that gives the graph to agents.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use harness_agent_hub::{graphify, recall};
use harness_graph_view::Graph;
use slint::ComponentHandle;

use super::graph_view;
use crate::ui::{AppWindow, GraphUi};

mod installer;
mod runner;

use installer::{Installer, Step};
use runner::run;

/// Lines of command output kept for the page.
const LOG_LINES: usize = 14;

#[derive(Clone)]
struct Ctx {
    home: PathBuf,
    workspace: Arc<Mutex<PathBuf>>,
    window: slint::Weak<AppWindow>,
    /// One command at a time.
    busy: Arc<AtomicBool>,
}

/// What the page shows, gathered off the UI thread.
#[allow(clippy::struct_excessive_bools)]
struct State {
    /// The graph for the picture, when there is one.
    graph: Option<Arc<Graph>>,
    cli: Option<PathBuf>,
    mcp: bool,
    plan: String,
    summary: Option<String>,
    has_html: bool,
    has_report: bool,
    give_agents: bool,
}

thread_local! {
    static CTX: std::cell::RefCell<Option<Ctx>> = const { std::cell::RefCell::new(None) };
}

impl Ctx {
    fn root(&self) -> PathBuf {
        self.workspace.lock().map(|w| w.clone()).unwrap_or_default()
    }
}

pub fn set_workspace(window: &AppWindow, workspace: &Path) {
    let Some(ctx) = CTX.with(|cell| cell.borrow().clone()) else {
        return;
    };
    if let Ok(mut root) = ctx.workspace.lock() {
        if root.as_path() == workspace {
            return;
        }
        *root = workspace.to_path_buf();
    }
    let ui = window.global::<GraphUi>();
    ui.set_project(workspace.to_string_lossy().as_ref().into());
    ui.set_has_graph(false);
    ui.set_checked(false);
    ui.set_summary(slint::SharedString::default());
    ui.set_has_html(false);
    ui.set_has_report(false);
    graph_view::set_workspace(window, workspace);
    if ui.get_view_wide() {
        refresh(&ctx, None);
    }
}

pub fn install(window: &AppWindow, home: &Path, workspace: &Path) {
    let ctx = Ctx {
        home: home.to_path_buf(),
        workspace: Arc::new(Mutex::new(workspace.to_path_buf())),
        window: window.as_weak(),
        busy: Arc::new(AtomicBool::new(false)),
    };
    CTX.with(|cell| *cell.borrow_mut() = Some(ctx.clone()));
    let ui = window.global::<GraphUi>();
    ui.set_install_command(graphify::INSTALL_COMMAND.into());
    ui.set_project(workspace.to_string_lossy().as_ref().into());
    ui.set_give_agents(recall::Store::new(home).config().graph);
    graph_view::install(window, home, workspace);

    let c = ctx.clone();
    ui.on_refresh(move || refresh(&c, None));

    let c = ctx.clone();
    ui.on_build(move || {
        let workspace = c.root();
        let mut steps: Vec<Step> = if !graphify::own_installed() && graphify::uv().is_some() {
            Installer::Uv.steps()
        } else {
            Vec::new()
        };
        steps.push(Box::new(move || build_command(&workspace)));
        run(
            &c,
            steps,
            "The graph is up to date.",
            "graphify update failed",
        );
    });

    // "Set up Graphify": install what is missing, then build the graph, in one go.
    let c = ctx.clone();
    ui.on_install(move || {
        let Some(installer) = Installer::find() else {
            return refresh(
                &c,
                Some(format!(
                    "Nothing on this computer can install Graphify. Install uv, then run: {}",
                    graphify::INSTALL_COMMAND
                )),
            );
        };
        let mut steps = installer.steps();
        let workspace = c.root();
        steps.push(Box::new(move || build_command(&workspace)));
        run(
            &c,
            steps,
            "Graphify is installed and the graph is built.",
            "Setting up Graphify failed",
        );
    });

    let c = ctx.clone();
    ui.on_open_html(move || open_path(&graphify::paths(&c.root()).html));
    let c = ctx.clone();
    ui.on_open_report(move || open_path(&graphify::paths(&c.root()).report));

    let c = ctx;
    ui.on_set_give_agents(move |on| {
        let c = c.clone();
        spawn(move || {
            let store = recall::Store::new(&c.home);
            let mut config = store.config();
            config.graph = on;
            let note = store
                .set_config(config)
                .err()
                .map(|error| format!("Could not save the setting: {error}"));
            refresh(&c, note);
        });
    });
}

fn spawn(work: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name("harness-graph".into())
        .spawn(work);
}

/// `graphify update <workspace>`: Graphify's code-only pass, run through `graphify::SHIM` in Graphify's own
/// Python so one unreadable file cannot abort the build.
fn build_command(workspace: &Path) -> Result<Command, String> {
    let program = graphify::cli().ok_or(
        "graphify was installed but is not found yet. Restart Harness, then press Build graph.",
    )?;
    let mut command = match graphify::python_of(&program) {
        Some(python) => {
            let mut command = Command::new(python);
            command.args(["-I", "-c", graphify::SHIM]);
            command
        }
        None => Command::new(program),
    };
    command
        .arg("update")
        .arg(workspace)
        .current_dir(workspace)
        .env("GRAPHIFY_NO_TIPS", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1");
    Ok(command)
}

/// "1,204" for 1204.
pub(super) fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "1,204 nodes · 3,310 links · 18 communities · updated 2h ago".
fn summary(stats: graphify::Stats, updated: Option<&str>) -> String {
    let mut text = format!(
        "{} nodes · {} links",
        grouped(stats.nodes),
        grouped(stats.links)
    );
    if stats.communities > 0 {
        let _ = write!(text, " · {} communities", grouped(stats.communities));
    }
    if let Some(age) = updated {
        let _ = write!(text, " · updated {age}");
    }
    text
}

fn gather(root: &Path, home: &Path) -> State {
    let files = graphify::paths(root);
    let graph_text = std::fs::read_to_string(&files.graph).ok();
    let graph = graph_text.as_deref().and_then(Graph::parse).map(Arc::new);
    let graph_summary = graph_text.map(|text| {
        let updated = std::fs::metadata(&files.graph)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| recall::age(recall::now_ms(), i64::try_from(d.as_millis()).unwrap_or(0)));
        graphify::stats(&text).map_or_else(
            || "graph.json is there, but it is not a graph this page can read.".to_owned(),
            |stats| summary(stats, updated.as_deref()),
        )
    });
    State {
        graph,
        cli: graphify::cli(),
        mcp: graphify::mcp_cli().is_some(),
        plan: Installer::find().map(|i| i.plan()).unwrap_or_default(),
        summary: graph_summary,
        has_html: files.html.is_file(),
        has_report: files.report.is_file(),
        give_agents: recall::Store::new(home).config().graph,
    }
}

/// Looks again and shows it; `note` replaces the status line.
fn refresh(ctx: &Ctx, note: Option<String>) {
    let ctx = ctx.clone();
    spawn(move || {
        let root = ctx.root();
        let state = gather(&root, &ctx.home);
        let weak = ctx.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            if ctx.root() != root {
                return;
            }
            let ui = window.global::<GraphUi>();
            ui.set_installed(state.cli.is_some());
            ui.set_cli_path(
                state
                    .cli
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
            );
            ui.set_mcp(state.mcp);
            ui.set_install_plan(state.plan.into());
            ui.set_has_graph(state.summary.is_some());
            ui.set_checked(true);
            ui.set_summary(state.summary.unwrap_or_default().into());
            ui.set_has_html(state.has_html);
            ui.set_has_report(state.has_report);
            ui.set_give_agents(state.give_agents);
            if let Some(note) = note {
                ui.set_note(note.into());
            }
            graph_view::show(&window, state.graph);
        });
    });
}

fn show_running(ctx: &Ctx, busy: bool, log: String) {
    let weak = ctx.window.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<GraphUi>();
            ui.set_busy(busy);
            ui.set_log(log.into());
        }
    });
}

/// Opens a file with the system's default program (the graph page in the browser, the report in an editor).
pub(super) fn open_path(path: &Path) {
    if !path.is_file() {
        return;
    }
    let program = if cfg!(windows) {
        "explorer.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(program).arg(path).spawn();
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{graphify, grouped, summary, Installer};
    use harness_agent_hub::graphify::Stats;

    #[test]
    fn summary_groups_digits_and_skips_what_is_missing() {
        assert_eq!(grouped(7), "7");
        assert_eq!(grouped(1204), "1,204");
        assert_eq!(grouped(1_234_567), "1,234,567");
        let stats = Stats {
            nodes: 1204,
            links: 3310,
            communities: 18,
        };
        assert_eq!(
            summary(stats, Some("2h ago")),
            "1,204 nodes · 3,310 links · 18 communities · updated 2h ago"
        );
        let bare = Stats {
            nodes: 3,
            links: 0,
            communities: 0,
        };
        assert_eq!(summary(bare, None), "3 nodes · 0 links");
    }

    #[test]
    fn each_installer_says_what_it_runs_and_ends_with_graphify() {
        let winget = Installer::WingetThenUv("winget".into());
        assert_eq!(
            winget.plan(),
            "winget install astral-sh.uv, then uv tool install \"graphifyy[mcp]==0.9.87\""
        );
        assert_eq!(winget.steps().len(), 2);
        assert_eq!(Installer::Uv.plan(), "uv tool install \"graphifyy[mcp]==0.9.87\"");
        for installer in [
            Installer::Pipx("pipx".into()),
            Installer::Pip("python".into()),
        ] {
            assert!(installer.plan().contains("graphifyy[mcp]"));
            let mut steps = installer.steps();
            let command = steps.remove(0)().expect("command");
            assert!(command.get_args().any(|a| a == graphify::PACKAGE));
        }
    }
}
