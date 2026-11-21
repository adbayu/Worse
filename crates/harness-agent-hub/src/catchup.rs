//! Seed and catch-up context for agents that missed turns (spec §7).

use std::fmt::Write as _;

/// History budget in characters (~30k tokens).
pub const HISTORY_BUDGET_CHARS: usize = 120_000;
/// Per-field length of a compacted (older) turn line.
pub const COMPACT_FIELD_CHARS: usize = 160;
/// Output of one tool call in a failover or verbatim turn block.
pub const TOOL_OUTPUT_CHARS: usize = 1_500;
/// Input (arguments) of one tool call: flattened to one line.
pub const TOOL_INPUT_CHARS: usize = 300;
/// Total budget of one tool digest list (failover block, or all tools of a turn).
pub const TOOL_BLOCK_BUDGET_CHARS: usize = 24_000;
/// Total budget of the `tools:` section of one verbatim turn.
pub const TURN_TOOL_BUDGET_CHARS: usize = 4_000;
/// Output of one tool call inside a verbatim turn.
pub const TURN_TOOL_OUTPUT_CHARS: usize = 400;
const CONDENSED_USER_CHARS: usize = 600;
const CONDENSED_ASSISTANT_CHARS: usize = 1_500;
const CONDENSED_TOOL_TITLES: usize = 12;

/// What an interrupted (or previous) agent already ran or read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolDigest {
    /// Display title of the call (`Read src/lib.rs`).
    pub title: String,
    /// Arguments as text (JSON or plain).
    pub input: String,
    /// Output the agent already saw.
    pub output: String,
}

/// One earlier turn as context.
#[derive(Debug, Clone, Default)]
pub struct ContextTurn {
    pub seq: u32,
    /// Display name of the agent that answered.
    pub agent: String,
    pub user: String,
    pub assistant: String,
    /// Tool calls of the turn (digested).
    pub tools: Vec<ToolDigest>,
}

fn full(turn: &ContextTurn) -> String {
    let mut out = format!(
        "[turn {}] user:\n{}\n[turn {}] assistant ({}):\n{}\n",
        turn.seq, turn.user, turn.seq, turn.agent, turn.assistant
    );
    if !turn.tools.is_empty() {
        let _ = write!(
            out,
            "[turn {}] tools:\n{}",
            turn.seq,
            render_tools(&turn.tools, TURN_TOOL_BUDGET_CHARS, TURN_TOOL_OUTPUT_CHARS)
        );
    }
    out
}

fn clip(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut out: String = flat.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Clips `text` to `max` characters, keeping its head and its tail.
#[must_use]
pub fn clip_head_tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let head_len = max / 2;
    let tail_len = max - head_len;
    let head: String = text.chars().take(head_len).collect();
    let tail: String = text.chars().skip(count - tail_len).collect();
    let omitted = count - head_len - tail_len;
    format!("{head}\n… ({omitted} chars omitted) …\n{tail}")
}

fn title_of(tool: &ToolDigest) -> &str {
    let title = tool.title.trim();
    if title.is_empty() {
        "tool call"
    } else {
        title
    }
}

fn render_tool(tool: &ToolDigest, output_chars: usize) -> String {
    let mut out = format!("- {}", title_of(tool));
    let input = clip(&tool.input, TOOL_INPUT_CHARS);
    if !input.is_empty() {
        let _ = write!(out, "\n  input: {input}");
    }
    let output = tool.output.trim();
    if !output.is_empty() {
        let _ = write!(out, "\n  output:\n{}", clip_head_tail(output, output_chars));
    }
    out.push('\n');
    out
}

fn render_tool_title(tool: &ToolDigest) -> String {
    format!("- {}\n", title_of(tool))
}

/// Renders tool digests within `budget`: the newest calls keep their output, older ones fall back
/// to the title only. The output keeps the original call order.
#[must_use]
pub fn render_tools(tools: &[ToolDigest], budget: usize, output_chars: usize) -> String {
    let mut rendered: Vec<String> = Vec::with_capacity(tools.len());
    let mut used = 0usize;
    for tool in tools.iter().rev() {
        let detailed = render_tool(tool, output_chars);
        if used + detailed.len() <= budget {
            used += detailed.len();
            rendered.push(detailed);
            continue;
        }
        let line = render_tool_title(tool);
        if used + line.len() > budget {
            break;
        }
        used += line.len();
        rendered.push(line);
    }
    rendered.reverse();
    rendered.concat()
}

fn condensed(turn: &ContextTurn) -> String {
    let mut out = format!(
        "[turn {}, {}] user: {}\nassistant: {}\n",
        turn.seq,
        turn.agent,
        clip(&turn.user, CONDENSED_USER_CHARS),
        clip_head_tail(turn.assistant.trim(), CONDENSED_ASSISTANT_CHARS)
    );
    if !turn.tools.is_empty() {
        let titles: Vec<&str> = turn
            .tools
            .iter()
            .take(CONDENSED_TOOL_TITLES)
            .map(title_of)
            .collect();
        let more = turn.tools.len().saturating_sub(CONDENSED_TOOL_TITLES);
        let _ = write!(out, "tools: {}", titles.join("; "));
        if more > 0 {
            let _ = write!(out, " (+{more} more)");
        }
        out.push('\n');
    }
    out
}

fn compact(turn: &ContextTurn) -> String {
    format!(
        "[turn {}, {}] user: {} / assistant: {}\n",
        turn.seq,
        turn.agent,
        clip(&turn.user, COMPACT_FIELD_CHARS),
        clip(&turn.assistant, COMPACT_FIELD_CHARS)
    )
}

/// Renders turns within `budget`: the newest verbatim (up to 60% of the budget, or the whole budget for the
/// newest turn alone), then condensed turns (user request, head and tail of the answer, tool titles) up to
/// 85%, then one-line turns; the oldest are dropped with a note.
#[must_use]
pub fn render_history(turns: &[ContextTurn], budget: usize) -> String {
    type Tier = (fn(&ContextTurn) -> String, usize);
    let tiers: [Tier; 3] = [
        (full, budget / 10 * 6),
        (condensed, budget / 100 * 85),
        (compact, budget),
    ];
    let mut parts: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut next = turns.len();
    for (render, cap) in tiers {
        while next > 0 {
            let text = render(&turns[next - 1]);
            let first_fits = parts.is_empty() && used + text.len() <= budget;
            if used + text.len() > cap && !first_fits {
                break;
            }
            used += text.len();
            parts.push(text);
            next -= 1;
        }
    }
    let mut out = String::new();
    if next > 0 {
        let _ = writeln!(out, "({next} earlier turns omitted)");
    }
    for text in parts.iter().rev() {
        out.push_str(text);
    }
    out
}

/// Seed for a new native session: every earlier turn of the Harness session.
#[must_use]
pub fn seed_block(turns: &[ContextTurn]) -> Option<String> {
    if turns.is_empty() {
        return None;
    }
    Some(format!(
        "<harness-conversation kind=\"seed\">\nThis Harness conversation was started before you joined; other agents may have answered earlier turns. Conversation so far:\n{}</harness-conversation>",
        render_history(turns, HISTORY_BUDGET_CHARS)
    ))
}

/// Catch-up for an existing native session: turns answered by other agents since this agent's last turn.
#[must_use]
pub fn catchup_block(turns: &[ContextTurn]) -> Option<String> {
    if turns.is_empty() {
        return None;
    }
    Some(format!(
        "<harness-conversation kind=\"catch-up\">\nWhile you were away, other agents handled these turns of the same conversation:\n{}</harness-conversation>",
        render_history(turns, HISTORY_BUDGET_CHARS)
    ))
}

/// Labelled partial output of an agent that hit its usage limit during this turn (spec §6.4).
#[must_use]
pub fn partial_block(agent: &str, partial_text: &str, tools: &[ToolDigest]) -> String {
    let tools = if tools.is_empty() {
        String::from("(none)")
    } else {
        render_tools(tools, TOOL_BLOCK_BUDGET_CHARS, TOOL_OUTPUT_CHARS)
    };
    format!(
        "<harness-failover from=\"{agent}\">\nPartial output from {agent} before its usage limit was reached (it did not finish this turn; continue the user's request below):\n{partial_text}\nTool calls already made by {agent}:\n{tools}The results above are already known: do not run those tool calls again unless their output may be stale.\n</harness-failover>"
    )
}

#[cfg(test)]
mod tests;
