use super::{
    ask, fail, generation, int, open, set_result, sv, window, ApiError, AppWindow, CommitResult,
    ComponentHandle, GitCall, GitUi, Model, PanelGit, PushProgress, PushResult, SharedString,
};

pub(super) fn commit(w: &AppWindow, push_after: bool) {
    let message = w.global::<PanelGit>().get_message().trim().to_string();
    if message.is_empty() {
        return;
    }
    let ui = w.global::<GitUi>();
    let include_unstaged = ui.get_include_unstaged();
    let changed = commit_paths(w, include_unstaged);
    let gate = crate::security::verdict(&changed);
    if matches!(gate, crate::security::gate::Verdict::Block(_)) {
        let text = gate.notice().unwrap_or_default();
        return fail(w, &ApiError::Conflict(text));
    }
    // `warn` and `skipped` both commit, and both say why in the result line.
    let warned = gate.notice();
    w.global::<PanelGit>().set_notice(SharedString::new());
    ui.set_busy(true);
    set_result(&ui, "", "", "");
    ask(
        |reply| GitCall::Commit {
            message,
            include_unstaged,
            reply,
        },
        move |w, result: Result<CommitResult, ApiError>| {
            let r = match result {
                Ok(r) => r,
                Err(e) => return fail(w, &e),
            };
            w.global::<PanelGit>().set_message(SharedString::new());
            let short: String = r.hash.chars().take(7).collect();
            open(w, if push_after { "push" } else { "" });
            set_result(&w.global::<GitUi>(), "committed", &short, &r.summary);
            if let Some(text) = &warned {
                w.global::<PanelGit>().set_notice(sv(text));
            }
            if push_after {
                push(w);
            }
        },
    );
}

/// Paths the commit covers: the staged rows, plus the unstaged ones when `include_unstaged` is on.
fn commit_paths(w: &AppWindow, include_unstaged: bool) -> Vec<String> {
    let git = w.global::<PanelGit>();
    let mut paths: Vec<String> = git
        .get_staged()
        .iter()
        .map(|row| row.path.to_string())
        .collect();
    if include_unstaged {
        paths.extend(git.get_unstaged().iter().map(|row| row.path.to_string()));
    }
    paths
}

pub(super) fn push(w: &AppWindow) {
    let ui = w.global::<GitUi>();
    if ui.get_pushing() {
        return;
    }
    ui.set_pushing(true);
    let (progress, mut phases) = tokio::sync::mpsc::unbounded_channel();
    let asked = generation();
    let _ = slint::spawn_local(async move {
        while let Some(phase) = phases.recv().await {
            if generation() != asked {
                continue;
            }
            if let (PushProgress::Started { remote, branch }, Some(w)) = (phase, window()) {
                set_result(&w.global::<GitUi>(), "pushing", &branch, &remote);
            }
        }
    });
    ask(
        |reply| GitCall::Push {
            remote: None,
            branch: None,
            progress: Some(progress),
            reply,
        },
        |w, result: Result<PushResult, ApiError>| {
            let r = match result {
                Ok(r) => r,
                Err(e) => return fail(w, &e),
            };
            let ui = w.global::<GitUi>();
            ui.set_pushing(false);
            ui.set_upstream(sv(&format!("{}/{}", r.remote, r.branch)));
            ui.set_ahead(int(r.ahead));
            ui.set_behind(int(r.behind));
            let kind = if r.set_upstream {
                "pushed-upstream"
            } else {
                "pushed"
            };
            set_result(&ui, kind, &r.branch, &r.remote);
        },
    );
}
