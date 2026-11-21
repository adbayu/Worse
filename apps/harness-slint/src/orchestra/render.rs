use super::{
    role_int, sv, sync, window, OrchAgent, OrchConn, OrchEdge, OrchLink, OrchNode, OrchestraNode,
    OrchestraRole, OrchestraUi, NODE_H, NODE_W, STATE,
};
use slint::ComponentHandle;
use std::fmt::Write as _;
#[allow(clippy::too_many_lines)]
pub(super) fn render() {
    let Some(w) = window() else {
        return;
    };
    let agents = crate::bridge::orchestra_agents();
    STATE.with(|cell| {
        let s = cell.borrow();
        let ui = w.global::<OrchestraUi>();
        let name_of = |agent: &str| {
            agents
                .iter()
                .find(|a| a.0 == agent)
                .map_or_else(|| format!("{agent} (missing)"), |a| a.1.clone())
        };
        let nodes: Vec<OrchNode> = s
            .graph
            .nodes
            .iter()
            .map(|n| {
                let name = name_of(n.agent.as_str());
                let usable = agents.iter().any(|a| a.0 == n.agent.as_str() && a.2);
                OrchNode {
                    id: sv(&n.id),
                    initial: sv(&name
                        .chars()
                        .next()
                        .map(|c| c.to_uppercase().to_string())
                        .unwrap_or_default()),
                    name: sv(&name),
                    role: role_int(n.role),
                    x: n.x,
                    y: n.y,
                    muted: !usable,
                }
            })
            .collect();
        let find = |id: &str| s.graph.nodes.iter().find(|n| n.id == id);
        let edges: Vec<OrchEdge> = s
            .graph
            .edges
            .iter()
            .filter_map(|e| {
                let (a, b) = (find(&e.from)?, find(&e.to)?);
                let (top, bottom) = if a.y <= b.y { (a, b) } else { (b, a) };
                let mut commands = String::new();
                let _ = write!(
                    commands,
                    "M {} {} L {} {}",
                    top.x + NODE_W / 2.0,
                    top.y + NODE_H,
                    bottom.x + NODE_W / 2.0,
                    bottom.y
                );
                Some(OrchEdge {
                    commands: sv(&commands),
                    role: role_int(top.role),
                })
            })
            .collect();
        let links: Vec<OrchLink> = s
            .graph
            .edges
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let other = if e.from == s.selected {
                    &e.to
                } else if e.to == s.selected {
                    &e.from
                } else {
                    return None;
                };
                let node = find(other)?;
                Some(OrchLink {
                    edge: i32::try_from(i).unwrap_or(i32::MAX),
                    name: sv(&name_of(node.agent.as_str())),
                })
            })
            .collect();
        let addable: Vec<OrchAgent> = agents
            .iter()
            .filter(|a| !s.graph.nodes.iter().any(|n| n.agent.as_str() == a.0))
            .map(|a| OrchAgent {
                id: sv(&a.0),
                name: sv(&if a.2 {
                    a.1.clone()
                } else {
                    format!("{} (disabled)", a.1)
                }),
            })
            .collect();
        let all: Vec<OrchAgent> = agents
            .iter()
            .map(|a| OrchAgent {
                id: sv(&a.0),
                name: sv(&a.1),
            })
            .collect();
        let conn = |i: usize, from: &OrchestraNode, to: &OrchestraNode| OrchConn {
            edge: i32::try_from(i).unwrap_or(-1),
            from_id: sv(&from.id),
            from_agent: sv(from.agent.as_str()),
            from_name: sv(&name_of(from.agent.as_str())),
            to_id: sv(&to.id),
            to_agent: sv(to.agent.as_str()),
            to_name: sv(&name_of(to.agent.as_str())),
            role: role_int(to.role),
        };
        let connections: Vec<OrchConn> = s
            .graph
            .edges
            .iter()
            .enumerate()
            .filter_map(|(i, e)| Some(conn(i, find(&e.from)?, find(&e.to)?)))
            .collect();
        // Pairs that can still be connected, orchestrator first (for "+ Add connection").
        let mut order: Vec<&OrchestraNode> = s.graph.nodes.iter().collect();
        order.sort_by_key(|n| n.role != OrchestraRole::Orchestrator);
        let mut candidates = Vec::new();
        for a in &order {
            for b in &s.graph.nodes {
                let linked =
                    s.graph.edges.iter().any(|e| {
                        (e.from == a.id && e.to == b.id) || (e.from == b.id && e.to == a.id)
                    });
                let seen = candidates
                    .iter()
                    .any(|c: &OrchConn| c.from_id == b.id.as_str() && c.to_id == a.id.as_str());
                if a.id != b.id && !linked && !seen && b.role != OrchestraRole::Orchestrator {
                    candidates.push(conn(usize::MAX, a, b));
                }
            }
        }
        sync(&ui.get_agents(), all, |m| ui.set_agents(m));
        sync(&ui.get_connections(), connections, |m| {
            ui.set_connections(m);
        });
        sync(&ui.get_candidates(), candidates, |m| ui.set_candidates(m));
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for n in &s.graph.nodes {
            x0 = x0.min(n.x);
            y0 = y0.min(n.y);
            x1 = x1.max(n.x + NODE_W);
            y1 = y1.max(n.y + NODE_H);
        }
        if s.graph.nodes.is_empty() {
            (x0, y0, x1, y1) = (0.0, 0.0, NODE_W, NODE_H);
        }
        ui.set_bx(x0);
        ui.set_by(y0);
        ui.set_bw(x1 - x0);
        ui.set_bh(y1 - y0);
        let selected = s.graph.nodes.iter().find(|n| n.id == s.selected);
        ui.set_selected(sv(selected.map_or("", |n| n.id.as_str())));
        ui.set_selected_name(sv(&selected
            .map(|n| name_of(n.agent.as_str()))
            .unwrap_or_default()));
        ui.set_selected_role(selected.map_or(1, |n| role_int(n.role)));
        let instructions = selected.map_or("", |n| n.instructions.as_str());
        if ui.get_instructions().as_str() != instructions {
            ui.set_instructions(sv(instructions));
        }
        sync(&ui.get_nodes(), nodes, |m| ui.set_nodes(m));
        sync(&ui.get_edges(), edges, |m| ui.set_edges(m));
        sync(&ui.get_links(), links, |m| ui.set_links(m));
        sync(&ui.get_addable(), addable, |m| ui.set_addable(m));
    });
}
