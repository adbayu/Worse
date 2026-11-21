//! Minimal ACP client over a child's stdio (newline-delimited JSON-RPC 2.0).
//!
//! Ordering contract (spec §9): the reader task hands every `session/update` notification and every agent
//! request for a session to that session's route **synchronously and in wire order**, and only then
//! resolves the oneshot of a response. A consumer that posts "request finished" into the same FIFO as the
//! route therefore always observes all earlier updates first.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};

use crate::config::SHUTDOWN_GRACE;
use crate::process::{kill_tree, spawn_piped};
use crate::quota::AgentErrorInfo;

mod connection;

/// ACP protocol version we speak.
pub const ACP_PROTOCOL_VERSION: u64 = 1;

/// JSON-RPC error from the agent, or a transport failure.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    /// Transport closed (agent exited).
    pub const EXITED: i64 = -32099;

    fn exited() -> Self {
        Self {
            code: Self::EXITED,
            message: "agent process exited".into(),
            data: None,
        }
    }

    /// As classification input.
    #[must_use]
    pub fn info(&self) -> AgentErrorInfo {
        AgentErrorInfo {
            code: Some(self.code),
            message: self.message.clone(),
            data: self.data.clone(),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

/// Something the agent sent for one ACP session.
#[derive(Debug)]
pub enum Inbound {
    /// `session/update` notification payload (`params.update`).
    Update { native: String, update: Value },
    /// Agent→client request (e.g. `session/request_permission`); answer with [`AcpConnection::respond`].
    Request {
        native: String,
        id: Value,
        method: String,
        params: Value,
    },
}

/// Per-session sink called by the reader task. Must not block.
pub type RouteSink = Arc<dyn Fn(Inbound) + Send + Sync>;

type Pending = HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>;

#[derive(Default)]
struct ConnState {
    next_id: u64,
    pending: Pending,
    routes: HashMap<String, RouteSink>,
    closed: bool,
}

/// One running ACP agent process.
pub struct AcpConnection {
    generation: u64,
    pid: Option<u32>,
    state: Arc<Mutex<ConnState>>,
    writer: Mutex<Option<mpsc::UnboundedSender<String>>>,
    child: tokio::sync::Mutex<Option<Child>>,
    agent_capabilities: Mutex<Value>,
    /// `agentInfo` from `initialize` (name, version).
    agent_info: Mutex<Value>,
    steering: std::sync::atomic::AtomicBool,
    /// Last stderr lines (at most [`STDERR_TAIL_LINES`]), the `trace` of a typed failure (§9.2.5 R20).
    stderr_tail: Arc<Mutex<std::collections::VecDeque<String>>>,
}

/// Stderr lines kept per agent process for failure traces.
const STDERR_TAIL_LINES: usize = 40;
/// Longest kept stderr line (chars).
const STDERR_LINE_CHARS: usize = 300;

impl std::fmt::Debug for AcpConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpConnection")
            .field("generation", &self.generation)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

fn dispatch(state: &Arc<Mutex<ConnState>>, weak: &std::sync::Weak<AcpConnection>, message: &Value) {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let id = message.get("id").cloned();
    match (method, id) {
        (None, Some(id)) => {
            let Some(id) = id.as_u64() else { return };
            let sender = state.lock().ok().and_then(|mut s| s.pending.remove(&id));
            if let Some(sender) = sender {
                let result = if let Some(error) = message.get("error") {
                    Err(RpcError {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("agent error")
                            .to_owned(),
                        data: error.get("data").cloned(),
                    })
                } else {
                    Ok(message.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = sender.send(result);
            }
        }
        (Some(method), id) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let native = params
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let sink = native
                .as_ref()
                .and_then(|n| state.lock().ok().and_then(|s| s.routes.get(n).cloned()));
            match (id, sink, native) {
                (None, Some(sink), Some(native)) if method == "session/update" => {
                    let update = params.get("update").cloned().unwrap_or(Value::Null);
                    sink(Inbound::Update { native, update });
                }
                (None, _, _) => {} // unrouted notification (e.g. replay during session/load): dropped
                (Some(id), Some(sink), Some(native)) => sink(Inbound::Request {
                    native,
                    id,
                    method,
                    params,
                }),
                (Some(id), _, _) => {
                    if let Some(connection) = weak.upgrade() {
                        connection.respond_error(&id, -32601, "method not supported by Harness");
                    }
                }
            }
        }
        (None, None) => {}
    }
}

/// Serializes a JSON-RPC 2.0 message, moving the field values.
fn envelope(fields: Vec<(&str, Value)>) -> String {
    let mut map = serde_json::Map::new();
    map.insert("jsonrpc".into(), Value::from("2.0"));
    for (key, value) in fields {
        map.insert(key.into(), value);
    }
    Value::Object(map).to_string()
}
