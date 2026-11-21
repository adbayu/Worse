//! Harness MCP memory server (spec §8.3): MCP over stdio, newline-delimited JSON-RPC.

use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::{json, Value};

use crate::memory::{MemoryFiles, MemoryTarget};
use crate::recall;
use crate::session_notes;

mod recall_tools;

use recall_tools::{
    recall_add, recall_forget, recall_recent, recall_search, recall_session, RecallCall,
};

/// MCP protocol version we answer with.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Runs the server until stdin closes.
///
/// `home` is the Harness data dir (session notes, the recall store); `project`, `session` and `agent` (the
/// display name memories are attributed to) are optional so a helper started by hand still answers.
///
/// # Errors
///
/// I/O errors on stdout.
pub fn serve(
    memory: &MemoryFiles,
    home: &Path,
    project: Option<&Path>,
    session: Option<&str>,
    agent: Option<&str>,
    input: impl BufRead,
    mut output: impl Write,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            write_line(
                &mut output,
                &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}),
            )?;
            continue;
        };
        let Some(id) = message.get("id").cloned() else {
            continue; // notification (e.g. notifications/initialized)
        };
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let reply = match method {
            "initialize" => Ok(json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "harness-memory", "version": env!("CARGO_PKG_VERSION")}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_list()})),
            "tools/call" => Ok(call_tool(memory, home, project, session, agent, &params)),
            _ => Err(json!({"code": -32601, "message": format!("method not found: {method}")})),
        };
        let response = match reply {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error}),
        };
        write_line(&mut output, &response)?;
    }
    Ok(())
}

fn write_line(output: &mut impl Write, value: &Value) -> std::io::Result<()> {
    output.write_all(value.to_string().as_bytes())?;
    output.write_all(b"\n")?;
    output.flush()
}

fn tool_list() -> Value {
    let scope = json!({"type":"string","enum":["global","project"],"description":"global = ~/.harness/MEMORY.md shared by all projects; project = <project>/.harness/MEMORY.md"});
    json!([
        {
            "name": "memory_read",
            "description": "Read the shared Harness MEMORY.md (shared by every agent: Claude Code, Codex, agy).",
            "inputSchema": {"type":"object","properties":{"scope":scope},"required":["scope"]}
        },
        {
            "name": "memory_write",
            "description": "Write the shared Harness MEMORY.md. mode=append adds a line, mode=replace overwrites.",
            "inputSchema": {"type":"object","properties":{
                "scope": scope,
                "content": {"type":"string"},
                "mode": {"type":"string","enum":["append","replace"],"default":"append"}
            },"required":["scope","content"]}
        },
        {
            "name": "session_notes_read",
            "description": SESSION_NOTES_READ,
            "inputSchema": {"type":"object","properties":{}}
        },
        {
            "name": "session_notes_write",
            "description": SESSION_NOTES_WRITE,
            "inputSchema": {"type":"object","properties":{
                "content": {
                    "type":"string",
                    "description": format!(
                        "The full notes text (markdown), replacing the previous notes. At most {} characters.",
                        crate::session_notes::MAX_NOTES_CHARS
                    )
                }
            },"required":["content"]}
        },
        {
            "name": "memory_search",
            "description": MEMORY_SEARCH,
            "inputSchema": {"type":"object","properties":{
                "query": {"type":"string","description":"Words to look for (names, identifiers, topics). Not a question to answer."},
                "kind": {"type":"string","enum":["fact","preference","decision","howto","episode"],"description":"Only this kind. episode = digests of earlier sessions."},
                "limit": {"type":"integer","minimum":1,"maximum":20,"default":8}
            },"required":["query"]}
        },
        {
            "name": "memory_add",
            "description": MEMORY_ADD,
            "inputSchema": {"type":"object","properties":{
                "text": {"type":"string","description": format!("One fact in a sentence or two, at most {} characters. No credentials.", crate::recall::MAX_TEXT_CHARS)},
                "kind": {"type":"string","enum":["fact","preference","decision","howto"],"default":"fact"},
                "scope": {"type":"string","enum":["project","global"],"default":"project","description":"project = only sessions in this project see it; global = every project (user preferences)."},
                "pinned": {"type":"boolean","description":"Show it in every new session of its scope, whatever the request. Default: true for preferences, false otherwise."},
                "replaces": {"type":"string","description":"Id of a memory this one corrects or updates; the old one is retired."},
                "ttl_days": {"type":"integer","minimum":1,"description":"Forget it after this many days (temporary facts)."}
            },"required":["text"]}
        },
        {
            "name": "memory_forget",
            "description": "Forget one memory of the Harness local memory by its id (from memory_search or the recall block). Use it when a memory is wrong or no longer true and nothing replaces it.",
            "inputSchema": {"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}
        },
        {
            "name": "memory_recent",
            "description": "The latest sessions of this project (what was asked, how it ended) and the latest saved memories, newest first. Use it to see what was worked on before this session.",
            "inputSchema": {"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":20,"default":8}}}
        },
        {
            "name": "memory_session",
            "description": "Read an earlier Harness session: its digest and its working notes (files with line pointers, findings, decisions). Pass the session id shown by memory_search / memory_recent, or the id of an episode memory.",
            "inputSchema": {"type":"object","properties":{"session":{"type":"string"}},"required":["session"]}
        }
    ])
}

const MEMORY_SEARCH: &str =
    "Search the Harness local memory shared by every agent and session of this user: saved facts, \
preferences, decisions, how-tos and digests of earlier sessions, for this project and global. Word search \
(no embeddings): pass the names and terms that matter. Call it before asking the user to repeat something, \
and when the task refers to earlier work.";

const MEMORY_ADD: &str =
    "Save one durable memory to the Harness local memory, visible to every agent in later sessions. Save \
what a future session would otherwise have to rediscover or ask again: a user preference, a decision and \
its reason, a fact about the project or environment, a procedure that worked. Not for progress of the \
current task (use session_notes_write) and never credentials. The same text is not stored twice; pass \
`replaces` to correct an older memory.";

const SESSION_NOTES_READ: &str =
    "Read the working notes of this Harness session (files with line pointers, findings, decisions, next \
steps). Call it when your context was compacted, after another agent handed the session over, or when you \
are unsure what was already established. The same notes are injected into this prompt at the start of a \
fresh session.";

const SESSION_NOTES_WRITE: &str =
    "Replace the working notes of this Harness session. Call it after each meaningful discovery or \
decision while you work: relevant files with line pointers, root causes, decisions taken, what is already \
done, what is next. They are injected into the prompt when another agent takes the session over (quota \
failover) or a native session starts fresh, so a later agent can continue without re-reading everything. \
Write the complete notes every time (markdown, at most 16k characters: condense instead of growing \
without bound) and never put secrets (API keys, tokens, passwords) in them.";

fn tool_error(message: &str) -> Value {
    json!({"content":[{"type":"text","text":message}],"isError":true})
}

fn text_result(text: impl Into<String>) -> Value {
    json!({"content":[{"type":"text","text":text.into()}]})
}

/// The recall tools (`memory_search`, `memory_add`, `memory_forget`, `memory_recent`, `memory_session`).
fn call_recall(
    name: &str,
    args: &Value,
    home: &Path,
    project: Option<&Path>,
    session: Option<&str>,
    agent: Option<&str>,
) -> Value {
    let call = RecallCall {
        store: recall::Store::new(home),
        home,
        project: project.map(|p| recall::project_key(&p.to_string_lossy())),
        session,
        agent,
        now: recall::now_ms(),
    };
    match name {
        "memory_search" => recall_search(&call, args),
        "memory_add" => recall_add(&call, args),
        "memory_forget" => recall_forget(&call, args),
        "memory_recent" => recall_recent(&call, args),
        "memory_session" => recall_session(&call, args),
        _ => tool_error(&format!("unknown tool: {name}")),
    }
}

fn call_tool(
    memory: &MemoryFiles,
    home: &Path,
    project: Option<&Path>,
    session: Option<&str>,
    agent: Option<&str>,
    params: &Value,
) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    if matches!(
        name,
        "memory_search" | "memory_add" | "memory_forget" | "memory_recent" | "memory_session"
    ) {
        return call_recall(name, &args, home, project, session, agent);
    }
    // 会话笔记工具不需要 scope 参数，先于 memory_read / memory_write 处理。
    if name.starts_with("session_notes_") {
        let Some(session) = session else {
            return tool_error(
                "no Harness session for this helper (no --session / HARNESS_SESSION_ID), \
so this session has no working notes",
            );
        };
        return match name {
            "session_notes_read" => {
                let text = session_notes::read(home, session);
                let text = if text.trim().is_empty() {
                    "(no notes yet)".to_owned()
                } else {
                    text
                };
                json!({"content":[{"type":"text","text":text}]})
            }
            "session_notes_write" => {
                let Some(content) = args.get("content").and_then(Value::as_str) else {
                    return tool_error("content is required (the full notes text)");
                };
                match session_notes::write(home, session, content) {
                    Ok(()) => json!({"content":[{"type":"text","text":"ok"}]}),
                    Err(e) => tool_error(&format!("write failed: {e}")),
                }
            }
            _ => tool_error(&format!("unknown tool: {name}")),
        };
    }
    let target = match args.get("scope").and_then(Value::as_str) {
        Some("global") => MemoryTarget::Global,
        Some("project") => match project {
            Some(root) => MemoryTarget::Project(root.to_path_buf()),
            None => return tool_error("no project configured for this session"),
        },
        _ => return tool_error("scope must be \"global\" or \"project\""),
    };
    match name {
        "memory_read" => match memory.read(&target) {
            Ok(text) => json!({"content":[{"type":"text","text":text}]}),
            Err(e) => tool_error(&format!("read failed: {e}")),
        },
        "memory_write" => {
            let Some(content) = args.get("content").and_then(Value::as_str) else {
                return tool_error("content is required");
            };
            let append = args.get("mode").and_then(Value::as_str) != Some("replace");
            match memory.write(&target, content, append) {
                Ok(()) => json!({"content":[{"type":"text","text":"ok"}]}),
                Err(e) => tool_error(&format!("write failed: {e}")),
            }
        }
        _ => tool_error(&format!("unknown tool: {name}")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
