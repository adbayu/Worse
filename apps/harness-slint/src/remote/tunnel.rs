//! Supervisor of the tunnel helper (docs/plans/android-apk-and-tunnel.md §3.1).
//!
//! One child process: `<tunnel_bin> http <port> [--name <subdomain>]`, stdout and stderr piped, no console
//! window. The public URL is the first `https://…` token in that output; when the helper prints none within
//! [`TunnelConfig::timeout`] the configured `<subdomain>.<tunnel_domain>` is used, else the state becomes an
//! error. Child death is always an error state, never silence. The user's real tunnel helper is never spawned by
//! tests: they generate a `.cmd` stub in a temp directory.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Lines of raw output kept for the "Raw output" expander.
const MAX_LOG: usize = 50;
/// Default wait for the first URL line.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);
/// `CREATE_NO_WINDOW` (winbase.h): the helper is a console program started from a GUI app.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What the tunnel is doing; drives the Settings status line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Off,
    Starting,
    Up {
        url: String,
    },
    Error {
        message: String,
    },
}

impl State {
    /// True while a child process is expected to be alive.
    #[cfg(test)]
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Starting | Self::Up { .. })
    }
}

/// One `start` request.
#[derive(Clone, Debug)]
pub struct TunnelConfig {
    /// Configured helper path (`RemoteSettings::tunnel_bin`).
    pub bin: String,
    /// Local port of the remote server.
    pub port: u16,
    /// Named tunnel subdomain; empty = quick tunnel (random host).
    pub subdomain: String,
    /// Zone used when the helper prints no URL at all.
    pub tunnel_domain: String,
    /// How long to wait for a URL before failing.
    pub timeout: Duration,
}

impl TunnelConfig {
    /// Config with the default 30 s URL timeout.
    #[must_use]
    pub fn new(bin: &str, port: u16, subdomain: &str, tunnel_domain: &str) -> Self {
        Self {
            bin: bin.to_owned(),
            port,
            subdomain: subdomain.trim().to_owned(),
            tunnel_domain: tunnel_domain.trim().to_owned(),
            timeout: START_TIMEOUT,
        }
    }

    /// `https://<subdomain>.<tunnel_domain>` when both are set.
    #[must_use]
    fn fallback_url(&self) -> Option<String> {
        let sub = self.subdomain.trim().trim_matches('.');
        let domain = self.tunnel_domain.trim().trim_matches('.');
        (!sub.is_empty() && !domain.is_empty()).then(|| format!("https://{sub}.{domain}"))
    }
}

#[derive(Default)]
struct Shared {
    state: State,
    log: VecDeque<String>,
    child: Option<Child>,
}

/// Handle of the tunnel supervisor; cheap to clone, all clones share one child process.
#[derive(Clone, Default)]
pub struct Tunnel {
    shared: Arc<Mutex<Shared>>,
    /// Set while `stop` runs, so the monitor does not report an intended exit as an error.
    stopping: Arc<AtomicBool>,
}

impl Tunnel {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current state (`Off` once stopped).
    #[must_use]
    pub fn state(&self) -> State {
        lock(&self.shared).state.clone()
    }

    /// Raw output, oldest first (at most [`MAX_LOG`] lines).
    #[must_use]
    pub fn log(&self) -> Vec<String> {
        lock(&self.shared).log.iter().cloned().collect()
    }

    /// Spawns the helper. `Err` when the binary cannot be started; the state is an error then too.
    ///
    /// # Errors
    ///
    /// The spawn error of the helper process.
    pub fn start(&self, cfg: &TunnelConfig) -> Result<(), String> {
        self.stop();
        let bin = resolve_bin(&cfg.bin);
        let mut command = command_for(&bin, cfg);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let message = format!("cannot start {}: {error}", bin.display());
                self.fail(message.clone());
                return Err(message);
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        {
            let mut shared = lock(&self.shared);
            shared.log.clear();
            shared.state = State::Starting;
            shared.child = Some(child);
        }
        self.stopping.store(false, Ordering::SeqCst);
        if let Some(pipe) = stdout {
            self.reader(pipe);
        }
        if let Some(pipe) = stderr {
            self.reader(pipe);
        }
        self.timeout_thread(cfg.timeout, cfg.fallback_url());
        self.monitor_thread();
        Ok(())
    }

    /// Kills the child tree and returns to `Off`.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let child = {
            let mut shared = lock(&self.shared);
            shared.state = State::Off;
            shared.child.take()
        };
        if let Some(mut child) = child {
            kill_tree(&mut child);
            let _ = child.wait();
        }
        self.stopping.store(false, Ordering::SeqCst);
    }

    fn fail(&self, message: String) {
        let mut shared = lock(&self.shared);
        shared.state = State::Error { message };
    }

    /// Drains one output pipe; a `https://…` token while still starting becomes the public URL.
    fn reader(&self, pipe: impl Read + Send + 'static) {
        let shared = Arc::clone(&self.shared);
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let mut inner = lock(&shared);
                if inner.log.len() >= MAX_LOG {
                    inner.log.pop_front();
                }
                inner.log.push_back(line.clone());
                if inner.state == State::Starting {
                    if let Some(url) = first_url(&line) {
                        inner.state = State::Up { url };
                    }
                }
            }
        });
    }

    /// Fails an unchanged `Starting` state after `timeout`, using the configured fallback URL when present.
    fn timeout_thread(&self, timeout: Duration, fallback: Option<String>) {
        let shared = Arc::clone(&self.shared);
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            let mut inner = lock(&shared);
            if inner.state != State::Starting {
                return;
            }
            inner.state = match fallback {
                Some(url) => State::Up { url },
                None => State::Error {
                    message: format!(
                        "no public URL from the tunnel within {} s",
                        timeout.as_secs()
                    ),
                },
            };
        });
    }

    /// Turns an unexpected child exit into an error state.
    fn monitor_thread(&self) {
        let shared = Arc::clone(&self.shared);
        let stopping = Arc::clone(&self.stopping);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(250));
            let mut inner = lock(&shared);
            let Some(child) = inner.child.as_mut() else {
                return;
            };
            let exited = match child.try_wait() {
                Ok(Some(status)) => Some(status.to_string()),
                Ok(None) => None,
                Err(error) => Some(error.to_string()),
            };
            let Some(status) = exited else {
                continue;
            };
            inner.child = None;
            if !stopping.load(Ordering::SeqCst) {
                inner.state = State::Error {
                    message: format!("the tunnel exited ({status})"),
                };
            }
            return;
        });
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            kill_tree(&mut child);
            let _ = child.wait();
        }
    }
}

fn lock(shared: &Arc<Mutex<Shared>>) -> std::sync::MutexGuard<'_, Shared> {
    match shared.lock() {
        Ok(guard) => guard,
        // A panicking reader thread must not disable the page; the state is still readable.
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Configured path → the file actually executed: on Windows `.exe` is appended when the path has no
/// extension and the extensionless path is not a file (`C:\tools\cftunnel` → `C:\tools\cftunnel.exe`).
#[must_use]
pub fn resolve_bin(bin: &str) -> PathBuf {
    let path = PathBuf::from(bin.trim());
    if !cfg!(windows) || path.extension().is_some() || path.is_file() {
        return path;
    }
    let mut name = path.into_os_string();
    name.push(".exe");
    PathBuf::from(name)
}

fn command_for(bin: &PathBuf, cfg: &TunnelConfig) -> Command {
    let mut args: Vec<String> = vec!["http".to_owned(), cfg.port.to_string()];
    if !cfg.subdomain.is_empty() {
        args.push("--name".to_owned());
        args.push(cfg.subdomain.clone());
    }
    // `.cmd` / `.bat` helpers (the test stub, user scripts) need a shell on Windows.
    let ext = bin
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if cfg!(windows) && (ext == "cmd" || ext == "bat") {
        let mut command = Command::new("cmd");
        command.arg("/c").arg(bin);
        command.args(&args);
        return command;
    }
    let mut command = Command::new(bin);
    command.args(&args);
    command
}

/// Kills the helper and every process it started (`taskkill /T /F` on Windows).
fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    let _ = child.kill();
}

/// First `https://…` token of a line, without trailing punctuation the helper may append.
#[must_use]
pub fn first_url(line: &str) -> Option<String> {
    let mut rest = line;
    while let Some(start) = rest.find("https://") {
        let tail = &rest[start..];
        let url: String = tail
            .chars()
            .take_while(|&c| {
                !c.is_whitespace()
                    && !matches!(
                        c,
                        '"' | '\'' | '<' | '>' | ')' | '(' | ']' | '[' | '}' | '{' | ',' | ';'
                    )
            })
            .collect();
        let url = url.trim_end_matches(['.', '!', '?', ':']).to_owned();
        let host = url.strip_prefix("https://").unwrap_or_default();
        if !host.is_empty() && host.contains('.') {
            return Some(url);
        }
        rest = &tail[1..];
    }
    None
}

#[cfg(test)]
mod tests;
