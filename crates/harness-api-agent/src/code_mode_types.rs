//! Type declarations for the `codemode` tool description, rendered from the live tool list.

use serde_json::Value;

const HEADER: &str = "Execute code to achieve a goal.\nAvailable:\n";
const FOOTER: &str = "\nWrite an async arrow function in JavaScript that returns the result. \
Do NOT use TypeScript syntax. Do NOT define named functions then call them. Use it for multi-step \
work (many files, filtering, aggregating); return only the filtered result. After a write_file or \
edit_file call, read the file back with read_file before returning.\n";
const EXAMPLE: &str =
    "Example: async () => { const r = await codemode.list_dir({ path: \"src\" }); return \
r.split(\"\\n\").slice(0, 20).join(\"\\n\"); }";

const HIDDEN: [&str; 2] = ["run_command", "codemode"];

pub fn description(tools: &[Value]) -> String {
    let mut out = String::from(HEADER);
    let mut seen: Vec<String> = Vec::new();
    for tool in tools {
        let name = tool
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if name.is_empty() || HIDDEN.contains(&name) || seen.iter().any(|s| s.as_str() == name) {
            continue;
        }
        seen.push(name.to_string());
        out.push_str(&signature(tool, name));
        out.push('\n');
    }
    out.push_str(FOOTER);
    out.push_str(EXAMPLE);
    out
}

fn signature(tool: &Value, name: &str) -> String {
    let function = tool.get("function");
    let params = function
        .and_then(|f| f.get("parameters"))
        .and_then(|p| p.get("properties"))
        .and_then(Value::as_object);
    let fields = params.map(fields).unwrap_or_default();
    let doc = function
        .and_then(|f| f.get("description"))
        .and_then(Value::as_str)
        .map(one_line)
        .unwrap_or_default();
    if doc.is_empty() {
        format!("codemode.{name}(args: {{{fields}}}): Promise<string>")
    } else {
        format!("codemode.{name}(args: {{{fields}}}): Promise<string> // {doc}")
    }
}

fn fields(params: &serde_json::Map<String, Value>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (key, schema) in params {
        let ty = schema
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("any")
            .to_string();
        let ty = if ty == "array" {
            let item = schema
                .get("items")
                .and_then(|i| i.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("any");
            format!("{item}[]")
        } else {
            ty
        };
        parts.push(format!("{key}: {ty}"));
    }
    parts.join(", ")
}

fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(160).collect();
    if out.len() < flat.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Vec<Value> {
        vec![
            json!({"type": "function", "function": {
                "name": "read_file",
                "description": "Read a file.\nReturns text.",
                "parameters": {"type": "object", "properties": {
                    "path": {"type": "string"},
                    "lines": {"type": "array", "items": {"type": "integer"}}
                }}
            }}),
            json!({"type": "function", "function": {
                "name": "run_command",
                "description": "Run a shell command.",
                "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}
            }}),
            json!({"type": "function", "function": {
                "name": "codemode",
                "description": "Recursive.",
                "parameters": {"type": "object", "properties": {"code": {"type": "string"}}}
            }}),
        ]
    }

    #[test]
    fn includes_tools_and_excludes_hidden() {
        let text = description(&sample());
        assert!(text.contains("codemode.read_file(args: {"), "{text}");
        assert!(text.contains("path: string"), "{text}");
        assert!(text.contains("lines: integer[]"), "{text}");
        assert!(text.contains("// Read a file. Returns text."), "{text}");
        assert!(!text.contains("run_command"), "{text}");
        assert!(!text.contains("codemode.codemode"), "{text}");
        assert!(text.contains("Do NOT use TypeScript syntax."));
        assert!(text.contains("Example: async () =>"));
    }

    #[test]
    fn deduplicates_repeated_names() {
        let mut tools = sample();
        let repeat = tools[0].clone();
        tools.push(repeat);
        let text = description(&tools);
        assert_eq!(text.matches("codemode.read_file(").count(), 1, "{text}");
    }
}
