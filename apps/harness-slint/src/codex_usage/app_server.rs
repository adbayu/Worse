//! Minimal JSON-RPC client for `codex app-server --stdio`.

use std::process::Stdio;

use std::path::PathBuf;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

pub(super) struct AppServer {
    _child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

impl AppServer {
    pub(super) async fn start() -> Result<Self, String> {
        let program = harness_agent_hub::agents::resolve_program("codex")
            .unwrap_or_else(|| PathBuf::from("codex"));
        let mut command = Command::new(program);
        command
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let mut child = command
            .spawn()
            .map_err(|error| format!("Cannot start Codex: {error}"))?;
        let input = child.stdin.take().ok_or("Codex stdin unavailable")?;
        let output = child.stdout.take().ok_or("Codex stdout unavailable")?;
        let mut app_server = Self {
            _child: child,
            input,
            output: BufReader::new(output).lines(),
        };
        send(
            &mut app_server.input,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "worse-harness",
                        "title": "Worse Harness",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {}
                }
            }),
        )
        .await?;
        let initialized = response(&mut app_server.output, 1).await?;
        if initialized.get("error").is_some() {
            return Err("Codex app-server initialization failed".into());
        }
        send(
            &mut app_server.input,
            json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}),
        )
        .await?;
        Ok(app_server)
    }

    pub(super) async fn request(&mut self, id: u64, request: Value) -> Result<Value, String> {
        send(&mut self.input, request).await?;
        response(&mut self.output, id).await
    }
}

pub(super) async fn send(
    input: &mut tokio::process::ChildStdin,
    value: Value,
) -> Result<(), String> {
    input
        .write_all(value.to_string().as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    input
        .write_all(b"\n")
        .await
        .map_err(|error| error.to_string())
}

pub(super) async fn response(
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> Result<Value, String> {
    while let Some(line) = output
        .next_line()
        .await
        .map_err(|error| error.to_string())?
    {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message["id"].as_u64() == Some(id) {
            return Ok(message);
        }
    }
    Err("Codex app-server closed before returning usage".into())
}
