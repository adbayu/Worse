use super::{
    json, new_id, now_ms, store, AgentId, ApiError, ApiResult, EventKind, PermissionDecision,
    PermissionRequestId, PrefChange, QuotaState, SendAccepted, SendMessage, SessionActor,
    SessionCmd, SwitchReason, TurnId, TurnOutcome, TurnStatus, TurnView,
};

impl SessionActor {
    pub(super) async fn on_command(&mut self, command: SessionCmd) {
        match command {
            SessionCmd::Send(request, reply) => {
                let result = self.on_send(request, None).await;
                let _ = reply.send(result);
                self.admit_next().await;
            }
            SessionCmd::SetAgent(agent, reply) => {
                let result = self.on_set_agent(agent).await;
                let _ = reply.send(result);
            }
            SessionCmd::Stop(reply) => {
                self.on_stop().await;
                let _ = reply.send(Ok(()));
            }
            SessionCmd::CancelQueued(turn_id, reply) => {
                let result = self.on_cancel_queued(&turn_id).await;
                let _ = reply.send(result);
            }
            SessionCmd::RespondPermission(request_id, option_id, reply) => {
                let result = self.on_respond_permission(&request_id, &option_id).await;
                let _ = reply.send(result);
            }
            SessionCmd::Snapshot(reply) => {
                let _ = reply.send(Ok(self.full_snapshot().await));
            }
            SessionCmd::SendScheduled(request, origin, reply) => {
                let result = self.on_send(request, Some(origin)).await;
                let _ = reply.send(result);
                self.admit_next().await;
            }
            SessionCmd::Rename(title, reply) => {
                let result = self.on_rename(&title).await;
                let _ = reply.send(result);
            }
            SessionCmd::Rewind(command, reply) => {
                let result = self.on_rewind(command).await;
                let _ = reply.send(result);
                self.admit_next().await;
            }
            SessionCmd::ApplyFiles(request, reply) => {
                let result = self.on_apply_files(request).await;
                let _ = reply.send(result);
            }
            SessionCmd::EditQueued(turn_id, text, reply) => {
                let result = self.on_edit_queued(&turn_id, text).await;
                let _ = reply.send(result);
            }
            SessionCmd::ReorderQueued(turn_ids, reply) => {
                let _ = reply.send(self.on_reorder_queued(turn_ids));
            }
            SessionCmd::RunQueuedNow(turn_id, reply) => {
                self.on_run_queued_now(&turn_id, reply);
                self.admit_next().await;
            }
            SessionCmd::Steer(request, reply) => {
                self.on_steer(request, reply).await;
                self.admit_next().await;
            }
            SessionCmd::StopBackgroundTask(task_id, reply) => {
                let _ = reply.send(self.on_stop_background_task(&task_id).await);
            }
            SessionCmd::Shutdown(reply) => {
                let _ = reply.send(());
            }
            // Handled in `run` (it ends the loop); kept for exhaustiveness.
            SessionCmd::Retire(reply) => {
                let _ = reply.send(Ok(()));
            }
            SessionCmd::Prefs(reply) => {
                let agent = self.row.active_agent.clone();
                let prefs = self.prefs.get(&agent).cloned().unwrap_or_default();
                let _ = reply.send(Ok((agent, prefs)));
            }
            SessionCmd::SetPref(change, reply) => {
                let result = self.on_set_pref(change).await;
                let _ = reply.send(result);
            }
        }
    }

    /// Stores a mode/model/config choice for the active agent. Never touches a running turn: the driver
    /// applies it before the next prompt (phase-9-parity.md §9.1.5).
    pub(super) async fn on_set_pref(&mut self, change: PrefChange) -> ApiResult<()> {
        let agent = self.row.active_agent.clone();
        change.validate(&self.ctx.advertised(&agent).await)?;
        let mut prefs = self.prefs.get(&agent).cloned().unwrap_or_default();
        change.apply(&mut prefs);
        if self.prefs.get(&agent) == Some(&prefs) {
            return Ok(());
        }
        let (session, stored_agent) = (self.row.id.clone(), agent.clone());
        let (kind, key, value) = change.row();
        let (kind, key, value) = (kind, key.to_owned(), value.to_owned());
        self.ctx
            .store
            .run(move |c| store::save_pref(c, &session, &stored_agent, (kind, &key, &value)))
            .await?;
        self.prefs.insert(agent.clone(), prefs);
        self.emit(EventKind::SessionOptionsChanged { agent });
        Ok(())
    }

    pub(super) async fn on_send(
        &mut self,
        request: SendMessage,
        origin: Option<harness_core_api::TurnOrigin>,
    ) -> ApiResult<SendAccepted> {
        if request.text.trim().is_empty() {
            return Err(ApiError::InvalidArgument("message text is empty".into()));
        }
        if let Some(client_id) = &request.client_message_id {
            if let Some(turn_id) = self.client_ids.get(client_id) {
                let turn = self
                    .turn(turn_id)
                    .ok_or_else(|| ApiError::NotFound(turn_id.to_string()))?;
                return Ok(SendAccepted {
                    turn_id: turn.id.clone(),
                    seq: turn.seq,
                    position: self.position_of(&turn.id),
                });
            }
        }
        if let Some(agent) = &request.agent {
            self.ctx.require_available(agent)?;
        }
        crate::context::validate(
            &self.ctx,
            &self.row.workspace_path,
            &self.row.id,
            &request.context,
        )
        .await?;
        let turn_id = TurnId::new(new_id("turn"));
        let context =
            crate::context::store_images(&self.ctx.config.home, &turn_id, request.context.clone())
                .await?;
        let seq = self.turns.last().map_or(1, |t| t.seq + 1);
        let turn = TurnView {
            id: turn_id,
            seq,
            user_text: request.text.clone(),
            agent: None,
            assistant_text: String::new(),
            status: TurnStatus::Queued,
            stop_reason: None,
            error: None,
            attempts: Vec::new(),
            tool_calls: Vec::new(),
            permissions: Vec::new(),
            created_at: now_ms(),
            finished_at: None,
            segments: Vec::new(),
            context,
            plan: Vec::new(),
            usage: None,
            context_usage: None,
            origin,
            thoughts: Vec::new(),
        };
        let session_id = self.row.id.clone();
        let stored = turn.clone();
        let client_id = request.client_message_id.clone();
        self.ctx
            .store
            .run(move |c| store::insert_turn(c, &session_id, &stored, client_id.as_deref()))
            .await?;
        if let Some(client_id) = request.client_message_id {
            self.client_ids.insert(client_id, turn.id.clone());
        }
        if let Some(agent) = request.agent {
            self.requested.insert(turn.id.clone(), agent);
        }
        let ahead = u32::from(self.running.is_some() || self.paused.is_some())
            + u32::try_from(self.queue.len()).unwrap_or(u32::MAX);
        self.queue.push_back(turn.id.clone());
        let accepted = SendAccepted {
            turn_id: turn.id.clone(),
            seq,
            position: ahead,
        };
        self.emit(EventKind::TurnQueued {
            turn_id: turn.id.clone(),
            seq,
            user_text: turn.user_text.clone(),
            position: ahead,
            background: false,
            images: harness_core_api::ContextItem::image_paths(&turn.context),
        });
        self.turns.push(turn);
        self.touch().await;
        Ok(accepted)
    }

    pub(super) async fn on_set_agent(&mut self, agent: AgentId) -> ApiResult<()> {
        self.ctx.require_available(&agent)?;
        // A manual swap is a deliberate retry: clear the exhaustion mark.
        if !self.ctx.quota.is_usable(&agent) {
            self.ctx.set_quota(&agent, QuotaState::Ok).await;
        }
        self.set_sticky(&agent, SwitchReason::Manual).await;
        if let Some(paused) = self.paused.take() {
            self.requested.insert(paused.clone(), agent);
            self.start_turn(paused).await;
        }
        Ok(())
    }

    pub(super) async fn on_stop(&mut self) {
        if let Some(running) = self.running.as_mut() {
            running.cancelling = true;
            if running.prompt_sent {
                if let (Some(connection), Some(native)) = (&running.connection, &running.native) {
                    connection.notify("session/cancel", json!({"sessionId": native}));
                }
            }
            self.cancel_pending_permissions();
            return;
        }
        if let Some(paused) = self.paused.take() {
            self.finish(
                &paused,
                TurnStatus::Cancelled,
                None,
                None,
                TurnOutcome::Cancelled,
            )
            .await;
            self.admit_next().await;
        }
    }

    pub(super) async fn on_cancel_queued(&mut self, turn_id: &TurnId) -> ApiResult<()> {
        let Some(index) = self.queue.iter().position(|t| t == turn_id) else {
            return Err(ApiError::NotFound(format!("queued turn {turn_id}")));
        };
        self.queue.remove(index);
        self.finish(
            turn_id,
            TurnStatus::Cancelled,
            None,
            None,
            TurnOutcome::Cancelled,
        )
        .await;
        Ok(())
    }

    pub(super) async fn on_respond_permission(
        &mut self,
        request_id: &PermissionRequestId,
        option_id: &str,
    ) -> ApiResult<()> {
        let Some(index) = self
            .pending
            .iter()
            .position(|p| &p.view.request_id == request_id)
        else {
            return Err(ApiError::AlreadyResolved(request_id.to_string()));
        };
        let Some(option) = self.pending[index]
            .view
            .options
            .iter()
            .find(|o| o.option_id == option_id)
            .cloned()
        else {
            return Err(ApiError::InvalidArgument(format!(
                "unknown option {option_id}"
            )));
        };
        let pending = self.pending.remove(index);
        pending.connection.respond(
            &pending.rpc_id,
            json!({"outcome": {"outcome": "selected", "optionId": option.option_id}}),
        );
        let decision = PermissionDecision::Selected {
            option_id: option.option_id,
            kind: option.kind,
        };
        self.record_permission(&pending.view, decision).await;
        Ok(())
    }

    pub(super) async fn on_shutdown(&mut self) {
        self.release_task_leases().await;
        self.abandon_steer();
        self.on_stop().await;
        if let Some(running) = self.running.take() {
            let turn_id = running.turn_id.clone();
            if let Some(connection) = &running.connection {
                self.ctx
                    .pool
                    .release(&running.agent, connection.generation())
                    .await;
            }
            self.finish(
                &turn_id,
                TurnStatus::Cancelled,
                None,
                Some("hub shut down".into()),
                TurnOutcome::Cancelled,
            )
            .await;
        }
        let queued: Vec<TurnId> = self.queue.drain(..).chain(self.paused.take()).collect();
        for turn_id in queued {
            self.finish(
                &turn_id,
                TurnStatus::Interrupted,
                None,
                None,
                TurnOutcome::Cancelled,
            )
            .await;
        }
    }
}
