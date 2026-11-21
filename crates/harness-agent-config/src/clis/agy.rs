use super::{
    mcp, single, tag, AgentKind, CliConfig, CommandsConfig, ConfigError, ConfigPaths, Dialect,
    HooksConfig, InstructionsConfig, McpConfig, McpServer, McpTransport, PathBuf, Result, Scope,
    SkillsConfig, SubagentsConfig, WritePlan,
};

// ================================================================================================ agy

/// agy / Antigravity (`~/.gemini`, `<ws>/.agents`).
pub struct AgyConfig<'a> {
    pub paths: &'a ConfigPaths,
}

impl AgyConfig<'_> {
    pub(crate) fn mcp_file(&self, scope: Scope) -> Result<PathBuf> {
        Ok(match scope {
            Scope::User => self.paths.gemini_dir.join("config").join("mcp_config.json"),
            Scope::Workspace => self
                .paths
                .workspace()?
                .join(".agents")
                .join("mcp_config.json"),
        })
    }
}

impl CliConfig for AgyConfig<'_> {
    fn kind(&self) -> AgentKind {
        AgentKind::Agy
    }
}

impl McpConfig for AgyConfig<'_> {
    async fn list_mcp(&self, scope: Scope) -> Result<Vec<(McpServer, PathBuf)>> {
        let path = self.mcp_file(scope)?;
        Ok(tag(mcp::list_json(&path, Dialect::Agy).await?, &path))
    }
    async fn plan_mcp_upsert(
        &self,
        scope: Scope,
        server: &McpServer,
        previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        if server.transport == McpTransport::Sse {
            return Err(ConfigError::invalid("agy servers are stdio or http"));
        }
        Ok(single(
            mcp::plan_upsert_json(&self.mcp_file(scope)?, Dialect::Agy, server, previous_name)
                .await?,
        ))
    }
    async fn plan_mcp_delete(&self, scope: Scope, name: &str) -> Result<WritePlan> {
        Ok(single(
            mcp::plan_delete_json(&self.mcp_file(scope)?, name).await?,
        ))
    }
    async fn plan_mcp_enabled(&self, scope: Scope, name: &str, enabled: bool) -> Result<WritePlan> {
        Ok(single(
            mcp::plan_enabled_json(&self.mcp_file(scope)?, Dialect::Agy, name, enabled).await?,
        ))
    }
}

impl SkillsConfig for AgyConfig<'_> {}
impl CommandsConfig for AgyConfig<'_> {}
impl HooksConfig for AgyConfig<'_> {}
impl SubagentsConfig for AgyConfig<'_> {}

impl InstructionsConfig for AgyConfig<'_> {
    fn instruction_paths(&self, scope: Scope) -> Result<Vec<PathBuf>> {
        Ok(match scope {
            Scope::User => vec![self.paths.gemini_dir.join("GEMINI.md")],
            Scope::Workspace => {
                let ws = self.paths.workspace()?;
                vec![ws.join("GEMINI.md"), ws.join("AGENTS.md")]
            }
        })
    }
}
