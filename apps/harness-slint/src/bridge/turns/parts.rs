use super::{
    sv, PlanEntry, PlanRow, ScheduleKind, ScheduleRow, ScheduledPrompt, ToolContent, ToolLocation,
    ToolPart, MAX_DIFF_LINES,
};

fn part(kind: i32, text: &str) -> ToolPart {
    ToolPart {
        kind,
        text: sv(&crate::markdown::expand_tabs(text)),
    }
}

const PART_LINES: usize = 200;
const PART_CHARS: usize = 12_000;

fn clip(text: &str) -> std::borrow::Cow<'_, str> {
    let mut end = text.len();
    let mut cut = false;
    if let Some((at, _)) = text.char_indices().nth(PART_CHARS) {
        end = at;
        cut = true;
    }
    if let Some((at, _)) = text[..end].match_indices('\n').nth(PART_LINES) {
        end = at;
        cut = true;
    }
    if !cut {
        return std::borrow::Cow::Borrowed(text);
    }
    let rest = text[end..].lines().filter(|l| !l.is_empty()).count();
    std::borrow::Cow::Owned(format!(
        "{}\n… {rest} more lines not shown",
        text[..end].trim_end()
    ))
}

fn fenced_body(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    let rest = trimmed.strip_prefix("```")?;
    let (_, body) = rest.split_once('\n')?;
    let body = body.strip_suffix("```")?;
    let body = body.strip_suffix('\n').unwrap_or(body);
    let body = body.strip_suffix('\r').unwrap_or(body);
    if body
        .lines()
        .any(|line| line.trim_start().starts_with("```"))
    {
        return None;
    }
    Some(body)
}

pub(super) const THOUGHT_PREVIEW_CHARS: usize = 200;

pub(super) fn thought_preview(text: &str) -> String {
    let mut out = String::new();
    let mut count = 0;
    for word in text.split_whitespace() {
        if count >= THOUGHT_PREVIEW_CHARS {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
            count += 1;
        }
        out.push_str(word);
        count += word.chars().count();
    }
    out
}

/// Line diff for a tool card: common head / tail lines are dropped, the middle shows as removed then added.
fn diff_parts(out: &mut Vec<ToolPart>, old: Option<&str>, new: &str) {
    let old_lines: Vec<&str> = old.map(|o| o.lines().collect()).unwrap_or_default();
    let new_lines: Vec<&str> = new.lines().collect();
    let head = old_lines
        .iter()
        .zip(&new_lines)
        .take_while(|(a, b)| a == b)
        .count();
    let tail = old_lines[head..]
        .iter()
        .rev()
        .zip(new_lines[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let removed = &old_lines[head..old_lines.len() - tail];
    let added = &new_lines[head..new_lines.len() - tail];
    let mut shown = 0usize;
    for (kind, lines) in [(3, removed), (2, added)] {
        for line in lines {
            if shown == MAX_DIFF_LINES {
                out.push(part(
                    6,
                    &format!("… {} more lines", removed.len() + added.len() - shown),
                ));
                return;
            }
            out.push(part(kind, line));
            shown += 1;
        }
    }
}

pub(super) fn diff_counts(content: &[ToolContent]) -> (i32, i32) {
    let mut added = 0usize;
    let mut removed = 0usize;
    for item in content {
        if let ToolContent::Diff {
            old_text, new_text, ..
        } = item
        {
            let old_lines: Vec<&str> = old_text
                .as_deref()
                .map(|o| o.lines().collect())
                .unwrap_or_default();
            let new_lines: Vec<&str> = new_text.lines().collect();
            let head = old_lines
                .iter()
                .zip(&new_lines)
                .take_while(|(a, b)| a == b)
                .count();
            let tail = old_lines[head..]
                .iter()
                .rev()
                .zip(new_lines[head..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            removed += old_lines.len() - head - tail;
            added += new_lines.len() - head - tail;
        }
    }
    (
        i32::try_from(added).unwrap_or(i32::MAX),
        i32::try_from(removed).unwrap_or(i32::MAX),
    )
}

/// Lines shown under a collapsed edit row.
const PEEK_LINES: usize = 6;

/// The first changed lines of a tool card (removed / added rows of its diffs), for the collapsed row; "…" when
/// there are more.
pub(super) fn peek_parts(parts: &[ToolPart]) -> Vec<ToolPart> {
    let changed: Vec<&ToolPart> = parts
        .iter()
        .filter(|p| p.kind == 2 || p.kind == 3)
        .collect();
    let mut out: Vec<ToolPart> = changed
        .iter()
        .take(PEEK_LINES)
        .map(|p| (*p).clone())
        .collect();
    if changed.len() > PEEK_LINES {
        out.push(part(
            6,
            &format!("{} more lines", changed.len() - PEEK_LINES),
        ));
    }
    out
}

/// `ToolPart` rows of an expanded tool card. Section labels carry an id ("input" / "output") translated in Slint.
pub(super) fn tool_parts(
    content: &[ToolContent],
    locations: &[ToolLocation],
    raw_input: Option<&str>,
    raw_output: Option<&str>,
) -> Vec<ToolPart> {
    let mut out = Vec::new();
    for location in locations {
        let text = match location.line {
            Some(line) => format!("{}:{line}", location.path),
            None => location.path.clone(),
        };
        out.push(part(4, &text));
    }
    for item in content {
        match item {
            ToolContent::Text { text } => match fenced_body(text) {
                Some(body) => out.push(part(6, &clip(body.trim_end()))),
                None => out.push(part(0, &clip(text.trim_end()))),
            },
            ToolContent::Diff {
                path,
                old_text,
                new_text,
            } => {
                out.push(part(1, path));
                diff_parts(&mut out, old_text.as_deref(), new_text);
            }
            ToolContent::Terminal { .. } => {}
        }
    }
    for (label, raw) in [("input", raw_input), ("output", raw_output)] {
        if let Some(raw) = raw.filter(|r| !r.trim().is_empty()) {
            out.push(part(5, label));
            out.push(part(6, &clip(raw.trim_end())));
        }
    }
    out
}

pub(super) fn plan_rows(entries: &[PlanEntry]) -> Vec<PlanRow> {
    entries
        .iter()
        .map(|e| PlanRow {
            text: sv(&e.content),
            status: match e.status.as_str() {
                "in_progress" => 1,
                "completed" => 2,
                _ => 0,
            },
            priority: sv(&e.priority),
        })
        .collect()
}

/// "12.3k / 200k" style token counts.
pub(super) fn tokens(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = n as f64;
    if n >= 1_000_000 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if n >= 1000 {
        format!("{:.1}k", value / 1000.0)
    } else {
        n.to_string()
    }
}

pub(super) fn schedule_row(s: &ScheduledPrompt, now: i64) -> ScheduleRow {
    let (kind, every_min, at) = match &s.kind {
        ScheduleKind::Interval { seconds } => (
            "interval",
            i32::try_from(seconds / 60).unwrap_or(i32::MAX),
            String::new(),
        ),
        ScheduleKind::Daily { minute_of_day, .. } => (
            "daily",
            0,
            format!("{:02}:{:02}", minute_of_day / 60, minute_of_day % 60),
        ),
    };
    let next_in = s.next_run.map_or(-1, |next| {
        i32::try_from(((next - now).max(0) + 59_999) / 60_000).unwrap_or(i32::MAX)
    });
    ScheduleRow {
        id: sv(&s.id),
        title: sv(&s.title),
        prompt: sv(&s.prompt),
        enabled: s.enabled,
        kind: sv(kind),
        every_min,
        at: sv(&at),
        next_in,
        last: sv(s.last_result.as_deref().unwrap_or_default()),
    }
}
