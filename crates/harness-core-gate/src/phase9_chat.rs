//! Phase 9.2 core gate (spec phase-9-parity.md §9.2.4): organize, fork, rewind / edit / retry, file undo, find,
//! context blocks, slash commands, plan / usage / tool content, permission options, command search, read-only
//! import and scheduled prompts; then the §9.2.5 follow-ups (queue ops, pickers, typed failures, applied flag,
//! git ops, projects tree) in [more]. Run through `scripts/e2e/phase9-chat.mjs`, which provides the isolated home
//! (`HOME`, `USERPROFILE`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`). Import roots are fixtures written here; the real
//! `~/.zcode` and `~/.claude` are never read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use harness_agent_config::{AgentKind, ConfigPaths};
use harness_agent_hub::{HubConfig, SharedFileSearch};
use harness_core_api::{
    ActionEntry, AgentId, ApiError, ApplyFileChanges, BlockKind, ChangeDirection, CommandSearch,
    CommandSource, ContextItem, CreateSession, EditResend, EventKind, FileAction, FindField,
    FindInSession, ForkSession, GroupColor, HarnessCore, ImportRequest, ImportScan, ImportSource,
    ListSessions, MentionKind, MentionQuery, NewSchedule, PermissionDecision, PermissionOptionKind,
    Retry, Rewind, ScheduleKind, ScheduleTarget, SearchScope, SendMessage, SessionId,
    SessionSummary, TurnId, TurnStatus, UpdateGroup, UpdateSchedule,
};
use serde_json::json;

use super::{check, step, Env, Gate, A, B, C, R};

mod more;

mod context;
mod import;
mod organize;
mod rewind;
mod schedules;
mod tools;

use context::{context, find};
use import::import;
use organize::{fork, organize};
use rewind::{file_undo, norm, rewind_edit_retry};
use schedules::schedules;
use tools::{background, command_search, commands, permissions, plan_usage_tools};

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

fn api<T>(what: &str, r: Result<T, ApiError>) -> R<T> {
    r.map_err(|e| format!("{what}: {e}"))
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) -> R<()> {
    if let Some(dir) = path.parent() {
        io("mkdir", std::fs::create_dir_all(dir))?;
    }
    io(
        &format!("write {}", path.display()),
        std::fs::write(path, bytes),
    )
}

/// Data dir = `HARNESS_HOME`; workspace, git fixture, import fixtures, agent state and log live next to it.
struct Chat {
    env: Env,
    repo: PathBuf,
    import_root: PathBuf,
}

fn chat_env() -> R<Chat> {
    let mut env = Env::from_env()?;
    let parent = env
        .home
        .parent()
        .ok_or("HARNESS_HOME has no parent")?
        .to_path_buf();
    for key in ["CLAUDE_CONFIG_DIR", "CODEX_HOME"] {
        let dir = std::env::var_os(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .ok_or(format!("{key} must be set (isolated)"))?;
        check(
            dir.starts_with(&parent),
            format!("{key} is not inside the isolated home"),
        )?;
    }
    env.workspace = parent.join("ws92");
    env.state = parent.join("fake-state92");
    env.log = parent.join("fake-agent92.jsonl");
    for dir in [&env.workspace, &env.state] {
        io("mkdir", std::fs::create_dir_all(dir))?;
    }
    let repo = parent.join("repo92");
    let import_root = parent.join("import92");
    Ok(Chat {
        env,
        repo,
        import_root,
    })
}

fn config(chat: &Chat) -> R<HubConfig> {
    let mut config = chat.env.config();
    // fake-a 声明图片与嵌入上下文能力，fake-b 不声明：同一上下文按能力生成不同块。
    if let Some(a) = config.agents.iter_mut().find(|a| a.id.as_str() == A) {
        a.args.extend(["--caps".into(), "image,embedded".into()]);
    }
    config.turn_snapshots = true;
    config.file_search = Some(SharedFileSearch(Arc::new(
        harness_workspace::WorkspaceFileSearch::new(),
    )));
    config.config_paths = Some(
        ConfigPaths::from_env(&chat.env.home, None).map_err(|e| format!("config paths: {e}"))?,
    );
    config
        .agent_kinds
        .insert(AgentId::new(A), AgentKind::ClaudeCode);
    config.import_roots = harness_import::ImportRoots {
        zcode: Some(chat.import_root.join("zcode")),
        claude: Some(chat.import_root.join("claude")),
        codex: None,
    };
    config.schedule_min_interval_secs = 1;
    Ok(config)
}

async fn run() -> R<()> {
    let chat = chat_env()?;
    seed_workspace(&chat)?;
    let gate = super::phase9::start(config(&chat)?).await?;
    organize(&chat.env, &gate).await?;
    fork(&chat.env, &gate).await?;
    let rewound = rewind_edit_retry(&chat.env, &gate).await?;
    file_undo(&chat, &gate).await?;
    find(&chat.env, &gate).await?;
    context(&chat.env, &gate, &rewound).await?;
    commands(&chat.env, &gate).await?;
    background(&chat.env, &gate).await?;
    plan_usage_tools(&chat.env, &gate).await?;
    permissions(&chat.env, &gate).await?;
    command_search(&chat.env, &gate).await?;
    import(&chat, &gate).await?;
    more::run(&chat, &gate).await?;
    schedules(&chat, gate).await
}

// ---- fixtures ---------------------------------------------------------------------------------------------

fn seed_workspace(chat: &Chat) -> R<()> {
    let ws = &chat.env.workspace;
    write(&ws.join("ctx").join("small.txt"), "small text\n")?;
    // PNG 签名即可：门禁只关心 MIME 与大小，不解码图片。
    write(&ws.join("ctx").join("pic.png"), b"\x89PNG\r\n\x1a\nfixture")?;
    write(&ws.join("big.png"), vec![0_u8; 6 * 1024 * 1024])?;
    write(&ws.join("settings-notes.md"), "notes\n")?;
    write(
        &ws.join(".claude").join("commands").join("review.md"),
        "---\ndescription: Workspace review\n---\nReview $ARGUMENTS\n",
    )?;
    write(
        &ws.join(".claude").join("commands").join("ws-only.md"),
        "---\ndescription: Workspace only\n---\nBody\n",
    )?;
    let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .ok_or("CLAUDE_CONFIG_DIR must be set")?;
    write(
        &claude_dir.join("commands").join("user-only.md"),
        "---\ndescription: User only\n---\nBody\n",
    )?;
    write(
        &chat
            .env
            .home
            .join("skills")
            .join("gate-skill")
            .join("SKILL.md"),
        "---\nname: gate-skill\ndescription: Gate skill\n---\nSKILL-BODY-7731\n",
    )?;
    // git 仓库（文件撤销依赖回合快照的 git 树）。
    if chat.repo.exists() {
        io("rm repo", std::fs::remove_dir_all(&chat.repo))?;
    }
    io("mkdir repo", std::fs::create_dir_all(&chat.repo))?;
    super::workspace::git(&chat.repo, &["init", "-q", "-b", "main"])?;
    for (key, value) in [
        ("user.name", "Harness Gate"),
        ("user.email", "gate@example.invalid"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
        ("core.hooksPath", "no-hooks"),
    ] {
        super::workspace::git(&chat.repo, &["config", key, value])?;
    }
    write(&chat.repo.join("README.md"), "hello\n")?;
    super::workspace::git(&chat.repo, &["add", "-A"])?;
    super::workspace::git(&chat.repo, &["commit", "-q", "-m", "initial"])?;
    Ok(())
}

async fn create(gate: &Gate, workspace: &Path, agent: &str, title: &str) -> R<SessionId> {
    let summary = api(
        "create_session",
        gate.hub
            .create_session(CreateSession {
                workspace_path: workspace.to_string_lossy().into_owned(),
                workspace_identity: None,
                title: Some(title.into()),
                agent: Some(AgentId::new(agent)),
                worktree: None,
            })
            .await,
    )?;
    Ok(summary.id)
}

async fn send_ctx(
    gate: &Gate,
    session: &SessionId,
    text: &str,
    context: Vec<ContextItem>,
) -> Result<TurnId, ApiError> {
    gate.hub
        .send_message(SendMessage {
            session_id: session.clone(),
            text: text.into(),
            agent: None,
            client_message_id: None,
            context,
        })
        .await
        .map(|a| a.turn_id)
}

async fn run_turn(gate: &Gate, session: &SessionId, text: &str) -> R<TurnId> {
    let turn = gate.send(session, text, None).await?;
    gate.completed(session, &turn).await?;
    Ok(turn)
}

async fn summary_of(gate: &Gate, session: &SessionId) -> R<SessionSummary> {
    let list = api(
        "list_sessions",
        gate.hub.list_sessions(ListSessions::default()).await,
    )?;
    list.into_iter()
        .find(|s| &s.id == session)
        .ok_or_else(|| format!("session {session} not listed"))
}

async fn turns(gate: &Gate, session: &SessionId) -> R<Vec<harness_core_api::TurnView>> {
    Ok(api("open_session", gate.hub.open_session(session.clone()).await)?.turns)
}

fn saw(gate: &Gate, find: impl Fn(&EventKind) -> bool) -> bool {
    gate.event_index(find).is_some()
}

// ---- 1. organize ------------------------------------------------------------------------------------------

// ---- 3. rewind / edit / retry -----------------------------------------------------------------------------

// ---- 5. find ----------------------------------------------------------------------------------------------

// ---- 7. commands ------------------------------------------------------------------------------------------

// ---- 11. import -------------------------------------------------------------------------------------------

// ---- 12. schedules ----------------------------------------------------------------------------------------
