use super::{
    json, now_ms, result, store, AgentId, Arc, HarnessCore, Hub, OrchestraNode, QuotaState,
    RecvError, SendMessage, SessionId, TurnStatus, Value, MAX_TASK_CHARS,
};

/// `parent`'s own quota: top-level sessions use Max concurrent subagents, subagents use Max subagents per subagent.
pub(super) fn quota(hub: &Arc<Hub>, parent: &store::SessionRow) -> Result<(), String> {
    let nested = hub.ctx.subagents.is_child(&parent.id);
    let limit = hub.ctx.config.delegation.limit_for(nested);
    if hub.ctx.subagents.active_children(&parent.id) < limit {
        return Ok(());
    }
    let setting = if nested {
        "Max subagents per subagent"
    } else {
        "Max concurrent subagents"
    };
    Err(format!(
        "at most {limit} subagents can run at once (Settings › Orchestra › {setting}); wait for one to finish"
    ))
}

/// The caller's own subagent, or an error.
fn own_child(hub: &Arc<Hub>, parent: &store::SessionRow, id: &str) -> Result<SessionId, String> {
    let child = SessionId::new(id.trim());
    let known = hub.ctx.subagents.get(&child);
    if id.trim().is_empty() || known.as_ref().map(|v| &v.parent_id) != Some(&parent.id) {
        return Err(format!("{id} is not a subagent of this session"));
    }
    Ok(child)
}

pub(super) fn task_text(args: &Value, key: &str) -> Result<String, String> {
    let text = args[key].as_str().unwrap_or_default().trim().to_owned();
    if text.is_empty() {
        return Err(format!("{key} is required"));
    }
    if text.chars().count() > MAX_TASK_CHARS {
        return Err(format!("{key} is longer than {MAX_TASK_CHARS} characters"));
    }
    Ok(text)
}

/// `subagent_message`: queue (or interrupt and send) a message to a running or idle subagent.
pub(super) async fn message(
    hub: &Arc<Hub>,
    parent: &store::SessionRow,
    args: &Value,
) -> Result<Value, String> {
    let child = own_child(hub, parent, args["session_id"].as_str().unwrap_or_default())?;
    let text = task_text(args, "text")?;
    let active = hub
        .ctx
        .subagents
        .get(&child)
        .is_some_and(|v| v.status.is_active());
    if !active {
        quota(hub, parent)?;
    }
    let interrupted = args["interrupt"].as_bool() == Some(true) && active;
    if interrupted {
        let mut events = hub.ctx.events.subscribe();
        let _ = hub.stop(child.clone()).await;
        // 打断：等当前这一轮真正结束再发送，最多 10 秒（§9.8.1），以事件为准而不是固定延时。
        let ended = async {
            loop {
                if !busy(hub, &child).await {
                    return;
                }
                match events.recv().await {
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                }
            }
        };
        if tokio::time::timeout(std::time::Duration::from_secs(10), ended)
            .await
            .is_err()
        {
            return Err("the subagent did not stop within 10 s; the message was not sent".into());
        }
    }
    let accepted = hub
        .send_message(SendMessage {
            session_id: child.clone(),
            text,
            agent: None,
            client_message_id: None,
            context: Vec::new(),
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "sessionId": child,
        "turnId": accepted.turn_id,
        "position": accepted.position,
        "interrupted": interrupted,
        "queued": active && !interrupted,
    }))
}

async fn busy(hub: &Arc<Hub>, child: &SessionId) -> bool {
    hub.open_session(child.clone()).await.is_ok_and(|d| {
        d.turns
            .iter()
            .any(|t| matches!(t.status, TurnStatus::Running | TurnStatus::Paused))
    })
}

/// `subagent_wait`: block until a subagent turn finishes or a note arrives (or the timeout passes).
pub(super) async fn wait(
    hub: &Arc<Hub>,
    parent: &store::SessionRow,
    args: &Value,
    closed: impl std::future::Future<Output = ()>,
) -> Result<Value, String> {
    let only = match args["session_ids"].as_array() {
        Some(ids) if !ids.is_empty() => Some(
            ids.iter()
                .map(|id| own_child(hub, parent, id.as_str().unwrap_or_default()))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        _ => None,
    };
    let timeout = args["timeout_s"].as_u64().unwrap_or(300).min(3600);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout);
    let coord = &hub.ctx.subagents.coord;
    tokio::pin!(closed);
    let (finished, notes) = loop {
        // 先登记唤醒再检查，避免检查与等待之间到达的通知丢失。
        let notified = coord.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let (finished, notes) = coord.take_ready(&parent.id, only.as_deref());
        if !finished.is_empty() || !notes.is_empty() || tokio::time::Instant::now() >= deadline {
            break (finished, notes);
        }
        tokio::select! {
            () = &mut notified => {}
            () = tokio::time::sleep_until(deadline) => {}
            // 父 agent 放弃等待：后台子 agent 继续运行，不做任何停止。
            () = &mut closed => return Err("cancelled".into()),
        }
    };
    let mut results = Vec::new();
    for (child, turn) in &finished {
        match result(hub, child, turn).await {
            Ok(value) => results.push(value),
            Err(error) => results
                .push(json!({"sessionId": child, "outcome": "failed", "error": error, "text": ""})),
        }
    }
    let views: Vec<Value> = hub
        .ctx
        .subagents
        .of_parent(&parent.id)
        .into_iter()
        .filter(|v| !matches!(&only, Some(o) if !o.contains(&v.session_id)))
        .map(|v| {
            json!({
                "sessionId": v.session_id,
                "agentName": hub.ctx.display_name(&v.agent),
                "title": v.title,
                "status": v.status,
                "activity": v.activity,
            })
        })
        .collect();
    let notes: Vec<Value> = notes
        .into_iter()
        .map(|n| {
            let title = hub
                .ctx
                .subagents
                .get(&n.child)
                .map(|v| v.title)
                .unwrap_or_default();
            json!({"sessionId": n.child, "title": title, "text": n.text})
        })
        .collect();
    Ok(json!({"subagents": views, "notes": notes, "finished": results}))
}

/// `notify_parent`: a subagent leaves a note for its parent and shows it as its activity.
pub(super) async fn notify(
    hub: &Arc<Hub>,
    child: &store::SessionRow,
    args: &Value,
) -> Result<Value, String> {
    let text = args["text"].as_str().unwrap_or_default().trim().to_owned();
    if text.is_empty() {
        return Err("text is required".into());
    }
    let text: String = text
        .chars()
        .take(crate::delegate_coord::MAX_NOTE_CHARS)
        .collect();
    let Some(mut view) = hub.ctx.subagents.get(&child.id) else {
        return Err("notify_parent is only for subagents".into());
    };
    let first = text.lines().next().unwrap_or_default();
    view.activity = format!("Note: {first}").chars().take(160).collect();
    let parent = view.parent_id.clone();
    hub.ctx.put_subagent(view).await;
    hub.ctx.subagents.coord.add_note(
        &parent,
        crate::delegate_coord::Note {
            child: child.id.clone(),
            text,
            at: now_ms(),
        },
    );
    Ok(json!({"delivered": true}))
}

fn claim_paths(args: &Value) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for path in args["paths"].as_array().into_iter().flatten() {
        let normalized = crate::delegate_coord::normalize(path.as_str().unwrap_or_default())?;
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    Ok(out)
}

fn claim_json(c: &crate::delegate_coord::Claim) -> Value {
    json!({"path": c.path, "sessionId": c.holder, "title": c.title, "agent": c.agent, "note": c.note})
}

/// `claim_paths`: advisory claims, scoped to the workspace identity key.
pub(super) fn claim(
    hub: &Arc<Hub>,
    caller: &store::SessionRow,
    args: &Value,
) -> Result<Value, String> {
    let paths = claim_paths(args)?;
    if paths.is_empty() {
        let all = hub.ctx.subagents.coord.claims_in(&caller.workspace_key);
        return Ok(
            json!({"granted": [], "conflicts": [], "claims": all.iter().map(claim_json).collect::<Vec<_>>()}),
        );
    }
    if let Some(scope) = hub.ctx.scope_of(&caller.id) {
        if let Some(path) = paths
            .iter()
            .find(|p| !crate::delegate::allows_path(&scope, p))
        {
            return Err(format!(
                "{path} is outside the scope of this subagent, which allows only: {}",
                scope.paths.join(", ")
            ));
        }
    }
    let request = crate::delegate_coord::Claim {
        workspace: caller.workspace_key.clone(),
        path: String::new(),
        holder: caller.id.clone(),
        title: caller.title.clone(),
        agent: caller.active_agent.clone(),
        note: args["note"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
    };
    let outcome = hub.ctx.subagents.coord.claim(&request, &paths);
    Ok(json!({
        "granted": outcome.granted,
        "conflicts": outcome.conflicts.iter().map(|(path, held)| {
            let mut v = claim_json(held);
            v["requested"] = json!(path);
            v
        }).collect::<Vec<_>>(),
        "claims": outcome.all.iter().map(claim_json).collect::<Vec<_>>(),
    }))
}

pub(super) fn release(
    hub: &Arc<Hub>,
    caller: &store::SessionRow,
    args: &Value,
) -> Result<Value, String> {
    let paths = claim_paths(args)?;
    let released = hub.ctx.subagents.coord.release(&caller.id, &paths);
    Ok(json!({"released": released}))
}

pub(super) fn agent_state(a: &harness_core_api::AgentInfo) -> &'static str {
    if !matches!(
        a.availability,
        harness_core_api::Availability::Available { .. }
    ) {
        "not installed"
    } else if matches!(a.quota, QuotaState::Exhausted { .. }) {
        "limit reached"
    } else {
        "available"
    }
}

/// The agents `parent` may delegate to when Orchestra is active (§9.11): the team, minus the parent's own agent.
pub(super) async fn team(hub: &Arc<Hub>, parent: &store::SessionRow) -> Option<Vec<OrchestraNode>> {
    let orchestra = hub.load_orchestra().await;
    orchestra.orchestrator()?;
    let config = &hub.ctx.config;
    let me = &parent.active_agent;
    let same = |agent: &AgentId| {
        agent == me
            || (crate::accounts::is_claude(config, agent) && crate::accounts::is_claude(config, me))
    };
    Some(
        orchestra
            .team()
            .into_iter()
            .filter(|n| !same(&n.agent))
            .cloned()
            .collect(),
    )
}

pub(super) async fn agents(hub: &Arc<Hub>, parent: &store::SessionRow) -> Value {
    let list = hub.list_agents().await.unwrap_or_default();
    let Some(team) = team(hub, parent).await else {
        return Value::Array(
            list.iter()
                .map(|a| json!({"id": a.id, "name": a.display_name, "state": agent_state(a)}))
                .collect(),
        );
    };
    Value::Array(
        team.iter()
            .filter_map(|node| {
                let a = list.iter().find(|a| a.id == node.agent)?;
                Some(json!({
                    "id": a.id,
                    "name": a.display_name,
                    "state": agent_state(a),
                    "role": node.role.label(),
                    "instructions": node.instructions.lines().next().unwrap_or_default(),
                }))
            })
            .collect(),
    )
}
