use super::{
    shell, str_arg, truncate, Ctx, Duration, Outcome, Stdio, Value, COMMAND_DEFAULT_SECS,
    COMMAND_MAX_SECS, OUTPUT_MAX,
};

impl Ctx<'_> {
    pub(super) async fn run_command(&self, args: &Value) -> Outcome {
        let Some(command) = str_arg(args, "command") else {
            return Outcome::err("command is required");
        };
        let secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(COMMAND_DEFAULT_SECS)
            .clamp(1, COMMAND_MAX_SECS);
        if self.agent.async_tasks() && args.get("background").and_then(Value::as_bool) == Some(true)
        {
            return match crate::background::start_command(
                self.agent,
                self.session,
                self.cwd,
                command,
            ) {
                Ok(text) => Outcome::ok(text),
                Err(text) => Outcome::err(text),
            };
        }
        let mut process = shell(command);
        process
            .current_dir(self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = match process.spawn() {
            Ok(child) => child,
            Err(e) => return Outcome::err(format!("cannot start the shell: {e}")),
        };
        let output = tokio::select! {
            () = self.token.cancelled() => return Outcome::err("cancelled"),
            out = tokio::time::timeout(Duration::from_secs(secs), child.wait_with_output()) => out,
        };
        let output = match output {
            Err(_) => return Outcome::err(format!("timed out after {secs} s")),
            Ok(Err(e)) => return Outcome::err(e.to_string()),
            Ok(Ok(output)) => output,
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output
            .status
            .code()
            .map_or_else(|| "killed".to_owned(), |c| c.to_string());
        let mut text = format!("exit code: {code}\n");
        if !stdout.trim().is_empty() {
            text.push_str(stdout.trim_end());
            text.push('\n');
        }
        if !stderr.trim().is_empty() {
            text.push_str("stderr:\n");
            text.push_str(stderr.trim_end());
        }
        let text = truncate(&text, OUTPUT_MAX);
        if output.status.success() {
            Outcome::ok(text)
        } else {
            let mut outcome = Outcome::ok(text);
            outcome.ok = false;
            outcome
        }
    }
}
