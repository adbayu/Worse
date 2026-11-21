//! Diff confirmation dialog for planned agent-config and document writes.

use super::{
    model, sv, with_state, After, AppWindow, ComponentHandle, ConfigUi, DiffLine, DiffUi, DocUi,
    Req, SharedString, State,
};

pub(crate) fn diff_lines(diff: &str) -> Vec<DiffLine> {
    diff.lines()
        .map(|line| {
            let kind =
                if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
                    3
                } else if line.starts_with('+') {
                    1
                } else if line.starts_with('-') {
                    2
                } else {
                    0
                };
            DiffLine {
                kind,
                text: sv(line),
            }
        })
        .collect()
}

impl State {
    // ---- diff confirmation -------------------------------------------------------------------------------

    pub(super) fn show_diff(&self, title: &str, path: &str, diff: &str, no_op: bool) {
        let ui = self.window.global::<DiffUi>();
        ui.set_title(sv(title));
        ui.set_path(sv(path));
        ui.set_lines(model(diff_lines(diff)));
        ui.set_no_op(no_op);
        ui.set_error(SharedString::new());
        ui.set_open(true);
    }

    pub(super) fn plan_failed(&self, error: &str) {
        if self.window.global::<ConfigUi>().get_editing() {
            self.window.global::<ConfigUi>().set_error(sv(error));
        }
        if self.after == After::Doc {
            self.window.global::<DocUi>().set_error(sv(error));
        }
        crate::skills::plan_finished(Some(error));
        self.status(error, true);
    }

    pub(super) fn applied(&mut self, result: Result<(), String>) {
        let diff = self.window.global::<DiffUi>();
        match result {
            Ok(()) => {
                diff.set_open(false);
                diff.set_lines(model(Vec::new()));
                match self.after {
                    After::Config => {
                        self.close_editor();
                        self.reload_config();
                    }
                    After::Doc => self.load_doc(),
                }
                crate::skills::plan_finished(None);
            }
            Err(error) => diff.set_error(sv(error)),
        }
    }
}

pub(super) fn wire(window: &AppWindow) {
    let diff = window.global::<DiffUi>();
    diff.on_confirm(|| with_state(|s| s.send(Req::Confirm)));
    diff.on_cancel(|| {
        with_state(|s| {
            s.send(Req::Cancel);
            let diff = s.window.global::<DiffUi>();
            diff.set_open(false);
            diff.set_lines(model(Vec::new()));
            crate::skills::cancelled();
        });
    });
}
