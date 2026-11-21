use super::{
    branch_for, clean_body, clean_title, lane_ids, load, new_id, not_found, now_ms, params, place,
    renumber, rows_of, save, view, ApiError, ApiResult, Board, BoardLane, CardData, CardState,
    CardView, CreateSession, EventKind, HarnessCore, Hub, NewCard, Path, Row, SendMessage,
    UpdateCard, WorktreeRequest, MAX_BOARD_CARDS,
};

impl Hub {
    pub(crate) fn board_changed(&self, key: &str) {
        self.ctx.events.emit(
            None,
            EventKind::BoardChanged {
                workspace_key: key.to_owned(),
            },
        );
    }

    pub(super) async fn card_view(&self, id: String) -> ApiResult<CardView> {
        let card = self
            .ctx
            .store
            .run(move |c| match load(c, &id)? {
                Some(row) => view(c, row).map(Some),
                None => Ok(None),
            })
            .await?;
        card.ok_or_else(|| ApiError::NotFound("card".into()))
    }

    pub(crate) async fn do_board(&self, workspace_key: String) -> ApiResult<Board> {
        self.check_open()?;
        let key = workspace_key.clone();
        let (cards, sync, link_repo) = self
            .ctx
            .store
            .run(move |c| {
                let mut rows = rows_of(c, &key)?;
                rows.sort_by_key(|r| (r.lane.index(), r.position, r.updated_at));
                let cards = rows
                    .into_iter()
                    .map(|row| view(c, row))
                    .collect::<Result<Vec<_>, _>>()?;
                let (sync, link_repo) = crate::board_sync::meta(c, &key)?;
                Ok((cards, sync, link_repo))
            })
            .await?;
        Ok(Board {
            workspace_key,
            mode: if sync.is_some() { "github" } else { "local" }.into(),
            cards,
            sync,
            link_repo,
        })
    }

    pub(crate) async fn do_create_card(&self, request: NewCard) -> ApiResult<CardView> {
        self.check_open()?;
        let key = request.workspace_key.trim().to_owned();
        let key = if key.is_empty() {
            request.workspace_path.trim().to_owned()
        } else {
            key
        };
        if key.is_empty() {
            return Err(ApiError::InvalidArgument("card has no project".into()));
        }
        let title = clean_title(&request.title)?;
        let body = clean_body(&request.body)?;
        let id = new_id("card");
        let mut row = Row {
            id: id.clone(),
            workspace_key: key.clone(),
            data: CardData {
                title,
                body,
                workspace_path: request.workspace_path,
                ..CardData::default()
            },
            lane: request.lane,
            position: 0,
            session_id: None,
            updated_at: now_ms(),
        };
        let lane = request.lane;
        let count_key = key.clone();
        self.ctx
            .store
            .run(move |c| {
                let count: i64 = c.query_row(
                    "SELECT COUNT(*) FROM kanban_cards WHERE workspace_key=?1",
                    params![count_key],
                    |r| r.get(0),
                )?;
                if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_BOARD_CARDS {
                    return Ok(false);
                }
                row.position =
                    u32::try_from(lane_ids(c, &row.workspace_key, lane)?.len()).unwrap_or(u32::MAX);
                save(c, &row)?;
                Ok(true)
            })
            .await?
            .then_some(())
            .ok_or_else(|| {
                ApiError::InvalidArgument(format!("a board holds at most {MAX_BOARD_CARDS} cards"))
            })?;
        self.board_changed(&key);
        if lane == BoardLane::Todo {
            return self.do_start_card(id).await;
        }
        self.card_view(id).await
    }

    pub(crate) async fn do_start_card(&self, card_id: String) -> ApiResult<CardView> {
        self.check_open()?;
        let id = card_id.clone();
        let (row, linked) = self
            .ctx
            .store
            .run(move |c| {
                let Some(row) = load(c, &id)? else {
                    return Ok(None);
                };
                let linked = crate::board_sync::is_linked(c, &row.workspace_key)?;
                Ok(Some((row, linked)))
            })
            .await?
            .ok_or_else(|| not_found(&card_id))?;
        if row.session_id.is_some() || !linked {
            return self.start_local_card(card_id).await;
        }
        let number = match (row.data.issue_number, row.data.item_id.as_deref()) {
            (None, Some(item)) => {
                match self.convert_draft(&row.workspace_key, &card_id, item).await {
                    Ok(number) => Some(number),
                    Err(error) => {
                        self.fail_card(&card_id, &row.workspace_key).await?;
                        return Err(ApiError::InvalidArgument(error));
                    }
                }
            }
            (number, _) => number,
        };
        match number {
            Some(number) => {
                if let Err(error) = self.start_issue_card(&row.workspace_key, number).await {
                    self.fail_card(&card_id, &row.workspace_key).await?;
                    return Err(ApiError::InvalidArgument(error));
                }
            }
            None => self.board_sync.wake(),
        }
        self.card_view(card_id).await
    }

    pub(crate) async fn fail_card(&self, card_id: &str, key: &str) -> ApiResult<()> {
        let id = card_id.to_owned();
        self.ctx
            .store
            .run(move |c| {
                if let Some(mut row) = load(c, &id)? {
                    row.data.state = CardState::Failed;
                    row.updated_at = now_ms();
                    save(c, &row)?;
                }
                Ok(())
            })
            .await?;
        self.board_changed(key);
        Ok(())
    }

    pub(crate) async fn do_update_card(&self, request: UpdateCard) -> ApiResult<CardView> {
        self.check_open()?;
        let title = request.title.as_deref().map(clean_title).transpose()?;
        let body = request.body.as_deref().map(clean_body).transpose()?;
        let id = request.id.clone();
        let key = self
            .ctx
            .store
            .run(move |c| {
                let Some(mut row) = load(c, &id)? else {
                    return Ok(None);
                };
                if let Some(title) = title {
                    row.data.title = title;
                }
                if let Some(body) = body {
                    row.data.body = body;
                }
                row.updated_at = now_ms();
                save(c, &row)?;
                Ok(Some(row.workspace_key))
            })
            .await?
            .ok_or_else(|| not_found(&request.id))?;
        self.board_changed(&key);
        self.card_view(request.id).await
    }

    pub(crate) async fn do_move_card(
        &self,
        card_id: String,
        lane: BoardLane,
        index: u32,
    ) -> ApiResult<CardView> {
        self.check_open()?;
        let id = card_id.clone();
        let moved = self
            .ctx
            .store
            .run(move |c| {
                let Some(mut row) = load(c, &id)? else {
                    return Ok(None);
                };
                place(c, &mut row, lane, usize::try_from(index).ok())?;
                Ok(Some((row.workspace_key, row.session_id.is_none())))
            })
            .await?;
        let (key, fresh) = moved.ok_or_else(|| not_found(&card_id))?;
        self.board_changed(&key);
        if lane == BoardLane::Todo && fresh {
            return self.do_start_card(card_id).await;
        }
        self.card_view(card_id).await
    }

    pub(crate) async fn do_delete_card(&self, card_id: String) -> ApiResult<()> {
        self.check_open()?;
        let id = card_id.clone();
        let item = self
            .ctx
            .store
            .run(move |c| {
                Ok(load(c, &id)?
                    .and_then(|row| row.data.item_id.map(|item| (row.workspace_key, item))))
            })
            .await?;
        if let Some((key, item)) = item {
            self.remove_board_item(&key, &item)
                .await
                .map_err(ApiError::InvalidArgument)?;
        }
        let id = card_id.clone();
        let key = self
            .ctx
            .store
            .run(move |c| {
                let Some(row) = load(c, &id)? else {
                    return Ok(None);
                };
                c.execute("DELETE FROM kanban_cards WHERE id=?1", params![id])?;
                renumber(c, &lane_ids(c, &row.workspace_key, row.lane)?)?;
                Ok(Some(row.workspace_key))
            })
            .await?
            .ok_or_else(|| not_found(&card_id))?;
        self.board_changed(&key);
        Ok(())
    }

    pub(super) async fn start_local_card(&self, card_id: String) -> ApiResult<CardView> {
        let id = card_id.clone();
        let row = self
            .ctx
            .store
            .run(move |c| load(c, &id))
            .await?
            .ok_or_else(|| not_found(&card_id))?;
        if row.session_id.is_some() {
            return self.card_view(card_id).await;
        }
        let path = if row.data.workspace_path.trim().is_empty() {
            row.workspace_key.clone()
        } else {
            row.data.workspace_path.clone()
        };
        let is_repo =
            crate::snapshot::git_text(Path::new(&path), &["rev-parse", "--is-inside-work-tree"])
                .await
                .is_ok_and(|out| out.trim() == "true");
        let identity = (row.workspace_key != path).then(|| row.workspace_key.clone());
        let summary = self
            .do_create_session(CreateSession {
                workspace_path: path,
                workspace_identity: identity,
                title: Some(row.data.title.clone()),
                agent: None,
                worktree: is_repo.then(|| WorktreeRequest {
                    branch: Some(branch_for(&row.data.title, &row.id)),
                    base: None,
                }),
            })
            .await?;
        let session = summary.id.clone();
        let id = card_id.clone();
        let linked = session.clone();
        self.ctx
            .store
            .run(move |c| {
                let Some(mut row) = load(c, &id)? else {
                    return Ok(());
                };
                row.session_id = Some(linked);
                row.data.state = CardState::Idle;
                row.data.agent = Some(summary.active_agent.clone());
                place(c, &mut row, BoardLane::InProgress, None)
            })
            .await?;
        self.board_changed(&row.workspace_key);
        let text = if row.data.body.trim().is_empty() {
            row.data.title.clone()
        } else {
            format!("{}\n\n{}", row.data.title, row.data.body)
        };
        let sent = self
            .send_message(SendMessage {
                session_id: session,
                text,
                agent: None,
                client_message_id: None,
                context: Vec::new(),
            })
            .await;
        if let Err(error) = sent {
            let id = card_id.clone();
            self.ctx
                .store
                .run(move |c| {
                    if let Some(mut row) = load(c, &id)? {
                        row.data.state = CardState::Failed;
                        row.updated_at = now_ms();
                        save(c, &row)?;
                    }
                    Ok(())
                })
                .await?;
            self.board_changed(&row.workspace_key);
            return Err(error);
        }
        self.card_view(card_id).await
    }
}
