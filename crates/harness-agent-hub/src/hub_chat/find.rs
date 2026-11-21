use super::{
    as_u32, context, find_all, fold, score, store, AgentConfigService, ApiResult, Concept,
    ConfigScope, ConfigTarget, ContextItem, EntryBody, FileHit, FindField, FindHit, FindInSession,
    Hub, MentionCandidate, MentionKind, MentionQuery, ResolvedItem, SessionCmd, SessionId,
    SessionRow, SessionSummary,
};

impl Hub {
    pub(crate) async fn do_find(&self, request: FindInSession) -> ApiResult<Vec<FindHit>> {
        let detail = self.ask(&request.session_id, SessionCmd::Snapshot).await?;
        let limit = match request.limit {
            0 => 200,
            n => n.min(500),
        } as usize;
        let query = fold(&request.query, request.case_sensitive);
        let mut hits = Vec::new();
        let length = as_u32(query.len());
        'turns: for turn in &detail.turns {
            let mut fields: Vec<(FindField, Option<String>, &str)> = vec![
                (FindField::User, None, turn.user_text.as_str()),
                (FindField::Assistant, None, turn.assistant_text.as_str()),
            ];
            for tool in &turn.tool_calls {
                fields.push((
                    FindField::Tool,
                    Some(tool.tool_call_id.clone()),
                    tool.title.as_str(),
                ));
            }
            for (field, tool, text) in fields {
                for (start, snippet) in find_all(text, &query, request.case_sensitive) {
                    hits.push(FindHit {
                        turn_id: turn.id.clone(),
                        seq: turn.seq,
                        field,
                        tool_call_id: tool.clone(),
                        start: as_u32(start),
                        len: length,
                        snippet,
                    });
                    if hits.len() >= limit {
                        break 'turns;
                    }
                }
            }
        }
        Ok(hits)
    }

    pub(crate) async fn do_resolve_context(
        &self,
        session: SessionId,
        items: Vec<ContextItem>,
    ) -> ApiResult<Vec<ResolvedItem>> {
        self.check_open()?;
        let row = self.load_row(&session).await?;
        let caps = self.ctx.chat.caps(&row.active_agent);
        Ok(context::check(&self.ctx, &row.workspace_path, &session, &items, caps).await)
    }

    pub(super) async fn search_files(
        &self,
        workspace: &str,
        query: &str,
        limit: u32,
    ) -> ApiResult<Option<Vec<FileHit>>> {
        let Some(source) = &self.ctx.config.file_search else {
            return Ok(None);
        };
        Ok(Some(
            source
                .0
                .search(workspace.to_owned(), query.to_owned(), limit)
                .await?,
        ))
    }

    pub(crate) async fn do_mentions(
        &self,
        request: MentionQuery,
    ) -> ApiResult<Vec<MentionCandidate>> {
        self.check_open()?;
        let row = self.load_row(&request.session_id).await?;
        let limit = match request.limit {
            0 => 20,
            n => n.min(50),
        };
        let wants = |k: MentionKind| request.kinds.is_empty() || request.kinds.contains(&k);
        let mut out = Vec::new();
        if wants(MentionKind::File) || wants(MentionKind::Folder) {
            let files = self
                .search_files(&row.workspace_path, &request.query, limit)
                .await?
                .unwrap_or_default();
            for hit in files {
                let kind = if hit.is_dir {
                    MentionKind::Folder
                } else {
                    MentionKind::File
                };
                if !wants(kind) {
                    continue;
                }
                let item = if hit.is_dir {
                    ContextItem::Folder {
                        path: hit.path.clone(),
                    }
                } else {
                    ContextItem::File {
                        path: hit.path.clone(),
                    }
                };
                out.push(MentionCandidate {
                    kind,
                    label: hit.name,
                    detail: hit.path,
                    item,
                    insert_text: None,
                });
            }
        }
        if wants(MentionKind::Session) {
            let current = request.session_id.clone();
            let query = request.query.trim().to_owned();
            let sessions = self
                .ctx
                .store
                .run(move |c| store::list_sessions(c, None, None, 2000))
                .await?;
            let mut ranked: Vec<(u32, SessionSummary)> = sessions
                .into_iter()
                .filter(|s| s.id != current)
                .filter_map(|s| score(&query, &s.title).map(|n| (n, s)))
                .collect();
            ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.updated_at.cmp(&a.1.updated_at)));
            for (_, summary) in ranked.into_iter().take(limit as usize) {
                out.push(MentionCandidate {
                    kind: MentionKind::Session,
                    label: summary.title.clone(),
                    detail: summary.workspace_path.clone(),
                    item: ContextItem::Session {
                        session_id: summary.id,
                    },
                    insert_text: None,
                });
            }
        }
        if wants(MentionKind::Skill) || wants(MentionKind::Subagent) {
            let mut config = self
                .config_mentions(
                    &row,
                    &request.query,
                    wants(MentionKind::Skill),
                    wants(MentionKind::Subagent),
                )
                .await;
            config.truncate(limit as usize);
            out.extend(config);
        }
        out.truncate(limit as usize);
        Ok(out)
    }

    /// Skills and subagents of the session's active agent kind from `harness-agent-config` (workspace scope first,
    /// then user; the first definition of a name wins), ranked by name (phase-9-parity.md §9.2.5 R13).
    pub(super) async fn config_mentions(
        &self,
        row: &SessionRow,
        query: &str,
        skills: bool,
        subagents: bool,
    ) -> Vec<MentionCandidate> {
        let (Some(kind), Some(paths)) = (
            self.agent_kind(&row.active_agent),
            &self.ctx.config.config_paths,
        ) else {
            return Vec::new();
        };
        let mut paths = paths.clone();
        paths.workspace = Some(std::path::PathBuf::from(&row.workspace_path));
        let service = AgentConfigService::new(paths, self.ctx.config.auth_probe.0.clone());
        let query = query.trim();
        let mut ranked: Vec<(u32, MentionCandidate)> = Vec::new();
        let concepts = [(Concept::Skill, skills), (Concept::Subagent, subagents)];
        for (concept, wanted) in concepts {
            if !wanted {
                continue;
            }
            for scope in [ConfigScope::Workspace, ConfigScope::User] {
                // 不支持的概念或作用域（如 Codex 的 subagent）只是没有候选，不是错误。
                let Ok(entries) = service
                    .list(concept, ConfigTarget { agent: kind, scope })
                    .await
                else {
                    continue;
                };
                for entry in entries {
                    let (mention, name, description, insert_text) = match entry.body {
                        EntryBody::Skill(skill) => {
                            (MentionKind::Skill, skill.name, skill.description, None)
                        }
                        EntryBody::Subagent(agent) => {
                            let text = format!("@agent-{}", agent.name);
                            (
                                MentionKind::Subagent,
                                agent.name,
                                agent.description,
                                Some(text),
                            )
                        }
                        _ => continue,
                    };
                    if entry.enabled == Some(false)
                        || ranked
                            .iter()
                            .any(|(_, c)| c.kind == mention && c.label == name)
                    {
                        continue;
                    }
                    let Some(score) = score(query, &name) else {
                        continue;
                    };
                    ranked.push((
                        score,
                        MentionCandidate {
                            kind: mention,
                            label: name,
                            detail: description.unwrap_or_default(),
                            item: ContextItem::File {
                                path: entry.source_path.to_string_lossy().into_owned(),
                            },
                            insert_text,
                        },
                    ));
                }
            }
        }
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.cmp(&b.1.label)));
        ranked.into_iter().map(|(_, c)| c).collect()
    }
}
