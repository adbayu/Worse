//! The `harness-panels` worker: one current-thread tokio runtime that owns the workspace services, the panel
//! state and the terminal emulator. Single writer of panel state; results of spawned operations come back as
//! [`Done`] messages and are applied only when their sequence number is still the latest (stale results of a
//! superseded refresh / search are dropped, never raced).

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use harness_core_api::diff::FileDiff;
use harness_core_api::workspace::{
    GitChangeKind, GitFileChange, GitSection, GitStatus, SearchText, TerminalCreate,
    WorkspaceEventKind, WorkspaceEventStream, WorkspaceServices,
};
use harness_core_api::{ApiError, SessionId, TurnId, TurnStatus};
use harness_workspace::{Workspace, WorkspaceConfig};
use slint::Weak;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::diffmodel::{self, file_view};
use super::term::{self, Emulator};
use super::view::{self, DiffView, GitFile, GitView, Hit, TermFrame, TermStatus};
use super::{with_hub, Cmd};
use crate::ui::AppWindow;

mod command;
mod diff;
mod events;
mod git;
mod search;
mod terminal;

/// Terminal / search repaint coalescing (one frame).
const FRAME: Duration = Duration::from_millis(16);
/// Git refresh debounce after watcher events.
const GIT_DEBOUNCE: Duration = Duration::from_millis(300);
const MAX_HITS: usize = 500;
const FILE_HITS: u32 = 200;
const PREVIEW_BEFORE: u64 = 10;
const PREVIEW_AFTER: u64 = 60;
const PREVIEW_FILE_LINES: u64 = 200;

/// Results of spawned operations.
enum Done {
    Git(u64, Result<GitStatus, ApiError>),
    Action {
        result: Result<String, String>,
        clear_message: bool,
    },
    /// A git call changed the repository: refresh the Git panel quietly.
    Changed,
    Diff(u64, DiffSource, Result<Vec<FileDiff>, String>),
    Files(u64, Result<Vec<Hit>, ApiError>),
    Text(u64, Result<(Vec<Hit>, u64, u64, bool), ApiError>),
    Preview(u64, String, Result<Vec<(u64, String, bool)>, String>),
}

/// What the Diff panel currently shows (for refresh).
#[derive(Debug, Clone)]
enum DiffSource {
    Git {
        path: String,
        staged: bool,
    },
    Turn {
        session: SessionId,
        turn: TurnId,
        label: String,
    },
}

/// Ordered terminal I/O (writes and resizes must reach the PTY in UI order).
enum TermOp {
    Write(String, String),
    Resize(String, u16, u16),
}

struct Terminal {
    id: Option<String>,
    emu: Emulator,
    /// Rows last sent to the UI.
    shown: Vec<term::Line>,
    status: TermStatus,
    dirty: bool,
}

struct Worker {
    window: Weak<AppWindow>,
    /// `None` = no session or folder open yet: the panels open nothing.
    root: Option<PathBuf>,
    ws: Option<Workspace>,
    events: Option<WorkspaceEventStream>,
    done_tx: mpsc::UnboundedSender<Done>,
    term_tx: Option<mpsc::UnboundedSender<TermOp>>,
    seq: u64,
    git_seq: u64,
    git: Option<GitStatus>,
    watch: Option<String>,
    git_due: Option<Instant>,
    diff_seq: u64,
    diff_source: Option<DiffSource>,
    diff_files: Vec<FileDiff>,
    diff_current: usize,
    search_seq: u64,
    search_id: Option<String>,
    hits: Vec<Hit>,
    search_files: bool,
    search_dirty: bool,
    preview_seq: u64,
    size: Option<(u16, u16)>,
    terminal: Option<Terminal>,
    frame_due: Option<Instant>,
}

/// Per-worker cache of content checks (spec `file-safety.md` §4).
fn content_cache() -> &'static harness_workspace::filetype::ContentCache {
    static CACHE: OnceLock<harness_workspace::filetype::ContentCache> = OnceLock::new();
    CACHE.get_or_init(harness_workspace::filetype::ContentCache::new)
}

/// Warning line for a workspace-relative path, joined with any existing notice.
fn with_content_warning(root: Option<&PathBuf>, path: &str, notice: String) -> String {
    let Some(root) = root else {
        return notice;
    };
    let check = content_cache().inspect(&root.join(path));
    let Some(warning) = check.warning else {
        return notice;
    };
    if notice.is_empty() {
        warning
    } else {
        format!("{notice} · {warning}")
    }
}

/// Thread body: builds the runtime and runs the worker until `Cmd::Shutdown` / the channel closes.
pub(crate) fn run(
    window: Weak<AppWindow>,
    root: Option<PathBuf>,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("harness-slint: cannot start the panels runtime: {error}");
            return;
        }
    };
    runtime.block_on(async move {
        let (done_tx, mut done_rx) = mpsc::unbounded_channel();
        // 修复：没有打开会话/文件夹时不再回退到当前目录（否则面板显示无关文件夹的 Git 状态）。
        let root = root.or_else(|| {
            std::env::var_os("HARNESS_WORKSPACE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        });
        let mut worker = Worker {
            window,
            root,
            ws: None,
            events: None,
            done_tx,
            term_tx: None,
            seq: 0,
            git_seq: 0,
            git: None,
            watch: None,
            git_due: None,
            diff_seq: 0,
            diff_source: None,
            diff_files: Vec::new(),
            diff_current: 0,
            search_seq: 0,
            search_id: None,
            hits: Vec::new(),
            search_files: true,
            search_dirty: false,
            preview_seq: 0,
            size: None,
            terminal: None,
            frame_due: None,
        };
        loop {
            let frame_due = worker.frame_due;
            let git_due = worker.git_due;
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(Cmd::Shutdown) | None => break,
                    Some(cmd) => worker.command(cmd).await,
                },
                Some(done) = done_rx.recv() => worker.done(done),
                event = next_event(&mut worker.events) => match event {
                    Some(kind) => worker.event(kind),
                    None => worker.events = None,
                },
                () = sleep_until(frame_due) => worker.flush(),
                () = sleep_until(git_due) => {
                    worker.git_due = None;
                    worker.git_refresh(false);
                    worker.refresh_git_diff();
                }
            }
        }
        super::trace("worker: loop ended");
        worker.shutdown().await;
        super::trace("worker: workspace shut down");
    });
    drop(runtime);
    super::trace("worker: runtime dropped");
}

async fn next_event(events: &mut Option<WorkspaceEventStream>) -> Option<WorkspaceEventKind> {
    match events {
        Some(stream) => stream.recv().await.map(|e| e.kind),
        None => std::future::pending().await,
    }
}

async fn sleep_until(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due).await,
        None => std::future::pending().await,
    }
}

fn change_code(kind: GitChangeKind) -> (&'static str, &'static str) {
    match kind {
        GitChangeKind::Modified => ("M", "modified"),
        GitChangeKind::Added => ("A", "added"),
        GitChangeKind::Deleted => ("D", "deleted"),
        GitChangeKind::Renamed => ("R", "renamed"),
        GitChangeKind::Untracked => ("U", "untracked"),
        GitChangeKind::Conflicted => ("C", "conflicted"),
    }
}

fn git_file(change: &GitFileChange) -> GitFile {
    let (code, kind) = change_code(change.kind);
    GitFile {
        path: change.path.clone(),
        code,
        kind,
        added: change.added,
        removed: change.removed,
        staged: change.section == GitSection::Staged,
    }
}

fn error_text(error: &ApiError) -> String {
    match error {
        ApiError::InvalidArgument(text) | ApiError::Internal(text) => text.clone(),
        other => other.to_string(),
    }
}

impl Worker {
    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// Opens the workspace on first use (no thread, process or index before a panel asks for one).
    async fn workspace(&mut self) -> Option<Workspace> {
        if let Some(ws) = &self.ws {
            return Some(ws.clone());
        }
        let Some(root) = self.root.clone() else {
            let text = "Open a folder or a session first".to_owned();
            view::git_result(&self.window, text.clone(), true, false);
            view::term_status(
                &self.window,
                TermStatus {
                    error: text,
                    ..TermStatus::default()
                },
            );
            return None;
        };
        match Workspace::open(root, WorkspaceConfig::default()).await {
            Ok(ws) => {
                self.events = Some(ws.subscribe());
                self.ws = Some(ws.clone());
                Some(ws)
            }
            Err(error) => {
                let text = format!("Workspace unavailable: {}", error_text(&error));
                view::git_result(&self.window, text.clone(), true, false);
                view::search(&self.window, Vec::new(), false, String::new(), text.clone());
                view::term_status(
                    &self.window,
                    TermStatus {
                        error: text,
                        ..TermStatus::default()
                    },
                );
                None
            }
        }
    }

    /// 修复：面板（Git/终端/搜索/Diff）一直停在启动时的工作区；打开另一个文件夹的会话后切换到该会话的工作区。
    async fn set_root(&mut self, root: PathBuf) {
        if self.root.as_ref() == Some(&root) {
            return;
        }
        // 之前没有工作区时，已打开的面板显示过“先打开文件夹”，现在也要刷新。
        let was_open = self.ws.is_some() || self.root.is_none();
        let had_terminal = self.terminal.is_some() || (self.root.is_none() && self.size.is_some());
        // 关闭旧工作区（终端、监视器、搜索），再清空依赖它的面板状态。
        self.shutdown().await;
        self.root = Some(root);
        self.events = None;
        self.watch = None;
        self.git = None;
        self.git_due = None;
        self.diff_source = None;
        self.diff_files.clear();
        self.diff_current = 0;
        self.search_id = None;
        self.hits.clear();
        self.terminal = None;
        self.frame_due = None;
        view::diff(&self.window, DiffView::default());
        view::search(
            &self.window,
            Vec::new(),
            false,
            String::new(),
            String::new(),
        );
        if !was_open {
            return;
        }
        let Some(ws) = self.workspace().await else {
            return;
        };
        match ws.watch_start().await {
            Ok(id) => self.watch = Some(id),
            Err(error) => eprintln!("harness-slint: watcher unavailable: {error}"),
        }
        self.git_refresh(true);
        if had_terminal {
            self.term_start(&ws).await;
        }
    }
}

/// `C:\…\pwsh.exe` → `pwsh`.
fn shell_name(shell: &str) -> String {
    let name = shell.rsplit(['\\', '/']).next().unwrap_or(shell);
    name.strip_suffix(".exe").unwrap_or(name).to_owned()
}
