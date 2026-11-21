//! `harness-fake-agent --name <name>`: a scripted ACP agent for gates (spec phase-8-core.md §13).
//!
//! Environment: `FAKE_AGENT_STATE` (directory for persisted sessions and `<name>.exhausted` flag files),
//! `FAKE_AGENT_LOG` (JSONL file; every received prompt/new/load is appended).
//!
//! Directives, read only from the LAST text block of a prompt (the user's message):
//! `[[slow N]]` stream N chunks 50 ms apart (honours `session/cancel`), `[[permission]]` ask for a tool
//! permission, `[[mem-write global|project TEXT]]` / `[[mem-read global|project]]` call the Harness MCP
//! memory server, `[[quota-start NAME]]` / `[[quota-mid NAME]]` fail with a usage-limit error when this agent
//! is NAME. A `<state>/<name>.exhausted` file makes every prompt fail with a usage-limit error.
//! Round 8.2: `[[say TEXT]]` streams TEXT as one chunk; `[[write REL_PATH TEXT]]` reports an `edit` tool call and
//! writes TEXT + newline to `REL_PATH` under the session cwd (turn-diff gates; paths with `..` are refused).
//! Phase 9.1: advertises modes `default|plan|acceptEdits`, models `fake-small|fake-large` and the config option
//! `effort` (`low|medium|high`) on new/load, implements `session/set_mode`, `session/set_model` and
//! `session/set_config_option` (logged), logs the `mcpServers` of every new/load; `[[options]]` reports the
//! session's current values.
//! Phase 9.2: `--caps image,embedded` sets `promptCapabilities`; every prompt log entry carries `blockTypes`.
//! `[[plan A|B|C]]` sends a plan (first completed, second in progress, rest pending); `[[tools]]` reports a tool
//! call with diff, terminal and text content plus a location; `[[commands]]` sends `available_commands_update`;
//! `[[usage USED SIZE]]` sends `usage_update` and adds prompt `usage` to the result; `[[permission all]]` asks with
//! the four ACP option kinds and notes the chosen option id; `[[think TEXT]]` sends TEXT as an
//! `agent_thought_chunk` (in directive order, so put it first to have it before the reply).
//! Round 9.2.5 (typed failures): `[[fail auth]]` fails with ACP `auth_required` (-32000), `[[fail exit]]` writes a
//! line to stderr and exits mid-prompt, `[[fail other]]` fails with -32603 plus `data`, `[[fail transient [SECS]]]`
//! fails with `harnessErrorKind: "transient"` (and `retryAfterSecs`). A `<state>/<name>.initfail` file makes
//! `session/new` and `session/load` fail.
//! `[[bg]]` announces a background task, ends it after 300 ms and then runs an autonomous turn "autonomous ok".

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

mod prompt;
// 修复：#[cfg(windows)] 误贴到 mod session 上，非 Windows 平台缺少会话实现。
mod session;

#[cfg_attr(not(windows), allow(dead_code))]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct SessionData {
    cwd: String,
    mcp: Vec<Value>,
    history: Vec<(String, String)>,
    mode: String,
    model: String,
    effort: String,
}

impl SessionData {
    fn new(cwd: String, mcp: Vec<Value>, history: Vec<(String, String)>) -> Self {
        Self {
            cwd,
            mcp,
            history,
            mode: MODES[0].to_owned(),
            model: MODELS[0].to_owned(),
            effort: "medium".to_owned(),
        }
    }
}

const MODES: [&str; 3] = ["default", "plan", "acceptEdits"];
const MODELS: [&str; 2] = ["fake-small", "fake-large"];
const EFFORTS: [&str; 3] = ["low", "medium", "high"];

fn config_options(effort: &str) -> Value {
    json!([{
        "id": "effort", "name": "Reasoning effort", "category": "thought_level", "type": "select",
        "currentValue": effort,
        "options": EFFORTS.iter().map(|e| json!({"value": e, "name": e})).collect::<Vec<_>>()
    }])
}

/// ACP option advertisement of one session (`modes`, `models`, `configOptions`).
fn advertisement(data: &SessionData) -> Value {
    json!({
        "modes": {
            "currentModeId": data.mode,
            "availableModes": MODES.iter().map(|m| json!({"id": m, "name": m, "description": format!("fake {m} mode")})).collect::<Vec<_>>()
        },
        "models": {
            "currentModelId": data.model,
            "availableModels": MODELS.iter().map(|m| json!({"modelId": m, "name": m})).collect::<Vec<_>>()
        },
        "configOptions": config_options(&data.effort)
    })
}

struct Agent {
    name: String,
    state_dir: Option<PathBuf>,
    log: Option<PathBuf>,
    out: mpsc::UnboundedSender<String>,
    sessions: Mutex<HashMap<String, SessionData>>,
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    next_id: Mutex<u64>,
    /// `promptCapabilities.image` / `embeddedContext` (`--caps`).
    image: bool,
    embedded: bool,
}

type Reply = Result<Value, Value>;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut name = "fake".to_owned();
    let mut caps = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--name" {
            if let Some(value) = args.next() {
                name = value;
            }
        } else if arg == "--caps" {
            if let Some(value) = args.next() {
                caps = value;
            }
        }
    }
    let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                break;
            }
        }
    });
    let agent = Arc::new(Agent {
        state_dir: std::env::var_os("FAKE_AGENT_STATE").map(PathBuf::from),
        log: std::env::var_os("FAKE_AGENT_LOG").map(PathBuf::from),
        sessions: Mutex::new(HashMap::new()),
        cancels: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        next_id: Mutex::new(0),
        image: caps.split(',').any(|c| c == "image"),
        embedded: caps.split(',').any(|c| c == "embedded"),
        name,
        out,
    });
    agent.load_persisted();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match (method, id) {
            (None, Some(id)) => {
                let sender = id
                    .as_u64()
                    .and_then(|id| agent.pending.lock().ok().and_then(|mut p| p.remove(&id)));
                if let Some(sender) = sender {
                    let _ = sender.send(message.get("result").cloned().unwrap_or(Value::Null));
                }
            }
            (Some(method), None) => {
                if method == "session/cancel" {
                    if let Some(session) = params.get("sessionId").and_then(Value::as_str) {
                        agent.cancel_flag(session).store(true, Ordering::SeqCst);
                    }
                }
            }
            (Some(method), Some(id)) => {
                let agent = Arc::clone(&agent);
                tokio::spawn(async move {
                    let reply = agent.handle(&method, &params).await;
                    agent.reply(&id, reply);
                });
            }
            (None, None) => {}
        }
    }
    // stdin closed: the client is shutting us down.
    drop(agent);
    let _ = writer.await;
}

impl Agent {
    fn send(&self, value: &Value) {
        let _ = self.out.send(value.to_string());
    }

    fn reply(&self, id: &Value, reply: Reply) {
        match reply {
            Ok(result) => self.send(&json!({"jsonrpc":"2.0","id":id,"result":result})),
            Err(error) => self.send(&json!({"jsonrpc":"2.0","id":id,"error":error})),
        }
    }

    fn update(&self, session: &str, update: &Value) {
        self.send(&json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session,"update":update}}));
    }

    async fn request(&self, method: &str, params: Value) -> Value {
        let (tx, rx) = oneshot::channel();
        let id = {
            let Ok(mut next) = self.next_id.lock() else {
                return Value::Null;
            };
            *next += 1;
            *next
        };
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, tx);
        }
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        rx.await.unwrap_or(Value::Null)
    }

    fn cancel_flag(&self, session: &str) -> Arc<AtomicBool> {
        match self.cancels.lock() {
            Ok(mut map) => Arc::clone(map.entry(session.to_owned()).or_default()),
            Err(_) => Arc::new(AtomicBool::new(false)),
        }
    }

    fn log(&self, entry: &Value) {
        let Some(path) = &self.log else { return };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{entry}");
        }
    }
}

fn commands_update() -> Value {
    json!({"sessionUpdate": "available_commands_update", "availableCommands": [
        {"name": "review", "description": "Agent review", "input": {"hint": "path"}},
        {"name": "compact", "description": "Compact the conversation"}
    ]})
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn parse_directives(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let mut words = after[..end].split_whitespace().map(str::to_owned);
        if let Some(verb) = words.next() {
            out.push((verb, words.collect()));
        }
        rest = &after[end + 2..];
    }
    out
}

/// Spawns the MCP server described by an ACP `McpServer` entry, calls one tool, returns its text.
async fn call_mcp(server: &Value, tool: &str, arguments: Value) -> Result<String, String> {
    let command = server
        .get("command")
        .and_then(Value::as_str)
        .ok_or("no command")?;
    let mut process = tokio::process::Command::new(command);
    if let Some(args) = server.get("args").and_then(Value::as_array) {
        process.args(args.iter().filter_map(Value::as_str));
    }
    if let Some(env) = server.get("env").and_then(Value::as_array) {
        for pair in env {
            if let (Some(k), Some(v)) = (
                pair.get("name").and_then(Value::as_str),
                pair.get("value").and_then(Value::as_str),
            ) {
                process.env(k, v);
            }
        }
    }
    process
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    process.creation_flags(CREATE_NO_WINDOW);
    let mut child = process.spawn().map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fake-agent","version":"0"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":tool,"arguments":arguments}}),
    ];
    for message in messages {
        stdin
            .write_all(format!("{message}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
    }
    stdin.flush().await.map_err(|e| e.to_string())?;
    let mut lines = BufReader::new(stdout).lines();
    let mut result = Err("no tools/call response".to_owned());
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message.get("id").and_then(Value::as_u64) == Some(2) {
            let text = message
                .pointer("/result/content/0/text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let is_error =
                message.pointer("/result/isError").and_then(Value::as_bool) == Some(true);
            result = if is_error { Err(text) } else { Ok(text) };
            break;
        }
    }
    drop(stdin);
    let _ = child.wait().await;
    result
}
