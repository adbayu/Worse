use super::{
    image_mime, sv, ChatState, ChatUi, Chip, ChipRow, ComponentHandle, ContextItem,
    MentionCandidate, Path, SharedString, LONG_PASTE_CHARS, MAX_HISTORY,
};

impl ChatState {
    // ---- composer --------------------------------------------------------------------------------------

    pub(super) fn add_file_chip(&mut self, path: &str) {
        let exists = self.chip_data.iter().any(|c| match c {
            Chip::File(p) | Chip::Image { path: p, .. } => p == path,
            _ => false,
        });
        if exists {
            return;
        }
        let name = Path::new(path)
            .file_name()
            .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned());
        let (kind, chip) = match image_mime(path) {
            Some(mime) => (
                "image",
                Chip::Image {
                    path: path.to_string(),
                    mime: mime.to_string(),
                },
            ),
            None => ("file", Chip::File(path.to_string())),
        };
        self.chips.push(ChipRow {
            kind: sv(kind),
            label: sv(&name),
            detail: SharedString::new(),
        });
        self.chip_data.push(chip);
    }

    pub(super) fn add_session_chip(&mut self, id: &str) {
        if self
            .chip_data
            .iter()
            .any(|c| matches!(c, Chip::Session(s) if s == id))
        {
            return;
        }
        let title = self
            .sessions
            .iter()
            .find(|(sid, _)| sid == id)
            .map_or_else(|| id.to_string(), |(_, t)| t.clone());
        self.chips.push(ChipRow {
            kind: sv("session"),
            label: sv(&title),
            detail: SharedString::new(),
        });
        self.chip_data.push(Chip::Session(id.to_string()));
    }

    pub(super) fn add_skill_chip(&mut self, skill: &MentionCandidate) {
        if self
            .chip_data
            .iter()
            .any(|c| matches!(c, Chip::Skill(item) if *item == skill.item))
        {
            return;
        }
        self.chips.push(ChipRow {
            kind: sv("skill"),
            label: sv(&skill.label),
            detail: SharedString::new(),
        });
        self.chip_data.push(Chip::Skill(skill.item.clone()));
    }

    pub(super) fn draft_edited(&mut self, draft: &str) {
        if draft == self.last_draft {
            return;
        }
        let previous = std::mem::replace(&mut self.last_draft, draft.to_string());
        // 浏览历史时由 history() 写入的草稿不算编辑；用户真正改动后退出历史浏览。
        if let Some(pos) = self.history_pos {
            if self.history.get(pos).map_or(true, |h| h != draft) {
                self.history_pos = None;
            }
        }
        if self.long_paste(&previous, draft) {
            return;
        }
        self.update_mention(draft);
    }

    /// Moves a long pasted block out of the input into a "Pasted text" chip (`ZCode` long-paste attachment).
    pub(super) fn long_paste(&mut self, previous: &str, draft: &str) -> bool {
        let prefix = previous
            .char_indices()
            .zip(draft.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((i, a), _)| i + a.len_utf8());
        let tail = previous[prefix..]
            .chars()
            .rev()
            .zip(draft[prefix..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum::<usize>();
        let inserted = &draft[prefix..draft.len() - tail];
        if inserted.chars().count() < LONG_PASTE_CHARS {
            return false;
        }
        let lines = inserted.lines().count();
        self.chips.push(ChipRow {
            kind: sv("paste"),
            label: self
                .window
                .global::<ChatUi>()
                .invoke_action_label(sv("pastedText")),
            detail: sv(&format!("{lines} lines")),
        });
        self.chip_data.push(Chip::Paste(inserted.to_string()));
        let rest = format!("{}{}", &draft[..prefix], &draft[draft.len() - tail..]);
        self.last_draft.clone_from(&rest);
        self.window.set_draft(sv(&rest));
        true
    }

    pub(super) fn history(&mut self, delta: i32) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let draft = self.window.get_draft().to_string();
        let next = match (self.history_pos, delta < 0) {
            (None, true) if draft.is_empty() => {
                self.history_stash = draft;
                Some(self.history.len() - 1)
            }
            (Some(pos), true) => Some(pos.saturating_sub(1)),
            (Some(pos), false) if pos + 1 < self.history.len() => Some(pos + 1),
            (Some(_), false) => {
                // 越过最新一条：回到浏览前的草稿。
                self.history_pos = None;
                let stash = std::mem::take(&mut self.history_stash);
                self.last_draft.clone_from(&stash);
                self.window.set_draft(sv(&stash));
                return true;
            }
            _ => None,
        };
        let Some(pos) = next else {
            return false;
        };
        self.history_pos = Some(pos);
        let text = self.history[pos].clone();
        self.last_draft.clone_from(&text);
        self.window.set_draft(sv(&text));
        true
    }

    /// Final message: pasted text is folded into the text; files, images and sessions become ACP context items
    /// (`Command::SendContext`, spec §9.2.1). The prompt is recorded in the history.
    pub(super) fn prepare_send(&mut self, text: &str) -> (String, Vec<ContextItem>) {
        let mut out = text.trim_end().to_string();
        let mut context = Vec::new();
        for chip in &self.chip_data {
            match chip {
                Chip::Paste(data) => {
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    out.push_str(data);
                }
                Chip::File(path) => context.push(ContextItem::File { path: path.clone() }),
                Chip::Image { path, mime } => context.push(ContextItem::Image {
                    path: Some(path.clone()),
                    data_base64: None,
                    mime: mime.clone(),
                }),
                Chip::Session(id) => context.push(ContextItem::Session {
                    session_id: harness_core_api::SessionId::new(id.as_str()),
                }),
                Chip::Skill(item) => context.push(item.clone()),
            }
        }
        self.chips.set_vec(Vec::new());
        self.chip_data.clear();
        self.ui().set_mention_open(false);
        if !text.trim().is_empty() && self.history.last().map_or(true, |h| h != text) {
            self.history.push(text.to_string());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }
        self.history_pos = None;
        self.last_draft.clear();
        (out, context)
    }

    /// Puts a rewound turn back into the composer: its text and its context items as chips.
    pub(super) fn restore(&mut self, text: &str, items: Vec<ContextItem>) {
        self.chips.set_vec(Vec::new());
        self.chip_data.clear();
        for item in items {
            match item {
                ContextItem::File { path }
                | ContextItem::Folder { path }
                | ContextItem::Image {
                    path: Some(path), ..
                } => {
                    self.add_file_chip(&path);
                }
                ContextItem::Image { path: None, .. }
                | ContextItem::Research
                | ContextItem::CodeMode => {}
                ContextItem::Text { text, .. } => {
                    let lines = text.lines().count();
                    self.chips.push(ChipRow {
                        kind: sv("paste"),
                        label: self
                            .window
                            .global::<ChatUi>()
                            .invoke_action_label(sv("pastedText")),
                        detail: sv(&format!("{lines} lines")),
                    });
                    self.chip_data.push(Chip::Paste(text));
                }
                ContextItem::Session { session_id } => self.add_session_chip(session_id.as_str()),
            }
        }
        self.last_draft = text.to_string();
        self.window.set_draft(sv(text));
    }
}
