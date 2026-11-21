use super::{
    check, git, step, write, DiffLineKind, GitChangeKind, Path, Workspace, WorkspaceServices, R,
};

pub(super) async fn git_scenarios(ws: &Workspace, root: &Path) -> R<()> {
    let status = ws
        .git_status()
        .await
        .map_err(|e| format!("git_status: {e}"))?;
    check(
        status.is_repository
            && status.git_available
            && status.branch.as_deref() == Some("main")
            && status.staged.is_empty()
            && status.unstaged.is_empty(),
        format!("clean status: {status:?}"),
    )?;
    write(&root.join("README.md"), "hello\nthere\nworld\n")?;
    write(&root.join("new.txt"), "brand new\n")?;
    let status = ws.git_status().await.map_err(|e| e.to_string())?;
    check(
        status
            .unstaged
            .iter()
            .any(|c| c.path == "README.md" && c.kind == GitChangeKind::Modified)
            && status
                .unstaged
                .iter()
                .any(|c| c.path == "new.txt" && c.kind == GitChangeKind::Untracked),
        format!("dirty status: {status:?}"),
    )?;
    step("git_status");

    let diff = ws
        .git_file_diff("README.md".into(), false)
        .await
        .map_err(|e| format!("git_file_diff: {e}"))?;
    let added: Vec<_> = diff
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == DiffLineKind::Added)
        .collect();
    check(
        diff.path == "README.md"
            && diff.added == 1
            && diff.removed == 0
            && added.len() == 1
            && added[0].text == "there"
            && added[0].new_line == Some(2),
        format!("worktree diff: {diff:?}"),
    )?;
    let empty = ws
        .git_file_diff("README.md".into(), true)
        .await
        .map_err(|e| e.to_string())?;
    check(
        empty.hunks.is_empty(),
        format!("staged diff before staging: {empty:?}"),
    )?;
    step("git_file_diff");

    ws.git_stage(vec!["README.md".into(), "new.txt".into()])
        .await
        .map_err(|e| format!("git_stage: {e}"))?;
    let status = ws.git_status().await.map_err(|e| e.to_string())?;
    check(
        status.staged.iter().any(|c| c.path == "README.md")
            && status.staged.iter().any(|c| c.path == "new.txt")
            && status.unstaged.is_empty(),
        format!("after stage: {status:?}"),
    )?;
    let staged = ws
        .git_file_diff("README.md".into(), true)
        .await
        .map_err(|e| e.to_string())?;
    check(staged.added == 1, format!("staged diff: {staged:?}"))?;
    ws.git_unstage(vec!["new.txt".into()])
        .await
        .map_err(|e| format!("git_unstage: {e}"))?;
    let status = ws.git_status().await.map_err(|e| e.to_string())?;
    check(
        status.staged.iter().any(|c| c.path == "README.md")
            && !status.staged.iter().any(|c| c.path == "new.txt")
            && status.unstaged.iter().any(|c| c.path == "new.txt"),
        format!("after unstage: {status:?}"),
    )?;
    check(
        ws.git_stage(vec!["../escape".into()]).await.is_err(),
        "stage outside the root was accepted",
    )?;
    step("git_stage_unstage");
    Ok(())
}

pub(super) async fn git_commit(ws: &Workspace, root: &Path) -> R<()> {
    check(
        ws.git_commit("  ".into()).await.is_err(),
        "empty commit message accepted",
    )?;
    let commit = ws
        .git_commit("gate: second commit".into())
        .await
        .map_err(|e| format!("git_commit: {e}"))?;
    check(
        commit.hash.len() >= 40 && commit.summary == "gate: second commit",
        format!("commit: {commit:?}"),
    )?;
    check(
        git(root, &["rev-parse", "HEAD"])? == commit.hash,
        "commit hash differs from HEAD",
    )?;
    check(
        ws.git_commit("nothing".into()).await.is_err(),
        "commit with nothing staged accepted",
    )?;
    git(root, &["branch", "feature-x"])?;
    let branches = ws
        .git_branches()
        .await
        .map_err(|e| format!("git_branches: {e}"))?;
    check(
        branches.iter().any(|b| b.name == "main" && b.current)
            && branches.iter().any(|b| b.name == "feature-x" && !b.current),
        format!("branches: {branches:?}"),
    )?;
    // Leave the fixture clean for the turn-diff scenario.
    std::fs::remove_file(root.join("new.txt")).map_err(|e| e.to_string())?;
    step("git_commit");
    Ok(())
}
