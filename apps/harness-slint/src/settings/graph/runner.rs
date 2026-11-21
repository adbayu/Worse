use super::{
    refresh, show_running, spawn, Arc, BufRead, BufReader, Command, Ctx, Mutex, Ordering, Stdio,
    Step, LOG_LINES,
};

/// Runs `steps` one after the other on a worker thread, showing the last lines of their output; stops at the
/// first one that fails.
pub(super) fn run(ctx: &Ctx, steps: Vec<Step>, done: &'static str, failed: &'static str) {
    if ctx.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    let ctx = ctx.clone();
    spawn(move || {
        show_running(&ctx, true, String::new());
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut note = done.to_owned();
        for step in steps {
            let outcome = step().and_then(|command| run_one(&ctx, command, &lines));
            if let Err(reason) = outcome {
                note = format!("{failed}: {reason}");
                break;
            }
        }
        let tail = lines
            .lock()
            .map(|all| all[all.len().saturating_sub(LOG_LINES)..].join("\n"))
            .unwrap_or_default();
        ctx.busy.store(false, Ordering::SeqCst);
        show_running(&ctx, false, tail);
        refresh(&ctx, Some(note));
    });
}

/// Runs one command to its end, feeding its output lines to the page. `Err`: why it did not succeed.
fn run_one(ctx: &Ctx, mut command: Command, lines: &Arc<Mutex<Vec<String>>>) -> Result<(), String> {
    let shown = format!(
        "> {} {}",
        command.get_program().to_string_lossy(),
        command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    );
    if let Ok(mut all) = lines.lock() {
        all.push(shown);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window for the child.
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let mut readers = Vec::new();
    let streams: [Option<Box<dyn std::io::Read + Send>>; 2] = [
        child.stdout.take().map(|s| Box::new(s) as _),
        child.stderr.take().map(|s| Box::new(s) as _),
    ];
    for stream in streams.into_iter().flatten() {
        let lines = Arc::clone(lines);
        let ctx = ctx.clone();
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                let line = line.trim_end().to_owned();
                if line.is_empty() {
                    continue;
                }
                let Ok(mut all) = lines.lock() else {
                    return;
                };
                all.push(line);
                let from = all.len().saturating_sub(LOG_LINES);
                let tail = all[from..].join("\n");
                drop(all);
                show_running(&ctx, true, tail);
            }
        }));
    }
    let status = child.wait();
    for reader in readers {
        let _ = reader.join();
    }
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(status.code().map_or_else(
            || "the command was stopped. Its output is below.".to_owned(),
            |code| format!("exit code {code}. The output is below."),
        )),
        Err(error) => Err(error.to_string()),
    }
}
