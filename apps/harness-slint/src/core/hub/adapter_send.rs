use super::{
    api, emit, prompt_title, Adapter, AgentId, ApiError, Event, FindTargets, ItemId,
    DEFAULT_SESSION_TITLE,
};

impl Adapter {
    pub(super) fn send_request(
        &self,
        session: String,
        agent: AgentId,
        text: String,
        context: Vec<api::ContextItem>,
    ) -> api::SendMessage {
        // 只有与会话当前粘滞 agent 不同时才作为手动切换传给 hub；相同则交给 hub 的粘滞/故障转移逻辑。
        let current = self
            .sticky
            .get(&api::SessionId::new(session.clone()))
            .cloned();
        let agent = (!agent.is_empty() && current.as_deref() != Some(agent.as_str()))
            .then(|| api::AgentId::new(agent));
        api::SendMessage {
            session_id: api::SessionId::new(session),
            text,
            agent,
            client_message_id: Some(uuid::Uuid::new_v4().to_string()),
            context,
        }
    }

    pub(super) async fn send(
        &mut self,
        session: String,
        agent: AgentId,
        text: String,
        context: Vec<api::ContextItem>,
    ) {
        let first = self
            .view
            .as_ref()
            .is_some_and(|v| v.id.0 == session && v.turns.is_empty());
        let title = if first { prompt_title(&text) } else { None };
        let request = self.send_request(session.clone(), agent, text, context);
        if let Err(error) = self.core.send_message(request).await {
            self.report("send", &error);
            return;
        }
        if let Some(title) = title {
            self.auto_title(session, title).await;
        }
    }

    pub(super) async fn auto_title(&self, session: String, title: String) {
        let id = api::SessionId::new(session);
        let current = self
            .core
            .list_sessions(api::ListSessions::default())
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.id == id)
            .map(|s| s.title);
        let untitled = current
            .as_deref()
            .is_some_and(|t| t.trim().is_empty() || t.trim() == DEFAULT_SESSION_TITLE);
        if !untitled {
            return;
        }
        if let Err(error) = self.core.rename_session(id, title).await {
            self.report("rename session", &error);
        }
    }

    /// Steer = the message goes to the head of the queue (the hub emits `QueueChanged`, mirrored as `Event::Queue`).
    pub(super) async fn steer(
        &mut self,
        session: String,
        agent: AgentId,
        text: String,
        context: Vec<api::ContextItem>,
    ) {
        let request = self.send_request(session, agent, text, context);
        if let Err(error) = self.core.steer(request).await {
            self.report("steer", &error);
        }
    }

    pub(super) async fn cancel_queued(&mut self, session: String, id: String) {
        let turn = api::TurnId::new(id);
        if let Err(error) = self
            .core
            .cancel_queued(api::SessionId::new(session.clone()), turn.clone())
            .await
        {
            self.report("cancel queued message", &error);
            return;
        }
        // hub 可能不为排队中被移除的回合发事件，所以本地同步移除。
        // 修复：之后到达的该回合 TurnFinished 已不在队列里，会走正常结束路径，把正在运行的回合标记为结束
        // （Stop 按钮消失）；记录已取消的回合，其 TurnFinished 到达时直接忽略。
        if let Some(view) = self.view.as_mut().filter(|v| v.id.0 == session) {
            if view.dequeue(&turn).is_some() {
                view.cancelled.insert(turn);
                let items = view.queue_items();
                emit(&self.ev, Event::Queue { session, items });
            }
        }
    }

    /// Answers with the exact option the agent offered (the only permission answer path).
    pub(super) async fn choose(&mut self, session: String, request: u64, option_id: String) {
        let Some(pending) = self.permissions.get(&request) else {
            return;
        };
        if !pending.options.iter().any(|o| o.option_id == option_id) {
            self.emit(Event::Error(format!(
                "the agent did not offer the answer {option_id}"
            )));
            return;
        }
        let request_id = pending.request_id.clone();
        if let Err(error) = self
            .core
            .respond_permission(api::SessionId::new(session), request_id, option_id)
            .await
        {
            self.report("permission", &error);
        }
    }

    /// Hub turn of `item` in the opened session `session`.
    pub(super) fn turn_of(&self, session: &str, item: ItemId) -> Result<api::TurnId, ApiError> {
        self.view
            .as_ref()
            .filter(|v| v.id.0 == session)
            .and_then(|v| v.item_turn.get(&item).cloned())
            .ok_or_else(|| ApiError::NotFound(format!("item {item} of session {session}")))
    }

    /// Item lookup of the opened session for find hits: turn → (user, reply) and tool call → item.
    pub(super) fn find_targets(&self, session: &str) -> FindTargets {
        self.view
            .as_ref()
            .filter(|v| v.id.0 == session)
            .map(|v| FindTargets {
                turns: v
                    .turns
                    .iter()
                    .map(|(id, t)| (id.clone(), (t.user, t.reply)))
                    .collect(),
                tools: v.tools.clone(),
            })
            .unwrap_or_default()
    }
}
