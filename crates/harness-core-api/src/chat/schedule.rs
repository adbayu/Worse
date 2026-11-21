use super::{AgentId, Deserialize, Serialize, SessionId, TurnId};

// ---- scheduled prompts ----------------------------------------------------------------------------

/// Where a scheduled prompt goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ScheduleTarget {
    #[serde(rename_all = "camelCase")]
    Session { session_id: SessionId },
    /// A new session per run.
    #[serde(rename_all = "camelCase")]
    NewSession {
        workspace_path: String,
        agent: Option<AgentId>,
    },
}

/// When a scheduled prompt runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ScheduleKind {
    /// Every `seconds` (≥ the hub minimum).
    Interval { seconds: u32 },
    /// Local time; `weekdays` 1=Mon…7=Sun, empty = every day.
    #[serde(rename_all = "camelCase")]
    Daily {
        minute_of_day: u32,
        weekdays: Vec<u8>,
    },
}

/// One scheduled prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledPrompt {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub target: ScheduleTarget,
    pub kind: ScheduleKind,
    pub enabled: bool,
    /// Unix ms; `None` while disabled.
    pub next_run: Option<i64>,
    pub last_run: Option<i64>,
    pub last_result: Option<String>,
    pub created_at: i64,
}

/// `create_schedule` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSchedule {
    pub title: String,
    pub prompt: String,
    pub target: ScheduleTarget,
    pub kind: ScheduleKind,
    pub enabled: bool,
}

/// `update_schedule` request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSchedule {
    pub id: String,
    pub title: Option<String>,
    pub prompt: Option<String>,
    pub target: Option<ScheduleTarget>,
    pub kind: Option<ScheduleKind>,
    pub enabled: Option<bool>,
}

/// `run_schedule_now` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRun {
    pub session_id: SessionId,
    pub turn_id: TurnId,
}
