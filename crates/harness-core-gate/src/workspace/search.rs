use super::{
    check, next_event, step, write, Env, SearchText, Workspace, WorkspaceConfig,
    WorkspaceEventKind, WorkspaceServices, R,
};

pub(super) async fn text_search(env: &Env, ws: &Workspace, config: &WorkspaceConfig) -> R<()> {
    // ---- text search: progress before result, regex, case, cancel -------------------------------------
    let mut events = ws.subscribe();
    let found = ws
        .search_text(SearchText {
            search_id: "s1".into(),
            query: "needle".into(),
            is_regex: false,
            case_sensitive: false,
            max_results: 0,
        })
        .await
        .map_err(|e| format!("search_text: {e}"))?;
    check(
        found.match_count == 2 && found.file_count == 2,
        format!("search_text counts: {found:?}"),
    )?;
    check(
        found
            .matches
            .iter()
            .any(|m| m.path == "src/lib.rs" && m.line == 2 && m.text.contains("NEEDLE")),
        format!("search_text matches: {:?}", found.matches),
    )?;
    let progress = events
        .try_recv()
        .ok_or("no SearchProgress before the result")?;
    check(
        matches!(&progress.kind, WorkspaceEventKind::SearchProgress { search_id, .. } if search_id == "s1"),
        format!("first event was not progress: {progress:?}"),
    )?;
    let sensitive = ws
        .search_text(SearchText {
            search_id: "s2".into(),
            query: r"needle\s+two".into(),
            is_regex: true,
            case_sensitive: true,
            max_results: 0,
        })
        .await
        .map_err(|e| format!("regex search: {e}"))?;
    check(
        sensitive.match_count == 1
            && sensitive.matches.first().map(|m| m.path.as_str()) == Some("docs/guide.md"),
        format!("regex search: {sensitive:?}"),
    )?;
    check(
        ws.search_text(SearchText {
            search_id: "s3".into(),
            query: "(".into(),
            is_regex: true,
            case_sensitive: false,
            max_results: 0,
        })
        .await
        .is_err(),
        "invalid regex was accepted",
    )?;
    search_cancel(env, config).await?;
    check(ws.stats().searches_running == 0, "search left registered")?;
    step("search_text");
    Ok(())
}

/// Cancel a running search (large non-repo tree so the scan is still running when the first batch arrives).
pub(super) async fn search_cancel(env: &Env, config: &WorkspaceConfig) -> R<()> {
    let big = env.home.join("search-big");
    if !big.exists() {
        let line = "match-me lorem ipsum dolor sit amet consectetur\n".repeat(400);
        for i in 0..400 {
            write(&big.join(format!("d{}/f{i}.txt", i % 20)), &line)?;
        }
    }
    let ws = Workspace::open(big, config.clone())
        .await
        .map_err(|e| format!("open big: {e}"))?;
    let status = ws
        .git_status()
        .await
        .map_err(|e| format!("git_status non-repo: {e}"))?;
    check(
        !status.is_repository,
        "search-big unexpectedly inside a repository (GIT_CEILING_DIRECTORIES missing?)",
    )?;
    let mut events = ws.subscribe();
    let runner = ws.clone();
    let task = tokio::spawn(async move {
        runner
            .search_text(SearchText {
                search_id: "cancel-me".into(),
                query: "match-me".into(),
                is_regex: false,
                case_sensitive: false,
                max_results: 1_000_000,
            })
            .await
    });
    next_event(&mut events, "first progress of the big search", |k| {
        matches!(k, WorkspaceEventKind::SearchProgress { search_id, .. } if search_id == "cancel-me")
    })
    .await?;
    ws.cancel_search("cancel-me".into())
        .await
        .map_err(|e| e.to_string())?;
    let outcome = task.await.map_err(|e| e.to_string())?;
    let shutdown = ws.shutdown().await;
    check(
        matches!(&outcome, Err(e) if e.to_string().contains("cancelled")),
        format!(
            "cancelled search returned {:?}",
            outcome.map(|r| r.match_count)
        ),
    )?;
    shutdown.map_err(|e| e.to_string())
}
