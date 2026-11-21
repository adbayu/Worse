use std::cell::RefCell;
use std::time::Duration;

use harness_core_api::{ApiError, Orchestra, OrchestraEdge, OrchestraNode, OrchestraRole};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};

use crate::core::{ChatCall, Command};
use crate::ui::{AppWindow, OrchAgent, OrchConn, OrchEdge, OrchLink, OrchNode, OrchestraUi};

mod render;

use render::render;

const NODE_W: f32 = 176.0;
const NODE_H: f32 = 52.0;

#[derive(Default)]
struct State {
    window: Option<Weak<AppWindow>>,
    graph: Orchestra,
    selected: String,
    generation: u64,
    save_timer: Option<slint::Timer>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn sv(text: &str) -> SharedString {
    SharedString::from(text)
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::from(std::rc::Rc::new(VecModel::from(rows)))
}

fn sync<T: Clone + PartialEq + 'static>(
    current: &ModelRc<T>,
    rows: Vec<T>,
    set: impl FnOnce(ModelRc<T>),
) {
    if let Some(vm) = current.as_any().downcast_ref::<VecModel<T>>() {
        if vm.row_count() == rows.len() {
            for (i, row) in rows.into_iter().enumerate() {
                if vm.row_data(i).as_ref() != Some(&row) {
                    vm.set_row_data(i, row);
                }
            }
            return;
        }
    }
    set(model(rows));
}

fn window() -> Option<AppWindow> {
    STATE.with(|s| s.borrow().window.as_ref().and_then(Weak::upgrade))
}

fn role_int(role: OrchestraRole) -> i32 {
    match role {
        OrchestraRole::Orchestrator => 0,
        OrchestraRole::Subagent => 1,
        OrchestraRole::Reviewer => 2,
        OrchestraRole::Planner => 3,
    }
}

fn role_of(value: i32) -> OrchestraRole {
    match value {
        0 => OrchestraRole::Orchestrator,
        2 => OrchestraRole::Reviewer,
        3 => OrchestraRole::Planner,
        _ => OrchestraRole::Subagent,
    }
}

/// Brand dot of an agent (design palette): Claude, Codex, `DeepSeek`, agy, Gemini; accent otherwise.
pub fn agent_color(id: &str) -> slint::Color {
    let id = id.to_ascii_lowercase();
    let rgb = if id.contains("claude") {
        0xE8_8A5C
    } else if id.contains("codex") {
        0x3D_D68C
    } else if id.contains("deepseek") {
        0x4D_6BFE
    } else if id.contains("agy") || id.contains("antigravity") {
        0xB0_7CFF
    } else if id.contains("gemini") {
        0x5B_C0FF
    } else {
        0x7C_8CFF
    };
    slint::Color::from_argb_encoded(0xFF00_0000 | rgb)
}

fn index(value: i32) -> Option<usize> {
    usize::try_from(value).ok()
}

#[allow(clippy::too_many_lines)]
pub fn install(window: &AppWindow) {
    STATE.with(|s| s.borrow_mut().window = Some(window.as_weak()));
    let ui = window.global::<OrchestraUi>();
    ui.on_select(|id| {
        edit(false, |s| s.selected = id.to_string());
    });
    ui.on_add_node(|agent, x, y| {
        edit(true, |s| {
            let agent = agent.to_string();
            if s.graph.nodes.iter().any(|n| n.agent.as_str() == agent) {
                return;
            }
            let offset = f32::from(u8::try_from(s.graph.nodes.len() % 8).unwrap_or(0)) * 16.0;
            let role = if s.graph.orchestrator().is_none() {
                OrchestraRole::Orchestrator
            } else {
                OrchestraRole::Subagent
            };
            s.graph.nodes.push(OrchestraNode {
                id: agent.clone(),
                agent: agent.as_str().into(),
                role,
                x: x + offset,
                y: y + offset,
                instructions: String::new(),
            });
            s.selected = agent;
        });
    });
    ui.on_move_node(|i, x, y| {
        edit(false, |s| {
            if let Some(node) = index(i).and_then(|i| s.graph.nodes.get_mut(i)) {
                node.x = x.round();
                node.y = y.round();
            }
        });
    });
    ui.on_drop_node(save_now);
    ui.on_set_role(|i, role| {
        edit(true, |s| {
            let Some(i) = index(i).filter(|i| *i < s.graph.nodes.len()) else {
                return;
            };
            let role = role_of(role);
            if role == OrchestraRole::Orchestrator {
                for node in &mut s.graph.nodes {
                    if node.role == OrchestraRole::Orchestrator {
                        node.role = OrchestraRole::Subagent;
                    }
                }
            }
            s.graph.nodes[i].role = role;
        });
    });
    ui.on_connect_at(|i, x, y| {
        edit(true, |s| {
            let Some(from) = index(i)
                .and_then(|i| s.graph.nodes.get(i))
                .map(|n| n.id.clone())
            else {
                return;
            };
            let Some(to) = s
                .graph
                .nodes
                .iter()
                .rev()
                .find(|n| {
                    x >= n.x && x <= n.x + NODE_W && y >= n.y - 8.0 && y <= n.y + NODE_H + 8.0
                })
                .map(|n| n.id.clone())
            else {
                return;
            };
            let exists = s
                .graph
                .edges
                .iter()
                .any(|e| (e.from == from && e.to == to) || (e.from == to && e.to == from));
            if from != to && !exists {
                s.graph.edges.push(OrchestraEdge { from, to });
            }
        });
    });
    ui.on_remove_edge(|i| {
        edit(true, |s| {
            if let Some(i) = index(i).filter(|i| *i < s.graph.edges.len()) {
                s.graph.edges.remove(i);
            }
        });
    });
    ui.on_clear_links(|i| {
        edit(true, |s| {
            if let Some(id) = index(i)
                .and_then(|i| s.graph.nodes.get(i))
                .map(|n| n.id.clone())
            {
                s.graph.edges.retain(|e| e.from != id && e.to != id);
            }
        });
    });
    ui.on_remove_node(|i| {
        edit(true, |s| {
            if let Some(i) = index(i).filter(|i| *i < s.graph.nodes.len()) {
                let id = s.graph.nodes.remove(i).id;
                s.graph.edges.retain(|e| e.from != id && e.to != id);
                if s.selected == id {
                    s.selected.clear();
                }
            }
        });
    });
    ui.on_agent_name(|id| {
        let id = id.to_string();
        crate::bridge::orchestra_agents()
            .into_iter()
            .find(|a| a.0 == id)
            .map_or_else(|| sv(&id), |a| sv(&a.1))
    });
    ui.on_agent_color(|id| agent_color(id.as_str()));
    ui.on_line_count(|text| {
        let text = text.trim_end();
        i32::try_from(if text.is_empty() {
            0
        } else {
            text.lines().count()
        })
        .unwrap_or(i32::MAX)
    });
    ui.on_connect(|from, to| {
        edit(true, |s| {
            let (from, to) = (from.to_string(), to.to_string());
            let known = |id: &str| s.graph.nodes.iter().any(|n| n.id == id);
            let exists = s
                .graph
                .edges
                .iter()
                .any(|e| (e.from == from && e.to == to) || (e.from == to && e.to == from));
            if from != to && known(&from) && known(&to) && !exists {
                s.graph.edges.push(OrchestraEdge { from, to });
            }
        });
    });
    ui.on_clear(|| {
        edit(true, |s| {
            s.graph = Orchestra::default();
            s.selected.clear();
        });
    });
    ui.on_edit_instructions(|text| {
        STATE.with(|cell| {
            let mut s = cell.borrow_mut();
            let selected = s.selected.clone();
            if let Some(node) = s.graph.nodes.iter_mut().find(|n| n.id == selected) {
                node.instructions = text.to_string();
            }
            let timer = slint::Timer::default();
            timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(600),
                save_now,
            );
            s.save_timer = Some(timer);
        });
    });
}

pub fn open() {
    let asked = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.generation += 1;
        s.generation
    });
    render();
    let (reply, answer) = tokio::sync::oneshot::channel();
    crate::bridge::send(Command::Chat(ChatCall::GetOrchestra(reply)));
    let _ = slint::spawn_local(async move {
        let result = answer.await.unwrap_or(Err(ApiError::ShutDown));
        let current = STATE.with(|s| s.borrow().generation);
        if current != asked {
            return;
        }
        match result {
            Ok(graph) => {
                STATE.with(|s| s.borrow_mut().graph = graph);
                set_error("");
            }
            Err(error) => set_error(&error.to_string()),
        }
        render();
    });
}

fn edit(save: bool, change: impl FnOnce(&mut State)) {
    STATE.with(|cell| change(&mut cell.borrow_mut()));
    render();
    if save {
        save_now();
    }
}

fn save_now() {
    let graph = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.save_timer = None;
        s.graph.clone()
    });
    let (reply, answer) = tokio::sync::oneshot::channel();
    crate::bridge::send(Command::Chat(ChatCall::SetOrchestra(graph, reply)));
    let _ = slint::spawn_local(async move {
        match answer.await.unwrap_or(Err(ApiError::ShutDown)) {
            Ok(()) => set_error(""),
            Err(error) => set_error(&error.to_string()),
        }
    });
}

fn set_error(text: &str) {
    if let Some(w) = window() {
        w.global::<OrchestraUi>().set_error(sv(text));
    }
}
