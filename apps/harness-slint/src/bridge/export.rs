//! "Export as Markdown": the full transcript of a session, including reasoning and tool calls.

use std::fmt::Write as _;
use std::path::PathBuf;

use harness_core_api::{
    ContextItem, SessionDetail, ThoughtView, ToolCallStatus, ToolCallView, ToolContent,
    TurnSegment, TurnView,
};

use super::UiState;
use crate::core::{ChatCall, SessionId};

impl UiState {
    pub(super) fn export_session(&self, session: SessionId) {
        self.chat_call(
            |reply| ChatCall::Export { session, reply },
            |s, detail| {
                let names: Vec<(String, String)> = s
                    .agent_list
                    .iter()
                    .map(|a| (a.id.clone(), a.name.clone()))
                    .collect();
                let start = Some(s.workspace.clone());
                std::thread::spawn(move || save(&detail, &names, start));
            },
        );
    }
}

fn save(detail: &SessionDetail, names: &[(String, String)], start: Option<PathBuf>) {
    let mut dialog = rfd::FileDialog::new()
        .add_filter("Markdown", &["md"])
        .set_file_name(format!("{}.md", file_stem(&detail.summary.title)));
    if let Some(dir) = start {
        dialog = dialog.set_directory(dir);
    }
    let Some(path) = dialog.save_file() else {
        return;
    };
    let name = |id: &str| {
        names
            .iter()
            .find(|(k, _)| k == id)
            .map_or_else(|| id.to_owned(), |(_, v)| v.clone())
    };
    let text = render(detail, &name);
    let result = std::fs::write(&path, text);
    let _ = slint::invoke_from_event_loop(move || match result {
        Ok(()) => {
            use harness_platform::PathRevealer;
            let _ = harness_platform::desktop::SystemPathRevealer::new().reveal(&path);
        }
        Err(error) => super::with_state(|s| {
            s.fail(&format!("Export failed: {}: {error}", path.display()));
        }),
    });
}

fn file_stem(title: &str) -> String {
    let stem: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .chars()
        .take(80)
        .collect();
    if stem.is_empty() {
        "session".into()
    } else {
        stem
    }
}

pub(super) fn render(detail: &SessionDetail, name: &dyn Fn(&str) -> String) -> String {
    let s = &detail.summary;
    let mut out = String::new();
    let title = if s.title.trim().is_empty() {
        "Untitled session"
    } else {
        s.title.trim()
    };
    let _ = writeln!(out, "# {title}\n");
    let _ = writeln!(out, "- Session: `{}`", s.id.as_str());
    let _ = writeln!(out, "- Workspace: `{}`", s.workspace_path);
    let _ = writeln!(out, "- Created: {}", date(s.created_at));
    let agents: Vec<String> = s.agents_used.iter().map(|a| name(a.as_str())).collect();
    if !agents.is_empty() {
        let _ = writeln!(out, "- Agents: {}", agents.join(", "));
    }
    let _ = writeln!(out, "- Turns: {}", detail.turns.len());
    for turn in &detail.turns {
        out.push_str("\n---\n\n");
        render_turn(&mut out, turn, name);
    }
    out
}

#[allow(clippy::too_many_lines)]
fn render_turn(out: &mut String, turn: &TurnView, name: &dyn Fn(&str) -> String) {
    let _ = writeln!(out, "## [User] · {}\n", date(turn.created_at));
    let _ = writeln!(out, "{}\n", turn.user_text.trim_end());
    for item in &turn.context {
        let line = match item {
            ContextItem::File { path } => format!("- File: `{path}`"),
            ContextItem::Folder { path } => format!("- Folder: `{path}`"),
            ContextItem::Image { path, mime, .. } => {
                format!("- Image: `{}`", path.as_deref().unwrap_or(mime))
            }
            ContextItem::Text { title, text } => format!(
                "- Pasted text{}:\n\n{}",
                title
                    .as_deref()
                    .map(|t| format!(" ({t})"))
                    .unwrap_or_default(),
                fence(text, "")
            ),
            ContextItem::Session { session_id } => format!("- Session: `{}`", session_id.as_str()),
            ContextItem::Research => "- Deep research mode".to_owned(),
            ContextItem::CodeMode => "- Code mode".to_owned(),
        };
        let _ = writeln!(out, "{line}");
    }
    if !turn.context.is_empty() {
        out.push('\n');
    }
    let last = turn.attempts.last().map_or(1, |a| a.attempt);
    for attempt in turn.attempts.iter().filter(|a| a.attempt != last) {
        let _ = writeln!(
            out,
            "## [{}] · attempt {} · {}\n",
            name(attempt.agent.as_str()),
            attempt.attempt,
            attempt.status
        );
        for thought in turn
            .thoughts
            .iter()
            .filter(|t| t.attempt == attempt.attempt)
        {
            write_thought(out, thought);
        }
        if !attempt.partial_text.trim().is_empty() {
            let _ = writeln!(out, "{}\n", attempt.partial_text.trim_end());
        }
        if let Some(error) = &attempt.error {
            let _ = writeln!(out, "> Error: {}\n", one_line(error));
        }
    }
    let agent = turn
        .attempts
        .last()
        .map(|a| a.agent.as_str())
        .or_else(|| turn.agent.as_ref().map(harness_core_api::AgentId::as_str))
        .unwrap_or("agent");
    let _ = writeln!(out, "## [{}]\n", name(agent));
    let mut thoughts: Vec<&ThoughtView> =
        turn.thoughts.iter().filter(|t| t.attempt == last).collect();
    thoughts.sort_by_key(|t| t.text_offset);
    let mut thoughts = thoughts.into_iter().peekable();
    let mut pos = 0usize;
    let segments: Vec<TurnSegment> = if turn.segments.is_empty() {
        vec![TurnSegment::Text {
            text: turn.assistant_text.clone(),
        }]
    } else {
        turn.segments.clone()
    };
    for segment in &segments {
        match segment {
            TurnSegment::Text { text } => {
                let mut rest = text.as_str();
                let mut at = pos;
                while let Some(t) = thoughts.peek() {
                    if t.text_offset > at + rest.len() {
                        break;
                    }
                    let mut cut = t.text_offset.saturating_sub(at).min(rest.len());
                    while !rest.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    write_text(out, &rest[..cut]);
                    rest = &rest[cut..];
                    at += cut;
                    if let Some(t) = thoughts.next() {
                        write_thought(out, t);
                    }
                }
                write_text(out, rest);
                pos += text.len();
            }
            TurnSegment::Tool { tool_call_id } => {
                while thoughts.peek().is_some_and(|t| t.text_offset <= pos) {
                    if let Some(t) = thoughts.next() {
                        write_thought(out, t);
                    }
                }
                if let Some(tool) = turn
                    .tool_calls
                    .iter()
                    .find(|t| &t.tool_call_id == tool_call_id)
                {
                    write_tool(out, tool);
                }
            }
            TurnSegment::Steer { text } => {
                let _ = writeln!(out, "## [User · steer]\n\n{}\n", text.trim_end());
                let _ = writeln!(out, "## [{}]\n", name(agent));
            }
        }
    }
    for t in thoughts {
        write_thought(out, t);
    }
    for tool in &turn.tool_calls {
        if !segments.iter().any(
            |s| matches!(s, TurnSegment::Tool { tool_call_id } if *tool_call_id == tool.tool_call_id),
        ) {
            write_tool(out, tool);
        }
    }
    if let Some(error) = &turn.error {
        let _ = writeln!(out, "> Error: {}\n", one_line(error));
    }
}

fn write_text(out: &mut String, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        let _ = writeln!(out, "{text}\n");
    }
}

fn write_thought(out: &mut String, thought: &ThoughtView) {
    let text = thought.text.trim();
    if text.is_empty() {
        return;
    }
    out.push_str("> **Thinking**\n>\n");
    for line in text.lines() {
        if line.is_empty() {
            out.push_str(">\n");
        } else {
            let _ = writeln!(out, "> {line}");
        }
    }
    out.push('\n');
}

fn write_tool(out: &mut String, tool: &ToolCallView) {
    let status = match tool.status {
        Some(ToolCallStatus::Completed) => "completed",
        Some(ToolCallStatus::Failed) => "failed",
        Some(ToolCallStatus::InProgress) => "in progress",
        Some(ToolCallStatus::Pending) => "pending",
        None => "unknown",
    };
    let kind = tool.kind.as_deref().unwrap_or("tool");
    let _ = writeln!(
        out,
        "**Tool call** ({kind}, {status}): {}\n",
        one_line(&tool.title)
    );
    if let Some(input) = tool.raw_input.as_deref().filter(|s| !s.trim().is_empty()) {
        let _ = writeln!(out, "Input:\n\n{}", fence(&pretty(input), "json"));
    }
    let mut output = String::new();
    for content in &tool.content {
        match content {
            ToolContent::Text { text } => {
                output.push_str(text);
                output.push('\n');
            }
            ToolContent::Diff {
                path,
                old_text,
                new_text,
            } => {
                let _ = writeln!(out, "Diff `{path}`:\n");
                let mut diff = String::new();
                for line in old_text.as_deref().unwrap_or_default().lines() {
                    let _ = writeln!(diff, "-{line}");
                }
                for line in new_text.lines() {
                    let _ = writeln!(diff, "+{line}");
                }
                let _ = writeln!(out, "{}", fence(&diff, "diff"));
            }
            ToolContent::Terminal { terminal_id } => {
                let _ = writeln!(output, "[terminal {terminal_id}]");
            }
        }
    }
    if output.trim().is_empty() {
        if let Some(raw) = tool.raw_output.as_deref().filter(|s| !s.trim().is_empty()) {
            output = pretty(raw);
        }
    }
    if !output.trim().is_empty() {
        let _ = writeln!(out, "Output:\n\n{}", fence(output.trim_end(), ""));
    }
}

fn pretty(raw: &str) -> String {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| match v {
            serde_json::Value::String(s) => Some(s),
            other => serde_json::to_string_pretty(&other).ok(),
        })
        .unwrap_or_else(|| raw.to_owned())
}

fn fence(text: &str, lang: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let ticks = "`".repeat(longest.max(2) + 1);
    format!("{ticks}{lang}\n{}\n{ticks}\n", text.trim_end())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn date(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60
    )
}

#[cfg(test)]
mod tests {
    use super::{date, fence};

    #[test]
    fn dates_and_fences() {
        assert_eq!(date(0), "1970-01-01 00:00 UTC");
        assert_eq!(date(1_763_300_000_000), "2025-11-16 13:33 UTC");
        assert!(fence("a ``` b", "").starts_with("````\n"));
    }
}
