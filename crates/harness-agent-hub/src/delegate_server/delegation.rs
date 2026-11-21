use super::{
    agent_state, delegation_scope, json, now_ms, quota, result, set_scope, store, task_text, team,
    turn_ended, AgentId, Arc, CreateSession, EventKind, HarnessCore, Hub, OrchestraNode,
    OrchestraRole, RecvError, SendMessage, SessionId, Value, PREAMBLE,
};

/// Runs the task as one turn of a (new or existing) subagent session and waits for its answer.
#[allow(clippy::too_many_lines)]
pub(super) async fn delegate(
    hub: &Arc<Hub>,
    parent: &store::SessionRow,
    args: &Value,
    closed: impl std::future::Future<Output = ()>,
) -> Result<Value, String> {
    let mut task = task_text(args, "task")?;
    let background = args["background"].as_bool() == Some(true);
    quota(hub, parent)?;
    let scope = delegation_scope(hub, parent, args)?;
    let mut agent = args["agent"]
        .as_str()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(AgentId::new);
    let mut role_line = String::new();
    let team = team(hub, parent).await;
    // No agent named: the decision model picks one (and a model), when one is set. Its pick is checked like
    // a named agent below; without a pick the rules decide as before.
    let mut routed: Option<crate::decision::Choice> = None;
    if agent.is_none() && args["session_id"].as_str().map_or(true, str::is_empty) {
        let candidates = candidates(hub, team.as_deref(), &parent.active_agent).await;
        match crate::decision::choose(&hub.ctx.config.home, &task, &candidates).await {
            Ok(choice) => {
                agent = Some(AgentId::new(choice.agent.clone()));
                routed = Some(choice);
            }
            Err(reason) => tracing::debug!(%reason, "delegation picked by rule"),
        }
    }
    if let Some(team) = team {
        let node = if let Some(id) = &agent {
            team.iter()
                .find(|n| &n.agent == id)
                .ok_or_else(|| format!("{id} is not in the Orchestra team; use list_agents"))?
        } else {
            let list = hub.list_agents().await.unwrap_or_default();
            team.iter()
                .filter(|n| n.role == OrchestraRole::Subagent)
                .find(|n| {
                    list.iter()
                        .any(|a| a.id == n.agent && agent_state(a) == "available")
                })
                .ok_or(
                    "no available Subagent in the Orchestra team; pass an agent from list_agents",
                )?
        };
        agent = Some(node.agent.clone());
        role_line = format!("[Harness Orchestra] Your role: {}.", node.role.label());
        if !node.instructions.trim().is_empty() {
            role_line = format!("{role_line}\n{}", node.instructions.trim());
        }
    }
    if let Some(agent) = &agent {
        hub.ctx
            .require_available(agent)
            .map_err(|e| e.to_string())?;
    }
    let title = args["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| task.lines().next().unwrap_or_default())
        .chars()
        .take(60)
        .collect::<String>();
    let (child, send_agent) = if let Some(id) =
        args["session_id"].as_str().filter(|s| !s.is_empty())
    {
        {
            let child = SessionId::new(id);
            let known = hub.ctx.subagents.get(&child);
            if known.as_ref().map(|v| &v.parent_id) != Some(&parent.id) {
                return Err(format!("{id} is not a subagent of this session"));
            }
            (child, agent)
        }
    } else {
        {
            let request = CreateSession {
                workspace_path: parent.workspace_path.clone(),
                workspace_identity: Some(parent.workspace_key.clone()),
                title: Some(title.clone()),
                agent,
                worktree: None,
            };
            let summary = hub
                .create_child_session(request, &parent.id)
                .await
                .map_err(|e| e.to_string())?;
            // New subagents start in Machine mode (every permission request is approved), so they
            // never stop to ask (user request 2026-11-05). The user can turn it off in the subagent's
            // session.
            if let Err(error) = hub.set_machine(summary.id.clone(), true).await {
                tracing::warn!(session = %summary.id, %error, "subagent machine mode not applied");
            }
            if let Some(model) = routed.as_ref().and_then(|choice| choice.model.clone()) {
                if let Err(error) = hub.set_model(summary.id.clone(), model).await {
                    tracing::warn!(session = %summary.id, %error, "the routed model was not applied");
                }
            }
            // §9.8.1：新子会话的第一轮附带协作说明。
            task = if role_line.is_empty() {
                format!("{PREAMBLE}\n\n{task}")
            } else {
                format!("{PREAMBLE}\n\n{role_line}\n\n{task}")
            };
            (summary.id, None)
        }
    };
    set_scope(hub, &child, &scope).await;
    let described = crate::delegate::describe(&scope, now_ms());
    task = format!("{task}\n\nScope of this subagent: {described}. Actions outside it are denied.");
    let mut events = hub.ctx.events.subscribe();
    let accepted = hub
        .send_message(SendMessage {
            session_id: child.clone(),
            text: task,
            agent: send_agent,
            client_message_id: None,
            context: Vec::new(),
        })
        .await
        .map_err(|e| e.to_string())?;
    if background {
        let agent = hub
            .ctx
            .subagents
            .get(&child)
            .map_or_else(|| parent.active_agent.clone(), |v| v.agent);
        let mut started = json!({
            "sessionId": child,
            "turnId": accepted.turn_id,
            "agent": agent,
            "agentName": hub.ctx.display_name(&agent),
            "background": true,
        });
        note_route(&mut started, routed.as_ref());
        return Ok(started);
    }
    let wait = async {
        loop {
            match events.recv().await {
                Ok(event) if event.session_id.as_ref() == Some(&child) => {
                    if matches!(&event.kind, EventKind::TurnFinished { turn_id, .. } if turn_id == &accepted.turn_id)
                    {
                        return true;
                    }
                }
                Ok(_) => {}
                // 事件丢失时以会话快照为准，避免漏掉 TurnFinished 而永远等待。
                Err(RecvError::Lagged(_)) => {
                    if turn_ended(hub, &child, &accepted.turn_id).await {
                        return true;
                    }
                }
                Err(RecvError::Closed) => return false,
            }
        }
    };
    tokio::select! {
        finished = wait => {
            if !finished {
                return Err("Harness is shutting down".into());
            }
        }
        () = closed => {
            // The parent gave up waiting (e.g. the user stopped it): the subagent keeps running and its
            // answer is pushed to the parent when it finishes (`push`).
            return Err("cancelled".into());
        }
    }
    hub.ctx
        .subagents
        .coord
        .mark_reported(&parent.id, &child, &accepted.turn_id);
    let mut answer = result(hub, &child, &accepted.turn_id).await?;
    note_route(&mut answer, routed.as_ref());
    Ok(answer)
}

/// Tells the delegating agent that the decision model picked the subagent, and why.
fn note_route(result: &mut Value, routed: Option<&crate::decision::Choice>) {
    let (Some(choice), Some(object)) = (routed, result.as_object_mut()) else {
        return;
    };
    object.insert(
        "routedBy".into(),
        json!({
            "decisionModel": true,
            "agent": choice.agent,
            "model": choice.model,
            "reason": choice.reason,
        }),
    );
}

/// The agents a task without a named agent can go to: the available members of the Orchestra team with
/// their roles and notes, or every available agent when no team applies. Each with the models it offers.
async fn candidates(
    hub: &Arc<Hub>,
    team: Option<&[OrchestraNode]>,
    caller: &AgentId,
) -> Vec<crate::decision::Candidate> {
    let list = hub.list_agents().await.unwrap_or_default();
    let available: Vec<&harness_core_api::AgentInfo> = list
        .iter()
        .filter(|a| agent_state(a) == "available")
        .collect();
    let mut out = Vec::new();
    for agent in available {
        let node = team.and_then(|team| team.iter().find(|n| n.agent == agent.id));
        if team.is_some() && node.is_none() {
            continue;
        }
        let models = hub
            .ctx
            .advertised(&agent.id)
            .await
            .models
            .unwrap_or_default()
            .into_iter()
            .map(|m| (m.id, m.name))
            .collect();
        out.push(crate::decision::Candidate {
            id: agent.id.to_string(),
            name: agent.display_name.clone(),
            role: node.map(|n| n.role.label().to_owned()).unwrap_or_default(),
            note: match node.and_then(|n| n.instructions.lines().next()) {
                Some(first) if !first.trim().is_empty() => first.trim().to_owned(),
                // Outside a team the delegating agent is a candidate too: say which one it is.
                _ if &agent.id == caller => "the agent that is delegating this task".to_owned(),
                _ => String::new(),
            },
            models,
        });
    }
    out
}
