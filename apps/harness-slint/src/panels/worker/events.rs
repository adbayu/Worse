use super::{
    error_text, view, DiffSource, DiffView, Done, Hit, Instant, TermOp, Worker, WorkspaceEventKind,
    WorkspaceServices, GIT_DEBOUNCE, MAX_HITS,
};

impl Worker {
    // ---- results and events -----------------------------------------------------------------------------------

    pub(super) fn done(&mut self, done: Done) {
        match done {
            Done::Git(seq, result) if seq == self.git_seq => match result {
                Ok(status) => self.apply_git(status),
                Err(error) => view::git_result(&self.window, error_text(&error), true, false),
            },
            Done::Action {
                result,
                clear_message,
            } => {
                match result {
                    Ok(text) => view::git_result(&self.window, text, false, clear_message),
                    Err(text) => view::git_result(&self.window, text, true, false),
                }
                self.git_refresh(false);
                self.refresh_git_diff();
            }
            Done::Changed => {
                self.git_refresh(false);
                self.refresh_git_diff();
            }
            Done::Diff(seq, source, result) if seq == self.diff_seq => {
                self.diff_source = Some(source);
                self.diff_current = 0;
                match result {
                    Ok(files) => {
                        self.diff_files = files;
                        self.render_diff();
                    }
                    Err(reason) => {
                        self.diff_files.clear();
                        let mut out = DiffView {
                            source: if matches!(self.diff_source, Some(DiffSource::Turn { .. })) {
                                "turn"
                            } else {
                                "git"
                            },
                            title: "Turn changes".into(),
                            notice: reason,
                            ..DiffView::default()
                        };
                        if let Some(DiffSource::Git { path, .. }) = &self.diff_source {
                            out.title.clone_from(path);
                        }
                        view::diff(&self.window, out);
                    }
                }
            }
            Done::Files(seq, result) if seq == self.search_seq => match result {
                Ok(hits) => {
                    self.hits = hits;
                    view::search(
                        &self.window,
                        self.hits.clone(),
                        false,
                        self.search_summary(None),
                        String::new(),
                    );
                }
                Err(error) => view::search(
                    &self.window,
                    Vec::new(),
                    false,
                    String::new(),
                    error_text(&error),
                ),
            },
            Done::Text(seq, result) if seq == self.search_seq => {
                self.search_id = None;
                self.search_dirty = false;
                match result {
                    Ok((hits, matches, files, truncated)) => {
                        self.hits = hits;
                        self.hits.truncate(MAX_HITS);
                        view::search(
                            &self.window,
                            self.hits.clone(),
                            false,
                            self.search_summary(Some((matches, files, truncated))),
                            String::new(),
                        );
                    }
                    Err(error) => view::search(
                        &self.window,
                        self.hits.clone(),
                        false,
                        String::new(),
                        error_text(&error),
                    ),
                }
            }
            Done::Preview(seq, path, result) if seq == self.preview_seq => match result {
                Ok(lines) => view::preview(&self.window, path, lines),
                Err(text) => view::preview(&self.window, path, vec![(0, text, false)]),
            },
            // Stale result of a superseded request.
            _ => {}
        }
    }

    pub(super) fn event(&mut self, kind: WorkspaceEventKind) {
        match kind {
            WorkspaceEventKind::TerminalOutput { id, data } => {
                let Some(terminal) = &mut self.terminal else {
                    return;
                };
                if terminal.id.as_deref() != Some(id.as_str()) {
                    return;
                }
                let replies = terminal.emu.feed(&data);
                terminal.dirty = true;
                if let Some(tx) = &self.term_tx {
                    for reply in replies {
                        let _ = tx.send(TermOp::Write(id.clone(), reply));
                    }
                }
                self.schedule_frame();
            }
            WorkspaceEventKind::TerminalExited { id, code } => {
                let Some(terminal) = &mut self.terminal else {
                    return;
                };
                if terminal.id.as_deref() != Some(id.as_str()) {
                    return;
                }
                terminal.id = None;
                terminal.dirty = true;
                terminal.status.running = false;
                terminal.status.exited = true;
                terminal.status.exit_code = code;
                view::term_status(&self.window, terminal.status.clone());
                self.schedule_frame();
            }
            WorkspaceEventKind::FilesChanged { watch_id, .. } => {
                if self.watch.as_deref() == Some(watch_id.as_str()) {
                    self.git_due = Some(Instant::now() + GIT_DEBOUNCE);
                }
            }
            WorkspaceEventKind::WatchClosed { watch_id, .. } => {
                if self.watch.as_deref() == Some(watch_id.as_str()) {
                    self.watch = None;
                }
            }
            WorkspaceEventKind::SearchProgress { search_id, matches } => {
                if self.search_id.as_deref() != Some(search_id.as_str())
                    || self.hits.len() >= MAX_HITS
                {
                    return;
                }
                let room = MAX_HITS - self.hits.len();
                self.hits
                    .extend(matches.into_iter().take(room).map(|m| Hit {
                        path: m.path,
                        line: m.line,
                        column: m.column,
                        text: m.text,
                        is_dir: false,
                    }));
                self.search_dirty = true;
                self.schedule_frame();
            }
        }
    }

    pub(super) async fn shutdown(&mut self) {
        self.term_tx = None;
        if let Some(ws) = self.ws.take() {
            if let Err(error) = ws.shutdown().await {
                eprintln!("harness-slint: panels shutdown: {error}");
            }
        }
    }
}
