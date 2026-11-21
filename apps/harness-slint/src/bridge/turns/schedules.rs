use super::{
    now_ms, schedule_row, ChatCall, Command, DialogTarget, ModelRc, NewSchedule, ScheduleKind,
    ScheduleRow, ScheduleTarget, ScheduledPrompt, UiState, UpdateSchedule, VecModel,
};

impl UiState {
    // ---- scheduled prompts (R18) -----------------------------------------------------------------------

    pub(in crate::bridge) fn load_schedules(&self) {
        self.chat_call(ChatCall::SchedulesEnabled, |s, on| {
            s.ui().set_schedules_enabled(on);
        });
        self.chat_call(ChatCall::ListSchedules, |s, list: Vec<ScheduledPrompt>| {
            let now = now_ms();
            let rows: Vec<ScheduleRow> = list.iter().map(|p| schedule_row(p, now)).collect();
            s.turn.schedules = list;
            s.ui().set_schedules(ModelRc::new(VecModel::from(rows)));
        });
    }

    pub(super) fn create_schedule(&self, prompt: &str, minutes: &str) {
        let minutes: u32 = minutes.trim().parse().unwrap_or(60).max(1);
        let title: String = prompt
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .chars()
            .take(40)
            .collect();
        let target = match self.active.clone() {
            Some(session) => ScheduleTarget::Session {
                session_id: harness_core_api::SessionId::new(session),
            },
            None => ScheduleTarget::NewSession {
                workspace_path: self.workspace.to_string_lossy().into_owned(),
                agent: (!self.sticky.is_empty())
                    .then(|| harness_core_api::AgentId::new(self.sticky.as_str())),
            },
        };
        let schedule = NewSchedule {
            title,
            prompt: prompt.trim().to_string(),
            target,
            kind: ScheduleKind::Interval {
                seconds: minutes.saturating_mul(60),
            },
            enabled: true,
        };
        self.chat_call(
            |reply| ChatCall::CreateSchedule(schedule, reply),
            |s, _| s.load_schedules(),
        );
    }

    pub(in crate::bridge) fn schedule_action(&mut self, action: &str, id: &str) {
        let id = id.to_string();
        match action {
            "open" => {
                self.ui().set_schedules_open(true);
                self.load_schedules();
            }
            "enable" | "disable" => {
                let on = action == "enable";
                self.chat_call(
                    |reply| ChatCall::SetSchedulesEnabled(on, reply),
                    |s, ()| s.load_schedules(),
                );
            }
            "new" => self.open_dialog(DialogTarget::NewSchedule, "", "60", true),
            "toggle" => {
                let enabled = self
                    .turn
                    .schedules
                    .iter()
                    .find(|p| p.id == id)
                    .is_some_and(|p| !p.enabled);
                let update = UpdateSchedule {
                    id,
                    title: None,
                    prompt: None,
                    target: None,
                    kind: None,
                    enabled: Some(enabled),
                };
                self.chat_call(
                    |reply| ChatCall::UpdateSchedule(update, reply),
                    |s, _| s.load_schedules(),
                );
            }
            "run" => self.chat_call(
                |reply| ChatCall::RunScheduleNow(id, reply),
                |s, run| {
                    crate::chat::toast_label("scheduleStarted");
                    s.load_schedules();
                    s.send(Command::OpenSession {
                        session: run.session_id.to_string(),
                    });
                },
            ),
            "delete" => self.chat_call(
                |reply| ChatCall::DeleteSchedule(id, reply),
                |s, ()| s.load_schedules(),
            ),
            _ => {}
        }
    }
}
