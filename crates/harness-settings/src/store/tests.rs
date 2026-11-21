use super::*;
use crate::model::FollowUp;

type R = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn migrates_legacy_and_keeps_it() -> R {
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join(LEGACY_FILE),
        r#"{"theme":"dark","language":"zh-CN"}"#,
    )?;
    let store = SettingsStore::open(dir.path()).await?;
    assert_eq!(store.snapshot().appearance.theme, ThemePref::Dark);
    assert_eq!(store.snapshot().general.language, Language::ZhCn);
    assert!(dir.path().join(LEGACY_FILE).exists());
    let text = std::fs::read_to_string(dir.path().join(SETTINGS_FILE))?;
    assert!(text.starts_with("{\n  \"schemaVersion\": 1"), "{text}");
    Ok(())
}

#[tokio::test]
async fn unknown_keys_survive_and_same_value_is_noop() -> R {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(SETTINGS_FILE);
    std::fs::write(
        &path,
        "{\n    \"future\": {\"x\": 1},\n    \"general\": {\"language\": \"en-US\", \"later\": true},\n    \"shortcuts\": {\"a\": [\"Ctrl+A\"]}\n}\n",
    )?;
    let store = SettingsStore::open(dir.path()).await?;
    let mut rx = store.subscribe();
    let outcome = store
        .patch(|s| {
            s.general.follow_up = FollowUp::Steer;
            s.shortcuts.clear();
        })
        .await?;
    assert_eq!(outcome, SaveOutcome::Changed);
    assert!(rx.has_changed()?);
    rx.borrow_and_update();
    let text = std::fs::read_to_string(&path)?;
    assert!(text.contains("\"future\": {\n        \"x\": 1"), "{text}");
    assert!(text.contains("\"later\": true"));
    assert!(text.contains("\"followUp\": \"steer\""));
    assert!(!text.contains("Ctrl+A"));
    let modified = std::fs::metadata(&path)?.modified()?;
    let outcome = store
        .patch(|s| s.general.follow_up = FollowUp::Steer)
        .await?;
    assert_eq!(outcome, SaveOutcome::Unchanged);
    assert!(!rx.has_changed()?);
    assert_eq!(std::fs::metadata(&path)?.modified()?, modified);
    let reopened = SettingsStore::open(dir.path()).await?;
    assert_eq!(reopened.snapshot().general.follow_up, FollowUp::Steer);
    Ok(())
}

#[tokio::test]
async fn newer_schema_is_read_only_and_reload_sees_external_edits() -> R {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(SETTINGS_FILE);
    std::fs::write(
        &path,
        r#"{"schemaVersion": 9, "appearance": {"theme": "light"}}"#,
    )?;
    let store = SettingsStore::open(dir.path()).await?;
    assert_eq!(store.snapshot().appearance.theme, ThemePref::Light);
    assert!(matches!(
        store.patch(|s| s.appearance.theme = ThemePref::Dark).await,
        Err(SettingsError::NewerSchema { found: 9, .. })
    ));
    std::fs::write(&path, r#"{"appearance": {"theme": "dark"}}"#)?;
    assert!(store.reload().await?);
    assert_eq!(store.snapshot().appearance.theme, ThemePref::Dark);
    assert!(!store.reload().await?);
    assert_eq!(
        store
            .patch(|s| s.memory.workspace_memory_enabled = false)
            .await?,
        SaveOutcome::Changed
    );
    Ok(())
}

#[tokio::test]
async fn corrupt_file_is_backed_up() -> R {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join(SETTINGS_FILE), "{not json")?;
    let store = SettingsStore::open(dir.path()).await?;
    assert_eq!(*store.snapshot(), Settings::default());
    let backups = std::fs::read_dir(dir.path())?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
        .count();
    assert_eq!(backups, 1);
    Ok(())
}
