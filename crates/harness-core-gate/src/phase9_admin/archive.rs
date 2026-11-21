use super::{
    busy_turn, check, e, listed, step, ApiError, Env, Gate, HarnessCore, ListArchived,
    ListSessions, SessionId, SessionSelection, A, R,
};

/// One continuous scenario: the steps depend on the archive state left by the previous ones.
#[allow(clippy::too_many_lines)]
pub(super) async fn archive(env: &Env, gate: &Gate) -> R<()> {
    let hub = &gate.hub;
    let mut ids = Vec::new();
    for title in ["Archive alpha", "Archive beta"] {
        let s = gate.session(env, A, title).await?;
        let t = gate.send(&s, title, None).await?;
        gate.completed(&s, &t).await?;
        ids.push(s);
    }
    let (alpha, beta) = (ids[0].clone(), ids[1].clone());
    hub.archive_sessions(ids.clone()).await.map_err(e)?;
    check(
        !listed(gate, &alpha).await? && !listed(gate, &beta).await?,
        "archived sessions still listed",
    )?;
    let page = hub
        .list_archived(ListArchived::default())
        .await
        .map_err(e)?;
    check(
        page.total == 2 && page.rows.len() == 2 && !page.has_more,
        format!(
            "archive page: total {} rows {}",
            page.total,
            page.rows.len()
        ),
    )?;
    let first = hub
        .list_archived(ListArchived {
            limit: Some(1),
            sort: harness_core_api::ArchiveSort::Title,
            ..ListArchived::default()
        })
        .await
        .map_err(e)?;
    check(
        first.has_more && first.rows.first().is_some_and(|r| r.summary.id == alpha),
        "archive paging/sort",
    )?;
    let query = hub
        .list_archived(ListArchived {
            query: Some("BETA".into()),
            ..ListArchived::default()
        })
        .await
        .map_err(e)?;
    check(
        query.total == 1 && query.rows.first().is_some_and(|r| r.summary.id == beta),
        "archive query",
    )?;
    check(
        matches!(
            hub.archive_sessions(vec![SessionId::new("ses_nope")]).await,
            Err(ApiError::NotFound(_))
        ),
        "archiving an unknown session not NotFound",
    )?;
    // Busy session: refused, stays active.
    let busy = gate.session(env, A, "Archive busy").await?;
    let t = busy_turn(gate, &busy).await?;
    check(
        matches!(
            hub.archive_sessions(vec![busy.clone()]).await,
            Err(ApiError::Busy(_))
        ),
        "archiving a running session not refused",
    )?;
    gate.completed(&busy, &t).await?;
    check(
        listed(gate, &busy).await?,
        "refused archive hid the session",
    )?;
    // Unarchive by selection.
    let n = hub
        .unarchive(SessionSelection::Sessions(vec![alpha.clone()]))
        .await
        .map_err(e)?;
    check(n == 1 && listed(gate, &alpha).await?, "unarchive")?;
    // Sending to an archived session unarchives it and runs the turn.
    let t = gate.send(&beta, "wake up", None).await?;
    gate.completed(&beta, &t).await?;
    check(listed(gate, &beta).await?, "send did not unarchive")?;
    // Delete archived by project.
    hub.archive_sessions(vec![beta.clone()]).await.map_err(e)?;
    let key = hub
        .list_sessions(ListSessions::default())
        .await
        .map_err(e)?
        .into_iter()
        .find(|s| s.id == alpha)
        .map(|s| s.workspace_key)
        .ok_or("alpha missing")?;
    let deleted = hub
        .delete_archived(SessionSelection::Project(key))
        .await
        .map_err(e)?;
    check(deleted == 1, format!("delete_archived removed {deleted}"))?;
    check(
        hub.stored_rows(beta.clone()).await.map_err(e)? == 0
            && hub.stored_rows(alpha.clone()).await.map_err(e)? > 0,
        "delete_archived removed the wrong rows",
    )?;
    let page = hub
        .list_archived(ListArchived::default())
        .await
        .map_err(e)?;
    check(page.total == 0, "archive not empty after delete")?;
    step("archive");
    Ok(())
}
