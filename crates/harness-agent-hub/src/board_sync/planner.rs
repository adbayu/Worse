use super::{
    new_id, now_ms, place, rows_of, save, BoardLane, CardData, Connection, HashSet, HubError, Link,
    Remote, Row, MAX_BOARD_CARDS, MAX_CARD_BODY, MAX_CARD_TITLE,
};

pub(super) struct Plan {
    pub(super) changed: bool,
    pub(super) pushes: Vec<(String, String, BoardLane)>,
    pub(super) uploads: Vec<(String, Option<u64>, String, String, BoardLane)>,
    pub(super) started: Vec<String>,
}

#[allow(clippy::too_many_lines)]
pub(super) fn plan(
    c: &Connection,
    key: &str,
    path: &str,
    link: &Link,
    remote: &[Remote],
) -> Result<Plan, HubError> {
    let mut rows = rows_of(c, key)?;
    let mut plan = Plan {
        changed: false,
        pushes: Vec::new(),
        uploads: Vec::new(),
        started: Vec::new(),
    };
    let mut seen = HashSet::new();
    for item in remote {
        seen.insert(item.item_id.clone());
        let remote_lane = item.option.as_deref().and_then(|o| link.lane_for(o));
        let found = rows
            .iter()
            .position(|r| r.data.item_id.as_deref() == Some(item.item_id.as_str()))
            .or_else(|| {
                item.number.and_then(|n| {
                    rows.iter()
                        .position(|r| r.data.item_id.is_none() && r.data.issue_number == Some(n))
                })
            });
        let Some(index) = found else {
            if rows.len() >= MAX_BOARD_CARDS {
                continue;
            }
            let lane = remote_lane.unwrap_or_default();
            let title = match item.number {
                Some(n) => format!("#{n} {}", item.title),
                None => item.title.clone(),
            };
            let mut row = Row {
                id: new_id("card"),
                workspace_key: key.to_owned(),
                data: CardData {
                    title: title.chars().take(MAX_CARD_TITLE).collect(),
                    body: item
                        .url
                        .clone()
                        .unwrap_or_else(|| item.body.clone())
                        .chars()
                        .take(MAX_CARD_BODY)
                        .collect(),
                    workspace_path: path.to_owned(),
                    issue_number: item.number,
                    url: item.url.clone(),
                    item_id: Some(item.item_id.clone()),
                    synced_lane: Some(lane),
                    ..CardData::default()
                },
                lane: BoardLane::Backlog,
                position: 0,
                session_id: None,
                updated_at: now_ms(),
            };
            place(c, &mut row, lane, None)?;
            if remote_lane.is_none() {
                plan.pushes
                    .push((row.id.clone(), item.item_id.clone(), lane));
            }
            plan.changed = true;
            rows.push(row);
            continue;
        };
        let row = &mut rows[index];
        if row.data.item_id.is_none() {
            row.data.item_id = Some(item.item_id.clone());
            row.data.local_only = false;
            save(c, row)?;
        }
        match remote_lane {
            Some(remote) if Some(remote) != row.data.synced_lane => {
                row.data.synced_lane = Some(remote);
                if row.lane == remote {
                    save(c, row)?;
                } else {
                    place(c, row, remote, None)?;
                    plan.changed = true;
                    if remote == BoardLane::Todo && row.session_id.is_none() {
                        plan.started.push(row.id.clone());
                    }
                }
            }
            _ => {
                if row.data.synced_lane != Some(row.lane) || remote_lane.is_none() {
                    plan.pushes
                        .push((row.id.clone(), item.item_id.clone(), row.lane));
                }
            }
        }
    }
    for row in &mut rows {
        if let Some(item) = row.data.item_id.clone() {
            if !seen.contains(&item) {
                row.data.item_id = None;
                row.data.local_only = true;
                row.updated_at = now_ms();
                save(c, row)?;
                plan.changed = true;
            }
        } else if !row.data.local_only {
            plan.uploads.push((
                row.id.clone(),
                row.data.issue_number,
                row.data.title.clone(),
                row.data.body.clone(),
                row.lane,
            ));
        }
    }
    Ok(plan)
}
