use super::{
    io_error, lock, search_error, text_match, ApiError, ApiResult, Arc, AsyncReadExt,
    CancellationToken, CreateOptions, EntryType, FileContent, FileHit, IgnoreRules, Ordering,
    PathBuf, PtyEvent, PtyManager, PtySink, SearchText, TerminalCreate, TerminalInfo, TextMatch,
    TextSearchOptions, TextSearchResult, WatchOptions, WatchSignal, WatchSink, Workspace,
    WorkspaceEventKind, DEFAULT_MAX_RESULTS, DEFAULT_READ_BYTES, MAX_READ_BYTES,
};

impl Workspace {
    pub(super) async fn do_read_file(
        &self,
        path: String,
        max_bytes: u64,
    ) -> ApiResult<FileContent> {
        let (absolute, relative) = self.resolve(path).await?;
        let limit = match max_bytes {
            0 => DEFAULT_READ_BYTES,
            n => n.min(MAX_READ_BYTES),
        };
        let file = tokio::fs::File::open(&absolute)
            .await
            .map_err(|e| io_error(&relative, &e))?;
        let metadata = file.metadata().await.map_err(|e| io_error(&relative, &e))?;
        if !metadata.is_file() {
            return Err(ApiError::InvalidArgument(format!("not a file: {relative}")));
        }
        let size = metadata.len();
        let mut bytes = Vec::with_capacity(usize::try_from(size.min(limit)).unwrap_or(0));
        file.take(limit)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| io_error(&relative, &e))?;
        let binary = harness_fs::is_probably_binary(&bytes[..bytes.len().min(8192)]);
        Ok(FileContent {
            path: relative,
            text: if binary {
                String::new()
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            },
            size,
            truncated: size > limit,
            binary,
        })
    }

    pub(super) async fn resolve(&self, path: String) -> ApiResult<(PathBuf, String)> {
        self.blocking(move |inner| inner.root.resolve(&path)).await
    }

    pub(super) async fn do_search_files(
        &self,
        query: String,
        limit: u32,
    ) -> ApiResult<Vec<FileHit>> {
        self.check_open()?;
        let index = self.index(false).await?;
        let root = self.root_str();
        let limit = usize::try_from(limit.clamp(1, 1000)).unwrap_or(1000);
        self.blocking(move |_| {
            Ok(harness_search::search_entries(&index, &root, &query, limit)
                .into_iter()
                .map(|entry| FileHit {
                    name: entry.name,
                    path: entry.relative_path.replace('\\', "/"),
                    is_dir: entry.kind == EntryType::Directory,
                })
                .collect())
        })
        .await
    }

    pub(super) async fn do_search_text(&self, request: SearchText) -> ApiResult<TextSearchResult> {
        self.check_open()?;
        if request.query.is_empty() {
            return Err(ApiError::InvalidArgument("search query is empty".into()));
        }
        let token = CancellationToken::new();
        let generation = self.inner.search_generation.fetch_add(1, Ordering::SeqCst);
        {
            let mut searches = lock(&self.inner.searches);
            if let Some((_, previous)) =
                searches.insert(request.search_id.clone(), (generation, token.clone()))
            {
                previous.cancel();
            }
        }
        let result = async {
            let index = self.index(false).await?;
            let root = self.root_str();
            let bus = self.inner.bus.clone();
            let max_results = match request.max_results {
                0 => DEFAULT_MAX_RESULTS,
                n => n,
            };
            let options = TextSearchOptions {
                query: request.query.clone(),
                is_regex: request.is_regex,
                case_sensitive: request.case_sensitive,
                max_results: usize::try_from(max_results).unwrap_or(usize::MAX),
                max_file_bytes: 2 * 1024 * 1024,
            };
            let search_id = request.search_id.clone();
            let cancel = token.clone();
            self.blocking(move |_| {
                let mut all: Vec<TextMatch> = Vec::new();
                let mut on_progress = |batch: Vec<harness_search::TextMatch>| {
                    let matches: Vec<TextMatch> = batch.into_iter().map(text_match).collect();
                    all.extend(matches.iter().cloned());
                    bus.emit(&WorkspaceEventKind::SearchProgress {
                        search_id: search_id.clone(),
                        matches,
                    });
                };
                let summary =
                    harness_search::search_text(&index, &root, &options, &cancel, &mut on_progress)
                        .map_err(search_error)?;
                if summary.cancelled || cancel.is_cancelled() {
                    return Err(ApiError::Internal("cancelled".into()));
                }
                Ok(TextSearchResult {
                    matches: all,
                    match_count: summary.match_count as u64,
                    file_count: summary.file_count as u64,
                    truncated: summary.truncated,
                })
            })
            .await
        }
        .await;
        let mut searches = lock(&self.inner.searches);
        if searches
            .get(&request.search_id)
            .is_some_and(|(g, _)| *g == generation)
        {
            searches.remove(&request.search_id);
        }
        result
    }

    pub(super) async fn do_terminal_create(
        &self,
        request: TerminalCreate,
    ) -> ApiResult<TerminalInfo> {
        self.check_open()?;
        if request.cols == 0 || request.rows == 0 {
            return Err(ApiError::InvalidArgument(
                "terminal size must be positive".into(),
            ));
        }
        let cwd = match request.cwd {
            Some(cwd) if !cwd.trim().is_empty() => self.resolve(cwd).await?.0,
            _ => PathBuf::from(self.inner.root.as_str()),
        };
        let env = self.inner.config.terminal_env.as_ref().map(|pairs| {
            harness_pty::EnvMap::from_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        });
        let options = CreateOptions {
            cols: request.cols,
            rows: request.rows,
            cwd: Some(cwd.to_string_lossy().into_owned()),
            shell: request.shell.filter(|s| !s.trim().is_empty()),
            args: Vec::new(),
            env,
            name: Some("xterm-256color".into()),
            attached: true,
            flow_control: false,
        };
        let inner = Arc::clone(&self.inner);
        let sink: PtySink = Arc::new(move |id: &str, event: PtyEvent| {
            // Called under the session's output lock: only a set lookup and an unbounded send (no blocking).
            let live = lock(&inner.terminals).contains(id);
            if !live {
                return;
            }
            match event {
                PtyEvent::Data(data) => inner.bus.emit(&WorkspaceEventKind::TerminalOutput {
                    id: id.to_owned(),
                    data,
                }),
                PtyEvent::Exit(code) => {
                    lock(&inner.terminals).remove(id);
                    inner.bus.emit(&WorkspaceEventKind::TerminalExited {
                        id: id.to_owned(),
                        code,
                    });
                }
            }
        });
        let this = self.clone();
        // ConPTY start and shell spawn block; keep them off the runtime threads.
        let created = self
            .blocking(move |inner| {
                // The id is unknown until `create` returns, but output may arrive before that: hold the
                // terminals lock across `create` so the sink waits for the id to be registered.
                let mut terminals = lock(&inner.terminals);
                let created = this
                    .pty()
                    .create(options, sink)
                    .map_err(|e| ApiError::Internal(e.to_string()))?;
                terminals.insert(created.id.clone());
                Ok(created)
            })
            .await?;
        Ok(TerminalInfo {
            id: created.id,
            shell: created.shell,
            cwd: created.cwd,
            pid: created.pid,
        })
    }

    pub(super) fn terminal_known(&self, id: &str) -> ApiResult<&PtyManager> {
        self.check_open()?;
        let pty = self
            .inner
            .pty
            .get()
            .ok_or_else(|| ApiError::NotFound(format!("terminal {id}")))?;
        if !lock(&self.inner.terminals).contains(id) {
            return Err(ApiError::NotFound(format!("terminal {id}")));
        }
        Ok(pty)
    }

    pub(super) async fn do_watch_start(&self) -> ApiResult<String> {
        self.check_open()?;
        let root = self.root_str();
        let this = self.clone();
        self.blocking(move |inner| {
            let excludes = [".git/", "target/", "node_modules/"].map(str::to_owned);
            let rules = IgnoreRules::build(&root, true, &excludes).map_err(ApiError::Internal)?;
            let bus = inner.bus.clone();
            let root_for_sink = inner.root.clone();
            let sink: WatchSink = Arc::new(move |id: &str, signal: WatchSignal| match signal {
                WatchSignal::Change { event, rescan } => {
                    let paths = event
                        .changed_path
                        .and_then(|p| root_for_sink.relative(&p))
                        .into_iter()
                        .collect();
                    bus.emit(&WorkspaceEventKind::FilesChanged {
                        watch_id: id.to_owned(),
                        paths,
                        rescan,
                    });
                }
                WatchSignal::Closed { reason } => {
                    bus.emit(&WorkspaceEventKind::WatchClosed {
                        watch_id: id.to_owned(),
                        reason: reason.wire(),
                    });
                }
            });
            let id = this
                .watch_hub()
                .watch(
                    &root,
                    WatchOptions {
                        recursive: true,
                        ignore: Some(rules),
                    },
                    sink,
                )
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            lock(&inner.watches).insert(id.clone());
            Ok(id)
        })
        .await
    }
}
