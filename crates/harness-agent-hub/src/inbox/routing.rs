use super::{
    clip, params, pending_items, router_message, save_source, sources, store, AgentId,
    CreateSession, HarnessCore, Hub, InboxSource, Item, Opening, OptionalExtension, SendMessage,
    SessionId, Value, BATCH_LIMIT, BRIEF_LIMIT, DISMISS_LIMIT, ROUTER_BATCHES,
};

impl Hub {
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn dispatch(&self, source_id: &str) {
        let _guard = self.inbox.dispatch_lock.lock().await;
        let Ok(source) = self.source(source_id).await else {
            return;
        };
        let lookup = source.id.clone();
        let rows = self
            .ctx
            .store
            .run(move |c| pending_items(c, &lookup, None))
            .await
            .unwrap_or_default();
        if rows.iter().any(|(_, state)| state == "sent") {
            return;
        }
        let batch: Vec<Item> = rows
            .into_iter()
            .filter(|(_, state)| state == "queued")
            .take(BATCH_LIMIT)
            .map(|(item, _)| item)
            .collect();
        if batch.is_empty() {
            return;
        }
        if !source.route {
            self.fallback(&source, batch, "new ticket").await;
            return;
        }
        let orchestra = self.load_orchestra().await;
        let Some(root) = orchestra.orchestrator().cloned() else {
            self.fallback(&source, batch, "router fallback").await;
            return;
        };
        let router = match self.router_for(&source, &root.agent).await {
            Ok(router) => router,
            Err(error) => {
                tracing::warn!(%error, repo = %source.repo, "inbox router not available");
                self.fallback(&source, batch, "router fallback").await;
                return;
            }
        };
        let mut team = vec![format!(
            "- {} — {} (Orchestrator, you; pick it to do the work yourself in a new worktree session)",
            root.agent,
            self.ctx.display_name(&root.agent)
        )];
        for node in orchestra.team() {
            let note = node
                .instructions
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned();
            team.push(format!(
                "- {} — {} ({}){}",
                node.agent,
                self.ctx.display_name(&node.agent),
                node.role.label(),
                if note.is_empty() {
                    String::new()
                } else {
                    format!(": {note}")
                }
            ));
        }
        let ids: Vec<String> = batch.iter().map(|i| i.event_id.clone()).collect();
        let marked = self
            .ctx
            .store
            .run(move |c| {
                for id in &ids {
                    c.execute(
                        "UPDATE inbox_pending SET state='sent' WHERE event_id=?1",
                        params![id],
                    )?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = marked {
            tracing::warn!(%error, "inbox batch not marked");
            return;
        }
        let sent = self
            .send_message(SendMessage {
                session_id: router,
                text: router_message(&source, &team, &batch),
                agent: None,
                client_message_id: None,
                context: Vec::new(),
            })
            .await;
        match sent {
            Ok(_) => {
                let lookup = source.id.clone();
                let _ = self
                    .ctx
                    .store
                    .run(move |c| {
                        c.execute(
                            "UPDATE inbox_routers SET batches=batches+1 WHERE source_id=?1",
                            params![lookup],
                        )?;
                        Ok(())
                    })
                    .await;
            }
            Err(error) => {
                tracing::warn!(%error, "inbox batch not sent to the router");
                self.fallback(&source, batch, "router fallback").await;
            }
        }
        self.inbox_changed();
    }

    pub(super) async fn router_for(
        &self,
        source: &InboxSource,
        agent: &AgentId,
    ) -> Result<SessionId, String> {
        let lookup = source.id.clone();
        let existing: Option<(String, String, i64)> = self
            .ctx
            .store
            .run(move |c| {
                let row = c
                    .query_row(
                        "SELECT session_id, agent, batches FROM inbox_routers WHERE source_id=?1",
                        params![lookup],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?;
                let Some((session, agent, batches)) = row else {
                    return Ok(None);
                };
                let alive = store::load_session(c, &SessionId::new(&session))?.is_some();
                Ok(alive.then_some((session, agent, batches)))
            })
            .await
            .map_err(|e| e.to_string())?;
        if let Some((session, current, batches)) = existing {
            if current == agent.as_str() && batches < ROUTER_BATCHES {
                let id = SessionId::new(session);
                self.ctx.routers.add(&id, &source.id);
                return Ok(id);
            }
            self.drop_router(&session).await;
        }
        let dir = self.ctx.config.home.join("inbox-routers").join(&source.id);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| format!("router folder not created: {e}"))?;
        let summary = self
            .create_router_session(
                CreateSession {
                    workspace_path: dir.to_string_lossy().into_owned(),
                    workspace_identity: None,
                    title: Some(format!("Inbox router · {}", source.repo)),
                    agent: Some(agent.clone()),
                    worktree: None,
                },
                &source.id,
            )
            .await
            .map_err(|e| e.to_string())?;
        let (sid, session) = (source.id.clone(), summary.id.clone());
        let _ = self
            .ctx
            .store
            .run(move |c| {
                if let Some(mut current) = sources(c)?.into_iter().find(|s| s.id == sid) {
                    current.router_session_id = Some(session);
                    save_source(c, &current)?;
                }
                Ok(())
            })
            .await;
        Ok(summary.id)
    }

    pub(super) async fn drop_router(&self, session: &str) {
        let id = SessionId::new(session);
        let _ = HarnessCore::stop(self, id.clone()).await;
        self.ctx.routers.remove(session);
        if let Err(error) = self.delete_session_now(id).await {
            tracing::warn!(%error, "old inbox router not deleted");
        }
    }

    pub(super) async fn router_finished(&self, source_id: String) {
        let lookup = source_id.clone();
        let sent: Vec<Item> = self
            .ctx
            .store
            .run(move |c| pending_items(c, &lookup, Some("sent")))
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(item, _)| item)
            .collect();
        if let Ok(source) = self.source(&source_id).await {
            if !sent.is_empty() {
                self.fallback(&source, sent, "router fallback").await;
            }
            self.dispatch(&source_id).await;
        }
    }

    pub(crate) async fn route_trigger(
        &self,
        router: &SessionId,
        args: &Value,
    ) -> Result<Value, String> {
        let source_id = self
            .ctx
            .routers
            .source_of(router)
            .ok_or("only the Harness inbox router can route triggers")?;
        let trigger = args["trigger_id"]
            .as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or("trigger_id is required")?
            .to_owned();
        let agent = args["agent"]
            .as_str()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(AgentId::new)
            .ok_or("agent is required; call list_agents")?;
        let brief = clip(
            args["brief"].as_str().unwrap_or_default().trim(),
            BRIEF_LIMIT,
        );
        let source = self.source(&source_id).await.map_err(|e| e.to_string())?;
        let orchestra = self.load_orchestra().await;
        let root = orchestra
            .orchestrator()
            .ok_or("the Orchestra has no Orchestrator")?;
        if root.agent != agent && !orchestra.team().iter().any(|n| n.agent == agent) {
            return Err(format!(
                "{agent} is not in the Orchestra team; call list_agents"
            ));
        }
        let item = self
            .take_pending(&source_id, &trigger, true)
            .await
            .ok_or_else(|| {
                format!("{trigger} is not waiting for routing (already handled or unknown)")
            })?;
        let name = self.ctx.display_name(&agent);
        let opening = Opening {
            agent: Some(agent),
            brief: (!brief.is_empty()).then_some(brief),
            shadow: false,
        };
        let (accepted, outcome, ticket) = self.deliver(&source, &item, opening).await;
        let reason = if outcome == "new ticket" {
            format!("routed to {name}")
        } else {
            format!("routed to {name} ({outcome})")
        };
        self.update_trigger(trigger.clone(), accepted, reason, ticket.clone())
            .await;
        self.inbox_changed();
        Ok(serde_json::json!({"triggerId": trigger, "ticketId": ticket, "agentName": name}))
    }

    pub(crate) async fn dismiss_trigger(
        &self,
        router: &SessionId,
        args: &Value,
    ) -> Result<Value, String> {
        let source_id = self
            .ctx
            .routers
            .source_of(router)
            .ok_or("only the Harness inbox router can dismiss triggers")?;
        let trigger = args["trigger_id"]
            .as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or("trigger_id is required")?
            .to_owned();
        let reason: String = args["reason"]
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(DISMISS_LIMIT)
            .collect();
        self.take_pending(&source_id, &trigger, true)
            .await
            .ok_or_else(|| {
                format!("{trigger} is not waiting for routing (already handled or unknown)")
            })?;
        let reason = if reason.is_empty() {
            "no reason given".to_owned()
        } else {
            reason
        };
        self.update_trigger(trigger.clone(), false, format!("dismissed: {reason}"), None)
            .await;
        self.inbox_changed();
        Ok(serde_json::json!({"triggerId": trigger}))
    }
}
