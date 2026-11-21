use super::{
    first_message, follow_message, gate, gh, git, insert_trigger, new_id, now_ms, parse_json,
    save_ticket, shadow_message, store, text_of, ticket_branch, BoardLane, CreateSession,
    EventKind, HarnessCore, Hub, InboxSource, Item, Opening, Path, SendMessage, Stored, Ticket,
    TicketStatus, TriggerRecord, Verdict, WorktreeRequest,
};

impl Hub {
    pub(super) async fn promote(&self, id: &str) -> Result<Stored, String> {
        let stored = self.ticket(id).await.map_err(|e| e.to_string())?;
        if !stored.ticket.shadow {
            return Ok(stored);
        }
        let session = stored
            .ticket
            .session_id
            .clone()
            .ok_or("the ticket has no session")?;
        let source = self
            .source(&stored.ticket.source_id)
            .await
            .map_err(|e| e.to_string())?;
        let number = stored.ticket.number;
        let title = format!("#{number} {}", stored.ticket.title);
        let body = stored.ticket.url.clone();
        let path = source.workspace_path.clone();
        let review = stored.ticket.status == TicketStatus::Review;
        let lookup = session.clone();
        let linked = self
            .ctx
            .store
            .run(move |c| {
                let Some(row) = store::load_session(c, &lookup)? else {
                    return Ok(None);
                };
                let card = crate::board::link_card(
                    c,
                    &row.workspace_key,
                    &path,
                    &title,
                    &body,
                    &lookup,
                    Some(row.active_agent.clone()),
                    Some(number),
                )?;
                if review {
                    crate::board::move_session_cards(c, &lookup, BoardLane::Review)?;
                }
                Ok(Some((card, row.workspace_key)))
            })
            .await
            .map_err(|e| e.to_string())?;
        let (card, key) = linked.ok_or("the ticket's session was deleted")?;
        self.ctx
            .events
            .emit(None, EventKind::BoardChanged { workspace_key: key });
        let promoted = self
            .update_ticket(id, move |s| {
                s.ticket.shadow = false;
                s.ticket.card_id = Some(card);
                true
            })
            .await
            .map_err(|e| e.to_string())?;
        promoted.ok_or_else(|| "the ticket was removed".to_owned())
    }

    pub(super) async fn record(
        &self,
        source: &InboxSource,
        item: &Item,
        accepted: bool,
        reason: &str,
        ticket: Option<String>,
    ) {
        let record = TriggerRecord {
            id: new_id("trg"),
            source_id: source.id.clone(),
            repo: source.repo.clone(),
            event_id: item.event_id.clone(),
            number: item.number,
            author: item.author.clone(),
            kind: item.kind.clone(),
            url: item.url.clone(),
            accepted,
            reason: reason.to_owned(),
            ticket_id: ticket,
            received_at: now_ms(),
        };
        if let Err(error) = self
            .ctx
            .store
            .run(move |c| insert_trigger(c, &record))
            .await
        {
            tracing::warn!(%error, "inbox trigger not saved");
        }
    }

    pub(super) async fn handle_item(&self, source: &InboxSource, item: Item) {
        let checked = source.clone();
        let probe = item.clone();
        let verdict = self.ctx.store.run(move |c| gate(c, &checked, &probe)).await;
        let verdict = match verdict {
            Ok(v) => v,
            Err(error) => {
                tracing::warn!(%error, "inbox gate failed");
                return;
            }
        };
        match verdict {
            Verdict::Seen => {}
            Verdict::Reject(reason) => self.record(source, &item, false, reason, None).await,
            Verdict::FollowUp(stored) => {
                let id = stored.ticket.id.clone();
                let (accepted, reason) = self.follow_up(source, &item, *stored).await;
                self.record(source, &item, accepted, reason, Some(id)).await;
            }
            Verdict::New if source.route => self.queue(source, &item).await,
            Verdict::New => {
                let id = self.open_ticket(source, &item, Opening::default()).await;
                self.record(source, &item, true, "new ticket", Some(id))
                    .await;
            }
        }
        self.inbox_changed();
    }

    pub(super) async fn follow_up(
        &self,
        source: &InboxSource,
        item: &Item,
        stored: Stored,
    ) -> (bool, &'static str) {
        let id = stored.ticket.id.clone();
        let Some(session) = stored.ticket.session_id.clone() else {
            return (false, "ticket has no session");
        };
        if stored.ticket.shadow {
            if let Err(error) = self.promote(&id).await {
                tracing::info!(%error, "shadow ticket not promoted");
            }
        }
        let sent = self
            .send_message(SendMessage {
                session_id: session,
                text: follow_message(&stored, source, item),
                agent: None,
                client_message_id: None,
                context: Vec::new(),
            })
            .await;
        match sent {
            Ok(_) => {
                let _ = self
                    .update_ticket(&id, |s| {
                        s.ticket.turns += 1;
                        s.ticket.status = TicketStatus::Working;
                        s.ticket.pending_post = false;
                        true
                    })
                    .await;
                (true, "follow-up")
            }
            Err(error) => {
                tracing::info!(%error, "inbox follow-up not sent");
                (false, "follow-up not delivered")
            }
        }
    }

    pub(super) async fn open_ticket(
        &self,
        source: &InboxSource,
        item: &Item,
        opening: Opening,
    ) -> String {
        let id = new_id("tkt");
        let now = now_ms();
        let mut stored = Stored {
            ticket: Ticket {
                id: id.clone(),
                source_id: source.id.clone(),
                repo: source.repo.clone(),
                number: item.number,
                title: item.title.clone().unwrap_or_default(),
                url: item.url.clone(),
                requester: item.author.clone(),
                status: TicketStatus::Working,
                session_id: None,
                card_id: None,
                branch: None,
                pr_url: None,
                pending_post: false,
                turns: 0,
                shadow: opening.shadow,
                error: None,
                created_at: now,
                updated_at: now,
            },
            base: String::new(),
            worktree: String::new(),
            summary: String::new(),
        };
        let first = stored.clone();
        let mut first = first;
        if let Err(error) = self
            .ctx
            .store
            .run(move |c| save_ticket(c, &mut first))
            .await
        {
            tracing::warn!(%error, "ticket not saved");
            return id;
        }
        self.inbox_changed();
        if let Err(error) = self.start_ticket(source, item, &mut stored, &opening).await {
            stored.ticket.status = TicketStatus::Failed;
            stored.ticket.error = Some(error);
        }
        let mut last = stored;
        if let Err(error) = self.ctx.store.run(move |c| save_ticket(c, &mut last)).await {
            tracing::warn!(%error, "ticket not saved");
        }
        self.inbox_changed();
        id
    }

    pub(super) async fn start_ticket(
        &self,
        source: &InboxSource,
        item: &Item,
        stored: &mut Stored,
        opening: &Opening,
    ) -> Result<(), String> {
        if stored.ticket.title.is_empty() {
            let path = format!("repos/{}/issues/{}", source.repo, item.number);
            let issue = parse_json(&gh(&["api", &path], None).await?)?;
            stored.ticket.title = text_of(&issue["title"]);
            let url = text_of(&issue["html_url"]);
            if !url.is_empty() {
                stored.ticket.url = url;
            }
        }
        let number = stored.ticket.number;
        let title = stored.ticket.title.clone();
        let branch = ticket_branch(number, &title);
        let identity =
            (source.workspace_key != source.workspace_path).then(|| source.workspace_key.clone());
        let summary = self
            .do_create_session(CreateSession {
                workspace_path: source.workspace_path.clone(),
                workspace_identity: identity,
                title: Some(format!("#{number} {title}")),
                agent: opening.agent.clone().or_else(|| source.agent.clone()),
                worktree: Some(WorktreeRequest {
                    branch: Some(branch),
                    base: None,
                }),
            })
            .await
            .map_err(|e| e.to_string())?;
        let worktree = summary
            .worktree
            .clone()
            .ok_or_else(|| "the session has no worktree".to_owned())?;
        stored.ticket.session_id = Some(summary.id.clone());
        stored.ticket.branch = Some(worktree.branch.clone());
        stored.worktree.clone_from(&worktree.path);
        stored.base = git(Path::new(&worktree.path), &["rev-parse", "HEAD"])
            .await
            .map(|s| s.trim().to_owned())?;
        if !opening.shadow {
            let key = summary.workspace_key.clone();
            let path = source.workspace_path.clone();
            let card_title = format!("#{number} {title}");
            let card_body = stored.ticket.url.clone();
            let session = summary.id.clone();
            let agent = summary.active_agent.clone();
            let card = self
                .ctx
                .store
                .run(move |c| {
                    crate::board::link_card(
                        c,
                        &key,
                        &path,
                        &card_title,
                        &card_body,
                        &session,
                        Some(agent),
                        Some(number),
                    )
                })
                .await
                .map_err(|e| e.to_string())?;
            stored.ticket.card_id = Some(card);
            self.ctx.events.emit(
                None,
                EventKind::BoardChanged {
                    workspace_key: summary.workspace_key.clone(),
                },
            );
        }
        let mut snapshot = stored.clone();
        self.ctx
            .store
            .run(move |c| save_ticket(c, &mut snapshot))
            .await
            .map_err(|e| e.to_string())?;
        self.inbox_changed();
        self.send_message(SendMessage {
            session_id: summary.id,
            text: if opening.shadow {
                shadow_message(stored, source, item)
            } else {
                first_message(stored, source, item, opening.brief.as_deref())
            },
            agent: None,
            client_message_id: None,
            context: Vec::new(),
        })
        .await
        .map_err(|e| e.to_string())?;
        stored.ticket.turns = 1;
        Ok(())
    }
}
