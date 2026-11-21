use super::{
    now_ms, sv, BackgroundTask, BackgroundTaskRow, HashMap, ModelRc, RefCell, SubagentRow,
    SubagentStatus, SubagentView, VecDeque,
};

pub(in crate::bridge) fn subagent_row(s: &SubagentView) -> SubagentRow {
    let status = match s.status {
        SubagentStatus::Running => 0,
        SubagentStatus::NeedsApproval => 1,
        SubagentStatus::Done => 2,
        SubagentStatus::Failed => 3,
        SubagentStatus::Cancelled => 4,
        SubagentStatus::Interrupted => 5,
    };
    let secs = |ms: i64| i32::try_from(ms / 1000).unwrap_or(i32::MAX);
    SubagentRow {
        id: sv(s.session_id.as_str()),
        agent: sv(s.agent.as_str()),
        title: sv(&s.title),
        status,
        activity: sv(&s.activity),
        preview: sv(&s.result_preview),
        started_s: secs(s.started_at),
        // 0 = still running: the panel measures against `now-s`.
        finished_s: s.finished_at.map_or(0, secs),
        agent_name: sv(s.agent.as_str()),
        trail: ModelRc::default(),
        steps: 0,
        scope: sv(&scope_text(s)),
    }
}

/// Scope line of the subagent card (§9.8.2): paths, tools, network and time limit.
fn scope_text(s: &SubagentView) -> String {
    let Some(scope) = &s.scope else {
        return String::new();
    };
    let mut parts = vec![if scope.paths.is_empty() {
        "Whole workspace".to_owned()
    } else {
        scope.paths.join(", ")
    }];
    if !scope.tools.is_empty() {
        parts.push(scope.tools.join(", "));
    }
    if !scope.network {
        parts.push("No network".to_owned());
    }
    if let Some(deadline) = scope.expires_at {
        let mins = ((deadline - now_ms()).max(0)) / 60_000;
        parts.push(if mins >= 1 {
            format!("{mins} min left")
        } else {
            "Expiring now".to_owned()
        });
    }
    parts.join(" · ")
}

/// What a subagent did, kept by the UI for the team strip: the core only reports the current activity.
#[derive(Default)]
pub(super) struct Trail {
    /// The activity reported last (still shown as "now" while it runs).
    pub(super) last: String,
    /// The ones before it, newest first.
    pub(super) before: VecDeque<String>,
    pub(super) steps: u32,
}

/// Steps kept per subagent besides the current one.
pub(super) const TRAIL_STEPS: usize = 3;

thread_local! {
    static TRAILS: RefCell<HashMap<String, Trail>> = RefCell::default();
}

impl Trail {
    /// Records `activity` when it is a new step. Returns the steps to show before the current one: when the
    /// subagent reports no activity (it is thinking), the last finished step leads that list.
    pub(super) fn note(&mut self, activity: &str) -> Vec<String> {
        if !activity.is_empty() && activity != self.last {
            if !self.last.is_empty() {
                self.before.push_front(std::mem::take(&mut self.last));
                self.before.truncate(TRAIL_STEPS);
            }
            activity.clone_into(&mut self.last);
            self.steps += 1;
        }
        let lead = (activity.is_empty() && !self.last.is_empty()).then(|| self.last.clone());
        lead.into_iter()
            .chain(self.before.iter().cloned())
            .take(TRAIL_STEPS)
            .collect()
    }
}

/// Team-strip facts of the subagents in `items` (trail, step count), in the same order. Subagents that left
/// the list are forgotten.
pub(super) fn trails(items: &[SubagentView]) -> Vec<(Vec<String>, u32)> {
    TRAILS.with(|cell| {
        let mut map = cell.borrow_mut();
        map.retain(|id, _| items.iter().any(|s| s.session_id.as_str() == id));
        items
            .iter()
            .map(|s| {
                let trail = map.entry(s.session_id.as_str().to_owned()).or_default();
                let shown = trail.note(&s.activity);
                (shown, trail.steps)
            })
            .collect()
    })
}

pub(super) fn background_row(t: &BackgroundTask) -> BackgroundTaskRow {
    let secs = |ms: i64| i32::try_from(ms / 1000).unwrap_or(i32::MAX);
    BackgroundTaskRow {
        id: sv(&t.id),
        agent: sv(t.agent.as_str()),
        name: sv(&t.name),
        description: sv(&t.description),
        summary: sv(&t.summary),
        state: sv(&t.state),
        running: t.is_running(),
        can_stop: t.can_stop,
        started_s: secs(t.started_at),
        finished_s: t.finished_at.map_or(0, secs),
    }
}
