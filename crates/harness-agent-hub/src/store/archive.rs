use super::{
    params, session_from_row, summary, Connection, HubError, OptionalExtension, SessionId,
    SessionSummary,
};

/// Adds `sessions.archived_at` when missing (additive; `user_version` stays 1).
pub(super) fn ensure_archive_column(c: &Connection) -> Result<(), HubError> {
    let mut statement = c.prepare("SELECT name FROM pragma_table_info('sessions')")?;
    let has = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(Result::ok)
        .any(|name| name == "archived_at");
    if !has {
        c.execute("ALTER TABLE sessions ADD COLUMN archived_at INTEGER", [])?;
    }
    Ok(())
}

/// Keys of removed projects are hidden from unfiltered lists.
pub(super) const NOT_REMOVED: &str =
    "s.workspace_key NOT IN (SELECT workspace_key FROM projects WHERE removed_at IS NOT NULL)";

/// Project rows: (key, path, sessions, archived, `last_used`, `removed_at`).
pub(crate) type ProjectRow = (String, String, u32, u32, Option<i64>, Option<i64>);

pub(crate) fn list_projects(c: &Connection) -> Result<Vec<ProjectRow>, HubError> {
    let mut statement = c.prepare(
        "WITH keys AS (SELECT workspace_key AS k FROM sessions s
                        WHERE NOT EXISTS(SELECT 1 FROM inbox_routers r WHERE r.session_id=s.id)
                       UNION SELECT workspace_key FROM projects)
         SELECT k,
           COALESCE((SELECT workspace_path FROM sessions WHERE workspace_key=k ORDER BY updated_at DESC LIMIT 1),
                    (SELECT workspace_path FROM projects WHERE workspace_key=k), k),
           (SELECT COUNT(*) FROM sessions WHERE workspace_key=k AND archived_at IS NULL),
           (SELECT COUNT(*) FROM sessions WHERE workspace_key=k AND archived_at IS NOT NULL),
           (SELECT MAX(updated_at) FROM sessions WHERE workspace_key=k),
           (SELECT removed_at FROM projects WHERE workspace_key=k)
         FROM keys",
    )?;
    let count = |n: i64| u32::try_from(n).unwrap_or(u32::MAX);
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                count(r.get(2)?),
                count(r.get(3)?),
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Marks a project removed (`Some(ms)`) or restores it (`None`). Returns true when the state changed.
pub(crate) fn set_project_removed(
    c: &Connection,
    key: &str,
    removed_at: Option<i64>,
) -> Result<bool, HubError> {
    let before: Option<Option<i64>> = c
        .query_row(
            "SELECT removed_at FROM projects WHERE workspace_key=?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?;
    if before.flatten().is_some() == removed_at.is_some() {
        return Ok(false);
    }
    c.execute(
        "INSERT INTO projects(workspace_key, workspace_path, removed_at)
         VALUES(?1, COALESCE((SELECT workspace_path FROM sessions WHERE workspace_key=?1
                              ORDER BY updated_at DESC LIMIT 1), ?1), ?2)
         ON CONFLICT(workspace_key) DO UPDATE SET removed_at=?2",
        params![key, removed_at],
    )?;
    Ok(true)
}

pub(crate) fn session_ids_of_project(
    c: &Connection,
    key: &str,
) -> Result<Vec<SessionId>, HubError> {
    let mut statement = c.prepare("SELECT id FROM sessions WHERE workspace_key=?1")?;
    let ids = statement
        .query_map(params![key], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids.into_iter().map(SessionId::new).collect())
}

/// Of `ids`, the sessions that exist and are archived (or not archived when `archived = false`).
pub(crate) fn filter_archived(
    c: &Connection,
    ids: &[SessionId],
    archived: bool,
) -> Result<Vec<SessionId>, HubError> {
    let mut out = Vec::new();
    for id in ids {
        let state: Option<Option<i64>> = c
            .query_row(
                "SELECT archived_at FROM sessions WHERE id=?1",
                params![id.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if state.is_some_and(|at| at.is_some() == archived) {
            out.push(id.clone());
        }
    }
    Ok(out)
}

pub(crate) fn archived_of_project(c: &Connection, key: &str) -> Result<Vec<SessionId>, HubError> {
    let mut statement =
        c.prepare("SELECT id FROM sessions WHERE workspace_key=?1 AND archived_at IS NOT NULL")?;
    let ids = statement
        .query_map(params![key], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids.into_iter().map(SessionId::new).collect())
}

/// Sets (`Some`) or clears (`None`) `archived_at`; returns how many rows changed state.
pub(crate) fn set_archived(
    c: &Connection,
    ids: &[SessionId],
    at: Option<i64>,
) -> Result<u32, HubError> {
    let mut changed = 0u32;
    for id in ids {
        let n = if at.is_some() {
            c.execute(
                "UPDATE sessions SET archived_at=?2 WHERE id=?1 AND archived_at IS NULL",
                params![id.as_str(), at],
            )?
        } else {
            c.execute(
                "UPDATE sessions SET archived_at=NULL WHERE id=?1 AND archived_at IS NOT NULL",
                params![id.as_str()],
            )?
        };
        changed += u32::try_from(n).unwrap_or(0);
    }
    Ok(changed)
}

/// Deletes sessions with every dependent row, in one transaction. Returns how many sessions were deleted.
pub(crate) fn delete_sessions(c: &Connection, ids: &[SessionId]) -> Result<u32, HubError> {
    let tx = c.unchecked_transaction()?;
    let mut deleted = 0u32;
    for id in ids {
        let id = id.as_str();
        for sql in [
            "DELETE FROM turn_events WHERE turn_id IN (SELECT id FROM turns WHERE session_id=?1)",
            "DELETE FROM turn_attempts WHERE turn_id IN (SELECT id FROM turns WHERE session_id=?1)",
            "DELETE FROM turn_snapshots WHERE turn_id IN (SELECT id FROM turns WHERE session_id=?1)",
            "DELETE FROM turns WHERE session_id=?1",
            "DELETE FROM agent_handles WHERE session_id=?1",
            "DELETE FROM session_prefs WHERE session_id=?1",
        ] {
            tx.execute(sql, params![id])?;
        }
        deleted += u32::try_from(tx.execute("DELETE FROM sessions WHERE id=?1", params![id])?)
            .unwrap_or(0);
    }
    tx.commit()?;
    Ok(deleted)
}

/// Rows left in any table for `id` (gate check after delete).
pub(crate) fn rows_of_session(c: &Connection, id: &SessionId) -> Result<i64, HubError> {
    Ok(c.query_row(
        "SELECT (SELECT COUNT(*) FROM sessions WHERE id=?1) + (SELECT COUNT(*) FROM turns WHERE session_id=?1)
              + (SELECT COUNT(*) FROM agent_handles WHERE session_id=?1)
              + (SELECT COUNT(*) FROM session_prefs WHERE session_id=?1)",
        params![id.as_str()],
        |r| r.get(0),
    )?)
}

/// One page of archived sessions and the total.
pub(crate) fn list_archived(
    c: &Connection,
    request: &harness_core_api::ListArchived,
    limit: u32,
) -> Result<(Vec<(SessionSummary, i64)>, u32), HubError> {
    let order = match request.sort {
        harness_core_api::ArchiveSort::ArchivedNewest => "s.archived_at DESC, s.id",
        harness_core_api::ArchiveSort::ArchivedOldest => "s.archived_at ASC, s.id",
        harness_core_api::ArchiveSort::Title => "lower(s.title) ASC, s.id",
    };
    let filter = format!(
        "FROM sessions s WHERE s.archived_at IS NOT NULL AND (?1 IS NULL OR s.workspace_key=?1)
         AND (?1 IS NOT NULL OR {NOT_REMOVED}) AND (?2 IS NULL OR lower(s.title) LIKE ?2)"
    );
    let pattern = request
        .query
        .as_deref()
        .filter(|q| !q.trim().is_empty())
        .map(|q| format!("%{}%", q.trim().to_lowercase()));
    let key = request.workspace_key.as_deref();
    let total: i64 = c.query_row(
        &format!("SELECT COUNT(*) {filter}"),
        params![key, pattern],
        |r| r.get(0),
    )?;
    let mut statement = c.prepare(&format!(
        "SELECT s.id,s.title,s.workspace_path,s.workspace_key,s.active_agent,s.created_at,s.updated_at,
                s.archived_at {filter} ORDER BY {order} LIMIT ?3 OFFSET ?4"
    ))?;
    let rows = statement
        .query_map(
            params![key, pattern, i64::from(limit), i64::from(request.offset)],
            |r| Ok((session_from_row(r)?, r.get::<_, i64>(7)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let rows = rows
        .iter()
        .map(|(row, at)| Ok((summary(c, row)?, *at)))
        .collect::<Result<Vec<_>, HubError>>()?;
    Ok((rows, u32::try_from(total).unwrap_or(u32::MAX)))
}
