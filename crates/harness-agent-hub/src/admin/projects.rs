use super::{
    now_ms, oneshot, store, ActorMsg, ActorTx, ApiResult, EventKind, HashMap, Hub, ListProjects,
    ProjectInfo, SessionCmd, SessionId, UNUSED_AFTER_MS,
};

impl Hub {
    pub(crate) async fn do_list_projects(
        &self,
        request: ListProjects,
    ) -> ApiResult<Vec<ProjectInfo>> {
        self.check_open()?;
        let rows = self.ctx.store.run(store::list_projects).await?;
        let now = now_ms();
        let query = request
            .query
            .as_deref()
            .map(|q| q.trim().to_lowercase())
            .filter(|q| !q.is_empty());
        let mut out: Vec<ProjectInfo> = rows
            .into_iter()
            .filter(|row| row.5.is_some() == request.removed)
            .filter(|row| {
                query
                    .as_ref()
                    .map_or(true, |q| row.1.to_lowercase().contains(q.as_str()))
            })
            .map(
                |(key, path, sessions, archived, last_used, removed_at)| ProjectInfo {
                    unused: last_used.map_or(true, |t| now - t > UNUSED_AFTER_MS),
                    workspace_key: key,
                    workspace_path: path,
                    sessions,
                    archived,
                    last_used,
                    removed_at,
                },
            )
            .collect();
        out.sort_by(|a, b| {
            b.last_used
                .cmp(&a.last_used)
                .then(a.workspace_path.cmp(&b.workspace_path))
        });
        Ok(out)
    }

    /// Asks each live actor of `ids` to retire (registry lock held by the caller). The first busy one aborts.
    pub(super) async fn retire(
        actors: &mut HashMap<SessionId, ActorTx>,
        ids: &[SessionId],
    ) -> ApiResult<()> {
        for id in ids {
            let Some(tx) = actors.get(id).cloned() else {
                continue;
            };
            let (reply, rx) = oneshot::channel();
            if tx.send(ActorMsg::Cmd(SessionCmd::Retire(reply))).is_err() {
                actors.remove(id);
                continue;
            }
            match rx.await {
                Ok(Err(error)) => return Err(error),
                Ok(Ok(())) | Err(_) => {
                    actors.remove(id);
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn do_remove_projects(
        &self,
        keys: Vec<String>,
        delete_data: bool,
    ) -> ApiResult<()> {
        self.check_open()?;
        let mut actors = self.actors.lock().await;
        if delete_data {
            let lookup = keys.clone();
            let ids = self
                .ctx
                .store
                .run(move |c| {
                    let mut ids = Vec::new();
                    for key in &lookup {
                        ids.extend(store::session_ids_of_project(c, key)?);
                    }
                    Ok(ids)
                })
                .await?;
            Self::retire(&mut actors, &ids).await?;
            self.delete_rows(ids).await?;
        }
        let now = now_ms();
        self.ctx
            .store
            .run(move |c| {
                for key in &keys {
                    store::set_project_removed(c, key, Some(now))?;
                }
                Ok(())
            })
            .await?;
        drop(actors);
        self.ctx.events.emit(None, EventKind::SessionsChanged);
        Ok(())
    }

    pub(crate) async fn do_restore_projects(&self, keys: Vec<String>) -> ApiResult<()> {
        self.check_open()?;
        let changed = self
            .ctx
            .store
            .run(move |c| {
                let mut changed = false;
                for key in &keys {
                    changed |= store::set_project_removed(c, key, None)?;
                }
                Ok(changed)
            })
            .await?;
        if changed {
            self.ctx.events.emit(None, EventKind::SessionsChanged);
        }
        Ok(())
    }
}
