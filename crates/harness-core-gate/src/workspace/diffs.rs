use super::{
    check, git, step, write, DiffLineKind, Env, EventKind, FileDiffStatus, Gate, HarnessCore, Hub,
    HubConfig, ListSessions, Path, SessionId, TurnSegment, TurnStatus, A, R,
};

pub(super) async fn turn_diff(env: &Env, gate: &Gate, root: &Path) -> R<()> {
    let summary = gate
        .hub
        .create_session(harness_core_api::CreateSession {
            workspace_path: root.to_string_lossy().into_owned(),
            workspace_identity: None,
            title: Some("turn diff".into()),
            agent: Some(harness_core_api::AgentId::new(A)),
            worktree: None,
        })
        .await
        .map_err(|e| format!("create_session: {e}"))?;
    let session = summary.id;
    let edit = gate
        .send(
            &session,
            "edit please [[say before-edit]] [[write notes/agent.txt AGENT-LINE]] [[say after-edit]]",
            None,
        )
        .await?;
    let view = gate.completed(&session, &edit).await?;
    check(
        root.join("notes/agent.txt").exists(),
        "fake agent did not write the file",
    )?;
    let diff = gate
        .hub
        .turn_diff(session.clone(), edit.clone())
        .await
        .map_err(|e| format!("turn_diff: {e}"))?;
    check(
        diff.available && !diff.truncated && diff.files.len() == 1,
        format!("turn diff: {diff:?}"),
    )?;
    let file = &diff.files[0];
    check(
        file.path == "notes/agent.txt"
            && file.status == FileDiffStatus::Added
            && file.added == 1
            && file
                .hunks
                .iter()
                .flat_map(|h| h.lines.iter())
                .any(|l| l.kind == DiffLineKind::Added && l.text == "AGENT-LINE"),
        format!("turn diff file: {file:?}"),
    )?;
    // The user's index is never touched by snapshots.
    check(
        git(root, &["diff", "--cached", "--name-only"])?.is_empty(),
        "snapshot changed the user's index",
    )?;

    // segments: text → tool → text (API 0.2.0)
    let kinds: Vec<&str> = view
        .segments
        .iter()
        .map(|s| match s {
            TurnSegment::Text { .. } => "text",
            TurnSegment::Tool { .. } => "tool",
            TurnSegment::Steer { .. } => "steer",
        })
        .collect();
    let tool_at = kinds.iter().position(|k| *k == "tool");
    check(
        tool_at.is_some_and(|i| {
            i > 0 && kinds[..i].contains(&"text") && kinds[i + 1..].contains(&"text")
        }),
        format!("segments: {:?}", view.segments),
    )?;
    if let Some(TurnSegment::Text { text }) = view.segments.first() {
        check(
            text.contains("before-edit") && !text.contains("after-edit"),
            format!("first text segment: {text:?}"),
        )?;
    }

    let quiet = gate.send(&session, "no edits this time", None).await?;
    gate.completed(&session, &quiet).await?;
    let none = gate
        .hub
        .turn_diff(session.clone(), quiet)
        .await
        .map_err(|e| e.to_string())?;
    check(
        none.available && none.files.is_empty(),
        format!("turn without edits: {none:?}"),
    )?;

    // Non-repository workspace (the gate's default workspace) → available: false.
    let plain = gate.session(env, A, "non repo").await?;
    let plain_turn = gate.send(&plain, "plain [[write x.txt X]]", None).await?;
    gate.completed(&plain, &plain_turn).await?;
    let unavailable = gate
        .hub
        .turn_diff(plain, plain_turn)
        .await
        .map_err(|e| e.to_string())?;
    check(
        !unavailable.available && unavailable.files.is_empty() && unavailable.reason.is_some(),
        format!("non-repo turn diff: {unavailable:?}"),
    )?;
    let _ = std::fs::remove_file(env.workspace.join("x.txt"));
    step("turn_diff");
    Ok(())
}

pub(super) async fn api_8_2(env: &Env, gate: &Gate) -> R<()> {
    agents_file(env).await?;
    running_and_agents_used(gate, env).await?;

    // ---- local time label ---------------------------------------------------------------------------
    let now = crate::now_secs();
    let today = harness_core_api::time::local_time_label_at(now, now);
    let older = harness_core_api::time::local_time_label_at(now - 3 * 86_400, now);
    let is_clock = |s: &str| {
        s.len() == 5 && s.as_bytes()[2] == b':' && s.bytes().filter(u8::is_ascii_digit).count() == 4
    };
    check(is_clock(&today), format!("today label: {today:?}"))?;
    check(
        older.len() == 11 && older.as_bytes()[2] == b'-' && is_clock(&older[6..]),
        format!("older label: {older:?}"),
    )?;
    step("api_8_2");
    Ok(())
}

/// `<home>/agents.json` applied by `Hub::start` (second hub in its own home).
pub(super) async fn agents_file(env: &Env) -> R<()> {
    let home = env.home.join("agents-home");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let program = env.exe("harness-fake-agent");
    let file = serde_json::json!({
        "replaceBuiltins": false,
        "agents": [{
            "id": "fake-d",
            "displayName": "Fake D (agents.json)",
            "program": program.to_string_lossy(),
            "args": ["--name", "fake-d"],
            "dialect": "generic"
        }]
    });
    write(&home.join("agents.json"), &file.to_string())?;
    let mut config: HubConfig = env.config();
    config.home = home;
    config.load_agents_file = true;
    let hub = Hub::start(config)
        .await
        .map_err(|e| format!("hub with agents.json: {e}"))?;
    let agents = hub.list_agents().await.map_err(|e| e.to_string());
    let fallback = hub.get_fallback_order().await.map_err(|e| e.to_string());
    hub.shutdown().await.map_err(|e| e.to_string())?;
    let agents = agents?;
    check(
        agents
            .iter()
            .any(|a| a.id.as_str() == "fake-d" && a.display_name == "Fake D (agents.json)")
            && agents.iter().any(|a| a.id.as_str() == A),
        format!(
            "agents.json not applied: {:?}",
            agents.iter().map(|a| a.id.as_str()).collect::<Vec<_>>()
        ),
    )?;
    check(
        fallback?.last().map(harness_core_api::AgentId::as_str) == Some("fake-d"),
        "agents.json agent not appended to the fallback order",
    )?;
    Ok(())
}

/// Running text in the snapshot, then `agents_used` after a manual swap.
pub(super) async fn running_and_agents_used(gate: &Gate, env: &Env) -> R<()> {
    let session = gate.session(env, A, "running snapshot").await?;
    let turn = gate.send(&session, "stream [[slow 60]]", None).await?;
    gate.wait_event("third delta of the running turn", |e| {
        matches!(&e.kind, EventKind::MessageDelta { turn_id, text, .. } if turn_id == &turn && text.starts_with("chunk-2"))
    })
    .await?;
    let running = gate.turn(&session, &turn).await?;
    check(
        running.status == TurnStatus::Running && running.assistant_text.contains("chunk-2"),
        format!(
            "running snapshot: {:?} {:?}",
            running.status, running.assistant_text
        ),
    )?;
    gate.hub
        .stop(session.clone())
        .await
        .map_err(|e| e.to_string())?;
    gate.finished(&session, &turn).await?;

    // ---- agents_used --------------------------------------------------------------------------------
    let switch = gate.send(&session, "on b", Some(crate::B)).await?;
    gate.completed(&session, &switch).await?;
    let list = gate
        .hub
        .list_sessions(ListSessions::default())
        .await
        .map_err(|e| e.to_string())?;
    let row = list
        .iter()
        .find(|s| s.id == session)
        .ok_or("session missing from list")?;
    check(
        row.agents_used
            .iter()
            .map(harness_core_api::AgentId::as_str)
            .collect::<Vec<_>>()
            == [A, crate::B],
        format!("agents_used: {:?}", row.agents_used),
    )?;
    let detail = gate
        .hub
        .open_session(session.clone())
        .await
        .map_err(|e| e.to_string())?;
    check(
        detail.summary.agents_used == row.agents_used,
        "agents_used differs between list and open",
    )?;
    let _: &SessionId = &session;
    Ok(())
}
