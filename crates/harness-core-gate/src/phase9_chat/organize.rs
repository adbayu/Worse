use super::{
    api, check, create, run_turn, saw, step, summary_of, turns, ApiError, Env, EventKind,
    ForkSession, Gate, GroupColor, HarnessCore, UpdateGroup, A, R,
};

#[allow(clippy::too_many_lines)]
pub(super) async fn organize(env: &Env, gate: &Gate) -> R<()> {
    let ws = env.workspace.clone();
    let s1 = create(gate, &ws, A, "first").await?;
    let s2 = create(gate, &ws, A, "second").await?;
    api(
        "rename",
        gate.hub
            .rename_session(s1.clone(), "  Renamed  ".into())
            .await,
    )?;
    check(
        summary_of(gate, &s1).await?.title == "Renamed",
        "rename not trimmed/saved",
    )?;
    let invalid = gate.hub.rename_session(s1.clone(), "   ".into()).await;
    check(
        matches!(invalid, Err(ApiError::InvalidArgument(_))),
        format!("empty rename: {invalid:?}"),
    )?;
    api("pin", gate.hub.set_pinned(vec![s1.clone()], true).await)?;
    check(summary_of(gate, &s1).await?.pinned, "pin not saved")?;
    // 新会话已读；回合结束后未读；显式标记已读/未读。
    check(!summary_of(gate, &s2).await?.unread, "new session unread")?;
    run_turn(gate, &s2, "unread rule").await?;
    check(
        summary_of(gate, &s2).await?.unread,
        "finished turn did not mark unread",
    )?;
    api("read", gate.hub.set_unread(s2.clone(), false).await)?;
    check(
        !summary_of(gate, &s2).await?.unread,
        "set_unread(false) ignored",
    )?;
    api("unread", gate.hub.set_unread(s2.clone(), true).await)?;
    check(
        summary_of(gate, &s2).await?.unread,
        "set_unread(true) ignored",
    )?;

    let colors = [
        GroupColor::Gray,
        GroupColor::Red,
        GroupColor::Orange,
        GroupColor::Yellow,
        GroupColor::Green,
        GroupColor::Blue,
        GroupColor::Purple,
    ];
    let mut ids = Vec::new();
    for (i, color) in colors.into_iter().enumerate() {
        let group = api(
            "create_group",
            gate.hub.create_group(format!("Group {i}"), color).await,
        )?;
        ids.push(group.id);
    }
    let bad = gate.hub.create_group(String::new(), GroupColor::Red).await;
    check(
        matches!(bad, Err(ApiError::InvalidArgument(_))),
        format!("empty group name: {bad:?}"),
    )?;
    let listed = api("list_groups", gate.hub.list_groups().await)?;
    check(
        listed.iter().map(|g| &g.id).eq(ids.iter()) && listed[6].color == GroupColor::Purple,
        "groups not listed in creation order with colours",
    )?;
    let updated = api(
        "update_group",
        gate.hub
            .update_group(UpdateGroup {
                id: ids[0].clone(),
                name: Some("Work".into()),
                color: Some(GroupColor::Blue),
            })
            .await,
    )?;
    check(
        updated.name == "Work" && updated.color == GroupColor::Blue,
        "group update not applied",
    )?;
    let reversed: Vec<String> = ids.iter().rev().cloned().collect();
    api("reorder", gate.hub.reorder_groups(reversed.clone()).await)?;
    let listed = api("list_groups", gate.hub.list_groups().await)?;
    check(
        listed.iter().map(|g| &g.id).eq(reversed.iter()),
        "reorder not applied",
    )?;
    let partial = gate.hub.reorder_groups(reversed[1..].to_vec()).await;
    check(
        matches!(partial, Err(ApiError::InvalidArgument(_))),
        format!("partial reorder: {partial:?}"),
    )?;
    let work = ids[0].clone();
    api(
        "move s1",
        gate.hub
            .move_sessions(vec![s1.clone()], Some(work.clone()))
            .await,
    )?;
    api(
        "move s2",
        gate.hub
            .move_sessions(vec![s2.clone()], Some(work.clone()))
            .await,
    )?;
    let (a, b) = (summary_of(gate, &s1).await?, summary_of(gate, &s2).await?);
    check(
        a.group_id.as_deref() == Some(work.as_str())
            && b.group_id.as_deref() == Some(work.as_str()),
        "move to group not applied",
    )?;
    check(b.order > a.order, "last moved session is not on top")?;
    api(
        "move s2 out",
        gate.hub.move_sessions(vec![s2.clone()], None).await,
    )?;
    check(
        summary_of(gate, &s2).await?.group_id.is_none(),
        "move to ungrouped",
    )?;
    api("delete group", gate.hub.delete_group(work.clone()).await)?;
    check(
        summary_of(gate, &s1).await?.group_id.is_none(),
        "deleting a group did not ungroup its sessions",
    )?;
    check(
        api("list_groups", gate.hub.list_groups().await)?.len() == 6,
        "group not deleted",
    )?;
    check(
        saw(gate, |k| matches!(k, EventKind::GroupsChanged))
            && saw(gate, |k| matches!(k, EventKind::SessionUpdated { .. })),
        "GroupsChanged / SessionUpdated not seen",
    )?;
    step("organize");
    Ok(())
}

// ---- 2. fork ----------------------------------------------------------------------------------------------

pub(super) async fn fork(env: &Env, gate: &Gate) -> R<()> {
    let source = create(gate, &env.workspace, A, "fork source").await?;
    let first = run_turn(gate, &source, "fork one [[say alpha]]").await?;
    run_turn(gate, &source, "fork two [[say beta]]").await?;
    let forked = api(
        "fork_session",
        gate.hub
            .fork_session(ForkSession {
                session_id: source.clone(),
                up_to_turn: Some(first),
                title: None,
            })
            .await,
    )?;
    let origin = forked
        .forked_from
        .clone()
        .ok_or("fork has no forked_from")?;
    check(
        origin.session_id == source && origin.turn_seq == 1 && forked.turn_count == 1,
        format!("fork origin {origin:?}, turns {}", forked.turn_count),
    )?;
    let copied = turns(gate, &forked.id).await?;
    check(
        copied.len() == 1 && copied[0].user_text.starts_with("fork one"),
        "fork did not copy exactly turn 1",
    )?;
    run_turn(gate, &forked.id, "after fork").await?;
    let prompts = env.prompts(A, "after fork");
    let blocks = prompts.last().ok_or("fork prompt not logged")?;
    let seed = blocks
        .iter()
        .find(|b| b.contains("kind=\"seed\""))
        .ok_or("fork prompt has no seed block")?;
    check(
        seed.contains("fork one") && !seed.contains("fork two"),
        "seed of the fork does not hold exactly the copied turn",
    )?;
    step("fork");
    Ok(())
}
