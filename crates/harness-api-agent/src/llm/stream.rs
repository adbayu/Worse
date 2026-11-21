use super::*;

/// Accumulates streamed deltas into a [`Completion`] and forwards text to the client.
pub(super) struct Accumulator<'a> {
    pub(super) agent: &'a Agent,
    pub(super) session: &'a str,
    pub(super) out: Completion,
    pub(super) calls: Vec<(String, String, String)>,
}

impl Accumulator<'_> {
    pub(super) fn text(&mut self, kind: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        if kind == "agent_message_chunk" {
            self.out.content.push_str(text);
        } else {
            self.out.reasoning.push_str(text);
        }
        self.agent.update(
            self.session,
            &json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}}),
        );
    }

    pub(super) fn usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else { return };
        let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
        self.out.input_tokens = self.out.input_tokens.max(n("prompt_tokens"));
        self.out.output_tokens = self.out.output_tokens.max(n("completion_tokens"));
        let (read, write) = cache_tokens(usage);
        self.out.cache_read_tokens = self.out.cache_read_tokens.max(read);
        self.out.cache_write_tokens = self.out.cache_write_tokens.max(write);
    }

    pub(super) fn tool_delta(&mut self, call: &Value) {
        let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
        let index = match call.get("index").and_then(Value::as_u64) {
            Some(i) => usize::try_from(i).unwrap_or(usize::MAX),
            // 部分兼容服务不带 index：新 id 视为新调用，否则续写最后一个。
            None if !id.is_empty() && self.calls.last().map_or(true, |c| c.0 != id) => {
                self.calls.len()
            }
            None => self.calls.len().saturating_sub(1),
        };
        if index == usize::MAX {
            return;
        }
        while self.calls.len() <= index {
            self.calls
                .push((String::new(), String::new(), String::new()));
        }
        let slot = &mut self.calls[index];
        if !id.is_empty() {
            id.clone_into(&mut slot.0);
        }
        if let Some(f) = call.get("function") {
            if let Some(name) = f.get("name").and_then(Value::as_str) {
                slot.1.push_str(name);
            }
            if let Some(args) = f.get("arguments") {
                match args {
                    Value::String(s) => slot.2.push_str(s),
                    Value::Null => {}
                    other => slot.2.push_str(&other.to_string()),
                }
            }
        }
    }

    /// One parsed chunk (streamed) or the whole response (non-streamed).
    pub(super) fn chunk(&mut self, value: &Value) -> Result<(), Value> {
        if let Some(error) = value.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| error.to_string(), str::to_owned);
            return Err(
                json!({"code": -32603, "message": format!("{}: {}", self.agent.provider.label, preview(self.agent, &message))}),
            );
        }
        self.usage(value.get("usage"));
        let Some(choice) = value.pointer("/choices/0") else {
            return Ok(());
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.out.finish_reason = Some(reason.to_owned());
        }
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        let Some(delta) = delta else { return Ok(()) };
        for key in ["reasoning_content", "reasoning"] {
            if let Some(text) = delta.get(key).and_then(Value::as_str) {
                self.text("agent_thought_chunk", text);
            }
        }
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            self.text("agent_message_chunk", text);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                self.tool_delta(call);
            }
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Completion {
        for (i, (id, name, arguments)) in self.calls.into_iter().enumerate() {
            if name.is_empty() {
                continue;
            }
            self.out.tool_calls.push(ToolCall {
                id: if id.is_empty() {
                    format!("call_{i}_{}", now_secs())
                } else {
                    id
                },
                name,
                arguments,
            });
        }
        self.out
    }
}
