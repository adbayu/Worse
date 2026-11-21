use super::{params, Connection, ForkOrigin, HubError, OptionalExtension, SessionId, TurnId};

/// Deletes turns with `seq >= from_seq` (all turn tables) and every native handle of the session.
pub(crate) fn truncate_turns(
    c: &Connection,
    session: &SessionId,
    from_seq: u32,
) -> Result<(), HubError> {
    let tx = c.unchecked_transaction()?;
    let id = session.as_str();
    for sql in [
        "DELETE FROM turn_events WHERE turn_id IN (SELECT id FROM turns WHERE session_id=?1 AND seq>=?2)",
        "DELETE FROM turn_attempts WHERE turn_id IN (SELECT id FROM turns WHERE session_id=?1 AND seq>=?2)",
        "DELETE FROM turn_snapshots WHERE turn_id IN (SELECT id FROM turns WHERE session_id=?1 AND seq>=?2)",
        "DELETE FROM turns WHERE session_id=?1 AND seq>=?2",
    ] {
        tx.execute(sql, params![id, from_seq])?;
    }
    tx.execute("DELETE FROM agent_handles WHERE session_id=?1", params![id])?;
    tx.commit()?;
    Ok(())
}

/// Copies turns of `from` (seq ≤ `up_to_seq`) to `to` with new ids; attempts and events are copied too.
pub(crate) fn copy_turns(
    c: &Connection,
    from: &SessionId,
    to: &SessionId,
    up_to_seq: u32,
    new_id: &mut dyn FnMut() -> String,
) -> Result<(), HubError> {
    let tx = c.unchecked_transaction()?;
    let turns: Vec<String> = {
        let mut statement = tx.prepare(
            "SELECT id FROM turns WHERE session_id=?1 AND seq<=?2
             AND status NOT IN ('queued','running','paused') ORDER BY seq",
        )?;
        let rows = statement
            .query_map(params![from.as_str(), up_to_seq], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for old in turns {
        let new = new_id();
        tx.execute(
            "INSERT INTO turns(id,session_id,seq,client_message_id,user_text,agent_id,assistant_text,status,
                               stop_reason,error,created_at,finished_at,context_json,usage_json,
                               context_usage_json,origin_json)
             SELECT ?2,?3,seq,NULL,user_text,agent_id,assistant_text,status,stop_reason,error,created_at,
                    finished_at,context_json,usage_json,context_usage_json,origin_json FROM turns WHERE id=?1",
            params![old, new, to.as_str()],
        )?;
        tx.execute(
            "INSERT INTO turn_attempts(turn_id,attempt,agent_id,status,partial_text,error_class,error,resets_at)
             SELECT ?2,attempt,agent_id,status,partial_text,error_class,error,resets_at
             FROM turn_attempts WHERE turn_id=?1",
            params![old, new],
        )?;
        tx.execute(
            "INSERT INTO turn_events(turn_id,idx,kind,payload)
             SELECT ?2,idx,kind,payload FROM turn_events WHERE turn_id=?1",
            params![old, new],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn set_fork_origin(
    c: &Connection,
    session: &SessionId,
    origin: &ForkOrigin,
) -> Result<(), HubError> {
    c.execute(
        "UPDATE sessions SET forked_from_session=?2, forked_from_seq=?3 WHERE id=?1",
        params![
            session.as_str(),
            origin.session_id.as_str(),
            origin.turn_seq
        ],
    )?;
    Ok(())
}

pub(crate) fn set_undone(c: &Connection, turn: &TurnId, undone: bool) -> Result<(), HubError> {
    c.execute(
        "UPDATE turn_snapshots SET undone=?2 WHERE turn_id=?1",
        params![turn.as_str(), i64::from(undone)],
    )?;
    Ok(())
}

pub(crate) fn is_undone(c: &Connection, turn: &TurnId) -> Result<bool, HubError> {
    Ok(c.query_row(
        "SELECT COALESCE(undone,0) FROM turn_snapshots WHERE turn_id=?1",
        params![turn.as_str()],
        |r| r.get::<_, i64>(0),
    )
    .optional()?
    .is_some_and(|v| v != 0))
}
