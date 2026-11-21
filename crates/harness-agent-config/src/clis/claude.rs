use super::{
    hooks, mcp, md_store, single, tag, tag_hooks, AgentKind, CliConfig, Command, CommandsConfig,
    Concept, ConfigError, ConfigPaths, Dialect, Hook, HooksConfig, InstructionsConfig, McpConfig,
    McpServer, PathBuf, Result, Scope, Skill, SkillsConfig, Subagent, SubagentsConfig, WritePlan,
};

// ================================================================================================ Claude Code

/// Claude Code (`~/.claude`, `~/.claude.json`, `<ws>/.claude`, `<ws>/.mcp.json`).
pub struct ClaudeCodeConfig<'a> {
    pub paths: &'a ConfigPaths,
}

impl ClaudeCodeConfig<'_> {
    pub(super) fn base(&self, scope: Scope) -> Result<PathBuf> {
        Ok(match scope {
            Scope::User => self.paths.claude_dir.clone(),
            Scope::Workspace => self.paths.workspace()?.join(".claude"),
        })
    }

    pub(super) fn mcp_file(&self, scope: Scope) -> Result<PathBuf> {
        Ok(match scope {
            Scope::User => self.paths.claude_json.clone(),
            Scope::Workspace => self.paths.workspace()?.join(".mcp.json"),
        })
    }

    pub(crate) fn settings_file(&self, scope: Scope) -> Result<PathBuf> {
        Ok(self.base(scope)?.join("settings.json"))
    }

    pub(crate) fn settings_local_file(&self) -> Result<PathBuf> {
        Ok(self
            .paths
            .workspace()?
            .join(".claude")
            .join("settings.local.json"))
    }

    pub(crate) fn dirs(&self, scope: Scope) -> Result<Vec<PathBuf>> {
        let base = self.base(scope)?;
        Ok(vec![
            base.join("skills"),
            base.join("commands"),
            base.join("agents"),
        ])
    }
}

impl CliConfig for ClaudeCodeConfig<'_> {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }
}

impl McpConfig for ClaudeCodeConfig<'_> {
    async fn list_mcp(&self, scope: Scope) -> Result<Vec<(McpServer, PathBuf)>> {
        let path = self.mcp_file(scope)?;
        let mut servers = mcp::list_json(&path, Dialect::Claude).await?;
        if scope == Scope::Workspace {
            let approved = mcp::claude_project_enabled(&self.settings_local_file()?).await?;
            for s in &mut servers {
                s.enabled = Some(approved(&s.name));
            }
        }
        Ok(tag(servers, &path))
    }

    async fn plan_mcp_upsert(
        &self,
        scope: Scope,
        server: &McpServer,
        previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        let mut plan = single(
            mcp::plan_upsert_json(
                &self.mcp_file(scope)?,
                Dialect::Claude,
                server,
                previous_name,
            )
            .await?,
        );
        if scope == Scope::Workspace {
            if let Some(enabled) = server.enabled {
                plan.changes.push(
                    mcp::plan_claude_project_enabled(
                        &self.settings_local_file()?,
                        &server.name,
                        enabled,
                    )
                    .await?,
                );
            }
        }
        Ok(plan)
    }

    async fn plan_mcp_delete(&self, scope: Scope, name: &str) -> Result<WritePlan> {
        Ok(single(
            mcp::plan_delete_json(&self.mcp_file(scope)?, name).await?,
        ))
    }

    async fn plan_mcp_enabled(&self, scope: Scope, name: &str, enabled: bool) -> Result<WritePlan> {
        if scope == Scope::User {
            return Err(ConfigError::unsupported(
                AgentKind::ClaudeCode,
                Concept::Mcp,
                scope,
                "Claude Code has no on/off switch for user-scope servers; delete the server instead",
            ));
        }
        if !self
            .list_mcp(scope)
            .await?
            .iter()
            .any(|(s, _)| s.name == name)
        {
            return Err(ConfigError::NotFound(name.to_owned()));
        }
        Ok(single(
            mcp::plan_claude_project_enabled(&self.settings_local_file()?, name, enabled).await?,
        ))
    }
}

impl SkillsConfig for ClaudeCodeConfig<'_> {
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

impl CommandsConfig for ClaudeCodeConfig<'_> {
    async fn list_commands(&self, scope: Scope) -> Result<Vec<(Command, PathBuf)>> {
        md_store::list_commands(&self.base(scope)?.join("commands"), true).await
    }
    async fn plan_command_upsert(
        &self,
        scope: Scope,
        command: &Command,
        previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        md_store::plan_command_upsert(
            &self.base(scope)?.join("commands"),
            true,
            command,
            previous_name,
        )
        .await
    }
    async fn plan_command_delete(&self, scope: Scope, name: &str) -> Result<WritePlan> {
        md_store::plan_command_delete(&self.base(scope)?.join("commands"), true, name).await
    }
}

impl HooksConfig for ClaudeCodeConfig<'_> {
    async fn list_hooks(&self, scope: Scope) -> Result<Vec<(Hook, PathBuf)>> {
        let path = self.settings_file(scope)?;
        Ok(tag_hooks(hooks::list(&path).await?, &path))
    }
    async fn plan_hook_upsert(
        &self,
        scope: Scope,
        hook: &Hook,
        previous_id: Option<&str>,
    ) -> Result<WritePlan> {
        Ok(single(
            hooks::plan_upsert(&self.settings_file(scope)?, hook, previous_id).await?,
        ))
    }
    async fn plan_hook_delete(&self, scope: Scope, id: &str) -> Result<WritePlan> {
        Ok(single(
            hooks::plan_delete(&self.settings_file(scope)?, id).await?,
        ))
    }
}

impl SubagentsConfig for ClaudeCodeConfig<'_> {
    async fn list_subagents(&self, scope: Scope) -> Result<Vec<(Subagent, PathBuf)>> {
        md_store::list_subagents(&self.base(scope)?.join("agents")).await
    }
    async fn plan_subagent_upsert(
        &self,
        scope: Scope,
        agent: &Subagent,
        previous_name: Option<&str>,
    ) -> Result<WritePlan> {
        md_store::plan_subagent_upsert(&self.base(scope)?.join("agents"), agent, previous_name)
            .await
    }
    async fn plan_subagent_delete(&self, scope: Scope, name: &str) -> Result<WritePlan> {
        md_store::plan_subagent_delete(&self.base(scope)?.join("agents"), name).await
    }
}

impl InstructionsConfig for ClaudeCodeConfig<'_> {
    fn instruction_paths(&self, scope: Scope) -> Result<Vec<PathBuf>> {
        Ok(match scope {
            Scope::User => vec![self.paths.claude_dir.join("CLAUDE.md")],
            Scope::Workspace => {
                let ws = self.paths.workspace()?;
                vec![
                    ws.join("CLAUDE.md"),
                    ws.join(".claude").join("CLAUDE.md"),
                    ws.join("CLAUDE.local.md"),
                ]
            }
        })
    }
}
