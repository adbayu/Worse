use super::{
    file_row, fuzzy, sv, ChatState, ChatUi, ComponentHandle, Effect, MentionCandidate, MentionKind,
    PaletteRow, SharedString, Target, MAX_ROWS, SLASH,
};

impl ChatState {
    // Builds the "/" (commands) or "@" (files, sessions, agents) popup; one flat pass per row source.
    #[allow(clippy::too_many_lines)]
    pub(super) fn update_mention(&mut self, draft: &str) {
        let window = self.window.clone_strong();
        let ui = window.global::<ChatUi>();
        let slash = draft.starts_with('/') && !draft.contains(char::is_whitespace);
        let token_start = draft.rfind(char::is_whitespace).map_or(0, |i| {
            i + draft[i..].chars().next().map_or(1, char::len_utf8)
        });
        let token = &draft[token_start..];
        let trigger = if slash {
            "/"
        } else if token.starts_with('@') {
            "@"
        } else {
            ui.set_mention_open(false);
            return;
        };
        let query = &token[1..];
        self.mention_start = token_start;
        let mut rows: Vec<(i32, usize, PaletteRow, Target)> = Vec::new();
        if trigger == "/" {
            for (name, action) in SLASH {
                if let Some(score) = fuzzy::score(query, name) {
                    let order = rows.len();
                    rows.push((
                        score,
                        order,
                        PaletteRow {
                            kind: sv("command"),
                            id: sv(name),
                            title: sv(&format!("/{name}")),
                            detail: sv(&self.action_title(action)),
                            shortcut: SharedString::new(),
                            icon: 0,
                        },
                        Target::Action(action.into()),
                    ));
                }
            }
            // R13：agent 广播的命令与 CLI 配置里的命令（hub `list_commands`），写入草稿后随消息发给 agent。
            for (name, description) in &self.agent_commands {
                if let Some(score) = fuzzy::score(query, name) {
                    let order = rows.len();
                    rows.push((
                        score,
                        order,
                        PaletteRow {
                            kind: sv("command"),
                            id: sv(name),
                            title: sv(&format!("/{name}")),
                            detail: sv(description),
                            shortcut: SharedString::new(),
                            icon: 0,
                        },
                        Target::Slash(name.clone()),
                    ));
                }
            }
        } else {
            for (id, title) in &self.sessions {
                if self.active.as_deref() == Some(id.as_str()) {
                    continue;
                }
                let shown = if title.is_empty() { id } else { title };
                if let Some(score) = fuzzy::score(query, shown) {
                    let order = rows.len();
                    rows.push((
                        score + 2,
                        order,
                        PaletteRow {
                            kind: sv("session"),
                            id: sv(id),
                            title: sv(shown),
                            detail: sv(id),
                            shortcut: SharedString::new(),
                            icon: 0,
                        },
                        Target::Session(id.clone()),
                    ));
                }
            }
            self.want_files();
            for (id, name) in crate::bridge::agent_names() {
                if let Some(score) = fuzzy::score(query, &name).or_else(|| fuzzy::score(query, &id))
                {
                    let order = rows.len();
                    rows.push((
                        score + 4,
                        order,
                        PaletteRow {
                            kind: sv("agent"),
                            id: sv(&id),
                            title: sv(&name),
                            detail: sv(&id),
                            shortcut: SharedString::new(),
                            icon: 0,
                        },
                        Target::Agent(id),
                    ));
                }
            }
            for path in self.files.iter().flatten() {
                if let Some(score) = fuzzy::score(query, path) {
                    let order = rows.len();
                    rows.push((score, order, file_row(path), Target::File(path.clone())));
                }
            }
            // R13：技能与子 agent 排在会话、agent 与文件之后（同分时更靠后）。
            for (index, skill) in self.skills.iter().enumerate() {
                if let Some(score) = fuzzy::score(query, &skill.label) {
                    let order = rows.len();
                    let kind = if skill.kind == MentionKind::Subagent {
                        "subagent"
                    } else {
                        "skill"
                    };
                    rows.push((
                        score - 1,
                        order,
                        PaletteRow {
                            kind: sv(kind),
                            id: sv(&skill.label),
                            title: sv(&skill.label),
                            detail: sv(&skill.detail),
                            shortcut: SharedString::new(),
                            icon: 0,
                        },
                        Target::Skill(index),
                    ));
                }
            }
        }
        if !query.is_empty() {
            rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        rows.truncate(MAX_ROWS);
        self.mention_targets = rows.iter().map(|r| r.3.clone()).collect();
        self.mention_rows
            .set_vec(rows.into_iter().map(|r| r.2).collect::<Vec<_>>());
        ui.set_mention_trigger(sv(trigger));
        ui.set_mention_index(0);
        ui.set_mention_open(true);
    }

    pub(super) fn mention_pick(&mut self, index: usize) -> Effect {
        let Some(target) = self.mention_targets.get(index).cloned() else {
            return Effect::None;
        };
        self.ui().set_mention_open(false);
        let draft = self.last_draft.clone();
        let head = draft
            .get(..self.mention_start)
            .unwrap_or_default()
            .to_string();
        let (next, effect) = match target {
            // 文件：以 @路径 写入正文（agent CLI 都识别 @path），同时不额外加附件。
            Target::File(path) => (format!("{head}@{path} "), Effect::None),
            // agent：本条消息改由该 agent 处理（切换会话的 sticky agent），移除 @ 记号。
            Target::Agent(id) => (head, Effect::ChooseAgent(id)),
            // 斜杠命令：立即执行并清空输入。
            Target::Action(id) => (String::new(), Effect::Command(id)),
            // 会话：作为 #session 上下文附件（ContextItem::Session），移除 @ 记号。
            Target::Session(id) => {
                self.add_session_chip(&id);
                (head, Effect::None)
            }
            Target::Slash(name) => (format!("/{name} "), Effect::None),
            // 子 agent 插入 `@agent-<name>`（Claude Code 语法），不加附件；技能作为 SKILL.md 资源附件。
            Target::Skill(index) => match self.skills.get(index).cloned() {
                Some(MentionCandidate {
                    insert_text: Some(insert),
                    ..
                }) => (format!("{head}{insert} "), Effect::None),
                Some(skill) => {
                    self.add_skill_chip(&skill);
                    (head, Effect::None)
                }
                None => (draft, Effect::None),
            },
            Target::Header => (draft, Effect::None),
        };
        self.last_draft.clone_from(&next);
        self.window.set_draft(sv(&next));
        effect
    }
}
