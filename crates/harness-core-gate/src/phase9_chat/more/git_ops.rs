use super::*;

pub(super) fn git(cwd: &Path, args: &[&str]) -> R<String> {
    crate::workspace::git(cwd, args)
}

#[allow(clippy::too_many_lines)]
pub(super) async fn git_ops(chat: &Chat) -> R<()> {
    let parent = chat.repo.parent().ok_or("repo has no parent")?;
    let root = parent.join("repo92-git");
    let remote = parent.join("repo92-remote.git");
    for dir in [&root, &remote] {
        if dir.exists() {
            io("rm", std::fs::remove_dir_all(dir))?;
        }
        io("mkdir", std::fs::create_dir_all(dir))?;
    }
    git(&remote, &["init", "-q", "--bare"])?;
    git(&root, &["init", "-q", "-b", "main"])?;
    for (key, value) in [
        ("user.name", "Harness Gate"),
        ("user.email", "gate@example.invalid"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
        ("core.hooksPath", "no-hooks"),
    ] {
        git(&root, &["config", key, value])?;
    }
    write(&root.join("shared.txt"), "base\n")?;
    git(&root, &["add", "-A"])?;
    git(&root, &["commit", "-q", "-m", "base"])?;
    let remote_path = remote.to_string_lossy().replace('\\', "/");
    git(&root, &["remote", "add", "origin", &remote_path])?;

    let ws = api(
        "open workspace",
        harness_workspace::Workspace::open(
            root.clone(),
            harness_workspace::WorkspaceConfig::default(),
        )
        .await,
    )?;
    let list = api("branch list", ws.git_branch_list().await)?;
    check(
        list.current.as_deref() == Some("main") && !list.detached && list.upstream.is_none(),
        format!("initial branches {list:?}"),
    )?;
    let created = api("switch -c", ws.git_switch("other".into(), true).await)?;
    check(
        created
            == SwitchOutcome::Switched {
                branch: "other".into(),
            },
        format!("create branch {created:?}"),
    )?;
    write(&root.join("shared.txt"), "other\n")?;
    let committed = api(
        "commit_with (other)",
        ws.git_commit_with("change shared".into(), true).await,
    )?;
    check(!committed.hash.is_empty(), "no commit hash")?;
    api("switch main", ws.git_switch("main".into(), false).await)?;
    write(&root.join("shared.txt"), "local\n")?;
    let blocked = api("switch blocked", ws.git_switch("other".into(), false).await)?;
    check(
        blocked
            == SwitchOutcome::Blocked {
                files: vec!["shared.txt".into()],
            }
            && std::fs::read_to_string(root.join("shared.txt"))
                .ok()
                .as_deref()
                == Some("local\n")
            && api("branch list", ws.git_branch_list().await)?
                .current
                .as_deref()
                == Some("main"),
        format!("blocked switch {blocked:?}"),
    )?;
    git(&root, &["checkout", "--", "shared.txt"])?;
    let nothing = ws.git_commit_with("nothing".into(), false).await;
    check(
        matches!(nothing, Err(ApiError::InvalidArgument(_))),
        format!("commit with nothing staged: {nothing:?}"),
    )?;
    write(&root.join("new.txt"), "new\n")?;
    api(
        "commit_with include_unstaged",
        ws.git_commit_with("add new".into(), true).await,
    )?;
    let tracked = git(&root, &["ls-files", "new.txt"])?;
    check(tracked.trim() == "new.txt", "untracked file not committed")?;
    git(
        &root,
        &["merge", "-q", "--no-ff", "other", "-m", "merge other"],
    )?;

    let progress = std::sync::Mutex::new(Vec::new());
    let report = |p: PushProgress| {
        if let Ok(mut all) = progress.lock() {
            all.push(p);
        }
    };
    let pushed = api("push", ws.git_push(None, None, &report).await)?;
    let phases = progress.lock().map(|p| p.clone()).unwrap_or_default();
    check(
        pushed.remote == "origin"
            && pushed.branch == "main"
            && pushed.set_upstream
            && pushed.ahead == 0
            && pushed.behind == 0
            && matches!(phases.first(), Some(PushProgress::Started { .. }))
            && phases.last() == Some(&PushProgress::Finished),
        format!("push {pushed:?} {phases:?}"),
    )?;
    let list = api("branch list (pushed)", ws.git_branch_list().await)?;
    check(
        list.upstream.as_deref() == Some("origin/main") && list.ahead == 0,
        format!("upstream after push {list:?}"),
    )?;
    write(&root.join("ahead.txt"), "a\n")?;
    api(
        "commit ahead",
        ws.git_commit_with("ahead".into(), true).await,
    )?;
    let list = api("branch list (ahead)", ws.git_branch_list().await)?;
    check(list.ahead == 1, format!("ahead after commit {list:?}"))?;
    let again = api("push again", ws.git_push(None, None, &|_| {}).await)?;
    check(
        !again.set_upstream && again.ahead == 0,
        format!("second push {again:?}"),
    )?;

    let full = api("log", ws.git_log(0, 50).await)?;
    let merge = full
        .rows
        .iter()
        .find(|r| r.subject == "merge other")
        .ok_or("merge commit missing from log")?;
    check(
        merge.parents.len() == 2
            && merge.parent_lanes.len() == 2
            && merge.parent_lanes[0] != merge.parent_lanes[1]
            && full.rows.iter().any(|r| r.lanes >= 2)
            && full
                .rows
                .first()
                .is_some_and(|r| r.refs.iter().any(|x| x.contains("main")))
            && !full.has_more,
        format!("graph rows {:?}", full.rows),
    )?;
    let first = api("log page 1", ws.git_log(0, 2).await)?;
    let second = api("log page 2", ws.git_log(2, 2).await)?;
    check(
        first.has_more
            && first.rows.len() == 2
            && second.rows.first().map(|r| &r.hash) == full.rows.get(2).map(|r| &r.hash)
            && second.rows.first().map(|r| r.lane) == full.rows.get(2).map(|r| r.lane),
        "log pages do not line up",
    )?;
    let _ = ws.shutdown().await;
    step("git_ops");
    Ok(())
}
