use super::{
    create_link, fetch_items, gh, graphql, json, linked_keys, load, load_link, now_ms, params,
    plan, rows_of, run_loop, save, save_link, set_status, split_repo, text, ApiError, ApiResult,
    Arc, Board, BoardLane, Connection, Duration, Hub, HubError, Link, Row, Value, ADD_ITEM,
    CONVERT_DRAFT, DELETE_ITEM, REPO_QUERY,
};

impl Hub {
    pub(crate) async fn ensure_board_sync(&self) {
        let keys = self.ctx.store.run(linked_keys).await.unwrap_or_default();
        if let Ok(mut set) = self.board_sync.linked.lock() {
            *set = keys.iter().cloned().collect();
        }
        if keys.is_empty() || self.check_open().is_err() {
            self.board_sync.stop();
            return;
        }
        let Ok(mut slot) = self.board_sync.task.lock() else {
            return;
        };
        if slot.as_ref().is_some_and(|t| !t.is_finished()) {
            self.board_sync.wake();
            return;
        }
        *slot = Some(tokio::spawn(run_loop(
            self.me.clone(),
            Arc::clone(&self.board_sync.wake),
        )));
    }

    pub(super) async fn board_source(&self, key: &str) -> ApiResult<harness_core_api::InboxSource> {
        let wanted = key.to_owned();
        self.ctx
            .store
            .run(move |c| {
                Ok(crate::inbox::sources(c)?
                    .into_iter()
                    .find(|s| s.workspace_key == wanted))
            })
            .await?
            .ok_or_else(|| {
                ApiError::InvalidArgument(
                    "add this project's repository in Settings › Inbox first".into(),
                )
            })
    }

    pub(crate) async fn do_link_board(&self, workspace_key: String) -> ApiResult<Board> {
        self.check_open()?;
        let source = self.board_source(&workspace_key).await?;
        let link = create_link(&source.repo)
            .await
            .map_err(ApiError::InvalidArgument)?;
        let key = workspace_key.clone();
        let source_id = source.id.clone();
        self.ctx
            .store
            .run(move |c| {
                for mut row in rows_of(c, &key)? {
                    if row.data.item_id.is_none() && row.data.issue_number.is_none() {
                        row.data.local_only = true;
                        save(c, &row)?;
                    }
                }
                save_link(c, &key, &source_id, &link)
            })
            .await?;
        self.ensure_board_sync().await;
        self.sync_now(&workspace_key).await;
        self.board_changed(&workspace_key);
        self.do_board(workspace_key).await
    }

    pub(crate) async fn do_unlink_board(&self, workspace_key: String) -> ApiResult<()> {
        self.check_open()?;
        let key = workspace_key.clone();
        self.ctx
            .store
            .run(move |c| {
                c.execute(
                    "DELETE FROM board_links WHERE workspace_key=?1",
                    params![key],
                )?;
                Ok(())
            })
            .await?;
        self.ensure_board_sync().await;
        self.board_changed(&workspace_key);
        Ok(())
    }

    pub(crate) async fn do_sync_board(&self, workspace_key: String) -> ApiResult<Board> {
        self.check_open()?;
        self.sync_now(&workspace_key).await;
        self.do_board(workspace_key).await
    }

    pub(super) async fn sync_now(&self, key: &str) {
        let _guard = self.board_sync.lock.lock().await;
        let result = self.sync_one(key).await;
        let wanted = key.to_owned();
        let error = result.as_ref().err().cloned();
        let saved = self
            .ctx
            .store
            .run(move |c| {
                if let Some((source, mut link)) = load_link(c, &wanted)? {
                    if error.is_none() {
                        link.last_sync = Some(now_ms());
                    }
                    link.error = error.or_else(|| link.workflow_error.clone());
                    save_link(c, &wanted, &source, &link)?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = saved {
            tracing::warn!(%error, "board link state not saved");
        }
        if let Err(error) = result {
            tracing::info!(%error, "board sync failed");
            self.board_changed(key);
        }
    }

    pub(super) async fn sync_one(&self, key: &str) -> Result<(), String> {
        let wanted = key.to_owned();
        let Some((source_id, link)) = self
            .ctx
            .store
            .run(move |c| load_link(c, &wanted))
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let path = self
            .ctx
            .store
            .run(move |c| {
                Ok(crate::inbox::sources(c)?
                    .into_iter()
                    .find(|s| s.id == source_id))
            })
            .await
            .map_err(|e| e.to_string())?
            .map_or_else(|| key.to_owned(), |s| s.workspace_path);
        let remote = fetch_items(&link).await?;
        let wanted = key.to_owned();
        let plan_link = link.clone();
        let mut plan = self
            .ctx
            .store
            .run(move |c| plan(c, &wanted, &path, &plan_link, &remote))
            .await
            .map_err(|e| e.to_string())?;
        let mut changed = plan.changed;
        let mut first_error: Option<String> = None;
        for (card, item, lane) in plan.pushes {
            match set_status(&link, &item, lane).await {
                Ok(()) => {
                    self.update_card(&card, move |row, _| {
                        row.data.synced_lane = Some(lane);
                        Ok(false)
                    })
                    .await?;
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        for (card, number, title, body, lane) in plan.uploads {
            match self.upload(&link, &card, number, &title, &body).await {
                Ok((number, url, item)) => {
                    let pushed = set_status(&link, &item, lane).await;
                    let synced = pushed.is_ok();
                    if let Err(error) = pushed {
                        first_error.get_or_insert(error);
                    }
                    let fresh = self
                        .update_card(&card, move |row, _| {
                            row.data.issue_number = Some(number);
                            row.data.url = Some(url);
                            row.data.item_id = Some(item);
                            if synced {
                                row.data.synced_lane = Some(lane);
                            }
                            Ok(row.session_id.is_none() && row.lane == BoardLane::Todo)
                        })
                        .await?;
                    if fresh {
                        plan.started.push(card);
                    }
                    changed = true;
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        if changed {
            self.board_changed(key);
        }
        for card in plan.started {
            if let Err(error) = self.do_start_card(card).await {
                first_error.get_or_insert(error.to_string());
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(crate) async fn remove_board_item(&self, key: &str, item: &str) -> Result<(), String> {
        let _guard = self.board_sync.lock.lock().await;
        let wanted = key.to_owned();
        let Some((_, link)) = self
            .ctx
            .store
            .run(move |c| load_link(c, &wanted))
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        graphql(DELETE_ITEM, json!({ "p": link.project_id, "i": item }))
            .await
            .map(|_| ())
            .map_err(|e| format!("could not remove the card from the GitHub project: {e}"))
    }

    pub(crate) async fn convert_draft(
        &self,
        key: &str,
        card: &str,
        item: &str,
    ) -> Result<u64, String> {
        let wanted = key.to_owned();
        let (_, link) = self
            .ctx
            .store
            .run(move |c| load_link(c, &wanted))
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the board is not linked to a GitHub project")?;
        let (owner, name) = split_repo(&link.repo)?;
        let repo = graphql(REPO_QUERY, json!({ "o": owner, "r": name })).await?;
        let repo_id = text(&repo["repository"]["id"]);
        if repo_id.is_empty() {
            return Err(format!("repository {} not found", link.repo));
        }
        let data = graphql(CONVERT_DRAFT, json!({ "i": item, "r": repo_id })).await?;
        let content = &data["convertProjectV2DraftIssueItemToIssue"]["item"]["content"];
        let number = content["number"]
            .as_u64()
            .ok_or("GitHub did not turn the draft into an issue")?;
        let url = text(&content["url"]);
        self.update_card(card, move |row, _| {
            row.data.issue_number = Some(number);
            row.data.url = Some(url);
            Ok(false)
        })
        .await?;
        Ok(number)
    }

    pub(super) async fn update_card(
        &self,
        id: &str,
        change: impl FnOnce(&mut Row, &Connection) -> Result<bool, HubError> + Send + 'static,
    ) -> Result<bool, String> {
        let id = id.to_owned();
        self.ctx
            .store
            .run(move |c| {
                let Some(mut row) = load(c, &id)? else {
                    return Ok(false);
                };
                let placed = change(&mut row, c)?;
                if !placed {
                    row.updated_at = now_ms();
                    save(c, &row)?;
                }
                Ok(placed || row.lane == BoardLane::Todo && row.session_id.is_none())
            })
            .await
            .map_err(|e| e.to_string())
    }

    pub(super) async fn upload(
        &self,
        link: &Link,
        card: &str,
        number: Option<u64>,
        title: &str,
        body: &str,
    ) -> Result<(u64, String, String), String> {
        let created = number.is_none();
        let issue = if let Some(n) = number {
            gh(&["api", &format!("repos/{}/issues/{n}", link.repo)], None).await?
        } else {
            let payload = json!({ "title": title, "body": body }).to_string();
            gh(
                &[
                    "api",
                    "-X",
                    "POST",
                    &format!("repos/{}/issues", link.repo),
                    "--input",
                    "-",
                ],
                Some(payload),
            )
            .await?
        };
        let issue: Value = serde_json::from_str(&issue).map_err(|e| e.to_string())?;
        let number = issue["number"]
            .as_u64()
            .ok_or("GitHub issue has no number")?;
        let node = text(&issue["node_id"]);
        let url = text(&issue["html_url"]);
        if created {
            let saved_url = url.clone();
            self.update_card(card, move |row, _| {
                row.data.issue_number = Some(number);
                row.data.url = Some(saved_url);
                Ok(false)
            })
            .await?;
        }
        let vars = json!({ "p": link.project_id, "c": node });
        let data = if let Ok(data) = graphql(ADD_ITEM, vars.clone()).await {
            data
        } else {
            tokio::time::sleep(Duration::from_secs(3)).await;
            graphql(ADD_ITEM, vars).await?
        };
        let item = text(&data["addProjectV2ItemById"]["item"]["id"]);
        if item.is_empty() {
            return Err("GitHub did not add the issue to the project".to_owned());
        }
        Ok((number, url, item))
    }
}
