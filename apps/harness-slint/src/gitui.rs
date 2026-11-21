//! Git operations of 9.2B round 3 (spec phase-9-parity.md §7.4 "Git"; inventory GIT-007..GIT-011): branch
//! switcher, commit dialog, push dialog and commit graph (`ui/panels/gitops.slint`).
//!
//! Ownership: the panels worker owns the repository and answers each [`GitCall`] on its own oneshot; it refreshes
//! the Git panel (`PanelGit`) itself after Switch / Commit / Push. This module is the only writer of `GitUi`; the
//! graph rows and the lane state live here on the UI thread and are dropped when the dialog closes.
//!
//! ```text
//! GitUi callback ──▶ gitui::ask ──GitCall{…, reply}──▶ panels worker (owns Workspace, git CLI)
//!                        │                                 │ Switch/Commit/Push → refresh PanelGit itself
//!   spawn_local(await) ◀─┴──────────── reply (oneshot) ◀───┘
//!        └─ generation check (dialog reopened? drop the stale page) → write GitUi
//! push progress: worker ──PushProgress (unbounded)──▶ spawn_local loop ──▶ GitUi.result-kind = "pushing"
//! ```

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use harness_core_api::workspace::{
    CommitResult, GitBranches, GitLogPage, GitLogRow, PushProgress, PushResult, SwitchOutcome,
};
use harness_core_api::ApiError;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};

use crate::panels::{git, GitCall, GitReply};
use crate::ui::{AppWindow, BranchRow, ChatUi, GitUi, GraphRow, PanelGit};

mod branches;
mod commits;
mod graph;

use branches::{load_branches, show_branches, switch_branch};
use commits::{commit, push};
use graph::load_graph;

/// Commits per graph page ("Load more" fetches the next one).
const PAGE: u32 = 100;
const LANE_W: u32 = 14;
const ROW_H: u32 = 28;
const MID: u32 = ROW_H / 2;
/// Line ends one pixel beyond the row (the Slint viewbox starts at -1) so rows join without a seam.
const TOP: i32 = -1;
const BOTTOM: u32 = ROW_H + 1;

#[derive(Default)]
struct State {
    window: Option<Weak<AppWindow>>,
    /// Bumped on every open / close; a reply of an older generation is dropped.
    generation: u64,
    rows: Vec<GraphRow>,
    skip: u32,
    /// Lane → hash of the commit the line in that lane runs down to (carried across pages).
    open_lanes: BTreeMap<u32, String>,
    /// Full text of the last error (Copy error).
    error: String,
    /// Every branch of the last `Branches` reply; `GitUi.branches` shows those matching the filter.
    branches: Vec<BranchRow>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn sv(text: &str) -> SharedString {
    SharedString::from(text)
}

fn int(value: u64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn window() -> Option<AppWindow> {
    STATE.with(|s| s.borrow().window.as_ref().and_then(Weak::upgrade))
}

fn generation() -> u64 {
    STATE.with(|s| s.borrow().generation)
}

/// Sends a git call; `done` runs on the UI thread with the reply unless the dialog was reopened or closed since.
fn ask<T: 'static>(
    make: impl FnOnce(GitReply<T>) -> GitCall,
    done: impl FnOnce(&AppWindow, Result<T, ApiError>) + 'static,
) {
    let asked = generation();
    let (reply, answer) = tokio::sync::oneshot::channel();
    git(make(reply));
    let _ = slint::spawn_local(async move {
        let result = answer.await.unwrap_or(Err(ApiError::ShutDown));
        if generation() != asked {
            return;
        }
        if let Some(w) = window() {
            done(&w, result);
        }
    });
}

fn set_result(ui: &GitUi<'_>, kind: &str, a: &str, b: &str) {
    ui.set_result_kind(sv(kind));
    ui.set_result_a(sv(a));
    ui.set_result_b(sv(b));
}

fn fail(w: &AppWindow, error: &ApiError) {
    let text = error.to_string();
    let ui = w.global::<GitUi>();
    ui.set_busy(false);
    ui.set_pushing(false);
    set_result(&ui, "error", &text, "");
    STATE.with(|s| s.borrow_mut().error = text);
}

fn open(w: &AppWindow, which: &str) {
    let ui = w.global::<GitUi>();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.generation += 1;
        // 关闭或切换对话框时释放提交图数据（spec：大数据按需加载、关闭即释放）。
        s.rows = Vec::new();
        s.skip = 0;
        s.open_lanes.clear();
    });
    ui.set_graph(ModelRc::default());
    ui.set_graph_more(false);
    ui.set_graph_selected(-1);
    ui.set_busy(false);
    // 旧一代的回复会被丢弃，进行中的推送状态也随之复位，否则 Push 按钮会一直禁用。
    ui.set_pushing(false);
    ui.set_dialog(sv(which));
    if which.is_empty() {
        ui.set_branches(ModelRc::default());
        STATE.with(|s| s.borrow_mut().branches = Vec::new());
        return;
    }
    set_result(&ui, "", "", "");
    match which {
        "branches" => {
            ui.set_filter(SharedString::new());
            ui.set_blocked(SharedString::new());
            ui.set_blocked_branch(SharedString::new());
            load_branches();
        }
        "push" => load_branches(),
        "commit" => ui.set_include_unstaged(true),
        "graph" => load_graph(),
        _ => {}
    }
}

/// Connects the `GitUi` callbacks (UI thread).
pub fn install(app: &AppWindow) {
    STATE.with(|s| s.borrow_mut().window = Some(app.as_weak()));
    let ui = app.global::<GitUi>();
    ui.on_filter_edited(|_| {
        if let Some(w) = window() {
            show_branches(&w);
        }
    });
    ui.on_open(|which| {
        if let Some(w) = window() {
            open(&w, &which);
        }
    });
    ui.on_switch_branch(|branch, create| {
        if let Some(w) = window() {
            switch_branch(&w, &branch, create);
        }
    });
    ui.on_commit(|push_after| {
        if let Some(w) = window() {
            commit(&w, push_after);
        }
    });
    ui.on_push(|| {
        if let Some(w) = window() {
            push(&w);
        }
    });
    ui.on_load_more(load_graph);
    ui.on_copy_result(|| {
        if let Some(w) = window() {
            let text = STATE.with(|s| s.borrow().error.clone());
            w.global::<ChatUi>().invoke_copy(sv(&text));
        }
    });
    ui.on_copy_hash(|hash| {
        if let Some(w) = window() {
            w.global::<ChatUi>().invoke_copy(hash);
        }
    });
}
