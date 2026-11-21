//! Unit tests: parsers on canned output and the full read path on temp
//! repositories built with the git CLI.

use std::path::Path;
use std::process::Command;

use crate::parse::{parse_graph_records, parse_numstat, parse_status_porcelain};
use crate::runner::never_cancelled;
use crate::types::{
    ChangeKind, ChangeSection, DiffAvailability, GraphRefKind, HeadRefType, WorkspaceRepositoryKind,
};
use crate::{ChangeSource, DiffQuery, GitError, GitService};

mod repo_cases;
mod runner_cases;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(dir: &Path, rel: &str, content: &[u8]) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.invalid"]);
    git(dir, &["config", "core.autocrlf", "false"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn same_path(a: &str, b: &Path) -> bool {
    let canon = |p: &Path| {
        harness_fs::realpath(&p.to_string_lossy())
            .unwrap()
            .replace('\\', "/")
            .to_lowercase()
    };
    canon(Path::new(a)) == canon(b)
}

/// Builds: committed `a.txt`, `pkg/b.ts`, `ren_old.txt`; then modifies `a.txt`
/// (unstaged), stages `pkg/b.ts`, renames `ren_old.txt` (staged), adds an
/// untracked text file, an untracked empty file and an untracked binary.
fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init_repo(dir);
    write(dir, "a.txt", b"one\ntwo\n");
    write(dir, "pkg/b.ts", b"export {}\n");
    write(dir, "ren_old.txt", b"rename me\nline2\nline3\n");
    write(dir, ".gitignore", b"*.log\n");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "initial"]);
    write(dir, "a.txt", b"one\nTWO\nthree\n");
    write(dir, "pkg/b.ts", b"export const x = 1;\nexport {}\n");
    git(dir, &["add", "pkg/b.ts"]);
    git(dir, &["mv", "ren_old.txt", "ren new.txt"]);
    write(dir, "pkg/new file ü.md", b"hello\nworld");
    write(dir, "empty.txt", b"");
    write(dir, "blob.bin", b"\0\x01\x02");
    write(dir, "debug.log", b"ignored\n");
    tmp
}

#[test]
fn parses_porcelain_v2() {
    let out =
        "# branch.oid abc\0# branch.head feat\0# branch.upstream origin/feat\0# branch.ab +2 -3\0\
1 .M N... 100644 100644 100644 h1 h2 a b.txt\0\
2 R. N... 100644 100644 100644 h1 h2 R100 new.txt\0old.txt\0\
u UU N... 100644 100644 100644 100644 h1 h2 h3 c.txt\0? d dir/\0";
    let parsed = parse_status_porcelain(out);
    assert_eq!(parsed.branch_name.as_deref(), Some("feat"));
    assert_eq!(parsed.tracking_branch_name.as_deref(), Some("origin/feat"));
    assert_eq!((parsed.ahead, parsed.behind), (2, 3));
    assert_eq!(parsed.entries.len(), 4);
    assert_eq!(parsed.entries[0].path, "a b.txt");
    assert_eq!(parsed.entries[0].kind, ChangeKind::Modified);
    assert_eq!(parsed.entries[1].original_path.as_deref(), Some("old.txt"));
    assert_eq!(parsed.entries[1].kind, ChangeKind::Renamed);
    assert!(parsed.entries[2].is_conflicted);
    assert_eq!(parsed.entries[3].path, "d dir/");
    let detached = parse_status_porcelain("# branch.head (detached)\0");
    assert_eq!(detached.head_ref_type, HeadRefType::Detached);
    assert_eq!(detached.branch_name, None);
}

#[test]
fn parses_numstat_and_graph() {
    let stats = parse_numstat("3\t1\ta.txt\0-\t-\tbin.dat\0\x30\t0\t\0old.txt\0new.txt\0");
    let rows: Vec<_> = stats
        .iter()
        .map(|(k, v)| (k.clone(), v.added, v.removed, v.kind))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("a.txt".to_owned(), 3, 1, None),
            ("bin.dat".to_owned(), 0, 0, None),
            ("new.txt".to_owned(), 0, 0, Some(ChangeKind::Renamed)),
        ]
    );
    let graph = parse_graph_records(
        "h1\0p1 p2\0Ann\x001700000000\0subj\0HEAD -> main, tag: v1, origin/main, refs/heads/x\x1e\nh2\0\0\0\0s2\0\x1e\n",
    );
    assert_eq!(graph.len(), 2);
    assert_eq!(graph[0].parents, vec!["p1", "p2"]);
    assert_eq!(graph[0].authored_at_ms, Some(1_700_000_000_000));
    let refs: Vec<_> = graph[0]
        .refs
        .iter()
        .map(|r| (r.name.as_str(), r.kind))
        .collect();
    assert_eq!(
        refs,
        vec![
            ("HEAD", GraphRefKind::Head),
            ("main", GraphRefKind::Branch),
            ("v1", GraphRefKind::Tag),
            ("origin/main", GraphRefKind::Remote),
            ("x", GraphRefKind::Branch),
        ]
    );
    assert_eq!(graph[1].author_name, None);
    assert_eq!(graph[1].authored_at_ms, None);
}
