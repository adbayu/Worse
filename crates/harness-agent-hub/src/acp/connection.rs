use super::{
    dispatch, envelope, json, kill_tree, mpsc, oneshot, spawn_piped, AcpConnection, Arc,
    AsyncBufReadExt, AsyncWriteExt, BufReader, ConnState, Duration, Mutex, Path, Pending,
    RouteSink, RpcError, Value, ACP_PROTOCOL_VERSION, SHUTDOWN_GRACE, STDERR_LINE_CHARS,
    STDERR_TAIL_LINES,
};

impl AcpConnection {
    /// Spawns the agent and starts reader/writer tasks. `on_exit` runs once when stdout closes.
    ///
    /// # Errors
    ///
    /// Spawn failures.
    pub fn spawn(
        program: &Path,
        args: &[String],
        env: &[(String, String)],
        generation: u64,
        on_exit: Box<dyn FnOnce() + Send>,
    ) -> std::io::Result<Arc<Self>> {
        let mut child = spawn_piped(program, args, env, None)?;
        let pid = child.id();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("agent stdin missing"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("agent stdout missing"))?;
        let stderr = child.stderr.take();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let state = Arc::new(Mutex::new(ConnState::default()));
        let stderr_tail = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let connection = Arc::new(Self {
            generation,
            pid,
            state: Arc::clone(&state),
            writer: Mutex::new(Some(tx)),
            child: tokio::sync::Mutex::new(Some(child)),
            agent_capabilities: Mutex::new(Value::Null),
            agent_info: Mutex::new(Value::Null),
            steering: std::sync::atomic::AtomicBool::new(false),
            stderr_tail: Arc::clone(&stderr_tail),
        });

        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = rx.recv().await {
                let write = harness_perf::span_with(
                    "bridge_send",
                    vec![(
                        "bytes",
                        harness_perf::AnnotatedValue::Int(
                            i64::try_from(line.len()).unwrap_or(i64::MAX),
                        ),
                    )],
                );
                let written = stdin.write_all(line.as_bytes()).await.is_ok()
                    && stdin.write_all(b"\n").await.is_ok()
                    && stdin.flush().await.is_ok();
                write.end();
                if !written {
                    break;
                }
            }
            // Dropping stdin = EOF for the agent (graceful shutdown signal).
        });

        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "harness_agent_stderr", pid = ?pid, "{line}");
                    if let Ok(mut tail) = stderr_tail.lock() {
                        if tail.len() == STDERR_TAIL_LINES {
                            tail.pop_front();
                        }
                        tail.push_back(line.chars().take(STDERR_LINE_CHARS).collect());
                    }
                }
            });
        }

        let reader_state = Arc::clone(&state);
        let weak = Arc::downgrade(&connection);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(message) => {
                        let read = harness_perf::span_with(
                            "bridge_recv",
                            vec![(
                                "bytes",
                                harness_perf::AnnotatedValue::Int(
                                    i64::try_from(line.len()).unwrap_or(i64::MAX),
                                ),
                            )],
                        );
                        dispatch(&reader_state, &weak, &message);
                        read.end();
                    }
                    Err(error) => tracing::debug!(%error, "non-JSON line from agent"),
                }
            }
            let pending = match reader_state.lock() {
                Ok(mut s) => {
                    s.closed = true;
                    s.routes.clear();
                    std::mem::take(&mut s.pending)
                }
                Err(_) => Pending::new(),
            };
            for (_, sender) in pending {
                let _ = sender.send(Err(RpcError::exited()));
            }
            on_exit();
        });
        Ok(connection)
    }

    /// Last stderr lines of the process, `None` when it wrote none.
    #[must_use]
    pub fn stderr_tail(&self) -> Option<String> {
        let tail = self.stderr_tail.lock().ok()?;
        (!tail.is_empty()).then(|| tail.iter().cloned().collect::<Vec<_>>().join("\n"))
    }

    /// Pool generation of this process.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// OS process id.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// True after the agent's stdout closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.state.lock().map_or(true, |s| s.closed)
    }

    /// `agentCapabilities.loadSession` from `initialize`.
    #[must_use]
    pub fn supports_load(&self) -> bool {
        self.agent_capabilities
            .lock()
            .ok()
            .and_then(|c| c.get("loadSession").and_then(Value::as_bool))
            .unwrap_or(false)
    }

    /// `agentCapabilities.promptCapabilities` from `initialize` (image, embeddedContext); phase-9-parity.md §9.2.1.
    #[must_use]
    pub fn prompt_capabilities(&self) -> (bool, bool) {
        self.agent_capabilities
            .lock()
            .ok()
            .map_or((false, false), |c| {
                let flag = |k: &str| {
                    c.get("promptCapabilities")
                        .and_then(|m| m.get(k))
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                };
                (flag("image"), flag("embeddedContext"))
            })
    }

    /// `agentCapabilities.mcpCapabilities` from `initialize` (http, sse).
    #[must_use]
    pub fn mcp_capabilities(&self) -> (bool, bool) {
        self.agent_capabilities
            .lock()
            .ok()
            .map_or((false, false), |c| {
                let flag = |k: &str| {
                    c.get("mcpCapabilities")
                        .and_then(|m| m.get(k))
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                };
                (flag("http"), flag("sse"))
            })
    }

    #[must_use]
    pub fn supports_steering(&self) -> bool {
        self.steering.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Enqueues a request (synchronously, in order) and returns the receiver of its response.
    pub fn send_request(
        &self,
        method: &str,
        params: Value,
    ) -> oneshot::Receiver<Result<Value, RpcError>> {
        let (tx, rx) = oneshot::channel();
        let id = {
            let Ok(mut state) = self.state.lock() else {
                let _ = tx.send(Err(RpcError::exited()));
                return rx;
            };
            if state.closed {
                drop(state);
                let _ = tx.send(Err(RpcError::exited()));
                return rx;
            }
            state.next_id += 1;
            let id = state.next_id;
            state.pending.insert(id, tx);
            id
        };
        let line = envelope(vec![
            ("id", Value::from(id)),
            ("method", Value::from(method)),
            ("params", params),
        ]);
        if !self.write(line) {
            if let Ok(mut state) = self.state.lock() {
                if let Some(sender) = state.pending.remove(&id) {
                    let _ = sender.send(Err(RpcError::exited()));
                }
            }
        }
        rx
    }

    /// Request and await its response.
    ///
    /// # Errors
    ///
    /// The agent's JSON-RPC error or [`RpcError::EXITED`].
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.send_request(method, params)
            .await
            .unwrap_or_else(|_| Err(RpcError::exited()))
    }

    /// Sends a notification (e.g. `session/cancel`).
    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.write(envelope(vec![
            ("method", Value::from(method)),
            ("params", params),
        ]));
    }

    /// Answers an agent→client request.
    pub fn respond(&self, id: &Value, result: Value) {
        let _ = self.write(envelope(vec![("id", id.clone()), ("result", result)]));
    }

    /// Answers an agent→client request with an error.
    pub fn respond_error(&self, id: &Value, code: i64, message: &str) {
        let _ = self.write(
            json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}).to_string(),
        );
    }

    /// Routes inbound traffic of one ACP session to `sink` (replaces an earlier route).
    pub fn register_route(&self, native: &str, sink: RouteSink) {
        if let Ok(mut state) = self.state.lock() {
            state.routes.insert(native.to_owned(), sink);
        }
    }

    /// ACP `initialize`.
    ///
    /// # Errors
    ///
    /// Agent errors.
    pub async fn initialize(&self) -> Result<(), RpcError> {
        let result = self
            .call(
                "initialize",
                json!({
                    "protocolVersion": ACP_PROTOCOL_VERSION,
                    "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false, "_meta": {"jetbrains": {"air": {"version": 1, "capabilities": ["asyncTasks"]}}}},
                    "clientInfo": {"name": "harness", "version": env!("CARGO_PKG_VERSION")}
                }),
            )
            .await?;
        if let Ok(mut caps) = self.agent_capabilities.lock() {
            *caps = result
                .get("agentCapabilities")
                .cloned()
                .unwrap_or(Value::Null);
        }
        if let Ok(mut info) = self.agent_info.lock() {
            *info = result.get("agentInfo").cloned().unwrap_or(Value::Null);
        }
        let steering = result
            .pointer("/_meta/steering/supported")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.steering
            .store(steering, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// `agentInfo.name` / `agentInfo.version` from `initialize` (phase-9-parity.md §9.1.5a).
    #[must_use]
    pub fn agent_info(&self) -> (Option<String>, Option<String>) {
        self.agent_info.lock().ok().map_or((None, None), |info| {
            let field = |k: &str| info.get(k).and_then(Value::as_str).map(str::to_owned);
            (field("name"), field("version"))
        })
    }

    /// Closes stdin, waits the grace period, then kills the process tree.
    pub async fn shutdown(&self) {
        if let Ok(mut writer) = self.writer.lock() {
            writer.take();
        }
        let mut guard = self.child.lock().await;
        let Some(mut child) = guard.take() else {
            return;
        };
        if tokio::time::timeout(SHUTDOWN_GRACE, child.wait())
            .await
            .is_err()
        {
            if let Some(pid) = child.id() {
                kill_tree(pid).await;
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        }
    }

    pub(super) fn write(&self, line: String) -> bool {
        self.writer
            .lock()
            .ok()
            .and_then(|w| w.as_ref().map(|tx| tx.send(line).is_ok()))
            .unwrap_or(false)
    }
}
