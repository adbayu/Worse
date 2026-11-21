use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use harness_core_api::{
    AgentId, ApiError, ApiResult, BackgroundTask, ErrorClass, EventKind, ToolCallStatus,
    TurnAttempt, TurnId, TurnOrigin, TurnOutcome, TurnStatus, TurnView,
};
use serde_json::{json, Value};

use super::{ActorMsg, ActorTx, Running, SessionActor};
use crate::acp::AcpConnection;
use crate::store;
use crate::util::{new_id, now_ms};

mod autonomous;

const MAX_TASKS: usize = 50;
const IDLE_END: Duration = Duration::from_secs(20);
const TERMINAL: [&str; 5] = ["completed", "failed", "stopped", "cancelled", "killed"];

pub(super) struct Tracked {
    view: BackgroundTask,
    native: String,
    lease: Option<Lease>,
}

struct Lease {
    connection: Arc<AcpConnection>,
    _watch: Watch,
}

pub(super) struct Watch(Arc<AtomicBool>);

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

pub(super) struct Autonomous {
    pub(super) task_id: String,
    _watch: Watch,
    idle: bool,
    stamp: u64,
}

pub(super) fn is_turn_content(update: &Value) -> bool {
    matches!(
        update.get("sessionUpdate").and_then(Value::as_str),
        Some(
            "agent_message_chunk"
                | "agent_thought_chunk"
                | "tool_call"
                | "tool_call_update"
                | "plan"
        )
    )
}

pub(super) fn is_task_update(update: &Value) -> bool {
    update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.starts_with("async_task_"))
}

fn text(update: &Value, key: &str) -> Option<String> {
    update.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn watch(tx: &ActorTx, agent: &AgentId, connection: &Arc<AcpConnection>) -> Watch {
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let tx = tx.clone();
    let agent = agent.clone();
    let connection = Arc::clone(connection);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if flag.load(Ordering::SeqCst) {
                return;
            }
            if connection.is_closed() {
                let _ = tx.send(ActorMsg::AgentExited(agent, connection.generation()));
                return;
            }
        }
    });
    Watch(done)
}

impl SessionActor {
    pub(super) fn background_views(&self) -> Vec<BackgroundTask> {
        self.tasks.iter().rev().map(|t| t.view.clone()).collect()
    }

    fn emit_tasks(&self) {
        self.emit(EventKind::BackgroundTasksChanged {
            tasks: self.background_views(),
        });
    }

    fn agent_of_native(&self, native: &str) -> Option<AgentId> {
        if let Some(running) = self
            .running
            .as_ref()
            .filter(|r| r.native.as_deref() == Some(native))
        {
            return Some(running.agent.clone());
        }
        self.handles
            .iter()
            .find(|(_, h)| h.native == native)
            .map(|(agent, _)| agent.clone())
    }

    async fn lease_connection(&self, agent: &AgentId) -> Option<Arc<AcpConnection>> {
        let generation = self
            .running
            .as_ref()
            .filter(|r| &r.agent == agent)
            .and_then(|r| r.connection.as_ref())
            .map(|c| c.generation())
            .or_else(|| self.handles.get(agent).and_then(|h| h.generation))?;
        self.ctx.pool.acquire_live(agent, generation).await
    }

    async fn release_lease(&self, agent: &AgentId, lease: Lease) {
        self.ctx
            .pool
            .release(agent, lease.connection.generation())
            .await;
    }

    pub(super) async fn on_task_update(&mut self, native: &str, update: &Value) {
        let Some(agent) = self.agent_of_native(native) else {
            return;
        };
        let Some(id) = text(update, "asyncTaskId") else {
            return;
        };
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("async_task_spawned") => self.on_task_spawned(agent, native, id, update).await,
            Some("async_task_progress") => {
                let Some(task) = self.tasks.iter_mut().find(|t| t.view.id == id) else {
                    return;
                };
                if let Some(description) = text(update, "description") {
                    task.view.description = description;
                }
                if let Some(file) = text(update, "outputFilePath") {
                    task.view.output_file = Some(file);
                }
                self.emit_tasks();
            }
            Some("async_task_state_update") => {
                let state = text(update, "state").unwrap_or_default();
                let summary = text(update, "summary");
                if self
                    .running
                    .as_ref()
                    .and_then(|r| r.autonomous.as_ref())
                    .is_some_and(|a| a.task_id == id)
                {
                    if TERMINAL.contains(&state.as_str()) {
                        self.end_autonomous(&state, summary).await;
                    }
                    return;
                }
                self.on_task_state(&agent, &id, state, summary, update)
                    .await;
            }
            _ => {}
        }
    }

    async fn on_task_spawned(&mut self, agent: AgentId, native: &str, id: String, update: &Value) {
        let task_type = text(update, "taskType").unwrap_or_default();
        let description = text(update, "description").unwrap_or_default();
        if task_type == "turn" {
            self.start_autonomous(agent, native, id, &description, false)
                .await;
            return;
        }
        if self.tasks.iter().any(|t| t.view.id == id) {
            return;
        }
        let lease = self.lease_connection(&agent).await.map(|connection| Lease {
            _watch: watch(&self.tx, &agent, &connection),
            connection,
        });
        let view = BackgroundTask {
            id,
            agent,
            name: text(update, "name").unwrap_or_default(),
            task_type,
            description,
            state: "running".into(),
            summary: String::new(),
            output_file: text(update, "outputFilePath"),
            can_stop: update
                .get("canStop")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            started_at: now_ms(),
            finished_at: None,
        };
        self.tasks.push(Tracked {
            view,
            native: native.to_owned(),
            lease,
        });
        self.trim_tasks();
        self.emit_tasks();
    }

    pub(super) async fn on_agent_exited(&mut self, agent: &AgentId, generation: u64) {
        if self.running.as_ref().is_some_and(|r| {
            r.autonomous.is_some()
                && &r.agent == agent
                && r.connection.as_ref().map(|c| c.generation()) == Some(generation)
        }) {
            let error = "agent process exited".to_owned();
            self.end_running(
                TurnStatus::Failed,
                None,
                Some(error.clone()),
                TurnOutcome::Failed {
                    error,
                    class: ErrorClass::Fatal,
                },
            )
            .await;
        }
        let mut changed = false;
        for task in &mut self.tasks {
            let hit = task.view.is_running()
                && &task.view.agent == agent
                && task
                    .lease
                    .as_ref()
                    .is_some_and(|l| l.connection.generation() == generation);
            if hit {
                task.view.state = "failed".into();
                task.view.summary = "agent process exited".into();
                task.view.finished_at = Some(now_ms());
                task.lease = None;
                changed = true;
            }
        }
        if changed {
            self.emit_tasks();
        }
    }

    pub(super) async fn on_stop_background_task(&mut self, id: &str) -> ApiResult<()> {
        if self
            .running
            .as_ref()
            .and_then(|r| r.autonomous.as_ref())
            .is_some_and(|a| a.task_id == id)
        {
            self.on_stop().await;
            return Ok(());
        }
        let Some(task) = self
            .tasks
            .iter_mut()
            .find(|t| t.view.id == id && t.view.is_running())
        else {
            return Err(ApiError::NotFound(format!("running background task {id}")));
        };
        if !task.view.can_stop {
            return Err(ApiError::InvalidArgument(format!(
                "background task {id} cannot be stopped"
            )));
        }
        let Some(connection) = task.lease.as_ref().map(|l| Arc::clone(&l.connection)) else {
            return Err(ApiError::NotFound(format!("running background task {id}")));
        };
        drop(connection.send_request(
            "_session/async_task/stop",
            json!({"sessionId": task.native, "asyncTaskId": id}),
        ));
        task.view.state = "stopping".into();
        self.emit_tasks();
        Ok(())
    }

    pub(super) async fn release_task_leases(&mut self) {
        let leases: Vec<(AgentId, Lease)> = self
            .tasks
            .iter_mut()
            .filter_map(|t| t.lease.take().map(|l| (t.view.agent.clone(), l)))
            .collect();
        for (agent, lease) in leases {
            self.release_lease(&agent, lease).await;
        }
    }
}
