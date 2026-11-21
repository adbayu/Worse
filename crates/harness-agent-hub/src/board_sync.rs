use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use harness_core_api::{
    ApiError, ApiResult, Board, BoardLane, BoardSync, EventKind, RecvError, MAX_BOARD_CARDS,
    MAX_CARD_BODY, MAX_CARD_TITLE,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Notify;

use crate::board::{load, place, rows_of, save, CardData, Row};
use crate::error::HubError;
use crate::gh::gh;
use crate::hub::Hub;
use crate::util::{new_id, now_ms};

mod github;
mod planner;
mod project_setup;
mod sync;

use github::{
    ensure_status_field, fetch_items, graphql, set_status, split_repo, text, Remote, ADD_ITEM,
    CONVERT_DRAFT, CREATE_PROJECT, DELETE_ITEM, DELETE_WORKFLOW, FIGHTING_WORKFLOWS, REPO_QUERY,
    WORKFLOWS,
};
use planner::plan;
use project_setup::create_link;

pub(crate) const TABLE: &str = "
CREATE TABLE IF NOT EXISTS board_links(workspace_key TEXT PRIMARY KEY, source_id TEXT NOT NULL, json TEXT NOT NULL);
";

const SYNC_EVERY: Duration = Duration::from_secs(120);
const DEBOUNCE: Duration = Duration::from_secs(2);
const SCOPE_HINT: &str =
    "GitHub Project access needs the project scope: run `gh auth refresh -s project`";

#[derive(Default)]
pub(crate) struct BoardSyncSlot {
    wake: Arc<Notify>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    lock: tokio::sync::Mutex<()>,
    linked: Mutex<HashSet<String>>,
}

impl BoardSyncSlot {
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    pub(crate) fn stop(&self) {
        if let Ok(mut slot) = self.task.lock() {
            if let Some(task) = slot.take() {
                task.abort();
            }
        }
    }

    fn is_linked(&self, key: &str) -> bool {
        self.linked.lock().is_ok_and(|set| set.contains(key))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Link {
    repo: String,
    project_id: String,
    project_number: u64,
    project_url: String,
    project_title: String,
    status_field_id: String,
    options: BTreeMap<String, String>,
    linked_at: i64,
    #[serde(default)]
    last_sync: Option<i64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    workflow_error: Option<String>,
}

impl Link {
    fn option_for(&self, lane: BoardLane) -> Option<&String> {
        self.options.get(lane.as_str())
    }

    fn lane_for(&self, option: &str) -> Option<BoardLane> {
        self.options
            .iter()
            .find(|(_, id)| id.as_str() == option)
            .and_then(|(lane, _)| BoardLane::parse(lane))
    }
}

fn load_link(c: &Connection, key: &str) -> Result<Option<(String, Link)>, HubError> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT source_id, json FROM board_links WHERE workspace_key=?1",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(source, json)| serde_json::from_str(&json).ok().map(|l| (source, l))))
}

fn save_link(c: &Connection, key: &str, source: &str, link: &Link) -> Result<(), HubError> {
    c.execute(
        "INSERT INTO board_links(workspace_key, source_id, json) VALUES(?1, ?2, ?3)
         ON CONFLICT(workspace_key) DO UPDATE SET source_id=?2, json=?3",
        params![key, source, serde_json::to_string(link)?],
    )?;
    Ok(())
}

fn linked_keys(c: &Connection) -> Result<Vec<String>, HubError> {
    let mut statement = c.prepare("SELECT workspace_key FROM board_links")?;
    let keys = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(Result::ok)
        .collect();
    Ok(keys)
}

pub(crate) fn is_linked(c: &Connection, key: &str) -> Result<bool, HubError> {
    Ok(load_link(c, key)?.is_some())
}

pub(crate) fn meta(
    c: &Connection,
    key: &str,
) -> Result<(Option<BoardSync>, Option<String>), HubError> {
    if let Some((_, link)) = load_link(c, key)? {
        let sync = BoardSync {
            repo: link.repo,
            project_title: link.project_title,
            project_url: link.project_url,
            last_sync: link.last_sync,
            error: link.error,
        };
        return Ok((Some(sync), None));
    }
    let repo = crate::inbox::sources(c)?
        .into_iter()
        .find(|s| s.workspace_key == key)
        .map(|s| s.repo);
    Ok((None, repo))
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn aliases(lane: BoardLane) -> &'static [&'static str] {
    match lane {
        BoardLane::Backlog => &["backlog"],
        BoardLane::Todo => &["todo", "ready"],
        BoardLane::InProgress => &["inprogress", "doing"],
        BoardLane::Review => &["review", "inreview"],
        BoardLane::Done => &["done"],
    }
}

fn lane_color(lane: BoardLane) -> &'static str {
    match lane {
        BoardLane::Backlog => "GRAY",
        BoardLane::Todo => "BLUE",
        BoardLane::InProgress => "YELLOW",
        BoardLane::Review => "PURPLE",
        BoardLane::Done => "GREEN",
    }
}

async fn run_loop(hub: Weak<Hub>, wake: Arc<Notify>) {
    loop {
        let keys = {
            let Some(hub) = hub.upgrade() else { return };
            if hub.check_open().is_err() {
                return;
            }
            hub.ctx.store.run(linked_keys).await.unwrap_or_default()
        };
        if keys.is_empty() {
            return;
        }
        for key in keys {
            let Some(hub) = hub.upgrade() else { return };
            hub.sync_now(&key).await;
        }
        tokio::select! {
            () = tokio::time::sleep(SYNC_EVERY) => {}
            () = wake.notified() => {
                tokio::time::sleep(DEBOUNCE).await;
            }
        }
    }
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
        let EventKind::BoardChanged { workspace_key } = event.kind else {
            continue;
        };
        let Some(hub) = hub.upgrade() else { return };
        if hub.board_sync.is_linked(&workspace_key) {
            hub.board_sync.wake();
        }
    }
}
