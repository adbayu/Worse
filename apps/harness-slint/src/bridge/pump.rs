use super::{mpsc, with_state, Duration, Event};

/// Applies a batch of core events on the UI thread.
fn apply(events: Vec<Event>) {
    with_state(|state| {
        for event in events {
            state.apply(event);
        }
    });
}

/// Coalesces core events and hands them to the UI thread in batches (spec §3 "Streaming batching").
pub async fn pump(mut events: mpsc::UnboundedReceiver<Event>) {
    const FRAME: Duration = Duration::from_millis(33);
    while let Some(first) = events.recv().await {
        let streaming = matches!(first, Event::TextDelta { .. });
        let mut batch = vec![first];
        if streaming {
            let deadline = tokio::time::Instant::now() + FRAME;
            while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.recv()).await {
                merge(&mut batch, event);
            }
        } else {
            while let Ok(event) = events.try_recv() {
                merge(&mut batch, event);
            }
        }
        if slint::invoke_from_event_loop(move || apply(batch)).is_err() {
            return;
        }
    }
}

fn merge(batch: &mut Vec<Event>, event: Event) {
    if let Event::TextDelta { session, id, text } = &event {
        if let Some(Event::TextDelta {
            session: last_session,
            id: last_id,
            text: last_text,
        }) = batch.last_mut()
        {
            if last_session == session && last_id == id {
                last_text.push_str(text);
                return;
            }
        }
    }
    batch.push(event);
}
