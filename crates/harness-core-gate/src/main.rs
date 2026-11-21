//! Phase 8 core gate (spec phase-8-core.md §13). Run through `scripts/e2e/phase8-core.mjs`, which provides an
//! isolated `HARNESS_HOME`, measures idle RSS and verifies that every agent process is gone.
//!
//! Protocol on stdout: `STEP <name> OK`, `AGENT_PID <pid>`, `IDLE_RSS_CHECK <pid>` (then one stdin line is
//! awaited), finally `GATE_OK` or `GATE_FAIL <reason>`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use harness_agent_hub::agents::{AgentSpec, ErrorDialect, Transport};
use harness_agent_hub::{Hub, HubConfig};
use harness_core_api::{
    AgentId, AgentRuntimeState, CreateSession, EventKind, HarnessCore, HubEvent, MemoryScope,
    MemoryWriteMode, PauseReason, PermissionDecision, QuotaState, RecvError, SendMessage,
    SessionId, SwitchReason, TurnId, TurnStatus, TurnView,
};
use serde_json::Value;
use tokio::io::AsyncBufReadExt;

mod phase9;
mod phase9_admin;
mod phase9_chat;
mod phase9_coord;
mod phase9_delegate;
mod phase9_providers;
mod smoke;
mod workspace;

mod scenario_quota;
mod scenario_turns;

use scenario_quota::{exhaustion, idle, pid_alive, restart, sticky_and_swap};
use scenario_turns::{agent_switch, basics, failover, tools_and_memory};

type R<T> = Result<T, String>;

const A: &str = "fake-a";
const B: &str = "fake-b";
const C: &str = "fake-c";
const IDLE: Duration = Duration::from_secs(2);
const TURN_TIMEOUT: Duration = Duration::from_secs(30);

fn check(condition: bool, message: impl Into<String>) -> R<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn step(name: &str) {
    println!("STEP {name} OK");
}

struct Env {
    home: PathBuf,
    workspace: PathBuf,
    state: PathBuf,
    log: PathBuf,
    bin_dir: PathBuf,
}

impl Env {
    fn from_env() -> R<Self> {
        let home = std::env::var_os("HARNESS_HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or("HARNESS_HOME must be set (isolated home)")?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let bin_dir = exe.parent().ok_or("exe has no parent")?.to_path_buf();
        let env = Self {
            workspace: home.join("workspace"),
            state: home.join("fake-state"),
            log: home.join("fake-agent.jsonl"),
            bin_dir,
            home,
        };
        for dir in [&env.home, &env.workspace, &env.state] {
            std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        }
        Ok(env)
    }

    fn exe(&self, name: &str) -> PathBuf {
        self.bin_dir.join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        })
    }

    fn config(&self) -> HubConfig {
        let agent = |id: &str, name: &str| AgentSpec {
            id: AgentId::new(id),
            display_name: name.to_owned(),
            program: self
                .exe("harness-fake-agent")
                .to_string_lossy()
                .into_owned(),
            args: vec!["--name".into(), id.to_owned()],
            env: vec![
                (
                    "FAKE_AGENT_STATE".into(),
                    self.state.to_string_lossy().into_owned(),
                ),
                (
                    "FAKE_AGENT_LOG".into(),
                    self.log.to_string_lossy().into_owned(),
                ),
            ],
            install_hint: "build harness-fake-agent".into(),
            transport: Transport::Acp,
            dialect: ErrorDialect::Generic,
        };
        let agents = vec![agent(A, "Fake A"), agent(B, "Fake B"), agent(C, "Fake C")];
        HubConfig {
            home: self.home.clone(),
            default_fallback_order: agents.iter().map(|a| a.id.clone()).collect(),
            key_pools: Vec::new(),
            rotate_keys: true,
            agents,
            agent_idle_timeout: IDLE,
            helpers: harness_agent_hub::helpers::HelperLaunch::Separate {
                memory_mcp: self.exe("harness-memory-mcp"),
                api_agent: self.exe("harness-api-agent"),
            },
            event_capacity: 8192,
            load_agents_file: false,
            turn_snapshots: true,
            agent_defaults: std::collections::HashMap::new(),
            agent_kinds: std::collections::HashMap::new(),
            auth_probe: harness_agent_hub::SharedAuthProbe::default(),
            file_search: None,
            // 门禁绝不指向真实 CLI 配置或历史目录：只用夹具（phase9-chat 自行设置）。
            config_paths: None,
            import_roots: harness_import::ImportRoots::default(),
            schedule_min_interval_secs: 1,
            agy_creds: harness_agent_hub::SharedAgyCredStore::default(),
            delegation: harness_agent_hub::DelegationPolicy::default(),
            fact_check: harness_agent_hub::FactCheckPolicy::default(),
            auto_update: {
                let policy = harness_agent_hub::AutoUpdatePolicy::default();
                policy.set_on(false);
                policy
            },
            disabled_agents: harness_agent_hub::DisabledAgents::default(),
            agent_selector: harness_agent_hub::selector::SharedAgentSelector::default(),
        }
    }

    fn workspace(&self) -> String {
        self.workspace.to_string_lossy().into_owned()
    }

    fn log_entries(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Prompt blocks every time `agent` received a prompt whose user message (last block) is `user`.
    fn prompts(&self, agent: &str, user: &str) -> Vec<Vec<String>> {
        self.log_entries()
            .into_iter()
            .filter(|e| e["event"] == "prompt" && e["agent"] == agent)
            .filter_map(|e| {
                let blocks: Vec<String> = e["blocks"]
                    .as_array()?
                    .iter()
                    .filter_map(|b| b.as_str().map(str::to_owned))
                    .collect();
                (blocks.last().map(String::as_str) == Some(user)).then_some(blocks)
            })
            .collect()
    }

    fn set_exhausted(&self, agent: &str, resets_at: i64) -> R<()> {
        std::fs::write(
            self.state.join(format!("{agent}.exhausted")),
            resets_at.to_string(),
        )
        .map_err(|e| e.to_string())
    }

    fn clear_exhausted(&self, agent: &str) {
        let _ = std::fs::remove_file(self.state.join(format!("{agent}.exhausted")));
    }
}

/// Live hub + event recorder.
struct Gate {
    hub: Arc<Hub>,
    events: Arc<Mutex<Vec<HubEvent>>>,
}

impl Gate {
    async fn start(env: &Env) -> R<Self> {
        let hub = Hub::start(env.config()).await.map_err(|e| e.to_string())?;
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut stream = hub.subscribe();
        let sink = Arc::clone(&events);
        tokio::spawn(async move {
            loop {
                match stream.recv().await {
                    Ok(event) => {
                        if let EventKind::AgentRuntime { pid: Some(pid), .. } = &event.kind {
                            println!("AGENT_PID {pid}");
                        }
                        if let Ok(mut all) = sink.lock() {
                            all.push(event);
                        }
                    }
                    Err(RecvError::Lagged(n)) => println!("NOTE events lagged by {n}"),
                    Err(RecvError::Closed) => break,
                }
            }
        });
        Ok(Self { hub, events })
    }

    fn events(&self) -> Vec<HubEvent> {
        self.events.lock().map(|e| e.clone()).unwrap_or_default()
    }

    async fn session(&self, env: &Env, agent: &str, title: &str) -> R<SessionId> {
        let summary = self
            .hub
            .create_session(CreateSession {
                workspace_path: env.workspace(),
                workspace_identity: None,
                title: Some(title.into()),
                agent: Some(AgentId::new(agent)),
                worktree: None,
            })
            .await
            .map_err(|e| format!("create_session: {e}"))?;
        Ok(summary.id)
    }

    async fn send(&self, session: &SessionId, text: &str, agent: Option<&str>) -> R<TurnId> {
        let accepted = self
            .hub
            .send_message(SendMessage {
                session_id: session.clone(),
                text: text.into(),
                agent: agent.map(AgentId::new),
                client_message_id: None,
                context: Vec::new(),
            })
            .await
            .map_err(|e| format!("send_message: {e}"))?;
        Ok(accepted.turn_id)
    }

    async fn turn(&self, session: &SessionId, turn: &TurnId) -> R<TurnView> {
        let detail = self
            .hub
            .open_session(session.clone())
            .await
            .map_err(|e| format!("open_session: {e}"))?;
        detail
            .turns
            .into_iter()
            .find(|t| &t.id == turn)
            .ok_or_else(|| format!("turn {turn} missing"))
    }

    async fn wait_turn(
        &self,
        session: &SessionId,
        turn: &TurnId,
        done: impl Fn(&TurnView) -> bool,
        what: &str,
    ) -> R<TurnView> {
        let deadline = Instant::now() + TURN_TIMEOUT;
        loop {
            let view = self.turn(session, turn).await?;
            if done(&view) {
                return Ok(view);
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "timeout waiting for {what}: status {:?}, text {:?}",
                    view.status, view.assistant_text
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn finished(&self, session: &SessionId, turn: &TurnId) -> R<TurnView> {
        self.wait_turn(
            session,
            turn,
            |t| {
                matches!(
                    t.status,
                    TurnStatus::Completed | TurnStatus::Cancelled | TurnStatus::Failed
                )
            },
            "turn end",
        )
        .await
    }

    async fn completed(&self, session: &SessionId, turn: &TurnId) -> R<TurnView> {
        let view = self.finished(session, turn).await?;
        check(
            view.status == TurnStatus::Completed,
            format!("turn {turn} ended {:?} ({:?})", view.status, view.error),
        )?;
        Ok(view)
    }

    async fn wait_event(&self, what: &str, find: impl Fn(&HubEvent) -> bool) -> R<HubEvent> {
        let deadline = Instant::now() + TURN_TIMEOUT;
        loop {
            if let Some(event) = self.events().into_iter().find(|e| find(e)) {
                return Ok(event);
            }
            if Instant::now() > deadline {
                return Err(format!("timeout waiting for event: {what}"));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn event_index(&self, find: impl Fn(&EventKind) -> bool) -> Option<usize> {
        self.events().iter().position(|e| find(&e.kind))
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("smoke") {
        return smoke::run(&args[1..]).await;
    }
    if args.first().map(String::as_str) == Some("phase9-settings") {
        return phase9::main().await;
    }
    if args.first().map(String::as_str) == Some("phase9-chat") {
        return phase9_chat::main().await;
    }
    match run().await {
        Ok(()) => {
            println!("GATE_OK");
            std::process::ExitCode::SUCCESS
        }
        Err(reason) => {
            println!("GATE_FAIL {reason}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> R<()> {
    let env = Env::from_env()?;
    // 隔离 home 位于 ZCode 仓库内（Worse/.work/tmp）；没有 ceiling 时 git 会向上找到用户仓库，快照会写入其对象库。
    check(
        std::env::var_os("GIT_CEILING_DIRECTORIES").is_some_and(|v| !v.is_empty()),
        "GIT_CEILING_DIRECTORIES must be set (the isolated home is inside a repository)",
    )?;
    let gate = Gate::start(&env).await?;
    let result = scenarios(&env, gate).await;
    // Never leave fake agents behind, whatever happened.
    for agent in [A, B, C] {
        env.clear_exhausted(agent);
    }
    result
}

async fn scenarios(env: &Env, gate: Gate) -> R<()> {
    let s1 = basics(env, &gate).await?;
    tools_and_memory(env, &gate, &s1).await?;
    workspace::run(env, &gate).await?;
    let s2 = agent_switch(env, &gate).await?;
    let s3 = failover(env, &gate).await?;
    sticky_and_swap(env, &gate, &s3).await?;
    exhaustion(env, &gate, &s3).await?;
    idle(&gate).await?;
    restart(env, gate, &s2, &s3).await
}
