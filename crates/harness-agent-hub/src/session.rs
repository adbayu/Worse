//! Session actor (spec §6.4, §7, §9): single owner of one session's transcript, queue, sticky agent,
//! native handles and pending permissions. Every input (UI commands, agent traffic, driver results)
//! arrives through one unbounded FIFO inbox, which gives the ordering guarantee of spec §9.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use harness_core_api::{
    AgentId, ApiError, ApiResult, ErrorClass, EventKind, PauseReason, PermissionDecision,
    PermissionOption, PermissionOptionKind, PermissionRequestId, PermissionRequestView, QuotaState,
    SendAccepted, SendMessage, SessionDetail, SwitchReason, ToolCallStatus, ToolCallView,
    ToolContent, TurnAttempt, TurnId, TurnOutcome, TurnStatus, TurnView,
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use crate::acp::{AcpConnection, Inbound, RouteSink, RpcError};
use crate::catchup::{
    catchup_block, clip_head_tail, partial_block, seed_block, ContextTurn, ToolDigest,
    TOOL_OUTPUT_CHARS,
};
use crate::hub::HubCtx;
use crate::memory::{injection_block, MemoryTarget};
use crate::options::{PrefChange, Prefs};
use crate::quota::{format_utc, AgentErrorInfo};
use crate::store::{self, HandleRow, SessionRow, ToolMark};
use crate::util::{new_id, now_ms, now_secs};

mod agy_guard;
mod background;
mod chat;
mod queue;
pub(crate) use chat::{Resend, RewindCmd, RewindOutcome};

mod admission;
mod commands;
mod driver_setup;
mod inbound;
mod permissions;
mod prompt;
mod tool_views;
mod turn_end;

use driver_setup::{drive_setup, take_after_snapshot, take_before_snapshot};
use tool_views::{permission_option, scope_paths, text_block, tool_digests, tool_view};

/// UI-originated commands.
pub(crate) enum SessionCmd {
    Send(SendMessage, oneshot::Sender<ApiResult<SendAccepted>>),
    SetAgent(AgentId, oneshot::Sender<ApiResult<()>>),
    Stop(oneshot::Sender<ApiResult<()>>),
    CancelQueued(TurnId, oneshot::Sender<ApiResult<()>>),
    RespondPermission(PermissionRequestId, String, oneshot::Sender<ApiResult<()>>),
    Snapshot(oneshot::Sender<ApiResult<SessionDetail>>),
    Shutdown(oneshot::Sender<()>),
    /// Active agent and the session's own choices for it (phase-9-parity.md §9.1.5).
    Prefs(oneshot::Sender<ApiResult<(AgentId, Prefs)>>),
    SetPref(PrefChange, oneshot::Sender<ApiResult<()>>),
    /// Archive/delete (phase-9-parity.md §9.1.5a): refuses with `Busy` while a turn is running, queued or paused
    /// or a permission is pending; otherwise answers `Ok` and the actor exits.
    Retire(oneshot::Sender<ApiResult<()>>),
    /// Send of a scheduled prompt: the turn carries its origin (phase-9-parity.md §9.2.1, TL-014).
    SendScheduled(
        SendMessage,
        harness_core_api::TurnOrigin,
        oneshot::Sender<ApiResult<SendAccepted>>,
    ),
    /// Title (the actor owns `row.title`).
    Rename(String, oneshot::Sender<ApiResult<()>>),
    /// Rewind / edit-and-resend / retry (truncate at a turn, optionally restore files, optionally resend).
    Rewind(RewindCmd, oneshot::Sender<ApiResult<RewindOutcome>>),
    /// Undo / reapply a turn's file changes (refused while busy).
    ApplyFiles(
        harness_core_api::ApplyFileChanges,
        oneshot::Sender<ApiResult<harness_core_api::FileRestoreReport>>,
    ),
    /// Queue edit / reorder / run-now / steer (phase-9-parity.md §9.2.5 R12).
    EditQueued(TurnId, String, oneshot::Sender<ApiResult<()>>),
    ReorderQueued(Vec<TurnId>, oneshot::Sender<ApiResult<()>>),
    RunQueuedNow(TurnId, oneshot::Sender<ApiResult<()>>),
    Steer(
        SendMessage,
        oneshot::Sender<ApiResult<harness_core_api::SteerAccepted>>,
    ),
    StopBackgroundTask(String, oneshot::Sender<ApiResult<()>>),
}

pub(crate) struct Ready {
    agent: AgentId,
    connection: Arc<AcpConnection>,
    native: String,
    fresh: bool,
    /// `session/new` / `session/load` result (option advertisement), `None` when reused.
    advertised: Option<Value>,
    /// Choices now in effect in the native session.
    applied: Prefs,
}

/// What the driver needs to bring a native session to the session's choices.
struct PrefPlan {
    own: Prefs,
    defaults: Option<harness_core_api::AgentDefaults>,
    applied: Option<Prefs>,
}

/// Everything the actor consumes, in one FIFO.
pub(crate) enum ActorMsg {
    Cmd(SessionCmd),
    Inbound(Inbound),
    Ready(u64, Result<Ready, AgentErrorInfo>),
    PromptDone(u64, Result<Value, RpcError>),
    SteerDone(Result<Value, RpcError>),
    Wake,
    AgentExited(AgentId, u64),
    IdleEnd(u64, u64),
}

pub(crate) type ActorTx = mpsc::UnboundedSender<ActorMsg>;

struct Handle {
    native: String,
    last_seq: u32,
    /// Pool generation of the process that has this native session loaded.
    generation: Option<u64>,
    /// Mode/model/config choices applied to the native session in that generation.
    applied: Option<Prefs>,
}

struct Running {
    turn_id: TurnId,
    agent: AgentId,
    key: u64,
    connection: Option<Arc<AcpConnection>>,
    native: Option<String>,
    text: String,
    thought: String,
    cancelling: bool,
    prompt_sent: bool,
    autonomous: Option<background::Autonomous>,
}

struct PendingPermission {
    view: PermissionRequestView,
    rpc_id: Value,
    connection: Arc<AcpConnection>,
}

pub(crate) struct SessionActor {
    ctx: Arc<HubCtx>,
    row: SessionRow,
    turns: Vec<TurnView>,
    client_ids: HashMap<String, TurnId>,
    requested: HashMap<TurnId, AgentId>,
    /// Index of the first attempt of the current failover round per turn (a pause ends a round).
    round_base: HashMap<TurnId, usize>,
    queue: VecDeque<TurnId>,
    running: Option<Running>,
    paused: Option<TurnId>,
    handles: HashMap<AgentId, Handle>,
    pending: Vec<PendingPermission>,
    tx: ActorTx,
    next_key: u64,
    /// Where tool calls appeared in the live stream, per turn (segments, spec §15.1 request 4).
    tool_marks: HashMap<TurnId, Vec<ToolMark>>,
    /// Turns whose `before` snapshot was requested (spec §15.2); they get an `after` snapshot at the end.
    snapshotted: HashSet<TurnId>,
    /// Mode/model/config choices per agent (owner of `session_prefs` rows of this session).
    prefs: HashMap<AgentId, Prefs>,
    steering: Option<queue::SteerFlight>,
    tasks: Vec<background::Tracked>,
}

const ACTOR_IDLE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

impl SessionActor {
    fn evictable(&self) -> bool {
        !self.busy() && self.tasks.is_empty() && self.snapshotted.is_empty()
    }

    /// Loads a session from the store and starts its actor task.
    pub(crate) async fn spawn(ctx: Arc<HubCtx>, row: SessionRow) -> ApiResult<ActorTx> {
        let id = row.id.clone();
        let (turns, handles, prefs) = ctx
            .store
            .run(move |c| {
                Ok((
                    store::load_turns_slim(c, &id)?,
                    store::load_handles(c, &id)?,
                    store::load_prefs(c, &id)?,
                ))
            })
            .await?;
        let (tx, rx) = mpsc::unbounded_channel();
        let mut client_ids = HashMap::new();
        let turns = turns
            .into_iter()
            .map(|(turn, client_id)| {
                if let Some(client_id) = client_id {
                    client_ids.insert(client_id, turn.id.clone());
                }
                turn
            })
            .collect();
        let actor = Self {
            ctx,
            row,
            turns,
            client_ids,
            requested: HashMap::new(),
            round_base: HashMap::new(),
            queue: VecDeque::new(),
            running: None,
            paused: None,
            handles: handles
                .into_iter()
                .map(|h| {
                    (
                        h.agent,
                        Handle {
                            native: h.native,
                            last_seq: h.last_seq,
                            generation: None,
                            applied: None,
                        },
                    )
                })
                .collect(),
            pending: Vec::new(),
            tx: tx.clone(),
            next_key: 0,
            tool_marks: HashMap::new(),
            snapshotted: HashSet::new(),
            prefs,
            steering: None,
            tasks: Vec::new(),
        };
        tokio::spawn(actor.run(rx));
        Ok(tx)
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<ActorMsg>) {
        loop {
            let message = match tokio::time::timeout(ACTOR_IDLE, rx.recv()).await {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(_) if self.evictable() => break,
                Err(_) => continue,
            };
            let shutdown = match message {
                ActorMsg::Cmd(SessionCmd::Shutdown(reply)) => {
                    self.on_shutdown().await;
                    let _ = reply.send(());
                    true
                }
                ActorMsg::Cmd(SessionCmd::Retire(reply)) => {
                    if self.busy() {
                        let _ = reply.send(Err(ApiError::Busy(format!(
                            "session {} has a running, queued or paused turn",
                            self.row.id
                        ))));
                        false
                    } else {
                        let _ = reply.send(Ok(()));
                        true
                    }
                }
                ActorMsg::Cmd(command) => {
                    self.on_command(command).await;
                    false
                }
                ActorMsg::Inbound(inbound) => {
                    self.on_inbound(inbound).await;
                    false
                }
                ActorMsg::Ready(key, result) => {
                    self.on_ready(key, result).await;
                    false
                }
                ActorMsg::PromptDone(key, result) => {
                    self.on_prompt_done(key, result).await;
                    false
                }
                ActorMsg::SteerDone(result) => {
                    self.on_steer_done(result).await;
                    false
                }
                ActorMsg::Wake => {
                    self.on_wake().await;
                    false
                }
                ActorMsg::AgentExited(agent, generation) => {
                    self.on_agent_exited(&agent, generation).await;
                    false
                }
                ActorMsg::IdleEnd(key, stamp) => {
                    self.on_idle_end(key, stamp).await;
                    false
                }
            };
            if shutdown {
                break;
            }
        }
    }

    // ---- snapshot -------------------------------------------------------------------------------------

    fn snapshot(&self) -> SessionDetail {
        let mut turns = self.turns.clone();
        if let Some(running) = &self.running {
            // API request 3 (spec §15.1): the running turn carries the text streamed so far. The actor owns both
            // the text and the event order, so no MessageDelta is lost or counted twice by a UI.
            if let Some(turn) = turns.iter_mut().find(|t| t.id == running.turn_id) {
                turn.assistant_text.clone_from(&running.text);
                if let Some(attempt) = turn.attempts.last_mut() {
                    attempt.partial_text.clone_from(&running.text);
                }
                let last_attempt = turn.attempts.last().map_or(1, |a| a.attempt);
                let marks = self
                    .tool_marks
                    .get(&running.turn_id)
                    .map_or(&[][..], Vec::as_slice);
                turn.segments = store::build_segments(&running.text, last_attempt, marks);
            }
        }
        let mut agents_used: Vec<AgentId> = Vec::new();
        for agent in turns
            .iter()
            .flat_map(|t| t.attempts.iter().map(|a| &a.agent).chain(t.agent.iter()))
        {
            if !agents_used.contains(agent) {
                agents_used.push(agent.clone());
            }
        }
        SessionDetail {
            summary: harness_core_api::SessionSummary {
                id: self.row.id.clone(),
                title: self.row.title.clone(),
                workspace_path: self.row.workspace_path.clone(),
                workspace_key: self.row.workspace_key.clone(),
                active_agent: self.row.active_agent.clone(),
                created_at: self.row.created_at,
                updated_at: self.row.updated_at,
                turn_count: u32::try_from(self.turns.len()).unwrap_or(u32::MAX),
                agents_used,
                pinned: false,
                group_id: None,
                order: 0,
                unread: false,
                forked_from: None,
                imported_from: None,
                parent_id: None,
                worktree: None,
            },
            turns,
            queue: self.queue.iter().cloned().collect(),
            pending_permissions: self.pending.iter().map(|p| p.view.clone()).collect(),
            plan: Vec::new(),
            context_usage: None,
            // 修复说明：快照在 actor 内同步构建，此前本会话的事件都已发布；客户端据此丢弃快照已包含的旧事件
            // （否则重试/回退后重开会话时，排在快照之前的 TurnQueued 会覆盖快照的条目映射，留下一直“Streaming”的行）。
            event_seq: self.ctx.events.last_seq(),
            subagents: Vec::new(),
            cost: None,
            background_tasks: self.background_views(),
        }
    }

    fn position_of(&self, turn_id: &TurnId) -> u32 {
        self.queue.iter().position(|t| t == turn_id).map_or(0, |i| {
            u32::try_from(i).unwrap_or(u32::MAX) + u32::from(self.running.is_some())
        })
    }

    fn turn(&self, turn_id: &TurnId) -> Option<&TurnView> {
        self.turns.iter().find(|t| &t.id == turn_id)
    }

    fn turn_mut(&mut self, turn_id: &TurnId) -> Option<&mut TurnView> {
        self.turns.iter_mut().find(|t| &t.id == turn_id)
    }

    fn emit(&self, kind: EventKind) {
        self.ctx.events.emit(Some(&self.row.id), kind);
    }
}
