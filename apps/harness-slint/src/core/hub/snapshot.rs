use super::{
    api, is_reject, reset_label, tool_status, HashSet, Item, ItemId, Notice, PermissionDecision,
    ToolCallView, ToolStatus, TurnMap, TurnStatus, View,
};

/// Turns one snapshot turn into UI items (user text, earlier attempts + failover notices, tools, reply).
pub(super) fn snapshot_turn(
    view: &mut View,
    turn: &api::TurnView,
    items: &mut Vec<(ItemId, Item)>,
) {
    let start = items.len();
    snapshot_turn_items(view, turn, items);
    let ended = !matches!(
        turn.status,
        api::TurnStatus::Running | api::TurnStatus::Queued | api::TurnStatus::Paused
    );
    if !ended {
        return;
    }
    let cut = turn.status != api::TurnStatus::Completed;
    let mut marked = false;
    for (_, item) in &mut items[start..] {
        match item {
            Item::Tool { status, .. }
                if matches!(status, ToolStatus::Running | ToolStatus::AwaitingPermission) =>
            {
                *status = ToolStatus::Failed;
            }
            Item::Assistant { status, .. } => marked = *status != TurnStatus::Done,
            _ => {}
        }
    }
    if cut && !marked {
        let status = match turn.status {
            api::TurnStatus::Cancelled => TurnStatus::Cancelled,
            api::TurnStatus::Failed => TurnStatus::Failed,
            _ => TurnStatus::Interrupted,
        };
        let id = view.alloc_in(&turn.id);
        items.push((
            id,
            Item::Assistant {
                agent: turn.agent.as_ref().map(|a| a.0.clone()).unwrap_or_default(),
                text: String::new(),
                status,
            },
        ));
    }
}

#[allow(clippy::too_many_lines)]
pub(super) fn snapshot_turn_items(
    view: &mut View,
    turn: &api::TurnView,
    items: &mut Vec<(ItemId, Item)>,
) {
    let user = view.alloc_in(&turn.id);
    items.push((
        user,
        Item::User {
            text: turn.user_text.clone(),
            background: turn.origin.as_ref().is_some_and(|o| o.background.is_some()),
            images: api::ContextItem::image_paths(&turn.context),
        },
    ));
    let last = turn.attempts.len().saturating_sub(1);
    for (index, attempt) in turn.attempts.iter().enumerate().take(last) {
        if !attempt.partial_text.is_empty() {
            let id = view.alloc_in(&turn.id);
            items.push((
                id,
                Item::Assistant {
                    agent: attempt.agent.0.clone(),
                    text: attempt.partial_text.clone(),
                    status: TurnStatus::Interrupted,
                },
            ));
        }
        if let (Some(api::ErrorClass::QuotaExhausted { resets_at }), Some(next)) =
            (&attempt.error_class, turn.attempts.get(index + 1))
        {
            let id = view.alloc_in(&turn.id);
            items.push((
                id,
                Item::Notice {
                    notice: Notice::Failover {
                        from: attempt.agent.0.clone(),
                        to: next.agent.0.clone(),
                        reset: reset_label(*resets_at),
                    },
                },
            ));
        }
    }
    let denied: HashSet<&str> = turn
        .permissions
        .iter()
        .filter_map(|(title, decision)| match decision {
            PermissionDecision::Selected { kind, .. } if is_reject(*kind) => Some(title.as_str()),
            _ => None,
        })
        .collect();
    let agent = turn.agent.as_ref().map(|a| a.0.clone()).unwrap_or_default();
    let mut map = TurnMap {
        user: Some(user),
        ..TurnMap::default()
    };
    let status = match turn.status {
        api::TurnStatus::Running => Some(TurnStatus::Streaming),
        api::TurnStatus::Completed => Some(TurnStatus::Done),
        api::TurnStatus::Cancelled => Some(TurnStatus::Cancelled),
        api::TurnStatus::Failed => Some(TurnStatus::Failed),
        api::TurnStatus::Interrupted => Some(TurnStatus::Interrupted),
        api::TurnStatus::Queued | api::TurnStatus::Paused => None,
    };
    let interleaved = turn
        .segments
        .iter()
        .any(|s| matches!(s, api::TurnSegment::Text { text } if !text.trim().is_empty()));
    if interleaved {
        snapshot_segments(view, turn, &agent, &denied, status, &mut map, items);
        if turn.status == api::TurnStatus::Paused {
            let id = view.alloc_in(&turn.id);
            items.push((
                id,
                Item::Notice {
                    notice: Notice::Paused { reset: None },
                },
            ));
        }
        view.turns.insert(turn.id.clone(), map);
        return;
    }
    for tool in &turn.tool_calls {
        let id = view.alloc_in(&turn.id);
        view.tools.insert(tool.tool_call_id.clone(), id);
        view.merged.insert(tool.tool_call_id.clone(), tool.clone());
        items.push((
            id,
            snapshot_tool(&agent, tool, denied.contains(tool.title.as_str())),
        ));
    }
    for segment in &turn.segments {
        if let api::TurnSegment::Steer { text } = segment {
            let id = view.alloc_in(&turn.id);
            items.push((
                id,
                Item::User {
                    text: text.clone(),
                    background: false,
                    images: Vec::new(),
                },
            ));
        }
    }
    if let Some(status) = status {
        let text = if turn.status == api::TurnStatus::Failed && turn.assistant_text.is_empty() {
            turn.error.clone().unwrap_or_default()
        } else {
            turn.assistant_text.clone()
        };
        if !text.is_empty() || status == TurnStatus::Streaming {
            let id = view.alloc_in(&turn.id);
            items.push((
                id,
                Item::Assistant {
                    agent: agent.clone(),
                    text,
                    status,
                },
            ));
            map.reply = Some(id);
            if status == TurnStatus::Streaming {
                map.assistant = Some(id);
            }
        }
    }
    if turn.status == api::TurnStatus::Paused {
        let id = view.alloc_in(&turn.id);
        items.push((
            id,
            Item::Notice {
                notice: Notice::Paused { reset: None },
            },
        ));
    }
    view.turns.insert(turn.id.clone(), map);
}

#[allow(clippy::too_many_lines)]
pub(super) fn snapshot_segments(
    view: &mut View,
    turn: &api::TurnView,
    agent: &str,
    denied: &HashSet<&str>,
    status: Option<TurnStatus>,
    map: &mut TurnMap,
    items: &mut Vec<(ItemId, Item)>,
) {
    let mut placed: HashSet<&str> = HashSet::new();
    let mut pending = String::new();
    let mut last_text: Option<usize> = None;
    let push_tool = |view: &mut View, items: &mut Vec<(ItemId, Item)>, tool: &ToolCallView| {
        let id = view.alloc_in(&turn.id);
        view.tools.insert(tool.tool_call_id.clone(), id);
        view.merged.insert(tool.tool_call_id.clone(), tool.clone());
        items.push((
            id,
            snapshot_tool(agent, tool, denied.contains(tool.title.as_str())),
        ));
    };
    let flush = |view: &mut View,
                 items: &mut Vec<(ItemId, Item)>,
                 pending: &mut String,
                 last_text: &mut Option<usize>| {
        let text = pending.trim();
        if text.is_empty() {
            pending.clear();
            return;
        }
        let id = view.alloc_in(&turn.id);
        items.push((
            id,
            Item::Assistant {
                agent: agent.to_string(),
                text: text.to_string(),
                status: TurnStatus::Done,
            },
        ));
        *last_text = Some(items.len() - 1);
        pending.clear();
    };
    for segment in &turn.segments {
        match segment {
            api::TurnSegment::Text { text } => pending.push_str(text),
            api::TurnSegment::Tool { tool_call_id } => {
                flush(view, items, &mut pending, &mut last_text);
                if let Some(tool) = turn
                    .tool_calls
                    .iter()
                    .find(|t| &t.tool_call_id == tool_call_id)
                {
                    if placed.insert(tool.tool_call_id.as_str()) {
                        push_tool(view, items, tool);
                    }
                }
            }
            api::TurnSegment::Steer { text } => {
                flush(view, items, &mut pending, &mut last_text);
                let id = view.alloc_in(&turn.id);
                items.push((
                    id,
                    Item::User {
                        text: text.clone(),
                        background: false,
                        images: Vec::new(),
                    },
                ));
            }
        }
    }
    let trailing_text = !pending.trim().is_empty();
    for tool in &turn.tool_calls {
        if !placed.contains(tool.tool_call_id.as_str()) {
            flush(view, items, &mut pending, &mut last_text);
            push_tool(view, items, tool);
        }
    }
    flush(view, items, &mut pending, &mut last_text);
    let Some(status) = status else {
        return;
    };
    let ends_with_text = trailing_text && last_text == Some(items.len() - 1);
    match last_text {
        Some(index) if ends_with_text || status != TurnStatus::Streaming => {
            let (id, item) = &mut items[index];
            if let Item::Assistant { status: s, .. } = item {
                *s = status;
            }
            map.reply = Some(*id);
            if status == TurnStatus::Streaming {
                map.assistant = Some(*id);
            }
        }
        _ => {
            let text = if status == TurnStatus::Failed {
                turn.error.clone().unwrap_or_default()
            } else {
                String::new()
            };
            if text.is_empty() && status != TurnStatus::Streaming {
                return;
            }
            let id = view.alloc_in(&turn.id);
            items.push((
                id,
                Item::Assistant {
                    agent: agent.to_string(),
                    text,
                    status,
                },
            ));
            map.reply = Some(id);
            if status == TurnStatus::Streaming {
                map.assistant = Some(id);
            }
        }
    }
}

pub(super) fn snapshot_tool(agent: &str, tool: &ToolCallView, denied: bool) -> Item {
    Item::Tool {
        agent: agent.to_string(),
        title: tool.title.clone(),
        status: if denied {
            ToolStatus::Denied
        } else {
            tool_status(tool.status)
        },
    }
}

pub(super) const TAIL_TURNS: usize = 3;
pub(super) const TAIL_ITEMS: usize = 400;

pub(super) fn tail_start(turns: &[api::TurnView]) -> usize {
    let mut budget = 0;
    let mut start = turns.len();
    while start > 0 {
        let cost = 1 + turns[start - 1].tool_calls.len();
        if turns.len() - start >= TAIL_TURNS && budget + cost > TAIL_ITEMS {
            break;
        }
        budget += cost;
        start -= 1;
    }
    start
}
