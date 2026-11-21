use super::{params, Connection, GroupColor, HubError, SessionGroup, SessionId};

pub(crate) fn list_groups(c: &Connection) -> Result<Vec<SessionGroup>, HubError> {
    let mut statement =
        c.prepare("SELECT id,name,color,position FROM session_groups ORDER BY position, id")?;
    let rows = statement
        .query_map([], |r| {
            Ok(SessionGroup {
                id: r.get(0)?,
                name: r.get(1)?,
                color: GroupColor::parse(&r.get::<_, String>(2)?),
                position: r.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn insert_group(c: &Connection, group: &SessionGroup) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO session_groups(id,name,color,position) VALUES(?1,?2,?3,?4)",
        params![group.id, group.name, group.color.as_str(), group.position],
    )?;
    Ok(())
}

pub(crate) fn update_group(c: &Connection, group: &SessionGroup) -> Result<(), HubError> {
    c.execute(
        "UPDATE session_groups SET name=?2, color=?3, position=?4 WHERE id=?1",
        params![group.id, group.name, group.color.as_str(), group.position],
    )?;
    Ok(())
}

/// Deletes a group and ungroups its sessions (one transaction). Returns the ungrouped sessions.
pub(crate) fn delete_group(c: &Connection, id: &str) -> Result<Vec<SessionId>, HubError> {
    let tx = c.unchecked_transaction()?;
    let members = {
        let mut statement = tx.prepare("SELECT id FROM sessions WHERE group_id=?1")?;
        let rows = statement
            .query_map(params![id], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    tx.execute(
        "UPDATE sessions SET group_id=NULL WHERE group_id=?1",
        params![id],
    )?;
    tx.execute("DELETE FROM session_groups WHERE id=?1", params![id])?;
    tx.commit()?;
    Ok(members.into_iter().map(SessionId::new).collect())
}

pub(crate) fn set_group_positions(c: &Connection, ids: &[String]) -> Result<(), HubError> {
    let tx = c.unchecked_transaction()?;
    for (position, id) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE session_groups SET position=?2 WHERE id=?1",
            params![id, i64::try_from(position).unwrap_or(i64::MAX)],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Moves sessions into a group on top of the section: the first id gets the highest order.
pub(crate) fn move_sessions(
    c: &Connection,
    ids: &[SessionId],
    group: Option<&str>,
    now: i64,
) -> Result<(), HubError> {
    let tx = c.unchecked_transaction()?;
    let count = i64::try_from(ids.len()).unwrap_or(i64::MAX);
    // 修复：同一毫秒内连续两次"移到顶部"会得到相同的 now_ms，后移的会话不在最上面（phase9-chat 门禁复现）。
    // 以现有最大排序值 + 1 为下限，保证每次移动都严格位于所有已有会话之上。
    let highest: Option<i64> =
        tx.query_row("SELECT MAX(sort_order) FROM sessions", [], |row| row.get(0))?;
    let now = highest.map_or(now, |h| now.max(h.saturating_add(1)));
    for (index, id) in ids.iter().enumerate() {
        let order = now + count - i64::try_from(index).unwrap_or(0);
        tx.execute(
            "UPDATE sessions SET group_id=?2, sort_order=?3 WHERE id=?1",
            params![id.as_str(), group, order],
        )?;
    }
    tx.commit()?;
    Ok(())
}
