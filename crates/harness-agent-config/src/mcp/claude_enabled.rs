use super::{get_bool, get_list, set_list, FileChange, JsonFile, Path, Result};

// ---------------------------------------------------------------------------------------------------------------
// Claude Code workspace approval (`.claude/settings.local.json`).

pub(super) const ENABLED_KEY: &str = "enabledMcpjsonServers";
pub(super) const DISABLED_KEY: &str = "disabledMcpjsonServers";
pub(super) const ENABLE_ALL_KEY: &str = "enableAllProjectMcpServers";

/// Whether Claude Code uses project server `name` per the local settings file.
pub(crate) async fn claude_project_enabled(settings_local: &Path) -> Result<impl Fn(&str) -> bool> {
    let file = JsonFile::load(settings_local).await?;
    let root = file.tree.as_object().cloned().unwrap_or_default();
    let enabled = get_list(&root, ENABLED_KEY);
    let disabled = get_list(&root, DISABLED_KEY);
    let all = get_bool(&root, ENABLE_ALL_KEY).unwrap_or(false);
    Ok(move |name: &str| {
        !disabled.iter().any(|n| n == name) && (all || enabled.iter().any(|n| n == name))
    })
}

pub(crate) async fn plan_claude_project_enabled(
    settings_local: &Path,
    name: &str,
    enabled: bool,
) -> Result<FileChange> {
    let mut file = JsonFile::load(settings_local).await?;
    let root = file.root();
    let (add, remove) = if enabled {
        (ENABLED_KEY, DISABLED_KEY)
    } else {
        (DISABLED_KEY, ENABLED_KEY)
    };
    let mut removed = get_list(root, remove);
    if removed.iter().any(|n| n == name) {
        removed.retain(|n| n != name);
        set_list(root, remove, &removed);
    }
    let all = get_bool(root, ENABLE_ALL_KEY).unwrap_or(false);
    if !(enabled && all) {
        let mut added = get_list(root, add);
        if !added.iter().any(|n| n == name) {
            added.push(name.to_owned());
            set_list(root, add, &added);
        }
    }
    Ok(file.into_change())
}
