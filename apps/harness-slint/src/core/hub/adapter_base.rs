use super::{
    accounts_call, api, create_request, emit, emit_overview, emit_setup_status, prefs,
    providers_call, reset_label, settings_call, summary, Adapter, AgentId, AgentState, AgentStatus,
    ApiError, Arc, Command, Event, PathBuf, Pref,
};

impl Adapter {
    pub(super) fn emit(&self, event: Event) {
        emit(&self.ev, event);
    }

    pub(super) fn report(&self, what: &str, error: &ApiError) {
        self.emit(Event::Error(format!("{what}: {error}")));
    }

    pub(super) fn active_id(&self) -> Option<String> {
        self.view.as_ref().map(|v| v.id.0.clone())
    }

    pub(super) async fn startup(&mut self) {
        self.refresh_settings().await;
        self.refresh_agents().await;
        self.refresh_sessions().await;
    }

    pub(super) async fn resync(&mut self) {
        self.startup().await;
        if let Some(id) = self.active_id() {
            self.open(&id, false).await;
        }
    }

    pub(super) async fn refresh_settings(&mut self) {
        match self.core.get_fallback_order().await {
            Ok(order) => self.settings.fallback_order = order.into_iter().map(|a| a.0).collect(),
            Err(error) => self.report("fallback order", &error),
        }
        self.emit(Event::Settings(self.settings.clone()));
    }

    pub(super) fn on_store(&mut self, snapshot: &harness_settings::Settings) {
        let theme = prefs::theme_of(snapshot);
        let language = prefs::language_tag(snapshot.general.language);
        if theme != self.settings.theme || language != self.settings.language {
            self.settings.theme = theme;
            self.settings.language = language;
            self.emit(Event::Settings(self.settings.clone()));
        }
    }

    pub(super) async fn refresh_agents(&mut self) {
        match self.core.list_agents().await {
            Ok(list) => self.agents = list,
            Err(error) => {
                self.report("agents", &error);
                return;
            }
        }
        let statuses = self
            .agents
            .iter()
            .map(|a| AgentStatus {
                id: a.id.0.clone(),
                name: a.display_name.clone(),
                state: if matches!(a.availability, api::Availability::AdapterMissing { .. }) {
                    AgentState::AdapterMissing
                } else if !a.availability.is_available() {
                    AgentState::Unavailable
                } else if let api::QuotaState::Exhausted { resets_at, .. } = &a.quota {
                    AgentState::Exhausted {
                        reset: reset_label(*resets_at),
                    }
                } else {
                    AgentState::Available
                },
            })
            .collect();
        self.emit(Event::Agents(statuses));
    }

    pub(super) async fn refresh_sessions(&mut self) {
        match self.core.list_sessions(api::ListSessions::default()).await {
            Ok(list) => {
                for s in &list {
                    self.sticky.insert(s.id.clone(), s.active_agent.0.clone());
                }
                self.fleet.set_sessions(&list);
                self.emit(Event::Sessions(list.iter().map(summary).collect()));
                if let Some(fleet) = self.fleet.changed() {
                    self.emit(Event::Fleet(fleet));
                }
            }
            Err(error) => self.report("sessions", &error),
        }
    }

    pub(super) async fn handle(&mut self, command: Command) {
        match command {
            Command::CreateSession { agent, workspace } => {
                self.create(false, agent, workspace).await;
            }
            Command::CreateWorktreeSession { agent, workspace } => {
                self.create(true, agent, workspace).await;
            }
            Command::OpenSession { session } => self.open(&session, false).await,
            Command::OpenSessionAll { session } => self.open(&session, true).await,
            Command::Send {
                session,
                agent,
                text,
            } => self.send(session, agent, text, Vec::new()).await,
            Command::SendContext {
                session,
                agent,
                text,
                context,
            } => self.send(session, agent, text, context).await,
            Command::ChoosePermission {
                session,
                request,
                option_id,
            } => self.choose(session, request, option_id).await,
            Command::Chat(call) => self.chat(call),
            Command::Cancel { session } => {
                if let Err(error) = self.core.stop(api::SessionId::new(session)).await {
                    self.report("stop", &error);
                }
            }
            Command::CancelQueued { session, id } => self.cancel_queued(session, id).await,
            Command::StopBackgroundTask { session, id } => {
                let result = self
                    .core
                    .stop_background_task(api::SessionId::new(session), id)
                    .await;
                if let Err(error) = result {
                    self.report("stop background task", &error);
                }
            }
            command @ (Command::EditQueued { .. }
            | Command::ReorderQueued { .. }
            | Command::RunQueuedNow { .. }) => self.queue_command(command).await,
            Command::Steer {
                session,
                agent,
                text,
                context,
            } => self.steer(session, agent, text, context).await,
            Command::SetMode { session, mode } => {
                self.set_pref(session, Pref::Mode(mode)).await;
            }
            Command::SetModel { session, model } => {
                self.set_pref(session, Pref::Model(model)).await;
            }
            Command::SetConfigOption { session, id, value } => {
                self.set_pref(session, Pref::Config(id, value)).await;
            }
            Command::SetMachine { session, on } => self.set_pref(session, Pref::Machine(on)).await,
            Command::SessionOptions { session } => self.session_options(&session, false).await,
            Command::RetryInit { session } => self.retry_failed(session).await,
            Command::SetStickyAgent { session, agent } => {
                let result = self
                    .core
                    .set_active_agent(api::SessionId::new(session), api::AgentId::new(agent))
                    .await;
                if let Err(error) = result {
                    self.report("switch agent", &error);
                }
                self.refresh_agents().await;
            }

            Command::SetFallbackOrder(order) => {
                let order = order.into_iter().map(api::AgentId::new).collect();
                if let Err(error) = self.core.set_fallback_order(order).await {
                    self.report("fallback order", &error);
                }
                self.refresh_settings().await;
            }
            command @ (Command::AgentOptions(_)
            | Command::AgentsOverview
            | Command::TestAgent(_)
            | Command::Accounts(_)
            | Command::Providers(_)
            | Command::AgyAccounts(_)
            | Command::Setup(_)
            | Command::Settings(_)) => self.spawn_settings(command),
            Command::Shutdown => {}
        }
    }

    /// Settings-page commands run in their own task: the test may take up to 20 s and the settings calls must not
    /// stall hub event handling.
    pub(super) fn spawn_settings(&self, command: Command) {
        let (core, ev) = (Arc::clone(&self.core), self.ev.clone());
        match command {
            // 读取选项时 hub 可能先启动 agent 探测（几秒）：放到独立任务里，避免阻塞聊天命令。
            Command::AgentOptions(agent) => drop(tokio::spawn(async move {
                match core.agent_options(api::AgentId::new(agent.clone())).await {
                    Ok(options) => emit(&ev, prefs::agent_options_event(agent, &options)),
                    Err(error) => emit(&ev, Event::Error(format!("agent options: {error}"))),
                }
            })),
            Command::AgentsOverview => drop(tokio::spawn(async move {
                emit_overview(&*core, &ev).await;
            })),
            Command::TestAgent(agent) => drop(tokio::spawn(async move {
                match core.test_agent(api::AgentId::new(agent)).await {
                    Ok(result) => emit(&ev, Event::AgentTested(result)),
                    Err(error) => emit(&ev, Event::Error(format!("agent test: {error}"))),
                }
                emit_overview(&*core, &ev).await;
            })),
            Command::Accounts(call) => drop(tokio::spawn(accounts_call(core, ev, call))),
            Command::Providers(call) => drop(tokio::spawn(providers_call(core, ev, call))),
            Command::AgyAccounts(op) => drop(tokio::spawn(async move {
                let sign_in = match &op {
                    api::AgyAccountOp::SignIn { id } => Some(id.clone()),
                    _ => None,
                };
                let result = core.agy_accounts(op).await.map_err(|e| e.to_string());
                let sign_in = sign_in.filter(|_| result.is_ok());
                emit(&ev, Event::AgyAccounts { sign_in, result });
            })),
            Command::Setup(crate::core::SetupCall::Status) => drop(tokio::spawn(async move {
                emit_setup_status(&*core, &ev).await;
            })),
            // 安装可能持续数分钟：独立任务，不阻塞聊天命令；进度与结束由 hub 的 AdapterInstall 事件送达。
            Command::Setup(crate::core::SetupCall::Install(agent)) => {
                drop(tokio::spawn(async move {
                    let result = if agent == "gh" {
                        core.install_gh().await
                    } else {
                        core.install_adapter(api::AgentId::new(agent.clone()))
                            .await
                            .map(drop)
                    };
                    if let Err(error) = result {
                        // 参数错误等不经过 hub 事件，这里补一个失败结束事件，让 UI 退出 "Installing…" 状态。
                        emit(
                            &ev,
                            Event::AdapterInstall {
                                agent,
                                status: api::AdapterInstallStatus::Failed,
                                line: None,
                                message: Some(error.to_string()),
                            },
                        );
                        emit_setup_status(&*core, &ev).await;
                    }
                }));
            }
            Command::Settings(call) => drop(tokio::spawn(async move {
                settings_call(&*core, call).await;
            })),
            _ => {}
        }
    }

    /// R12 queue edit / reorder / run-now. The answer is the hub's `QueueChanged` (→ `Event::Queue`).
    pub(super) async fn queue_command(&self, command: Command) {
        let (what, result) = match command {
            Command::EditQueued { session, id, text } => (
                "edit queued message",
                self.core
                    .edit_queued(api::SessionId::new(session), api::TurnId::new(id), text)
                    .await,
            ),
            Command::ReorderQueued { session, ids } => (
                "reorder queue",
                self.core
                    .reorder_queued(
                        api::SessionId::new(session),
                        ids.into_iter().map(api::TurnId::new).collect(),
                    )
                    .await,
            ),
            Command::RunQueuedNow { session, id } => (
                "run queued message now",
                self.core
                    .run_queued_now(api::SessionId::new(session), api::TurnId::new(id))
                    .await,
            ),
            _ => return,
        };
        if let Err(error) = result {
            self.report(what, &error);
        }
    }

    pub(super) async fn create(
        &mut self,
        worktree: bool,
        agent: AgentId,
        workspace: Option<PathBuf>,
    ) {
        let target = workspace.as_deref();
        let request = create_request(&self.workspace, worktree, &agent, target);
        let chosen = request.agent.is_some();
        let mut result = self.core.create_session(request).await;
        // 修复：所选 agent 已卸载/失效时不要让新建会话失败，退回 hub 的默认顺序。
        if chosen
            && matches!(
                result,
                Err(api::ApiError::AgentUnavailable(_) | api::ApiError::NotFound(_))
            )
        {
            result = self
                .core
                .create_session(create_request(
                    &self.workspace,
                    worktree,
                    &AgentId::new(),
                    target,
                ))
                .await;
        }
        match result {
            Ok(created) => {
                self.refresh_sessions().await;
                self.open(created.id.as_str(), false).await;
            }
            Err(error) => self.report("new session", &error),
        }
    }
}
