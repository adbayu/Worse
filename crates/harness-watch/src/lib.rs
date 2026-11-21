//! Harness file watcher: a port of `ZCode`'s `FileWatcherService`
//! (`packages/services/src/fileWatcher/fileWatcherService.ts`) on top of
//! `notify` (`ReadDirectoryChangesW` on Windows, `FSEvents` on macOS,
//! inotify on Linux).
//!
//! ```text
//! notify thread(s) --raw event--> Subscription.pending (bounded set) --deadline--> scheduler thread
//!                                                                             |  debounce 150 ms / max-wait 1 s
//!                                                                             +--> sink(id, WatchSignal)
//! ```
//!
//! TS parity rules:
//! - `watch` returns string ids (`"0"`, `"1"`, ...); events are
//!   `{dirPath, changedPath?}` where `dirPath` is the path exactly as given and
//!   `changedPath` is present only when the flush window saw exactly one known
//!   path (Node `path.resolve(dirPath, fileName)`).
//! - trailing debounce plus an independent max-wait timer.
//! - a window that saw a create/remove/rename re-checks the directory identity
//!   (dev/ino on Unix, volume/file index on Windows) once at flush; a changed
//!   identity emits one final path-less event and closes the subscription.
//! - a watcher error emits one final `{dirPath}` and closes the subscription.
//!
//! Additions (opt-in or invisible on the TS wire): ignore rules, a bounded
//! pending set whose overflow, like a backend overflow, becomes a `rescan`
//! flag on a path-less event.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod filter;
mod identity;
mod scheduler;

#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use notify::event::{EventKind, ModifyKind};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;

pub use filter::IgnoreRules;
use identity::Identity;
use scheduler::SchedMsg;

mod dispatch;

use dispatch::{close_subscription, flush_if_due, on_raw_event};

/// TS `DEBOUNCE_MS`.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(150);
/// TS `MAX_WAIT_MS`.
pub const DEFAULT_MAX_WAIT: Duration = Duration::from_millis(1000);
/// Distinct paths kept per flush window before the window degrades to a rescan.
pub const DEFAULT_MAX_PENDING_PATHS: usize = 4096;

/// TS `FileWatchEvent` (`@zcode/shared`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileWatchEvent {
    /// The watched path, exactly as passed to `watch`.
    pub dir_path: String,
    /// Present only when the window saw exactly one known path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_path: Option<String>,
}

/// Why a subscription closed on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseReason {
    /// The watched directory was moved, deleted or replaced.
    IdentityChanged,
    /// The platform watcher reported an error (its text).
    Error(String),
}

impl CloseReason {
    /// Contract r2 wire text: `identity-changed` or `error: <text>`.
    #[must_use]
    pub fn wire(&self) -> String {
        match self {
            Self::IdentityChanged => "identity-changed".to_owned(),
            Self::Error(text) => format!("error: {text}"),
        }
    }
}

/// What a subscription's sink receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchSignal {
    /// One coalesced change window.
    Change {
        /// The TS event.
        event: FileWatchEvent,
        /// The window overflowed (backend overflow or too many paths): the
        /// consumer should re-read the directory instead of trusting paths.
        rescan: bool,
    },
    /// The subscription closed itself after its final `Change`. No further
    /// signals follow and the id is no longer known to the hub.
    Closed {
        /// Why.
        reason: CloseReason,
    },
}

/// Receives signals for one subscription. Called from the scheduler thread,
/// so it must not block (queue the signal and return).
pub type WatchSink = Arc<dyn Fn(&str, WatchSignal) + Send + Sync>;

/// Options of one `watch` call.
#[derive(Debug, Clone, Default)]
pub struct WatchOptions {
    /// Watch the whole tree (TS `recursive`, default `false`).
    pub recursive: bool,
    /// Paths to drop before coalescing. `None` reports everything (TS default).
    pub ignore: Option<IgnoreRules>,
}

/// Hub-wide tuning.
#[derive(Debug, Clone, Copy)]
pub struct HubConfig {
    /// Trailing debounce.
    pub debounce: Duration,
    /// Max wait since the first event of a window.
    pub max_wait: Duration,
    /// Bound on distinct paths per window.
    pub max_pending_paths: usize,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            debounce: DEFAULT_DEBOUNCE,
            max_wait: DEFAULT_MAX_WAIT,
            max_pending_paths: DEFAULT_MAX_PENDING_PATHS,
        }
    }
}

/// Errors from `watch`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WatchError {
    /// TS `throw new Error(...)` text.
    #[error("{0}")]
    App(String),
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // 与 TS 的批次标记一一对应。
struct WindowState {
    pending: BTreeSet<String>,
    unknown: bool,
    saw_rename: bool,
    overflow: bool,
    debounce_at: Option<Instant>,
    max_wait_at: Option<Instant>,
    closed: bool,
}

impl WindowState {
    fn deadline(&self) -> Option<Instant> {
        match (self.debounce_at, self.max_wait_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

struct Subscription {
    id: String,
    dir_path: String,
    roots: Vec<PathBuf>,
    /// Windows: the parent is watched too, to notice the root going away.
    parent_watched: bool,
    identity: Option<Identity>,
    ignore: Option<IgnoreRules>,
    state: Mutex<WindowState>,
    sink: WatchSink,
    watcher: Mutex<Option<RecommendedWatcher>>,
}

struct HubInner {
    config: HubConfig,
    subs: Mutex<HashMap<String, Arc<Subscription>>>,
    next_id: AtomicU64,
    sched: Mutex<mpsc::Sender<SchedMsg>>,
}

/// Owns every subscription and the scheduler thread.
pub struct WatchHub {
    inner: Arc<HubInner>,
}

impl Default for WatchHub {
    fn default() -> Self {
        Self::new(HubConfig::default())
    }
}

impl Drop for WatchHub {
    fn drop(&mut self) {
        self.dispose_all();
        let _ = lock(&self.inner.sched).send(SchedMsg::Stop);
    }
}

impl WatchHub {
    /// Creates a hub and starts its scheduler thread.
    #[must_use]
    pub fn new(config: HubConfig) -> Self {
        let (tx, rx) = mpsc::channel();
        let inner = Arc::new(HubInner {
            config,
            subs: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            sched: Mutex::new(tx),
        });
        scheduler::spawn(Arc::downgrade(&inner), rx);
        Self { inner }
    }

    /// Starts watching `path`. Fails like TS when the path cannot be stat'ed
    /// or the platform watcher cannot be created.
    ///
    /// # Errors
    /// `WatchError::App("无法监视目录 '<path>': <reason>")`.
    pub fn watch(
        &self,
        path: &str,
        options: WatchOptions,
        sink: WatchSink,
    ) -> Result<String, WatchError> {
        let fail = |message: String| WatchError::App(format!("无法监视目录 '{path}': {message}"));
        let id = self
            .inner
            .next_id
            .fetch_add(1, Ordering::Relaxed)
            .to_string();
        harness_fs::stat(path).map_err(|e| fail(e.to_string()))?;
        let identity = Identity::of(Path::new(path));
        let mut roots = vec![PathBuf::from(path)];
        if let Ok(real) = std::fs::canonicalize(path) {
            let real = PathBuf::from(harness_fs::strip_verbatim(&real.to_string_lossy()).as_ref());
            if !roots.contains(&real) {
                roots.push(real);
            }
        }
        let sub = Arc::new(Subscription {
            id: id.clone(),
            dir_path: path.to_owned(),
            roots,
            parent_watched: cfg!(windows),
            identity,
            ignore: options.ignore,
            state: Mutex::new(WindowState::default()),
            sink,
            watcher: Mutex::new(None),
        });
        let weak = Arc::downgrade(&sub);
        let hub = Arc::downgrade(&self.inner);
        let mut watcher = notify::recommended_watcher(move |result| {
            if let (Some(sub), Some(hub)) = (weak.upgrade(), hub.upgrade()) {
                on_raw_event(&hub, &sub, result);
            }
        })
        .map_err(|e| fail(e.to_string()))?;
        let mode = if options.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        watcher.watch(Path::new(path), mode).map_err(|e| {
            // 与 Node 一致：fs.watch 失败的消息形如 `ENOENT: no such file or directory, watch '<path>'`。
            let message = match &e.kind {
                notify::ErrorKind::Io(io) => {
                    harness_fs::FsError::from_io(io, "watch", Path::new(path)).to_string()
                }
                _ => e.to_string(),
            };
            fail(message)
        })?;
        if cfg!(windows) {
            // notify 在 Windows 上发现被监视目录被删除（ERROR_ACCESS_DENIED 且目录不存在）时会静默
            // 取消监视、不报任何事件；Node/libuv 则报错，TS 据此发最终事件并关闭。这里额外非递归监视
            // 父目录，看到根目录被删除/改名时触发身份校验，从而同样发出最终事件并关闭订阅。
            if let Some(parent) = Path::new(path)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
            {
                if let Err(error) = watcher.watch(parent, RecursiveMode::NonRecursive) {
                    tracing::debug!(%error, path, "parent watch unavailable");
                }
            }
        }
        *lock(&sub.watcher) = Some(watcher);
        lock(&self.inner.subs).insert(id.clone(), sub);
        tracing::debug!(id, path, recursive = options.recursive, "watch started");
        Ok(id)
    }

    /// Stops a subscription. Unknown ids are a no-op (TS `cleanup`). Returns
    /// whether the id was active.
    #[allow(clippy::must_use_candidate)] // 调用方通常不关心返回值（TS 的 unwatch 无返回值）。
    pub fn unwatch(&self, id: &str) -> bool {
        let removed = lock(&self.inner.subs).remove(id);
        removed.is_some_and(|sub| {
            close_subscription(&sub);
            true
        })
    }

    /// Stops every subscription (TS `disposeAll`).
    pub fn dispose_all(&self) {
        let subs: Vec<_> = lock(&self.inner.subs).drain().map(|(_, s)| s).collect();
        for sub in subs {
            close_subscription(&sub);
        }
    }

    /// Whether `id` is active.
    #[must_use]
    pub fn is_active(&self, id: &str) -> bool {
        lock(&self.inner.subs).contains_key(id)
    }

    /// Number of active subscriptions.
    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.inner.subs).len()
    }

    /// Whether no subscription is active.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
