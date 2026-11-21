#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

fn frame(event: &str, data: &str) -> Frame {
    Frame {
        event: event.to_owned(),
        data: data.to_owned(),
    }
}

#[test]
fn sse_splits_frames_across_chunks() {
    let mut sse = Sse::default();
    assert_eq!(sse.feed(b"data: {\"a\":").len(), 0);
    assert_eq!(sse.feed(b"1}\n\n"), vec![frame("", "{\"a\":1}")]);
    // Named event, comment line, CRLF endings and two frames in one chunk.
    let out = sse.feed(b": keepalive\n\nevent: hermes.tool.progress\r\ndata: {\"tool\":\"terminal\"}\r\n\r\ndata: [DONE]\n\n");
    assert_eq!(
        out,
        vec![
            frame("hermes.tool.progress", "{\"tool\":\"terminal\"}"),
            frame("", "[DONE]")
        ]
    );
    // The event name does not leak into the next frame.
    assert_eq!(sse.feed(b"data: x\n\n"), vec![frame("", "x")]);
}

#[test]
fn sse_joins_multi_line_data_and_ignores_other_fields() {
    let mut sse = Sse::default();
    let out = sse.feed(b"id: 7\nretry: 100\ndata: one\ndata: two\n\n");
    assert_eq!(out, vec![frame("", "one\ntwo")]);
}

#[test]
fn tool_kinds() {
    assert_eq!(tool_kind("terminal"), "execute");
    assert_eq!(tool_kind("write_file"), "edit");
    assert_eq!(tool_kind("patch"), "edit");
    assert_eq!(tool_kind("search_files"), "search");
    assert_eq!(tool_kind("read_file"), "read");
    assert_eq!(tool_kind("web_extract"), "fetch");
    assert_eq!(tool_kind("delegate_task"), "think");
    assert_eq!(tool_kind("vision_analyze"), "other");
}

#[test]
fn approval_options_follow_the_gateway() {
    let ids = |choices: &[&str]| -> Vec<String> {
        approval_options(&choices.iter().map(|c| (*c).to_owned()).collect::<Vec<_>>())
            .iter()
            .map(|o| {
                format!(
                    "{}:{}",
                    o["optionId"].as_str().unwrap(),
                    o["kind"].as_str().unwrap()
                )
            })
            .collect()
    };
    assert_eq!(
        ids(&["once", "session", "always", "deny"]),
        [
            "once:allow_once",
            "session:allow_always",
            "always:allow_always",
            "deny:reject_once"
        ]
    );
    assert_eq!(
        ids(&["once", "deny"]),
        ["once:allow_once", "deny:reject_once"]
    );
    // Nothing usable: the two safe choices.
    assert_eq!(ids(&[]), ["once:allow_once", "deny:reject_once"]);
    assert_eq!(ids(&["weird"]), ["once:allow_once", "deny:reject_once"]);
}

#[test]
fn capabilities_answers() {
    assert!(is_capabilities(
        200,
        &json!({"object": "hermes.api_server.capabilities", "platform": "hermes-agent"})
    ));
    assert!(!is_capabilities(
        200,
        &json!({"object": "list", "data": []})
    ));
    assert!(!is_capabilities(404, &Value::Null));
    assert!(is_capabilities(
        401,
        &json!({"error": {"code": "gateway_auth_failed"}})
    ));
    assert!(!is_capabilities(
        401,
        &json!({"error": {"code": "invalid_api_key"}})
    ));
}
