//! Chat and workspace parity types (API 0.5.0, spec `phase-9-parity.md` §9.2.1).

use serde::{Deserialize, Serialize};

use crate::types::{AgentId, SessionId, SessionSummary, TurnId};
use crate::workspace::FileHit;
use crate::{ApiResult, BoxFuture};

mod command_center;
mod import;
mod schedule;
mod turn_actions;
mod turn_control;
mod usage;

pub use command_center::{
    ActionEntry, CommandSearch, CommandSearchResult, FileSearchSource, ScoredId, SearchScope,
    SessionHit,
};
pub use import::{ImportCandidate, ImportReport, ImportRequest, ImportScan, ImportSource};
pub use schedule::{
    NewSchedule, ScheduleKind, ScheduleRun, ScheduleTarget, ScheduledPrompt, UpdateSchedule,
};
pub use turn_actions::{
    ApplyFileChanges, ChangeDirection, EditResend, FileAction, FileChangePlan, FileRestoreReport,
    ForkSession, PlannedFile, Retry, Rewind, RewindResult,
};
pub use turn_control::{Failure, FailureKind, QueuedTurn, RetryHint, SteerAccepted, SteerMode};
pub use usage::{ContextUsage, Cost, PlanEntry, TokenUsage, ToolContent, ToolLocation, TurnOrigin};

// ---- session organisation -------------------------------------------------------------------------

/// Where a forked session came from (TL-016).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkOrigin {
    pub session_id: SessionId,
    /// Last copied turn of the source.
    pub turn_seq: u32,
}

/// The seven sidebar group colours (GRP-002); the UI maps them to theme tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GroupColor {
    #[default]
    Gray,
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl GroupColor {
    /// Stable storage name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gray => "gray",
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Purple => "purple",
        }
    }

    /// Parses a storage name (unknown → gray).
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "red" => Self::Red,
            "orange" => Self::Orange,
            "yellow" => Self::Yellow,
            "green" => Self::Green,
            "blue" => Self::Blue,
            "purple" => Self::Purple,
            _ => Self::Gray,
        }
    }

    /// All colours in picker order.
    pub const ALL: [Self; 7] = [
        Self::Gray,
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Purple,
    ];
}

/// A sidebar group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionGroup {
    pub id: String,
    pub name: String,
    pub color: GroupColor,
    pub position: i64,
}

/// `update_group` request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateGroup {
    pub id: String,
    pub name: Option<String>,
    pub color: Option<GroupColor>,
}

// ---- find ------------------------------------------------------------------------------------------

/// `find_in_session` request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindInSession {
    pub session_id: SessionId,
    pub query: String,
    pub case_sensitive: bool,
    /// 0 → 200, max 500.
    pub limit: u32,
}

/// Field a [`FindHit`] is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FindField {
    User,
    Assistant,
    /// Tool call title.
    Tool,
}

/// One match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindHit {
    pub turn_id: TurnId,
    pub seq: u32,
    pub field: FindField,
    /// Tool call id for [`FindField::Tool`].
    pub tool_call_id: Option<String>,
    /// Start and length in chars of that field.
    pub start: u32,
    pub len: u32,
    /// Up to 40 chars around the match.
    pub snippet: String,
}

// ---- context items (attachments and mentions) -----------------------------------------------------

/// Something attached to a user turn (ATT-*, MEN-002/003).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContextItem {
    /// `@file`; absolute or workspace-relative path.
    File { path: String },
    /// `@folder`.
    Folder { path: String },
    /// An image file or pasted image data (base64). Stored items keep only `path`.
    #[serde(rename_all = "camelCase")]
    Image {
        path: Option<String>,
        data_base64: Option<String>,
        mime: String,
    },
    /// Pasted text.
    Text { title: Option<String>, text: String },
    /// `#session` mention.
    #[serde(rename_all = "camelCase")]
    Session { session_id: SessionId },
    /// Deep research mode for this turn: the hub adds the research instructions.
    Research,
    /// Code mode for this turn: the agent runs with its code-mode variant (one script calling many tools).
    CodeMode,
    /// PRD mode for this turn (0.25.0): the hub adds the PRD discovery instructions (interview, spec file,
    /// quality gate, plan-first task list).
    Prd,
    /// Work item context for this turn (0.26.0): the hub adds the Work edition instructions.
    #[serde(rename_all = "camelCase")]
    WorkItem { path: String },
}

impl ContextItem {
    /// Paths of the stored images among `items`.
    #[must_use]
    pub fn image_paths(items: &[Self]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| match item {
                Self::Image {
                    path: Some(path), ..
                } => Some(path.clone()),
                _ => None,
            })
            .collect()
    }
}

/// Limits of context items (spec §9.2.1).
pub mod limits {
    /// Items per turn.
    pub const MAX_ITEMS: usize = 10;
    /// One image.
    pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
    /// Pasted text.
    pub const MAX_PASTED_BYTES: u64 = 1024 * 1024;
    /// All items of a turn.
    pub const MAX_TOTAL_BYTES: u64 = 20 * 1024 * 1024;
    /// Text files up to this size are embedded (when the agent accepts embedded context).
    pub const MAX_EMBED_BYTES: u64 = 256 * 1024;
    /// Transcript of a mentioned session.
    pub const MAX_SESSION_BYTES: usize = 32 * 1024;
    /// Image mime types accepted.
    pub const IMAGE_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
}

/// ACP block a context item becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockKind {
    /// Embedded text resource.
    Resource,
    ResourceLink,
    Image,
    Text,
    /// The agent cannot take it (an image for a text-only agent); a note is sent instead.
    Unsupported,
}

/// `resolve_context` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedItem {
    pub index: u32,
    pub ok: bool,
    pub error: Option<String>,
    pub block_kind: BlockKind,
    pub bytes: u64,
    /// Chip label (file name, session title, …).
    pub label: String,
}

/// Kind of a mention candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MentionKind {
    File,
    Folder,
    Session,
    /// A skill of the active agent (`harness-agent-config`); `item` is its `SKILL.md` (API 0.5.1).
    Skill,
    /// A subagent of the active agent; the UI inserts `insert_text` and adds no chip (API 0.5.1).
    Subagent,
}

/// `mention_candidates` request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MentionQuery {
    pub session_id: SessionId,
    pub query: String,
    /// Empty = all kinds.
    pub kinds: Vec<MentionKind>,
    /// 0 → 20, max 50.
    pub limit: u32,
}

/// One mention popup row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MentionCandidate {
    pub kind: MentionKind,
    pub label: String,
    /// Relative path, session workspace, or skill / subagent description.
    pub detail: String,
    /// For `Subagent` only the definition file, for a preview (the mention itself is `insert_text`).
    pub item: ContextItem,
    /// Text the UI inserts into the draft instead of adding a chip (`@agent-<name>` for subagents).
    #[serde(default)]
    pub insert_text: Option<String>,
}

// ---- slash commands -------------------------------------------------------------------------------

/// Where a slash command comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandSource {
    /// ACP `available_commands_update`.
    Agent,
    /// CLI user config (`~/.claude/commands`, Codex prompts).
    User,
    /// CLI workspace config (`<ws>/.claude/commands`).
    Workspace,
    Harness,
}

/// One slash command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    /// Without the leading `/`.
    pub name: String,
    pub description: String,
    pub input_hint: Option<String>,
    pub source: CommandSource,
    pub path: Option<String>,
}

/// `list_commands` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandList {
    pub agent: AgentId,
    /// The agent advertised commands at least once.
    pub known: bool,
    pub commands: Vec<SlashCommand>,
}
