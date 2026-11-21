use super::{
    mcp, md_store, single, tag, AgentKind, CliConfig, CommandsConfig, Concept, ConfigError,
    ConfigPaths, Dialect, HooksConfig, InstructionsConfig, McpConfig, McpServer, PathBuf, Result,
    Scope, Skill, SkillsConfig, SubagentsConfig, WritePlan,
};

// ================================================================================================ Harness overlay

/// Harness MCP overlay (`<data dir>/mcp.json`), user scope only.
pub struct HarnessOverlayConfig<'a> {
    pub paths: &'a ConfigPaths,
}

impl HarnessOverlayConfig<'_> {
    pub(crate) fn file(&self, scope: Scope) -> Result<PathBuf> {
        match scope {
            Scope::User => Ok(self.paths.harness_dir.join(crate::overlay::OVERLAY_FILE)),
            Scope::Workspace => Err(self.unsupported(Concept::Mcp, scope)),
        }
    }
}

impl CliConfig for HarnessOverlayConfig<'_> {
    fn kind(&self) -> AgentKind {
        AgentKind::Harness
    }
}

impl McpConfig for HarnessOverlayConfig<'_> {
    async fn list_mcp(&self, scope: Scope) -> Result<Vec<(McpServer, PathBuf)>> {
        let path = self.file(scope)?;
        Ok(tag(mcp::list_json(&path, Dialect::Overlay).await?, &path))
    }
    async fn plan_mcp_upsert(
        &self,
        scope: Scope,
        server: &McpServer,
        previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        if crate::overlay::is_builtin(&server.name) {
            return Err(ConfigError::invalid(format!("{} is reserved", server.name)));
        }
        Ok(single(
            mcp::plan_upsert_json(&self.file(scope)?, Dialect::Overlay, server, previous_name)
                .await?,
        ))
    }
    async fn plan_mcp_delete(&self, scope: Scope, name: &str) -> Result<WritePlan> {
        Ok(single(
            mcp::plan_delete_json(&self.file(scope)?, name).await?,
        ))
    }
    async fn plan_mcp_enabled(&self, scope: Scope, name: &str, enabled: bool) -> Result<WritePlan> {
        Ok(single(
            mcp::plan_enabled_json(&self.file(scope)?, Dialect::Overlay, name, enabled).await?,
        ))
    }
}

impl HarnessOverlayConfig<'_> {
    pub(super) fn skills_dir(&self, scope: Scope) -> Result<PathBuf> {
        match scope {
            Scope::User => Ok(self.paths.harness_dir.join("skills")),
            Scope::Workspace => Err(self.unsupported(Concept::Skill, scope)),
        }
    }
}

impl SkillsConfig for HarnessOverlayConfig<'_> {
    async fn list_skills(&self, scope: Scope) -> Result<Vec<(Skill, PathBuf)>> {
        md_store::list_skills(&self.skills_dir(scope)?).await
    }
    async fn plan_skill_upsert(
        &self,
        scope: Scope,
        skill: &Skill,
        previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        md_store::plan_skill_upsert(&self.skills_dir(scope)?, skill, previous_name).await
    }
    async fn plan_skill_delete(&self, scope: Scope, name: &str) -> Result<WritePlan> {
        md_store::plan_skill_delete(&self.skills_dir(scope)?, name).await
    }
}

impl CommandsConfig for HarnessOverlayConfig<'_> {}
impl HooksConfig for HarnessOverlayConfig<'_> {}
impl SubagentsConfig for HarnessOverlayConfig<'_> {}
impl InstructionsConfig for HarnessOverlayConfig<'_> {}
