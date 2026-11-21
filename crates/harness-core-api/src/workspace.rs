//! Workspace services: files, search, git, terminals and file watching for one workspace root
//! (spec phase-8-core.md §15.3). Implemented by `harness_workspace::Workspace`.
//!
//! Everything is lazy: a terminal manager, watcher or search index exists only after its first call.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::diff::FileDiff;
use crate::{ApiResult, BoxFuture};

mod bus;

pub use bus::{WorkspaceEventBus, WorkspaceEventStream};

/// `read_file` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    /// Workspace-relative path with `/`.
    pub path: String,
    /// UTF-8 text (lossy); empty for binary files.
    pub text: String,
    /// File size in bytes.
    pub size: u64,
    /// `text` stops at `max_bytes`.
    pub truncated: bool,
    /// The file looks binary (NUL bytes / known binary type).
    pub binary: bool,
}

/// One file-name search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHit {
    pub name: String,
    /// Workspace-relative path with `/`.
    pub path: String,
    pub is_dir: bool,
}

/// `search_text` request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchText {
    /// Caller-chosen id used by `cancel_search` and `SearchProgress` events.
    pub search_id: String,
    pub query: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    /// Stop after this many matching lines (0 → 2000).
    pub max_results: u32,
}

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextMatch {
    /// Workspace-relative path with `/`.
    pub path: String,
    /// 1-based line.
    pub line: u64,
    /// 1-based UTF-16 column of the first match.
    pub column: u64,
    /// Line text (at most 500 UTF-16 units).
    pub text: String,
}

/// `search_text` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextSearchResult {
    pub matches: Vec<TextMatch>,
    pub match_count: u64,
    pub file_count: u64,
    /// `max_results` stopped the search.
    pub truncated: bool,
}

/// Git status section of a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GitSection {
    Staged,
    Unstaged,
    Untracked,
    Conflicted,
}

/// Kind of a git change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GitChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileChange {
    /// Workspace-relative path with `/`.
    pub path: String,
    pub old_path: Option<String>,
    pub kind: GitChangeKind,
    pub section: GitSection,
    pub added: u64,
    pub removed: u64,
}

/// `git_status` result.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub is_repository: bool,
    pub git_available: bool,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub detached: bool,
    pub staged: Vec<GitFileChange>,
    /// Unstaged, untracked and conflicted changes.
    pub unstaged: Vec<GitFileChange>,
}

/// One local branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchInfo {
    pub name: String,
    pub current: bool,
    pub upstream: Option<String>,
}

/// `git_commit` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitResult {
    pub hash: String,
    /// First line of `git commit` output.
    pub summary: String,
}

/// Branch switcher data (phase-9-parity.md §9.2.5 R19).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranches {
    pub current: Option<String>,
    pub detached: bool,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub branches: Vec<BranchInfo>,
}

/// `git_switch` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum SwitchOutcome {
    Switched {
        branch: String,
    },
    /// Git refused: local changes to these files would be overwritten. Nothing changed.
    Blocked {
        files: Vec<String>,
    },
}

/// Progress of a push (sent on the call's progress channel).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum PushProgress {
    Started { remote: String, branch: String },
    Finished,
    Failed { message: String },
}

/// `git_push` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResult {
    pub remote: String,
    pub branch: String,
    /// The push set the upstream (`--set-upstream`).
    pub set_upstream: bool,
    /// Ahead / behind the upstream after the push.
    pub ahead: u64,
    pub behind: u64,
    /// Last lines of git's output.
    pub summary: String,
}

/// One commit graph row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLogRow {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    /// Unix seconds.
    pub time: i64,
    pub subject: String,
    /// Decorations (`HEAD -> main`, `origin/main`, `tag: v1`).
    pub refs: Vec<String>,
    /// Column of this commit's dot.
    pub lane: u32,
    /// Column each parent continues in (same order as `parents`).
    pub parent_lanes: Vec<u32>,
    /// Columns in use on this row (for the line drawing width).
    pub lanes: u32,
}

/// `git_log` page.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLogPage {
    pub rows: Vec<GitLogRow>,
    pub has_more: bool,
}

/// `terminal_create` request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCreate {
    pub cols: u16,
    pub rows: u16,
    /// Working directory; `None` = workspace root. Relative paths are resolved against the root.
    pub cwd: Option<String>,
    /// Shell program; `None` = platform default (`powershell.exe` / `$SHELL`).
    pub shell: Option<String>,
}

/// A running terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    pub id: String,
    pub shell: String,
    pub cwd: String,
    pub pid: Option<u32>,
}

/// Lazy-start counters (gates use them to prove nothing runs before first use).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStats {
    pub terminals: u32,
    pub watches: u32,
    pub searches_running: u32,
    pub index_built: bool,
    pub pty_started: bool,
    pub watcher_started: bool,
}

/// One workspace event. `seq` grows by one per event delivered to a subscriber.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEvent {
    pub seq: u64,
    pub kind: WorkspaceEventKind,
}

/// Workspace event payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkspaceEventKind {
    /// Terminal output (decoded UTF-8, never empty). Always before the terminal's `TerminalExited`.
    TerminalOutput { id: String, data: String },
    /// The shell exited on its own. Not sent after `terminal_close`.
    TerminalExited { id: String, code: i64 },
    /// Coalesced file changes under the root (workspace-relative paths; empty + `rescan` = re-read all).
    #[serde(rename_all = "camelCase")]
    FilesChanged {
        watch_id: String,
        paths: Vec<String>,
        rescan: bool,
    },
    /// A watch closed itself (root moved/deleted, watcher error).
    #[serde(rename_all = "camelCase")]
    WatchClosed { watch_id: String, reason: String },
    /// A batch of matches of a running `search_text`.
    #[serde(rename_all = "camelCase")]
    SearchProgress {
        search_id: String,
        matches: Vec<TextMatch>,
    },
}

/// Services of one workspace root (spec §15.3). Paths are workspace-relative with `/`, or absolute inside the
/// root; a path outside the root is rejected with `ApiError::InvalidArgument`.
pub trait WorkspaceServices: Send + Sync {
    /// The canonical workspace root.
    fn root(&self) -> String;

    /// Reads a text file (at most `max_bytes`, 0 → 1 MiB).
    fn read_file(&self, path: String, max_bytes: u64) -> BoxFuture<'_, ApiResult<FileContent>>;

    /// Opens a file or folder with the OS default application.
    fn open_external(&self, path: String) -> BoxFuture<'_, ApiResult<()>>;

    /// Fuzzy file-name search (builds the index on first use).
    fn search_files(&self, query: String, limit: u32) -> BoxFuture<'_, ApiResult<Vec<FileHit>>>;

    /// Content search; `SearchProgress` events precede the result.
    fn search_text(&self, request: SearchText) -> BoxFuture<'_, ApiResult<TextSearchResult>>;

    /// Cancels a running `search_text` (it then fails with `ApiError::Internal("cancelled")`).
    fn cancel_search(&self, search_id: String) -> BoxFuture<'_, ApiResult<()>>;

    /// Branch, upstream and changed files.
    fn git_status(&self) -> BoxFuture<'_, ApiResult<GitStatus>>;

    /// Diff of one file: staged = index vs HEAD, else worktree vs index (untracked: whole file).
    fn git_file_diff(&self, path: String, staged: bool) -> BoxFuture<'_, ApiResult<FileDiff>>;

    /// Stages paths (`git add -A -- paths`).
    fn git_stage(&self, paths: Vec<String>) -> BoxFuture<'_, ApiResult<()>>;

    /// Unstages paths.
    fn git_unstage(&self, paths: Vec<String>) -> BoxFuture<'_, ApiResult<()>>;

    /// Commits the index.
    fn git_commit(&self, message: String) -> BoxFuture<'_, ApiResult<CommitResult>>;

    /// Local branches.
    fn git_branches(&self) -> BoxFuture<'_, ApiResult<Vec<BranchInfo>>>;

    /// Starts a shell on a PTY.
    fn terminal_create(&self, request: TerminalCreate) -> BoxFuture<'_, ApiResult<TerminalInfo>>;

    /// Writes input to a terminal.
    fn terminal_write(&self, id: String, data: String) -> BoxFuture<'_, ApiResult<()>>;

    /// Resizes a terminal.
    fn terminal_resize(&self, id: String, cols: u16, rows: u16) -> BoxFuture<'_, ApiResult<()>>;

    /// Kills a terminal's process tree; no events of `id` follow.
    fn terminal_close(&self, id: String) -> BoxFuture<'_, ApiResult<()>>;

    /// Watches the root recursively (gitignore-aware); returns the watch id.
    fn watch_start(&self) -> BoxFuture<'_, ApiResult<String>>;

    /// Stops a watch.
    fn watch_stop(&self, watch_id: String) -> BoxFuture<'_, ApiResult<()>>;

    /// Lazy-start counters.
    fn stats(&self) -> WorkspaceStats;

    /// Subscribes to workspace events from now on.
    fn subscribe(&self) -> WorkspaceEventStream;

    /// Kills terminals, stops watchers, cancels searches. Idempotent.
    fn shutdown(&self) -> BoxFuture<'_, ApiResult<()>>;
}
