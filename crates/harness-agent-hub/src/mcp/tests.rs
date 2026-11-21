use super::*;

fn serve_lines(home: &Path, session: Option<&str>, lines: &[&str]) -> Vec<Value> {
    serve_in(home, None, session, None, lines)
}

fn serve_in(
    home: &Path,
    project: Option<&Path>,
    session: Option<&str>,
    agent: Option<&str>,
    lines: &[&str],
) -> Vec<Value> {
    let memory = MemoryFiles::new(home);
    let input = lines.join("\n");
    let mut output = Vec::new();
    serve(
        &memory,
        home,
        project,
        session,
        agent,
        std::io::Cursor::new(input.into_bytes()),
        &mut output,
    )
    .unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect()
}

fn tool_text(reply: &Value) -> &str {
    reply["result"]["content"][0]["text"].as_str().unwrap()
}

#[test]
fn notes_tools_round_trip_with_a_session() {
    let home = std::env::temp_dir().join(format!("harness-mcp-notes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let replies = serve_lines(
        &home,
        Some("ses-1"),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"session_notes_read","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"session_notes_write","arguments":{"content":"files: mcp.rs:111"}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"session_notes_read","arguments":{}}}"#,
        ],
    );
    let names: Vec<&str> = replies[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "memory_read",
            "memory_write",
            "session_notes_read",
            "session_notes_write",
            "memory_search",
            "memory_add",
            "memory_forget",
            "memory_recent",
            "memory_session"
        ]
    );
    assert_eq!(tool_text(&replies[1]), "(no notes yet)");
    assert_eq!(tool_text(&replies[2]), "ok");
    assert_eq!(tool_text(&replies[3]), "files: mcp.rs:111\n");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn notes_tools_without_a_session_say_so() {
    let home = std::env::temp_dir().join(format!("harness-mcp-nosession-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let replies = serve_lines(
        &home,
        None,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"session_notes_read","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"session_notes_write","arguments":{"content":"x"}}}"#,
        ],
    );
    for reply in &replies {
        assert_eq!(reply["result"]["isError"], true);
        assert!(tool_text(reply).contains("HARNESS_SESSION_ID"), "{reply}");
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[allow(clippy::needless_pass_by_value)]
fn call(id: u32, name: &str, arguments: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
        .to_string()
}

/// One agent saves in one session; another agent in a later session of the same project finds it, reads
/// the earlier session, corrects the memory and forgets it. A session of another project sees nothing.
#[test]
#[allow(clippy::too_many_lines)]
fn recall_tools_carry_memories_across_sessions_and_agents() {
    let home = std::env::temp_dir().join(format!(
        "harness-mcp-recall-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let project = Path::new("/work/demo");
    // Session 1, Claude Code.
    let lines = [
        call(
            1,
            "memory_add",
            json!({"text": "Pairing tokens expire after ten minutes because of replay attacks", "kind": "decision"}),
        ),
        call(
            2,
            "memory_add",
            json!({"text": "Reply in Indonesian, casual", "kind": "preference", "scope": "global"}),
        ),
        call(
            3,
            "memory_add",
            json!({"text": "pairing tokens expire after ten minutes because of  replay attacks"}),
        ),
        call(
            4,
            "memory_add",
            json!({"text": "the deploy key is ghp_abcdefghijklmnopqrstuvwxyz0123456789"}),
        ),
        call(
            5,
            "session_notes_write",
            json!({"content": "auth.rs:40 is_live reads the clock twice"}),
        ),
    ];
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let first = serve_in(
        &home,
        Some(project),
        Some("ses-1"),
        Some("Claude Code"),
        &lines,
    );
    assert!(tool_text(&first[0]).starts_with("saved as m"));
    assert!(tool_text(&first[2]).starts_with("already stored as m"));
    assert_eq!(first[3]["result"]["isError"], true);
    assert!(tool_text(&first[3]).contains("Never store credentials"));
    crate::recall::Store::new(&home)
        .put_episode(
            "ses-1",
            "/work/demo",
            "Pairing expiry",
            "Claude Code",
            "Asked: fix the pairing expiry | Result: fixed",
        )
        .unwrap();
    let decision = tool_text(&first[0])
        .trim_start_matches("saved as ")
        .to_owned();

    // Session 2, Codex, same project.
    let lines = [
        call(
            1,
            "memory_search",
            json!({"query": "why do pairing tokens expire"}),
        ),
        call(2, "memory_recent", json!({})),
        call(3, "memory_session", json!({"session": "ses-1"})),
        call(
            4,
            "memory_add",
            json!({"text": "Pairing tokens expire after five minutes since v0.2", "kind": "decision", "replaces": decision}),
        ),
        call(
            5,
            "memory_search",
            json!({"query": "pairing tokens expire"}),
        ),
        call(6, "memory_search", json!({"query": "kubernetes ingress"})),
    ];
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let second = serve_in(&home, Some(project), Some("ses-2"), Some("Codex"), &lines);
    let found = tool_text(&second[0]);
    assert!(
        found.contains("decision")
            && found.contains("Claude Code")
            && found.contains("replay attacks"),
        "{found}"
    );
    assert!(
        found.contains("session ses-1"),
        "the episode of the earlier session is found too: {found}"
    );
    let recent = tool_text(&second[1]);
    assert!(
        recent.contains("Reply in Indonesian") && recent.contains("global · pinned"),
        "{recent}"
    );
    let earlier = tool_text(&second[2]);
    assert!(
        earlier.contains("\"Pairing expiry\"")
            && earlier.contains("auth.rs:40 is_live reads the clock twice"),
        "{earlier}"
    );
    let after = tool_text(&second[4]);
    assert!(
        after.contains("five minutes") && after.contains("Codex") && !after.contains("ten minutes"),
        "{after}"
    );
    assert!(tool_text(&second[5]).starts_with("No memory matches"));

    // Another project: only the global preference.
    let lines = [
        call(1, "memory_search", json!({"query": "pairing tokens"})),
        call(2, "memory_recent", json!({})),
    ];
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let other = serve_in(
        &home,
        Some(Path::new("/work/other")),
        Some("ses-3"),
        Some("Codex"),
        &lines,
    );
    assert!(tool_text(&other[0]).starts_with("No memory matches"));
    assert!(
        tool_text(&other[1]).contains("Reply in Indonesian")
            && !tool_text(&other[1]).contains("Pairing")
    );

    // Forget.
    let id = tool_text(&second[3])
        .trim_start_matches("saved as ")
        .to_owned();
    let lines = [
        call(1, "memory_forget", json!({"id": id})),
        call(2, "memory_forget", json!({"id": "m-unknown"})),
    ];
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let gone = serve_in(&home, Some(project), Some("ses-2"), Some("Codex"), &lines);
    assert_eq!(tool_text(&gone[0]), "forgotten");
    assert_eq!(gone[1]["result"]["isError"], true);
    let _ = std::fs::remove_dir_all(&home);
}
