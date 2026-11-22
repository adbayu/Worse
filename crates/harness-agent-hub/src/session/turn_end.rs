use super::{
    chat, json, now_ms, store, take_after_snapshot, AgentErrorInfo, AgentId, ErrorClass, EventKind,
    HandleRow, QuotaState, RpcError, SessionActor, TurnAttempt, TurnId, TurnOutcome, TurnStatus,
    Value,
};

impl SessionActor {
    pub(super) async fn on_prompt_done(&mut self, key: u64, result: Result<Value, RpcError>) {
        let Some(running) = self.running.as_ref().filter(|r| r.key == key) else {
            return;
        };
        let cancelling = running.cancelling;
        let turn_id = running.turn_id.clone();
        let agent = running.agent.clone();
        if let Ok(value) = &result {
            chat::record_prompt_usage(self, &turn_id, value);
        }
        match result {
            Ok(value) => {
                let stop_reason = value
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("end_turn")
                    .to_owned();
                if stop_reason == "cancelled" || cancelling {
                    self.end_running(
                        TurnStatus::Cancelled,
                        Some(stop_reason),
                        None,
                        TurnOutcome::Cancelled,
                    )
                    .await;
                } else {
                    let outcome = TurnOutcome::Completed {
                        stop_reason: stop_reason.clone(),
                    };
                    self.end_running(TurnStatus::Completed, Some(stop_reason), None, outcome)
                        .await;
                }
            }
            Err(_) if cancelling => {
                self.end_running(TurnStatus::Cancelled, None, None, TurnOutcome::Cancelled)
                    .await;
            }
            Err(error) => {
                self.on_attempt_failed(
                    &turn_id,
                    &agent,
                    error.info(),
                    crate::failure::Phase::Prompt,
                )
                .await;
            }
        }
    }

    pub(super) async fn on_attempt_failed(
        &mut self,
        turn_id: &TurnId,
        agent: &AgentId,
        error: AgentErrorInfo,
        phase: crate::failure::Phase,
    ) {
        let class = self.ctx.classify(agent, &error);
        if let ErrorClass::QuotaExhausted { resets_at } = class {
            // Spec §6.4 failover: record the partial attempt, mark the agent exhausted, re-run the same turn.
            let partial = self
                .running
                .as_ref()
                .map(|r| r.text.clone())
                .unwrap_or_default();
            self.cancel_pending_permissions();
            self.flush_thought().await;
            if let Some(running) = self.running.take() {
                if let Some(connection) = &running.connection {
                    self.ctx
                        .pool
                        .release(&running.agent, connection.generation())
                        .await;
                }
            }
            self.update_last_attempt(
                turn_id,
                "quotaExhausted",
                partial,
                Some(error.message.clone()),
                Some(class.clone()),
            )
            .await;
            if crate::agy_ops::is_agy(&self.ctx, agent)
                && crate::agy_agents::account_of(agent).is_none()
                && crate::agy_ops::rotate_on_quota(&self.ctx).await
            {
                let attempts = self.turn(turn_id).map_or(0, |t| t.attempts.len());
                self.round_base.insert(turn_id.clone(), attempts);
                self.start_turn(turn_id.clone()).await;
                return;
            }
            self.ctx
                .set_quota(
                    agent,
                    QuotaState::Exhausted {
                        resets_at,
                        reason: error.message.clone(),
                    },
                )
                .await;
            self.start_turn(turn_id.clone()).await;
            return;
        }
        // 类型化失败（§9.2.5 R20）：在 TurnFinished 之前发出，UI 据此显示错误横幅与重试提示。
        let trace = self
            .running
            .as_ref()
            .and_then(|r| r.connection.as_ref())
            .and_then(|c| c.stderr_tail());
        let failure = crate::failure::classify(&error, &class, phase, trace);
        self.emit(EventKind::TurnFailure {
            turn_id: turn_id.clone(),
            agent: agent.clone(),
            failure,
        });
        let message = error.message.clone();
        self.end_running(
            TurnStatus::Failed,
            None,
            Some(message.clone()),
            TurnOutcome::Failed {
                error: message,
                class,
            },
        )
        .await;
    }

    pub(super) async fn end_running(
        &mut self,
        status: TurnStatus,
        stop_reason: Option<String>,
        error: Option<String>,
        outcome: TurnOutcome,
    ) {
        self.cancel_pending_permissions();
        self.flush_thought().await;
        // agy 把超时的命令转到自己的后台任务里，ACP 上看不到；在回合结束前登记，交给后台任务列表跟踪。
        self.note_agy_async();
        let Some(running) = self.running.take() else {
            return;
        };
        if let Some(connection) = &running.connection {
            self.ctx
                .pool
                .release(&running.agent, connection.generation())
                .await;
        }
        let attempt_status = match status {
            TurnStatus::Completed => "completed",
            TurnStatus::Cancelled => "cancelled",
            _ => "failed",
        };
        let class = match &outcome {
            TurnOutcome::Failed { class, .. } => Some(class.clone()),
            _ => None,
        };
        self.update_last_attempt(
            &running.turn_id,
            attempt_status,
            running.text.clone(),
            error.clone(),
            class,
        )
        .await;
        let marks = self.tool_marks.remove(&running.turn_id).unwrap_or_default();
        if let Some(turn) = self.turn_mut(&running.turn_id) {
            let last_attempt = turn.attempts.last().map_or(1, |a| a.attempt);
            turn.segments = store::build_segments(&running.text, last_attempt, &marks);
            turn.assistant_text = running.text;
        }
        if status == TurnStatus::Completed || status == TurnStatus::Cancelled {
            let seq = self.turn(&running.turn_id).map_or(0, |t| t.seq);
            if let Some(handle) = self.handles.get_mut(&running.agent) {
                handle.last_seq = handle.last_seq.max(seq);
            }
            self.persist_handle(&running.agent).await;
        }
        self.finish(&running.turn_id, status, stop_reason, error, outcome)
            .await;
        self.admit_next().await;
    }

    pub(super) async fn finish(
        &mut self,
        turn_id: &TurnId,
        status: TurnStatus,
        stop_reason: Option<String>,
        error: Option<String>,
        outcome: TurnOutcome,
    ) {
        let agent = self.turn(turn_id).and_then(|t| t.agent.clone());
        if self.snapshotted.remove(turn_id) {
            // After-snapshot before TurnFinished: a UI that sees TurnFinished can ask turn_diff at once (§15.2).
            take_after_snapshot(&self.ctx, turn_id, &self.row.workspace_path).await;
        }
        self.tool_marks.remove(turn_id);
        if let Some(turn) = self.turn_mut(turn_id) {
            turn.status = status;
            turn.stop_reason = stop_reason;
            turn.error = error;
            turn.finished_at = Some(now_ms());
        }
        self.persist_turn(turn_id).await;
        self.touch().await;
        if status == TurnStatus::Completed {
            self.capture_episode();
        }
        self.emit(EventKind::TurnFinished {
            turn_id: turn_id.clone(),
            agent,
            outcome,
        });
    }

    pub(super) async fn note_failover(&mut self, turn_id: &TurnId, from: &AgentId, to: &AgentId) {
        let resets_at = match self.ctx.quota.get(from) {
            QuotaState::Exhausted { resets_at, .. } => resets_at,
            QuotaState::Ok => None,
        };
        self.append_event(
            turn_id,
            "failover",
            json!({"from": from, "to": to, "resetsAt": resets_at}),
        )
        .await;
    }

    pub(super) async fn update_last_attempt(
        &mut self,
        turn_id: &TurnId,
        status: &str,
        partial: String,
        error: Option<String>,
        class: Option<ErrorClass>,
    ) {
        let Some(attempt) = self.turn_mut(turn_id).and_then(|t| t.attempts.last_mut()) else {
            return;
        };
        status.clone_into(&mut attempt.status);
        attempt.partial_text = partial;
        attempt.error = error;
        attempt.error_class = class;
        let attempt = attempt.clone();
        self.persist_attempt(turn_id, attempt).await;
    }

    pub(super) async fn persist_attempt(&self, turn_id: &TurnId, attempt: TurnAttempt) {
        let turn_id = turn_id.clone();
        self.log_store(
            self.ctx
                .store
                .run(move |c| store::upsert_attempt(c, &turn_id, &attempt))
                .await,
        );
    }

    pub(super) async fn persist_turn(&self, turn_id: &TurnId) {
        let Some(turn) = self.turn(turn_id).cloned() else {
            return;
        };
        self.log_store(
            self.ctx
                .store
                .run(move |c| store::update_turn(c, &turn))
                .await,
        );
    }

    pub(super) async fn persist_handle(&self, agent: &AgentId) {
        let Some(handle) = self.handles.get(agent) else {
            return;
        };
        let row = HandleRow {
            agent: agent.clone(),
            native: handle.native.clone(),
            last_seq: handle.last_seq,
        };
        let session_id = self.row.id.clone();
        self.log_store(
            self.ctx
                .store
                .run(move |c| store::upsert_handle(c, &session_id, &row))
                .await,
        );
    }

    pub(super) async fn append_event(&self, turn_id: &TurnId, kind: &'static str, payload: Value) {
        let turn_id = turn_id.clone();
        self.log_store(
            self.ctx
                .store
                .run(move |c| store::append_turn_event(c, &turn_id, kind, &payload))
                .await,
        );
    }

    pub(super) async fn touch(&mut self) {
        self.row.updated_at = now_ms();
        let row = self.row.clone();
        let result = self
            .ctx
            .store
            .run(move |c| {
                store::update_session(c, &row)?;
                store::summary(c, &row)
            })
            .await;
        match result {
            Ok(summary) => self.emit(EventKind::SessionUpdated { summary }),
            Err(error) => tracing::warn!(%error, "session update failed"),
        }
    }

    pub(super) fn log_store(&self, result: Result<(), crate::HubError>) {
        if let Err(error) = result {
            tracing::warn!(session = %self.row.id, %error, "store write failed");
        }
    }
}
