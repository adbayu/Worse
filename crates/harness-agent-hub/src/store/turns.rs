use super::{
    apply_event, build_segments, params, parse_status, slim_turn, status_str, trim_turn, AgentId,
    Connection, ErrorClass, HubError, SessionId, ToolMark, TurnAttempt, TurnId, TurnView, Value,
    FULL_TURNS,
};

pub(crate) fn insert_turn(
    c: &Connection,
    session: &SessionId,
    turn: &TurnView,
    client_message_id: Option<&str>,
) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO turns(id,session_id,seq,client_message_id,user_text,agent_id,assistant_text,status,created_at,
                           context_json,origin_json)
         VALUES(?1,?2,?3,?4,?5,?6,'',?7,?8,?9,?10)",
        params![
            turn.id.as_str(),
            session.as_str(),
            turn.seq,
            client_message_id,
            turn.user_text,
            turn.agent.as_ref().map(AgentId::as_str),
            status_str(turn.status),
            turn.created_at,
            crate::store_chat::json_list(&turn.context)?,
            crate::store_chat::json_opt(turn.origin.as_ref())?
        ],
    )?;
    Ok(())
}

pub(crate) fn update_turn(c: &Connection, turn: &TurnView) -> Result<(), HubError> {
    c.execute(
        "UPDATE turns SET agent_id=?2, assistant_text=?3, status=?4, stop_reason=?5, error=?6, finished_at=?7,
           usage_json=?8, context_usage_json=?9 WHERE id=?1",
        params![
            turn.id.as_str(),
            turn.agent.as_ref().map(AgentId::as_str),
            turn.assistant_text,
            status_str(turn.status),
            turn.stop_reason,
            turn.error,
            turn.finished_at,
            crate::store_chat::json_opt(turn.usage.as_ref())?,
            crate::store_chat::json_opt(turn.context_usage.as_ref())?
        ],
    )?;
    Ok(())
}

pub(crate) fn upsert_attempt(
    c: &Connection,
    turn: &TurnId,
    attempt: &TurnAttempt,
) -> Result<(), HubError> {
    let (class, resets_at) = match &attempt.error_class {
        Some(class) => (
            Some(serde_json::to_string(class)?),
            match class {
                ErrorClass::QuotaExhausted { resets_at } => *resets_at,
                _ => None,
            },
        ),
        None => (None, None),
    };
    c.execute(
        "INSERT INTO turn_attempts(turn_id,attempt,agent_id,status,partial_text,error_class,error,resets_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(turn_id,attempt) DO UPDATE SET status=?4, partial_text=?5, error_class=?6, error=?7, resets_at=?8",
        params![
            turn.as_str(),
            attempt.attempt,
            attempt.agent.as_str(),
            attempt.status,
            attempt.partial_text,
            class,
            attempt.error,
            resets_at
        ],
    )?;
    Ok(())
}

pub(crate) fn append_turn_event(
    c: &Connection,
    turn: &TurnId,
    kind: &str,
    payload: &Value,
) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO turn_events(turn_id,idx,kind,payload)
         VALUES(?1,(SELECT COALESCE(MAX(idx),0)+1 FROM turn_events WHERE turn_id=?1),?2,?3)",
        params![turn.as_str(), kind, payload.to_string()],
    )?;
    Ok(())
}

/// Turns of a session with attempts, tool calls and permission decisions; also returns client ids.
pub(crate) fn load_turns(
    c: &Connection,
    session: &SessionId,
) -> Result<Vec<(TurnView, Option<String>)>, HubError> {
    load_turns_inner(c, session, false)
}

pub(crate) fn load_turns_slim(
    c: &Connection,
    session: &SessionId,
) -> Result<Vec<(TurnView, Option<String>)>, HubError> {
    load_turns_inner(c, session, true)
}

fn load_turns_inner(
    c: &Connection,
    session: &SessionId,
    slim: bool,
) -> Result<Vec<(TurnView, Option<String>)>, HubError> {
    let mut statement = c.prepare(
        "SELECT id,seq,user_text,agent_id,assistant_text,status,stop_reason,error,created_at,finished_at,client_message_id,
                context_json,usage_json,context_usage_json,origin_json
         FROM turns WHERE session_id=?1 ORDER BY seq",
    )?;
    let mut turns = statement
        .query_map(params![session.as_str()], |r| {
            Ok((
                TurnView {
                    id: TurnId::new(r.get::<_, String>(0)?),
                    seq: r.get(1)?,
                    user_text: r.get(2)?,
                    agent: r.get::<_, Option<String>>(3)?.map(AgentId::new),
                    assistant_text: r.get(4)?,
                    status: parse_status(&r.get::<_, String>(5)?),
                    stop_reason: r.get(6)?,
                    error: r.get(7)?,
                    attempts: Vec::new(),
                    tool_calls: Vec::new(),
                    permissions: Vec::new(),
                    created_at: r.get(8)?,
                    finished_at: r.get(9)?,
                    segments: Vec::new(),
                    context: crate::store_chat::parse_json(r.get::<_, Option<String>>(11)?)
                        .unwrap_or_default(),
                    plan: Vec::new(),
                    usage: crate::store_chat::parse_json(r.get::<_, Option<String>>(12)?),
                    context_usage: crate::store_chat::parse_json(r.get::<_, Option<String>>(13)?),
                    origin: crate::store_chat::parse_json(r.get::<_, Option<String>>(14)?),
                    thoughts: Vec::new(),
                },
                r.get::<_, Option<String>>(10)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut attempts_statement = c.prepare(
        "SELECT attempt,agent_id,status,partial_text,error_class,error FROM turn_attempts WHERE turn_id=?1 ORDER BY attempt",
    )?;
    let mut events_statement =
        c.prepare("SELECT kind,payload FROM turn_events WHERE turn_id=?1 ORDER BY idx")?;
    let keep_from = if slim {
        turns.len().saturating_sub(FULL_TURNS)
    } else {
        0
    };
    for (index, (turn, _)) in turns.iter_mut().enumerate() {
        turn.attempts = attempts_statement
            .query_map(params![turn.id.as_str()], |r| {
                Ok(TurnAttempt {
                    attempt: r.get(0)?,
                    agent: AgentId::new(r.get::<_, String>(1)?),
                    status: r.get(2)?,
                    partial_text: r.get(3)?,
                    error_class: r
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    error: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let events = events_statement
            .query_map(params![turn.id.as_str()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut marks: Vec<ToolMark> = Vec::new();
        for (kind, payload) in events {
            if let Ok(value) = serde_json::from_str::<Value>(&payload) {
                apply_event(turn, &mut marks, &kind, value);
            }
        }
        let last_attempt = turn.attempts.last().map_or(1, |a| a.attempt);
        turn.segments = build_segments(&turn.assistant_text, last_attempt, &marks);
        if index < keep_from {
            slim_turn(turn);
        } else if slim {
            trim_turn(turn);
        }
    }
    Ok(turns)
}
