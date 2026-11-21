//! Subagent registry and live status tracker (phase-9-parity.md §9.8).
//!
//! Single owner of the `subagents` table and the only emitter of `EventKind::SubagentUpdated`. Status is derived
//! from the child session's own hub events by [`track`].

use std::collections::HashMap;
use std::sync::{Mutex, Weak};

use harness_core_api::{
    EventKind, RecvError, ScopeView, SessionId, SubagentStatus, SubagentView, ToolCallView,
    TurnOutcome,
};
use rusqlite::{params, Connection};

use crate::error::HubError;
use crate::hub::HubCtx;
use crate::util::now_ms;

mod scope;

pub(crate) use scope::{allows_path, blocked, describe, narrow, relative_to};

/// Characters of the answer kept as `result_preview`.
pub(crate) const PREVIEW_CHARS: usize = 300;

/// Loopback endpoint of the delegate server (address + per-run token).
#[derive(Clone)]
pub(crate) struct Endpoint {
    pub(crate) addr: String,
    pub(crate) token: String,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 令牌不得出现在日志里（§9.8 Transport）。
        f.debug_struct("Endpoint")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

/// In-memory copy of the `subagents` table (child id → view).
#[derive(Default)]
pub(crate) struct Registry {
    views: Mutex<HashMap<SessionId, SubagentView>>,
    pub(crate) endpoint: std::sync::OnceLock<Endpoint>,
    /// Notes, report marks and path claims (§9.8.1).
    pub(crate) coord: crate::delegate_coord::Coordination,
}

impl Registry {
    /// Loads every row; rows still active become `interrupted` (the hub that ran them is gone).
    pub(crate) fn load(c: &Connection) -> Result<Self, HubError> {
        let mut statement = c.prepare("SELECT json FROM subagents")?;
        let rows = statement
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut views = HashMap::new();
        for json in rows {
            let Ok(mut view) = serde_json::from_str::<SubagentView>(&json) else {
                continue;
            };
            if view.status.is_active() {
                view.status = SubagentStatus::Interrupted;
                view.finished_at.get_or_insert(view.started_at);
                save(c, &view)?;
            }
            views.insert(view.session_id.clone(), view);
        }
        Ok(Self {
            views: Mutex::new(views),
            endpoint: std::sync::OnceLock::new(),
            coord: crate::delegate_coord::Coordination::default(),
        })
    }

    pub(crate) fn get(&self, child: &SessionId) -> Option<SubagentView> {
        self.views.lock().ok().and_then(|v| v.get(child).cloned())
    }

    pub(crate) fn is_child(&self, session: &SessionId) -> bool {
        self.views.lock().is_ok_and(|v| v.contains_key(session))
    }

    /// Subagents of `parent`, newest first.
    pub(crate) fn of_parent(&self, parent: &SessionId) -> Vec<SubagentView> {
        let mut out: Vec<SubagentView> = self
            .views
            .lock()
            .map(|v| {
                v.values()
                    .filter(|s| &s.parent_id == parent)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        out
    }

    /// Active direct subagents of `parent` (their own subagents are not counted).
    pub(crate) fn active_children(&self, parent: &SessionId) -> usize {
        self.views.lock().map_or(0, |v| {
            v.values()
                .filter(|s| s.status.is_active() && &s.parent_id == parent)
                .count()
        })
    }

    fn insert(&self, view: SubagentView) {
        if let Ok(mut views) = self.views.lock() {
            views.insert(view.session_id.clone(), view);
        }
    }
}

pub(crate) const TABLE: &str = "CREATE TABLE IF NOT EXISTS subagents(child_id TEXT PRIMARY KEY, parent_id TEXT NOT NULL, json TEXT NOT NULL);";

pub(crate) fn save(c: &Connection, view: &SubagentView) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO subagents(child_id,parent_id,json) VALUES(?1,?2,?3)
         ON CONFLICT(child_id) DO UPDATE SET json=?3",
        params![
            view.session_id.as_str(),
            view.parent_id.as_str(),
            serde_json::to_string(view)?
        ],
    )?;
    Ok(())
}

/// Parent of a subagent session (for `SessionSummary.parent_id`).
pub(crate) fn parent_of(c: &Connection, child: &SessionId) -> Result<Option<SessionId>, HubError> {
    use rusqlite::OptionalExtension;
    Ok(c.query_row(
        "SELECT parent_id FROM subagents WHERE child_id=?1",
        params![child.as_str()],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(SessionId::new))
}

impl HubCtx {
    /// Single write path of a subagent view: memory, store, `SubagentUpdated` on the parent.
    pub(crate) async fn put_subagent(&self, view: SubagentView) {
        if self.subagents.get(&view.session_id).as_ref() == Some(&view) {
            return;
        }
        self.subagents.insert(view.clone());
        let stored = view.clone();
        if let Err(error) = self.store.run(move |c| save(c, &stored)).await {
            tracing::warn!(%error, "saving subagent failed");
        }
        let parent = view.parent_id.clone();
        self.events
            .emit(Some(&parent), EventKind::SubagentUpdated { subagent: view });
    }
}

/// First `PREVIEW_CHARS` characters of an answer, whitespace collapsed.
pub(crate) fn preview(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = collapsed.chars().take(PREVIEW_CHARS).collect();
    if collapsed.chars().count() > PREVIEW_CHARS {
        out.push('…');
    }
    out
}

/// Follows hub events and keeps every subagent's status, agent and activity current. Holds only a weak
/// reference so the hub can drop; ends when the event bus closes.
pub(crate) async fn track(ctx: Weak<HubCtx>) {
    let Some(mut events) = ctx.upgrade().map(|c| c.events.subscribe()) else {
        return;
    };
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return,
        };
        let Some(ctx) = ctx.upgrade() else {
            return;
        };
        let Some(child) = event.session_id.as_ref() else {
            continue;
        };
        if let EventKind::TurnFinished { turn_id, .. } = &event.kind {
            // §9.8.1：会话一轮结束即释放它的路径占用；子会话的结束轮次登记为待汇报，供 subagent_wait 取走。
            ctx.subagents.coord.release(child, &[]);
            if let Some(view) = ctx.subagents.get(child) {
                ctx.subagents
                    .coord
                    .turn_finished(&view.parent_id, child, turn_id);
            }
        }
        let Some(current) = ctx.subagents.get(child) else {
            continue;
        };
        if let Some(next) = advance(current, &event.kind) {
            ctx.put_subagent(next).await;
        }
    }
}

/// Status change of one subagent for one of its session's events (`None` = no change).
fn advance(mut view: SubagentView, kind: &EventKind) -> Option<SubagentView> {
    match kind {
        EventKind::TurnStarted { agent, .. } => {
            if !view.status.is_active() {
                // 用户直接在子会话里继续对话：视为新一轮，重新计时。
                view.started_at = now_ms();
                view.finished_at = None;
                view.activity.clear();
            }
            view.status = SubagentStatus::Running;
            view.agent = agent.clone();
        }
        EventKind::ToolCall { tool, .. } | EventKind::ToolCallUpdated { tool, .. } => {
            let title = tool.title.trim();
            if title.is_empty() || title == view.activity {
                return None;
            }
            view.activity = title.chars().take(160).collect();
        }
        EventKind::PermissionRequested { .. } => view.status = SubagentStatus::NeedsApproval,
        EventKind::PermissionResolved { .. } if view.status == SubagentStatus::NeedsApproval => {
            view.status = SubagentStatus::Running;
        }
        EventKind::TurnFinished { outcome, .. } => {
            view.status = match outcome {
                TurnOutcome::Completed { .. } => SubagentStatus::Done,
                TurnOutcome::Cancelled => SubagentStatus::Cancelled,
                TurnOutcome::Failed { .. } => SubagentStatus::Failed,
            };
            view.finished_at = Some(now_ms());
        }
        _ => return None,
    }
    Some(view)
}

impl HubCtx {
    /// Scope of a subagent session; `None` for a top-level session (unrestricted).
    pub(crate) fn scope_of(&self, session: &SessionId) -> Option<ScopeView> {
        self.subagents.get(session).and_then(|view| view.scope)
    }
}

#[cfg(test)]
mod tests;
