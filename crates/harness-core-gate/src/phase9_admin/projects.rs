use super::{
    busy_turn, check, create_in, e, listed, project, sessions_changed, step, ApiError, Env, Gate,
    HarnessCore, ListProjects, R,
};

pub(super) async fn projects(env: &Env, gate: &Gate) -> R<()> {
    let hub = &gate.hub;
    let other = env
        .workspace
        .parent()
        .ok_or("workspace has no parent")?
        .join("ws9-other");
    std::fs::create_dir_all(&other).map_err(|err| err.to_string())?;
    let s = create_in(gate, &other, "other project").await?;
    let t = gate.send(&s, "hello other", None).await?;
    gate.completed(&s, &t).await?;
    let info = project(gate, &other, false)
        .await?
        .ok_or("new project not listed")?;
    check(
        info.sessions == 1 && !info.unused && info.removed_at.is_none(),
        format!("project row: {info:?}"),
    )?;
    check(
        project(gate, &env.workspace, false).await?.is_some(),
        "first project not listed",
    )?;
    let found = hub
        .list_projects(ListProjects {
            query: Some("WS9-OTHER".into()),
            removed: false,
        })
        .await
        .map_err(e)?;
    check(found.len() == 1, format!("project query: {found:?}"))?;
    let key = info.workspace_key.clone();
    // Remove keeps data; it only hides the project and its sessions.
    let changes = sessions_changed(gate);
    hub.remove_projects(vec![key.clone()], false)
        .await
        .map_err(e)?;
    check(
        project(gate, &other, false).await?.is_none()
            && project(gate, &other, true)
                .await?
                .is_some_and(|p| p.removed_at.is_some()),
        "removed project still listed as active",
    )?;
    check(
        !listed(gate, &s).await?,
        "session of removed project listed",
    )?;
    check(
        hub.stored_rows(s.clone()).await.map_err(e)? > 0,
        "remove without delete lost data",
    )?;
    check(
        sessions_changed(gate) > changes,
        "no SessionsChanged on remove",
    )?;
    hub.restore_projects(vec![key.clone()]).await.map_err(e)?;
    check(
        listed(gate, &s).await?,
        "restored project's session not listed",
    )?;
    // Remove with data while a turn runs: refused, nothing changed.
    let t = busy_turn(gate, &s).await?;
    check(
        matches!(
            hub.remove_projects(vec![key.clone()], true).await,
            Err(ApiError::Busy(_))
        ),
        "remove-with-data of a running session not refused",
    )?;
    check(
        project(gate, &other, false).await?.is_some()
            && hub.stored_rows(s.clone()).await.map_err(e)? > 0,
        "refused remove changed state",
    )?;
    gate.completed(&s, &t).await?;
    hub.remove_projects(vec![key.clone()], true)
        .await
        .map_err(e)?;
    check(
        hub.stored_rows(s.clone()).await.map_err(e)? == 0,
        "remove-with-data left rows",
    )?;
    check(
        matches!(
            hub.open_session(s.clone()).await,
            Err(ApiError::NotFound(_))
        ),
        "deleted session still opens",
    )?;
    // A new session in a removed project restores it.
    let again = create_in(gate, &other, "back again").await?;
    check(
        project(gate, &other, false)
            .await?
            .is_some_and(|p| p.sessions == 1 && p.removed_at.is_none()),
        "create_session did not restore the project",
    )?;
    check(listed(gate, &again).await?, "new session not listed")?;
    step("projects");
    Ok(())
}
