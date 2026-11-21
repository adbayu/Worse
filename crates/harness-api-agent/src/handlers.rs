use super::{
    hermes, json, session, text_param, Agent, Arc, CancellationToken, Reply, Session, Value,
};

impl Agent {
    pub(super) async fn handle(&self, method: &str, params: &Value) -> Reply {
        match method {
            "initialize" => {
                self.note_client(params);
                Ok(self.initialize_result())
            }
            "_session/async_task/stop" => {
                Ok(json!({"stopped": self.jobs.stop(&text_param(params, "asyncTaskId"))}))
            }
            "session/new" => Ok(self.new_session(params).await),
            "session/load" => self.load_session(params).await,
            "session/prompt" => self.prompt(params).await,
            "session/set_mode" => self.set_mode(params).await,
            "session/set_model" => {
                let model = text_param(params, "modelId");
                if model == self.provider.model {
                    Ok(json!({}))
                } else {
                    Err(json!({"code": -32602, "message": format!("unknown model {model}")}))
                }
            }
            _ => Err(json!({"code": -32601, "message": format!("unknown method {method}")})),
        }
    }

    pub(super) fn initialize_result(&self) -> Value {
        json!({
                "protocolVersion": 1,
                "agentCapabilities": {
                    "loadSession": true,
                    "promptCapabilities": {"image": true, "audio": false, "embeddedContext": true},
                    "mcpCapabilities": {"http": false, "sse": false}
                },
                "authMethods": [],
                "agentInfo": {"name": "harness-api-agent", "title": self.provider.label, "version": env!("CARGO_PKG_VERSION")}
        })
    }

    pub(super) fn advertisement(&self, mode: &str) -> Value {
        json!({
            "modes": {
                "currentModeId": mode,
                "availableModes": [
                    {"id": "default", "name": "Ask", "description": "Ask before every file change and command"},
                    {"id": "acceptEdits", "name": "Accept edits", "description": "Edit files in the project without asking; commands still ask"},
                    {"id": "readOnly", "name": "Read only", "description": "Read and search only; no changes, no commands"}
                ]
            },
            "models": {
                "currentModelId": self.provider.model,
                "availableModels": [{"modelId": self.provider.model, "name": self.provider.model}]
            }
        })
    }

    pub(super) async fn new_session(&self, params: &Value) -> Value {
        let id = format!("api-{}", uuid::Uuid::new_v4().simple());
        let session = Session::new(
            text_param(params, "cwd"),
            params
                .get("mcpServers")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        let mut result = self.advertisement(&session.mode);
        if hermes::is_gateway(self).await {
            // A gateway decides approvals by its own rules: Harness's modes do not apply.
            result["modes"] = json!({"currentModeId": "default", "availableModes": []});
        }
        session
            .persist(self.provider.state_dir.as_deref(), &id)
            .await;
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(id.clone(), Arc::new(tokio::sync::Mutex::new(session)));
        }
        result["sessionId"] = json!(id);
        result
    }

    pub(super) async fn load_session(&self, params: &Value) -> Reply {
        let id = text_param(params, "sessionId");
        let mcp = params
            .get("mcpServers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let loaded = if let Some(existing) = self.session(&id) {
            let mut s = existing.lock().await;
            s.set_mcp(mcp);
            (s.transcript(), s.mode.clone())
        } else {
            let Some(session) =
                Session::restore(self.provider.state_dir.as_deref(), &id, mcp).await
            else {
                return Err(json!({"code": -32002, "message": format!("session {id} not found")}));
            };
            let out = (session.transcript(), session.mode.clone());
            if let Ok(mut sessions) = self.sessions.lock() {
                sessions.insert(id.clone(), Arc::new(tokio::sync::Mutex::new(session)));
            }
            out
        };
        let (transcript, mode) = loaded;
        let gateway = hermes::is_gateway(self).await;
        // ACP: replay the conversation as updates before answering.
        for (user, text) in transcript {
            let kind = if user {
                "user_message_chunk"
            } else {
                "agent_message_chunk"
            };
            self.update(
                &id,
                &json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}}),
            );
        }
        let mut result = self.advertisement(&mode);
        if gateway {
            result["modes"] = json!({"currentModeId": "default", "availableModes": []});
        }
        Ok(result)
    }

    pub(super) async fn set_mode(&self, params: &Value) -> Reply {
        let id = text_param(params, "sessionId");
        let mode = text_param(params, "modeId");
        if !matches!(mode.as_str(), "default" | "acceptEdits" | "readOnly") {
            return Err(json!({"code": -32602, "message": format!("invalid mode {mode}")}));
        }
        let Some(session) = self.session(&id) else {
            return Err(json!({"code": -32002, "message": format!("session {id} not found")}));
        };
        {
            let mut s = session.lock().await;
            s.mode.clone_from(&mode);
            s.persist(self.provider.state_dir.as_deref(), &id).await;
        }
        self.update(
            &id,
            &json!({"sessionUpdate": "current_mode_update", "currentModeId": mode}),
        );
        Ok(json!({}))
    }

    pub(super) async fn prompt(&self, params: &Value) -> Reply {
        let id = text_param(params, "sessionId");
        let Some(session) = self.session(&id) else {
            return Err(json!({"code": -32002, "message": format!("session {id} not found")}));
        };
        let user = session::prompt_content(params.get("prompt"));
        let token = CancellationToken::new();
        if let Ok(mut map) = self.cancels.lock() {
            map.insert(id.clone(), token.clone());
        }
        let mut s = session.lock().await;
        let result = if hermes::is_gateway(self).await {
            hermes::run_turn(self, &id, &mut s, user, &token).await
        } else {
            session::run_turn(self, &id, &mut s, user, &token).await
        };
        s.persist(self.provider.state_dir.as_deref(), &id).await;
        drop(s);
        if let Ok(mut map) = self.cancels.lock() {
            map.remove(&id);
        }
        result
    }
}
