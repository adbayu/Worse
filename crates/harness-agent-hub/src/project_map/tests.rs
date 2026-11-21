use super::scan::{summarize, walk};
use super::*;

fn temp() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("harness-map-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn maps_folders_entries_and_skips_dependencies() {
    let root = temp().join("demo");
    for file in [
        "Cargo.toml",
        "crates/core/Cargo.toml",
        "crates/core/src/lib.rs",
        "crates/core/src/a.rs",
        "crates/app/src/main.rs",
        "docs/guide.md",
        "node_modules/pkg/index.js",
        "target/debug/x.rs",
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x").unwrap();
    }
    let tree = walk(&root);
    assert_eq!(tree.dirs.get("crates"), Some(&4));
    assert!(!tree.dirs.contains_key("node_modules") && !tree.dirs.contains_key("target"));
    assert_eq!(tree.entries.first().map(String::as_str), Some("Cargo.toml"));
    assert!(tree.entries.iter().any(|e| e == "crates/app/src/main.rs"));
    let text = render(&root, &tree, &[("crates/core/src/a.rs".into(), 4)], None);
    assert!(text.starts_with("<harness-map>\nMap of demo (file tree and git history)"));
    assert!(text.contains("- crates/ 4: core, app") || text.contains("- crates/ 4: app, core"));
    assert!(text.contains("crates/core/src/a.rs (4)") && text.ends_with("</harness-map>"));
    assert!(text.lines().count() <= 40);

    let home = temp();
    let workspace = root.to_string_lossy().into_owned();
    let block = prompt_block(&home, &workspace).unwrap();
    assert!(block.contains("crates/"));
    let cache = cache_path(&home, &workspace);
    let (stamp_now, _) = read_cache(&cache).unwrap();
    assert_eq!(stamp_now, stamp(&root));
    write_cache(&cache, &stamp_now, "<harness-map>\ncached\n</harness-map>");
    assert!(prompt_block(&home, &workspace).unwrap().contains("cached"));
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn summarizes_a_graph() {
    let graph: Value = serde_json::from_str(
        r#"{"nodes":[
            {"id":"hub","label":"Hub","source_file":"crates/hub/src/hub.rs","community":1},
            {"id":"session","label":"Session","source_file":"crates/hub/src/session.rs","community":1},
            {"id":"pool","label":"Pool","source_file":"crates/hub/src/pool.rs","community":1},
            {"id":"ui","label":"App","source_file":"apps/ui/main.rs","community":2}],
          "links":[{"source":"hub","target":"session"},{"source":"hub","target":"pool"},
            {"source":"ui","target":"hub"}]}"#,
    )
    .unwrap();
    let summary = summarize(&graph).unwrap();
    assert_eq!(
        summary.god[0],
        ("Hub".into(), "crates/hub/src/hub.rs".into(), 3)
    );
    assert_eq!(summary.communities[0].0, 3);
    assert_eq!(summary.communities[0].1[0], "Hub");
    assert_eq!(summary.communities[0].2, "crates/hub/src");
}

#[test]
fn reads_heads_from_refs_and_packed_refs() {
    let root = temp();
    let git = root.join(".git");
    std::fs::create_dir_all(git.join("refs/heads")).unwrap();
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(git.join("packed-refs"), "# pack\nabc123 refs/heads/main\n").unwrap();
    assert_eq!(git_head(&root).as_deref(), Some("abc123"));
    std::fs::write(git.join("refs/heads/main"), "def456\n").unwrap();
    assert_eq!(git_head(&root).as_deref(), Some("def456"));
    let _ = std::fs::remove_dir_all(root);
}
