//! Minimal MCP stdio client for the servers passed in ACP `session/new`.
//! Harness's own servers start with the session's first prompt; every other server starts only when the model
//! loads its tools (`mcp_load_tools`), so their definitions are not resent with every request. Started servers
//! are kept until the session is dropped.

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::oneshot;

// 修复：拆分时 #[cfg(windows)] 误贴到了 mod server 上，导致 Linux/macOS 编译失败；它只属于这个常量。
mod server;

use server::{sanitize, Server};
#[cfg_attr(not(windows), allow(dead_code))]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// `initialize` and `tools/list`: a server that is not up by then (e.g. its desktop app is closed) is skipped.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest function name the chat API accepts (`^[a-zA-Z0-9_-]{1,64}$`).
const NAME_MAX: usize = 64;

/// The meta tool that loads the tools of on-demand servers.
pub const LOAD_TOOL: &str = "mcp_load_tools";
/// Most tools one `mcp_load_tools` call activates.
const LOAD_MAX: usize = 15;
/// A server with at most this many tools is activated whole when no query is given.
const LOAD_ALL_MAX: usize = 40;

/// Harness's own servers (memory, delegate, computer) are small and part of the workflow: always loaded.
/// Every other server is on demand, so its tool definitions are not resent with every request.
fn eager(name: &str) -> bool {
    name.starts_with("harness-")
}

struct Listed {
    function: String,
    description: String,
    definition: Value,
}

#[derive(Default)]
struct Inner {
    servers: HashMap<String, Arc<Server>>,
    /// Server → its tools, once started and listed.
    catalog: HashMap<String, Vec<Listed>>,
    failed: HashMap<String, String>,
    /// Function name → (server, MCP tool name).
    tools: HashMap<String, (String, String)>,
    /// Definitions sent to the model, in activation order (stable prefix for prompt caching).
    active: Vec<Value>,
    active_names: HashSet<String>,
}

/// The MCP servers of one session. Eager servers start with the session; the others start the first time
/// the model loads their tools with `mcp_load_tools` (or calls one of their tools by name).
pub struct McpSet {
    /// (name, spec), in configuration order.
    specs: Vec<(String, Value)>,
    inner: tokio::sync::Mutex<Inner>,
}

impl Default for McpSet {
    fn default() -> Self {
        Self::new(&[])
    }
}

async fn start_listed(spec: &Value) -> Result<(Server, Vec<Value>), String> {
    let server = Server::start(spec).await?;
    let listed = server
        .call_within("tools/list", json!({}), START_TIMEOUT)
        .await?;
    let tools = listed
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok((server, tools))
}

/// The first sentence of a tool description, for the `mcp_load_tools` result.
fn first_sentence(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let end = line.find(". ").map_or(line.len(), |i| i + 1);
    line[..end].chars().take(160).collect()
}

/// Tools of `tools` matching the query words, best first (all tools when there are no words).
fn rank<'a>(tools: &'a [Listed], words: &[&str], max: usize) -> Vec<&'a Listed> {
    let mut ranked: Vec<(usize, &Listed)> = tools
        .iter()
        .map(|t| {
            let haystack = format!("{} {}", t.function, t.description).to_lowercase();
            (words.iter().filter(|w| haystack.contains(**w)).count(), t)
        })
        .filter(|(score, _)| words.is_empty() || *score > 0)
        .collect();
    ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    ranked.into_iter().take(max).map(|(_, t)| t).collect()
}

impl Inner {
    fn insert(&mut self, name: &str, server: Server, tools: &[Value]) {
        let mut listed = Vec::new();
        for tool in tools {
            let Some(tool_name) = tool.get("name").and_then(Value::as_str) else {
                continue;
            };
            let mut function: String = sanitize(&format!("mcp__{name}__{tool_name}"))
                .chars()
                .take(NAME_MAX)
                .collect();
            while self.tools.contains_key(&function) {
                function = format!(
                    "{}_",
                    function.chars().take(NAME_MAX - 1).collect::<String>()
                );
            }
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let parameters = tool
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            let definition = json!({"type": "function", "function": {
                "name": function,
                "description": description,
                "parameters": parameters
            }});
            self.tools
                .insert(function.clone(), (name.to_owned(), tool_name.to_owned()));
            listed.push(Listed {
                function,
                description,
                definition,
            });
        }
        self.catalog.insert(name.to_owned(), listed);
        self.servers.insert(name.to_owned(), Arc::new(server));
    }

    fn activate(&mut self, server: &str, function: &str) {
        if self.active_names.contains(function) {
            return;
        }
        if let Some(tool) = self
            .catalog
            .get(server)
            .and_then(|tools| tools.iter().find(|t| t.function == function))
        {
            self.active.push(tool.definition.clone());
            self.active_names.insert(function.to_owned());
        }
    }
}

impl McpSet {
    pub fn new(specs: &[Value]) -> Self {
        let specs = specs
            .iter()
            .map(|spec| {
                let name = spec
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("mcp")
                    .to_owned();
                (name, spec.clone())
            })
            .collect();
        Self {
            specs,
            inner: tokio::sync::Mutex::new(Inner::default()),
        }
    }

    /// Starts the eager servers in parallel and activates all their tools.
    pub async fn start_eager(&self) {
        let started = futures_util::future::join_all(
            self.specs
                .iter()
                .filter(|(name, _)| eager(name))
                .map(|(name, spec)| async move { (name, start_listed(spec).await) }),
        )
        .await;
        let mut inner = self.inner.lock().await;
        for (name, result) in started {
            match result {
                Ok((server, tools)) => {
                    inner.insert(name, server, &tools);
                    let functions: Vec<String> = inner
                        .catalog
                        .get(name.as_str())
                        .map(|tools| tools.iter().map(|t| t.function.clone()).collect())
                        .unwrap_or_default();
                    for function in functions {
                        inner.activate(name, &function);
                    }
                }
                Err(error) => {
                    inner.failed.insert(name.clone(), error);
                }
            }
        }
    }

    /// The on-demand servers, for the `mcp_load_tools` schema.
    fn deferred(&self) -> Vec<&str> {
        self.specs
            .iter()
            .map(|(name, _)| name.as_str())
            .filter(|name| !eager(name))
            .collect()
    }

    /// Tool definitions for the next request: the active MCP tools plus `mcp_load_tools`.
    pub async fn definitions(&self) -> Vec<Value> {
        let mut out = self.inner.lock().await.active.clone();
        let deferred = self.deferred();
        if !deferred.is_empty() {
            out.push(json!({"type": "function", "function": {
                "name": LOAD_TOOL,
                "description": "Load the tools of an MCP server (deployment, remote servers over SSH, design \
                    apps and more). These servers are not loaded by default to keep requests small: call this \
                    only when the task needs one, with a query naming what you want to do. The matching tools \
                    become callable from your next response.",
                "parameters": {"type": "object", "properties": {
                    "server": {"type": "string", "enum": deferred},
                    "query": {"type": "string", "description": "Words describing the tools you need, e.g. \"deploy logs\". Omit to list the server's tools."}
                }, "required": ["server"]}
            }}));
        }
        out
    }

    /// Whether `function` is answered by this set (the meta tool or an MCP tool).
    pub fn handles(function: &str) -> bool {
        function == LOAD_TOOL || function.starts_with("mcp__")
    }

    /// Starts and lists `server` if needed.
    async fn ensure(&self, inner: &mut Inner, server: &str) -> Result<(), String> {
        if inner.catalog.contains_key(server) {
            return Ok(());
        }
        if let Some(error) = inner.failed.get(server) {
            return Err(format!("MCP server {server} is unavailable: {error}"));
        }
        let spec = self
            .specs
            .iter()
            .find(|(name, _)| name == server)
            .map(|(_, spec)| spec)
            .ok_or_else(|| format!("unknown MCP server {server}"))?;
        match start_listed(spec).await {
            Ok((started, tools)) => {
                inner.insert(server, started, &tools);
                Ok(())
            }
            Err(error) => {
                inner.failed.insert(server.to_owned(), error.clone());
                Err(format!("MCP server {server} is unavailable: {error}"))
            }
        }
    }

    async fn load(&self, args: &Value) -> Result<String, String> {
        let server = args
            .get("server")
            .and_then(Value::as_str)
            .ok_or("server is required")?;
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let mut inner = self.inner.lock().await;
        self.ensure(&mut inner, server).await?;
        let tools = inner.catalog.get(server).map_or(&[][..], Vec::as_slice);
        if words.is_empty() && tools.len() > LOAD_ALL_MAX {
            let names: Vec<&str> = tools.iter().map(|t| t.function.as_str()).collect();
            return Ok(format!(
                "{server} has {} tools; call {LOAD_TOOL} again with a query to load the ones you need:\n{}",
                names.len(),
                names.join("\n")
            ));
        }
        let max = if words.is_empty() {
            LOAD_ALL_MAX
        } else {
            LOAD_MAX
        };
        let picked: Vec<(String, String)> = rank(tools, &words, max)
            .into_iter()
            .map(|t| (t.function.clone(), first_sentence(&t.description)))
            .collect();
        if picked.is_empty() {
            return Ok(format!(
                "No {server} tool matches \"{query}\". Try other words or omit the query to list the tools."
            ));
        }
        let mut text = format!(
            "Loaded {} tool(s); call them directly from your next response:\n",
            picked.len()
        );
        for (function, summary) in &picked {
            inner.activate(server, function);
            text.push_str("- ");
            text.push_str(function);
            text.push_str(": ");
            text.push_str(summary);
            text.push('\n');
        }
        Ok(text)
    }

    /// Calls a tool; returns its text content (`Err` when the tool reports an error).
    pub async fn call(&self, function: &str, arguments: Value) -> Result<String, String> {
        if function == LOAD_TOOL {
            return self.load(&arguments).await;
        }
        let (server, tool) = {
            let mut inner = self.inner.lock().await;
            if !inner.tools.contains_key(function) {
                // A tool of a server that is not loaded yet, called by its full name: load it on the fly.
                let owner = self
                    .specs
                    .iter()
                    .map(|(name, _)| name)
                    .find(|name| function.starts_with(&sanitize(&format!("mcp__{name}__"))))
                    .cloned()
                    .ok_or_else(|| format!("unknown tool {function}"))?;
                self.ensure(&mut inner, &owner).await?;
                inner.activate(&owner, function);
            }
            let (name, tool) = inner
                .tools
                .get(function)
                .cloned()
                .ok_or_else(|| format!("unknown tool {function}"))?;
            let server = inner.servers.get(&name).cloned().ok_or("server gone")?;
            (server, tool)
        };
        let result = server
            .call("tools/call", json!({"name": tool, "arguments": arguments}))
            .await?;
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|c| c.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            Err(text)
        } else {
            Ok(text)
        }
    }
}

#[cfg(test)]
mod tests;
