//! The hub: [`HarnessCore`] implementation, shared context, actor registry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use harness_core::WorkspaceKey;
use harness_core_api::{
    AccountInfo, AddAccount, AddProvider, AgentOverview, AgentTestResult, ArchivePage,
    ListArchived, ListProjects, MemoryDoc, MemoryFileInfo, MemoryPlan, ProjectInfo, ProviderInfo,
    SessionSelection, UpdateProvider,
};
use harness_core_api::{
    AgentId, AgentInfo, ApiError, ApiResult, BoxFuture, CreateSession, ErrorClass, EventKind,
    EventStream, HarnessCore, ListSessions, MemoryScope, MemoryWriteMode, PermissionRequestId,
    QuotaState, ResumableRun, SendAccepted, SendMessage, SessionDetail, SessionId, SessionSummary,
    TurnDiff, TurnId,
};
use harness_core_api::{AgentOptions, SessionConfigOptions, SessionModels, SessionModes};
use harness_core_api::{
    ApplyFileChanges, ChangeDirection, CommandList, CommandSearch, CommandSearchResult,
    ContextItem, EditResend, FileChangePlan, FileRestoreReport, FindHit, FindInSession,
    ForkSession, GroupColor, MentionCandidate, MentionQuery, ResolvedItem, Retry, Rewind,
    RewindResult, SessionGroup, UpdateGroup,
};
use harness_core_api::{
    ImportCandidate, ImportReport, ImportRequest, ImportScan, NewSchedule, ScheduleRun,
    ScheduledPrompt, UpdateSchedule,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::agents::AgentSpec;
use crate::bus::EventBus;
use crate::config::HubConfig;
use crate::error::HubError;
use crate::memory::{MemoryFiles, MemoryTarget};
use crate::options::{self, Advertised, PrefChange, Prefs, CACHE_PREFIX};
use crate::pool::AgentPool;
use crate::quota::{AgentErrorInfo, QuotaRegistry};
use crate::session::{ActorMsg, ActorTx, SessionActor, SessionCmd};
use crate::store::{self, SessionRow, Store};
use crate::util::{new_id, now_ms, now_secs};

mod api;
mod ctx;
mod sessions;
mod start;

const FALLBACK_KEY: &str = "fallback_order";

/// State shared by the hub and its session actors.
pub(crate) struct HubCtx {
    pub(crate) config: HubConfig,
    pub(crate) store: Store,
    pub(crate) pool: AgentPool,
    pub(crate) quota: QuotaRegistry,
    pub(crate) events: EventBus,
    pub(crate) memory: MemoryFiles,
    pub(crate) specs: HashMap<AgentId, AgentSpec>,
    fallback: Mutex<Vec<AgentId>>,
    /// Advertised modes/models/options per agent (lazy cache of store keys `agent_options:<id>`).
    advertised: Mutex<HashMap<AgentId, Advertised>>,
    /// Serializes option probes (phase-9-parity.md §9.1 `agent_options` probe).
    probe_lock: tokio::sync::Mutex<()>,
    /// Lazy chat caches (prompt capabilities, advertised slash commands).
    pub(crate) chat: crate::hub_chat::ChatState,
    /// Subagent sessions and the delegate endpoint (phase-9-parity.md §9.8).
    pub(crate) subagents: crate::delegate::Registry,
    pub(crate) machine: crate::machine::Machine,
    pub(crate) routers: crate::inbox::Routers,
    pub(crate) agy: crate::agy_ops::AgyState,
    /// Next member of each key pool (`config.key_pools`) for the round robin over API keys.
    rotation: Mutex<HashMap<usize, usize>>,
}

/// The agent hub. Create with [`Hub::start`]; use through [`HarnessCore`].
pub struct Hub {
    pub(crate) ctx: Arc<HubCtx>,
    pub(crate) actors: tokio::sync::Mutex<HashMap<SessionId, ActorTx>>,
    shut_down: AtomicBool,
    /// Weak self for the lazy scheduler task (never keeps the hub alive).
    pub(crate) me: std::sync::Weak<Hub>,
    pub(crate) scheduler: crate::schedule::Scheduler,
    pub(crate) inbox: crate::inbox::Inbox,
    pub(crate) board_sync: crate::board_sync::BoardSyncSlot,
    /// Accept task of the delegate server (aborted on shutdown).
    delegate_task: Mutex<Option<tokio::task::AbortHandle>>,
}
