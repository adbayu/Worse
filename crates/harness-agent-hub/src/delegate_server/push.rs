use super::*;

/// Delay before a finished subagent turn is pushed, so a blocking `delegate` or a running `subagent_wait`
/// can take the answer first.
const PUSH_DELAY: std::time::Duration = std::time::Duration::from_millis(1500);
/// Longest subagent answer copied into a pushed parent message.
const PUSH_TEXT_CHARS: usize = 20_000;

/// Delivers finished subagent turns to an idle parent as a new turn, so the parent does not have to poll
/// with `subagent_wait`. Runs when a child turn ends (parent idle) and when the parent's own turn ends
/// (results that arrived while it was busy). Ends when the hub or the event bus goes away.
pub(crate) async fn push(hub: Weak<Hub>) {
    let Some(mut events) = hub.upgrade().map(|h| h.ctx.events.subscribe()) else {
        return;
    };
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return,
        };
        let EventKind::TurnFinished { outcome, .. } = &event.kind else {
            continue;
        };
        let Some(session) = event.session_id.clone() else {
            continue;
        };
        let Some(strong) = hub.upgrade() else {
            return;
        };
        let parent = match strong.ctx.subagents.get(&session) {
            Some(view) => view.parent_id,
            // The parent itself was stopped by the user: do not restart it right away.
            None if matches!(outcome, harness_core_api::TurnOutcome::Cancelled) => continue,
            None => session,
        };
        if !strong.ctx.subagents.coord.has_pending(&parent) {
            continue;
        }
        drop(strong);
        let hub = hub.clone();
        tokio::spawn(async move {
            tokio::time::sleep(PUSH_DELAY).await;
            if let Some(hub) = hub.upgrade() {
                push_to(&hub, &parent).await;
            }
        });
    }
}

async fn push_to(hub: &Arc<Hub>, parent: &SessionId) {
    let Ok(detail) = hub.open_session(parent.clone()).await else {
        return;
    };
    let busy = detail.turns.iter().any(|t| {
        matches!(
            t.status,
            TurnStatus::Queued | TurnStatus::Running | TurnStatus::Paused
        )
    });
    if busy || !hub.ctx.subagents.coord.has_pending(parent) {
        return;
    }
    let (finished, notes) = hub.ctx.subagents.coord.take_ready(parent, None);
    if finished.is_empty() {
        return;
    }
    let mut text = String::from(
        "[Harness] A background subagent finished. Review its result below and continue the task; \
         you do not need to call subagent_wait for it.",
    );
    for (child, turn) in &finished {
        let title = hub
            .ctx
            .subagents
            .get(child)
            .map(|v| v.title)
            .unwrap_or_default();
        match result(hub, child, turn).await {
            Ok(value) => {
                let answer: String = value["text"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(PUSH_TEXT_CHARS)
                    .collect();
                let _ = write!(
                    text,
                    "\n\n## Subagent \"{title}\" ({}, session {child}): {}",
                    value["agentName"].as_str().unwrap_or_default(),
                    value["outcome"].as_str().unwrap_or_default(),
                );
                if let Some(error) = value["error"].as_str().filter(|e| !e.is_empty()) {
                    let _ = write!(text, "\nError: {error}");
                }
                let _ = write!(text, "\n{answer}");
            }
            Err(error) => {
                let _ = write!(
                    text,
                    "\n\n## Subagent \"{title}\" (session {child}): failed\n{error}"
                );
            }
        }
    }
    for note in &notes {
        let _ = write!(text, "\n\nNote from {}: {}", note.child, note.text);
    }
    if let Err(error) = hub
        .send_message(SendMessage {
            session_id: parent.clone(),
            text,
            agent: None,
            client_message_id: None,
            context: Vec::new(),
        })
        .await
    {
        tracing::warn!(%parent, %error, "pushing subagent result failed");
    }
}
