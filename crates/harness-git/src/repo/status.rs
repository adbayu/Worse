use super::{
    build_untracked_stats, join, parse_numstat, parse_status_porcelain, thread, CancelFn,
    ExecResult, GitError, GitRepo, HashMap, PoisonError, RepositorySummary, Resolution, StatMap,
    StatusSnapshot,
};

impl GitRepo {
    pub(super) fn run_status(
        &self,
        resolution: &Resolution,
        cancel: CancelFn<'_>,
    ) -> Result<ExecResult, GitError> {
        let collapsed = self
            .collapsed_untracked_roots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&resolution.repo_root);
        let run = |mode: &str| {
            let flag = format!("--untracked-files={mode}");
            self.git(
                &resolution.repo_root,
                &["status", "--porcelain=v2", "--branch", &flag, "-z"],
                cancel,
            )
        };
        let result = run(if collapsed { "normal" } else { "all" })?;
        if collapsed || !result.output_truncated {
            return Ok(result);
        }
        // 大仓库逐文件未跟踪状态超过输出上限：按 repoRoot 记住折叠模式（与 TS 一致）。
        self.collapsed_untracked_roots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(resolution.repo_root.clone());
        tracing::warn!(
            repo_root = %resolution.repo_root,
            "git status detailed output exceeded limit; collapsing untracked directories"
        );
        run("normal")
    }

    /// `getStatus` (status and both numstats run concurrently).
    ///
    /// # Errors
    /// `GitError::App` with the TS `ensureGitCommandSucceeded` messages.
    pub fn status(
        &self,
        workspace_path: &str,
        cancel: CancelFn<'_>,
    ) -> Result<StatusSnapshot, GitError> {
        let resolution = self.resolve_repository(workspace_path, cancel)?;
        self.status_for(resolution, cancel)
    }

    /// `getStatus` on an already resolved repository (lets one op share a
    /// single `rev-parse`, like the TS in-flight resolution reuse).
    ///
    /// # Errors
    /// See [`GitRepo::status`].
    pub fn status_for(
        &self,
        resolution: Resolution,
        cancel: CancelFn<'_>,
    ) -> Result<StatusSnapshot, GitError> {
        if !resolution.is_usable() {
            return Ok(StatusSnapshot {
                summary: resolution.empty_summary(),
                resolution,
                entries: Vec::new(),
                staged_stats: StatMap::default(),
                unstaged_stats: StatMap::default(),
                untracked_stats: HashMap::new(),
            });
        }
        let root = resolution.repo_root.as_str();
        let (status, staged, unstaged) = thread::scope(|scope| {
            let staged = scope.spawn(|| {
                self.git(
                    root,
                    &[
                        "diff",
                        "--cached",
                        "--numstat",
                        "-z",
                        "--find-renames",
                        "--",
                    ],
                    cancel,
                )
            });
            let unstaged = scope.spawn(|| {
                self.git(
                    root,
                    &["diff", "--numstat", "-z", "--find-renames", "--"],
                    cancel,
                )
            });
            let status = self.run_status(&resolution, cancel);
            (status, join(staged), join(unstaged))
        });
        let (status, staged, unstaged) = (status?, staged?, unstaged?);
        status.ensure_ok("git status")?;
        staged.ensure_ok("git diff --cached --numstat")?;
        unstaged.ensure_ok("git diff --numstat")?;
        let parsed = parse_status_porcelain(&status.stdout);
        let untracked_stats = build_untracked_stats(root, &parsed.entries);
        let summary = RepositorySummary {
            workspace_path: resolution.workspace_path.clone(),
            repo_root: resolution.repo_root.clone(),
            workspace_in_repo_path: resolution.workspace_in_repo_path.clone(),
            auto_refresh_watch_paths: resolution.auto_refresh_watch_paths.clone(),
            branch_name: parsed.branch_name,
            tracking_branch_name: parsed.tracking_branch_name,
            head_ref_type: parsed.head_ref_type,
            ahead: parsed.ahead,
            behind: parsed.behind,
            is_dirty: !parsed.entries.is_empty(),
            is_git_available: true,
            is_repository: true,
        };
        Ok(StatusSnapshot {
            resolution,
            summary,
            entries: parsed.entries,
            staged_stats: parse_numstat(&staged.stdout),
            unstaged_stats: parse_numstat(&unstaged.stdout),
            untracked_stats,
        })
    }
}
