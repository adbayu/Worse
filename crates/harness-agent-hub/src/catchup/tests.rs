#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn digest(title: &str, output: &str) -> ToolDigest {
    ToolDigest {
        title: title.to_owned(),
        input: String::from("{}"),
        output: output.to_owned(),
    }
}

#[test]
fn head_and_tail_survive_multibyte_clipping() {
    let text = "汉".repeat(3_000);
    let clipped = clip_head_tail(&text, TOOL_OUTPUT_CHARS);
    assert!(clipped.contains("(1500 chars omitted)"), "{clipped}");
    assert!(clipped.starts_with('汉'));
    assert!(clipped.ends_with('汉'));
    let kept = clipped.matches('汉').count();
    assert_eq!(kept, TOOL_OUTPUT_CHARS);
    // Short text is untouched.
    assert_eq!(clip_head_tail("abc", TOOL_OUTPUT_CHARS), "abc");
}

#[test]
fn total_budget_keeps_newest_full_and_older_as_titles() {
    let body = "x".repeat(2_000);
    let tools: Vec<ToolDigest> = (0..10)
        .map(|i| digest(&format!("call-{i}"), &body))
        .collect();
    let rendered = render_tools(&tools, 5_000, TOOL_OUTPUT_CHARS);
    assert!(rendered.contains("call-9"));
    assert!(rendered.contains("output:"));
    assert!(rendered.contains("- call-0\n"), "{rendered}");
    assert!(!rendered.contains("call-0\n  input:"), "{rendered}");
    let first = rendered.find("call-0").unwrap();
    let last = rendered.find("call-9").unwrap();
    assert!(first < last);
    assert!(rendered.len() <= 5_000, "{}", rendered.len());
}

#[test]
fn partial_block_carries_tool_output() {
    let tools = vec![digest("Read src/lib.rs", "pub fn main()")];
    let block = partial_block("Codex", "half an answer", &tools);
    assert!(block.contains("Partial output from Codex"));
    assert!(block.contains("Tool calls already made by Codex"));
    assert!(block.contains("Read src/lib.rs"));
    assert!(block.contains("pub fn main()"));
    assert!(block.contains("do not run those tool calls again"));
    // No tools at all keeps the old wording.
    assert!(partial_block("agy", "x", &[]).contains("(none)"));
}

#[test]
fn older_turns_keep_a_condensed_answer_before_one_line_turns() {
    let answer = format!("{}decided: use folds{}", "a".repeat(400), "b".repeat(6_000));
    let turns: Vec<ContextTurn> = (0..30)
        .map(|seq| ContextTurn {
            seq,
            agent: String::from("Codex"),
            user: format!("request {seq}"),
            assistant: answer.clone(),
            tools: vec![digest(&format!("Read file-{seq}"), "out")],
        })
        .collect();
    let budget = 60_000;
    let text = render_history(&turns, budget);
    assert!(text.len() <= budget, "{}", text.len());
    assert!(text.contains("[turn 29] user:"), "newest turn is verbatim");
    assert!(text.contains("decided: use folds\n") || text.contains("tools: Read file-"));
    assert!(
        text.contains("tools: Read file-"),
        "condensed turns keep tool titles"
    );
    let first = text
        .find("request 0")
        .or_else(|| text.find("earlier turns omitted"));
    assert!(first.is_some());
}

#[test]
fn history_with_tools_stays_within_budget() {
    let body = "y".repeat(20_000);
    let turns: Vec<ContextTurn> = (0..8)
        .map(|seq| ContextTurn {
            seq,
            agent: String::from("Codex"),
            user: String::from("do it"),
            assistant: String::from("done"),
            tools: vec![digest(&format!("shell-{seq}"), &body)],
        })
        .collect();
    let budget = 60_000;
    let text = render_history(&turns, budget);
    assert!(text.len() <= budget, "{}", text.len());
    assert!(text.contains("tools:"));
    assert!(text.contains("chars omitted)"));
    assert!(text.contains("earlier turns omitted") || text.contains("[turn 0]"));
}
