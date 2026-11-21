//! Branch switcher, commit-all, push and the commit graph (phase-9-parity.md §9.2.5 R19). Everything runs the git
//! CLI asynchronously through [`git::git`] (no prompt, `LC_ALL=C`, no console window, time limit).

use harness_core_api::workspace::{
    BranchInfo, CommitResult, GitBranches, GitLogPage, GitLogRow, PushProgress, PushResult,
    SwitchOutcome,
};
use harness_core_api::{ApiError, ApiResult};

use crate::git::{self, failed, git};

/// Largest log page.
pub const LOG_PAGE_MAX: u32 = 500;
/// Deepest log row a page may reach (lanes are computed from the first commit).
pub const LOG_DEPTH_MAX: u32 = 5000;

const FIELD: char = '\u{1f}';
const RECORD: char = '\u{1e}';

/// Ahead / behind of HEAD against its upstream; `(0, 0)` without one.
async fn ahead_behind(root: &str) -> ApiResult<(u64, u64)> {
    let run = git(
        root,
        &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
    )
    .await?;
    if !run.ok {
        return Ok((0, 0));
    }
    let mut parts = run
        .stdout
        .split_whitespace()
        .map(|n| n.parse().unwrap_or(0));
    Ok((parts.next().unwrap_or(0), parts.next().unwrap_or(0)))
}

/// Branch switcher data.
pub(crate) async fn branch_list(root: &str) -> ApiResult<GitBranches> {
    let inside = git(root, &["rev-parse", "--is-inside-work-tree"]).await?;
    if !inside.ok {
        return Err(ApiError::InvalidArgument("not a git repository".into()));
    }
    let head = git(root, &["symbolic-ref", "-q", "--short", "HEAD"]).await?;
    let current = (head.ok && !head.stdout.is_empty()).then(|| head.stdout.clone());
    let refs = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)%1f%(upstream:short)",
            "refs/heads",
        ],
    )
    .await?;
    if !refs.ok {
        return Err(failed("for-each-ref", &refs));
    }
    let branches: Vec<BranchInfo> = refs
        .stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(FIELD);
            let name = fields.next()?.to_owned();
            let upstream = fields.next().filter(|u| !u.is_empty()).map(str::to_owned);
            Some(BranchInfo {
                current: current.as_deref() == Some(name.as_str()),
                name,
                upstream,
            })
        })
        .collect();
    let upstream = branches
        .iter()
        .find(|b| b.current)
        .and_then(|b| b.upstream.clone());
    let (ahead, behind) = if upstream.is_some() {
        ahead_behind(root).await?
    } else {
        (0, 0)
    };
    Ok(GitBranches {
        detached: current.is_none(),
        current,
        upstream,
        ahead,
        behind,
        branches,
    })
}

/// Files of git's "would be overwritten" refusal (tab-indented lines), `None` for other errors.
fn overwritten_files(stderr: &str) -> Option<Vec<String>> {
    if !stderr.contains("would be overwritten") {
        return None;
    }
    Some(
        stderr
            .lines()
            .filter(|l| l.starts_with('\t'))
            .map(|l| l.trim().replace('\\', "/"))
            .collect(),
    )
}

/// `git switch [-c] branch`. A refusal because of local changes is `Blocked` (nothing changed), not an error.
pub(crate) async fn switch(root: &str, branch: &str, create: bool) -> ApiResult<SwitchOutcome> {
    let name = branch.trim();
    if name.is_empty() || name.starts_with('-') {
        return Err(ApiError::InvalidArgument(format!(
            "invalid branch name {branch:?}"
        )));
    }
    if create {
        let valid = git(root, &["check-ref-format", "--branch", name]).await?;
        if !valid.ok {
            return Err(ApiError::InvalidArgument(format!(
                "invalid branch name {name:?}"
            )));
        }
    }
    let args: &[&str] = if create {
        &["switch", "-c", name]
    } else {
        &["switch", name]
    };
    let run = git(root, args).await?;
    if run.ok {
        return Ok(SwitchOutcome::Switched {
            branch: name.to_owned(),
        });
    }
    match overwritten_files(&run.stderr) {
        Some(files) => Ok(SwitchOutcome::Blocked { files }),
        None => Err(failed("switch", &run)),
    }
}

/// Commit; with `include_unstaged` first `git add -A -- .` (the workspace folder only).
pub(crate) async fn commit_with(
    root: &str,
    message: &str,
    include_unstaged: bool,
) -> ApiResult<CommitResult> {
    if message.trim().is_empty() {
        return Err(ApiError::InvalidArgument("commit message is empty".into()));
    }
    if include_unstaged {
        git::stage(root, &[".".to_owned()]).await?;
    }
    git::commit(root, message).await
}

/// `git push`; sets the upstream (`--set-upstream remote branch`) when the branch has none.
pub(crate) async fn push(
    root: &str,
    remote: Option<String>,
    branch: Option<String>,
    progress: &(dyn Fn(PushProgress) + Send + Sync),
) -> ApiResult<PushResult> {
    let list = branch_list(root).await?;
    let Some(branch) = branch.or_else(|| list.current.clone()) else {
        return Err(ApiError::Conflict(
            "HEAD is detached: switch to a branch before pushing".into(),
        ));
    };
    let tracked = list
        .branches
        .iter()
        .find(|b| b.name == branch)
        .and_then(|b| b.upstream.clone());
    let remote = match remote {
        Some(r) => r,
        None => {
            if let Some((r, _)) = tracked.as_deref().and_then(|u| u.split_once('/')) {
                r.to_owned()
            } else {
                let remotes = git(root, &["remote"]).await?;
                let names: Vec<&str> = remotes.stdout.lines().collect();
                if names.contains(&"origin") {
                    "origin".to_owned()
                } else if let Some(first) = names.first() {
                    (*first).to_owned()
                } else {
                    return Err(ApiError::Conflict("the repository has no remote".into()));
                }
            }
        }
    };
    let set_upstream = tracked.is_none();
    progress(PushProgress::Started {
        remote: remote.clone(),
        branch: branch.clone(),
    });
    let args: Vec<&str> = if set_upstream {
        vec!["push", "--porcelain", "--set-upstream", &remote, &branch]
    } else {
        vec!["push", "--porcelain", &remote, &branch]
    };
    let run = git(root, &args).await?;
    if !run.ok {
        let message = if run.stderr.is_empty() {
            run.stdout.clone()
        } else {
            run.stderr.clone()
        };
        progress(PushProgress::Failed {
            message: message.clone(),
        });
        return Err(ApiError::Internal(format!("git push failed: {message}")));
    }
    progress(PushProgress::Finished);
    let (ahead, behind) = ahead_behind(root).await?;
    let summary = run
        .stdout
        .lines()
        .chain(run.stderr.lines())
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    Ok(PushResult {
        remote,
        branch,
        set_upstream,
        ahead,
        behind,
        summary,
    })
}

/// One parsed `git log` record.
struct Commit {
    hash: String,
    parents: Vec<String>,
    author: String,
    time: i64,
    subject: String,
    refs: Vec<String>,
}

fn parse_log(text: &str) -> Vec<Commit> {
    text.split(RECORD)
        .filter_map(|record| {
            let record = record.trim_start_matches(['\n', '\r']);
            if record.is_empty() {
                return None;
            }
            let fields: Vec<&str> = record.split(FIELD).collect();
            let field = |i: usize| fields.get(i).copied().unwrap_or_default();
            let hash = field(0).trim().to_owned();
            if hash.is_empty() {
                return None;
            }
            Some(Commit {
                hash,
                parents: field(1).split_whitespace().map(str::to_owned).collect(),
                author: field(2).to_owned(),
                time: field(3).trim().parse().unwrap_or(0),
                subject: field(4).to_owned(),
                refs: field(5)
                    .split(", ")
                    .map(str::trim)
                    .filter(|r| !r.is_empty())
                    .map(str::to_owned)
                    .collect(),
            })
        })
        .collect()
}

/// Assigns graph columns in topological order: each column waits for one commit hash; a commit takes the column
/// that waits for it (others waiting for it end there), its first parent continues in that column and further
/// parents reuse the column already waiting for them or take a free one.
fn lanes(commits: Vec<Commit>) -> Vec<GitLogRow> {
    let mut columns: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());
    for commit in commits {
        let waiting: Vec<usize> = columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.as_deref() == Some(commit.hash.as_str()))
            .map(|(i, _)| i)
            .collect();
        let lane = match waiting.first() {
            Some(&i) => i,
            None => free_column(&mut columns),
        };
        for &i in &waiting {
            columns[i] = None;
        }
        let mut parent_lanes = Vec::with_capacity(commit.parents.len());
        for (n, parent) in commit.parents.iter().enumerate() {
            let column = if n == 0 {
                lane
            } else if let Some(i) = columns.iter().position(|c| c.as_deref() == Some(parent)) {
                i
            } else {
                free_column(&mut columns)
            };
            columns[column] = Some(parent.clone());
            parent_lanes.push(u32::try_from(column).unwrap_or(u32::MAX));
        }
        while columns.last().is_some_and(Option::is_none) {
            columns.pop();
        }
        let used = columns.len().max(lane + 1);
        rows.push(GitLogRow {
            hash: commit.hash,
            parents: commit.parents,
            author: commit.author,
            time: commit.time,
            subject: commit.subject,
            refs: commit.refs,
            lane: u32::try_from(lane).unwrap_or(u32::MAX),
            parent_lanes,
            lanes: u32::try_from(used).unwrap_or(u32::MAX),
        });
    }
    rows
}

fn free_column(columns: &mut Vec<Option<String>>) -> usize {
    if let Some(i) = columns.iter().position(Option::is_none) {
        i
    } else {
        columns.push(None);
        columns.len() - 1
    }
}

/// One page of the commit graph (all branches, topological order). Lanes are computed from the newest commit so
/// pages line up; `skip + limit` is capped at [`LOG_DEPTH_MAX`].
pub(crate) async fn log(root: &str, skip: u32, limit: u32) -> ApiResult<GitLogPage> {
    let limit = limit.clamp(1, LOG_PAGE_MAX);
    if skip >= LOG_DEPTH_MAX {
        return Ok(GitLogPage::default());
    }
    let end = (skip + limit).min(LOG_DEPTH_MAX);
    let count = format!("--max-count={}", end + 1);
    let run = git(
        root,
        &[
            "log",
            "--all",
            "--topo-order",
            &count,
            "--format=%H%x1f%P%x1f%an%x1f%at%x1f%s%x1f%D%x1e",
        ],
    )
    .await?;
    if !run.ok {
        // 空仓库（还没有提交）时 git log 失败：这是“没有历史”，不是错误。
        let empty = git(root, &["rev-parse", "--verify", "-q", "HEAD"]).await?;
        if !empty.ok {
            return Ok(GitLogPage::default());
        }
        return Err(failed("log", &run));
    }
    let commits = parse_log(&run.stdout);
    let total = commits.len();
    let rows = lanes(commits);
    let has_more = total > end as usize && end < LOG_DEPTH_MAX;
    let rows = rows
        .into_iter()
        .skip(skip as usize)
        .take((end - skip) as usize)
        .collect();
    Ok(GitLogPage { rows, has_more })
}

/// Hash → row index (tests).
#[cfg(test)]
fn index(rows: &[GitLogRow]) -> std::collections::HashMap<&str, usize> {
    rows.iter()
        .enumerate()
        .map(|(i, r)| (r.hash.as_str(), i))
        .collect()
}

#[cfg(test)]
mod tests;
