use super::{
    clip, comment_items, gh, iso, issue_items, now_ms, params, parse_json, save_source,
    shadow_candidates, shadow_gate, sources, text_of, tickets, GhStatus, Hub, InboxSource, Item,
    Opening, OptionalExtension, SessionId, Stored, TriggerRecord, Verdict, MAX_ACTIVE,
    POLL_OVERLAP_MS, TRIGGER_TEXT_LIMIT,
};

impl Hub {
    pub(super) async fn poll_source(&self, source: InboxSource) -> Result<(), String> {
        let _guard = self.inbox.poll_lock.lock().await;
        let started = now_ms();
        let result = self.poll_once(&source).await;
        let id = source.id.clone();
        let error = result.as_ref().err().cloned();
        let ok = result.is_ok();
        let saved = self
            .ctx
            .store
            .run(move |c| {
                if let Some(mut current) = sources(c)?.into_iter().find(|s| s.id == id) {
                    current.last_poll = Some(started);
                    current.last_error = error;
                    save_source(c, &current)?;
                }
                if ok {
                    c.execute(
                        "INSERT INTO inbox_cursors(source_id, since) VALUES(?1, ?2)
                         ON CONFLICT(source_id) DO UPDATE SET since=?2",
                        params![id, started],
                    )?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = saved {
            tracing::warn!(%error, "inbox poll state not saved");
        }
        self.inbox_changed();
        self.board_sync.wake();
        result
    }

    pub(crate) async fn start_issue_card(
        &self,
        workspace_key: &str,
        number: u64,
    ) -> Result<(), String> {
        let key = workspace_key.to_owned();
        let source = self
            .inbox_sources()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|s| s.workspace_key == key)
            .ok_or_else(|| "no Inbox repository for this board".to_owned())?;
        let source_id = source.id.clone();
        let active: i64 = self
            .ctx
            .store
            .run(move |c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM inbox_tickets WHERE source_id=?1 AND status IN ('working','needsApproval','review')",
                    params![source_id],
                    |r| r.get(0),
                )?)
            })
            .await
            .map_err(|e| e.to_string())?;
        if active >= MAX_ACTIVE {
            return Err("too many open tickets".to_owned());
        }
        let path = format!("repos/{}/issues/{number}", source.repo);
        let issue = parse_json(&gh(&["api", &path], None).await?)?;
        let (status, login) = self.gh_status().await;
        if status != GhStatus::Ready {
            return Err(login);
        }
        let title = text_of(&issue["title"]);
        let body = text_of(&issue["body"]);
        let author = text_of(&issue["user"]["login"]);
        let now = now_ms();
        let item = Item {
            event_id: format!("card:{number}:{now}"),
            kind: "card".into(),
            number,
            author: login,
            text: clip(
                &format!("Issue by {author}: {title}\n\n{body}"),
                TRIGGER_TEXT_LIMIT,
            ),
            title: Some(title),
            created: now,
            url: text_of(&issue["html_url"]),
            ..Item::default()
        };
        let ticket = self.open_ticket(&source, &item, Opening::default()).await;
        self.record(&source, &item, true, "card moved to Todo", Some(ticket))
            .await;
        Ok(())
    }

    pub(super) async fn poll_once(&self, source: &InboxSource) -> Result<(), String> {
        let id = source.id.clone();
        let cursor: Option<i64> = self
            .ctx
            .store
            .run(move |c| {
                Ok(c.query_row(
                    "SELECT since FROM inbox_cursors WHERE source_id=?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .await
            .map_err(|e| e.to_string())?;
        let since = format!(
            "since={}",
            iso(cursor
                .map_or(source.armed_at, |c| c - POLL_OVERLAP_MS)
                .max(source.armed_at))
        );
        let comments_path = format!("repos/{}/issues/comments", source.repo);
        let issues_path = format!("repos/{}/issues", source.repo);
        let comments = gh(
            &[
                "api",
                "-X",
                "GET",
                &comments_path,
                "-f",
                &since,
                "-F",
                "per_page=100",
            ],
            None,
        )
        .await?;
        let issues = gh(
            &[
                "api",
                "-X",
                "GET",
                &issues_path,
                "-f",
                &since,
                "-f",
                "state=all",
                "-F",
                "per_page=50",
            ],
            None,
        )
        .await?;
        let mut items = comment_items(&parse_json(&comments)?);
        let (issue_list, closed) = issue_items(&parse_json(&issues)?);
        let shadows = shadow_candidates(source, &issue_list);
        items.extend(issue_list);
        items.retain(|item| item.created >= source.armed_at);
        items.sort_by_key(|item| item.created);
        for item in items {
            self.handle_item(source, item).await;
        }
        for item in shadows {
            self.handle_shadow(source, item).await;
        }
        for number in closed {
            self.close_thread(source, number).await;
        }
        self.dispatch(&source.id).await;
        Ok(())
    }

    pub(super) async fn handle_shadow(&self, source: &InboxSource, item: Item) {
        let checked = source.clone();
        let probe = item.clone();
        let verdict = self
            .ctx
            .store
            .run(move |c| shadow_gate(c, &checked, &probe))
            .await;
        match verdict {
            Ok(Verdict::New) => {
                let opening = Opening {
                    shadow: true,
                    ..Opening::default()
                };
                let id = self.open_ticket(source, &item, opening).await;
                self.record(source, &item, true, "shadow ticket", Some(id))
                    .await;
                self.inbox_changed();
            }
            Ok(Verdict::Reject(reason)) => {
                self.record(source, &item, false, reason, None).await;
                self.inbox_changed();
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "inbox shadow gate failed"),
        }
    }

    pub(crate) async fn start_routing(&self) {
        let rows = self
            .ctx
            .store
            .run(|c| {
                c.execute(
                    "UPDATE inbox_pending SET state='queued' WHERE state='sent'",
                    [],
                )?;
                let mut statement = c.prepare("SELECT session_id, source_id FROM inbox_routers")?;
                let rows: Vec<(String, String)> = statement
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .filter_map(Result::ok)
                    .collect();
                Ok(rows)
            })
            .await;
        match rows {
            Ok(rows) => {
                for (session, source) in rows {
                    self.ctx.routers.add(&SessionId::new(session), &source);
                }
            }
            Err(error) => tracing::warn!(%error, "inbox routers not loaded"),
        }
    }

    pub(super) async fn queue(&self, source: &InboxSource, item: &Item) {
        let row = (
            item.event_id.clone(),
            source.id.clone(),
            serde_json::to_string(item).unwrap_or_default(),
            now_ms(),
        );
        let saved = self
            .ctx
            .store
            .run(move |c| {
                c.execute(
                    "INSERT OR IGNORE INTO inbox_pending(event_id, source_id, json, state, received_at)
                     VALUES(?1, ?2, ?3, 'queued', ?4)",
                    params![row.0, row.1, row.2, row.3],
                )?;
                Ok(())
            })
            .await;
        if let Err(error) = saved {
            tracing::warn!(%error, "inbox trigger not queued");
            let id = self.open_ticket(source, item, Opening::default()).await;
            self.record(source, item, true, "new ticket", Some(id))
                .await;
            return;
        }
        self.record(source, item, true, "waiting for router", None)
            .await;
    }

    pub(super) async fn update_trigger(
        &self,
        event_id: String,
        accepted: bool,
        reason: String,
        ticket: Option<String>,
    ) {
        let saved = self
            .ctx
            .store
            .run(move |c| {
                let json: Option<String> = c
                    .query_row(
                        "SELECT json FROM inbox_triggers WHERE event_id=?1",
                        params![event_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(mut record) =
                    json.and_then(|j| serde_json::from_str::<TriggerRecord>(&j).ok())
                else {
                    return Ok(());
                };
                record.accepted = accepted;
                record.reason = reason;
                if ticket.is_some() {
                    record.ticket_id = ticket;
                }
                c.execute(
                    "UPDATE inbox_triggers SET json=?2 WHERE event_id=?1",
                    params![event_id, serde_json::to_string(&record)?],
                )?;
                Ok(())
            })
            .await;
        if let Err(error) = saved {
            tracing::warn!(%error, "inbox trigger not updated");
        }
    }

    pub(super) async fn take_pending(
        &self,
        source_id: &str,
        event_id: &str,
        only_sent: bool,
    ) -> Option<Item> {
        let (source_id, event_id) = (source_id.to_owned(), event_id.to_owned());
        self.ctx
            .store
            .run(move |c| {
                let json: Option<String> = c
                    .query_row(
                        "SELECT json FROM inbox_pending WHERE event_id=?1 AND source_id=?2 AND (?3 = 0 OR state='sent')",
                        params![event_id, source_id, i64::from(only_sent)],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(json) = json else {
                    return Ok(None);
                };
                if c.execute("DELETE FROM inbox_pending WHERE event_id=?1", params![event_id])? == 0 {
                    return Ok(None);
                }
                Ok(serde_json::from_str::<Item>(&json).ok())
            })
            .await
            .ok()
            .flatten()
    }

    pub(super) async fn active_on_thread(&self, source_id: &str, number: u64) -> Option<Stored> {
        let source_id = source_id.to_owned();
        let n = i64::try_from(number).unwrap_or(i64::MAX);
        self.ctx
            .store
            .run(move |c| {
                Ok(
                    tickets(c, "WHERE source_id=?1 AND number=?2", &[&source_id, &n])?
                        .into_iter()
                        .find(|s| s.ticket.status.is_active()),
                )
            })
            .await
            .ok()
            .flatten()
    }

    pub(super) async fn deliver(
        &self,
        source: &InboxSource,
        item: &Item,
        opening: Opening,
    ) -> (bool, &'static str, Option<String>) {
        if let Some(stored) = self.active_on_thread(&source.id, item.number).await {
            let id = stored.ticket.id.clone();
            let (accepted, reason) = self.follow_up(source, item, stored).await;
            return (accepted, reason, Some(id));
        }
        let id = self.open_ticket(source, item, opening).await;
        (true, "new ticket", Some(id))
    }

    pub(super) async fn fallback(&self, source: &InboxSource, items: Vec<Item>, reason: &str) {
        for item in items {
            let Some(item) = self.take_pending(&source.id, &item.event_id, false).await else {
                continue;
            };
            let (accepted, outcome, ticket) = self.deliver(source, &item, Opening::default()).await;
            let text = if outcome == "new ticket" {
                reason
            } else {
                outcome
            };
            self.update_trigger(item.event_id.clone(), accepted, text.to_owned(), ticket)
                .await;
        }
        self.inbox_changed();
    }
}
