use super::{
    api, check, config, create, step, turns, Chat, Duration, Gate, HarnessCore, Instant,
    NewSchedule, ScheduleKind, ScheduleTarget, SessionId, TurnId, UpdateSchedule, A, R,
};

pub(super) async fn turn_count(gate: &Gate, s: &SessionId) -> R<usize> {
    Ok(turns(gate, s).await?.len())
}

pub(super) async fn wait_scheduled(
    gate: &Gate,
    s: &SessionId,
    schedule: &str,
    within: Duration,
) -> R<TurnId> {
    let deadline = Instant::now() + within;
    loop {
        let found = turns(gate, s)
            .await?
            .into_iter()
            .find(|t| t.origin.as_ref().is_some_and(|o| o.schedule_id == schedule));
        if let Some(turn) = found {
            return Ok(turn.id);
        }
        if Instant::now() > deadline {
            return Err(format!("schedule {schedule} did not run within {within:?}"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[allow(clippy::too_many_lines)]
pub(super) async fn schedules(chat: &Chat, gate: Gate) -> R<()> {
    let s = create(&gate, &chat.env.workspace, A, "scheduled").await?;
    check(
        !api("schedules_enabled", gate.hub.schedules_enabled().await)?,
        "scheduler enabled by default",
    )?;
    check(
        api("list_schedules", gate.hub.list_schedules().await)?.is_empty(),
        "schedules exist by default",
    )?;
    let created = api(
        "create_schedule",
        gate.hub
            .create_schedule(NewSchedule {
                title: "every second".into(),
                prompt: "scheduled ping".into(),
                target: ScheduleTarget::Session {
                    session_id: s.clone(),
                },
                kind: ScheduleKind::Interval { seconds: 1 },
                enabled: true,
            })
            .await,
    )?;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    check(
        turn_count(&gate, &s).await? == 0,
        "a schedule ran while the scheduler was off",
    )?;
    api("enable", gate.hub.set_schedules_enabled(true).await)?;
    let first = wait_scheduled(&gate, &s, &created.id, Duration::from_secs(3)).await?;
    api(
        "pause schedule",
        gate.hub
            .update_schedule(UpdateSchedule {
                id: created.id.clone(),
                title: Some("renamed".into()),
                enabled: Some(false),
                ..UpdateSchedule::default()
            })
            .await,
    )?;
    gate.finished(&s, &first).await?;
    let run = api(
        "run now",
        gate.hub.run_schedule_now(created.id.clone()).await,
    )?;
    let view = gate.completed(&run.session_id, &run.turn_id).await?;
    check(
        view.origin
            .as_ref()
            .is_some_and(|o| o.schedule_title == "renamed"),
        "run-now turn has no origin",
    )?;
    api("delete", gate.hub.delete_schedule(created.id.clone()).await)?;
    check(
        !api("list", gate.hub.list_schedules().await)?
            .iter()
            .any(|p| p.id == created.id),
        "schedule not deleted",
    )?;
    // 错过的运行不补跑：停 hub 超过一个周期后重启，重启时不立即运行。
    let restart = create(&gate, &chat.env.workspace, A, "restart").await?;
    let periodic = api(
        "create periodic",
        gate.hub
            .create_schedule(NewSchedule {
                title: "every 2 s".into(),
                prompt: "periodic".into(),
                target: ScheduleTarget::Session {
                    session_id: restart.clone(),
                },
                kind: ScheduleKind::Interval { seconds: 2 },
                enabled: true,
            })
            .await,
    )?;
    let turn = wait_scheduled(&gate, &restart, &periodic.id, Duration::from_secs(4)).await?;
    gate.finished(&restart, &turn).await?;
    api("shutdown", gate.hub.shutdown().await)?;
    drop(gate);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let gate = super::super::phase9::start(config(chat)?).await?;
    let started = Instant::now();
    let count = turn_count(&gate, &restart).await?;
    tokio::time::sleep(Duration::from_millis(700)).await;
    check(
        turn_count(&gate, &restart).await? == count && started.elapsed() < Duration::from_secs(2),
        "missed runs were caught up after the restart",
    )?;
    let listed = api("list", gate.hub.list_schedules().await)?;
    let next = listed
        .iter()
        .find(|p| p.id == periodic.id)
        .and_then(|p| p.next_run)
        .ok_or("periodic schedule lost its next run")?;
    check(
        next >= super::super::now_secs() * 1000 - 1000,
        "next run was not rescheduled from the restart",
    )?;
    api("disable", gate.hub.set_schedules_enabled(false).await)?;
    api(
        "delete periodic",
        gate.hub.delete_schedule(periodic.id).await,
    )?;
    step("schedules");
    api("final shutdown", gate.hub.shutdown().await)
}
