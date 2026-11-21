use super::{
    now_ms, params, parse_json, session_cost, BoardLane, CardView, Connection, HubError,
    OptionalExtension, Row, SessionId, COLUMNS,
};

pub(crate) fn rows_of(c: &Connection, key: &str) -> Result<Vec<Row>, HubError> {
    let mut statement = c.prepare(&format!(
        "SELECT {COLUMNS} FROM kanban_cards WHERE workspace_key=?1"
    ))?;
    let rows = statement
        .query_map(params![key], read_row)?
        .filter_map(Result::ok)
        .collect();
    Ok(rows)
}

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let json: String = r.get(2)?;
    let lane: String = r.get(3)?;
    let position: i64 = r.get(4)?;
    Ok(Row {
        id: r.get(0)?,
        workspace_key: r.get(1)?,
        data: serde_json::from_str(&json).unwrap_or_default(),
        lane: BoardLane::parse(&lane).unwrap_or_default(),
        position: u32::try_from(position.max(0)).unwrap_or(u32::MAX),
        session_id: r.get::<_, Option<String>>(5)?.map(SessionId::new),
        updated_at: r.get(6)?,
    })
}

pub(crate) fn load(c: &Connection, id: &str) -> Result<Option<Row>, HubError> {
    Ok(c.query_row(
        &format!("SELECT {COLUMNS} FROM kanban_cards WHERE id=?1"),
        params![id],
        read_row,
    )
    .optional()?)
}

pub(super) fn by_session(c: &Connection, session: &SessionId) -> Result<Vec<Row>, HubError> {
    let mut statement = c.prepare(&format!(
        "SELECT {COLUMNS} FROM kanban_cards WHERE session_id=?1"
    ))?;
    let rows = statement
        .query_map(params![session.as_str()], read_row)?
        .filter_map(Result::ok)
        .collect();
    Ok(rows)
}

pub(crate) fn lane_ids(
    c: &Connection,
    key: &str,
    lane: BoardLane,
) -> Result<Vec<String>, HubError> {
    let mut statement = c.prepare(
        "SELECT id FROM kanban_cards WHERE workspace_key=?1 AND lane=?2 ORDER BY position, updated_at",
    )?;
    let ids = statement
        .query_map(params![key, lane.as_str()], |r| r.get::<_, String>(0))?
        .filter_map(Result::ok)
        .collect();
    Ok(ids)
}

pub(super) fn renumber(c: &Connection, ids: &[String]) -> Result<(), HubError> {
    for (index, id) in ids.iter().enumerate() {
        c.execute(
            "UPDATE kanban_cards SET position=?2 WHERE id=?1",
            params![id, i64::try_from(index).unwrap_or(i64::MAX)],
        )?;
    }
    Ok(())
}

pub(crate) fn save(c: &Connection, row: &Row) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO kanban_cards(id, workspace_key, json, lane, position, session_id, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET json=?3, lane=?4, position=?5, session_id=?6, updated_at=?7",
        params![
            row.id,
            row.workspace_key,
            serde_json::to_string(&row.data)?,
            row.lane.as_str(),
            i64::from(row.position),
            row.session_id.as_ref().map(SessionId::as_str),
            row.updated_at,
        ],
    )?;
    Ok(())
}

pub(crate) fn place(
    c: &Connection,
    row: &mut Row,
    lane: BoardLane,
    index: Option<usize>,
) -> Result<(), HubError> {
    let old = lane_ids(c, &row.workspace_key, row.lane)?;
    let rest: Vec<String> = old.into_iter().filter(|id| *id != row.id).collect();
    if row.lane != lane {
        renumber(c, &rest)?;
    }
    let mut target = if row.lane == lane {
        rest
    } else {
        lane_ids(c, &row.workspace_key, lane)?
            .into_iter()
            .filter(|id| *id != row.id)
            .collect()
    };
    let at = index.unwrap_or(target.len()).min(target.len());
    target.insert(at, row.id.clone());
    row.lane = lane;
    row.position = u32::try_from(at).unwrap_or(u32::MAX);
    row.updated_at = now_ms();
    save(c, row)?;
    renumber(c, &target)
}

pub(super) fn view(c: &Connection, row: Row) -> Result<CardView, HubError> {
    let (branch, cost) = match &row.session_id {
        Some(session) => {
            let worktree: Option<String> = c
                .query_row(
                    "SELECT worktree_json FROM sessions WHERE id=?1",
                    params![session.as_str()],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let branch = parse_json::<harness_core_api::WorktreeInfo>(worktree).map(|w| w.branch);
            (branch, session_cost(c, session)?.map(|cost| cost.total))
        }
        None => (None, None),
    };
    Ok(CardView {
        id: row.id,
        lane: row.lane,
        position: row.position,
        title: row.data.title,
        body: row.data.body,
        session_id: row.session_id,
        state: row.data.state,
        agent: row.data.agent,
        branch,
        cost,
        issue_number: row.data.issue_number,
        url: row.data.url,
        updated_at: row.updated_at,
    })
}
