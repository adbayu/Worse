//! `SettingsStore`: the single owner and writer of `<data dir>/settings.json` (spec §9.1.1).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{watch, Mutex};

use crate::error::SettingsError;
use crate::fsx;
use crate::json::{detect_indent, OValue};
use crate::model::{Language, Settings, ThemePref, SCHEMA_VERSION};

/// File name of the settings file inside the data dir.
pub const SETTINGS_FILE: &str = "settings.json";
/// Phase 8 UI preferences file (migration source, never written).
pub const LEGACY_FILE: &str = "ui-settings.json";

/// Dotted paths the typed value replaces wholesale on save (user-owned maps where removing a key must delete it).
const REPLACE_PATHS: &[&str] = &["shortcuts"];

/// Result of a save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The file was written and subscribers were notified.
    Changed,
    /// Same content as on disk: nothing written, no event.
    Unchanged,
}

struct State {
    /// File text as last read or written by this store (`None` = no file).
    disk_text: Option<String>,
    /// Set when the file was written by a newer schema: never overwritten.
    newer_schema: Option<u64>,
}

/// Owner of `settings.json`. Create one per process; clone the `Arc` to share.
pub struct SettingsStore {
    path: PathBuf,
    state: Mutex<State>,
    tx: watch::Sender<Arc<Settings>>,
}

impl SettingsStore {
    /// Opens the store for `data_dir`: reads `settings.json`, or migrates `ui-settings.json` when only that exists.
    /// A corrupt `settings.json` is renamed to `settings.json.corrupt-<ms>` and defaults are used.
    ///
    /// # Errors
    ///
    /// I/O failures.
    pub async fn open(data_dir: &Path) -> Result<Self, SettingsError> {
        let path = data_dir.join(SETTINGS_FILE);
        let text = fsx::read_to_string_opt(&path).await?;
        let (settings, disk_text, newer_schema, migrate) = match text {
            Some(text) => match parse(&text) {
                Ok((settings, version)) => {
                    let newer = (version > SCHEMA_VERSION).then_some(version);
                    if let Some(found) = newer {
                        tracing::warn!(
                            found,
                            "settings.json written by a newer Harness; read-only"
                        );
                    }
                    (settings, Some(text), newer, false)
                }
                Err(message) => {
                    let backup =
                        path.with_file_name(format!("{SETTINGS_FILE}.corrupt-{}", now_ms()));
                    tracing::warn!(%message, backup = %backup.display(), "settings.json unreadable; defaults used");
                    tokio::fs::rename(&path, &backup).await?;
                    (Settings::default(), None, None, false)
                }
            },
            None => match migrate_legacy(data_dir).await? {
                Some(settings) => (settings, None, None, true),
                None => (Settings::default(), None, None, false),
            },
        };
        let (tx, _rx) = watch::channel(Arc::new(settings.clone()));
        let store = Self {
            path,
            state: Mutex::new(State {
                disk_text,
                newer_schema,
            }),
            tx,
        };
        if migrate {
            store.save(settings).await?;
            tracing::info!("settings.json created from ui-settings.json");
        }
        Ok(store)
    }

    /// Path of `settings.json`.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Current settings.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Settings> {
        self.tx.borrow().clone()
    }

    /// Change notifications (the current value is marked seen for new receivers).
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Arc<Settings>> {
        self.tx.subscribe()
    }

    /// Applies `edit` to a copy of the current settings and saves it (unknown keys kept, atomic, no-op when equal).
    ///
    /// # Errors
    ///
    /// [`SettingsError::NewerSchema`] for a file of a newer Harness; I/O failures.
    pub async fn patch(
        &self,
        edit: impl FnOnce(&mut Settings),
    ) -> Result<SaveOutcome, SettingsError> {
        let mut next = (*self.snapshot()).clone();
        edit(&mut next);
        self.save(next).await
    }

    /// Re-reads the file when it changed on disk (edited outside Harness). Returns whether the value changed.
    ///
    /// # Errors
    ///
    /// I/O failures or an invalid file (the current value is kept).
    pub async fn reload(&self) -> Result<bool, SettingsError> {
        let mut state = self.state.lock().await;
        let text = fsx::read_to_string_opt(&self.path).await?;
        if text == state.disk_text {
            return Ok(false);
        }
        let (settings, version) = if let Some(t) = &text {
            parse(t).map_err(|message| SettingsError::Invalid {
                path: self.path.display().to_string(),
                message,
            })?
        } else {
            (Settings::default(), SCHEMA_VERSION)
        };
        state.newer_schema = (version > SCHEMA_VERSION).then_some(version);
        state.disk_text = text;
        let changed = *self.snapshot() != settings;
        if changed {
            self.tx.send_replace(Arc::new(settings));
        }
        Ok(changed)
    }

    async fn save(&self, settings: Settings) -> Result<SaveOutcome, SettingsError> {
        let settings = settings.normalized();
        let mut state = self.state.lock().await;
        if let Some(found) = state.newer_schema {
            return Err(SettingsError::NewerSchema {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        // 以磁盘当前内容为合并底稿：外部写入的未知键必须保留，而不是用内存里的旧快照覆盖它们。
        let current_text = fsx::read_to_string_opt(&self.path).await?;
        let mut tree = match current_text.as_deref().map(OValue::parse) {
            Some(Ok(tree @ OValue::Object(_))) => tree,
            _ => OValue::Object(crate::json::OMap::new()),
        };
        let before = tree.clone();
        let typed_text = serde_json::to_string(&settings).map_err(|e| SettingsError::Invalid {
            path: self.path.display().to_string(),
            message: e.to_string(),
        })?;
        let typed = OValue::parse(&typed_text).map_err(|e| SettingsError::Invalid {
            path: self.path.display().to_string(),
            message: e.to_string(),
        })?;
        tree.merge_from(&typed, REPLACE_PATHS);
        let unchanged_on_disk = current_text.is_some() && tree == before;
        if unchanged_on_disk {
            state.disk_text = current_text;
            drop(state);
            if *self.snapshot() != settings {
                self.tx.send_replace(Arc::new(settings));
            }
            return Ok(SaveOutcome::Unchanged);
        }
        let indent = current_text
            .as_deref()
            .map_or_else(|| "  ".into(), detect_indent);
        let text = tree.to_pretty(&indent, true);
        fsx::write_atomic(&self.path, text.as_bytes()).await?;
        state.disk_text = Some(text);
        drop(state);
        self.tx.send_replace(Arc::new(settings));
        Ok(SaveOutcome::Changed)
    }
}

/// Parses settings text: typed value (normalised) and the declared schema version.
fn parse(text: &str) -> Result<(Settings, u64), String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if !value.is_object() {
        return Err("top level is not an object".into());
    }
    let version = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(SCHEMA_VERSION);
    let settings: Settings = serde_json::from_value(value).map_err(|e| e.to_string())?;
    Ok((settings.normalized(), version))
}

/// Reads Phase 8 `ui-settings.json` (`{ theme, language }`) into settings; `None` when absent or unusable.
async fn migrate_legacy(data_dir: &Path) -> Result<Option<Settings>, SettingsError> {
    let Some(text) = fsx::read_to_string_opt(&data_dir.join(LEGACY_FILE)).await? else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        tracing::warn!("ui-settings.json unreadable; not migrated");
        return Ok(None);
    };
    let mut settings = Settings::default();
    if let Some(theme) = value.get("theme").and_then(serde_json::Value::as_str) {
        settings.appearance.theme = match theme {
            "light" => ThemePref::Light,
            "dark" => ThemePref::Dark,
            _ => ThemePref::System,
        };
    }
    if let Some(language) = value
        .get("language")
        .and_then(serde_json::Value::as_str)
        .and_then(Language::from_tag)
    {
        settings.general.language = language;
    }
    Ok(Some(settings))
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

#[cfg(test)]
mod tests;
