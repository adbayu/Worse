use super::{json, Coordinate, Duration, Enigo, Key, Mouse, Value};

fn point_schema(extra: &Value) -> Value {
    let mut properties = json!({
        "x": {"type": "number", "description": "X in pixels of the last screenshot"},
        "y": {"type": "number", "description": "Y in pixels of the last screenshot"}
    });
    if let (Some(base), Some(more)) = (properties.as_object_mut(), extra.as_object()) {
        for (k, v) in more {
            base.insert(k.clone(), v.clone());
        }
    }
    json!({"type": "object", "properties": properties, "required": ["x", "y"]})
}

pub(super) fn tools() -> Value {
    json!([
        {"name": "screenshot", "description": "Capture a monitor of this computer (primary by default). Coordinates of the other tools are pixels of the last screenshot.",
         "inputSchema": {"type": "object", "properties": {"monitor": {"type": "integer", "description": "Monitor index from the screenshot text"}}}},
        {"name": "click", "description": "Click at a point of the last screenshot.",
         "inputSchema": point_schema(&json!({"button": {"type": "string", "enum": ["left", "right", "middle"]}, "count": {"type": "integer", "description": "1 = click, 2 = double click"}}))},
        {"name": "move", "description": "Move the mouse to a point of the last screenshot.", "inputSchema": point_schema(&json!({}))},
        {"name": "drag", "description": "Drag with the left button from (x, y) to (to_x, to_y).",
         "inputSchema": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}, "to_x": {"type": "number"}, "to_y": {"type": "number"}}, "required": ["x", "y", "to_x", "to_y"]}},
        {"name": "scroll", "description": "Scroll at a point; dy > 0 scrolls down, dx > 0 scrolls right (wheel notches).",
         "inputSchema": point_schema(&json!({"dx": {"type": "integer"}, "dy": {"type": "integer"}}))},
        {"name": "type", "description": "Type text into the focused control.",
         "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
        {"name": "key", "description": "Press keys, e.g. \"enter\", \"ctrl+c\", \"alt+tab\", \"ctrl+shift+t\". Several combos may be separated by spaces.",
         "inputSchema": {"type": "object", "properties": {"keys": {"type": "string"}}, "required": ["keys"]}},
        {"name": "windows", "description": "List visible windows: title, app and screen rectangle.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "cursor", "description": "Current mouse position in pixels of the last screenshot.", "inputSchema": {"type": "object", "properties": {}}}
    ])
}

pub(super) fn text(message: &str) -> Value {
    json!({"content": [{"type": "text", "text": message}]})
}

pub(super) fn failure(message: &str) -> Value {
    json!({"content": [{"type": "text", "text": message}], "isError": true})
}

pub(super) fn num(args: &Value, key: &str) -> Result<f64, String> {
    args.get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing number {key}"))
}

pub(super) fn int(args: &Value, key: &str, default: i64) -> i64 {
    args.get(key).and_then(Value::as_i64).unwrap_or(default)
}

pub(super) fn pause() {
    std::thread::sleep(Duration::from_millis(60));
}

/// Moves the pointer to (x, y) along a short eased path instead of jumping there, so a person watching the
/// screen can follow what the agent does. `HARNESS_MACHINE_GLIDE=0` restores the jump.
pub(super) fn glide(input: &mut Enigo, x: i32, y: i32) -> Result<(), String> {
    let jump = std::env::var("HARNESS_MACHINE_GLIDE")
        .is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("false"));
    if let (false, Ok((sx, sy))) = (jump, input.location()) {
        let (dx, dy) = (f64::from(x - sx), f64::from(y - sy));
        let distance = dx.hypot(dy);
        if distance >= 6.0 {
            let (steps, each) = glide_plan(distance);
            for i in 1..steps {
                // Smoothstep: slow start, slow arrival.
                let t = f64::from(i) / f64::from(steps);
                let e = t * t * (3.0 - 2.0 * t);
                input
                    .move_mouse(
                        sx + (dx * e).round() as i32,
                        sy + (dy * e).round() as i32,
                        Coordinate::Abs,
                    )
                    .map_err(|e| e.to_string())?;
                std::thread::sleep(each);
            }
        }
    }
    input
        .move_mouse(x, y, Coordinate::Abs)
        .map_err(|e| e.to_string())
}

/// Steps and pause per step of a glide over `distance` pixels: 120 ms for a short hop, 360 ms across the
/// screen, about one step per 16 ms.
pub(super) fn glide_plan(distance: f64) -> (i32, Duration) {
    let total_ms = (120.0 + distance * 0.2).clamp(120.0, 360.0);
    let steps = (total_ms / 16.0).round().clamp(6.0, 24.0) as i32;
    (
        steps,
        Duration::from_micros((total_ms * 1000.0 / f64::from(steps)) as u64),
    )
}

/// First characters of typed text for the live view (one line, no more than 60 characters).
pub(super) fn typed_preview(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut out: String = flat.chars().take(60).collect();
    if flat.chars().count() > 60 {
        out.push('…');
    }
    out
}

pub(super) fn key_of(name: &str) -> Result<Key, String> {
    let lower = name.trim().to_ascii_lowercase();
    Ok(match lower.as_str() {
        "ctrl" | "control" => Key::Control,
        "shift" => Key::Shift,
        "alt" | "option" => Key::Alt,
        "win" | "windows" | "super" | "cmd" | "command" | "meta" => Key::Meta,
        "enter" | "return" => Key::Return,
        "tab" => Key::Tab,
        "esc" | "escape" => Key::Escape,
        "space" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "capslock" => Key::CapsLock,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        "plus" => Key::Unicode('+'),
        _ => {
            let mut chars = lower.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Unicode(c),
                _ => return Err(format!("unknown key {name}")),
            }
        }
    })
}

pub(super) fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(char::from(TABLE[(n >> 18) as usize & 63]));
        out.push(char::from(TABLE[(n >> 12) as usize & 63]));
        out.push(if chunk.len() > 1 {
            char::from(TABLE[(n >> 6) as usize & 63])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(TABLE[n as usize & 63])
        } else {
            '='
        });
    }
    out
}
