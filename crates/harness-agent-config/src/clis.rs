//! One trait per concept, implemented per CLI. A concept or scope a CLI does not have returns
//! [`ConfigError::Unsupported`] (the trait default).

#![allow(async_fn_in_trait)]

use std::path::{Path, PathBuf};

use crate::error::{ConfigError, Result};
use crate::hooks;
use crate::mcp::{self, Dialect};
use crate::md_store;
use crate::paths::ConfigPaths;
use crate::plan::WritePlan;
use crate::types::{
    AgentKind, Command, Concept, ConceptSupport, Hook, InstructionFile, McpServer, McpTransport,
    Scope, Skill, Subagent,
};

mod agy;
mod claude;
mod codex;
mod overlay;
mod support;

pub use agy::AgyConfig;
pub use claude::ClaudeCodeConfig;
pub use codex::CodexConfig;
pub use overlay::HarnessOverlayConfig;
pub use support::support_of;

/// Common base of the concept traits.
pub trait CliConfig {
    fn kind(&self) -> AgentKind;

    /// Error for a concept/scope this CLI lacks.
    fn unsupported(&self, concept: Concept, scope: Scope) -> ConfigError {
        let reason = support_of(self.kind(), concept)
            .reason
            .unwrap_or_else(|| "not available for this scope".into());
        ConfigError::unsupported(self.kind(), concept, scope, reason)
    }
}

/// MCP servers.
pub trait McpConfig: CliConfig {
    async fn list_mcp(&self, scope: Scope) -> Result<Vec<(McpServer, PathBuf)>>;
    async fn plan_mcp_upsert(
        &self,
        scope: Scope,
        server: &McpServer,
        previous_name: Option<&str>,
    ) -> Result<WritePlan>;
    async fn plan_mcp_delete(&self, scope: Scope, name: &str) -> Result<WritePlan>;
    async fn plan_mcp_enabled(&self, scope: Scope, name: &str, enabled: bool) -> Result<WritePlan>;
}

/// Skills.
pub trait SkillsConfig: CliConfig {
    async fn list_skills(&self, scope: Scope) -> Result<Vec<(Skill, PathBuf)>> {
        Err(self.unsupported(Concept::Skill, scope))
    }
    async fn plan_skill_upsert(
        &self,
        scope: Scope,
        _skill: &Skill,
        _previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        Err(self.unsupported(Concept::Skill, scope))
    }
    async fn plan_skill_delete(&self, scope: Scope, _name: &str) -> Result<WritePlan> {
        Err(self.unsupported(Concept::Skill, scope))
    }
}

/// Slash commands / custom prompts.
pub trait CommandsConfig: CliConfig {
    async fn list_commands(&self, scope: Scope) -> Result<Vec<(Command, PathBuf)>> {
        Err(self.unsupported(Concept::Command, scope))
    }
    async fn plan_command_upsert(
        &self,
        scope: Scope,
        _command: &Command,
        _previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        Err(self.unsupported(Concept::Command, scope))
    }
    async fn plan_command_delete(&self, scope: Scope, _name: &str) -> Result<WritePlan> {
        Err(self.unsupported(Concept::Command, scope))
    }
}

/// Hooks.
pub trait HooksConfig: CliConfig {
    async fn list_hooks(&self, scope: Scope) -> Result<Vec<(Hook, PathBuf)>> {
        Err(self.unsupported(Concept::Hook, scope))
    }
    async fn plan_hook_upsert(
        &self,
        scope: Scope,
        _hook: &Hook,
        _previous_id: Option<&str>,
    ) -> Result<WritePlan> {
        Err(self.unsupported(Concept::Hook, scope))
    }
    async fn plan_hook_delete(&self, scope: Scope, _id: &str) -> Result<WritePlan> {
        Err(self.unsupported(Concept::Hook, scope))
    }
}

/// Subagents.
pub trait SubagentsConfig: CliConfig {
    async fn list_subagents(&self, scope: Scope) -> Result<Vec<(Subagent, PathBuf)>> {
        Err(self.unsupported(Concept::Subagent, scope))
    }
    async fn plan_subagent_upsert(
        &self,
        scope: Scope,
        _agent: &Subagent,
        _previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        Err(self.unsupported(Concept::Subagent, scope))
    }
    async fn plan_subagent_delete(&self, scope: Scope, _name: &str) -> Result<WritePlan> {
        Err(self.unsupported(Concept::Subagent, scope))
    }
}

/// Instructions files (CLAUDE.md / AGENTS.md / GEMINI.md): the fixed candidate files the user picks from.
pub trait InstructionsConfig: CliConfig {
    fn instruction_paths(&self, scope: Scope) -> Result<Vec<PathBuf>> {
        Err(self.unsupported(Concept::Instructions, scope))
    }
}

/// Lists the instruction candidates with existence and size (metadata only).
pub async fn instruction_files(
    cli: &impl InstructionsConfig,
    scope: Scope,
) -> Result<Vec<InstructionFile>> {
    let mut out = Vec::new();
    for path in cli.instruction_paths(scope)? {
        let meta = tokio::fs::metadata(&path).await.ok();
        out.push(InstructionFile {
            exists: meta.as_ref().is_some_and(std::fs::Metadata::is_file),
            size: meta.map_or(0, |m| m.len()),
            path,
            scope,
        });
    }
    Ok(out)
}

fn single(change: crate::plan::FileChange) -> WritePlan {
    WritePlan::single(change)
}

fn tag(items: Vec<McpServer>, path: &Path) -> Vec<(McpServer, PathBuf)> {
    items.into_iter().map(|s| (s, path.to_path_buf())).collect()
}

fn tag_hooks(items: Vec<Hook>, path: &Path) -> Vec<(Hook, PathBuf)> {
    items.into_iter().map(|h| (h, path.to_path_buf())).collect()
}
