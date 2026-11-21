//! Hermes Agent through its gateway (`hermes gateway`; the API server listens on
//! `http://127.0.0.1:8642/v1` by default and needs `API_SERVER_KEY`).
//!
//! A Hermes gateway is not a model endpoint: it runs a whole agent with its own tools, memory and session
//! store on the gateway's host. So a turn against it does not run Harness's tool loop and sends no tools. It
//! sends the user's message with the session header and relays what Hermes streams back:
//!
//! - `data:` chunks in the `OpenAI` shape: `delta.content` (answer) and `delta.reasoning_content` (thinking);
//! - `event: hermes.tool.progress` with `{tool, label, toolCallId, status: "running" | "completed"}`;
//! - `event: hermes.status` with `{kind, text}`;
//! - `event: approval.request` with `{run_id, command, description, choices}`, answered with
//!   `POST {base}/runs/{run_id}/approval {"choice": …}`.
//!
//! The conversation lives in Hermes (`X-Hermes-Session-Id`), so only the new message is sent; the local copy
//! is kept for `session/load` replay. Closing the stream stops the run, which is how a turn is cancelled.
//!
//! A gateway is recognised by `GET {base}/capabilities` (`"object": "hermes.api_server.capabilities"`), once
//! per endpoint; `HARNESS_PROVIDER_KIND=hermes` or `=openai` skips the question.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::StreamExt as _;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::llm;
use crate::session::Session;
use crate::{Agent, Kind, Reply};

mod events;

use events::{approval, chunk, close_tools, finish, tool_progress};

/// Header that selects (and creates) the Hermes session a request continues.
const SESSION_HEADER: &str = "X-Hermes-Session-Id";
const CAPABILITIES_OBJECT: &str = "hermes.api_server.capabilities";
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);
/// A gateway run may think or wait on a tool for a long time; keepalive comments reset this.
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// One server-sent event: the `event:` name (empty for plain `data:` frames) and its data.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub event: String,
    pub data: String,
}

/// Splits a byte stream into server-sent events. Frames end at a blank line; comment lines (`: keepalive`)
/// and fields other than `event` / `data` are skipped.
#[derive(Default)]
pub(crate) struct Sse {
    buffer: Vec<u8>,
    event: String,
    data: String,
}

impl Sse {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Frame> {
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() || !self.event.is_empty() {
                    out.push(Frame {
                        event: std::mem::take(&mut self.event),
                        data: std::mem::take(&mut self.data),
                    });
                }
            } else if let Some(name) = line.strip_prefix("event:") {
                name.trim().clone_into(&mut self.event);
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(data.strip_prefix(' ').unwrap_or(data));
            }
        }
        out
    }
}

/// ACP tool kind for a Hermes tool name (icons and grouping in the chat).
pub(crate) fn tool_kind(tool: &str) -> &'static str {
    let t = tool.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    if has(&["terminal", "shell", "exec", "process", "code"]) {
        "execute"
    } else if has(&["write", "patch", "edit", "create"]) {
        "edit"
    } else if has(&["search", "grep", "find", "glob"]) {
        "search"
    } else if has(&["read", "list", "view", "cat"]) {
        "read"
    } else if has(&["web", "browser", "fetch", "http", "url"]) {
        "fetch"
    } else if has(&["delegate", "think", "plan", "todo", "memory"]) {
        "think"
    } else {
        "other"
    }
}

/// ACP permission options for the choices a gateway offers, in its order (`once` when it names none).
pub(crate) fn approval_options(choices: &[String]) -> Vec<Value> {
    let known = |c: &str| match c {
        "once" => Some(("Allow once", "allow_once")),
        "session" => Some(("Allow for this session", "allow_always")),
        "always" => Some(("Always allow", "allow_always")),
        "deny" => Some(("Deny", "reject_once")),
        _ => None,
    };
    let mut out: Vec<Value> = choices
        .iter()
        .filter_map(|c| {
            known(c).map(|(name, kind)| json!({"optionId": c, "name": name, "kind": kind}))
        })
        .collect();
    if out.is_empty() {
        out = vec![
            json!({"optionId": "once", "name": "Allow once", "kind": "allow_once"}),
            json!({"optionId": "deny", "name": "Deny", "kind": "reject_once"}),
        ];
    }
    out
}

/// Whether the configured endpoint is a Hermes gateway.
pub async fn is_gateway(agent: &Agent) -> bool {
    match agent.provider.kind {
        Kind::Hermes => return true,
        Kind::OpenAi => return false,
        Kind::Auto => {}
    }
    match agent.gateway_mode.load(Ordering::SeqCst) {
        1 => return false,
        2 => return true,
        _ => {}
    }
    let key = agent.provider.base_url.clone();
    let file = agent
        .provider
        .state_dir
        .as_ref()
        .map(|d| d.join("gateway.json"));
    let mut hermes = None;
    if let Some(file) = &file {
        if let Ok(text) = tokio::fs::read_to_string(file).await {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if v.get("key").and_then(Value::as_str) == Some(key.as_str()) {
                    hermes = v.get("hermes").and_then(Value::as_bool);
                }
            }
        }
    }
    if hermes.is_none() {
        hermes = probe(agent).await;
        if let (Some(h), Some(file)) = (hermes, &file) {
            if let Some(dir) = file.parent() {
                let _ = tokio::fs::create_dir_all(dir).await;
            }
            let _ = tokio::fs::write(file, json!({"key": key, "hermes": h}).to_string()).await;
        }
    }
    // Unknown (the endpoint did not answer): this request takes the ordinary path, which reports the
    // connection problem; the next turn asks again.
    match hermes {
        Some(true) => agent.gateway_mode.store(2, Ordering::SeqCst),
        Some(false) => agent.gateway_mode.store(1, Ordering::SeqCst),
        None => {}
    }
    hermes == Some(true)
}

/// `Some(true)` for a Hermes gateway, `Some(false)` for anything that answered otherwise, `None` when
/// nothing answered.
async fn probe(agent: &Agent) -> Option<bool> {
    let provider = &agent.provider;
    let mut request = agent
        .http
        .get(format!("{}/capabilities", provider.base_url))
        .timeout(PROBE_TIMEOUT);
    if !provider.api_key.is_empty() {
        request = request.bearer_auth(&provider.api_key);
    }
    let response = request.send().await.ok()?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    Some(is_capabilities(status, &value))
}

/// A capabilities answer of a Hermes gateway, or its "wrong key" answer (so the key problem is reported by
/// the gateway path, in Hermes's words).
pub(crate) fn is_capabilities(status: u16, body: &Value) -> bool {
    if (200..300).contains(&status) {
        return body.get("object").and_then(Value::as_str) == Some(CAPABILITIES_OBJECT);
    }
    status == 401
        && body.pointer("/error/code").and_then(Value::as_str) == Some("gateway_auth_failed")
}

fn failure(agent: &Agent, text: &str) -> Value {
    json!({"code": -32603, "message": format!("{}: {text}", agent.provider.label)})
}

/// What one turn collected.
#[derive(Default)]
struct Turn {
    content: String,
    finish: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    /// Tool calls announced as running and not yet completed.
    open: HashSet<String>,
    approvals: u32,
}

/// One prompt turn against a Hermes gateway.
#[allow(clippy::too_many_lines)]
pub async fn run_turn(
    agent: &Agent,
    id: &str,
    session: &mut Session,
    user: Value,
    token: &CancellationToken,
) -> Reply {
    let provider = &agent.provider;
    session
        .messages
        .push(json!({"role": "user", "content": user.clone()}));
    let mut messages = Vec::new();
    if !session.cwd.is_empty() {
        // Hermes layers `system` messages on its own prompt for this request only.
        messages.push(json!({"role": "system", "content": format!(
            "This conversation comes from the Harness desktop app. The user's project directory there is {}.",
            session.cwd
        )}));
    }
    messages.push(json!({"role": "user", "content": user}));
    let body = json!({"model": provider.model, "stream": true, "messages": messages});
    let mut request = agent
        .http
        .post(format!("{}/chat/completions", provider.base_url))
        .header(SESSION_HEADER, format!("harness-{id}"))
        .json(&body);
    if !provider.api_key.is_empty() {
        request = request.bearer_auth(&provider.api_key);
    }
    let response = tokio::select! {
        () = token.cancelled() => return Ok(json!({"stopReason": "cancelled"})),
        sent = request.send() => sent.map_err(|e| llm::transient(
            &format!("{}: cannot reach the Hermes gateway at {} ({}). Is `hermes gateway` running?", provider.label, provider.base_url, e.without_url()),
            None,
        ))?,
    };
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| text.chars().take(300).collect());
        return Err(match status {
            401 | 403 => failure(agent, "the gateway rejected the key. Use the API_SERVER_KEY of that gateway as the API key."),
            429 => llm::transient(&format!("{}: the gateway is at its limit of concurrent runs", provider.label), Some(2)),
            _ => failure(agent, &format!("HTTP {status}: {detail}")),
        });
    }
    let mut turn = Turn::default();
    let mut sse = Sse::default();
    let mut stream = response.bytes_stream();
    let mut done = false;
    while !done {
        let next = tokio::select! {
            // Dropping the stream closes the connection, which interrupts the run on the gateway.
            () = token.cancelled() => return Ok(finish(agent, id, session, turn, true)),
            next = tokio::time::timeout(IDLE_TIMEOUT, stream.next()) => next,
        };
        let bytes = match next {
            Err(_) => {
                return Err(llm::transient(
                    &format!("{}: no data for 10 minutes", provider.label),
                    None,
                ))
            }
            Ok(None) => break,
            Ok(Some(Err(e))) => {
                return Err(llm::transient(
                    &format!("{}: stream broken ({})", provider.label, e.without_url()),
                    None,
                ))
            }
            Ok(Some(Ok(bytes))) => bytes,
        };
        for frame in sse.feed(&bytes) {
            if frame.data == "[DONE]" {
                done = true;
                break;
            }
            let Ok(value) = serde_json::from_str::<Value>(&frame.data) else {
                continue;
            };
            match frame.event.as_str() {
                "" => chunk(agent, id, &mut turn, &value)?,
                "hermes.tool.progress" => tool_progress(agent, id, &mut turn, &value),
                "hermes.status" => {
                    if let Some(text) = value
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|t| !t.is_empty())
                    {
                        agent.update(id, &json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": format!("\n{text}\n")}}));
                    }
                }
                "approval.request" => {
                    tokio::select! {
                        () = token.cancelled() => return Ok(finish(agent, id, session, turn, true)),
                        () = approval(agent, id, &mut turn, &value) => {}
                    }
                }
                _ => {}
            }
        }
    }
    if !done && turn.finish.is_none() {
        return Err(llm::transient(
            &format!(
                "{}: the gateway closed the stream before the answer finished",
                provider.label
            ),
            None,
        ));
    }
    if turn.finish.as_deref() == Some("error") && turn.content.trim().is_empty() {
        close_tools(agent, id, &mut turn, "failed");
        return Err(failure(agent, "the Hermes run failed before it answered"));
    }
    Ok(finish(agent, id, session, turn, false))
}

#[cfg(test)]
mod tests;
