//! UI-agnostic API of the Harness agent hub (spec `docs/specs/harness/phase-8-core.md` §4).
//!
//! The UI (Slint, agent B) talks to the hub only through [`HarnessCore`] and [`EventStream`]. Every type is
//! serde-serializable so the same calls can later be carried over a remote transport. After publication the
//! API only grows additively; changes are logged in the spec (§4.1).

mod admin;
mod board;
pub mod chat;
pub mod diff;
mod error;
mod events;
mod inbox;
mod insights;
mod options;
mod orchestra;
pub mod time;
mod types;
pub mod workspace;

use std::future::Future;
use std::pin::Pin;

pub use admin::{
    AccountInfo, AdapterInstallResult, AddAccount, AddProvider, AgentOverview, AgentTestResult,
    AgyAccountInfo, AgyAccountOp, AgyAccounts, ArchivePage, ArchiveSort, ArchivedSession,
    AuthPresence, GhSetup, ListArchived, ListProjects, MemoryDoc, MemoryFileInfo, MemoryPlan,
    ProjectInfo, ProviderInfo, SessionSelection, SetupAgent, SetupState, SetupStatus,
    UpdateProvider,
};
pub use board::{
    Board, BoardLane, BoardSync, CardState, CardView, NewCard, UpdateCard, MAX_BOARD_CARDS,
    MAX_CARD_BODY, MAX_CARD_TITLE,
};
pub use chat::{
    ActionEntry, ApplyFileChanges, BlockKind, ChangeDirection, CommandList, CommandSearch,
    CommandSearchResult, CommandSource, ContextItem, ContextUsage, Cost, EditResend, Failure,
    FailureKind, FileAction, FileChangePlan, FileRestoreReport, FileSearchSource, FindField,
    FindHit, FindInSession, ForkOrigin, ForkSession, GroupColor, ImportCandidate, ImportReport,
    ImportRequest, ImportScan, ImportSource, MentionCandidate, MentionKind, MentionQuery,
    NewSchedule, PlanEntry, PlannedFile, QueuedTurn, ResolvedItem, Retry, RetryHint, Rewind,
    RewindResult, ScheduleKind, ScheduleRun, ScheduleTarget, ScheduledPrompt, ScoredId,
    SearchScope, SessionGroup, SessionHit, SlashCommand, SteerAccepted, SteerMode, TokenUsage,
    ToolContent, ToolLocation, TurnOrigin, UpdateGroup, UpdateSchedule,
};
pub use error::{ApiError, ApiResult};
pub use events::{
    AdapterInstallStatus, EventKind, EventStream, HubEvent, PauseReason, RecvError, SwitchReason,
    TurnOutcome,
};
pub use inbox::{
    GhStatus, InboxSource, InboxSourceInput, InboxStatus, Ticket, TicketStatus, TriggerRecord,
    DEFAULT_INBOX_HANDLE, DEFAULT_INBOX_INTERVAL, DEFAULT_INBOX_MAX_TURNS, MAX_INBOX_INTERVAL,
    MAX_INBOX_SOURCES, MIN_INBOX_INTERVAL,
};
pub use insights::{PullError, PullInfo, PullList, UsageCost, UsageRow, UsageStats};
pub use options::{
    AgentDefaults, AgentOptions, ConfigOption, ConfigValue, ModeInfo, ModelInfo,
    SessionConfigOptions, SessionModels, SessionModes,
};
pub use orchestra::{
    Orchestra, OrchestraEdge, OrchestraNode, OrchestraRole, MAX_ORCHESTRA_INSTRUCTIONS,
    MAX_ORCHESTRA_NODES,
};
pub use types::{
    AgentId, AgentInfo, AgentRuntimeState, Availability, BackgroundTask, CreateSession, ErrorClass,
    ListSessions, MemoryScope, MemoryWriteMode, PermissionDecision, PermissionOption,
    PermissionOptionKind, PermissionRequestId, PermissionRequestView, QuotaState, ResumableRun,
    ScopeView, SendAccepted, SendMessage, SessionCost, SessionDetail, SessionId, SessionSummary,
    SubagentStatus, SubagentView, ThoughtView, ToolCallStatus, ToolCallView, TurnAttempt, TurnDiff,
    TurnId, TurnSegment, TurnStatus, TurnView, WorktreeInfo, WorktreeRequest,
};

/// API version string; bumped (minor) on every additive change.
pub const API_VERSION: &str = "0.27.0";

/// Default body of the methods added in 0.4.0 and later.
fn unsupported<T: Send + 'static>(name: &'static str) -> BoxFuture<'static, ApiResult<T>> {
    Box::pin(async move { Err(ApiError::Internal(format!("{name} not supported"))) })
}

/// Boxed future returned by [`HarnessCore`] methods (keeps the trait dyn-compatible).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The hub as seen by a UI. Implemented by `harness_agent_hub::Hub`.
///
/// All methods are cheap to call from any task; they forward to the hub's owners (session actors,
/// quota registry, store) and resolve when the owner has accepted or answered the request.
pub trait HarnessCore: Send + Sync {
    /// Every configured agent with availability, quota and process state.
    fn list_agents(&self) -> BoxFuture<'_, ApiResult<Vec<AgentInfo>>>;

    /// Global failover order (default `claude-code`, `codex`, `agy`).
    fn get_fallback_order(&self) -> BoxFuture<'_, ApiResult<Vec<AgentId>>>;

    /// Replaces the failover order. Unknown agent ids are rejected with [`ApiError::InvalidArgument`].
    fn set_fallback_order(&self, order: Vec<AgentId>) -> BoxFuture<'_, ApiResult<()>>;

    /// Creates an empty session; `agent = None` picks the first available agent in the fallback order.
    fn create_session(&self, request: CreateSession) -> BoxFuture<'_, ApiResult<SessionSummary>>;

    /// Lists sessions, newest first.
    fn list_sessions(&self, request: ListSessions)
        -> BoxFuture<'_, ApiResult<Vec<SessionSummary>>>;

    /// Snapshot of one session (transcript, queue, pending permissions). Also the recovery path after
    /// [`RecvError::Lagged`].
    fn open_session(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<SessionDetail>>;

    /// Accepts a user turn. `agent = Some(..)` is a manual swap and becomes the sticky agent.
    fn send_message(&self, request: SendMessage) -> BoxFuture<'_, ApiResult<SendAccepted>>;

    /// Manual swap without sending. Clears the agent's exhaustion mark and resumes a paused turn.
    fn set_active_agent(
        &self,
        session_id: SessionId,
        agent: AgentId,
    ) -> BoxFuture<'_, ApiResult<()>>;

    /// Clears an agent's exhaustion mark and wakes sessions paused by quota exhaustion.
    fn clear_agent_quota(&self, agent: AgentId) -> BoxFuture<'_, ApiResult<()>> {
        let _ = agent;
        unsupported("clear_agent_quota")
    }

    /// Cancels the running turn of a session. Queued turns stay queued.
    fn stop(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<()>>;

    /// Removes one queued (not yet running) turn.
    fn cancel_queued(&self, session_id: SessionId, turn_id: TurnId)
        -> BoxFuture<'_, ApiResult<()>>;

    /// Replaces the text of a queued turn; `Conflict` once it started (API 0.5.1, phase-9-parity.md §9.2.5).
    fn edit_queued(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        text: String,
    ) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (session_id, turn_id, text);
        unsupported("edit_queued")
    }

    /// Reorders the queue; `turn_ids` must be a permutation of it, else `Conflict` (API 0.5.1).
    fn reorder_queued(
        &self,
        session_id: SessionId,
        turn_ids: Vec<TurnId>,
    ) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (session_id, turn_ids);
        unsupported("reorder_queued")
    }

    /// Moves a queued turn to the head of the queue (never cancels the running turn; API 0.5.1).
    fn run_queued_now(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (session_id, turn_id);
        unsupported("run_queued_now")
    }

    /// Steers the running turn when the agent can take mid-turn input, otherwise queues the message at the head
    /// of the queue (API 0.5.1).
    fn steer(&self, request: SendMessage) -> BoxFuture<'_, ApiResult<SteerAccepted>> {
        let _ = request;
        unsupported("steer")
    }

    fn stop_background_task(
        &self,
        session_id: SessionId,
        task_id: String,
    ) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (session_id, task_id);
        unsupported("stop_background_task")
    }

    /// The run the hub stopped in the middle of, if the user can resume it (API 0.23.0).
    fn resumable_run(
        &self,
        session_id: SessionId,
    ) -> BoxFuture<'_, ApiResult<Option<ResumableRun>>> {
        let _ = session_id;
        unsupported("resumable_run")
    }

    /// Continues an interrupted run with one new turn; the resume prompt lists the finished steps.
    fn resume_run(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<SendAccepted>> {
        let _ = session_id;
        unsupported("resume_run")
    }

    /// Dismisses the resume offer of a session without continuing it.
    fn discard_run(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<()>> {
        let _ = session_id;
        unsupported("discard_run")
    }

    /// Answers a pending permission request with one of its option ids.
    fn respond_permission(
        &self,
        session_id: SessionId,
        request_id: PermissionRequestId,
        option_id: String,
    ) -> BoxFuture<'_, ApiResult<()>>;

    /// Reads a MEMORY.md file (empty string when it does not exist).
    fn memory_read(&self, scope: MemoryScope) -> BoxFuture<'_, ApiResult<String>>;

    /// Writes a MEMORY.md file.
    fn memory_write(
        &self,
        scope: MemoryScope,
        content: String,
        mode: MemoryWriteMode,
    ) -> BoxFuture<'_, ApiResult<()>>;

    /// Workspace changes made during one turn (spec §15.2). Default: unsupported.
    fn turn_diff(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> BoxFuture<'_, ApiResult<TurnDiff>> {
        let _ = (session_id, turn_id);
        Box::pin(async { Err(ApiError::Internal("turn_diff not supported".into())) })
    }

    /// Option lists of an agent as last advertised (cached across restarts; `known = false` until the agent's
    /// first session). Default: unsupported.
    fn agent_options(&self, agent: AgentId) -> BoxFuture<'_, ApiResult<AgentOptions>> {
        let _ = agent;
        Box::pin(async { Err(ApiError::Internal("agent_options not supported".into())) })
    }

    /// Modes of the session's active agent (spec phase-9-parity.md §9.1.5). Default: unsupported.
    fn list_modes(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<SessionModes>> {
        let _ = session_id;
        Box::pin(async { Err(ApiError::Internal("list_modes not supported".into())) })
    }

    /// Models of the session's active agent. Default: unsupported.
    fn list_models(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<SessionModels>> {
        let _ = session_id;
        Box::pin(async { Err(ApiError::Internal("list_models not supported".into())) })
    }

    /// Config options (e.g. reasoning effort) of the session's active agent. Default: unsupported.
    fn list_config_options(
        &self,
        session_id: SessionId,
    ) -> BoxFuture<'_, ApiResult<SessionConfigOptions>> {
        let _ = session_id;
        Box::pin(async {
            Err(ApiError::Internal(
                "list_config_options not supported".into(),
            ))
        })
    }

    /// Chooses the mode of the session's active agent. Stored per (session, agent); applied before the next
    /// prompt, never to a running turn. Unknown ids (when the agent advertised a list) → `InvalidArgument`.
    fn set_mode(&self, session_id: SessionId, mode_id: String) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (session_id, mode_id);
        Box::pin(async { Err(ApiError::Internal("set_mode not supported".into())) })
    }

    /// Chooses the model of the session's active agent (same rules as [`HarnessCore::set_mode`]).
    fn set_model(&self, session_id: SessionId, model_id: String) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (session_id, model_id);
        Box::pin(async { Err(ApiError::Internal("set_model not supported".into())) })
    }

    /// Sets one config option of the session's active agent (same rules as [`HarnessCore::set_mode`]).
    fn set_config_option(
        &self,
        session_id: SessionId,
        config_id: String,
        value: String,
    ) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (session_id, config_id, value);
        Box::pin(async { Err(ApiError::Internal("set_config_option not supported".into())) })
    }

    fn set_machine(&self, session_id: SessionId, on: bool) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (session_id, on);
        Box::pin(async { Err(ApiError::Internal("set_machine not supported".into())) })
    }

    fn machine(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<bool>> {
        let _ = session_id;
        Box::pin(async { Ok(false) })
    }

    /// Agent cards: install state, ACP adapter version, login presence (phase-9-parity.md §9.1.5a).
    fn agents_overview(&self) -> BoxFuture<'_, ApiResult<Vec<AgentOverview>>> {
        unsupported("agents_overview")
    }

    /// Spawns the agent separately, runs ACP `initialize`, stops it.
    fn test_agent(&self, agent: AgentId) -> BoxFuture<'_, ApiResult<AgentTestResult>> {
        let _ = agent;
        unsupported("test_agent")
    }

    /// Extra login profiles (API 0.6.0, phase-9-parity.md §9.6).
    fn list_accounts(&self) -> BoxFuture<'_, ApiResult<Vec<AccountInfo>>> {
        unsupported("list_accounts")
    }

    /// Adds a login profile; it becomes an agent on the next hub start.
    fn add_account(&self, request: AddAccount) -> BoxFuture<'_, ApiResult<AccountInfo>> {
        let _ = request;
        unsupported("add_account")
    }

    /// Removes a login profile entry (the profile directory is kept).
    fn remove_account(&self, id: AgentId) -> BoxFuture<'_, ApiResult<()>> {
        let _ = id;
        unsupported("remove_account")
    }

    /// Antigravity (agy) login slots (API 0.18.0); every op answers with the fresh list.
    fn agy_accounts(&self, op: AgyAccountOp) -> BoxFuture<'_, ApiResult<AgyAccounts>> {
        let _ = op;
        unsupported("agy_accounts")
    }

    /// Custom OpenAI-compatible providers (API 0.7.0, phase-9-parity.md §9.7). Never returns keys.
    fn list_providers(&self) -> BoxFuture<'_, ApiResult<Vec<ProviderInfo>>> {
        unsupported("list_providers")
    }

    /// Adds a provider; it becomes an agent on the next hub start.
    fn add_provider(&self, request: AddProvider) -> BoxFuture<'_, ApiResult<ProviderInfo>> {
        let _ = request;
        unsupported("add_provider")
    }

    /// Changes a provider (`None` fields are kept); applies on the next hub start.
    fn update_provider(&self, request: UpdateProvider) -> BoxFuture<'_, ApiResult<ProviderInfo>> {
        let _ = request;
        unsupported("update_provider")
    }

    /// Removes a provider and its session files.
    fn remove_provider(&self, id: AgentId) -> BoxFuture<'_, ApiResult<()>> {
        let _ = id;
        unsupported("remove_provider")
    }

    /// Global MEMORY.md plus the project MEMORY.md of every listed project.
    fn memory_files(&self) -> BoxFuture<'_, ApiResult<Vec<MemoryFileInfo>>> {
        unsupported("memory_files")
    }

    /// One MEMORY.md with its content.
    fn memory_doc(&self, scope: MemoryScope) -> BoxFuture<'_, ApiResult<MemoryDoc>> {
        let _ = scope;
        unsupported("memory_doc")
    }

    /// Plans replacing a MEMORY.md (diff for the confirm step).
    fn memory_plan(
        &self,
        scope: MemoryScope,
        content: String,
    ) -> BoxFuture<'_, ApiResult<MemoryPlan>> {
        let _ = (scope, content);
        unsupported("memory_plan")
    }

    /// Applies a plan; [`ApiError::Conflict`] when the file changed since the plan.
    fn memory_apply(&self, plan: MemoryPlan) -> BoxFuture<'_, ApiResult<()>> {
        let _ = plan;
        unsupported("memory_apply")
    }

    /// Projects (workspaces) with session counts.
    fn list_projects(&self, request: ListProjects) -> BoxFuture<'_, ApiResult<Vec<ProjectInfo>>> {
        let _ = request;
        unsupported("list_projects")
    }

    /// Removes projects from the lists; `delete_data` also deletes their sessions ([`ApiError::Busy`] if one
    /// is busy, then nothing changes).
    fn remove_projects(
        &self,
        workspace_keys: Vec<String>,
        delete_data: bool,
    ) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (workspace_keys, delete_data);
        unsupported("remove_projects")
    }

    /// Undoes [`HarnessCore::remove_projects`] (sessions deleted with data stay deleted).
    fn restore_projects(&self, workspace_keys: Vec<String>) -> BoxFuture<'_, ApiResult<()>> {
        let _ = workspace_keys;
        unsupported("restore_projects")
    }

    /// Archives idle sessions ([`ApiError::Busy`] if one is busy, then nothing changes).
    fn archive_sessions(&self, session_ids: Vec<SessionId>) -> BoxFuture<'_, ApiResult<()>> {
        let _ = session_ids;
        unsupported("archive_sessions")
    }

    /// One page of archived sessions.
    fn list_archived(&self, request: ListArchived) -> BoxFuture<'_, ApiResult<ArchivePage>> {
        let _ = request;
        unsupported("list_archived")
    }

    /// Unarchives; returns how many sessions changed.
    fn unarchive(&self, selection: SessionSelection) -> BoxFuture<'_, ApiResult<u32>> {
        let _ = selection;
        unsupported("unarchive")
    }

    /// Deletes archived sessions (others in the selection are skipped); returns how many were deleted.
    fn delete_archived(&self, selection: SessionSelection) -> BoxFuture<'_, ApiResult<u32>> {
        let _ = selection;
        unsupported("delete_archived")
    }

    // ---- API 0.5.0 (phase-9-parity.md §9.2.1) ----------------------------------------------------

    /// Renames a session (trimmed, 1–200 chars).
    fn rename_session(&self, session_id: SessionId, title: String) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (session_id, title);
        unsupported("rename_session")
    }

    /// Pins or unpins sessions.
    fn set_pinned(
        &self,
        session_ids: Vec<SessionId>,
        pinned: bool,
    ) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (session_ids, pinned);
        unsupported("set_pinned")
    }

    /// Marks a session unread (`true`) or read (`false`).
    fn set_unread(&self, session_id: SessionId, unread: bool) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (session_id, unread);
        unsupported("set_unread")
    }

    /// Sidebar groups by position.
    fn list_groups(&self) -> BoxFuture<'_, ApiResult<Vec<SessionGroup>>> {
        unsupported("list_groups")
    }

    /// Creates a group (appended last).
    fn create_group(
        &self,
        name: String,
        color: GroupColor,
    ) -> BoxFuture<'_, ApiResult<SessionGroup>> {
        let _ = (name, color);
        unsupported("create_group")
    }

    /// Renames or recolours a group.
    fn update_group(&self, request: UpdateGroup) -> BoxFuture<'_, ApiResult<SessionGroup>> {
        let _ = request;
        unsupported("update_group")
    }

    /// Deletes a group; its sessions become ungrouped.
    fn delete_group(&self, group_id: String) -> BoxFuture<'_, ApiResult<()>> {
        let _ = group_id;
        unsupported("delete_group")
    }

    /// Reorders groups (a permutation of all ids).
    fn reorder_groups(&self, group_ids: Vec<String>) -> BoxFuture<'_, ApiResult<()>> {
        let _ = group_ids;
        unsupported("reorder_groups")
    }

    /// Moves sessions into a group (`None` = ungrouped) on top of that section.
    fn move_sessions(
        &self,
        session_ids: Vec<SessionId>,
        group_id: Option<String>,
    ) -> BoxFuture<'_, ApiResult<()>> {
        let _ = (session_ids, group_id);
        unsupported("move_sessions")
    }

    /// Copies finished turns into a new session.
    fn fork_session(&self, request: ForkSession) -> BoxFuture<'_, ApiResult<SessionSummary>> {
        let _ = request;
        unsupported("fork_session")
    }

    /// Removes a turn and every later turn ([`ApiError::Busy`] while busy).
    fn rewind_session(&self, request: Rewind) -> BoxFuture<'_, ApiResult<RewindResult>> {
        let _ = request;
        unsupported("rewind_session")
    }

    /// Rewinds to a turn and sends the edited text in its place.
    fn edit_and_resend(&self, request: EditResend) -> BoxFuture<'_, ApiResult<SendAccepted>> {
        let _ = request;
        unsupported("edit_and_resend")
    }

    /// Re-runs the last turn.
    fn retry_turn(&self, request: Retry) -> BoxFuture<'_, ApiResult<SendAccepted>> {
        let _ = request;
        unsupported("retry_turn")
    }

    /// Plans undoing / reapplying a turn's file changes (safe/unsafe per file).
    fn plan_file_changes(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        direction: ChangeDirection,
    ) -> BoxFuture<'_, ApiResult<FileChangePlan>> {
        let _ = (session_id, turn_id, direction);
        unsupported("plan_file_changes")
    }

    /// Restores files of a turn (unsafe ones only with `include_unsafe`).
    fn apply_file_changes(
        &self,
        request: ApplyFileChanges,
    ) -> BoxFuture<'_, ApiResult<FileRestoreReport>> {
        let _ = request;
        unsupported("apply_file_changes")
    }

    /// Text matches in a session's transcript.
    fn find_in_session(&self, request: FindInSession) -> BoxFuture<'_, ApiResult<Vec<FindHit>>> {
        let _ = request;
        unsupported("find_in_session")
    }

    /// Validates context items for the session's active agent without sending.
    fn resolve_context(
        &self,
        session_id: SessionId,
        items: Vec<ContextItem>,
    ) -> BoxFuture<'_, ApiResult<Vec<ResolvedItem>>> {
        let _ = (session_id, items);
        unsupported("resolve_context")
    }

    /// `@` / `#` popup rows.
    fn mention_candidates(
        &self,
        request: MentionQuery,
    ) -> BoxFuture<'_, ApiResult<Vec<MentionCandidate>>> {
        let _ = request;
        unsupported("mention_candidates")
    }

    /// Slash commands for the session's active agent.
    fn list_commands(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<CommandList>> {
        let _ = session_id;
        unsupported("list_commands")
    }

    /// Command center ranking over actions, sessions and files.
    fn command_search(
        &self,
        request: CommandSearch,
    ) -> BoxFuture<'_, ApiResult<CommandSearchResult>> {
        let _ = request;
        unsupported("command_search")
    }

    /// Lists importable sessions (read-only).
    fn import_scan(&self, request: ImportScan) -> BoxFuture<'_, ApiResult<Vec<ImportCandidate>>> {
        let _ = request;
        unsupported("import_scan")
    }

    /// Imports sessions as finished turns (dedupe by source id).
    fn import_sessions(&self, request: ImportRequest) -> BoxFuture<'_, ApiResult<ImportReport>> {
        let _ = request;
        unsupported("import_sessions")
    }

    /// Global switch of the scheduler (default off).
    fn schedules_enabled(&self) -> BoxFuture<'_, ApiResult<bool>> {
        unsupported("schedules_enabled")
    }

    /// Turns the scheduler on or off.
    fn set_schedules_enabled(&self, enabled: bool) -> BoxFuture<'_, ApiResult<()>> {
        let _ = enabled;
        unsupported("set_schedules_enabled")
    }

    /// Scheduled prompts.
    fn list_schedules(&self) -> BoxFuture<'_, ApiResult<Vec<ScheduledPrompt>>> {
        unsupported("list_schedules")
    }

    /// Creates a scheduled prompt.
    fn create_schedule(&self, request: NewSchedule) -> BoxFuture<'_, ApiResult<ScheduledPrompt>> {
        let _ = request;
        unsupported("create_schedule")
    }

    /// Changes a scheduled prompt.
    fn update_schedule(
        &self,
        request: UpdateSchedule,
    ) -> BoxFuture<'_, ApiResult<ScheduledPrompt>> {
        let _ = request;
        unsupported("update_schedule")
    }

    /// Deletes a scheduled prompt.
    fn delete_schedule(&self, schedule_id: String) -> BoxFuture<'_, ApiResult<()>> {
        let _ = schedule_id;
        unsupported("delete_schedule")
    }

    /// Runs a scheduled prompt now (also when the scheduler is off).
    fn run_schedule_now(&self, schedule_id: String) -> BoxFuture<'_, ApiResult<ScheduleRun>> {
        let _ = schedule_id;
        unsupported("run_schedule_now")
    }

    /// First-run setup rows: which CLIs and ACP adapters are installed, and whether npm is available
    /// (API 0.9.0, phase-9-parity.md §9.10). Never runs a program.
    fn setup_status(&self) -> BoxFuture<'_, ApiResult<SetupStatus>> {
        unsupported("setup_status")
    }

    /// Installs the ACP adapter of `agent` with `npm install -g` (progress as [`EventKind::AdapterInstall`]).
    /// `InvalidArgument` when the agent has no known adapter or its CLI is missing.
    fn install_adapter(&self, agent: AgentId) -> BoxFuture<'_, ApiResult<AdapterInstallResult>> {
        let _ = agent;
        unsupported("install_adapter")
    }

    fn install_gh(&self) -> BoxFuture<'_, ApiResult<()>> {
        unsupported("install_gh")
    }

    /// The Orchestra graph (API 0.10.0, phase-9-parity.md §9.11); empty when unset.
    fn get_orchestra(&self) -> BoxFuture<'_, ApiResult<Orchestra>> {
        unsupported("get_orchestra")
    }

    /// Replaces the Orchestra graph. `InvalidArgument` when a §9.11 rule is broken.
    fn set_orchestra(&self, orchestra: Orchestra) -> BoxFuture<'_, ApiResult<()>> {
        let _ = orchestra;
        unsupported("set_orchestra")
    }

    fn board(&self, workspace_key: String) -> BoxFuture<'_, ApiResult<Board>> {
        let _ = workspace_key;
        unsupported("board")
    }

    fn create_card(&self, request: NewCard) -> BoxFuture<'_, ApiResult<CardView>> {
        let _ = request;
        unsupported("create_card")
    }

    fn update_card(&self, request: UpdateCard) -> BoxFuture<'_, ApiResult<CardView>> {
        let _ = request;
        unsupported("update_card")
    }

    fn move_card(
        &self,
        card_id: String,
        lane: BoardLane,
        index: u32,
    ) -> BoxFuture<'_, ApiResult<CardView>> {
        let _ = (card_id, lane, index);
        unsupported("move_card")
    }

    fn delete_card(&self, card_id: String) -> BoxFuture<'_, ApiResult<()>> {
        let _ = card_id;
        unsupported("delete_card")
    }

    fn start_card(&self, card_id: String) -> BoxFuture<'_, ApiResult<CardView>> {
        let _ = card_id;
        unsupported("start_card")
    }

    fn link_board(&self, workspace_key: String) -> BoxFuture<'_, ApiResult<Board>> {
        let _ = workspace_key;
        unsupported("link_board")
    }

    fn unlink_board(&self, workspace_key: String) -> BoxFuture<'_, ApiResult<()>> {
        let _ = workspace_key;
        unsupported("unlink_board")
    }

    fn sync_board(&self, workspace_key: String) -> BoxFuture<'_, ApiResult<Board>> {
        let _ = workspace_key;
        unsupported("sync_board")
    }

    /// Token use per agent and UTC day over the last `days` days (1..=366).
    fn usage_stats(&self, days: u32) -> BoxFuture<'_, ApiResult<UsageStats>> {
        let _ = days;
        unsupported("usage_stats")
    }

    /// Open pull requests of the known projects.
    fn list_pulls(&self) -> BoxFuture<'_, ApiResult<PullList>> {
        unsupported("list_pulls")
    }

    fn inbox_status(&self) -> BoxFuture<'_, ApiResult<InboxStatus>> {
        unsupported("inbox_status")
    }

    fn set_inbox_enabled(&self, enabled: bool) -> BoxFuture<'_, ApiResult<()>> {
        let _ = enabled;
        unsupported("set_inbox_enabled")
    }

    fn save_inbox_source(&self, source: InboxSourceInput) -> BoxFuture<'_, ApiResult<InboxSource>> {
        let _ = source;
        unsupported("save_inbox_source")
    }

    fn delete_inbox_source(&self, source_id: String) -> BoxFuture<'_, ApiResult<()>> {
        let _ = source_id;
        unsupported("delete_inbox_source")
    }

    fn poll_inbox_now(&self, source_id: String) -> BoxFuture<'_, ApiResult<()>> {
        let _ = source_id;
        unsupported("poll_inbox_now")
    }

    fn list_tickets(&self) -> BoxFuture<'_, ApiResult<Vec<Ticket>>> {
        unsupported("list_tickets")
    }

    fn list_triggers(&self, limit: u32) -> BoxFuture<'_, ApiResult<Vec<TriggerRecord>>> {
        let _ = limit;
        unsupported("list_triggers")
    }

    fn post_ticket(&self, ticket_id: String) -> BoxFuture<'_, ApiResult<()>> {
        let _ = ticket_id;
        unsupported("post_ticket")
    }

    fn stop_ticket(&self, ticket_id: String) -> BoxFuture<'_, ApiResult<()>> {
        let _ = ticket_id;
        unsupported("stop_ticket")
    }

    fn discard_ticket(&self, ticket_id: String) -> BoxFuture<'_, ApiResult<()>> {
        let _ = ticket_id;
        unsupported("discard_ticket")
    }

    /// Subscribes to hub events from now on.
    fn subscribe(&self) -> EventStream;

    /// Cancels running turns, stops every agent process and closes the store.
    fn shutdown(&self) -> BoxFuture<'_, ApiResult<()>>;
}
