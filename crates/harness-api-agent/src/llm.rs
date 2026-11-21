//! One streamed `POST {base}/chat/completions` call (OpenAI-compatible) and the error mapping of §9.7.

use std::time::Duration;

use futures_util::StreamExt as _;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::Agent;

mod images;
mod stream;

use images::image_rejection;
pub use images::{
    has_images, image_part, message_size, prune_images, strip_images, with_cache_breakpoints,
};
use stream::Accumulator;

/// No byte from the provider for this long ends the call as a transient error.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// A `Retry-After` up to this many seconds is a short rate limit (transient), above it the quota is gone.
const SHORT_RETRY_SECS: u64 = 120;
const BODY_PREVIEW: usize = 500;

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Default)]
pub struct Completion {
    pub content: String,
    /// Streamed `reasoning_content` / `reasoning` text (thinking models).
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    /// `choices[0].finish_reason` of the last chunk that had one (`stop`, `tool_calls`, `length`, …).
    pub finish_reason: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Prompt tokens served from the provider's cache.
    pub cache_read_tokens: u64,
    /// Prompt tokens written into the provider's cache.
    pub cache_write_tokens: u64,
    pub cancelled: bool,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn transient(message: &str, retry_after: Option<u64>) -> Value {
    let mut data = json!({"harnessErrorKind": "transient"});
    if let Some(secs) = retry_after {
        data["retryAfterSecs"] = json!(secs);
    }
    json!({"code": -32603, "message": message, "data": data})
}

fn quota(label: &str, retry_after: Option<u64>) -> Value {
    let resets_at = now_secs() + retry_after.unwrap_or(3600);
    json!({
        "code": -32603,
        "message": format!("{label} usage limit reached"),
        "data": {"harnessErrorKind": "quota_exhausted", "resetsAt": resets_at}
    })
}

/// Body preview with the API key scrubbed (a proxy may echo request headers).
fn preview(agent: &Agent, body: &str) -> String {
    let mut text: String = body.chars().take(BODY_PREVIEW).collect();
    let key = agent.provider.api_key.as_str();
    if key.len() >= 8 {
        text = text.replace(key, "***");
    }
    text.trim().to_owned()
}

/// Maps a non-success HTTP status to an ACP error (phase-9-parity.md §9.7).
fn status_error(agent: &Agent, status: u16, retry_after: Option<u64>, body: &str) -> Value {
    let label = &agent.provider.label;
    match status {
        401 | 403 => json!({
            "code": -32000,
            "message": format!("Authentication required: {label} rejected the API key (HTTP {status})"),
            "data": {"detail": preview(agent, body)}
        }),
        402 => quota(label, retry_after),
        429 => match retry_after {
            Some(secs) if secs <= SHORT_RETRY_SECS => {
                transient(&format!("{label} is rate limited (HTTP 429)"), Some(secs))
            }
            _ => quota(label, retry_after),
        },
        500..=599 => transient(
            &format!(
                "{label} server error (HTTP {status}): {}",
                preview(agent, body)
            ),
            None,
        ),
        _ => json!({
            "code": -32603,
            "message": format!("{label} request failed (HTTP {status}): {}", preview(agent, body))
        }),
    }
}

/// `(cache read, cache write)` tokens of one `usage` object, across provider spellings.
pub fn cache_tokens(usage: &Value) -> (u64, u64) {
    let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
    let read = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .max(n("prompt_cache_hit_tokens"))
        .max(n("cache_read_input_tokens"));
    let write = n("cache_creation_input_tokens");
    (read, write)
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

enum Sent {
    Ok(reqwest::Response),
    Cancelled,
    /// HTTP status, `Retry-After` seconds, body.
    Status(u16, Option<u64>, String),
}

async fn send(
    agent: &Agent,
    messages: &[Value],
    tools: &[Value],
    token: &CancellationToken,
) -> Result<Sent, Value> {
    let provider = &agent.provider;
    let mut body = json!({
        "model": provider.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true}
    });
    if provider.max_tokens > 0 {
        body["max_tokens"] = json!(provider.max_tokens);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    let mut request = agent
        .http
        .post(format!("{}/chat/completions", provider.base_url))
        .json(&body);
    if !provider.api_key.is_empty() {
        request = request.bearer_auth(&provider.api_key);
    }
    let response = tokio::select! {
        () = token.cancelled() => return Ok(Sent::Cancelled),
        sent = request.send() => sent.map_err(|e| transient(&format!("{}: cannot reach {} ({})", provider.label, provider.base_url, e.without_url()), None))?,
    };
    if response.status().is_success() {
        return Ok(Sent::Ok(response));
    }
    let status = response.status().as_u16();
    let after = retry_after(response.headers());
    let text = response.text().await.unwrap_or_default();
    Ok(Sent::Status(status, after, text))
}

/// Asks the provider whether it passes the `system` message to the model: the code word lives only in the
/// system message, so a model that never saw it cannot repeat it. `None` when the probe itself failed.
pub async fn probe_system_role(agent: &Agent) -> Option<bool> {
    let provider = &agent.provider;
    let word: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .filter(char::is_ascii_alphabetic)
        .take(6)
        .collect::<String>()
        .to_ascii_uppercase();
    let word = format!("HX{word}");
    let mut body = json!({
        "model": provider.model,
        "messages": [
            {"role": "system", "content": format!("The code word is {word}. When asked for the code word, reply with it and nothing else.")},
            {"role": "user", "content": "What is the code word?"}
        ],
        "stream": false
    });
    // Thinking models spend part of the budget on reasoning before the answer.
    body["max_tokens"] = json!(2048);
    let mut request = agent
        .http
        .post(format!("{}/chat/completions", provider.base_url))
        .timeout(Duration::from_secs(90))
        .json(&body);
    if !provider.api_key.is_empty() {
        request = request.bearer_auth(&provider.api_key);
    }
    let response = request.send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value: Value = response.json().await.ok()?;
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)?;
    if content.trim().is_empty() {
        return None;
    }
    Some(content.to_ascii_uppercase().contains(&word))
}

/// `messages` without any `reasoning_content` field.
pub fn strip_reasoning(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let mut m = m.clone();
            if let Some(object) = m.as_object_mut() {
                object.remove("reasoning_content");
            }
            m
        })
        .collect()
}

/// The text of a message's `content` (string, or the text parts of an array).
pub fn content_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

/// Streams one completion. Text and reasoning are forwarded as ACP updates while they arrive.
#[allow(clippy::too_many_lines)]
pub async fn complete(
    agent: &Agent,
    session: &str,
    messages: &[Value],
    tools: &[Value],
    token: &CancellationToken,
) -> Result<Completion, Value> {
    let provider = &agent.provider;
    let echoed = messages
        .iter()
        .any(|m| m.get("reasoning_content").is_some());
    // Prompt-cache breakpoints, dropped for good if the provider rejects them (HTTP 400). Reasoning echoes
    // are dropped the same way (older reasoners reject `reasoning_content` in the input).
    let mut marked = agent.cache_control() && provider.prompt_cache;
    let mut echoing = agent.echo_reasoning();
    let pictured = has_images(messages);
    let mut seeing = agent.vision();
    let mut sends = 0_u8;
    let response = loop {
        sends += 1;
        // Two independent steps: history without `reasoning_content`, then the cache markers on top.
        let base = if !echoing && echoed {
            strip_reasoning(messages)
        } else {
            messages.to_vec()
        };
        let base = if pictured && !seeing {
            strip_images(&base)
        } else {
            base
        };
        let payload = if marked {
            with_cache_breakpoints(&base)
        } else {
            base
        };
        match send(agent, &payload, tools, token).await? {
            Sent::Ok(response) => break response,
            Sent::Cancelled => {
                return Ok(Completion {
                    cancelled: true,
                    ..Completion::default()
                })
            }
            Sent::Status(status, _, ref text)
                if pictured && seeing && sends < 5 && image_rejection(status, text) =>
            {
                agent.disable_vision();
                seeing = false;
            }
            Sent::Status(400, _, _) if sends < 4 && (marked || (echoing && echoed)) => {
                if marked {
                    agent.disable_cache_control();
                    marked = false;
                } else {
                    agent.disable_echo_reasoning();
                    echoing = false;
                }
            }
            Sent::Status(400 | 415 | 422, _, _) if pictured && seeing && sends < 5 => {
                agent.disable_vision();
                seeing = false;
            }
            Sent::Status(status, after, text) => {
                return Err(status_error(agent, status, after, &text))
            }
        }
    };
    let mut acc = Accumulator {
        agent,
        session,
        out: Completion::default(),
        calls: Vec::new(),
    };
    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let mut raw = String::new();
    let mut saw_data = false;
    loop {
        let next = tokio::select! {
            () = token.cancelled() => {
                let mut out = acc.finish();
                out.cancelled = true;
                out.tool_calls.clear();
                return Ok(out);
            }
            next = tokio::time::timeout(IDLE_TIMEOUT, stream.next()) => next,
        };
        let bytes = match next {
            Err(_) => {
                return Err(transient(
                    &format!("{}: no data for 5 minutes", provider.label),
                    None,
                ))
            }
            Ok(None) => break,
            Ok(Some(Err(e))) => {
                return Err(transient(
                    &format!("{}: stream broken ({})", provider.label, e.without_url()),
                    None,
                ))
            }
            Ok(Some(Ok(bytes))) => bytes,
        };
        buffer.extend_from_slice(&bytes);
        while let Some(pos) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data:") {
                saw_data = true;
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(acc.finish());
                }
                if let Ok(value) = serde_json::from_str::<Value>(data) {
                    acc.chunk(&value)?;
                }
            } else if !saw_data && !line.starts_with(':') && !line.starts_with("event:") {
                raw.push_str(line);
            }
        }
    }
    if !saw_data {
        // 服务忽略了 stream=true，直接返回整段 JSON。
        raw.push_str(String::from_utf8_lossy(&buffer).trim());
        let value: Value = serde_json::from_str(&raw).map_err(|_| {
            json!({"code": -32603, "message": format!("{}: unexpected response: {}", provider.label, preview(agent, &raw))})
        })?;
        acc.chunk(&value)?;
    } else if acc.out.finish_reason.is_none() {
        // The stream closed without `[DONE]` or a finish reason: the response is cut off, not complete.
        return Err(transient(
            &format!(
                "{}: stream ended before the response finished",
                provider.label
            ),
            None,
        ));
    }
    Ok(acc.finish())
}
#[cfg(test)]
mod tests;
