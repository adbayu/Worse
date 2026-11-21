//! Store additions of API 0.5.0 (phase-9-parity.md §9.2.2): sidebar columns, groups, turn truncation and copies,
//! schedules. Additive migrations; `user_version` stays 1. Every function runs inside `Store::run` (single writer).

use harness_core_api::{
    ForkOrigin, GroupColor, ScheduledPrompt, SessionGroup, SessionId, SessionSummary, TurnId,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::HubError;

mod groups;
mod imports;
mod truncate;

pub(crate) use groups::{
    delete_group, insert_group, list_groups, move_sessions, set_group_positions, update_group,
};
pub(crate) use imports::{
    delete_schedule, imported_keys, list_schedules, native_session_ids, save_schedule,
    set_imported_from, transcript_matches,
};
pub(crate) use truncate::{copy_turns, is_undone, set_fork_origin, set_undone, truncate_turns};

const TABLES: &str = "
CREATE TABLE IF NOT EXISTS session_groups(
  id TEXT PRIMARY KEY, name TEXT NOT NULL, color TEXT NOT NULL, position INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS schedules(
  id TEXT PRIMARY KEY, json TEXT NOT NULL, created_at INTEGER NOT NULL);
";

/// Adds the 0.5.0 columns and tables when missing.
pub(crate) fn migrate(c: &Connection) -> Result<(), HubError> {
    c.execute_batch(TABLES)?;
    c.execute_batch(crate::delegate::TABLE)?;
    c.execute_batch(crate::board::TABLE)?;
    c.execute_batch(crate::inbox::TABLES)?;
    c.execute_batch(crate::board_sync::TABLE)?;
    for (table, column, kind) in [
        ("sessions", "pinned_at", "INTEGER"),
        ("sessions", "group_id", "TEXT"),
        ("sessions", "sort_order", "INTEGER"),
        ("sessions", "unread_mark", "INTEGER"),
        ("sessions", "read_at", "INTEGER"),
        ("sessions", "forked_from_session", "TEXT"),
        ("sessions", "forked_from_seq", "INTEGER"),
        ("sessions", "imported_from", "TEXT"),
        ("sessions", "worktree_json", "TEXT"),
        ("turns", "context_json", "TEXT"),
        ("turns", "usage_json", "TEXT"),
        ("turns", "context_usage_json", "TEXT"),
        ("turns", "origin_json", "TEXT"),
        ("turn_snapshots", "undone", "INTEGER"),
    ] {
        if !has_column(c, table, column)? {
            c.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"),
                [],
            )?;
            if column == "read_at" {
                // 迁移前的会话视为已读，否则升级后所有历史会话都会突然显示为未读。
                c.execute("UPDATE sessions SET read_at=updated_at", [])?;
            }
        }
    }
    Ok(())
}

fn has_column(c: &Connection, table: &str, column: &str) -> Result<bool, HubError> {
    let mut statement = c.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
    let found = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(Result::ok)
        .any(|name| name == column);
    Ok(found)
}

pub(crate) fn json_opt<T: Serialize>(value: Option<&T>) -> Result<Option<String>, HubError> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(HubError::from)
}

pub(crate) fn json_list<T: Serialize>(value: &[T]) -> Result<Option<String>, HubError> {
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::to_string(value)?))
    }
}

pub(crate) fn parse_json<T: DeserializeOwned>(text: Option<String>) -> Option<T> {
    text.and_then(|t| serde_json::from_str(&t).ok())
}

/// Fills the 0.5.0 sidebar fields of a summary from the session row.
pub(crate) fn decorate(c: &Connection, summary: &mut SessionSummary) -> Result<(), HubError> {
    let row = c
        .query_row(
            "SELECT pinned_at, group_id, COALESCE(sort_order, 0), COALESCE(unread_mark, 0), read_at,
                    forked_from_session, forked_from_seq, imported_from,
                    (SELECT MAX(finished_at) FROM turns WHERE session_id=?1), worktree_json
             FROM sessions WHERE id=?1",
            params![summary.id.as_str()],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<u32>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                    r.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        pinned,
        group,
        order,
        mark,
        read_at,
        fork_session,
        fork_seq,
        imported,
        finished,
        worktree,
    )) = row
    else {
        return Ok(());
    };
    summary.parent_id = crate::delegate::parent_of(c, &summary.id)?;
    summary.pinned = pinned.is_some();
    summary.group_id = group;
    summary.order = order;
    summary.unread = mark != 0 || finished.is_some_and(|f| f > read_at.unwrap_or(i64::MIN));
    summary.forked_from = fork_session.map(|session_id| ForkOrigin {
        session_id: SessionId::new(session_id),
        turn_seq: fork_seq.unwrap_or(0),
    });
    summary.imported_from = imported;
    summary.worktree = parse_json(worktree);
    Ok(())
}

pub(crate) fn set_worktree(
    c: &Connection,
    id: &SessionId,
    info: &harness_core_api::WorktreeInfo,
) -> Result<(), HubError> {
    c.execute(
        "UPDATE sessions SET worktree_json=?2 WHERE id=?1",
        params![id.as_str(), serde_json::to_string(info)?],
    )?;
    Ok(())
}

pub(crate) fn session_cost_usd(c: &Connection, id: &SessionId) -> Result<Option<f64>, HubError> {
    let mut statement = c.prepare(
        "SELECT COALESCE(agent_id, ''), context_usage_json FROM turns
         WHERE session_id=?1 AND context_usage_json IS NOT NULL ORDER BY seq",
    )?;
    let rows = statement
        .query_map(params![id.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .filter_map(Result::ok);
    let mut last: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let mut total = 0.0_f64;
    let mut any = false;
    for (agent, json) in rows {
        let Some(usage) = parse_json::<harness_core_api::chat::ContextUsage>(Some(json)) else {
            continue;
        };
        let Some(cost) = usage.cost else {
            continue;
        };
        if !cost.currency.eq_ignore_ascii_case("USD") {
            continue;
        }
        let Ok(amount) = cost.amount.trim().parse::<f64>() else {
            continue;
        };
        if !amount.is_finite() || amount < 0.0 {
            continue;
        }
        let previous = last.insert(agent, amount).unwrap_or(0.0);
        total += if amount >= previous {
            amount - previous
        } else {
            amount
        };
        any = true;
    }
    Ok(any.then_some(total))
}

pub(crate) fn session_cost(
    c: &Connection,
    id: &SessionId,
) -> Result<Option<harness_core_api::SessionCost>, HubError> {
    let own = session_cost_usd(c, id)?;
    let mut statement = c.prepare("SELECT child_id FROM subagents WHERE parent_id=?1")?;
    let children: Vec<String> = statement
        .query_map(params![id.as_str()], |r| r.get::<_, String>(0))?
        .filter_map(Result::ok)
        .collect();
    let mut sub: Option<f64> = None;
    for child in children {
        if let Some(amount) = session_cost_usd(c, &SessionId::new(child))? {
            sub = Some(sub.unwrap_or(0.0) + amount);
        }
    }
    if own.is_none() && sub.is_none() {
        return Ok(None);
    }
    let usd = |amount: f64| harness_core_api::chat::Cost {
        amount: format!("{amount:.4}"),
        currency: "USD".into(),
    };
    let own_amount = own.unwrap_or(0.0);
    Ok(Some(harness_core_api::SessionCost {
        own: usd(own_amount),
        subagents: sub.map(usd),
        total: usd(own_amount + sub.unwrap_or(0.0)),
    }))
}
pub(crate) fn worktrees_of(
    c: &Connection,
    ids: &[SessionId],
) -> Result<Vec<harness_core_api::WorktreeInfo>, HubError> {
    let mut found = Vec::new();
    for id in ids {
        let json: Option<String> = c
            .query_row(
                "SELECT worktree_json FROM sessions WHERE id=?1",
                params![id.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(info) = parse_json(json) {
            found.push(info);
        }
    }
    Ok(found)
}

/// Marks a new session read at creation (new sessions start read).
pub(crate) fn mark_read(c: &Connection, id: &SessionId, now: i64) -> Result<bool, HubError> {
    Ok(c.execute(
        "UPDATE sessions SET read_at=?2, unread_mark=0 WHERE id=?1",
        params![id.as_str(), now],
    )? > 0)
}

pub(crate) fn mark_unread(c: &Connection, id: &SessionId) -> Result<bool, HubError> {
    Ok(c.execute(
        "UPDATE sessions SET unread_mark=1 WHERE id=?1",
        params![id.as_str()],
    )? > 0)
}

pub(crate) fn set_pinned(
    c: &Connection,
    ids: &[SessionId],
    at: Option<i64>,
) -> Result<Vec<SessionId>, HubError> {
    let mut changed = Vec::new();
    for id in ids {
        if c.execute(
            "UPDATE sessions SET pinned_at=?2 WHERE id=?1",
            params![id.as_str(), at],
        )? > 0
        {
            changed.push(id.clone());
        }
    }
    Ok(changed)
}

pub(crate) fn session_exists(c: &Connection, id: &SessionId) -> Result<bool, HubError> {
    Ok(c.query_row(
        "SELECT 1 FROM sessions WHERE id=?1",
        params![id.as_str()],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

fn snippet(text: &str, needle: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = text
        .chars()
        .map(|ch| ch.to_lowercase().next().unwrap_or(ch))
        .collect();
    let needle: Vec<char> = needle.chars().collect();
    let start = (0..=lower.len().saturating_sub(needle.len()))
        .find(|&i| lower.get(i..i + needle.len()) == Some(&needle[..]))
        .unwrap_or(0);
    let from = start.saturating_sub(30);
    let to = (start + needle.len() + 50).min(chars.len());
    chars[from..to]
        .iter()
        .collect::<String>()
        .replace('\n', " ")
}

/// Replaces the user text of a (queued) turn (phase-9-parity.md §9.2.5 R12).
pub(crate) fn set_user_text(c: &Connection, turn: &TurnId, text: &str) -> Result<(), HubError> {
    c.execute(
        "UPDATE turns SET user_text=?2 WHERE id=?1",
        params![turn.as_str(), text],
    )?;
    Ok(())
}
