use super::{
    options, store, AccountInfo, AddAccount, AddProvider, AgentId, AgentInfo, AgentOptions,
    AgentOverview, AgentTestResult, ApiError, ApiResult, ApplyFileChanges, ArchivePage, BoxFuture,
    ChangeDirection, CommandList, CommandSearch, CommandSearchResult, ContextItem, CreateSession,
    EditResend, EventKind, EventStream, FileChangePlan, FileRestoreReport, FindHit, FindInSession,
    ForkSession, GroupColor, HarnessCore, Hub, ImportCandidate, ImportReport, ImportRequest,
    ImportScan, ListArchived, ListProjects, ListSessions, MemoryDoc, MemoryFileInfo, MemoryPlan,
    MemoryScope, MemoryWriteMode, MentionCandidate, MentionQuery, NewSchedule, PermissionRequestId,
    PrefChange, Prefs, ProjectInfo, ProviderInfo, ResolvedItem, ResumableRun, Retry, Rewind,
    RewindResult, ScheduleRun, ScheduledPrompt, SendAccepted, SendMessage, SessionCmd,
    SessionConfigOptions, SessionDetail, SessionGroup, SessionId, SessionModels, SessionModes,
    SessionSelection, SessionSummary, TurnDiff, TurnId, UpdateGroup, UpdateProvider,
    UpdateSchedule,
};

impl HarnessCore for Hub {
    fn list_agents(&self) -> BoxFuture<'_, ApiResult<Vec<AgentInfo>>> {
        Box::pin(async move { Ok(self.do_list_agents().await) })
    }

    fn get_fallback_order(&self) -> BoxFuture<'_, ApiResult<Vec<AgentId>>> {
        Box::pin(async move { Ok(self.ctx.fallback_order()) })
    }

    fn set_fallback_order(&self, order: Vec<AgentId>) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_set_fallback(order))
    }

    fn create_session(&self, request: CreateSession) -> BoxFuture<'_, ApiResult<SessionSummary>> {
        Box::pin(self.do_create_session(request))
    }

    fn list_sessions(
        &self,
        request: ListSessions,
    ) -> BoxFuture<'_, ApiResult<Vec<SessionSummary>>> {
        Box::pin(async move {
            self.check_open()?;
            let limit = request.limit.unwrap_or(200).min(10_000);
            Ok(self
                .ctx
                .store
                .run(move |c| {
                    store::list_sessions(
                        c,
                        request.workspace_key.as_deref(),
                        request.query.as_deref(),
                        limit,
                    )
                })
                .await?)
        })
    }

    fn open_session(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<SessionDetail>> {
        Box::pin(async move { self.ask(&session_id, SessionCmd::Snapshot).await })
    }

    fn send_message(&self, request: SendMessage) -> BoxFuture<'_, ApiResult<SendAccepted>> {
        Box::pin(async move {
            let id = request.session_id.clone();
            self.check_open()?;
            self.unarchive_for_send(&id).await?;
            self.ask(&id, |reply| SessionCmd::Send(request, reply))
                .await
        })
    }

    fn set_active_agent(
        &self,
        session_id: SessionId,
        agent: AgentId,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.ask(&session_id, |reply| SessionCmd::SetAgent(agent, reply))
                .await
        })
    }

    fn stop(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            // §9.8: stopping a parent leaves its subagents running; their results reach the parent later
            // (delegate_server::push). A subagent is stopped from its own session.
            self.ask(&session_id, SessionCmd::Stop).await
        })
    }
    fn resumable_run(
        &self,
        session_id: SessionId,
    ) -> BoxFuture<'_, ApiResult<Option<ResumableRun>>> {
        Box::pin(self.do_resumable_run(session_id))
    }

    fn resume_run(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<SendAccepted>> {
        Box::pin(self.do_resume_run(session_id))
    }

    fn discard_run(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_discard_run(session_id))
    }

    fn cancel_queued(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.ask(&session_id, |reply| {
                SessionCmd::CancelQueued(turn_id, reply)
            })
            .await
        })
    }

    fn edit_queued(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        text: String,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.ask(&session_id, |reply| {
                SessionCmd::EditQueued(turn_id, text, reply)
            })
            .await
        })
    }

    fn reorder_queued(
        &self,
        session_id: SessionId,
        turn_ids: Vec<TurnId>,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.ask(&session_id, |reply| {
                SessionCmd::ReorderQueued(turn_ids, reply)
            })
            .await
        })
    }

    fn run_queued_now(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.ask(&session_id, |reply| {
                SessionCmd::RunQueuedNow(turn_id, reply)
            })
            .await
        })
    }

    fn steer(
        &self,
        request: SendMessage,
    ) -> BoxFuture<'_, ApiResult<harness_core_api::SteerAccepted>> {
        Box::pin(async move {
            let session = request.session_id.clone();
            self.ask(&session, |reply| SessionCmd::Steer(request, reply))
                .await
        })
    }

    fn stop_background_task(
        &self,
        session_id: SessionId,
        task_id: String,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.ask(&session_id, |reply| {
                SessionCmd::StopBackgroundTask(task_id, reply)
            })
            .await
        })
    }

    fn respond_permission(
        &self,
        session_id: SessionId,
        request_id: PermissionRequestId,
        option_id: String,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.ask(&session_id, |reply| {
                SessionCmd::RespondPermission(request_id, option_id, reply)
            })
            .await
        })
    }

    fn memory_read(&self, scope: MemoryScope) -> BoxFuture<'_, ApiResult<String>> {
        Box::pin(async move {
            let memory = self.ctx.memory.clone();
            let target = Self::memory_target(&scope);
            tokio::task::spawn_blocking(move || memory.read(&target))
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?
                .map_err(|e| ApiError::Storage(e.to_string()))
        })
    }

    fn memory_write(
        &self,
        scope: MemoryScope,
        content: String,
        mode: MemoryWriteMode,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            let memory = self.ctx.memory.clone();
            let target = Self::memory_target(&scope);
            let append = mode == MemoryWriteMode::Append;
            tokio::task::spawn_blocking(move || memory.write(&target, &content, append))
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?
                .map_err(|e| ApiError::Storage(e.to_string()))?;
            self.ctx
                .events
                .emit(None, EventKind::MemoryChanged { scope });
            Ok(())
        })
    }

    fn turn_diff(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> BoxFuture<'_, ApiResult<TurnDiff>> {
        Box::pin(self.do_turn_diff(session_id, turn_id))
    }

    fn agent_options(&self, agent: AgentId) -> BoxFuture<'_, ApiResult<AgentOptions>> {
        Box::pin(async move {
            self.check_open()?;
            if !self.ctx.specs.contains_key(&agent) {
                return Err(ApiError::NotFound(format!("agent {agent}")));
            }
            self.ctx.probe_advertised(&agent).await;
            let advertised = self.ctx.advertised(&agent).await;
            let defaults = self.ctx.config.agent_defaults.get(&agent);
            let none = Prefs::default();
            Ok(AgentOptions {
                modes: options::modes_view(agent.clone(), &advertised, &none, defaults),
                models: options::models_view(agent.clone(), &advertised, &none, defaults),
                config: options::config_view(agent, &advertised, &none, defaults),
            })
        })
    }

    fn list_modes(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<SessionModes>> {
        Box::pin(async move {
            let (agent, advertised, prefs) = self.session_options(&session_id).await?;
            let defaults = self.ctx.config.agent_defaults.get(&agent);
            Ok(options::modes_view(
                agent.clone(),
                &advertised,
                &prefs,
                defaults,
            ))
        })
    }

    fn list_models(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<SessionModels>> {
        Box::pin(async move {
            let (agent, advertised, prefs) = self.session_options(&session_id).await?;
            let defaults = self.ctx.config.agent_defaults.get(&agent);
            Ok(options::models_view(
                agent.clone(),
                &advertised,
                &prefs,
                defaults,
            ))
        })
    }

    fn list_config_options(
        &self,
        session_id: SessionId,
    ) -> BoxFuture<'_, ApiResult<SessionConfigOptions>> {
        Box::pin(async move {
            let (agent, advertised, prefs) = self.session_options(&session_id).await?;
            let defaults = self.ctx.config.agent_defaults.get(&agent);
            Ok(options::config_view(
                agent.clone(),
                &advertised,
                &prefs,
                defaults,
            ))
        })
    }

    fn set_mode(&self, session_id: SessionId, mode_id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.ask(&session_id, |reply| {
                SessionCmd::SetPref(PrefChange::Mode(mode_id), reply)
            })
            .await
        })
    }

    fn set_model(&self, session_id: SessionId, model_id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.ask(&session_id, |reply| {
                SessionCmd::SetPref(PrefChange::Model(model_id), reply)
            })
            .await
        })
    }

    fn set_config_option(
        &self,
        session_id: SessionId,
        config_id: String,
        value: String,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.ask(&session_id, |reply| {
                SessionCmd::SetPref(PrefChange::Config(config_id, value), reply)
            })
            .await
        })
    }

    fn set_machine(&self, session_id: SessionId, on: bool) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.ctx
                .machine
                .set(session_id.as_str(), on)
                .await
                .map_err(|error| ApiError::Internal(format!("machine mode: {error}")))
        })
    }

    fn machine(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<bool>> {
        Box::pin(async move { Ok(self.ctx.machine.is_on(session_id.as_str())) })
    }

    fn agents_overview(&self) -> BoxFuture<'_, ApiResult<Vec<AgentOverview>>> {
        Box::pin(self.do_agents_overview())
    }

    fn setup_status(&self) -> BoxFuture<'_, ApiResult<harness_core_api::SetupStatus>> {
        Box::pin(async move {
            self.check_open()?;
            Ok(self.do_setup_status())
        })
    }

    fn install_gh(&self) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_install_gh())
    }

    fn install_adapter(
        &self,
        agent: AgentId,
    ) -> BoxFuture<'_, ApiResult<harness_core_api::AdapterInstallResult>> {
        Box::pin(async move {
            self.check_open()?;
            self.do_install_adapter(agent).await
        })
    }

    fn test_agent(&self, agent: AgentId) -> BoxFuture<'_, ApiResult<AgentTestResult>> {
        Box::pin(self.do_test_agent(agent))
    }

    fn list_accounts(&self) -> BoxFuture<'_, ApiResult<Vec<AccountInfo>>> {
        Box::pin(async move {
            self.check_open()?;
            let loaded = |id: &AgentId| self.ctx.specs.contains_key(id);
            crate::accounts::list(&self.ctx.config, &loaded).await
        })
    }

    fn add_account(&self, request: AddAccount) -> BoxFuture<'_, ApiResult<AccountInfo>> {
        Box::pin(async move {
            self.check_open()?;
            let loaded = |id: &AgentId| self.ctx.specs.contains_key(id);
            crate::accounts::add(&self.ctx.config, request, &loaded).await
        })
    }

    fn remove_account(&self, id: AgentId) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.check_open()?;
            crate::accounts::remove(&self.ctx.config, &id).await
        })
    }

    fn agy_accounts(
        &self,
        op: harness_core_api::AgyAccountOp,
    ) -> BoxFuture<'_, ApiResult<harness_core_api::AgyAccounts>> {
        Box::pin(async move {
            self.check_open()?;
            crate::agy_ops::run(&self.ctx, op).await
        })
    }

    fn list_providers(&self) -> BoxFuture<'_, ApiResult<Vec<ProviderInfo>>> {
        Box::pin(async move {
            self.check_open()?;
            let loaded = |id: &AgentId| self.ctx.specs.contains_key(id);
            crate::providers::list(&self.ctx.config, &loaded).await
        })
    }

    fn add_provider(&self, request: AddProvider) -> BoxFuture<'_, ApiResult<ProviderInfo>> {
        Box::pin(async move {
            self.check_open()?;
            let loaded = |id: &AgentId| self.ctx.specs.contains_key(id);
            crate::providers::add(&self.ctx.config, request, &loaded).await
        })
    }

    fn update_provider(&self, request: UpdateProvider) -> BoxFuture<'_, ApiResult<ProviderInfo>> {
        Box::pin(async move {
            self.check_open()?;
            let loaded = |id: &AgentId| self.ctx.specs.contains_key(id);
            crate::providers::update(&self.ctx.config, request, &loaded).await
        })
    }

    fn remove_provider(&self, id: AgentId) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.check_open()?;
            crate::providers::remove(&self.ctx.config, &id).await
        })
    }

    fn memory_files(&self) -> BoxFuture<'_, ApiResult<Vec<MemoryFileInfo>>> {
        Box::pin(self.do_memory_files())
    }

    fn memory_doc(&self, scope: MemoryScope) -> BoxFuture<'_, ApiResult<MemoryDoc>> {
        Box::pin(self.do_memory_doc(scope))
    }

    fn memory_plan(
        &self,
        scope: MemoryScope,
        content: String,
    ) -> BoxFuture<'_, ApiResult<MemoryPlan>> {
        Box::pin(self.do_memory_plan(scope, content))
    }

    fn memory_apply(&self, plan: MemoryPlan) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_memory_apply(plan))
    }

    fn list_projects(&self, request: ListProjects) -> BoxFuture<'_, ApiResult<Vec<ProjectInfo>>> {
        Box::pin(self.do_list_projects(request))
    }

    fn remove_projects(
        &self,
        keys: Vec<String>,
        delete_data: bool,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_remove_projects(keys, delete_data))
    }

    fn restore_projects(&self, keys: Vec<String>) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_restore_projects(keys))
    }

    fn archive_sessions(&self, ids: Vec<SessionId>) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_archive_sessions(ids))
    }

    fn list_archived(&self, request: ListArchived) -> BoxFuture<'_, ApiResult<ArchivePage>> {
        Box::pin(self.do_list_archived(request))
    }

    fn unarchive(&self, selection: SessionSelection) -> BoxFuture<'_, ApiResult<u32>> {
        Box::pin(self.do_unarchive(selection))
    }

    fn delete_archived(&self, selection: SessionSelection) -> BoxFuture<'_, ApiResult<u32>> {
        Box::pin(self.do_delete_archived(selection))
    }

    // ---- API 0.5.0 (phase-9-parity.md §9.2.1; bodies in hub_chat.rs) ------------------------------------

    fn rename_session(&self, session_id: SessionId, title: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_rename(session_id, title))
    }

    fn set_pinned(
        &self,
        session_ids: Vec<SessionId>,
        pinned: bool,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_set_pinned(session_ids, pinned))
    }

    fn set_unread(&self, session_id: SessionId, unread: bool) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_set_unread(session_id, unread))
    }

    fn list_groups(&self) -> BoxFuture<'_, ApiResult<Vec<SessionGroup>>> {
        Box::pin(self.do_list_groups())
    }

    fn create_group(
        &self,
        name: String,
        color: GroupColor,
    ) -> BoxFuture<'_, ApiResult<SessionGroup>> {
        Box::pin(self.do_create_group(name, color))
    }

    fn update_group(&self, request: UpdateGroup) -> BoxFuture<'_, ApiResult<SessionGroup>> {
        Box::pin(self.do_update_group(request))
    }

    fn delete_group(&self, group_id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_delete_group(group_id))
    }

    fn reorder_groups(&self, group_ids: Vec<String>) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_reorder_groups(group_ids))
    }

    fn move_sessions(
        &self,
        session_ids: Vec<SessionId>,
        group_id: Option<String>,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_move_sessions(session_ids, group_id))
    }

    fn fork_session(&self, request: ForkSession) -> BoxFuture<'_, ApiResult<SessionSummary>> {
        Box::pin(self.do_fork(request))
    }

    fn rewind_session(&self, request: Rewind) -> BoxFuture<'_, ApiResult<RewindResult>> {
        Box::pin(self.do_rewind(request))
    }

    fn edit_and_resend(&self, request: EditResend) -> BoxFuture<'_, ApiResult<SendAccepted>> {
        Box::pin(self.do_edit_and_resend(request))
    }

    fn retry_turn(&self, request: Retry) -> BoxFuture<'_, ApiResult<SendAccepted>> {
        Box::pin(self.do_retry(request))
    }

    fn plan_file_changes(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        direction: ChangeDirection,
    ) -> BoxFuture<'_, ApiResult<FileChangePlan>> {
        Box::pin(self.do_plan_files(session_id, turn_id, direction))
    }

    fn apply_file_changes(
        &self,
        request: ApplyFileChanges,
    ) -> BoxFuture<'_, ApiResult<FileRestoreReport>> {
        Box::pin(self.do_apply_files(request))
    }

    fn find_in_session(&self, request: FindInSession) -> BoxFuture<'_, ApiResult<Vec<FindHit>>> {
        Box::pin(self.do_find(request))
    }

    fn resolve_context(
        &self,
        session_id: SessionId,
        items: Vec<ContextItem>,
    ) -> BoxFuture<'_, ApiResult<Vec<ResolvedItem>>> {
        Box::pin(self.do_resolve_context(session_id, items))
    }

    fn mention_candidates(
        &self,
        request: MentionQuery,
    ) -> BoxFuture<'_, ApiResult<Vec<MentionCandidate>>> {
        Box::pin(self.do_mentions(request))
    }

    fn list_commands(&self, session_id: SessionId) -> BoxFuture<'_, ApiResult<CommandList>> {
        Box::pin(self.do_list_commands(session_id))
    }

    fn command_search(
        &self,
        request: CommandSearch,
    ) -> BoxFuture<'_, ApiResult<CommandSearchResult>> {
        Box::pin(self.do_command_search(request))
    }

    fn import_scan(&self, request: ImportScan) -> BoxFuture<'_, ApiResult<Vec<ImportCandidate>>> {
        Box::pin(self.do_import_scan(request))
    }

    fn import_sessions(&self, request: ImportRequest) -> BoxFuture<'_, ApiResult<ImportReport>> {
        Box::pin(self.do_import(request))
    }

    fn schedules_enabled(&self) -> BoxFuture<'_, ApiResult<bool>> {
        Box::pin(self.do_schedules_enabled())
    }

    fn get_orchestra(&self) -> BoxFuture<'_, ApiResult<harness_core_api::Orchestra>> {
        Box::pin(self.do_get_orchestra())
    }

    fn set_orchestra(
        &self,
        orchestra: harness_core_api::Orchestra,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_set_orchestra(orchestra))
    }

    fn set_schedules_enabled(&self, enabled: bool) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_set_schedules_enabled(enabled))
    }

    fn list_schedules(&self) -> BoxFuture<'_, ApiResult<Vec<ScheduledPrompt>>> {
        Box::pin(self.do_list_schedules())
    }

    fn create_schedule(&self, request: NewSchedule) -> BoxFuture<'_, ApiResult<ScheduledPrompt>> {
        Box::pin(self.do_create_schedule(request))
    }

    fn update_schedule(
        &self,
        request: UpdateSchedule,
    ) -> BoxFuture<'_, ApiResult<ScheduledPrompt>> {
        Box::pin(self.do_update_schedule(request))
    }

    fn delete_schedule(&self, schedule_id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_delete_schedule(schedule_id))
    }

    fn run_schedule_now(&self, schedule_id: String) -> BoxFuture<'_, ApiResult<ScheduleRun>> {
        Box::pin(self.do_run_schedule_now(schedule_id))
    }

    fn board(&self, workspace_key: String) -> BoxFuture<'_, ApiResult<harness_core_api::Board>> {
        Box::pin(self.do_board(workspace_key))
    }

    fn create_card(
        &self,
        request: harness_core_api::NewCard,
    ) -> BoxFuture<'_, ApiResult<harness_core_api::CardView>> {
        Box::pin(self.do_create_card(request))
    }

    fn update_card(
        &self,
        request: harness_core_api::UpdateCard,
    ) -> BoxFuture<'_, ApiResult<harness_core_api::CardView>> {
        Box::pin(self.do_update_card(request))
    }

    fn move_card(
        &self,
        card_id: String,
        lane: harness_core_api::BoardLane,
        index: u32,
    ) -> BoxFuture<'_, ApiResult<harness_core_api::CardView>> {
        Box::pin(self.do_move_card(card_id, lane, index))
    }

    fn delete_card(&self, card_id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_delete_card(card_id))
    }

    fn start_card(&self, card_id: String) -> BoxFuture<'_, ApiResult<harness_core_api::CardView>> {
        Box::pin(self.do_start_card(card_id))
    }

    fn link_board(
        &self,
        workspace_key: String,
    ) -> BoxFuture<'_, ApiResult<harness_core_api::Board>> {
        Box::pin(self.do_link_board(workspace_key))
    }

    fn unlink_board(&self, workspace_key: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_unlink_board(workspace_key))
    }

    fn sync_board(
        &self,
        workspace_key: String,
    ) -> BoxFuture<'_, ApiResult<harness_core_api::Board>> {
        Box::pin(self.do_sync_board(workspace_key))
    }

    fn inbox_status(&self) -> BoxFuture<'_, ApiResult<harness_core_api::InboxStatus>> {
        Box::pin(self.do_inbox_status())
    }

    fn set_inbox_enabled(&self, enabled: bool) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_set_inbox_enabled(enabled))
    }

    fn save_inbox_source(
        &self,
        source: harness_core_api::InboxSourceInput,
    ) -> BoxFuture<'_, ApiResult<harness_core_api::InboxSource>> {
        Box::pin(self.do_save_inbox_source(source))
    }

    fn delete_inbox_source(&self, source_id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_delete_inbox_source(source_id))
    }

    fn poll_inbox_now(&self, source_id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_poll_inbox_now(source_id))
    }

    fn list_tickets(&self) -> BoxFuture<'_, ApiResult<Vec<harness_core_api::Ticket>>> {
        Box::pin(self.do_list_tickets())
    }

    fn list_triggers(
        &self,
        limit: u32,
    ) -> BoxFuture<'_, ApiResult<Vec<harness_core_api::TriggerRecord>>> {
        Box::pin(self.do_list_triggers(limit))
    }

    fn post_ticket(&self, ticket_id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_post_ticket(ticket_id))
    }

    fn stop_ticket(&self, ticket_id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_stop_ticket(ticket_id))
    }

    fn discard_ticket(&self, ticket_id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_discard_ticket(ticket_id))
    }

    fn subscribe(&self) -> EventStream {
        self.ctx.events.subscribe()
    }

    fn shutdown(&self) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(self.do_shutdown())
    }
}
