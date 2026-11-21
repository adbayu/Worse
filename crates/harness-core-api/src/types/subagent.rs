use super::{AgentId, Deserialize, Serialize, SessionId};

/// Lifecycle of a subagent (API 0.8.0, §9.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubagentStatus {
    Running,
    NeedsApproval,
    Done,
    Failed,
    Cancelled,
    /// The hub stopped while it was running.
    Interrupted,
}

impl SubagentStatus {
    /// Store / wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::NeedsApproval => "needsApproval",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    /// Inverse of [`SubagentStatus::as_str`]; unknown = interrupted.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "needsApproval" => Self::NeedsApproval,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Interrupted,
        }
    }

    /// Still working (running or waiting for the user).
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::NeedsApproval)
    }
}

/// Permissions of a subagent, always a narrowing of its parent (phase-9-parity.md 9.8.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeView {
    /// Workspace-relative paths or folders; empty means the whole workspace.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Tool title fragments; empty means every tool.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Whether network tools are allowed.
    #[serde(default)]
    pub network: bool,
    /// Unix milliseconds after which the scope denies every tool.
    #[serde(default)]
    pub expires_at: Option<i64>,
}

/// One subagent session of a parent (API 0.8.0, §9.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentView {
    pub session_id: SessionId,
    pub parent_id: SessionId,
    pub agent: AgentId,
    pub title: String,
    pub status: SubagentStatus,
    /// Last tool title or other short live activity ("" when none).
    pub activity: String,
    /// Unix milliseconds of the latest delegated turn.
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// First characters of the latest answer.
    pub result_preview: String,
    /// Permissions of this subagent, narrowed against the parent (9.8.2).
    #[serde(default)]
    pub scope: Option<ScopeView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTask {
    pub id: String,
    pub agent: AgentId,
    pub name: String,
    pub task_type: String,
    pub description: String,
    pub state: String,
    pub summary: String,
    pub output_file: Option<String>,
    pub can_stop: bool,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

impl BackgroundTask {
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.finished_at.is_none()
    }
}
