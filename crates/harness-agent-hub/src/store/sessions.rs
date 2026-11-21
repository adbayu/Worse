use super::{
    agents_used, params, AgentId, Connection, HubError, OptionalExtension, SessionId, SessionRow,
    SessionSummary, NOT_REMOVED,
};

pub(crate) fn insert_session(c: &Connection, row: &SessionRow) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO sessions(id,title,workspace_path,workspace_key,active_agent,created_at,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            row.id.as_str(),
            row.title,
            row.workspace_path,
            row.workspace_key,
            row.active_agent.as_str(),
            row.created_at,
            row.updated_at
        ],
    )?;
    Ok(())
}

pub(crate) fn update_session(c: &Connection, row: &SessionRow) -> Result<(), HubError> {
    c.execute(
        "UPDATE sessions SET title=?2, active_agent=?3, updated_at=?4 WHERE id=?1",
        params![
            row.id.as_str(),
            row.title,
            row.active_agent.as_str(),
            row.updated_at
        ],
    )?;
    Ok(())
}

pub(crate) fn load_session(c: &Connection, id: &SessionId) -> Result<Option<SessionRow>, HubError> {
    Ok(c.query_row(
        "SELECT id,title,workspace_path,workspace_key,active_agent,created_at,updated_at FROM sessions WHERE id=?1",
        params![id.as_str()],
        session_from_row,
    )
    .optional()?)
}

pub(super) fn session_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: SessionId::new(r.get::<_, String>(0)?),
        title: r.get(1)?,
        workspace_path: r.get(2)?,
        workspace_key: r.get(3)?,
        active_agent: AgentId::new(r.get::<_, String>(4)?),
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

pub(crate) fn turn_count(c: &Connection, id: &SessionId) -> Result<u32, HubError> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM turns WHERE session_id=?1",
        params![id.as_str()],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

pub(crate) fn summary(c: &Connection, row: &SessionRow) -> Result<SessionSummary, HubError> {
    let mut summary = SessionSummary {
        id: row.id.clone(),
        title: row.title.clone(),
        workspace_path: row.workspace_path.clone(),
        workspace_key: row.workspace_key.clone(),
        active_agent: row.active_agent.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
        turn_count: turn_count(c, &row.id)?,
        agents_used: agents_used(c, &row.id)?,
        pinned: false,
        group_id: None,
        order: 0,
        unread: false,
        forked_from: None,
        imported_from: None,
        parent_id: None,
        worktree: None,
    };
    crate::store_chat::decorate(c, &mut summary)?;
    Ok(summary)
}

pub(crate) fn list_sessions(
    c: &Connection,
    workspace_key: Option<&str>,
    query: Option<&str>,
    limit: u32,
) -> Result<Vec<SessionSummary>, HubError> {
    let pattern = query.map(|q| format!("%{}%", q.to_lowercase()));
    // Archived sessions and (unless filtered by key) removed projects are not listed (§9.1.5a).
    let mut statement = c.prepare(&format!(
        "SELECT s.id,s.title,s.workspace_path,s.workspace_key,s.active_agent,s.created_at,s.updated_at
         FROM sessions s
         WHERE s.archived_at IS NULL AND (?1 IS NULL OR s.workspace_key=?1)
           AND NOT EXISTS(SELECT 1 FROM subagents g WHERE g.child_id=s.id)
           AND NOT EXISTS(SELECT 1 FROM inbox_routers r WHERE r.session_id=s.id)
           AND (?1 IS NOT NULL OR {NOT_REMOVED})
           AND (?2 IS NULL OR lower(s.title) LIKE ?2 OR EXISTS(
                SELECT 1 FROM turns t WHERE t.session_id=s.id
                AND (lower(t.user_text) LIKE ?2 OR lower(t.assistant_text) LIKE ?2)))
         ORDER BY s.updated_at DESC LIMIT ?3"
    ))?;
    let rows = statement
        .query_map(
            params![workspace_key, pattern, i64::from(limit)],
            session_from_row,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    rows.iter().map(|row| summary(c, row)).collect()
}
