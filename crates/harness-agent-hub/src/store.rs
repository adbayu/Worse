//! SQLite session store (spec §5). Single writer: every call goes through [`Store::run`], which serializes
//! on one connection inside `spawn_blocking`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use harness_core_api::{
    AgentId, ErrorClass, PermissionDecision, QuotaState, SessionId, SessionSummary, ToolCallView,
    TurnAttempt, TurnId, TurnSegment, TurnStatus, TurnView,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::error::HubError;

mod archive;
mod handles;
mod sessions;
mod shape;
mod snapshots;
mod turns;

pub(crate) use archive::{
    archived_of_project, delete_sessions, filter_archived, list_archived, list_projects,
    rows_of_session, session_ids_of_project, set_archived, set_project_removed,
};
use archive::{ensure_archive_column, NOT_REMOVED};
pub(crate) use handles::{
    get_setting, load_handles, load_prefs, load_quota, save_pref, save_quota, set_setting,
    upsert_handle,
};
use sessions::session_from_row;
pub(crate) use sessions::{insert_session, list_sessions, load_session, summary, update_session};
pub(crate) use shape::{agents_used, build_segments, merge_tool, ToolMark};
use shape::{apply_event, slim_turn, trim_turn, FULL_TURNS};
pub use snapshots::SnapshotRow;
pub(crate) use snapshots::{load_snapshot, set_snapshot_after, set_snapshot_before, turn_session};
pub(crate) use turns::{
    append_turn_event, insert_turn, load_turns, load_turns_slim, update_turn, upsert_attempt,
};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions(
  id TEXT PRIMARY KEY, title TEXT NOT NULL, workspace_path TEXT NOT NULL, workspace_key TEXT NOT NULL,
  active_agent TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS sessions_ws ON sessions(workspace_key, updated_at);
CREATE TABLE IF NOT EXISTS turns(
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL, seq INTEGER NOT NULL, client_message_id TEXT,
  user_text TEXT NOT NULL, agent_id TEXT, assistant_text TEXT NOT NULL DEFAULT '', status TEXT NOT NULL,
  stop_reason TEXT, error TEXT, created_at INTEGER NOT NULL, finished_at INTEGER,
  UNIQUE(session_id, seq));
CREATE TABLE IF NOT EXISTS turn_attempts(
  turn_id TEXT NOT NULL, attempt INTEGER NOT NULL, agent_id TEXT NOT NULL, status TEXT NOT NULL,
  partial_text TEXT NOT NULL DEFAULT '', error_class TEXT, error TEXT, resets_at INTEGER,
  PRIMARY KEY(turn_id, attempt));
-- 修复说明：原 UNIQUE(turn_id, agent_id) 会拒绝暂停后恢复轮次中同一 agent 的新尝试，写入失败只记 warn，
-- 导致该尝试未落盘。幂等规则（同一轮 failover 内每个 agent 至多一次）由会话 actor 这一唯一所有者执行（spec §6.4）。
CREATE TABLE IF NOT EXISTS turn_events(
  turn_id TEXT NOT NULL, idx INTEGER NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL,
  PRIMARY KEY(turn_id, idx));
CREATE TABLE IF NOT EXISTS agent_handles(
  session_id TEXT NOT NULL, agent_id TEXT NOT NULL, native_session_id TEXT NOT NULL, last_seq INTEGER NOT NULL,
  PRIMARY KEY(session_id, agent_id));
CREATE TABLE IF NOT EXISTS agent_quota(
  agent_id TEXT PRIMARY KEY, state TEXT NOT NULL, resets_at INTEGER, reason TEXT, updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- Round 8.2 (spec §15.2): git tree snapshots around a turn. Additive, so user_version stays 1.
CREATE TABLE IF NOT EXISTS turn_snapshots(
  turn_id TEXT PRIMARY KEY, before_tree TEXT, after_tree TEXT, error TEXT);
-- Phase 9.1 (phase-9-parity.md §9.1.5): mode/model/config choices per (session, agent). Additive.
CREATE TABLE IF NOT EXISTS session_prefs(
  session_id TEXT NOT NULL, agent_id TEXT NOT NULL, kind TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
  PRIMARY KEY(session_id, agent_id, kind, key));
-- Phase 9.1 (§9.1.5a): removed projects (undo list). Additive; sessions.archived_at is added by
-- ensure_archive_column.
CREATE TABLE IF NOT EXISTS projects(
  workspace_key TEXT PRIMARY KEY, workspace_path TEXT NOT NULL, removed_at INTEGER);
";

/// Persisted session row.
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: SessionId,
    pub title: String,
    pub workspace_path: String,
    pub workspace_key: String,
    pub active_agent: AgentId,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Persisted native sub-session handle (cache).
#[derive(Debug, Clone)]
pub struct HandleRow {
    pub agent: AgentId,
    pub native: String,
    pub last_seq: u32,
}

/// Cloneable store handle.
#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
}

impl Store {
    /// Opens (and migrates) the store; turns left queued/running by a crash become `interrupted`.
    ///
    /// # Errors
    ///
    /// SQLite or I/O errors.
    pub fn open(path: &Path) -> Result<Self, HubError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.execute_batch(SCHEMA)?;
        ensure_archive_column(&connection)?;
        crate::store_chat::migrate(&connection)?;
        crate::journal::migrate(&connection)?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 0 {
            connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if version > SCHEMA_VERSION {
            return Err(HubError::Config(format!(
                "store schema {version} is newer than this build ({SCHEMA_VERSION})"
            )));
        }
        connection.execute(
            "UPDATE turns SET status='interrupted' WHERE status IN ('queued','running','paused')",
            [],
        )?;
        connection.execute(
            "UPDATE turn_attempts SET status='interrupted' WHERE status='running'",
            [],
        )?;
        crate::journal::mark_orphans(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Runs `f` on the connection in a blocking task.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, or a poisoned/cancelled task.
    pub async fn run<T, F>(&self, f: F) -> Result<T, HubError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, HubError> + Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let guard = connection
                .lock()
                .map_err(|_| HubError::Config("store lock poisoned".into()))?;
            f(&guard)
        })
        .await
        .map_err(|e| HubError::Config(format!("store task failed: {e}")))?
    }
}

pub(crate) fn status_str(status: TurnStatus) -> &'static str {
    match status {
        TurnStatus::Queued => "queued",
        TurnStatus::Running => "running",
        TurnStatus::Paused => "paused",
        TurnStatus::Completed => "completed",
        TurnStatus::Cancelled => "cancelled",
        TurnStatus::Failed => "failed",
        TurnStatus::Interrupted => "interrupted",
    }
}

fn parse_status(s: &str) -> TurnStatus {
    match s {
        "queued" => TurnStatus::Queued,
        "running" => TurnStatus::Running,
        "paused" => TurnStatus::Paused,
        "completed" => TurnStatus::Completed,
        "cancelled" => TurnStatus::Cancelled,
        "failed" => TurnStatus::Failed,
        _ => TurnStatus::Interrupted,
    }
}
