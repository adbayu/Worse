#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{run, shell_search, Request, CONTEXT_MAX};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

fn tree() -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("harness-search-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
    std::fs::create_dir_all(dir.join("target/debug")).unwrap();
    let many = (0..60).fold(String::new(), |mut acc, i| {
        use std::fmt::Write as _;
        let _ = writeln!(acc, "let needle_{i} = {i};");
        acc
    });
    std::fs::write(dir.join("src/many.rs"), many).unwrap();
    std::fs::write(dir.join("src/one.rs"), "a\nb\nfn needle() {}\nc\nd\n").unwrap();
    std::fs::write(dir.join("node_modules/pkg/index.js"), "needle\n").unwrap();
    std::fs::write(dir.join("target/debug/out.rs"), "needle\n").unwrap();
    std::fs::write(dir.join("src/blob.bin"), b"needle\0\x01\x02").unwrap();
    dir
}

fn request(dir: &std::path::Path, pattern: &str) -> Request {
    Request {
        root: dir.to_path_buf(),
        cwd: dir.to_path_buf(),
        pattern: pattern.into(),
        glob: None,
        insensitive: false,
        files_only: false,
        context: 0,
    }
}

#[test]
fn skips_dependencies_binaries_and_caps_each_file() {
    let dir = tree();
    let out = run(
        &request(&dir, "needle"),
        &AtomicBool::new(false),
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(out.starts_with("61 matches in 2 files"), "{out}");
    assert!(
        !out.contains("node_modules") && !out.contains("target") && !out.contains("blob"),
        "{out}"
    );
    assert!(out.contains("… 40 more in"), "{out}");
    let mut files = request(&dir, "needle");
    files.files_only = true;
    let listed = run(&files, &AtomicBool::new(false), Duration::from_secs(20)).unwrap();
    assert!(listed.lines().nth(1).unwrap().ends_with("(60)"), "{listed}");
    let mut explicit = request(&dir.join("node_modules"), "needle");
    explicit.cwd = dir.clone();
    let inside = run(&explicit, &AtomicBool::new(false), Duration::from_secs(20)).unwrap();
    assert!(inside.starts_with("1 matches in 1 files"), "{inside}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn context_cancel_and_deadline() {
    let dir = tree();
    let mut with = request(&dir, "fn needle");
    with.context = CONTEXT_MAX + 5;
    let out = run(&with, &AtomicBool::new(false), Duration::from_secs(20)).unwrap();
    assert!(
        out.contains("one.rs-2- b") && out.contains("one.rs-5- d"),
        "{out}"
    );
    assert_eq!(
        run(
            &request(&dir, "needle"),
            &AtomicBool::new(true),
            Duration::from_secs(20)
        ),
        Err("cancelled".into())
    );
    let late = run(
        &request(&dir, "needle"),
        &AtomicBool::new(false),
        Duration::ZERO,
    )
    .unwrap();
    assert!(late.contains("stopped after 0 s"), "{late}");
    assert!(run(
        &request(&dir, "("),
        &AtomicBool::new(false),
        Duration::from_secs(1)
    )
    .is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn recognises_recursive_shell_searches() {
    for yes in [
        "Get-ChildItem -Recurse -Include *.rs | Select-String foo",
        "gci -r | sls 'bar'",
        "grep -rn foo .",
        "grep -R foo src",
        "grep --recursive foo",
        "findstr /s /i foo *.ts",
        "dir /s | Select-String x",
    ] {
        assert!(shell_search(yes), "{yes}");
    }
    for no in [
        "Select-String -Path src/main.rs -Pattern foo",
        "grep foo file.txt",
        "grep -n foo file.txt",
        "Get-ChildItem -Recurse src | Measure-Object",
        "rg foo",
        "cargo test",
    ] {
        assert!(!shell_search(no), "{no}");
    }
}
