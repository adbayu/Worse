use super::{Deserialize, Serialize};

// ---- plan, usage, tool content --------------------------------------------------------------------

/// One ACP plan entry (todo row).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    pub content: String,
    /// `high`, `medium`, `low`.
    pub priority: String,
    /// `pending`, `in_progress`, `completed`.
    pub status: String,
}

/// Cost reported by the agent; `amount` is decimal text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    pub amount: String,
    pub currency: String,
}

/// Context window use (ACP `usage_update`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub used: u64,
    pub size: u64,
    pub cost: Option<Cost>,
}

/// Token counts of one prompt (ACP `session/prompt` result `usage`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub total: u64,
    pub thought: Option<u64>,
    pub cached_read: Option<u64>,
    pub cached_write: Option<u64>,
}

/// Tool call content (ACP `ToolCallContent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolContent {
    Text {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Diff {
        path: String,
        old_text: Option<String>,
        new_text: String,
    },
    #[serde(rename_all = "camelCase")]
    Terminal {
        terminal_id: String,
    },
}

/// A file location a tool touches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolLocation {
    pub path: String,
    pub line: Option<u32>,
}

/// Why a turn was sent (TL-014).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnOrigin {
    pub schedule_id: String,
    pub schedule_title: String,
    #[serde(default)]
    pub background: Option<String>,
}
