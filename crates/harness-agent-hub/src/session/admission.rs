use super::{
    drive_setup, format_utc, json, now_secs, take_before_snapshot, ActorMsg, AgentErrorInfo,
    AgentId, Arc, EventKind, Handle, PauseReason, PrefPlan, QuotaState, Ready, RpcError, Running,
    SessionActor, SwitchReason, TurnAttempt, TurnId, TurnOutcome, TurnStatus,
};

impl SessionActor {
    pub(super) async fn admit_next(&mut self) {
        if self.running.is_some() || self.paused.is_some() || self.steering.is_some() {
            return;
        }
        if let Some(turn_id) = self.queue.pop_front() {
            self.start_turn(turn_id).await;
        }
    }

    pub(super) async fn on_wake(&mut self) {
        if self.running.is_none() {
            if let Some(paused) = self.paused.take() {
                self.start_turn(paused).await;
            }
        }
    }

    /// Picks the agent for a turn: explicit request, else sticky, else fallback order (spec §6.4).
    pub(super) fn select_agent(&self, turn_id: &TurnId) -> Option<AgentId> {
        // Idempotency (spec §6.4): within one failover round an agent runs the turn at most once.
        // A pause ends the round, so a manual swap or a reset may retry an agent that already tried.
        let base = self.round_base.get(turn_id).copied().unwrap_or(0);
        let tried: Vec<AgentId> = self
            .turn(turn_id)
            .map(|t| {
                t.attempts
                    .iter()
                    .skip(base)
                    .map(|a| a.agent.clone())
                    .collect()
            })
            .unwrap_or_default();
        let order = self.ctx.fallback_order();
        let input = crate::selector::Selection {
            requested: self.requested.get(turn_id),
            sticky: &self.row.active_agent,
            order: &order,
            tried: &tried,
        };
        // 修复：设置里关闭的 agent（如 Codex）仍留在 hub 的 fallback 顺序中，额度用尽时会被自动切换过去；
        // 关闭的 agent 只隐藏在 UI 里，hub 并不知道。依据设置 agents.<id>.enabled，failover 跳过它们。
        let disabled = &self.ctx.config.disabled_agents;
        let usable = |agent: &AgentId| {
            (agent == &self.row.active_agent || !disabled.contains(agent))
                && self.ctx.is_installed(agent)
                && self.ctx.quota.is_usable(agent)
        };
        self.ctx.config.agent_selector.0.select(&input, &usable)
    }

    pub(super) async fn set_sticky(&mut self, agent: &AgentId, reason: SwitchReason) {
        if &self.row.active_agent == agent {
            return;
        }
        let from = self.row.active_agent.clone();
        let message = match &reason {
            SwitchReason::Manual => format!(
                "switched from {} to {}",
                self.ctx.display_name(&from),
                self.ctx.display_name(agent)
            ),
            SwitchReason::QuotaExhausted { resets_at } => format!(
                "switched from {} to {}: {} limit reached ({})",
                self.ctx.display_name(&from),
                self.ctx.display_name(agent),
                self.ctx.display_name(&from),
                resets_at.map_or_else(
                    || "reset time unknown".to_owned(),
                    |t| format!("resets at {}", format_utc(t))
                )
            ),
            SwitchReason::Unavailable => format!(
                "switched from {} to {}: {} is not available",
                self.ctx.display_name(&from),
                self.ctx.display_name(agent),
                self.ctx.display_name(&from)
            ),
        };
        self.row.active_agent = agent.clone();
        self.touch().await;
        self.emit(EventKind::AgentSwitched {
            from: Some(from),
            to: agent.clone(),
            reason,
            message,
        });
    }

    pub(super) async fn start_turn(&mut self, turn_id: TurnId) {
        let Some(agent) = self.select_agent(&turn_id) else {
            self.pause(turn_id).await;
            return;
        };
        if self.requested.remove(&turn_id).is_some() {
            self.set_sticky(&agent, SwitchReason::Manual).await;
        } else if agent != self.row.active_agent {
            let previous = self.row.active_agent.clone();
            let reason = match self.ctx.quota.get(&previous) {
                QuotaState::Exhausted { resets_at, .. } => {
                    SwitchReason::QuotaExhausted { resets_at }
                }
                QuotaState::Ok => SwitchReason::Unavailable,
            };
            self.note_failover(&turn_id, &previous, &agent).await;
            self.set_sticky(&agent, reason).await;
        }
        let attempt_no = self.turn(&turn_id).map_or(1, |t| {
            u32::try_from(t.attempts.len()).unwrap_or(u32::MAX) + 1
        });
        let attempt = TurnAttempt {
            attempt: attempt_no,
            agent: agent.clone(),
            status: "running".into(),
            partial_text: String::new(),
            error: None,
            error_class: None,
        };
        if let Some(turn) = self.turn_mut(&turn_id) {
            turn.status = TurnStatus::Running;
            turn.agent = Some(agent.clone());
            turn.attempts.push(attempt.clone());
        }
        self.persist_turn(&turn_id).await;
        self.persist_attempt(&turn_id, attempt).await;
        self.emit(EventKind::TurnStarted {
            turn_id: turn_id.clone(),
            agent: agent.clone(),
            attempt: attempt_no,
        });
        self.next_key += 1;
        let key = self.next_key;
        let turn_id_for_snapshot = turn_id.clone();
        let code_mode = self.turn(&turn_id).is_some_and(|turn| {
            turn.context
                .iter()
                .any(|item| matches!(item, harness_core_api::ContextItem::CodeMode))
        });
        self.running = Some(Running {
            turn_id,
            agent: agent.clone(),
            key,
            connection: None,
            native: None,
            text: String::new(),
            thought: String::new(),
            cancelling: false,
            prompt_sent: false,
            autonomous: None,
        });
        // 快照只在第一次尝试前拍（failover 重跑保留同一 before，spec §15.2）。
        let snapshot =
            self.ctx.config.turn_snapshots && self.snapshotted.insert(turn_id_for_snapshot.clone());
        self.spawn_driver(
            key,
            agent,
            snapshot.then_some(turn_id_for_snapshot),
            code_mode,
        );
    }

    pub(super) async fn pause(&mut self, turn_id: TurnId) {
        let agents = self
            .ctx
            .fallback_order()
            .into_iter()
            .map(|agent| {
                let resets_at = match self.ctx.quota.get(&agent) {
                    QuotaState::Exhausted { resets_at, .. } => resets_at,
                    QuotaState::Ok => None,
                };
                (agent, resets_at)
            })
            .collect();
        let attempts = self.turn(&turn_id).map_or(0, |t| t.attempts.len());
        self.round_base.insert(turn_id.clone(), attempts);
        if let Some(turn) = self.turn_mut(&turn_id) {
            turn.status = TurnStatus::Paused;
        }
        self.persist_turn(&turn_id).await;
        self.emit(EventKind::TurnPaused {
            turn_id: turn_id.clone(),
            reason: PauseReason::AllAgentsExhausted { agents },
        });
        self.paused = Some(turn_id);
        if let Some(reset) = self.ctx.quota.earliest_reset() {
            let tx = self.tx.clone();
            let wait = u64::try_from(reset - now_secs()).unwrap_or(0) + 1;
            tokio::spawn(async move {
                // Product rule (spec §6.4): resume a paused turn when the earliest known reset passes.
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                let _ = tx.send(ActorMsg::Wake);
            });
        }
    }

    // ---- driver: process + native session setup ---------------------------------------------------

    pub(super) fn spawn_driver(
        &self,
        key: u64,
        agent: AgentId,
        snapshot: Option<TurnId>,
        code_mode: bool,
    ) {
        let ctx = Arc::clone(&self.ctx);
        let tx = self.tx.clone();
        let handle = self
            .handles
            .get(&agent)
            .map(|h| (h.native.clone(), h.generation));
        let plan = PrefPlan {
            own: self.prefs.get(&agent).cloned().unwrap_or_default(),
            defaults: self.ctx.config.agent_defaults.get(&agent).cloned(),
            applied: self.handles.get(&agent).and_then(|h| h.applied.clone()),
        };
        let cwd = self.row.workspace_path.clone();
        let session = self.row.id.clone();
        tokio::spawn(async move {
            if let Some(turn_id) = snapshot {
                // Before the prompt is sent (it is sent only after Ready), so agent edits cannot precede it.
                take_before_snapshot(&ctx, &turn_id, &cwd).await;
            }
            let result =
                drive_setup(&ctx, &tx, &agent, handle, &cwd, plan, &session, code_mode).await;
            let _ = tx.send(ActorMsg::Ready(key, result));
        });
    }

    pub(super) async fn on_ready(&mut self, key: u64, result: Result<Ready, AgentErrorInfo>) {
        let Some(running) = self.running.as_ref().filter(|r| r.key == key) else {
            if let Ok(ready) = result {
                // Stale setup (turn already ended): give the process back.
                self.ctx
                    .pool
                    .release(&ready.agent, ready.connection.generation())
                    .await;
            }
            return;
        };
        let turn_id = running.turn_id.clone();
        let agent = running.agent.clone();
        let ready = match result {
            Ok(ready) => ready,
            Err(error) => {
                self.on_attempt_failed(&turn_id, &agent, error, crate::failure::Phase::Setup)
                    .await;
                return;
            }
        };
        let generation = ready.connection.generation();
        let previous_seq = if ready.fresh {
            0
        } else {
            self.handles.get(&agent).map_or(0, |h| h.last_seq)
        };
        self.handles.insert(
            agent.clone(),
            Handle {
                native: ready.native.clone(),
                last_seq: previous_seq,
                generation: Some(generation),
                applied: Some(ready.applied.clone()),
            },
        );
        self.persist_handle(&agent).await;
        if let Some(result) = &ready.advertised {
            if self
                .ctx
                .record_advertised(&agent, |a| a.merge_response(result))
                .await
            {
                self.emit(EventKind::SessionOptionsChanged {
                    agent: agent.clone(),
                });
            }
        }
        let cancelling = self.running.as_ref().is_some_and(|r| r.cancelling);
        if let Some(running) = self.running.as_mut() {
            running.connection = Some(Arc::clone(&ready.connection));
            running.native = Some(ready.native.clone());
        }
        if cancelling {
            self.end_running(TurnStatus::Cancelled, None, None, TurnOutcome::Cancelled)
                .await;
            return;
        }
        let (image, embedded) = ready.connection.prompt_capabilities();
        let caps = crate::context::Caps { image, embedded };
        self.ctx.chat.set_caps(&agent, caps);
        let blocks = self
            .prompt_blocks(&turn_id, &agent, ready.fresh, previous_seq, caps)
            .await;
        let receiver = ready.connection.send_request(
            "session/prompt",
            json!({"sessionId": ready.native, "prompt": blocks}),
        );
        if let Some(running) = self.running.as_mut() {
            running.prompt_sent = true;
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = receiver.await.unwrap_or_else(|_| {
                Err(RpcError {
                    code: RpcError::EXITED,
                    message: "agent process exited".into(),
                    data: None,
                })
            });
            // Posted after the reader delivered all updates of this prompt (spec §9).
            let _ = tx.send(ActorMsg::PromptDone(key, result));
        });
    }
}
