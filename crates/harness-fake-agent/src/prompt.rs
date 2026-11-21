use super::{
    call_mcp, commands_update, json, now_secs, parse_directives, Agent, Duration, Ordering,
    PathBuf, Reply, Value,
};

impl Agent {
    // 指令分派表：一条指令一行，按 rustfmt 换行后略超 100 行，拆开反而更难读。
    #[allow(clippy::too_many_lines)]
    pub(super) async fn prompt(&self, params: &Value) -> Reply {
        let session = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let blocks: Vec<String> = params
            .get("prompt")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let block_types: Vec<String> = params
            .get("prompt")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|b| {
                        b.get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_owned()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let user = blocks.last().cloned().unwrap_or_default();
        self.log(&json!({"agent": self.name, "pid": std::process::id(), "event": "prompt", "sessionId": session, "blocks": blocks, "blockTypes": block_types}));
        let cancel = self.cancel_flag(&session);
        cancel.store(false, Ordering::SeqCst);
        if let Some(resets_at) = self.exhausted_flag() {
            return Err(self.quota_error(resets_at));
        }
        let directives = parse_directives(&user);
        let mut reply = format!("[{}] ", self.name);
        let mut notes: Vec<String> = Vec::new();
        let mut usage: Option<Value> = None;
        for (verb, args) in &directives {
            let targeted = args.first().map(String::as_str) == Some(self.name.as_str());
            match verb.as_str() {
                "quota-start" if targeted => return Err(self.quota_error(now_secs() + 3600)),
                "fail" if args.first().map(String::as_str) == Some("exit") => {
                    self.chunk(&session, "partial ", &mut reply);
                    eprintln!("fake agent {}: simulated crash", self.name);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    std::process::exit(3);
                }
                "fail" => return Err(self.fail_error(args)),
                "quota-mid" if targeted => {
                    for i in 0..3 {
                        self.chunk(&session, &format!("partial-{i} "), &mut reply);
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                    return Err(self.quota_error(now_secs() + 3600));
                }
                "slow" => {
                    let n: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(20);
                    for i in 0..n {
                        if cancel.load(Ordering::SeqCst) {
                            self.remember(&session, &user, &reply);
                            return Ok(json!({"stopReason": "cancelled"}));
                        }
                        self.chunk(&session, &format!("chunk-{i} "), &mut reply);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
                "permission" => notes.push(format!(
                    "permission={}",
                    self.ask_permission(&session, args.first().map(String::as_str) == Some("all"))
                        .await
                )),
                "plan" => self.send_plan(&session, &args.join(" ")),
                "tools" => self.send_tools(&session),
                "commands" => self.update(&session, &commands_update()),
                "bg" => self.background(&session),
                "usage" => usage = Some(self.send_usage(&session, args)),
                "options" => notes.push(self.options_note(&session)),
                "say" => self.chunk(&session, &format!("{} ", args.join(" ")), &mut reply),
                "think" => self.update(
                    &session,
                    &json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": args.join(" ")}}),
                ),
                "write" => notes.push(self.write_file(&session, args).await),
                "mem-write" | "mem-read" => {
                    notes.push(self.memory_call(&session, verb, args).await);
                }
                _ => {}
            }
        }
        if cancel.load(Ordering::SeqCst) {
            self.remember(&session, &user, &reply);
            return Ok(json!({"stopReason": "cancelled"}));
        }
        let saw_memory = blocks.iter().any(|b| b.contains("<harness-memory"));
        let summary = format!(
            "reply to: {} | blocks={} memory={} {}",
            user.chars().take(80).collect::<String>(),
            blocks.len(),
            saw_memory,
            notes.join(" ")
        );
        self.chunk(&session, &summary, &mut reply);
        self.remember(&session, &user, &reply);
        let mut result = json!({"stopReason": "end_turn"});
        if let Some(usage) = usage {
            result["usage"] = usage;
        }
        Ok(result)
    }

    pub(super) fn background(&self, session: &str) {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let task = format!("bg-{suffix}");
        let turn = format!("turn-{suffix}");
        self.update(
            session,
            &json!({"sessionUpdate": "async_task_spawned", "asyncTaskId": task, "name": "sleep", "taskType": "local_bash", "description": "gate sleep", "showInTranscript": true, "canStop": true}),
        );
        let out = self.out.clone();
        let session = session.to_owned();
        tokio::spawn(async move {
            let send = |update: Value| {
                let _ = out.send(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session,"update":update}}).to_string());
            };
            tokio::time::sleep(Duration::from_millis(300)).await;
            send(
                json!({"sessionUpdate": "async_task_state_update", "asyncTaskId": task, "state": "completed", "summary": "exit code 0"}),
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
            send(
                json!({"sessionUpdate": "async_task_spawned", "asyncTaskId": turn, "name": "Working", "taskType": "turn", "description": "gate sleep (exit code 0)", "showInTranscript": true, "canStop": true}),
            );
            send(
                json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "autonomous ok"}}),
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
            send(
                json!({"sessionUpdate": "async_task_state_update", "asyncTaskId": turn, "state": "completed"}),
            );
        });
    }

    pub(super) fn chunk(&self, session: &str, text: &str, reply: &mut String) {
        reply.push_str(text);
        self.update(
            session,
            &json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}),
        );
    }

    pub(super) fn remember(&self, session: &str, user: &str, reply: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(data) = sessions.get_mut(session) {
                data.history.push(("user".into(), user.to_owned()));
                data.history.push(("agent".into(), reply.to_owned()));
            }
        }
        self.persist();
    }

    /// `usage_update` now; returns the prompt-result `usage`.
    pub(super) fn send_usage(&self, session: &str, args: &[String]) -> Value {
        let arg =
            |i: usize, default: u64| args.get(i).and_then(|a| a.parse().ok()).unwrap_or(default);
        self.update(
            session,
            &json!({"sessionUpdate": "usage_update", "used": arg(0, 1200), "size": arg(1, 200_000), "cost": {"amount": 0.25, "currency": "USD"}}),
        );
        json!({"inputTokens": 100, "outputTokens": 20, "totalTokens": 120, "cachedReadTokens": 7})
    }

    pub(super) fn send_plan(&self, session: &str, spec: &str) {
        let entries: Vec<Value> = spec
            .split('|')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .enumerate()
            .map(|(i, content)| {
                let status = match i {
                    0 => "completed",
                    1 => "in_progress",
                    _ => "pending",
                };
                json!({"content": content, "priority": if i == 0 { "high" } else { "medium" }, "status": status})
            })
            .collect();
        self.update(
            session,
            &json!({"sessionUpdate": "plan", "entries": entries}),
        );
    }

    pub(super) fn send_tools(&self, session: &str) {
        let id = format!("tool-{}", uuid::Uuid::new_v4().simple());
        self.update(
            session,
            &json!({"sessionUpdate": "tool_call", "toolCallId": id, "title": "edit src/lib.rs", "kind": "edit", "status": "in_progress",
                "locations": [{"path": "src/lib.rs", "line": 3}]}),
        );
        self.update(
            session,
            &json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "completed",
                "content": [
                    {"type": "diff", "path": "src/lib.rs", "oldText": "old line", "newText": "new line"},
                    {"type": "terminal", "terminalId": "term-1"},
                    {"type": "content", "content": {"type": "text", "text": "tool says hi"}}
                ],
                "rawOutput": {"ok": true}}),
        );
    }

    pub(super) async fn ask_permission(&self, session: &str, all: bool) -> String {
        let tool_call_id = format!("tool-{}", uuid::Uuid::new_v4().simple());
        let tool = json!({"toolCallId": tool_call_id, "title": "write demo.txt", "kind": "edit", "status": "pending", "rawInput": {"path": "demo.txt"}});
        let mut update = tool.clone();
        if let Some(map) = update.as_object_mut() {
            map.insert("sessionUpdate".into(), json!("tool_call"));
        }
        self.update(session, &update);
        let options = if all {
            json!([
                {"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
                {"optionId": "allow-always", "name": "Always allow", "kind": "allow_always"},
                {"optionId": "deny", "name": "Deny", "kind": "reject_once"},
                {"optionId": "deny-always", "name": "Always deny", "kind": "reject_always"}
            ])
        } else {
            json!([
                {"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
                {"optionId": "deny", "name": "Deny", "kind": "reject_once"}
            ])
        };
        let response = self
            .request(
                "session/request_permission",
                json!({
                    "sessionId": session,
                    "toolCall": tool,
                    "options": options
                }),
            )
            .await;
        let outcome = response.get("outcome").cloned().unwrap_or(Value::Null);
        let chosen = outcome
            .get("optionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let decision = match outcome.get("outcome").and_then(Value::as_str) {
            Some("selected") => match chosen.as_str() {
                "allow" | "allow-always" => "allowed",
                _ => "denied",
            },
            _ => "cancelled",
        };
        let status = if decision == "allowed" {
            "completed"
        } else {
            "failed"
        };
        self.update(
            session,
            &json!({"sessionUpdate": "tool_call_update", "toolCallId": tool_call_id, "status": status}),
        );
        if all {
            format!("{decision}:{chosen}")
        } else {
            decision.to_owned()
        }
    }

    pub(super) async fn write_file(&self, session: &str, args: &[String]) -> String {
        let Some(relative) = args.first() else {
            return "write=missing-path".into();
        };
        if relative.split(['/', '\\']).any(|part| part == "..")
            || std::path::Path::new(relative).is_absolute()
        {
            return "write=refused".into();
        }
        let cwd = self
            .sessions
            .lock()
            .ok()
            .and_then(|s| s.get(session).map(|d| d.cwd.clone()))
            .unwrap_or_default();
        let tool_call_id = format!("tool-{}", uuid::Uuid::new_v4().simple());
        self.update(
            session,
            &json!({"sessionUpdate": "tool_call", "toolCallId": tool_call_id, "title": format!("write {relative}"), "kind": "edit", "status": "in_progress", "rawInput": {"path": relative}}),
        );
        let path = PathBuf::from(cwd).join(relative);
        let content = format!("{}\n", args.get(1..).unwrap_or_default().join(" "));
        let result = async {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, content).await
        }
        .await;
        let status = if result.is_ok() {
            "completed"
        } else {
            "failed"
        };
        self.update(
            session,
            &json!({"sessionUpdate": "tool_call_update", "toolCallId": tool_call_id, "status": status}),
        );
        format!("write={status}")
    }

    pub(super) async fn memory_call(&self, session: &str, verb: &str, args: &[String]) -> String {
        let server = self
            .sessions
            .lock()
            .ok()
            .and_then(|s| s.get(session).and_then(|d| d.mcp.first().cloned()));
        let Some(server) = server else {
            return format!("{verb}=no-mcp-server");
        };
        let scope = args.first().cloned().unwrap_or_else(|| "global".into());
        let (tool, arguments) = if verb == "mem-write" {
            (
                "memory_write",
                json!({"scope": scope, "content": args.get(1..).unwrap_or_default().join(" "), "mode": "append"}),
            )
        } else {
            ("memory_read", json!({"scope": scope}))
        };
        match call_mcp(&server, tool, arguments).await {
            Ok(text) => format!("{verb}[{scope}]=<<{}>>", text.trim()),
            Err(error) => format!("{verb}=error:{error}"),
        }
    }
}
