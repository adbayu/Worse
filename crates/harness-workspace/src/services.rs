use super::{
    git, lock, ApiError, ApiResult, Arc, BoxFuture, BranchInfo, CommitResult, FileContent,
    FileDiff, FileHit, GitStatus, Ordering, SearchError, SearchText, TerminalCreate, TerminalInfo,
    TextMatch, TextSearchResult, Workspace, WorkspaceEventStream, WorkspaceServices,
    WorkspaceStats,
};

pub(super) fn io_error(path: &str, error: &std::io::Error) -> ApiError {
    if error.kind() == std::io::ErrorKind::NotFound {
        ApiError::NotFound(path.to_owned())
    } else {
        ApiError::Internal(format!("{path}: {error}"))
    }
}

pub(super) fn search_error(error: SearchError) -> ApiError {
    match error {
        SearchError::Cancelled => ApiError::Internal("cancelled".into()),
        SearchError::InvalidPattern(text) => ApiError::InvalidArgument(text),
        SearchError::Fs(e) => ApiError::Internal(e.to_string()),
    }
}

pub(super) fn text_match(m: harness_search::TextMatch) -> TextMatch {
    TextMatch {
        path: m.relative_path.replace('\\', "/"),
        line: m.line,
        column: m.column as u64,
        text: m.text,
    }
}

impl WorkspaceServices for Workspace {
    fn root(&self) -> String {
        self.root_str()
    }

    fn read_file(&self, path: String, max_bytes: u64) -> BoxFuture<'_, ApiResult<FileContent>> {
        Box::pin(async move {
            self.check_open()?;
            self.do_read_file(path, max_bytes).await
        })
    }

    fn open_external(&self, path: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.check_open()?;
            let (absolute, _) = self.resolve(path).await?;
            if !tokio::fs::try_exists(&absolute).await.unwrap_or(false) {
                return Err(ApiError::NotFound(absolute.display().to_string()));
            }
            git::open_with_os(&absolute)
        })
    }

    fn search_files(&self, query: String, limit: u32) -> BoxFuture<'_, ApiResult<Vec<FileHit>>> {
        Box::pin(self.do_search_files(query, limit))
    }

    fn search_text(&self, request: SearchText) -> BoxFuture<'_, ApiResult<TextSearchResult>> {
        Box::pin(self.do_search_text(request))
    }

    fn cancel_search(&self, search_id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            if let Some((_, token)) = lock(&self.inner.searches).remove(&search_id) {
                token.cancel();
            }
            Ok(())
        })
    }

    fn git_status(&self) -> BoxFuture<'_, ApiResult<GitStatus>> {
        Box::pin(async move {
            self.check_open()?;
            self.blocking(|inner| git::status(&inner.git, inner.root.as_str()))
                .await
        })
    }

    fn git_file_diff(&self, path: String, staged: bool) -> BoxFuture<'_, ApiResult<FileDiff>> {
        Box::pin(async move {
            self.check_open()?;
            let (_, relative) = self.resolve(path).await?;
            self.blocking(move |inner| {
                git::file_diff(&inner.git, inner.root.as_str(), &relative, staged)
            })
            .await
        })
    }

    fn git_stage(&self, paths: Vec<String>) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.check_open()?;
            let relative = self.relative_paths(paths).await?;
            git::stage(self.inner.root.as_str(), &relative).await
        })
    }

    fn git_unstage(&self, paths: Vec<String>) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.check_open()?;
            let relative = self.relative_paths(paths).await?;
            git::unstage(self.inner.root.as_str(), &relative).await
        })
    }

    fn git_commit(&self, message: String) -> BoxFuture<'_, ApiResult<CommitResult>> {
        Box::pin(async move {
            self.check_open()?;
            git::commit(self.inner.root.as_str(), &message).await
        })
    }

    fn git_branches(&self) -> BoxFuture<'_, ApiResult<Vec<BranchInfo>>> {
        Box::pin(async move {
            self.check_open()?;
            self.blocking(|inner| git::branches(&inner.git, inner.root.as_str()))
                .await
        })
    }

    fn terminal_create(&self, request: TerminalCreate) -> BoxFuture<'_, ApiResult<TerminalInfo>> {
        Box::pin(self.do_terminal_create(request))
    }

    fn terminal_write(&self, id: String, data: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.terminal_known(&id)?;
            self.blocking(move |inner| {
                inner
                    .pty
                    .get()
                    .ok_or_else(|| ApiError::NotFound(format!("terminal {id}")))?
                    .write(&id, data.as_bytes())
                    .map_err(|e| ApiError::NotFound(e.to_string()))
            })
            .await
        })
    }

    fn terminal_resize(&self, id: String, cols: u16, rows: u16) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            if cols == 0 || rows == 0 {
                return Err(ApiError::InvalidArgument(
                    "terminal size must be positive".into(),
                ));
            }
            self.terminal_known(&id)?
                .resize(&id, cols, rows)
                .map_err(|e| ApiError::NotFound(e.to_string()))
        })
    }

    fn terminal_close(&self, id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            self.terminal_known(&id)?;
            // Removed first: the sink drops anything the dying shell still prints (no events after close).
            lock(&self.inner.terminals).remove(&id);
            let pty_id = id.clone();
            let killed = {
                let inner = Arc::clone(&self.inner);
                tokio::task::spawn_blocking(move || {
                    inner.pty.get().is_some_and(|p| p.kill(&pty_id))
                })
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?
            };
            if killed {
                Ok(())
            } else {
                Err(ApiError::NotFound(format!("terminal {id}")))
            }
        })
    }

    fn watch_start(&self) -> BoxFuture<'_, ApiResult<String>> {
        Box::pin(self.do_watch_start())
    }

    fn watch_stop(&self, watch_id: String) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            let removed = lock(&self.inner.watches).remove(&watch_id);
            let stopped = self
                .inner
                .watch
                .get()
                .is_some_and(|hub| hub.unwatch(&watch_id));
            if removed || stopped {
                Ok(())
            } else {
                Err(ApiError::NotFound(format!("watch {watch_id}")))
            }
        })
    }

    fn stats(&self) -> WorkspaceStats {
        WorkspaceStats {
            terminals: u32::try_from(lock(&self.inner.terminals).len()).unwrap_or(u32::MAX),
            watches: u32::try_from(lock(&self.inner.watches).len()).unwrap_or(u32::MAX),
            searches_running: u32::try_from(lock(&self.inner.searches).len()).unwrap_or(u32::MAX),
            index_built: self.inner.index_built.load(Ordering::SeqCst),
            pty_started: self.inner.pty.get().is_some(),
            watcher_started: self.inner.watch.get().is_some(),
        }
    }

    fn subscribe(&self) -> WorkspaceEventStream {
        self.inner.bus.subscribe()
    }

    fn shutdown(&self) -> BoxFuture<'_, ApiResult<()>> {
        Box::pin(async move {
            if self.inner.shut_down.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
            for (_, (_, token)) in lock(&self.inner.searches).drain() {
                token.cancel();
            }
            lock(&self.inner.terminals).clear();
            lock(&self.inner.watches).clear();
            let inner = Arc::clone(&self.inner);
            tokio::task::spawn_blocking(move || {
                if let Some(pty) = inner.pty.get() {
                    pty.dispose_all();
                }
                if let Some(hub) = inner.watch.get() {
                    hub.dispose_all();
                }
            })
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
            self.inner.bus.close();
            Ok(())
        })
    }
}

impl Workspace {
    pub(super) async fn relative_paths(&self, paths: Vec<String>) -> ApiResult<Vec<String>> {
        if paths.is_empty() {
            return Err(ApiError::InvalidArgument("no paths given".into()));
        }
        self.blocking(move |inner| {
            paths
                .iter()
                .map(|p| inner.root.resolve(p).map(|(_, rel)| rel))
                .collect()
        })
        .await
    }
}
