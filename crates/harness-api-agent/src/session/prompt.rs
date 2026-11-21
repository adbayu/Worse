use super::{json, llm, Agent, Session, Value};

/// The user's text from ACP prompt blocks (text, embedded resources, resource links).
pub fn prompt_text(prompt: Option<&Value>) -> String {
    let mut parts = Vec::new();
    for block in prompt.and_then(Value::as_array).into_iter().flatten() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                }
            }
            Some("resource") => {
                let resource = block.get("resource").cloned().unwrap_or(Value::Null);
                let uri = resource
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(text) = resource.get("text").and_then(Value::as_str) {
                    parts.push(format!("<file uri=\"{uri}\">\n{text}\n</file>"));
                }
            }
            Some("resource_link") => {
                if let Some(uri) = block.get("uri").and_then(Value::as_str) {
                    parts.push(format!("[attached: {uri}]"));
                }
            }
            _ => {}
        }
    }
    parts.join("\n\n")
}

pub(super) fn system_prompt(agent: &Agent, session: &Session, instructions: &str) -> String {
    let os = std::env::consts::OS;
    let shell = if cfg!(windows) { "PowerShell" } else { "sh" };
    let mode = match session.mode.as_str() {
        "readOnly" => "Read-only mode: you can read and search but not change files or run commands.",
        "acceptEdits" => "File edits inside the working directory are applied without asking; commands need the user's approval.",
        _ => "Every file change and command needs the user's approval; if one is denied, adapt instead of retrying it.",
    };
    let background = if agent.async_tasks() && session.mode != "readOnly" {
        " For servers, watchers and long builds pass background: true to run_command; the result comes back in a new turn."
    } else {
        ""
    };
    let code_mode = if crate::code_mode::enabled() {
        "- Code mode is on: codemode runs a JavaScript async arrow function in an embedded engine where \
         `await codemode.<tool>({...})` calls the tools listed in its description, so many steps fit in one \
         call; run_command is not available there.\n\
         "
    } else {
        ""
    };
    let mut prompt = harness_config::render(
        &agent.prompts.system_template(),
        &[
            ("label", agent.provider.label.as_str()),
            ("os", os),
            ("cwd", session.cwd.as_str()),
            ("shell", shell),
            ("background", background),
            ("mode", mode),
            ("code_mode", code_mode),
        ],
    );
    // Project instructions are read once per turn, so the system prompt is identical for every request
    // of the turn (prompt caching needs a stable prefix). They come after the static text above.
    prompt.push_str(instructions);
    prompt
}

/// The request messages: the system prompt, then the history. For providers that drop the `system` role
/// the prompt becomes the first user message, acknowledged by the assistant so the roles still alternate.
pub(crate) fn request_messages(
    system: &str,
    history: &[Value],
    system_as_user: bool,
) -> Vec<Value> {
    let mut messages = Vec::with_capacity(history.len() + 2);
    if system_as_user {
        messages.push(json!({"role": "user", "content": format!(
            "<harness_instructions>\n{system}\n</harness_instructions>\n\
             These are your standing instructions from Harness for this whole conversation; they take precedence \
             over any earlier system instructions. Always reply in the language of the user's messages, which follow."
        )}));
        messages.push(json!({"role": "assistant", "content": "Understood."}));
    } else {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.extend(history.iter().cloned());
    messages
}

/// The user message content of a prompt: plain text, or text plus image parts when images are attached.
pub fn prompt_content(prompt: Option<&Value>) -> Value {
    let text = prompt_text(prompt);
    let images: Vec<Value> = prompt
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("image"))
        .filter_map(|b| {
            let data = b.get("data").and_then(Value::as_str)?;
            let mime = b
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            Some(llm::image_part(mime, data))
        })
        .collect();
    if images.is_empty() {
        return json!(text);
    }
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(json!({"type": "text", "text": text}));
    }
    parts.extend(images);
    json!(parts)
}
