use super::{
    agent_name, emit, fake_agent, now_ms, watch, AgentId, AgentState, Event, FakeCore, Item, Job,
    Notice, QueuedMessage, Run, SessionId,
};

impl FakeCore {
    pub(super) async fn send(&mut self, session: SessionId, agent: AgentId, text: String) {
        // Sending to an archived session unarchives it (same rule as the hub, §9.1.5a); sync_summary re-lists it.
        self.admin.archived.retain(|(s, _)| s.id != session);
        if !self.ensure_open(&session).await {
            emit(
                &self.ev,
                Event::Error(format!("session {session} not found")),
            );
            return;
        }
        let Some(s) = self.open.get_mut(&session) else {
            return;
        };
        if s.sticky != agent {
            s.sticky.clone_from(&agent);
            emit(
                &self.ev,
                Event::Sticky {
                    session: session.clone(),
                    agent,
                },
            );
        }
        let Some(s) = self.open.get_mut(&session) else {
            return;
        };
        if s.run.is_some() || s.paused {
            let id = s.queue_id();
            s.queue.push_back(QueuedMessage { id, text });
            let items = s.queue.iter().cloned().collect();
            emit(&self.ev, Event::Queue { session, items });
            return;
        }
        self.start_turn(&session, text, false);
    }

    /// Starts a turn with the session's sticky agent (falling back when it is not available).
    pub(super) fn start_turn(&mut self, session: &SessionId, prompt: String, continuation: bool) {
        let Some(sticky) = self.open.get(session).map(|s| s.sticky.clone()) else {
            return;
        };
        let agent = if self.is_available(&sticky) {
            Some(sticky.clone())
        } else {
            self.next_available_after(&sticky)
        };
        let Some(agent) = agent else {
            self.pause(session, prompt);
            return;
        };
        if agent != sticky {
            if let Some(s) = self.open.get_mut(session) {
                s.sticky.clone_from(&agent);
            }
            emit(
                &self.ev,
                Event::Sticky {
                    session: session.clone(),
                    agent: agent.clone(),
                },
            );
        }
        let run_id = self.next_run;
        self.next_run += 1;
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let Some(s) = self.open.get_mut(session) else {
            return;
        };
        if !continuation {
            if s.summary.title.is_empty() {
                s.summary.title = prompt.chars().take(60).collect();
            }
            let id = s.push(Item::User {
                text: prompt.clone(),
                background: false,
                images: Vec::new(),
            });
            let item = Item::User {
                text: prompt.clone(),
                background: false,
                images: Vec::new(),
            };
            emit(
                &self.ev,
                Event::ItemAdded {
                    session: session.clone(),
                    id,
                    item,
                },
            );
        }
        let Some(s) = self.open.get_mut(session) else {
            return;
        };
        if !s.summary.agents_used.contains(&agent) {
            s.summary.agents_used.push(agent.clone());
        }
        s.summary.updated_ms = now_ms();
        s.run = Some(Run {
            id: run_id,
            agent: agent.clone(),
            prompt: prompt.clone(),
            assistant: None,
            tool: None,
            cancel: cancel_tx,
            permission: None,
        });
        emit(
            &self.ev,
            Event::Running {
                session: session.clone(),
                running: true,
            },
        );
        self.sync_summary(session);
        let job = Job {
            run: run_id,
            session: session.clone(),
            name: agent_name(&agent),
            agent,
            prompt,
            continuation,
        };
        tokio::spawn(fake_agent(job, self.agent_tx.clone(), cancel_rx));
    }

    pub(super) fn pause(&mut self, session: &SessionId, prompt: String) {
        let reset = self.agents.iter().find_map(|a| {
            if let AgentState::Exhausted { reset } = &a.state {
                reset.clone()
            } else {
                None
            }
        });
        let Some(s) = self.open.get_mut(session) else {
            return;
        };
        s.paused = true;
        // 未完成的提示词放回队首，等有可用 agent 时继续，而不是丢弃。
        let id = s.queue_id();
        s.queue.push_front(QueuedMessage { id, text: prompt });
        let items: Vec<QueuedMessage> = s.queue.iter().cloned().collect();
        let notice = Item::Notice {
            notice: Notice::Paused { reset },
        };
        let id = s.push(notice.clone());
        emit(
            &self.ev,
            Event::ItemAdded {
                session: session.clone(),
                id,
                item: notice,
            },
        );
        emit(
            &self.ev,
            Event::Paused {
                session: session.clone(),
                paused: true,
            },
        );
        emit(
            &self.ev,
            Event::Queue {
                session: session.clone(),
                items,
            },
        );
        emit(
            &self.ev,
            Event::Running {
                session: session.clone(),
                running: false,
            },
        );
    }

    pub(super) fn cancel(&mut self, session: &SessionId) {
        let Some(run) = self.open.get_mut(session).and_then(|s| s.run.as_mut()) else {
            return;
        };
        let _ = run.cancel.send(true);
        if let Some((request, _reply)) = run.permission.take() {
            emit(
                &self.ev,
                Event::PermissionCleared {
                    session: session.clone(),
                    request,
                },
            );
        }
    }

    pub(super) fn respond_permission(&mut self, session: &SessionId, request: u64, allow: bool) {
        let Some(run) = self.open.get_mut(session).and_then(|s| s.run.as_mut()) else {
            return;
        };
        let Some((pending, _)) = &run.permission else {
            return;
        };
        if *pending != request {
            return;
        }
        if let Some((_, reply)) = run.permission.take() {
            let _ = reply.send(allow);
        }
        emit(
            &self.ev,
            Event::PermissionCleared {
                session: session.clone(),
                request,
            },
        );
    }
}
