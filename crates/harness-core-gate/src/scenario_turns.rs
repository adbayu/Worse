use super::{
    check, step, AgentId, Duration, Env, EventKind, Gate, HarnessCore, Instant, MemoryScope,
    MemoryWriteMode, PermissionDecision, QuotaState, SessionId, SwitchReason, TurnStatus, A, B, R,
};

pub(super) async fn basics(env: &Env, gate: &Gate) -> R<SessionId> {
    // ---- memory injection + send/stream ------------------------------------------------------------
    gate.hub
        .memory_write(
            MemoryScope::Global,
            "GLOBAL-MARK-7".into(),
            MemoryWriteMode::Replace,
        )
        .await
        .map_err(|e| e.to_string())?;
    gate.hub
        .memory_write(
            MemoryScope::Project {
                workspace_path: env.workspace(),
            },
            "PROJECT-MARK-9".into(),
            MemoryWriteMode::Replace,
        )
        .await
        .map_err(|e| e.to_string())?;
    let s1 = gate.session(env, A, "basic").await?;
    let text = "hello stream [[slow 5]]";
    let t = gate.send(&s1, text, None).await?;
    let view = gate.completed(&s1, &t).await?;
    check(
        view.assistant_text.contains("chunk-4"),
        format!("stream text: {}", view.assistant_text),
    )?;
    let deltas = gate
        .events()
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::MessageDelta { turn_id, .. } if turn_id == &t))
        .count();
    check(deltas >= 6, format!("expected >=6 deltas, got {deltas}"))?;
    step("send_stream");
    let blocks = env
        .prompts(A, text)
        .pop()
        .ok_or("fake-a prompt not logged")?;
    let joined = blocks.join("\n");
    check(
        joined.contains("<harness-memory")
            && joined.contains("GLOBAL-MARK-7")
            && joined.contains("PROJECT-MARK-9"),
        "MEMORY.md injection block missing from the agent context",
    )?;
    check(
        view.assistant_text.contains("memory=true"),
        "agent did not see memory",
    )?;
    step("memory_injection");

    // ---- stop mid-stream --------------------------------------------------------------------------
    let t = gate.send(&s1, "long [[slow 200]]", None).await?;
    gate.wait_event("first delta of long turn", |e| {
        matches!(&e.kind, EventKind::MessageDelta { turn_id, text, .. } if turn_id == &t && text.starts_with("chunk-"))
    })
    .await?;
    let stopped_at = Instant::now();
    gate.hub.stop(s1.clone()).await.map_err(|e| e.to_string())?;
    let view = gate.finished(&s1, &t).await?;
    check(
        view.status == TurnStatus::Cancelled,
        format!("stop: status {:?}", view.status),
    )?;
    check(
        stopped_at.elapsed() < Duration::from_secs(3),
        "stop took too long",
    )?;
    check(
        !view.assistant_text.contains("chunk-199"),
        "stream was not interrupted",
    )?;
    step("stop_mid_stream");

    // ---- queued second message ordering -----------------------------------------------------------
    let first = gate.send(&s1, "first [[slow 8]]", None).await?;
    let second = gate.send(&s1, "second", None).await?;
    let detail = gate
        .hub
        .open_session(s1.clone())
        .await
        .map_err(|e| e.to_string())?;
    check(detail.queue.contains(&second), "second message not queued")?;
    gate.completed(&s1, &first).await?;
    gate.completed(&s1, &second).await?;
    let first_end = gate
        .event_index(|k| matches!(k, EventKind::TurnFinished { turn_id, .. } if turn_id == &first))
        .ok_or("no finish event for first")?;
    let second_start = gate
        .event_index(|k| matches!(k, EventKind::TurnStarted { turn_id, .. } if turn_id == &second))
        .ok_or("no start event for second")?;
    check(
        first_end < second_start,
        "second turn started before the first finished",
    )?;
    step("queue_order");
    Ok(s1)
}

pub(super) async fn tools_and_memory(env: &Env, gate: &Gate, s1: &SessionId) -> R<()> {
    let s1 = s1.clone();
    // ---- permissions ------------------------------------------------------------------------------
    for (option, expect) in [
        ("allow", "permission=allowed"),
        ("deny", "permission=denied"),
    ] {
        let t = gate
            .send(&s1, &format!("tool {option} [[permission]]"), None)
            .await?;
        let event = gate
            .wait_event("permission request", |e| {
                matches!(&e.kind, EventKind::PermissionRequested { request } if request.turn_id == t)
            })
            .await?;
        let EventKind::PermissionRequested { request } = event.kind else {
            return Err("unexpected event".into());
        };
        gate.hub
            .respond_permission(s1.clone(), request.request_id.clone(), option.into())
            .await
            .map_err(|e| e.to_string())?;
        let view = gate.completed(&s1, &t).await?;
        check(
            view.assistant_text.contains(expect),
            format!("{option}: {}", view.assistant_text),
        )?;
        check(
            matches!(view.permissions.first(), Some((_, PermissionDecision::Selected { option_id, .. })) if option_id == option),
            format!(
                "permission decision not recorded on the turn: {:?}",
                view.permissions
            ),
        )?;
        step(&format!("permission_{option}"));
    }

    // ---- memory via MCP across two different agents ------------------------------------------------
    let t = gate
        .send(
            &s1,
            "[[mem-write project shared-from-a]] [[mem-write global global-from-a]]",
            Some(A),
        )
        .await?;
    let view = gate.completed(&s1, &t).await?;
    check(
        view.assistant_text.contains("mem-write[project]=<<ok>>"),
        format!("mcp write: {}", view.assistant_text),
    )?;
    let t = gate
        .send(&s1, "[[mem-read project]] [[mem-read global]]", Some(B))
        .await?;
    let view = gate.completed(&s1, &t).await?;
    check(
        view.agent.as_ref().map(AgentId::as_str) == Some(B)
            && view.assistant_text.contains("shared-from-a")
            && view.assistant_text.contains("global-from-a"),
        format!(
            "fake-b did not read fake-a's memory: {}",
            view.assistant_text
        ),
    )?;
    let project = gate
        .hub
        .memory_read(MemoryScope::Project {
            workspace_path: env.workspace(),
        })
        .await
        .map_err(|e| e.to_string())?;
    check(
        project.contains("shared-from-a"),
        "project MEMORY.md lacks the MCP write",
    )?;
    check(
        env.workspace.join(".harness").join("MEMORY.md").is_file()
            && !env.workspace.join("CLAUDE.md").exists()
            && !env.workspace.join("AGENTS.md").exists()
            && !env.workspace.join("GEMINI.md").exists(),
        "memory must live in <project>/.harness/MEMORY.md and never in repo rule files",
    )?;
    step("memory_mcp_cross_agent");
    Ok(())
}

pub(super) async fn agent_switch(env: &Env, gate: &Gate) -> R<SessionId> {
    // ---- agent switch catch-up --------------------------------------------------------------------
    let s2 = gate.session(env, A, "switch").await?;
    let t1 = gate.send(&s2, "alpha-one", None).await?;
    gate.completed(&s2, &t1).await?;
    let t2 = gate.send(&s2, "beta-two", Some(B)).await?;
    gate.completed(&s2, &t2).await?;
    let b_prompt = env
        .prompts(B, "beta-two")
        .pop()
        .ok_or("fake-b prompt missing")?
        .join("\n");
    check(
        b_prompt.contains("kind=\"seed\"") && b_prompt.contains("alpha-one"),
        "second agent did not receive the earlier turn (seed)",
    )?;
    let t3 = gate.send(&s2, "beta-three", None).await?;
    let v3 = gate.completed(&s2, &t3).await?;
    check(
        v3.agent.as_ref().map(AgentId::as_str) == Some(B),
        "sticky agent was not fake-b",
    )?;
    let t4 = gate.send(&s2, "alpha-four", Some(A)).await?;
    gate.completed(&s2, &t4).await?;
    let a_prompt = env
        .prompts(A, "alpha-four")
        .pop()
        .ok_or("fake-a prompt missing")?
        .join("\n");
    check(
        a_prompt.contains("kind=\"catch-up\"")
            && a_prompt.contains("beta-two")
            && a_prompt.contains("beta-three")
            && !a_prompt.contains("alpha-one"),
        format!("first agent did not receive exactly the missed turns: {a_prompt}"),
    )?;
    step("agent_switch_catchup");
    Ok(s2)
}

pub(super) async fn failover(env: &Env, gate: &Gate) -> R<SessionId> {
    // ---- failover mid-turn ------------------------------------------------------------------------
    let s3 = gate.session(env, A, "failover").await?;
    let user = "do work [[quota-mid fake-a]]";
    let t = gate.send(&s3, user, None).await?;
    let view = gate.completed(&s3, &t).await?;
    check(
        view.agent.as_ref().map(AgentId::as_str) == Some(B),
        format!("failover served by {:?}", view.agent),
    )?;
    check(
        view.attempts.len() == 2
            && view.attempts[0].agent.as_str() == A
            && view.attempts[0].status == "quotaExhausted"
            && view.attempts[0].partial_text.contains("partial-2")
            && view.attempts[1].agent.as_str() == B,
        format!("attempts: {:?}", view.attempts),
    )?;
    check(env.prompts(A, user).len() == 1, "turn ran twice on fake-a")?;
    let b_blocks = env
        .prompts(B, user)
        .pop()
        .ok_or("fake-b failover prompt missing")?;
    check(
        b_blocks.iter().filter(|b| b.contains(user)).count() == 1,
        "user message duplicated in the failover prompt",
    )?;
    check(
        b_blocks
            .iter()
            .any(|b| b.contains("<harness-failover") && b.contains("partial-0")),
        "failed agent's partial output not forwarded",
    )?;
    let switched = gate
        .wait_event("failover switch event", |e| {
            e.session_id.as_ref() == Some(&s3)
                && matches!(
                    &e.kind,
                    EventKind::AgentSwitched {
                        reason: SwitchReason::QuotaExhausted { resets_at: Some(_) },
                        ..
                    }
                )
        })
        .await?;
    if let EventKind::AgentSwitched { message, .. } = &switched.kind {
        check(
            message.starts_with("switched from Fake A to Fake B: Fake A limit reached (resets at "),
            format!("switch message: {message}"),
        )?;
    }
    let agents = gate.hub.list_agents().await.map_err(|e| e.to_string())?;
    check(
        agents.iter().any(|a| {
            a.id.as_str() == A
                && matches!(
                    a.quota,
                    QuotaState::Exhausted {
                        resets_at: Some(_),
                        ..
                    }
                )
        }),
        "fake-a not marked exhausted with a reset time",
    )?;
    step("failover_mid_turn");

    Ok(s3)
}
