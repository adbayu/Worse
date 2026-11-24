use super::{emit, now_ms, queue, Command, Event, FakeCore, Session, SessionSummary, VecDeque};

impl FakeCore {
    #[allow(clippy::too_many_lines)]
    pub(super) async fn handle(&mut self, command: Command) {
        match command {
            Command::CreateSession { agent, .. } | Command::CreateWorktreeSession { agent, .. } => {
                let id = uuid::Uuid::new_v4().to_string();
                let sticky = if agent.is_empty() || !self.is_available(&agent) {
                    self.first_available()
                        .unwrap_or_else(|| "claude-code".into())
                } else {
                    agent
                };
                let summary = SessionSummary {
                    id: id.clone(),
                    title: String::new(),
                    updated_ms: now_ms(),
                    ..SessionSummary::default()
                };
                self.open.insert(
                    id.clone(),
                    Session {
                        summary,
                        items: Vec::new(),
                        next_item: 1,
                        sticky,
                        queue: VecDeque::new(),
                        next_queue: 1,
                        run: None,
                        paused: false,
                    },
                );
                self.persist_session(&id).await;
                self.sync_summary(&id);
                self.emit_opened(&id);
            }
            Command::OpenSession { session } | Command::OpenSessionAll { session } => {
                if self.ensure_open(&session).await {
                    self.emit_opened(&session);
                    self.emit_session_options(&session, false);
                } else {
                    emit(
                        &self.ev,
                        Event::Error(format!("session {session} not found")),
                    );
                }
            }
            // fake 不处理附件：带上下文的发送按普通发送处理。
            Command::Send {
                session,
                agent,
                text,
            }
            | Command::SendContext {
                session,
                agent,
                text,
                ..
            } => self.send(session, agent, text).await,
            Command::Cancel { session } => self.cancel(&session),
            Command::CancelQueued { session, id } => {
                if let Some(s) = self.open.get_mut(&session) {
                    s.queue.retain(|q| q.id != id);
                    let items = s.queue.iter().cloned().collect();
                    emit(&self.ev, Event::Queue { session, items });
                }
            }
            // fake 只提供 allow / deny 两类选项：以 deny/reject 开头的选项按拒绝处理。
            Command::ChoosePermission {
                session,
                request,
                option_id,
            } => {
                let allow = !(option_id.starts_with("deny") || option_id.starts_with("reject"));
                self.respond_permission(&session, request, allow);
            }
            Command::Chat(call) => self.chat_call(call).await,
            Command::EditQueued { session, id, text } => {
                self.queue_op(session, queue::QueueOp::Edit(id, text));
            }
            Command::ReorderQueued { session, ids } => {
                self.queue_op(session, queue::QueueOp::Reorder(ids));
            }
            Command::RunQueuedNow { session, id } => {
                self.queue_op(session, queue::QueueOp::RunNow(id));
            }
            // fake 不处理附件：带上下文的插话按普通插话处理。
            Command::Steer {
                session,
                agent,
                text,
                ..
            } => self.steer(session, agent, text).await,
            Command::SetMode { session, mode } => self.set_pref(&session, |p| p.mode = Some(mode)),
            Command::SetMachine { session, on } => self.set_pref(&session, |p| p.machine = on),
            Command::SetModel { session, model } => {
                self.set_pref(&session, |p| p.model = Some(model));
            }
            Command::SetConfigOption { session, id, value } => {
                if id == "effort" {
                    self.set_pref(&session, |p| p.effort = Some(value));
                } else {
                    emit(
                        &self.ev,
                        Event::Error(format!("unknown config option {id}")),
                    );
                }
            }
            Command::SessionOptions { session } => self.emit_session_options(&session, false),
            Command::SetStickyAgent { session, agent } => {
                if !self.ensure_open(&session).await {
                    return;
                }
                if let Some(s) = self.open.get_mut(&session) {
                    s.sticky.clone_from(&agent);
                }
                emit(
                    &self.ev,
                    Event::Sticky {
                        session: session.clone(),
                        agent,
                    },
                );
                self.persist_session(&session).await;
            }
            Command::ClearAgentQuota(_) => {}

            Command::SetFallbackOrder(order) => {
                self.settings.fallback_order = order;
                self.persist_settings().await;
                emit(&self.ev, Event::Settings(self.settings.clone()));
            }
            Command::AgentOptions(agent) => {
                emit(&self.ev, super::super::prefs::fake_agent_options(agent));
            }
            Command::AgentsOverview => self.emit_overview(),
            Command::TestAgent(agent) => self.test_agent(&agent),
            // fake core 没有账号存储（§9.6 由 hub 负责）：列表恒为空，增删报告不支持。
            Command::Accounts(call) => emit(
                &self.ev,
                Event::Accounts(match call {
                    super::super::AccountsCall::List => Ok(Vec::new()),
                    _ => Err("accounts need the agent hub".into()),
                }),
            ),
            // provider 同样由 hub 存储（§9.7）。
            Command::AgyAccounts(op) => emit(
                &self.ev,
                Event::AgyAccounts {
                    sign_in: None,
                    result: match op {
                        harness_core_api::AgyAccountOp::List => {
                            Ok(harness_core_api::AgyAccounts::default())
                        }
                        _ => Err("Antigravity accounts need the agent hub".into()),
                    },
                },
            ),
            Command::Providers(call) => emit(
                &self.ev,
                Event::Providers(match call {
                    super::super::ProvidersCall::List => Ok(Vec::new()),
                    _ => Err("providers need the agent hub".into()),
                }),
            ),
            // fake core 没有需要安装的适配器（§9.10）：空行 + 有 npm，首次启动对话框不会弹出。
            Command::Setup(super::super::SetupCall::Status) => emit(
                &self.ev,
                Event::SetupStatus(Ok(harness_core_api::SetupStatus {
                    npm_path: Some("fake://npm".into()),
                    npm_steps: String::new(),
                    agents: Vec::new(),
                    gh: harness_core_api::GhSetup {
                        path: Some("fake://gh".into()),
                        bundled: false,
                    },
                })),
            ),
            Command::Setup(super::super::SetupCall::Install(agent)) => emit(
                &self.ev,
                Event::AdapterInstall {
                    agent,
                    status: harness_core_api::AdapterInstallStatus::Failed,
                    line: None,
                    message: Some("adapters need the agent hub".into()),
                },
            ),
            Command::Settings(call) => self.settings_call(call).await,
            // fake core 不会启动失败，也不产生类型化失败：RetryInit 没有可重试的内容。
            Command::Shutdown | Command::RetryInit { .. } | Command::StopBackgroundTask { .. } => {}
        }
    }
}
