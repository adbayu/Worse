use tokio::sync::mpsc;

use super::{
    emit, outcome_status, reset_label, tool_detail, tool_status, tool_title, Event, EventKind,
    Item, Notice, PauseReason, TurnMap, TurnOutcome, TurnStatus, View,
};

pub(super) fn turn_queued(
    view: &mut View,
    ev: &mpsc::UnboundedSender<Event>,
    session: String,
    kind: EventKind,
) {
    let EventKind::TurnQueued {
        turn_id,
        user_text,
        position,
        background,
        images,
        ..
    } = kind
    else {
        return;
    };
    view.turns.insert(turn_id.clone(), TurnMap::default());
    if position > 0 {
        if !images.is_empty() {
            view.queued_images.insert(turn_id.clone(), images);
        }
        view.queued.push((turn_id, user_text));
        emit(
            ev,
            Event::Queue {
                session,
                items: view.queue_items(),
            },
        );
    } else {
        let id = view.alloc_in(&turn_id);
        view.turns.entry(turn_id).or_default().user = Some(id);
        emit(
            ev,
            Event::ItemAdded {
                session,
                id,
                item: Item::User {
                    text: user_text,
                    background,
                    images,
                },
            },
        );
    }
}

pub(super) fn turn_started(
    view: &mut View,
    ev: &mpsc::UnboundedSender<Event>,
    session: String,
    kind: EventKind,
) {
    let EventKind::TurnStarted {
        turn_id, attempt, ..
    } = kind
    else {
        return;
    };
    if let Some(text) = view.dequeue(&turn_id) {
        let images = view.queued_images.remove(&turn_id).unwrap_or_default();
        let id = view.alloc_in(&turn_id);
        view.turns.entry(turn_id.clone()).or_default().user = Some(id);
        emit(
            ev,
            Event::ItemAdded {
                session: session.clone(),
                id,
                item: Item::User {
                    text,
                    background: false,
                    images,
                },
            },
        );
    }
    let items = view.queue_items();
    let turn = view.turns.entry(turn_id).or_default();
    if let Some(previous) = turn.assistant.take() {
        if attempt > 1 {
            emit(
                ev,
                Event::TurnEnded {
                    session: session.clone(),
                    id: previous,
                    status: TurnStatus::Interrupted,
                },
            );
        }
    }
    emit(
        ev,
        Event::Queue {
            session: session.clone(),
            items,
        },
    );
    emit(
        ev,
        Event::Paused {
            session: session.clone(),
            paused: false,
        },
    );
    view.running = true;
    emit(
        ev,
        Event::Running {
            session,
            running: true,
        },
    );
}

pub(super) fn message_delta(
    view: &mut View,
    ev: &mpsc::UnboundedSender<Event>,
    session: String,
    kind: EventKind,
) {
    let EventKind::MessageDelta {
        turn_id,
        agent,
        text,
    } = kind
    else {
        return;
    };
    let existing = view.turns.get(&turn_id).and_then(|t| t.assistant);
    let id = if let Some(id) = existing {
        id
    } else {
        let id = view.alloc_in(&turn_id);
        let turn = view.turns.entry(turn_id).or_default();
        turn.assistant = Some(id);
        turn.reply = Some(id);
        emit(
            ev,
            Event::ItemAdded {
                session: session.clone(),
                id,
                item: Item::Assistant {
                    agent: agent.0,
                    text: String::new(),
                    status: TurnStatus::Streaming,
                },
            },
        );
        id
    };
    emit(ev, Event::TextDelta { session, id, text });
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn tool_call(
    view: &mut View,
    ev: &mpsc::UnboundedSender<Event>,
    session: String,
    kind: EventKind,
) {
    let EventKind::ToolCall {
        turn_id,
        agent,
        tool,
    } = kind
    else {
        return;
    };
    // 工具调用之后的文字属于新的助手段落，先结束当前段落以保持时间顺序。
    if let Some(previous) = view
        .turns
        .entry(turn_id.clone())
        .or_default()
        .assistant
        .take()
    {
        emit(
            ev,
            Event::TurnEnded {
                session: session.clone(),
                id: previous,
                status: TurnStatus::Done,
            },
        );
    }
    let tool = view.merge_tool(&tool);
    if let Some(&item) = view.tools.get(&tool.tool_call_id) {
        emit(
            ev,
            Event::ToolUpdated {
                session: session.clone(),
                id: item,
                status: tool_status(tool.status),
            },
        );
        tool_title(ev, &session, item, &tool.title);
        tool_detail(ev, &session, item, &tool);
        return;
    }
    let item = view.alloc_in(&turn_id);
    view.tools.insert(tool.tool_call_id.clone(), item);
    emit(
        ev,
        Event::ItemAdded {
            session: session.clone(),
            id: item,
            item: Item::Tool {
                agent: agent.0,
                title: tool.title.clone(),
                status: tool_status(tool.status),
            },
        },
    );
    tool_detail(ev, &session, item, &tool);
}

pub(super) fn turn_finished(
    view: &mut View,
    ev: &mpsc::UnboundedSender<Event>,
    session: String,
    kind: EventKind,
) {
    let EventKind::TurnFinished {
        turn_id,
        agent,
        outcome,
    } = kind
    else {
        return;
    };
    if view.cancelled.remove(&turn_id) {
        return;
    }
    if view.dequeue(&turn_id).is_some() {
        // 排队中被取消的回合从未运行：只更新队列，不能把正在运行的回合标记为结束。
        emit(
            ev,
            Event::Queue {
                session,
                items: view.queue_items(),
            },
        );
        return;
    }
    let status = outcome_status(&outcome);
    let assistant = view
        .turns
        .get_mut(&turn_id)
        .and_then(|t| t.assistant.take());
    if let Some(id) = assistant {
        emit(
            ev,
            Event::TurnEnded {
                session: session.clone(),
                id,
                status,
            },
        );
    } else if let TurnOutcome::Failed { error, .. } = &outcome {
        let id = view.alloc_in(&turn_id);
        view.turns.entry(turn_id.clone()).or_default().reply = Some(id);
        emit(
            ev,
            Event::ItemAdded {
                session: session.clone(),
                id,
                item: Item::Assistant {
                    agent: agent.map(|a| a.0).unwrap_or_default(),
                    text: error.clone(),
                    status: TurnStatus::Failed,
                },
            },
        );
    }
    view.running = false;
    emit(
        ev,
        Event::Running {
            session,
            running: false,
        },
    );
}

pub(super) fn turn_paused(
    view: &mut View,
    ev: &mpsc::UnboundedSender<Event>,
    session: String,
    kind: EventKind,
) {
    let EventKind::TurnPaused { turn_id, reason } = kind else {
        return;
    };
    let PauseReason::AllAgentsExhausted { agents } = reason;
    let earliest = agents.iter().filter_map(|(_, r)| *r).min();
    if let Some(previous) = view
        .turns
        .get_mut(&turn_id)
        .and_then(|t| t.assistant.take())
    {
        emit(
            ev,
            Event::TurnEnded {
                session: session.clone(),
                id: previous,
                status: TurnStatus::Interrupted,
            },
        );
    }
    let id = view.alloc_in(&turn_id);
    emit(
        ev,
        Event::ItemAdded {
            session: session.clone(),
            id,
            item: Item::Notice {
                notice: Notice::Paused {
                    reset: reset_label(earliest),
                },
            },
        },
    );
    view.running = false;
    emit(
        ev,
        Event::Running {
            session: session.clone(),
            running: false,
        },
    );
    emit(
        ev,
        Event::Paused {
            session,
            paused: true,
        },
    );
}

pub(super) fn steer_injected(
    view: &mut View,
    ev: &mpsc::UnboundedSender<Event>,
    session: String,
    kind: EventKind,
) {
    let EventKind::SteerInjected {
        turn_id,
        steered_turn_id,
        text,
    } = kind
    else {
        return;
    };
    view.turns.entry(steered_turn_id).or_default();
    if let Some(previous) = view
        .turns
        .entry(turn_id.clone())
        .or_default()
        .assistant
        .take()
    {
        emit(
            ev,
            Event::TurnEnded {
                session: session.clone(),
                id: previous,
                status: TurnStatus::Done,
            },
        );
    }
    let id = view.alloc_in(&turn_id);
    emit(
        ev,
        Event::ItemAdded {
            session,
            id,
            item: Item::User {
                text,
                background: false,
                images: Vec::new(),
            },
        },
    );
}
