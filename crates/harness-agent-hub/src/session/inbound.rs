use super::{
    background, json, store, tool_view, AgentId, EventKind, Inbound, PrefChange, SessionActor,
    ToolMark, Value,
};

impl SessionActor {
    pub(super) async fn on_inbound(&mut self, inbound: Inbound) {
        match inbound {
            Inbound::Update { native, update } => {
                // 命令列表可能在 session/new 之后、Ready 之前到达，此时 running.native 尚未设置；按 native 归属接收。
                if update.get("sessionUpdate").and_then(Value::as_str)
                    == Some("available_commands_update")
                {
                    self.on_commands_update(&native, &update).await;
                } else if background::is_task_update(&update) {
                    self.on_task_update(&native, &update).await;
                } else if self.running.as_ref().and_then(|r| r.native.as_ref()) == Some(&native) {
                    self.on_update(&update).await;
                    self.arm_idle_end();
                } else if self.running.is_none() {
                    self.on_unprompted_update(&native, &update).await;
                }
            }
            Inbound::Request {
                native,
                id,
                method,
                params,
            } => {
                let matches = self
                    .running
                    .as_ref()
                    .is_some_and(|r| r.native.as_ref() == Some(&native) && !r.cancelling);
                let connection = self.running.as_ref().and_then(|r| r.connection.clone());
                match (method.as_str(), matches, connection) {
                    ("session/request_permission", true, Some(connection)) => {
                        self.on_permission_request(connection, id, &params).await;
                    }
                    ("session/request_permission", _, Some(connection)) => {
                        connection.respond(&id, json!({"outcome": {"outcome": "cancelled"}}));
                    }
                    (_, _, Some(connection)) => {
                        connection.respond_error(&id, -32601, "method not supported by Harness");
                    }
                    _ => {}
                }
            }
        }
    }

    pub(super) async fn flush_thought(&mut self) -> bool {
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        if running.thought.trim().is_empty() {
            running.thought.clear();
            return false;
        }
        let text = std::mem::take(&mut running.thought);
        let turn_id = running.turn_id.clone();
        let text_offset = running.text.len();
        let Some(turn) = self.turn_mut(&turn_id) else {
            return true;
        };
        let thought = harness_core_api::ThoughtView {
            attempt: turn.attempts.last().map_or(1, |a| a.attempt),
            text_offset,
            text,
        };
        turn.thoughts.push(thought.clone());
        let payload = serde_json::to_value(&thought).unwrap_or(Value::Null);
        self.append_event(&turn_id, "thought", payload).await;
        true
    }

    #[allow(clippy::too_many_lines)]
    pub(super) async fn on_update(&mut self, update: &Value) {
        let Some(running) = self.running.as_mut() else {
            return;
        };
        let turn_id = running.turn_id.clone();
        let agent = running.agent.clone();
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("");
        match kind {
            "agent_message_chunk" | "agent_thought_chunk" => {
                let Some(text) = update
                    .get("content")
                    .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                    .and_then(|c| c.get("text"))
                    .and_then(Value::as_str)
                else {
                    return;
                };
                let mut text = text.to_owned();
                if kind == "agent_message_chunk" {
                    let after_thought = self.flush_thought().await;
                    let Some(running) = self.running.as_mut() else {
                        return;
                    };
                    if after_thought
                        && running
                            .text
                            .chars()
                            .last()
                            .is_some_and(|c| !c.is_whitespace())
                        && text.chars().next().is_some_and(|c| !c.is_whitespace())
                    {
                        text.insert_str(0, "\n\n");
                    }
                    running.text.push_str(&text);
                    self.emit(EventKind::MessageDelta {
                        turn_id,
                        agent,
                        text,
                    });
                } else {
                    running.thought.push_str(&text);
                    self.emit(EventKind::ThoughtDelta {
                        turn_id,
                        agent,
                        text,
                    });
                }
            }
            "tool_call" | "tool_call_update" => {
                self.flush_thought().await;
                let tool = tool_view(update);
                let offset = self.running.as_ref().map_or(0, |r| r.text.len());
                let mut mark = None;
                if let Some(turn) = self.turn_mut(&turn_id) {
                    let attempt = turn.attempts.last().map_or(1, |a| a.attempt);
                    if !turn
                        .tool_calls
                        .iter()
                        .any(|t| t.tool_call_id == tool.tool_call_id)
                    {
                        mark = Some(ToolMark {
                            tool_call_id: tool.tool_call_id.clone(),
                            attempt: Some(attempt),
                            text_offset: Some(offset),
                            steer: None,
                        });
                    }
                    store::merge_tool(&mut turn.tool_calls, tool.clone());
                }
                let mut payload = serde_json::to_value(&tool).unwrap_or(Value::Null);
                if let (Some(mark), Some(object)) = (&mark, payload.as_object_mut()) {
                    object.insert("attempt".into(), json!(mark.attempt));
                    object.insert("textOffset".into(), json!(mark.text_offset));
                }
                if let Some(mark) = mark {
                    self.tool_marks
                        .entry(turn_id.clone())
                        .or_default()
                        .push(mark);
                }
                self.append_event(&turn_id, "tool_call", payload).await;
                let guard = crate::agy_delegate::builtin_spawn(&tool.title, None)
                    .map(|_| (turn_id.clone(), agent.clone(), tool.clone()));
                let event = if kind == "tool_call" {
                    EventKind::ToolCall {
                        turn_id,
                        agent,
                        tool,
                    }
                } else {
                    EventKind::ToolCallUpdated {
                        turn_id,
                        agent,
                        tool,
                    }
                };
                self.emit(event);
                if let Some((turn_id, agent, tool)) = guard {
                    self.guard_agy_spawn(&turn_id, &agent, &tool).await;
                }
            }
            "current_mode_update" | "config_option_update" => {
                self.on_agent_option_update(&agent, update).await;
            }
            "plan" | "usage_update" => self.on_progress_update(&turn_id, &agent, update).await,
            _ => {}
        }
    }

    /// The agent changed its own mode/options during a turn (e.g. leaving plan mode). That becomes the
    /// session's choice too, otherwise the next turn would switch the agent back.
    pub(super) async fn on_agent_option_update(&mut self, agent: &AgentId, update: &Value) {
        let changed = self
            .ctx
            .record_advertised(agent, |a| a.merge_update(update))
            .await;
        let mut followed = Vec::new();
        if let Some(mode) = update
            .get("currentModeId")
            .and_then(Value::as_str)
            .filter(|_| {
                update.get("sessionUpdate").and_then(Value::as_str) == Some("current_mode_update")
            })
        {
            if let Some(applied) = self.handles.get_mut(agent).and_then(|h| h.applied.as_mut()) {
                applied.mode = Some(mode.to_owned());
            }
            if self
                .prefs
                .get(agent)
                .is_some_and(|p| p.mode.as_ref().is_some_and(|m| m != mode))
            {
                followed.push(PrefChange::Mode(mode.to_owned()));
            }
        }
        for change in followed {
            if let Some(prefs) = self.prefs.get_mut(agent) {
                change.apply(prefs);
            }
            let (session, stored_agent) = (self.row.id.clone(), agent.clone());
            let (kind, key, value) = change.row();
            let (key, value) = (key.to_owned(), value.to_owned());
            let result = self
                .ctx
                .store
                .run(move |c| store::save_pref(c, &session, &stored_agent, (kind, &key, &value)))
                .await;
            self.log_store(result);
        }
        if changed {
            self.emit(EventKind::SessionOptionsChanged {
                agent: agent.clone(),
            });
        }
    }
}
