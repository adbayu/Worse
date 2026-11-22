//! agy background commands as Harness background tasks (reader: `crate::agy_tasks`). When an agy turn ends
//! with a shell tool call still running, agy has moved it into its own task; the actor polls agy's brain
//! folder, shows each task in the background task list with its log, holds the agy process alive while
//! it runs (a lease, like ACP async tasks) and marks it done from agy's completion message.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use harness_core_api::{AgentId, BackgroundTask, ToolCallStatus};

use super::{now_ms, watch, ActorMsg, ActorTx, Lease, SessionActor, Tracked, Value, Watch};
use crate::agy_tasks::{self, Snapshot};

const POLL: Duration = Duration::from_secs(3);
/// How long a running command waits for its task log before it is dropped.
const MATCH_WINDOW_MS: i64 = 120_000;
const PREFIX: &str = "agy-";

pub(in crate::session) struct AgyTrack {
    agent: AgentId,
    native: String,
    since_ms: i64,
    /// Summaries of running commands not yet matched to a task log, with the time they were noted.
    pending: VecDeque<(String, i64)>,
    _poll: Watch,
}

fn poll(tx: &ActorTx) -> Watch {
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let tx = tx.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(POLL).await;
            if flag.load(Ordering::SeqCst) || tx.send(ActorMsg::AgyPoll).is_err() {
                return;
            }
        }
    });
    Watch(done)
}

fn task_id(native: &str, name: &str) -> String {
    format!("{PREFIX}{native}/{name}")
}

impl SessionActor {
    /// Called before a turn ends: agy shell commands still running there went to agy's background.
    pub(in crate::session) fn note_agy_async(&mut self) {
        let Some(running) = self.running.as_ref() else {
            return;
        };
        if !crate::agy_ops::is_agy(&self.ctx, &running.agent) {
            return;
        }
        let Some(native) = running.native.clone() else {
            return;
        };
        let agent = running.agent.clone();
        let Some(turn) = self.turn(&running.turn_id) else {
            return;
        };
        let since = turn.created_at;
        let summaries: Vec<String> = turn
            .tool_calls
            .iter()
            .filter(|c| {
                matches!(
                    c.status,
                    None | Some(ToolCallStatus::Pending | ToolCallStatus::InProgress)
                )
            })
            .filter_map(|c| agy_tasks::async_command(c.raw_input.as_deref()))
            .collect();
        if summaries.is_empty() {
            return;
        }
        if self
            .agy
            .as_ref()
            .is_some_and(|t| t.native != native || t.agent != agent)
        {
            self.agy = None;
        }
        let tx = self.tx.clone();
        let track = self.agy.get_or_insert_with(|| AgyTrack {
            agent,
            native,
            since_ms: since,
            pending: VecDeque::new(),
            _poll: poll(&tx),
        });
        track.since_ms = track.since_ms.min(since);
        let now = now_ms();
        track
            .pending
            .extend(summaries.into_iter().map(|s| (s, now)));
    }

    pub(in crate::session) async fn on_agy_poll(&mut self) {
        let Some(dir) = self
            .agy
            .as_ref()
            .and_then(|t| agy_tasks::session_dir(&t.native))
        else {
            self.agy = None;
            return;
        };
        let snap = tokio::task::spawn_blocking(move || agy_tasks::snapshot(&dir))
            .await
            .unwrap_or_default();
        self.adopt_agy_tasks(&snap).await;
        self.finish_agy_tasks(&snap).await;
        let now = now_ms();
        if let Some(track) = self.agy.as_mut() {
            track.pending.retain(|(_, at)| now - at < MATCH_WINDOW_MS);
        }
        let running = self
            .tasks
            .iter()
            .any(|t| t.view.id.starts_with(PREFIX) && t.view.is_running());
        if !running && self.agy.as_ref().is_some_and(|t| t.pending.is_empty()) {
            self.agy = None;
        }
    }

    async fn adopt_agy_tasks(&mut self, snap: &Snapshot) {
        let Some(track) = self.agy.as_ref() else {
            return;
        };
        let (agent, native, since) = (track.agent.clone(), track.native.clone(), track.since_ms);
        let fresh: Vec<agy_tasks::AgyTask> = snap
            .tasks
            .iter()
            .filter(|t| t.modified_ms >= since && !snap.finished.contains_key(&t.name))
            .filter(|t| {
                let id = task_id(&native, &t.name);
                !self.tasks.iter().any(|k| k.view.id == id)
            })
            .cloned()
            .collect();
        let mut changed = false;
        for task in fresh {
            let Some((summary, _)) = self.agy.as_mut().and_then(|t| t.pending.pop_front()) else {
                break;
            };
            let lease = self.lease_connection(&agent).await.map(|connection| Lease {
                _watch: watch(&self.tx, &agent, &connection),
                connection,
            });
            self.tasks.push(Tracked {
                view: BackgroundTask {
                    id: task_id(&native, &task.name),
                    agent: agent.clone(),
                    name: summary,
                    task_type: "shell".into(),
                    description: format!("Antigravity {}", task.name),
                    state: "running".into(),
                    summary: String::new(),
                    output_file: Some(task.log.display().to_string()),
                    can_stop: false,
                    started_at: if task.modified_ms > 0 {
                        task.modified_ms
                    } else {
                        now_ms()
                    },
                    finished_at: None,
                },
                native: native.clone(),
                lease,
            });
            changed = true;
        }
        if changed {
            self.trim_tasks();
            self.emit_tasks();
        }
    }

    async fn finish_agy_tasks(&mut self, snap: &Snapshot) {
        let done: Vec<(String, AgentId, agy_tasks::Finished)> = self
            .tasks
            .iter()
            .filter(|t| t.view.is_running() && t.view.id.starts_with(PREFIX))
            .filter_map(|t| {
                let name = t.view.id.rsplit('/').next()?;
                let finished = snap.finished.get(name)?.clone();
                Some((t.view.id.clone(), t.view.agent.clone(), finished))
            })
            .collect();
        for (id, agent, finished) in done {
            let state = if finished.ok { "completed" } else { "failed" };
            self.on_task_state(
                &agent,
                &id,
                state.into(),
                Some(finished.summary),
                &Value::Null,
            )
            .await;
        }
    }
}
