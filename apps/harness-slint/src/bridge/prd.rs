//! PRD page (PRD mode plan, milestone 3): reads `docs/specs/*/prd.json` of the open workspace on a worker
//! thread and fills `PrdUi` (phase columns, feature headers, task cards, gate and progress). The file is
//! written by the agent in PRD mode (`harness-agent-hub/src/prd.rs`), so parsing is lenient: missing
//! fields show as empty, unknown statuses as planned. Milestones 4 and 5: each card shows the subagent
//! working on it (`live.rs`), and Run sends the phase or task to the agent in PRD mode, which delegates it.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use harness_core_api::ContextItem;

use super::{with_state, UiState};
use crate::ui::{ChatUi, PrdNode, PrdPhase, PrdSpec, PrdUi};

mod doc;
mod live;

use doc::{load, Loaded};
use live::Sub;

thread_local! {
    /// A reload is running: timer ticks meanwhile are dropped.
    static LOADING: Cell<bool> = const { Cell::new(false) };
}

fn sv(text: &str) -> SharedString {
    SharedString::from(text)
}

fn to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

impl UiState {
    fn prd_ui(&self) -> PrdUi<'_> {
        self.window.global::<PrdUi>()
    }

    /// Workspace of the open session (a worktree or another folder), else the app workspace.
    fn prd_root(&self) -> PathBuf {
        self.active
            .as_ref()
            .and_then(|id| self.summaries.iter().find(|s| &s.id == id))
            .map(|s| s.workspace_path.clone())
            .filter(|p| !p.trim().is_empty())
            .map_or_else(|| self.workspace.clone(), PathBuf::from)
    }

    fn selected_slug(&self) -> String {
        let ui = self.prd_ui();
        let index = usize::try_from(ui.get_selected()).unwrap_or(0);
        ui.get_specs()
            .row_data(index)
            .map(|s| s.slug.to_string())
            .unwrap_or_default()
    }

    fn prd_refresh(&self, want: String) {
        if LOADING.with(|l| l.replace(true)) {
            return;
        }
        let root = self.prd_root();
        let spawned = std::thread::Builder::new()
            .name("prd-load".into())
            .spawn(move || {
                let loaded = load(&root, &want);
                let _ = slint::invoke_from_event_loop(move || {
                    LOADING.with(|l| l.set(false));
                    with_state(|s| s.apply_prd(loaded));
                });
            });
        if spawned.is_err() {
            LOADING.with(|l| l.set(false));
        }
    }

    fn apply_prd(&self, loaded: Loaded) {
        let ui = self.prd_ui();
        let doc = loaded.doc;
        let specs: Vec<PrdSpec> = loaded
            .specs
            .iter()
            .map(|(slug, title)| PrdSpec {
                slug: sv(slug),
                title: sv(title),
            })
            .collect();
        ui.set_specs(ModelRc::new(VecModel::from(specs)));
        ui.set_selected(to_i32(loaded.selected));
        ui.set_title(sv(&doc.title));
        ui.set_spec_path(sv(&doc.spec_path));
        ui.set_gate_mode(sv(&doc.gate_mode));
        ui.set_gate_passed(doc.gate_passed);
        ui.set_gate_issues(to_i32(doc.gate_issues));
        ui.set_done(to_i32(doc.phases.iter().map(|p| p.done).sum()));
        ui.set_total(to_i32(doc.phases.iter().map(|p| p.total).sum()));
        ui.set_error(sv(&loaded.error));
        let phases: Vec<PrdPhase> = doc
            .phases
            .iter()
            .map(|p| PrdPhase {
                id: sv(&p.id),
                title: sv(&p.title),
                status: sv(&p.status),
                done: to_i32(p.done),
                total: to_i32(p.total),
            })
            .collect();
        ui.set_phases(ModelRc::new(VecModel::from(phases)));
        let subs = self.fleet_subs();
        let nodes: Vec<PrdNode> = doc
            .nodes
            .iter()
            .map(|n| {
                let key = if n.task.is_empty() { &n.id } else { &n.task };
                let found = if n.feature {
                    None
                } else {
                    live::find(&subs, key)
                };
                let shown = found.map_or(n.agent.as_str(), |s| s.agent.as_str());
                PrdNode {
                    phase: to_i32(n.phase),
                    kind: sv(if n.feature { "feature" } else { "sub" }),
                    id: sv(&n.id),
                    title: sv(&n.title),
                    status: sv(&n.status),
                    req: sv(&n.req),
                    task: sv(&n.task),
                    agent: sv(&n.agent),
                    files: sv(&n.files),
                    done_when: sv(&n.done_when),
                    depends: sv(&n.depends),
                    live: found.map_or(-1, |s| s.status),
                    live_agent: sv(found.map_or("", |s| s.agent.as_str())),
                    live_activity: sv(found.map_or("", |s| s.activity.as_str())),
                    initial: sv(&live::initial(shown)),
                }
            })
            .collect();
        ui.set_nodes(ModelRc::new(VecModel::from(nodes)));
    }

    /// Subagents of the open session (Orchestra live data) for linking cards to their worker.
    fn fleet_subs(&self) -> Vec<Sub> {
        let fleet = self.window.global::<ChatUi>().get_fleet();
        fleet
            .iter()
            .flat_map(|group| group.subagents.iter().collect::<Vec<_>>())
            .map(|row| Sub {
                agent: if row.agent_name.is_empty() {
                    row.agent.to_string()
                } else {
                    row.agent_name.to_string()
                },
                title: row.title.to_string(),
                status: row.status,
                activity: row.activity.to_string(),
                started: row.started_s,
            })
            .collect()
    }

    /// The PRD named in run requests: its title and `tasks.md` path.
    fn prd_ref(&self) -> String {
        let ui = self.prd_ui();
        let title = ui.get_title().to_string();
        let spec = ui.get_spec_path().to_string();
        let tasks = Path::new(&spec).parent().map_or_else(
            || spec.clone(),
            |dir| dir.join("tasks.md").to_string_lossy().replace('\\', "/"),
        );
        if title.is_empty() {
            format!("the PRD ({tasks})")
        } else {
            format!("the PRD \"{title}\" ({tasks})")
        }
    }

    /// Sends a run request in PRD mode: to the open chat, else to a new one. The page stays open so the
    /// cards show the subagents as they start.
    fn prd_send(&mut self, text: String) {
        if let Some(session) = self.active.clone() {
            // add_modes sees the Prd item and makes PRD mode sticky for this chat.
            let mut context = vec![ContextItem::Prd];
            self.add_modes(&session, &mut context);
            self.send_message(session, text, context);
        } else {
            let mut context = Vec::new();
            self.take_draft_modes(&mut context);
            if !context.contains(&ContextItem::Prd) {
                context.push(ContextItem::Prd);
            }
            self.pending_send = Some((text, context));
            self.new_session(false, None);
        }
        self.show_modes();
    }

    /// "Run phase": the agent delegates every ready task of the phase, one subagent per task.
    fn prd_run_phase(&mut self, phase: &str) {
        let prd = self.prd_ref();
        self.prd_send(format!(
            "Run {phase} of {prd}. For each task of this phase whose dependencies are done, delegate one \
             subagent with the harness-delegate tools (parallel only when target files are disjoint): \
             claim_paths on its target files first, and start its task text with \"<TASK id>: <task \
             title>\". Check each result against its Done when, release the paths, and update status and \
             assignedAgent in tasks.md and prd.json."
        ));
    }

    /// "Run" on a card: the agent delegates that one task to a subagent.
    fn prd_run_task(&mut self, task: &str) {
        let prd = self.prd_ref();
        self.prd_send(format!(
            "Run {task} of {prd}. Delegate it to one subagent with the harness-delegate tools: \
             claim_paths on its target files first, start the task text with \"{task}: <task title>\", \
             and give it only this task (REQ ids, acceptance criteria, allowed files, Done when). Check \
             the result against its Done when, release the paths, and update status and assignedAgent in \
             tasks.md and prd.json."
        ));
    }
}

pub(super) fn wire(window: &crate::ui::AppWindow) {
    let ui = window.global::<PrdUi>();
    ui.on_refresh(|| {
        with_state(|s| {
            let want = s.selected_slug();
            s.prd_refresh(want);
        });
    });
    ui.on_select(|index| {
        with_state(|s| {
            let slug = s
                .prd_ui()
                .get_specs()
                .row_data(usize::try_from(index).unwrap_or(0))
                .map(|spec| spec.slug.to_string())
                .unwrap_or_default();
            s.prd_ui().set_selected(index);
            s.prd_refresh(slug);
        });
    });
    ui.on_run_phase(|phase| with_state(|s| s.prd_run_phase(&phase)));
    ui.on_run_task(|task| with_state(|s| s.prd_run_task(&task)));
}
