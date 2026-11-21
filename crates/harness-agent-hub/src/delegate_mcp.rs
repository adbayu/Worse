//! `harness-delegate` MCP server (phase-9-parity.md §9.8): stdio MCP for the parent agent, forwarding each tool
//! call as one JSON line over a loopback TCP connection to the hub's delegate server.
//!
//! Synchronous like the memory server; every `tools/call` runs on its own thread because `delegate` blocks until
//! the subagent finishes and an agent may run several in parallel.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::mcp::MCP_PROTOCOL_VERSION;

mod texts;
mod tools;

use texts::{answer, claim_text, message_text, routed_note, wait_text};
use tools::{always_loaded, router_tools};
pub(crate) use tools::{nested_tools, tool_list};

/// How often a waiting call sends `notifications/progress` (keeps client tool timeouts from firing when the client
/// resets them on progress).
const PROGRESS_EVERY: Duration = Duration::from_secs(15);

/// Where the hub listens; from `HARNESS_DELEGATE_ADDR` / `_TOKEN` / `_SESSION`.
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct DelegateEnv {
    pub addr: String,
    pub token: String,
    pub session: String,
    /// `HARNESS_DELEGATE_ROLE=child` (§9.8.1): only selects the tool list; the hub enforces the role itself.
    pub child: bool,
    pub router: bool,
    /// `HARNESS_DELEGATE_ROLE=nested`: a subagent that may start subagents (delegate tools plus `notify_parent`).
    pub nested: bool,
    /// `HARNESS_DELEGATE_COMPACT=1` (agy): small tool definitions, details in the instructions.
    pub compact: bool,
}

impl std::fmt::Debug for DelegateEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegateEnv")
            .field("addr", &self.addr)
            .field("session", &self.session)
            .field("child", &self.child)
            .field("router", &self.router)
            .field("nested", &self.nested)
            .finish_non_exhaustive()
    }
}

impl DelegateEnv {
    /// Reads the three env vars; `None` when any is missing.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let get = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Some(Self {
            addr: get("HARNESS_DELEGATE_ADDR")?,
            token: get("HARNESS_DELEGATE_TOKEN")?,
            session: get("HARNESS_DELEGATE_SESSION")?,
            child: get("HARNESS_DELEGATE_ROLE").as_deref() == Some("child"),
            router: get("HARNESS_DELEGATE_ROLE").as_deref() == Some("router"),
            nested: get("HARNESS_DELEGATE_ROLE").as_deref() == Some("nested"),
            compact: get("HARNESS_DELEGATE_COMPACT").as_deref() == Some("1"),
        })
    }
}

type Out = Arc<Mutex<Box<dyn Write + Send>>>;

/// Runs the server until stdin closes, then waits for running calls.
///
/// # Errors
///
/// I/O errors on stdin.
pub fn serve(
    env: &DelegateEnv,
    input: impl BufRead,
    output: Box<dyn Write + Send>,
) -> std::io::Result<()> {
    let out: Out = Arc::new(Mutex::new(output));
    let mut workers = Vec::new();
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            write(
                &out,
                &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}),
            );
            continue;
        };
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        match method {
            "initialize" => {
                let mut result = json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "harness-delegate", "version": env!("CARGO_PKG_VERSION")}
                });
                if !env.child && !env.router {
                    result["instructions"] = json!(PARENT_INSTRUCTIONS);
                }
                if env.compact && !env.router {
                    let base = result["instructions"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    result["instructions"] =
                        json!(format!("{base}\n\n{}", crate::delegate_compact::DETAILS)
                            .trim()
                            .to_owned());
                }
                reply(&out, &id, &result);
            }
            "ping" => reply(&out, &id, &json!({})),
            "tools/list" => {
                let list = |child: bool| {
                    if env.compact {
                        crate::delegate_compact::tools(child)
                    } else {
                        tool_list(child)
                    }
                };
                let tools = always_loaded(if env.router {
                    router_tools()
                } else if env.nested {
                    nested_tools(&list(false), &list(true))
                } else {
                    list(env.child)
                });
                reply(&out, &id, &json!({ "tools": tools }));
            }
            "tools/call" => {
                let (env, out, params) = (env.clone(), Arc::clone(&out), message["params"].clone());
                workers.push(std::thread::spawn(move || {
                    let result = call(&env, &out, &params);
                    reply(&out, &id, &result);
                }));
            }
            _ => write(
                &out,
                &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("method not found: {method}")}}),
            ),
        }
        workers.retain(|w| !w.is_finished());
    }
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}

fn write(out: &Out, value: &Value) {
    if let Ok(mut out) = out.lock() {
        let _ = out.write_all(format!("{value}\n").as_bytes());
        let _ = out.flush();
    }
}

fn reply(out: &Out, id: &Value, result: &Value) {
    write(out, &json!({"jsonrpc":"2.0","id":id,"result":result}));
}

const PARENT_INSTRUCTIONS: &str = "You run inside Harness. Subagents in Harness are the user's other AI agents (for example Codex, DeepSeek or another Claude account), configured in the Harness Orchestra. Whenever the user asks you to deploy, spawn, use or ask a subagent, delegate work, or ask another agent or model, call this server's list_agents and delegate tools. Do not use your own built-in subagent, Task or Agent tool for that: it runs a copy of yourself, not the user's subagents.";

fn tool_text(text: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

#[allow(clippy::too_many_lines)]
fn call(env: &DelegateEnv, out: &Out, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let op = match name {
        "list_agents" => "agents",
        "delegate" => "delegate",
        "subagent_message" => "message",
        "subagent_wait" => "wait",
        "notify_parent" => "notify",
        "claim_paths" => "claim",
        "release_paths" => "release",
        "route_trigger" => "route",
        "dismiss_trigger" => "dismiss",
        _ => return tool_text(&format!("unknown tool: {name}"), true),
    };
    let progress = params["_meta"]["progressToken"].clone();
    let request = json!({"token": env.token, "parent": env.session, "op": op, "args": args});
    let response = match exchange(env, &request, out, &progress) {
        Ok(value) => value,
        Err(error) => {
            return tool_text(
                &format!("Harness delegate server unreachable: {error}"),
                true,
            );
        }
    };
    if response["ok"].as_bool() != Some(true) {
        let error = response["error"].as_str().unwrap_or("delegate failed");
        return tool_text(error, true);
    }
    let result = &response["result"];
    if op == "agents" {
        let lines: Vec<String> = result
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| {
                        let mut line = format!(
                            "{} — {} ({})",
                            x["id"].as_str().unwrap_or(""),
                            x["name"].as_str().unwrap_or(""),
                            x["state"].as_str().unwrap_or("")
                        );
                        if let Some(role) = x["role"].as_str() {
                            let _ = write!(line, " · role: {role}");
                            if let Some(note) = x["instructions"].as_str().filter(|n| !n.is_empty())
                            {
                                let _ = write!(line, " · {note}");
                            }
                        }
                        line
                    })
                    .collect()
            })
            .unwrap_or_default();
        return tool_text(&lines.join("\n"), false);
    }
    match op {
        "message" => return tool_text(&message_text(result), false),
        "wait" => return wait_text(result),
        "notify" => return tool_text("Note delivered to the parent agent.", false),
        "route" => {
            return tool_text(
                &format!(
                    "Routed {} to {} (ticket {}).",
                    result["triggerId"].as_str().unwrap_or(""),
                    result["agentName"].as_str().unwrap_or(""),
                    result["ticketId"].as_str().unwrap_or("none")
                ),
                false,
            );
        }
        "dismiss" => {
            return tool_text(
                &format!("Dismissed {}.", result["triggerId"].as_str().unwrap_or("")),
                false,
            );
        }
        "claim" => return claim_text(result),
        "release" => {
            return tool_text(
                &format!(
                    "Released {} claim(s).",
                    result["released"].as_u64().unwrap_or(0)
                ),
                false,
            );
        }
        _ => {}
    }
    if result["background"].as_bool() == Some(true) {
        return tool_text(
            &format!(
                "subagent {} ({}) started in the background (turn {}). Its answer is delivered to you as a new message when it finishes (no need to call subagent_wait); send more instructions with subagent_message.{}",
                result["sessionId"].as_str().unwrap_or(""),
                result["agentName"].as_str().unwrap_or(""),
                result["turnId"].as_str().unwrap_or(""),
                routed_note(result)
            ),
            false,
        );
    }
    let (text, is_error) = answer(result);
    tool_text(&format!("{text}{}", routed_note(result)), is_error)
}

/// Sends one request line and reads one reply line, emitting progress while waiting.
fn exchange(
    env: &DelegateEnv,
    request: &Value,
    out: &Out,
    progress: &Value,
) -> std::io::Result<Value> {
    let mut stream = TcpStream::connect(&env.addr)?;
    stream.write_all(format!("{request}\n").as_bytes())?;
    stream.flush()?;
    stream.set_read_timeout(Some(PROGRESS_EVERY))?;
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    let mut ticks: u64 = 0;
    loop {
        match reader.read_until(b'\n', &mut line) {
            Ok(_) if line.ends_with(b"\n") => break,
            Ok(0) => break,
            Ok(_) => {}
            // 读超时只是“仍在运行”，已读到的字节保留在 line 中，继续等待。
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                ticks += 1;
                if !progress.is_null() {
                    write(
                        out,
                        &json!({"jsonrpc":"2.0","method":"notifications/progress","params":{
                        "progressToken": progress, "progress": ticks, "message": "subagent working"}}),
                    );
                }
            }
            Err(e) => return Err(e),
        }
    }
    serde_json::from_slice(&line).map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests;
