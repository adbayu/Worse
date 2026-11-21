use super::{AgentId, Deserialize, PermissionRequestId, Serialize, TurnId};

/// Lifecycle of a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Queued,
    Running,
    /// Every agent is exhausted; waits for a reset or a manual swap.
    Paused,
    Completed,
    Cancelled,
    Failed,
    /// Was queued/running when the hub stopped unexpectedly; not re-run.
    Interrupted,
}

/// Error classification of an agent failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "camelCase")]
pub enum ErrorClass {
    /// Usage limit / credits exhausted. Triggers failover.
    #[serde(rename_all = "camelCase")]
    QuotaExhausted { resets_at: Option<i64> },
    /// Short rate limit or network error; the user may resend.
    #[serde(rename_all = "camelCase")]
    Transient { retry_after_secs: Option<u64> },
    /// Anything else.
    Fatal,
}

/// One execution of a turn by one agent (a failover produces several).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnAttempt {
    pub attempt: u32,
    pub agent: AgentId,
    /// `running`, `completed`, `cancelled`, `failed`, `quotaExhausted`.
    pub status: String,
    pub partial_text: String,
    pub error: Option<String>,
    pub error_class: Option<ErrorClass>,
}

/// Tool call status (ACP `ToolCallStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// Display copy of a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallView {
    pub tool_call_id: String,
    pub title: String,
    /// ACP `ToolKind` (`read`, `edit`, `execute`, ...).
    pub kind: Option<String>,
    pub status: Option<ToolCallStatus>,
    /// Raw input as JSON text (display only).
    pub raw_input: Option<String>,
    /// Text / diff / terminal content (API 0.5.0; text capped at 64 KB).
    #[serde(default)]
    pub content: Vec<crate::chat::ToolContent>,
    #[serde(default)]
    pub locations: Vec<crate::chat::ToolLocation>,
    /// Raw output as JSON text (capped at 64 KB).
    #[serde(default)]
    pub raw_output: Option<String>,
}

/// Kind of a permission option (ACP `PermissionOptionKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// One answer the agent offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: PermissionOptionKind,
}

/// A pending permission request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequestView {
    pub request_id: PermissionRequestId,
    pub turn_id: TurnId,
    pub agent: AgentId,
    pub tool: ToolCallView,
    pub options: Vec<PermissionOption>,
}

/// How a permission request ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum PermissionDecision {
    #[serde(rename_all = "camelCase")]
    Selected {
        option_id: String,
        kind: PermissionOptionKind,
    },
    /// Stop, failover or agent exit.
    Cancelled,
}

/// One turn of a session snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnView {
    pub id: TurnId,
    pub seq: u32,
    pub user_text: String,
    /// Agent that served (or is serving) the turn; `None` while queued.
    pub agent: Option<AgentId>,
    pub assistant_text: String,
    pub status: TurnStatus,
    pub stop_reason: Option<String>,
    pub error: Option<String>,
    pub attempts: Vec<TurnAttempt>,
    pub tool_calls: Vec<ToolCallView>,
    /// `(request title, decision)` per answered permission request.
    pub permissions: Vec<(String, PermissionDecision)>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    /// Text and tool calls in live order; text segments concatenate to `assistant_text` (API 0.2.0).
    #[serde(default)]
    pub segments: Vec<TurnSegment>,
    /// Attachments and mentions of the user turn (API 0.5.0).
    #[serde(default)]
    pub context: Vec<crate::chat::ContextItem>,
    /// Latest ACP plan of the turn.
    #[serde(default)]
    pub plan: Vec<crate::chat::PlanEntry>,
    /// Token counts from the prompt result.
    #[serde(default)]
    pub usage: Option<crate::chat::TokenUsage>,
    /// Latest context window use reported during the turn.
    #[serde(default)]
    pub context_usage: Option<crate::chat::ContextUsage>,
    /// Set when a scheduled prompt sent the turn.
    #[serde(default)]
    pub origin: Option<crate::chat::TurnOrigin>,
    /// Agent reasoning in stream order (API 0.20.0).
    #[serde(default)]
    pub thoughts: Vec<ThoughtView>,
}

/// One block of agent reasoning within a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThoughtView {
    pub attempt: u32,
    /// Byte length of the attempt's assistant text when the reasoning ended.
    pub text_offset: usize,
    pub text: String,
}

/// One piece of a turn's transcript in display order (spec §15.1 request 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TurnSegment {
    /// Assistant text.
    Text {
        text: String,
    },
    /// A tool call; details in `TurnView.tool_calls` with the same id.
    #[serde(rename_all = "camelCase")]
    Tool {
        tool_call_id: String,
    },
    Steer {
        text: String,
    },
}

/// Changes an agent made to the workspace during one turn (spec §15.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDiff {
    pub turn_id: TurnId,
    /// `false` when no snapshot exists (not a git repository, git missing, snapshots disabled, pruned).
    pub available: bool,
    /// Why the diff is unavailable.
    pub reason: Option<String>,
    pub files: Vec<crate::diff::FileDiff>,
    /// The diff output hit the size cap; `files` is incomplete.
    pub truncated: bool,
    /// The turn's changes were undone (API 0.5.0).
    #[serde(default)]
    pub undone: bool,
}
