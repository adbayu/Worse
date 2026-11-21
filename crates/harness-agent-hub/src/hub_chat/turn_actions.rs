use super::{
    new_id, now_ms, store, store_chat, ApiError, ApiResult, ApplyFileChanges, ChangeDirection,
    EditResend, EventKind, FileChangePlan, FileRestoreReport, ForkOrigin, ForkSession, Hub, Resend,
    Retry, Rewind, RewindCmd, RewindResult, SendAccepted, SessionCmd, SessionId, SessionRow,
    SessionSummary, TurnId,
};

impl Hub {
    pub(crate) async fn do_fork(&self, request: ForkSession) -> ApiResult<SessionSummary> {
        self.check_open()?;
        let source = self.load_row(&request.session_id).await?;
        let up_to = request.up_to_turn.clone();
        let source_id = source.id.clone();
        let now = now_ms();
        let row = SessionRow {
            id: SessionId::new(new_id("ses")),
            title: request
                .title
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| format!("{} (fork)", source.title)),
            workspace_path: source.workspace_path.clone(),
            workspace_key: source.workspace_key.clone(),
            active_agent: source.active_agent.clone(),
            created_at: now,
            updated_at: now,
        };
        let summary = self
            .ctx
            .store
            .run(move |c| {
                let turns = store::load_turns(c, &source_id)?;
                let finished = |t: &harness_core_api::TurnView| {
                    !matches!(
                        t.status,
                        harness_core_api::TurnStatus::Queued
                            | harness_core_api::TurnStatus::Running
                            | harness_core_api::TurnStatus::Paused
                    )
                };
                let seq = match &up_to {
                    Some(turn) => {
                        turns
                            .iter()
                            .find(|(t, _)| &t.id == turn)
                            .ok_or_else(|| crate::HubError::NotFound(format!("turn {turn}")))?
                            .0
                            .seq
                    }
                    None => turns
                        .iter()
                        .filter(|(t, _)| finished(t))
                        .map(|(t, _)| t.seq)
                        .max()
                        .unwrap_or(0),
                };
                store::insert_session(c, &row)?;
                store_chat::mark_read(c, &row.id, now)?;
                store_chat::copy_turns(c, &source_id, &row.id, seq, &mut || new_id("turn"))?;
                store_chat::set_fork_origin(
                    c,
                    &row.id,
                    &ForkOrigin {
                        session_id: source_id.clone(),
                        turn_seq: seq,
                    },
                )?;
                store::summary(c, &row)
            })
            .await?;
        self.ctx.events.emit(
            Some(&summary.id),
            EventKind::SessionCreated {
                summary: summary.clone(),
            },
        );
        Ok(summary)
    }

    pub(crate) async fn do_rewind(&self, request: Rewind) -> ApiResult<RewindResult> {
        let command = RewindCmd {
            turn: request.turn_id,
            restore_files: request.restore_files,
            resend: Resend::None,
        };
        let outcome = self
            .ask(&request.session_id, |reply| {
                SessionCmd::Rewind(command, reply)
            })
            .await?;
        Ok(outcome.result)
    }

    pub(super) async fn rewind_and_send(
        &self,
        session: &SessionId,
        command: RewindCmd,
    ) -> ApiResult<SendAccepted> {
        self.unarchive_for_send(session).await?;
        let outcome = self
            .ask(session, |reply| SessionCmd::Rewind(command, reply))
            .await?;
        outcome
            .accepted
            .ok_or_else(|| ApiError::Internal("rewind did not resend".into()))
    }

    pub(crate) async fn do_edit_and_resend(&self, request: EditResend) -> ApiResult<SendAccepted> {
        let command = RewindCmd {
            turn: request.turn_id,
            restore_files: request.restore_files,
            resend: Resend::Edit {
                text: request.text,
                context: request.context,
                agent: request.agent,
            },
        };
        self.rewind_and_send(&request.session_id, command).await
    }

    pub(crate) async fn do_retry(&self, request: Retry) -> ApiResult<SendAccepted> {
        let command = RewindCmd {
            turn: request.turn_id,
            restore_files: request.restore_files,
            resend: Resend::Retry {
                agent: request.agent,
            },
        };
        self.rewind_and_send(&request.session_id, command).await
    }

    pub(crate) async fn do_plan_files(
        &self,
        session: SessionId,
        turn: TurnId,
        direction: ChangeDirection,
    ) -> ApiResult<FileChangePlan> {
        self.check_open()?;
        let row = self.load_row(&session).await?;
        let lookup = turn.clone();
        let owner = self
            .ctx
            .store
            .run(move |c| store::turn_session(c, &lookup))
            .await?;
        if owner.as_ref() != Some(&session) {
            return Err(ApiError::NotFound(format!(
                "turn {turn} in session {session}"
            )));
        }
        let workspace = std::path::PathBuf::from(&row.workspace_path);
        crate::files::plan_turn(&self.ctx, &workspace, &turn, direction).await
    }

    pub(crate) async fn do_apply_files(
        &self,
        request: ApplyFileChanges,
    ) -> ApiResult<FileRestoreReport> {
        let session = request.session_id.clone();
        self.ask(&session, |reply| SessionCmd::ApplyFiles(request, reply))
            .await
    }
}
