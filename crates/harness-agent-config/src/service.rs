//! `AgentConfigService`: the generic facade the settings pages use (list → plan → preview → apply).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_settings::fsx;
use tokio::sync::broadcast;

use crate::auth::AuthProbe;
use crate::clis::{
    instruction_files, support_of, AgyConfig, ClaudeCodeConfig, CodexConfig, CommandsConfig,
    HarnessOverlayConfig, HooksConfig, InstructionsConfig, McpConfig, SkillsConfig,
    SubagentsConfig,
};
use crate::error::{ConfigError, Result};
use crate::paths::ConfigPaths;
use crate::plan::{self, ApplyOutcome, FileChange, Kind, WritePlan};
use crate::types::{
    AgentKind, AuthState, Concept, ConceptSupport, ConfigChanged, ConfigTarget, Entry, EntryBody,
    McpServer, Scope,
};

mod copy;

use copy::{mcp_summary, skill_file_copies};

/// Runs `$body` with `$c` bound to the concrete CLI config of `$agent`.
macro_rules! with_cli {
    ($paths:expr, $agent:expr, $c:ident => $body:expr) => {
        match $agent {
            AgentKind::ClaudeCode => {
                let $c = ClaudeCodeConfig { paths: $paths };
                $body
            }
            AgentKind::Codex => {
                let $c = CodexConfig { paths: $paths };
                $body
            }
            AgentKind::Agy => {
                let $c = AgyConfig { paths: $paths };
                $body
            }
            AgentKind::Harness => {
                let $c = HarnessOverlayConfig { paths: $paths };
                $body
            }
        }
    };
}

mod files;

/// Facade over every CLI and concept.
pub struct AgentConfigService {
    paths: ConfigPaths,
    probe: Arc<dyn AuthProbe>,
    events: broadcast::Sender<ConfigChanged>,
}

impl AgentConfigService {
    #[must_use]
    pub fn new(paths: ConfigPaths, probe: Arc<dyn AuthProbe>) -> Self {
        let (events, _) = broadcast::channel(16);
        Self {
            paths,
            probe,
            events,
        }
    }

    #[must_use]
    pub fn paths(&self) -> &ConfigPaths {
        &self.paths
    }

    /// Switches the workspace used for `Scope::Workspace`.
    pub fn set_workspace(&mut self, workspace: Option<PathBuf>) {
        self.paths.workspace = workspace;
    }

    /// Notifications after every successful [`AgentConfigService::apply`].
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ConfigChanged> {
        self.events.subscribe()
    }

    /// Support table for `agent` (one row per concept).
    #[must_use]
    pub fn capabilities(&self, agent: AgentKind) -> Vec<ConceptSupport> {
        Concept::ALL.iter().map(|c| support_of(agent, *c)).collect()
    }

    fn check_support(target: ConfigTarget, concept: Concept) -> Result<()> {
        let s = support_of(target.agent, concept);
        let ok = match target.scope {
            Scope::User => s.user,
            Scope::Workspace => s.workspace,
        };
        if ok {
            Ok(())
        } else {
            Err(ConfigError::unsupported(
                target.agent,
                concept,
                target.scope,
                s.reason
                    .unwrap_or_else(|| "not available for this scope".into()),
            ))
        }
    }

    /// Login presence of `agent` (never reads credentials).
    pub async fn auth_state(&self, agent: AgentKind) -> AuthState {
        self.probe.probe(agent, &self.paths).await
    }

    /// Entries of `concept` for `target`.
    ///
    /// # Errors
    ///
    /// Unsupported, invalid file, I/O.
    pub async fn list(&self, concept: Concept, target: ConfigTarget) -> Result<Vec<Entry>> {
        Self::check_support(target, concept)?;
        let scope = target.scope;
        let p = &self.paths;
        Ok(match concept {
            Concept::Mcp => with_cli!(p, target.agent, c => c.list_mcp(scope).await?)
                .into_iter()
                .map(|(s, path)| Entry {
                    name: s.name.clone(),
                    scope,
                    enabled: s.enabled,
                    summary: mcp_summary(&s),
                    source_path: path,
                    body: EntryBody::Mcp(s),
                })
                .collect(),
            Concept::Skill => with_cli!(p, target.agent, c => c.list_skills(scope).await?)
                .into_iter()
                .map(|(s, path)| Entry {
                    name: s.name.clone(),
                    scope,
                    enabled: None,
                    summary: s.description.clone().unwrap_or_default(),
                    source_path: path,
                    body: EntryBody::Skill(s),
                })
                .collect(),
            Concept::Command => with_cli!(p, target.agent, c => c.list_commands(scope).await?)
                .into_iter()
                .map(|(s, path)| Entry {
                    name: s.name.clone(),
                    scope,
                    enabled: None,
                    summary: s.description.clone().unwrap_or_default(),
                    source_path: path,
                    body: EntryBody::Command(s),
                })
                .collect(),
            Concept::Hook => with_cli!(p, target.agent, c => c.list_hooks(scope).await?)
                .into_iter()
                .map(|(h, path)| Entry {
                    name: h.id.clone(),
                    scope,
                    enabled: None,
                    summary: format!(
                        "{} {}: {}",
                        h.event,
                        h.matcher.clone().unwrap_or_default(),
                        h.command
                    ),
                    source_path: path,
                    body: EntryBody::Hook(h),
                })
                .collect(),
            Concept::Subagent => with_cli!(p, target.agent, c => c.list_subagents(scope).await?)
                .into_iter()
                .map(|(s, path)| Entry {
                    name: s.name.clone(),
                    scope,
                    enabled: None,
                    summary: s.description.clone().unwrap_or_default(),
                    source_path: path,
                    body: EntryBody::Subagent(s),
                })
                .collect(),
            Concept::Instructions => {
                let files = with_cli!(p, target.agent, c => instruction_files(&c, scope).await?);
                files
                    .into_iter()
                    .map(|f| Entry {
                        name: f
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        scope,
                        enabled: None,
                        summary: f.path.display().to_string(),
                        source_path: f.path.clone(),
                        body: EntryBody::Instructions {
                            file: f,
                            text: None,
                        },
                    })
                    .collect()
            }
        })
    }

    /// Plan to create or update `body` (renaming from `previous_name`; for hooks the previous id).
    ///
    /// # Errors
    ///
    /// Unsupported, invalid input or file, I/O.
    pub async fn plan_upsert(
        &self,
        target: ConfigTarget,
        body: &EntryBody,
        previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        Self::check_support(target, body.concept())?;
        let scope = target.scope;
        let p = &self.paths;
        match body {
            EntryBody::Mcp(s) => {
                with_cli!(p, target.agent, c => c.plan_mcp_upsert(scope, s, previous_name).await)
            }
            EntryBody::Skill(s) => {
                with_cli!(p, target.agent, c => c.plan_skill_upsert(scope, s, previous_name).await)
            }
            EntryBody::Command(s) => {
                with_cli!(p, target.agent, c => c.plan_command_upsert(scope, s, previous_name).await)
            }
            EntryBody::Hook(h) => {
                with_cli!(p, target.agent, c => c.plan_hook_upsert(scope, h, previous_name).await)
            }
            EntryBody::Subagent(s) => {
                with_cli!(p, target.agent, c => c.plan_subagent_upsert(scope, s, previous_name).await)
            }
            EntryBody::Instructions { file, text } => {
                self.plan_instructions(target, &file.path, text.as_deref().unwrap_or_default())
                    .await
            }
        }
    }

    /// Plan to delete entry `name` (hook id for hooks).
    ///
    /// # Errors
    ///
    /// Unsupported, not found, invalid file, I/O.
    pub async fn plan_delete(
        &self,
        concept: Concept,
        target: ConfigTarget,
        name: &str,
    ) -> Result<WritePlan> {
        Self::check_support(target, concept)?;
        let scope = target.scope;
        let p = &self.paths;
        match concept {
            Concept::Mcp => with_cli!(p, target.agent, c => c.plan_mcp_delete(scope, name).await),
            Concept::Skill => {
                with_cli!(p, target.agent, c => c.plan_skill_delete(scope, name).await)
            }
            Concept::Command => {
                with_cli!(p, target.agent, c => c.plan_command_delete(scope, name).await)
            }
            Concept::Hook => with_cli!(p, target.agent, c => c.plan_hook_delete(scope, name).await),
            Concept::Subagent => {
                with_cli!(p, target.agent, c => c.plan_subagent_delete(scope, name).await)
            }
            Concept::Instructions => Err(ConfigError::unsupported(
                target.agent,
                concept,
                scope,
                "Harness does not delete instruction files",
            )),
        }
    }

    /// Plan to enable/disable entry `name` (MCP only).
    ///
    /// # Errors
    ///
    /// Unsupported, not found, invalid file, I/O.
    pub async fn plan_set_enabled(
        &self,
        concept: Concept,
        target: ConfigTarget,
        name: &str,
        enabled: bool,
    ) -> Result<WritePlan> {
        Self::check_support(target, concept)?;
        if concept != Concept::Mcp {
            return Err(ConfigError::unsupported(
                target.agent,
                concept,
                target.scope,
                "only MCP servers can be switched off",
            ));
        }
        let p = &self.paths;
        with_cli!(p, target.agent, c => c.plan_mcp_enabled(target.scope, name, enabled).await)
    }

    /// Text of an instructions file (must be one of the candidates of `target`).
    ///
    /// # Errors
    ///
    /// Not a candidate, I/O.
    pub async fn read_instructions(
        &self,
        target: ConfigTarget,
        path: &Path,
    ) -> Result<Option<String>> {
        self.check_instruction_path(target, path)?;
        Ok(fsx::read_to_string_opt(path).await?)
    }

    /// Plan to write an instructions file the user picked.
    ///
    /// # Errors
    ///
    /// Not a candidate, I/O.
    pub async fn plan_instructions(
        &self,
        target: ConfigTarget,
        path: &Path,
        text: &str,
    ) -> Result<WritePlan> {
        self.check_instruction_path(target, path)?;
        let before = fsx::read_to_string_opt(path).await?;
        Ok(WritePlan::single(FileChange::write(
            path.to_path_buf(),
            before,
            text.to_owned(),
            Kind::Text,
        )))
    }

    fn check_instruction_path(&self, target: ConfigTarget, path: &Path) -> Result<()> {
        Self::check_support(target, Concept::Instructions)?;
        let p = &self.paths;
        let allowed = with_cli!(p, target.agent, c => c.instruction_paths(target.scope)?);
        if allowed.iter().any(|a| a == path) {
            Ok(())
        } else {
            Err(ConfigError::Forbidden(format!(
                "{} is not an instructions file of this CLI",
                path.display()
            )))
        }
    }
}

#[cfg(test)]
mod tests;
