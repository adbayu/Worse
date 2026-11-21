use harness_core_api::AgentId;
use serde::Deserialize;

use super::{HubConfig, AGENTS_FILE};
use crate::agents::{AgentSpec, ErrorDialect, Transport};
use crate::error::HubError;

impl HubConfig {
    /// Applies `<home>/agents.json` when it exists (spec §15.1 request 1): an entry with a known id replaces
    /// that agent in place, a new id is appended to the agents and to the default fallback order;
    /// `replaceBuiltins: true` drops the previous list first.
    ///
    /// # Errors
    ///
    /// Unreadable or invalid file (the hub must not start with a half-read configuration).
    pub async fn apply_agents_file(&mut self) -> Result<(), HubError> {
        let path = self.agents_file();
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(HubError::Config(format!("{}: {e}", path.display()))),
        };
        let file: AgentsFile = serde_json::from_slice(&bytes)
            .map_err(|e| HubError::Config(format!("{}: {e}", path.display())))?;
        let (replace, entries) = match file {
            AgentsFile::List(entries) => (false, entries),
            AgentsFile::Object {
                replace_builtins,
                agents,
            } => (replace_builtins, agents),
        };
        if replace {
            self.agents.clear();
            self.default_fallback_order.clear();
        }
        for entry in entries {
            let spec = entry.into_spec(&path)?;
            if let Some(slot) = self.agents.iter_mut().find(|a| a.id == spec.id) {
                *slot = spec;
            } else {
                self.default_fallback_order.push(spec.id.clone());
                self.agents.push(spec);
            }
        }
        if self.agents.is_empty() {
            return Err(HubError::Config(format!("{}: no agents", path.display())));
        }
        Ok(())
    }
}

/// `agents.json`: a plain list or `{ replaceBuiltins, agents }`.
#[derive(Deserialize)]
#[serde(untagged)]
enum AgentsFile {
    List(Vec<AgentEntry>),
    #[serde(rename_all = "camelCase")]
    Object {
        #[serde(default)]
        replace_builtins: bool,
        agents: Vec<AgentEntry>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentEntry {
    id: String,
    display_name: Option<String>,
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
    install_hint: Option<String>,
    dialect: Option<String>,
}

impl AgentEntry {
    fn into_spec(self, file: &std::path::Path) -> Result<AgentSpec, HubError> {
        let id = self.id.trim().to_owned();
        if id.is_empty() || self.program.trim().is_empty() {
            return Err(HubError::Config(format!(
                "{}: every agent needs a non-empty id and program",
                file.display()
            )));
        }
        let dialect = match self.dialect.as_deref() {
            None | Some("generic") => ErrorDialect::Generic,
            Some("claudeCode") => ErrorDialect::ClaudeCode,
            Some("codex") => ErrorDialect::Codex,
            Some("agy") => ErrorDialect::Agy,
            Some(other) => {
                return Err(HubError::Config(format!(
                    "{}: unknown dialect {other} for agent {id}",
                    file.display()
                )))
            }
        };
        Ok(AgentSpec {
            display_name: self.display_name.unwrap_or_else(|| id.clone()),
            id: AgentId::new(id),
            program: self.program,
            args: self.args,
            env: self.env.into_iter().collect(),
            install_hint: self
                .install_hint
                .unwrap_or_else(|| format!("configured in {AGENTS_FILE}")),
            transport: Transport::Acp,
            dialect,
        })
    }
}
