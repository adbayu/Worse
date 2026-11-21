//! Built-in coding tools of the API agent and their ACP reporting/permissions (phase-9-parity.md §9.7).

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::llm::ToolCall;
use crate::mcp::McpSet;
use crate::Agent;

mod command;
// 修复：#[cfg(windows)] 误贴到 mod files 上，Linux/macOS 上缺少 read_file 等工具导致编译失败。
mod files;
mod provider;

pub use provider::{providers, ToolEnv, ToolProvider};

#[cfg_attr(not(windows), allow(dead_code))]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Longest tool result handed back to the model.
pub const OUTPUT_MAX: usize = 30_000;
const READ_LINES: usize = 2000;
const LINE_MAX: usize = 2000;
const READ_BYTES_MAX: u64 = 10 * 1024 * 1024;
const LIST_MAX: usize = 1000;
const COMMAND_DEFAULT_SECS: u64 = 120;
const COMMAND_MAX_SECS: u64 = 600;

const WRITE_TOOLS: [&str; 3] = ["write_file", "edit_file", "run_command"];

fn function(name: &str, description: &str, parameters: &Value) -> Value {
    json!({"type": "function", "function": {"name": name, "description": description, "parameters": parameters}})
}

/// Chat-API tool definitions of the built-in tools for a mode (`readOnly` hides the write tools).
pub fn definitions(mode: &str, background: bool) -> Vec<Value> {
    let path = json!({"type": "string", "description": "File path, absolute or relative to the working directory"});
    let mut out = vec![
        function(
            "read_file",
            "Read a text file. Returns numbered lines. Use offset/limit for large files; take a generous range \
             once instead of several small reads of the same file.",
            &json!({"type": "object", "properties": {
                "path": path,
                "offset": {"type": "integer", "description": "First line to read (1-based)"},
                "limit": {"type": "integer", "description": "Number of lines (default 2000)"}
            }, "required": ["path"]}),
        ),
        function(
            "list_dir",
            "List the entries of a directory (directories end with /).",
            &json!({"type": "object", "properties": {"path": {"type": "string", "description": "Directory, default the working directory"}}}),
        ),
        function(
            "search",
            "Search file contents with a regular expression. Fast and bounded: skips .gitignore'd, binary, \
             dependency and build folders, stops after 20 s with partial results. Returns path:line: text. \
             Use this instead of grep/Select-String/findstr in run_command. Combine alternatives in one pattern \
             (a|b|c); set path or glob to narrow it; files_only first when you only need where something is.",
            &json!({"type": "object", "properties": {
                "pattern": {"type": "string", "description": "Regular expression"},
                "path": {"type": "string", "description": "File or directory to search, default the working directory"},
                "glob": {"type": "string", "description": "Only files matching this glob, e.g. *.rs"},
                "case_insensitive": {"type": "boolean"},
                "files_only": {"type": "boolean", "description": "List matching files with their match counts instead of lines"},
                "context": {"type": "integer", "description": "Lines of context around each match, at most 3"}
            }, "required": ["pattern"]}),
        ),
    ];
    if mode != "readOnly" {
        out.push(function(
            "write_file",
            "Create or overwrite a file with the given content.",
            &json!({"type": "object", "properties": {"path": path, "content": {"type": "string"}}, "required": ["path", "content"]}),
        ));
        out.push(function(
            "edit_file",
            "Replace an exact string in a file. old_string must match exactly once unless replace_all is true. Read the file first.",
            &json!({"type": "object", "properties": {
                "path": path,
                "old_string": {"type": "string"},
                "new_string": {"type": "string"},
                "replace_all": {"type": "boolean"}
            }, "required": ["path", "old_string", "new_string"]}),
        ));
        out.push(function(
            "run_command",
            if cfg!(windows) {
                "Run a PowerShell command in the working directory and return its exit code and output."
            } else {
                "Run a shell command (sh -c) in the working directory and return its exit code and output."
            },
            &command_schema(background),
        ));
    }
    for provider in providers() {
        out.extend(provider.definitions());
    }
    out
}

fn command_schema(background: bool) -> Value {
    let mut schema = json!({"type": "object", "properties": {
        "command": {"type": "string"},
        "timeout_secs": {"type": "integer", "description": "Default 120, at most 600"}
    }, "required": ["command"]});
    if background {
        schema["properties"]["background"] = json!({
            "type": "boolean",
            "description": "Run without waiting (servers, long builds, watchers). Returns at once; the result arrives in a new turn when the command ends. No timeout."
        });
    }
    schema
}

pub fn shell(command: &str) -> tokio::process::Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut process = if cfg!(windows) {
        let mut p = tokio::process::Command::new("powershell.exe");
        p.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("[Console]::OutputEncoding=[Text.Encoding]::UTF8; {command}"),
        ]);
        p
    } else {
        let mut p = tokio::process::Command::new("sh");
        p.args(["-c", command]);
        p
    };
    #[cfg(windows)]
    process.creation_flags(CREATE_NO_WINDOW);
    process
}

/// Per-call context.
pub struct Ctx<'a> {
    pub agent: &'a Agent,
    pub session: &'a str,
    pub cwd: &'a Path,
    pub mode: &'a str,
    pub always: &'a mut HashSet<String>,
    pub token: &'a CancellationToken,
    pub mcp: &'a McpSet,
    pub images: &'a std::sync::Mutex<Vec<Value>>,
}

const IMAGE_BYTES_MAX: usize = 5 * 1024 * 1024;

fn image_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Lexical normalisation (no filesystem access, works for files that do not exist yet).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn writes(name: &str, args: &Value) -> bool {
    WRITE_TOOLS.contains(&name)
        || (crate::factcheck::handles(name) && crate::factcheck::writes(args))
}

fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let path = Path::new(path.trim());
    if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&cwd.join(path))
    }
}

pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let head: String = text.chars().take(max / 2).collect();
    let tail: String = {
        let rev: Vec<char> = text.chars().rev().take(max / 2).collect();
        rev.into_iter().rev().collect()
    };
    format!("{head}\n… [output truncated] …\n{tail}")
}

struct Outcome {
    text: String,
    ok: bool,
    content: Vec<Value>,
}

impl Outcome {
    fn ok(text: String) -> Self {
        Self {
            content: vec![
                json!({"type": "content", "content": {"type": "text", "text": truncate(&text, 4000)}}),
            ],
            text,
            ok: true,
        }
    }

    fn err(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            content: vec![json!({"type": "content", "content": {"type": "text", "text": text}})],
            text: format!("Error: {text}"),
            ok: false,
        }
    }
}

pub(crate) fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn title_of(name: &str, args: &Value) -> (String, &'static str) {
    let path = str_arg(args, "path").unwrap_or(".");
    match name {
        "read_file" => (format!("Read {path}"), "read"),
        "list_dir" => (format!("List {path}"), "read"),
        "search" => (
            format!("Search {}", str_arg(args, "pattern").unwrap_or_default()),
            "search",
        ),
        "write_file" => (format!("Write {path}"), "edit"),
        "edit_file" => (format!("Edit {path}"), "edit"),
        "run_command" => (
            format!(
                "Run {}",
                str_arg(args, "command")
                    .unwrap_or_default()
                    .chars()
                    .take(120)
                    .collect::<String>()
            ),
            "execute",
        ),
        other if crate::code_mode::handles(other) => (crate::code_mode::title(args), "execute"),
        other if crate::web::handles(other) => (crate::web::title(other, args), "fetch"),
        other if crate::factcheck::handles(other) => (crate::factcheck::title(args), "other"),
        other => (other.to_owned(), "other"),
    }
}

impl Ctx<'_> {
    fn update(&self, value: &Value) {
        self.agent.update(self.session, value);
    }

    fn needs_permission(&self, name: &str, args: &Value) -> bool {
        if !writes(name, args) || self.always.contains(name) {
            return false;
        }
        if name == "run_command" {
            return true;
        }
        let inside = str_arg(args, "path")
            .or_else(|| str_arg(args, "dir"))
            .is_some_and(|p| resolve(self.cwd, p).starts_with(normalize(self.cwd)));
        !(self.mode == "acceptEdits" && inside)
    }

    /// Asks through ACP; `true` when allowed.
    async fn ask(&mut self, name: &str, tool: &Value) -> bool {
        let response = self
            .agent
            .request(
                "session/request_permission",
                json!({
                    "sessionId": self.session,
                    "toolCall": tool,
                    "options": [
                        {"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
                        {"optionId": "allow-always", "name": "Always allow", "kind": "allow_always"},
                        {"optionId": "deny", "name": "Deny", "kind": "reject_once"}
                    ]
                }),
            )
            .await;
        let outcome = response.get("outcome").cloned().unwrap_or(Value::Null);
        if outcome.get("outcome").and_then(Value::as_str) != Some("selected") {
            return false;
        }
        match outcome.get("optionId").and_then(Value::as_str) {
            Some("allow") => true,
            Some("allow-always") => {
                self.always.insert(name.to_owned());
                true
            }
            _ => false,
        }
    }

    /// Runs one model tool call with ACP reporting; returns the text for the `tool` message.
    pub async fn run(&mut self, call: &ToolCall) -> String {
        let args: Value = if call.arguments.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str(&call.arguments) {
                Ok(args) => args,
                Err(e) => return format!("Error: arguments are not valid JSON ({e})"),
            }
        };
        let (title, kind) = title_of(&call.name, &args);
        let id = format!("{}-{}", call.id, uuid::Uuid::new_v4().simple());
        let mut tool = json!({"toolCallId": id, "title": title, "kind": kind, "status": "pending", "rawInput": args});
        if let Some(path) = str_arg(&args, "path") {
            tool["locations"] = json!([{"path": resolve(self.cwd, path).to_string_lossy()}]);
        }
        let mut update = tool.clone();
        update["sessionUpdate"] = json!("tool_call");
        self.update(&update);
        let outcome = if call.name == "run_command"
            && str_arg(&args, "command").is_some_and(crate::search::shell_search)
        {
            Outcome::err(
                "refused: recursive text search through the shell is slow and unbounded here. Use the search \
                 tool (pattern, path, glob, files_only, context) instead; it is faster and stops after 20 s.",
            )
        } else if self.mode == "readOnly" && writes(&call.name, &args) {
            Outcome::err("refused: the session is in read-only mode")
        } else if self.needs_permission(&call.name, &args) && !self.ask(&call.name, &tool).await {
            Outcome::err("the user denied this action")
        } else {
            self.update(&json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "in_progress"}));
            self.execute(&call.name, &args).await
        };
        self.update(&json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "status": if outcome.ok { "completed" } else { "failed" },
            "content": outcome.content
        }));
        truncate(&outcome.text, OUTPUT_MAX)
    }

    async fn execute(&mut self, name: &str, args: &Value) -> Outcome {
        if let Some(tools) = provider::provider_for(name) {
            let env = ToolEnv {
                http: &self.agent.http,
                cwd: self.cwd,
            };
            return match tools.run(env, name, args).await {
                Ok(text) => Outcome::ok(text),
                Err(text) => Outcome::err(text),
            };
        }
        match name {
            "read_file" => self.read_file(args).await,
            "list_dir" => self.list_dir(args).await,
            "search" => self.search(args).await,
            "write_file" => self.write_file(args).await,
            "edit_file" => self.edit_file(args).await,
            "run_command" => self.run_command(args).await,
            other if McpSet::handles(other) => match self.mcp.call(other, args.clone()).await {
                Ok(text) => Outcome::ok(text),
                Err(text) => Outcome::err(text),
            },
            other if crate::code_mode::handles(other) => {
                Outcome::ok(crate::code_mode::run(self, args).await)
            }
            other => Outcome::err(format!("unknown tool {other}")),
        }
    }
}
