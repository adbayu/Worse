#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn open() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    migrate(&c).unwrap();
    c
}

fn session() -> SessionId {
    SessionId::new("sess-1")
}

#[test]
fn tool_results_make_a_run_resumable() {
    let c = open();
    let turn = TurnId::new("turn-1");
    begin(
        &c,
        &session(),
        &turn,
        &AgentId::new("claude-code"),
        "do it",
        1,
    )
    .unwrap();
    step(&c, &turn, "started", None, None, None, None, 2).unwrap();
    mark_orphans(&c).unwrap();
    assert!(offer(&c, &session()).unwrap().is_none());

    let turn = TurnId::new("turn-6");
    begin(
        &c,
        &session(),
        &turn,
        &AgentId::new("claude-code"),
        "do it",
        1,
    )
    .unwrap();
    step(
        &c,
        &turn,
        "tool_call",
        Some("t1"),
        Some("read a"),
        None,
        None,
        3,
    )
    .unwrap();
    step(
        &c,
        &turn,
        "tool_result",
        Some("t1"),
        Some("read a"),
        None,
        Some("ok"),
        4,
    )
    .unwrap();
    mark_orphans(&c).unwrap();
    let run = offer(&c, &session()).unwrap().unwrap();
    assert_eq!(run.steps, 1);
    assert_eq!(run.last_step.as_deref(), Some("read a"));
    assert_eq!(run.user_text, "do it");
    assert!(!run.resumed);
    mark_resumed(&c, &turn, 5).unwrap();
    assert!(offer(&c, &session()).unwrap().unwrap().resumed);
}

#[test]
fn partial_updates_inherit_the_tool_title_and_input() {
    let c = open();
    let turn = TurnId::new("turn-9");
    begin(&c, &session(), &turn, &AgentId::new("claude-code"), "x", 1).unwrap();
    step(
        &c,
        &turn,
        "tool_call",
        Some("t1"),
        Some("Read README.md"),
        Some("{}"),
        None,
        2,
    )
    .unwrap();
    step(
        &c,
        &turn,
        "tool_call",
        Some("t2"),
        Some("ping"),
        None,
        None,
        3,
    )
    .unwrap();
    step(
        &c,
        &turn,
        "tool_result",
        Some("t1"),
        Some(""),
        None,
        None,
        4,
    )
    .unwrap();
    let rows = steps(&c, &turn).unwrap();
    let last = rows.last().unwrap();
    assert_eq!(last.tool_title.as_deref(), Some("Read README.md"));
    assert_eq!(last.tool_input.as_deref(), Some("{}"));
    assert_eq!(last.summary(), "3. Read README.md -> -");
}

#[test]
fn finished_and_discarded_runs_are_not_offered() {
    let c = open();
    let turn = TurnId::new("turn-2");
    begin(&c, &session(), &turn, &AgentId::new("codex"), "x", 1).unwrap();
    step(
        &c,
        &turn,
        "tool_result",
        Some("t1"),
        Some("shell"),
        None,
        Some("out"),
        2,
    )
    .unwrap();
    mark_orphans(&c).unwrap();
    assert!(offer(&c, &session()).unwrap().is_some());
    discard(&c, &session()).unwrap();
    assert!(offer(&c, &session()).unwrap().is_none());

    let turn = TurnId::new("turn-3");
    begin(&c, &session(), &turn, &AgentId::new("codex"), "x", 3).unwrap();
    step(
        &c,
        &turn,
        "tool_result",
        Some("t1"),
        Some("shell"),
        None,
        Some("out"),
        4,
    )
    .unwrap();
    finish(&c, &turn, 5).unwrap();
    mark_orphans(&c).unwrap();
    assert!(offer(&c, &session()).unwrap().is_none());
}

fn result(c: &Connection, turn: &TurnId, at: i64) {
    step(
        c,
        turn,
        "tool_result",
        Some("t1"),
        Some("shell"),
        None,
        Some("out"),
        at,
    )
    .unwrap();
}

#[test]
fn only_the_newest_run_of_a_session_is_offered() {
    let c = open();
    let old = TurnId::new("turn-old");
    begin(&c, &session(), &old, &AgentId::new("codex"), "old", 1).unwrap();
    result(&c, &old, 2);
    c.execute(
        "INSERT INTO runs(turn_id,session_id,agent,user_text,state,started_at)
         VALUES('turn-new','sess-1','codex','new','running',10)",
        [],
    )
    .unwrap();
    result(&c, &TurnId::new("turn-new"), 11);
    mark_orphans(&c).unwrap();
    let run = offer(&c, &session()).unwrap().unwrap();
    assert_eq!(run.user_text, "new");
    discard(&c, &session()).unwrap();
    mark_orphans(&c).unwrap();
    assert!(offer(&c, &session()).unwrap().is_none());
}

#[test]
fn a_new_turn_closes_the_pending_offer() {
    let c = open();
    let old = TurnId::new("turn-a");
    begin(&c, &session(), &old, &AgentId::new("codex"), "a", 1).unwrap();
    result(&c, &old, 2);
    mark_orphans(&c).unwrap();
    assert!(offer(&c, &session()).unwrap().is_some());
    let new = TurnId::new("turn-b");
    begin(&c, &session(), &new, &AgentId::new("codex"), "b", 3).unwrap();
    finish(&c, &new, 4).unwrap();
    mark_orphans(&c).unwrap();
    assert!(offer(&c, &session()).unwrap().is_none());
}

#[test]
fn begin_clears_previous_steps_of_the_same_turn() {
    let c = open();
    let turn = TurnId::new("turn-4");
    begin(&c, &session(), &turn, &AgentId::new("codex"), "x", 1).unwrap();
    step(
        &c,
        &turn,
        "tool_result",
        Some("t1"),
        Some("shell"),
        None,
        Some("out"),
        2,
    )
    .unwrap();
    begin(&c, &session(), &turn, &AgentId::new("codex"), "x", 3).unwrap();
    assert_eq!(steps(&c, &turn).unwrap().len(), 0);
}

#[test]
fn long_tool_text_is_capped() {
    let c = open();
    let turn = TurnId::new("turn-5");
    begin(&c, &session(), &turn, &AgentId::new("codex"), "x", 1).unwrap();
    let long = "a".repeat(TEXT_CAP * 2);
    step(
        &c,
        &turn,
        "tool_result",
        Some("t1"),
        Some("read"),
        Some(&long),
        Some(&long),
        2,
    )
    .unwrap();
    let rows = steps(&c, &turn).unwrap();
    assert!(rows[0].tool_input.as_ref().unwrap().chars().count() <= TEXT_CAP + 1);
}

#[test]
fn tool_status_maps_to_step_state() {
    assert_eq!(step_state(Some(ToolCallStatus::Completed)), "tool_result");
    assert_eq!(step_state(Some(ToolCallStatus::Failed)), "tool_result");
    assert_eq!(step_state(Some(ToolCallStatus::InProgress)), "tool_call");
    assert_eq!(step_state(None), "tool_call");
}
