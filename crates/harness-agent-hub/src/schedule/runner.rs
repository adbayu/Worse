use super::{
    next_run, now_ms, run_loop, store_chat, ApiError, ApiResult, Arc, CreateSession, EventKind,
    Hub, ScheduleRun, ScheduleTarget, ScheduledPrompt, SendMessage, SessionCmd, TurnOrigin,
};

impl Hub {
    /// Sends the prompt and records `last_run`/`last_result` (and `next_run` for a due run).
    pub(super) async fn run_and_record(
        &self,
        mut schedule: ScheduledPrompt,
        due: bool,
    ) -> ApiResult<ScheduleRun> {
        let now = now_ms();
        let outcome = self.run_schedule(&schedule).await;
        schedule.last_run = Some(now);
        match &outcome {
            Ok(_) => schedule.last_result = Some("sent".into()),
            Err(error) => {
                schedule.last_result = Some(error.to_string());
                if matches!(error, ApiError::NotFound(_)) {
                    // 目标会话已不存在：停用该计划，避免每个周期重复失败（spec §9.2.1）。
                    schedule.enabled = false;
                    schedule.next_run = None;
                }
            }
        }
        if due && schedule.enabled {
            schedule.next_run = next_run(&schedule.kind, now);
        }
        let stored = schedule.clone();
        self.ctx
            .store
            .run(move |c| store_chat::save_schedule(c, &stored))
            .await?;
        self.ctx.events.emit(None, EventKind::SchedulesChanged);
        outcome
    }

    pub(super) async fn run_schedule(&self, schedule: &ScheduledPrompt) -> ApiResult<ScheduleRun> {
        let session_id = match &schedule.target {
            ScheduleTarget::Session { session_id } => {
                self.load_row(session_id).await?;
                session_id.clone()
            }
            ScheduleTarget::NewSession {
                workspace_path,
                agent,
            } => {
                self.do_create_session(CreateSession {
                    workspace_path: workspace_path.clone(),
                    workspace_identity: None,
                    title: Some(schedule.title.clone()),
                    agent: agent.clone(),
                    worktree: None,
                })
                .await?
                .id
            }
        };
        let request = SendMessage {
            session_id: session_id.clone(),
            text: schedule.prompt.clone(),
            agent: None,
            client_message_id: None,
            context: Vec::new(),
        };
        let origin = TurnOrigin {
            schedule_id: schedule.id.clone(),
            schedule_title: schedule.title.clone(),
            background: None,
        };
        self.unarchive_for_send(&session_id).await?;
        let accepted = self
            .ask(&session_id, |reply| {
                SessionCmd::SendScheduled(request, origin, reply)
            })
            .await?;
        Ok(ScheduleRun {
            session_id,
            turn_id: accepted.turn_id,
        })
    }

    /// Starts, wakes or stops the lazy task to match the switch and the enabled schedules.
    pub(crate) async fn ensure_scheduler(&self) {
        let wanted = self.check_open().is_ok()
            && self.do_schedules_enabled().await.unwrap_or(false)
            && self
                .do_list_schedules()
                .await
                .is_ok_and(|list| list.iter().any(|s| s.enabled));
        let scheduler = &self.scheduler;
        if !wanted {
            scheduler.stop();
            return;
        }
        let Ok(mut slot) = scheduler.task.lock() else {
            return;
        };
        if slot.as_ref().is_some_and(|t| !t.is_finished()) {
            scheduler.wake.notify_one();
            return;
        }
        let weak = self.me.clone();
        let wake = Arc::clone(&scheduler.wake);
        *slot = Some(tokio::spawn(run_loop(weak, wake)));
    }
}
