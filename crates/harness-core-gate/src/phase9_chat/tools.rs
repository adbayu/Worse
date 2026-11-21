use super::{
    api, check, create, norm, run_turn, step, turns, ActionEntry, ApiError, CommandSearch,
    CommandSource, Duration, Env, EventKind, Gate, HarnessCore, Instant, PermissionDecision,
    PermissionOptionKind, SearchScope, TurnStatus, A, R,
};

pub(super) async fn background(env: &Env, gate: &Gate) -> R<()> {
    let s = create(gate, &env.workspace, A, "background").await?;
    run_turn(gate, &s, "[[bg]] start").await?;
    let finished = |e: &harness_core_api::HubEvent| {
        e.session_id.as_ref() == Some(&s)
            && matches!(&e.kind, EventKind::BackgroundTasksChanged { tasks }
                if tasks.iter().any(|t| t.name == "sleep" && t.state == "completed" && t.finished_at.is_some()))
    };
    gate.wait_event("BackgroundTasksChanged completed", finished)
        .await?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let autonomous = loop {
        let list = turns(gate, &s).await?;
        if let Some(turn) = list.iter().find(|t| {
            t.origin
                .as_ref()
                .and_then(|o| o.background.as_ref())
                .is_some()
                && t.status == TurnStatus::Completed
        }) {
            break turn.clone();
        }
        if Instant::now() > deadline {
            return Err(format!("no autonomous turn: {list:?}"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    check(
        autonomous.assistant_text == "autonomous ok"
            && autonomous.user_text == "Background task finished: gate sleep (exit code 0)",
        format!("autonomous turn {autonomous:?}"),
    )?;
    let detail = api("open_session", gate.hub.open_session(s.clone()).await)?;
    let task = detail
        .background_tasks
        .iter()
        .find(|t| t.name == "sleep")
        .ok_or("background task missing from the snapshot")?;
    check(
        task.summary == "exit code 0" && task.task_type == "local_bash",
        format!("task {task:?}"),
    )?;
    let stopped = gate
        .hub
        .stop_background_task(s.clone(), task.id.clone())
        .await;
    check(
        matches!(stopped, Err(ApiError::NotFound(_))),
        format!("stop of a finished task: {stopped:?}"),
    )?;
    step("background");
    Ok(())
}

pub(super) async fn commands(env: &Env, gate: &Gate) -> R<()> {
    let s = create(gate, &env.workspace, A, "commands").await?;
    let before = api("list_commands", gate.hub.list_commands(s.clone()).await)?;
    let source_of = |list: &harness_core_api::CommandList, name: &str| {
        list.commands
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.source)
    };
    check(
        source_of(&before, "review") == Some(CommandSource::Workspace)
            && source_of(&before, "ws-only") == Some(CommandSource::Workspace)
            && source_of(&before, "user-only") == Some(CommandSource::User),
        format!(
            "config commands before the agent spoke: {:?}",
            before.commands
        ),
    )?;
    run_turn(gate, &s, "[[commands]] list").await?;
    gate.wait_event(
        "CommandsChanged",
        |e| matches!(&e.kind, EventKind::CommandsChanged { agent } if agent.as_str() == A),
    )
    .await?;
    let after = api("list_commands", gate.hub.list_commands(s.clone()).await)?;
    let review = after
        .commands
        .iter()
        .find(|c| c.name == "review")
        .ok_or("review missing")?;
    check(
        after.known
            && review.source == CommandSource::Agent
            && review.description == "Agent review"
            && after.commands.iter().filter(|c| c.name == "review").count() == 1
            && source_of(&after, "compact") == Some(CommandSource::Agent)
            && source_of(&after, "ws-only") == Some(CommandSource::Workspace)
            && source_of(&after, "user-only") == Some(CommandSource::User),
        format!("merged commands {:?}", after.commands),
    )?;
    check(
        source_of(&after, "gate-skill") == Some(CommandSource::Harness)
            && after
                .commands
                .iter()
                .any(|c| c.name == "gate-skill" && c.description == "Gate skill"),
        format!("harness skill missing: {:?}", after.commands),
    )?;
    run_turn(gate, &s, "/gate-skill hello there").await?;
    let expanded = env
        .log_entries()
        .into_iter()
        .filter(|e| e["event"] == "prompt" && e["agent"] == A)
        .filter_map(|e| e["blocks"].as_array()?.last()?.as_str().map(str::to_owned))
        .any(|last| {
            last.starts_with("<harness-skill name=\"gate-skill\"")
                && last.contains("SKILL-BODY-7731")
                && last.ends_with("for this request: hello there")
        });
    check(expanded, "harness skill was not expanded in the prompt")?;
    step("commands");
    Ok(())
}

// ---- 8. plan / usage / tool content -----------------------------------------------------------------------

pub(super) async fn plan_usage_tools(env: &Env, gate: &Gate) -> R<()> {
    let s = create(gate, &env.workspace, A, "plan").await?;
    let turn = run_turn(
        gate,
        &s,
        "[[think pondering deeply]] [[plan A|B|C]] [[usage 50 1000]] [[tools]] p1",
    )
    .await?;
    let mine = |e: &harness_core_api::HubEvent| e.session_id.as_ref() == Some(&s);
    gate.wait_event("ThoughtDelta", |e| {
        mine(e) && matches!(&e.kind, EventKind::ThoughtDelta { text, .. } if text.contains("pondering deeply"))
    })
    .await?;
    gate.wait_event("PlanUpdated", |e| {
        mine(e) && matches!(&e.kind, EventKind::PlanUpdated { entries, .. } if entries.len() == 3)
    })
    .await?;
    gate.wait_event("UsageUpdated", |e| {
        mine(e)
            && matches!(&e.kind, EventKind::UsageUpdated { usage, .. } if usage.used == 50 && usage.size == 1000)
    })
    .await?;
    gate.wait_event("tool content", |e| {
        mine(e)
            && matches!(&e.kind, EventKind::ToolCallUpdated { tool, .. } if tool.content.len() == 3)
    })
    .await?;
    let detail = api("open_session", gate.hub.open_session(s.clone()).await)?;
    let view = detail
        .turns
        .iter()
        .find(|t| t.id == turn)
        .ok_or("turn missing")?;
    let usage = view.usage.as_ref().ok_or("prompt usage missing")?;
    let tool = view.tool_calls.first().ok_or("tool missing")?;
    check(
        view.plan.len() == 3
            && view.plan[0].status == "completed"
            && usage.total == 120
            && usage.cached_read == Some(7)
            && detail.context_usage.as_ref().is_some_and(|u| u.used == 50)
            && detail.plan.len() == 3
            && tool.content.len() == 3
            && tool.locations.len() == 1
            && tool.raw_output.is_some(),
        "plan / usage / tool content not in the snapshot",
    )?;
    step("plan_usage_tools");
    Ok(())
}

// ---- 9. permissions ---------------------------------------------------------------------------------------

pub(super) async fn permissions(env: &Env, gate: &Gate) -> R<()> {
    let s = create(gate, &env.workspace, A, "permissions").await?;
    let turn = gate.send(&s, "[[permission all]] q", None).await?;
    let event = gate
        .wait_event("PermissionRequested", |e| {
            e.session_id.as_ref() == Some(&s)
                && matches!(&e.kind, EventKind::PermissionRequested { .. })
        })
        .await?;
    let EventKind::PermissionRequested { request } = event.kind else {
        return Err("unexpected event".into());
    };
    let kinds: Vec<PermissionOptionKind> = request.options.iter().map(|o| o.kind).collect();
    check(
        kinds
            == [
                PermissionOptionKind::AllowOnce,
                PermissionOptionKind::AllowAlways,
                PermissionOptionKind::RejectOnce,
                PermissionOptionKind::RejectAlways,
            ],
        format!("options {kinds:?}"),
    )?;
    api(
        "respond",
        gate.hub
            .respond_permission(s.clone(), request.request_id, "deny-always".into())
            .await,
    )?;
    let view = gate.completed(&s, &turn).await?;
    check(
        view.permissions.iter().any(|(_, d)| {
            matches!(d, PermissionDecision::Selected { option_id, kind: PermissionOptionKind::RejectAlways } if option_id == "deny-always")
        }) && view.assistant_text.contains("denied:deny-always"),
        format!("decision not recorded: {:?}", view.permissions),
    )?;
    step("permissions");
    Ok(())
}

// ---- 10. command search -----------------------------------------------------------------------------------

pub(super) async fn command_search(env: &Env, gate: &Gate) -> R<()> {
    let s = create(gate, &env.workspace, A, "Settings research").await?;
    let actions = vec![
        ActionEntry {
            id: "new-session".into(),
            title: "New session".into(),
            keywords: vec!["create".into()],
        },
        ActionEntry {
            id: "open-settings".into(),
            title: "Open settings".into(),
            keywords: vec!["preferences".into()],
        },
        ActionEntry {
            id: "theme".into(),
            title: "Toggle theme".into(),
            // 关键词与标题同为词前缀匹配时，关键词扣 50 分：标题命中排前。
            keywords: vec!["appearance settings".into()],
        },
    ];
    let result = api(
        "command_search",
        gate.hub
            .command_search(CommandSearch {
                query: "settings".into(),
                workspace_path: Some(env.workspace()),
                scopes: vec![
                    SearchScope::Actions,
                    SearchScope::Sessions,
                    SearchScope::Files,
                ],
                actions,
                limit: 20,
            })
            .await,
    )?;
    let ids: Vec<&str> = result.actions.iter().map(|a| a.id.as_str()).collect();
    check(
        ids.first() == Some(&"open-settings")
            && ids.contains(&"theme")
            && !ids.contains(&"new-session"),
        format!("action ranking {ids:?}"),
    )?;
    check(
        result.sessions.iter().any(|h| h.summary.id == s),
        "session hit missing",
    )?;
    check(
        result.files_available
            && result
                .files
                .iter()
                .any(|f| norm(&f.path) == "settings-notes.md"),
        format!("file hits {:?}", result.files),
    )?;
    step("command_search");
    Ok(())
}
