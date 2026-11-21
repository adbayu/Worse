use super::{
    sv, ChatCall, Command, DialogTarget, GroupColor, GroupRow, ModelRc, SessionGroup, SessionId,
    SessionRow, SessionSelection, SettingsCall, UiState, UpdateGroup, VecModel,
};

impl UiState {
    // ---- sessions (R10 / R11) ------------------------------------------------------------------------

    pub(in crate::bridge) fn session_action(&mut self, action: &str, id: &str) {
        let Some(session) = self.target_session(id) else {
            return;
        };
        let title = self
            .summaries
            .iter()
            .find(|s| s.id == session)
            .map(|s| s.title.clone())
            .unwrap_or_default();
        match action {
            "rename" => self.open_dialog(DialogTarget::Rename(session), &title, "", false),
            "pin" | "unpin" => self.chat_call(
                |reply| ChatCall::SetPinned {
                    sessions: vec![session],
                    pinned: action == "pin",
                    reply,
                },
                |_, ()| {},
            ),
            "unread" => {
                self.turn.keep_unread = Some(session.clone());
                self.set_unread(&session, true);
            }
            "toggle-pin" => {
                let pinned = self.summaries.iter().any(|s| s.id == session && s.pinned);
                self.session_action(if pinned { "unpin" } else { "pin" }, &session);
            }
            "fork" => self.fork(session, None),
            "export" => self.export_session(session),
            "delete" => self.open_dialog(DialogTarget::Delete(session), "", "", false),
            "ungroup" => self.move_sessions(session, None),
            other => {
                if let Some(group) = other.strip_prefix("group:") {
                    self.move_sessions(session, Some(group.to_string()));
                }
            }
        }
    }

    pub(super) fn move_sessions(&self, session: SessionId, group: Option<String>) {
        self.chat_call(
            |reply| ChatCall::MoveSessions {
                sessions: vec![session],
                group,
                reply,
            },
            |_, ()| {},
        );
    }

    /// Delete = archive, then delete the archived copy (hub keeps deletion behind the archive).
    pub(super) fn delete_session(&self, session: SessionId) {
        let again = session.clone();
        self.request(
            |reply| Command::Settings(SettingsCall::ArchiveSessions(vec![session], reply)),
            move |s, ()| {
                s.request(
                    |reply| {
                        Command::Settings(SettingsCall::DeleteArchived(
                            SessionSelection::Sessions(vec![harness_core_api::SessionId::new(
                                again,
                            )]),
                            reply,
                        ))
                    },
                    |_, _count: u32| crate::settings::sessions_changed(),
                );
            },
        );
    }

    pub(in crate::bridge) fn load_groups(&self) {
        self.chat_call(ChatCall::ListGroups, |s, mut groups: Vec<SessionGroup>| {
            groups.sort_by_key(|g| g.position);
            s.turn.groups = groups;
            let rows: Vec<GroupRow> = s
                .turn
                .groups
                .iter()
                .map(|g| GroupRow {
                    id: sv(&g.id),
                    name: sv(&g.name),
                    color: sv(g.color.as_str()),
                })
                .collect();
            s.ui().set_groups(ModelRc::new(VecModel::from(rows)));
            s.refresh_sessions();
        });
    }

    pub(super) fn update_group(&self, update: UpdateGroup) {
        self.chat_call(
            |reply| ChatCall::UpdateGroup(update, reply),
            |s, _| s.load_groups(),
        );
    }

    pub(super) fn group_action(&mut self, action: &str, id: &str) {
        if action == "new-session" {
            if !id.is_empty() {
                self.new_session(false, Some(std::path::PathBuf::from(id)));
            }
            return;
        }
        let name = self
            .turn
            .groups
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.name.clone())
            .unwrap_or_default();
        match action {
            "view" => self.refresh_sessions(),
            "new" => self.open_dialog(DialogTarget::NewGroup, "", "", false),
            "rename" => {
                self.open_dialog(DialogTarget::RenameGroup(id.to_string()), &name, "", false);
            }
            "delete" => self.open_dialog(DialogTarget::DeleteGroup(id.to_string()), "", "", false),
            "toggle" => {
                if !self.turn.collapsed.remove(id) {
                    self.turn.collapsed.insert(id.to_string());
                }
                self.refresh_sessions();
            }
            "up" | "down" => {
                let mut order: Vec<String> =
                    self.turn.groups.iter().map(|g| g.id.clone()).collect();
                let Some(from) = order.iter().position(|g| g == id) else {
                    return;
                };
                let to = if action == "up" {
                    from.checked_sub(1)
                } else {
                    Some(from + 1).filter(|&t| t < order.len())
                };
                if let Some(to) = to {
                    order.swap(from, to);
                    self.chat_call(
                        |reply| ChatCall::ReorderGroups(order, reply),
                        |s, ()| s.load_groups(),
                    );
                }
            }
            other => {
                if let Some(color) = other.strip_prefix("color:") {
                    self.update_group(UpdateGroup {
                        id: id.to_string(),
                        name: None,
                        color: Some(GroupColor::parse(color)),
                    });
                }
            }
        }
    }

    /// Drag a session onto a group header (or the session list): moves it there.
    pub(super) fn drop_session(&self, session: &str, group: &str) {
        let group = (!group.is_empty()).then(|| group.to_string());
        self.move_sessions(session.to_string(), group);
    }

    /// Projects tree of the sidebar: the given sessions by `project` (workspace key) in first-seen order, each with
    /// its header row (folder name, session count; collapse is UI-local).
    pub(super) fn project_sections<'a>(
        &self,
        sessions: Vec<&'a crate::core::SessionSummary>,
    ) -> Vec<(SessionRow, Vec<&'a crate::core::SessionSummary>)> {
        let mut projects: Vec<(&str, Vec<&crate::core::SessionSummary>)> = Vec::new();
        for s in sessions {
            match projects.iter_mut().find(|(key, _)| *key == s.project) {
                Some((_, list)) => list.push(s),
                None => projects.push((s.project.as_str(), vec![s])),
            }
        }
        projects
            .into_iter()
            .map(|(key, members)| {
                let id = format!("project:{key}");
                let folder = members
                    .iter()
                    .find(|s| s.branch.is_none() && !s.workspace_path.is_empty())
                    .map_or(key, |s| s.workspace_path.as_str());
                let name = folder
                    .rsplit(['/', '\\'])
                    .find(|part| !part.is_empty())
                    .unwrap_or(folder);
                let header = SessionRow {
                    header: sv("project"),
                    header_id: sv(&id),
                    header_label: sv(name),
                    header_color: sv(folder),
                    header_count: i32::try_from(members.len()).unwrap_or(i32::MAX),
                    collapsed: self.turn.collapsed.contains(&id),
                    ..SessionRow::default()
                };
                (header, members)
            })
            .collect()
    }

    /// Sidebar rows: Pinned, then the user groups (by position; collapsed ones show their header only), then the
    /// timeline (Today / Yesterday / …). Returns the rows and the displayed session order.
    pub(in crate::bridge) fn session_rows(&self) -> (Vec<SessionRow>, Vec<(String, String)>) {
        let mut rows = Vec::new();
        let mut order = Vec::new();
        let base = |s: &crate::core::SessionSummary| {
            let (_, unit, ago) = crate::chat::timeline(s.updated_ms);
            SessionRow {
                id: sv(&s.id),
                title: sv(&s.title),
                agents: sv(&s
                    .agents_used
                    .iter()
                    .map(|a| self.agent_name(a))
                    .collect::<Vec<_>>()
                    .join(" · ")),
                active: self.is_active(&s.id),
                ago_unit: sv(unit),
                ago_n: ago,
                pinned: s.pinned,
                unread: s.unread,
                worktree: s.branch.is_some(),
                ..SessionRow::default()
            }
        };
        let known_group = |g: &Option<String>| {
            g.as_ref()
                .is_some_and(|id| self.turn.groups.iter().any(|x| &x.id == id))
        };
        let mut push = |rows: &mut Vec<SessionRow>,
                        list: Vec<&crate::core::SessionSummary>,
                        header: SessionRow| {
            let mut first = Some(header);
            for s in list {
                let mut row = base(s);
                if let Some(h) = first.take() {
                    row.header = h.header;
                    row.header_id = h.header_id;
                    row.header_label = h.header_label;
                    row.header_color = h.header_color;
                    row.header_count = h.header_count;
                }
                order.push((s.id.clone(), s.title.clone()));
                rows.push(row);
            }
        };
        let pinned: Vec<_> = self.summaries.iter().filter(|s| s.pinned).collect();
        push(
            &mut rows,
            pinned,
            SessionRow {
                header: sv("pinned"),
                ..SessionRow::default()
            },
        );
        for group in &self.turn.groups {
            let collapsed = self.turn.collapsed.contains(&group.id);
            let header = SessionRow {
                header: sv("group"),
                header_id: sv(&group.id),
                header_label: sv(&group.name),
                header_color: sv(group.color.as_str()),
                collapsed,
                ..SessionRow::default()
            };
            let members: Vec<_> = self
                .summaries
                .iter()
                .filter(|s| !s.pinned && s.group_id.as_deref() == Some(group.id.as_str()))
                .collect();
            if collapsed || members.is_empty() {
                rows.push(header);
            } else {
                push(&mut rows, members, header);
            }
        }
        let rest: Vec<&crate::core::SessionSummary> = self
            .summaries
            .iter()
            .filter(|s| !s.pinned && !known_group(&s.group_id))
            .collect();
        if self.ui().get_by_project() {
            // 项目树：其余会话按 `project`（工作区 key）分组，组内保持 hub 的顺序；折叠状态是界面局部状态。
            for (header, members) in self.project_sections(rest) {
                if header.collapsed {
                    rows.push(header);
                } else {
                    push(&mut rows, members, header);
                }
            }
            return (rows, order);
        }
        let mut last_group = "";
        for s in rest {
            let (group, _, _) = crate::chat::timeline(s.updated_ms);
            let mut row = base(s);
            if group != last_group {
                row.header = sv(group);
            }
            last_group = group;
            order.push((s.id.clone(), s.title.clone()));
            rows.push(row);
        }
        (rows, order)
    }
}
