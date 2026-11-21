//! Phase 9.1 core gate, steps 8–11 (phase-9-parity.md §9.1.7): agents overview + test probe, MEMORY.md
//! plan/apply, projects, archive. Runs after the relocation step against the relocated data dir. No real CLI is
//! launched: every agent is the fake agent or a missing program, and login presence comes from a static probe.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use harness_agent_config::{AgentKind, AuthState, StaticProbe};
use harness_agent_hub::agents::AgentSpec;
use harness_agent_hub::{HubConfig, SharedAuthProbe};
use harness_core_api::{
    AgentId, AgentRuntimeState, ApiError, AuthPresence, CreateSession, EventKind, HarnessCore,
    ListArchived, ListProjects, ListSessions, MemoryScope, ProjectInfo, SessionId,
    SessionSelection,
};

use super::phase9::{hub_config, start};
use super::{check, step, Env, Gate, A, B, C, R};

mod archive;
mod projects;

use archive::archive;
use projects::projects;

const MISSING: &str = "gate-missing";

#[allow(clippy::needless_pass_by_value)] // used as `map_err(e)`, which passes by value
fn e(error: ApiError) -> String {
    error.to_string()
}

fn admin_config(env: &Env, data_dir: &Path) -> HubConfig {
    let mut config = hub_config(env, data_dir, false);
    let mut missing: AgentSpec = config.agents[0].clone();
    missing.id = AgentId::new(MISSING);
    missing.display_name = "Gate Missing".into();
    missing.program = "harness-gate-no-such-agent-cli".into();
    missing.install_hint = "install gate-missing".into();
    config.agents.push(missing);
    config.agent_kinds = HashMap::from([
        (AgentId::new(A), AgentKind::ClaudeCode),
        (AgentId::new(B), AgentKind::Codex),
        (AgentId::new(C), AgentKind::Agy),
    ]);
    config.auth_probe = SharedAuthProbe(Arc::new(StaticProbe(BTreeMap::from([
        (AgentKind::ClaudeCode, AuthState::Present),
        (AgentKind::Codex, AuthState::Absent),
    ]))));
    config
}

pub async fn run(env: &Env, data_dir: &Path) -> R<()> {
    let gate = start(admin_config(env, data_dir)).await?;
    agents_overview(&gate).await?;
    gate.hub.shutdown().await.map_err(e)?;
    // The tested version survives a restart (store cache).
    let gate = start(admin_config(env, data_dir)).await?;
    let rows = gate.hub.agents_overview().await.map_err(e)?;
    check(
        rows.iter()
            .any(|r| r.id.as_str() == C && r.version.as_deref() == Some("0.1.0")),
        "tested version lost across restart",
    )?;
    step("agents_overview");
    memory_admin(env, &gate, data_dir).await?;
    projects(env, &gate).await?;
    archive(env, &gate).await?;
    gate.hub.shutdown().await.map_err(e)
}

// ---- 8. agents overview ----------------------------------------------------------------------------------

async fn agents_overview(gate: &Gate) -> R<()> {
    let hub = &gate.hub;
    let rows = hub.agents_overview().await.map_err(e)?;
    let row = |id: &str| {
        rows.iter()
            .find(|r| r.id.as_str() == id)
            .cloned()
            .ok_or(format!("overview has no {id}"))
    };
    let (a, b, c, missing) = (row(A)?, row(B)?, row(C)?, row(MISSING)?);
    check(
        a.installed && a.command_path.is_some() && a.install_hint.is_none(),
        format!("installed agent row: {a:?}"),
    )?;
    check(
        !missing.installed
            && missing.install_hint.as_deref() == Some("install gate-missing")
            && missing.auth == AuthPresence::Unknown,
        format!("missing agent row: {missing:?}"),
    )?;
    check(
        a.auth == AuthPresence::Present
            && b.auth == AuthPresence::Absent
            && c.auth == AuthPresence::Unknown,
        format!("auth presence {:?}/{:?}/{:?}", a.auth, b.auth, c.auth),
    )?;
    check(
        a.login_hint.is_some() && missing.login_hint.is_none(),
        "login hints",
    )?;
    check(
        c.version.is_none() && c.runtime == AgentRuntimeState::Stopped,
        format!("untested agent row: {c:?}"),
    )?;
    let before = gate.events().len();
    let tested = hub.test_agent(AgentId::new(C)).await.map_err(e)?;
    check(
        tested.ok && tested.version.as_deref() == Some("0.1.0") && tested.error.is_none(),
        format!("test_agent: {tested:?}"),
    )?;
    println!("NOTE test_agent latency {} ms", tested.latency_ms);
    // The probe is a separate process: the pool never started C.
    check(
        !gate.events()[before..].iter().any(
            |ev| matches!(&ev.kind, EventKind::AgentRuntime { agent, .. } if agent.as_str() == C),
        ),
        "test_agent went through the agent pool",
    )?;
    let failed = hub.test_agent(AgentId::new(MISSING)).await.map_err(e)?;
    check(
        !failed.ok
            && failed
                .error
                .as_deref()
                .is_some_and(|m| m.contains("install gate-missing")),
        format!("missing test_agent: {failed:?}"),
    )?;
    check(
        matches!(
            hub.test_agent(AgentId::new("nope")).await,
            Err(ApiError::NotFound(_))
        ),
        "unknown agent test not NotFound",
    )?;
    let rows = hub.agents_overview().await.map_err(e)?;
    check(
        rows.iter().any(|r| {
            r.id.as_str() == C
                && r.version.as_deref() == Some("0.1.0")
                && r.runtime == AgentRuntimeState::Stopped
        }),
        "overview does not show the tested version",
    )
}

// ---- 9. memory -------------------------------------------------------------------------------------------

async fn memory_admin(env: &Env, gate: &Gate, data_dir: &Path) -> R<()> {
    let hub = &gate.hub;
    let files = hub.memory_files().await.map_err(e)?;
    let workspace = env.workspace();
    check(
        files.first().map(|f| &f.scope) == Some(&MemoryScope::Global)
            && files.iter().any(|f| {
                f.scope
                    == MemoryScope::Project {
                        workspace_path: workspace.clone(),
                    }
            }),
        format!("memory files: {files:?}"),
    )?;
    let global = MemoryScope::Global;
    let text = "# Gate memory\ngate memory v1\n".to_owned();
    let plan = hub
        .memory_plan(global.clone(), text.clone())
        .await
        .map_err(e)?;
    check(
        !plan.no_op && plan.diff.contains("+gate memory v1"),
        format!("plan diff: {}", plan.diff),
    )?;
    hub.memory_apply(plan).await.map_err(e)?;
    let path = data_dir.join("MEMORY.md");
    check(
        std::fs::read_to_string(&path).unwrap_or_default() == text,
        "global MEMORY.md not written",
    )?;
    let doc = hub.memory_doc(global.clone()).await.map_err(e)?;
    check(doc.exists && doc.content == text, "memory_doc read back")?;
    check(
        gate.event_index(
            |k| matches!(k, EventKind::MemoryChanged { scope } if *scope == MemoryScope::Global),
        )
        .is_some(),
        "no MemoryChanged event",
    )?;
    let same = hub
        .memory_plan(global.clone(), text.clone())
        .await
        .map_err(e)?;
    check(
        same.no_op && same.diff.is_empty(),
        "same text is not a no-op",
    )?;
    // Conflict: the file changes between plan and apply (e.g. an agent's memory_write).
    let stale = hub
        .memory_plan(global.clone(), "stale edit\n".into())
        .await
        .map_err(e)?;
    let external = format!("{text}written by an agent\n");
    std::fs::write(&path, &external).map_err(|err| err.to_string())?;
    check(
        matches!(hub.memory_apply(stale).await, Err(ApiError::Conflict(_))),
        "stale memory plan applied",
    )?;
    check(
        std::fs::read_to_string(&path).unwrap_or_default() == external,
        "conflicting apply touched MEMORY.md",
    )?;
    // Project scope, then a fresh native session receives both files (spec §8.2).
    let project = MemoryScope::Project {
        workspace_path: workspace.clone(),
    };
    let plan = hub
        .memory_plan(project.clone(), "project memory p1\n".into())
        .await
        .map_err(e)?;
    hub.memory_apply(plan).await.map_err(e)?;
    check(
        std::fs::read_to_string(env.workspace.join(".harness").join("MEMORY.md"))
            .unwrap_or_default()
            == "project memory p1\n",
        "project MEMORY.md not written",
    )?;
    let s = gate.session(env, A, "memory").await?;
    let t = gate.send(&s, "memory check", None).await?;
    gate.completed(&s, &t).await?;
    let prompts = env.prompts(A, "memory check");
    check(
        prompts.last().is_some_and(|blocks| {
            let all = blocks.join("\n");
            all.contains("written by an agent") && all.contains("project memory p1")
        }),
        "new native session did not receive the edited memory",
    )?;
    step("memory_admin");
    Ok(())
}

// ---- 10. projects ----------------------------------------------------------------------------------------

async fn create_in(gate: &Gate, path: &Path, title: &str) -> R<SessionId> {
    Ok(gate
        .hub
        .create_session(CreateSession {
            workspace_path: path.to_string_lossy().into_owned(),
            workspace_identity: None,
            title: Some(title.into()),
            agent: Some(AgentId::new(A)),
            worktree: None,
        })
        .await
        .map_err(|err| format!("create_session: {err}"))?
        .id)
}

async fn project(gate: &Gate, path: &Path, removed: bool) -> R<Option<ProjectInfo>> {
    let wanted = path.to_string_lossy().into_owned();
    Ok(gate
        .hub
        .list_projects(ListProjects {
            query: None,
            removed,
        })
        .await
        .map_err(e)?
        .into_iter()
        .find(|p| p.workspace_path == wanted))
}

async fn listed(gate: &Gate, id: &SessionId) -> R<bool> {
    Ok(gate
        .hub
        .list_sessions(ListSessions::default())
        .await
        .map_err(e)?
        .iter()
        .any(|s| &s.id == id))
}

/// Starts a long turn and waits until it streams (so the session is busy).
async fn busy_turn(gate: &Gate, s: &SessionId) -> R<harness_core_api::TurnId> {
    let t = gate.send(s, "[[slow 40]] busy", None).await?;
    gate.wait_turn(
        s,
        &t,
        |v| v.assistant_text.contains("chunk-1"),
        "busy turn streaming",
    )
    .await?;
    Ok(t)
}

fn sessions_changed(gate: &Gate) -> usize {
    gate.events()
        .iter()
        .filter(|ev| matches!(ev.kind, EventKind::SessionsChanged))
        .count()
}

// ---- 11. archive -----------------------------------------------------------------------------------------
