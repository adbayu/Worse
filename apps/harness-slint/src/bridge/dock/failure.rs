use super::{
    kind_id, sv, ChatCall, ChatUi, Command, ComponentHandle, DockUi, FailureEvent, ResumableRun,
    RetryHint, SharedString, UiState,
};

impl UiState {
    // ---- typed failure (R20) -------------------------------------------------------------------------

    pub(crate) fn failure(&mut self, event: FailureEvent) {
        if let Some(session) = &event.session {
            if !self.is_active(session) {
                return;
            }
        }
        // 启动失败时 port 还会再发一条同文的 Event::Error；类型化横幅已展示它，普通横幅不再重复。
        if self.window.get_error_text().as_str() == event.message {
            self.window.set_error_text(SharedString::new());
        }
        let details = [event.details.as_deref(), event.trace.as_deref()]
            .into_iter()
            .flatten()
            .filter(|d| !d.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let (retry, wait) = match event.retry {
            RetryHint::Retry => ("retry", 0),
            RetryHint::After { secs } => ("after", secs.unwrap_or_default()),
            RetryHint::Login => ("login", 0),
            RetryHint::No => ("no", 0),
        };
        let dock = self.dock();
        dock.set_failure_kind(sv(kind_id(event.kind)));
        dock.set_failure_message(sv(&event.message));
        dock.set_failure_details(sv(&details));
        dock.set_failure_retry(sv(retry));
        dock.set_failure_wait(i32::try_from(wait).unwrap_or(i32::MAX));
        dock.set_failure_expanded(false);
        dock.set_failure_shown(true);
        self.dock_state.failure_copy = if details.is_empty() {
            event.message
        } else {
            format!("{}\n\n{details}", event.message)
        };
        self.dock_state.failure_session = event.session;
        self.dock_state.countdown = (wait > 0).then(|| {
            let timer = slint::Timer::default();
            let weak = self.window.as_weak();
            timer.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_secs(1),
                move || {
                    if let Some(window) = weak.upgrade() {
                        let dock = window.global::<DockUi>();
                        dock.set_failure_wait((dock.get_failure_wait() - 1).max(0));
                    }
                },
            );
            timer
        });
    }

    /// True when `message` is the text of the shown typed failure (the port's duplicate `Event::Error`).
    pub(crate) fn failure_repeats(&self, message: &str) -> bool {
        self.dock().get_failure_shown() && self.dock().get_failure_message().as_str() == message
    }

    pub(crate) fn dismiss_failure(&mut self) {
        self.dock_state.countdown = None;
        self.dock_state.failure_session = None;
        self.dock_state.failure_copy.clear();
        let dock = self.dock();
        dock.set_failure_shown(false);
        dock.set_failure_expanded(false);
    }

    pub(super) fn failure_action(&mut self, action: &str) {
        match action {
            "retry" => {
                let session = self.dock_state.failure_session.clone();
                self.dismiss_failure();
                self.send(Command::RetryInit { session });
            }
            "copy" => {
                let kind = self.dock().get_failure_kind();
                let title = self.dock().invoke_failure_title(kind);
                let text = format!("{title}\n{}", self.dock_state.failure_copy);
                self.window.global::<ChatUi>().invoke_copy(sv(&text));
            }
            "dismiss" => self.dismiss_failure(),
            _ => {}
        }
    }

    // ---- resumable run (docs/specs/harness/resumable-runs.md §6) -------------------------------------

    /// Asks the hub for the open session's interrupted run; the banner appears when there is one.
    pub(super) fn load_resumable(&mut self) {
        let Some(session) = self.active.clone() else {
            self.show_resumable(None);
            return;
        };
        self.chat_call(
            move |reply| ChatCall::ResumableRun { session, reply },
            super::super::UiState::show_resumable,
        );
    }

    pub(super) fn show_resumable(&mut self, run: Option<ResumableRun>) {
        let dock = self.dock();
        if let Some(run) = run.filter(|run| !run.resumed) {
            let plural = if run.finished_steps == 1 { "" } else { "s" };
            dock.set_resume_title(sv(&format!(
                "{} finished step{plural} before the interruption, for: {}",
                run.finished_steps,
                run.user_text.lines().next().unwrap_or_default()
            )));
            dock.set_resume_shown(true);
        } else {
            dock.set_resume_title(SharedString::new());
            dock.set_resume_shown(false);
        }
    }

    pub(super) fn resume_action(&mut self, action: &str) {
        let Some(session) = self.active.clone() else {
            return;
        };
        self.dock().set_resume_shown(false);
        match action {
            "resume" => self.chat_call(
                move |reply| ChatCall::ResumeRun { session, reply },
                |_, ()| {},
            ),
            "discard" => self.chat_call(
                move |reply| ChatCall::DiscardRun { session, reply },
                |_, ()| {},
            ),
            _ => {}
        }
    }
}
