use super::{
    api, mpsc, Event, ItemId, Path, SessionSummary, ToolCallStatus, ToolCallView, ToolStatus,
    TurnOutcome, TurnStatus,
};

pub(super) fn emit(ev: &mpsc::UnboundedSender<Event>, event: Event) {
    // UI 已退出时发送失败是正常的关停路径，忽略即可。
    let _ = ev.send(event);
}

/// `resets_at` (unix seconds, UTC) as a short local display time (`HH:MM`, or `MM-DD HH:MM` on another day).
pub(super) fn reset_label(resets_at: Option<i64>) -> Option<String> {
    resets_at
        .map(api::time::local_time_label)
        .filter(|label| !label.is_empty())
}

pub(super) fn tool_status(status: Option<ToolCallStatus>) -> ToolStatus {
    match status {
        None | Some(ToolCallStatus::Pending | ToolCallStatus::InProgress) => ToolStatus::Running,
        Some(ToolCallStatus::Completed) => ToolStatus::Done,
        Some(ToolCallStatus::Failed) => ToolStatus::Failed,
    }
}

pub(super) fn outcome_status(outcome: &TurnOutcome) -> TurnStatus {
    match outcome {
        TurnOutcome::Completed { .. } => TurnStatus::Done,
        TurnOutcome::Cancelled => TurnStatus::Cancelled,
        TurnOutcome::Failed { .. } => TurnStatus::Failed,
    }
}

pub(super) fn summary(s: &api::SessionSummary) -> SessionSummary {
    SessionSummary {
        id: s.id.0.clone(),
        title: s.title.clone(),
        updated_ms: u64::try_from(s.updated_at).unwrap_or(0),
        agents_used: vec![s.active_agent.0.clone()],
        pinned: s.pinned,
        group_id: s.group_id.clone(),
        order: s.order,
        unread: s.unread,
        forked_from: s.forked_from.as_ref().map(|f| f.session_id.0.clone()),
        imported: s.imported_from.is_some(),
        workspace_path: s.workspace_path.clone(),
        project: s.workspace_key.clone(),
        branch: s.worktree.as_ref().map(|w| w.branch.clone()),
    }
}

/// Emits `Event::ToolDetail` when the agent sent anything beyond the title and status.
pub(super) fn tool_detail(
    ev: &mpsc::UnboundedSender<Event>,
    session: &str,
    id: ItemId,
    tool: &ToolCallView,
) {
    let empty = tool.content.is_empty()
        && tool.locations.is_empty()
        && tool.kind.is_none()
        && tool.raw_input.is_none()
        && tool.raw_output.is_none();
    if empty {
        return;
    }
    emit(
        ev,
        Event::ToolDetail {
            session: session.to_owned(),
            id,
            kind: tool.kind.clone(),
            content: tool.content.clone(),
            locations: tool.locations.clone(),
            raw_input: tool.raw_input.clone(),
            raw_output: tool.raw_output.clone(),
        },
    );
}

pub(super) fn tool_title(
    ev: &mpsc::UnboundedSender<Event>,
    session: &str,
    id: ItemId,
    title: &str,
) {
    let title = title.trim();
    if title.is_empty() {
        return;
    }
    emit(
        ev,
        Event::ToolTitle {
            session: session.to_owned(),
            id,
            title: title.to_owned(),
        },
    );
}

pub(super) const DEFAULT_SESSION_TITLE: &str = "New session";
pub(super) const TITLE_CHARS: usize = 60;

pub(super) fn prompt_title(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= TITLE_CHARS {
        return Some(collapsed);
    }
    let mut out: String = collapsed.chars().take(TITLE_CHARS - 1).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    Some(out)
}

pub(super) const DETAIL_LINES: usize = 6;
pub(super) const DETAIL_VALUE_CHARS: usize = 160;

pub(super) fn detail_value(text: &str) -> String {
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let more = first.chars().count() > DETAIL_VALUE_CHARS || text.trim().lines().count() > 1;
    let mut out: String = first.chars().take(DETAIL_VALUE_CHARS).collect();
    if more {
        out.push('…');
    }
    out
}

pub(super) fn permission_detail(tool: &ToolCallView) -> String {
    let Some(raw) = tool
        .raw_input
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    else {
        return tool.kind.clone().unwrap_or_default();
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(raw) else {
        return raw.to_owned();
    };
    if let Some(description) = map
        .get("description")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        return description.to_owned();
    }
    let lines: Vec<String> = map
        .iter()
        .filter_map(|(key, value)| {
            let text = match value {
                serde_json::Value::String(s) if !s.trim().is_empty() => detail_value(s),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                _ => return None,
            };
            Some(format!("{key}: {text}"))
        })
        .take(DETAIL_LINES)
        .collect();
    if lines.is_empty() {
        return tool.kind.clone().unwrap_or_default();
    }
    lines.join("\n")
}

pub(super) fn with_content_warnings(detail: String, tool: &ToolCallView) -> String {
    let warnings: Vec<String> = tool
        .locations
        .iter()
        .map(|l| Path::new(&l.path))
        .filter(|p| p.is_file())
        .take(4)
        .filter_map(|p| {
            let warning = harness_workspace::filetype::inspect(p).warning?;
            let name = p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            Some(format!("⚠ {name}: {warning}"))
        })
        .collect();
    if warnings.is_empty() {
        return detail;
    }
    if detail.is_empty() {
        return warnings.join("\n");
    }
    format!("{detail}\n{}", warnings.join("\n"))
}
