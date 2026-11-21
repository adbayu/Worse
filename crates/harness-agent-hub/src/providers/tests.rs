#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::specs::pools;
use super::*;

fn entry(id: &str, base_url: &str, model: &str, kind: &str) -> ProviderEntry {
    ProviderEntry {
        id: id.into(),
        label: id.into(),
        base_url: base_url.into(),
        model: model.into(),
        api_key: "key".into(),
        kind: kind.into(),
    }
}

/// The settings page sends one add per key, each in its own task: every key must end up in the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_adds_keep_every_entry() {
    let home = std::env::temp_dir().join(format!(
        "harness-providers-{}",
        uuid::Uuid::new_v4().simple()
    ));
    tokio::fs::create_dir_all(&home).await.expect("temp home");
    let mut config = HubConfig::from_env().expect("config");
    config.home = home.clone();
    let config = std::sync::Arc::new(config);
    let mut tasks = Vec::new();
    for i in 1..=6 {
        let config = std::sync::Arc::clone(&config);
        tasks.push(tokio::spawn(async move {
            let request = AddProvider {
                label: format!("Pool {i}"),
                base_url: "http://127.0.0.1:1234/v1".into(),
                model: "m".into(),
                api_key: format!("key-{i}"),
                kind: String::new(),
            };
            add(&config, request, &|_| false).await.map(|_| ())
        }));
    }
    for task in tasks {
        task.await.expect("join").expect("add");
    }
    let file = load(&home).await.expect("load");
    assert_eq!(file.providers.len(), 6);
    assert_eq!(
        pools(&file.providers).len(),
        1,
        "same endpoint and model: one round-robin pool"
    );
    let _ = tokio::fs::remove_dir_all(&home).await;
}

fn ids(pool: &[AgentId]) -> Vec<&str> {
    pool.iter().map(AgentId::as_str).collect()
}

#[test]
fn pools_group_command_code_keys_and_equal_providers() {
    let entries = vec![
        entry("command-code-a", "", "", KIND_COMMAND_CODE),
        entry("api-one", "https://x/v1", "m", ""),
        entry("command-code-b", "", "", KIND_COMMAND_CODE),
        entry("api-two", "https://x/v1", "m", ""),
        entry("api-other-model", "https://x/v1", "other", ""),
        entry("api-alone", "https://y/v1", "m", ""),
    ];
    let pools = pools(&entries);
    assert_eq!(pools.len(), 2);
    assert_eq!(ids(&pools[0]), ["command-code-a", "command-code-b"]);
    assert_eq!(ids(&pools[1]), ["api-one", "api-two"]);
}

#[test]
fn one_command_code_key_is_still_a_pool_but_one_provider_is_not() {
    // A single key matters: a session asked for the built-in agent then uses that key.
    let pools = pools(&[
        entry("command-code-a", "", "", KIND_COMMAND_CODE),
        entry("api-one", "https://x/v1", "m", ""),
    ]);
    assert_eq!(pools.len(), 1);
    assert_eq!(ids(&pools[0]), ["command-code-a"]);
}

#[test]
fn keys_without_a_key_stay_out() {
    let mut empty = entry("command-code-a", "", "", KIND_COMMAND_CODE);
    empty.api_key = "  ".into();
    assert_eq!(pools(&[empty]).len(), 0);
}

#[test]
fn the_built_in_agent_stands_for_the_command_code_pool() {
    let pools = pools(&[
        entry("command-code-a", "", "", KIND_COMMAND_CODE),
        entry("api-one", "https://x/v1", "m", ""),
        entry("api-two", "https://x/v1", "m", ""),
    ]);
    let of = |id: &str| pool_of(&pools, &AgentId::new(id)).map(|(i, _)| i);
    assert_eq!(of(COMMAND_CODE), Some(0));
    assert_eq!(of("command-code-a"), Some(0));
    assert_eq!(of("api-two"), Some(1));
    assert_eq!(of("claude-code"), None);
    assert_eq!(of("api-unknown"), None);
}

#[test]
fn stored_file_keeps_old_entries_readable() {
    // Files written before `kind` and `rotation` existed.
    let file: ProvidersFile = serde_json::from_str(
        r#"{"providers":[{"id":"api-a","label":"A","baseUrl":"https://x/v1","model":"m","apiKey":"k"}]}"#,
    )
    .unwrap();
    assert_eq!(file.providers[0].kind, "");
    assert!(file.rotation.is_none());
    // And a plain provider is written back without the new fields.
    let text = serde_json::to_string(&file).unwrap();
    assert!(!text.contains("kind") && !text.contains("rotation"));
}
