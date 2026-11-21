use super::*;

/// A plain `data:` chunk in the `OpenAI` shape.
pub(super) fn chunk(agent: &Agent, id: &str, turn: &mut Turn, value: &Value) -> Result<(), Value> {
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| error.to_string(), str::to_owned);
        return Err(failure(agent, &message));
    }
    if let Some(usage) = value.get("usage") {
        let n = |keys: &[&str]| {
            keys.iter()
                .find_map(|k| usage.get(*k).and_then(Value::as_u64))
                .unwrap_or(0)
        };
        turn.input_tokens = turn.input_tokens.max(n(&["prompt_tokens", "input_tokens"]));
        turn.output_tokens = turn
            .output_tokens
            .max(n(&["completion_tokens", "output_tokens"]));
    }
    let Some(choice) = value.pointer("/choices/0") else {
        return Ok(());
    };
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        turn.finish = Some(reason.to_owned());
    }
    let Some(delta) = choice.get("delta") else {
        return Ok(());
    };
    if let Some(text) = delta
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    {
        agent.update(id, &json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": text}}));
    }
    if let Some(text) = delta
        .get("content")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    {
        turn.content.push_str(text);
        agent.update(id, &json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}));
    }
    Ok(())
}

/// `hermes.tool.progress`: a tool row that starts and later completes.
pub(super) fn tool_progress(agent: &Agent, id: &str, turn: &mut Turn, value: &Value) {
    let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default();
    let call = text("toolCallId");
    if call.is_empty() {
        return;
    }
    let tool = text("tool");
    if text("status") == "running" {
        let label = if text("label").is_empty() {
            tool
        } else {
            text("label")
        };
        turn.open.insert(call.to_owned());
        agent.update(
            id,
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": call,
                "title": label,
                "kind": tool_kind(tool),
                "status": "in_progress"
            }),
        );
    } else if turn.open.remove(call) {
        agent.update(id, &json!({"sessionUpdate": "tool_call_update", "toolCallId": call, "status": "completed"}));
    }
}

/// Tools still shown as running when the turn ends get a final status.
pub(super) fn close_tools(agent: &Agent, id: &str, turn: &mut Turn, status: &str) {
    for call in turn.open.drain() {
        agent.update(
            id,
            &json!({"sessionUpdate": "tool_call_update", "toolCallId": call, "status": status}),
        );
    }
}

/// `approval.request`: asks the user through ACP and tells the gateway the answer. No answer (the client went
/// away) denies.
pub(super) async fn approval(agent: &Agent, id: &str, turn: &mut Turn, value: &Value) {
    let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default();
    let run = text("run_id");
    if run.is_empty() {
        return;
    }
    turn.approvals += 1;
    let command = text("command");
    let description = text("description");
    let title = if description.is_empty() {
        "Hermes asks for approval"
    } else {
        description
    };
    let choices: Vec<String> = value
        .get("choices")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|c| c.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let call = format!("hermes-approval-{}", turn.approvals);
    let mut tool = json!({
        "toolCallId": call,
        "title": title,
        "kind": "execute",
        "status": "pending"
    });
    if !command.is_empty() {
        tool["content"] =
            json!([{"type": "content", "content": {"type": "text", "text": command}}]);
        tool["rawInput"] = json!({"command": command});
    }
    let response = agent
        .request(
            "session/request_permission",
            json!({"sessionId": id, "toolCall": tool, "options": approval_options(&choices)}),
        )
        .await;
    let outcome = response.get("outcome").cloned().unwrap_or(Value::Null);
    let choice = if outcome.get("outcome").and_then(Value::as_str) == Some("selected") {
        outcome
            .get("optionId")
            .and_then(Value::as_str)
            .unwrap_or("deny")
            .to_owned()
    } else {
        "deny".to_owned()
    };
    let mut body = json!({"choice": choice});
    if let Some(request_id) = value
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
    {
        body["request_id"] = json!(request_id);
    }
    let mut request = agent
        .http
        .post(format!("{}/runs/{run}/approval", agent.provider.base_url))
        .timeout(Duration::from_secs(20))
        .json(&body);
    if !agent.provider.api_key.is_empty() {
        request = request.bearer_auth(&agent.provider.api_key);
    }
    // A failed answer leaves the run waiting; the gateway times the approval out by itself.
    let _ = request.send().await;
    // The row of the request itself is settled now; the command, if allowed, shows as its own tool row.
    let status = if choice == "deny" {
        "failed"
    } else {
        "completed"
    };
    agent.update(
        id,
        &json!({"sessionUpdate": "tool_call_update", "toolCallId": call, "status": status}),
    );
}

/// Ends the turn: keeps the answer for replay and reports why it stopped.
pub(super) fn finish(
    agent: &Agent,
    id: &str,
    session: &mut Session,
    mut turn: Turn,
    cancelled: bool,
) -> Value {
    close_tools(
        agent,
        id,
        &mut turn,
        if cancelled { "failed" } else { "completed" },
    );
    if !turn.content.trim().is_empty() {
        session
            .messages
            .push(json!({"role": "assistant", "content": turn.content}));
    }
    if cancelled || turn.finish.as_deref() == Some("cancelled") {
        return json!({"stopReason": "cancelled"});
    }
    let stop = if turn.finish.as_deref() == Some("length") {
        "max_tokens"
    } else {
        "end_turn"
    };
    json!({"stopReason": stop, "usage": {
        "inputTokens": turn.input_tokens,
        "outputTokens": turn.output_tokens,
        "totalTokens": turn.input_tokens + turn.output_tokens
    }})
}
