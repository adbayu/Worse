use super::{
    api, check, create, io, run_turn, step, write, ApplyFileChanges, ChangeDirection, Chat,
    EventKind, FailureKind, Gate, HarnessCore, Retry, RetryHint, TurnId, TurnOutcome, A, B, C, R,
};

pub(super) async fn failed_turn(
    gate: &Gate,
    session: &harness_core_api::SessionId,
    text: &str,
) -> R<(TurnId, harness_core_api::Failure)> {
    let turn = gate.send(session, text, None).await?;
    let view = gate.finished(session, &turn).await?;
    check(
        view.status == harness_core_api::TurnStatus::Failed,
        format!("{text}: turn ended {:?}", view.status),
    )?;
    let failure_at = gate
        .event_index(|k| matches!(k, EventKind::TurnFailure { turn_id, .. } if turn_id == &turn))
        .ok_or(format!("{text}: no TurnFailure"))?;
    let finished_at = gate
        .event_index(|k| {
            matches!(k, EventKind::TurnFinished { turn_id, outcome: TurnOutcome::Failed { .. }, .. }
                if turn_id == &turn)
        })
        .ok_or(format!("{text}: no failed TurnFinished"))?;
    check(
        failure_at < finished_at,
        format!("{text}: TurnFailure after TurnFinished"),
    )?;
    gate.events()
        .into_iter()
        .find_map(|e| match e.kind {
            EventKind::TurnFailure {
                turn_id, failure, ..
            } if turn_id == turn => Some((turn_id, failure)),
            _ => None,
        })
        .ok_or_else(|| format!("{text}: failure missing"))
}

pub(super) async fn failures(chat: &Chat, gate: &Gate) -> R<()> {
    let env = &chat.env;
    let s = create(gate, &env.workspace, B, "failures").await?;
    let (_, auth) = failed_turn(gate, &s, "[[fail auth]]").await?;
    check(
        auth.kind == FailureKind::Auth && auth.retry == RetryHint::Login,
        format!("auth failure {auth:?}"),
    )?;
    let (_, other) = failed_turn(gate, &s, "[[fail other]]").await?;
    check(
        other.kind == FailureKind::Other
            && other.retry == RetryHint::Retry
            && other
                .details
                .as_deref()
                .is_some_and(|d| d.contains("code -32603") && d.contains("fake failure")),
        format!("other failure {other:?}"),
    )?;
    let (_, transient) = failed_turn(gate, &s, "[[fail transient 7]]").await?;
    check(
        transient.kind == FailureKind::Transient
            && transient.retry == RetryHint::After { secs: Some(7) },
        format!("transient failure {transient:?}"),
    )?;
    let (_, exited) = failed_turn(gate, &s, "[[fail exit]]").await?;
    check(
        exited.kind == FailureKind::ProcessExited
            && exited.retry == RetryHint::Retry
            && exited
                .trace
                .as_deref()
                .is_some_and(|t| t.contains("simulated crash")),
        format!("exit failure {exited:?}"),
    )?;
    // 进程退出后下一回合重新拉起 agent，照常完成。
    run_turn(gate, &s, "after crash").await?;
    let flag = env.state.join(format!("{C}.initfail"));
    write(&flag, "1")?;
    let c = create(gate, &env.workspace, C, "init failure").await?;
    let init = failed_turn(gate, &c, "hello").await;
    io("rm initfail", std::fs::remove_file(&flag))?;
    let (init_turn, init) = init?;
    check(
        init.kind == FailureKind::InitFailed && init.retry == RetryHint::Retry,
        format!("init failure {init:?}"),
    )?;
    // 重试同一回合（RetryInit { session } 的 hub 路径）：标志已删除，回合应照常完成。
    let retried = api(
        "retry_turn",
        gate.hub
            .retry_turn(Retry {
                session_id: c.clone(),
                turn_id: init_turn,
                agent: None,
                restore_files: false,
            })
            .await,
    )?;
    gate.completed(&c, &retried.turn_id).await?;
    // 快照的 event_seq 覆盖此前本会话的全部事件（客户端据此丢弃已包含在快照里的旧事件）。
    let detail = api("open_session", gate.hub.open_session(c.clone()).await)?;
    let last = gate
        .events()
        .into_iter()
        .filter(|e| e.session_id.as_ref() == Some(&c))
        .map(|e| e.seq)
        .max()
        .unwrap_or(0);
    check(
        detail.event_seq >= last && last > 0,
        format!(
            "snapshot event_seq {} < last session event {last}",
            detail.event_seq
        ),
    )?;
    step("failures");
    Ok(())
}

// ---- 16. applied flag -------------------------------------------------------------------------------------

pub(super) async fn applied_flag(chat: &Chat, gate: &Gate) -> R<()> {
    let s = create(gate, &chat.repo, A, "applied").await?;
    let turn = run_turn(gate, &s, "[[write notes/c.txt cee]]").await?;
    let plan = |direction| {
        gate.hub
            .plan_file_changes(s.clone(), turn.clone(), direction)
    };
    let undo = api("plan undo", plan(ChangeDirection::Undo).await)?;
    let reapply = api("plan reapply", plan(ChangeDirection::Reapply).await)?;
    check(
        undo.available && !undo.applied && reapply.applied,
        format!(
            "fresh turn: undo.applied {} reapply.applied {}",
            undo.applied, reapply.applied
        ),
    )?;
    api(
        "apply undo",
        gate.hub
            .apply_file_changes(ApplyFileChanges {
                session_id: s.clone(),
                turn_id: turn.clone(),
                direction: ChangeDirection::Undo,
                paths: None,
                include_unsafe: false,
            })
            .await,
    )?;
    let undo = api("plan undo (after)", plan(ChangeDirection::Undo).await)?;
    let reapply = api("plan reapply (after)", plan(ChangeDirection::Reapply).await)?;
    check(
        undo.applied && !reapply.applied,
        format!(
            "after undo: undo.applied {} reapply.applied {}",
            undo.applied, reapply.applied
        ),
    )?;
    step("applied_flag");
    Ok(())
}
