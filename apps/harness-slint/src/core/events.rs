use super::{
    api, AgentId, AgentStatus, FleetSession, Item, ItemId, QueuedMessage, SessionId,
    SessionSummary, Settings, ToolStatus, TurnStatus,
};

// 9.2 的新事件字段由 B 的 9.2 UI 读取；在其接入前不算死代码错误。
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum Event {
    Settings(Settings),
    /// What an agent advertised (hub `agent_options`); pairs are (id, display name). `known == false` until the
    /// agent was started once (lists are empty then).
    AgentOptions {
        agent: AgentId,
        modes: Vec<(String, String)>,
        models: Vec<(String, String)>,
        efforts: Vec<(String, String)>,
        known: bool,
    },
    Agents(Vec<AgentStatus>),
    /// Agents overview rows (installed, version, login presence, runtime, quota).
    AgentsOverview(Vec<api::AgentOverview>),
    /// Result of `Command::TestAgent`.
    AgentTested(api::AgentTestResult),
    /// Account list after `Command::Accounts`, or the error of the call.
    Accounts(Result<Vec<api::AccountInfo>, String>),
    /// Provider list after `Command::Providers`, or the error of the call.
    Providers(Result<Vec<api::ProviderInfo>, String>),
    /// Antigravity account list after `Command::AgyAccounts`; `sign_in` names the account whose login console
    /// the UI opens now (agy was just signed out for it).
    AgyAccounts {
        sign_in: Option<String>,
        result: Result<api::AgyAccounts, String>,
    },
    /// First-run setup rows after `Command::Setup(Status)` or an install, or the error of the call (§9.10).
    SetupStatus(Result<api::SetupStatus, String>),
    /// Progress of an adapter install: `Running` carries one output line, the final status a message.
    AdapterInstall {
        agent: AgentId,
        status: api::AdapterInstallStatus,
        line: Option<String>,
        message: Option<String>,
    },
    Sessions(Vec<SessionSummary>),
    SessionOpened {
        session: SessionSummary,
        items: Vec<(ItemId, Item)>,
        sticky: AgentId,
        queued: Vec<QueuedMessage>,
        paused: bool,
    },
    ItemAdded {
        session: SessionId,
        id: ItemId,
        item: Item,
    },
    TextDelta {
        session: SessionId,
        id: ItemId,
        text: String,
    },
    TurnEnded {
        session: SessionId,
        id: ItemId,
        status: TurnStatus,
    },
    ToolUpdated {
        session: SessionId,
        id: ItemId,
        status: ToolStatus,
    },
    ToolTitle {
        session: SessionId,
        id: ItemId,
        title: String,
    },
    PermissionRequested {
        session: SessionId,
        request: u64,
        title: String,
        detail: String,
    },
    PermissionCleared {
        session: SessionId,
        request: u64,
    },
    Sticky {
        session: SessionId,
        agent: AgentId,
    },
    Running {
        session: SessionId,
        running: bool,
    },
    Queue {
        session: SessionId,
        items: Vec<QueuedMessage>,
    },
    Paused {
        session: SessionId,
        paused: bool,
    },
    /// Exact answers of a permission request; follows each `PermissionRequested` (answer with `ChoosePermission`).
    PermissionChoices {
        session: SessionId,
        request: u64,
        options: Vec<PermissionChoice>,
    },
    /// Expandable tool card content (R8); replace semantics. Sent after `ItemAdded` / `ToolUpdated` when the agent
    /// sent content, and for every tool card right after `SessionOpened`.
    ToolDetail {
        session: SessionId,
        id: ItemId,
        kind: Option<String>,
        content: Vec<api::ToolContent>,
        locations: Vec<api::ToolLocation>,
        raw_input: Option<String>,
        raw_output: Option<String>,
    },
    /// A subagent anywhere under the open session read or edited a file (drives the Live tab's Subagents follow).
    SubagentTool {
        root: SessionId,
        title: String,
        kind: Option<String>,
        content: Vec<api::ToolContent>,
        locations: Vec<api::ToolLocation>,
    },
    /// Thinking delta of the running turn (live only, not persisted).
    Thought {
        session: SessionId,
        text: String,
    },
    /// Agent plan / todo list; replace semantics. Also sent right after `SessionOpened` (empty = none).
    Plan {
        session: SessionId,
        entries: Vec<api::PlanEntry>,
    },
    /// Subagents the session delegated to (phase-9-parity.md §9.8), newest first; replace semantics. Also sent
    /// right after `SessionOpened`.
    Subagents {
        session: SessionId,
        items: Vec<api::SubagentView>,
    },
    Fleet(Vec<FleetSession>),
    /// Background tasks of the session (§9.17), newest first; replace semantics. Also sent right after
    /// `SessionOpened`.
    BackgroundTasks {
        session: SessionId,
        tasks: Vec<api::BackgroundTask>,
    },
    /// The opened session is a subagent of `parent` (`(id, title)`), or a top-level session (`None`). Sent right
    /// after `SessionOpened`.
    ParentLink {
        session: SessionId,
        parent: Option<(String, String)>,
    },
    SessionMeta {
        session: SessionId,
        worktree: Option<api::WorktreeInfo>,
        cost: Option<api::SessionCost>,
        /// Output tokens the agent reported for the session's last turn, when that turn has finished and the
        /// agent reports token counts (generation speed, `speed.rs`).
        tokens: Option<u64>,
    },
    /// Context meter; also sent right after `SessionOpened` when known.
    Usage {
        session: SessionId,
        usage: api::ContextUsage,
    },
    /// The agent's slash commands changed: re-ask `ChatCall::Commands`.
    Commands {
        agent: AgentId,
    },
    /// Groups changed: re-ask `ChatCall::ListGroups`.
    Groups,
    /// Scheduled prompts changed: re-ask `ChatCall::ListSchedules`.
    Schedules,
    Board {
        workspace_key: String,
    },
    Inbox,
    /// A turn's files were restored (undo / reapply); `item` is the turn's user bubble when shown.
    FilesRestored {
        session: SessionId,
        item: Option<ItemId>,
        direction: api::ChangeDirection,
    },
    /// Mode / model / effort pickers of a session (phase-9-parity.md §9.2.5 R13); replace semantics. Sent after
    /// `SessionOpened`, before (`pending`) and after each `Set*` command, when the agent re-advertises, when the
    /// sticky agent changes and when the session starts or stops running (`locked` = a turn runs; a choice then
    /// applies from the next prompt). Pairs are (id, display name); `known == false` until the agent ran once.
    SessionOptions {
        session: SessionId,
        agent: AgentId,
        mode: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        /// Config option id that carries the effort (`effort`, or the agent's `thought_level` option).
        effort_id: Option<String>,
        /// Config option id behind the model picker when the agent lists its models only as a config option
        /// (`None` = the ACP model list, set with `session/set_model`).
        model_id: Option<String>,
        modes: Vec<(String, String)>,
        models: Vec<(String, String)>,
        efforts: Vec<(String, String)>,
        known: bool,
        /// A `Set*` call is in flight.
        pending: bool,
        locked: bool,
        machine: bool,
    },
    /// Typed failure (R20). `session: None` = the agent hub failed to start (`Command::RetryInit { session: None }`
    /// retries); otherwise a turn failed, `item` is its user bubble when shown. Sent before the failed turn's
    /// `TurnEnded` / failed assistant item.
    Failure {
        session: Option<SessionId>,
        item: Option<ItemId>,
        kind: api::FailureKind,
        message: String,
        details: Option<String>,
        trace: Option<String>,
        retry: api::RetryHint,
    },
    Error(String),
}

/// One answer a permission request offers.
#[allow(dead_code)] // read by the 9.2 UI (agent B)
#[derive(Clone, Debug)]
pub struct PermissionChoice {
    pub id: String,
    pub name: String,
    pub kind: api::PermissionOptionKind,
}
