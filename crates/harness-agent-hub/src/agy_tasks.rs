//! agy moves a shell command into its own background task when it outlives `WaitMsBeforeAsync`. Over ACP the
//! tool call then just stays "in progress", so Harness reads the tasks from agy's brain folder instead:
//! `<home>/.gemini/antigravity-cli/brain/<native session>/.system_generated/`, where `tasks/task-N.log` holds
//! the output and `messages/*.json` gets a "Task id … finished with result:" message when one ends.
//!
//! Sync on purpose (directory reads); the session actor calls it through `spawn_blocking`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// One agy background task found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgyTask {
    /// `task-N`.
    pub name: String,
    pub number: u64,
    pub log: PathBuf,
    /// Log modification time, ms since the epoch.
    pub modified_ms: i64,
}

/// How a task ended, from agy's completion message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finished {
    pub ok: bool,
    pub summary: String,
}

/// Everything known about the tasks of one native session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub tasks: Vec<AgyTask>,
    pub finished: HashMap<String, Finished>,
}

const SUMMARY_CAP: usize = 600;

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// agy's working folder of one native session.
pub(crate) fn session_dir(native: &str) -> Option<PathBuf> {
    if native.is_empty() || native.contains(['/', '\\', '.']) {
        return None;
    }
    Some(
        home()?
            .join(".gemini")
            .join("antigravity-cli")
            .join("brain")
            .join(native)
            .join(".system_generated"),
    )
}

/// The `toolSummary` (or the command) of an agy shell tool call that may go to the background.
pub(crate) fn async_command(raw_input: Option<&str>) -> Option<String> {
    let input: Value = serde_json::from_str(raw_input?).ok()?;
    input.get("WaitMsBeforeAsync")?;
    let text = ["toolSummary", "CommandLine"]
        .iter()
        .find_map(|k| {
            input
                .get(*k)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or("background command");
    Some(text.chars().take(200).collect())
}

/// Reads tasks and completion messages under `dir` (a [`session_dir`]).
pub(crate) fn snapshot(dir: &Path) -> Snapshot {
    Snapshot {
        tasks: tasks(&dir.join("tasks")),
        finished: finished(&dir.join("messages")),
    }
}

fn tasks(dir: &Path) -> Vec<AgyTask> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<AgyTask> = entries
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let name = file.strip_suffix(".log")?.to_owned();
            let number = name.strip_prefix("task-")?.parse().ok()?;
            let modified_ms = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_millis()).ok())
                .unwrap_or(0);
            Some(AgyTask {
                name,
                number,
                log: e.path(),
                modified_ms,
            })
        })
        .collect();
    out.sort_by_key(|t| t.number);
    out
}

fn finished(dir: &Path) -> HashMap<String, Finished> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| parse_message(&text))
        .collect()
}

/// `("task-N", Finished)` from one agy message file, when it reports a finished task.
fn parse_message(text: &str) -> Option<(String, Finished)> {
    let msg: Value = serde_json::from_str(text).ok()?;
    let sender = msg.get("sender").and_then(Value::as_str)?;
    let name = sender
        .rsplit('/')
        .next()
        .filter(|n| n.starts_with("task-"))?;
    let content = msg.get("content").and_then(Value::as_str)?;
    let (_, result) = content.split_once("finished with result:")?;
    let result = result.trim();
    let ok = !result.contains("exited with code") || result.contains("exited with code 0.");
    let summary: String = result
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("Command output:"))
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    Some((
        name.to_owned(),
        Finished {
            ok,
            summary: summary.chars().take(SUMMARY_CAP).collect(),
        },
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn reads_tasks_and_finish_messages() {
        let dir = std::env::temp_dir().join(format!("harness-agy-tasks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tasks")).unwrap();
        std::fs::create_dir_all(dir.join("messages")).unwrap();
        std::fs::write(dir.join("tasks/task-12.log"), "building").unwrap();
        std::fs::write(dir.join("tasks/task-3.log"), "").unwrap();
        std::fs::write(dir.join("tasks/notes.txt"), "").unwrap();
        let msg = serde_json::json!({
            "sender": "abc/task-3",
            "content": "Task id \"abc/task-3\" finished with result:\nCommand exited with code 1.\n\nCommand output:\nerror: boom\n"
        });
        std::fs::write(dir.join("messages/m.json"), msg.to_string()).unwrap();
        let snap = snapshot(&dir);
        let names: Vec<&str> = snap.tasks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["task-3", "task-12"]);
        let done = &snap.finished["task-3"];
        assert!(!done.ok);
        assert!(done.summary.contains("error: boom"), "{}", done.summary);
        assert!(!snap.finished.contains_key("task-12"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_async_commands() {
        let raw = r#"{"CommandLine":"cargo build","WaitMsBeforeAsync":500,"toolSummary":"Build release"}"#;
        assert_eq!(async_command(Some(raw)).as_deref(), Some("Build release"));
        assert_eq!(async_command(Some(r#"{"CommandLine":"ls"}"#)), None);
        assert_eq!(async_command(None), None);
        assert!(session_dir("../x").is_none());
    }
}
