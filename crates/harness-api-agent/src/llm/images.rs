use super::*;

pub(super) const IMAGE_NOTE: &str =
    "[image attached, but this model cannot view images; say so instead of guessing its content]";
pub(super) const IMAGE_GONE: &str = "[earlier image removed from context]";
pub(super) const IMAGE_COST: usize = 1500;

/// An OpenAI-style image part from base64 data.
pub fn image_part(mime: &str, data: &str) -> Value {
    json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{data}")}})
}

pub(super) fn is_image(part: &Value) -> bool {
    part.get("type").and_then(Value::as_str) == Some("image_url")
}

pub(super) fn image_parts(message: &Value) -> impl Iterator<Item = &Value> {
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| is_image(p))
}

pub fn has_images(messages: &[Value]) -> bool {
    messages.iter().any(|m| image_parts(m).next().is_some())
}

/// Serialized size of a message for the history budget; an image counts as a small fixed cost.
pub fn message_size(message: &Value) -> usize {
    let data: usize = image_parts(message)
        .map(|p| p["image_url"]["url"].as_str().map_or(0, str::len))
        .sum();
    let count = image_parts(message).count();
    message.to_string().len().saturating_sub(data) + count * IMAGE_COST
}

pub(super) fn replace_images(message: &mut Value, note: &str) {
    let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    if !parts.iter().any(is_image) {
        return;
    }
    let text = parts
        .iter()
        .map(|p| {
            if is_image(p) {
                note.to_owned()
            } else {
                p.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    message["content"] = json!(text);
}

/// `messages` with every image replaced by a text note (for models that reject image input).
pub fn strip_images(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let mut m = m.clone();
            replace_images(&mut m, IMAGE_NOTE);
            m
        })
        .collect()
}

/// Keeps images only in the newest messages that carry them, up to `keep` images.
pub fn prune_images(messages: &mut [Value], keep: usize) {
    let mut seen = 0;
    for message in messages.iter_mut().rev() {
        let count = image_parts(message).count();
        if count == 0 {
            continue;
        }
        if seen >= keep {
            replace_images(message, IMAGE_GONE);
        }
        seen += count;
    }
}

pub(super) fn image_rejection(status: u16, body: &str) -> bool {
    if !matches!(status, 400 | 404 | 415 | 422) {
        return false;
    }
    let body = body.to_ascii_lowercase();
    ["image", "vision", "modal"]
        .iter()
        .any(|w| body.contains(w))
}

/// One text part carrying an Anthropic-compatible cache breakpoint.
pub(super) fn marked_part(text: &str) -> Value {
    json!({"type": "text", "text": text, "cache_control": {"type": "ephemeral"}})
}

/// A message that can hold a cache breakpoint: not the system message, with content to mark.
pub(super) fn markable(message: &Value) -> bool {
    if message.get("role").and_then(Value::as_str) == Some("system") {
        return false;
    }
    match message.get("content") {
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(parts)) => parts.last().is_some_and(Value::is_object),
        _ => false,
    }
}

/// `messages` with prompt-cache breakpoints (a copy; the input is untouched):
/// one on the system message (the stable prefix) and one on the last non-system message.
pub fn with_cache_breakpoints(messages: &[Value]) -> Vec<Value> {
    let mut out = messages.to_vec();
    if let Some(system) = out
        .iter_mut()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("system"))
    {
        if let Some(text) = content_text(system.get("content")) {
            if !text.is_empty() {
                system["content"] = json!([marked_part(&text)]);
            }
        }
    }
    // An assistant message with empty content cannot hold a breakpoint: fall back to the one before it.
    if let Some(i) = out.iter().rposition(markable) {
        let message = &mut out[i];
        match message.get("content") {
            Some(Value::String(text)) => {
                let text = text.clone();
                message["content"] = json!([marked_part(&text)]);
            }
            Some(Value::Array(parts)) => {
                let mut parts = parts.clone();
                if let Some(last) = parts.last_mut().and_then(Value::as_object_mut) {
                    last.insert("cache_control".into(), json!({"type": "ephemeral"}));
                }
                message["content"] = Value::Array(parts);
            }
            _ => {}
        }
    }
    out
}
