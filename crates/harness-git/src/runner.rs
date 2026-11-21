//! git CLI execution: binary discovery, environment, timeouts, output caps and
//! cancellation. Mirrors `providers/gitEnvironmentProvider.ts`,
//! `providers/gitCommandProvider.ts` and `config.ts`.
//!
//! Commands are always spawned with an argv vector (never through a shell).

use std::io::Read;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::GitError;
use crate::util::js_trim;

mod binary;
mod reader;

use binary::{base_command, candidates, probe};
use reader::{kill_tree, submit_reader, Pipe, ReaderEvent};

/// `DEFAULT_GIT_DISCOVERY_TIMEOUT_MS`.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
/// `DEFAULT_GIT_COMMAND_TIMEOUT_MS`.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
/// `DEFAULT_GIT_DIFF_TIMEOUT_MS`.
pub const DIFF_TIMEOUT: Duration = Duration::from_secs(20);
/// `DEFAULT_GIT_OUTPUT_BYTES`.
pub const OUTPUT_BYTES: usize = 512 * 1024;
/// `DEFAULT_GIT_DIFF_BYTES`.
pub const DIFF_BYTES: usize = 1024 * 1024;

/// How often a running command re-checks the cancel callback. Completion is
/// event-driven and never waits for this interval.
const CANCEL_POLL: Duration = Duration::from_millis(25);

/// Variables a parent git (for example a pre-push hook) may inject; they are
/// removed so commands depend only on the explicit cwd.
const GIT_LOCAL_ENV_VARS: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_DIR",
    "GIT_GRAFT_FILE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_INTERNAL_SUPER_PREFIX",
    "GIT_NAMESPACE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_WORK_TREE",
];

const FORCED_ENV: &[(&str, &str)] = &[
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_PAGER", "cat"),
    ("PAGER", "cat"),
    ("TERM", "dumb"),
    ("LC_ALL", "C"),
    ("LANG", "C"),
];

/// Cooperative cancellation check supplied by the caller.
pub type CancelFn<'a> = &'a (dyn Fn() -> bool + Sync);

/// Never cancelled.
#[must_use]
pub fn never_cancelled() -> bool {
    false
}

/// Outcome of one git invocation (`GitCommandExecutionResult`).
#[derive(Debug, Clone, Default)]
pub struct ExecResult {
    /// Lossy UTF-8 stdout.
    pub stdout: String,
    /// Lossy UTF-8 stderr (or the spawn error).
    pub stderr: String,
    /// Exit code; `-2` when spawning failed, `None` when killed.
    pub exit_code: Option<i32>,
    /// Killed because of the timeout.
    pub timed_out: bool,
    /// Killed because an output stream exceeded the cap.
    pub output_truncated: bool,
    /// Wall time.
    pub duration: Duration,
    /// Timeout that applied.
    pub timeout: Duration,
}

impl ExecResult {
    /// `toResultMessage`.
    #[must_use]
    pub fn message(&self) -> String {
        let stderr = js_trim(&self.stderr);
        if !stderr.is_empty() {
            return stderr.to_owned();
        }
        let stdout = js_trim(&self.stdout);
        if !stdout.is_empty() {
            return stdout.to_owned();
        }
        match self.exit_code {
            Some(code) => format!("exitCode={code}"),
            None => "exitCode=null".to_owned(),
        }
    }

    /// `ensureGitCommandSucceeded`.
    ///
    /// # Errors
    /// `GitError::App` with the TS message when the command timed out, was
    /// truncated or exited with a code outside `allowed`.
    pub fn ensure(&self, label: &str, allowed: &[i32]) -> Result<&Self, GitError> {
        if self.timed_out {
            return Err(GitError::App(format!(
                "{label} timed out after {}ms (elapsed={}ms)",
                self.timeout.as_millis(),
                self.duration.as_millis()
            )));
        }
        if self.output_truncated {
            return Err(GitError::App(format!("{label} output exceeded limit")));
        }
        if self.exit_code.is_some_and(|code| allowed.contains(&code)) {
            return Ok(self);
        }
        Err(GitError::App(format!("{label} failed: {}", self.message())))
    }

    /// `ensure` with `[0]`.
    ///
    /// # Errors
    /// See [`ExecResult::ensure`].
    pub fn ensure_ok(&self, label: &str) -> Result<&Self, GitError> {
        self.ensure(label, &[0])
    }
}

/// Fixed binary choice for tests (`None` = git missing).
type BinaryOverride = Option<String>;

/// Process-wide git runner with a cached binary probe.
#[derive(Debug, Default)]
pub struct GitRunner {
    binary: OnceLock<Option<String>>,
    binary_override: Option<BinaryOverride>,
}

impl GitRunner {
    /// Runner using the `ZCODE_GIT_BINARY` / `git` / well-known candidates.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runner with a fixed binary (`None` simulates a missing git).
    #[must_use]
    pub fn with_binary(binary: Option<String>) -> Self {
        Self {
            binary: OnceLock::new(),
            binary_override: Some(binary),
        }
    }

    /// `resolveGitBinary` (probed once per process, like the TS cache).
    pub fn binary(&self) -> Option<&str> {
        self.binary
            .get_or_init(|| {
                if let Some(fixed) = &self.binary_override {
                    return fixed.clone();
                }
                let found = candidates().into_iter().find(|c| probe(c));
                tracing::info!(binary = ?found, "git binary resolved");
                found
            })
            .as_deref()
    }

    /// Runs `git <args>` in `cwd`.
    ///
    /// # Errors
    /// `GitError::App("Git binary is not available")` without a binary, and
    /// `GitError::Cancelled` when `cancel` fires (the child is killed).
    pub fn run(
        &self,
        cwd: &str,
        args: &[&str],
        timeout: Duration,
        max_output: usize,
        cancel: CancelFn<'_>,
    ) -> Result<ExecResult, GitError> {
        let Some(binary) = self.binary() else {
            return Err(GitError::App("Git binary is not available".to_owned()));
        };
        tracing::debug!(cwd, ?args, "git");
        exec(binary, Some(cwd), args, timeout, max_output, cancel)
    }
}

/// Spawns `binary args` (argv vector, never a shell) and collects its output.
///
/// 修复说明（性能）：旧实现用 `try_wait` + 1–10ms `thread::sleep` 轮询子进程退出。Windows
/// 默认计时器精度约 15.6ms，`sleep(1ms)` 实际常睡满一个时间片，每个 git 进程平均多出约 5.5ms
/// （fixes/ts.md 实测 31.2 vs 25.7ms）。现在完成路径是事件驱动的：读取任务在管道 EOF 时经
/// channel 立即通知，主线程阻塞在 `recv_timeout` 上（超时/取消检查不再给正常完成增加延迟），
/// 随后用阻塞 `wait()` 取退出码；读取任务跑在进程级复用线程池上，不再每条命令新建两个线程。
fn exec(
    binary: &str,
    cwd: Option<&str>,
    args: &[&str],
    timeout: Duration,
    max_output: usize,
    cancel: CancelFn<'_>,
) -> Result<ExecResult, GitError> {
    // 已取消的请求不再启动新进程（旧实现靠首轮轮询发现取消，事件驱动后在启动前检查）。
    if cancel() {
        return Err(GitError::Cancelled);
    }
    let started = Instant::now();
    let mut command = base_command(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Ok(spawn_failure(binary, &error, started, timeout)),
    };
    let (tx, rx) = mpsc::channel::<ReaderEvent>();
    let mut pending = 0usize;
    if let Some(stream) = child.stdout.take() {
        submit_reader(Pipe::Stdout(stream), max_output, tx.clone());
        pending += 1;
    }
    if let Some(stream) = child.stderr.take() {
        submit_reader(Pipe::Stderr(stream), max_output, tx.clone());
        pending += 1;
    }
    drop(tx);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut timed_out = false;
    let mut cancelled = false;
    let mut output_truncated = false;
    let mut killed = false;
    while pending > 0 {
        let event = if killed {
            // 已终止：只等管道排空（与旧实现 join 读取线程的语义一致）。
            rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        } else {
            let remaining = timeout.saturating_sub(started.elapsed());
            rx.recv_timeout(remaining.min(CANCEL_POLL))
        };
        match event {
            Ok(ReaderEvent::Done(is_stdout, buf)) => {
                if is_stdout {
                    stdout = buf;
                } else {
                    stderr = buf;
                }
                pending -= 1;
            }
            Ok(ReaderEvent::Truncated) => {
                output_truncated = true;
                if !killed {
                    kill_tree(&mut child);
                    killed = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if cancel() {
                    cancelled = true;
                } else if started.elapsed() >= timeout {
                    timed_out = true;
                }
                if cancelled || timed_out {
                    kill_tree(&mut child);
                    killed = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // 两条管道都已 EOF：子进程已退出或正在退出，阻塞等待即可，无需轮询。
    let exit_code = match child.wait() {
        Ok(status) => status.code(),
        Err(error) => {
            tracing::warn!(%error, "git wait failed");
            None
        }
    };
    if cancelled {
        return Err(GitError::Cancelled);
    }
    Ok(ExecResult {
        stdout: into_string_lossy(stdout),
        stderr: into_string_lossy(stderr),
        exit_code: if timed_out || output_truncated {
            None
        } else {
            exit_code
        },
        timed_out,
        output_truncated,
        duration: started.elapsed(),
        timeout,
    })
}

/// Result for a child that could not be spawned (`exitCode = -2`).
fn spawn_failure(
    binary: &str,
    error: &std::io::Error,
    started: Instant,
    timeout: Duration,
) -> ExecResult {
    // Windows 上 cwd 不存在时 CreateProcess 返回 ERROR_DIRECTORY(267)；
    // Node/libuv 报告为 `spawn <bin> ENOENT`，TS 据此按“非仓库”降级，这里保持同一文案。
    let missing = error.kind() == std::io::ErrorKind::NotFound
        || (cfg!(windows) && error.raw_os_error() == Some(267));
    let stderr = if missing {
        format!("spawn {binary} ENOENT")
    } else {
        error.to_string()
    };
    ExecResult {
        stderr,
        exit_code: Some(-2),
        duration: started.elapsed(),
        timeout,
        ..ExecResult::default()
    }
}

/// Lossy UTF-8 without copying output that is already valid.
fn into_string_lossy(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}
