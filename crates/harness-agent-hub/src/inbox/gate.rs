use super::{
    is_active_status, mentions, now_ms, params, tickets, Connection, HubError, InboxSource, Item,
    OptionalExtension, Verdict, MARKER, MAX_ACTIVE, NEW_PER_HOUR,
};

pub(super) fn gate(c: &Connection, source: &InboxSource, item: &Item) -> Result<Verdict, HubError> {
    let seen: Option<String> = c
        .query_row(
            "SELECT id FROM inbox_triggers WHERE event_id=?1",
            params![item.event_id],
            |r| r.get(0),
        )
        .optional()?;
    if seen.is_some() {
        return Ok(Verdict::Seen);
    }
    if item.text.contains(MARKER) {
        return Ok(Verdict::Reject("own post"));
    }
    let author = item.author.to_lowercase();
    if !source
        .allowlist
        .iter()
        .any(|login| login.to_lowercase() == author)
    {
        return Ok(Verdict::Reject("author not allowed"));
    }
    let number = i64::try_from(item.number).unwrap_or(i64::MAX);
    let open = tickets(
        c,
        "WHERE source_id=?1 AND number=?2",
        &[&source.id, &number],
    )?
    .into_iter()
    .find(|s| is_active_status(s.ticket.status));
    if let Some(stored) = open {
        if stored.ticket.shadow && !mentions(&item.text, &source.handle) {
            return Ok(Verdict::Reject("no mention"));
        }
        if stored.ticket.turns >= source.max_turns {
            return Ok(Verdict::Reject("turn limit"));
        }
        return Ok(Verdict::FollowUp(Box::new(stored)));
    }
    if !mentions(&item.text, &source.handle) {
        return Ok(Verdict::Reject("no mention"));
    }
    limits(c, source)
}

fn limits(c: &Connection, source: &InboxSource) -> Result<Verdict, HubError> {
    let hour_ago = now_ms() - 3_600_000;
    let recent: i64 = c.query_row(
        "SELECT (SELECT COUNT(*) FROM inbox_tickets WHERE source_id=?1 AND created_at>?2)
              + (SELECT COUNT(*) FROM inbox_pending WHERE source_id=?1 AND received_at>?2)",
        params![source.id, hour_ago],
        |r| r.get(0),
    )?;
    if recent >= NEW_PER_HOUR {
        return Ok(Verdict::Reject("rate limit"));
    }
    let active: i64 = c.query_row(
        "SELECT (SELECT COUNT(*) FROM inbox_tickets WHERE source_id=?1 AND status IN ('working','needsApproval','review'))
              + (SELECT COUNT(*) FROM inbox_pending WHERE source_id=?1)",
        params![source.id],
        |r| r.get(0),
    )?;
    if active >= MAX_ACTIVE {
        return Ok(Verdict::Reject("too many open tickets"));
    }
    Ok(Verdict::New)
}

pub(super) fn pending_items(
    c: &Connection,
    source_id: &str,
    state: Option<&str>,
) -> Result<Vec<(Item, String)>, HubError> {
    let mut statement = c.prepare(
        "SELECT json, state FROM inbox_pending WHERE source_id=?1 AND (?2 IS NULL OR state=?2) ORDER BY received_at",
    )?;
    let rows = statement
        .query_map(params![source_id, state], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .filter_map(Result::ok)
        .filter_map(|(json, state)| serde_json::from_str(&json).ok().map(|item| (item, state)))
        .collect();
    Ok(rows)
}

pub(super) fn shadow_gate(
    c: &Connection,
    source: &InboxSource,
    item: &Item,
) -> Result<Verdict, HubError> {
    let seen: Option<String> = c
        .query_row(
            "SELECT id FROM inbox_triggers WHERE event_id=?1",
            params![item.event_id],
            |r| r.get(0),
        )
        .optional()?;
    if seen.is_some() {
        return Ok(Verdict::Seen);
    }
    if item.text.contains(MARKER) {
        return Ok(Verdict::Reject("own post"));
    }
    let number = i64::try_from(item.number).unwrap_or(i64::MAX);
    let tickets_on_thread: i64 = c.query_row(
        "SELECT COUNT(*) FROM inbox_tickets WHERE source_id=?1 AND number=?2",
        params![source.id, number],
        |r| r.get(0),
    )?;
    let queued = pending_items(c, &source.id, None)?
        .iter()
        .any(|(i, _)| i.number == item.number);
    if tickets_on_thread > 0 || queued {
        return Ok(Verdict::Seen);
    }
    limits(c, source)
}

pub(super) fn shadow_candidates(source: &InboxSource, issues: &[Item]) -> Vec<Item> {
    let labels: Vec<String> = source
        .shadow_labels
        .iter()
        .map(|l| l.to_lowercase())
        .collect();
    if labels.is_empty() {
        return Vec::new();
    }
    issues
        .iter()
        .filter(|i| !i.pull && i.open && i.updated >= source.armed_at)
        .filter(|i| i.labels.iter().any(|l| labels.contains(&l.to_lowercase())))
        .map(|i| {
            let mut item = i.clone();
            item.event_id = format!("shadow:{}", i.event_id.trim_start_matches("issue:"));
            item.kind = "shadow".into();
            item
        })
        .collect()
}
