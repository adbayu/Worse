use super::*;

#[test]
fn status_changes_and_summary() {
    let tmp = fixture();
    let dir = tmp.path();
    let service = GitService::new();
    let summary = service
        .repository_summary(&s(dir), &never_cancelled)
        .unwrap();
    assert!(summary.is_repository && summary.is_git_available && summary.is_dirty);
    assert_eq!(summary.branch_name.as_deref(), Some("main"));
    assert_eq!(summary.workspace_in_repo_path, ".");
    assert!(same_path(&summary.repo_root, dir));
    assert_ne!(summary.auto_refresh_watch_paths.len(), 0);

    let unstaged = service
        .changes(&s(dir), ChangeSource::Unstaged, &never_cancelled)
        .unwrap();
    let rows: Vec<_> = unstaged
        .iter()
        .map(|c| (c.repo_relative_path.as_str(), c.section, c.added, c.removed))
        .collect();
    // empty.txt (+0/-0) and blob.bin (binary, 0 lines) are filtered; debug.log is ignored.
    assert_eq!(
        rows,
        vec![
            ("a.txt", ChangeSection::Unstaged, 2, 1),
            ("pkg/new file ü.md", ChangeSection::Untracked, 2, 0),
        ]
    );
    let a = &unstaged[0];
    assert!(a
        .path
        .ends_with(if cfg!(windows) { "\\a.txt" } else { "/a.txt" }));
    assert_eq!(a.x.as_deref(), Some("."));
    let json = serde_json::to_value(&unstaged[1]).unwrap();
    assert!(
        json.get("x").is_none(),
        "untracked x must be omitted: {json}"
    );
    assert_eq!(json["y"], "?");

    let staged = service
        .changes(&s(dir), ChangeSource::Staged, &never_cancelled)
        .unwrap();
    let rows: Vec<_> = staged
        .iter()
        .map(|c| (c.repo_relative_path.as_str(), c.kind, c.added, c.removed))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("pkg/b.ts", ChangeKind::Modified, 1, 0),
            ("ren new.txt", ChangeKind::Renamed, 0, 0),
        ]
    );
}

#[test]
fn subdirectory_workspace_is_scoped() {
    let tmp = fixture();
    let pkg = tmp.path().join("pkg");
    let service = GitService::new();
    let refresh = service
        .refresh(&s(&pkg), true, true, &never_cancelled)
        .unwrap();
    assert_eq!(refresh.summary.workspace_in_repo_path, "pkg");
    let unstaged: Vec<_> = refresh
        .unstaged_changes
        .iter()
        .map(|c| c.workspace_relative_path.as_str())
        .collect();
    assert_eq!(unstaged, vec!["new file ü.md"]);
    let staged: Vec<_> = refresh
        .staged_changes
        .iter()
        .map(|c| c.workspace_relative_path.as_str())
        .collect();
    assert_eq!(staged, vec!["b.ts"]);
    let identity = refresh.identity.unwrap();
    assert_eq!(identity.user_name.as_deref(), Some("Test User"));
    assert_eq!(identity.scope_label.as_deref(), Some("local"));
    // No upstream: empty comparison with headRef = branch.
    let comparison = refresh.branch_comparison.unwrap();
    assert_eq!(comparison.base_ref, None);
    assert_eq!(comparison.head_ref.as_deref(), Some("main"));
    assert_eq!(comparison.changes.len(), 0);
}

#[test]
fn diffs_for_each_source() {
    let tmp = fixture();
    let dir = tmp.path();
    let service = GitService::new();
    let query = |path: &str, source: Option<&str>| DiffQuery {
        workspace_path: s(dir),
        path: path.to_owned(),
        staged: None,
        source_id: source.map(str::to_owned),
    };
    let unstaged = service
        .diff(&query("a.txt", None), &never_cancelled)
        .unwrap();
    assert_eq!(unstaged.availability, DiffAvailability::Patch);
    assert!(unstaged.patch.as_deref().unwrap().contains("+TWO"));
    assert_eq!(unstaged.before_content.as_deref(), Some("one\ntwo\n"));
    assert_eq!(unstaged.after_content.as_deref(), Some("one\nTWO\nthree\n"));
    assert_eq!(unstaged.summary, None);

    let staged = service
        .diff(&query("pkg/b.ts", Some("staged")), &never_cancelled)
        .unwrap();
    assert_eq!(staged.availability, DiffAvailability::Patch);
    assert_eq!(staged.before_content.as_deref(), Some("export {}\n"));

    let untracked = service
        .diff(&query("pkg/new file ü.md", None), &never_cancelled)
        .unwrap();
    assert_eq!(
        untracked.patch.as_deref(),
        Some("--- /dev/null\n+++ b/pkg/new file ü.md\n@@ -0,0 +1,2 @@\n+hello\n+world\n\\ No newline at end of file\n")
    );
    assert_eq!(untracked.before_content.as_deref(), Some(""));

    let binary = service
        .diff(&query("blob.bin", None), &never_cancelled)
        .unwrap();
    assert_eq!(binary.availability, DiffAvailability::Binary);
    assert_eq!(
        binary.summary.as_deref(),
        Some("Binary diff is not previewable.")
    );

    let missing = service
        .diff(&query("nope.txt", None), &never_cancelled)
        .unwrap();
    assert_eq!(missing.availability, DiffAvailability::Unavailable);
    assert_eq!(
        missing.summary.as_deref(),
        Some("No Git diff is available for this file.")
    );

    let branch = service
        .diff(&query("a.txt", Some("branch")), &never_cancelled)
        .unwrap();
    assert_eq!(
        branch.summary.as_deref(),
        Some("Current branch does not have an upstream branch.")
    );

    let outside = service.diff(&query("../outside.txt", None), &never_cancelled);
    assert!(
        matches!(outside, Err(GitError::App(m)) if m == "Path is outside repository scope: ../outside.txt")
    );
}

#[test]
fn graph_branches_ignored_and_worktree() {
    let tmp = fixture();
    let dir = tmp.path();
    git(dir, &["commit", "-q", "-m", "second"]);
    git(dir, &["tag", "v1"]);
    git(dir, &["branch", "side"]);
    let service = GitService::new();

    let graph = service
        .commit_graph(&s(dir), Some(1.0), None, &never_cancelled)
        .unwrap();
    assert_eq!(graph.commits.len(), 1);
    assert!(graph.has_more);
    assert_eq!(graph.commits[0].subject, "second");
    let kinds: Vec<_> = graph.commits[0]
        .refs
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(kinds, vec!["HEAD", "main", "v1", "side"]);
    let page2 = service
        .commit_graph(&s(dir), Some(5.0), Some(1.0), &never_cancelled)
        .unwrap();
    assert_eq!(page2.commits.len(), 1);
    assert!(!page2.has_more);

    let branches = service.local_branches(&s(dir), &never_cancelled).unwrap();
    let names: Vec<_> = branches
        .branches
        .iter()
        .map(|b| (b.name.as_str(), b.is_current))
        .collect();
    assert_eq!(names, vec![("main", true), ("side", false)]);
    assert!(branches.branches[0].commit_timestamp_ms.is_some());

    let ignored = service
        .ignored_paths(
            &s(dir),
            &[
                "debug.log".to_owned(),
                "a.txt".to_owned(),
                "../x".to_owned(),
            ],
            &never_cancelled,
        )
        .unwrap();
    assert_eq!(ignored.len(), 1);
    assert!(ignored[0].ends_with("debug.log"));

    let info = service
        .workspace_repository_info(&s(dir), &never_cancelled)
        .unwrap();
    assert_eq!(info.kind, WorkspaceRepositoryKind::MainTree);
    let wt = tmp.path().join("wt");
    git(dir, &["worktree", "add", "-q", &s(&wt), "side"]);
    let info = service
        .workspace_repository_info(&s(&wt), &never_cancelled)
        .unwrap();
    assert_eq!(info.kind, WorkspaceRepositoryKind::LinkedWorktree);
}

#[test]
fn non_repository_and_missing_git() {
    let tmp = tempfile::tempdir().unwrap();
    let service = GitService::new();
    let summary = service
        .repository_summary(&s(tmp.path()), &never_cancelled)
        .unwrap();
    assert!(summary.is_git_available);
    assert!(!summary.is_repository);
    assert_eq!(summary.repo_root, s(tmp.path()));
    assert_eq!(summary.workspace_in_repo_path, ".");

    let missing = tmp.path().join("does-not-exist");
    let summary = service
        .repository_summary(&s(&missing), &never_cancelled)
        .unwrap();
    assert!(!summary.is_repository);

    let graph = service
        .commit_graph(&s(tmp.path()), None, None, &never_cancelled)
        .unwrap();
    assert_eq!(graph.commits.len(), 0);

    let no_git = GitService::with_binary(None);
    let summary = no_git
        .repository_summary(&s(tmp.path()), &never_cancelled)
        .unwrap();
    assert!(!summary.is_git_available);
    let diff = no_git
        .diff(
            &DiffQuery {
                workspace_path: s(tmp.path()),
                path: "x".to_owned(),
                ..DiffQuery::default()
            },
            &never_cancelled,
        )
        .unwrap();
    assert_eq!(
        diff.summary.as_deref(),
        Some("Git binary is not available in the current environment.")
    );
    let json =
        serde_json::to_value(no_git.identity(&s(tmp.path()), &never_cancelled).unwrap()).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"userName":null,"userEmail":null,"nameSource":null,"emailSource":null,"scopeLabel":null})
    );
}

#[test]
fn empty_repository_has_empty_graph() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let service = GitService::new();
    let graph = service
        .commit_graph(&s(tmp.path()), None, None, &never_cancelled)
        .unwrap();
    assert_eq!(graph.commits.len(), 0);
    let branches = service
        .local_branches(&s(tmp.path()), &never_cancelled)
        .unwrap();
    assert_eq!(branches.branches.len(), 0);
    assert_eq!(branches.current_branch_name.as_deref(), Some("main"));
}
