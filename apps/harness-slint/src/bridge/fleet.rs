use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use harness_core_api::SubagentStatus;

use super::turns::{now_s, subagent_row};
use super::UiState;
use crate::core::FleetSession;
use crate::ui::{ChatUi, FleetGroup, SubagentRow};

const ORCH_H: i32 = 196;
const NODE_H: i32 = 116;
const NODE_GAP: i32 = 28;
const GROUP_GAP: i32 = 56;

fn int(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// Orchestra models kept across `Event::Fleet` updates, so a subagent's new activity line changes one card
/// instead of rebuilding every group and card on the canvas.
#[derive(Default)]
pub(super) struct FleetView {
    groups: Option<Rc<VecModel<FleetGroup>>>,
    subagents: Vec<Rc<VecModel<SubagentRow>>>,
}

fn same_group(a: &FleetGroup, b: &FleetGroup) -> bool {
    a.id == b.id
        && a.title == b.title
        && a.prompt == b.prompt
        && a.agent == b.agent
        && a.agent_name == b.agent_name
        && a.running == b.running
        && a.open == b.open
        && a.y == b.y
        && a.height == b.height
        && a.total == b.total
        && a.active == b.active
}

fn sync_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let count = rows.len();
    for (index, row) in rows.into_iter().enumerate() {
        match model.row_data(index) {
            Some(old) if old == row => {}
            Some(_) => model.set_row_data(index, row),
            None => model.push(row),
        }
    }
    while model.row_count() > count {
        model.remove(model.row_count() - 1);
    }
}

impl UiState {
    pub(super) fn fleet(&mut self, list: &[FleetSession]) {
        let ui = self.window.global::<ChatUi>();
        ui.set_now_s(now_s());
        let mut y = 0;
        let mut running = 0;
        let mut waiting = 0;
        let mut sessions = 0;
        let mut groups = Vec::with_capacity(list.len());
        let mut rows = Vec::with_capacity(list.len());
        for s in list {
            // Card index of each shown subagent; nested ones hang off their nearest shown ancestor.
            let mut shown: Vec<Option<usize>> = Vec::with_capacity(s.subagents.len());
            let mut active: Vec<SubagentRow> = Vec::new();
            for (index, a) in s.subagents.iter().enumerate() {
                if !a.status.is_active() {
                    shown.push(None);
                    continue;
                }
                let mut up = s.parents.get(index).copied().flatten();
                let parent = loop {
                    match up {
                        Some(p) if p < index => match shown[p] {
                            Some(card) => break Some(card),
                            None => up = s.parents.get(p).copied().flatten(),
                        },
                        _ => break None,
                    }
                };
                let depth = parent
                    .and_then(|card| active.get(card))
                    .map_or(0, |row: &SubagentRow| row.depth + 1);
                shown.push(Some(active.len()));
                active.push(SubagentRow {
                    agent_name: self.agent_name(a.agent.as_str()).into(),
                    depth,
                    parent: parent.map_or(-1, int),
                    ..subagent_row(a)
                });
            }
            running += active.len();
            waiting += s
                .subagents
                .iter()
                .filter(|a| matches!(a.status, SubagentStatus::NeedsApproval))
                .count();
            if s.running || !active.is_empty() {
                sessions += 1;
            }
            let n = int(active.len());
            let height = (n * NODE_H + (n - 1).max(0) * NODE_GAP).max(ORCH_H);
            groups.push(FleetGroup {
                id: s.id.as_str().into(),
                title: s.title.as_str().into(),
                prompt: s.prompt.trim().into(),
                agent: s.agent.as_str().into(),
                agent_name: self.agent_name(&s.agent).into(),
                running: s.running,
                open: s.open,
                y,
                height,
                total: int(s.subagents.len()),
                active: n,
                subagents: ModelRc::default(),
            });
            rows.push(active);
            y += height + GROUP_GAP;
        }
        ui.set_fleet_running(int(running));
        ui.set_fleet_waiting(int(waiting));
        ui.set_fleet_sessions(int(sessions));
        ui.set_fleet_height((y - GROUP_GAP).max(0));
        drop(ui);
        self.show_fleet(groups, rows);
    }

    fn show_fleet(&mut self, groups: Vec<FleetGroup>, rows: Vec<Vec<SubagentRow>>) {
        let ui = self.window.global::<ChatUi>();
        let view = &mut self.fleet_view;
        let model = if let Some(model) = &view.groups {
            model.clone()
        } else {
            let model = Rc::new(VecModel::default());
            ui.set_fleet(ModelRc::from(model.clone()));
            view.groups = Some(model.clone());
            model
        };
        let count = groups.len();
        for (index, (mut group, active)) in groups.into_iter().zip(rows).enumerate() {
            if view.subagents.len() <= index {
                view.subagents.push(Rc::new(VecModel::default()));
            }
            let subs = view.subagents[index].clone();
            sync_rows(&subs, active);
            group.subagents = ModelRc::from(subs);
            match model.row_data(index) {
                Some(old) if same_group(&old, &group) => {}
                Some(_) => model.set_row_data(index, group),
                None => model.push(group),
            }
        }
        while model.row_count() > count {
            model.remove(model.row_count() - 1);
        }
        view.subagents.truncate(count);
    }
}
