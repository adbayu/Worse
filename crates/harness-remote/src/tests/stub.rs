use super::*;

pub(super) const TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------- stub core

/// Minimal `HarnessCore`: a session list, an event sender; everything else is unsupported.
pub(super) struct StubCore {
    tx: broadcast::Sender<HubEvent>,
    seq: AtomicU64,
}

impl StubCore {
    pub(super) fn new() -> Arc<Self> {
        let (tx, _) = broadcast::channel(64);
        Arc::new(Self {
            tx,
            seq: AtomicU64::new(0),
        })
    }

    pub(super) fn publish(&self, kind: EventKind) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.tx.send(HubEvent {
            seq,
            session_id: None,
            kind,
        });
    }
}

pub(super) fn summary() -> SessionSummary {
    SessionSummary {
        id: SessionId::from("s1"),
        title: "Test session".to_owned(),
        workspace_path: "C:/work".to_owned(),
        workspace_key: "wk".to_owned(),
        active_agent: AgentId::from("fake-a"),
        created_at: 1,
        updated_at: 2,
        turn_count: 1,
        agents_used: Vec::new(),
        pinned: false,
        group_id: None,
        order: 0,
        unread: false,
        forked_from: None,
        imported_from: None,
        parent_id: None,
        worktree: None,
    }
}

pub(super) fn unused<T>(method: &'static str) -> BoxFuture<'static, ApiResult<T>> {
    Box::pin(async move { Err(ApiError::Internal(format!("{method} not supported"))) })
}

impl HarnessCore for StubCore {
    fn list_agents(&self) -> BoxFuture<'_, ApiResult<Vec<harness_core_api::AgentInfo>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn get_fallback_order(&self) -> BoxFuture<'_, ApiResult<Vec<AgentId>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn set_fallback_order(&self, _order: Vec<AgentId>) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn create_session(&self, _request: CreateSession) -> BoxFuture<'_, ApiResult<SessionSummary>> {
        unused("create_session")
    }

    fn list_sessions(
        &self,
        _request: ListSessions,
    ) -> BoxFuture<'_, ApiResult<Vec<SessionSummary>>> {
        Box::pin(async { Ok(vec![summary()]) })
    }

    fn open_session(
        &self,
        _session_id: SessionId,
    ) -> BoxFuture<'_, ApiResult<harness_core_api::SessionDetail>> {
        unused("open_session")
    }

    fn send_message(
        &self,
        _request: SendMessage,
    ) -> BoxFuture<'_, ApiResult<harness_core_api::SendAccepted>> {
        unused("send_message")
    }

    fn set_active_agent(
        &self,
        _session_id: SessionId,
        _agent: AgentId,
    ) -> BoxFuture<'_, ApiResult<()>> {
        unused("set_active_agent")
    }

    fn stop(&self, _session_id: SessionId) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn list_modes(
        &self,
        session_id: SessionId,
    ) -> BoxFuture<'_, ApiResult<harness_core_api::SessionModes>> {
        Box::pin(async move {
            Ok(harness_core_api::SessionModes {
                agent: AgentId::from("stub"),
                current: Some(session_id.to_string()),
                available: Vec::new(),
                known: true,
            })
        })
    }

    fn cancel_queued(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn respond_permission(
        &self,
        _session_id: SessionId,
        _request_id: PermissionRequestId,
        _option_id: String,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn memory_read(&self, _scope: MemoryScope) -> BoxFuture<'_, ApiResult<String>> {
        Box::pin(async { Ok(String::new()) })
    }

    fn memory_write(
        &self,
        _scope: MemoryScope,
        _content: String,
        _mode: MemoryWriteMode,
    ) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn subscribe(&self) -> EventStream {
        EventStream::new(self.tx.subscribe())
    }

    fn shutdown(&self) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async { Ok(()) })
    }
}
