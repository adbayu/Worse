use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use harness_core_api::{
    AgentId, ApiError, ApiResult, BoardLane, CreateSession, EventKind, GhStatus, HarnessCore,
    InboxSource, InboxSourceInput, InboxStatus, RecvError, SendMessage, SessionId, Ticket,
    TicketStatus, TriggerRecord, TurnId, TurnOutcome, WorktreeRequest, DEFAULT_INBOX_HANDLE,
    DEFAULT_INBOX_INTERVAL, DEFAULT_INBOX_MAX_TURNS, MAX_INBOX_INTERVAL, MAX_INBOX_SOURCES,
    MIN_INBOX_INTERVAL,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Notify;

use crate::error::HubError;
use crate::gh::{gh, git};
use crate::hub::Hub;
use crate::store;
use crate::util::{new_id, now_ms};

mod db;
mod gate;
mod outbound;
mod polling;
mod routing;
mod sources;
mod text;
mod ticket_api;
mod tickets;

pub(crate) use db::sources;
use db::{insert_trigger, is_active_status, load_ticket, save_source, save_ticket, tickets};
use gate::{gate, pending_items, shadow_candidates, shadow_gate};
use text::{
    clip, comment_items, first_message, follow_message, invalid, iso, issue_items, mentions,
    parse_json, router_message, shadow_message, text_of, ticket_branch, valid_repo,
};

pub(crate) const TABLES: &str = "
CREATE TABLE IF NOT EXISTS inbox_sources(id TEXT PRIMARY KEY, json TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS inbox_cursors(source_id TEXT PRIMARY KEY, since INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS inbox_triggers(
  id TEXT PRIMARY KEY, source_id TEXT NOT NULL, event_id TEXT NOT NULL UNIQUE, json TEXT NOT NULL,
  received_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS inbox_tickets(
  id TEXT PRIMARY KEY, source_id TEXT NOT NULL, number INTEGER NOT NULL, session_id TEXT, status TEXT NOT NULL,
  json TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS inbox_tickets_thread ON inbox_tickets(source_id, number);
CREATE INDEX IF NOT EXISTS inbox_tickets_session ON inbox_tickets(session_id);
CREATE TABLE IF NOT EXISTS inbox_pending(
  event_id TEXT PRIMARY KEY, source_id TEXT NOT NULL, json TEXT NOT NULL, state TEXT NOT NULL,
  received_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS inbox_routers(
  source_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, agent TEXT NOT NULL, batches INTEGER NOT NULL);
";

const ENABLED_KEY: &str = "inbox_enabled";
const MARKER: &str = "<!-- harness:";
const SUMMARY_LIMIT: usize = 3000;
const NEW_PER_HOUR: i64 = 5;
const MAX_ACTIVE: i64 = 4;
const GH_CACHE_MS: i64 = 60_000;
const POLL_OVERLAP_MS: i64 = 300_000;
const TRIGGER_TEXT_LIMIT: usize = 20_000;
const BATCH_LIMIT: usize = 10;
const ROUTER_BATCHES: i64 = 20;
const BRIEF_LIMIT: usize = 4000;
const DISMISS_LIMIT: usize = 200;

#[derive(Default)]
pub(crate) struct Inbox {
    wake: Arc<Notify>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    poll_lock: tokio::sync::Mutex<()>,
    dispatch_lock: tokio::sync::Mutex<()>,
    outbound: Mutex<HashSet<String>>,
    pub(crate) gh: Mutex<Option<(GhStatus, String, i64)>>,
}

impl Inbox {
    pub(crate) fn stop(&self) {
        if let Ok(mut slot) = self.task.lock() {
            if let Some(task) = slot.take() {
                task.abort();
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Routers(Mutex<HashMap<String, String>>);

impl Routers {
    pub(crate) fn is_router(&self, session: &SessionId) -> bool {
        self.source_of(session).is_some()
    }

    fn source_of(&self, session: &SessionId) -> Option<String> {
        self.0
            .lock()
            .ok()
            .and_then(|map| map.get(session.as_str()).cloned())
    }

    pub(crate) fn add(&self, session: &SessionId, source_id: &str) {
        if let Ok(mut map) = self.0.lock() {
            map.retain(|_, s| s != source_id);
            map.insert(session.as_str().to_owned(), source_id.to_owned());
        }
    }

    fn remove(&self, session: &str) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(session);
        }
    }
}

#[derive(Default)]
struct Opening {
    agent: Option<AgentId>,
    brief: Option<String>,
    shadow: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    ticket: Ticket,
    #[serde(default)]
    base: String,
    #[serde(default)]
    worktree: String,
    #[serde(default)]
    summary: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    event_id: String,
    kind: String,
    number: u64,
    author: String,
    text: String,
    title: Option<String>,
    created: i64,
    url: String,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    updated: i64,
    #[serde(default)]
    open: bool,
    #[serde(default)]
    pull: bool,
}

enum Verdict {
    Seen,
    Reject(&'static str),
    FollowUp(Box<Stored>),
    New,
}

async fn run_loop(hub: Weak<Hub>, wake: Arc<Notify>) {
    loop {
        let sleep_for = {
            let Some(hub) = hub.upgrade() else { return };
            if hub.check_open().is_err() || !hub.inbox_enabled().await.unwrap_or(false) {
                return;
            }
            let list = hub.inbox_sources().await.unwrap_or_default();
            let now = now_ms();
            for source in list.iter().filter(|s| s.enabled) {
                let due = source
                    .last_poll
                    .map_or(true, |t| now - t >= i64::from(source.interval_secs) * 1000);
                if due {
                    if let Err(error) = hub.poll_source(source.clone()).await {
                        tracing::info!(repo = %source.repo, %error, "inbox poll failed");
                    }
                }
            }
            let list = hub.inbox_sources().await.unwrap_or_default();
            let now = now_ms();
            list.iter()
                .filter(|s| s.enabled)
                .map(|s| s.last_poll.unwrap_or(now) + i64::from(s.interval_secs) * 1000)
                .min()
                .map(|next| {
                    Duration::from_millis(u64::try_from((next - now).max(1000)).unwrap_or(1000))
                })
        };
        match sleep_for {
            Some(duration) => {
                tokio::select! {
                    () = tokio::time::sleep(duration) => {}
                    () = wake.notified() => {}
                }
            }
            None => wake.notified().await,
        }
    }
}

enum Change {
    Working,
    Approval,
    Failed(String),
    Completed(TurnId),
}

pub(crate) async fn track(hub: Weak<Hub>) {
    let Some(mut events) = hub.upgrade().map(|h| h.ctx.events.subscribe()) else {
        return;
    };
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return,
        };
        let Some(session) = event.session_id.clone() else {
            continue;
        };
        if let Some(hub) = hub.upgrade() {
            if let Some(source_id) = hub.ctx.routers.source_of(&session) {
                if matches!(event.kind, EventKind::TurnFinished { .. }) {
                    tokio::spawn(async move { hub.router_finished(source_id).await });
                }
                continue;
            }
        }
        let change = match event.kind {
            EventKind::TurnStarted { .. } | EventKind::PermissionResolved { .. } => Change::Working,
            EventKind::PermissionRequested { .. } => Change::Approval,
            EventKind::TurnFinished {
                turn_id, outcome, ..
            } => match outcome {
                TurnOutcome::Completed { .. } => Change::Completed(turn_id),
                TurnOutcome::Cancelled => Change::Failed("stopped".into()),
                TurnOutcome::Failed { error, .. } => Change::Failed(error),
            },
            _ => continue,
        };
        let Some(hub) = hub.upgrade() else {
            return;
        };
        let lookup = session.clone();
        let found = hub
            .ctx
            .store
            .run(move |c| {
                Ok(tickets(c, "WHERE session_id=?1", &[&lookup.as_str()])?
                    .into_iter()
                    .find(|s| s.ticket.status.is_active()))
            })
            .await;
        let Ok(Some(stored)) = found else {
            continue;
        };
        let id = stored.ticket.id.clone();
        match change {
            Change::Working => {
                let _ = hub
                    .update_ticket(&id, |s| {
                        if s.ticket.status == TicketStatus::Working || !s.ticket.status.is_active()
                        {
                            return false;
                        }
                        s.ticket.status = TicketStatus::Working;
                        true
                    })
                    .await;
            }
            Change::Approval => {
                let _ = hub
                    .update_ticket(&id, |s| {
                        if !s.ticket.status.is_active() {
                            return false;
                        }
                        s.ticket.status = TicketStatus::NeedsApproval;
                        true
                    })
                    .await;
            }
            Change::Failed(error) => {
                let _ = hub
                    .update_ticket(&id, move |s| {
                        if !s.ticket.status.is_active() {
                            return false;
                        }
                        s.ticket.status = TicketStatus::Failed;
                        s.ticket.error = Some(error);
                        true
                    })
                    .await;
            }
            Change::Completed(turn) => {
                tokio::spawn(async move { hub.finish_turn(id, session, turn).await });
            }
        }
    }
}
