use super::{
    advertisement, config_options, json, now_secs, Agent, PathBuf, Reply, SessionData, Value,
    EFFORTS, MODELS, MODES,
};

impl Agent {
    pub(super) fn persist_path(&self) -> Option<PathBuf> {
        self.state_dir
            .as_ref()
            .map(|d| d.join(format!("{}-sessions.json", self.name)))
    }

    pub(super) fn load_persisted(&self) {
        let Some(path) = self.persist_path() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        if let Ok(mut sessions) = self.sessions.lock() {
            for (id, data) in map {
                let history = data
                    .get("history")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|i| {
                                Some((
                                    i.get(0)?.as_str()?.to_owned(),
                                    i.get(1)?.as_str()?.to_owned(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let cwd = data
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                sessions.insert(id, SessionData::new(cwd, Vec::new(), history));
            }
        }
    }

    pub(super) fn persist(&self) {
        let Some(path) = self.persist_path() else {
            return;
        };
        let Ok(sessions) = self.sessions.lock() else {
            return;
        };
        let map: serde_json::Map<String, Value> = sessions
            .iter()
            .map(|(id, s)| (id.clone(), json!({"cwd": s.cwd, "history": s.history})))
            .collect();
        drop(sessions);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, Value::Object(map).to_string());
    }

    pub(super) fn exhausted_flag(&self) -> Option<i64> {
        let path = self
            .state_dir
            .as_ref()?
            .join(format!("{}.exhausted", self.name));
        let text = std::fs::read_to_string(path).ok()?;
        Some(
            text.trim()
                .parse::<i64>()
                .unwrap_or_else(|_| now_secs() + 3600),
        )
    }

    pub(super) fn init_fail(&self) -> bool {
        self.state_dir
            .as_ref()
            .is_some_and(|d| d.join(format!("{}.initfail", self.name)).exists())
    }

    /// `[[fail KIND ...]]` errors (phase-9-parity.md §9.2.5 R20).
    pub(super) fn fail_error(&self, args: &[String]) -> Value {
        match args.first().map(String::as_str) {
            Some("auth") => json!({"code": -32000, "message": "Authentication required"}),
            Some("transient") => {
                let secs = args.get(1).and_then(|s| s.parse::<u64>().ok()).unwrap_or(5);
                json!({
                    "code": -32603,
                    "message": format!("{} is overloaded, try again later", self.name),
                    "data": {"harnessErrorKind": "transient", "retryAfterSecs": secs}
                })
            }
            _ => json!({
                "code": -32603,
                "message": format!("{} failed: model refused the request", self.name),
                "data": {"reason": "fake failure", "agent": self.name}
            }),
        }
    }

    pub(super) fn quota_error(&self, resets_at: i64) -> Value {
        json!({
            "code": -32603,
            "message": format!("{} usage limit reached|{resets_at}", self.name),
            "data": {"harnessErrorKind": "quota_exhausted", "resetsAt": resets_at}
        })
    }

    pub(super) async fn handle(&self, method: &str, params: &Value) -> Reply {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": 1,
                "agentCapabilities": {"loadSession": true, "promptCapabilities": {"image": self.image, "audio": false, "embeddedContext": self.embedded}, "mcpCapabilities": {"http": false, "sse": false}},
                "authMethods": [],
                "agentInfo": {"name": self.name, "version": "0.1.0"}
            })),
            "session/new" | "session/load" if self.init_fail() => Err(json!({
                "code": -32603,
                "message": format!("{} could not start the session", self.name),
                "data": {"reason": "initfail flag"}
            })),
            "session/new" => Ok(self.new_session(params)),
            "session/load" => self.load_session(params),
            "session/prompt" => self.prompt(params).await,
            "session/set_mode" | "session/set_model" | "session/set_config_option" => {
                self.set_option(method, params)
            }
            "_session/async_task/stop" => Ok(json!({"stopped": true})),
            _ => Err(json!({"code": -32601, "message": format!("unknown method {method}")})),
        }
    }

    pub(super) fn new_session(&self, params: &Value) -> Value {
        let id = format!("{}-{}", self.name, uuid::Uuid::new_v4().simple());
        let mcp = params
            .get("mcpServers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let data = SessionData::new(
            params
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            mcp.clone(),
            Vec::new(),
        );
        let mut result = advertisement(&data);
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(id.clone(), data);
        }
        self.persist();
        self.log(&json!({"agent": self.name, "pid": std::process::id(), "event": "new", "sessionId": id, "mcpServers": mcp}));
        result["sessionId"] = json!(id);
        result
    }

    pub(super) fn load_session(&self, params: &Value) -> Reply {
        let id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mcp = params
            .get("mcpServers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let (history, result) = {
            let Ok(mut sessions) = self.sessions.lock() else {
                return Err(json!({"code": -32603, "message": "lock"}));
            };
            let Some(session) = sessions.get_mut(&id) else {
                return Err(json!({"code": -32002, "message": format!("session {id} not found")}));
            };
            session.mcp.clone_from(&mcp);
            (session.history.clone(), advertisement(session))
        };
        // ACP: replay the conversation as updates before answering.
        for (role, text) in &history {
            let kind = if role == "user" {
                "user_message_chunk"
            } else {
                "agent_message_chunk"
            };
            self.update(
                &id,
                &json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}}),
            );
        }
        self.log(&json!({"agent": self.name, "pid": std::process::id(), "event": "load", "sessionId": id, "replayed": history.len(), "mcpServers": mcp}));
        Ok(result)
    }

    /// `session/set_mode` / `set_model` / `set_config_option`: validate, store, log.
    pub(super) fn set_option(&self, method: &str, params: &Value) -> Reply {
        let session = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let text = |key: &str| {
            params
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let invalid =
            |what: String| Err(json!({"code": -32602, "message": format!("invalid {what}")}));
        let (key, value) = match method {
            "session/set_mode" => ("mode".to_owned(), text("modeId")),
            "session/set_model" => ("model".to_owned(), text("modelId")),
            _ => (text("configId"), text("value")),
        };
        let allowed: &[&str] = match key.as_str() {
            "mode" => &MODES,
            "model" => &MODELS,
            "effort" => &EFFORTS,
            _ => return invalid(format!("config option {key}")),
        };
        if !allowed.contains(&value.as_str()) {
            return invalid(format!("{key} {value}"));
        }
        let effort = {
            let Ok(mut sessions) = self.sessions.lock() else {
                return Err(json!({"code": -32603, "message": "lock"}));
            };
            let Some(data) = sessions.get_mut(&session) else {
                return Err(
                    json!({"code": -32002, "message": format!("session {session} not found")}),
                );
            };
            match key.as_str() {
                "mode" => data.mode.clone_from(&value),
                "model" => data.model.clone_from(&value),
                _ => data.effort.clone_from(&value),
            }
            data.effort.clone()
        };
        self.log(&json!({"agent": self.name, "pid": std::process::id(), "event": "set", "sessionId": session, "key": key, "value": value}));
        match key.as_str() {
            "mode" => {
                self.update(
                    &session,
                    &json!({"sessionUpdate": "current_mode_update", "currentModeId": value}),
                );
                Ok(json!({}))
            }
            "model" => Ok(json!({})),
            _ => Ok(json!({"configOptions": config_options(&effort)})),
        }
    }

    pub(super) fn options_note(&self, session: &str) -> String {
        self.sessions
            .lock()
            .ok()
            .and_then(|s| {
                s.get(session).map(|d| {
                    format!(
                        "options=mode:{},model:{},effort:{}",
                        d.mode, d.model, d.effort
                    )
                })
            })
            .unwrap_or_else(|| "options=unknown".to_owned())
    }
}
