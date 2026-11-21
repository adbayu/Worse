use super::{
    error_text, mpsc, shell_name, term, view, Emulator, Instant, TermFrame, TermOp, TermStatus,
    Terminal, TerminalCreate, Worker, Workspace, WorkspaceServices, FRAME,
};

impl Worker {
    // ---- terminal ---------------------------------------------------------------------------------------------

    /// First size from the panel = the panel was opened: start the shell (lazy).
    pub(super) async fn term_resize(&mut self, ws: &Workspace, cols: u16, rows: u16) {
        self.size = Some((cols, rows));
        match &mut self.terminal {
            None => self.term_start(ws).await,
            Some(terminal) => {
                terminal.emu.resize(cols, rows);
                terminal.dirty = true;
                if let (Some(id), Some(tx)) = (&terminal.id, &self.term_tx) {
                    let _ = tx.send(TermOp::Resize(id.clone(), cols, rows));
                }
                self.schedule_frame();
            }
        }
    }

    pub(super) async fn term_start(&mut self, ws: &Workspace) {
        let (cols, rows) = self.size.unwrap_or((80, 24));
        if self.term_tx.is_none() {
            let (tx, mut rx) = mpsc::unbounded_channel::<TermOp>();
            let ws = ws.clone();
            tokio::spawn(async move {
                while let Some(op) = rx.recv().await {
                    let result = match op {
                        TermOp::Write(id, data) => ws.terminal_write(id, data).await,
                        TermOp::Resize(id, cols, rows) => ws.terminal_resize(id, cols, rows).await,
                    };
                    if let Err(error) = result {
                        // The shell may just have exited; the exit event reports that.
                        let _ = error;
                    }
                }
            });
            self.term_tx = Some(tx);
        }
        // Created inline (not spawned): the id is known before the loop handles the shell's first output, so
        // no early output can be dropped as "unknown terminal".
        let created = ws
            .terminal_create(TerminalCreate {
                cols,
                rows,
                cwd: None,
                shell: None,
            })
            .await;
        let (id, status) = match created {
            Ok(info) => (
                Some(info.id),
                TermStatus {
                    running: true,
                    title: shell_name(&info.shell),
                    ..TermStatus::default()
                },
            ),
            Err(error) => (
                None,
                TermStatus {
                    error: format!("Cannot start a shell: {}", error_text(&error)),
                    ..TermStatus::default()
                },
            ),
        };
        view::term_status(&self.window, status.clone());
        self.terminal = Some(Terminal {
            id,
            emu: Emulator::new(cols, rows),
            shown: Vec::new(),
            status,
            dirty: true,
        });
        self.schedule_frame();
    }
    pub(super) async fn term_kill(&mut self, ws: &Workspace) {
        if let Some(id) = self.terminal.as_mut().and_then(|t| t.id.take()) {
            let _ = ws.terminal_close(id).await;
        }
    }

    pub(super) fn term_key(&mut self, text: &str, ctrl: bool, alt: bool) {
        let Some(terminal) = &mut self.terminal else {
            return;
        };
        let (Some(id), Some(tx)) = (&terminal.id, &self.term_tx) else {
            return;
        };
        if let Some(bytes) = term::key_bytes(text, ctrl, alt, terminal.emu.app_cursor()) {
            terminal.emu.scroll_to_bottom();
            terminal.dirty = true;
            let _ = tx.send(TermOp::Write(id.clone(), bytes));
            self.schedule_frame();
        }
    }

    pub(super) fn schedule_frame(&mut self) {
        if self.frame_due.is_none() {
            self.frame_due = Some(Instant::now() + FRAME);
        }
    }

    /// Repaints the dirty terminal rows and the streaming search hits (at most once per frame).
    pub(super) fn flush(&mut self) {
        self.frame_due = None;
        if let Some(terminal) = &mut self.terminal {
            if terminal.dirty {
                terminal.dirty = false;
                let screen = terminal.emu.screen();
                let full = terminal.shown.len() != screen.lines.len();
                let changed = screen
                    .lines
                    .iter()
                    .enumerate()
                    .filter(|(i, line)| full || terminal.shown.get(*i) != Some(*line))
                    .map(|(i, line)| (i, line.clone()))
                    .collect();
                let rows = screen.lines.len();
                terminal.shown = screen.lines;
                let cursor = screen.cursor.filter(|_| terminal.status.running);
                view::term_frame(
                    &self.window,
                    TermFrame {
                        rows,
                        changed,
                        cursor,
                    },
                );
            }
        }
        if self.search_dirty {
            self.search_dirty = false;
            view::search(
                &self.window,
                self.hits.clone(),
                true,
                self.search_summary(None),
                String::new(),
            );
        }
    }
}
