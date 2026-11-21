use super::{view, Cmd, DiffSource, Worker, WorkspaceServices};

impl Worker {
    pub(super) async fn command(&mut self, cmd: Cmd) {
        if let Cmd::SetRoot(root) = cmd {
            self.set_root(root).await;
            return;
        }
        let Some(ws) = self.workspace().await else {
            return;
        };
        match cmd {
            Cmd::GitRefresh => {
                if self.watch.is_none() {
                    // The watcher starts with the git panel and then stays on (spec §10): it keeps the
                    // status and a shown working-tree diff fresh.
                    match ws.watch_start().await {
                        Ok(id) => self.watch = Some(id),
                        Err(error) => eprintln!("harness-slint: watcher unavailable: {error}"),
                    }
                }
                self.git_refresh(true);
            }
            Cmd::Stage(path) => self.git_action(&ws, vec![path], true),
            Cmd::Unstage(path) => self.git_action(&ws, vec![path], false),
            Cmd::StageAll => {
                let paths = self
                    .git
                    .as_ref()
                    .map(|g| g.unstaged.iter().map(|c| c.path.clone()).collect());
                self.git_action(&ws, paths.unwrap_or_default(), true);
            }
            Cmd::UnstageAll => {
                let paths = self
                    .git
                    .as_ref()
                    .map(|g| g.staged.iter().map(|c| c.path.clone()).collect());
                self.git_action(&ws, paths.unwrap_or_default(), false);
            }
            Cmd::Commit(message) => self.git_commit(&ws, message),
            Cmd::Git(call) => self.git_call(&ws, call),
            Cmd::OpenDiff { path, staged } => self.load_diff(DiffSource::Git { path, staged }),
            Cmd::DiffSelect(index) => {
                if index < self.diff_files.len() {
                    self.diff_current = index;
                    self.render_diff();
                }
            }
            Cmd::ShowTurn => self.show_turn().await,
            Cmd::DiffRefresh => match self.diff_source.clone() {
                Some(source) => self.load_diff(source),
                None => self.show_turn().await,
            },
            Cmd::TurnFinished => {
                if matches!(self.diff_source, Some(DiffSource::Turn { .. })) {
                    self.show_turn().await;
                }
            }
            Cmd::Search {
                query,
                files,
                regex,
                case_sensitive,
            } => self.search(&ws, query, files, regex, case_sensitive),
            Cmd::SearchOpen(index) => self.open_hit(&ws, index),
            Cmd::SearchClosePreview => {
                self.preview_seq = self.next_seq();
                view::preview(&self.window, String::new(), Vec::new());
            }
            Cmd::TermKey { text, ctrl, alt } => self.term_key(&text, ctrl, alt),
            Cmd::TermResize { cols, rows } => self.term_resize(&ws, cols, rows).await,
            Cmd::TermScroll(lines) => {
                if let Some(terminal) = &mut self.terminal {
                    terminal.emu.scroll(lines);
                    terminal.dirty = true;
                    self.schedule_frame();
                }
            }
            Cmd::TermRestart => {
                self.term_kill(&ws).await;
                self.term_start(&ws).await;
            }
            Cmd::TermClose => {
                self.term_kill(&ws).await;
                if let Some(terminal) = &mut self.terminal {
                    terminal.status.running = false;
                    terminal.status.exited = true;
                    terminal.status.exit_code = -1;
                    view::term_status(&self.window, terminal.status.clone());
                }
            }
            Cmd::Shutdown | Cmd::SetRoot(_) => {}
        }
    }
}
