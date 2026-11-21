use super::{
    emit, fake_choices, now_ms, AgentMsg, AgentOut, AgentState, Event, FakeCore, Item, Notice,
    QueuedMessage, SessionId, ToolStatus, TurnStatus,
};

impl FakeCore {
    #[allow(clippy::too_many_lines)]
    pub(super) async fn on_agent(&mut self, message: AgentMsg) {
        let AgentMsg { run, session, out } = message;
        let current = self
            .open
            .get(&session)
            .and_then(|s| s.run.as_ref())
            .is_some_and(|r| r.id == run);
        if !current {
            // 过期 run（已取消/已切换）的迟到消息直接丢弃；权限请求的 reply 随之 drop，agent 视为拒绝。
            return;
        }
        let Some(s) = self.open.get_mut(&session) else {
            return;
        };
        let Some(r) = s.run.as_mut() else { return };
        let agent = r.agent.clone();
        match out {
            AgentOut::Delta(text) => {
                let existing = r.assistant;
                let id = if let Some(id) = existing {
                    id
                } else {
                    let item = Item::Assistant {
                        agent: agent.clone(),
                        text: String::new(),
                        status: TurnStatus::Streaming,
                    };
                    let id = s.push(item.clone());
                    if let Some(r) = s.run.as_mut() {
                        r.assistant = Some(id);
                    }
                    emit(
                        &self.ev,
                        Event::ItemAdded {
                            session: session.clone(),
                            id,
                            item,
                        },
                    );
                    id
                };
                let Some(s) = self.open.get_mut(&session) else {
                    return;
                };
                if let Some(Item::Assistant { text: buffer, .. }) = s.item_mut(id) {
                    buffer.push_str(&text);
                }
                emit(&self.ev, Event::TextDelta { session, id, text });
            }
            AgentOut::ToolStart(title) => {
                let item = Item::Tool {
                    agent,
                    title,
                    status: ToolStatus::Running,
                };
                let id = s.push(item.clone());
                if let Some(r) = s.run.as_mut() {
                    r.tool = Some(id);
                    // 工具之后的文字属于新的助手段落，保证时间顺序与显示顺序一致。
                    r.assistant = None;
                }
                emit(&self.ev, Event::ItemAdded { session, id, item });
            }
            AgentOut::ToolEnd(status) => {
                let Some(id) = r.tool else { return };
                if let Some(Item::Tool {
                    status: current, ..
                }) = s.item_mut(id)
                {
                    *current = status;
                }
                emit(
                    &self.ev,
                    Event::ToolUpdated {
                        session,
                        id,
                        status,
                    },
                );
            }
            AgentOut::Permission {
                title,
                detail,
                reply,
            } => {
                let request = self.next_request;
                self.next_request += 1;
                let Some(s) = self.open.get_mut(&session) else {
                    return;
                };
                let Some(r) = s.run.as_mut() else { return };
                r.permission = Some((request, reply));
                let tool = r.tool;
                if let Some(id) = tool {
                    if let Some(Item::Tool { status, .. }) = s.item_mut(id) {
                        *status = ToolStatus::AwaitingPermission;
                    }
                    emit(
                        &self.ev,
                        Event::ToolUpdated {
                            session: session.clone(),
                            id,
                            status: ToolStatus::AwaitingPermission,
                        },
                    );
                }
                emit(
                    &self.ev,
                    Event::PermissionRequested {
                        session: session.clone(),
                        request,
                        title,
                        detail,
                    },
                );
                emit(
                    &self.ev,
                    Event::PermissionChoices {
                        session,
                        request,
                        options: fake_choices(),
                    },
                );
            }
            AgentOut::Exhausted { reset } => {
                let prompt = r.prompt.clone();
                let assistant = r.assistant;
                s.run = None;
                if let Some(id) = assistant {
                    if let Some(Item::Assistant { status, .. }) = s.item_mut(id) {
                        *status = TurnStatus::Interrupted;
                    }
                    emit(
                        &self.ev,
                        Event::TurnEnded {
                            session: session.clone(),
                            id,
                            status: TurnStatus::Interrupted,
                        },
                    );
                }
                if let Some(a) = self.agents.iter_mut().find(|a| a.id == agent) {
                    a.state = AgentState::Exhausted {
                        reset: reset.clone(),
                    };
                }
                emit(&self.ev, Event::Agents(self.agents.clone()));
                if let Some(to) = self.next_available_after(&agent) {
                    let notice = Item::Notice {
                        notice: Notice::Failover {
                            from: agent,
                            to: to.clone(),
                            reset,
                        },
                    };
                    let Some(s) = self.open.get_mut(&session) else {
                        return;
                    };
                    let id = s.push(notice.clone());
                    s.sticky.clone_from(&to);
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
                        Event::Sticky {
                            session: session.clone(),
                            agent: to,
                        },
                    );
                    self.start_turn(&session, prompt, true);
                } else {
                    self.pause(&session, prompt);
                }
                self.persist_session(&session).await;
            }
            AgentOut::Finished { cancelled } => {
                let assistant = r.assistant;
                let tool = r.tool;
                s.run = None;
                let status = if cancelled {
                    TurnStatus::Cancelled
                } else {
                    TurnStatus::Done
                };
                if let Some(id) = assistant {
                    if let Some(Item::Assistant {
                        status: current, ..
                    }) = s.item_mut(id)
                    {
                        *current = status;
                    }
                    emit(
                        &self.ev,
                        Event::TurnEnded {
                            session: session.clone(),
                            id,
                            status,
                        },
                    );
                }
                if let Some(id) = tool {
                    let Some(s) = self.open.get_mut(&session) else {
                        return;
                    };
                    if let Some(Item::Tool {
                        status: tool_status,
                        ..
                    }) = s.item_mut(id)
                    {
                        if matches!(
                            tool_status,
                            ToolStatus::Running | ToolStatus::AwaitingPermission
                        ) {
                            *tool_status = ToolStatus::Failed;
                            emit(
                                &self.ev,
                                Event::ToolUpdated {
                                    session: session.clone(),
                                    id,
                                    status: ToolStatus::Failed,
                                },
                            );
                        }
                    }
                }
                emit(
                    &self.ev,
                    Event::Running {
                        session: session.clone(),
                        running: false,
                    },
                );
                let Some(s) = self.open.get_mut(&session) else {
                    return;
                };
                s.summary.updated_ms = now_ms();
                let next = if s.paused {
                    None
                } else {
                    s.queue.pop_front().map(|q| q.text)
                };
                let items: Vec<QueuedMessage> = s.queue.iter().cloned().collect();
                self.persist_session(&session).await;
                self.sync_summary(&session);
                if let Some(prompt) = next {
                    emit(
                        &self.ev,
                        Event::Queue {
                            session: session.clone(),
                            items,
                        },
                    );
                    self.start_turn(&session, prompt, false);
                }
            }
        }
    }

    pub(super) async fn shutdown(&mut self) {
        let ids: Vec<SessionId> = self.open.keys().cloned().collect();
        for id in &ids {
            if let Some(run) = self.open.get_mut(id).and_then(|s| s.run.take()) {
                let _ = run.cancel.send(true);
            }
            self.persist_session(id).await;
        }
    }
}
