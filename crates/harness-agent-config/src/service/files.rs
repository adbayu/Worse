use super::{
    fsx, plan, skill_file_copies, AgentConfigService, AgentKind, AgyConfig, ApplyOutcome,
    ClaudeCodeConfig, CodexConfig, Concept, ConfigChanged, ConfigError, ConfigTarget, EntryBody,
    FileChange, HarnessOverlayConfig, InstructionsConfig, Kind, Path, PathBuf, Result, Scope,
    WritePlan,
};

impl AgentConfigService {
    /// Plan to replace a whole config file with `text` (raw editor). Only files that hold supported concepts are
    /// accepted, JSON/TOML must parse, credentials files are refused.
    ///
    /// # Errors
    ///
    /// Not a known config file, invalid text, I/O.
    pub async fn plan_raw(&self, path: &Path, text: &str) -> Result<WritePlan> {
        if fsx::is_forbidden(path) || !self.is_config_path(path) {
            return Err(ConfigError::Forbidden(format!(
                "{} is not an editable config file",
                path.display()
            )));
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let kind = match ext.as_str() {
            "json" => {
                serde_json::from_str::<serde_json::Value>(text)
                    .map_err(|e| ConfigError::invalid(e.to_string()))?;
                Kind::Json
            }
            "toml" => {
                text.parse::<toml_edit::DocumentMut>()
                    .map_err(|e| ConfigError::invalid(e.to_string()))?;
                Kind::Toml
            }
            _ => Kind::Text,
        };
        let before = fsx::read_to_string_opt(path).await?;
        Ok(WritePlan::single(FileChange::write(
            path.to_path_buf(),
            before,
            text.to_owned(),
            kind,
        )))
    }

    pub(super) fn is_config_path(&self, path: &Path) -> bool {
        let p = &self.paths;
        let scopes: &[Scope] = if p.workspace.is_some() {
            &[Scope::User, Scope::Workspace]
        } else {
            &[Scope::User]
        };
        let mut files: Vec<PathBuf> = vec![
            p.harness_dir.join(crate::overlay::OVERLAY_FILE),
            p.harness_dir.join(crate::launch::AGENTS_FILE),
        ];
        let mut dirs: Vec<PathBuf> = Vec::new();
        for &scope in scopes {
            let claude = ClaudeCodeConfig { paths: p };
            let codex = CodexConfig { paths: p };
            let agy = AgyConfig { paths: p };
            files.extend(claude.settings_file(scope).ok());
            files.extend(codex.config_file(scope).ok());
            files.extend(agy.mcp_file(scope).ok());
            for kind in AgentKind::CLIS {
                files.extend(
                    with_cli!(p, kind, c => c.instruction_paths(scope)).unwrap_or_default(),
                );
            }
            dirs.extend(claude.dirs(scope).unwrap_or_default());
            dirs.extend(codex.dirs(scope).unwrap_or_default());
        }
        files.push(p.claude_json.clone());
        if let Some(ws) = &p.workspace {
            files.push(ws.join(".mcp.json"));
            files.push(ws.join(".claude").join("settings.local.json"));
        }
        let is_md = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"));
        let no_parent_refs = path
            .components()
            .all(|c| !matches!(c, std::path::Component::ParentDir));
        files.iter().any(|f| f == path)
            || (is_md && no_parent_refs && dirs.iter().any(|d| path.starts_with(d)))
    }

    /// Plan to copy entry `name` of `concept` from one target to another (MCP-004, SKL-003, COM-002).
    ///
    /// # Errors
    ///
    /// Unsupported on either side, not found, invalid, I/O.
    pub async fn plan_copy(
        &self,
        concept: Concept,
        from: ConfigTarget,
        to: ConfigTarget,
        name: &str,
    ) -> Result<WritePlan> {
        let entry = self
            .list(concept, from)
            .await?
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| ConfigError::NotFound(name.to_owned()))?;
        let body = match entry.body {
            EntryBody::Mcp(mut s) => {
                // 目标 CLI 的开关语义不同：复制时只保留“启用”，避免把来源 CLI 的禁用状态误写成另一种格式。
                s.enabled = None;
                if to.agent != AgentKind::Harness {
                    s.agents.clear();
                }
                s.startup_timeout_sec = s
                    .startup_timeout_sec
                    .filter(|_| to.agent == AgentKind::Codex);
                s.tool_timeout_sec = s.tool_timeout_sec.filter(|_| to.agent == AgentKind::Codex);
                s.extra.clear();
                EntryBody::Mcp(s)
            }
            EntryBody::Hook(mut h) => {
                h.id.clear();
                EntryBody::Hook(h)
            }
            other => other,
        };
        let is_skill = matches!(body, EntryBody::Skill(_));
        let mut plan = self.plan_upsert(to, &body, None).await?;
        if is_skill {
            let source_dir = entry.source_path.parent().map(Path::to_path_buf);
            let target_dir = plan
                .changes
                .first()
                .and_then(|c| c.path.parent())
                .map(Path::to_path_buf);
            if let (Some(source_dir), Some(target_dir)) = (source_dir, target_dir) {
                plan.changes
                    .extend(skill_file_copies(&source_dir, &target_dir).await?);
            }
        }
        Ok(plan)
    }

    /// Applies a plan (conflict check, atomic writes) and broadcasts [`ConfigChanged`].
    ///
    /// # Errors
    ///
    /// Conflict or I/O.
    pub async fn apply(&self, plan: &WritePlan) -> Result<ApplyOutcome> {
        let outcome = plan::apply(plan).await?;
        if !outcome.written.is_empty() {
            tracing::info!(files = outcome.written.len(), "agent config written");
            let _ = self.events.send(ConfigChanged {
                paths: outcome.written.clone(),
            });
        }
        Ok(outcome)
    }
}
