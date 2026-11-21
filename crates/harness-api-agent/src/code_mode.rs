//! Code mode (`HARNESS_CODE_MODE=1`): one `codemode` tool that runs a `JavaScript` async arrow
//! function in an embedded `QuickJS` engine where the agent's own tools are callable.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use crate::code_mode_engine::{self, Script};
use crate::tools::{self, Ctx};

pub const NAME: &str = "codemode";
const WALL: Duration = Duration::from_secs(60);

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("HARNESS_CODE_MODE").is_ok_and(|v| v.trim() == "1"))
}

pub fn handles(name: &str) -> bool {
    name == NAME
}

pub fn title(args: &Value) -> String {
    let snippet = tools::str_arg(args, "code")
        .and_then(|code| code.lines().map(str::trim).find(|line| !line.is_empty()))
        .map(|line| line.chars().take(80).collect::<String>())
        .filter(|line| !line.is_empty());
    match snippet {
        Some(snippet) => format!("Run code: {snippet}"),
        None => "Run code".to_owned(),
    }
}

pub fn definition(available: &[Value]) -> Value {
    json!({"type": "function", "function": {
        "name": NAME,
        "description": crate::code_mode_types::description(available),
        "parameters": {"type": "object", "properties": {
            "code": {"type": "string", "description": "JavaScript async arrow function to execute"}
        }, "required": ["code"]}
    }})
}

pub async fn run(ctx: &mut Ctx<'_>, args: &Value) -> String {
    let Some(code) = tools::str_arg(args, "code") else {
        return "Error: code is required".to_owned();
    };
    if code.trim().is_empty() {
        return "Error: code is empty".to_owned();
    }
    let (request_tx, mut request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let interrupted = Arc::new(AtomicBool::new(false));
    let script = Script::new(
        code.to_owned(),
        request_tx,
        reply_rx,
        Arc::clone(&interrupted),
        WALL,
    );
    let mut handle = tokio::task::spawn_blocking(move || code_mode_engine::run(script));
    let watchdog = code_mode_engine::watch(Arc::clone(&interrupted), WALL);
    let mut open = true;
    let payload = loop {
        tokio::select! {
            request = request_rx.recv(), if open => match request {
                Some(request) => {
                    let text = host_call(ctx, &request.name, &request.args).await;
                    let _ = reply_tx.send(text);
                }
                None => open = false,
            },
            finished = &mut handle => break finished,
        }
    };
    interrupted.store(true, Ordering::SeqCst);
    let _ = watchdog.join();
    match payload {
        Ok(text) => text,
        Err(error) => json!({"error": format!("code mode: {error}"), "logs": []}).to_string(),
    }
}

async fn host_call(ctx: &mut Ctx<'_>, name: &str, args: &str) -> String {
    if let Some(text) = refusal(name) {
        return text;
    }
    let call = crate::llm::ToolCall {
        id: format!("{NAME}-{}", uuid::Uuid::new_v4().simple()),
        name: name.to_owned(),
        arguments: args.to_owned(),
    };
    Box::pin(ctx.run(&call)).await
}

fn refusal(name: &str) -> Option<String> {
    (name == NAME || name == "run_command").then(|| {
        format!("Error: {name} is not available inside code mode; use the plain tool instead")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_lists_tools_and_hides_run_command() {
        let def = definition(&tools::definitions("default", false));
        let text = def["function"]["description"].as_str().unwrap_or_default();
        assert_eq!(def["function"]["name"], "codemode");
        assert!(text.contains("codemode.read_file(args: {"), "{text}");
        assert!(text.contains("codemode.write_file(args: {"), "{text}");
        assert!(!text.contains("run_command"), "{text}");
        assert!(!text.contains("codemode.codemode"), "{text}");
    }

    #[test]
    fn title_uses_the_first_code_line() {
        assert_eq!(title(&json!({})), "Run code");
        assert_eq!(
            title(&json!({"code": "\n  async () => { return 1; }\n"})),
            "Run code: async () => { return 1; }"
        );
    }

    #[test]
    fn schema_requires_code() {
        let def = definition(&[]);
        assert_eq!(def["function"]["parameters"]["required"], json!(["code"]));
        assert_eq!(
            def["function"]["parameters"]["properties"]["code"]["type"],
            json!("string")
        );
    }

    #[test]
    fn run_command_and_codemode_are_refused_inside_code_mode() {
        assert_eq!(
            refusal("run_command").unwrap_or_default(),
            "Error: run_command is not available inside code mode; use the plain tool instead"
        );
        assert_eq!(
            refusal(NAME).unwrap_or_default(),
            "Error: codemode is not available inside code mode; use the plain tool instead"
        );
        assert!(refusal("read_file").is_none());
    }
}
