use super::{
    join, kind_from_numstat, normalize_git_path, normalize_graph_max_count, normalize_graph_skip,
    parse_branch_refs, parse_config_value, parse_graph_records, parse_numstat, resolve_input_path,
    thread, BranchChange, BranchComparisonSnapshot, CancelFn, CommitGraph, ConfigValue, GitError,
    GitRepo, HashSet, HeadRefType, Identity, LocalBranchList, Resolution, StatusSnapshot,
};

impl GitRepo {
    /// `getIgnoredPaths`.
    ///
    /// # Errors
    /// `GitError::App` when `git check-ignore` fails (exit other than 0/1).
    pub fn ignored_paths(
        &self,
        workspace_path: &str,
        paths: &[String],
        cancel: CancelFn<'_>,
    ) -> Result<Vec<String>, GitError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let resolution = self.resolve_repository(workspace_path, cancel)?;
        if !resolution.is_usable() {
            return Ok(Vec::new());
        }
        let pairs: Vec<(String, String)> = paths
            .iter()
            .filter_map(|path| {
                let relative = Self::normalize_input_path(&resolution, path).ok()?;
                Some((
                    resolve_input_path(&resolution.workspace_path, path),
                    relative,
                ))
            })
            .collect();
        if pairs.is_empty() {
            return Ok(Vec::new());
        }
        let mut args: Vec<&str> = vec!["check-ignore", "--"];
        args.extend(pairs.iter().map(|(_, relative)| relative.as_str()));
        let result = self.git(&resolution.repo_root, &args, cancel)?;
        if result.exit_code == Some(1) {
            return Ok(Vec::new());
        }
        result.ensure_ok("git check-ignore")?;
        let ignored: HashSet<String> = result
            .stdout
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .filter(|line| !line.is_empty())
            .map(normalize_git_path)
            .collect();
        Ok(pairs
            .into_iter()
            .filter(|(_, relative)| ignored.contains(relative))
            .map(|(absolute, _)| absolute)
            .collect())
    }

    /// `listLocalBranches`.
    ///
    /// # Errors
    /// Status or `git for-each-ref` failures.
    pub fn local_branches(
        &self,
        workspace_path: &str,
        cancel: CancelFn<'_>,
    ) -> Result<LocalBranchList, GitError> {
        let resolution = self.resolve_repository(workspace_path, cancel)?;
        if !resolution.is_usable() {
            let status = self.status_for(resolution, cancel)?;
            return Ok(LocalBranchList {
                head_ref_type: status.summary.head_ref_type,
                current_branch_name: status.summary.branch_name,
                branches: Vec::new(),
            });
        }
        // 性能：`for-each-ref` 只依赖 repoRoot，与 status 的三条命令互不依赖，并发执行；
        // 错误优先级保持 TS 顺序（先 status，再 for-each-ref），输出不变。
        let root = resolution.repo_root.clone();
        let (status, result) = thread::scope(|scope| {
            let refs = scope.spawn(|| {
                self.git(
                    &root,
                    &[
                        "for-each-ref",
                        "refs/heads",
                        "--format=%(refname:short)%00%(upstream:short)%00%(objectname)%00%(committerdate:unix)",
                    ],
                    cancel,
                )
            });
            let status = self.status_for(resolution, cancel);
            (status, join(refs))
        });
        let status = status?;
        let head_ref_type = status.summary.head_ref_type;
        let current_branch_name = status.summary.branch_name.clone();
        let result = result?;
        result.ensure_ok("git for-each-ref refs/heads")?;
        let current = if head_ref_type == HeadRefType::Branch {
            current_branch_name.as_deref()
        } else {
            None
        };
        let branches = parse_branch_refs(&result.stdout, current);
        Ok(LocalBranchList {
            head_ref_type,
            current_branch_name,
            branches,
        })
    }

    /// `getCommitGraph`.
    ///
    /// # Errors
    /// `git log` failures other than "no commits yet".
    pub fn commit_graph(
        &self,
        workspace_path: &str,
        max_count: Option<f64>,
        skip: Option<f64>,
        cancel: CancelFn<'_>,
    ) -> Result<CommitGraph, GitError> {
        let empty = CommitGraph {
            commits: Vec::new(),
            has_more: false,
        };
        let resolution = self.resolve_repository(workspace_path, cancel)?;
        if !resolution.is_usable() {
            return Ok(empty);
        }
        let max_count = normalize_graph_max_count(max_count);
        let skip_arg = format!("--skip={}", normalize_graph_skip(skip));
        let max_arg = format!("--max-count={}", max_count + 1);
        let result = self.git(
            &resolution.repo_root,
            &[
                "log",
                "HEAD",
                "--branches",
                "--tags",
                "--remotes",
                "--date-order",
                "--topo-order",
                &skip_arg,
                &max_arg,
                "--format=%H%x00%P%x00%an%x00%at%x00%s%x00%D%x1e",
            ],
            cancel,
        )?;
        if result.exit_code != Some(0) {
            let stderr = result.stderr.to_lowercase();
            if stderr.contains("does not have any commits yet")
                || stderr.contains("your current branch")
                || stderr.contains("bad default revision")
                || stderr.contains("ambiguous argument 'head'")
            {
                return Ok(empty);
            }
            result.ensure_ok("git log visible refs")?;
        }
        let mut commits = parse_graph_records(&result.stdout);
        let limit = usize::try_from(max_count).unwrap_or(usize::MAX);
        let has_more = commits.len() > limit;
        commits.truncate(limit);
        Ok(CommitGraph { commits, has_more })
    }

    /// `getBranchComparison` on an existing status snapshot (the TS in-flight
    /// reuse makes `refresh` share one snapshot; callers pass it explicitly).
    ///
    /// # Errors
    /// `git diff --numstat upstream...HEAD` failures.
    pub fn branch_comparison(
        &self,
        status: &StatusSnapshot,
        cancel: CancelFn<'_>,
    ) -> Result<BranchComparisonSnapshot, GitError> {
        let tracking = status.summary.tracking_branch_name.clone();
        let head_ref = Some(
            status
                .summary
                .branch_name
                .clone()
                .unwrap_or_else(|| "HEAD".to_owned()),
        );
        let Some(tracking_name) = tracking.clone().filter(|_| status.resolution.is_usable()) else {
            return Ok(BranchComparisonSnapshot {
                resolution: status.resolution.clone(),
                base_ref: tracking,
                head_ref,
                comparison_label: None,
                changes: Vec::new(),
            });
        };
        let range = format!("{tracking_name}...HEAD");
        let result = self.git(
            &status.resolution.repo_root,
            &["diff", "--numstat", "-z", "--find-renames", &range, "--"],
            cancel,
        )?;
        result.ensure_ok("git diff --numstat upstream...HEAD")?;
        let changes = parse_numstat(&result.stdout)
            .iter()
            .map(|(path, stat)| BranchChange {
                path: path.clone(),
                original_path: stat.original_path.clone(),
                kind: kind_from_numstat(stat),
                added: stat.added,
                removed: stat.removed,
            })
            .collect();
        let label = match &status.summary.branch_name {
            Some(branch) => format!("{branch} -> {tracking_name}"),
            None => format!("HEAD -> {tracking_name}"),
        };
        Ok(BranchComparisonSnapshot {
            resolution: status.resolution.clone(),
            base_ref: Some(tracking_name),
            head_ref,
            comparison_label: Some(label),
            changes,
        })
    }

    pub(super) fn config_value(
        &self,
        root: &str,
        key: &str,
        cancel: CancelFn<'_>,
    ) -> Result<ConfigValue, GitError> {
        let result = self.git(
            root,
            &["config", "--show-scope", "--show-origin", "--get", key],
            cancel,
        )?;
        if result.exit_code == Some(1) {
            return Ok(ConfigValue::default());
        }
        result.ensure_ok("git config")?;
        Ok(parse_config_value(&result.stdout))
    }

    /// `getIdentity`.
    ///
    /// # Errors
    /// `git config` failures other than "unset" (exit 1).
    pub fn identity(
        &self,
        workspace_path: &str,
        cancel: CancelFn<'_>,
    ) -> Result<Identity, GitError> {
        let resolution = self.resolve_repository(workspace_path, cancel)?;
        self.identity_for(&resolution, cancel)
    }

    /// `getIdentity` on an already resolved repository.
    ///
    /// # Errors
    /// See [`GitRepo::identity`].
    pub fn identity_for(
        &self,
        resolution: &Resolution,
        cancel: CancelFn<'_>,
    ) -> Result<Identity, GitError> {
        if !resolution.is_usable() {
            return Ok(Identity::default());
        }
        let root = resolution.repo_root.as_str();
        let (name, email) = thread::scope(|scope| {
            let email = scope.spawn(|| self.config_value(root, "user.email", cancel));
            let name = self.config_value(root, "user.name", cancel);
            (name, join(email))
        });
        let (name, email) = (name?, email?);
        Ok(Identity {
            user_name: name.value,
            user_email: email.value,
            name_source: name.source,
            email_source: email.source,
            scope_label: name.scope.or(email.scope),
        })
    }
}
