use std::path::Path;
use std::sync::Weak;

use harness_core_api::{
    AgentId, ApiError, ApiResult, Board, BoardLane, CardState, CardView, CreateSession, EventKind,
    HarnessCore, NewCard, RecvError, SendMessage, SessionId, TurnOutcome, UpdateCard,
    WorktreeRequest, MAX_BOARD_CARDS, MAX_CARD_BODY, MAX_CARD_TITLE,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::HubError;
use crate::hub::{Hub, HubCtx};
use crate::store_chat::{parse_json, session_cost};
use crate::util::{new_id, now_ms};

mod cards;
mod rows;

use rows::{by_session, renumber, view};
pub(crate) use rows::{lane_ids, load, place, rows_of, save};

pub(crate) const TABLE: &str = "
CREATE TABLE IF NOT EXISTS kanban_cards(
  id TEXT PRIMARY KEY, workspace_key TEXT NOT NULL, json TEXT NOT NULL, lane TEXT NOT NULL,
  position INTEGER NOT NULL, session_id TEXT, updated_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS kanban_cards_ws ON kanban_cards(workspace_key, lane, position);
CREATE INDEX IF NOT EXISTS kanban_cards_session ON kanban_cards(session_id);
";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CardData {
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) body: String,
    #[serde(default)]
    pub(crate) workspace_path: String,
    #[serde(default)]
    pub(crate) state: CardState,
    #[serde(default)]
    pub(crate) agent: Option<AgentId>,
    #[serde(default)]
    pub(crate) issue_number: Option<u64>,
    #[serde(default)]
    pub(crate) url: Option<String>,
    #[serde(default)]
    pub(crate) item_id: Option<String>,
    #[serde(default)]
    pub(crate) synced_lane: Option<BoardLane>,
    #[serde(default)]
    pub(crate) local_only: bool,
}

pub(crate) struct Row {
    pub(crate) id: String,
    pub(crate) workspace_key: String,
    pub(crate) data: CardData,
    pub(crate) lane: BoardLane,
    pub(crate) position: u32,
    pub(crate) session_id: Option<SessionId>,
    pub(crate) updated_at: i64,
}

const COLUMNS: &str = "id, workspace_key, json, lane, position, session_id, updated_at";

#[allow(clippy::too_many_arguments)]
pub(crate) fn link_card(
    c: &Connection,
    workspace_key: &str,
    workspace_path: &str,
    title: &str,
    body: &str,
    session: &SessionId,
    agent: Option<AgentId>,
    issue_number: Option<u64>,
) -> Result<String, HubError> {
    let lane = BoardLane::InProgress;
    let existing = issue_number
        .map(|_| rows_of(c, workspace_key))
        .transpose()?
        .and_then(|rows| {
            rows.into_iter()
                .find(|r| r.data.issue_number == issue_number && r.session_id.is_none())
        });
    if let Some(mut row) = existing {
        row.session_id = Some(session.clone());
        row.data.state = CardState::Idle;
        row.data.agent = agent;
        place(c, &mut row, lane, None)?;
        return Ok(row.id);
    }
    let row = Row {
        id: new_id("card"),
        workspace_key: workspace_key.to_owned(),
        data: CardData {
            title: title.chars().take(MAX_CARD_TITLE).collect(),
            body: body.chars().take(MAX_CARD_BODY).collect(),
            workspace_path: workspace_path.to_owned(),
            state: CardState::Idle,
            agent,
            issue_number,
            url: body.starts_with("https://").then(|| body.to_owned()),
            ..CardData::default()
        },
        lane,
        position: u32::try_from(lane_ids(c, workspace_key, lane)?.len()).unwrap_or(u32::MAX),
        session_id: Some(session.clone()),
        updated_at: now_ms(),
    };
    save(c, &row)?;
    Ok(row.id)
}

pub(crate) fn move_session_cards(
    c: &Connection,
    session: &SessionId,
    lane: BoardLane,
) -> Result<Vec<String>, HubError> {
    let mut keys = Vec::new();
    for mut row in by_session(c, session)? {
        if row.lane == lane {
            continue;
        }
        place(c, &mut row, lane, None)?;
        if !keys.contains(&row.workspace_key) {
            keys.push(row.workspace_key);
        }
    }
    Ok(keys)
}

fn clean_title(title: &str) -> ApiResult<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(ApiError::InvalidArgument("card title is empty".into()));
    }
    if title.chars().count() > MAX_CARD_TITLE {
        return Err(ApiError::InvalidArgument(format!(
            "card title is longer than {MAX_CARD_TITLE} characters"
        )));
    }
    Ok(title.to_owned())
}

fn clean_body(body: &str) -> ApiResult<String> {
    if body.chars().count() > MAX_CARD_BODY {
        return Err(ApiError::InvalidArgument(format!(
            "card body is longer than {MAX_CARD_BODY} characters"
        )));
    }
    Ok(body.trim_end().to_owned())
}

fn branch_for(title: &str, id: &str) -> String {
    let mut slug = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 32 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "task" } else { slug };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let mut hash: u32 = 0x811c_9dc5;
    for byte in id.bytes().chain(nanos.to_le_bytes()) {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("harness/card-{slug}-{:04x}", hash & 0xffff)
}

fn not_found(id: &str) -> ApiError {
    ApiError::NotFound(format!("card {id}"))
}

enum Change {
    Started(AgentId),
    Approval,
    Resumed,
    Finished(TurnOutcome),
}

fn apply(c: &Connection, session: &SessionId, change: &Change) -> Result<Vec<String>, HubError> {
    let mut keys = Vec::new();
    for mut row in by_session(c, session)? {
        let before = (row.lane, row.data.state, row.data.agent.clone());
        let mut lane = row.lane;
        match change {
            Change::Started(agent) => {
                row.data.state = CardState::Running;
                row.data.agent = Some(agent.clone());
                if matches!(row.lane, BoardLane::Todo | BoardLane::Review) {
                    lane = BoardLane::InProgress;
                }
            }
            Change::Approval => row.data.state = CardState::NeedsApproval,
            Change::Resumed => {
                if row.data.state == CardState::NeedsApproval {
                    row.data.state = CardState::Running;
                }
            }
            Change::Finished(TurnOutcome::Completed { .. }) => {
                row.data.state = CardState::Idle;
                if matches!(row.lane, BoardLane::Todo | BoardLane::InProgress) {
                    lane = BoardLane::Review;
                }
            }
            Change::Finished(TurnOutcome::Failed { .. }) => row.data.state = CardState::Failed,
            Change::Finished(TurnOutcome::Cancelled) => row.data.state = CardState::Idle,
        }
        if before == (lane, row.data.state, row.data.agent.clone()) {
            continue;
        }
        if lane == row.lane {
            row.updated_at = now_ms();
            save(c, &row)?;
        } else {
            place(c, &mut row, lane, None)?;
        }
        if !keys.contains(&row.workspace_key) {
            keys.push(row.workspace_key);
        }
    }
    Ok(keys)
}

pub(crate) async fn track(ctx: Weak<HubCtx>) {
    let Some(mut events) = ctx.upgrade().map(|c| c.events.subscribe()) else {
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
        let change = match event.kind {
            EventKind::TurnStarted { agent, .. } => Change::Started(agent),
            EventKind::PermissionRequested { .. } => Change::Approval,
            EventKind::PermissionResolved { .. } => Change::Resumed,
            EventKind::TurnFinished { outcome, .. } => Change::Finished(outcome),
            _ => continue,
        };
        let Some(ctx) = ctx.upgrade() else {
            return;
        };
        let refresh_cost = matches!(change, Change::Finished(_));
        let result = ctx
            .store
            .run(move |c| {
                let mut keys = apply(c, &session, &change)?;
                if refresh_cost {
                    for row in by_session(c, &session)? {
                        if !keys.contains(&row.workspace_key) {
                            keys.push(row.workspace_key);
                        }
                    }
                }
                Ok(keys)
            })
            .await;
        match result {
            Ok(keys) => {
                for key in keys {
                    ctx.events
                        .emit(None, EventKind::BoardChanged { workspace_key: key });
                }
            }
            Err(error) => tracing::warn!(%error, "board status not saved"),
        }
    }
}
