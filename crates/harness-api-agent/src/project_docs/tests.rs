use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

/// Temporary project directory, removed on drop (the crate has no `tempfile` dependency).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Option<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "harness-api-agent-docs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).ok()?;
        Some(Self { path })
    }

    /// `name` relative to the project root, as an absolute path string.
    fn dir(&self, name: &str) -> String {
        if name.is_empty() {
            return self.path.to_string_lossy().into_owned();
        }
        self.path.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Project with `.git` at its root plus the given `(relative path, content)` files.
fn project(files: &[(&str, &str)]) -> Option<TempDir> {
    let dir = TempDir::new()?;
    for (name, body) in files {
        let path = dir.path.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::write(&path, body).ok()?;
    }
    Some(dir)
}

#[test]
fn no_project_docs_adds_nothing() {
    let Some(project) = project(&[(".git/config", "")]) else {
        return;
    };
    assert_eq!(instructions(&project.dir("")), "");
}

#[test]
fn nested_agents_md_outermost_first() {
    let Some(project) = project(&[
        (".git/config", ""),
        ("AGENTS.md", "root rules"),
        ("sub/AGENTS.md", "sub rules"),
    ]) else {
        return;
    };
    let out = instructions(&project.dir("sub"));
    let root = out.find("root rules").unwrap_or(usize::MAX);
    let sub = out.find("sub rules").unwrap_or(usize::MAX);
    assert!(root < sub, "{out}");
    // The file in `cwd` gets a relative pointer, the one above it an absolute one.
    assert!(
        out.contains("# Project instructions (AGENTS.md)\nsub rules"),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "# Project instructions ({}",
            project.dir("AGENTS.md").replace('\\', "/")
        )),
        "{out}"
    );
    assert!(out.ends_with(CLOSING), "{out}");
}

#[test]
fn claude_md_used_when_no_agents_md() {
    let Some(project) = project(&[(".git/config", ""), ("CLAUDE.md", "claude rules")]) else {
        return;
    };
    let out = instructions(&project.dir(""));
    assert!(
        out.contains("# Project instructions (CLAUDE.md)\nclaude rules"),
        "{out}"
    );
}

#[test]
fn agents_md_wins_over_claude_md() {
    let Some(project) = project(&[
        (".git/config", ""),
        ("AGENTS.md", "agents rules"),
        ("CLAUDE.md", "claude rules"),
    ]) else {
        return;
    };
    let out = instructions(&project.dir(""));
    assert!(out.contains("agents rules"), "{out}");
    assert!(!out.contains("claude rules"), "{out}");
}

#[test]
fn import_only_claude_md_is_skipped() {
    let Some(project) = project(&[(".git/config", ""), ("CLAUDE.md", "@AGENTS.md\n")]) else {
        return;
    };
    assert_eq!(instructions(&project.dir("")), "");
}

#[test]
fn import_only_claude_md_does_not_shadow_nested_agents_md() {
    let Some(project) = project(&[
        (".git/config", ""),
        ("CLAUDE.md", "@AGENTS.md"),
        ("sub/AGENTS.md", "sub rules"),
    ]) else {
        return;
    };
    let out = instructions(&project.dir("sub"));
    assert!(!out.contains("CLAUDE.md"), "{out}");
    assert!(out.contains("sub rules"), "{out}");
}

#[test]
fn walk_stops_at_git() {
    let Some(project) = project(&[
        ("AGENTS.md", "outside rules"),
        ("repo/.git/config", ""),
        ("repo/AGENTS.md", "inside rules"),
    ]) else {
        return;
    };
    let out = instructions(&project.dir("repo"));
    assert!(out.contains("inside rules"), "{out}");
    assert!(!out.contains("outside rules"), "{out}");
}

#[test]
fn content_is_capped_at_32_kb() {
    let big = "z".repeat(BUDGET * 2);
    let Some(project) = project(&[(".git/config", ""), ("AGENTS.md", &big)]) else {
        return;
    };
    let out = instructions(&project.dir(""));
    assert_eq!(out.matches('z').count(), BUDGET);
    assert!(out.contains("[truncated]"), "missing truncation note");
    assert!(out.ends_with(CLOSING));
}

#[test]
fn budget_is_shared_between_files() {
    let root = "z".repeat(BUDGET - 10);
    let sub = "q".repeat(100);
    let Some(project) = project(&[
        (".git/config", ""),
        ("AGENTS.md", &root),
        ("sub/AGENTS.md", &sub),
    ]) else {
        return;
    };
    let out = instructions(&project.dir("sub"));
    assert_eq!(out.matches('z').count(), BUDGET - 10);
    assert_eq!(out.matches('q').count(), 10);
    assert!(out.contains("[truncated]"), "{out}");
}

#[test]
fn cut_keeps_utf8_boundaries() {
    let body = "é".repeat(BUDGET);
    let Some(project) = project(&[(".git/config", ""), ("AGENTS.md", &body)]) else {
        return;
    };
    let out = instructions(&project.dir(""));
    assert_eq!(out.matches('é').count(), BUDGET / 2);
    assert!(out.contains("[truncated]"), "{out}");
}
