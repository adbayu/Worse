use super::*;

/// " [Picked by the decision model: <agent>, model <m>: <reason>]" when Harness's decision model chose the
/// subagent of a `delegate` call that named none; empty otherwise.
pub(super) fn routed_note(result: &Value) -> String {
    let route = &result["routedBy"];
    if route["decisionModel"].as_bool() != Some(true) {
        return String::new();
    }
    let mut note = "\n\n[Picked by the decision model".to_owned();
    if let Some(model) = route["model"].as_str().filter(|m| !m.is_empty()) {
        note.push_str(", model ");
        note.push_str(model);
    }
    if let Some(reason) = route["reason"].as_str().filter(|r| !r.is_empty()) {
        note.push_str(": ");
        note.push_str(reason);
    }
    note.push(']');
    note
}

/// Header line + answer of one finished subagent turn (blocking `delegate` and `subagent_wait`).
pub(super) fn answer(result: &Value) -> (String, bool) {
    let outcome = result["outcome"].as_str().unwrap_or("failed");
    let mut text = format!(
        "subagent {} ({}) {outcome}",
        result["sessionId"].as_str().unwrap_or(""),
        result["agentName"].as_str().unwrap_or("")
    );
    if let Some(error) = result["error"].as_str().filter(|e| !e.is_empty()) {
        text.push_str(": ");
        text.push_str(error);
    }
    text.push_str("\n\n");
    text.push_str(result["text"].as_str().unwrap_or(""));
    (text, outcome != "done")
}

pub(super) fn message_text(result: &Value) -> String {
    let id = result["sessionId"].as_str().unwrap_or("");
    if result["interrupted"].as_bool() == Some(true) {
        format!(
            "Stopped subagent {id}'s turn and sent the message. Its reply is delivered to you as a new message."
        )
    } else if result["queued"].as_bool() == Some(true) {
        format!(
            "Queued for subagent {id}; it runs after the current turn. Its reply is delivered to you as a new message."
        )
    } else {
        format!("Sent to subagent {id}. Its reply is delivered to you as a new message.")
    }
}

pub(super) fn wait_text(result: &Value) -> Value {
    let mut out = String::from("Subagents:\n");
    for v in result["subagents"].as_array().into_iter().flatten() {
        let activity = v["activity"].as_str().unwrap_or("");
        let _ = writeln!(
            out,
            "- {} ({}) {}: {}{}",
            v["sessionId"].as_str().unwrap_or(""),
            v["agentName"].as_str().unwrap_or(""),
            v["title"].as_str().unwrap_or(""),
            v["status"].as_str().unwrap_or(""),
            if activity.is_empty() {
                String::new()
            } else {
                format!(" — {activity}")
            }
        );
    }
    let notes = result["notes"].as_array().cloned().unwrap_or_default();
    let finished = result["finished"].as_array().cloned().unwrap_or_default();
    if notes.is_empty() && finished.is_empty() {
        out.push_str("\nNo new answers or notes yet.");
    }
    for n in &notes {
        let _ = write!(
            out,
            "\nNote from {} ({}):\n{}\n",
            n["sessionId"].as_str().unwrap_or(""),
            n["title"].as_str().unwrap_or(""),
            n["text"].as_str().unwrap_or("")
        );
    }
    for f in &finished {
        out.push_str("\n---\n");
        out.push_str(&answer(f).0);
        out.push('\n');
    }
    tool_text(&out, false)
}

pub(super) fn claim_text(result: &Value) -> Value {
    let list = |key: &str| result[key].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    let granted: Vec<String> = list("granted")
        .iter()
        .filter_map(|p| p.as_str().map(str::to_owned))
        .collect();
    if !granted.is_empty() {
        let _ = writeln!(out, "Granted: {}", granted.join(", "));
    }
    let conflicts = list("conflicts");
    for c in &conflicts {
        let _ = writeln!(
            out,
            "CONFLICT: {} is held by {} ({}, {}) via {}{} — do not edit it; coordinate first.",
            c["requested"].as_str().unwrap_or(""),
            c["sessionId"].as_str().unwrap_or(""),
            c["title"].as_str().unwrap_or(""),
            c["agent"].as_str().unwrap_or(""),
            c["path"].as_str().unwrap_or(""),
            c["note"]
                .as_str()
                .filter(|n| !n.is_empty())
                .map(|n| format!(": {n}"))
                .unwrap_or_default()
        );
    }
    out.push_str("Current claims in this workspace:\n");
    let claims = list("claims");
    if claims.is_empty() {
        out.push_str("(none)\n");
    }
    for c in &claims {
        let _ = writeln!(
            out,
            "- {} — {} ({})",
            c["path"]
                .as_str()
                .map_or("", |p| if p.is_empty() { "(whole workspace)" } else { p }),
            c["title"].as_str().unwrap_or(""),
            c["sessionId"].as_str().unwrap_or("")
        );
    }
    tool_text(&out, !conflicts.is_empty())
}
