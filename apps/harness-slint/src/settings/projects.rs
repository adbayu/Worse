//! Projects page: list, selection, removal and undo; also the session-change refresh hook.

use super::util::{date_of, folder_name, same_path, short_path};
use super::{
    api, model, sv, with_state, AppWindow, ComponentHandle, ProjectRow, ProjectsUi, Req, State,
};

impl State {
    // ---- projects ----------------------------------------------------------------------------------------

    pub(super) fn refresh_projects(&mut self) {
        self.list_seq += 1;
        let query = self.window.global::<ProjectsUi>().get_query().to_string();
        self.send(Req::Projects {
            seq: self.list_seq,
            query,
        });
    }

    pub(super) fn projects_listed(
        &mut self,
        seq: u64,
        result: Result<Vec<api::ProjectInfo>, String>,
    ) {
        if seq != self.list_seq {
            return;
        }
        let ui = self.window.global::<ProjectsUi>();
        match result {
            Ok(list) => {
                ui.set_available(true);
                self.selected
                    .retain(|k| list.iter().any(|p| &p.workspace_key == k));
                self.projects = list;
                self.show_projects();
            }
            Err(error) => {
                // hub 不可用时退回只读：不显示任何可操作的行，错误写到状态栏。
                ui.set_available(false);
                self.projects.clear();
                self.selected.clear();
                self.show_projects();
                self.status(&error, true);
            }
        }
    }

    pub(super) fn show_projects(&self) {
        let ui = self.window.global::<ProjectsUi>();
        let unused_only = ui.get_unused_only();
        let open = crate::bridge::workspace_path();
        let rows: Vec<ProjectRow> = self
            .projects
            .iter()
            .filter(|p| !unused_only || p.unused)
            .map(|p| ProjectRow {
                id: sv(&p.workspace_key),
                name: sv(folder_name(&p.workspace_path)),
                path: sv(&p.workspace_path),
                short: sv(short_path(&p.workspace_path)),
                sessions: i32::try_from(p.sessions).unwrap_or(i32::MAX),
                last_used: sv(p.last_used.map(date_of).unwrap_or_default()),
                unused: p.unused,
                selected: self.selected.contains(&p.workspace_key),
                current: same_path(&p.workspace_path, &open),
            })
            .collect();
        let unused = self.projects.iter().filter(|p| p.unused).count();
        ui.set_unused_count(i32::try_from(unused).unwrap_or(i32::MAX));
        let total: u64 = self.projects.iter().map(|p| u64::from(p.sessions)).sum();
        ui.set_total_sessions(i32::try_from(total).unwrap_or(i32::MAX));
        ui.set_selected(i32::try_from(self.selected.len()).unwrap_or(i32::MAX));
        ui.set_rows(model(rows));
    }

    pub(super) fn select_only_project(&mut self, key: &str) {
        self.selected.clear();
        self.selected.insert(key.to_string());
        self.show_projects();
    }

    pub(super) fn select_project(&mut self, key: &str, on: bool) {
        if on {
            self.selected.insert(key.to_string());
        } else {
            self.selected.remove(key);
        }
        self.show_projects();
    }

    pub(super) fn remove_projects(&mut self, delete_data: bool) {
        let keys: Vec<String> = self.selected.iter().cloned().collect();
        if keys.is_empty() {
            return;
        }
        self.send(Req::RemoveProjects { keys, delete_data });
    }

    pub(super) fn projects_removed(
        &mut self,
        keys: Vec<String>,
        delete_data: bool,
        result: Result<(), String>,
    ) {
        match result {
            Ok(()) => {
                self.selected.clear();
                // 删除数据不可撤销，只有“移除”提供撤销。
                self.removed = if delete_data { Vec::new() } else { keys };
                let undo = i32::try_from(self.removed.len()).unwrap_or(i32::MAX);
                self.window.global::<ProjectsUi>().set_undo_count(undo);
                self.refresh_projects();
            }
            Err(error) => self.status(&error, true),
        }
    }

    pub(super) fn undo_remove(&mut self) {
        let keys = std::mem::take(&mut self.removed);
        self.window.global::<ProjectsUi>().set_undo_count(0);
        if !keys.is_empty() {
            self.send(Req::RestoreProjects(keys));
        }
    }

    pub(super) fn projects_restored(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.status(&error, true);
        }
        self.refresh_projects();
    }
}

pub fn sessions_changed() {
    with_state(|s| {
        // 设置页未打开时也会收到会话事件，此时不发请求。
        if !s.window.get_settings_open() {
            return;
        }
        match s.page().as_str() {
            "projects" => s.refresh_projects(),
            "archive" => s.refresh_archive(0),
            _ => {}
        }
    });
}

pub(super) fn wire(window: &AppWindow) {
    let projects = window.global::<ProjectsUi>();
    projects.on_search(|_| with_state(State::refresh_projects));
    projects.on_set_unused_only(|on| {
        with_state(|s| {
            s.window.global::<ProjectsUi>().set_unused_only(on);
            s.show_projects();
        });
    });
    projects.on_select(|key, on| with_state(|s| s.select_project(&key, on)));
    projects.on_select_only(|key| with_state(|s| s.select_only_project(&key)));
    projects.on_remove_selected(|delete_data| with_state(|s| s.remove_projects(delete_data)));
    projects.on_undo_remove(|| with_state(State::undo_remove));
}
