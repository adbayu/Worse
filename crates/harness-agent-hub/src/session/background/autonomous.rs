use super::{
    is_turn_content, new_id, now_ms, store, text, watch, ActorMsg, AgentId, Autonomous, EventKind,
    Running, SessionActor, ToolCallStatus, TurnAttempt, TurnId, TurnOrigin, TurnOutcome,
    TurnStatus, TurnView, Value, IDLE_END, MAX_TASKS, TERMINAL,
};

impl SessionActor {
    pub(super) fn trim_tasks(&mut self) {
        while self.tasks.len() > MAX_TASKS {
            let Some(index) = self.tasks.iter().position(|t| !t.view.is_running()) else {
                return;
            };
            self.tasks.remove(index);
        }
    }

    pub(super) async fn on_task_state(
        &mut self,
        agent: &AgentId,
        id: &str,
        state: String,
        summary: Option<String>,
        update: &Value,
    ) {
        let Some(task) = self.tasks.iter_mut().find(|t| t.view.id == id) else {
            return;
        };
        if !task.view.is_running() {
            return;
        }
        if let Some(file) = text(update, "outputFilePath") {
            task.view.output_file = Some(file);
        }
        if let Some(summary) = summary {
            task.view.summary = summary;
        }
        let terminal = TERMINAL.contains(&state.as_str());
        task.view.state = state;
        let lease = if terminal {
            task.view.finished_at = Some(now_ms());
            task.lease.take()
        } else {
            None
        };
        if let Some(lease) = lease {
            self.release_lease(agent, lease).await;
        }
        self.emit_tasks();
    }

    pub(super) async fn start_autonomous(
        &mut self,
        agent: AgentId,
        native: &str,
        task_id: String,
        description: &str,
        idle: bool,
    ) {
        if self.running.is_some() || self.steering.is_some() || self.paused.is_some() {
            return;
        }
        let Some(connection) = self.lease_connection(&agent).await else {
            return;
        };
        let turn_id = TurnId::new(new_id("turn"));
        let seq = self.turns.last().map_or(1, |t| t.seq + 1);
        let user_text = if idle {
            "Agent continued on its own (background work finished)".to_owned()
        } else if description.trim().is_empty() {
            "Background task finished".to_owned()
        } else {
            format!("Background task finished: {}", description.trim())
        };
        let attempt = TurnAttempt {
            attempt: 1,
            agent: agent.clone(),
            status: "running".into(),
            partial_text: String::new(),
            error: None,
            error_class: None,
        };
        let turn = TurnView {
            id: turn_id.clone(),
            seq,
            user_text,
            agent: Some(agent.clone()),
            assistant_text: String::new(),
            status: TurnStatus::Running,
            stop_reason: None,
            error: None,
            attempts: vec![attempt.clone()],
            tool_calls: Vec::new(),
            permissions: Vec::new(),
            created_at: now_ms(),
            finished_at: None,
            segments: Vec::new(),
            context: Vec::new(),
            plan: Vec::new(),
            usage: None,
            context_usage: None,
            origin: Some(TurnOrigin {
                schedule_id: String::new(),
                schedule_title: String::new(),
                background: Some(task_id.clone()),
            }),
            thoughts: Vec::new(),
        };
        let session_id = self.row.id.clone();
        let stored = turn.clone();
        if let Err(error) = self
            .ctx
            .store
            .run(move |c| store::insert_turn(c, &session_id, &stored, None))
            .await
        {
            tracing::warn!(session = %self.row.id, %error, "autonomous turn insert failed");
            self.ctx.pool.release(&agent, connection.generation()).await;
            return;
        }
        self.emit(EventKind::TurnQueued {
            turn_id: turn_id.clone(),
            seq,
            user_text: turn.user_text.clone(),
            position: 0,
            background: true,
            images: Vec::new(),
        });
        self.turns.push(turn);
        self.persist_attempt(&turn_id, attempt).await;
        self.emit(EventKind::TurnStarted {
            turn_id: turn_id.clone(),
            agent: agent.clone(),
            attempt: 1,
        });
        self.next_key += 1;
        let watch = watch(&self.tx, &agent, &connection);
        self.running = Some(Running {
            turn_id,
            agent,
            key: self.next_key,
            connection: Some(connection),
            native: Some(native.to_owned()),
            text: String::new(),
            thought: String::new(),
            cancelling: false,
            prompt_sent: true,
            autonomous: Some(Autonomous {
                task_id,
                _watch: watch,
                idle,
                stamp: 0,
            }),
        });
        self.touch().await;
    }

    pub(crate) async fn on_unprompted_update(&mut self, native: &str, update: &Value) {
        if self.running.is_some() || !is_turn_content(update) {
            return;
        }
        let Some(agent) = self.agent_of_native(native) else {
            return;
        };
        self.start_autonomous(agent, native, new_id("agent"), "", true)
            .await;
        if self
            .running
            .as_ref()
            .is_some_and(|r| r.native.as_deref() == Some(native))
        {
            self.on_update(update).await;
            self.arm_idle_end();
        }
    }

    pub(crate) fn arm_idle_end(&mut self) {
        let Some(running) = self.running.as_mut() else {
            return;
        };
        let key = running.key;
        let Some(autonomous) = running.autonomous.as_mut().filter(|a| a.idle) else {
            return;
        };
        autonomous.stamp += 1;
        let stamp = autonomous.stamp;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(IDLE_END).await;
            let _ = tx.send(ActorMsg::IdleEnd(key, stamp));
        });
    }

    pub(crate) async fn on_idle_end(&mut self, key: u64, stamp: u64) {
        let Some(running) = self.running.as_ref() else {
            return;
        };
        let current = running.key == key
            && running
                .autonomous
                .as_ref()
                .is_some_and(|a| a.idle && a.stamp == stamp);
        if !current {
            return;
        }
        let turn_id = running.turn_id.clone();
        let tool_running = self
            .turns
            .iter()
            .find(|t| t.id == turn_id)
            .is_some_and(|t| {
                t.tool_calls.iter().any(|c| {
                    matches!(
                        c.status,
                        Some(ToolCallStatus::Pending | ToolCallStatus::InProgress)
                    )
                })
            });
        if tool_running {
            self.arm_idle_end();
        } else {
            self.end_autonomous("completed", None).await;
        }
    }

    pub(super) async fn end_autonomous(&mut self, state: &str, summary: Option<String>) {
        let cancelling = self.running.as_ref().is_some_and(|r| r.cancelling);
        match state {
            "completed" if !cancelling => {
                let stop_reason = "end_turn".to_owned();
                let outcome = TurnOutcome::Completed {
                    stop_reason: stop_reason.clone(),
                };
                self.end_running(TurnStatus::Completed, Some(stop_reason), None, outcome)
                    .await;
            }
            "failed" if !cancelling => {
                let Some((turn_id, agent, text)) = self
                    .running
                    .as_ref()
                    .map(|r| (r.turn_id.clone(), r.agent.clone(), r.text.clone()))
                else {
                    return;
                };
                let message = summary
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| Some(text.trim().to_owned()).filter(|t| !t.is_empty()))
                    .unwrap_or_else(|| "background turn failed".into());
                let info = crate::quota::AgentErrorInfo {
                    code: None,
                    message,
                    data: None,
                };
                self.on_attempt_failed(&turn_id, &agent, info, crate::failure::Phase::Prompt)
                    .await;
            }
            _ => {
                self.end_running(
                    TurnStatus::Cancelled,
                    Some("cancelled".into()),
                    None,
                    TurnOutcome::Cancelled,
                )
                .await;
            }
        }
    }
}
