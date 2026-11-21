use super::{
    mcp, md_store, single, tag, AgentKind, CliConfig, Command, CommandsConfig, Concept,
    ConfigError, ConfigPaths, HooksConfig, InstructionsConfig, McpConfig, McpServer, PathBuf,
    Result, Scope, Skill, SkillsConfig, SubagentsConfig, WritePlan,
};

// ================================================================================================ Codex

/// Codex (`$CODEX_HOME`, `<ws>/.codex`).
pub struct CodexConfig<'a> {
    pub paths: &'a ConfigPaths,
}

impl CodexConfig<'_> {
    pub(super) fn base(&self, scope: Scope) -> Result<PathBuf> {
        Ok(match scope {
            Scope::User => self.paths.codex_home.clone(),
            Scope::Workspace => self.paths.workspace()?.join(".codex"),
        })
    }

    pub(crate) fn config_file(&self, scope: Scope) -> Result<PathBuf> {
        Ok(self.base(scope)?.join("config.toml"))
    }

    pub(super) fn prompts_dir(&self, scope: Scope) -> Result<PathBuf> {
        match scope {
            Scope::User => Ok(self.paths.codex_home.join("prompts")),
            Scope::Workspace => Err(self.unsupported(Concept::Command, scope)),
        }
    }

    pub(crate) fn dirs(&self, scope: Scope) -> Result<Vec<PathBuf>> {
        let base = self.base(scope)?;
        let mut dirs = vec![base.join("skills")];
        if scope == Scope::User {
            dirs.push(base.join("prompts"));
        }
        Ok(dirs)
    }
}

impl CliConfig for CodexConfig<'_> {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }
}

impl McpConfig for CodexConfig<'_> {
    async fn list_mcp(&self, scope: Scope) -> Result<Vec<(McpServer, PathBuf)>> {
        let path = self.config_file(scope)?;
        Ok(tag(mcp::list_toml(&path).await?, &path))
    }
    async fn plan_mcp_upsert(
        &self,
        scope: Scope,
        server: &McpServer,
        previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        Ok(single(
            mcp::plan_upsert_toml(&self.config_file(scope)?, server, previous_name).await?,
        ))
    }
    async fn plan_mcp_delete(&self, scope: Scope, name: &str) -> Result<WritePlan> {
        Ok(single(
            mcp::plan_delete_toml(&self.config_file(scope)?, name).await?,
        ))
    }
    async fn plan_mcp_enabled(&self, scope: Scope, name: &str, enabled: bool) -> Result<WritePlan> {
        Ok(single(
            mcp::plan_enabled_toml(&self.config_file(scope)?, name, enabled).await?,
        ))
    }
}

impl SkillsConfig for CodexConfig<'_> {
    async fn list_skills(&self, scope: Scope) -> Result<Vec<(Skill, PathBuf)>> {
        md_store::list_skills(&self.base(scope)?.join("skills")).await
    }
    async fn plan_skill_upsert(
        &self,
        scope: Scope,
        skill: &Skill,
        previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        md_store::plan_skill_upsert(&self.base(scope)?.join("skills"), skill, previous_name).await
    }
    async fn plan_skill_delete(&self, scope: Scope, name: &str) -> Result<WritePlan> {
        md_store::plan_skill_delete(&self.base(scope)?.join("skills"), name).await
    }
}

impl CommandsConfig for CodexConfig<'_> {
    async fn list_commands(&self, scope: Scope) -> Result<Vec<(Command, PathBuf)>> {
        md_store::list_commands(&self.prompts_dir(scope)?, false).await
    }
    async fn plan_command_upsert(
        &self,
        scope: Scope,
        command: &Command,
        previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        if command.model.is_some() || command.allowed_tools.is_some() {
            return Err(ConfigError::invalid(
                "Codex prompts have no model or allowed-tools fields",
            ));
        }
        md_store::plan_command_upsert(&self.prompts_dir(scope)?, false, command, previous_name)
            .await
    }
    async fn plan_command_delete(&self, scope: Scope, name: &str) -> Result<WritePlan> {
        md_store::plan_command_delete(&self.prompts_dir(scope)?, false, name).await
    }
}

impl HooksConfig for CodexConfig<'_> {}
impl SubagentsConfig for CodexConfig<'_> {}

impl InstructionsConfig for CodexConfig<'_> {
    fn instruction_paths(&self, scope: Scope) -> Result<Vec<PathBuf>> {
        Ok(match scope {
            Scope::User => vec![self.paths.codex_home.join("AGENTS.md")],
            Scope::Workspace => vec![self.paths.workspace()?.join("AGENTS.md")],
        })
    }
}
