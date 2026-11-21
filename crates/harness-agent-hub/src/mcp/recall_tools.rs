use super::{recall, session_notes, text_result, tool_error, Path, Value};

/// One memory as a result line: id, kind, age, who wrote it, text.
fn memory_line(memory: &recall::Memory, now: i64) -> String {
    use std::fmt::Write as _;
    let mut head = format!(
        "[{}] {} · {}",
        memory.id,
        memory.kind.as_str(),
        recall::age(now, memory.at)
    );
    if let Some(agent) = &memory.agent {
        let _ = write!(head, " · {agent}");
    }
    if memory.project.is_none() {
        head.push_str(" · global");
    }
    if memory.pinned {
        head.push_str(" · pinned");
    }
    if memory.kind == recall::Kind::Episode {
        if let Some(session) = &memory.session {
            let _ = write!(head, " · session {session}");
        }
        if let Some(title) = &memory.title {
            let _ = write!(head, " · \"{title}\"");
        }
    }
    format!("{head}\n{}", memory.text)
}

/// What the recall tools know about their caller.
pub(super) struct RecallCall<'a> {
    pub(super) store: recall::Store,
    pub(super) home: &'a Path,
    /// [`recall::project_key`] of the session's workspace.
    pub(super) project: Option<String>,
    pub(super) session: Option<&'a str>,
    pub(super) agent: Option<&'a str>,
    pub(super) now: i64,
}

fn limit_of(args: &Value, default: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(default)
        .clamp(1, 20)
}

pub(super) fn recall_search(call: &RecallCall<'_>, args: &Value) -> Value {
    let Some(query) = args.get("query").and_then(Value::as_str) else {
        return tool_error("query is required");
    };
    let kind = match args.get("kind").and_then(Value::as_str) {
        None => None,
        Some("episode") => Some(recall::Kind::Episode),
        Some(other) => match recall::Kind::parse(other) {
            Some(kind) => Some(kind),
            None => return tool_error("kind must be fact, preference, decision, howto or episode"),
        },
    };
    let all = call.store.load();
    let seen = recall::visible(&all, call.project.as_deref());
    let hits = recall::search(
        &seen,
        &recall::Query {
            text: query,
            limit: limit_of(args, 8),
            need: 1,
            kind,
        },
    );
    if hits.is_empty() {
        return text_result(format!(
            "No memory matches \"{query}\" ({} stored for this project and global). Try other words, or memory_recent.",
            seen.len()
        ));
    }
    let lines: Vec<String> = hits
        .iter()
        .map(|hit| memory_line(hit.memory, call.now))
        .collect();
    text_result(lines.join("\n\n"))
}

pub(super) fn recall_add(call: &RecallCall<'_>, args: &Value) -> Value {
    let Some(text) = args.get("text").and_then(Value::as_str) else {
        return tool_error("text is required");
    };
    let kind = match args.get("kind").and_then(Value::as_str) {
        None => None,
        Some(other) => match recall::Kind::parse(other) {
            Some(kind) => Some(kind),
            None => return tool_error("kind must be fact, preference, decision or howto"),
        },
    };
    let project = match args.get("scope").and_then(Value::as_str) {
        Some("global") => None,
        Some("project") | None => match &call.project {
            Some(key) => Some(key.clone()),
            None => {
                return tool_error("no project configured for this session: use scope \"global\"")
            }
        },
        Some(_) => return tool_error("scope must be \"project\" or \"global\""),
    };
    let new = recall::NewMemory {
        kind,
        text: text.to_owned(),
        project,
        pinned: args.get("pinned").and_then(Value::as_bool),
        session: call.session.map(str::to_owned),
        agent: call.agent.map(str::to_owned),
        replaces: args
            .get("replaces")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ttl_days: args
            .get("ttl_days")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
    };
    match call.store.add(new) {
        Ok(recall::Added::New(id)) => text_result(format!("saved as {id}")),
        Ok(recall::Added::Duplicate(id)) => {
            text_result(format!("already stored as {id} (nothing added)"))
        }
        Err(message) => tool_error(&message),
    }
}

pub(super) fn recall_forget(call: &RecallCall<'_>, args: &Value) -> Value {
    let Some(id) = args.get("id").and_then(Value::as_str) else {
        return tool_error("id is required");
    };
    match call.store.forget(id) {
        Ok(0) => tool_error(&format!("no memory with id {id}")),
        Ok(_) => text_result("forgotten"),
        Err(e) => tool_error(&format!("write failed: {e}")),
    }
}

pub(super) fn recall_recent(call: &RecallCall<'_>, args: &Value) -> Value {
    let all = call.store.load();
    let mut seen = recall::visible(&all, call.project.as_deref());
    seen.retain(|m| m.kind != recall::Kind::Episode || m.session.as_deref() != call.session);
    seen.sort_by_key(|m| std::cmp::Reverse(m.at));
    if seen.is_empty() {
        return text_result("(the local memory is empty for this project)");
    }
    let lines: Vec<String> = seen
        .iter()
        .take(limit_of(args, 8))
        .map(|m| memory_line(m, call.now))
        .collect();
    text_result(lines.join("\n\n"))
}

pub(super) fn recall_session(call: &RecallCall<'_>, args: &Value) -> Value {
    let Some(wanted) = args.get("session").and_then(Value::as_str) else {
        return tool_error("session is required");
    };
    let all = call.store.load();
    let seen = recall::visible(&all, call.project.as_deref());
    let Some(episode) = seen.iter().find(|m| {
        m.kind == recall::Kind::Episode && (m.session.as_deref() == Some(wanted) || m.id == wanted)
    }) else {
        return tool_error(&format!(
            "no earlier session {wanted} in this project's memory (memory_recent lists them)"
        ));
    };
    let id = episode.session.clone().unwrap_or_default();
    let notes = session_notes::read(call.home, &id);
    let notes = if notes.trim().is_empty() {
        "(that session kept no working notes)".to_owned()
    } else {
        notes
    };
    text_result(format!(
        "{}\n\nWorking notes of that session:\n{notes}",
        memory_line(episode, call.now)
    ))
}
