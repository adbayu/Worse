//! Workspace services for one workspace root (spec `docs/specs/harness/phase-8-core.md` §15.3).
//!
//! [`Workspace`] implements [`WorkspaceServices`]. It is the single owner of its terminals (`PtyManager`),
//! watches (`WatchHub`), search index cache and running searches. Every heavy part starts on first use:
//! `Workspace::open` only canonicalizes the root.

mod file_search;
mod git;
mod git_ops;
mod paths;

pub use git_ops::{LOG_DEPTH_MAX, LOG_PAGE_MAX};

pub use file_search::WorkspaceFileSearch;

/// Content-based file type checks for the workspace (`docs/specs/harness/file-safety.md`).
pub use harness_fs::filetype;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use harness_core_api::diff::FileDiff;
use harness_core_api::workspace::{
    BranchInfo, CommitResult, FileContent, FileHit, GitStatus, SearchText, TerminalCreate,
    TerminalInfo, TextMatch, TextSearchResult, WorkspaceEventBus, WorkspaceEventKind,
    WorkspaceEventStream, WorkspaceServices, WorkspaceStats,
};
use harness_core_api::{ApiError, ApiResult, BoxFuture};
use harness_pty::{CreateOptions, PtyConfig, PtyEvent, PtyManager, PtySink};
use harness_search::{CancellationToken, EntryType, IndexCache, SearchError, TextSearchOptions};
use harness_watch::{IgnoreRules, WatchOptions, WatchSignal, WatchSink};
use tokio::io::AsyncReadExt;

use crate::paths::Root;

mod ops;
mod services;

use services::{io_error, search_error, text_match};

/// Default `read_file` limit.
const DEFAULT_READ_BYTES: u64 = 1024 * 1024;
/// Hard `read_file` limit.
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;
/// Default `search_text` result cap.
const DEFAULT_MAX_RESULTS: u32 = 2000;

/// Tuning of a workspace.
#[derive(Debug, Clone)]
pub struct WorkspaceConfig {
    /// Watcher debounce (spec: 150 ms).
    pub watch_debounce: Duration,
    /// Watcher max wait (spec: 1 s).
    pub watch_max_wait: Duration,
    /// Environment for terminals; `None` = the process environment.
    pub terminal_env: Option<Vec<(String, String)>>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            watch_debounce: Duration::from_millis(150),
            watch_max_wait: Duration::from_secs(1),
            terminal_env: None,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Inner {
    root: Root,
    config: WorkspaceConfig,
    bus: WorkspaceEventBus,
    pty: OnceLock<PtyManager>,
    watch: OnceLock<harness_watch::WatchHub>,
    index: OnceLock<IndexCache>,
    index_built: AtomicBool,
    /// Live terminal ids; output of an id not in here is dropped (no events after `terminal_close`).
    terminals: Mutex<HashSet<String>>,
    watches: Mutex<HashSet<String>>,
    /// Running searches: id → (generation, token). The generation tells a finished search whether the entry
    /// is still its own (a new search with the same id replaces and cancels the old one).
    searches: Mutex<HashMap<String, (u64, CancellationToken)>>,
    search_generation: AtomicU64,
    shut_down: AtomicBool,
    git: harness_git::GitService,
}

/// Services of one workspace root. Cheap to clone (shared state).
#[derive(Clone)]
pub struct Workspace {
    inner: Arc<Inner>,
}

impl Workspace {
    /// Opens `root` (must be an existing directory). No thread, process or index is started.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` when the root is missing or not a directory.
    pub async fn open(root: PathBuf, config: WorkspaceConfig) -> ApiResult<Self> {
        let root = tokio::task::spawn_blocking(move || Root::open(&root))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))??;
        Ok(Self {
            inner: Arc::new(Inner {
                root,
                config,
                bus: WorkspaceEventBus::new(),
                pty: OnceLock::new(),
                watch: OnceLock::new(),
                index: OnceLock::new(),
                index_built: AtomicBool::new(false),
                terminals: Mutex::new(HashSet::new()),
                watches: Mutex::new(HashSet::new()),
                searches: Mutex::new(HashMap::new()),
                search_generation: AtomicU64::new(0),
                shut_down: AtomicBool::new(false),
                git: harness_git::GitService::new(),
            }),
        })
    }

    fn check_open(&self) -> ApiResult<()> {
        if self.inner.shut_down.load(Ordering::SeqCst) {
            Err(ApiError::ShutDown)
        } else {
            Ok(())
        }
    }

    fn root_str(&self) -> String {
        self.inner.root.as_str().to_owned()
    }

    /// Branch switcher data: current branch (or detached), upstream with ahead / behind, local branches
    /// (phase-9-parity.md §9.2.5 R19).
    ///
    /// # Errors
    ///
    /// `InvalidArgument` outside a repository; git failures.
    pub async fn git_branch_list(&self) -> ApiResult<harness_core_api::workspace::GitBranches> {
        self.check_open()?;
        git_ops::branch_list(self.inner.root.as_str()).await
    }

    /// `git switch [-c] branch`. Local changes that would be overwritten give `SwitchOutcome::Blocked` and
    /// change nothing.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a bad name; other git failures (unknown branch, existing name on create).
    pub async fn git_switch(
        &self,
        branch: String,
        create: bool,
    ) -> ApiResult<harness_core_api::workspace::SwitchOutcome> {
        self.check_open()?;
        git_ops::switch(self.inner.root.as_str(), &branch, create).await
    }

    /// Commit; `include_unstaged` first stages every change of the workspace folder (`git add -A -- .`).
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for an empty message or nothing to commit; git failures (hooks).
    pub async fn git_commit_with(
        &self,
        message: String,
        include_unstaged: bool,
    ) -> ApiResult<CommitResult> {
        self.check_open()?;
        git_ops::commit_with(self.inner.root.as_str(), &message, include_unstaged).await
    }

    /// `git push` of `branch` (default: current) to `remote` (default: the upstream's, else `origin`, else the
    /// first remote); sets the upstream when the branch has none. `progress` gets Started, then Finished or Failed.
    ///
    /// # Errors
    ///
    /// `Conflict` when detached or without remote; git failures (rejected, auth: no prompt is shown).
    pub async fn git_push(
        &self,
        remote: Option<String>,
        branch: Option<String>,
        progress: &(dyn Fn(harness_core_api::workspace::PushProgress) + Send + Sync),
    ) -> ApiResult<harness_core_api::workspace::PushResult> {
        self.check_open()?;
        git_ops::push(self.inner.root.as_str(), remote, branch, progress).await
    }

    /// One page of the commit graph (all branches, topological order, lanes from the newest commit).
    ///
    /// # Errors
    ///
    /// Git failures (an empty repository is an empty page).
    pub async fn git_log(
        &self,
        skip: u32,
        limit: u32,
    ) -> ApiResult<harness_core_api::workspace::GitLogPage> {
        self.check_open()?;
        git_ops::log(self.inner.root.as_str(), skip, limit).await
    }

    /// Runs blocking work off the async runtime.
    async fn blocking<T, F>(&self, f: F) -> ApiResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Inner) -> ApiResult<T> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || f(&inner))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
    }

    async fn index(&self, refresh: bool) -> ApiResult<Arc<harness_search::WorkspaceIndex>> {
        self.blocking(move |inner| {
            let cache = inner.index.get_or_init(IndexCache::new);
            let index = cache
                .get(
                    inner.root.as_str(),
                    None,
                    refresh,
                    &CancellationToken::new(),
                )
                .map_err(search_error)?;
            inner.index_built.store(true, Ordering::SeqCst);
            Ok(index)
        })
        .await
    }

    fn pty(&self) -> &PtyManager {
        self.inner
            .pty
            .get_or_init(|| PtyManager::new(PtyConfig::default()))
    }

    fn watch_hub(&self) -> &harness_watch::WatchHub {
        self.inner.watch.get_or_init(|| {
            harness_watch::WatchHub::new(harness_watch::HubConfig {
                debounce: self.inner.config.watch_debounce,
                max_wait: self.inner.config.watch_max_wait,
                ..harness_watch::HubConfig::default()
            })
        })
    }
}
