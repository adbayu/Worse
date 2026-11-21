use super::{
    new_id, now_ms, oneshot, store, ActorMsg, ActorTx, Advertised, AgentId, AgentInfo, ApiError,
    ApiResult, CreateSession, EventKind, Hub, Ordering, Prefs, SessionCmd, SessionId, SessionRow,
    SessionSummary, TurnDiff, TurnId, WorkspaceKey, FALLBACK_KEY,
};

impl Hub {
    pub(super) async fn do_turn_diff(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> ApiResult<TurnDiff> {
        self.check_open()?;
        let (lookup_session, lookup_turn) = (session_id.clone(), turn_id.clone());
        let (session, owner, snapshot) = self
            .ctx
            .store
            .run(move |c| {
                Ok((
                    store::load_session(c, &lookup_session)?,
                    store::turn_session(c, &lookup_turn)?,
                    store::load_snapshot(c, &lookup_turn)?,
                ))
            })
            .await?;
        let session = session.ok_or_else(|| ApiError::NotFound(format!("session {session_id}")))?;
        if owner.as_ref() != Some(&session_id) {
            return Err(ApiError::NotFound(format!(
                "turn {turn_id} in session {session_id}"
            )));
        }
        let unavailable = |reason: String| TurnDiff {
            turn_id: turn_id.clone(),
            available: false,
            reason: Some(reason),
            files: Vec::new(),
            truncated: false,
            undone: false,
        };
        let disabled = if self.ctx.config.turn_snapshots {
            "no snapshot was taken for this turn"
        } else {
            "turn snapshots are disabled (HARNESS_TURN_SNAPSHOTS=0)"
        };
        let Some(snapshot) = snapshot else {
            return Ok(unavailable(disabled.into()));
        };
        let Some(before) = snapshot.before_tree else {
            return Ok(unavailable(
                snapshot.error.unwrap_or_else(|| disabled.into()),
            ));
        };
        let workspace = std::path::PathBuf::from(&session.workspace_path);
        let after = match snapshot.after_tree {
            Some(after) => after,
            // Running (or ended without an after snapshot): compare against the worktree now.
            None => crate::snapshot::take(&self.ctx.config.home.join("tmp"), &workspace)
                .await
                .map_err(ApiError::Internal)?
                .ok_or_else(|| {
                    ApiError::Internal("workspace is no longer a git repository".into())
                })?,
        };
        for tree in [&before, &after] {
            if !crate::snapshot::exists(&workspace, tree).await {
                return Ok(unavailable("snapshot no longer in the repository".into()));
            }
        }
        let undone_turn = turn_id.clone();
        let undone = self
            .ctx
            .store
            .run(move |c| crate::store_chat::is_undone(c, &undone_turn))
            .await?;
        let (files, truncated) = crate::snapshot::diff(&workspace, &before, &after)
            .await
            .map_err(ApiError::Internal)?;
        Ok(TurnDiff {
            turn_id,
            available: true,
            reason: None,
            files,
            truncated,
            undone,
        })
    }

    /// Active agent, its advertisement and the session's choices (read through the session actor).
    pub(super) async fn session_options(
        &self,
        session_id: &SessionId,
    ) -> ApiResult<(AgentId, Advertised, Prefs)> {
        let (agent, prefs) = self.ask(session_id, SessionCmd::Prefs).await?;
        let advertised = self.ctx.advertised(&agent).await;
        Ok((agent, advertised, prefs))
    }

    pub(super) async fn do_list_agents(&self) -> Vec<AgentInfo> {
        let mut out = Vec::new();
        for spec in &self.ctx.config.agents {
            out.push(AgentInfo {
                id: spec.id.clone(),
                display_name: spec.display_name.clone(),
                availability: spec.probe(),
                quota: self.ctx.quota.get(&spec.id),
                runtime: self.ctx.pool.runtime_state(&spec.id).await,
            });
        }
        out
    }

    pub(crate) async fn do_create_session(
        &self,
        request: CreateSession,
    ) -> ApiResult<SessionSummary> {
        self.create_inner(request, None, None).await
    }

    pub(crate) async fn create_router_session(
        &self,
        request: CreateSession,
        source_id: &str,
    ) -> ApiResult<SessionSummary> {
        self.create_inner(request, None, Some(source_id.to_owned()))
            .await
    }

    /// A subagent session of `parent` (§9.8): registered as a child before `SessionCreated` is emitted, so no
    /// client ever lists it as a top-level session.
    pub(crate) async fn create_child_session(
        &self,
        request: CreateSession,
        parent: &SessionId,
    ) -> ApiResult<SessionSummary> {
        self.create_inner(request, Some(parent.clone()), None).await
    }

    #[allow(clippy::too_many_lines)]
    pub(super) async fn create_inner(
        &self,
        request: CreateSession,
        parent: Option<SessionId>,
        router: Option<String>,
    ) -> ApiResult<SessionSummary> {
        self.check_open()?;
        let key = WorkspaceKey::resolve(
            request.workspace_identity.as_deref(),
            &request.workspace_path,
        )
        .map_err(|e| ApiError::InvalidArgument(e.to_string()))?;
        let agent = match request.agent {
            Some(agent) => {
                // Several API keys of one service: sessions take them in turn. The rotation only picks
                // members that are installed, so the availability check comes after it.
                let agent = self.ctx.rotated(agent);
                self.ctx.require_available(&agent)?;
                agent
            }
            None => self.ctx.rotated(
                self.ctx
                    .fallback_order()
                    .into_iter()
                    .find(|a| self.ctx.is_installed(a) && self.ctx.quota.is_usable(a))
                    .or_else(|| self.ctx.fallback_order().into_iter().next())
                    .ok_or_else(|| ApiError::AgentUnavailable("no agents configured".into()))?,
            ),
        };
        let now = now_ms();
        let child = parent.as_ref().map(|p| {
            crate::delegate_server::new_view(
                &SessionId::new(new_id("ses")),
                p,
                &agent,
                request.title.as_deref().unwrap_or_default(),
            )
        });
        let id = child
            .as_ref()
            .map_or_else(|| SessionId::new(new_id("ses")), |c| c.session_id.clone());
        let mut workspace_path = request.workspace_path;
        let worktree = match request.worktree.as_ref().filter(|_| parent.is_none()) {
            Some(wanted) => {
                let created = crate::worktree::create(
                    &self.ctx.config.home,
                    std::path::Path::new(&workspace_path),
                    id.as_str(),
                    wanted,
                )
                .await
                .map_err(ApiError::InvalidArgument)?;
                workspace_path = created.cwd.to_string_lossy().into_owned();
                Some(created.info)
            }
            None => None,
        };
        let row = SessionRow {
            id,
            title: request.title.unwrap_or_else(|| "New session".into()),
            workspace_path,
            workspace_key: key.as_str().to_owned(),
            active_agent: agent,
            created_at: now,
            updated_at: now,
        };
        // A new session in a removed project restores the project (phase-9-parity.md §9.1.5a).
        let stored_child = child.clone();
        let router_source = router.clone();
        let (summary, restored) = self
            .ctx
            .store
            .run(move |c| {
                let restored = router_source.is_none()
                    && store::set_project_removed(c, &row.workspace_key, None)?;
                store::insert_session(c, &row)?;
                if let Some(source) = &router_source {
                    c.execute(
                        "INSERT INTO inbox_routers(source_id, session_id, agent, batches) VALUES(?1, ?2, ?3, 0)
                         ON CONFLICT(source_id) DO UPDATE SET session_id=?2, agent=?3, batches=0",
                        rusqlite::params![source, row.id.as_str(), row.active_agent.as_str()],
                    )?;
                }
                if let Some(info) = &worktree {
                    crate::store_chat::set_worktree(c, &row.id, info)?;
                }
                if let Some(view) = &stored_child {
                    crate::delegate::save(c, view)?;
                }
                // New sessions start read (phase-9-parity.md §9.2.1 set_unread).
                crate::store_chat::mark_read(c, &row.id, now)?;
                Ok((store::summary(c, &row)?, restored))
            })
            .await?;
        if restored {
            self.ctx.events.emit(None, EventKind::SessionsChanged);
        }
        if let Some(view) = child {
            self.ctx.put_subagent(view).await;
        }
        if let Some(source) = &router {
            self.ctx.routers.add(&summary.id, source);
        }
        self.ctx.events.emit(
            Some(&summary.id),
            EventKind::SessionCreated {
                summary: summary.clone(),
            },
        );
        Ok(summary)
    }

    pub(super) async fn do_set_fallback(&self, order: Vec<AgentId>) -> ApiResult<()> {
        if order.is_empty() {
            return Err(ApiError::InvalidArgument("fallback order is empty".into()));
        }
        if let Some(unknown) = order.iter().find(|a| !self.ctx.specs.contains_key(*a)) {
            return Err(ApiError::InvalidArgument(format!(
                "unknown agent {unknown}"
            )));
        }
        let json = serde_json::to_string(&order).map_err(|e| ApiError::Internal(e.to_string()))?;
        self.ctx
            .store
            .run(move |c| store::set_setting(c, FALLBACK_KEY, &json))
            .await?;
        if let Ok(mut current) = self.ctx.fallback.lock() {
            *current = order;
        }
        Ok(())
    }

    pub(super) async fn do_shutdown(&self) -> ApiResult<()> {
        if self.shut_down.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.scheduler.stop();
        self.inbox.stop();
        self.board_sync.stop();
        if let Some(task) = self.delegate_task.lock().ok().and_then(|mut t| t.take()) {
            task.abort();
        }
        let actors: Vec<ActorTx> = self.actors.lock().await.drain().map(|(_, tx)| tx).collect();
        for tx in actors {
            let (reply, rx) = oneshot::channel();
            if tx.send(ActorMsg::Cmd(SessionCmd::Shutdown(reply))).is_ok() {
                let _ = rx.await;
            }
        }
        self.ctx.pool.shutdown_all().await;
        Ok(())
    }
}
