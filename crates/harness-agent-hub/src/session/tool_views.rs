use super::{
    chat, clip_head_tail, json, PermissionOption, PermissionOptionKind, ToolCallStatus,
    ToolCallView, ToolContent, ToolDigest, Value, TOOL_OUTPUT_CHARS,
};

pub(super) fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

/// What the agent already saw of a tool call: its text content, or the raw output as fallback.
///
/// The output is clipped here, before it is kept in a `ContextTurn`: a tool result can be well over a
/// megabyte and `prompt_blocks` builds one digest per call of every turn, so keeping the full text would
/// copy it repeatedly only for [`render_tools`](crate::catchup::render_tools) to cut it again.
pub(super) fn tool_digests(calls: &[ToolCallView]) -> Vec<ToolDigest> {
    calls
        .iter()
        .map(|call| ToolDigest {
            title: call.title.clone(),
            input: call.raw_input.clone().unwrap_or_default(),
            output: clip_head_tail(&tool_output(call), TOOL_OUTPUT_CHARS),
        })
        .collect()
}

fn tool_output(call: &ToolCallView) -> String {
    let mut parts: Vec<String> = Vec::new();
    for item in &call.content {
        match item {
            ToolContent::Text { text } => parts.push(text.clone()),
            ToolContent::Diff { path, new_text, .. } => {
                parts.push(format!("(diff {path}, {} lines)", new_text.lines().count()));
            }
            ToolContent::Terminal { terminal_id } => {
                parts.push(format!("(terminal {terminal_id})"));
            }
        }
    }
    if parts.is_empty() {
        return call.raw_output.clone().unwrap_or_default();
    }
    parts.join("\n")
}

/// Paths a permission request touches, relative to the workspace, for the scope check (§9.8.2).
pub(super) fn scope_paths(workspace: &str, tool: &ToolCallView) -> Vec<String> {
    tool.locations
        .iter()
        .map(|location| crate::delegate::relative_to(workspace, &location.path))
        .collect()
}

pub(super) fn tool_view(value: &Value) -> ToolCallView {
    ToolCallView {
        tool_call_id: value
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        kind: value.get("kind").and_then(Value::as_str).map(str::to_owned),
        status: value
            .get("status")
            .and_then(Value::as_str)
            .and_then(|s| match s {
                "pending" => Some(ToolCallStatus::Pending),
                "in_progress" => Some(ToolCallStatus::InProgress),
                "completed" => Some(ToolCallStatus::Completed),
                "failed" => Some(ToolCallStatus::Failed),
                _ => None,
            }),
        raw_input: value.get("rawInput").map(Value::to_string),
        content: chat::tool_content(value),
        locations: chat::tool_locations(value),
        raw_output: value.get("rawOutput").map(|v| {
            chat::capped(match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
        }),
    }
}

pub(super) fn permission_option(value: &Value) -> Option<PermissionOption> {
    let kind = match value.get("kind").and_then(Value::as_str)? {
        "allow_once" => PermissionOptionKind::AllowOnce,
        "allow_always" => PermissionOptionKind::AllowAlways,
        "reject_once" => PermissionOptionKind::RejectOnce,
        "reject_always" => PermissionOptionKind::RejectAlways,
        _ => return None,
    };
    Some(PermissionOption {
        option_id: value.get("optionId").and_then(Value::as_str)?.to_owned(),
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        kind,
    })
}
