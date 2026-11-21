use super::{
    api, check, create, norm, run_turn, send_ctx, step, ApiError, BlockKind, ContextItem, Env,
    FindField, FindInSession, Gate, HarnessCore, MentionKind, MentionQuery, SessionId, A, B, R,
};

pub(super) async fn find(env: &Env, gate: &Gate) -> R<()> {
    let s = create(gate, &env.workspace, A, "find").await?;
    run_turn(gate, &s, "[[tools]] Needle question").await?;
    let query = |text: &str, case_sensitive: bool| FindInSession {
        session_id: s.clone(),
        query: text.into(),
        case_sensitive,
        limit: 50,
    };
    let hits = api(
        "find needle",
        gate.hub.find_in_session(query("needle", false)).await,
    )?;
    let user = hits
        .iter()
        .find(|h| h.field == FindField::User)
        .ok_or("no user hit")?;
    check(
        user.start == 10 && user.len == 6 && user.seq == 1,
        format!("user hit offsets {user:?}"),
    )?;
    check(
        hits.iter().any(|h| h.field == FindField::Assistant),
        "no assistant hit",
    )?;
    let tool = api(
        "find tool",
        gate.hub.find_in_session(query("src/lib", false)).await,
    )?;
    check(
        tool.iter()
            .any(|h| h.field == FindField::Tool && h.tool_call_id.is_some() && h.start == 5),
        format!("no tool hit: {tool:?}"),
    )?;
    let exact = api(
        "find case",
        gate.hub.find_in_session(query("needle", true)).await,
    )?;
    check(
        !exact.iter().any(|h| h.field == FindField::User),
        "case-sensitive find matched a different case",
    )?;
    step("find");
    Ok(())
}

// ---- 6. context -------------------------------------------------------------------------------------------

pub(super) fn ctx_items(other: &SessionId) -> Vec<ContextItem> {
    vec![
        ContextItem::File {
            path: "ctx/small.txt".into(),
        },
        ContextItem::Folder { path: "ctx".into() },
        ContextItem::Image {
            path: Some("ctx/pic.png".into()),
            data_base64: None,
            mime: "image/png".into(),
        },
        ContextItem::Text {
            title: Some("note".into()),
            text: "pasted".into(),
        },
        ContextItem::Session {
            session_id: other.clone(),
        },
    ]
}

/// The last six block types of the prompt whose user text is `user`.
pub(super) fn tail_types(env: &Env, agent: &str, user: &str) -> R<Vec<String>> {
    let entry = env
        .log_entries()
        .into_iter()
        .rev()
        .filter(|e| e["event"] == "prompt" && e["agent"] == agent)
        .find(|e| {
            e["blocks"]
                .as_array()
                .and_then(|b| b.last())
                .and_then(|b| b.as_str())
                == Some(user)
        })
        .ok_or(format!("prompt {user} not logged"))?;
    let types: Vec<String> = entry["blockTypes"]
        .as_array()
        .ok_or("no blockTypes")?
        .iter()
        .filter_map(|t| t.as_str().map(str::to_owned))
        .collect();
    Ok(types[types.len().saturating_sub(6)..].to_vec())
}

#[allow(clippy::too_many_lines)]
pub(super) async fn context(env: &Env, gate: &Gate, other: &SessionId) -> R<()> {
    let ws = &env.workspace;
    let sa = create(gate, ws, A, "ctx a").await?;
    let sb = create(gate, ws, B, "ctx b").await?;
    let ta = api(
        "send ctx a",
        send_ctx(gate, &sa, "ctx prompt a", ctx_items(other)).await,
    )?;
    gate.completed(&sa, &ta).await?;
    let tb = api(
        "send ctx b",
        send_ctx(gate, &sb, "ctx prompt b", ctx_items(other)).await,
    )?;
    gate.completed(&sb, &tb).await?;
    let a_types = tail_types(env, A, "ctx prompt a")?;
    check(
        a_types == ["resource", "resource_link", "image", "text", "text", "text"],
        format!("fake-a block types {a_types:?}"),
    )?;
    let b_types = tail_types(env, B, "ctx prompt b")?;
    check(
        b_types
            == [
                "resource_link",
                "resource_link",
                "text",
                "text",
                "text",
                "text",
            ],
        format!("fake-b block types {b_types:?}"),
    )?;
    let stored = gate.turn(&sa, &ta).await?;
    check(stored.context.len() == 5, "turn context not stored")?;

    let eleven: Vec<ContextItem> = (0..11)
        .map(|i| ContextItem::Text {
            title: None,
            text: format!("t{i}"),
        })
        .collect();
    for (what, items) in [
        ("11 items", eleven),
        (
            "6 MB image",
            vec![ContextItem::Image {
                path: Some("big.png".into()),
                data_base64: None,
                mime: "image/png".into(),
            }],
        ),
        (
            "missing file",
            vec![ContextItem::File {
                path: "ctx/missing.txt".into(),
            }],
        ),
    ] {
        let result = send_ctx(gate, &sa, "rejected", items).await;
        check(
            matches!(result, Err(ApiError::InvalidArgument(_))),
            format!("{what} accepted: {result:?}"),
        )?;
    }
    let kinds = |rows: &[harness_core_api::ResolvedItem]| -> Vec<BlockKind> {
        rows.iter().map(|r| r.block_kind).collect()
    };
    let ra = api(
        "resolve a",
        gate.hub.resolve_context(sa.clone(), ctx_items(other)).await,
    )?;
    check(
        ra.iter().all(|r| r.ok)
            && kinds(&ra)
                == [
                    BlockKind::Resource,
                    BlockKind::ResourceLink,
                    BlockKind::Image,
                    BlockKind::Text,
                    BlockKind::Text,
                ],
        format!("resolve a {ra:?}"),
    )?;
    let rb = api(
        "resolve b",
        gate.hub.resolve_context(sb.clone(), ctx_items(other)).await,
    )?;
    check(
        kinds(&rb)
            == [
                BlockKind::ResourceLink,
                BlockKind::ResourceLink,
                BlockKind::Unsupported,
                BlockKind::Text,
                BlockKind::Text,
            ],
        format!("resolve b {rb:?}"),
    )?;
    let files = api(
        "mentions files",
        gate.hub
            .mention_candidates(MentionQuery {
                session_id: sa.clone(),
                query: "small".into(),
                kinds: vec![MentionKind::File],
                limit: 10,
            })
            .await,
    )?;
    check(
        files.iter().any(|m| {
            matches!(&m.item, ContextItem::File { path } if norm(path).ends_with("ctx/small.txt"))
        }),
        format!("file mention missing: {files:?}"),
    )?;
    let sessions = api(
        "mentions sessions",
        gate.hub
            .mention_candidates(MentionQuery {
                session_id: sa.clone(),
                query: "rewind".into(),
                kinds: vec![MentionKind::Session],
                limit: 10,
            })
            .await,
    )?;
    check(
        sessions
            .iter()
            .any(|m| matches!(&m.item, ContextItem::Session { session_id } if session_id == other)),
        format!("session mention missing: {sessions:?}"),
    )?;
    step("context");
    Ok(())
}
