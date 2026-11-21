use super::{
    ask, fail, int, open, set_result, sv, window, ApiError, AppWindow, BranchRow, ComponentHandle,
    GitBranches, GitCall, GitUi, ModelRc, Rc, SharedString, SwitchOutcome, VecModel, STATE,
};

pub(super) fn load_branches() {
    if let Some(w) = window() {
        w.global::<GitUi>().set_busy(true);
    }
    ask(
        GitCall::Branches,
        |w, result: Result<GitBranches, ApiError>| {
            let b = match result {
                Ok(b) => b,
                Err(e) => return fail(w, &e),
            };
            let ui = w.global::<GitUi>();
            let rows: Vec<BranchRow> = b
                .branches
                .iter()
                .map(|r| BranchRow {
                    name: sv(&r.name),
                    current: r.current,
                    upstream: sv(r.upstream.as_deref().unwrap_or_default()),
                })
                .collect();
            STATE.with(|s| s.borrow_mut().branches = rows);
            show_branches(w);
            ui.set_current(sv(b.current.as_deref().unwrap_or_default()));
            ui.set_upstream(sv(b.upstream.as_deref().unwrap_or_default()));
            ui.set_ahead(int(b.ahead));
            ui.set_behind(int(b.behind));
            ui.set_busy(false);
        },
    );
}

/// Shows the branches whose name contains the filter (case-insensitive).
pub(super) fn show_branches(w: &AppWindow) {
    let ui = w.global::<GitUi>();
    let filter = ui.get_filter().to_lowercase();
    let rows: Vec<BranchRow> = STATE.with(|s| {
        s.borrow()
            .branches
            .iter()
            .filter(|b| filter.is_empty() || b.name.to_lowercase().contains(&filter))
            .cloned()
            .collect()
    });
    ui.set_branches(ModelRc::from(Rc::new(VecModel::from(rows))));
}

pub(super) fn switch_branch(w: &AppWindow, branch: &str, create: bool) {
    let branch = branch.trim().to_string();
    if branch.is_empty() {
        return;
    }
    // 输入的分支名已存在时按切换处理，不再尝试创建（git 会报 "already exists"）。
    let exists = STATE.with(|s| {
        s.borrow()
            .branches
            .iter()
            .any(|b| b.name.as_str() == branch)
    });

    let ui = w.global::<GitUi>();
    ui.set_busy(true);
    ui.set_blocked(SharedString::new());
    let asked = branch.clone();
    ask(
        |reply| GitCall::Switch {
            branch,
            create: create && !exists,
            reply,
        },
        move |w, result: Result<SwitchOutcome, ApiError>| {
            let ui = w.global::<GitUi>();
            ui.set_busy(false);
            match result {
                Ok(SwitchOutcome::Switched { branch }) => {
                    open(w, "");
                    set_result(&w.global::<GitUi>(), "switched", &branch, "");
                }
                Ok(SwitchOutcome::Blocked { files }) => {
                    ui.set_blocked(sv(&files.join("\n")));
                    ui.set_blocked_branch(sv(&asked));
                }
                Err(e) => fail(w, &e),
            }
        },
    );
}
