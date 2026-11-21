use super::{
    sv, ChangeDirection, ChangeRow, ChatCall, Command, DialogTarget, FileAction, FileChangePlan,
    ItemId, Model, ModelRc, OutputRow, SessionId, UiState, VecModel,
};

impl UiState {
    // ---- agent slash commands (R13) ------------------------------------------------------------------

    pub(in crate::bridge) fn load_commands(&self) {
        let Some(session) = self.active.clone() else {
            return;
        };
        self.chat_call(
            |reply| ChatCall::Commands { session, reply },
            |_, list| {
                crate::chat::agent_commands(
                    list.commands
                        .into_iter()
                        .map(|c| {
                            let detail = match c.input_hint {
                                Some(hint) if !hint.is_empty() => {
                                    format!("{} · {hint}", c.description)
                                }
                                _ => c.description,
                            };
                            (c.name, detail)
                        })
                        .collect(),
                );
            },
        );
    }

    // ---- changed files (R14) -------------------------------------------------------------------------

    pub(in crate::bridge) fn plan_changes(&self, item: ItemId, direction: ChangeDirection) {
        self.plan_changes_with(item, direction, None);
    }

    pub(super) fn plan_changes_with(
        &self,
        item: ItemId,
        direction: ChangeDirection,
        undo: Option<FileChangePlan>,
    ) {
        let Some(session) = self.active.clone() else {
            return;
        };
        let asked = session.clone();
        self.chat_call_quiet(
            |reply| ChatCall::PlanFiles {
                session,
                item,
                direction,
                reply,
            },
            move |s, plan| s.changes_planned(&asked, item, plan, undo),
        );
    }

    /// `undo` = the Undo plan already fetched when this is the follow-up Reapply plan.
    pub(super) fn changes_planned(
        &mut self,
        session: &str,
        item: ItemId,
        plan: FileChangePlan,
        undo: Option<FileChangePlan>,
    ) {
        if !self.is_active(session) || !self.rows.contains_key(&item) {
            return;
        }
        let usable = plan.available && !plan.files.is_empty();
        if plan.direction == ChangeDirection::Undo {
            // 修复：撤销之后 hub 仍返回可用的撤销计划（每个文件都标为不安全），卡片一直显示“撤销”。
            // 早先用“没有安全文件”猜测撤销已生效；现在按 hub 的 `applied`（文件已等于撤销目标）判断，
            // 已撤销时改显示“重新应用”计划（phase-9-parity.md §9.2.5 R14 follow-up）。
            if !usable || plan.applied {
                let keep = (usable && !plan.applied).then_some(plan);
                self.plan_changes_with(item, ChangeDirection::Reapply, keep);
                return;
            }
        } else if !usable || plan.applied {
            match undo {
                Some(undo) => self.show_changes(item, undo),
                None => self.update_row(item, |row| {
                    row.changes = ModelRc::default();
                    row.change_dir = 0;
                }),
            }
            return;
        }
        self.show_changes(item, plan);
    }

    pub(super) fn show_changes(&mut self, item: ItemId, plan: FileChangePlan) {
        // 同一轮次（failover）只在最后一个 assistant 条目上显示一张卡片。
        let turn = plan.turn_id.to_string();
        match self.turn.change_turns.get(&turn).copied() {
            Some(other) if other > item => return,
            Some(other) if other != item => self.update_row(other, |row| {
                row.changes = ModelRc::default();
                row.change_dir = 0;
            }),
            _ => {}
        }
        self.turn.change_turns.insert(turn, item);
        let mut grew = false;
        for f in &plan.files {
            if !self.turn.outputs.contains(&f.path) {
                self.turn.outputs.push(f.path.clone());
                grew = true;
            }
        }
        if grew {
            let outputs: Vec<OutputRow> = self
                .turn
                .outputs
                .iter()
                .map(|path| OutputRow {
                    name: sv(path.rsplit(['/', '\\']).next().unwrap_or(path)),
                    path: sv(path),
                })
                .collect();
            self.ui().set_outputs(ModelRc::new(VecModel::from(outputs)));
        }
        let rows: Vec<ChangeRow> = plan
            .files
            .iter()
            .map(|f| ChangeRow {
                path: sv(&f.path),
                action: match f.action {
                    FileAction::Restore => 0,
                    FileAction::Delete => 1,
                },
                safe: f.safe,
                reason: sv(f.reason.as_deref().unwrap_or_default()),
            })
            .collect();
        let dir = if plan.direction == ChangeDirection::Undo {
            1
        } else {
            2
        };
        let note = plan.reason.unwrap_or_default();
        self.update_row(item, |row| {
            row.changes = ModelRc::new(VecModel::from(rows));
            row.change_dir = dir;
            row.change_note = sv(&note);
        });
    }

    pub(in crate::bridge) fn files_restored(&self, session: &str, item: Option<ItemId>) {
        if !self.is_active(session) {
            return;
        }
        match item {
            Some(item) => self.plan_changes(item, ChangeDirection::Undo),
            None => {
                for &item in self.turn.change_turns.values() {
                    self.plan_changes(item, ChangeDirection::Undo);
                }
            }
        }
    }

    pub(super) fn apply_changes(&self, item: ItemId, direction: ChangeDirection) {
        let Some(session) = self.active.clone() else {
            return;
        };
        self.chat_call(
            |reply| ChatCall::ApplyFiles {
                session,
                item,
                direction,
                paths: None,
                include_unsafe: false,
                reply,
            },
            move |s, report| {
                crate::chat::toast_label(if report.skipped.is_empty() {
                    "filesRestored"
                } else {
                    "filesSkipped"
                });
                s.plan_changes(item, ChangeDirection::Undo);
            },
        );
    }

    // ---- turn actions (R10) --------------------------------------------------------------------------

    pub(super) fn turn_action(&mut self, action: &str, id: i32) {
        let (Some(session), Ok(item)) = (self.active.clone(), u64::try_from(id)) else {
            return;
        };
        match action {
            "edit" => {
                let text = self
                    .rows
                    .get(&item)
                    .and_then(|&i| self.chat.row_data(i))
                    .map(|r| r.text.to_string())
                    .unwrap_or_default();
                self.open_dialog(DialogTarget::Edit { session, item }, &text, "", true);
            }
            "rewind" => self.open_dialog(DialogTarget::Rewind { session, item }, "", "", false),
            "fork" => self.fork(session, Some(item)),
            "retry" => self.chat_call(
                |reply| ChatCall::Retry {
                    session,
                    item,
                    agent: None,
                    restore_files: false,
                    reply,
                },
                |_, ()| {},
            ),
            "undo" => self.apply_changes(item, ChangeDirection::Undo),
            "reapply" => self.apply_changes(item, ChangeDirection::Reapply),
            _ => {}
        }
    }

    pub(super) fn fork(&self, session: SessionId, item: Option<ItemId>) {
        self.chat_call(
            |reply| ChatCall::Fork {
                session,
                item,
                reply,
            },
            |s, summary| {
                s.send(Command::OpenSession {
                    session: summary.id,
                });
            },
        );
    }
}
