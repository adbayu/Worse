use super::{
    check, json, BufRead, BufReader, Child, ChildStdin, ChildStdout, Command, Path, Stdio, Value,
    Write, R,
};

pub(crate) fn is_error(result: &Value) -> bool {
    result["isError"].as_bool() == Some(true)
}

pub(crate) fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Minimal blocking MCP client over the child's stdio.
pub(crate) struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    pub(crate) fn start(exe: &Path, vars: &[(String, String)]) -> R<Self> {
        let mut command = Command::new(exe);
        command
            .arg("--delegate")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (k, v) in vars {
            command.env(k, v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("spawn delegate mcp: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        let mut mcp = Self {
            child,
            stdin: Some(stdin),
            stdout,
            next: 0,
        };
        let init = mcp.request(
            "initialize",
            &json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
        )?;
        check(
            init["serverInfo"]["name"] == "harness-delegate",
            format!("initialize {init}"),
        )?;
        Ok(mcp)
    }

    pub(crate) fn request(&mut self, method: &str, params: &Value) -> R<Value> {
        self.next += 1;
        let id = self.next;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let stdin = self.stdin.as_mut().ok_or("stdin closed")?;
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| e.to_string())?;
        loop {
            let mut reply = String::new();
            let n = self
                .stdout
                .read_line(&mut reply)
                .map_err(|e| e.to_string())?;
            check(n > 0, "delegate mcp closed stdout")?;
            let value: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
            if value["id"] == id {
                return value
                    .get("result")
                    .cloned()
                    .ok_or_else(|| format!("{method} error: {value}"));
            }
        }
    }

    pub(crate) fn close(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

/// Shared handle so blocking MCP calls can run on the blocking pool between async hub checks.
#[derive(Clone)]
pub(crate) struct Client(pub(crate) std::sync::Arc<std::sync::Mutex<Mcp>>);

impl Client {
    pub(crate) async fn blocking(
        &self,
        f: impl FnOnce(&mut Mcp) -> R<Value> + Send + 'static,
    ) -> R<Value> {
        let inner = std::sync::Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || {
            let mut mcp = inner.lock().map_err(|_| "mcp lock poisoned")?;
            f(&mut mcp)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    pub(crate) async fn call(&self, name: &'static str, arguments: Value) -> R<Value> {
        self.blocking(move |m| {
            m.request("tools/call", &json!({"name": name, "arguments": arguments}))
        })
        .await
    }
}
