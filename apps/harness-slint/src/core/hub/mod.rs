//! Adapter from the UI port ([`Command`] / [`Event`]) onto `harness_core_api::HarnessCore`
//! (spec `docs/specs/harness/phase-8-ui.md` §8).
//!
//! The hub owns sessions, turns, the sticky agent, quota/failover, the queue and permissions. This adapter
//! only translates: commands become API calls, hub events become UI items for the one opened session.
//! Theme and language live in the harness-settings store (`<data dir>/settings.json`, single writer, phase-9-parity.md
//! §9.1.2); the fallback order is read from and written to the hub.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_agent_hub::agents::{AgentSpec, ErrorDialect, Transport};
use harness_agent_hub::{Hub, HubConfig};
use harness_core_api::{
    self as api, ApiError, EventKind, HarnessCore, HubEvent, PauseReason, PermissionDecision,
    PermissionOption, PermissionOptionKind, PermissionRequestView, RecvError, SwitchReason,
    ToolCallStatus, ToolCallView, TurnOutcome,
};
use harness_settings::SettingsStore;
use serde::Deserialize;
use tokio::sync::mpsc;

use super::prefs;
use super::{
    AgentId, AgentState, AgentStatus, ChatCall, Command, Event, FindMatch, Item, ItemId, Notice,
    PermissionChoice, QueuedMessage, Reply, SessionSummary, Settings, SettingsCall, ToolStatus,
    TurnStatus,
};

/// One extra agent from `HARNESS_UI_AGENTS` (a JSON file; development and gates only, spec §8).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentOverride {
    id: String,
    name: String,
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

#[derive(Default, Clone)]
struct TurnMap {
    /// Assistant item still streaming (taken when it ends).
    assistant: Option<ItemId>,
    /// User bubble and last assistant item shown for the turn (find-in-session targets).
    user: Option<ItemId>,
    reply: Option<ItemId>,
}

struct PendingPermission {
    request_id: api::PermissionRequestId,
    options: Vec<PermissionOption>,
    tool_call_id: String,
}

/// Item bookkeeping of the opened session.
struct View {
    id: api::SessionId,
    next_item: ItemId,
    turns: HashMap<api::TurnId, TurnMap>,
    tools: HashMap<String, ItemId>,
    merged: HashMap<String, ToolCallView>,
    denied: HashSet<String>,
    /// Queued (not yet running) turns in admission order with their user text. Their user bubble is added
    /// when the turn starts; until then they are shown (and cancellable) in the composer's queue list.
    queued: Vec<(api::TurnId, String)>,
    /// Image paths of queued turns, shown on the user bubble once the turn starts.
    queued_images: HashMap<api::TurnId, Vec<String>>,
    /// Queued turns this UI cancelled; their later `TurnFinished` must not end the running turn.
    cancelled: HashSet<api::TurnId>,
    /// Turn of every shown item: turn actions (fork, rewind, retry, files) take an item id (`ChatCall`).
    item_turn: HashMap<ItemId, api::TurnId>,
    /// Last turn that ended with a typed failure (`Command::RetryInit { session: Some }` re-runs it).
    last_failed: Option<api::TurnId>,
    /// A turn of the session runs (the pickers are `locked`).
    running: bool,
    /// `SessionDetail.event_seq` of the snapshot this view was built from: the session's events at or below it are
    /// already shown.
    since: u64,
    /// Subagents of the session (§9.8), newest first.
    subagents: Vec<api::SubagentView>,
}

impl View {
    fn new(id: api::SessionId) -> Self {
        Self {
            last_failed: None,
            running: false,
            since: 0,
            subagents: Vec::new(),
            id,
            next_item: 1,
            turns: HashMap::new(),
            tools: HashMap::new(),
            merged: HashMap::new(),
            denied: HashSet::new(),
            queued: Vec::new(),
            queued_images: HashMap::new(),
            cancelled: HashSet::new(),
            item_turn: HashMap::new(),
        }
    }

    /// Allocates an item that belongs to `turn`.
    fn alloc_in(&mut self, turn: &api::TurnId) -> ItemId {
        let id = self.alloc();
        self.item_turn.insert(id, turn.clone());
        id
    }

    fn merge_tool(&mut self, tool: &ToolCallView) -> ToolCallView {
        let Some(existing) = self.merged.get_mut(&tool.tool_call_id) else {
            self.merged.insert(tool.tool_call_id.clone(), tool.clone());
            return tool.clone();
        };
        if !tool.title.trim().is_empty() {
            existing.title.clone_from(&tool.title);
        }
        if tool.kind.is_some() {
            existing.kind.clone_from(&tool.kind);
        }
        if tool.status.is_some() {
            existing.status.clone_from(&tool.status);
        }
        if tool.raw_input.is_some() {
            existing.raw_input.clone_from(&tool.raw_input);
        }
        if !tool.content.is_empty() {
            existing.content.clone_from(&tool.content);
        }
        if !tool.locations.is_empty() {
            existing.locations.clone_from(&tool.locations);
        }
        if tool.raw_output.is_some() {
            existing.raw_output.clone_from(&tool.raw_output);
        }
        existing.clone()
    }

    fn queue_items(&self) -> Vec<QueuedMessage> {
        self.queued
            .iter()
            .map(|(id, text)| QueuedMessage {
                id: id.0.clone(),
                text: text.clone(),
            })
            .collect()
    }

    /// Removes `turn` from the queue; returns its user text when it was queued.
    fn dequeue(&mut self, turn: &api::TurnId) -> Option<String> {
        let index = self.queued.iter().position(|(id, _)| id == turn)?;
        Some(self.queued.remove(index).1)
    }

    fn alloc(&mut self) -> ItemId {
        let id = self.next_item;
        self.next_item += 1;
        id
    }
}

/// A session option change (`Command::SetMode` / `SetModel` / `SetConfigOption`).
enum Pref {
    Mode(String),
    Model(String),
    Config(String, String),
    Machine(bool),
}

struct Adapter {
    core: Arc<dyn HarnessCore>,
    ev: mpsc::UnboundedSender<Event>,
    /// Project of the next new session without an explicit folder: the last one a session was created in
    /// (persisted as `general.lastWorkspace`, like Antigravity's last selected project).
    workspace: PathBuf,
    store: Arc<SettingsStore>,
    settings: Settings,
    agents: Vec<api::AgentInfo>,
    sticky: HashMap<api::SessionId, AgentId>,
    view: Option<View>,
    permissions: HashMap<u64, PendingPermission>,
    next_request: u64,
    fleet: super::fleet::Fleet,
}

/// Find hits per call (the hub caps at 500).
const FIND_LIMIT: u32 = 200;

/// Runs `call` on the runtime and answers on `reply`. A dropped receiver (page closed) is not an error.
fn answer<T: Send + 'static>(
    reply: Reply<T>,
    call: impl std::future::Future<Output = Result<T, ApiError>> + Send + 'static,
) {
    tokio::spawn(async move {
        let _ = reply.send(call.await);
    });
}

fn ids(sessions: Vec<String>) -> Vec<api::SessionId> {
    sessions.into_iter().map(api::SessionId::new).collect()
}

/// Request of `Command::CreateSession`: carries the composer's agent so a new session does not fall back to the
/// hub's own order (which starts with Claude Code) and visibly revert the choice.
/// 修复：`workspace` 为 `Some` 时使用该目录（“Open folder…”选择的文件夹），不再重启应用换工作区。
fn create_request(
    workspace: &Path,
    worktree: bool,
    agent: &AgentId,
    target: Option<&Path>,
) -> api::CreateSession {
    let workspace = target.unwrap_or(workspace);
    api::CreateSession {
        workspace_path: workspace.to_string_lossy().into_owned(),
        worktree: worktree.then(api::WorktreeRequest::default),
        agent: (!agent.is_empty()).then(|| api::AgentId::new(agent)),
        ..api::CreateSession::default()
    }
}

/// Item lookup of the opened session for find hits (copied before the call is spawned).
#[derive(Default)]
struct FindTargets {
    turns: HashMap<api::TurnId, (Option<ItemId>, Option<ItemId>)>,
    tools: HashMap<String, ItemId>,
}

impl FindTargets {
    /// Live transcripts split a reply into several items around tool cards; the hit points at the last one
    /// (exact after a re-open, which shows one reply item per turn).
    fn place(&self, hit: api::FindHit) -> FindMatch {
        let turn = self.turns.get(&hit.turn_id).copied().unwrap_or_default();
        let item = match hit.field {
            api::FindField::User => turn.0,
            api::FindField::Assistant => turn.1,
            api::FindField::Tool => hit
                .tool_call_id
                .as_ref()
                .and_then(|id| self.tools.get(id).copied()),
        };
        FindMatch {
            item,
            field: hit.field,
            start: hit.start,
            len: hit.len,
            snippet: hit.snippet,
        }
    }
}

fn is_reject(kind: PermissionOptionKind) -> bool {
    matches!(
        kind,
        PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways
    )
}

mod adapter_base;
mod adapter_chat;
mod adapter_events;
mod adapter_open;
mod adapter_send;
mod adapter_session;
mod convert;
mod runtime;
mod session_flow;
mod snapshot;
#[cfg(test)]
mod tests;

use convert::{
    emit, outcome_status, permission_detail, prompt_title, reset_label, summary, tool_detail,
    tool_status, tool_title, with_content_warnings, DEFAULT_SESSION_TITLE,
};
use runtime::{accounts_call, emit_overview, emit_setup_status, providers_call, settings_call};
pub use runtime::{core_handle, run};
use snapshot::{snapshot_turn, tail_start};
