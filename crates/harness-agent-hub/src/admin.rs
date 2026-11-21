//! Agents overview, MEMORY.md plan/apply, projects and archive (phase-9-parity.md §9.1.5a).
//!
//! Owners: the store for rows; session actors for live state. Archive/delete/remove-with-data hold the actor
//! registry lock while they retire the affected actors and change the store, so no actor is re-spawned in between.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use harness_agent_config::{AgentKind, AuthState, ConfigPaths};
use harness_core_api::{
    AgentId, AgentOverview, AgentTestResult, ApiError, ApiResult, ArchivePage, ArchivedSession,
    AuthPresence, EventKind, ListArchived, ListProjects, MemoryDoc, MemoryFileInfo, MemoryPlan,
    MemoryScope, ProjectInfo, SessionId, SessionSelection,
};
use serde_json::json;
use tokio::sync::oneshot;

use crate::acp::AcpConnection;
use crate::agents::resolve_program;
use crate::hub::Hub;
use crate::memory::fingerprint;
use crate::session::{ActorMsg, ActorTx, SessionCmd};
use crate::store;
use crate::util::now_ms;

mod archive;
mod memory;
mod projects;

/// ACP `initialize` budget of the Test button.
const TEST_TIMEOUT: Duration = Duration::from_secs(20);
/// A project without a session update for this long is "unused".
const UNUSED_AFTER_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const INFO_PREFIX: &str = "agent_info:";
const ARCHIVE_PAGE_MAX: u32 = 200;

fn login_hint(kind: AgentKind) -> Option<String> {
    match kind {
        AgentKind::ClaudeCode => {
            Some("Run `claude` in a terminal and sign in (or set ANTHROPIC_API_KEY).".into())
        }
        AgentKind::Codex => Some("Run `codex login` in a terminal (or set OPENAI_API_KEY).".into()),
        AgentKind::Agy => {
            Some("Run `agy` in a terminal and sign in (or set GEMINI_API_KEY).".into())
        }
        AgentKind::Harness => None,
    }
}

fn presence(state: AuthState) -> AuthPresence {
    match state {
        AuthState::Present => AuthPresence::Present,
        AuthState::Absent => AuthPresence::Absent,
        AuthState::Unknown => AuthPresence::Unknown,
    }
}

impl Hub {
    pub(crate) fn agent_kind(&self, agent: &AgentId) -> Option<AgentKind> {
        self.ctx
            .config
            .agent_kinds
            .get(agent)
            .copied()
            .or_else(|| AgentKind::from_agent_id(agent.as_str()))
            .filter(|k| *k != AgentKind::Harness)
    }

    async fn cached_info(&self, agent: &AgentId) -> (Option<String>, Option<String>) {
        let key = format!("{INFO_PREFIX}{agent}");
        let stored = self
            .ctx
            .store
            .run(move |c| store::get_setting(c, &key))
            .await
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok());
        let field = |k: &str| {
            stored
                .as_ref()
                .and_then(|v| v.get(k))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        (field("name"), field("version"))
    }

    async fn record_info(&self, agent: &AgentId, info: &(Option<String>, Option<String>)) {
        if info.1.is_none() || &self.cached_info(agent).await == info {
            return;
        }
        let key = format!("{INFO_PREFIX}{agent}");
        let json = json!({"name": info.0, "version": info.1}).to_string();
        if let Err(error) = self
            .ctx
            .store
            .run(move |c| store::set_setting(c, &key, &json))
            .await
        {
            tracing::warn!(%error, "saving agent info failed");
        }
    }

    pub(crate) async fn do_agents_overview(&self) -> ApiResult<Vec<AgentOverview>> {
        self.check_open()?;
        let paths = ConfigPaths::from_env(&self.ctx.config.home, None).ok();
        let mut out = Vec::new();
        for spec in &self.ctx.config.agents {
            let availability = spec.probe();
            let adapter_package = crate::setup::known_cli(&spec.program)
                .and_then(|k| k.package)
                .map(str::to_owned);
            let (installed, command_path, install_hint, adapter_missing) = match availability {
                harness_core_api::Availability::Available { command_path } => {
                    (true, Some(command_path), None, false)
                }
                harness_core_api::Availability::NotInstalled { hint } => {
                    (false, None, Some(hint), false)
                }
                harness_core_api::Availability::AdapterMissing { hint, .. } => {
                    (false, None, Some(hint), true)
                }
            };
            if let Some(live) = self.ctx.pool.agent_info(&spec.id).await {
                self.record_info(&spec.id, &live).await;
            }
            let (_, version) = self.cached_info(&spec.id).await;
            let kind = self.agent_kind(&spec.id);
            let account_auth = crate::accounts::auth_for_spec(&self.ctx.config, spec).await;
            let auth =
                match (kind, &paths) {
                    // 账号 agent（§9.6）：看其 profile 目录里的登录文件，而不是主 ~/.claude。
                    _ if account_auth.is_some() => account_auth.unwrap_or(AuthPresence::Unknown),
                    // API provider（§9.7）：有密钥即视为已登录（只看是否为空，不读取或返回密钥）。
                    _ if crate::providers::is_provider(spec) => {
                        if spec.env.iter().any(|(k, v)| {
                            k == crate::providers::ENV_API_KEY && !v.trim().is_empty()
                        }) {
                            AuthPresence::Present
                        } else {
                            AuthPresence::Absent
                        }
                    }
                    // A Command Code key (providers.rs): the key in its environment is its login.
                    _ if spec.env.iter().any(|(k, v)| {
                        k == crate::providers::ENV_COMMAND_CODE_KEY && !v.trim().is_empty()
                    }) =>
                    {
                        AuthPresence::Present
                    }
                    (Some(kind), Some(paths)) => {
                        presence(self.ctx.config.auth_probe.0.probe(kind, paths).await)
                    }
                    _ => AuthPresence::Unknown,
                };
            out.push(AgentOverview {
                id: spec.id.clone(),
                display_name: spec.display_name.clone(),
                installed,
                command_path,
                install_hint,
                version,
                auth,
                login_hint: kind.and_then(login_hint),
                runtime: self.ctx.pool.runtime_state(&spec.id).await,
                quota: self.ctx.quota.get(&spec.id),
                adapter_missing,
                adapter_package,
            });
        }
        Ok(out)
    }

    pub(crate) async fn do_test_agent(&self, agent: AgentId) -> ApiResult<AgentTestResult> {
        self.check_open()?;
        let spec = self
            .ctx
            .specs
            .get(&agent)
            .cloned()
            .ok_or_else(|| ApiError::NotFound(format!("agent {agent}")))?;
        let failed = |latency_ms: u64, error: String| AgentTestResult {
            agent: agent.clone(),
            ok: false,
            latency_ms,
            agent_name: None,
            version: None,
            error: Some(error),
        };
        let Some(program) = resolve_program(&spec.program) else {
            return Ok(failed(
                0,
                format!(
                    "{} is not installed: {}",
                    spec.display_name, spec.install_hint
                ),
            ));
        };
        let started = Instant::now();
        // A separate process (generation 0), never the pooled one: the test must not disturb running turns.
        let connection =
            match AcpConnection::spawn(&program, &spec.args, &spec.env, 0, Box::new(|| {})) {
                Ok(connection) => connection,
                Err(error) => return Ok(failed(0, format!("failed to start: {error}"))),
            };
        let result = tokio::time::timeout(TEST_TIMEOUT, connection.initialize()).await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let info = connection.agent_info();
        connection.shutdown().await;
        match result {
            Ok(Ok(())) => {
                self.record_info(&agent, &info).await;
                Ok(AgentTestResult {
                    agent,
                    ok: true,
                    latency_ms,
                    agent_name: info.0,
                    version: info.1,
                    error: None,
                })
            }
            Ok(Err(error)) => Ok(failed(latency_ms, format!("initialize failed: {error}"))),
            Err(_) => Ok(failed(
                latency_ms,
                format!("no initialize answer within {}s", TEST_TIMEOUT.as_secs()),
            )),
        }
    }
}
