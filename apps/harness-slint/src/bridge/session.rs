use super::{
    show_workspace, AgentId, AgentState, AgentStatus, Command, ContextItem, FileReq, Item, ItemId,
    ListDir, ModelRc, PathBuf, PermOption, PermissionOptionKind, QueueRow, QueuedMessage,
    SessionId, SessionSummary, Settings, UiState, VecModel,
};

impl UiState {
    pub(super) fn open_session(
        &mut self,
        summary: &SessionSummary,
        items: &[(ItemId, Item)],
        sticky: AgentId,
        queued: &[QueuedMessage],
        paused: bool,
    ) {
        if self.turn.keep_unread.as_deref() != Some(summary.id.as_str()) {
            self.turn.keep_unread = None;
        }
        self.active = Some(summary.id.clone());
        self.opened_title.clone_from(&summary.title);
        self.set_workspace(&summary.workspace_path);
        if let Some(existing) = self.summaries.iter_mut().find(|s| s.id == summary.id) {
            *existing = summary.clone();
        }
        self.rows.clear();
        self.thumbs.clear_rows();
        self.texts.clear();
        self.blocks.clear();
        self.chat.set_vec(Vec::new());
        self.reset_turn_view();
        let items = self.split_history(items);
        for (id, item) in items {
            self.push_item(*id, item);
        }
        self.sticky = sticky;
        self.set_queue(queued);
        self.window.set_paused(paused);
        self.window.set_perm_visible(false);
        self.refresh_agents();
        self.refresh_sessions();
        self.session_opened(items, summary.unread);
        self.reset_dock();
        self.sync_draft();
        if let Some((text, mut context)) = self.pending_send.take() {
            self.add_modes(&summary.id, &mut context);
            self.send_message(summary.id.clone(), text, context);
        }
    }

    /// 修复：工作区只在启动时设置一次，打开其它文件夹的会话后右侧路径、左下角项目名、文件树和 Git/终端面板
    /// 仍停在旧文件夹；现在跟随打开的会话（空路径 = 保持当前工作区）。
    pub(super) fn set_workspace(&mut self, path: &str) {
        let path = PathBuf::from(path);
        // 与界面上显示的工作区比较：启动时界面为空，第一次打开会话时必须显示。
        if path.as_os_str().is_empty()
            || self.window.get_workspace().as_str() == path.to_string_lossy()
        {
            return;
        }
        self.workspace.clone_from(&path);
        show_workspace(&self.window, &path);
        let _ = self.files_tx.send(FileReq::List(ListDir {
            dir: path.clone(),
            depth: 0,
        }));
        crate::chat::set_workspace(&path);
        crate::live::set_workspace(&path);
        crate::security::set_workspace(&path);
        crate::settings::recall::set_workspace(&path);
        crate::settings::graph::set_workspace(&self.window, &path);
        crate::panels::set_workspace(path);
    }

    /// New session on the composer's current agent: passing it in `CreateSession` keeps a choice made before any
    /// session existed (`SetStickyAgent` needs a session) from being replaced by the hub's default order.
    /// `workspace` 为该会话指定工作区（“Open folder…”，None = 启动时的工作区）。
    pub(super) fn new_session(&mut self, worktree: bool, workspace: Option<PathBuf>) {
        let agent = self.sticky.clone();
        // 修复：None 时用当前打开会话所属的项目，没有打开的会话时用草稿选的项目（bridge/draft.rs），
        // 不再总是回到启动时的工作区。
        let workspace = workspace
            .or_else(|| self.active_project())
            .or_else(|| Some(self.draft_target()));
        if worktree {
            self.send(Command::CreateWorktreeSession { agent, workspace });
        } else {
            self.send(Command::CreateSession { agent, workspace });
        }
    }

    /// Plain text goes as `Send`; with files, images or sessions attached as `SendContext` (spec §9.2.1).
    pub(super) fn send_message(&self, session: SessionId, text: String, context: Vec<ContextItem>) {
        let agent = self.sticky.clone();
        if context.is_empty() {
            self.send(Command::Send {
                session,
                agent,
                text,
            });
        } else {
            self.send(Command::SendContext {
                session,
                agent,
                text,
                context,
            });
        }
    }

    /// Leaves the open session (it was archived or deleted): empty chat, nothing queued.
    pub(super) fn clear_session(&mut self) {
        self.active = None;
        self.turn.keep_unread = None;
        self.reset_turn_view();
        self.rows.clear();
        self.thumbs.clear_rows();
        self.texts.clear();
        self.blocks.clear();
        self.chat.set_vec(Vec::new());
        self.clear_history();
        self.set_queue(&[]);
        self.window.set_paused(false);
        self.window.set_running(false);
        self.window.set_perm_visible(false);
        self.reset_dock();
        self.sync_draft();
    }

    pub(super) fn set_perm_choices(
        &mut self,
        options: Vec<(String, String, PermissionOptionKind)>,
    ) {
        let rows: Vec<PermOption> = options
            .iter()
            .map(|(id, label, kind)| PermOption {
                id: id.as_str().into(),
                label: label.as_str().into(),
                kind: match kind {
                    PermissionOptionKind::AllowOnce => 0,
                    PermissionOptionKind::AllowAlways => 1,
                    PermissionOptionKind::RejectOnce => 2,
                    PermissionOptionKind::RejectAlways => 3,
                },
            })
            .collect();
        self.window
            .set_perm_options(ModelRc::new(VecModel::from(rows)));
        self.perm_choices = options
            .into_iter()
            .map(|(id, _, kind)| (id, kind))
            .collect();
    }

    /// Approve / deny (buttons, keys 1/2, Esc): the agent's own "once" option when it offered one, else its
    /// "always" one. Before the options arrive nothing is sent.
    pub(super) fn answer_permission(&self, request: u64, allow: bool) {
        let Some(session) = self.active.clone() else {
            return;
        };
        let (once, always) = if allow {
            (
                PermissionOptionKind::AllowOnce,
                PermissionOptionKind::AllowAlways,
            )
        } else {
            (
                PermissionOptionKind::RejectOnce,
                PermissionOptionKind::RejectAlways,
            )
        };
        let pick = self
            .perm_choices
            .iter()
            .find(|(_, kind)| *kind == once)
            .or_else(|| self.perm_choices.iter().find(|(_, kind)| *kind == always));
        // 修复（R9）：旧的 RespondPermission 兜底按“第一个 allow/reject 选项”猜测，可能把 always 当成 once；
        // A 的核心已对每个请求先发 PermissionChoices，选项到达前卡片显示“正在加载选项”，这里不再猜测。
        if let Some((option_id, _)) = pick {
            self.send(Command::ChoosePermission {
                session,
                request,
                option_id: option_id.clone(),
            });
        }
    }

    pub(super) fn set_queue(&self, items: &[QueuedMessage]) {
        self.queue.set_vec(
            items
                .iter()
                .map(|q| QueueRow {
                    id: q.id.as_str().into(),
                    text: q.text.as_str().into(),
                })
                .collect::<Vec<_>>(),
        );
        self.window
            .set_queued(i32::try_from(items.len()).unwrap_or(i32::MAX));
    }

    pub(super) fn is_active(&self, session: &str) -> bool {
        self.active.as_deref() == Some(session)
    }

    /// Theme, language and zoom are applied by `crate::settings` from the store's own snapshots (Phase 9.1);
    /// the core's settings event only drives the fallback order here.
    pub(super) fn apply_settings(&mut self, settings: Settings) {
        self.settings = settings;
        self.refresh_agents();
    }

    pub(super) fn apply_agents(&mut self, list: Vec<AgentStatus>) {
        crate::settings::agents_changed(
            list.iter()
                .map(|a| {
                    (
                        a.id.clone(),
                        a.name.clone(),
                        !matches!(
                            a.state,
                            AgentState::Unavailable | AgentState::AdapterMissing
                        ),
                    )
                })
                .collect(),
        );
        self.agent_list = list;
        if self.sticky.is_empty() {
            self.sticky = self
                .settings
                .fallback_order
                .iter()
                .find(|id| {
                    self.agent_list
                        .iter()
                        .any(|a| &&a.id == id && a.state == AgentState::Available)
                })
                .cloned()
                .unwrap_or_default();
        }
        self.refresh_agents();
        self.refresh_sessions();
    }
}
