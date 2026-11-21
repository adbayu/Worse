use super::{
    check, now_secs, step, AgentId, AgentRuntimeState, AsyncBufReadExt, Duration, Env, EventKind,
    Gate, HarnessCore, Instant, PauseReason, SessionId, SwitchReason, TurnStatus, A, B, C, IDLE, R,
};

/// The OS process-list line of `pid` (diagnostics: tells PID reuse apart from a surviving agent).
pub(super) fn process_line(pid: u32) -> String {
    let output = if cfg!(windows) {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
    } else {
        std::process::Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
    };
    output
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

pub(super) fn pid_alive(pid: u32) -> bool {
    let output = if cfg!(windows) {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
    } else {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
    };
    match output {
        Ok(out) if cfg!(windows) => {
            String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
        }
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

pub(super) async fn sticky_and_swap(env: &Env, gate: &Gate, s3: &SessionId) -> R<()> {
    let s3 = s3.clone();
    // ---- sticky after failover --------------------------------------------------------------------
    let t = gate.send(&s3, "after failover", None).await?;
    let view = gate.completed(&s3, &t).await?;
    check(
        view.agent.as_ref().map(AgentId::as_str) == Some(B),
        "did not stay on fake-b",
    )?;
    check(
        env.prompts(A, "after failover").is_empty(),
        "switched back to the exhausted agent",
    )?;
    let detail = gate
        .hub
        .open_session(s3.clone())
        .await
        .map_err(|e| e.to_string())?;
    check(
        detail.summary.active_agent.as_str() == B,
        "sticky agent not fake-b",
    )?;
    step("sticky_after_failover");

    // ---- manual swap ------------------------------------------------------------------------------
    gate.hub
        .set_active_agent(s3.clone(), AgentId::new(A))
        .await
        .map_err(|e| e.to_string())?;
    gate.wait_event("manual switch event", |e| {
        e.session_id.as_ref() == Some(&s3)
            && matches!(&e.kind, EventKind::AgentSwitched { reason: SwitchReason::Manual, to, .. } if to.as_str() == A)
    })
    .await?;
    let t = gate.send(&s3, "back on a", None).await?;
    let view = gate.completed(&s3, &t).await?;
    check(
        view.agent.as_ref().map(AgentId::as_str) == Some(A),
        "manual swap ignored",
    )?;
    let a_prompt = env
        .prompts(A, "back on a")
        .pop()
        .ok_or("fake-a prompt missing")?
        .join("\n");
    check(
        a_prompt.contains("after failover"),
        "swapped-in agent missed the turns served by fake-b",
    )?;
    step("manual_swap");

    // ---- failover at start ------------------------------------------------------------------------
    let user = "start fails [[quota-start fake-a]]";
    let t = gate.send(&s3, user, None).await?;
    let view = gate.completed(&s3, &t).await?;
    check(
        view.agent.as_ref().map(AgentId::as_str) == Some(B)
            && view
                .attempts
                .first()
                .is_some_and(|a| a.status == "quotaExhausted" && a.partial_text.is_empty()),
        format!("start failover attempts: {:?}", view.attempts),
    )?;
    step("failover_at_start");
    Ok(())
}

pub(super) async fn exhaustion(env: &Env, gate: &Gate, s3: &SessionId) -> R<()> {
    let s3 = s3.clone();
    // ---- all exhausted: pause, resume by manual swap ----------------------------------------------
    let far = now_secs() + 3600;
    for agent in [A, B, C] {
        env.set_exhausted(agent, far)?;
    }
    let user = "everyone is out";
    let t = gate.send(&s3, user, None).await?;
    let view = gate
        .wait_turn(&s3, &t, |v| v.status == TurnStatus::Paused, "paused turn")
        .await?;
    // fake-a is still marked exhausted from the start-failover step, so this round tries fake-b and fake-c only.
    let tried: Vec<&str> = view.attempts.iter().map(|a| a.agent.as_str()).collect();
    check(
        tried == [B, C] && view.attempts.iter().all(|a| a.status == "quotaExhausted"),
        format!("paused after attempts {tried:?}"),
    )?;
    let paused = gate
        .wait_event(
            "pause event",
            |e| matches!(&e.kind, EventKind::TurnPaused { turn_id, .. } if turn_id == &t),
        )
        .await?;
    if let EventKind::TurnPaused {
        reason: PauseReason::AllAgentsExhausted { agents },
        ..
    } = &paused.kind
    {
        check(
            agents.len() == 3 && agents.iter().all(|(_, reset)| reset.is_some()),
            format!("pause reason: {agents:?}"),
        )?;
    }
    // A queued message behind a paused turn stays queued.
    let behind = gate.send(&s3, "queued behind pause", None).await?;
    env.clear_exhausted(B);
    gate.hub
        .set_active_agent(s3.clone(), AgentId::new(B))
        .await
        .map_err(|e| e.to_string())?;
    let view = gate.completed(&s3, &t).await?;
    check(
        view.agent.as_ref().map(AgentId::as_str) == Some(B),
        "resumed on wrong agent",
    )?;
    let b_blocks = env.prompts(B, user);
    check(
        b_blocks.len() == 2,
        format!("fake-b prompts for paused turn: {}", b_blocks.len()),
    )?;
    check(
        b_blocks
            .last()
            .is_some_and(|blocks| blocks.iter().filter(|b| b.contains(user)).count() == 1),
        "user message duplicated after resume",
    )?;
    gate.completed(&s3, &behind).await?;
    step("all_exhausted_pause_manual_resume");

    // ---- all exhausted: resume when a reset passes -------------------------------------------------
    let soon = now_secs() + 2;
    for agent in [A, B, C] {
        env.set_exhausted(agent, soon)?;
    }
    let user = "wait for reset";
    let t = gate.send(&s3, user, None).await?;
    gate.wait_turn(&s3, &t, |v| v.status == TurnStatus::Paused, "paused turn")
        .await?;
    for agent in [A, B, C] {
        env.clear_exhausted(agent);
    }
    let view = gate.completed(&s3, &t).await?;
    // Agents still marked exhausted (far reset) are skipped; the turn resumes once fake-b's reset passes.
    check(
        view.attempts.len() >= 2
            && view
                .attempts
                .last()
                .is_some_and(|a| a.status == "completed"),
        format!("reset resume attempts: {:?}", view.attempts),
    )?;
    step("all_exhausted_pause_reset_resume");
    Ok(())
}

pub(super) async fn idle(gate: &Gate) -> R<()> {
    // ---- idle shutdown ----------------------------------------------------------------------------
    let deadline = Instant::now() + IDLE + Duration::from_secs(15);
    loop {
        let agents = gate.hub.list_agents().await.map_err(|e| e.to_string())?;
        if agents
            .iter()
            .all(|a| a.runtime == AgentRuntimeState::Stopped)
        {
            break;
        }
        check(
            Instant::now() < deadline,
            format!(
                "agents not stopped after idle: {:?}",
                agents.iter().map(|a| a.runtime).collect::<Vec<_>>()
            ),
        )?;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let pids: Vec<u32> = gate
        .events()
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentRuntime { pid: Some(pid), .. } => Some(*pid),
            _ => None,
        })
        .collect();
    check(!pids.is_empty(), "no agent pid observed")?;
    // 修复说明：门禁偶发 "agent processes still alive after idle stop"。诊断显示失败 pid 的事件序列以正常
    // Stopped 结尾，且生成失败消息时 tasklist 已查不到该进程：hub 已 wait 到退出，但 Windows 进程列表短暂仍列出
    // 正在销毁的进程。存活判定因此在短时间内复查（进程真的残留时 3 秒后仍会失败），而不是一次快照即判失败。
    let mut alive: Vec<u32> = pids.iter().copied().filter(|p| pid_alive(*p)).collect();
    let recheck = Instant::now() + Duration::from_secs(3);
    while !alive.is_empty() && Instant::now() < recheck {
        tokio::time::sleep(Duration::from_millis(200)).await;
        alive.retain(|p| pid_alive(*p));
    }
    let history = |pid: u32| -> Vec<String> {
        gate.events()
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::AgentRuntime {
                    agent,
                    state,
                    pid: Some(p),
                } if *p == pid => Some(format!("{agent}:{state:?}")),
                _ => None,
            })
            .collect()
    };
    check(
        alive.is_empty(),
        format!(
            "agent processes still alive after idle stop: {:?}",
            alive
                .iter()
                .map(|p| (*p, history(*p), process_line(*p)))
                .collect::<Vec<_>>()
        ),
    )?;
    step("idle_shutdown");

    // ---- idle RSS of the core (measured by the driver script) -------------------------------------
    println!("IDLE_RSS_CHECK {}", std::process::id());
    let mut line = String::new();
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let _ = tokio::time::timeout(Duration::from_secs(30), stdin.read_line(&mut line)).await;
    println!("IDLE_RSS_ACK {}", line.trim());
    Ok(())
}

pub(super) async fn restart(env: &Env, gate: Gate, s2: &SessionId, s3: &SessionId) -> R<()> {
    let s2 = s2.clone();
    // ---- resume after restart ---------------------------------------------------------------------
    let handle_before = env
        .log_entries()
        .into_iter()
        .filter(|e| e["event"] == "new" && e["agent"] == A)
        .count();
    gate.hub.shutdown().await.map_err(|e| e.to_string())?;
    drop(gate);
    let gate = Gate::start(env).await?;
    let detail = gate
        .hub
        .open_session(s2.clone())
        .await
        .map_err(|e| e.to_string())?;
    check(
        detail.turns.len() == 4,
        format!("transcript after restart: {} turns", detail.turns.len()),
    )?;
    check(
        detail.summary.active_agent.as_str() == A,
        "sticky agent lost on restart",
    )?;
    // Regression: every attempt of a turn resumed after a pause is persisted (fake-b ran it in two rounds).
    let failover_detail = gate
        .hub
        .open_session(s3.clone())
        .await
        .map_err(|e| e.to_string())?;
    let resumed = failover_detail
        .turns
        .iter()
        .find(|t| t.user_text == "everyone is out")
        .ok_or("resumed turn missing after restart")?;
    let tried: Vec<&str> = resumed.attempts.iter().map(|a| a.agent.as_str()).collect();
    check(
        tried == [B, C, B] && resumed.status == TurnStatus::Completed,
        format!(
            "persisted attempts after restart: {tried:?} {:?}",
            resumed.status
        ),
    )?;
    // fake-a is still marked exhausted (persisted, far reset); a manual swap clears it.
    gate.hub
        .set_active_agent(s2.clone(), AgentId::new(A))
        .await
        .map_err(|e| e.to_string())?;
    let loads_before = env
        .log_entries()
        .iter()
        .filter(|e| e["event"] == "load")
        .count();
    let t = gate.send(&s2, "after restart", None).await?;
    let view = gate.completed(&s2, &t).await?;
    check(
        view.agent.as_ref().map(AgentId::as_str) == Some(A),
        "restart turn on wrong agent",
    )?;
    let entries = env.log_entries();
    check(
        entries.iter().filter(|e| e["event"] == "load").count() > loads_before,
        "native session was not resumed with session/load",
    )?;
    check(
        entries
            .iter()
            .filter(|e| e["event"] == "new" && e["agent"] == A)
            .count()
            == handle_before,
        "a new native session was created instead of resuming",
    )?;
    let blocks = env
        .prompts(A, "after restart")
        .pop()
        .ok_or("restart prompt missing")?
        .join("\n");
    check(
        !blocks.contains("kind=\"seed\""),
        "resumed session was re-seeded",
    )?;
    step("resume_after_restart");

    gate.hub.shutdown().await.map_err(|e| e.to_string())?;
    let pids: Vec<u32> = gate
        .events()
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentRuntime { pid: Some(pid), .. } => Some(*pid),
            _ => None,
        })
        .collect();
    let alive: Vec<u32> = pids.iter().copied().filter(|p| pid_alive(*p)).collect();
    check(
        alive.is_empty(),
        format!("agent processes alive after hub shutdown: {alive:?}"),
    )?;
    step("shutdown_cleanup");
    Ok(())
}
