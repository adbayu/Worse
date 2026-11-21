use super::{
    cfg, check, hub_config, start, step, AgentConfigService, AgentId, AgentKind, ApiError,
    ConfigTarget, EntryBody, Env, EventKind, Gate, HarnessCore, McpServer, McpTransport, Scope,
    SessionId, A, B, R,
};

pub(super) fn overlay_server(name: &str, transport: McpTransport) -> EntryBody {
    EntryBody::Mcp(McpServer {
        name: name.into(),
        transport,
        command: (transport == McpTransport::Stdio).then(|| "node".to_owned()),
        args: if transport == McpTransport::Stdio {
            vec![format!("{name}.js")]
        } else {
            Vec::new()
        },
        url: (transport != McpTransport::Stdio).then(|| "https://example.invalid/mcp".to_owned()),
        ..McpServer::default()
    })
}

/// Names of the `mcpServers` of every logged `new` of `agent`, in order.
pub(super) fn logged_news(env: &Env, agent: &str) -> Vec<(String, Vec<String>)> {
    env.log_entries()
        .into_iter()
        .filter(|e| e["event"] == "new" && e["agent"] == agent)
        .map(|e| {
            let names = e["mcpServers"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s["name"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            (
                e["sessionId"].as_str().unwrap_or_default().to_owned(),
                names,
            )
        })
        .collect()
}

pub(super) async fn overlay_session_new(env: &Env, svc: &AgentConfigService) -> R<SessionId> {
    let target = ConfigTarget::new(AgentKind::Harness, Scope::User);
    for body in [
        overlay_server("gate-stdio", McpTransport::Stdio),
        overlay_server("gate-http", McpTransport::Http),
    ] {
        let plan = cfg("overlay", svc.plan_upsert(target, &body, None).await)?;
        cfg("overlay", svc.apply(&plan).await)?;
    }
    let gate = start(hub_config(env, &env.home, false)).await?;
    let s1 = gate.session(env, A, "phase9").await?;
    let t = gate.send(&s1, "hello overlay", None).await?;
    gate.completed(&s1, &t).await?;
    let news = logged_news(env, A);
    let (_, names) = news.last().ok_or("no session/new logged")?;
    check(
        names == &["harness-memory", "harness-delegate", "gate-stdio"],
        format!("session/new mcpServers {names:?}, expected harness-memory + harness-delegate + gate-stdio"),
    )?;
    // Edit while the native session exists: the running native session keeps what it got.
    let plan = cfg(
        "overlay",
        svc.plan_upsert(
            target,
            &overlay_server("gate-late", McpTransport::Stdio),
            None,
        )
        .await,
    )?;
    cfg("overlay", svc.apply(&plan).await)?;
    let t = gate.send(&s1, "second turn", None).await?;
    gate.completed(&s1, &t).await?;
    check(
        logged_news(env, A).len() == news.len(),
        "an overlay edit re-created the running native session",
    )?;
    let s2 = gate.session(env, A, "phase9-b").await?;
    let t = gate.send(&s2, "hello again", None).await?;
    gate.completed(&s2, &t).await?;
    let (_, names) = logged_news(env, A)
        .last()
        .cloned()
        .ok_or("no session/new logged")?;
    check(
        names.iter().any(|n| n == "gate-late"),
        format!("next session/new did not see the overlay edit: {names:?}"),
    )?;
    gate.hub.shutdown().await.map_err(|e| e.to_string())?;
    step("overlay_session_new");
    Ok(s1)
}

// ---- 6. modes / models / options -----------------------------------------------------------------------

pub(super) fn sets_logged(env: &Env, key: &str, value: &str) -> usize {
    env.log_entries()
        .iter()
        .filter(|e| e["event"] == "set" && e["key"] == key && e["value"] == value)
        .count()
}

pub(super) async fn options_text(gate: &Gate, s: &SessionId, prefix: &str) -> R<String> {
    let t = gate.send(s, &format!("{prefix}[[options]]"), None).await?;
    Ok(gate.completed(s, &t).await?.assistant_text)
}

/// One continuous scenario: splitting it would hide the ordering the steps depend on.
#[allow(clippy::too_many_lines)]
pub(super) async fn modes_models(env: &Env, s1: &SessionId) -> R<()> {
    let gate = start(hub_config(env, &env.home, true)).await?;
    let hub = &gate.hub;
    let e = |e: ApiError| e.to_string();
    let modes = hub.list_modes(s1.clone()).await.map_err(e)?;
    check(
        modes.known && modes.available.len() == 3 && modes.current.as_deref() == Some("default"),
        format!("modes after first turn: {modes:?}"),
    )?;
    let model_list = hub.list_models(s1.clone()).await.map_err(e)?;
    check(
        model_list.known && model_list.available.len() == 2,
        format!("models: {model_list:?}"),
    )?;
    let options = hub.list_config_options(s1.clone()).await.map_err(e)?;
    check(
        options.known && options.options.iter().any(|o| o.id == "effort"),
        format!("config options: {options:?}"),
    )?;
    hub.set_mode(s1.clone(), "plan".into()).await.map_err(e)?;
    hub.set_model(s1.clone(), "fake-large".into())
        .await
        .map_err(e)?;
    hub.set_config_option(s1.clone(), "effort".into(), "high".into())
        .await
        .map_err(e)?;
    for bad in [
        hub.set_mode(s1.clone(), "bogus".into()).await,
        hub.set_model(s1.clone(), "bogus".into()).await,
        hub.set_config_option(s1.clone(), "effort".into(), "max".into())
            .await,
    ] {
        check(
            matches!(bad, Err(ApiError::InvalidArgument(_))),
            format!("invalid id accepted: {bad:?}"),
        )?;
    }
    check(
        gate.event_index(|k| matches!(k, EventKind::SessionOptionsChanged { .. }))
            .is_some(),
        "no SessionOptionsChanged event",
    )?;
    check(
        hub.list_modes(s1.clone())
            .await
            .map_err(e)?
            .current
            .as_deref()
            == Some("plan"),
        "list_modes does not show the choice",
    )?;
    let text = options_text(&gate, s1, "").await?;
    check(
        text.contains("options=mode:plan,model:fake-large,effort:high"),
        format!("choices not applied before the prompt: {text}"),
    )?;
    check(
        sets_logged(env, "mode", "plan") >= 1 && sets_logged(env, "effort", "high") >= 1,
        "set calls not logged by the agent",
    )?;
    // A choice made while a turn runs applies only to the next turn.
    let t = gate.send(s1, "[[slow 20]] [[options]]", None).await?;
    gate.wait_turn(
        s1,
        &t,
        |v| v.assistant_text.contains("chunk-1"),
        "slow turn streaming",
    )
    .await?;
    hub.set_mode(s1.clone(), "acceptEdits".into())
        .await
        .map_err(e)?;
    let running = gate.completed(s1, &t).await?.assistant_text;
    check(
        running.contains("options=mode:plan,"),
        format!("running turn was changed: {running}"),
    )?;
    let text = options_text(&gate, s1, "").await?;
    check(
        text.contains("options=mode:acceptEdits,"),
        format!("choice not applied to the next turn: {text}"),
    )?;
    // Agent defaults (AGT-007) on a fresh native session.
    let sb = gate.session(env, B, "defaults").await?;
    let text = options_text(&gate, &sb, "").await?;
    check(
        text.contains("model:fake-large"),
        format!("agent default not applied: {text}"),
    )?;
    let agent_options = hub.agent_options(AgentId::new(B)).await.map_err(e)?;
    check(
        agent_options.models.current.as_deref() == Some("fake-large") && agent_options.modes.known,
        format!("agent_options: {agent_options:?}"),
    )?;
    hub.shutdown().await.map_err(e)?;
    // Persisted across a hub restart (new agent process → session/load → choices re-applied).
    let gate = start(hub_config(env, &env.home, false)).await?;
    check(
        gate.hub
            .list_modes(s1.clone())
            .await
            .map_err(e)?
            .current
            .as_deref()
            == Some("acceptEdits"),
        "mode choice lost across restart",
    )?;
    let text = options_text(&gate, s1, "").await?;
    check(
        text.contains("options=mode:acceptEdits,model:fake-large,effort:high"),
        format!("choices not re-applied after restart: {text}"),
    )?;
    gate.hub.shutdown().await.map_err(e)?;
    step("modes_models");
    Ok(())
}
