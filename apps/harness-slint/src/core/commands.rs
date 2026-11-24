use super::{api, AgentId, ItemId, PathBuf, SessionId, SessionSummary};

// 9.2 的新命令（ChoosePermission、SendContext、Chat）由 B 的 9.2 UI 构造；在其接入前不算死代码错误。
#[allow(dead_code)]
#[derive(Debug)]
pub enum Command {
    /// `agent` is the composer's current choice: the new session starts on it (§6.4). Empty = hub picks.
    /// `workspace` overrides the startup workspace ("Open folder…"); `None` = the workspace the app started in.
    CreateSession {
        agent: AgentId,
        workspace: Option<PathBuf>,
    },
    CreateWorktreeSession {
        agent: AgentId,
        workspace: Option<PathBuf>,
    },
    OpenSession {
        session: SessionId,
    },
    OpenSessionAll {
        session: SessionId,
    },
    /// The agent is a parameter of every send (one session, many agents).
    Send {
        session: SessionId,
        agent: AgentId,
        text: String,
    },
    Cancel {
        session: SessionId,
    },
    /// Removes one queued (not yet running) message.
    CancelQueued {
        session: SessionId,
        id: String,
    },
    /// Stops a running background task (§9.17). Errors come back as `Event::Error`; the task list as
    /// `Event::BackgroundTasks`.
    StopBackgroundTask {
        session: SessionId,
        id: String,
    },
    /// Answers a permission request with the exact option the agent offered (`Event::PermissionChoices`).
    ChoosePermission {
        session: SessionId,
        request: u64,
        option_id: String,
    },
    /// `Send` with attachments and `@` / `#` mentions (ACP content blocks, phase-9-parity.md §9.2.1).
    SendContext {
        session: SessionId,
        agent: AgentId,
        text: String,
        context: Vec<api::ContextItem>,
    },
    /// Chat and workspace calls (hub API 0.5.0); each carries its own reply channel.
    Chat(ChatCall),
    /// Replaces the text of a queued message (phase-9-parity.md §9.2.5 R12). Errors (`Conflict` once it started)
    /// come back as `Event::Error`; the queue as `Event::Queue`.
    EditQueued {
        session: SessionId,
        id: String,
        text: String,
    },
    /// New queue order; `ids` must list every queued message once.
    ReorderQueued {
        session: SessionId,
        ids: Vec<String>,
    },
    /// Moves a queued message to the head of the queue (it runs next; the running turn is not stopped).
    RunQueuedNow {
        session: SessionId,
        id: String,
    },
    /// Sends while a turn runs: the message goes to the head of the queue (ACP v1 has no mid-turn input).
    Steer {
        session: SessionId,
        agent: AgentId,
        text: String,
        context: Vec<api::ContextItem>,
    },
    /// Mode / model / config option (effort) of a session's next prompts (R13); answered by `Event::SessionOptions`.
    SetMode {
        session: SessionId,
        mode: String,
    },
    SetModel {
        session: SessionId,
        model: String,
    },
    SetConfigOption {
        session: SessionId,
        id: String,
        value: String,
    },
    SetMachine {
        session: SessionId,
        on: bool,
    },
    /// Re-requests `Event::SessionOptions` of a session.
    SessionOptions {
        session: SessionId,
    },
    /// Retry after a failure (R20): `None` restarts the agent hub after `Event::Failure { session: None }`;
    /// `Some(session)` re-runs that session's last failed turn.
    RetryInit {
        session: Option<SessionId>,
    },
    SetStickyAgent {
        session: SessionId,
        agent: AgentId,
    },
    ClearAgentQuota(AgentId),
    SetFallbackOrder(Vec<AgentId>),
    /// Requests the modes / models / efforts an agent advertised (answered by `Event::AgentOptions`, R7).
    AgentOptions(AgentId),
    /// Requests the Agents overview (answered by `Event::AgentsOverview`, phase-9-parity.md §9.1.5a).
    AgentsOverview,
    /// Runs the connection test of one agent in a separate process (never the pooled one). Answered by
    /// `Event::AgentTested`, then a fresh `Event::AgentsOverview`.
    TestAgent(AgentId),
    /// Extra Claude accounts (phase-9-parity.md §9.6); every call is answered by `Event::Accounts`.
    Accounts(AccountsCall),
    /// Custom API providers (phase-9-parity.md §9.7); every call is answered by `Event::Providers`.
    Providers(ProvidersCall),
    /// Antigravity accounts; every call is answered by `Event::AgyAccounts`.
    AgyAccounts(api::AgyAccountOp),
    /// First-run setup (phase-9-parity.md §9.10): `Status` is answered by `Event::SetupStatus`; `Install` streams
    /// `Event::AdapterInstall` and ends with fresh agents, overview and setup status.
    Setup(SetupCall),
    /// Memory / projects / archive calls of the settings pages; each carries its own reply channel.
    Settings(SettingsCall),
    Shutdown,
}

/// Account calls of the Agents page (phase-9-parity.md §9.6).
#[derive(Debug, Clone)]
pub enum AccountsCall {
    List,
    /// Adds a Claude Code account with this label.
    Add(String),
    Remove(AgentId),
}

/// First-run setup calls (phase-9-parity.md §9.10).
#[derive(Debug, Clone)]
pub enum SetupCall {
    Status,
    /// Installs the ACP adapter of this agent with npm.
    Install(AgentId),
}

/// Provider calls of the Agents page (phase-9-parity.md §9.7). `Debug` never prints the key.
#[derive(Clone)]
pub enum ProvidersCall {
    List,
    Add(api::AddProvider),
    Remove(AgentId),
}

impl std::fmt::Debug for ProvidersCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::List => f.write_str("List"),
            // AddProvider 自己的 Debug 已隐藏密钥。
            Self::Add(request) => f.debug_tuple("Add").field(request).finish(),
            Self::Remove(id) => f.debug_tuple("Remove").field(id).finish(),
        }
    }
}

/// Reply channel of a [`SettingsCall`]. `ApiError::Conflict` (memory changed since the plan) and `ApiError::Busy`
/// (a session is running) are expected outcomes the page shows; the call changed nothing then.
pub type Reply<T> = tokio::sync::oneshot::Sender<Result<T, api::ApiError>>;

/// Calls of the Memory, Projects and Archive pages (hub API 0.4.0, phase-9-parity.md §9.1.5a). Every mutation is
/// followed by `Event::Sessions` when the session list changed.
#[derive(Debug)]
pub enum SettingsCall {
    MemoryDoc(api::MemoryScope, Reply<api::MemoryDoc>),
    /// Plan a full replacement; show `plan.diff`, then `MemoryApply(plan)` on confirm (`no_op` plans need no apply).
    MemoryPlan(api::MemoryScope, String, Reply<api::MemoryPlan>),
    MemoryApply(api::MemoryPlan, Reply<()>),
    ListProjects(api::ListProjects, Reply<Vec<api::ProjectInfo>>),
    RemoveProjects {
        keys: Vec<String>,
        delete_data: bool,
        reply: Reply<()>,
    },
    RestoreProjects(Vec<String>, Reply<()>),
    ArchiveSessions(Vec<SessionId>, Reply<()>),
    ListArchived(api::ListArchived, Reply<api::ArchivePage>),
    Unarchive(api::SessionSelection, Reply<u32>),
    DeleteArchived(api::SessionSelection, Reply<u32>),
}

/// Chat and workspace calls (hub API 0.5.0, phase-9-parity.md §9.2, `.work/phase9/API_READY_9.2.md`).
///
/// Turn actions take the item id of any item of that turn in the opened session (user bubble, assistant text, tool
/// card); the port maps it to the hub turn and answers `ApiError::NotFound` when the item is unknown.
/// `Rewind`, `EditResend`, `Retry` and `ApplyFiles` answer `ApiError::Busy` while the session runs, queues or waits
/// for a permission; nothing changed then. After a rewind the port re-opens the session (`Event::SessionOpened`).
#[allow(dead_code)] // constructed by the 9.2 UI (agent B)
#[derive(Debug)]
pub enum ChatCall {
    Rename {
        session: SessionId,
        title: String,
        reply: Reply<()>,
    },
    SetPinned {
        sessions: Vec<SessionId>,
        pinned: bool,
        reply: Reply<()>,
    },
    /// Send `false` when a session is shown and on its `TurnEnded`.
    SetUnread {
        session: SessionId,
        unread: bool,
        reply: Reply<()>,
    },
    ListGroups(Reply<Vec<api::SessionGroup>>),
    CreateGroup {
        name: String,
        color: api::GroupColor,
        reply: Reply<api::SessionGroup>,
    },
    UpdateGroup(api::UpdateGroup, Reply<api::SessionGroup>),
    DeleteGroup(String, Reply<()>),
    /// A permutation of all group ids.
    ReorderGroups(Vec<String>, Reply<()>),
    /// Moves sessions into a group (`None` = ungrouped), on top of that section.
    MoveSessions {
        sessions: Vec<SessionId>,
        group: Option<String>,
        reply: Reply<()>,
    },
    /// Full snapshot of a session for "Export as Markdown".
    Export {
        session: SessionId,
        reply: Reply<api::SessionDetail>,
    },
    /// Copies the finished turns up to `item`'s turn (`None` = all) into a new session.
    Fork {
        session: SessionId,
        item: Option<ItemId>,
        reply: Reply<SessionSummary>,
    },
    /// Removes `item`'s turn and every later turn; the result carries the text and context for the composer.
    Rewind {
        session: SessionId,
        item: ItemId,
        restore_files: bool,
        reply: Reply<api::RewindResult>,
    },
    /// Rewinds to `item`'s turn and sends `text` in its place (`context: None` keeps the original context).
    EditResend {
        session: SessionId,
        item: ItemId,
        text: String,
        context: Option<Vec<api::ContextItem>>,
        agent: Option<AgentId>,
        restore_files: bool,
        reply: Reply<()>,
    },
    /// Re-runs the last turn (`item` = any item of it).
    Retry {
        session: SessionId,
        item: ItemId,
        agent: Option<AgentId>,
        restore_files: bool,
        reply: Reply<()>,
    },
    PlanFiles {
        session: SessionId,
        item: ItemId,
        direction: api::ChangeDirection,
        reply: Reply<api::FileChangePlan>,
    },
    /// `paths: None` = every safe file of the plan; unsafe files only with `include_unsafe`.
    ApplyFiles {
        session: SessionId,
        item: ItemId,
        direction: api::ChangeDirection,
        paths: Option<Vec<String>>,
        include_unsafe: bool,
        reply: Reply<api::FileRestoreReport>,
    },
    /// Find in the opened session; hits carry the item to scroll to.
    Find {
        session: SessionId,
        query: String,
        case_sensitive: bool,
        reply: Reply<Vec<FindMatch>>,
    },
    /// Validates composer chips for the session's active agent without sending.
    ResolveContext {
        session: SessionId,
        items: Vec<api::ContextItem>,
        reply: Reply<Vec<api::ResolvedItem>>,
    },
    /// `@` file / folder and `#` session popup rows.
    Mentions(api::MentionQuery, Reply<Vec<api::MentionCandidate>>),
    /// Slash commands (agent-advertised merged with agent-config commands).
    Commands {
        session: SessionId,
        reply: Reply<api::CommandList>,
    },
    /// Command center (Mod+K) ranking over the UI's actions, sessions and files.
    Search(api::CommandSearch, Reply<api::CommandSearchResult>),
    /// Read-only import of `ZCode` / Claude Code history.
    ImportScan(api::ImportScan, Reply<Vec<api::ImportCandidate>>),
    Import(api::ImportRequest, Reply<api::ImportReport>),
    /// Scheduled prompts (off by default).
    SchedulesEnabled(Reply<bool>),
    SetSchedulesEnabled(bool, Reply<()>),
    ListSchedules(Reply<Vec<api::ScheduledPrompt>>),
    CreateSchedule(api::NewSchedule, Reply<api::ScheduledPrompt>),
    UpdateSchedule(api::UpdateSchedule, Reply<api::ScheduledPrompt>),
    DeleteSchedule(String, Reply<()>),
    RunScheduleNow(String, Reply<api::ScheduleRun>),
    /// Orchestra graph (phase-9-parity.md §9.11).
    GetOrchestra(Reply<api::Orchestra>),
    SetOrchestra(api::Orchestra, Reply<()>),
    Board(String, Reply<api::Board>),
    CreateCard(api::NewCard, Reply<api::CardView>),
    UpdateCard(api::UpdateCard, Reply<api::CardView>),
    MoveCard {
        card: String,
        lane: api::BoardLane,
        index: u32,
        reply: Reply<api::CardView>,
    },
    DeleteCard(String, Reply<()>),
    StartCard(String, Reply<api::CardView>),
    LinkBoard(String, Reply<api::Board>),
    UnlinkBoard(String, Reply<()>),
    SyncBoard(String, Reply<api::Board>),
    InboxStatus(Reply<api::InboxStatus>),
    SetInboxEnabled(bool, Reply<()>),
    SaveInboxSource(api::InboxSourceInput, Reply<api::InboxSource>),
    DeleteInboxSource(String, Reply<()>),
    PollInboxNow(String, Reply<()>),
    Tickets(Reply<Vec<api::Ticket>>),
    Triggers(u32, Reply<Vec<api::TriggerRecord>>),
    PostTicket(String, Reply<()>),
    StopTicket(String, Reply<()>),
    DiscardTicket(String, Reply<()>),
    /// The run the hub left unfinished (docs/specs/harness/resumable-runs.md §6).
    ResumableRun {
        session: SessionId,
        reply: Reply<Option<api::ResumableRun>>,
    },
    /// Starts the resume turn (a normal send whose message carries the finished steps).
    ResumeRun {
        session: SessionId,
        reply: Reply<()>,
    },
    DiscardRun {
        session: SessionId,
        reply: Reply<()>,
    },
}

/// One find-in-session hit mapped onto the opened transcript.
#[allow(dead_code)] // read by the 9.2 UI (agent B)
#[derive(Clone, Debug)]
pub struct FindMatch {
    /// Item to scroll to (`None` when the turn is not shown, e.g. still queued).
    pub item: Option<ItemId>,
    pub field: api::FindField,
    /// Char offset and length inside the field text.
    pub start: u32,
    pub len: u32,
    pub snippet: String,
}
