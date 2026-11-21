use super::{
    params, AgentId, Connection, HubError, PermissionDecision, SessionId, ToolCallView,
    TurnSegment, TurnView, Value,
};

pub(super) const FULL_TURNS: usize = 8;
const SLIM_TEXT: usize = 256;
const FULL_TEXT: usize = 16 * 1024;

pub(super) fn trim_turn(turn: &mut TurnView) {
    for tool in &mut turn.tool_calls {
        for text in [tool.raw_input.as_mut(), tool.raw_output.as_mut()]
            .into_iter()
            .flatten()
        {
            clip_to(text, FULL_TEXT);
        }
        for content in &mut tool.content {
            if let harness_core_api::ToolContent::Text { text } = content {
                clip_to(text, FULL_TEXT);
            }
        }
    }
}

fn clip(text: &mut String) {
    clip_to(text, SLIM_TEXT);
}

fn clip_to(text: &mut String, limit: usize) {
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n…");
        text.shrink_to_fit();
    }
}

pub(super) fn slim_turn(turn: &mut TurnView) {
    {
        for tool in &mut turn.tool_calls {
            tool.raw_output = None;
            if let Some(input) = tool.raw_input.as_mut() {
                clip(input);
            }
            for content in &mut tool.content {
                match content {
                    harness_core_api::ToolContent::Text { text } => clip(text),
                    harness_core_api::ToolContent::Diff {
                        path,
                        old_text,
                        new_text,
                    } => {
                        let size = new_text.len() + old_text.as_ref().map_or(0, String::len);
                        if size > SLIM_TEXT * 2 {
                            *content = harness_core_api::ToolContent::Text {
                                text: format!("Edited {path} (diff not kept for older turns)"),
                            };
                        }
                    }
                    harness_core_api::ToolContent::Terminal { .. } => {}
                }
            }
            tool.content.shrink_to_fit();
        }
    }
    turn.tool_calls.shrink_to_fit();
    for thought in &mut turn.thoughts {
        clip(&mut thought.text);
    }
    if let Some(last) = turn.attempts.last_mut() {
        clip(&mut last.partial_text);
    }
}

/// Folds one stored turn event into the view.
pub(super) fn apply_event(
    turn: &mut TurnView,
    marks: &mut Vec<ToolMark>,
    kind: &str,
    value: Value,
) {
    match kind {
        "tool_call" => {
            let mark = ToolMark::from_payload(&value);
            if let Ok(tool) = serde_json::from_value::<ToolCallView>(value) {
                if !marks.iter().any(|m| m.tool_call_id == tool.tool_call_id) {
                    marks.push(ToolMark {
                        tool_call_id: tool.tool_call_id.clone(),
                        ..mark
                    });
                }
                merge_tool(&mut turn.tool_calls, tool);
            }
        }
        "permission" => {
            let title = value
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if let Some(Ok(decision)) = value
                .get("decision")
                .cloned()
                .map(serde_json::from_value::<PermissionDecision>)
            {
                turn.permissions.push((title, decision));
            }
        }
        "steer" => {
            if let Some(text) = value.get("text").and_then(Value::as_str) {
                marks.push(ToolMark {
                    steer: Some(text.to_owned()),
                    ..ToolMark::from_payload(&value)
                });
            }
        }
        "plan" => {
            if let Ok(entries) = serde_json::from_value(value) {
                turn.plan = entries;
            }
        }
        "thought" => {
            if let Ok(thought) = serde_json::from_value(value) {
                turn.thoughts.push(thought);
            }
        }
        _ => {}
    }
}

/// Inserts or merges a tool call by id (later non-empty fields win).
pub(crate) fn merge_tool(tools: &mut Vec<ToolCallView>, tool: ToolCallView) {
    if let Some(existing) = tools
        .iter_mut()
        .find(|t| t.tool_call_id == tool.tool_call_id)
    {
        if !tool.title.is_empty() {
            existing.title = tool.title;
        }
        if tool.kind.is_some() {
            existing.kind = tool.kind;
        }
        if tool.status.is_some() {
            existing.status = tool.status;
        }
        if tool.raw_input.is_some() {
            existing.raw_input = tool.raw_input;
        }
        if !tool.content.is_empty() {
            existing.content = tool.content;
        }
        if !tool.locations.is_empty() {
            existing.locations = tool.locations;
        }
        if tool.raw_output.is_some() {
            existing.raw_output = tool.raw_output;
        }
    } else {
        tools.push(tool);
    }
}

/// Where a tool call first appeared in a turn's live stream (spec §15.1 request 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolMark {
    pub tool_call_id: String,
    /// Attempt number; None for rows written before API 0.2.0.
    pub attempt: Option<u32>,
    /// Byte length of the attempt's assistant text when the call appeared.
    pub text_offset: Option<usize>,
    pub steer: Option<String>,
}

impl ToolMark {
    pub(super) fn from_payload(value: &Value) -> Self {
        Self {
            tool_call_id: String::new(),
            attempt: value
                .get("attempt")
                .and_then(Value::as_u64)
                .and_then(|a| u32::try_from(a).ok()),
            text_offset: value
                .get("textOffset")
                .and_then(Value::as_u64)
                .and_then(|o| usize::try_from(o).ok()),
            steer: None,
        }
    }
}

/// Ordered text/tool segments: tools of earlier attempts first, then the last attempt's text split at the
/// offsets where its tool calls appeared; marks without an offset go after the text.
pub(crate) fn build_segments(
    text: &str,
    last_attempt: u32,
    marks: &[ToolMark],
) -> Vec<TurnSegment> {
    let mut segments = Vec::new();
    let tool = |m: &ToolMark| match &m.steer {
        Some(text) => TurnSegment::Steer { text: text.clone() },
        None => TurnSegment::Tool {
            tool_call_id: m.tool_call_id.clone(),
        },
    };
    for mark in marks
        .iter()
        .filter(|m| m.attempt.is_some_and(|a| a < last_attempt))
    {
        segments.push(tool(mark));
    }
    let mut current: Vec<(usize, &ToolMark)> = marks
        .iter()
        .filter(|m| m.attempt.map_or(true, |a| a >= last_attempt))
        .map(|m| (m.text_offset.unwrap_or(usize::MAX), m))
        .collect();
    current.sort_by_key(|(offset, _)| *offset);
    let mut position = 0;
    for (offset, mark) in current {
        let mut cut = offset.min(text.len());
        while cut > position && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        if cut > position {
            segments.push(TurnSegment::Text {
                text: text[position..cut].to_owned(),
            });
            position = cut;
        }
        if offset == usize::MAX && position < text.len() {
            segments.push(TurnSegment::Text {
                text: text[position..].to_owned(),
            });
            position = text.len();
        }
        segments.push(tool(mark));
    }
    if position < text.len() {
        segments.push(TurnSegment::Text {
            text: text[position..].to_owned(),
        });
    }
    segments
}

/// Distinct agents that served a session, in order of first use (attempts first, then turn agents).
pub(crate) fn agents_used(c: &Connection, session: &SessionId) -> Result<Vec<AgentId>, HubError> {
    let mut statement = c.prepare(
        "SELECT a.agent_id FROM turn_attempts a JOIN turns t ON t.id=a.turn_id
         WHERE t.session_id=?1 ORDER BY t.seq, a.attempt",
    )?;
    let mut agents: Vec<String> = statement
        .query_map(params![session.as_str()], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut statement = c.prepare(
        "SELECT agent_id FROM turns WHERE session_id=?1 AND agent_id IS NOT NULL ORDER BY seq",
    )?;
    agents.extend(
        statement
            .query_map(params![session.as_str()], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?,
    );
    let mut out: Vec<AgentId> = Vec::new();
    for agent in agents {
        if !out.iter().any(|a| a.as_str() == agent) {
            out.push(AgentId::new(agent));
        }
    }
    Ok(out)
}
