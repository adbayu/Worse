//! Round 8.2 scenarios (spec phase-8-core.md §15.5): workspace services on an isolated fixture repository,
//! turn diffs, and the API 0.2.0 additions.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use harness_agent_hub::{Hub, HubConfig};
use harness_core_api::diff::{DiffLineKind, FileDiffStatus};
use harness_core_api::workspace::{
    GitChangeKind, SearchText, TerminalCreate, WorkspaceEventKind, WorkspaceEventStream,
    WorkspaceServices,
};
use harness_core_api::{EventKind, HarnessCore, ListSessions, SessionId, TurnSegment, TurnStatus};
use harness_workspace::{Workspace, WorkspaceConfig};

use crate::{check, step, Env, Gate, A, R};

mod diffs;
mod scm;
mod search;
mod terminal;

use diffs::{api_8_2, turn_diff};
use scm::{git_commit, git_scenarios};
use search::text_search;
use terminal::{terminal, watch};

const EVENT_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) fn git(cwd: &Path, args: &[&str]) -> R<String> {
    let mut command = std::process::Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    let out = command
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn write(path: &Path, text: &str) -> R<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("write {}: {e}", path.display()))
}

/// A fresh git repository under the isolated home (local identity, no hooks, no autocrlf).
fn fixture(env: &Env) -> R<PathBuf> {
    let root = env.home.join("fixture");
    if root.exists() {
        std::fs::remove_dir_all(&root).map_err(|e| format!("rm fixture: {e}"))?;
    }
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    git(&root, &["init", "-q", "-b", "main"])?;
    for (key, value) in [
        ("user.name", "Harness Gate"),
        ("user.email", "gate@example.invalid"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
        ("core.hooksPath", "no-hooks"),
    ] {
        git(&root, &["config", key, value])?;
    }
    write(&root.join("README.md"), "hello\nworld\n")?;
    write(
        &root.join("src/lib.rs"),
        "fn alpha() {}\n// NEEDLE one\nfn beta() {}\n",
    )?;
    write(&root.join("docs/guide.md"), "Guide\nneedle two\n")?;
    git(&root, &["add", "-A"])?;
    git(&root, &["commit", "-q", "-m", "initial"])?;
    Ok(root)
}

async fn next_event(
    events: &mut WorkspaceEventStream,
    what: &str,
    mut find: impl FnMut(&WorkspaceEventKind) -> bool,
) -> R<WorkspaceEventKind> {
    let deadline = Instant::now() + EVENT_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, events.recv()).await {
            Ok(Some(event)) if find(&event.kind) => return Ok(event.kind),
            Ok(Some(_)) => {}
            Ok(None) => return Err(format!("event stream closed waiting for {what}")),
            Err(_) => return Err(format!("timeout waiting for {what}")),
        }
    }
}

pub(crate) async fn run(env: &Env, gate: &Gate) -> R<()> {
    let root = fixture(env)?;
    let config = WorkspaceConfig {
        watch_debounce: Duration::from_millis(50),
        ..WorkspaceConfig::default()
    };
    let ws = Workspace::open(root.clone(), config.clone())
        .await
        .map_err(|e| format!("open: {e}"))?;
    let result = services(env, &ws, &root, &config).await;
    let shutdown = ws.shutdown().await.map_err(|e| e.to_string());
    result?;
    shutdown?;
    turn_diff(env, gate, &root).await?;
    api_8_2(env, gate).await
}

async fn services(env: &Env, ws: &Workspace, root: &Path, config: &WorkspaceConfig) -> R<()> {
    // ---- lazy start ---------------------------------------------------------------------------------
    let stats = ws.stats();
    check(
        stats.terminals == 0
            && stats.watches == 0
            && stats.searches_running == 0
            && !stats.index_built
            && !stats.pty_started
            && !stats.watcher_started,
        format!("services started eagerly: {stats:?}"),
    )?;
    step("workspace_lazy");

    // ---- read + containment -------------------------------------------------------------------------
    let file = ws
        .read_file("README.md".into(), 0)
        .await
        .map_err(|e| format!("read: {e}"))?;
    check(
        file.text == "hello\nworld\n" && file.path == "README.md" && !file.binary,
        format!("read_file: {file:?}"),
    )?;
    let nested = ws
        .read_file("src\\lib.rs".into(), 5)
        .await
        .map_err(|e| format!("read nested: {e}"))?;
    check(
        nested.path == "src/lib.rs" && nested.truncated && nested.text.len() == 5,
        format!("read_file limit: {nested:?}"),
    )?;
    check(
        ws.read_file("../outside.txt".into(), 0).await.is_err(),
        "path escape (..) was not rejected",
    )?;
    let outside = env.home.join("outside.txt");
    write(&outside, "secret")?;
    check(
        ws.read_file(outside.to_string_lossy().into_owned(), 0)
            .await
            .is_err(),
        "absolute path outside the root was not rejected",
    )?;
    step("workspace_read");

    // ---- file search ----------------------------------------------------------------------------------
    let hits = ws
        .search_files("lib".into(), 20)
        .await
        .map_err(|e| format!("search_files: {e}"))?;
    check(
        hits.iter().any(|h| h.path == "src/lib.rs" && !h.is_dir),
        format!("search_files: {hits:?}"),
    )?;
    check(ws.stats().index_built, "index not built after a search")?;
    step("search_files");
    text_search(env, ws, config).await?;
    git_scenarios(ws, root).await?;
    git_commit(ws, root).await?;
    terminal(ws).await?;
    watch(ws, root).await
}
