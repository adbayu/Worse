//! Agent process pool (spec §10): one process per agent id and launch variant, shared by all sessions;
//! spawned on demand, stopped after the idle timeout. Single owner of agent process lifecycle.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use harness_core_api::{AgentId, AgentRuntimeState, EventKind};
use tokio::sync::Mutex;

use crate::acp::AcpConnection;
use crate::agents::{resolve_program, AgentSpec};
use crate::bus::EventBus;

/// A pool slot: the agent plus the launch variant (`false` normal, `true` code mode).
type SlotKey = (AgentId, bool);

struct Slot {
    connection: Arc<AcpConnection>,
    busy: u32,
    idle_epoch: u64,
}

struct Shared {
    specs: HashMap<AgentId, AgentSpec>,
    idle: Duration,
    events: EventBus,
    slots: Mutex<HashMap<SlotKey, Slot>>,
    next_generation: AtomicU64,
    shutting_down: AtomicBool,
    /// Agents whose idle-stopped process is still shutting down (slot already removed).
    stopping: std::sync::Mutex<HashMap<SlotKey, u32>>,
}

/// How one acquire launches the process: which slot it fills and with which args/env.
struct Plan {
    key: SlotKey,
    spec: AgentSpec,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

/// Cloneable handle to the pool.
#[derive(Clone)]
pub struct AgentPool {
    shared: Arc<Shared>,
}

impl AgentPool {
    /// Pool over the configured agents.
    #[must_use]
    pub fn new(specs: &[AgentSpec], idle: Duration, events: EventBus) -> Self {
        Self {
            shared: Arc::new(Shared {
                specs: specs.iter().map(|s| (s.id.clone(), s.clone())).collect(),
                idle,
                events,
                slots: Mutex::new(HashMap::new()),
                next_generation: AtomicU64::new(1),
                shutting_down: AtomicBool::new(false),
                stopping: std::sync::Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Slot and launch of one acquire. `code_mode` selects the code mode variant only for an agent that has one;
    /// an agent without it always uses the normal process.
    fn plan(&self, agent: &AgentId, code_mode: bool) -> Result<Plan, String> {
        let spec = self
            .shared
            .specs
            .get(agent)
            .ok_or_else(|| format!("unknown agent {agent}"))?;
        Ok(plan(spec, code_mode))
    }

    /// Returns a live, initialized connection and marks it busy. Pair with [`AgentPool::release`].
    ///
    /// `code_mode` runs the turn on the code mode variant of the agent when it has one, else on the normal process;
    /// both variants keep their own slot, generation and native sessions.
    ///
    /// # Errors
    ///
    /// Unknown/not-installed agent, spawn or `initialize` failure (message for the transcript).
    pub async fn acquire(
        &self,
        agent: &AgentId,
        code_mode: bool,
    ) -> Result<Arc<AcpConnection>, String> {
        if self.shared.shutting_down.load(Ordering::SeqCst) {
            return Err("hub is shutting down".into());
        }
        let plan = self.plan(agent, code_mode)?;
        let mut slots = self.shared.slots.lock().await;
        if let Some(slot) = slots.get_mut(&plan.key) {
            if !slot.connection.is_closed() {
                slot.busy += 1;
                slot.idle_epoch += 1;
                let pid = slot.connection.pid();
                let connection = Arc::clone(&slot.connection);
                drop(slots);
                self.emit_runtime(agent, AgentRuntimeState::Busy, pid);
                return Ok(connection);
            }
            slots.remove(&plan.key);
        }
        let program = resolve_program(&plan.spec.program).ok_or_else(|| {
            format!(
                "{} is not installed: {}",
                plan.spec.display_name, plan.spec.install_hint
            )
        })?;
        let generation = self.shared.next_generation.fetch_add(1, Ordering::SeqCst);
        self.emit_runtime(agent, AgentRuntimeState::Starting, None);
        let on_exit = {
            let pool = self.clone();
            let key = plan.key.clone();
            Box::new(move || {
                tokio::spawn(async move { pool.on_process_exit(&key, generation).await });
            })
        };
        let connection = AcpConnection::spawn(&program, &plan.args, &plan.env, generation, on_exit)
            .map_err(|e| format!("failed to start {}: {e}", plan.spec.display_name))?;
        if let Err(error) = connection.initialize().await {
            connection.shutdown().await;
            self.emit_runtime(agent, AgentRuntimeState::Crashed, None);
            return Err(format!(
                "{} initialize failed: {error}",
                plan.spec.display_name
            ));
        }
        slots.insert(
            plan.key.clone(),
            Slot {
                connection: Arc::clone(&connection),
                busy: 1,
                idle_epoch: 0,
            },
        );
        drop(slots);
        self.emit_runtime(agent, AgentRuntimeState::Busy, connection.pid());
        Ok(connection)
    }

    /// The live process of `agent` at `generation`, whichever variant it is.
    pub async fn acquire_live(
        &self,
        agent: &AgentId,
        generation: u64,
    ) -> Option<Arc<AcpConnection>> {
        let mut slots = self.shared.slots.lock().await;
        let (_, slot) = slots.iter_mut().find(|((id, _), slot)| {
            id == agent
                && slot.connection.generation() == generation
                && !slot.connection.is_closed()
        })?;
        slot.busy += 1;
        slot.idle_epoch += 1;
        Some(Arc::clone(&slot.connection))
    }

    /// Ends one use of a connection; at zero users arms the idle stop.
    pub async fn release(&self, agent: &AgentId, generation: u64) {
        let mut slots = self.shared.slots.lock().await;
        let Some(key) = slots
            .iter()
            .find(|((id, _), slot)| id == agent && slot.connection.generation() == generation)
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        let Some(slot) = slots.get_mut(&key) else {
            return;
        };
        slot.busy = slot.busy.saturating_sub(1);
        if slot.busy > 0 {
            return;
        }
        slot.idle_epoch += 1;
        let epoch = slot.idle_epoch;
        let pid = slot.connection.pid();
        drop(slots);
        self.emit_runtime(agent, AgentRuntimeState::Idle, pid);
        let pool = self.clone();
        let idle = self.shared.idle;
        tokio::spawn(async move {
            // Idle stop is a product rule (spec §10), not a sync workaround: stop only if the epoch is unchanged.
            tokio::time::sleep(idle).await;
            pool.stop_if_idle(&key, generation, epoch).await;
        });
    }

    async fn stop_if_idle(&self, key: &SlotKey, generation: u64, epoch: u64) {
        let mut slots = self.shared.slots.lock().await;
        let still_idle = slots.get(key).is_some_and(|slot| {
            slot.connection.generation() == generation && slot.busy == 0 && slot.idle_epoch == epoch
        });
        if !still_idle {
            return;
        }
        let Some(slot) = slots.remove(key) else {
            return;
        };
        // 修复说明：原先先移除 slot 再 await shutdown（最长 SHUTDOWN_GRACE），这段时间里 runtime_state 已报告
        // Stopped 而进程仍存活（Agents 概览会显示错误状态）。现在 shutdown 完成前记为 stopping，runtime_state 仍报告
        // Idle。（phase8-core 门禁的偶发残留报告另有原因，见 harness-core-gate idle 检查的说明。）
        self.mark_stopping(key, true);
        drop(slots);
        let pid = slot.connection.pid();
        slot.connection.shutdown().await;
        self.mark_stopping(key, false);
        self.emit_runtime(&key.0, AgentRuntimeState::Stopped, pid);
    }

    fn mark_stopping(&self, key: &SlotKey, on: bool) {
        if let Ok(mut stopping) = self.shared.stopping.lock() {
            if on {
                *stopping.entry(key.clone()).or_default() += 1;
            } else if let Some(n) = stopping.get_mut(key) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    stopping.remove(key);
                }
            }
        }
    }

    fn is_stopping(&self, agent: &AgentId) -> bool {
        self.shared
            .stopping
            .lock()
            .is_ok_and(|stopping| stopping.keys().any(|(id, _)| id == agent))
    }

    async fn on_process_exit(&self, key: &SlotKey, generation: u64) {
        let mut slots = self.shared.slots.lock().await;
        let ours = slots
            .get(key)
            .is_some_and(|slot| slot.connection.generation() == generation);
        if !ours {
            return; // intentional stop already removed the slot
        }
        let slot = slots.remove(key);
        drop(slots);
        let pid = slot.as_ref().and_then(|s| s.connection.pid());
        if let Some(slot) = slot {
            slot.connection.shutdown().await;
        }
        self.emit_runtime(&key.0, AgentRuntimeState::Crashed, pid);
    }

    /// Current process state of an agent, over its normal and code mode process: busy, else crashed, else idle.
    pub async fn runtime_state(&self, agent: &AgentId) -> AgentRuntimeState {
        let slots = self.shared.slots.lock().await;
        let mut present = false;
        let mut busy = false;
        let mut crashed = false;
        for (_, slot) in slots.iter().filter(|((id, _), _)| id == agent) {
            present = true;
            if slot.connection.is_closed() {
                crashed = true;
            } else if slot.busy > 0 {
                busy = true;
            }
        }
        if busy {
            AgentRuntimeState::Busy
        } else if crashed {
            AgentRuntimeState::Crashed
        } else if present || self.is_stopping(agent) {
            AgentRuntimeState::Idle
        } else {
            AgentRuntimeState::Stopped
        }
    }

    /// `agentInfo` (name, version) of a live process of an agent, if any.
    pub async fn agent_info(&self, agent: &AgentId) -> Option<(Option<String>, Option<String>)> {
        let slots = self.shared.slots.lock().await;
        slots
            .iter()
            .find(|((id, _), slot)| id == agent && !slot.connection.is_closed())
            .map(|(_, slot)| slot.connection.agent_info())
    }

    /// Stops every process of one agent now, busy or not (its in-flight calls fail).
    pub async fn stop_agent(&self, agent: &AgentId) {
        let keys: Vec<SlotKey> = {
            let slots = self.shared.slots.lock().await;
            slots
                .keys()
                .filter(|(id, _)| id == agent)
                .cloned()
                .collect()
        };
        if keys.is_empty() {
            return;
        }
        let mut pid = None;
        for key in keys {
            let Some(slot) = self.shared.slots.lock().await.remove(&key) else {
                continue;
            };
            pid = pid.or_else(|| slot.connection.pid());
            slot.connection.shutdown().await;
        }
        self.emit_runtime(agent, AgentRuntimeState::Stopped, pid);
    }

    /// Stops every agent process (hub shutdown).
    pub async fn shutdown_all(&self) {
        self.shared.shutting_down.store(true, Ordering::SeqCst);
        let slots: Vec<(SlotKey, Slot)> = self.shared.slots.lock().await.drain().collect();
        let mut stopped: Vec<(AgentId, Option<u32>)> = Vec::new();
        for (key, slot) in slots {
            let pid = slot.connection.pid();
            slot.connection.shutdown().await;
            if let Some(entry) = stopped.iter_mut().find(|(agent, _)| agent == &key.0) {
                entry.1 = entry.1.or(pid);
            } else {
                stopped.push((key.0, pid));
            }
        }
        for (agent, pid) in stopped {
            self.emit_runtime(&agent, AgentRuntimeState::Stopped, pid);
        }
    }

    fn emit_runtime(&self, agent: &AgentId, state: AgentRuntimeState, pid: Option<u32>) {
        self.shared.events.emit(
            None,
            EventKind::AgentRuntime {
                agent: agent.clone(),
                state,
                pid,
            },
        );
    }
}

/// Slot and launch of one acquire of `spec`.
fn plan(spec: &AgentSpec, code_mode: bool) -> Plan {
    let variant = code_mode.then(|| spec.code_mode_launch()).flatten();
    let mut args = spec.args.clone();
    let mut env = spec.env.clone();
    if let Some(variant) = &variant {
        args.extend(variant.args.iter().cloned());
        env.extend(variant.env.iter().cloned());
    }
    Plan {
        key: (spec.id.clone(), variant.is_some()),
        spec: spec.clone(),
        args,
        env,
    }
}

#[cfg(test)]
mod tests;
