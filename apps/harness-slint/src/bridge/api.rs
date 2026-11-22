use super::{
    with_state, AgentState, AppWindow, Command, ComponentHandle, Model, PathBuf, SettingsCall,
    STATE,
};

/// One transcript row as plain text (find, export).
pub struct TranscriptRow {
    pub id: i32,
    /// `ChatItem.kind`: 0 user, 1 assistant, 2 tool, 3 failover, 4 paused, 5 thinking.
    pub kind: i32,
    pub who: String,
    pub text: String,
}

/// The loaded transcript of the open session, in display order.
pub fn transcript() -> Vec<TranscriptRow> {
    let mut rows = Vec::new();
    with_state(|s| {
        s.load_all_earlier();
        for i in 0..s.chat.row_count() {
            let Some(item) = s.chat.row_data(i) else {
                continue;
            };
            let text = match item.kind {
                0 | 1 => item.text.to_string(),
                2 => item.title.to_string(),
                3 => format!("Switched from {} to {}", item.from, item.to),
                5 => format!("Thought: {}", item.text),
                _ => "All agents reached their limits. Paused".to_string(),
            };
            rows.push(TranscriptRow {
                id: item.id,
                kind: item.kind,
                who: item.agent.to_string(),
                text,
            });
        }
    });
    rows
}

/// (id, display name) of every known agent.
pub fn agent_names() -> Vec<(String, String)> {
    let mut list = Vec::new();
    with_state(|s| {
        list = s
            .agent_list
            .iter()
            .filter(|a| {
                !matches!(
                    a.state,
                    AgentState::Unavailable | AgentState::AdapterMissing
                )
            })
            .map(|a| (a.id.clone(), a.name.clone()))
            .collect();
    });
    list
}

pub fn open_session(id: &str) {
    with_state(|s| {
        s.window.set_settings_open(false);
        s.send(Command::OpenSession {
            session: id.to_string(),
        });
    });
}

pub fn show_earlier() {
    with_state(|s| {
        if let Some(session) = s.active.clone() {
            s.send(Command::OpenSessionAll { session });
        }
    });
}

pub fn choose_agent(id: &str) {
    if let Some(window) = STATE.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.window.clone_strong()))
    }) {
        window.invoke_choose_agent(id.into());
    }
}

/// Session action from the command center ("rename", "fork", "toggle-pin"; empty id = the open session).
pub fn session_action(action: &str, id: &str) {
    with_state(|s| s.session_action(action, id));
}

/// Scheduled prompts action from the command center ("open").
pub fn schedule_action(action: &str, id: &str) {
    with_state(|s| s.schedule_action(action, id));
}

pub fn refresh_usage_parts() {
    with_state(|s| s.render_parts());
}

pub fn archive(id: String) {
    archive_session(id);
}

/// Archives one session from the sidebar (hub `archive_sessions`).
pub(super) fn archive_session(id: String) {
    with_state(|s| s.archive_sessions(vec![id]));
}

impl super::UiState {
    /// Archives sessions (one from the sidebar, or all of a workspace); restorable in Settings › Archived.
    pub(super) fn archive_sessions(&self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send(Command::Settings(SettingsCall::ArchiveSessions(ids, reply)));
        // 归档结果经 Event::Sessions 刷新列表；这里只把失败（运行中 Busy 等）显示出来。
        let spawned = slint::spawn_local(async move {
            let error = match answer.await {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error.to_string()),
                Err(_) => Some(harness_core_api::ApiError::ShutDown.to_string()),
            };
            if let Some(error) = error {
                with_state(|s| s.window.set_error_text(error.into()));
            }
        });
        if spawned.is_err() {
            self.window.set_error_text("event loop unavailable".into());
        }
    }
}

pub fn workspace_path() -> String {
    let mut out = String::new();
    with_state(|s| out = s.workspace.to_string_lossy().into_owned());
    out
}

/// “Open folder…”：在后台线程打开系统文件夹选择框（阻塞调用不能占住 UI 线程），选好后回到事件循环，
/// 用当前粘滞 agent 新建一个工作区为该文件夹的会话。修复：此前该回调把目录写进 RELAUNCH 并 `quit_event_loop`，
/// 由 `relaunch()` 重开进程，用户看到的是 Harness 直接退出。
pub fn install_open_folder(window: &AppWindow) {
    window.on_open_folder(|| {
        let mut start = PathBuf::from(workspace_path());
        std::thread::spawn(move || {
            if !start.is_dir() {
                start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            }
            let Some(dir) = rfd::FileDialog::new().set_directory(&start).pick_folder() else {
                return;
            };
            let _ = slint::invoke_from_event_loop(move || {
                with_state(|s| s.new_session(false, Some(dir)));
            });
        });
    });
}

pub fn orchestra_agents() -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    with_state(|s| {
        out = s
            .agent_list
            .iter()
            .map(|a| {
                (
                    a.id.as_str().to_owned(),
                    a.name.clone(),
                    !s.disabled.contains(&a.id),
                )
            })
            .collect();
    });
    out
}

/// Sends one command to the core (settings pages: overview, connection test).
pub fn send(command: Command) {
    let pending = STATE.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => {
            if let Some(state) = guard.as_mut() {
                state.send(command);
            }
            None
        }
        Err(_) => Some(command),
    });
    if let Some(command) = pending {
        slint::Timer::single_shot(std::time::Duration::ZERO, move || send(command));
    }
}

pub fn request_agent_options() {
    with_state(|s| {
        for agent in &s.agent_list {
            s.send(Command::AgentOptions(agent.id.clone()));
        }
    });
}

/// Drops the UI state (and with it the command/file senders) so the core thread can finish.
pub fn teardown() {
    STATE.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some(state) = guard.take() {
                let _ = state.cmd.send(Command::Shutdown);
            }
        }
    });
}
