use super::{params, Connection, HubError, OptionalExtension, SessionId, TurnId};

/// Snapshot row of a turn.
#[derive(Debug, Clone, Default)]
pub struct SnapshotRow {
    pub before_tree: Option<String>,
    pub after_tree: Option<String>,
    pub error: Option<String>,
}

pub(crate) fn set_snapshot_before(
    c: &Connection,
    turn: &TurnId,
    tree: Option<&str>,
    error: Option<&str>,
) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO turn_snapshots(turn_id,before_tree,error) VALUES(?1,?2,?3)
         ON CONFLICT(turn_id) DO NOTHING",
        params![turn.as_str(), tree, error],
    )?;
    Ok(())
}

pub(crate) fn set_snapshot_after(
    c: &Connection,
    turn: &TurnId,
    tree: Option<&str>,
    error: Option<&str>,
) -> Result<(), HubError> {
    c.execute(
        "UPDATE turn_snapshots SET after_tree=?2, error=COALESCE(error, ?3) WHERE turn_id=?1",
        params![turn.as_str(), tree, error],
    )?;
    Ok(())
}

pub(crate) fn load_snapshot(
    c: &Connection,
    turn: &TurnId,
) -> Result<Option<SnapshotRow>, HubError> {
    Ok(c.query_row(
        "SELECT before_tree,after_tree,error FROM turn_snapshots WHERE turn_id=?1",
        params![turn.as_str()],
        |r| {
            Ok(SnapshotRow {
                before_tree: r.get(0)?,
                after_tree: r.get(1)?,
                error: r.get(2)?,
            })
        },
    )
    .optional()?)
}

/// Session of a turn.
pub(crate) fn turn_session(c: &Connection, turn: &TurnId) -> Result<Option<SessionId>, HubError> {
    Ok(c.query_row(
        "SELECT session_id FROM turns WHERE id=?1",
        params![turn.as_str()],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(SessionId::new))
}
