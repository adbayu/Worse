#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use super::*;

fn tool(title: &str) -> ToolCallView {
    ToolCallView {
        tool_call_id: String::new(),
        title: title.to_owned(),
        kind: None,
        status: Some(harness_core_api::ToolCallStatus::Pending),
        raw_input: None,
        content: Vec::new(),
        locations: Vec::new(),
        raw_output: None,
    }
}

fn scope(paths: &[&str], tools: &[&str], network: bool, minutes: i64, now: i64) -> ScopeView {
    ScopeView {
        paths: paths.iter().map(|p| (*p).to_owned()).collect(),
        tools: tools.iter().map(|t| (*t).to_owned()).collect(),
        network,
        expires_at: (minutes > 0).then_some(now + minutes * 60_000),
    }
}

#[test]
fn a_narrower_scope_is_accepted() {
    let parent = scope(&["crates"], &["read"], false, 30, 0);
    let child = scope(&["crates/hub"], &["read"], false, 5, 0);
    let narrowed = narrow(Some(&parent), &child, 0).unwrap();
    assert_eq!(narrowed.paths, vec!["crates/hub".to_owned()]);
    assert_eq!(narrowed.expires_at, Some(300_000));
}

#[test]
fn an_empty_parent_scope_accepts_any_child_scope() {
    let child = scope(&["docs"], &["write"], true, 10, 0);
    let narrowed = narrow(None, &child, 0).unwrap();
    assert_eq!(narrowed, child);
}

#[test]
fn a_child_without_paths_cannot_widen_a_restricted_parent() {
    let parent = scope(&["crates"], &[], true, 0, 0);
    let child = scope(&[], &[], true, 0, 0);
    let error = narrow(Some(&parent), &child, 0).unwrap_err();
    assert!(error.contains("crates"), "{error}");
}

#[test]
fn a_sibling_path_is_wider_than_the_parent() {
    let parent = scope(&["crates/hub"], &[], true, 0, 0);
    let child = scope(&["docs"], &[], true, 0, 0);
    assert!(narrow(Some(&parent), &child, 0).is_err());
}

#[test]
fn tools_and_network_can_only_narrow() {
    let parent = scope(&[], &["read", "search"], false, 0, 0);
    assert!(narrow(Some(&parent), &scope(&[], &["read"], false, 0, 0), 0).is_ok());
    assert!(narrow(Some(&parent), &scope(&[], &["write"], false, 0, 0), 0).is_err());
    assert!(narrow(Some(&parent), &scope(&[], &[], false, 0, 0), 0).is_err());
    assert!(narrow(Some(&parent), &scope(&[], &["read"], true, 0, 0), 0).is_err());
}

#[test]
fn a_longer_deadline_is_rejected_and_a_missing_one_inherits() {
    let parent = scope(&[], &[], true, 10, 0);
    assert!(narrow(Some(&parent), &scope(&[], &[], true, 20, 0), 0).is_err());
    let inherited = narrow(Some(&parent), &scope(&[], &[], true, 0, 0), 0).unwrap();
    assert_eq!(inherited.expires_at, Some(600_000));
}

#[test]
fn paths_outside_the_scope_are_blocked_with_a_reason() {
    let scope = scope(&["crates/hub"], &["read"], true, 0, 0);
    assert!(allows_path(&scope, "crates/hub/src/lib.rs"));
    assert!(!allows_path(&scope, "crates/other/src/lib.rs"));
    assert!(!allows_path(&scope, "crates/hubby"));
    let reason = blocked(
        &scope,
        &tool("read file"),
        &["docs/readme.md".to_owned()],
        0,
    )
    .unwrap();
    assert!(reason.contains("docs/readme.md"), "{reason}");
    assert!(blocked(
        &scope,
        &tool("read file"),
        &["crates/hub/src/lib.rs".to_owned()],
        0
    )
    .is_none());
}

#[test]
fn tools_network_and_the_deadline_are_enforced() {
    let scope = scope(&[], &["read"], false, 5, 0);
    let reason = blocked(&scope, &tool("edit file"), &[], 0).unwrap();
    assert!(reason.contains("read"), "{reason}");
    let reason = blocked(&scope, &tool("read http://example.com"), &[], 0).unwrap();
    assert!(reason.contains("network"), "{reason}");
    let reason = blocked(&scope, &tool("read file"), &[], 600_000).unwrap();
    assert!(reason.contains("deadline"), "{reason}");
    assert!(blocked(&scope, &tool("read file"), &[], 60_000).is_none());
}

#[test]
fn describe_lists_the_limits() {
    let text = describe(&scope(&["src"], &["read"], false, 10, 0), 0);
    assert!(text.contains("src") && text.contains("read") && text.contains("no network"));
    assert!(text.contains("10 min left"), "{text}");
}

#[test]
fn absolute_paths_become_relative_to_the_workspace() {
    assert_eq!(
        relative_to("/work/project", "/work/project/src/main.rs"),
        "src/main.rs"
    );
    assert_eq!(relative_to("/work/project", "/etc/passwd"), "/etc/passwd");
    assert_eq!(
        relative_to("C:\\work", "C:\\work\\src\\main.rs"),
        "src/main.rs"
    );
}
