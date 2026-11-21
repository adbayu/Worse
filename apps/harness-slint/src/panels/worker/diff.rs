use super::{
    diffmodel, error_text, file_view, view, with_content_warning, with_hub, DiffSource, DiffView,
    Done, TurnStatus, Worker, WorkspaceServices,
};

impl Worker {
    // ---- diff -------------------------------------------------------------------------------------------------

    pub(super) fn load_diff(&mut self, source: DiffSource) {
        let Some(ws) = self.ws.clone() else {
            return;
        };
        self.diff_seq = self.next_seq();
        let seq = self.diff_seq;
        view::diff_loading(&self.window);
        let done = self.done_tx.clone();
        match source.clone() {
            DiffSource::Git { path, staged } => {
                tokio::spawn(async move {
                    let result = ws
                        .git_file_diff(path, staged)
                        .await
                        .map(|d| vec![d])
                        .map_err(|e| error_text(&e));
                    let _ = done.send(Done::Diff(seq, source, result));
                });
            }
            DiffSource::Turn { session, turn, .. } => {
                let Some(core) = with_hub(|hub| hub.core.clone()) else {
                    self.diff_notice("Turn diffs need the agent hub (not attached).");
                    return;
                };
                tokio::spawn(async move {
                    let result = match core.turn_diff(session, turn).await {
                        Ok(diff) if diff.available => Ok(diff.files),
                        Ok(diff) => Err(diff
                            .reason
                            .unwrap_or_else(|| "No snapshot for this turn".into())),
                        Err(error) => Err(error_text(&error)),
                    };
                    let _ = done.send(Done::Diff(seq, source, result));
                });
            }
        }
    }

    /// Diff of the last finished turn: of the session shown in the chat when known, else of this run.
    pub(super) async fn show_turn(&mut self) {
        let (core, active, last) = with_hub(|hub| {
            (
                hub.core.clone(),
                hub.active_session.clone(),
                hub.last_turn.clone(),
            )
        });
        let Some(core) = core else {
            self.diff_notice("Turn diffs need the agent hub (not attached).");
            return;
        };
        let mut target = last;
        if let Some(session) = active {
            if let Ok(detail) = core.open_session(session.clone()).await {
                let finished = detail.turns.iter().rev().find(|t| {
                    matches!(
                        t.status,
                        TurnStatus::Completed | TurnStatus::Cancelled | TurnStatus::Failed
                    )
                });
                if let Some(turn) = finished {
                    target = Some((session, turn.id.clone()));
                }
            }
        }
        let Some((session, turn)) = target else {
            self.diff_notice("No finished turn yet.");
            return;
        };
        let label = format!("Turn {}", turn.0.chars().take(8).collect::<String>());
        self.load_diff(DiffSource::Turn {
            session,
            turn,
            label,
        });
    }

    pub(super) fn diff_notice(&mut self, text: &str) {
        self.diff_seq = self.next_seq();
        self.diff_source = None;
        self.diff_files.clear();
        view::diff(
            &self.window,
            DiffView {
                source: "turn",
                title: "Turn changes".into(),
                notice: text.into(),
                ..DiffView::default()
            },
        );
    }

    pub(super) fn render_diff(&self) {
        let (source, title, subtitle) = match &self.diff_source {
            Some(DiffSource::Git { path, staged }) => (
                "git",
                path.clone(),
                if *staged { "Staged" } else { "Working tree" }.to_owned(),
            ),
            Some(DiffSource::Turn { label, .. }) => {
                ("turn", "Turn changes".to_owned(), label.clone())
            }
            None => ("", String::new(), String::new()),
        };
        let added = self.diff_files.iter().map(|f| u64::from(f.added)).sum();
        let removed = self.diff_files.iter().map(|f| u64::from(f.removed)).sum();
        let files = self
            .diff_files
            .iter()
            .map(|f| {
                (
                    f.path.clone(),
                    diffmodel::status_code(f.status),
                    u64::from(f.added),
                    u64::from(f.removed),
                )
            })
            .collect();
        let mut out = DiffView {
            source,
            title,
            subtitle,
            added,
            removed,
            files,
            current_file: self.diff_current,
            ..DiffView::default()
        };
        match self.diff_files.get(self.diff_current) {
            None => out.notice = "No changes.".into(),
            Some(file) => {
                let fv = file_view(file);
                out.notice = match fv.notice {
                    Some("binary") => "Binary file — no text diff.".into(),
                    Some(_) => "No textual changes.".into(),
                    None if fv.cut => format!("Diff cut at {} rows.", diffmodel::MAX_ROWS),
                    None => String::new(),
                };
                out.rows = fv.rows;
                out.pairs = fv.pairs;
                out.notice = with_content_warning(
                    self.root.as_ref(),
                    &file.path,
                    std::mem::take(&mut out.notice),
                );
            }
        }
        view::diff(&self.window, out);
    }
}
