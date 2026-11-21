use super::{
    api, emit, mpsc, prefs, Adapter, AgentOverride, AgentSpec, Arc, Command, ErrorDialect, Event,
    HarnessCore, HashMap, Hub, HubConfig, Path, PathBuf, RecvError, Settings, SettingsCall,
    SettingsStore, Transport,
};

/// Hub configuration: environment defaults, `home` from the UI, per-agent defaults from settings, optional agent
/// override file.
pub(super) async fn hub_config(home: &Path, store: &SettingsStore) -> Result<HubConfig, String> {
    let mut config = HubConfig::from_env().map_err(|e| e.to_string())?;
    config.home = home.to_path_buf();
    // §9.9：产品只发一个可执行文件；记忆/委派 MCP 与 API 代理都以本程序的模式参数启动（HARNESS_MEMORY_MCP 覆盖除外）。
    let exe = std::env::current_exe().map_err(|e| format!("current executable: {e}"))?;
    config.helpers =
        harness_agent_hub::helpers::HelperLaunch::for_app(exe).map_err(|e| e.to_string())?;
    config.agent_defaults = prefs::agent_defaults(&store.snapshot());
    let snapshot = store.snapshot();
    let mut applied = HashMap::new();
    for spec in &mut config.agents {
        let custom = snapshot.agent(spec.id.as_str()).program.unwrap_or_default();
        let custom = custom.trim();
        if !custom.is_empty() {
            custom.clone_into(&mut spec.program);
        }
        applied.insert(spec.id.as_str().to_owned(), custom.to_owned());
    }
    crate::settings::programs_applied(applied);
    let policy = config.delegation.clone();
    let fact_check = config.fact_check.clone();
    let auto_update = config.auto_update.clone();
    let disabled = config.disabled_agents.clone();
    let apply = move |settings: &harness_settings::Settings| {
        disabled.set(
            settings
                .agents
                .iter()
                .filter(|(_, prefs)| !prefs.enabled)
                .map(|(id, _)| api::AgentId::new(id.clone())),
        );
        policy.set_limit(usize::from(settings.orchestra.max_concurrent));
        policy.set_nested(settings.orchestra.nested_subagents);
        policy.set_nested_limit(usize::from(settings.orchestra.max_nested_per_subagent));
        fact_check.set_on(settings.research.fact_check);
        auto_update.set_on(settings.setup.auto_update_adapters);
    };
    apply(&store.snapshot());
    let mut changes = store.subscribe();
    tokio::spawn(async move {
        while changes.changed().await.is_ok() {
            let settings = changes.borrow_and_update().clone();
            apply(&settings);
        }
    });
    // `@` mentions and the command center search files through the workspace index (built on first use).
    config.file_search = Some(harness_agent_hub::SharedFileSearch(Arc::new(
        harness_workspace::WorkspaceFileSearch::new(),
    )));
    config.agy_creds = crate::agy_cred::shared();
    let Some(file) = std::env::var_os("HARNESS_UI_AGENTS").filter(|v| !v.is_empty()) else {
        return Ok(config);
    };
    let bytes = tokio::fs::read(&file)
        .await
        .map_err(|e| format!("HARNESS_UI_AGENTS: {e}"))?;
    let list: Vec<AgentOverride> =
        serde_json::from_slice(&bytes).map_err(|e| format!("HARNESS_UI_AGENTS: {e}"))?;
    config.agents = list
        .into_iter()
        .map(|a| AgentSpec {
            id: api::AgentId::new(a.id),
            display_name: a.name,
            program: a.program,
            args: a.args,
            env: a.env.into_iter().collect(),
            install_hint: "configured in HARNESS_UI_AGENTS".into(),
            transport: Transport::Acp,
            dialect: ErrorDialect::Generic,
        })
        .collect();
    config.default_fallback_order = config.agents.iter().map(|a| a.id.clone()).collect();
    Ok(config)
}

pub(super) static CORE: std::sync::Mutex<Option<Arc<dyn HarnessCore>>> =
    std::sync::Mutex::new(None);

pub(super) fn set_core_handle(core: Option<Arc<dyn HarnessCore>>) {
    let mut slot = CORE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = core;
}

/// The running hub, for in-process consumers outside the UI port (the remote phone server).
pub fn core_handle() -> Option<Arc<dyn HarnessCore>> {
    CORE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Runs the hub adapter until `Command::Shutdown` or the command channel closes.
pub async fn run(
    home: PathBuf,
    workspace: PathBuf,
    store: Arc<SettingsStore>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    ev: mpsc::UnboundedSender<Event>,
) {
    let core: Arc<dyn HarnessCore> = loop {
        let hub = match hub_config(&home, &store).await {
            Ok(config) => Hub::start(config).await.map_err(|e| e.to_string()),
            Err(error) => Err(error),
        };
        match hub {
            Ok(hub) => break hub,
            Err(error) => {
                emit(
                    &ev,
                    Event::Error(format!("agent hub failed to start: {error}")),
                );
                // 类型化失败（§9.2.5 R20）：UI 显示带重试按钮的横幅；`RetryInit { session: None }` 重新启动 hub。
                emit(
                    &ev,
                    Event::Failure {
                        session: None,
                        item: None,
                        kind: api::FailureKind::InitFailed,
                        message: "agent hub failed to start".into(),
                        details: Some(error),
                        trace: None,
                        retry: api::RetryHint::Retry,
                    },
                );
                // 没有 hub 时仍需等待 UI 关停，否则 core 线程提前退出会让 UI 的命令全部丢失且无提示。
                let mut retry = false;
                while let Some(command) = commands.recv().await {
                    match command {
                        Command::Shutdown => break,
                        Command::RetryInit { session: None } => {
                            retry = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !retry {
                    return;
                }
            }
        }
    };
    // Agent A's panels (Diff "Turn changes") read turn diffs from the same hub; called inside this runtime.
    crate::panels::attach_core(Arc::clone(&core));
    set_core_handle(Some(Arc::clone(&core)));
    let mut events = core.subscribe();
    let mut store_rx = store.subscribe();
    let snapshot = store.snapshot();
    let mut adapter = Adapter {
        core: Arc::clone(&core),
        ev,
        workspace,
        settings: Settings {
            theme: prefs::theme_of(&snapshot),
            language: prefs::language_tag(snapshot.general.language),
            fallback_order: Vec::new(),
        },
        agents: Vec::new(),
        sticky: HashMap::new(),
        view: None,
        permissions: HashMap::new(),
        next_request: 1,
        fleet: crate::core::fleet::Fleet::default(),
    };
    adapter.startup().await;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Shutdown) => break,
                Some(command) => adapter.handle(command).await,
            },
            event = events.recv() => match event {
                Ok(event) => adapter.on_event(event).await,
                // 事件丢失时按 API 约定重新拉取快照，而不是在缺口上继续拼接。
                Err(RecvError::Lagged(_)) => adapter.resync().await,
                Err(RecvError::Closed) => break,
            },
            // settings.json changed (UI, external edit): mirror theme/language into Event::Settings. Nothing is
            // written back, so there is no loop.
            changed = store_rx.changed() => {
                if changed.is_err() {
                    continue;
                }
                let snapshot = store_rx.borrow_and_update().clone();
                adapter.on_store(&snapshot);
            }
        }
    }
    set_core_handle(None);
    if let Err(error) = core.shutdown().await {
        emit(&adapter.ev, Event::Error(format!("hub shutdown: {error}")));
    }
}

pub(super) async fn emit_setup_status(core: &dyn HarnessCore, ev: &mpsc::UnboundedSender<Event>) {
    emit(
        ev,
        Event::SetupStatus(core.setup_status().await.map_err(|e| e.to_string())),
    );
}

pub(super) async fn emit_overview(core: &dyn HarnessCore, ev: &mpsc::UnboundedSender<Event>) {
    match core.agents_overview().await {
        Ok(rows) => emit(ev, Event::AgentsOverview(rows)),
        Err(error) => emit(ev, Event::Error(format!("agents overview: {error}"))),
    }
}

/// Runs one account call (phase-9-parity.md §9.6) and answers with the fresh list or the error.
pub(super) async fn accounts_call(
    core: Arc<dyn HarnessCore>,
    ev: mpsc::UnboundedSender<Event>,
    call: crate::core::AccountsCall,
) {
    let done = match call {
        crate::core::AccountsCall::List => Ok(()),
        crate::core::AccountsCall::Add(label) => core
            .add_account(api::AddAccount {
                base: api::AgentId::new("claude-code"),
                label,
            })
            .await
            .map(|_| ()),
        crate::core::AccountsCall::Remove(id) => core.remove_account(api::AgentId::new(id)).await,
    };
    let result = match done {
        Ok(()) => core.list_accounts().await.map_err(|e| e.to_string()),
        Err(error) => Err(error.to_string()),
    };
    emit(&ev, Event::Accounts(result));
}

/// Runs one provider call (phase-9-parity.md §9.7) and answers with the fresh list or the error.
pub(super) async fn providers_call(
    core: Arc<dyn HarnessCore>,
    ev: mpsc::UnboundedSender<Event>,
    call: crate::core::ProvidersCall,
) {
    let done = match call {
        crate::core::ProvidersCall::List => Ok(()),
        crate::core::ProvidersCall::Add(request) => core.add_provider(request).await.map(|_| ()),
        crate::core::ProvidersCall::Remove(id) => core.remove_provider(api::AgentId::new(id)).await,
    };
    let result = match done {
        Ok(()) => core.list_providers().await.map_err(|e| e.to_string()),
        Err(error) => Err(error.to_string()),
    };
    emit(&ev, Event::Providers(result));
}

/// Runs one settings-page call and answers on its own channel. A dropped receiver (page closed) is not an error.
/// Session-list changes come back as the hub's `SessionsChanged` event, which refreshes `Event::Sessions`.
pub(super) async fn settings_call(core: &dyn HarnessCore, call: SettingsCall) {
    match call {
        SettingsCall::MemoryDoc(scope, reply) => {
            let _ = reply.send(core.memory_doc(scope).await);
        }
        SettingsCall::MemoryPlan(scope, content, reply) => {
            let _ = reply.send(core.memory_plan(scope, content).await);
        }
        SettingsCall::MemoryApply(plan, reply) => {
            let _ = reply.send(core.memory_apply(plan).await);
        }
        SettingsCall::ListProjects(request, reply) => {
            let _ = reply.send(core.list_projects(request).await);
        }
        SettingsCall::RemoveProjects {
            keys,
            delete_data,
            reply,
        } => {
            let _ = reply.send(core.remove_projects(keys, delete_data).await);
        }
        SettingsCall::RestoreProjects(keys, reply) => {
            let _ = reply.send(core.restore_projects(keys).await);
        }
        SettingsCall::ArchiveSessions(ids, reply) => {
            let ids = ids.into_iter().map(api::SessionId::new).collect();
            let _ = reply.send(core.archive_sessions(ids).await);
        }
        SettingsCall::ListArchived(request, reply) => {
            let _ = reply.send(core.list_archived(request).await);
        }
        SettingsCall::Unarchive(selection, reply) => {
            let _ = reply.send(core.unarchive(selection).await);
        }
        SettingsCall::DeleteArchived(selection, reply) => {
            let _ = reply.send(core.delete_archived(selection).await);
        }
    }
}
