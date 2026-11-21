use super::{Deserialize, Serialize};

pub type AgentId = String;
pub type SessionId = String;
pub type ItemId = u64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AgentState {
    Available,
    /// CLI not installed / not detected.
    Unavailable,
    /// The CLI is installed, its ACP adapter is not (phase-9-parity.md §9.10).
    AdapterMissing,
    /// Credit or quota exhausted; `reset` is a display time ("10:00") when the agent reported one.
    Exhausted {
        reset: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct AgentStatus {
    pub id: AgentId,
    pub name: String,
    pub state: AgentState,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: SessionId,
    pub title: String,
    pub updated_ms: u64,
    pub agents_used: Vec<AgentId>,
    /// Pinned to the top of the sidebar (hub API 0.5.0).
    #[serde(default)]
    pub pinned: bool,
    /// Sidebar group; `None` = ungrouped.
    #[serde(default)]
    pub group_id: Option<String>,
    /// Sort key inside the sidebar section (larger = higher).
    #[serde(default)]
    pub order: i64,
    #[serde(default)]
    pub unread: bool,
    /// Source session id of a fork.
    #[serde(default)]
    pub forked_from: Option<SessionId>,
    /// Imported read-only from `ZCode` or Claude Code history.
    #[serde(default)]
    pub imported: bool,
    /// Workspace folder of the session (projects tree, phase-9-parity.md §9.2.5).
    #[serde(default)]
    pub workspace_path: String,
    /// Project key (`api::ProjectInfo.workspace_key` of `SettingsCall::ListProjects`); groups sessions under their
    /// project.
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Streaming,
    Done,
    Cancelled,
    /// Cut off by quota exhaustion; the next agent continues.
    Interrupted,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolStatus {
    Running,
    AwaitingPermission,
    Done,
    Denied,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Notice {
    Failover {
        from: AgentId,
        to: AgentId,
        reset: Option<String>,
    },
    Paused {
        reset: Option<String>,
    },
    Earlier {
        count: usize,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Item {
    User {
        text: String,
        /// Turn the agent started on its own when a background task ended (§9.17): shown as one muted line,
        /// not as a user bubble.
        #[serde(default)]
        background: bool,
        /// Paths of the images attached to the message.
        #[serde(default)]
        images: Vec<String>,
    },
    Assistant {
        agent: AgentId,
        text: String,
        status: TurnStatus,
    },
    Tool {
        agent: AgentId,
        title: String,
        status: ToolStatus,
    },
    Notice {
        notice: Notice,
    },
}

/// A message waiting in a session's run queue (not yet sent to an agent); `id` is the core's queue key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedMessage {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ThemePref {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub theme: ThemePref,
    /// "en-US" or "zh-CN".
    pub language: String,
    pub fallback_order: Vec<AgentId>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePref::System,
            language: "en-US".into(),
            fallback_order: vec!["claude-code".into(), "codex".into(), "agy".into()],
        }
    }
}
