//! Phase 9.2 follow-up gate steps (phase-9-parity.md §9.2.5): queue edit / reorder / run-now / steer, pickers and
//! skill / subagent mentions, typed failures, the file-change `applied` flag, git branch / switch / commit / push /
//! log, and the projects-tree data.

use std::path::Path;

use harness_core_api::workspace::{PushProgress, SwitchOutcome, WorkspaceServices as _};
use harness_core_api::{
    ApiError, ApplyFileChanges, ChangeDirection, EventKind, FailureKind, HarnessCore, ListProjects,
    ListSessions, MentionKind, MentionQuery, Retry, RetryHint, SendMessage, TurnId, TurnOutcome,
};

use super::{api, create, io, run_turn, step, write, Chat, Gate, A, B, C, R};
use crate::check;

mod failures;
mod git_ops;

use failures::{applied_flag, failures};
use git_ops::git_ops;

/// Runs steps 13–18.
pub(super) async fn run(chat: &Chat, gate: &Gate) -> R<()> {
    queue_ops(chat, gate).await?;
    pickers(chat, gate).await?;
    failures(chat, gate).await?;
    applied_flag(chat, gate).await?;
    git_ops(chat).await?;
    projects_tree(gate).await
}

fn queue_of(gate: &Gate, session: &harness_core_api::SessionId) -> Option<Vec<TurnId>> {
    gate.events().into_iter().rev().find_map(|e| match e.kind {
        EventKind::QueueChanged { queue } if e.session_id.as_ref() == Some(session) => {
            Some(queue.into_iter().map(|q| q.turn_id).collect())
        }
        _ => None,
    })
}

// ---- 13. queue ops ----------------------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
async fn queue_ops(chat: &Chat, gate: &Gate) -> R<()> {
    let env = &chat.env;
    let s = create(gate, &env.workspace, A, "queue").await?;
    let running = gate.send(&s, "[[slow 60]] long", None).await?;
    gate.wait_event(
        "queue: first turn started",
        |e| matches!(&e.kind, EventKind::TurnStarted { turn_id, .. } if turn_id == &running),
    )
    .await?;
    let q1 = gate.send(&s, "q-one", None).await?;
    let q2 = gate.send(&s, "q-two", None).await?;
    let q3 = gate.send(&s, "q-three", None).await?;
    api(
        "edit_queued",
        gate.hub
            .edit_queued(s.clone(), q2.clone(), "q-two edited".into())
            .await,
    )?;
    let empty = gate
        .hub
        .edit_queued(s.clone(), q2.clone(), "  ".into())
        .await;
    check(
        matches!(empty, Err(ApiError::InvalidArgument(_))),
        format!("empty edit: {empty:?}"),
    )?;
    let started = gate
        .hub
        .edit_queued(s.clone(), running.clone(), "x".into())
        .await;
    check(
        matches!(started, Err(ApiError::Conflict(_))),
        format!("editing the running turn: {started:?}"),
    )?;
    api(
        "reorder_queued",
        gate.hub
            .reorder_queued(s.clone(), vec![q3.clone(), q1.clone(), q2.clone()])
            .await,
    )?;
    let partial = gate
        .hub
        .reorder_queued(s.clone(), vec![q1.clone(), q2.clone()])
        .await;
    check(
        matches!(partial, Err(ApiError::Conflict(_))),
        format!("reorder without every turn: {partial:?}"),
    )?;
    api(
        "run_queued_now",
        gate.hub.run_queued_now(s.clone(), q2.clone()).await,
    )?;
    let steered = api(
        "steer",
        gate.hub
            .steer(SendMessage {
                session_id: s.clone(),
                text: "steered".into(),
                agent: None,
                client_message_id: None,
                context: Vec::new(),
            })
            .await,
    )?;
    let expected = vec![steered.turn_id.clone(), q2.clone(), q3.clone(), q1.clone()];
    let settled = gate
        .wait_event("queue: order after ops", |e| match &e.kind {
            EventKind::QueueChanged { queue } if e.session_id.as_ref() == Some(&s) => {
                queue.iter().map(|q| &q.turn_id).eq(expected.iter())
            }
            _ => false,
        })
        .await;
    check(
        settled.is_ok(),
        format!(
            "queue after ops {:?}, want {expected:?}",
            queue_of(gate, &s)
        ),
    )?;
    // 正在运行的回合不受影响：它照常完成，之后按新顺序逐个运行（串行准入）。
    gate.completed(&s, &running).await?;
    gate.completed(&s, &q1).await?;
    let order: Vec<TurnId> = gate
        .events()
        .into_iter()
        .filter(|e| e.session_id.as_ref() == Some(&s))
        .filter_map(|e| match e.kind {
            EventKind::TurnStarted {
                turn_id,
                attempt: 1,
                ..
            } => Some(turn_id),
            _ => None,
        })
        .collect();
    let mut want = vec![running];
    want.extend(expected);
    check(
        order == want,
        format!("start order {order:?}, want {want:?}"),
    )?;
    check(
        !env.prompts(A, "q-two edited").is_empty() && env.prompts(A, "q-two").is_empty(),
        "the agent did not get the edited text",
    )?;
    step("queue_ops");
    Ok(())
}

// ---- 14. pickers and skill / subagent mentions ------------------------------------------------------------

async fn pickers(chat: &Chat, gate: &Gate) -> R<()> {
    let ws = &chat.env.workspace;
    write(
        &ws.join(".claude")
            .join("skills")
            .join("gate-skill")
            .join("SKILL.md"),
        "---\nname: gate-skill\ndescription: Gate skill\n---\nDo it.\n",
    )?;
    write(
        &ws.join(".claude").join("agents").join("gate-helper.md"),
        "---\nname: gate-helper\ndescription: Gate helper agent\n---\nHelp.\n",
    )?;
    let s = create(gate, ws, A, "pickers").await?;
    let found = api(
        "mentions",
        gate.hub
            .mention_candidates(MentionQuery {
                session_id: s.clone(),
                query: "gate".into(),
                kinds: vec![MentionKind::Skill, MentionKind::Subagent],
                limit: 20,
            })
            .await,
    )?;
    let skill = found
        .iter()
        .find(|c| c.kind == MentionKind::Skill && c.label == "gate-skill");
    let helper = found
        .iter()
        .find(|c| c.kind == MentionKind::Subagent && c.label == "gate-helper");
    check(
        skill.is_some_and(|c| c.insert_text.is_none() && c.detail == "Gate skill")
            && helper.is_some_and(|c| c.insert_text.as_deref() == Some("@agent-gate-helper")),
        format!("skill / subagent candidates {found:?}"),
    )?;
    let all = api(
        "mentions (all kinds)",
        gate.hub
            .mention_candidates(MentionQuery {
                session_id: s.clone(),
                query: String::new(),
                kinds: Vec::new(),
                limit: 50,
            })
            .await,
    )?;
    let first_config = all
        .iter()
        .position(|c| matches!(c.kind, MentionKind::Skill | MentionKind::Subagent));
    let last_other = all
        .iter()
        .rposition(|c| !matches!(c.kind, MentionKind::Skill | MentionKind::Subagent));
    check(
        first_config.is_some() && first_config > last_other,
        "skills / subagents do not rank after files, folders and sessions",
    )?;
    // 模式 / 模型 / 力度：选择先存为会话偏好，下一次提示前生效。
    run_turn(gate, &s, "warm up").await?;
    api(
        "set_mode",
        gate.hub.set_mode(s.clone(), "plan".into()).await,
    )?;
    api(
        "set_config_option",
        gate.hub
            .set_config_option(s.clone(), "effort".into(), "high".into())
            .await,
    )?;
    let modes = api("list_modes", gate.hub.list_modes(s.clone()).await)?;
    let config = api(
        "list_config_options",
        gate.hub.list_config_options(s.clone()).await,
    )?;
    let effort = config.options.iter().find(|o| o.id == "effort");
    check(
        modes.current.as_deref() == Some("plan")
            && modes.known
            && effort.and_then(|o| o.current.as_deref()) == Some("high"),
        format!("picker values {modes:?} {effort:?}"),
    )?;
    check(
        super::saw(gate, |k| {
            matches!(k, EventKind::SessionOptionsChanged { .. })
        }),
        "SessionOptionsChanged not seen",
    )?;
    // 下一次提示前生效：fake agent 的 [[options]] 报告会话当前值。
    let options = run_turn(gate, &s, "[[options]]").await?;
    let view = gate.completed(&s, &options).await?;
    check(
        view.assistant_text.contains("mode:plan,") && view.assistant_text.contains("effort:high"),
        format!("next prompt options {:?}", view.assistant_text),
    )?;
    step("pickers");
    Ok(())
}

// ---- 15. typed failures -----------------------------------------------------------------------------------

// ---- 17. git ops ------------------------------------------------------------------------------------------

// ---- 18. projects tree ------------------------------------------------------------------------------------

async fn projects_tree(gate: &Gate) -> R<()> {
    let sessions = api(
        "list_sessions",
        gate.hub.list_sessions(ListSessions::default()).await,
    )?;
    let projects = api(
        "list_projects",
        gate.hub.list_projects(ListProjects::default()).await,
    )?;
    check(
        !sessions.is_empty()
            && sessions.iter().all(|s| {
                !s.workspace_path.is_empty()
                    && projects
                        .iter()
                        .any(|p| p.workspace_key == s.workspace_key && p.sessions > 0)
            }),
        "a session has no project row",
    )?;
    check(projects.len() >= 2, format!("projects {projects:?}"))?;
    step("projects_tree");
    Ok(())
}
