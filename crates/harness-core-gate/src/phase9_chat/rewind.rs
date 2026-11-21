use super::{
    api, check, create, run_turn, saw, step, turns, write, AgentId, ApiError, ApplyFileChanges,
    ChangeDirection, Chat, EditResend, Env, EventKind, FileAction, Gate, HarnessCore, Retry,
    Rewind, SessionId, A, B, R,
};

#[allow(clippy::too_many_lines)]
pub(super) async fn rewind_edit_retry(env: &Env, gate: &Gate) -> R<SessionId> {
    let s = create(gate, &env.workspace, A, "rewind").await?;
    let t1 = run_turn(gate, &s, "r one").await?;
    let t2 = run_turn(gate, &s, "r two").await?;
    run_turn(gate, &s, "r three").await?;
    let busy = gate.send(&s, "[[slow 40]] r busy", None).await?;
    let attempt = gate
        .hub
        .rewind_session(Rewind {
            session_id: s.clone(),
            turn_id: t2.clone(),
            restore_files: false,
        })
        .await;
    check(
        matches!(attempt, Err(ApiError::Busy(_))),
        format!("rewind while running: {attempt:?}"),
    )?;
    gate.completed(&s, &busy).await?;
    let news_before = env
        .log_entries()
        .iter()
        .filter(|e| e["event"] == "new" && e["agent"] == A)
        .count();
    let result = api(
        "rewind",
        gate.hub
            .rewind_session(Rewind {
                session_id: s.clone(),
                turn_id: t2.clone(),
                restore_files: false,
            })
            .await,
    )?;
    check(
        result.removed_turns == 3 && result.user_text == "r two",
        format!("rewind result {result:?}"),
    )?;
    check(turns(gate, &s).await?.len() == 1, "rewind left later turns")?;
    check(
        saw(gate, |k| {
            matches!(k, EventKind::TurnsRemoved { from_seq: 2 })
        }),
        "TurnsRemoved not seen",
    )?;
    let after = run_turn(gate, &s, "r after").await?;
    let news_after = env
        .log_entries()
        .iter()
        .filter(|e| e["event"] == "new" && e["agent"] == A)
        .count();
    check(
        news_after > news_before,
        "rewind did not drop the agent session handle",
    )?;
    let prompts = env.prompts(A, "r after");
    let blocks = prompts.last().ok_or("post-rewind prompt not logged")?;
    let seed = blocks
        .iter()
        .find(|b| b.contains("kind=\"seed\""))
        .ok_or("post-rewind prompt has no seed")?;
    check(
        seed.contains("r one") && !seed.contains("r two") && !seed.contains("r three"),
        "post-rewind seed holds removed turns",
    )?;
    let edited = api(
        "edit_and_resend",
        gate.hub
            .edit_and_resend(EditResend {
                session_id: s.clone(),
                turn_id: after,
                text: "r edited".into(),
                context: None,
                agent: None,
                restore_files: false,
            })
            .await,
    )?;
    gate.completed(&s, &edited.turn_id).await?;
    let now = turns(gate, &s).await?;
    check(
        now.len() == 2 && now[1].user_text == "r edited" && now[1].id == edited.turn_id,
        "edit-and-resend did not replace turn 2",
    )?;
    let old = gate
        .hub
        .retry_turn(Retry {
            session_id: s.clone(),
            turn_id: t1,
            agent: None,
            restore_files: false,
        })
        .await;
    check(
        matches!(old, Err(ApiError::InvalidArgument(_))),
        format!("retry of a non-last turn: {old:?}"),
    )?;
    let retried = api(
        "retry",
        gate.hub
            .retry_turn(Retry {
                session_id: s.clone(),
                turn_id: edited.turn_id,
                agent: Some(AgentId::new(B)),
                restore_files: false,
            })
            .await,
    )?;
    let view = gate.completed(&s, &retried.turn_id).await?;
    let now = turns(gate, &s).await?;
    check(
        now.len() == 2
            && view.user_text == "r edited"
            && view.agent.as_ref().map(AgentId::as_str) == Some(B),
        "retry did not re-run the last turn with the chosen agent",
    )?;
    step("rewind_edit_retry");
    Ok(s)
}

// ---- 4. file undo -----------------------------------------------------------------------------------------

pub(super) fn norm(path: &str) -> String {
    path.replace('\\', "/")
}

#[allow(clippy::too_many_lines)]
pub(super) async fn file_undo(chat: &Chat, gate: &Gate) -> R<()> {
    let repo = &chat.repo;
    let s = create(gate, repo, A, "files").await?;
    let turn = run_turn(gate, &s, "[[write notes/a.txt first]]").await?;
    let file = repo.join("notes").join("a.txt");
    check(file.exists(), "agent write missing")?;
    let plan = api(
        "plan undo",
        gate.hub
            .plan_file_changes(s.clone(), turn.clone(), ChangeDirection::Undo)
            .await,
    )?;
    let entry = plan
        .files
        .iter()
        .find(|f| norm(&f.path) == "notes/a.txt")
        .ok_or(format!("undo plan misses notes/a.txt: {plan:?}"))?;
    check(
        plan.available && entry.safe && entry.action == FileAction::Delete,
        format!("undo plan entry {entry:?}"),
    )?;
    let apply = |direction, include_unsafe| ApplyFileChanges {
        session_id: s.clone(),
        turn_id: turn.clone(),
        direction,
        paths: None,
        include_unsafe,
    };
    let report = api(
        "apply undo",
        gate.hub
            .apply_file_changes(apply(ChangeDirection::Undo, false))
            .await,
    )?;
    check(
        report.applied.iter().any(|p| norm(p) == "notes/a.txt") && !file.exists(),
        format!("undo not applied: {report:?}"),
    )?;
    api(
        "apply reapply",
        gate.hub
            .apply_file_changes(apply(ChangeDirection::Reapply, false))
            .await,
    )?;
    check(
        std::fs::read_to_string(&file).ok().as_deref() == Some("first\n"),
        "reapply did not restore the turn's content",
    )?;
    write(&file, "changed later\n")?;
    let plan = api(
        "plan undo (modified)",
        gate.hub
            .plan_file_changes(s.clone(), turn.clone(), ChangeDirection::Undo)
            .await,
    )?;
    check(
        plan.files
            .iter()
            .any(|f| norm(&f.path) == "notes/a.txt" && !f.safe && f.reason.is_some()),
        "a file modified after the turn is not unsafe",
    )?;
    let skipped = api(
        "apply undo (safe only)",
        gate.hub
            .apply_file_changes(apply(ChangeDirection::Undo, false))
            .await,
    )?;
    check(
        skipped
            .skipped
            .iter()
            .any(|(p, _)| norm(p) == "notes/a.txt")
            && std::fs::read_to_string(&file).ok().as_deref() == Some("changed later\n"),
        "unsafe file was not skipped",
    )?;
    let forced = api(
        "apply undo (include unsafe)",
        gate.hub
            .apply_file_changes(apply(ChangeDirection::Undo, true))
            .await,
    )?;
    check(
        forced.applied.iter().any(|p| norm(p) == "notes/a.txt") && !file.exists(),
        "include_unsafe did not apply",
    )?;
    let second = run_turn(gate, &s, "[[write notes/b.txt bee]]").await?;
    let b = repo.join("notes").join("b.txt");
    check(b.exists(), "second write missing")?;
    let rewound = api(
        "rewind with files",
        gate.hub
            .rewind_session(Rewind {
                session_id: s.clone(),
                turn_id: second.clone(),
                restore_files: true,
            })
            .await,
    )?;
    let files = rewound.files.ok_or("rewind returned no file report")?;
    check(
        files.applied.iter().any(|p| norm(p) == "notes/b.txt") && !b.exists(),
        format!("rewind did not restore files: {files:?}"),
    )?;
    check(
        saw(gate, |k| matches!(k, EventKind::FilesRestored { .. })),
        "FilesRestored not seen",
    )?;
    step("file_undo");
    Ok(())
}
