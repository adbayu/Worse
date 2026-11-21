use super::{
    params, AgentId, Connection, HandleRow, HashMap, HubError, OptionalExtension, QuotaState,
    SessionId,
};

pub(crate) fn upsert_handle(
    c: &Connection,
    session: &SessionId,
    handle: &HandleRow,
) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO agent_handles(session_id,agent_id,native_session_id,last_seq) VALUES(?1,?2,?3,?4)
         ON CONFLICT(session_id,agent_id) DO UPDATE SET native_session_id=?3, last_seq=?4",
        params![session.as_str(), handle.agent.as_str(), handle.native, handle.last_seq],
    )?;
    Ok(())
}

pub(crate) fn load_handles(
    c: &Connection,
    session: &SessionId,
) -> Result<Vec<HandleRow>, HubError> {
    let mut statement = c.prepare(
        "SELECT agent_id,native_session_id,last_seq FROM agent_handles WHERE session_id=?1",
    )?;
    let rows = statement
        .query_map(params![session.as_str()], |r| {
            Ok(HandleRow {
                agent: AgentId::new(r.get::<_, String>(0)?),
                native: r.get(1)?,
                last_seq: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn load_quota(c: &Connection) -> Result<HashMap<AgentId, QuotaState>, HubError> {
    let mut statement = c.prepare("SELECT agent_id,state,resets_at,reason FROM agent_quota")?;
    let rows = statement
        .query_map([], |r| {
            let agent = AgentId::new(r.get::<_, String>(0)?);
            let state: String = r.get(1)?;
            let quota = if state == "exhausted" {
                QuotaState::Exhausted {
                    resets_at: r.get(2)?,
                    reason: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                }
            } else {
                QuotaState::Ok
            };
            Ok((agent, quota))
        })?
        .collect::<Result<HashMap<_, _>, _>>()?;
    Ok(rows)
}

pub(crate) fn save_quota(
    c: &Connection,
    agent: &AgentId,
    quota: &QuotaState,
    now: i64,
) -> Result<(), HubError> {
    let (state, resets_at, reason) = match quota {
        QuotaState::Ok => ("ok", None, None),
        QuotaState::Exhausted { resets_at, reason } => {
            ("exhausted", *resets_at, Some(reason.as_str()))
        }
    };
    c.execute(
        "INSERT INTO agent_quota(agent_id,state,resets_at,reason,updated_at) VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(agent_id) DO UPDATE SET state=?2, resets_at=?3, reason=?4, updated_at=?5",
        params![agent.as_str(), state, resets_at, reason, now],
    )?;
    Ok(())
}

pub(crate) fn get_setting(c: &Connection, key: &str) -> Result<Option<String>, HubError> {
    Ok(c.query_row(
        "SELECT value FROM settings WHERE key=?1",
        params![key],
        |r| r.get(0),
    )
    .optional()?)
}

pub(crate) fn set_setting(c: &Connection, key: &str, value: &str) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2",
        params![key, value],
    )?;
    Ok(())
}

/// Mode/model/config choices of one session, per agent.
pub(crate) fn load_prefs(
    c: &Connection,
    session: &SessionId,
) -> Result<HashMap<AgentId, crate::options::Prefs>, HubError> {
    let mut statement =
        c.prepare("SELECT agent_id, kind, key, value FROM session_prefs WHERE session_id=?1")?;
    let rows = statement.query_map(params![session.as_str()], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut out: HashMap<AgentId, crate::options::Prefs> = HashMap::new();
    for row in rows {
        let (agent, kind, key, value) = row?;
        let prefs = out.entry(AgentId::new(agent)).or_default();
        match kind.as_str() {
            "mode" => prefs.mode = Some(value),
            "model" => prefs.model = Some(value),
            "config" => {
                prefs.config.insert(key, value);
            }
            _ => {}
        }
    }
    Ok(out)
}

pub(crate) fn save_pref(
    c: &Connection,
    session: &SessionId,
    agent: &AgentId,
    (kind, key, value): (&str, &str, &str),
) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO session_prefs(session_id,agent_id,kind,key,value) VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(session_id,agent_id,kind,key) DO UPDATE SET value=?5",
        params![session.as_str(), agent.as_str(), kind, key, value],
    )?;
    Ok(())
}
