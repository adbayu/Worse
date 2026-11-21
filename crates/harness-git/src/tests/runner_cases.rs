use super::*;

#[test]
fn cancellation_kills_command() {
    let tmp = fixture();
    let service = GitService::new();
    let result = service.repository_summary(&s(tmp.path()), &|| true);
    assert_eq!(result.unwrap_err(), GitError::Cancelled);
}

/// Long-running non-git command for the runner's timeout/cancel paths.
fn sleeper() -> (String, Vec<&'static str>) {
    if cfg!(windows) {
        ("ping".to_owned(), vec!["-n", "6", "127.0.0.1"])
    } else {
        ("sleep".to_owned(), vec!["5"])
    }
}

#[test]
fn runner_truncates_times_out_and_cancels_mid_run() {
    use crate::runner::GitRunner;
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir().unwrap();
    let cwd = s(tmp.path());
    let git_runner = GitRunner::new();
    let ok = git_runner
        .run(
            &cwd,
            &["--version"],
            Duration::from_secs(10),
            1024,
            &never_cancelled,
        )
        .unwrap();
    assert_eq!(ok.exit_code, Some(0));
    assert!(ok.stdout.starts_with("git version"));
    let truncated = git_runner
        .run(
            &cwd,
            &["--version"],
            Duration::from_secs(10),
            4,
            &never_cancelled,
        )
        .unwrap();
    assert!(truncated.output_truncated);
    assert_eq!(truncated.exit_code, None);

    let (binary, args) = sleeper();
    let runner = GitRunner::with_binary(Some(binary));
    let started = Instant::now();
    let timed_out = runner
        .run(
            &cwd,
            &args,
            Duration::from_millis(200),
            1024,
            &never_cancelled,
        )
        .unwrap();
    assert!(timed_out.timed_out);
    assert_eq!(timed_out.exit_code, None);
    assert!(started.elapsed() < Duration::from_secs(4));

    let started = Instant::now();
    let cancel = move || started.elapsed() > Duration::from_millis(100);
    let cancelled = runner.run(&cwd, &args, Duration::from_secs(30), 1024, &cancel);
    assert_eq!(cancelled.unwrap_err(), GitError::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn branch_diff_and_refresh_share_resolution() {
    let tmp = fixture();
    let dir = tmp.path();
    git(dir, &["branch", "base"]);
    git(dir, &["branch", "--set-upstream-to=base"]);
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "second"]);
    let service = GitService::new();
    let branch = service
        .diff(
            &DiffQuery {
                workspace_path: s(dir),
                path: "a.txt".to_owned(),
                staged: None,
                source_id: Some("branch".to_owned()),
            },
            &never_cancelled,
        )
        .unwrap();
    assert_eq!(branch.availability, DiffAvailability::Patch);
    assert_eq!(branch.before_content.as_deref(), Some("one\ntwo\n"));
    assert_eq!(branch.after_content.as_deref(), Some("one\nTWO\nthree\n"));

    let refresh = service
        .refresh(&s(dir), true, true, &never_cancelled)
        .unwrap();
    assert_eq!(
        refresh.summary.tracking_branch_name.as_deref(),
        Some("base")
    );
    let identity = refresh.identity.unwrap();
    assert_eq!(identity.user_name.as_deref(), Some("Test User"));
    let comparison = refresh.branch_comparison.unwrap();
    assert!(comparison.changes.iter().any(|c| c.path.ends_with("a.txt")));
    let branches = service.local_branches(&s(dir), &never_cancelled).unwrap();
    assert_eq!(branches.current_branch_name.as_deref(), Some("main"));
    assert_eq!(branches.branches.len(), 2);
}
