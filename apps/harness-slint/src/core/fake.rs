//! Round-1 local fake core: real session/queue/failover/permission/persistence logic, scripted fake agents.
//!
//! Agents are scripted (no CLI is spawned). `HARNESS_FAKE_AGENTS` lists the available agent ids
//! (default `claude-code,codex`; `agy` then shows as not installed). Prompt keywords drive the script:
//! `permission` asks for a write permission, `quota` exhausts the agent mid-stream (failover),
//! `slow` streams a long reply slowly (to test Stop). Anything else: a tool call + a markdown reply.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use harness_settings::SettingsStore;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};

use super::prefs;

mod admin;
mod agent;
mod agent_msgs;
mod chat;
mod handle;
mod queue;
mod store;
mod turn;

use super::{
    AgentId, AgentState, AgentStatus, Command, Event, Item, ItemId, Notice, QueuedMessage,
    SessionId, SessionSummary, Settings, ToolStatus, TurnStatus,
};

use agent::{demo_subagents, fake_agent};

const KNOWN_AGENTS: [(&str, &str); 3] = [
    ("claude-code", "Claude Code"),
    ("codex", "Codex"),
    ("agy", "agy"),
];

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionFile {
    summary: SessionSummary,
    sticky: AgentId,
    items: Vec<Item>,
}

struct Run {
    id: u64,
    agent: AgentId,
    prompt: String,
    assistant: Option<ItemId>,
    tool: Option<ItemId>,
    cancel: watch::Sender<bool>,
    permission: Option<(u64, oneshot::Sender<bool>)>,
}

struct Session {
    summary: SessionSummary,
    items: Vec<(ItemId, Item)>,
    next_item: ItemId,
    sticky: AgentId,
    queue: VecDeque<QueuedMessage>,
    next_queue: u64,
    run: Option<Run>,
    paused: bool,
}

impl Session {
    fn queue_id(&mut self) -> String {
        let id = format!("q{}", self.next_queue);
        self.next_queue += 1;
        id
    }

    fn push(&mut self, item: Item) -> ItemId {
        let id = self.next_item;
        self.next_item += 1;
        self.items.push((id, item));
        id
    }

    fn item_mut(&mut self, id: ItemId) -> Option<&mut Item> {
        self.items
            .iter_mut()
            .find(|(item_id, _)| *item_id == id)
            .map(|(_, item)| item)
    }
}

enum AgentOut {
    Delta(String),
    ToolStart(String),
    ToolEnd(ToolStatus),
    Permission {
        title: String,
        detail: String,
        reply: oneshot::Sender<bool>,
    },
    Exhausted {
        reset: Option<String>,
    },
    Finished {
        cancelled: bool,
    },
}

struct AgentMsg {
    run: u64,
    session: SessionId,
    out: AgentOut,
}

struct Job {
    run: u64,
    session: SessionId,
    agent: AgentId,
    name: String,
    prompt: String,
    continuation: bool,
}

/// What the fake core persists itself (only the fallback order; theme/language belong to the settings store).
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct FakeState {
    fallback_order: Option<Vec<AgentId>>,
}

/// 修复说明：fake core 原先把整个 Settings 写进 `<home>/settings.json`，而该文件现在归 harness-settings
/// 存储所有（唯一写者，phase-9-parity.md §9.1.2）；每次改主题/语言/fallback order 都会覆盖整个文件并丢掉
/// 存储的键。改为仅把 fallback order 存到 `fake-core.json`，主题/语言经存储读写。
const FAKE_STATE_FILE: &str = "fake-core.json";

pub struct FakeCore {
    sessions_dir: PathBuf,
    settings_file: PathBuf,
    ev: mpsc::UnboundedSender<Event>,
    agent_tx: mpsc::UnboundedSender<AgentMsg>,
    settings: Settings,
    agents: Vec<AgentStatus>,
    summaries: Vec<SessionSummary>,
    open: HashMap<SessionId, Session>,
    next_run: u64,
    next_request: u64,
    admin: admin::FakeAdmin,
    /// Mode / model / effort choices per session (R13).
    prefs: HashMap<SessionId, queue::FakePrefs>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

async fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await
}

fn emit(ev: &mpsc::UnboundedSender<Event>, event: Event) {
    // UI 已退出时发送失败是正常的关停路径，忽略即可。
    let _ = ev.send(event);
}

/// The four ACP answers the fake agent offers (`ChoosePermission` maps `deny*` to a rejection).
fn fake_choices() -> Vec<super::PermissionChoice> {
    use harness_core_api::PermissionOptionKind as Kind;
    [
        ("allow", "Allow", Kind::AllowOnce),
        ("allow-always", "Always allow", Kind::AllowAlways),
        ("deny", "Deny", Kind::RejectOnce),
        ("deny-always", "Always deny", Kind::RejectAlways),
    ]
    .into_iter()
    .map(|(id, name, kind)| super::PermissionChoice {
        id: id.into(),
        name: name.into(),
        kind,
    })
    .collect()
}

fn agent_name(id: &str) -> String {
    KNOWN_AGENTS
        .iter()
        .find(|(known, _)| *known == id)
        .map_or_else(|| id.to_string(), |(_, name)| (*name).to_string())
}

/// Runs the fake core until `Command::Shutdown` or the command channel closes.
pub async fn run(
    home: PathBuf,
    store: Arc<SettingsStore>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    ev: mpsc::UnboundedSender<Event>,
) {
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel();
    let mut store_rx = store.subscribe();
    let mut core = FakeCore::load(&home, &store, ev, agent_tx).await;
    core.emit_startup();
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Shutdown) => break,
                Some(command) => core.handle(command).await,
            },
            Some(message) = agent_rx.recv() => core.on_agent(message).await,
            changed = store_rx.changed() => {
                if changed.is_err() {
                    continue;
                }
                let snapshot = store_rx.borrow_and_update().clone();
                core.on_store(&snapshot);
            }
        }
    }
    core.shutdown().await;
}
