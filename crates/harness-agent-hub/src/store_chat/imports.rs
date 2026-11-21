use super::{params, snippet, Connection, HubError, ScheduledPrompt, SessionId};

/// Every `imported_from` value in the store.
pub(crate) fn imported_keys(c: &Connection) -> Result<Vec<String>, HubError> {
    let mut statement =
        c.prepare("SELECT imported_from FROM sessions WHERE imported_from IS NOT NULL")?;
    let rows = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn native_session_ids(c: &Connection) -> Result<Vec<String>, HubError> {
    let mut statement = c.prepare("SELECT DISTINCT native_session_id FROM agent_handles")?;
    let rows = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn set_imported_from(
    c: &Connection,
    session: &SessionId,
    key: &str,
) -> Result<(), HubError> {
    c.execute(
        "UPDATE sessions SET imported_from=?2 WHERE id=?1",
        params![session.as_str(), key],
    )?;
    Ok(())
}

// ---- schedules ------------------------------------------------------------------------------------

pub(crate) fn list_schedules(c: &Connection) -> Result<Vec<ScheduledPrompt>, HubError> {
    let mut statement = c.prepare("SELECT json FROM schedules ORDER BY created_at, id")?;
    let rows = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|json| serde_json::from_str(&json).ok())
        .collect())
}

pub(crate) fn save_schedule(c: &Connection, schedule: &ScheduledPrompt) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO schedules(id,json,created_at) VALUES(?1,?2,?3)
         ON CONFLICT(id) DO UPDATE SET json=?2",
        params![
            schedule.id,
            serde_json::to_string(schedule)?,
            schedule.created_at
        ],
    )?;
    Ok(())
}

pub(crate) fn delete_schedule(c: &Connection, id: &str) -> Result<bool, HubError> {
    Ok(c.execute("DELETE FROM schedules WHERE id=?1", params![id])? > 0)
}

/// Sessions whose transcript contains `pattern` (case-insensitive), newest first, with a short snippet.
pub(crate) fn transcript_matches(
    c: &Connection,
    pattern: &str,
    limit: u32,
) -> Result<Vec<(SessionId, String)>, HubError> {
    let needle = pattern.to_lowercase();
    let mut stmt = c.prepare(
        "SELECT t.session_id, t.user_text, t.assistant_text FROM turns t
         WHERE instr(lower(t.user_text), ?1) > 0 OR instr(lower(t.assistant_text), ?1) > 0
         ORDER BY t.created_at DESC LIMIT 2000",
    )?;
    let rows = stmt.query_map(params![needle], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out: Vec<(SessionId, String)> = Vec::new();
    for row in rows {
        let (id, user, assistant) = row?;
        let id = SessionId::new(id);
        if out.iter().any(|(seen, _)| seen == &id) {
            continue;
        }
        let text = if user.to_lowercase().contains(&needle) {
            user
        } else {
            assistant
        };
        out.push((id, snippet(&text, &needle)));
        if out.len() >= limit as usize {
            break;
        }
    }
    Ok(out)
}
