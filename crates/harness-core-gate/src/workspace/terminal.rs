use super::{
    check, next_event, step, write, Duration, Instant, Path, TerminalCreate, Workspace,
    WorkspaceEventKind, WorkspaceServices, EVENT_TIMEOUT, R,
};

pub(super) fn shell() -> (String, String, String) {
    if cfg!(windows) {
        (
            "cmd.exe".into(),
            "echo PTY_%OS%_OK\r\n".into(),
            "PTY_Windows_NT_OK".into(),
        )
    } else {
        (
            "/bin/sh".into(),
            "echo PTY_$((40+2))_OK\n".into(),
            "PTY_42_OK".into(),
        )
    }
}

pub(super) async fn terminal(ws: &Workspace) -> R<()> {
    let (shell, echo, marker) = shell();
    let newline = if cfg!(windows) { "\r\n" } else { "\n" };
    let mut events = ws.subscribe();
    let first = ws
        .terminal_create(TerminalCreate {
            cols: 100,
            rows: 30,
            cwd: None,
            shell: Some(shell.clone()),
        })
        .await
        .map_err(|e| format!("terminal_create: {e}"))?;
    check(
        ws.stats().pty_started && ws.stats().terminals == 1,
        "pty stats",
    )?;
    ws.terminal_write(first.id.clone(), echo)
        .await
        .map_err(|e| format!("terminal_write: {e}"))?;
    let mut output = String::new();
    let id = first.id.clone();
    next_event(&mut events, "echo marker", |k| match k {
        WorkspaceEventKind::TerminalOutput { id: from, data } if from == &id => {
            output.push_str(data);
            output.contains(&marker)
        }
        _ => false,
    })
    .await?;
    ws.terminal_resize(first.id.clone(), 120, 40)
        .await
        .map_err(|e| format!("terminal_resize: {e}"))?;
    check(
        ws.terminal_resize(first.id.clone(), 0, 10).await.is_err(),
        "zero-size resize accepted",
    )?;
    ws.terminal_write(first.id.clone(), format!("exit 7{newline}"))
        .await
        .map_err(|e| e.to_string())?;
    let exited = next_event(
        &mut events,
        "terminal exit",
        |k| matches!(k, WorkspaceEventKind::TerminalExited { id: from, .. } if from == &id),
    )
    .await?;
    check(
        matches!(exited, WorkspaceEventKind::TerminalExited { code: 7, .. }),
        format!("exit event: {exited:?}"),
    )?;
    check(
        ws.stats().terminals == 0,
        "exited terminal still registered",
    )?;
    terminal_close(ws, &mut events, shell).await?;
    step("terminal");
    Ok(())
}

/// Close of a live terminal: no events after close, child gone.
pub(super) async fn terminal_close(
    ws: &Workspace,
    events: &mut harness_core_api::workspace::WorkspaceEventStream,
    shell: String,
) -> R<()> {
    let second = ws
        .terminal_create(TerminalCreate {
            cols: 80,
            rows: 24,
            cwd: Some("src".into()),
            shell: Some(shell),
        })
        .await
        .map_err(|e| format!("terminal_create 2: {e}"))?;
    check(
        Path::new(&second.cwd).ends_with("src"),
        format!("terminal cwd: {}", second.cwd),
    )?;
    if let Some(pid) = second.pid {
        println!("AGENT_PID {pid}");
    }
    ws.terminal_close(second.id.clone())
        .await
        .map_err(|e| format!("terminal_close: {e}"))?;
    let closed_at = Instant::now();
    while let Some(event) = events.try_recv() {
        // Anything queued before the close returned is allowed; nothing may follow.
        let _ = event;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    while let Some(event) = events.try_recv() {
        check(
            !matches!(&event.kind, WorkspaceEventKind::TerminalOutput { id, .. } | WorkspaceEventKind::TerminalExited { id, .. } if id == &second.id),
            format!("event after close: {event:?}"),
        )?;
    }
    if let Some(pid) = second.pid {
        while crate::pid_alive(pid) {
            check(
                closed_at.elapsed() < Duration::from_secs(5),
                format!("terminal child {pid} survived close"),
            )?;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    check(
        ws.terminal_write(second.id.clone(), "x".into())
            .await
            .is_err(),
        "write to a closed terminal accepted",
    )?;
    check(
        ws.stats().terminals == 0,
        "closed terminal still registered",
    )?;
    Ok(())
}

pub(super) async fn watch(ws: &Workspace, root: &Path) -> R<()> {
    let mut events = ws.subscribe();
    let watch_id = ws
        .watch_start()
        .await
        .map_err(|e| format!("watch_start: {e}"))?;
    check(
        ws.stats().watcher_started && ws.stats().watches == 1,
        "watch stats",
    )?;
    // The OS watcher arms asynchronously; write until the change is seen (bounded by the event timeout).
    let target = root.join("docs").join("watched.txt");
    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut seen = false;
    let mut round = 0u32;
    while !seen && Instant::now() < deadline {
        round += 1;
        write(&target, &format!("change {round}\n"))?;
        let wait_until = Instant::now() + Duration::from_millis(700);
        while Instant::now() < wait_until {
            let left = wait_until.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, events.recv()).await {
                Ok(Some(event)) => {
                    if let WorkspaceEventKind::FilesChanged {
                        watch_id: from,
                        paths,
                        rescan,
                    } = &event.kind
                    {
                        if from == &watch_id
                            && (*rescan || paths.iter().any(|p| p == "docs/watched.txt"))
                        {
                            seen = true;
                            break;
                        }
                    }
                }
                Ok(None) => return Err("event stream closed during watch".into()),
                Err(_) => break,
            }
        }
    }
    check(seen, "no FilesChanged event for docs/watched.txt")?;
    ws.watch_stop(watch_id.clone())
        .await
        .map_err(|e| format!("watch_stop: {e}"))?;
    check(ws.stats().watches == 0, "watch still registered after stop")?;
    check(
        ws.watch_stop(watch_id).await.is_err(),
        "second watch_stop accepted",
    )?;
    std::fs::remove_file(&target).map_err(|e| e.to_string())?;
    step("watch");
    Ok(())
}
