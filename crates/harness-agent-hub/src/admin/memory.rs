use super::{
    fingerprint, ApiError, ApiResult, EventKind, Hub, ListProjects, MemoryDoc, MemoryFileInfo,
    MemoryPlan, MemoryScope,
};

impl Hub {
    pub(super) fn memory_path(&self, scope: &MemoryScope) -> String {
        self.ctx
            .memory
            .path(&Self::memory_target(scope))
            .to_string_lossy()
            .into_owned()
    }

    pub(super) async fn memory_read_opt(&self, scope: &MemoryScope) -> ApiResult<Option<String>> {
        let memory = self.ctx.memory.clone();
        let target = Self::memory_target(scope);
        tokio::task::spawn_blocking(move || memory.read_opt(&target))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .map_err(|e| ApiError::Storage(e.to_string()))
    }

    pub(crate) async fn do_memory_files(&self) -> ApiResult<Vec<MemoryFileInfo>> {
        let mut scopes = vec![MemoryScope::Global];
        for project in self.do_list_projects(ListProjects::default()).await? {
            scopes.push(MemoryScope::Project {
                workspace_path: project.workspace_path,
            });
        }
        let mut out = Vec::new();
        for scope in scopes {
            let path = self.memory_path(&scope);
            let meta = tokio::fs::metadata(&path).await.ok();
            out.push(MemoryFileInfo {
                exists: meta.is_some(),
                bytes: meta.as_ref().map_or(0, std::fs::Metadata::len),
                modified_ms: meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .and_then(|d| i64::try_from(d.as_millis()).ok()),
                scope,
                path,
            });
        }
        Ok(out)
    }

    pub(crate) async fn do_memory_doc(&self, scope: MemoryScope) -> ApiResult<MemoryDoc> {
        self.check_open()?;
        let current = self.memory_read_opt(&scope).await?;
        Ok(MemoryDoc {
            path: self.memory_path(&scope),
            exists: current.is_some(),
            content: current.unwrap_or_default(),
            scope,
        })
    }

    pub(crate) async fn do_memory_plan(
        &self,
        scope: MemoryScope,
        content: String,
    ) -> ApiResult<MemoryPlan> {
        self.check_open()?;
        let current = self.memory_read_opt(&scope).await?;
        let before = current.as_deref().unwrap_or_default();
        let no_op = current.as_deref() == Some(content.as_str());
        let path = self.memory_path(&scope);
        let diff = if no_op {
            String::new()
        } else {
            harness_agent_config::unified_diff(&path, before, &content)
        };
        Ok(MemoryPlan {
            base: fingerprint(current.as_deref()),
            scope,
            path,
            content,
            diff,
            no_op,
        })
    }

    pub(crate) async fn do_memory_apply(&self, plan: MemoryPlan) -> ApiResult<()> {
        self.check_open()?;
        if plan.no_op {
            return Ok(());
        }
        let memory = self.ctx.memory.clone();
        let target = Self::memory_target(&plan.scope);
        let (base, content) = (plan.base.clone(), plan.content.clone());
        let written =
            tokio::task::spawn_blocking(move || memory.write_checked(&target, &base, &content))
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?
                .map_err(|e| ApiError::Storage(e.to_string()))?;
        if !written {
            return Err(ApiError::Conflict(format!(
                "{} changed since the plan was made; review it again",
                plan.path
            )));
        }
        self.ctx
            .events
            .emit(None, EventKind::MemoryChanged { scope: plan.scope });
        Ok(())
    }
}
