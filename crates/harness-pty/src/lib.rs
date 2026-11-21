//! Harness terminal service: a port of `ZCode`'s `TerminalService`
//! (`packages/services/src/terminal/terminalService.ts`) on `portable-pty`
//! (`ConPTY` on Windows, a Unix PTY elsewhere). Wire semantics follow
//! `Worse/.work/phase3/contract.md` r2 §9.3/§9.4.
//!
//! ```text
//! create ─ spawn shell ─ contain tree (Job Object / process group)
//!          ├─ reader thread: read ≤64 KiB → streaming UTF-8 decode → emit Data
//!          └─ waiter thread: wait() → close PTY → drain until EOF or 200 ms idle → emit Exit → forget
//! write / resize / kill: any thread; kill silences the session first (no Exit after kill)
//! ```
//!
//! Every `Data` of a session is emitted before its `Exit`; both go through the
//! session's output lock, so the sink sees them in order.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod decode;
mod kill;
mod shell;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

pub use decode::Utf8Stream;
pub use shell::{
    default_env, is_executable, resolve_cwd, resolve_env, resolve_shell, windows_pty_info, EnvMap,
    WindowsPtyInfo,
};

use kill::TreeGuard;

mod manager;
mod reader;

use reader::{exit_code, finish_session, read_loop, ReaderSignal};

/// TS `TERMINAL_UNSUBSCRIBED_OUTPUT_MAX_CHARS`.
pub const UNATTACHED_BUFFER_MAX_UNITS: usize = 1_048_576;
/// node-pty's text for non-positive sizes.
pub const BAD_SIZE_MESSAGE: &str = "resizing must be done using positive cols and rows";

/// Tuning for a [`PtyManager`].
#[derive(Debug, Clone, Copy)]
pub struct PtyConfig {
    /// Bound for output buffered while detached (UTF-16 units, like TS).
    pub unattached_buffer_units: usize,
    /// Unacknowledged UTF-16 units at which a flow-controlled reader pauses.
    pub flow_high_water: usize,
    /// Read buffer size (contract: up to 64 KiB per `$/pty/output`).
    pub read_chunk: usize,
    /// After the shell exits, output still flowing is forwarded until EOF or
    /// this long without data (contract: 200 ms).
    pub exit_idle: Duration,
    /// Unix: `SIGHUP` → `SIGKILL` grace on kill (contract: 2 s).
    pub kill_grace: Duration,
}

impl Default for PtyConfig {
    fn default() -> Self {
        Self {
            unattached_buffer_units: UNATTACHED_BUFFER_MAX_UNITS,
            flow_high_water: 4 * 1024 * 1024,
            read_chunk: 64 * 1024,
            exit_idle: Duration::from_millis(200),
            kill_grace: Duration::from_secs(2),
        }
    }
}

/// Options of [`PtyManager::create`].
#[derive(Debug, Clone, Default)]
pub struct CreateOptions {
    /// Columns; must be positive.
    pub cols: u16,
    /// Rows; must be positive.
    pub rows: u16,
    /// Working directory, used verbatim; `None` = home / OS home / `/`.
    pub cwd: Option<String>,
    /// Shell; `None` = the TS default resolution.
    pub shell: Option<String>,
    /// Shell arguments (TS passes none).
    pub args: Vec<String>,
    /// Complete environment (replaces the process env); `None` = [`default_env`].
    pub env: Option<EnvMap>,
    /// Unix `TERM` name (node-pty `name`); ignored on Windows.
    pub name: Option<String>,
    /// Stream immediately (`true`, the harness-host mode) or buffer until
    /// [`PtyManager::attach`] like the TS emitter.
    pub attached: bool,
    /// Enable ack-based flow control ([`PtyManager::ack`]).
    pub flow_control: bool,
}

/// Result of [`PtyManager::create`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    /// Session id.
    pub id: String,
    /// Shell as used.
    pub shell: String,
    /// Working directory as used.
    pub cwd: String,
    /// Shell process id.
    pub pid: Option<u32>,
    /// TS `windowsPty`.
    pub windows_pty: Option<WindowsPtyInfo>,
}

/// Events delivered to a session's sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    /// Decoded output (never empty).
    Data(String),
    /// The shell exited with this code. Last event of the session.
    Exit(i64),
}

/// Receives events of one session. Called with the session's output lock
/// held, so it must not block or call back into the manager.
pub type PtySink = Arc<dyn Fn(&str, PtyEvent) + Send + Sync>;

/// Errors with TS message texts.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PtyError {
    /// `Terminal not found: <id>`.
    #[error("Terminal not found: {0}")]
    NotFound(String),
    /// Any other TS `Error` text.
    #[error("{0}")]
    App(String),
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

#[derive(Default)]
struct OutState {
    attached: bool,
    pending: VecDeque<PtyEvent>,
    pending_units: usize,
    flow_control: bool,
    unacked: usize,
    silenced: bool,
}

struct Session {
    id: String,
    sink: PtySink,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    tree: Mutex<TreeGuard>,
    out: Mutex<OutState>,
    flow: Condvar,
    exited: Mutex<bool>,
    exited_cv: Condvar,
}

impl Session {
    /// Emits or buffers one event, in order, under the output lock.
    fn emit(&self, config: &PtyConfig, event: PtyEvent) {
        let mut out = lock(&self.out);
        if out.silenced {
            return;
        }
        if out.attached {
            if let PtyEvent::Data(text) = &event {
                if out.flow_control {
                    out.unacked += utf16_len(text);
                }
            }
            (self.sink)(&self.id, event);
            return;
        }
        if let PtyEvent::Data(text) = &event {
            out.pending_units += utf16_len(text);
        }
        out.pending.push_back(event);
        // 与 TS 一致：无订阅者时有界缓存，超限丢弃最旧的输出块（保留至少一块）。
        while out.pending_units > config.unattached_buffer_units && out.pending.len() > 1 {
            match out.pending.pop_front() {
                Some(PtyEvent::Data(text)) => out.pending_units -= utf16_len(&text),
                Some(exit @ PtyEvent::Exit(_)) => {
                    out.pending.push_front(exit);
                    break;
                }
                None => break,
            }
        }
    }

    /// Blocks the reader while flow control says the consumer is behind.
    fn wait_for_credit(&self, config: &PtyConfig) {
        let mut out = lock(&self.out);
        while out.flow_control
            && out.attached
            && !out.silenced
            && out.unacked >= config.flow_high_water
        {
            out = self.flow.wait(out).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn silence(&self) {
        let mut out = lock(&self.out);
        out.silenced = true;
        out.pending.clear();
        self.flow.notify_all();
    }

    fn close_pty(&self) {
        let writer = lock(&self.writer).take();
        let master = lock(&self.master).take();
        drop(writer);
        drop(master);
    }

    fn mark_exited(&self) {
        *lock(&self.exited) = true;
        self.exited_cv.notify_all();
    }

    /// Waits until the shell was reaped or `deadline` passes.
    fn wait_exited(&self, deadline: Instant) -> bool {
        let mut exited = lock(&self.exited);
        while !*exited {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            exited = self
                .exited_cv
                .wait_timeout(exited, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// First kill stage: silence, hang up the tree, close the PTY.
    fn begin_kill(&self) {
        self.silence();
        lock(&self.tree).hang_up();
        if let Err(error) = lock(&self.killer).kill() {
            tracing::debug!(%error, id = self.id, "pty child kill");
        }
        self.close_pty();
    }

    /// Second stage: reap, escalating after the grace period.
    fn finish_kill(&self, deadline: Instant) {
        if !self.wait_exited(deadline) {
            tracing::warn!(id = self.id, "shell ignored hang-up; force kill");
            lock(&self.tree).force();
            self.wait_exited(Instant::now() + Duration::from_secs(1));
        }
        lock(&self.tree).force();
    }
}

struct ManagerInner {
    config: PtyConfig,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    next_id: AtomicU64,
}

/// Owns every terminal session.
pub struct PtyManager {
    inner: Arc<ManagerInner>,
}

impl Default for PtyManager {
    fn default() -> Self {
        Self::new(PtyConfig::default())
    }
}

impl Drop for PtyManager {
    fn drop(&mut self) {
        self.dispose_all();
    }
}

impl PtyManager {}

/// Where a sideloaded `conpty.dll` would come from: next to the executable,
/// else the first `PATH` entry holding one (`None` = the OS `ConPTY`).
///
/// portable-pty loads `conpty.dll` by bare name and prefers it over the inbox
/// `ConPTY`; the Windows 10 inbox conhost paces VT output in ~16 ms frames,
/// which is visible as typing latency. Diagnostic only: it mirrors the two
/// search locations the bridge controls, not every `LoadLibrary` rule.
#[must_use]
pub fn conpty_dll() -> Option<std::path::PathBuf> {
    if cfg!(not(windows)) {
        return None;
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("conpty.dll")));
    let on_path = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|dir| dir.join("conpty.dll"));
    beside.into_iter().chain(on_path).find(|dll| dll.is_file())
}
