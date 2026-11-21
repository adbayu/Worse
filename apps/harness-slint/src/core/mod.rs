//! Core port used by the UI (spec `docs/specs/harness/phase-8-ui.md` §3, §8).
//!
//! The core owns sessions, transcript items, sticky agent, fallback order, agent availability, the run queue,
//! permissions and settings persistence. The UI only sends [`Command`]s and mirrors [`Event`]s.
//! [`hub`] adapts the port onto `harness_core_api::HarnessCore` (the real agent hub, default);
//! [`fake`] is an in-process scripted core for UI work without agent binaries (`HARNESS_UI_CORE=fake`).

mod commands;
mod events;
pub mod fake;
mod fleet;
pub mod hub;
mod prefs;
mod types;

pub use commands::{
    AccountsCall, ChatCall, Command, FindMatch, ProvidersCall, Reply, SettingsCall, SetupCall,
};
pub use events::{Event, PermissionChoice};
pub use fleet::FleetSession;
pub use types::{
    AgentId, AgentState, AgentStatus, Item, ItemId, Notice, QueuedMessage, SessionId,
    SessionSummary, Settings, ThemePref, ToolStatus, TurnStatus,
};

use std::path::PathBuf;

use harness_core_api as api;
use serde::{Deserialize, Serialize};
