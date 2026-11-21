use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Instant;

use super::{binary, finished, report, Cmd, Kind, DRAIN_LIMIT, SCAN_LIMIT, STALL};

pub(super) fn run(home: &Path, rx: &mpsc::Receiver<Cmd>) {
    for cmd in rx {
        let (result, kind) = match cmd {
            Cmd::Download => (
                super::blocking_download(home).map(|_| Vec::new()),
                Kind::Tool,
            ),
            Cmd::Remove => (binary::remove(home).map(|_| Vec::new()), Kind::Tool),
            Cmd::Scan { root, offline } => (scan(home, &root, offline), Kind::Scan),
        };
        let _ = slint::invoke_from_event_loop(move || finished(result, &kind));
    }
}

fn scan(home: &Path, root: &Path, offline: bool) -> Result<Vec<report::Vuln>, String> {
    let program =
        binary::installed(home).ok_or_else(|| "the scanner is not installed".to_owned())?;
    let found = super::lockfiles::in_workspace(root);
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let mut command = Command::new(&program);
    command
        .arg("scan")
        .arg("--format")
        .arg("json")
        .arg("--verbosity")
        .arg("error");
    if offline {
        command.arg("--offline-vulnerabilities");
    }
    for path in &found {
        command.arg("-L").arg(path);
    }
    command
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let (status, stdout, stderr) = run_bounded(
        command
            .spawn()
            .map_err(|e| format!("cannot run the scanner: {e}"))?,
    )?;
    if !status.success() && serde_json::from_slice::<serde_json::Value>(&stdout).is_err() {
        let stderr = String::from_utf8_lossy(&stderr);
        let line = stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("the scanner reported a failure");
        return Err(format!("the scan failed: {}", line.trim()));
    }
    report::parse(&stdout)
}

/// Waits for the child while draining both pipes, and kills it once `SCAN_LIMIT` passed. The pipes are drained
/// on their own threads because a full pipe buffer would otherwise block the child forever.
fn run_bounded(
    mut child: std::process::Child,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), String> {
    fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        if let Some(mut pipe) = pipe {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = pipe.read_to_end(&mut buf);
                let _ = tx.send(buf);
            });
        }
        rx
    }
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("cannot run the scanner: {e}"))?
        {
            break status;
        }
        if started.elapsed() > SCAN_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the scan did not finish in time".to_owned());
        }
        std::thread::sleep(STALL);
    };
    // The child is gone, so its pipes close: wait for the readers instead of racing them. The cap only guards
    // against a grandchild that kept the pipe open.
    let collect = |rx: mpsc::Receiver<Vec<u8>>| rx.recv_timeout(DRAIN_LIMIT).unwrap_or_default();
    Ok((status, collect(out), collect(err)))
}
