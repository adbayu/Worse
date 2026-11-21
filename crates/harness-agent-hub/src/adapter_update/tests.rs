#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn target(package: &'static str, agent: &str) -> Target {
    Target {
        package,
        agent: AgentId::from(agent),
    }
}

#[test]
fn parses_versions_with_prefix_and_prerelease() {
    assert_eq!(parse_version("1.2.3"), Some(vec![1, 2, 3]));
    assert_eq!(parse_version(" v0.5 "), Some(vec![0, 5]));
    assert_eq!(parse_version("2.0.0-beta.4"), Some(vec![2, 0, 0]));
    assert_eq!(parse_version("3"), Some(vec![3]));
    assert_eq!(parse_version(""), None);
    assert_eq!(parse_version("latest"), None);
}

#[test]
fn compares_versions_numerically() {
    assert!(is_newer(&[1, 10, 0], &[1, 9, 9]));
    assert!(!is_newer(&[1, 9, 9], &[1, 10, 0]));
    assert!(!is_newer(&[1, 2, 3], &[1, 2, 3]));
    assert!(is_newer(&[1, 2, 3, 1], &[1, 2, 3]));
    assert!(!is_newer(&[1, 2], &[1, 2, 0]));
}

#[test]
fn reads_installed_versions_from_one_npm_ls() {
    let json = r#"{"dependencies":{
        "@agentclientprotocol/codex-acp":{"version":"0.7.2"},
        "@openai/codex":{"version":"0.51.0"},
        "typescript":{"version":"5.9.2"}
    }}"#;
    let installed = parse_globals(json).unwrap();
    assert_eq!(
        installed.get("@agentclientprotocol/codex-acp"),
        Some(&vec![0, 7, 2])
    );
    assert_eq!(installed.get("@openai/codex"), Some(&vec![0, 51, 0]));
    assert_eq!(installed.get("@anthropic-ai/claude-code"), None);
    assert_eq!(parse_globals(""), None);
    assert_eq!(parse_globals("{}").unwrap(), BTreeMap::new());
}

#[test]
fn upgrades_a_cli_only_when_npm_installed_it() {
    let targets = vec![
        target("@agentclientprotocol/codex-acp", "codex"),
        target("@openai/codex", "codex"),
        target("@anthropic-ai/claude-code", "claude"),
    ];
    let json = r#"{"dependencies":{"@agentclientprotocol/codex-acp":{"version":"0.7.2"},
        "@openai/codex":{"version":"0.51.0"}}}"#;
    let installed = parse_globals(json).unwrap();
    let selected: Vec<_> = select_targets(targets.clone(), &installed)
        .into_iter()
        .map(|target| target.package)
        .collect();
    assert_eq!(
        selected,
        vec!["@agentclientprotocol/codex-acp", "@openai/codex"]
    );
    assert_eq!(select_targets(targets, &BTreeMap::new()), Vec::new());
}

#[test]
fn check_is_due_once_a_day() {
    assert!(is_due(0, 1_000, CHECK_EVERY_SECS));
    assert!(!is_due(1_000, 1_000 + 60, CHECK_EVERY_SECS));
    assert!(is_due(1_000, 1_000 + CHECK_EVERY_SECS, CHECK_EVERY_SECS));
}
