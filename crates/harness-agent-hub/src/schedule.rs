//! Scheduled prompts (decision 8A, phase-9-parity.md §9.2.1). Off by default, nothing auto-created.
//!
//! Owners: the store holds the schedules (written only here, one statement each); one lazy tokio task owned by the
//! hub computes due runs. The task exists only while the global switch is on and an enabled schedule exists, runs
//! only while Harness is open, and never catches up missed runs (`next_run` is recomputed from now on start and when
//! the switch is turned on).

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use chrono::{Datelike, Local, NaiveTime, TimeZone};
use harness_core_api::{
    ApiError, ApiResult, CreateSession, EventKind, NewSchedule, ScheduleKind, ScheduleRun,
    ScheduleTarget, ScheduledPrompt, SendMessage, TurnOrigin, UpdateSchedule,
};
use tokio::sync::Notify;

use crate::hub::Hub;
use crate::session::SessionCmd;
use crate::store;
use crate::store_chat;
use crate::util::{new_id, now_ms};

mod runner;

const ENABLED_KEY: &str = "schedules_enabled";

/// The hub's scheduler slot: the lazy task and its wake-up.
#[derive(Default)]
pub(crate) struct Scheduler {
    wake: Arc<Notify>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Scheduler {
    pub(crate) fn stop(&self) {
        if let Ok(mut slot) = self.task.lock() {
            if let Some(task) = slot.take() {
                task.abort();
            }
        }
    }
}

/// Next run strictly after `now` (Unix ms), local time for `Daily`.
pub(crate) fn next_run(kind: &ScheduleKind, now: i64) -> Option<i64> {
    match kind {
        ScheduleKind::Interval { seconds } => Some(now + i64::from(*seconds) * 1000),
        ScheduleKind::Daily {
            minute_of_day,
            weekdays,
        } => {
            let at = NaiveTime::from_hms_opt(minute_of_day / 60, minute_of_day % 60, 0)?;
            let today = Local.timestamp_millis_opt(now).single()?.date_naive();
            (0..=7).find_map(|offset| {
                let day = today.checked_add_days(chrono::Days::new(offset))?;
                let weekday = u8::try_from(day.weekday().number_from_monday()).ok()?;
                if !weekdays.is_empty() && !weekdays.contains(&weekday) {
                    return None;
                }
                let local = Local.from_local_datetime(&day.and_time(at)).earliest()?;
                let ms = local.timestamp_millis();
                (ms > now).then_some(ms)
            })
        }
    }
}

impl Hub {
    fn check_schedule(
        &self,
        title: &str,
        prompt: &str,
        target: &ScheduleTarget,
        kind: &ScheduleKind,
    ) -> ApiResult<()> {
        let invalid = |m: &str| Err(ApiError::InvalidArgument(m.into()));
        let length = title.trim().chars().count();
        if length == 0 || length > 100 {
            return invalid("a schedule title has 1 to 100 characters");
        }
        if prompt.trim().is_empty() {
            return invalid("a schedule needs a prompt");
        }
        if let ScheduleTarget::NewSession { workspace_path, .. } = target {
            if workspace_path.trim().is_empty() {
                return invalid("a new-session schedule needs a workspace path");
            }
        }
        match kind {
            ScheduleKind::Interval { seconds } => {
                let min = self.ctx.config.schedule_min_interval_secs;
                if *seconds < min {
                    return Err(ApiError::InvalidArgument(format!(
                        "the interval is at least {min} seconds"
                    )));
                }
            }
            ScheduleKind::Daily {
                minute_of_day,
                weekdays,
            } => {
                if *minute_of_day >= 24 * 60 {
                    return invalid("minute_of_day is below 1440");
                }
                if weekdays.iter().any(|d| !(1..=7).contains(d)) {
                    return invalid("weekdays are 1 (Monday) to 7 (Sunday)");
                }
            }
        }
        Ok(())
    }

    async fn check_target(&self, target: &ScheduleTarget) -> ApiResult<()> {
        if let ScheduleTarget::Session { session_id } = target {
            self.load_row(session_id).await?;
        }
        Ok(())
    }

    pub(crate) async fn do_schedules_enabled(&self) -> ApiResult<bool> {
        self.check_open()?;
        Ok(self
            .ctx
            .store
            .run(|c| store::get_setting(c, ENABLED_KEY))
            .await?
            .is_some_and(|v| v == "true"))
    }

    pub(crate) async fn do_set_schedules_enabled(&self, enabled: bool) -> ApiResult<()> {
        self.check_open()?;
        let now = now_ms();
        self.ctx
            .store
            .run(move |c| {
                store::set_setting(c, ENABLED_KEY, if enabled { "true" } else { "false" })?;
                if enabled {
                    // 不补跑：开关打开时按当前时间重新计算 next_run（spec §9.2.1）。
                    reschedule_all(c, now)?;
                }
                Ok(())
            })
            .await?;
        self.ctx.events.emit(None, EventKind::SchedulesChanged);
        self.ensure_scheduler().await;
        Ok(())
    }

    pub(crate) async fn do_list_schedules(&self) -> ApiResult<Vec<ScheduledPrompt>> {
        self.check_open()?;
        Ok(self.ctx.store.run(store_chat::list_schedules).await?)
    }

    async fn find_schedule(&self, id: &str) -> ApiResult<ScheduledPrompt> {
        self.do_list_schedules()
            .await?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| ApiError::NotFound(format!("schedule {id}")))
    }

    async fn save(&self, schedule: ScheduledPrompt) -> ApiResult<ScheduledPrompt> {
        let stored = schedule.clone();
        self.ctx
            .store
            .run(move |c| store_chat::save_schedule(c, &stored))
            .await?;
        self.ctx.events.emit(None, EventKind::SchedulesChanged);
        self.ensure_scheduler().await;
        Ok(schedule)
    }

    pub(crate) async fn do_create_schedule(
        &self,
        request: NewSchedule,
    ) -> ApiResult<ScheduledPrompt> {
        self.check_open()?;
        self.check_schedule(
            &request.title,
            &request.prompt,
            &request.target,
            &request.kind,
        )?;
        self.check_target(&request.target).await?;
        let now = now_ms();
        self.save(ScheduledPrompt {
            id: new_id("sch"),
            title: request.title.trim().to_owned(),
            prompt: request.prompt,
            next_run: if request.enabled {
                next_run(&request.kind, now)
            } else {
                None
            },
            target: request.target,
            kind: request.kind,
            enabled: request.enabled,
            last_run: None,
            last_result: None,
            created_at: now,
        })
        .await
    }

    pub(crate) async fn do_update_schedule(
        &self,
        request: UpdateSchedule,
    ) -> ApiResult<ScheduledPrompt> {
        self.check_open()?;
        let mut schedule = self.find_schedule(&request.id).await?;
        let timing_changed = request.kind.is_some() || request.enabled.is_some();
        if let Some(title) = request.title {
            title.trim().clone_into(&mut schedule.title);
        }
        if let Some(prompt) = request.prompt {
            schedule.prompt = prompt;
        }
        if let Some(target) = request.target {
            self.check_target(&target).await?;
            schedule.target = target;
        }
        if let Some(kind) = request.kind {
            schedule.kind = kind;
        }
        if let Some(enabled) = request.enabled {
            schedule.enabled = enabled;
        }
        self.check_schedule(
            &schedule.title,
            &schedule.prompt,
            &schedule.target,
            &schedule.kind,
        )?;
        if !schedule.enabled {
            schedule.next_run = None;
        } else if timing_changed || schedule.next_run.is_none() {
            schedule.next_run = next_run(&schedule.kind, now_ms());
        }
        self.save(schedule).await
    }

    pub(crate) async fn do_delete_schedule(&self, id: String) -> ApiResult<()> {
        self.check_open()?;
        let lookup = id.clone();
        if !self
            .ctx
            .store
            .run(move |c| store_chat::delete_schedule(c, &lookup))
            .await?
        {
            return Err(ApiError::NotFound(format!("schedule {id}")));
        }
        self.ctx.events.emit(None, EventKind::SchedulesChanged);
        self.ensure_scheduler().await;
        Ok(())
    }

    pub(crate) async fn do_run_schedule_now(&self, id: String) -> ApiResult<ScheduleRun> {
        self.check_open()?;
        let schedule = self.find_schedule(&id).await?;
        self.run_and_record(schedule, false).await
    }

    /// Called once by `Hub::start`: no catch-up, then start the task if wanted.
    pub(crate) async fn start_scheduler(&self) {
        if self.do_schedules_enabled().await.unwrap_or(false) {
            let now = now_ms();
            if let Err(error) = self.ctx.store.run(move |c| reschedule_all(c, now)).await {
                tracing::warn!(%error, "rescheduling prompts failed");
            }
        }
        self.ensure_scheduler().await;
    }
}

fn reschedule_all(c: &rusqlite::Connection, now: i64) -> Result<(), crate::HubError> {
    for mut schedule in store_chat::list_schedules(c)? {
        schedule.next_run = if schedule.enabled {
            next_run(&schedule.kind, now)
        } else {
            None
        };
        store_chat::save_schedule(c, &schedule)?;
    }
    Ok(())
}

async fn run_loop(hub: Weak<Hub>, wake: Arc<Notify>) {
    loop {
        // 每轮只临时升级弱引用，睡眠期间不持有 Hub，Hub 释放后任务自然结束。
        let sleep_for = {
            let Some(hub) = hub.upgrade() else { return };
            if hub.check_open().is_err() {
                return;
            }
            let schedules = hub.do_list_schedules().await.unwrap_or_default();
            let now = now_ms();
            let due: Vec<ScheduledPrompt> = schedules
                .iter()
                .filter(|s| s.enabled && s.next_run.is_some_and(|n| n <= now))
                .cloned()
                .collect();
            for schedule in due {
                let id = schedule.id.clone();
                if let Err(error) = hub.run_and_record(schedule, true).await {
                    tracing::info!(schedule = %id, %error, "scheduled prompt not sent");
                }
            }
            let schedules = hub.do_list_schedules().await.unwrap_or_default();
            schedules
                .iter()
                .filter(|s| s.enabled)
                .filter_map(|s| s.next_run)
                .min()
                .map(|next| {
                    Duration::from_millis(u64::try_from((next - now_ms()).max(0)).unwrap_or(0))
                })
        };
        match sleep_for {
            Some(duration) => {
                tokio::select! {
                    () = tokio::time::sleep(duration) => {}
                    () = wake.notified() => {}
                }
            }
            None => wake.notified().await,
        }
    }
}

#[cfg(test)]
mod tests;
