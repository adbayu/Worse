//! Read-only port of `GitCliRepo` (`packages/services/src/git/repo/gitCliRepo.ts`).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::sync::{Mutex, PoisonError};
use std::thread;

use crate::error::GitError;
use crate::parse::{
    kind_from_numstat, parse_branch_refs, parse_config_value, parse_graph_records, parse_numstat,
    parse_status_porcelain, ConfigValue, LineStat, StatMap, StatusEntry,
};
use crate::runner::{CancelFn, ExecResult, GitRunner, COMMAND_TIMEOUT, OUTPUT_BYTES};
use crate::types::{
    ChangeKind, CommitGraph, HeadRefType, Identity, LocalBranchList, RepositorySummary, WatchPath,
    WorkspaceRepositoryInfo, WorkspaceRepositoryKind,
};
use crate::util::{
    js_trim, node_is_absolute, node_relative, node_resolve, normalize_git_path,
    normalize_workspace_in_repo_path, normalized_lines,
};

mod branches;
mod paths;
mod status;

use paths::{
    build_untracked_stats, build_watch_paths, is_missing_working_directory, is_not_repository,
    normalize_graph_max_count, normalize_graph_skip,
};
pub use paths::{resolve_input_path, to_absolute_path};

/// `GIT_UNTRACKED_STAT_MAX_BYTES`.
const UNTRACKED_STAT_MAX_BYTES: u64 = 1024 * 1024;
/// `GIT_UNTRACKED_STAT_CHUNK_BYTES`.
const UNTRACKED_STAT_CHUNK_BYTES: usize = 64 * 1024;
/// `GIT_UNTRACKED_STAT_CONCURRENCY`.
const UNTRACKED_STAT_CONCURRENCY: usize = 4;
const DEFAULT_GRAPH_MAX_COUNT: u64 = 100;
const MAX_GRAPH_MAX_COUNT_F64: f64 = 200.0;

/// `GitResolvedRepository`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// Workspace path as given.
    pub workspace_path: String,
    /// Repository root (git output; the workspace path when not a repo).
    pub repo_root: String,
    /// Workspace prefix inside the repository.
    pub workspace_in_repo_path: String,
    /// Watch paths.
    pub auto_refresh_watch_paths: Vec<WatchPath>,
    /// Git binary found.
    pub is_git_available: bool,
    /// Inside a repository.
    pub is_repository: bool,
}

impl Resolution {
    fn unavailable(workspace_path: &str, is_git_available: bool) -> Self {
        Self {
            workspace_path: workspace_path.to_owned(),
            repo_root: workspace_path.to_owned(),
            workspace_in_repo_path: ".".to_owned(),
            auto_refresh_watch_paths: Vec::new(),
            is_git_available,
            is_repository: false,
        }
    }

    /// Usable repository.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.is_git_available && self.is_repository
    }

    /// `createEmptySummary`.
    #[must_use]
    pub fn empty_summary(&self) -> RepositorySummary {
        RepositorySummary {
            workspace_path: self.workspace_path.clone(),
            repo_root: self.repo_root.clone(),
            workspace_in_repo_path: self.workspace_in_repo_path.clone(),
            auto_refresh_watch_paths: self.auto_refresh_watch_paths.clone(),
            branch_name: None,
            tracking_branch_name: None,
            head_ref_type: HeadRefType::Branch,
            ahead: 0,
            behind: 0,
            is_dirty: false,
            is_git_available: self.is_git_available,
            is_repository: self.is_repository,
        }
    }
}

/// `GitStatusSnapshot`.
#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    /// Resolution.
    pub resolution: Resolution,
    /// Summary.
    pub summary: RepositorySummary,
    /// Entries.
    pub entries: Vec<StatusEntry>,
    /// `diff --cached --numstat`.
    pub staged_stats: StatMap,
    /// `diff --numstat`.
    pub unstaged_stats: StatMap,
    /// Line counts of untracked files.
    pub untracked_stats: HashMap<String, LineStat>,
}

/// `GitBranchComparisonChange`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchChange {
    /// Path.
    pub path: String,
    /// Rename source.
    pub original_path: Option<String>,
    /// Kind.
    pub kind: ChangeKind,
    /// Added.
    pub added: u64,
    /// Removed.
    pub removed: u64,
}

/// `GitBranchComparisonSnapshot`.
#[derive(Debug, Clone)]
pub struct BranchComparisonSnapshot {
    /// Resolution.
    pub resolution: Resolution,
    /// Upstream.
    pub base_ref: Option<String>,
    /// Head.
    pub head_ref: Option<String>,
    /// Label.
    pub comparison_label: Option<String>,
    /// Changes.
    pub changes: Vec<BranchChange>,
}

/// The repository reader. One per process (holds the binary cache and the
/// collapsed-untracked memory, like the TS closure state).
#[derive(Debug, Default)]
pub struct GitRepo {
    pub(crate) runner: GitRunner,
    collapsed_untracked_roots: Mutex<HashSet<String>>,
}

impl GitRepo {
    /// Repository reader with the default binary discovery.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reader with a fixed binary (`None` simulates a missing git).
    #[must_use]
    pub fn with_binary(binary: Option<String>) -> Self {
        Self {
            runner: GitRunner::with_binary(binary),
            collapsed_untracked_roots: Mutex::new(HashSet::new()),
        }
    }

    pub(crate) fn git(
        &self,
        cwd: &str,
        args: &[&str],
        cancel: CancelFn<'_>,
    ) -> Result<ExecResult, GitError> {
        self.runner
            .run(cwd, args, COMMAND_TIMEOUT, OUTPUT_BYTES, cancel)
    }

    /// `resolveRepository`.
    ///
    /// # Errors
    /// `GitError::App` when `git rev-parse` fails for another reason.
    pub fn resolve_repository(
        &self,
        workspace_path: &str,
        cancel: CancelFn<'_>,
    ) -> Result<Resolution, GitError> {
        if self.runner.binary().is_none() {
            return Ok(Resolution::unavailable(workspace_path, false));
        }
        let result = self.git(
            workspace_path,
            &[
                "rev-parse",
                "--show-toplevel",
                "--show-prefix",
                "--absolute-git-dir",
                "--git-common-dir",
            ],
            cancel,
        )?;
        if result.exit_code != Some(0) {
            if is_missing_working_directory(&result) || is_not_repository(&result) {
                return Ok(Resolution::unavailable(workspace_path, true));
            }
            result.ensure_ok("git rev-parse")?;
        }
        let lines = normalized_lines(&result.stdout);
        let repo_root = js_trim(lines.first().copied().unwrap_or_default());
        if repo_root.is_empty() {
            return Err(GitError::App(
                "Failed to resolve Git repository root".to_owned(),
            ));
        }
        let line = |i: usize| lines.get(i).copied().unwrap_or_default();
        Ok(Resolution {
            workspace_path: workspace_path.to_owned(),
            repo_root: repo_root.to_owned(),
            workspace_in_repo_path: normalize_workspace_in_repo_path(line(1)),
            auto_refresh_watch_paths: build_watch_paths(
                workspace_path,
                js_trim(line(2)),
                js_trim(line(3)),
            ),
            is_git_available: true,
            is_repository: true,
        })
    }

    /// `getWorkspaceRepositoryInfo`.
    ///
    /// # Errors
    /// Propagates [`GitRepo::resolve_repository`] errors.
    pub fn workspace_repository_info(
        &self,
        workspace_path: &str,
        cancel: CancelFn<'_>,
    ) -> Result<WorkspaceRepositoryInfo, GitError> {
        let resolution = self.resolve_repository(workspace_path, cancel)?;
        let info = |kind| WorkspaceRepositoryInfo {
            workspace_path: workspace_path.to_owned(),
            kind,
            is_git_available: resolution.is_git_available,
        };
        if !resolution.is_usable() {
            return Ok(info(WorkspaceRepositoryKind::NotRepository));
        }
        let git_entry = node_resolve(&[&resolution.repo_root, ".git"]);
        if let Ok(meta) = std::fs::metadata(&git_entry) {
            if meta.is_file() {
                if let Ok(content) = std::fs::read(&git_entry) {
                    let content = String::from_utf8_lossy(&content);
                    let first = js_trim(
                        normalized_lines(&content)
                            .first()
                            .copied()
                            .unwrap_or_default(),
                    );
                    if let Some(raw) = first.strip_prefix("gitdir:") {
                        let raw = js_trim(raw);
                        let resolved = if raw.is_empty() {
                            String::new()
                        } else if node_is_absolute(raw) {
                            raw.to_owned()
                        } else {
                            node_resolve(&[&resolution.repo_root, raw])
                        };
                        // linked worktree 的 `.git` 文件指向 `<main>/.git/worktrees/<name>`。
                        if normalize_git_path(&resolved).contains("/.git/worktrees/") {
                            return Ok(info(WorkspaceRepositoryKind::LinkedWorktree));
                        }
                    }
                }
            }
        }
        Ok(info(WorkspaceRepositoryKind::MainTree))
    }

    /// `normalizeInputPath`.
    ///
    /// # Errors
    /// `GitError::App("Path is outside repository scope: <path>")`.
    pub fn normalize_input_path(resolution: &Resolution, path: &str) -> Result<String, GitError> {
        let raw = resolve_input_path(&resolution.workspace_path, path);
        let absolute = harness_fs::realpath(&raw).unwrap_or(raw);
        let relative = normalize_git_path(&node_relative(&resolution.repo_root, &absolute));
        if relative.is_empty() || relative == "." || relative == ".." || relative.starts_with("../")
        {
            return Err(GitError::App(format!(
                "Path is outside repository scope: {path}"
            )));
        }
        Ok(relative)
    }
}

/// Joins a scoped worker, mapping a panic to an internal app error.
pub(crate) fn join<T>(
    handle: thread::ScopedJoinHandle<'_, Result<T, GitError>>,
) -> Result<T, GitError> {
    handle
        .join()
        .unwrap_or_else(|_| Err(GitError::App("git worker panicked".to_owned())))
}
