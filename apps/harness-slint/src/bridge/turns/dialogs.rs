use super::{sv, ChatCall, Command, DialogTarget, GroupColor, SessionId, UiState, UpdateGroup};

impl UiState {
    // ---- shared text dialog ----------------------------------------------------------------------------

    pub(super) fn open_dialog(
        &mut self,
        target: DialogTarget,
        text: &str,
        text2: &str,
        multiline: bool,
    ) {
        let kind = match &target {
            DialogTarget::None => return,
            DialogTarget::Rename(_) => "rename",
            DialogTarget::Edit { .. } => "edit",
            DialogTarget::Rewind { .. } => "rewind",
            DialogTarget::Deny { .. } => "deny",
            DialogTarget::Delete(_) => "delete",
            DialogTarget::NewGroup => "new-group",
            DialogTarget::RenameGroup(_) => "rename-group",
            DialogTarget::DeleteGroup(_) => "delete-group",
            DialogTarget::RemoveProject(_) => "remove-project",
            DialogTarget::NewSchedule => "new-schedule",
        };
        // 确认类对话框（回退、删除）没有输入框：用一个只读说明 + 勾选/确认按钮。
        let confirm_only = matches!(
            target,
            DialogTarget::Rewind { .. }
                | DialogTarget::Delete(_)
                | DialogTarget::DeleteGroup(_)
                | DialogTarget::RemoveProject(_)
        );
        let destructive = confirm_only;
        self.turn.dialog = target;
        let ui = self.ui();
        ui.set_dialog_kind(sv(kind));
        ui.set_dialog_text(sv(text));
        ui.set_dialog_text2(sv(text2));
        ui.set_dialog_check(false);
        ui.set_dialog_multiline(multiline);
        ui.set_dialog_destructive(destructive);
        ui.set_dialog_confirm_only(confirm_only);
        ui.set_dialog_open(true);
    }

    pub(super) fn dialog_submit(&mut self) {
        let (text, text2, check) = {
            let ui = self.ui();
            (
                ui.get_dialog_text().to_string(),
                ui.get_dialog_text2().to_string(),
                ui.get_dialog_check(),
            )
        };
        let target = std::mem::take(&mut self.turn.dialog);
        let needs_text = matches!(
            target,
            DialogTarget::Rename(_)
                | DialogTarget::Edit { .. }
                | DialogTarget::Deny { .. }
                | DialogTarget::NewGroup
                | DialogTarget::RenameGroup(_)
                | DialogTarget::NewSchedule
        );
        if needs_text && text.trim().is_empty() {
            self.turn.dialog = target;
            return;
        }
        self.ui().set_dialog_open(false);
        match target {
            DialogTarget::None => {}
            DialogTarget::Rename(session) => self.chat_call(
                |reply| ChatCall::Rename {
                    session,
                    title: text.trim().to_string(),
                    reply,
                },
                |_, ()| {},
            ),
            DialogTarget::Edit { session, item } => self.chat_call(
                |reply| ChatCall::EditResend {
                    session,
                    item,
                    text,
                    context: None,
                    agent: None,
                    restore_files: check,
                    reply,
                },
                |_, ()| {},
            ),
            DialogTarget::Rewind { session, item } => self.chat_call(
                |reply| ChatCall::Rewind {
                    session,
                    item,
                    restore_files: check,
                    reply,
                },
                |_, result| crate::chat::restore_composer(&result.user_text, result.context),
            ),
            DialogTarget::Deny { session, request } => {
                self.deny_with_feedback(session, request, text);
            }
            DialogTarget::Delete(session) => self.delete_session(session),
            DialogTarget::NewGroup => self.chat_call(
                |reply| ChatCall::CreateGroup {
                    name: text.trim().to_string(),
                    color: GroupColor::Gray,
                    reply,
                },
                |s, _| s.load_groups(),
            ),
            DialogTarget::RenameGroup(id) => self.update_group(UpdateGroup {
                id,
                name: Some(text.trim().to_string()),
                color: None,
            }),
            DialogTarget::DeleteGroup(id) => {
                self.chat_call(
                    |reply| ChatCall::DeleteGroup(id, reply),
                    |s, ()| s.load_groups(),
                );
            }
            DialogTarget::RemoveProject(key) => {
                let ids = self
                    .summaries
                    .iter()
                    .filter(|s| s.project == key)
                    .map(|s| s.id.clone())
                    .collect();
                self.archive_sessions(ids);
            }
            DialogTarget::NewSchedule => self.create_schedule(&text, &text2),
        }
    }

    // ---- permission: deny with feedback (R9) -----------------------------------------------------------

    pub(super) fn deny_with_feedback(&self, session: SessionId, request: u64, text: String) {
        use harness_core_api::PermissionOptionKind as K;
        let option = self
            .perm_choices
            .iter()
            .find(|(_, kind)| *kind == K::RejectOnce)
            .or_else(|| {
                self.perm_choices
                    .iter()
                    .find(|(_, kind)| *kind == K::RejectAlways)
            });
        let Some((option_id, _)) = option else {
            // 选项尚未送达时无法拒绝（RespondPermission 兜底已移除）；保留文本，提示稍后再试。
            self.fail("Permission options are still loading");
            return;
        };
        self.send(Command::ChoosePermission {
            session: session.clone(),
            request,
            option_id: option_id.clone(),
        });
        // 拒绝后同一轮仍在运行：反馈作为下一条消息排队（CommandInbox 串行接收）。
        self.send(Command::Send {
            session,
            agent: self.sticky.clone(),
            text,
        });
    }
}
