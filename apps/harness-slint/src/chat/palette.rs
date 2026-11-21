use super::{
    file_row, fuzzy, sv, ChatState, Effect, PaletteRow, SharedString, Target, MAX_RECENT, MAX_ROWS,
    SECTIONS,
};

impl ChatState {
    // ---- command center --------------------------------------------------------------------------------

    pub(super) fn palette_query(&mut self, query: &str, scope: &str) {
        self.palette_query = (query.to_string(), scope.to_string());
        if scope == "all" || scope == "files" {
            self.want_files();
        }
        let mut scored: Vec<(i32, usize, PaletteRow, Target)> = Vec::new();
        let mut push = |score: Option<i32>, row: PaletteRow, target: Target| {
            if let Some(score) = score {
                let order = scored.len();
                scored.push((score, order, row, target));
            }
        };
        let sections = query.is_empty();
        let header = |id: &str| PaletteRow {
            kind: sv("header"),
            id: sv(id),
            title: sv(id),
            detail: SharedString::new(),
            shortcut: SharedString::new(),
            icon: 0,
        };
        if scope == "all" || scope == "actions" {
            let action_row = |id: &str| PaletteRow {
                kind: sv("action"),
                id: sv(id),
                title: sv(&self.action_title(id)),
                detail: SharedString::new(),
                shortcut: sv(&crate::settings::shortcut_label(id)),
                icon: 0,
            };
            let recent: Vec<String> = self
                .recent
                .iter()
                .filter(|id| self.action_enabled(id))
                .cloned()
                .collect();
            if sections && !recent.is_empty() {
                push(Some(0), header("recent"), Target::Header);
                for id in &recent {
                    push(Some(0), action_row(id), Target::Action(id.clone()));
                }
            }
            for (section, ids) in SECTIONS {
                let ids: Vec<&str> = ids
                    .iter()
                    .copied()
                    .filter(|id| {
                        self.action_enabled(id) && !(sections && recent.iter().any(|r| r == id))
                    })
                    .collect();
                if sections && !ids.is_empty() {
                    push(Some(0), header(section), Target::Header);
                }
                for id in ids {
                    let row = action_row(id);
                    // 动作略微优先：同分时排在会话和文件之前。
                    let ranked = fuzzy::score(query, &row.title).map(|s| s + 2);
                    push(ranked, row, Target::Action(id.into()));
                }
            }
        }
        if sections && scope == "all" && !self.sessions.is_empty() {
            push(Some(0), header("sessions"), Target::Header);
        }
        if scope == "all" || scope == "sessions" {
            for (id, title) in &self.sessions {
                let row = PaletteRow {
                    kind: sv("session"),
                    id: sv(id),
                    title: sv(title),
                    detail: SharedString::new(),
                    shortcut: SharedString::new(),
                    icon: 0,
                };
                push(fuzzy::score(query, title), row, Target::Session(id.clone()));
            }
        }
        if scope == "all" || scope == "files" {
            // 空查询时不列出整个工作区（ZCode 也只在输入后搜索文件）；文件范围除外。
            if !query.is_empty() || scope == "files" {
                for path in self.files.iter().flatten() {
                    push(
                        fuzzy::score(query, path),
                        file_row(path),
                        Target::File(path.clone()),
                    );
                }
            }
        }
        if !query.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        scored.truncate(MAX_ROWS);
        self.palette_targets = scored.iter().map(|s| s.3.clone()).collect();
        self.palette_rows
            .set_vec(scored.into_iter().map(|s| s.2).collect::<Vec<_>>());
        let first = self
            .palette_targets
            .iter()
            .position(|t| !matches!(t, Target::Header))
            .unwrap_or_default();
        self.ui()
            .set_palette_index(i32::try_from(first).unwrap_or_default());
    }

    /// ↑ / ↓ in the command center: moves to the next runnable row, skipping section labels.
    pub(super) fn palette_step(&self, delta: i32) {
        let count = self.palette_targets.len();
        if count == 0 {
            return;
        }
        let mut index = usize::try_from(self.ui().get_palette_index()).unwrap_or_default();
        for _ in 0..count {
            index = if delta < 0 {
                (index + count - 1) % count
            } else {
                (index + 1) % count
            };
            if !matches!(self.palette_targets.get(index), Some(Target::Header)) {
                break;
            }
        }
        self.ui()
            .set_palette_index(i32::try_from(index).unwrap_or_default());
    }

    pub(super) fn palette_run(&mut self, index: usize) -> Effect {
        let Some(target) = self.palette_targets.get(index).cloned() else {
            return Effect::None;
        };
        if matches!(target, Target::Header) {
            return Effect::None;
        }
        self.ui().set_palette_open(false);
        match target {
            Target::Action(id) => {
                self.recent.retain(|r| r != &id);
                self.recent.insert(0, id.clone());
                self.recent.truncate(MAX_RECENT);
                Effect::Command(id)
            }
            Target::Session(id) => Effect::OpenSession(id),
            Target::File(path) => {
                self.add_file_chip(&path);
                Effect::None
            }
            Target::Agent(id) => Effect::ChooseAgent(id),
            Target::Slash(_) | Target::Skill(_) | Target::Header => Effect::None,
        }
    }
}
