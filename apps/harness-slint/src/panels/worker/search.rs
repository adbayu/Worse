use super::{
    content_cache, error_text, view, Done, Hit, SearchText, Worker, Workspace, WorkspaceServices,
    FILE_HITS, MAX_HITS, PREVIEW_AFTER, PREVIEW_BEFORE, PREVIEW_FILE_LINES,
};

impl Worker {
    // ---- search -----------------------------------------------------------------------------------------------

    pub(super) fn search(
        &mut self,
        ws: &Workspace,
        query: String,
        files: bool,
        regex: bool,
        case_sensitive: bool,
    ) {
        self.search_seq = self.next_seq();
        let seq = self.search_seq;
        if let Some(previous) = self.search_id.take() {
            let ws = ws.clone();
            tokio::spawn(async move {
                let _ = ws.cancel_search(previous).await;
            });
        }
        self.hits.clear();
        self.search_files = files;
        if query.trim().is_empty() {
            view::search(
                &self.window,
                Vec::new(),
                false,
                String::new(),
                String::new(),
            );
            return;
        }
        view::search(&self.window, Vec::new(), true, String::new(), String::new());
        let ws = ws.clone();
        let done = self.done_tx.clone();
        if files {
            tokio::spawn(async move {
                let result = ws.search_files(query, FILE_HITS).await.map(|hits| {
                    hits.into_iter()
                        .map(|h| Hit {
                            path: h.path,
                            is_dir: h.is_dir,
                            ..Hit::default()
                        })
                        .collect()
                });
                let _ = done.send(Done::Files(seq, result));
            });
        } else {
            let search_id = format!("panel-{seq}");
            self.search_id = Some(search_id.clone());
            tokio::spawn(async move {
                let request = SearchText {
                    search_id,
                    query,
                    is_regex: regex,
                    case_sensitive,
                    max_results: u32::try_from(MAX_HITS).unwrap_or(500),
                };
                let result = ws.search_text(request).await.map(|r| {
                    let hits = r
                        .matches
                        .into_iter()
                        .map(|m| Hit {
                            path: m.path,
                            line: m.line,
                            column: m.column,
                            text: m.text,
                            is_dir: false,
                        })
                        .collect();
                    (hits, r.match_count, r.file_count, r.truncated)
                });
                let _ = done.send(Done::Text(seq, result));
            });
        }
    }

    pub(super) fn open_hit(&mut self, ws: &Workspace, index: usize) {
        let Some(hit) = self.hits.get(index).cloned() else {
            return;
        };
        if hit.is_dir {
            return;
        }
        self.preview_seq = self.next_seq();
        let seq = self.preview_seq;
        let ws = ws.clone();
        let done = self.done_tx.clone();
        tokio::spawn(async move {
            let result = match ws.read_file(hit.path.clone(), 0).await {
                Ok(file) if file.binary => Err("Binary file".to_owned()),
                Ok(file) => {
                    let (first, last) = if hit.line > 0 {
                        (
                            hit.line.saturating_sub(PREVIEW_BEFORE).max(1),
                            hit.line + PREVIEW_AFTER,
                        )
                    } else {
                        (1, PREVIEW_FILE_LINES)
                    };
                    Ok(file
                        .text
                        .lines()
                        .zip(1_u64..)
                        .skip_while(|(_, no)| *no < first)
                        .take_while(|(_, no)| *no <= last)
                        .map(|(text, no)| (no, text.replace('\t', "    "), no == hit.line))
                        .collect())
                }
                Err(error) => Err(error_text(&error)),
            };
            let _ = done.send(Done::Preview(seq, hit.path, result));
        });
    }

    pub(super) fn search_summary(&self, done: Option<(u64, u64, bool)>) -> String {
        let mut out = match done {
            Some((matches, files, truncated)) => {
                let more = if truncated { "+" } else { "" };
                format!("{matches}{more} matches in {files} files")
            }
            None if self.search_files => format!("{} files", self.hits.len()),
            None => format!("{} matches…", self.hits.len()),
        };
        if !self.search_files {
            return out;
        }
        if let Some(warning) = self.content_summary() {
            out.push_str(" · ");
            out.push_str(&warning);
        }
        out
    }

    /// `n files have content that does not match the name`, over the visible file hits.
    pub(super) fn content_summary(&self) -> Option<String> {
        let root = self.root.as_ref()?;
        let mut suspect = 0_u32;
        for hit in self.hits.iter().filter(|h| !h.is_dir) {
            if content_cache()
                .inspect(&root.join(&hit.path))
                .warning
                .is_some()
            {
                suspect += 1;
            }
        }
        (suspect > 0).then(|| {
            if suspect == 1 {
                "1 file has content that does not match its name".to_owned()
            } else {
                format!("{suspect} files have content that does not match their names")
            }
        })
    }
}
