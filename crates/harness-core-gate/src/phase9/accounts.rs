use super::{
    check, hub_config, io, read, start, step, write, AddAccount, AgentId, AuthPresence, Env,
    HarnessCore, HubConfig, Language, Layout, Path, PathBuf, SaveOutcome, SettingsStore, ThemePref,
    B, R,
};

/// Hub config whose first fake agent plays `claude-code`, so accounts can be based on it.
pub(super) fn accounts_config(env: &Env, home: &Path) -> HubConfig {
    let mut config = hub_config(env, home, false);
    if let Some(first) = config.agents.first_mut() {
        first.id = AgentId::new("claude-code");
    }
    config.default_fallback_order = config.agents.iter().map(|a| a.id.clone()).collect();
    config
}

pub(super) async fn accounts_step(env: &Env, layout: &Layout) -> R<()> {
    let home = layout.home.join("accounts-data");
    io("mkdir", std::fs::create_dir_all(&home))?;
    let gate = start(accounts_config(env, &home)).await?;
    let work = gate
        .hub
        .add_account(AddAccount {
            base: AgentId::new("claude-code"),
            label: "Work".into(),
        })
        .await
        .map_err(|e| format!("add_account: {e}"))?;
    check(
        work.id.as_str() == "claude-code-work" && !work.loaded,
        format!("new account {work:?}"),
    )?;
    check(
        work.auth == AuthPresence::Absent,
        "fresh profile must not look signed in",
    )?;
    let profile = PathBuf::from(&work.profile_dir);
    check(
        read(&profile.join("settings.json")).contains("unknownSetting"),
        "settings.json not shared into the profile",
    )?;
    check(
        !profile.join(".credentials.json").exists() && !profile.join(".claude.json").exists(),
        "credentials or .claude.json leaked into the profile",
    )?;
    let twin = gate
        .hub
        .add_account(AddAccount {
            base: AgentId::new("claude-code"),
            label: "Work".into(),
        })
        .await
        .map_err(|e| format!("add twin: {e}"))?;
    check(
        twin.id.as_str() == "claude-code-work-2",
        "duplicate label id",
    )?;
    gate.hub
        .remove_account(twin.id.clone())
        .await
        .map_err(|e| format!("remove twin: {e}"))?;
    check(
        gate.hub
            .add_account(AddAccount {
                base: AgentId::new(B),
                label: "x".into(),
            })
            .await
            .is_err(),
        "accounts must only be based on claude-code",
    )?;
    // A saved order from before the account existed must still get the account (fix in Hub::start).
    gate.hub
        .set_fallback_order(vec![AgentId::new(B), AgentId::new("claude-code")])
        .await
        .map_err(|e| format!("set order: {e}"))?;
    gate.hub.shutdown().await.map_err(|e| e.to_string())?;

    let gate = start(accounts_config(env, &home)).await?;
    let agents = gate.hub.list_agents().await.map_err(|e| e.to_string())?;
    check(
        agents.iter().any(|a| a.id.as_str() == "claude-code-work"),
        "account agent missing after restart",
    )?;
    let order = gate
        .hub
        .get_fallback_order()
        .await
        .map_err(|e| e.to_string())?;
    let ids: Vec<&str> = order.iter().map(AgentId::as_str).collect();
    // The saved order stays (B before claude-code); missing agents go to their default position, so the account
    // sits right after claude-code.
    let pos = |id: &str| ids.iter().position(|a| *a == id);
    check(
        ids.first() == Some(&B)
            && pos("claude-code").is_some()
            && pos("claude-code-work") == pos("claude-code").map(|p| p + 1),
        format!("fallback order after restart: {ids:?}"),
    )?;
    let listed = gate.hub.list_accounts().await.map_err(|e| e.to_string())?;
    check(
        listed.len() == 1 && listed.iter().all(|a| a.loaded),
        format!("accounts after restart: {listed:?}"),
    )?;
    gate.hub
        .remove_account(AgentId::new("claude-code-work"))
        .await
        .map_err(|e| format!("remove: {e}"))?;
    gate.hub.shutdown().await.map_err(|e| e.to_string())?;
    step("accounts");
    Ok(())
}

// ---- 1. settings ---------------------------------------------------------------------------------------

pub(super) async fn settings_roundtrip(env: &Env) -> R<()> {
    let dir = &env.home;
    write(
        &dir.join(harness_settings::LEGACY_FILE),
        r#"{"theme":"dark","language":"zh-CN"}"#,
    )?;
    let store = SettingsStore::open(dir)
        .await
        .map_err(|e| format!("open: {e}"))?;
    check(
        store.snapshot().appearance.theme == ThemePref::Dark
            && store.snapshot().general.language == Language::ZhCn,
        "ui-settings.json not migrated",
    )?;
    // An unknown key written by someone else must survive our writes.
    let path = dir.join(harness_settings::SETTINGS_FILE);
    let mut value: serde_json::Value =
        serde_json::from_str(&read(&path)).map_err(|e| format!("settings.json: {e}"))?;
    value["zzFuture"] = serde_json::json!({"keep": 1});
    write(
        &path,
        &serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?,
    )?;
    store.reload().await.map_err(|e| format!("reload: {e}"))?;
    let mut rx = store.subscribe();
    rx.borrow_and_update();
    let outcome = store
        .patch(|s| s.appearance.theme = ThemePref::Light)
        .await
        .map_err(|e| format!("patch: {e}"))?;
    check(outcome == SaveOutcome::Changed, "patch reported no change")?;
    check(
        rx.has_changed().unwrap_or(false),
        "no change event after patch",
    )?;
    let text = read(&path);
    check(
        text.contains("zzFuture") && text.contains("\"light\""),
        "settings.json lost the unknown key or the value",
    )?;
    let mtime = io("stat", std::fs::metadata(&path).and_then(|m| m.modified()))?;
    let same = store
        .patch(|s| s.appearance.theme = ThemePref::Light)
        .await
        .map_err(|e| format!("patch: {e}"))?;
    check(same == SaveOutcome::Unchanged, "same-value patch wrote")?;
    let after = io("stat", std::fs::metadata(&path).and_then(|m| m.modified()))?;
    check(mtime == after, "same-value patch changed the mtime")?;
    drop(store);
    let reopened = SettingsStore::open(dir)
        .await
        .map_err(|e| format!("reopen: {e}"))?;
    check(
        reopened.snapshot().appearance.theme == ThemePref::Light,
        "value lost after reopen",
    )?;
    step("settings_roundtrip");
    Ok(())
}
