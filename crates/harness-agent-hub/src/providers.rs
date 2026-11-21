//! Custom OpenAI-compatible providers as agents (phase-9-parity.md §9.7), and API keys of the Command Code CLI.
//!
//! Owner: the hub owns `<home>/providers.json` (the only place the API key is stored). Each provider becomes an
//! agent that runs Harness's own ACP agent `harness-api-agent`; the key reaches that process only through the env
//! var `HARNESS_PROVIDER_API_KEY`. The key is never returned by the API or logged.
//!
//! An entry of kind `command-code` is one API key of the Command Code CLI instead: it becomes an agent that
//! runs `command-code acp` with `COMMAND_CODE_API_KEY` set (the CLI's documented headless login), next to the
//! built-in Command Code agent, which uses the CLI's own login.
//!
//! Key pools: entries that stand for the same service with different keys (all Command Code keys; providers
//! with the same endpoint and model) form a pool. A new session asked for a pool member takes the members in
//! turn (round robin, see `Hub::rotated`), so sessions and delegated subagents spread over the keys; failover
//! on a usage limit walks the same agents, as for any other agent.

use std::path::{Path, PathBuf};

use harness_core_api::{AddProvider, AgentId, ApiError, ProviderInfo, UpdateProvider};
use serde::{Deserialize, Serialize};

use crate::agents::{AgentSpec, ErrorDialect, Transport, COMMAND_CODE};
use crate::config::HubConfig;

mod specs;

pub(crate) use specs::apply;
use specs::is_command_code_agent;

/// File name of the provider list inside the Harness home.
pub const PROVIDERS_FILE: &str = "providers.json";
/// Directory holding per-provider agent state (sessions).
pub const PROVIDERS_DIR: &str = "providers";
/// Env vars read by `harness-api-agent`.
pub const ENV_BASE_URL: &str = "HARNESS_PROVIDER_BASE_URL";
pub const ENV_MODEL: &str = "HARNESS_PROVIDER_MODEL";
pub const ENV_API_KEY: &str = "HARNESS_PROVIDER_API_KEY";
pub const ENV_LABEL: &str = "HARNESS_PROVIDER_LABEL";
pub const ENV_STATE: &str = "HARNESS_PROVIDER_STATE";
/// Kind of an entry that is one API key of the Command Code CLI.
pub const KIND_COMMAND_CODE: &str = "command-code";
/// Env var the Command Code CLI reads its API key from (its documented headless login).
pub const ENV_COMMAND_CODE_KEY: &str = "COMMAND_CODE_API_KEY";
const ID_PREFIX: &str = "api-";
/// Id prefix of Command Code key agents (the name also picks the Command Code logo in the UI).
const COMMAND_CODE_PREFIX: &str = "command-code-";
const LABEL_MAX: usize = 40;
const SLUG_MAX: usize = 24;

#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct ProvidersFile {
    #[serde(default)]
    pub providers: Vec<ProviderEntry>,
    /// `"off"` turns the round robin over key pools off (absent: on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderEntry {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    /// `""` = OpenAI-compatible endpoint; `"command-code"` = a Command Code CLI key.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
}

impl ProviderEntry {
    pub(crate) fn is_command_code(&self) -> bool {
        self.kind == KIND_COMMAND_CODE
    }
}

pub(crate) fn state_dir(home: &Path, id: &str) -> PathBuf {
    home.join(PROVIDERS_DIR).join(id)
}

/// Held across every read-modify-write of `providers.json`. The settings page sends one add per key, each in
/// its own task; without this, two adds read the same file and the later save drops the earlier entry.
static WRITE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn load(home: &Path) -> Result<ProvidersFile, ApiError> {
    let path = home.join(PROVIDERS_FILE);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| ApiError::Storage(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ProvidersFile::default()),
        Err(e) => Err(ApiError::Storage(format!("{}: {e}", path.display()))),
    }
}

async fn save(home: &Path, file: &ProvidersFile) -> Result<(), ApiError> {
    let path = home.join(PROVIDERS_FILE);
    let tmp = home.join(format!("{PROVIDERS_FILE}.tmp"));
    let bytes = serde_json::to_vec_pretty(file).map_err(|e| ApiError::Internal(e.to_string()))?;
    let storage = |e: std::io::Error| ApiError::Storage(format!("{}: {e}", path.display()));
    tokio::fs::create_dir_all(home).await.map_err(storage)?;
    tokio::fs::write(&tmp, bytes).await.map_err(storage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(storage)?;
    }
    tokio::fs::rename(&tmp, &path).await.map_err(storage)
}

fn slug(label: &str) -> String {
    let mut out = String::new();
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= SLUG_MAX {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        "provider".into()
    } else {
        out
    }
}

fn clean_label(label: &str) -> Result<String, ApiError> {
    let label = label.trim().to_owned();
    if label.is_empty() || label.chars().count() > LABEL_MAX {
        return Err(ApiError::InvalidArgument(format!(
            "name must be 1-{LABEL_MAX} characters"
        )));
    }
    Ok(label)
}

/// Normalises a base URL: scheme check, trailing `/` and `/chat/completions` removed.
fn clean_base_url(url: &str) -> Result<String, ApiError> {
    let mut url = url.trim().to_owned();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(ApiError::InvalidArgument(
            "endpoint must start with http:// or https://".into(),
        ));
    }
    while url.ends_with('/') {
        url.pop();
    }
    if let Some(stripped) = url.strip_suffix("/chat/completions") {
        url = stripped.trim_end_matches('/').to_owned();
    }
    if url.len() <= "https://".len() {
        return Err(ApiError::InvalidArgument("endpoint has no host".into()));
    }
    Ok(url)
}

fn clean_model(model: &str) -> Result<String, ApiError> {
    let model = model.trim().to_owned();
    if model.is_empty() {
        return Err(ApiError::InvalidArgument("model must not be empty".into()));
    }
    Ok(model)
}

/// The pool a session asked for `agent` rotates over, if any. The built-in Command Code agent stands for the
/// pool of Command Code keys.
pub(crate) fn pool_of<'a>(
    pools: &'a [Vec<AgentId>],
    agent: &AgentId,
) -> Option<(usize, &'a [AgentId])> {
    pools.iter().enumerate().find_map(|(i, pool)| {
        let member = pool.contains(agent)
            || (agent.as_str() == COMMAND_CODE
                && pool.iter().all(|a| is_command_code_agent(a.as_str())));
        member.then_some((i, pool.as_slice()))
    })
}

/// True when the agent is a provider agent (its env carries the provider base URL).
pub(crate) fn is_provider(spec: &AgentSpec) -> bool {
    spec.env.iter().any(|(k, _)| k == ENV_BASE_URL)
}

fn info(entry: &ProviderEntry, loaded: &(dyn Fn(&AgentId) -> bool + Sync)) -> ProviderInfo {
    let id = AgentId::new(entry.id.clone());
    ProviderInfo {
        loaded: loaded(&id),
        id,
        label: entry.label.clone(),
        base_url: entry.base_url.clone(),
        model: entry.model.clone(),
        has_key: !entry.api_key.trim().is_empty(),
        kind: entry.kind.clone(),
    }
}

pub(crate) async fn list(
    config: &HubConfig,
    loaded: &(dyn Fn(&AgentId) -> bool + Sync),
) -> Result<Vec<ProviderInfo>, ApiError> {
    let file = load(&config.home).await?;
    Ok(file.providers.iter().map(|e| info(e, loaded)).collect())
}

pub(crate) async fn add(
    config: &HubConfig,
    request: AddProvider,
    loaded: &(dyn Fn(&AgentId) -> bool + Sync),
) -> Result<ProviderInfo, ApiError> {
    let _write = WRITE.lock().await;
    let label = clean_label(&request.label)?;
    let command_code = match request.kind.trim() {
        "" => false,
        KIND_COMMAND_CODE => true,
        other => {
            return Err(ApiError::InvalidArgument(format!(
                "unknown provider kind {other}"
            )));
        }
    };
    // A Command Code key has no endpoint or model of its own: the CLI knows both.
    let (base_url, model) = if command_code {
        if request.api_key.trim().is_empty() {
            return Err(ApiError::InvalidArgument(
                "the API key must not be empty".into(),
            ));
        }
        (String::new(), String::new())
    } else {
        (
            clean_base_url(&request.base_url)?,
            clean_model(&request.model)?,
        )
    };
    let mut file = load(&config.home).await?;
    let taken = |id: &str| {
        file.providers.iter().any(|e| e.id == id)
            || config.agents.iter().any(|a| a.id.as_str() == id)
    };
    let prefix = if command_code {
        COMMAND_CODE_PREFIX
    } else {
        ID_PREFIX
    };
    let stem = format!("{prefix}{}", slug(&label));
    let mut id = stem.clone();
    let mut n = 2;
    while taken(&id) {
        id = format!("{stem}-{n}");
        n += 1;
    }
    let entry = ProviderEntry {
        id,
        label,
        base_url,
        model,
        api_key: request.api_key.trim().to_owned(),
        kind: if command_code {
            KIND_COMMAND_CODE.into()
        } else {
            String::new()
        },
    };
    file.providers.push(entry.clone());
    save(&config.home, &file).await?;
    Ok(info(&entry, loaded))
}

pub(crate) async fn update(
    config: &HubConfig,
    request: UpdateProvider,
    loaded: &(dyn Fn(&AgentId) -> bool + Sync),
) -> Result<ProviderInfo, ApiError> {
    let _write = WRITE.lock().await;
    let mut file = load(&config.home).await?;
    let Some(entry) = file
        .providers
        .iter_mut()
        .find(|e| e.id == request.id.as_str())
    else {
        return Err(ApiError::NotFound(format!("provider {}", request.id)));
    };
    if let Some(label) = &request.label {
        entry.label = clean_label(label)?;
    }
    // A Command Code key keeps its empty endpoint and model.
    if let Some(url) = request
        .base_url
        .as_ref()
        .filter(|_| !entry.is_command_code())
    {
        entry.base_url = clean_base_url(url)?;
    }
    if let Some(model) = request.model.as_ref().filter(|_| !entry.is_command_code()) {
        entry.model = clean_model(model)?;
    }
    if let Some(key) = &request.api_key {
        entry.api_key = key.trim().to_owned();
    }
    let out = info(entry, loaded);
    save(&config.home, &file).await?;
    Ok(out)
}

pub(crate) async fn remove(config: &HubConfig, id: &AgentId) -> Result<(), ApiError> {
    let _write = WRITE.lock().await;
    let mut file = load(&config.home).await?;
    let before = file.providers.len();
    file.providers.retain(|e| e.id != id.as_str());
    if file.providers.len() == before {
        return Err(ApiError::NotFound(format!("provider {id}")));
    }
    save(&config.home, &file).await?;
    let dir = state_dir(&config.home, id.as_str());
    if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(%error, dir = %dir.display(), "provider state not removed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
