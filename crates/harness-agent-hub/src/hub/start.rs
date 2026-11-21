use super::{
    oneshot, store, ActorMsg, ActorTx, AgentId, AgentPool, AgentSpec, ApiError, ApiResult, Arc,
    AtomicBool, EventBus, HashMap, Hub, HubConfig, HubCtx, HubError, MemoryFiles, MemoryScope,
    MemoryTarget, Mutex, Ordering, QuotaRegistry, SessionActor, SessionCmd, SessionId, Store,
    FALLBACK_KEY,
};

impl Hub {
    /// Opens the store under `config.home` and starts the hub (no agent process is started).
    ///
    /// # Errors
    ///
    /// Store/IO failures.
    pub async fn start(mut config: HubConfig) -> Result<Arc<Self>, HubError> {
        if config.load_agents_file {
            config.apply_agents_file().await?;
        }
        // §9.6：额外的 Claude 账号作为独立 agent（各自 CLAUDE_CONFIG_DIR），放在 Claude Code 之后参与 failover。
        crate::accounts::apply(&mut config).await;
        crate::agy_agents::apply(&mut config).await;
        // 自定义 API provider（§9.7）同样在启动时变成 agent，排在默认顺序末尾。
        crate::providers::apply(&mut config).await;
        let db_path = config.db_path();
        let store = tokio::task::spawn_blocking(move || Store::open(&db_path))
            .await
            .map_err(|e| HubError::Config(format!("store open task failed: {e}")))??;
        let (quota, fallback, subagents) = store
            .run(|c| {
                Ok((
                    store::load_quota(c)?,
                    store::get_setting(c, FALLBACK_KEY)?,
                    crate::delegate::Registry::load(c)?,
                ))
            })
            .await?;
        let specs: HashMap<AgentId, AgentSpec> = config
            .agents
            .iter()
            .map(|s| (s.id.clone(), s.clone()))
            .collect();
        let fallback = fallback
            .and_then(|json| serde_json::from_str::<Vec<AgentId>>(&json).ok())
            .map(|order| {
                // 修复：已保存的 fallback 顺序会丢掉之后新增的 agent（如新账号），使其永远不参与 failover。
                // 依据 §9.6：缺失的 agent 按默认顺序插到其前一个默认邻居之后。
                let mut order = order
                    .into_iter()
                    .filter(|a| specs.contains_key(a))
                    .collect::<Vec<_>>();
                for (i, agent) in config.default_fallback_order.iter().enumerate() {
                    if order.contains(agent) {
                        continue;
                    }
                    let position = config.default_fallback_order[..i]
                        .iter()
                        .rev()
                        .find_map(|prev| order.iter().position(|a| a == prev))
                        .map_or(0, |p| p + 1);
                    order.insert(position, agent.clone());
                }
                order
            })
            .filter(|order| !order.is_empty())
            .unwrap_or_else(|| config.default_fallback_order.clone());
        let events = EventBus::new(config.event_capacity);
        let pool = AgentPool::new(&config.agents, config.agent_idle_timeout, events.clone());
        let memory = MemoryFiles::new(config.home.clone());
        let machine = crate::machine::Machine::load(&config.home).await;
        crate::gh::set_home(&config.home);
        crate::tools::set_home(&config.home);
        let ctx = Arc::new(HubCtx {
            config,
            store,
            pool,
            quota: QuotaRegistry::with_state(quota),
            events,
            memory,
            specs,
            fallback: Mutex::new(fallback),
            advertised: Mutex::new(HashMap::new()),
            probe_lock: tokio::sync::Mutex::new(()),
            chat: crate::hub_chat::ChatState::default(),
            subagents,
            machine,
            routers: crate::inbox::Routers::default(),
            agy: crate::agy_ops::AgyState::default(),
            rotation: Mutex::new(HashMap::new()),
        });
        let hub = Arc::new_cyclic(|me| Self {
            ctx,
            actors: tokio::sync::Mutex::new(HashMap::new()),
            shut_down: AtomicBool::new(false),
            me: me.clone(),
            scheduler: crate::schedule::Scheduler::default(),
            inbox: crate::inbox::Inbox::default(),
            board_sync: crate::board_sync::BoardSyncSlot::default(),
            delegate_task: Mutex::new(None),
        });
        hub.start_scheduler().await;
        hub.start_delegate().await;
        hub.start_routing().await;
        tokio::spawn(crate::inbox::track(hub.me.clone()));
        hub.ensure_inbox().await;
        tokio::spawn(crate::board_sync::track(hub.me.clone()));
        hub.ensure_board_sync().await;
        tokio::spawn(crate::adapter_update::track(hub.me.clone()));
        Ok(hub)
    }

    /// Starts the subagent tracker and the loopback delegate server (§9.8). `HARNESS_DELEGATE=0` turns the
    /// delegate tool off; a bind failure only logs (agents then simply get no `harness-delegate` server).
    pub(super) async fn start_delegate(&self) {
        tokio::spawn(crate::delegate::track(Arc::downgrade(&self.ctx)));
        tokio::spawn(crate::board::track(Arc::downgrade(&self.ctx)));
        tokio::spawn(crate::resume::track(Arc::downgrade(&self.ctx)));
        if std::env::var("HARNESS_DELEGATE").is_ok_and(|v| v == "0") {
            return;
        }
        match crate::delegate_server::start(self.me.clone()).await {
            Ok((endpoint, task)) => {
                tokio::spawn(crate::delegate_server::push(self.me.clone()));
                let _ = self.ctx.subagents.endpoint.set(endpoint);
                if let Ok(mut slot) = self.delegate_task.lock() {
                    *slot = Some(task.abort_handle());
                }
            }
            Err(error) => tracing::warn!(%error, "delegate server not started"),
        }
    }

    pub(crate) fn check_open(&self) -> ApiResult<()> {
        if self.shut_down.load(Ordering::SeqCst) {
            Err(ApiError::ShutDown)
        } else {
            Ok(())
        }
    }

    pub(super) async fn actor(&self, id: &SessionId) -> ApiResult<ActorTx> {
        self.check_open()?;
        let mut actors = self.actors.lock().await;
        if let Some(tx) = actors.get(id) {
            if !tx.is_closed() {
                return Ok(tx.clone());
            }
        }
        let lookup = id.clone();
        let row = self
            .ctx
            .store
            .run(move |c| store::load_session(c, &lookup))
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("session {id}")))?;
        let tx = SessionActor::spawn(Arc::clone(&self.ctx), row).await?;
        actors.insert(id.clone(), tx.clone());
        Ok(tx)
    }

    pub(crate) async fn ask<T>(
        &self,
        id: &SessionId,
        make: impl FnOnce(oneshot::Sender<ApiResult<T>>) -> SessionCmd,
    ) -> ApiResult<T> {
        let tx = self.actor(id).await?;
        let (reply, rx) = oneshot::channel();
        if let Err(unsent) = tx.send(ActorMsg::Cmd(make(reply))) {
            self.actor(id)
                .await?
                .send(unsent.0)
                .map_err(|_| ApiError::ShutDown)?;
        }
        rx.await.map_err(|_| ApiError::ShutDown)?
    }

    pub(crate) fn memory_target(scope: &MemoryScope) -> MemoryTarget {
        match scope {
            MemoryScope::Global => MemoryTarget::Global,
            MemoryScope::Project { workspace_path } => {
                MemoryTarget::Project(std::path::PathBuf::from(workspace_path))
            }
        }
    }
}
