use super::{
    admin, demo_subagents, emit, mpsc, prefs, write_json, AgentId, AgentMsg, AgentState,
    AgentStatus, Event, FakeCore, FakeState, HashMap, Item, Path, Session, SessionFile, SessionId,
    Settings, SettingsStore, TurnStatus, VecDeque, FAKE_STATE_FILE, KNOWN_AGENTS,
};

impl FakeCore {
    pub(super) async fn load(
        home: &Path,
        store: &SettingsStore,
        ev: mpsc::UnboundedSender<Event>,
        agent_tx: mpsc::UnboundedSender<AgentMsg>,
    ) -> Self {
        let sessions_dir = home.join("sessions");
        let settings_file = home.join(FAKE_STATE_FILE);
        let state: FakeState = match tokio::fs::read(&settings_file).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => FakeState::default(),
        };
        let snapshot = store.snapshot();
        let defaults = Settings::default();
        let settings = Settings {
            theme: prefs::theme_of(&snapshot),
            language: prefs::language_tag(snapshot.general.language),
            fallback_order: state.fallback_order.unwrap_or(defaults.fallback_order),
        };
        let available_list =
            std::env::var("HARNESS_FAKE_AGENTS").unwrap_or_else(|_| "claude-code,codex".into());
        let available: Vec<&str> = available_list.split(',').map(str::trim).collect();
        let agents = KNOWN_AGENTS
            .iter()
            .map(|(id, name)| AgentStatus {
                id: (*id).to_string(),
                name: (*name).to_string(),
                state: if available.contains(id) {
                    AgentState::Available
                } else {
                    AgentState::Unavailable
                },
            })
            .collect();
        let mut summaries = Vec::new();
        if let Ok(mut dir) = tokio::fs::read_dir(&sessions_dir).await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(bytes) = tokio::fs::read(&path).await {
                    if let Ok(file) = serde_json::from_slice::<SessionFile>(&bytes) {
                        summaries.push(file.summary);
                    }
                }
            }
        }
        summaries.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
        Self {
            sessions_dir,
            settings_file,
            ev,
            agent_tx,
            settings,
            agents,
            summaries,
            open: HashMap::new(),
            next_run: 1,
            next_request: 1,
            admin: admin::FakeAdmin::default(),
            prefs: HashMap::new(),
        }
    }

    pub(super) fn emit_startup(&self) {
        emit(&self.ev, Event::Settings(self.settings.clone()));
        emit(&self.ev, Event::Agents(self.agents.clone()));
        emit(&self.ev, Event::Sessions(self.summaries.clone()));
    }

    pub(super) fn is_available(&self, agent: &str) -> bool {
        self.agents
            .iter()
            .any(|a| a.id == agent && a.state == AgentState::Available)
    }

    /// Next available agent in the fallback order after `from` (wrapping), never `from` itself.
    pub(super) fn next_available_after(&self, from: &str) -> Option<AgentId> {
        let order = &self.settings.fallback_order;
        let start = order.iter().position(|a| a == from).map_or(0, |p| p + 1);
        (0..order.len())
            .map(|i| &order[(start + i) % order.len()])
            .find(|a| a.as_str() != from && self.is_available(a))
            .cloned()
    }

    pub(super) fn first_available(&self) -> Option<AgentId> {
        self.settings
            .fallback_order
            .iter()
            .find(|a| self.is_available(a))
            .cloned()
    }

    pub(super) fn on_store(&mut self, snapshot: &harness_settings::Settings) {
        let theme = prefs::theme_of(snapshot);
        let language = prefs::language_tag(snapshot.general.language);
        if theme != self.settings.theme || language != self.settings.language {
            self.settings.theme = theme;
            self.settings.language = language;
            emit(&self.ev, Event::Settings(self.settings.clone()));
        }
    }

    pub(super) async fn persist_settings(&self) {
        let state = FakeState {
            fallback_order: Some(self.settings.fallback_order.clone()),
        };
        if let Err(error) = write_json(&self.settings_file, &state).await {
            emit(
                &self.ev,
                Event::Error(format!("settings not saved: {error}")),
            );
        }
    }

    pub(super) async fn persist_session(&self, session: &SessionId) {
        let Some(s) = self.open.get(session) else {
            return;
        };
        let file = SessionFile {
            summary: s.summary.clone(),
            sticky: s.sticky.clone(),
            items: s.items.iter().map(|(_, item)| item.clone()).collect(),
        };
        if let Err(error) =
            write_json(&self.sessions_dir.join(format!("{session}.json")), &file).await
        {
            emit(
                &self.ev,
                Event::Error(format!("session not saved: {error}")),
            );
        }
    }

    pub(super) fn sync_summary(&mut self, session: &SessionId) {
        let Some(s) = self.open.get(session) else {
            return;
        };
        let summary = s.summary.clone();
        self.summaries.retain(|existing| existing.id != summary.id);
        self.summaries.insert(0, summary);
        emit(&self.ev, Event::Sessions(self.summaries.clone()));
    }

    pub(super) async fn ensure_open(&mut self, session: &SessionId) -> bool {
        if self.open.contains_key(session) {
            return true;
        }
        let path = self.sessions_dir.join(format!("{session}.json"));
        let Ok(bytes) = tokio::fs::read(&path).await else {
            return false;
        };
        let Ok(file) = serde_json::from_slice::<SessionFile>(&bytes) else {
            return false;
        };
        let mut loaded = Session {
            summary: file.summary,
            items: Vec::new(),
            next_item: 1,
            sticky: file.sticky,
            queue: VecDeque::new(),
            next_queue: 1,
            run: None,
            paused: false,
        };
        for mut item in file.items {
            // 上次退出时仍在流式输出的回合不会再继续，按中断处理，避免恢复后永远显示“流式中”。
            if let Item::Assistant { status, .. } = &mut item {
                if *status == TurnStatus::Streaming {
                    *status = TurnStatus::Interrupted;
                }
            }
            loaded.push(item);
        }
        self.open.insert(session.clone(), loaded);
        true
    }

    pub(super) fn emit_opened(&self, session: &SessionId) {
        let Some(s) = self.open.get(session) else {
            return;
        };
        emit(
            &self.ev,
            Event::SessionOpened {
                session: s.summary.clone(),
                items: s.items.clone(),
                sticky: s.sticky.clone(),
                queued: s.queue.iter().cloned().collect(),
                paused: s.paused,
            },
        );
        emit(
            &self.ev,
            Event::Running {
                session: session.clone(),
                running: s.run.is_some(),
            },
        );
        // QC aid for the Orchestra live view: HARNESS_FAKE_SUBAGENTS=1 gives every session demo subagents.
        if std::env::var_os("HARNESS_FAKE_SUBAGENTS").is_some() {
            emit(
                &self.ev,
                Event::Subagents {
                    session: session.clone(),
                    items: demo_subagents(session),
                },
            );
            let other = "fake-background-session".to_owned();
            emit(
                &self.ev,
                Event::Fleet(vec![
                    super::super::FleetSession {
                        id: session.clone(),
                        title: s.summary.title.clone(),
                        prompt: "Split the hub into smaller modules and keep the tests green"
                            .into(),
                        agent: s.sticky.clone(),
                        running: s.run.is_some(),
                        open: true,
                        subagents: demo_subagents(session),
                        parents: Vec::new(),
                    },
                    super::super::FleetSession {
                        id: other.clone(),
                        title: "Refactor billing module".into(),
                        prompt: "Move invoice rounding into one helper".into(),
                        agent: "codex".into(),
                        running: true,
                        open: false,
                        subagents: demo_subagents(&other).into_iter().take(1).collect(),
                        parents: Vec::new(),
                    },
                ]),
            );
        }
    }
}
