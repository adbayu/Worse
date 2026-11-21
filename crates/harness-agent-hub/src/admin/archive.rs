use super::{
    now_ms, store, ApiError, ApiResult, ArchivePage, ArchivedSession, EventKind, Hub, ListArchived,
    SessionId, SessionSelection, ARCHIVE_PAGE_MAX,
};

impl Hub {
    pub(crate) async fn do_archive_sessions(&self, ids: Vec<SessionId>) -> ApiResult<()> {
        self.check_open()?;
        let mut actors = self.actors.lock().await;
        let lookup = ids.clone();
        let missing = self
            .ctx
            .store
            .run(move |c| {
                let mut missing = Vec::new();
                for id in &lookup {
                    if store::load_session(c, id)?.is_none() {
                        missing.push(id.clone());
                    }
                }
                Ok(missing)
            })
            .await?;
        if let Some(id) = missing.first() {
            return Err(ApiError::NotFound(format!("session {id}")));
        }
        Self::retire(&mut actors, &ids).await?;
        let now = now_ms();
        let changed = self
            .ctx
            .store
            .run(move |c| store::set_archived(c, &ids, Some(now)))
            .await?;
        drop(actors);
        if changed > 0 {
            self.ctx.events.emit(None, EventKind::SessionsChanged);
        }
        Ok(())
    }

    pub(crate) async fn do_list_archived(&self, request: ListArchived) -> ApiResult<ArchivePage> {
        self.check_open()?;
        let limit = request.limit.unwrap_or(50).clamp(1, ARCHIVE_PAGE_MAX);
        let offset = request.offset;
        let (rows, total) = self
            .ctx
            .store
            .run(move |c| store::list_archived(c, &request, limit))
            .await?;
        let shown = offset.saturating_add(u32::try_from(rows.len()).unwrap_or(u32::MAX));
        Ok(ArchivePage {
            rows: rows
                .into_iter()
                .map(|(summary, archived_at)| ArchivedSession {
                    summary,
                    archived_at,
                })
                .collect(),
            total,
            has_more: shown < total,
        })
    }

    pub(super) async fn archived_selection(
        &self,
        selection: SessionSelection,
    ) -> ApiResult<Vec<SessionId>> {
        Ok(self
            .ctx
            .store
            .run(move |c| match &selection {
                SessionSelection::Sessions(ids) => store::filter_archived(c, ids, true),
                SessionSelection::Project(key) => store::archived_of_project(c, key),
            })
            .await?)
    }

    pub(crate) async fn do_unarchive(&self, selection: SessionSelection) -> ApiResult<u32> {
        self.check_open()?;
        let ids = self.archived_selection(selection).await?;
        let changed = self
            .ctx
            .store
            .run(move |c| store::set_archived(c, &ids, None))
            .await?;
        if changed > 0 {
            self.ctx.events.emit(None, EventKind::SessionsChanged);
        }
        Ok(changed)
    }

    pub(super) async fn delete_rows(&self, ids: Vec<SessionId>) -> ApiResult<u32> {
        let (deleted, worktrees) = self
            .ctx
            .store
            .run(move |c| {
                let worktrees = crate::store_chat::worktrees_of(c, &ids)?;
                Ok((store::delete_sessions(c, &ids)?, worktrees))
            })
            .await?;
        for info in worktrees {
            if let Err(error) = crate::worktree::remove(&info).await {
                tracing::warn!(%error, path = %info.path, "worktree kept");
            }
        }
        Ok(deleted)
    }

    pub(crate) async fn do_delete_archived(&self, selection: SessionSelection) -> ApiResult<u32> {
        self.check_open()?;
        let mut actors = self.actors.lock().await;
        let ids = self.archived_selection(selection).await?;
        Self::retire(&mut actors, &ids).await?;
        let deleted = self.delete_rows(ids).await?;
        drop(actors);
        if deleted > 0 {
            self.ctx.events.emit(None, EventKind::SessionsChanged);
        }
        Ok(deleted)
    }

    pub(crate) async fn delete_session_now(&self, id: SessionId) -> ApiResult<()> {
        let mut actors = self.actors.lock().await;
        let ids = vec![id];
        Self::retire(&mut actors, &ids).await?;
        let deleted = self.delete_rows(ids).await?;
        drop(actors);
        if deleted > 0 {
            self.ctx.events.emit(None, EventKind::SessionsChanged);
        }
        Ok(())
    }

    /// `send_message` to an archived session unarchives it first (§9.1.5a).
    pub(crate) async fn unarchive_for_send(&self, id: &SessionId) -> ApiResult<()> {
        let ids = vec![id.clone()];
        let changed = self
            .ctx
            .store
            .run(move |c| store::set_archived(c, &ids, None))
            .await?;
        if changed > 0 {
            self.ctx.events.emit(None, EventKind::SessionsChanged);
        }
        Ok(())
    }

    /// Store rows left for a session (gate support: verifies deletes).
    ///
    /// # Errors
    ///
    /// Store failures.
    pub async fn stored_rows(&self, id: SessionId) -> ApiResult<i64> {
        Ok(self
            .ctx
            .store
            .run(move |c| store::rows_of_session(c, &id))
            .await?)
    }
}
