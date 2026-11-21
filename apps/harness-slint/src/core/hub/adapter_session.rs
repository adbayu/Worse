use super::session_flow::{
    message_delta, steer_injected, tool_call, turn_finished, turn_paused, turn_queued, turn_started,
};
use super::{
    api, emit, is_reject, reset_label, tool_detail, tool_status, tool_title, Adapter, Event,
    EventKind, Item, Notice, PermissionDecision, SwitchReason, ToolStatus,
};

impl Adapter {
    #[allow(clippy::too_many_lines)]
    pub(super) fn on_session_event(&mut self, kind: EventKind) {
        let Some(view) = self.view.as_mut() else {
            return;
        };
        let session = view.id.0.clone();
        let ev = self.ev.clone();
        match kind {
            kind @ EventKind::TurnQueued { .. } => turn_queued(view, &ev, session, kind),
            kind @ EventKind::TurnStarted { .. } => turn_started(view, &ev, session, kind),
            kind @ EventKind::MessageDelta { .. } => message_delta(view, &ev, session, kind),
            kind @ EventKind::ToolCall { .. } => tool_call(view, &ev, session, kind),
            EventKind::ToolCallUpdated { tool, .. } => {
                let tool = view.merge_tool(&tool);
                if let Some(&item) = view.tools.get(&tool.tool_call_id) {
                    let status = if view.denied.contains(&tool.tool_call_id) {
                        ToolStatus::Denied
                    } else {
                        tool_status(tool.status)
                    };
                    emit(
                        &ev,
                        Event::ToolUpdated {
                            session: session.clone(),
                            id: item,
                            status,
                        },
                    );
                    tool_title(&ev, &session, item, &tool.title);
                    tool_detail(&ev, &session, item, &tool);
                }
            }
            EventKind::ThoughtDelta { text, .. } => emit(&ev, Event::Thought { session, text }),
            EventKind::PlanUpdated { entries, .. } => emit(&ev, Event::Plan { session, entries }),
            EventKind::SubagentUpdated { subagent } => {
                match view
                    .subagents
                    .iter_mut()
                    .find(|s| s.session_id == subagent.session_id)
                {
                    Some(slot) => *slot = subagent,
                    None => view.subagents.insert(0, subagent),
                }
                emit(
                    &ev,
                    Event::Subagents {
                        session,
                        items: view.subagents.clone(),
                    },
                );
            }
            EventKind::UsageUpdated { usage, .. } => emit(&ev, Event::Usage { session, usage }),
            EventKind::BackgroundTasksChanged { tasks } => emit(
                &ev,
                Event::BackgroundTasks {
                    session,
                    tasks: tasks.clone(),
                },
            ),
            EventKind::FilesRestored { turn_id, direction } => {
                let item = view.turns.get(&turn_id).and_then(|t| t.user);
                emit(
                    &ev,
                    Event::FilesRestored {
                        session,
                        item,
                        direction,
                    },
                );
            }
            EventKind::PermissionRequested { request } => self.permission_requested(request),
            EventKind::PermissionResolved {
                request_id,
                decision,
                ..
            } => self.permission_resolved(&request_id, &decision),
            kind @ EventKind::TurnFinished { .. } => turn_finished(view, &ev, session, kind),
            kind @ EventKind::TurnPaused { .. } => turn_paused(view, &ev, session, kind),
            EventKind::AgentSwitched {
                from, to, reason, ..
            } => {
                if let (SwitchReason::QuotaExhausted { resets_at }, Some(from)) = (&reason, from) {
                    let id = view.alloc();
                    emit(
                        &ev,
                        Event::ItemAdded {
                            session: session.clone(),
                            id,
                            item: Item::Notice {
                                notice: Notice::Failover {
                                    from: from.0,
                                    to: to.0.clone(),
                                    reset: reset_label(*resets_at),
                                },
                            },
                        },
                    );
                }
                self.sticky.insert(view.id.clone(), to.0.clone());
                emit(
                    &ev,
                    Event::Sticky {
                        session,
                        agent: to.0,
                    },
                );
            }
            kind @ EventKind::SteerInjected { .. } => steer_injected(view, &ev, session, kind),
            EventKind::QueueChanged { queue } => {
                // hub 队列是唯一事实来源（替换语义）：按其顺序重建本地排队列表，不在 UI 侧推算顺序。
                view.queued = queue
                    .into_iter()
                    .map(|q| (q.turn_id, q.user_text))
                    .collect();
                for (turn, _) in &view.queued {
                    view.turns.entry(turn.clone()).or_default();
                }
                emit(
                    &ev,
                    Event::Queue {
                        session,
                        items: view.queue_items(),
                    },
                );
            }
            EventKind::TurnFailure {
                turn_id, failure, ..
            } => {
                view.last_failed = Some(turn_id.clone());
                let item = view.turns.get(&turn_id).and_then(|t| t.user);
                emit(
                    &ev,
                    Event::Failure {
                        session: Some(session),
                        item,
                        kind: failure.kind,
                        message: failure.message,
                        details: failure.details,
                        trace: failure.trace,
                        retry: failure.retry,
                    },
                );
            }
            _ => {}
        }
    }

    pub(super) fn permission_resolved(
        &mut self,
        request_id: &api::PermissionRequestId,
        decision: &PermissionDecision,
    ) {
        let Some(view) = self.view.as_mut() else {
            return;
        };
        let session = view.id.0.clone();
        let Some((&number, pending)) = self
            .permissions
            .iter()
            .find(|(_, p)| &p.request_id == request_id)
        else {
            return;
        };
        let tool = pending.tool_call_id.clone();
        self.permissions.remove(&number);
        emit(
            &self.ev,
            Event::PermissionCleared {
                session: session.clone(),
                request: number,
            },
        );
        let status = match decision {
            PermissionDecision::Selected { kind, .. } if is_reject(*kind) => {
                view.denied.insert(tool.clone());
                ToolStatus::Denied
            }
            PermissionDecision::Selected { .. } => ToolStatus::Running,
            PermissionDecision::Cancelled => ToolStatus::Failed,
        };
        if let Some(&item) = view.tools.get(&tool) {
            emit(
                &self.ev,
                Event::ToolUpdated {
                    session,
                    id: item,
                    status,
                },
            );
        }
    }
}
