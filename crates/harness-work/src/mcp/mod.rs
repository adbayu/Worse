//! The `harness-work` MCP server (stdio, JSON lines). Agents create, check, render, export and
//! import Work items here; they edit the source files with their own file tools.
//!
//! The project root is `HARNESS_WORK_PROJECT`, else the working directory.

mod sheet;
mod sheet_ops;
mod tools;

use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{json, Value};

use crate::item::WorkItem;
use crate::WORK_DIR;

pub(crate) fn text(s: &str) -> Value {
    json!({"content": [{"type": "text", "text": s}]})
}

pub(crate) fn failure(s: &str) -> Value {
    json!({"content": [{"type": "text", "text": s}], "isError": true})
}

pub(crate) fn arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub(crate) struct Server {
    pub project: PathBuf,
    /// Last evaluated sheet model, keyed by item folder and source mtimes (one at a time).
    pub sheets: RefCell<Option<sheet::Cached>>,
}

impl Server {
    pub(crate) fn new(project: PathBuf) -> Self {
        Self {
            project,
            sheets: RefCell::new(None),
        }
    }

    /// A path, a source file, or a bare slug (`q3-review`) → the item.
    pub(crate) fn item(&self, path: &str) -> Result<WorkItem, String> {
        let p = path.trim();
        if p.is_empty() {
            return Err("path is required (the item folder, its source file, or its slug)".into());
        }
        let raw = Path::new(p.split('#').next().unwrap_or_default());
        let candidates = [
            if raw.is_absolute() {
                raw.to_path_buf()
            } else {
                self.project.join(raw)
            },
            self.project.join(WORK_DIR).join(raw),
        ];
        let mut last = String::new();
        for c in candidates {
            match WorkItem::open(&c) {
                Ok(item) => return Ok(item),
                Err(e) => last = e.to_string(),
            }
        }
        Err(last)
    }

    pub(crate) fn file(&self, path: &str) -> PathBuf {
        let p = Path::new(path.trim());
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.project.join(p)
        }
    }
}

fn project() -> PathBuf {
    std::env::var_os("HARNESS_WORK_PROJECT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

/// Serves MCP on stdin/stdout until stdin closes.
pub fn run() -> ExitCode {
    let server = Server::new(project());
    // 预热字体，首个渲染请求不再等待系统字体扫描。
    std::thread::spawn(crate::render::warm_up);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let reply = match method {
            "initialize" => json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or_else(|| json!("2024-11-05")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "harness-work", "version": env!("CARGO_PKG_VERSION")},
                "instructions": tools::INSTRUCTIONS,
            }}),
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "tools/list" => {
                json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools::definitions()}})
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                json!({"jsonrpc": "2.0", "id": id, "result": server.call(name, &args)})
            }
            other => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unknown method {other}")}})
            }
        };
        if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
            break;
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests;
