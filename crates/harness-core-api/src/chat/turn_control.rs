use super::{Deserialize, Serialize, TurnId};

// ---- queue edit / steer (API 0.5.1, phase-9-parity.md §9.2.5 R12) -----------------------------------

/// One queued turn in admission order (`EventKind::QueueChanged`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedTurn {
    pub turn_id: TurnId,
    pub user_text: String,
}

/// How a steer was delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SteerMode {
    /// A new turn at the head of the queue (runs right after the current one, or now when idle). ACP v1 has no
    /// mid-turn input, so this is the only mode today.
    Queued,
    /// Reserved: injected into the running turn.
    Injected,
}

/// `steer` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerAccepted {
    pub turn_id: TurnId,
    pub seq: u32,
    pub mode: SteerMode,
}

// ---- typed failures (API 0.5.1, phase-9-parity.md §9.2.5 R20) ----------------------------------------

/// What kind of failure ended a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureKind {
    /// The agent CLI needs a login (ACP `auth_required`).
    Auth,
    /// The agent process exited during the turn.
    ProcessExited,
    /// Spawn, `initialize` or `session/new` / `session/load` failed before the prompt.
    InitFailed,
    /// Rate limit or network error.
    Transient,
    Other,
}

/// What the user can do about a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "hint", rename_all = "camelCase")]
pub enum RetryHint {
    /// Retry now (the port's `RetryInit { session }` re-runs the turn).
    Retry,
    /// Retry after a delay (transient).
    #[serde(rename_all = "camelCase")]
    After { secs: Option<u64> },
    /// Log in to the agent CLI first, then retry.
    Login,
    /// Retrying will not help.
    No,
}

/// A typed turn failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
    /// JSON-RPC code and `data`, at most 4 KB.
    pub details: Option<String>,
    /// Tail of the agent's stderr, when kept.
    pub trace: Option<String>,
    pub retry: RetryHint,
}
