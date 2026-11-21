use super::{
    gh, invalid, new_id, now_ms, params, save_source, sources, store, valid_repo, ApiError,
    ApiResult, EventKind, GhStatus, Hub, InboxSource, InboxSourceInput, InboxStatus,
    OptionalExtension, Path, DEFAULT_INBOX_HANDLE, DEFAULT_INBOX_INTERVAL, DEFAULT_INBOX_MAX_TURNS,
    ENABLED_KEY, GH_CACHE_MS, MAX_INBOX_INTERVAL, MAX_INBOX_SOURCES, MIN_INBOX_INTERVAL,
};

impl Hub {
    pub(super) fn inbox_changed(&self) {
        self.ctx.events.emit(None, EventKind::InboxChanged);
    }

    pub(super) async fn inbox_enabled(&self) -> ApiResult<bool> {
        Ok(self
            .ctx
            .store
            .run(|c| store::get_setting(c, ENABLED_KEY))
            .await?
            .is_some_and(|v| v == "true"))
    }

    pub(super) async fn inbox_sources(&self) -> ApiResult<Vec<InboxSource>> {
        Ok(self.ctx.store.run(sources).await?)
    }

    pub(super) async fn source(&self, id: &str) -> ApiResult<InboxSource> {
        self.inbox_sources()
            .await?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| ApiError::NotFound(format!("inbox source {id}")))
    }

    pub(super) async fn gh_status(&self) -> (GhStatus, String) {
        if let Some((status, detail, at)) = self.inbox.gh.lock().ok().and_then(|g| g.clone()) {
            if now_ms() - at < GH_CACHE_MS {
                return (status, detail);
            }
        }
        let result = if gh(&["--version"], None).await.is_err() {
            (GhStatus::Missing, "GitHub CLI (gh) not found".to_owned())
        } else if gh(&["auth", "status"], None).await.is_err() {
            (GhStatus::SignedOut, "Sign in with gh auth login".to_owned())
        } else {
            let login = gh(&["api", "user", "--jq", ".login"], None)
                .await
                .map(|l| l.trim().to_owned())
                .unwrap_or_default();
            (GhStatus::Ready, login)
        };
        if let Ok(mut slot) = self.inbox.gh.lock() {
            *slot = Some((result.0, result.1.clone(), now_ms()));
        }
        result
    }

    pub(crate) async fn do_inbox_status(&self) -> ApiResult<InboxStatus> {
        self.check_open()?;
        let (gh, gh_detail) = self.gh_status().await;
        Ok(InboxStatus {
            enabled: self.inbox_enabled().await?,
            gh,
            gh_detail,
            gh_path: crate::gh::resolve_gh()
                .map(|(path, _)| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            sources: self.inbox_sources().await?,
        })
    }

    pub(crate) async fn do_set_inbox_enabled(&self, enabled: bool) -> ApiResult<()> {
        self.check_open()?;
        let now = now_ms();
        self.ctx
            .store
            .run(move |c| {
                let was = store::get_setting(c, ENABLED_KEY)?.is_some_and(|v| v == "true");
                store::set_setting(c, ENABLED_KEY, if enabled { "true" } else { "false" })?;
                if enabled && !was {
                    for mut source in sources(c)? {
                        source.armed_at = now;
                        source.last_poll = None;
                        save_source(c, &source)?;
                        c.execute(
                            "DELETE FROM inbox_cursors WHERE source_id=?1",
                            params![source.id],
                        )?;
                    }
                }
                Ok(())
            })
            .await?;
        if let Ok(mut slot) = self.inbox.gh.lock() {
            *slot = None;
        }
        self.inbox_changed();
        self.ensure_inbox().await;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn do_save_inbox_source(
        &self,
        input: InboxSourceInput,
    ) -> ApiResult<InboxSource> {
        self.check_open()?;
        let repo = input.repo.trim().trim_end_matches(".git").to_owned();
        if !valid_repo(&repo) {
            return Err(invalid("the repository is written as owner/name"));
        }
        let path = input.workspace_path.trim().to_owned();
        if path.is_empty() {
            return Err(invalid("choose the local clone of the repository"));
        }
        let inside =
            crate::snapshot::git_text(Path::new(&path), &["rev-parse", "--is-inside-work-tree"])
                .await
                .is_ok_and(|out| out.trim() == "true");
        if !inside {
            return Err(invalid("the local folder is not a git repository"));
        }
        if crate::snapshot::git_text(Path::new(&path), &["remote", "get-url", "origin"])
            .await
            .is_err()
        {
            return Err(invalid("the local clone has no origin remote"));
        }
        let mut handle = input.handle.trim().to_owned();
        if handle.is_empty() {
            DEFAULT_INBOX_HANDLE.clone_into(&mut handle);
        }
        if !handle.starts_with('@') {
            handle.insert(0, '@');
        }
        if handle.len() < 2 || handle.chars().skip(1).any(char::is_whitespace) {
            return Err(invalid("the mention handle is one word, like @harness"));
        }
        let mut allowlist: Vec<String> = Vec::new();
        for login in &input.allowlist {
            let login = login.trim().trim_start_matches('@').to_owned();
            if !login.is_empty() && !allowlist.iter().any(|l| l.eq_ignore_ascii_case(&login)) {
                allowlist.push(login);
            }
        }
        let interval_secs = if input.interval_secs == 0 {
            DEFAULT_INBOX_INTERVAL
        } else {
            input
                .interval_secs
                .clamp(MIN_INBOX_INTERVAL, MAX_INBOX_INTERVAL)
        };
        let max_turns = if input.max_turns == 0 {
            DEFAULT_INBOX_MAX_TURNS
        } else {
            input.max_turns.min(50)
        };
        if input.route && self.load_orchestra().await.orchestrator().is_none() {
            return Err(invalid("Set an Orchestrator in Orchestra first"));
        }
        let mut shadow_labels: Vec<String> = Vec::new();
        for label in &input.shadow_labels {
            let label = label.trim().to_owned();
            if !label.is_empty() && !shadow_labels.iter().any(|l| l.eq_ignore_ascii_case(&label)) {
                shadow_labels.push(label);
            }
        }
        let key = if input.workspace_key.trim().is_empty() {
            path.clone()
        } else {
            input.workspace_key.trim().to_owned()
        };
        let existing = self.inbox_sources().await?;
        let previous = input
            .id
            .as_ref()
            .and_then(|id| existing.iter().find(|s| &s.id == id).cloned());
        if input.id.is_some() && previous.is_none() {
            return Err(ApiError::NotFound("inbox source".into()));
        }
        if previous.is_none() && existing.len() >= MAX_INBOX_SOURCES {
            return Err(invalid("Harness watches at most 20 repositories"));
        }
        if existing
            .iter()
            .any(|s| s.repo.eq_ignore_ascii_case(&repo) && Some(&s.id) != input.id.as_ref())
        {
            return Err(invalid("this repository is already watched"));
        }
        let now = now_ms();
        let rearm = previous.as_ref().map_or(true, |p| !p.enabled) && input.enabled;
        let source = InboxSource {
            id: previous
                .as_ref()
                .map_or_else(|| new_id("src"), |p| p.id.clone()),
            repo,
            workspace_path: path,
            workspace_key: key,
            enabled: input.enabled,
            handle,
            allowlist,
            interval_secs,
            agent: input.agent,
            draft_first: input.draft_first,
            max_turns,
            armed_at: if rearm {
                now
            } else {
                previous.as_ref().map_or(now, |p| p.armed_at)
            },
            last_poll: if rearm {
                None
            } else {
                previous.as_ref().and_then(|p| p.last_poll)
            },
            last_error: previous.as_ref().and_then(|p| p.last_error.clone()),
            route: input.route,
            shadow_labels,
            router_session_id: previous.as_ref().and_then(|p| p.router_session_id.clone()),
        };
        let stored = source.clone();
        self.ctx
            .store
            .run(move |c| {
                save_source(c, &stored)?;
                if rearm {
                    c.execute(
                        "DELETE FROM inbox_cursors WHERE source_id=?1",
                        params![stored.id],
                    )?;
                }
                Ok(())
            })
            .await?;
        self.inbox_changed();
        self.ensure_inbox().await;
        Ok(source)
    }

    pub(crate) async fn do_delete_inbox_source(&self, id: String) -> ApiResult<()> {
        self.check_open()?;
        let lookup = id.clone();
        let (removed, router) = self
            .ctx
            .store
            .run(move |c| {
                c.execute(
                    "DELETE FROM inbox_cursors WHERE source_id=?1",
                    params![lookup],
                )?;
                c.execute(
                    "DELETE FROM inbox_pending WHERE source_id=?1",
                    params![lookup],
                )?;
                let router: Option<String> = c
                    .query_row(
                        "SELECT session_id FROM inbox_routers WHERE source_id=?1",
                        params![lookup],
                        |r| r.get(0),
                    )
                    .optional()?;
                Ok((
                    c.execute("DELETE FROM inbox_sources WHERE id=?1", params![lookup])? > 0,
                    router,
                ))
            })
            .await?;
        if !removed {
            return Err(ApiError::NotFound(format!("inbox source {id}")));
        }
        if let Some(session) = router {
            self.drop_router(&session).await;
            let lookup = id.clone();
            let _ = self
                .ctx
                .store
                .run(move |c| {
                    c.execute(
                        "DELETE FROM inbox_routers WHERE source_id=?1",
                        params![lookup],
                    )?;
                    Ok(())
                })
                .await;
        }
        self.inbox_changed();
        self.ensure_inbox().await;
        Ok(())
    }

    pub(crate) async fn do_poll_inbox_now(&self, id: String) -> ApiResult<()> {
        self.check_open()?;
        if !self.inbox_enabled().await? {
            return Err(invalid("turn on Watch GitHub first"));
        }
        let source = self.source(&id).await?;
        self.poll_source(source).await.map_err(ApiError::Internal)
    }
}
