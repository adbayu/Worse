//! Phase 9.1 core gate (spec phase-9-parity.md §9.1.7): settings store, agent-config round trips, MCP overlay on
//! `session/new`, modes/models/options, data dir relocation. Run through `scripts/e2e/phase9-settings.mjs`, which
//! provides the isolated home (`HOME`, `CODEX_HOME`, `CLAUDE_CONFIG_DIR`) and scans every output and data file for
//! the fixture secrets below.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use harness_agent_config::{
    AgentConfigService, AgentKind, AuthState, Command, Concept, ConfigError, ConfigPaths,
    ConfigTarget, Entry, EntryBody, FileExistenceProbe, Hook, McpServer, McpTransport, Scope,
    Skill, Subagent,
};
use harness_agent_hub::{Hub, HubConfig};
use harness_core_api::{
    AddAccount, AgentDefaults, AgentId, ApiError, AuthPresence, EventKind, HarnessCore,
    ListSessions, SessionId,
};
use harness_settings::relocation::{self, FailPoint};
use harness_settings::{Language, SaveOutcome, SettingsStore, ThemePref};

use super::{check, step, Env, Gate, A, B, R};

mod accounts;
mod concepts;
mod overlay;
mod relocation_step;

use accounts::{accounts_step, settings_roundtrip};
use concepts::{auth, concept_roundtrip, conflict, diff_masking};
use overlay::{modes_models, options_text, overlay_session_new};
use relocation_step::relocation_step;

/// Fixture secrets (the script has the same list and scans for them; none may appear outside the fixtures).
pub const SECRETS: [&str; 6] = [
    "sk-ant-FIXTURE-cred-0001",
    "sk-codex-FIXTURE-auth-0002",
    "ya29.FIXTURE-oauth-0003",
    "sk-ant-FIXTURE-primary-0004",
    "FIXTURE-bearer-0005",
    "FIXTURE-envsecret-0006",
];

pub async fn main() -> std::process::ExitCode {
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

fn io<T>(what: &str, r: std::io::Result<T>) -> R<T> {
    r.map_err(|e| format!("{what}: {e}"))
}

fn cfg<T>(what: &str, r: Result<T, ConfigError>) -> R<T> {
    r.map_err(|e| format!("{what}: {e}"))
}

fn write(path: &Path, text: &str) -> R<()> {
    if let Some(dir) = path.parent() {
        io("mkdir", std::fs::create_dir_all(dir))?;
    }
    io(
        &format!("write {}", path.display()),
        std::fs::write(path, text),
    )
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

struct Layout {
    /// Isolated user home (parent of the data dir).
    home: PathBuf,
    claude_dir: PathBuf,
    codex_home: PathBuf,
    workspace: PathBuf,
}

/// Seeds the isolated home: CLI configs with unknown keys, TOML comments and dummy secrets.
fn seed(env: &Env) -> R<Layout> {
    let home = env
        .home
        .parent()
        .ok_or("HARNESS_HOME has no parent")?
        .to_path_buf();
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let claude_dir = var("CLAUDE_CONFIG_DIR").ok_or("CLAUDE_CONFIG_DIR must be set (isolated)")?;
    let codex_home = var("CODEX_HOME").ok_or("CODEX_HOME must be set (isolated)")?;
    for dir in [&claude_dir, &codex_home] {
        check(
            dir.starts_with(&home),
            format!("{} is not inside the isolated home", dir.display()),
        )?;
    }
    let [s1, s2, s3, s4, s5, s6] = SECRETS;
    write(
        &claude_dir.join(".credentials.json"),
        &format!(r#"{{"claudeAiOauth":{{"accessToken":"{s1}"}}}}"#),
    )?;
    write(
        &claude_dir.join(".claude.json"),
        &format!(
            "{{\n  \"primaryApiKey\": \"{s4}\",\n  \"unknownTop\": 1,\n  \"mcpServers\": {{\n    \"existing\": {{\"command\": \"node\", \"args\": [\"x.js\"], \"customKey\": \"keep\"}}\n  }}\n}}\n"
        ),
    )?;
    write(
        &claude_dir.join("settings.json"),
        "{\n  \"theme\": \"dark\",\n  \"unknownSetting\": true\n}\n",
    )?;
    write(
        &codex_home.join("auth.json"),
        &format!(r#"{{"OPENAI_API_KEY":"{s2}"}}"#),
    )?;
    write(
        &codex_home.join("config.toml"),
        &format!(
            "# keep this comment\nmodel = \"gpt-5\" # inline comment\n\n[mcp_servers.local]\ncommand = \"node\"\nargs = [\"local.js\"]\nenv = {{ API_KEY = \"{s6}\" }}\n\n[mcp_servers.remote]\nurl = \"https://example.invalid/mcp\"\nhttp_headers = {{ Authorization = \"Bearer {s5}\" }}\n"
        ),
    )?;
    write(
        &home.join(".gemini").join("oauth_creds.json"),
        &format!(r#"{{"access_token":"{s3}"}}"#),
    )?;
    let workspace = env.workspace.clone();
    write(
        &workspace.join(".claude").join("settings.json"),
        "{\"wsUnknown\": [1, 2]}\n",
    )?;
    write(&workspace.join("CLAUDE.md"), "# Workspace rules\n")?;
    Ok(Layout {
        home,
        claude_dir,
        codex_home,
        workspace,
    })
}

async fn run() -> R<()> {
    let env = phase9_env()?;
    let layout = seed(&env)?;
    settings_roundtrip(&env).await?;
    let svc = AgentConfigService::new(
        cfg(
            "paths",
            ConfigPaths::from_env(&env.home, Some(layout.workspace.clone())),
        )?,
        Arc::new(FileExistenceProbe),
    );
    concept_roundtrip(&svc, &layout).await?;
    conflict(&svc, &env).await?;
    diff_masking(&svc).await?;
    auth(&svc).await?;
    let session = overlay_session_new(&env, &svc).await?;
    modes_models(&env, &session).await?;
    relocation_step(&env, &layout, &session).await?;
    accounts_step(&env, &layout).await?;
    super::phase9_providers::run(&env, &layout.home).await?;
    super::phase9_delegate::run(&env, &layout.home).await?;
    let data_dir = relocation::resolve_data_dir(&env.home).await.data_dir;
    super::phase9_admin::run(&env, &data_dir).await
}

/// Data dir = `HARNESS_HOME`; workspace, fake-agent state and log live next to it (outside), so relocation moves only
/// Harness data.
fn phase9_env() -> R<Env> {
    let mut env = Env::from_env()?;
    let parent = env
        .home
        .parent()
        .ok_or("HARNESS_HOME has no parent")?
        .to_path_buf();
    env.workspace = parent.join("ws9");
    env.state = parent.join("fake-state9");
    env.log = parent.join("fake-agent9.jsonl");
    for dir in [&env.workspace, &env.state] {
        io("mkdir", std::fs::create_dir_all(dir))?;
    }
    Ok(env)
}

pub(super) fn hub_config(env: &Env, home: &Path, defaults: bool) -> HubConfig {
    let mut config = env.config();
    config.home = home.to_path_buf();
    config.turn_snapshots = false;
    if defaults {
        config.agent_defaults.insert(
            AgentId::new(B),
            AgentDefaults {
                model: Some("fake-large".into()),
                ..AgentDefaults::default()
            },
        );
    }
    config
}

pub(super) async fn start(config: HubConfig) -> R<Gate> {
    let hub = Hub::start(config).await.map_err(|e| e.to_string())?;
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut stream = hub.subscribe();
    let sink = Arc::clone(&events);
    tokio::spawn(async move {
        while let Ok(event) = stream.recv().await {
            if let EventKind::AgentRuntime { pid: Some(pid), .. } = &event.kind {
                println!("AGENT_PID {pid}");
            }
            if let Ok(mut all) = sink.lock() {
                all.push(event);
            }
        }
    });
    Ok(Gate { hub, events })
}

// ---- accounts (§9.6) -------------------------------------------------------------------------------------

// ---- 2. every concept cell -----------------------------------------------------------------------------

// ---- 5. overlay ----------------------------------------------------------------------------------------
