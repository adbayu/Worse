use super::{
    error_text, git_file, view, DiffSource, Done, GitStatus, GitView, Worker, Workspace,
    WorkspaceServices,
};

impl Worker {
    // ---- git --------------------------------------------------------------------------------------------------

    pub(super) fn git_refresh(&mut self, show_loading: bool) {
        let Some(ws) = self.ws.clone() else {
            return;
        };
        self.git_seq = self.next_seq();
        let seq = self.git_seq;
        if show_loading {
            view::git_loading(&self.window, true);
        }
        let done = self.done_tx.clone();
        tokio::spawn(async move {
            let _ = done.send(Done::Git(seq, ws.git_status().await));
        });
    }

    pub(super) fn git_action(&mut self, ws: &Workspace, paths: Vec<String>, stage: bool) {
        if paths.is_empty() {
            return;
        }
        let ws = ws.clone();
        let done = self.done_tx.clone();
        view::git_loading(&self.window, true);
        tokio::spawn(async move {
            let count = paths.len();
            let result = if stage {
                ws.git_stage(paths).await
            } else {
                ws.git_unstage(paths).await
            };
            let verb = if stage { "Staged" } else { "Unstaged" };
            let result = result
                .map(|()| format!("{verb} {count} file(s)"))
                .map_err(|e| error_text(&e));
            let _ = done.send(Done::Action {
                result,
                clear_message: false,
            });
        });
    }

    pub(super) fn git_commit(&mut self, ws: &Workspace, message: String) {
        let ws = ws.clone();
        let done = self.done_tx.clone();
        view::git_loading(&self.window, true);
        tokio::spawn(async move {
            let result = ws
                .git_commit(message)
                .await
                .map(|c| {
                    format!(
                        "Committed {} {}",
                        c.hash.chars().take(7).collect::<String>(),
                        c.summary
                    )
                })
                .map_err(|e| error_text(&e));
            let clear_message = result.is_ok();
            let _ = done.send(Done::Action {
                result,
                clear_message,
            });
        });
    }

    /// R19 calls: each runs in its own task and answers on its channel; changes refresh the Git panel.
    pub(super) fn git_call(&mut self, ws: &Workspace, call: super::super::GitCall) {
        use super::super::GitCall;
        let ws = ws.clone();
        let done = self.done_tx.clone();
        tokio::spawn(async move {
            let changed = match call {
                GitCall::Branches(reply) => {
                    let _ = reply.send(ws.git_branch_list().await);
                    false
                }
                GitCall::Log { skip, limit, reply } => {
                    let _ = reply.send(ws.git_log(skip, limit).await);
                    false
                }
                GitCall::Switch {
                    branch,
                    create,
                    reply,
                } => {
                    let _ = reply.send(ws.git_switch(branch, create).await);
                    true
                }
                GitCall::Commit {
                    message,
                    include_unstaged,
                    reply,
                } => {
                    let _ = reply.send(ws.git_commit_with(message, include_unstaged).await);
                    true
                }
                GitCall::Push {
                    remote,
                    branch,
                    progress,
                    reply,
                } => {
                    let report = move |p| {
                        if let Some(tx) = &progress {
                            let _ = tx.send(p);
                        }
                    };
                    let _ = reply.send(ws.git_push(remote, branch, &report).await);
                    true
                }
            };
            if changed {
                let _ = done.send(Done::Changed);
            }
        });
    }

    pub(super) fn apply_git(&mut self, status: GitStatus) {
        let sync = [(status.ahead, '↑'), (status.behind, '↓')]
            .iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, arrow)| format!("{arrow}{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let branch = match (&status.branch, status.detached) {
            (Some(branch), false) => branch.clone(),
            (Some(branch), true) => format!("({branch})"),
            (None, _) => String::new(),
        };
        let view = GitView {
            is_repo: status.is_repository,
            branch,
            sync,
            staged: status.staged.iter().map(git_file).collect(),
            unstaged: status.unstaged.iter().map(git_file).collect(),
            branches: Vec::new(),
        };
        self.git = Some(status);
        view::git_status(&self.window, view);
    }

    /// A shown working-tree diff follows the files (watcher / git action).
    pub(super) fn refresh_git_diff(&mut self) {
        if let Some(source @ DiffSource::Git { .. }) = self.diff_source.clone() {
            self.load_diff(source);
        }
    }
}
