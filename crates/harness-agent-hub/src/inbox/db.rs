use super::{
    now_ms, params, Connection, HubError, InboxSource, SessionId, Stored, TicketStatus,
    TriggerRecord,
};

pub(crate) fn sources(c: &Connection) -> Result<Vec<InboxSource>, HubError> {
    let mut statement = c.prepare("SELECT json FROM inbox_sources ORDER BY created_at")?;
    let rows = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(Result::ok)
        .filter_map(|json| serde_json::from_str(&json).ok())
        .collect();
    Ok(rows)
}

pub(super) fn save_source(c: &Connection, source: &InboxSource) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO inbox_sources(id, json, created_at) VALUES(?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET json=?2",
        params![source.id, serde_json::to_string(source)?, now_ms()],
    )?;
    Ok(())
}

pub(super) fn tickets(
    c: &Connection,
    filter: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Stored>, HubError> {
    let mut statement = c.prepare(&format!(
        "SELECT json FROM inbox_tickets {filter} ORDER BY created_at DESC"
    ))?;
    let rows = statement
        .query_map(args, |r| r.get::<_, String>(0))?
        .filter_map(Result::ok)
        .filter_map(|json| serde_json::from_str(&json).ok())
        .collect();
    Ok(rows)
}

pub(super) fn load_ticket(c: &Connection, id: &str) -> Result<Option<Stored>, HubError> {
    Ok(tickets(c, "WHERE id=?1", &[&id])?.into_iter().next())
}

pub(super) fn save_ticket(c: &Connection, stored: &mut Stored) -> Result<(), HubError> {
    stored.ticket.updated_at = now_ms();
    let t = &stored.ticket;
    c.execute(
        "INSERT INTO inbox_tickets(id, source_id, number, session_id, status, json, created_at, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET session_id=?4, status=?5, json=?6, updated_at=?8",
        params![
            t.id,
            t.source_id,
            i64::try_from(t.number).unwrap_or(i64::MAX),
            t.session_id.as_ref().map(SessionId::as_str),
            t.status.as_str(),
            serde_json::to_string(stored)?,
            t.created_at,
            t.updated_at,
        ],
    )?;
    Ok(())
}

pub(super) fn insert_trigger(c: &Connection, record: &TriggerRecord) -> Result<(), HubError> {
    c.execute(
        "INSERT OR IGNORE INTO inbox_triggers(id, source_id, event_id, json, received_at) VALUES(?1, ?2, ?3, ?4, ?5)",
        params![
            record.id,
            record.source_id,
            record.event_id,
            serde_json::to_string(record)?,
            record.received_at,
        ],
    )?;
    Ok(())
}

pub(super) fn is_active_status(status: TicketStatus) -> bool {
    status.is_active()
}
