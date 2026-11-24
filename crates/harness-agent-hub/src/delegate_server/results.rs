use super::{
    json, now_ms, preview, store, AgentId, Arc, HarnessCore, Hub, ScopeView, SessionId,
    SubagentStatus, SubagentView, TurnStatus, Value,
};

/// Coordination note put before a new subagent's first task (§9.8.1).
pub(super) const PREAMBLE: &str = "[Harness] You are a subagent working for another agent in this workspace; other agents may be editing files at the same time. Before you edit files, call the claim_paths tool (harness-delegate) with the files or folders you will change; if a path is held by someone else, do not edit it and use `notify_parent` to explain. Use `notify_parent` for important progress or when you are blocked. Your final answer is returned to the parent agent.";

pub(super) async fn turn_ended(
    hub: &Arc<Hub>,
    child: &SessionId,
    turn: &harness_core_api::TurnId,
) -> bool {
    hub.open_session(child.clone()).await.is_ok_and(|d| {
        d.turns.iter().any(|t| {
            &t.id == turn
                && !matches!(
                    t.status,
                    TurnStatus::Queued | TurnStatus::Running | TurnStatus::Paused
                )
        })
    })
}

pub(super) async fn result(
    hub: &Arc<Hub>,
    child: &SessionId,
    turn_id: &harness_core_api::TurnId,
) -> Result<Value, String> {
    let detail = hub
        .open_session(child.clone())
        .await
        .map_err(|e| e.to_string())?;
    let turn = detail
        .turns
        .iter()
        .find(|t| &t.id == turn_id)
        .ok_or("the subagent turn disappeared")?;
    let agent = turn
        .agent
        .clone()
        .unwrap_or_else(|| detail.summary.active_agent.clone());
    let outcome = match turn.status {
        TurnStatus::Completed => "done",
        TurnStatus::Cancelled => "cancelled",
        _ => "failed",
    };
    if let Some(mut view) = hub.ctx.subagents.get(child) {
        view.result_preview = preview(&turn.assistant_text);
        // 修复：取的是旧轮次（已取消）的结果时，若子会话已开始新一轮（续聊/Steer），不能把状态改回终态。
        let newer_active = detail.turns.iter().any(|t| {
            &t.id != turn_id
                && matches!(
                    t.status,
                    TurnStatus::Queued | TurnStatus::Running | TurnStatus::Paused
                )
        });
        if view.status.is_active() && !newer_active {
            // 防御：TurnFinished 尚未被 tracker 处理时也要给出终态。
            view.status = if outcome == "done" {
                SubagentStatus::Done
            } else if outcome == "cancelled" {
                SubagentStatus::Cancelled
            } else {
                SubagentStatus::Failed
            };
            view.finished_at = Some(now_ms());
        }
        hub.ctx.put_subagent(view).await;
    }
    Ok(json!({
        "sessionId": child,
        "agent": agent,
        "agentName": hub.ctx.display_name(&agent),
        "outcome": outcome,
        "error": turn.error,
        "text": turn.assistant_text,
    }))
}

/// The view registered for a new subagent before its session becomes visible.
pub(crate) fn new_view(
    child: &SessionId,
    parent: &SessionId,
    agent: &AgentId,
    title: &str,
) -> SubagentView {
    SubagentView {
        session_id: child.clone(),
        parent_id: parent.clone(),
        agent: agent.clone(),
        title: title.to_owned(),
        status: SubagentStatus::Running,
        activity: String::new(),
        started_at: now_ms(),
        finished_at: None,
        result_preview: String::new(),
        scope: None,
    }
}

/// The scope a delegation keeps: the parent's scope, narrowed by the optional `scope` argument (§9.8.2).
pub(super) fn delegation_scope(
    hub: &Arc<Hub>,
    parent: &store::SessionRow,
    args: &Value,
) -> Result<ScopeView, String> {
    let inherited = args["session_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .and_then(|id| hub.ctx.scope_of(&SessionId::new(id)))
        .or_else(|| hub.ctx.scope_of(&parent.id));
    let request = match args.get("scope") {
        Some(value) if !value.is_null() => requested_scope(value)?,
        _ => inherited.clone().unwrap_or_else(|| ScopeView {
            paths: Vec::new(),
            tools: Vec::new(),
            network: true,
            expires_at: None,
        }),
    };
    crate::delegate::narrow(inherited.as_ref(), &request, now_ms())
}

fn requested_scope(value: &Value) -> Result<ScopeView, String> {
    let mut paths = Vec::new();
    for path in value["paths"].as_array().into_iter().flatten() {
        let normalized = crate::delegate_coord::normalize(path.as_str().unwrap_or_default())?;
        if !paths.contains(&normalized) {
            paths.push(normalized);
        }
    }
    let tools = value["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>();
    let network = match value.get("network") {
        None | Some(Value::Null) => true,
        Some(flag) => flag.as_bool().unwrap_or(true),
    };
    let minutes = value["minutes"].as_i64().unwrap_or_default();
    Ok(ScopeView {
        paths,
        tools,
        network,
        expires_at: (minutes > 0).then(|| now_ms() + minutes * 60_000),
    })
}

/// Records `scope` on a subagent's view (the only write path for the registry).
pub(super) async fn set_scope(hub: &Arc<Hub>, child: &SessionId, scope: &ScopeView) {
    if let Some(mut view) = hub.ctx.subagents.get(child) {
        view.scope = Some(scope.clone());
        hub.ctx.put_subagent(view).await;
    }
}
