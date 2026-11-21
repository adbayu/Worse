use super::{AgentId, ContextItem, Deserialize, Serialize, SessionId, TurnId};

// ---- turn actions ---------------------------------------------------------------------------------

/// `fork_session` request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkSession {
    pub session_id: SessionId,
    /// Copy turns up to and including this one; `None` = all finished turns.
    pub up_to_turn: Option<TurnId>,
    pub title: Option<String>,
}

/// `rewind_session` request: removes `turn_id` and every later turn.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rewind {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    /// Also restore the workspace files the removed turns changed (safe files only).
    pub restore_files: bool,
}

/// `rewind_session` result.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindResult {
    pub removed_turns: u32,
    /// Text of the first removed turn (back into the composer).
    pub user_text: String,
    pub context: Vec<ContextItem>,
    /// Present when `restore_files` was requested.
    pub files: Option<FileRestoreReport>,
}

/// `edit_and_resend` request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditResend {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub text: String,
    /// `None` keeps the old turn's context items.
    pub context: Option<Vec<ContextItem>>,
    pub agent: Option<AgentId>,
    pub restore_files: bool,
}

/// `retry_turn` request (last turn only).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retry {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub agent: Option<AgentId>,
    pub restore_files: bool,
}

/// Direction of a file change operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeDirection {
    /// Back to the state before the turn.
    #[default]
    Undo,
    /// Forward to the state after the turn.
    Reapply,
}

/// What restoring does to one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileAction {
    /// Write the target content.
    Restore,
    /// The path does not exist in the target: delete it.
    Delete,
}

/// One path of a [`FileChangePlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedFile {
    /// Workspace-relative path with `/`.
    pub path: String,
    pub action: FileAction,
    /// The file is still as the turn left it (CHG-003).
    pub safe: bool,
    /// Why it is unsafe.
    pub reason: Option<String>,
}

/// `plan_file_changes` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangePlan {
    pub turn_id: TurnId,
    pub direction: ChangeDirection,
    pub available: bool,
    pub reason: Option<String>,
    pub files: Vec<PlannedFile>,
    /// Every planned file already equals the plan's target (undo: already undone; reapply: already applied).
    /// API 0.5.1, phase-9-parity.md §9.2.5.
    #[serde(default)]
    pub applied: bool,
}

/// `apply_file_changes` request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyFileChanges {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub direction: ChangeDirection,
    /// `None` = every planned path.
    pub paths: Option<Vec<String>>,
    pub include_unsafe: bool,
}

/// What a restore did.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRestoreReport {
    pub applied: Vec<String>,
    /// `(path, reason)`.
    pub skipped: Vec<(String, String)>,
}
