use super::{
    new_id, now_ms, store, store_chat, ApiError, ApiResult, EventKind, GroupColor, Hub, SessionCmd,
    SessionGroup, SessionId, UpdateGroup,
};

impl Hub {
    /// Emits `SessionUpdated` with fresh summaries.
    pub(crate) async fn emit_summaries(&self, ids: Vec<SessionId>) -> ApiResult<()> {
        let summaries = self
            .ctx
            .store
            .run(move |c| {
                let mut out = Vec::new();
                for id in &ids {
                    if let Some(row) = store::load_session(c, id)? {
                        out.push(store::summary(c, &row)?);
                    }
                }
                Ok(out)
            })
            .await?;
        for summary in summaries {
            self.ctx.events.emit(
                Some(&summary.id.clone()),
                EventKind::SessionUpdated { summary },
            );
        }
        Ok(())
    }

    // ---- organisation -------------------------------------------------------------------------------

    pub(crate) async fn do_rename(&self, id: SessionId, title: String) -> ApiResult<()> {
        self.ask(&id, |reply| SessionCmd::Rename(title, reply))
            .await
    }

    pub(crate) async fn do_set_pinned(&self, ids: Vec<SessionId>, pinned: bool) -> ApiResult<()> {
        self.check_open()?;
        let at = pinned.then(now_ms);
        let changed = self
            .ctx
            .store
            .run(move |c| store_chat::set_pinned(c, &ids, at))
            .await?;
        self.emit_summaries(changed).await
    }

    pub(crate) async fn do_set_unread(&self, id: SessionId, unread: bool) -> ApiResult<()> {
        self.check_open()?;
        let lookup = id.clone();
        let found = self
            .ctx
            .store
            .run(move |c| {
                if unread {
                    store_chat::mark_unread(c, &lookup)
                } else {
                    store_chat::mark_read(c, &lookup, now_ms())
                }
            })
            .await?;
        if !found {
            return Err(ApiError::NotFound(format!("session {id}")));
        }
        self.emit_summaries(vec![id]).await
    }

    pub(crate) async fn do_list_groups(&self) -> ApiResult<Vec<SessionGroup>> {
        self.check_open()?;
        Ok(self.ctx.store.run(store_chat::list_groups).await?)
    }

    pub(super) fn group_name(name: &str) -> ApiResult<String> {
        let name = name.trim();
        let count = name.chars().count();
        if count == 0 || count > 60 {
            return Err(ApiError::InvalidArgument(
                "a group name has 1 to 60 characters".into(),
            ));
        }
        Ok(name.to_owned())
    }

    pub(crate) async fn do_create_group(
        &self,
        name: String,
        color: GroupColor,
    ) -> ApiResult<SessionGroup> {
        self.check_open()?;
        let name = Self::group_name(&name)?;
        let group = self
            .ctx
            .store
            .run(move |c| {
                let position = store_chat::list_groups(c)?
                    .iter()
                    .map(|g| g.position + 1)
                    .max()
                    .unwrap_or(0);
                let group = SessionGroup {
                    id: new_id("grp"),
                    name,
                    color,
                    position,
                };
                store_chat::insert_group(c, &group)?;
                Ok(group)
            })
            .await?;
        self.ctx.events.emit(None, EventKind::GroupsChanged);
        Ok(group)
    }

    pub(crate) async fn do_update_group(&self, request: UpdateGroup) -> ApiResult<SessionGroup> {
        self.check_open()?;
        let name = request.name.as_deref().map(Self::group_name).transpose()?;
        let group = self
            .ctx
            .store
            .run(move |c| {
                let Some(mut group) = store_chat::list_groups(c)?
                    .into_iter()
                    .find(|g| g.id == request.id)
                else {
                    return Err(crate::HubError::NotFound(format!("group {}", request.id)));
                };
                if let Some(name) = name {
                    group.name = name;
                }
                if let Some(color) = request.color {
                    group.color = color;
                }
                store_chat::update_group(c, &group)?;
                Ok(group)
            })
            .await?;
        self.ctx.events.emit(None, EventKind::GroupsChanged);
        Ok(group)
    }

    pub(crate) async fn do_delete_group(&self, id: String) -> ApiResult<()> {
        self.check_open()?;
        let members = self
            .ctx
            .store
            .run(move |c| {
                if !store_chat::list_groups(c)?.iter().any(|g| g.id == id) {
                    return Err(crate::HubError::NotFound(format!("group {id}")));
                }
                store_chat::delete_group(c, &id)
            })
            .await?;
        self.ctx.events.emit(None, EventKind::GroupsChanged);
        self.emit_summaries(members).await
    }

    pub(crate) async fn do_reorder_groups(&self, ids: Vec<String>) -> ApiResult<()> {
        self.check_open()?;
        self.ctx
            .store
            .run(move |c| {
                let mut current: Vec<String> = store_chat::list_groups(c)?
                    .into_iter()
                    .map(|g| g.id)
                    .collect();
                let mut given = ids.clone();
                current.sort();
                given.sort();
                if current != given {
                    return Err(crate::HubError::Invalid(
                        "the order must list every group exactly once".into(),
                    ));
                }
                store_chat::set_group_positions(c, &ids)
            })
            .await?;
        self.ctx.events.emit(None, EventKind::GroupsChanged);
        Ok(())
    }

    pub(crate) async fn do_move_sessions(
        &self,
        ids: Vec<SessionId>,
        group: Option<String>,
    ) -> ApiResult<()> {
        self.check_open()?;
        let moved = ids.clone();
        self.ctx
            .store
            .run(move |c| {
                if let Some(group) = &group {
                    if !store_chat::list_groups(c)?.iter().any(|g| &g.id == group) {
                        return Err(crate::HubError::NotFound(format!("group {group}")));
                    }
                }
                for id in &ids {
                    if !store_chat::session_exists(c, id)? {
                        return Err(crate::HubError::NotFound(format!("session {id}")));
                    }
                }
                store_chat::move_sessions(c, &ids, group.as_deref(), now_ms())
            })
            .await?;
        self.emit_summaries(moved).await
    }
}
