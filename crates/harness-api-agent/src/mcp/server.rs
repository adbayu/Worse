use super::{
    json, oneshot, Arc, AsyncBufReadExt, AsyncWriteExt, BufReader, Child, ChildStdin, Duration,
    HashMap, Mutex, Stdio, Value, CALL_TIMEOUT, CREATE_NO_WINDOW, START_TIMEOUT,
};

pub(super) type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

pub(super) struct Server {
    _child: Child,
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Pending,
    next_id: Mutex<u64>,
}

impl Server {
    pub(super) async fn start(spec: &Value) -> Result<Self, String> {
        let command = spec
            .get("command")
            .and_then(Value::as_str)
            .ok_or("MCP server without command")?;
        let mut process = tokio::process::Command::new(command);
        if let Some(args) = spec.get("args").and_then(Value::as_array) {
            process.args(args.iter().filter_map(Value::as_str));
        }
        if let Some(env) = spec.get("env").and_then(Value::as_array) {
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
        let mut child = process.spawn().map_err(|e| format!("{command}: {e}"))?;
        let stdin = child.stdin.take().ok_or("MCP server has no stdin")?;
        let stdout = child.stdout.take().ok_or("MCP server has no stdout")?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(id) = message.get("id").and_then(Value::as_u64) else {
                    continue;
                };
                if message.get("method").is_some() {
                    continue;
                }
                let sender = reader_pending.lock().ok().and_then(|mut p| p.remove(&id));
                if let Some(sender) = sender {
                    let _ = sender.send(message);
                }
            }
        });
        let server = Self {
            _child: child,
            stdin: tokio::sync::Mutex::new(stdin),
            pending,
            next_id: Mutex::new(0),
        };
        server
            .call_within(
                "initialize",
                json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "harness-api-agent", "version": env!("CARGO_PKG_VERSION")}}),
                START_TIMEOUT,
            )
            .await?;
        server
            .write(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await?;
        Ok(server)
    }

    pub(super) async fn write(&self, message: &Value) -> Result<(), String> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(format!("{message}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }

    pub(super) async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.call_within(method, params, CALL_TIMEOUT).await
    }

    pub(super) async fn call_within(
        &self,
        method: &str,
        params: Value,
        limit: Duration,
    ) -> Result<Value, String> {
        let id = {
            let mut next = self.next_id.lock().map_err(|_| "lock")?;
            *next += 1;
            *next
        };
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, tx);
        }
        self.write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        let message = tokio::time::timeout(limit, rx)
            .await
            .map_err(|_| format!("MCP {method} timed out"))?
            .map_err(|_| "MCP server exited".to_owned())?;
        if let Some(error) = message.get("error") {
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| error.to_string(), str::to_owned));
        }
        Ok(message.get("result").cloned().unwrap_or(Value::Null))
    }
}

pub(super) fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
